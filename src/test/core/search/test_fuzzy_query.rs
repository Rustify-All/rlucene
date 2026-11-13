/*
 * Licensed to the Apache Software Foundation (ASF) under one or more
 * contributor license agreements.  See the NOTICE file distributed with
 * this work for additional information regarding copyright ownership.
 * The ASF licenses this file to You under the Apache License, Version 2.0
 * (the "License"); you may not use this file except in compliance with
 * the License.  You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */
use crate::core::document::document::Document;
use crate::core::document::field::Store;
use crate::core::document::field_type::FieldType;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::indexable_field::IndexableField;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::multi_reader::MultiReader;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term::Term;
use crate::core::search::boolean_clause::Occur;
use crate::core::search::boolean_query::Builder as BooleanQueryBuilder;
use crate::core::search::fuzzy_query::FuzzyQuery;
use crate::core::search::multi_term_query::TopTermsBoostOnlyBooleanQueryRewrite;
use crate::core::search::query::{QueryBase, QueryRef};
use crate::core::search::query_visitor::{
  DefaultQueryVisitor, EMPTY_VISITOR, EmptyQueryVisitor, QueryVisitor,
};
use crate::core::search::similarities_impl::classic_similarity;
use crate::core::store::directory::DirEnum;
use crate::core::util::CoreHelper;
use crate::core::util::automation::byte_run_automaton::ByteRunAutomaton;
use crate::core::util::automation::byte_runnable::ByteRunnable;
use crate::core::util::automation::levenshtein_automata::LevenshteinAutomata;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::io_utils::IOUtils;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::analysis::mock_tokenizer;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, at_least_usize, new_directory_shared, new_index_writer_config_with_analyzer,
  new_merge_policy_with_mock_mp, new_searcher_with_reader, new_string_field, new_text_field,
  random,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::{Rng, RngExt};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fmt::Write;

#[allow(dead_code)] // for quick search
struct TestFuzzyQuery;
#[test]
fn test_basic_prefix() -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;
  let writer = RandomIndexWriter::new(&mut random, directory.clone())?;
  let mut field_to_type = HashMap::new();
  add_doc(&mut random, "abc", &writer, &mut field_to_type)?;

  let reader = writer.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;
  writer.close(&mut random)?;

  let query = FuzzyQuery::with_max_edits_and_prefix(
    Term::from_text("field", "abc"),
    FuzzyQuery::DEFAULT_MAX_EDITS,
    1,
  )?;

  let hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(1, hits.len());

  searcher.get_index_reader().close()?;
  directory.close()
}
#[test]
fn test_fuzziness() -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  iwc.set_merge_policy(new_merge_policy_with_mock_mp(&mut random, false)?);

  let writer = RandomIndexWriter::with_config(&mut random, directory.clone(), iwc);
  let mut field_to_type = HashMap::new();

  add_doc(&mut random, "aaaaa", &writer, &mut field_to_type)?;
  add_doc(&mut random, "aaaab", &writer, &mut field_to_type)?;
  add_doc(&mut random, "aaabb", &writer, &mut field_to_type)?;
  add_doc(&mut random, "aabbb", &writer, &mut field_to_type)?;
  add_doc(&mut random, "abbbb", &writer, &mut field_to_type)?;
  add_doc(&mut random, "bbbbb", &writer, &mut field_to_type)?;
  add_doc(&mut random, "ddddd", &writer, &mut field_to_type)?;

  let reader = writer.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;
  writer.close(&mut random)?;

  let mut query = FuzzyQuery::with_max_edits_and_prefix(
    Term::from_text("field", "aaaaa"),
    FuzzyQuery::DEFAULT_MAX_EDITS,
    0,
  )?;
  let mut hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(3, hits.len());

  query = FuzzyQuery::with_max_edits_and_prefix(
    Term::from_text("field", "aaaaa"),
    FuzzyQuery::DEFAULT_MAX_EDITS,
    1,
  )?;
  hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(3, hits.len());

  query = FuzzyQuery::with_max_edits_and_prefix(
    Term::from_text("field", "aaaaa"),
    FuzzyQuery::DEFAULT_MAX_EDITS,
    2,
  )?;
  hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(3, hits.len());

  query = FuzzyQuery::with_max_edits_and_prefix(
    Term::from_text("field", "aaaaa"),
    FuzzyQuery::DEFAULT_MAX_EDITS,
    3,
  )?;
  hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(3, hits.len());

  query = FuzzyQuery::with_max_edits_and_prefix(
    Term::from_text("field", "aaaaa"),
    FuzzyQuery::DEFAULT_MAX_EDITS,
    4,
  )?;
  hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(2, hits.len());

  query = FuzzyQuery::with_max_edits_and_prefix(
    Term::from_text("field", "aaaaa"),
    FuzzyQuery::DEFAULT_MAX_EDITS,
    5,
  )?;
  hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(1, hits.len());

  query = FuzzyQuery::with_max_edits_and_prefix(
    Term::from_text("field", "aaaaa"),
    FuzzyQuery::DEFAULT_MAX_EDITS,
    6,
  )?;
  hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(1, hits.len());

  query = FuzzyQuery::with_max_edits_and_prefix(
    Term::from_text("field", "bbbbb"),
    FuzzyQuery::DEFAULT_MAX_EDITS,
    0,
  )?;
  hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(3, hits.len(), "3 documents should match");

  let mut order: &[&str] = &["bbbbb", "abbbb", "aabbb"];
  let mut stored_fields = searcher.stored_fields()?;
  for i in 0..hits.len() {
    let document = stored_fields.document(hits[i].doc)?;
    let term = document
      .get_field("field")
      .unwrap()
      .string_value()?
      .unwrap();
    assert_eq!(order[i], term.as_ref());
  }

  query = FuzzyQuery::with_options(
    Term::from_text("field", "bbbbb"),
    FuzzyQuery::DEFAULT_MAX_EDITS,
    0,
    2,
    false,
  )?;
  hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(2, hits.len(), "only 2 documents should match");

  order = &["bbbbb", "abbbb"];
  for i in 0..hits.len() {
    let document = stored_fields.document(hits[i].doc)?;
    let term = document
      .get_field("field")
      .unwrap()
      .string_value()?
      .unwrap();
    assert_eq!(order[i], term.as_ref());
  }

  query = FuzzyQuery::with_max_edits_and_prefix(
    Term::from_text("field", "xxxxx"),
    FuzzyQuery::DEFAULT_MAX_EDITS,
    0,
  )?;
  hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(0, hits.len());

  query = FuzzyQuery::with_max_edits_and_prefix(
    Term::from_text("field", "aaccc"),
    FuzzyQuery::DEFAULT_MAX_EDITS,
    0,
  )?;
  hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(0, hits.len());

  query = FuzzyQuery::with_max_edits_and_prefix(
    Term::from_text("field", "aaaaa"),
    FuzzyQuery::DEFAULT_MAX_EDITS,
    0,
  )?;
  hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(3, hits.len());

  let document = stored_fields.document(hits[0].doc)?;
  let term = document
    .get_field("field")
    .unwrap()
    .string_value()?
    .unwrap();
  assert_eq!("aaaaa", term.as_ref());

  let document = stored_fields.document(hits[1].doc)?;
  let term = document
    .get_field("field")
    .unwrap()
    .string_value()?
    .unwrap();
  assert_eq!("aaaab", term.as_ref());

  let document = stored_fields.document(hits[2].doc)?;
  let term = document
    .get_field("field")
    .unwrap()
    .string_value()?
    .unwrap();
  assert_eq!("aaabb", term.as_ref());

  query = FuzzyQuery::with_max_edits_and_prefix(
    Term::from_text("field", "aaaac"),
    FuzzyQuery::DEFAULT_MAX_EDITS,
    0,
  )?;
  hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(3, hits.len());

  let document = stored_fields.document(hits[0].doc)?;
  let term = document
    .get_field("field")
    .unwrap()
    .string_value()?
    .unwrap();
  assert_eq!("aaaaa", term.as_ref());

  let document = stored_fields.document(hits[1].doc)?;
  let term = document
    .get_field("field")
    .unwrap()
    .string_value()?
    .unwrap();
  assert_eq!("aaaab", term.as_ref());

  let document = stored_fields.document(hits[2].doc)?;
  let term = document
    .get_field("field")
    .unwrap()
    .string_value()?
    .unwrap();
  assert_eq!("aaabb", term.as_ref());

  query = FuzzyQuery::with_max_edits_and_prefix(
    Term::from_text("field", "aaaac"),
    FuzzyQuery::DEFAULT_MAX_EDITS,
    1,
  )?;
  hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(3, hits.len());

  let document = stored_fields.document(hits[0].doc)?;
  let term = document
    .get_field("field")
    .unwrap()
    .string_value()?
    .unwrap();
  assert_eq!("aaaaa", term.as_ref());

  let document = stored_fields.document(hits[1].doc)?;
  let term = document
    .get_field("field")
    .unwrap()
    .string_value()?
    .unwrap();
  assert_eq!("aaaab", term.as_ref());

  let document = stored_fields.document(hits[2].doc)?;
  let term = document
    .get_field("field")
    .unwrap()
    .string_value()?
    .unwrap();
  assert_eq!("aaabb", term.as_ref());

  query = FuzzyQuery::with_max_edits_and_prefix(
    Term::from_text("field", "aaaac"),
    FuzzyQuery::DEFAULT_MAX_EDITS,
    2,
  )?;
  hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(3, hits.len());

  let document = stored_fields.document(hits[0].doc)?;
  let term = document
    .get_field("field")
    .unwrap()
    .string_value()?
    .unwrap();
  assert_eq!("aaaaa", term.as_ref());

  let document = stored_fields.document(hits[1].doc)?;
  let term = document
    .get_field("field")
    .unwrap()
    .string_value()?
    .unwrap();
  assert_eq!("aaaab", term.as_ref());

  let document = stored_fields.document(hits[2].doc)?;
  let term = document
    .get_field("field")
    .unwrap()
    .string_value()?
    .unwrap();
  assert_eq!("aaabb", term.as_ref());

  query = FuzzyQuery::with_max_edits_and_prefix(
    Term::from_text("field", "aaaac"),
    FuzzyQuery::DEFAULT_MAX_EDITS,
    3,
  )?;
  hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(3, hits.len());

  let document = stored_fields.document(hits[0].doc)?;
  let term = document
    .get_field("field")
    .unwrap()
    .string_value()?
    .unwrap();
  assert_eq!("aaaaa", term.as_ref());

  let document = stored_fields.document(hits[1].doc)?;
  let term = document
    .get_field("field")
    .unwrap()
    .string_value()?
    .unwrap();
  assert_eq!("aaaab", term.as_ref());

  let document = stored_fields.document(hits[2].doc)?;
  let term = document
    .get_field("field")
    .unwrap()
    .string_value()?
    .unwrap();
  assert_eq!("aaabb", term.as_ref());

  query = FuzzyQuery::with_max_edits_and_prefix(
    Term::from_text("field", "aaaac"),
    FuzzyQuery::DEFAULT_MAX_EDITS,
    4,
  )?;
  hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(2, hits.len());

  let document = stored_fields.document(hits[0].doc)?;
  let term = document
    .get_field("field")
    .unwrap()
    .string_value()?
    .unwrap();
  assert_eq!("aaaaa", term.as_ref());

  let document = stored_fields.document(hits[1].doc)?;
  let term = document
    .get_field("field")
    .unwrap()
    .string_value()?
    .unwrap();
  assert_eq!("aaaab", term.as_ref());

  query = FuzzyQuery::with_max_edits_and_prefix(
    Term::from_text("field", "aaaac"),
    FuzzyQuery::DEFAULT_MAX_EDITS,
    5,
  )?;
  hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(0, hits.len());

  query = FuzzyQuery::with_max_edits_and_prefix(
    Term::from_text("field", "ddddX"),
    FuzzyQuery::DEFAULT_MAX_EDITS,
    0,
  )?;
  hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(1, hits.len());

  let document = stored_fields.document(hits[0].doc)?;
  let term = document
    .get_field("field")
    .unwrap()
    .string_value()?
    .unwrap();
  assert_eq!("ddddd", term.as_ref());

  query = FuzzyQuery::with_max_edits_and_prefix(
    Term::from_text("field", "ddddX"),
    FuzzyQuery::DEFAULT_MAX_EDITS,
    1,
  )?;
  hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(1, hits.len());

  let document = stored_fields.document(hits[0].doc)?;
  let term = document
    .get_field("field")
    .unwrap()
    .string_value()?
    .unwrap();
  assert_eq!("ddddd", term.as_ref());

  query = FuzzyQuery::with_max_edits_and_prefix(
    Term::from_text("field", "ddddX"),
    FuzzyQuery::DEFAULT_MAX_EDITS,
    2,
  )?;
  hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(1, hits.len());

  let document = stored_fields.document(hits[0].doc)?;
  let term = document
    .get_field("field")
    .unwrap()
    .string_value()?
    .unwrap();
  assert_eq!("ddddd", term.as_ref());

  query = FuzzyQuery::with_max_edits_and_prefix(
    Term::from_text("field", "ddddX"),
    FuzzyQuery::DEFAULT_MAX_EDITS,
    3,
  )?;
  hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(1, hits.len());

  let document = stored_fields.document(hits[0].doc)?;
  let term = document
    .get_field("field")
    .unwrap()
    .string_value()?
    .unwrap();
  assert_eq!("ddddd", term.as_ref());

  query = FuzzyQuery::with_max_edits_and_prefix(
    Term::from_text("field", "ddddX"),
    FuzzyQuery::DEFAULT_MAX_EDITS,
    4,
  )?;
  hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(1, hits.len());

  let document = stored_fields.document(hits[0].doc)?;
  let term = document
    .get_field("field")
    .unwrap()
    .string_value()?
    .unwrap();
  assert_eq!("ddddd", term.as_ref());

  query = FuzzyQuery::with_max_edits_and_prefix(
    Term::from_text("field", "ddddX"),
    FuzzyQuery::DEFAULT_MAX_EDITS,
    5,
  )?;
  hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(0, hits.len());

  query = FuzzyQuery::with_max_edits_and_prefix(
    Term::from_text("anotherfield", "ddddX"),
    FuzzyQuery::DEFAULT_MAX_EDITS,
    0,
  )?;
  hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(0, hits.len());

  drop(stored_fields);
  searcher.get_index_reader().close()?;
  directory.close()
}
#[test]
fn test_prefix_length_equal_string_length() -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = RandomIndexWriter::with_config(&mut random, directory.clone(), iwc);
  let mut field_to_type = HashMap::new();
  add_doc(&mut random, "b*a", &writer, &mut field_to_type)?;
  add_doc(&mut random, "b*ab", &writer, &mut field_to_type)?;
  add_doc(&mut random, "b*abc", &writer, &mut field_to_type)?;
  add_doc(&mut random, "b*abcd", &writer, &mut field_to_type)?;
  let multibyte = "아프리카코끼리속";
  add_doc(&mut random, multibyte, &writer, &mut field_to_type)?;
  let reader = writer.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;
  writer.close(&mut random)?;
  let mut max_edits = 0;
  let mut prefix_length = 3;
  let mut query = FuzzyQuery::with_max_edits_and_prefix(
    Term::from_text("field", "b*a"),
    max_edits,
    prefix_length,
  )?;
  let mut hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(1, hits.len());

  max_edits = 1;
  query = FuzzyQuery::with_max_edits_and_prefix(
    Term::from_text("field", "b*a"),
    max_edits,
    prefix_length,
  )?;
  hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(2, hits.len());

  max_edits = 2;
  query = FuzzyQuery::with_max_edits_and_prefix(
    Term::from_text("field", "b*a"),
    max_edits,
    prefix_length,
  )?;
  hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(3, hits.len());

  max_edits = 1;
  prefix_length = multibyte.chars().count() - 1;
  let multibyte_prefix: String = multibyte.chars().take(prefix_length).collect();
  query = FuzzyQuery::with_max_edits_and_prefix(
    Term::from_text("field", multibyte_prefix),
    max_edits,
    prefix_length,
  )?;
  hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(1, hits.len());

  searcher.get_index_reader().close()?;
  directory.close()
}
#[test]
fn test2() -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::with_automaton(&mut random, mock_tokenizer::KEYWORD.clone(), false);
  let iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = RandomIndexWriter::with_config(&mut random, directory.clone(), iwc);
  let mut field_to_type = HashMap::new();
  add_doc(&mut random, "LANGE", &writer, &mut field_to_type)?;
  add_doc(&mut random, "LUETH", &writer, &mut field_to_type)?;
  add_doc(&mut random, "PIRSING", &writer, &mut field_to_type)?;
  add_doc(&mut random, "RIEGEL", &writer, &mut field_to_type)?;
  add_doc(&mut random, "TRZECZIAK", &writer, &mut field_to_type)?;
  add_doc(&mut random, "WALKER", &writer, &mut field_to_type)?;
  add_doc(&mut random, "WBR", &writer, &mut field_to_type)?;
  add_doc(&mut random, "WE", &writer, &mut field_to_type)?;
  add_doc(&mut random, "WEB", &writer, &mut field_to_type)?;
  add_doc(&mut random, "WEBE", &writer, &mut field_to_type)?;
  add_doc(&mut random, "WEBER", &writer, &mut field_to_type)?;
  add_doc(&mut random, "WEBERE", &writer, &mut field_to_type)?;
  add_doc(&mut random, "WEBREE", &writer, &mut field_to_type)?;
  add_doc(&mut random, "WEBEREI", &writer, &mut field_to_type)?;
  add_doc(&mut random, "WBRE", &writer, &mut field_to_type)?;
  add_doc(&mut random, "WITTKOPF", &writer, &mut field_to_type)?;
  add_doc(&mut random, "WOJNAROWSKI", &writer, &mut field_to_type)?;
  add_doc(&mut random, "WRICKE", &writer, &mut field_to_type)?;

  let reader = writer.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;
  writer.close(&mut random)?;

  let query = FuzzyQuery::with_max_edits_and_prefix(Term::from_text("field", "WEBER"), 2, 1)?;
  let hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(8, hits.len());

  searcher.get_index_reader().close()?;
  directory.close()
}
#[test]
fn test_single_query_exact_match_scores_highest() -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;
  let writer = RandomIndexWriter::new(&mut random, directory.clone())?;
  let mut field_to_type = HashMap::new();
  add_doc(&mut random, "smith", &writer, &mut field_to_type)?;
  add_doc(&mut random, "smith", &writer, &mut field_to_type)?;
  add_doc(&mut random, "smith", &writer, &mut field_to_type)?;
  add_doc(&mut random, "smith", &writer, &mut field_to_type)?;
  add_doc(&mut random, "smith", &writer, &mut field_to_type)?;
  add_doc(&mut random, "smith", &writer, &mut field_to_type)?;
  add_doc(&mut random, "smythe", &writer, &mut field_to_type)?;
  add_doc(&mut random, "smdssasd", &writer, &mut field_to_type)?;

  let reader = writer.get_reader(&mut random)?;
  let searcher =
    crate::test_framework::core::util::lucene_test_case::new_searcher_with_reader_and_similarity(
      reader,
      std::sync::Arc::new(classic_similarity::new()),
    )?;

  writer.close(&mut random)?;
  let search_terms = ["smith", "smythe", "smdssasd"];
  let mut stored_fields = searcher.stored_fields()?;
  for search_term in search_terms {
    let query = FuzzyQuery::with_max_edits_and_prefix(Term::from_text("field", search_term), 2, 1)?;
    let hits = searcher.search(query, 1000)?.score_docs;
    let best_doc = stored_fields.document(hits[0].doc)?;
    assert!(!hits.is_empty());
    let top_match = best_doc
      .get_field("field")
      .unwrap()
      .string_value()?
      .unwrap();
    assert_eq!(search_term, top_match.as_ref());
    if hits.len() > 1 {
      let worst_doc = stored_fields.document(hits[hits.len() - 1].doc)?;
      let worst_match = worst_doc
        .get_field("field")
        .unwrap()
        .string_value()?
        .unwrap();
      assert_ne!(search_term, worst_match.as_ref());
    }
  }
  drop(stored_fields);
  searcher.get_index_reader().close()?;
  directory.close()
}
#[test]
fn test_multiple_queries_idf_works() -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;
  let writer = RandomIndexWriter::new(&mut random, directory.clone())?;
  let mut field_to_type = HashMap::new();

  add_doc(&mut random, "michael smith", &writer, &mut field_to_type)?;
  add_doc(&mut random, "michael lucero", &writer, &mut field_to_type)?;
  add_doc(&mut random, "doug cutting", &writer, &mut field_to_type)?;
  add_doc(&mut random, "doug cuttin", &writer, &mut field_to_type)?;
  add_doc(&mut random, "michael wardle", &writer, &mut field_to_type)?;
  add_doc(&mut random, "micheal vegas", &writer, &mut field_to_type)?;
  add_doc(&mut random, "michael lydon", &writer, &mut field_to_type)?;

  let reader = writer.get_reader(&mut random)?;
  let searcher =
    crate::test_framework::core::util::lucene_test_case::new_searcher_with_reader_and_similarity(
      reader,
      std::sync::Arc::new(classic_similarity::new()),
    )?;

  writer.close(&mut random)?;

  let mut query = BooleanQueryBuilder::new();
  let common_search_term = "michael";
  let common_query =
    FuzzyQuery::with_max_edits_and_prefix(Term::from_text("field", common_search_term), 2, 1)?;
  query.add(common_query, Occur::Should)?;

  let rare_search_term = "cutting";
  let rare_query =
    FuzzyQuery::with_max_edits_and_prefix(Term::from_text("field", rare_search_term), 2, 1)?;
  query.add(rare_query, Occur::Should)?;
  let hits = searcher.search(query.build(), 1000)?.score_docs;

  assert_eq!(7, hits.len());
  let best_doc = searcher.stored_fields()?.document(hits[0].doc)?;
  let top_match = best_doc
    .get_field("field")
    .unwrap()
    .string_value()?
    .unwrap();
  assert!(top_match.as_ref().contains(rare_search_term));

  let runner_up_doc = searcher.stored_fields()?.document(hits[1].doc)?;
  let runner_up_match = runner_up_doc
    .get_field("field")
    .unwrap()
    .string_value()?
    .unwrap();
  assert!(runner_up_match.as_ref().contains("cuttin"));

  let worst_doc = searcher
    .stored_fields()?
    .document(hits[hits.len() - 1].doc)?;
  let worst_match = worst_doc
    .get_field("field")
    .unwrap()
    .string_value()?
    .unwrap();
  assert!(worst_match.as_ref().contains("micheal"));

  searcher.get_index_reader().close()?;
  directory.close()
}
#[test]
fn test_tie_breaker() -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;
  let writer = RandomIndexWriter::new(&mut random, directory.clone())?;
  let mut field_to_type = HashMap::new();
  add_doc(&mut random, "a123456", &writer, &mut field_to_type)?;
  add_doc(&mut random, "c123456", &writer, &mut field_to_type)?;
  add_doc(&mut random, "d123456", &writer, &mut field_to_type)?;
  add_doc(&mut random, "e123456", &writer, &mut field_to_type)?;

  let directory2 = new_directory_shared(&mut random)?;
  let writer2 = RandomIndexWriter::new(&mut random, directory2.clone())?;
  let mut field_to_type2 = HashMap::new();
  add_doc(&mut random, "a123456", &writer2, &mut field_to_type2)?;
  add_doc(&mut random, "b123456", &writer2, &mut field_to_type2)?;
  add_doc(&mut random, "b123456", &writer2, &mut field_to_type2)?;
  add_doc(&mut random, "b123456", &writer2, &mut field_to_type2)?;
  add_doc(&mut random, "c123456", &writer2, &mut field_to_type2)?;
  add_doc(&mut random, "f123456", &writer2, &mut field_to_type2)?;

  let ir1 = writer.get_reader(&mut random)?;
  let ir2 = writer2.get_reader(&mut random)?;

  let mr = MultiReader::new(vec![ir1, ir2])?;
  let searcher = new_searcher_with_reader(mr)?;
  let fq = FuzzyQuery::with_options(Term::from_text("field", "z123456"), 1, 0, 2, false)?;
  let docs = searcher.search(fq, 2)?;
  assert_eq!(5, docs.total_hits.value());
  searcher.get_index_reader().close()?;
  writer.close(&mut random)?;
  writer2.close(&mut random)?;
  directory.close()?;
  directory2.close()
}
/// Test the TopTermsBoostOnlyBooleanQueryRewrite rewrite method.
#[test]
fn test_boost_only_rewrite() -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;
  let writer = RandomIndexWriter::new(&mut random, directory.clone())?;
  let mut field_to_type = HashMap::new();
  add_doc(&mut random, "Lucene", &writer, &mut field_to_type)?;
  add_doc(&mut random, "Lucene", &writer, &mut field_to_type)?;
  add_doc(&mut random, "Lucenne", &writer, &mut field_to_type)?;

  let reader = writer.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;
  writer.close(&mut random)?;

  let query = FuzzyQuery::with_rewrite(
    Term::from_text("field", "lucene"),
    FuzzyQuery::DEFAULT_MAX_EDITS,
    FuzzyQuery::DEFAULT_PREFIX_LENGTH,
    FuzzyQuery::DEFAULT_MAX_EXPANSIONS,
    FuzzyQuery::DEFAULT_TRANSPOSITIONS,
    TopTermsBoostOnlyBooleanQueryRewrite::new(50),
  )?;
  let hits = searcher.search(query, 1000)?.score_docs;
  assert_eq!(3, hits.len());
  assert_eq!(
    "Lucene",
    searcher
      .stored_fields()?
      .document(hits[0].doc)?
      .get_field("field")
      .unwrap()
      .string_value()?
      .unwrap()
      .as_ref()
  );
  assert_eq!(
    "Lucene",
    searcher
      .stored_fields()?
      .document(hits[1].doc)?
      .get_field("field")
      .unwrap()
      .string_value()?
      .unwrap()
      .as_ref()
  );
  assert_eq!(
    "Lucenne",
    searcher
      .stored_fields()?
      .document(hits[2].doc)?
      .get_field("field")
      .unwrap()
      .string_value()?
      .unwrap()
      .as_ref()
  );

  searcher.get_index_reader().close()?;
  directory.close()
}
#[test]
fn test_giga() -> Result<()> {
  let mut random = random();
  let index = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, index.clone())?;
  let mut field_to_type = HashMap::new();

  add_doc(&mut random, "Lucene in Action", &w, &mut field_to_type)?;
  add_doc(&mut random, "Lucene for Dummies", &w, &mut field_to_type)?;

  add_doc(&mut random, "Giga byte", &w, &mut field_to_type)?;

  add_doc(
    &mut random,
    "ManagingGigabytesManagingGigabyte",
    &w,
    &mut field_to_type,
  )?;
  add_doc(
    &mut random,
    "ManagingGigabytesManagingGigabytes",
    &w,
    &mut field_to_type,
  )?;

  add_doc(
    &mut random,
    "The Art of Computer Science",
    &w,
    &mut field_to_type,
  )?;
  add_doc(&mut random, "J. K. Rowling", &w, &mut field_to_type)?;
  add_doc(&mut random, "JK Rowling", &w, &mut field_to_type)?;
  add_doc(&mut random, "Joanne K Roling", &w, &mut field_to_type)?;
  add_doc(&mut random, "Bruce Willis", &w, &mut field_to_type)?;
  add_doc(&mut random, "Willis bruce", &w, &mut field_to_type)?;
  add_doc(&mut random, "Brute willis", &w, &mut field_to_type)?;
  add_doc(&mut random, "B. willis", &w, &mut field_to_type)?;
  let r = w.get_reader(&mut random)?;
  w.close(&mut random)?;

  let q = FuzzyQuery::with_max_edits(Term::from_text("field", "giga"), 0)?;

  let searcher = new_searcher_with_reader(r)?;
  let hits = searcher.search(q, 10)?.score_docs;
  assert_eq!(1, hits.len());
  assert_eq!(
    "Giga byte",
    searcher
      .stored_fields()?
      .document(hits[0].doc)?
      .get_field("field")
      .unwrap()
      .string_value()?
      .unwrap()
      .as_ref()
  );
  searcher.get_index_reader().close()?;
  w.close(&mut random)?;
  index.close()
}
#[test]
fn test_distance_as_edits_searching() -> Result<()> {
  let mut random = random();
  let index = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, index.clone())?;
  let mut field_to_type = HashMap::new();
  add_doc(&mut random, "foobar", &w, &mut field_to_type)?;
  add_doc(&mut random, "test", &w, &mut field_to_type)?;
  add_doc(&mut random, "working", &w, &mut field_to_type)?;
  let reader = w.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;
  w.close(&mut random)?;

  let mut q = FuzzyQuery::with_max_edits(Term::from_text("field", "fouba"), 2)?;
  let mut hits = searcher.search(q, 10)?.score_docs;
  assert_eq!(1, hits.len());
  assert_eq!(
    "foobar",
    searcher
      .stored_fields()?
      .document(hits[0].doc)?
      .get_field("field")
      .unwrap()
      .string_value()?
      .unwrap()
      .as_ref()
  );

  q = FuzzyQuery::with_max_edits(Term::from_text("field", "foubara"), 2)?;
  hits = searcher.search(q, 10)?.score_docs;
  assert_eq!(1, hits.len());
  assert_eq!(
    "foobar",
    searcher
      .stored_fields()?
      .document(hits[0].doc)?
      .get_field("field")
      .unwrap()
      .string_value()?
      .unwrap()
      .as_ref()
  );

  let expected = FuzzyQuery::with_max_edits(Term::from_text("field", "t"), 3).unwrap_err();
  assert!(format!("{expected}").contains("maxEdits"));

  searcher.get_index_reader().close()?;
  index.close()
}
#[test]
fn test_validation() {
  let expected =
    FuzzyQuery::with_options(Term::from_text("field", "foo"), -1, 0, 1, false).unwrap_err();
  assert!(format!("{expected}").contains("maxEdits"));

  let expected = FuzzyQuery::with_options(
    Term::from_text("field", "foo"),
    LevenshteinAutomata::MAXIMUM_SUPPORTED_DISTANCE + 1,
    0,
    1,
    false,
  )
  .unwrap_err();
  assert!(format!("{expected}").contains("maxEdits must be between"));

  let expected =
    FuzzyQuery::with_options(Term::from_text("field", "foo"), 1, 0, 0, false).unwrap_err();
  assert!(format!("{expected}").contains("maxExpansions must be positive"));

  let expected =
    FuzzyQuery::with_options(Term::from_text("field", "foo"), 1, 0, 0, false).unwrap_err();
  assert!(format!("{expected}").contains("maxExpansions must be positive"));
}
fn add_doc<R>(
  random: &mut R,
  text: &str,
  writer: &RandomIndexWriter<DirEnum>,
  field_to_type: &mut HashMap<String, FieldType>,
) -> Result<()>
where
  R: Rng + ?Sized,
{
  let mut doc = Document::new();
  doc.add(new_text_field(
    random,
    "field",
    text,
    Store::Yes,
    field_to_type,
  )?);
  writer.add_document(random, doc)?;

  Ok(())
}
fn random_simple_string<R>(random: &mut R, digits: usize) -> String
where
  R: Rng + ?Sized,
{
  let term_length = TestUtil::next_usize(random, 1, 8);
  let mut chars = String::with_capacity(term_length);

  for _ in 0..term_length {
    let ch = (b'a' + random.random_range(0..digits) as u8) as char;
    chars.push(ch);
  }

  chars
}
#[test]
fn test_random() -> Result<()> {
  let mut random = random();
  let digits = TestUtil::next_usize(&mut random, 2, 3);
  let vocabulary_size = digits << 7;
  let num_terms = std::cmp::min(at_least_usize(&mut random, 100), vocabulary_size);
  let mut terms = HashSet::new();
  while terms.len() < num_terms {
    terms.insert(random_simple_string(&mut random, digits));
  }

  let dir = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, dir.clone())?;
  let mut field_to_type = HashMap::new();
  for term in &terms {
    let mut doc = Document::new();
    doc.add(new_string_field(
      &mut random,
      "field",
      term.as_str(),
      Store::Yes,
      &mut field_to_type,
    )?);
    w.add_document(&mut random, doc)?;
  }
  let r = w.get_reader(&mut random)?;
  w.close(&mut random)?;
  let s = new_searcher_with_reader(r)?;
  let iters = at_least(&mut random, 200);
  for _iter in 0..iters {
    let query_term = random_simple_string(&mut random, digits);
    let prefix_length = random.random_range(0..query_term.len());
    let query_prefix = &query_term[0..prefix_length];

    let mut expected: [Vec<TermAndScore<'_>>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for term in &terms {
      if !term.starts_with(query_prefix) {
        continue;
      }
      let mut ed = get_distance(term, &query_term);
      let score = 1.0 - ed as f32 / std::cmp::min(query_term.len(), term.len()) as f32;
      while ed < 3 {
        expected[ed].push(TermAndScore::new(term, score));
        ed += 1;
      }
    }
    #[allow(clippy::needless_range_loop)]
    for ed in 0..3 {
      expected[ed].sort();
      let queue_size = TestUtil::next_usize(&mut random, 1, terms.len());
      let query = FuzzyQuery::with_options(
        Term::from_text("field", query_term.as_str()),
        ed as i32,
        prefix_length,
        queue_size,
        true,
      )?;
      let hits = s.search(query, terms.len())?;
      let mut actual = HashSet::new();
      let mut stored_fields = s.stored_fields()?;
      for hit in hits.score_docs {
        let doc = stored_fields.document(hit.doc)?;
        actual.insert(
          doc
            .get_field("field")
            .unwrap()
            .string_value()?
            .unwrap()
            .as_ref()
            .to_string(),
        );
      }
      let mut expected_top = HashSet::new();
      let limit = std::cmp::min(queue_size, expected[ed].len());
      #[allow(clippy::needless_range_loop)]
      for i in 0..limit {
        expected_top.insert(expected[ed][i].term);
      }

      if actual.len() != expected_top.len()
        || !actual
          .iter()
          .all(|term| expected_top.contains(term.as_str()))
      {
        let mut sb = String::new();
        writeln!(
          sb,
          "FAILED: query={} ed={} queueSize={} vs expected match size={} prefixLength={}",
          query_term,
          ed,
          queue_size,
          expected[ed].len(),
          prefix_length
        )?;

        let mut first = true;
        for term in &actual {
          if !expected_top.contains(term.as_str()) {
            if first {
              sb.push_str("  these matched but shouldn't:\n");
              first = false;
            }
            writeln!(sb, "    {term}")?;
          }
        }
        first = true;
        for term in &expected_top {
          if !actual.contains(*term) {
            if first {
              sb.push_str("  these did not match but should:\n");
              first = false;
            }
            writeln!(sb, "    {term}")?;
          }
        }
        panic!("{sb}");
      }
    }
  }

  let close_result = s.get_index_reader().close();
  IOUtils::use_or_suppress_result(close_result, dir.close())
}

#[derive(Debug, Clone)]
struct TermAndScore<'a> {
  term: &'a str,
  score: f32,
}

impl<'a> TermAndScore<'a> {
  fn new(term: &'a str, score: f32) -> Self {
    Self { term, score }
  }
}

impl Eq for TermAndScore<'_> {}

impl PartialEq for TermAndScore<'_> {
  fn eq(&self, other: &Self) -> bool {
    self.term == other.term && CoreHelper::compare_f32(self.score, other.score).is_eq()
  }
}

impl Ord for TermAndScore<'_> {
  fn cmp(&self, other: &Self) -> Ordering {
    other
      .score
      .partial_cmp(&self.score)
      .unwrap_or(Ordering::Equal)
      .then_with(|| self.term.cmp(other.term))
  }
}

impl PartialOrd for TermAndScore<'_> {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

fn get_distance(target: &str, other: &str) -> usize {
  let target_points = to_ints_ref(target);
  let other_points = to_ints_ref(other);
  let n = target_points.len();
  let m = other_points.len();
  let mut d = vec![vec![0; m + 1]; n + 1];

  if n == 0 || m == 0 {
    if n == m {
      return 0;
    } else {
      return std::cmp::max(n, m);
    }
  }

  for (i, row) in d.iter_mut().enumerate().take(n + 1) {
    row[0] = i;
  }
  #[allow(clippy::needless_range_loop)]
  for j in 0..=m {
    d[0][j] = j;
  }

  for j in 1..=m {
    let t_j = other_points[j - 1];

    for i in 1..=n {
      let cost = if target_points[i - 1] == t_j { 0 } else { 1 };
      d[i][j] = std::cmp::min(
        std::cmp::min(d[i - 1][j] + 1, d[i][j - 1] + 1),
        d[i - 1][j - 1] + cost,
      );
      if i > 1
        && j > 1
        && target_points[i - 1] == other_points[j - 2]
        && target_points[i - 2] == other_points[j - 1]
      {
        d[i][j] = std::cmp::min(d[i][j], d[i - 2][j - 2] + cost);
      }
    }
  }

  d[n][m]
}

fn to_ints_ref(s: &str) -> Vec<i32> {
  let mut ref_ = Vec::with_capacity(s.len());
  for cp in s.chars() {
    ref_.push(cp as i32);
  }
  ref_
}
#[test]
fn test_visitor() -> Result<()> {
  struct Visitor<'a> {
    visited: &'a mut bool,
  }

  impl QueryVisitor for Visitor<'_> {
    type SubVisitor<'a>
      = DefaultQueryVisitor<'a, Self>
    where
      Self: 'a;

    fn consume_terms_matching<A>(
      &mut self,
      _query: QueryRef<'_>,
      _field: &str,
      automaton: A,
    ) -> Result<()>
    where
      A: Fn() -> Result<Option<ByteRunAutomaton>>,
    {
      *self.visited = true;
      let mut automaton = automaton()?.expect("FuzzyQuery must supply an automaton");
      assert_matches(&mut automaton, "blob")?;
      assert_matches(&mut automaton, "bolb")?;
      assert_matches(&mut automaton, "blobby")?;
      assert_no_matches(&mut automaton, "bolbby")?;
      Ok(())
    }

    fn get_sub_visitor<'a>(
      &'a mut self,
      occur: Occur,
      parent: QueryRef<'_>,
    ) -> Self::SubVisitor<'a> {
      self.default_get_sub_visitor(occur, parent)
    }
  }

  let query = FuzzyQuery::with_max_edits(Term::from_text("field", "blob"), 2)?;
  let mut visited = false;
  query.visit(&mut Visitor {
    visited: &mut visited,
  })?;
  assert!(visited);
  Ok(())
}

fn assert_matches(automaton: &mut ByteRunAutomaton, text: &str) -> Result<()> {
  let bytes: crate::core::index::BytesRef<Vec<u8>> =
    crate::core::index::BytesRef::from_string(text);
  assert!(automaton.run(&bytes.bytes, bytes.offset, bytes.length)?);
  Ok(())
}

fn assert_no_matches(automaton: &mut ByteRunAutomaton, text: &str) -> Result<()> {
  let bytes: crate::core::index::BytesRef<Vec<u8>> =
    crate::core::index::BytesRef::from_string(text);
  assert!(!automaton.run(&bytes.bytes, bytes.offset, bytes.length)?);
  Ok(())
}
