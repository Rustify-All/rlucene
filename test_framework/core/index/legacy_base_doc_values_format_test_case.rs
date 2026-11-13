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
use crate::core::document::binary_doc_values_field::BinaryDocValuesField;
use crate::core::document::document::Document;
use crate::core::document::field::Store;
use crate::core::document::fields::Fields;
use crate::core::document::float_doc_values_field::FloatDocValuesField;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::sorted_doc_values_field::SortedDocValuesField;
use crate::core::document::sorted_numeric_doc_values_field::SortedNumericDocValuesField;
use crate::core::document::sorted_set_doc_values_field::SortedSetDocValuesField;
use crate::core::document::stored_field::StoredField;
use crate::core::document::string_field::StringField;
use crate::core::index::BytesRef;
use crate::core::index::BytesRefValue;
use crate::core::index::binary_doc_values::BinaryDocValues;
use crate::core::index::check_index::CheckIndex;
use crate::core::index::directory_reader;
use crate::core::index::doc_values::DocValues;
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::indexable_field::IndexableField;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::multi_doc_values::{
  MultiBinaryDocValues, MultiDocValues, MultiNumericDocValues,
};
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::sorted_doc_values::SortedDocValues;
use crate::core::index::sorted_numeric_doc_values::SortedNumericDocValues;
use crate::core::index::sorted_set_doc_values::SortedSetDocValues;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term::Term;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::search::boolean_clause::Occur;
use crate::core::search::boolean_query::Builder as BooleanQueryBuilder;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::term_query::TermQuery;
use crate::core::store::directory::Directory;
use crate::core::util::IOUtils;
use crate::core::util::automation::compiled_automaton::CompiledAutomaton;
use crate::core::util::automation::operations::Operations;
use crate::core::util::automation::reg_exp::RegExp;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::base_index_file_format_test_case::{
  BaseIndexFileFormatTestCase, BaseIndexFileFormatTestCaseDefaults,
};
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, create_temp_dir, create_temp_dir_with_prefix, get_only_leaf_reader,
  new_bytes_ref_empty, new_bytes_ref_from_bytes, new_bytes_ref_from_string, new_directory_shared,
  new_fs_directory, new_index_writer_config_with_analyzer, new_log_merge_policy,
  new_searcher_with_reader, new_string_field, new_text_field, rarely, usually,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::prelude::{IndexedRandom, SliceRandom};
use rand::{Rng, RngExt};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Barrier};
use std::thread;

pub struct LegacyBaseDocValuesFormatTestCaseDefaults;

impl<T> BaseIndexFileFormatTestCaseDefaults<T> for LegacyBaseDocValuesFormatTestCaseDefaults
where
  T: LegacyBaseDocValuesFormatTestCase,
{
  fn add_random_fields<R>(_test_case: &T, random: &mut R, document: &mut Document) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    if usually(random) {
      document.add(NumericDocValuesField::new(
        "ndv",
        random.random_range(0..1 << 12),
      ));
      let value = TestUtil::random_simple_string(random);
      document.add(BinaryDocValuesField::new(
        "bdv",
        new_bytes_ref_from_string(random, &value)?,
      ));
      let value = TestUtil::random_simple_string_range(random, 0, 2);
      document.add(SortedDocValuesField::new(
        "sdv",
        new_bytes_ref_from_string(random, &value)?,
      ));
    }
    let num_values = random.random_range(0..5);
    for _ in 0..num_values {
      let value = TestUtil::random_simple_string_range(random, 0, 2);
      document.add(SortedSetDocValuesField::new(
        "ssdv",
        new_bytes_ref_from_string(random, &value)?,
      ));
    }
    let num_values = random.random_range(0..5);
    for _ in 0..num_values {
      document.add(SortedNumericDocValuesField::new(
        "sndv",
        TestUtil::next_long(random, i64::MIN, i64::MAX),
      ));
    }
    Ok(())
  }
}

pub trait LegacyBaseDocValuesFormatTestCase:
  BaseIndexFileFormatTestCase<Defaults = LegacyBaseDocValuesFormatTestCaseDefaults>
{
  fn test_one_number<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let iwriter = RandomIndexWriter::new(random, directory.clone())?;

    let mut doc = Document::new();
    let long_term = "longtermlongtermlongtermlongtermlongtermlongtermlongtermlongterm\
         longtermlongtermlongtermlongtermlongtermlongtermlongtermlongterm\
         longtermlongterm";
    let text = format!("This is the text to be indexed. {}", long_term);

    let mut field_to_type = HashMap::new();
    doc.add(new_text_field(
      random,
      "fieldname",
      &text,
      Store::Yes,
      &mut field_to_type,
    )?);
    doc.add(NumericDocValuesField::new("dv", 5));

    iwriter.add_document(random, doc)?;
    iwriter.close(random)?;

    let ireader =
      self.maybe_wrap_with_merging_reader(directory_reader::open(directory.clone())?)?;
    let isearcher = new_searcher_with_reader(ireader)?;
    let mut stored_fields = isearcher.stored_fields()?;

    assert_eq!(
      1,
      isearcher.count(TermQuery::new(Term::from_text("fieldname", long_term)))?
    );

    let query = TermQuery::new(Term::from_text("fieldname", "text"));
    let hits = isearcher.search(query, 1)?;
    assert_eq!(1, hits.total_hits.value());

    for i in 0..hits.score_docs.len() {
      let hit_doc = stored_fields.document(hits.score_docs[i].doc)?;
      assert_eq!(text.as_str(), hit_doc.get("fieldname")?.unwrap().as_ref());

      assert_eq!(1, isearcher.get_leaf_contexts()?.len());
      let leaf = &isearcher.get_leaf_contexts()?[0];
      let mut dv = leaf.reader().get_numeric_doc_values("dv")?.unwrap();

      let doc_id = hits.score_docs[i].doc;
      assert_eq!(doc_id, dv.advance(doc_id)?);
      assert_eq!(5, dv.long_value()?);
    }
    Ok(())
  }
  fn test_one_float<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let iwriter = RandomIndexWriter::new(random, directory.clone())?;

    let mut doc = Document::new();
    let long_term = "longtermlongtermlongtermlongtermlongtermlongtermlongtermlongterm\
         longtermlongtermlongtermlongtermlongtermlongtermlongtermlongterm\
         longtermlongterm";
    let text = format!("This is the text to be indexed. {}", long_term);

    let mut field_to_type = HashMap::new();
    doc.add(new_text_field(
      random,
      "fieldname",
      &text,
      Store::Yes,
      &mut field_to_type,
    )?);
    doc.add(FloatDocValuesField::new("dv", 5.7f32));

    iwriter.add_document(random, doc)?;
    iwriter.close(random)?;

    let ireader =
      self.maybe_wrap_with_merging_reader(directory_reader::open(directory.clone())?)?;
    let isearcher = new_searcher_with_reader(ireader)?;
    let mut stored_fields = isearcher.stored_fields()?;

    assert_eq!(
      1,
      isearcher.count(TermQuery::new(Term::from_text("fieldname", long_term)))?
    );

    let query = TermQuery::new(Term::from_text("fieldname", "text"));
    let hits = isearcher.search(query, 1)?;
    assert_eq!(1, hits.total_hits.value());

    for i in 0..hits.score_docs.len() {
      let doc_id = hits.score_docs[i].doc;
      let hit_doc = stored_fields.document(doc_id)?;
      assert_eq!(text.as_str(), hit_doc.get("fieldname")?.unwrap().as_ref());

      assert_eq!(1, isearcher.get_leaf_contexts()?.len());
      let leaf = &isearcher.get_leaf_contexts()?[0];
      let mut dv = leaf.reader().get_numeric_doc_values("dv")?.unwrap();

      assert_eq!(doc_id, dv.advance(doc_id)?);
      assert_eq!(5.7f32.to_bits() as i32 as i64, dv.long_value()?);
    }
    Ok(())
  }

  fn test_two_numbers<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let iwriter = RandomIndexWriter::new(random, directory.clone())?;

    let mut doc = Document::new();
    let long_term = "longtermlongtermlongtermlongtermlongtermlongtermlongtermlongterm\
         longtermlongtermlongtermlongtermlongtermlongtermlongtermlongterm\
         longtermlongterm";
    let text = format!("This is the text to be indexed. {}", long_term);

    let mut field_to_type = HashMap::new();
    doc.add(new_text_field(
      random,
      "fieldname",
      &text,
      Store::Yes,
      &mut field_to_type,
    )?);
    doc.add(NumericDocValuesField::new("dv1", 5));
    doc.add(NumericDocValuesField::new("dv2", 17));

    iwriter.add_document(random, doc)?;
    iwriter.close(random)?;

    let ireader =
      self.maybe_wrap_with_merging_reader(directory_reader::open(directory.clone())?)?;
    let isearcher = new_searcher_with_reader(ireader)?;
    let mut stored_fields = isearcher.stored_fields()?;

    assert_eq!(
      1,
      isearcher.count(TermQuery::new(Term::from_text("fieldname", long_term)))?
    );

    let query = TermQuery::new(Term::from_text("fieldname", "text"));
    let hits = isearcher.search(query, 1)?;
    assert_eq!(1, hits.total_hits.value());

    for i in 0..hits.score_docs.len() {
      let doc_id = hits.score_docs[i].doc;
      let hit_doc = stored_fields.document(doc_id)?;
      assert_eq!(text.as_str(), hit_doc.get("fieldname")?.unwrap().as_ref());

      assert_eq!(1, isearcher.get_leaf_contexts()?.len());
      let leaf = &isearcher.get_leaf_contexts()?[0];

      let mut dv = leaf.reader().get_numeric_doc_values("dv1")?.unwrap();
      assert_eq!(doc_id, dv.advance(doc_id)?);
      assert_eq!(5, dv.long_value()?);

      let mut dv = leaf.reader().get_numeric_doc_values("dv2")?.unwrap();
      assert_eq!(doc_id, dv.advance(doc_id)?);
      assert_eq!(17, dv.long_value()?);
    }
    Ok(())
  }

  fn test_two_binary_values<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let iwriter = RandomIndexWriter::new(random, directory.clone())?;

    let mut doc = Document::new();
    let long_term = "longtermlongtermlongtermlongtermlongtermlongtermlongtermlongterm\
         longtermlongtermlongtermlongtermlongtermlongtermlongtermlongterm\
         longtermlongterm";
    let text = format!("This is the text to be indexed. {}", long_term);

    let mut field_to_type = HashMap::new();
    doc.add(new_text_field(
      random,
      "fieldname",
      &text,
      Store::Yes,
      &mut field_to_type,
    )?);
    doc.add(BinaryDocValuesField::new(
      "dv1",
      new_bytes_ref_from_string(random, long_term)?,
    ));
    doc.add(BinaryDocValuesField::new(
      "dv2",
      new_bytes_ref_from_string(random, &text)?,
    ));

    iwriter.add_document(random, doc)?;
    iwriter.close(random)?;

    let ireader =
      self.maybe_wrap_with_merging_reader(directory_reader::open(directory.clone())?)?;
    let isearcher = new_searcher_with_reader(ireader)?;
    let mut stored_fields = isearcher.stored_fields()?;

    assert_eq!(
      1,
      isearcher.count(TermQuery::new(Term::from_text("fieldname", long_term)))?
    );

    let query = TermQuery::new(Term::from_text("fieldname", "text"));
    let hits = isearcher.search(query, 1)?;
    assert_eq!(1, hits.total_hits.value());

    for i in 0..hits.score_docs.len() {
      let hit_doc_id = hits.score_docs[i].doc;
      let hit_doc = stored_fields.document(hit_doc_id)?;
      assert_eq!(text.as_str(), hit_doc.get("fieldname")?.unwrap().as_ref());

      assert_eq!(1, isearcher.get_leaf_contexts()?.len());
      let leaf = &isearcher.get_leaf_contexts()?[0];

      let mut dv = leaf.reader().get_binary_doc_values("dv1")?.unwrap();
      assert_eq!(hit_doc_id, dv.advance(hit_doc_id)?);
      assert_eq!(
        &new_bytes_ref_from_string(random, long_term)?,
        &dv.binary_value()?.into_owned()
      );

      let mut dv = leaf.reader().get_binary_doc_values("dv2")?.unwrap();
      assert_eq!(hit_doc_id, dv.advance(hit_doc_id)?);
      assert_eq!(
        &new_bytes_ref_from_string(random, &text)?,
        &dv.binary_value()?.into_owned()
      );
    }
    isearcher.get_index_reader().close()?;
    directory.close()?;
    Ok(())
  }

  fn test_variously_compressible_binary_values<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let iwriter = RandomIndexWriter::new(random, directory.clone())?;
    let num_docs = 1 + random.random_range(0..100);

    let mut written_values: HashMap<i32, BytesRef<Vec<u8>>> = HashMap::new();

    let vocab_range = 1 + random.random_range(0..(u8::MAX as usize - 1));

    let mut field_to_type = HashMap::new();
    for i in 0..num_docs {
      let mut doc = Document::new();

      let mut value = vec![0u8; 500 + random.random_range(0..1024)];
      for b in &mut value {
        *b = random.random_range(0..vocab_range) as u8;
      }

      let bytes_ref = new_bytes_ref_from_bytes(random, value.as_ref())?;
      written_values.insert(i, bytes_ref.clone());

      doc.add(new_text_field(
        random,
        "id",
        i.to_string(),
        Store::Yes,
        &mut field_to_type,
      )?);
      doc.add(BinaryDocValuesField::new("dv1", bytes_ref));
      iwriter.add_document(random, doc)?;
    }
    iwriter.force_merge(random, 1)?;
    iwriter.close(random)?;

    let ireader =
      self.maybe_wrap_with_merging_reader(directory_reader::open(directory.clone())?)?;
    let isearcher = new_searcher_with_reader(ireader)?;
    let mut stored_fields = isearcher.stored_fields()?;

    for i in 0..num_docs {
      let id = i.to_string();
      let query = TermQuery::new(Term::from_text("id", &id));
      let hits = isearcher.search(query, 1)?;
      assert_eq!(1, hits.total_hits.value());

      let hit_doc_id = hits.score_docs[0].doc;
      let hit_doc = stored_fields.document(hit_doc_id)?;
      assert_eq!(id.as_str(), hit_doc.get("id")?.unwrap().as_ref());

      assert_eq!(1, isearcher.get_leaf_contexts()?.len());
      let leaf = &isearcher.get_leaf_contexts()?[0];
      let mut dv = leaf.reader().get_binary_doc_values("dv1")?.unwrap();
      assert_eq!(hit_doc_id, dv.advance(hit_doc_id)?);
      assert_eq!(
        written_values.get(&{ i }).unwrap(),
        &dv.binary_value()?.into_owned()
      );
    }
    Ok(())
  }

  fn test_two_fields_mixed<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let iwriter = RandomIndexWriter::new(random, directory.clone())?;

    let mut doc = Document::new();
    let long_term = "longtermlongtermlongtermlongtermlongtermlongtermlongtermlongterm\
         longtermlongtermlongtermlongtermlongtermlongtermlongtermlongterm\
         longtermlongterm";
    let text = format!("This is the text to be indexed. {}", long_term);

    let mut field_to_type = HashMap::new();
    doc.add(new_text_field(
      random,
      "fieldname",
      &text,
      Store::Yes,
      &mut field_to_type,
    )?);
    doc.add(NumericDocValuesField::new("dv1", 5));
    doc.add(BinaryDocValuesField::new(
      "dv2",
      new_bytes_ref_from_string(random, "hello world")?,
    ));

    iwriter.add_document(random, doc)?;
    iwriter.close(random)?;

    let ireader =
      self.maybe_wrap_with_merging_reader(directory_reader::open(directory.clone())?)?;
    let isearcher = new_searcher_with_reader(ireader)?;
    let mut stored_fields = isearcher.stored_fields()?;

    assert_eq!(
      1,
      isearcher.count(TermQuery::new(Term::from_text("fieldname", long_term)))?
    );

    let query = TermQuery::new(Term::from_text("fieldname", "text"));
    let hits = isearcher.search(query, 1)?;
    assert_eq!(1, hits.total_hits.value());

    for i in 0..hits.score_docs.len() {
      let doc_id = hits.score_docs[i].doc;
      let hit_doc = stored_fields.document(doc_id)?;
      assert_eq!(text.as_str(), hit_doc.get("fieldname")?.unwrap().as_ref());

      assert_eq!(1, isearcher.get_leaf_contexts()?.len());
      let leaf = &isearcher.get_leaf_contexts()?[0];

      let mut dv = leaf.reader().get_numeric_doc_values("dv1")?.unwrap();
      assert_eq!(doc_id, dv.advance(doc_id)?);
      assert_eq!(5, dv.long_value()?);

      let mut dv2 = leaf.reader().get_binary_doc_values("dv2")?.unwrap();
      assert_eq!(doc_id, dv2.advance(doc_id)?);
      assert_eq!(
        &new_bytes_ref_from_string(random, "hello world")?,
        &dv2.binary_value()?.into_owned()
      );
    }
    Ok(())
  }

  fn test_three_fields_mixed<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let iwriter = RandomIndexWriter::new(random, directory.clone())?;

    let mut doc = Document::new();
    let long_term = "longtermlongtermlongtermlongtermlongtermlongtermlongtermlongterm\
         longtermlongtermlongtermlongtermlongtermlongtermlongtermlongterm\
         longtermlongterm";
    let text = format!("This is the text to be indexed. {}", long_term);

    let mut field_to_type = HashMap::new();
    doc.add(new_text_field(
      random,
      "fieldname",
      &text,
      Store::Yes,
      &mut field_to_type,
    )?);
    doc.add(SortedDocValuesField::new(
      "dv1",
      new_bytes_ref_from_string(random, "hello hello")?,
    ));
    doc.add(NumericDocValuesField::new("dv2", 5));
    doc.add(BinaryDocValuesField::new(
      "dv3",
      new_bytes_ref_from_string(random, "hello world")?,
    ));

    iwriter.add_document(random, doc)?;
    iwriter.close(random)?;

    let ireader =
      self.maybe_wrap_with_merging_reader(directory_reader::open(directory.clone())?)?;
    let isearcher = new_searcher_with_reader(ireader)?;

    assert_eq!(
      1,
      isearcher.count(TermQuery::new(Term::from_text("fieldname", long_term)))?
    );
    let query = TermQuery::new(Term::from_text("fieldname", "text"));
    let hits = isearcher.search(query, 1)?;
    let mut stored_fields = isearcher.stored_fields()?;
    assert_eq!(1, hits.total_hits.value());

    for i in 0..hits.score_docs.len() {
      let doc_id = hits.score_docs[i].doc;
      let hit_doc = stored_fields.document(doc_id)?;
      assert_eq!(text.as_str(), hit_doc.get("fieldname")?.unwrap().as_ref());

      assert_eq!(1, isearcher.get_leaf_contexts()?.len());
      let leaf = &isearcher.get_leaf_contexts()?[0];

      let mut dv = leaf.reader().get_sorted_doc_values("dv1")?.unwrap();
      assert_eq!(doc_id, dv.advance(doc_id)?);
      let ord = dv.ord_value()?;
      assert_eq!(
        new_bytes_ref_from_string::<_, Vec<u8>>(random, "hello hello")?.as_bytes(),
        dv.lookup_ord(ord)?.as_bytes()
      );

      let mut dv2 = leaf.reader().get_numeric_doc_values("dv2")?.unwrap();
      assert_eq!(doc_id, dv2.advance(doc_id)?);
      assert_eq!(5, dv2.long_value()?);

      let mut dv3 = leaf.reader().get_binary_doc_values("dv3")?.unwrap();
      assert_eq!(doc_id, dv3.advance(doc_id)?);
      assert_eq!(
        &new_bytes_ref_from_string(random, "hello world")?,
        &dv3.binary_value()?.into_owned()
      );
    }
    Ok(())
  }

  fn test_three_fields_mixed2<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let iwriter = RandomIndexWriter::new(random, directory.clone())?;

    let mut doc = Document::new();
    let long_term = "longtermlongtermlongtermlongtermlongtermlongtermlongtermlongterm\
         longtermlongtermlongtermlongtermlongtermlongtermlongtermlongterm\
         longtermlongterm";
    let text = format!("This is the text to be indexed. {}", long_term);

    let mut field_to_type = HashMap::new();
    doc.add(new_text_field(
      random,
      "fieldname",
      &text,
      Store::Yes,
      &mut field_to_type,
    )?);
    doc.add(BinaryDocValuesField::new(
      "dv1",
      new_bytes_ref_from_string(random, "hello world")?,
    ));
    doc.add(SortedDocValuesField::new(
      "dv2",
      new_bytes_ref_from_string(random, "hello hello")?,
    ));
    doc.add(NumericDocValuesField::new("dv3", 5));

    iwriter.add_document(random, doc)?;
    iwriter.close(random)?;

    let ireader =
      self.maybe_wrap_with_merging_reader(directory_reader::open(directory.clone())?)?;
    let isearcher = new_searcher_with_reader(ireader)?;
    let mut stored_fields = isearcher.stored_fields()?;

    assert_eq!(
      1,
      isearcher.count(TermQuery::new(Term::from_text("fieldname", long_term)))?
    );
    let query = TermQuery::new(Term::from_text("fieldname", "text"));
    let hits = isearcher.search(query, 1)?;
    assert_eq!(1, hits.total_hits.value());

    for i in 0..hits.score_docs.len() {
      let doc_id = hits.score_docs[i].doc;
      let hit_doc = stored_fields.document(doc_id)?;
      assert_eq!(text.as_str(), hit_doc.get("fieldname")?.unwrap().as_ref());

      assert_eq!(1, isearcher.get_leaf_contexts()?.len());
      let leaf = &isearcher.get_leaf_contexts()?[0];

      let mut dv = leaf.reader().get_sorted_doc_values("dv2")?.unwrap();
      assert_eq!(doc_id, dv.advance(doc_id)?);
      let ord = dv.ord_value()?;
      assert_eq!(
        new_bytes_ref_from_string::<_, Vec<u8>>(random, "hello hello")?.as_bytes(),
        dv.lookup_ord(ord)?.as_bytes()
      );

      let mut dv2 = leaf.reader().get_numeric_doc_values("dv3")?.unwrap();
      assert_eq!(doc_id, dv2.advance(doc_id)?);
      assert_eq!(5, dv2.long_value()?);

      let mut dv3 = leaf.reader().get_binary_doc_values("dv1")?.unwrap();
      assert_eq!(doc_id, dv3.advance(doc_id)?);
      assert_eq!(
        &new_bytes_ref_from_string(random, "hello world")?,
        &dv3.binary_value()?.into_owned()
      );
    }
    Ok(())
  }

  fn test_two_documents_numeric<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let analyzer = MockAnalyzer::new(random);

    let directory = new_directory_shared(random)?;
    let mut conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    conf.set_merge_policy(new_log_merge_policy(random)?);
    let iwriter = RandomIndexWriter::with_config(random, directory.clone(), conf);

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("dv", 1));
    iwriter.add_document(random, doc)?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("dv", 2));
    iwriter.add_document(random, doc)?;
    iwriter.force_merge(random, 1)?;
    iwriter.close(random)?;

    let ireader =
      self.maybe_wrap_with_merging_reader(directory_reader::open(directory.clone())?)?;
    let top_reader_context = (&ireader).get_context()?;
    assert_eq!(1, top_reader_context.leaves()?.len());
    let leaves = top_reader_context.leaves()?;
    let mut dv = leaves[0].reader().get_numeric_doc_values("dv")?.unwrap();
    assert_eq!(0, dv.next_doc()?);
    assert_eq!(1, dv.long_value()?);
    assert_eq!(1, dv.next_doc()?);
    assert_eq!(2, dv.long_value()?);
    Ok(())
  }

  fn test_two_documents_merged<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let analyzer = MockAnalyzer::new(random);

    let directory = new_directory_shared(random)?;
    let mut conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    conf.set_merge_policy(new_log_merge_policy(random)?);
    let iwriter = RandomIndexWriter::with_config(random, directory.clone(), conf);

    let mut doc = Document::new();
    let mut field_to_type = HashMap::new();
    doc.add(new_string_field(
      random,
      "id",
      "0",
      Store::Yes,
      &mut field_to_type,
    )?);
    doc.add(NumericDocValuesField::new("dv", -10));
    iwriter.add_document(random, doc)?;
    iwriter.commit(random)?;

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "1",
      Store::Yes,
      &mut field_to_type,
    )?);
    doc.add(NumericDocValuesField::new("dv", 99));
    iwriter.add_document(random, doc)?;
    iwriter.force_merge(random, 1)?;
    iwriter.close(random)?;

    let ireader =
      self.maybe_wrap_with_merging_reader(directory_reader::open(directory.clone())?)?;
    let top_reader_context = (&ireader).get_context()?;
    assert_eq!(1, top_reader_context.leaves()?.len());
    let leaves = top_reader_context.leaves()?;
    let leaf = leaves[0].reader();
    let mut dv = leaf.get_numeric_doc_values("dv")?.unwrap();
    let mut stored_fields = leaf.stored_fields()?;

    for i in 0..2 {
      let doc2 = stored_fields.document(i)?;
      let expected = if doc2.get("id")?.unwrap().as_ref() == "0" {
        -10
      } else {
        99
      };
      assert_eq!(i, dv.next_doc()?);
      assert_eq!(expected, dv.long_value()?);
    }
    Ok(())
  }

  fn test_big_numeric_range<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let analyzer = MockAnalyzer::new(random);

    let directory = new_directory_shared(random)?;
    let mut conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    conf.set_merge_policy(new_log_merge_policy(random)?);
    let iwriter = RandomIndexWriter::with_config(random, directory.clone(), conf);

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("dv", i64::MIN));
    iwriter.add_document(random, doc)?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("dv", i64::MAX));
    iwriter.add_document(random, doc)?;
    iwriter.force_merge(random, 1)?;
    iwriter.close(random)?;

    let ireader =
      self.maybe_wrap_with_merging_reader(directory_reader::open(directory.clone())?)?;
    let top_reader_context = (&ireader).get_context()?;
    assert_eq!(1, top_reader_context.leaves()?.len());
    let leaves = top_reader_context.leaves()?;
    let mut dv = leaves[0].reader().get_numeric_doc_values("dv")?.unwrap();
    assert_eq!(0, dv.next_doc()?);
    assert_eq!(i64::MIN, dv.long_value()?);
    assert_eq!(1, dv.next_doc()?);
    assert_eq!(i64::MAX, dv.long_value()?);
    Ok(())
  }

  fn test_big_numeric_range2<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let analyzer = MockAnalyzer::new(random);

    let directory = new_directory_shared(random)?;
    let mut conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    conf.set_merge_policy(new_log_merge_policy(random)?);
    let iwriter = RandomIndexWriter::with_config(random, directory.clone(), conf);

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("dv", -8841491950446638677));
    iwriter.add_document(random, doc)?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("dv", 9062230939892376225));
    iwriter.add_document(random, doc)?;
    iwriter.force_merge(random, 1)?;
    iwriter.close(random)?;

    let ireader =
      self.maybe_wrap_with_merging_reader(directory_reader::open(directory.clone())?)?;
    let top_reader_context = (&ireader).get_context()?;
    assert_eq!(1, top_reader_context.leaves()?.len());
    let leaves = top_reader_context.leaves()?;
    let mut dv = leaves[0].reader().get_numeric_doc_values("dv")?.unwrap();
    assert_eq!(0, dv.next_doc()?);
    assert_eq!(-8841491950446638677, dv.long_value()?);
    assert_eq!(1, dv.next_doc()?);
    assert_eq!(9062230939892376225, dv.long_value()?);
    Ok(())
  }

  fn test_bytes<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let analyzer = MockAnalyzer::new(random);

    let directory = new_directory_shared(random)?;
    let conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    let iwriter = RandomIndexWriter::with_config(random, directory.clone(), conf);

    let mut doc = Document::new();
    let long_term = "longtermlongtermlongtermlongtermlongtermlongtermlongtermlongterm\
         longtermlongtermlongtermlongtermlongtermlongtermlongtermlongterm\
         longtermlongterm";
    let text = format!("This is the text to be indexed. {}", long_term);

    let mut field_to_type = HashMap::new();
    doc.add(new_text_field(
      random,
      "fieldname",
      &text,
      Store::Yes,
      &mut field_to_type,
    )?);
    doc.add(BinaryDocValuesField::new(
      "dv",
      new_bytes_ref_from_string(random, "hello world")?,
    ));
    iwriter.add_document(random, doc)?;
    iwriter.close(random)?;

    let ireader =
      self.maybe_wrap_with_merging_reader(directory_reader::open(directory.clone())?)?;
    let isearcher = new_searcher_with_reader(ireader)?;
    let mut stored_fields = isearcher.stored_fields()?;

    assert_eq!(
      1,
      isearcher.count(TermQuery::new(Term::from_text("fieldname", long_term)))?
    );
    let query = TermQuery::new(Term::from_text("fieldname", "text"));
    let hits = isearcher.search(query, 1)?;
    assert_eq!(1, hits.total_hits.value());

    for i in 0..hits.score_docs.len() {
      let hit_doc_id = hits.score_docs[i].doc;
      let hit_doc = stored_fields.document(hit_doc_id)?;
      assert_eq!(text.as_str(), hit_doc.get("fieldname")?.unwrap().as_ref());

      assert_eq!(1, isearcher.get_leaf_contexts()?.len());
      let leaf = &isearcher.get_leaf_contexts()?[0];
      let mut dv = leaf.reader().get_binary_doc_values("dv")?.unwrap();
      assert_eq!(hit_doc_id, dv.advance(hit_doc_id)?);
      assert_eq!(
        &new_bytes_ref_from_string(random, "hello world")?,
        &dv.binary_value()?.into_owned()
      );
    }
    Ok(())
  }

  fn test_bytes_two_documents_merged<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let analyzer = MockAnalyzer::new(random);

    let directory = new_directory_shared(random)?;
    let mut conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    conf.set_merge_policy(new_log_merge_policy(random)?);
    let iwriter = RandomIndexWriter::with_config(random, directory.clone(), conf);

    let mut doc = Document::new();
    let mut field_to_type = HashMap::new();
    doc.add(new_string_field(
      random,
      "id",
      "0",
      Store::Yes,
      &mut field_to_type,
    )?);
    doc.add(BinaryDocValuesField::new(
      "dv",
      new_bytes_ref_from_string(random, "hello world 1")?,
    ));
    iwriter.add_document(random, doc)?;
    iwriter.commit(random)?;

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "1",
      Store::Yes,
      &mut field_to_type,
    )?);
    doc.add(BinaryDocValuesField::new(
      "dv",
      new_bytes_ref_from_string(random, "hello 2")?,
    ));
    iwriter.add_document(random, doc)?;
    iwriter.force_merge(random, 1)?;
    iwriter.close(random)?;

    let ireader =
      self.maybe_wrap_with_merging_reader(directory_reader::open(directory.clone())?)?;
    let top_reader_context = (&ireader).get_context()?;
    assert_eq!(1, top_reader_context.leaves()?.len());
    let leaves = top_reader_context.leaves()?;
    let leaf = leaves[0].reader();
    let mut dv = leaf.get_binary_doc_values("dv")?.unwrap();
    let mut stored_fields = leaf.stored_fields()?;
    for i in 0..2 {
      let doc2 = stored_fields.document(i)?;
      let expected = if doc2.get("id")?.unwrap().as_ref() == "0" {
        "hello world 1"
      } else {
        "hello 2"
      };
      assert_eq!(i, dv.next_doc()?);
      assert_eq!(expected, dv.binary_value()?.utf8_to_string()?);
    }
    Ok(())
  }

  fn test_bytes_merge_away_all_values<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwconfig = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwconfig.set_merge_policy(new_log_merge_policy(random)?);
    let iwriter = RandomIndexWriter::with_config(random, directory.clone(), iwconfig);

    let mut doc = Document::new();
    let mut field_to_type = HashMap::new();
    doc.add(new_string_field(
      random,
      "id",
      "0",
      Store::No,
      &mut field_to_type,
    )?);
    iwriter.add_document(random, doc)?;

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "1",
      Store::No,
      &mut field_to_type,
    )?);
    doc.add(BinaryDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "hi")?,
    ));
    iwriter.add_document(random, doc)?;
    iwriter.commit(random)?;
    iwriter.delete_documents_with_terms(random, vec![Term::from_text("id", "1")])?;
    iwriter.force_merge(random, 1)?;

    let ireader = iwriter.get_reader(random)?;
    iwriter.close(random)?;

    let mut dv = get_only_leaf_reader(&ireader)?
      .get_binary_doc_values("field")?
      .unwrap();
    assert_eq!(NO_MORE_DOCS, dv.next_doc()?);
    Ok(())
  }

  fn test_sorted_bytes<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let analyzer = MockAnalyzer::new(random);

    let directory = new_directory_shared(random)?;
    let conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    let iwriter = RandomIndexWriter::with_config(random, directory.clone(), conf);

    let mut doc = Document::new();
    let long_term = "longtermlongtermlongtermlongtermlongtermlongtermlongtermlongterm\
         longtermlongtermlongtermlongtermlongtermlongtermlongtermlongterm\
         longtermlongterm";
    let text = format!("This is the text to be indexed. {}", long_term);
    let mut field_to_type = HashMap::new();
    doc.add(new_text_field(
      random,
      "fieldname",
      &text,
      Store::Yes,
      &mut field_to_type,
    )?);
    doc.add(SortedDocValuesField::new(
      "dv",
      new_bytes_ref_from_string(random, "hello world")?,
    ));
    iwriter.add_document(random, doc)?;
    iwriter.close(random)?;

    let ireader =
      self.maybe_wrap_with_merging_reader(directory_reader::open(directory.clone())?)?;
    let isearcher = new_searcher_with_reader(ireader)?;

    assert_eq!(
      1,
      isearcher.count(TermQuery::new(Term::from_text("fieldname", long_term)))?
    );
    let query = TermQuery::new(Term::from_text("fieldname", "text"));
    let hits = isearcher.search(query, 1)?;
    assert_eq!(1, hits.total_hits.value());
    let mut stored_fields = isearcher.stored_fields()?;
    for i in 0..hits.score_docs.len() {
      let doc_id = hits.score_docs[i].doc;
      let hit_doc = stored_fields.document(doc_id)?;
      assert_eq!(text.as_str(), hit_doc.get("fieldname")?.unwrap().as_ref());
      assert_eq!(1, isearcher.get_leaf_contexts()?.len());
      let leaf = &isearcher.get_leaf_contexts()?[0];
      let mut dv = leaf.reader().get_sorted_doc_values("dv")?.unwrap();
      assert_eq!(doc_id, dv.advance(doc_id)?);
      let ord = dv.ord_value()?;
      assert_eq!(
        new_bytes_ref_from_string::<_, Vec<u8>>(random, "hello world")?.as_bytes(),
        dv.lookup_ord(ord)?.as_bytes()
      );
    }
    isearcher.reader_context.reader().close()?;
    directory.close()?;
    Ok(())
  }

  fn test_sorted_bytes_two_documents<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let analyzer = MockAnalyzer::new(random);

    let directory = new_directory_shared(random)?;
    let mut conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    conf.set_merge_policy(new_log_merge_policy(random)?);
    let iwriter = RandomIndexWriter::with_config(random, directory.clone(), conf);

    let mut doc = Document::new();
    doc.add(SortedDocValuesField::new(
      "dv",
      new_bytes_ref_from_string(random, "hello world 1")?,
    ));
    iwriter.add_document(random, doc)?;

    let mut doc = Document::new();
    doc.add(SortedDocValuesField::new(
      "dv",
      new_bytes_ref_from_string(random, "hello world 2")?,
    ));
    iwriter.add_document(random, doc)?;
    iwriter.force_merge(random, 1)?;
    iwriter.close(random)?;

    let ireader =
      self.maybe_wrap_with_merging_reader(directory_reader::open(directory.clone())?)?;
    let top_reader_context = (&ireader).get_context()?;
    assert_eq!(1, top_reader_context.leaves()?.len());
    let leaves = top_reader_context.leaves()?;
    let mut dv = leaves[0].reader().get_sorted_doc_values("dv")?.unwrap();
    assert_eq!(0, dv.next_doc()?);
    let ord = dv.ord_value()?;
    assert_eq!("hello world 1", dv.lookup_ord(ord)?.utf8_to_string()?);
    assert_eq!(1, dv.next_doc()?);
    let ord = dv.ord_value()?;
    assert_eq!("hello world 2", dv.lookup_ord(ord)?.utf8_to_string()?);
    ireader.close()?;
    directory.close()?;
    Ok(())
  }

  fn test_sorted_bytes_three_documents<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let analyzer = MockAnalyzer::new(random);

    let directory = new_directory_shared(random)?;
    let mut conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    conf.set_merge_policy(new_log_merge_policy(random)?);
    let iwriter = RandomIndexWriter::with_config(random, directory.clone(), conf);

    let mut doc = Document::new();
    doc.add(SortedDocValuesField::new(
      "dv",
      new_bytes_ref_from_string(random, "hello world 1")?,
    ));
    iwriter.add_document(random, doc)?;

    let mut doc = Document::new();
    doc.add(SortedDocValuesField::new(
      "dv",
      new_bytes_ref_from_string(random, "hello world 2")?,
    ));
    iwriter.add_document(random, doc)?;

    let mut doc = Document::new();
    doc.add(SortedDocValuesField::new(
      "dv",
      new_bytes_ref_from_string(random, "hello world 1")?,
    ));
    iwriter.add_document(random, doc)?;
    iwriter.force_merge(random, 1)?;
    iwriter.close(random)?;

    let ireader =
      self.maybe_wrap_with_merging_reader(directory_reader::open(directory.clone())?)?;
    let top_reader_context = (&ireader).get_context()?;
    assert_eq!(1, top_reader_context.leaves()?.len());
    let leaves = top_reader_context.leaves()?;
    let mut dv = leaves[0].reader().get_sorted_doc_values("dv")?.unwrap();
    assert_eq!(2, dv.get_value_count()?);
    assert_eq!(0, dv.next_doc()?);
    assert_eq!(0, dv.ord_value()?);
    assert_eq!("hello world 1", dv.lookup_ord(0)?.utf8_to_string()?);
    assert_eq!(1, dv.next_doc()?);
    assert_eq!(1, dv.ord_value()?);
    assert_eq!("hello world 2", dv.lookup_ord(1)?.utf8_to_string()?);
    assert_eq!(2, dv.next_doc()?);
    assert_eq!(0, dv.ord_value()?);
    ireader.close()?;
    directory.close()?;
    Ok(())
  }

  fn test_sorted_bytes_two_documents_merged<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let analyzer = MockAnalyzer::new(random);

    let directory = new_directory_shared(random)?;
    let mut conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    conf.set_merge_policy(new_log_merge_policy(random)?);
    let iwriter = RandomIndexWriter::with_config(random, directory.clone(), conf);

    let mut doc = Document::new();
    let mut field_to_type = HashMap::new();
    doc.add(new_string_field(
      random,
      "id",
      "0",
      Store::Yes,
      &mut field_to_type,
    )?);
    doc.add(SortedDocValuesField::new(
      "dv",
      new_bytes_ref_from_string(random, "hello world 1")?,
    ));
    iwriter.add_document(random, doc)?;
    iwriter.commit(random)?;

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "1",
      Store::Yes,
      &mut field_to_type,
    )?);
    doc.add(SortedDocValuesField::new(
      "dv",
      new_bytes_ref_from_string(random, "hello world 2")?,
    ));
    iwriter.add_document(random, doc)?;
    iwriter.force_merge(random, 1)?;
    iwriter.close(random)?;

    let ireader =
      self.maybe_wrap_with_merging_reader(directory_reader::open(directory.clone())?)?;
    let top_reader_context = (&ireader).get_context()?;
    assert_eq!(1, top_reader_context.leaves()?.len());
    let leaves = top_reader_context.leaves()?;
    let leaf = leaves[0].reader();
    let mut dv = leaf.get_sorted_doc_values("dv")?.unwrap();
    assert_eq!(2, dv.get_value_count()?);
    assert_eq!(0, dv.next_doc()?);
    let ord = dv.ord_value()?;
    assert_eq!(
      new_bytes_ref_from_string::<_, Vec<u8>>(random, "hello world 1")?.as_bytes(),
      dv.lookup_ord(ord)?.as_bytes()
    );
    assert_eq!(
      new_bytes_ref_from_string::<_, Vec<u8>>(random, "hello world 2")?.as_bytes(),
      dv.lookup_ord(1)?.as_bytes()
    );
    let mut stored_fields = leaf.stored_fields()?;
    for i in 0..2 {
      let doc2 = stored_fields.document(i)?;
      let expected = if doc2.get("id")?.unwrap().as_ref() == "0" {
        "hello world 1"
      } else {
        "hello world 2"
      };
      if dv.doc_id() < i {
        assert_eq!(i, dv.next_doc()?);
      }
      let ord = dv.ord_value()?;
      assert_eq!(expected, dv.lookup_ord(ord)?.utf8_to_string()?);
    }
    ireader.close()?;
    directory.close()?;
    Ok(())
  }

  fn test_sorted_merge_away_all_values<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwconfig = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwconfig.set_merge_policy(new_log_merge_policy(random)?);
    let iwriter = RandomIndexWriter::with_config(random, directory.clone(), iwconfig);

    let mut doc = Document::new();
    let mut field_to_type = HashMap::new();
    doc.add(new_string_field(
      random,
      "id",
      "0",
      Store::No,
      &mut field_to_type,
    )?);
    iwriter.add_document(random, doc)?;

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "1",
      Store::No,
      &mut field_to_type,
    )?);
    doc.add(SortedDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "hello")?,
    ));
    iwriter.add_document(random, doc)?;
    iwriter.commit(random)?;
    iwriter.delete_documents_with_terms(random, vec![Term::from_text("id", "1")])?;
    iwriter.force_merge(random, 1)?;

    let ireader = iwriter.get_reader(random)?;
    iwriter.close(random)?;

    let mut dv = get_only_leaf_reader(&ireader)?
      .get_sorted_doc_values("field")?
      .unwrap();
    assert_eq!(NO_MORE_DOCS, dv.next_doc()?);

    let mut terms_enum = dv.terms_enum()?;
    let lucene = new_bytes_ref_from_string::<_, Vec<u8>>(random, "lucene")?;
    assert!(!terms_enum.seek_exact(&lucene)?);
    assert_eq!(SeekStatus::End, terms_enum.seek_ceil(&lucene)?);
    assert_eq!(-1, dv.lookup_term(&lucene)?);
    ireader.close()?;
    directory.close()?;
    Ok(())
  }

  fn test_bytes_with_newline<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let analyzer = MockAnalyzer::new(random);

    let directory = new_directory_shared(random)?;
    let mut conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    conf.set_merge_policy(new_log_merge_policy(random)?);
    let iwriter = RandomIndexWriter::with_config(random, directory.clone(), conf);

    let mut doc = Document::new();
    doc.add(BinaryDocValuesField::new(
      "dv",
      new_bytes_ref_from_string(random, "hello\nworld\r1")?,
    ));
    iwriter.add_document(random, doc)?;
    iwriter.close(random)?;

    let ireader =
      self.maybe_wrap_with_merging_reader(directory_reader::open(directory.clone())?)?;
    let top_reader_context = (&ireader).get_context()?;
    assert_eq!(1, top_reader_context.leaves()?.len());
    let leaves = top_reader_context.leaves()?;
    let mut dv = leaves[0].reader().get_binary_doc_values("dv")?.unwrap();
    assert_eq!(0, dv.next_doc()?);
    assert_eq!(
      &new_bytes_ref_from_string(random, "hello\nworld\r1")?,
      &dv.binary_value()?.into_owned()
    );
    Ok(())
  }

  fn test_missing_sorted_bytes<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let analyzer = MockAnalyzer::new(random);

    let directory = new_directory_shared(random)?;
    let mut conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    conf.set_merge_policy(new_log_merge_policy(random)?);
    let iwriter = RandomIndexWriter::with_config(random, directory.clone(), conf);

    let mut doc = Document::new();
    doc.add(SortedDocValuesField::new(
      "dv",
      new_bytes_ref_from_string(random, "hello world 2")?,
    ));
    iwriter.add_document(random, doc)?;
    iwriter.add_document(random, Document::new())?;
    iwriter.close(random)?;

    let ireader =
      self.maybe_wrap_with_merging_reader(directory_reader::open(directory.clone())?)?;
    let top_reader_context = (&ireader).get_context()?;
    assert_eq!(1, top_reader_context.leaves()?.len());
    let leaves = top_reader_context.leaves()?;
    let mut dv = leaves[0].reader().get_sorted_doc_values("dv")?.unwrap();
    assert_eq!(0, dv.next_doc()?);
    let ord = dv.ord_value()?;
    assert_eq!(
      new_bytes_ref_from_string::<_, Vec<u8>>(random, "hello world 2")?.as_bytes(),
      dv.lookup_ord(ord)?.as_bytes()
    );
    assert_eq!(NO_MORE_DOCS, dv.next_doc()?);
    ireader.close()?;
    directory.close()?;
    Ok(())
  }

  fn test_sorted_terms_enum<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwconfig = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwconfig.set_merge_policy(new_log_merge_policy(random)?);
    let iwriter = RandomIndexWriter::with_config(random, directory.clone(), iwconfig);

    let mut doc = Document::new();
    doc.add(SortedDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "hello")?,
    ));
    iwriter.add_document(random, doc)?;

    let mut doc = Document::new();
    doc.add(SortedDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "world")?,
    ));
    iwriter.add_document(random, doc)?;

    let mut doc = Document::new();
    doc.add(SortedDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "beer")?,
    ));
    iwriter.add_document(random, doc)?;
    iwriter.force_merge(random, 1)?;

    let ireader = iwriter.get_reader(random)?;
    iwriter.close(random)?;

    let mut dv = get_only_leaf_reader(&ireader)?
      .get_sorted_doc_values("field")?
      .unwrap();
    assert_eq!(3, dv.get_value_count()?);

    let mut terms_enum = dv.terms_enum()?;

    assert_eq!("beer", terms_enum.next()?.unwrap().utf8_to_string()?);
    assert_eq!(0, terms_enum.ord()?);
    assert_eq!("hello", terms_enum.next()?.unwrap().utf8_to_string()?);
    assert_eq!(1, terms_enum.ord()?);
    assert_eq!("world", terms_enum.next()?.unwrap().utf8_to_string()?);
    assert_eq!(2, terms_enum.ord()?);

    assert_eq!(
      SeekStatus::NotFound,
      terms_enum.seek_ceil(&new_bytes_ref_from_string::<_, Vec<u8>>(random, "ha!")?)?
    );
    assert_eq!("hello", terms_enum.term()?.utf8_to_string()?);
    assert_eq!(1, terms_enum.ord()?);
    assert_eq!(
      SeekStatus::Found,
      terms_enum.seek_ceil(&new_bytes_ref_from_string::<_, Vec<u8>>(random, "beer")?)?
    );
    assert_eq!("beer", terms_enum.term()?.utf8_to_string()?);
    assert_eq!(0, terms_enum.ord()?);
    assert_eq!(
      SeekStatus::End,
      terms_enum.seek_ceil(&new_bytes_ref_from_string::<_, Vec<u8>>(random, "zzz")?)?
    );
    assert_eq!(
      SeekStatus::NotFound,
      terms_enum.seek_ceil(&new_bytes_ref_from_string::<_, Vec<u8>>(random, "aba")?)?
    );
    assert_eq!(0, terms_enum.ord()?);

    assert!(terms_enum.seek_exact(&new_bytes_ref_from_string::<_, Vec<u8>>(random, "beer")?)?);
    assert_eq!("beer", terms_enum.term()?.utf8_to_string()?);
    assert_eq!(0, terms_enum.ord()?);
    assert!(terms_enum.seek_exact(&new_bytes_ref_from_string::<_, Vec<u8>>(random, "hello")?)?);
    assert_eq!("hello", terms_enum.term()?.utf8_to_string()?);
    assert_eq!(1, terms_enum.ord()?);
    assert!(terms_enum.seek_exact(&new_bytes_ref_from_string::<_, Vec<u8>>(random, "world")?)?);
    assert_eq!("world", terms_enum.term()?.utf8_to_string()?);
    assert_eq!(2, terms_enum.ord()?);
    assert!(!terms_enum.seek_exact(&new_bytes_ref_from_string::<_, Vec<u8>>(random, "bogus")?)?);

    terms_enum.seek_exact_with_ord(0)?;
    assert_eq!("beer", terms_enum.term()?.utf8_to_string()?);
    assert_eq!(0, terms_enum.ord()?);
    terms_enum.seek_exact_with_ord(1)?;
    assert_eq!("hello", terms_enum.term()?.utf8_to_string()?);
    assert_eq!(1, terms_enum.ord()?);
    terms_enum.seek_exact_with_ord(2)?;
    assert_eq!("world", terms_enum.term()?.utf8_to_string()?);
    assert_eq!(2, terms_enum.ord()?);

    // NORMAL automaton
    let automaton = Operations::determinize(
      &RegExp::from_string(".*l.*")?.to_automaton()?,
      Operations::DEFAULT_DETERMINIZE_WORK_LIMIT,
    )?
    .into_owned();
    let mut terms_enum = dv.intersect(&CompiledAutomaton::from_automaton(automaton)?)?;
    assert_eq!("hello", terms_enum.next()?.unwrap().utf8_to_string()?);
    assert_eq!(1, terms_enum.ord()?);
    assert_eq!("world", terms_enum.next()?.unwrap().utf8_to_string()?);
    assert_eq!(2, terms_enum.ord()?);
    assert!(terms_enum.next()?.is_none());

    // SINGLE automaton
    let automaton = RegExp::from_string("hello")?.to_automaton()?;
    let mut terms_enum = dv.intersect(&CompiledAutomaton::from_automaton(automaton)?)?;
    assert_eq!("hello", terms_enum.next()?.unwrap().utf8_to_string()?);
    assert_eq!(1, terms_enum.ord()?);
    assert!(terms_enum.next()?.is_none());

    ireader.close()?;
    directory.close()?;
    Ok(())
  }

  fn test_empty_sorted_bytes<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let analyzer = MockAnalyzer::new(random);

    let directory = new_directory_shared(random)?;
    let mut conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    conf.set_merge_policy(new_log_merge_policy(random)?);
    let iwriter = RandomIndexWriter::with_config(random, directory.clone(), conf);

    let mut doc = Document::new();
    doc.add(SortedDocValuesField::new(
      "dv",
      new_bytes_ref_from_string(random, "")?,
    ));
    iwriter.add_document(random, doc)?;

    let mut doc = Document::new();
    doc.add(SortedDocValuesField::new(
      "dv",
      new_bytes_ref_from_string(random, "")?,
    ));
    iwriter.add_document(random, doc)?;
    iwriter.force_merge(random, 1)?;
    iwriter.close(random)?;

    let ireader =
      self.maybe_wrap_with_merging_reader(directory_reader::open(directory.clone())?)?;
    let top_reader_context = (&ireader).get_context()?;
    assert_eq!(1, top_reader_context.leaves()?.len());
    let leaves = top_reader_context.leaves()?;
    let mut dv = leaves[0].reader().get_sorted_doc_values("dv")?.unwrap();
    assert_eq!(0, dv.next_doc()?);
    assert_eq!(0, dv.ord_value()?);
    assert_eq!(1, dv.next_doc()?);
    assert_eq!(0, dv.ord_value()?);
    assert_eq!("", dv.lookup_ord(0)?.utf8_to_string()?);
    ireader.close()?;
    directory.close()?;
    Ok(())
  }

  fn test_empty_bytes<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let analyzer = MockAnalyzer::new(random);

    let directory = new_directory_shared(random)?;
    let mut conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    conf.set_merge_policy(new_log_merge_policy(random)?);
    let iwriter = RandomIndexWriter::with_config(random, directory.clone(), conf);

    let mut doc = Document::new();
    doc.add(BinaryDocValuesField::new(
      "dv",
      new_bytes_ref_from_string(random, "")?,
    ));
    iwriter.add_document(random, doc)?;

    let mut doc = Document::new();
    doc.add(BinaryDocValuesField::new(
      "dv",
      new_bytes_ref_from_string(random, "")?,
    ));
    iwriter.add_document(random, doc)?;
    iwriter.force_merge(random, 1)?;
    iwriter.close(random)?;

    let ireader =
      self.maybe_wrap_with_merging_reader(directory_reader::open(directory.clone())?)?;
    let top_reader_context = (&ireader).get_context()?;
    assert_eq!(1, top_reader_context.leaves()?.len());
    let leaves = top_reader_context.leaves()?;
    let mut dv = leaves[0].reader().get_binary_doc_values("dv")?.unwrap();
    assert_eq!(0, dv.next_doc()?);
    assert_eq!("", dv.binary_value()?.utf8_to_string()?);
    assert_eq!(1, dv.next_doc()?);
    assert_eq!("", dv.binary_value()?.utf8_to_string()?);
    Ok(())
  }

  fn test_very_large_but_legal_bytes<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let analyzer = MockAnalyzer::new(random);

    let directory = new_directory_shared(random)?;
    let mut conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    conf.set_merge_policy(new_log_merge_policy(random)?);
    let iwriter = RandomIndexWriter::with_config(random, directory.clone(), conf);

    let mut doc = Document::new();
    let mut bytes = vec![0u8; 32766];
    random.fill_bytes(&mut bytes);
    let b = new_bytes_ref_from_bytes(random, bytes.as_ref())?;
    doc.add(BinaryDocValuesField::new("dv", b.clone()));
    iwriter.add_document(random, doc)?;
    iwriter.close(random)?;

    let ireader =
      self.maybe_wrap_with_merging_reader(directory_reader::open(directory.clone())?)?;
    let top_reader_context = (&ireader).get_context()?;
    assert_eq!(1, top_reader_context.leaves()?.len());
    let leaves = top_reader_context.leaves()?;
    let mut dv = leaves[0].reader().get_binary_doc_values("dv")?.unwrap();
    assert_eq!(0, dv.next_doc()?);
    assert_eq!(&b, &dv.binary_value()?.into_owned());
    Ok(())
  }

  fn test_very_large_but_legal_sorted_bytes<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let analyzer = MockAnalyzer::new(random);

    let directory = new_directory_shared(random)?;
    let mut conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    conf.set_merge_policy(new_log_merge_policy(random)?);
    let iwriter = RandomIndexWriter::with_config(random, directory.clone(), conf);

    let mut doc = Document::new();
    let mut bytes = vec![0u8; 32766];
    random.fill_bytes(&mut bytes);
    let b = new_bytes_ref_from_bytes(random, bytes.as_ref())?;
    doc.add(SortedDocValuesField::new("dv", b.clone()));
    iwriter.add_document(random, doc)?;
    iwriter.close(random)?;

    let ireader =
      self.maybe_wrap_with_merging_reader(directory_reader::open(directory.clone())?)?;
    let top_reader_context = (&ireader).get_context()?;
    assert_eq!(1, top_reader_context.leaves()?.len());
    let leaves = top_reader_context.leaves()?;
    let mut dv = leaves[0].reader().get_sorted_doc_values("dv")?.unwrap();
    assert_eq!(0, dv.next_doc()?);
    let ord = dv.ord_value()?;
    assert_eq!(b.as_bytes(), dv.lookup_ord(ord)?.as_bytes());
    ireader.close()?;
    directory.close()?;
    Ok(())
  }

  fn test_codec_uses_own_bytes<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let analyzer = MockAnalyzer::new(random);

    let directory = new_directory_shared(random)?;
    let mut conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    conf.set_merge_policy(new_log_merge_policy(random)?);
    let iwriter = RandomIndexWriter::with_config(random, directory.clone(), conf);

    let mut doc = Document::new();
    doc.add(BinaryDocValuesField::new(
      "dv",
      new_bytes_ref_from_string(random, "boo!")?,
    ));
    iwriter.add_document(random, doc)?;
    iwriter.close(random)?;

    let ireader =
      self.maybe_wrap_with_merging_reader(directory_reader::open(directory.clone())?)?;
    let top_reader_context = (&ireader).get_context()?;
    assert_eq!(1, top_reader_context.leaves()?.len());
    let leaves = top_reader_context.leaves()?;
    let mut dv = leaves[0].reader().get_binary_doc_values("dv")?.unwrap();
    assert_eq!(0, dv.next_doc()?);
    assert_eq!("boo!", dv.binary_value()?.utf8_to_string()?);
    Ok(())
  }

  fn test_codec_uses_own_sorted_bytes<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let analyzer = MockAnalyzer::new(random);

    let directory = new_directory_shared(random)?;
    let mut conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    conf.set_merge_policy(new_log_merge_policy(random)?);
    let iwriter = RandomIndexWriter::with_config(random, directory.clone(), conf);

    let mut doc = Document::new();
    doc.add(SortedDocValuesField::new(
      "dv",
      new_bytes_ref_from_string(random, "boo!")?,
    ));
    iwriter.add_document(random, doc)?;
    iwriter.close(random)?;

    let ireader =
      self.maybe_wrap_with_merging_reader(directory_reader::open(directory.clone())?)?;
    let top_reader_context = (&ireader).get_context()?;
    assert_eq!(1, top_reader_context.leaves()?.len());
    let leaves = top_reader_context.leaves()?;
    let mut dv = leaves[0].reader().get_sorted_doc_values("dv")?.unwrap();
    assert_eq!(0, dv.next_doc()?);
    let ord = dv.ord_value()?;
    assert_eq!("boo!", dv.lookup_ord(ord)?.utf8_to_string()?);
    ireader.close()?;
    directory.close()?;
    Ok(())
  }

  fn test_doc_values_simple<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    conf.set_merge_policy(new_log_merge_policy(random)?);
    let writer = IndexWriter::new(dir.clone(), conf)?;

    let mut field_to_type = HashMap::new();
    for i in 0..5 {
      let mut doc = Document::new();
      doc.add(NumericDocValuesField::new("docId", i as i64));
      doc.add(new_text_field(
        random,
        "docId",
        i.to_string(),
        Store::No,
        &mut field_to_type,
      )?);
      writer.add_document(doc)?;
    }
    writer.commit()?;
    writer.force_merge_with_wait(1, true)?;
    writer.close()?;

    let reader = self.maybe_wrap_with_merging_reader(directory_reader::open(dir.clone())?)?;
    let top_reader_context = (&reader).get_context()?;
    assert_eq!(1, top_reader_context.leaves()?.len());

    let searcher = new_searcher_with_reader(reader)?;
    let mut query = BooleanQueryBuilder::new();
    query.add(TermQuery::new(Term::from_text("docId", "0")), Occur::Should)?;
    query.add(TermQuery::new(Term::from_text("docId", "1")), Occur::Should)?;
    query.add(TermQuery::new(Term::from_text("docId", "2")), Occur::Should)?;
    query.add(TermQuery::new(Term::from_text("docId", "3")), Occur::Should)?;
    query.add(TermQuery::new(Term::from_text("docId", "4")), Occur::Should)?;

    let search = searcher.search(query.build(), 10)?;
    assert_eq!(5, search.total_hits.value());
    let mut doc_values = get_only_leaf_reader(searcher.reader_context.reader())?
      .get_numeric_doc_values("docId")?
      .unwrap();
    for (i, score_doc) in search.score_docs.iter().enumerate() {
      let doc_id = i as i32;
      assert_eq!(doc_id, score_doc.doc);
      assert_eq!(doc_id, doc_values.advance(doc_id)?);
      assert_eq!(i as i64, doc_values.long_value()?);
    }
    Ok(())
  }

  fn test_random_sorted_bytes<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let cfg = new_index_writer_config_with_analyzer(random, analyzer)?;
    let writer = RandomIndexWriter::with_config(random, dir.clone(), cfg);
    let num_docs = at_least(random, 100);
    let mut doc_to_string = HashMap::new();
    let mut all_values = Vec::with_capacity(num_docs as usize);
    let max_length = TestUtil::next_usize(random, 1, 50);
    let mut field_to_type = HashMap::new();

    for i in 0..num_docs {
      let mut doc = Document::new();
      let id = i.to_string();
      doc.add(new_text_field(
        random,
        "id",
        &id,
        Store::Yes,
        &mut field_to_type,
      )?);
      let string = TestUtil::random_realistic_unicode_string_range(random, 1, max_length);
      let br = new_bytes_ref_from_string(random, &string)?;
      doc.add(SortedDocValuesField::new("field", br));
      all_values.push(string.clone());
      doc_to_string.insert(id, string);
      writer.add_document(random, doc)?;
    }

    if rarely(random) {
      writer.commit(random)?;
    }

    let num_docs_no_value = at_least(random, 10);
    for _ in 0..num_docs_no_value {
      let mut doc = Document::new();
      doc.add(new_text_field(
        random,
        "id",
        "noValue",
        Store::Yes,
        &mut field_to_type,
      )?);
      writer.add_document(random, doc)?;
    }

    if rarely(random) {
      writer.commit(random)?;
    }

    for i in 0..num_docs {
      let mut doc = Document::new();
      let id = (i + num_docs).to_string();
      doc.add(new_text_field(
        random,
        "id",
        &id,
        Store::Yes,
        &mut field_to_type,
      )?);
      let string = TestUtil::random_realistic_unicode_string_range(random, 1, max_length);
      let br = new_bytes_ref_from_string(random, &string)?;
      doc.add(SortedDocValuesField::new("field", br));
      all_values.push(string.clone());
      doc_to_string.insert(id, string);
      writer.add_document(random, doc)?;
    }

    writer.commit(random)?;

    let reader = writer.get_reader(random)?;
    let mut doc_values = MultiDocValues::get_sorted_values(&reader, "field")?.unwrap();
    all_values.sort_by(|a, b| TestUtil::string_codepoint_comparator(a, b));
    all_values.dedup();

    assert_eq!(all_values.len() as i32, doc_values.get_value_count()?);
    for (i, expected) in all_values.iter().enumerate() {
      let ord = i as i32;
      let actual = doc_values.lookup_ord(ord)?;
      assert_eq!(expected.as_str(), actual.utf8_to_string()?);
      let expected_ref = new_bytes_ref_from_string::<_, Vec<u8>>(random, expected)?;
      assert_eq!(ord, doc_values.lookup_term(&expected_ref)?);
    }

    let searcher = new_searcher_with_reader(reader)?;
    for (id, expected) in &doc_to_string {
      let hits = searcher.search(TermQuery::new(Term::from_text("id", id)), 2)?;
      assert_eq!(1, hits.total_hits.value());
      let doc_id = hits.score_docs[0].doc;
      let mut doc_values =
        MultiDocValues::get_sorted_values(searcher.reader_context.reader(), "field")?.unwrap();
      assert_eq!(doc_id, doc_values.advance(doc_id)?);
      let ord = doc_values.ord_value()?;
      let actual = doc_values.lookup_ord(ord)?;
      assert_eq!(expected.as_str(), actual.utf8_to_string()?);
    }

    searcher.reader_context.reader().close()?;
    writer.close(random)?;
    dir.close()?;
    Ok(())
  }

  fn do_test_numerics_vs_stored_fields<R, F>(
    &self,
    random: &mut R,
    density: f64,
    longs: &mut F,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
    F: FnMut(&mut R) -> i64,
  {
    self.do_test_numerics_vs_stored_fields_with_min_docs(random, density, longs, 256)
  }

  fn do_test_numerics_vs_stored_fields_with_min_docs<R, F>(
    &self,
    random: &mut R,
    density: f64,
    longs: &mut F,
    min_docs: i32,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
    F: FnMut(&mut R) -> i64,
  {
    let dir = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    let writer = RandomIndexWriter::with_config(random, dir.clone(), conf);
    let num_docs = at_least(random, (min_docs as f64 * 1.172) as i32);
    assert!(num_docs > 256);
    let mut field_to_type = HashMap::new();

    for i in 0..num_docs {
      if random.random::<f64>() > density {
        writer.add_document(random, Document::new())?;
        continue;
      }

      let mut doc = Document::new();
      doc.add(new_string_field(
        random,
        "id",
        i.to_string(),
        Store::No,
        &mut field_to_type,
      )?);
      let value = longs(random);
      doc.add(StoredField::from_string("stored", value.to_string())?);
      doc.add(NumericDocValuesField::new("dv", value));
      writer.add_document(random, doc)?;
      if random.random_range(0..31) == 0 {
        writer.commit(random)?;
      }
    }

    let num_deletions = random.random_range(0..=(num_docs / 10).max(0));
    for _ in 0..num_deletions {
      let id = random.random_range(0..num_docs);
      writer.delete_documents_with_terms(random, vec![Term::from_text("id", id.to_string())])?;
    }

    writer.force_merge(random, (num_docs / std::cmp::max(256, min_docs)).max(1))?;
    writer.close(random)?;

    self.assert_dv_iterate(dir.clone())
  }

  fn assert_dv_iterate<D>(&self, dir: Arc<D>) -> Result<()>
  where
    D: Directory + 'static,
  {
    let reader = self.maybe_wrap_with_merging_reader(directory_reader::open(dir)?)?;
    TestUtil::check_reader(&reader)?;
    let context = (&reader).get_context()?;

    for leaf in context.leaves()? {
      let leaf_reader = leaf.reader();
      let mut doc_values = DocValues::get_numeric(leaf_reader, "dv")?;
      doc_values.next_doc()?;
      let mut stored_fields = leaf_reader.stored_fields()?;
      let max_doc = leaf_reader.max_doc()?;

      for i in 0..max_doc {
        let stored_doc = stored_fields.document(i)?;
        let stored_value = stored_doc.get("stored")?;
        if let Some(stored_value) = stored_value {
          assert_eq!(i, doc_values.doc_id());
          assert_eq!(
            stored_value.parse::<i64>().unwrap(),
            doc_values.long_value()?
          );
          doc_values.next_doc()?;
        } else {
          assert!(doc_values.doc_id() > i);
        }
      }

      assert_eq!(NO_MORE_DOCS, doc_values.doc_id());
    }

    Ok(())
  }

  fn compare_stored_field_with_sorted_numerics_dv<R, CR>(
    &self,
    random: &mut R,
    directory_reader: &CR,
    stored_field: &str,
    dv_field: &str,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
    CR: crate::core::index::composite_reader::CompositeReader,
  {
    let context = directory_reader.get_context()?;
    for leaf in context.leaves()? {
      let reader = leaf.reader();
      let mut stored_fields = reader.stored_fields()?;
      let max_doc = reader.max_doc()?;
      let mut doc_values = match reader.get_sorted_numeric_doc_values(dv_field)? {
        Some(values) => values,
        None => {
          for doc in 0..max_doc {
            assert!(
              stored_fields
                .document(doc)?
                .get_values(stored_field)?
                .is_empty()
            );
          }
          continue;
        },
      };

      for doc in 0..max_doc {
        let stored_doc = stored_fields.document(doc)?;
        let stored_values = stored_doc.get_values(stored_field)?;

        if stored_values.is_empty() {
          assert!(!doc_values.advance_exact(doc)?);
          continue;
        }

        match random.random_range(0..3) {
          0 => assert_eq!(doc, doc_values.next_doc()?),
          1 => assert_eq!(doc, doc_values.advance(doc)?),
          _ => assert!(doc_values.advance_exact(doc)?),
        }

        assert_eq!(doc, doc_values.doc_id());
        let repeats = 1 + random.random_range(0..3);
        for r in 0..repeats {
          if r > 0 || random.random_bool(0.5) {
            assert!(doc_values.advance_exact(doc)?);
          }
          let count = doc_values.doc_value_count()? as usize;
          assert_eq!(stored_values.len(), count);
          for expected in stored_values.iter().take(count) {
            assert_eq!(expected.as_ref(), doc_values.next_value()?.to_string());
          }
        }
      }

      let iters = 1 + random.random_range(0..3);
      for _ in 0..iters {
        let mut doc_values = reader.get_sorted_numeric_doc_values(dv_field)?.unwrap();
        let mut doc = random.random_range(0..max_doc);
        while doc < max_doc {
          let stored_doc = stored_fields.document(doc)?;
          let stored_values = stored_doc.get_values(stored_field)?;

          if doc_values.advance_exact(doc)? {
            assert_eq!(doc, doc_values.doc_id());
            let repeats = 1 + random.random_range(0..3);
            for r in 0..repeats {
              if r > 0 || random.random_bool(0.5) {
                assert!(doc_values.advance_exact(doc)?);
              }
              let count = doc_values.doc_value_count()? as usize;
              assert_eq!(stored_values.len(), count);
              for expected in stored_values.iter().take(count) {
                assert_eq!(expected.as_ref(), doc_values.next_value()?.to_string());
              }
            }
          } else {
            assert!(stored_values.is_empty());
          }

          doc += 1 + random.random_range(0..5);
        }
      }

      for _ in 0..iters {
        let mut doc_values = reader.get_sorted_numeric_doc_values(dv_field)?.unwrap();
        let mut doc = random.random_range(0..max_doc);
        while doc != NO_MORE_DOCS {
          let next_doc = doc_values.advance(doc)?;
          for d in doc..if next_doc == NO_MORE_DOCS {
            max_doc
          } else {
            next_doc
          } {
            assert!(
              stored_fields
                .document(d)?
                .get_values(stored_field)?
                .is_empty()
            );
          }

          doc = next_doc;
          if doc != NO_MORE_DOCS {
            let stored_doc = stored_fields.document(doc)?;
            let stored_values = stored_doc.get_values(stored_field)?;
            let repeats = 1 + random.random_range(0..3);
            for r in 0..repeats {
              if r > 0 || random.random_bool(0.5) {
                assert!(doc_values.advance_exact(doc)?);
              }
              let count = doc_values.doc_value_count()? as usize;
              assert_eq!(stored_values.len(), count);
              for expected in stored_values.iter().take(count) {
                assert_eq!(expected.as_ref(), doc_values.next_value()?.to_string());
              }
            }

            let step = 1 + random.random_range(0..5);
            doc = if next_doc >= max_doc - step {
              NO_MORE_DOCS
            } else {
              next_doc + step
            };
          }
        }
      }
    }

    Ok(())
  }

  fn do_test_sorted_numerics_vs_stored_fields<R, FC, FV>(
    &self,
    random: &mut R,
    counts: &mut FC,
    values: &mut FV,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
    FC: FnMut(&mut R) -> usize,
    FV: FnMut(&mut R) -> i64,
  {
    let dir = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    let writer = RandomIndexWriter::with_config(random, dir.clone(), conf);
    let num_docs = at_least(random, 300);
    assert!(num_docs > 256);
    let mut field_to_type = HashMap::new();

    for i in 0..num_docs {
      let mut doc = Document::new();
      doc.add(new_string_field(
        random,
        "id",
        i.to_string(),
        Store::No,
        &mut field_to_type,
      )?);

      let value_count = counts(random);
      let mut value_array = Vec::with_capacity(value_count);
      for _ in 0..value_count {
        let value = values(random);
        value_array.push(value);
        doc.add(SortedNumericDocValuesField::new("dv", value));
      }

      value_array.sort();
      for value in value_array {
        doc.add(StoredField::from_string("stored", value.to_string())?);
      }

      writer.add_document(random, doc)?;
      if random.random_range(0..31) == 0 {
        writer.commit(random)?;
      }
    }

    let num_deletions = random.random_range(0..=(num_docs / 10).max(0));
    for _ in 0..num_deletions {
      let id = random.random_range(0..num_docs);
      writer.delete_documents_with_terms(random, vec![Term::from_text("id", id.to_string())])?;
    }

    writer.commit(random)?;
    {
      let reader = self.maybe_wrap_with_merging_reader(directory_reader::open(dir.clone())?)?;
      TestUtil::check_reader(&reader)?;
      self.compare_stored_field_with_sorted_numerics_dv(random, &reader, "stored", "dv")?;
    }

    writer.force_merge(random, (num_docs / 256).max(1))?;
    writer.commit(random)?;

    {
      let reader = self.maybe_wrap_with_merging_reader(directory_reader::open(dir.clone())?)?;
      TestUtil::check_reader(&reader)?;
      self.compare_stored_field_with_sorted_numerics_dv(random, &reader, "stored", "dv")?;
    }

    writer.close(random)?;
    Ok(())
  }

  fn test_boolean_numerics_vs_stored_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_iterations = at_least(random, 1);
    for _ in 0..num_iterations {
      self.do_test_numerics_vs_stored_fields(random, 1.0, &mut |r| r.random_range(0..2) as i64)?;
    }
    Ok(())
  }

  fn test_sparse_boolean_numerics_vs_stored_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_iterations = at_least(random, 1);
    for _ in 0..num_iterations {
      let density = random.random::<f64>();
      self
        .do_test_numerics_vs_stored_fields(random, density, &mut |r| r.random_range(0..2) as i64)?;
    }
    Ok(())
  }

  fn test_byte_numerics_vs_stored_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_iterations = at_least(random, 1);
    for _ in 0..num_iterations {
      self.do_test_numerics_vs_stored_fields(random, 1.0, &mut |r| {
        TestUtil::next_int(r, i8::MIN as i32, i8::MAX as i32) as i64
      })?;
    }
    Ok(())
  }

  fn test_sparse_byte_numerics_vs_stored_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_iterations = at_least(random, 1);
    for _ in 0..num_iterations {
      let density = random.random::<f64>();
      self.do_test_numerics_vs_stored_fields(random, density, &mut |r| {
        TestUtil::next_int(r, i8::MIN as i32, i8::MAX as i32) as i64
      })?;
    }
    Ok(())
  }

  fn test_short_numerics_vs_stored_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_iterations = at_least(random, 1);
    for _ in 0..num_iterations {
      self.do_test_numerics_vs_stored_fields(random, 1.0, &mut |r| {
        TestUtil::next_int(r, i16::MIN as i32, i16::MAX as i32) as i64
      })?;
    }
    Ok(())
  }

  fn test_sparse_short_numerics_vs_stored_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_iterations = at_least(random, 1);
    for _ in 0..num_iterations {
      let density = random.random::<f64>();
      self.do_test_numerics_vs_stored_fields(random, density, &mut |r| {
        TestUtil::next_int(r, i16::MIN as i32, i16::MAX as i32) as i64
      })?;
    }
    Ok(())
  }

  fn test_int_numerics_vs_stored_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_iterations = at_least(random, 1);
    for _ in 0..num_iterations {
      self.do_test_numerics_vs_stored_fields(random, 1.0, &mut |r| r.random::<i32>() as i64)?;
    }
    Ok(())
  }

  fn test_sparse_int_numerics_vs_stored_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_iterations = at_least(random, 1);
    for _ in 0..num_iterations {
      let density = random.random::<f64>();
      self.do_test_numerics_vs_stored_fields(random, density, &mut |r| r.random::<i32>() as i64)?;
    }
    Ok(())
  }

  fn test_long_numerics_vs_stored_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_iterations = at_least(random, 1);
    for _ in 0..num_iterations {
      self.do_test_numerics_vs_stored_fields(random, 1.0, &mut |r| r.random::<i64>())?;
    }
    Ok(())
  }

  fn test_sparse_long_numerics_vs_stored_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_iterations = at_least(random, 1);
    for _ in 0..num_iterations {
      let density = random.random::<f64>();
      self.do_test_numerics_vs_stored_fields(random, density, &mut |r| r.random::<i64>())?;
    }
    Ok(())
  }

  fn do_test_binary_vs_stored_fields<R, F>(
    &self,
    random: &mut R,
    density: f64,
    bytes: &mut F,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
    F: FnMut(&mut R) -> Vec<u8>,
  {
    let dir = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    let writer = RandomIndexWriter::with_config(random, dir.clone(), conf);
    let num_docs = at_least(random, 300);
    let mut field_to_type = HashMap::new();

    let mut doc = Document::new();
    for i in 0..num_docs {
      if random.random::<f64>() > density {
        writer.add_document(random, Document::new())?;
        continue;
      }

      doc.clear();
      doc.add(new_string_field(
        random,
        "id",
        i.to_string(),
        Store::No,
        &mut field_to_type,
      )?);
      let buffer = bytes(random);
      let stored_value = buffer.clone();
      let dv_value = buffer.len();
      doc.add(StoredField::from_binary("stored", stored_value)?);
      doc.add(BinaryDocValuesField::new(
        "dv",
        BytesRef::from_slice(buffer, 0, dv_value),
      ));
      writer.add_document(random, &mut doc)?;
      if random.random_range(0..31) == 0 {
        writer.commit(random)?;
      }
    }

    let num_deletions = random.random_range(0..=(num_docs / 10).max(0));
    for _ in 0..num_deletions {
      let id = random.random_range(0..num_docs);
      writer.delete_documents_with_terms(random, vec![Term::from_text("id", id.to_string())])?;
    }

    let reader = writer.get_reader(random)?;
    TestUtil::check_reader(&reader)?;
    let context = (&reader).get_context()?;
    for leaf in context.leaves()? {
      let leaf_reader = leaf.reader();
      let mut stored_fields = leaf_reader.stored_fields()?;
      let mut doc_values = DocValues::get_binary(leaf_reader, "dv")?;
      doc_values.next_doc()?;
      let max_doc = leaf_reader.max_doc()?;
      for i in 0..max_doc {
        let stored_doc = stored_fields.document(i)?;
        let binary_value = stored_doc.get_binary_value("stored")?;
        if let Some(binary_value) = binary_value {
          assert_eq!(i, doc_values.doc_id());
          assert_eq!(
            binary_value.as_bytes_ref().as_byte_slice(),
            doc_values.binary_value()?.as_bytes()
          );
          doc_values.next_doc()?;
        } else {
          assert!(doc_values.doc_id() > i);
        }
      }
      assert_eq!(NO_MORE_DOCS, doc_values.doc_id());
    }

    reader.close()?;
    writer.force_merge(random, 1)?;

    let reader = writer.get_reader(random)?;
    TestUtil::check_reader(&reader)?;
    let context = (&reader).get_context()?;
    for leaf in context.leaves()? {
      let leaf_reader = leaf.reader();
      let mut stored_fields = leaf_reader.stored_fields()?;
      let mut doc_values = DocValues::get_binary(leaf_reader, "dv")?;
      doc_values.next_doc()?;
      let max_doc = leaf_reader.max_doc()?;
      for i in 0..max_doc {
        let stored_doc = stored_fields.document(i)?;
        let binary_value = stored_doc.get_binary_value("stored")?;
        if let Some(binary_value) = binary_value {
          assert_eq!(i, doc_values.doc_id());
          assert_eq!(
            binary_value.as_bytes_ref().as_byte_slice(),
            doc_values.binary_value()?.as_bytes()
          );
          doc_values.next_doc()?;
        } else {
          assert!(doc_values.doc_id() > i);
        }
      }
      assert_eq!(NO_MORE_DOCS, doc_values.doc_id());
    }

    reader.close()?;
    writer.close(random)?;
    dir.close()?;
    Ok(())
  }

  fn test_binary_fixed_length_vs_stored_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    self.do_test_binary_fixed_length_vs_stored_fields(random, 1.0)
  }

  fn test_sparse_binary_fixed_length_vs_stored_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let density = random.random::<f64>();
    self.do_test_binary_fixed_length_vs_stored_fields(random, density)
  }

  fn do_test_binary_fixed_length_vs_stored_fields<R>(
    &self,
    random: &mut R,
    density: f64,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_iterations = at_least(random, 1);
    for _ in 0..num_iterations {
      let fixed_length = TestUtil::next_usize(random, 0, 10);
      self.do_test_binary_vs_stored_fields(random, density, &mut |r| {
        let mut buffer = vec![0u8; fixed_length];
        r.fill_bytes(&mut buffer);
        buffer
      })?;
    }
    Ok(())
  }

  fn test_binary_variable_length_vs_stored_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    self.do_test_binary_variable_length_vs_stored_fields(random, 1.0)
  }

  fn test_sparse_binary_variable_length_vs_stored_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let density = random.random::<f64>();
    self.do_test_binary_variable_length_vs_stored_fields(random, density)
  }

  fn do_test_binary_variable_length_vs_stored_fields<R>(
    &self,
    random: &mut R,
    density: f64,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_iterations = at_least(random, 1);
    for _ in 0..num_iterations {
      self.do_test_binary_vs_stored_fields(random, density, &mut |r| {
        let length = r.random_range(0..10);
        let mut buffer = vec![0u8; length];
        r.fill_bytes(&mut buffer);
        buffer
      })?;
    }
    Ok(())
  }

  fn do_test_sorted_vs_stored_fields_bytes<R, F>(
    &self,
    random: &mut R,
    num_docs: i32,
    density: f64,
    bytes: &mut F,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
    F: FnMut(&mut R) -> Vec<u8>,
  {
    let dir = new_fs_directory(random, create_temp_dir_with_prefix("dvduel")?)?;
    let analyzer = MockAnalyzer::new(random);
    let conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    let writer = RandomIndexWriter::with_config(random, dir.clone(), conf);
    let mut field_to_type = HashMap::new();

    for i in 0..num_docs {
      if random.random::<f64>() > density {
        writer.add_document(random, Document::new())?;
        continue;
      }

      let mut doc = Document::new();
      doc.add(new_string_field(
        random,
        "id",
        i.to_string(),
        Store::No,
        &mut field_to_type,
      )?);
      let buffer = bytes(random);
      let stored_value = buffer.clone();
      let dv_len = buffer.len();
      doc.add(StoredField::from_binary("stored", stored_value)?);
      doc.add(SortedDocValuesField::new(
        "dv",
        BytesRef::from_slice(buffer, 0, dv_len),
      ));
      writer.add_document(random, doc)?;
      if random.random_range(0..31) == 0 {
        writer.commit(random)?;
      }
    }

    let num_deletions = random.random_range(0..=(num_docs / 10).max(0));
    for _ in 0..num_deletions {
      let id = random.random_range(0..num_docs);
      writer.delete_documents_with_terms(random, vec![Term::from_text("id", id.to_string())])?;
    }

    let reader = writer.get_reader(random)?;
    TestUtil::check_reader(&reader)?;
    let context = (&reader).get_context()?;
    for leaf in context.leaves()? {
      let leaf_reader = leaf.reader();
      let mut stored_fields = leaf_reader.stored_fields()?;
      let mut doc_values = DocValues::get_sorted(leaf_reader, "dv")?;
      doc_values.next_doc()?;
      let max_doc = leaf_reader.max_doc()?;
      for i in 0..max_doc {
        let stored_doc = stored_fields.document(i)?;
        let binary_value = stored_doc.get_binary_value("stored")?;
        if let Some(binary_value) = binary_value {
          assert_eq!(i, doc_values.doc_id());
          let ord = doc_values.ord_value()?;
          assert_eq!(
            binary_value.as_bytes(),
            doc_values.lookup_ord(ord)?.as_bytes()
          );
          doc_values.next_doc()?;
        } else {
          assert!(doc_values.doc_id() > i);
        }
      }
      assert_eq!(NO_MORE_DOCS, doc_values.doc_id());
    }

    reader.close()?;
    writer.force_merge(random, 1)?;

    let reader = writer.get_reader(random)?;
    TestUtil::check_reader(&reader)?;
    let context = (&reader).get_context()?;
    for leaf in context.leaves()? {
      let leaf_reader = leaf.reader();
      let mut stored_fields = leaf_reader.stored_fields()?;
      let mut doc_values = DocValues::get_sorted(leaf_reader, "dv")?;
      doc_values.next_doc()?;
      let max_doc = leaf_reader.max_doc()?;
      for i in 0..max_doc {
        let stored_doc = stored_fields.document(i)?;
        let binary_value = stored_doc.get_binary_value("stored")?;
        if let Some(binary_value) = binary_value {
          assert_eq!(i, doc_values.doc_id());
          let ord = doc_values.ord_value()?;
          assert_eq!(
            binary_value.as_bytes(),
            doc_values.lookup_ord(ord)?.as_bytes()
          );
          doc_values.next_doc()?;
        } else {
          assert!(doc_values.doc_id() > i);
        }
      }
      assert_eq!(NO_MORE_DOCS, doc_values.doc_id());
    }

    reader.close()?;
    writer.close(random)?;
    dir.close()?;
    Ok(())
  }

  fn test_sorted_fixed_length_vs_stored_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_iterations = at_least(random, 1);
    for _ in 0..num_iterations {
      let fixed_length = TestUtil::next_usize(random, 1, 10);
      let num_docs = at_least(random, 300);
      self.do_test_sorted_vs_stored_fields(random, num_docs, 1.0, fixed_length, fixed_length)?;
    }
    Ok(())
  }

  fn test_sparse_sorted_fixed_length_vs_stored_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_iterations = at_least(random, 1);
    for _ in 0..num_iterations {
      let fixed_length = TestUtil::next_usize(random, 1, 10);
      let density = random.random::<f64>();
      let num_docs = at_least(random, 300);
      self.do_test_sorted_vs_stored_fields(
        random,
        num_docs,
        density,
        fixed_length,
        fixed_length,
      )?;
    }
    Ok(())
  }

  fn test_sorted_variable_length_vs_stored_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_iterations = at_least(random, 1);
    for _ in 0..num_iterations {
      let num_docs = at_least(random, 300);
      self.do_test_sorted_vs_stored_fields(random, num_docs, 1.0, 1, 10)?;
    }
    Ok(())
  }

  fn test_sparse_sorted_variable_length_vs_stored_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_iterations = at_least(random, 1);
    for _ in 0..num_iterations {
      let density = random.random::<f64>();
      let num_docs = at_least(random, 300);
      self.do_test_sorted_vs_stored_fields(random, num_docs, density, 1, 10)?;
    }
    Ok(())
  }
  fn do_test_sorted_vs_stored_fields<R>(
    &self,
    random: &mut R,
    num_docs: i32,
    density: f64,
    min_length: usize,
    max_length: usize,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    self.do_test_sorted_vs_stored_fields_bytes(random, num_docs, density, &mut |r| {
      let length = TestUtil::next_usize(r, min_length, max_length);
      let mut buffer = vec![0u8; length];
      r.fill_bytes(&mut buffer);
      buffer
    })
  }

  fn test_sorted_set_one_value<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let writer = RandomIndexWriter::new(random, directory.clone())?;

    let mut doc = Document::new();
    doc.add(SortedSetDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "hello")?,
    ));
    writer.add_document(random, doc)?;

    let reader = writer.get_reader(random)?;
    writer.close(random)?;

    let mut dv = get_only_leaf_reader(&reader)?
      .get_sorted_set_doc_values("field")?
      .unwrap();
    assert_eq!(0, dv.next_doc()?);
    assert_eq!(1, dv.doc_value_count()?);
    assert_eq!(0, dv.next_ord()?);
    assert_eq!(
      new_bytes_ref_from_string::<_, Vec<u8>>(random, "hello")?.as_bytes(),
      dv.lookup_ord(0)?.as_bytes()
    );
    Ok(())
  }

  fn test_sorted_set_two_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let writer = RandomIndexWriter::new(random, directory.clone())?;

    let mut doc = Document::new();
    doc.add(SortedSetDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "hello")?,
    ));
    doc.add(SortedSetDocValuesField::new(
      "field2",
      new_bytes_ref_from_string(random, "world")?,
    ));
    writer.add_document(random, doc)?;

    let reader = writer.get_reader(random)?;
    writer.close(random)?;

    let leaf = get_only_leaf_reader(&reader)?;
    let mut dv = leaf.get_sorted_set_doc_values("field")?.unwrap();
    assert_eq!(0, dv.next_doc()?);
    assert_eq!(1, dv.doc_value_count()?);
    assert_eq!(0, dv.next_ord()?);
    assert_eq!(
      new_bytes_ref_from_string::<_, Vec<u8>>(random, "hello")?.as_bytes(),
      dv.lookup_ord(0)?.as_bytes()
    );

    let mut dv = leaf.get_sorted_set_doc_values("field2")?.unwrap();
    assert_eq!(0, dv.next_doc()?);
    assert_eq!(1, dv.doc_value_count()?);
    assert_eq!(0, dv.next_ord()?);
    assert_eq!(
      new_bytes_ref_from_string::<_, Vec<u8>>(random, "world")?.as_bytes(),
      dv.lookup_ord(0)?.as_bytes()
    );
    Ok(())
  }

  fn test_sorted_set_two_documents_merged<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwconfig = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwconfig.set_merge_policy(new_log_merge_policy(random)?);
    let writer = RandomIndexWriter::with_config(random, directory.clone(), iwconfig);

    let mut doc = Document::new();
    doc.add(SortedSetDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "hello")?,
    ));
    writer.add_document(random, doc)?;
    writer.commit(random)?;

    let mut doc = Document::new();
    doc.add(SortedSetDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "world")?,
    ));
    writer.add_document(random, doc)?;
    writer.force_merge(random, 1)?;

    let reader = writer.get_reader(random)?;
    writer.close(random)?;

    let mut dv = get_only_leaf_reader(&reader)?
      .get_sorted_set_doc_values("field")?
      .unwrap();
    assert_eq!(2, dv.get_value_count()?);

    assert_eq!(0, dv.next_doc()?);
    assert_eq!(1, dv.doc_value_count()?);
    assert_eq!(0, dv.next_ord()?);
    assert_eq!(
      new_bytes_ref_from_string::<_, Vec<u8>>(random, "hello")?.as_bytes(),
      dv.lookup_ord(0)?.as_bytes()
    );

    assert_eq!(1, dv.next_doc()?);
    assert_eq!(1, dv.doc_value_count()?);
    assert_eq!(1, dv.next_ord()?);
    assert_eq!(
      new_bytes_ref_from_string::<_, Vec<u8>>(random, "world")?.as_bytes(),
      dv.lookup_ord(1)?.as_bytes()
    );
    Ok(())
  }

  fn test_sorted_set_two_values<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let writer = RandomIndexWriter::new(random, directory.clone())?;

    let mut doc = Document::new();
    doc.add(SortedSetDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "hello")?,
    ));
    doc.add(SortedSetDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "world")?,
    ));
    writer.add_document(random, doc)?;

    let reader = writer.get_reader(random)?;
    writer.close(random)?;

    let mut dv = get_only_leaf_reader(&reader)?
      .get_sorted_set_doc_values("field")?
      .unwrap();
    assert_eq!(0, dv.next_doc()?);
    assert_eq!(2, dv.doc_value_count()?);
    assert_eq!(0, dv.next_ord()?);
    assert_eq!(1, dv.next_ord()?);
    assert_eq!(
      new_bytes_ref_from_string::<_, Vec<u8>>(random, "hello")?.as_bytes(),
      dv.lookup_ord(0)?.as_bytes()
    );
    assert_eq!(
      new_bytes_ref_from_string::<_, Vec<u8>>(random, "world")?.as_bytes(),
      dv.lookup_ord(1)?.as_bytes()
    );
    Ok(())
  }

  fn test_sorted_set_two_values_unordered<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let writer = RandomIndexWriter::new(random, directory.clone())?;

    let mut doc = Document::new();
    doc.add(SortedSetDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "world")?,
    ));
    doc.add(SortedSetDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "hello")?,
    ));
    writer.add_document(random, doc)?;

    let reader = writer.get_reader(random)?;
    writer.close(random)?;

    let mut dv = get_only_leaf_reader(&reader)?
      .get_sorted_set_doc_values("field")?
      .unwrap();
    assert_eq!(0, dv.next_doc()?);
    assert_eq!(2, dv.doc_value_count()?);
    assert_eq!(0, dv.next_ord()?);
    assert_eq!(1, dv.next_ord()?);
    assert_eq!(
      new_bytes_ref_from_string::<_, Vec<u8>>(random, "hello")?.as_bytes(),
      dv.lookup_ord(0)?.as_bytes()
    );
    assert_eq!(
      new_bytes_ref_from_string::<_, Vec<u8>>(random, "world")?.as_bytes(),
      dv.lookup_ord(1)?.as_bytes()
    );
    Ok(())
  }

  fn test_sorted_set_three_values_two_docs<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwconfig = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwconfig.set_merge_policy(new_log_merge_policy(random)?);
    let writer = RandomIndexWriter::with_config(random, directory.clone(), iwconfig);

    let mut doc = Document::new();
    doc.add(SortedSetDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "hello")?,
    ));
    doc.add(SortedSetDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "world")?,
    ));
    writer.add_document(random, doc)?;
    writer.commit(random)?;

    let mut doc = Document::new();
    doc.add(SortedSetDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "hello")?,
    ));
    doc.add(SortedSetDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "beer")?,
    ));
    writer.add_document(random, doc)?;
    writer.force_merge(random, 1)?;

    let reader = writer.get_reader(random)?;
    writer.close(random)?;

    let mut dv = get_only_leaf_reader(&reader)?
      .get_sorted_set_doc_values("field")?
      .unwrap();
    assert_eq!(3, dv.get_value_count()?);
    assert_eq!(0, dv.next_doc()?);
    assert_eq!(2, dv.doc_value_count()?);
    assert_eq!(1, dv.next_ord()?);
    assert_eq!(2, dv.next_ord()?);
    assert_eq!(1, dv.next_doc()?);
    assert_eq!(2, dv.doc_value_count()?);
    assert_eq!(0, dv.next_ord()?);
    assert_eq!(1, dv.next_ord()?);
    assert_eq!(
      new_bytes_ref_from_string::<_, Vec<u8>>(random, "beer")?.as_bytes(),
      dv.lookup_ord(0)?.as_bytes()
    );
    assert_eq!(
      new_bytes_ref_from_string::<_, Vec<u8>>(random, "hello")?.as_bytes(),
      dv.lookup_ord(1)?.as_bytes()
    );
    assert_eq!(
      new_bytes_ref_from_string::<_, Vec<u8>>(random, "world")?.as_bytes(),
      dv.lookup_ord(2)?.as_bytes()
    );
    Ok(())
  }

  fn test_sorted_set_two_documents_last_missing<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwconfig = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwconfig.set_merge_policy(new_log_merge_policy(random)?);
    let writer = RandomIndexWriter::with_config(random, directory.clone(), iwconfig);

    let mut doc = Document::new();
    doc.add(SortedSetDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "hello")?,
    ));
    writer.add_document(random, doc)?;
    writer.add_document(random, Document::new())?;
    writer.force_merge(random, 1)?;

    let reader = writer.get_reader(random)?;
    writer.close(random)?;

    let mut dv = get_only_leaf_reader(&reader)?
      .get_sorted_set_doc_values("field")?
      .unwrap();
    assert_eq!(1, dv.get_value_count()?);
    assert_eq!(0, dv.next_doc()?);
    assert_eq!(1, dv.doc_value_count()?);
    assert_eq!(0, dv.next_ord()?);
    assert_eq!(
      new_bytes_ref_from_string::<_, Vec<u8>>(random, "hello")?.as_bytes(),
      dv.lookup_ord(0)?.as_bytes()
    );
    Ok(())
  }

  fn test_sorted_set_two_documents_last_missing_merge<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwconfig = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwconfig.set_merge_policy(new_log_merge_policy(random)?);
    let writer = RandomIndexWriter::with_config(random, directory.clone(), iwconfig);

    let mut doc = Document::new();
    doc.add(SortedSetDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "hello")?,
    ));
    writer.add_document(random, doc)?;
    writer.commit(random)?;
    writer.add_document(random, Document::new())?;
    writer.force_merge(random, 1)?;

    let reader = writer.get_reader(random)?;
    writer.close(random)?;

    let mut dv = get_only_leaf_reader(&reader)?
      .get_sorted_set_doc_values("field")?
      .unwrap();
    assert_eq!(1, dv.get_value_count()?);
    assert_eq!(0, dv.next_doc()?);
    assert_eq!(1, dv.doc_value_count()?);
    assert_eq!(0, dv.next_ord()?);
    assert_eq!(
      new_bytes_ref_from_string::<_, Vec<u8>>(random, "hello")?.as_bytes(),
      dv.lookup_ord(0)?.as_bytes()
    );
    Ok(())
  }

  fn test_sorted_set_two_documents_first_missing<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwconfig = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwconfig.set_merge_policy(new_log_merge_policy(random)?);
    let writer = RandomIndexWriter::with_config(random, directory.clone(), iwconfig);

    writer.add_document(random, Document::new())?;

    let mut doc = Document::new();
    doc.add(SortedSetDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "hello")?,
    ));
    writer.add_document(random, doc)?;
    writer.force_merge(random, 1)?;

    let reader = writer.get_reader(random)?;
    writer.close(random)?;

    let mut dv = get_only_leaf_reader(&reader)?
      .get_sorted_set_doc_values("field")?
      .unwrap();
    assert_eq!(1, dv.get_value_count()?);
    assert_eq!(1, dv.next_doc()?);
    assert_eq!(1, dv.doc_value_count()?);
    assert_eq!(0, dv.next_ord()?);
    assert_eq!(
      new_bytes_ref_from_string::<_, Vec<u8>>(random, "hello")?.as_bytes(),
      dv.lookup_ord(0)?.as_bytes()
    );
    Ok(())
  }

  fn test_sorted_set_two_documents_first_missing_merge<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwconfig = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwconfig.set_merge_policy(new_log_merge_policy(random)?);
    let writer = RandomIndexWriter::with_config(random, directory.clone(), iwconfig);

    writer.add_document(random, Document::new())?;
    writer.commit(random)?;

    let mut doc = Document::new();
    doc.add(SortedSetDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "hello")?,
    ));
    writer.add_document(random, doc)?;
    writer.force_merge(random, 1)?;

    let reader = writer.get_reader(random)?;
    writer.close(random)?;

    let mut dv = get_only_leaf_reader(&reader)?
      .get_sorted_set_doc_values("field")?
      .unwrap();
    assert_eq!(1, dv.get_value_count()?);
    assert_eq!(1, dv.next_doc()?);
    assert_eq!(1, dv.doc_value_count()?);
    assert_eq!(0, dv.next_ord()?);
    assert_eq!(
      new_bytes_ref_from_string::<_, Vec<u8>>(random, "hello")?.as_bytes(),
      dv.lookup_ord(0)?.as_bytes()
    );
    Ok(())
  }

  fn test_sorted_set_merge_away_all_values<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwconfig = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwconfig.set_merge_policy(new_log_merge_policy(random)?);
    let writer = RandomIndexWriter::with_config(random, directory.clone(), iwconfig);
    let mut field_to_type = HashMap::new();

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "0",
      Store::No,
      &mut field_to_type,
    )?);
    writer.add_document(random, doc)?;
    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "1",
      Store::No,
      &mut field_to_type,
    )?);
    doc.add(SortedSetDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "hello")?,
    ));
    writer.add_document(random, doc)?;
    writer.commit(random)?;
    writer.delete_documents_with_terms(random, vec![Term::from_text("id", "1")])?;
    writer.force_merge(random, 1)?;

    let reader = writer.get_reader(random)?;
    writer.close(random)?;

    let mut dv = get_only_leaf_reader(&reader)?
      .get_sorted_set_doc_values("field")?
      .unwrap();
    assert_eq!(0, dv.get_value_count()?);
    let mut terms_enum = dv.terms_enum()?;
    let lucene = new_bytes_ref_from_string::<_, Vec<u8>>(random, "lucene")?;
    assert!(!terms_enum.seek_exact(&lucene)?);
    assert_eq!(SeekStatus::End, terms_enum.seek_ceil(&lucene)?);
    assert_eq!(-1, dv.lookup_term(&lucene)?);
    Ok(())
  }

  fn test_sorted_set_terms_enum<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwconfig = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwconfig.set_merge_policy(new_log_merge_policy(random)?);
    let writer = RandomIndexWriter::with_config(random, directory.clone(), iwconfig);

    let mut doc = Document::new();
    doc.add(SortedSetDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "hello")?,
    ));
    doc.add(SortedSetDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "world")?,
    ));
    doc.add(SortedSetDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "beer")?,
    ));
    writer.add_document(random, doc)?;

    let reader = writer.get_reader(random)?;
    writer.close(random)?;

    let mut dv = get_only_leaf_reader(&reader)?
      .get_sorted_set_doc_values("field")?
      .unwrap();
    assert_eq!(3, dv.get_value_count()?);
    let mut terms_enum = dv.terms_enum()?;

    assert_eq!("beer", terms_enum.next()?.unwrap().utf8_to_string()?);
    assert_eq!(0, terms_enum.ord()?);
    assert_eq!("hello", terms_enum.next()?.unwrap().utf8_to_string()?);
    assert_eq!(1, terms_enum.ord()?);
    assert_eq!("world", terms_enum.next()?.unwrap().utf8_to_string()?);
    assert_eq!(2, terms_enum.ord()?);

    assert_eq!(
      SeekStatus::NotFound,
      terms_enum.seek_ceil(&new_bytes_ref_from_string::<_, Vec<u8>>(random, "ha!")?)?
    );
    assert_eq!("hello", terms_enum.term()?.utf8_to_string()?);
    assert_eq!(1, terms_enum.ord()?);
    assert_eq!(
      SeekStatus::Found,
      terms_enum.seek_ceil(&new_bytes_ref_from_string::<_, Vec<u8>>(random, "beer")?)?
    );
    assert_eq!("beer", terms_enum.term()?.utf8_to_string()?);
    assert_eq!(0, terms_enum.ord()?);
    assert_eq!(
      SeekStatus::End,
      terms_enum.seek_ceil(&new_bytes_ref_from_string::<_, Vec<u8>>(random, "zzz")?)?
    );

    assert!(terms_enum.seek_exact(&new_bytes_ref_from_string::<_, Vec<u8>>(random, "beer")?)?);
    assert_eq!("beer", terms_enum.term()?.utf8_to_string()?);
    assert_eq!(0, terms_enum.ord()?);
    assert!(terms_enum.seek_exact(&new_bytes_ref_from_string::<_, Vec<u8>>(random, "hello")?)?);
    assert_eq!("hello", terms_enum.term()?.utf8_to_string()?);
    assert_eq!(1, terms_enum.ord()?);
    assert!(terms_enum.seek_exact(&new_bytes_ref_from_string::<_, Vec<u8>>(random, "world")?)?);
    assert_eq!("world", terms_enum.term()?.utf8_to_string()?);
    assert_eq!(2, terms_enum.ord()?);
    assert!(!terms_enum.seek_exact(&new_bytes_ref_from_string::<_, Vec<u8>>(random, "bogus")?)?);

    terms_enum.seek_exact_with_ord(0)?;
    assert_eq!("beer", terms_enum.term()?.utf8_to_string()?);
    assert_eq!(0, terms_enum.ord()?);
    terms_enum.seek_exact_with_ord(1)?;
    assert_eq!("hello", terms_enum.term()?.utf8_to_string()?);
    assert_eq!(1, terms_enum.ord()?);
    terms_enum.seek_exact_with_ord(2)?;
    assert_eq!("world", terms_enum.term()?.utf8_to_string()?);
    assert_eq!(2, terms_enum.ord()?);

    // NORMAL automaton
    let automaton = Operations::determinize(
      &RegExp::from_string(".*l.*")?.to_automaton()?,
      Operations::DEFAULT_DETERMINIZE_WORK_LIMIT,
    )?
    .into_owned();
    let mut terms_enum = dv.intersect(&CompiledAutomaton::from_automaton(automaton)?)?;
    assert_eq!("hello", terms_enum.next()?.unwrap().utf8_to_string()?);
    assert_eq!(1, terms_enum.ord()?);
    assert_eq!("world", terms_enum.next()?.unwrap().utf8_to_string()?);
    assert_eq!(2, terms_enum.ord()?);
    assert!(terms_enum.next()?.is_none());

    // SINGLE automaton
    let automaton = RegExp::from_string("hello")?.to_automaton()?;
    let mut terms_enum = dv.intersect(&CompiledAutomaton::from_automaton(automaton)?)?;
    assert_eq!("hello", terms_enum.next()?.unwrap().utf8_to_string()?);
    assert_eq!(1, terms_enum.ord()?);
    assert!(terms_enum.next()?.is_none());

    reader.close()?;
    directory.close()?;
    Ok(())
  }

  fn compare_stored_field_with_sorted_set_dv<R, CR>(
    &self,
    random: &mut R,
    directory_reader: &CR,
    stored_field: &str,
    dv_field: &str,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
    CR: crate::core::index::composite_reader::CompositeReader,
  {
    let context = directory_reader.get_context()?;
    for leaf in context.leaves()? {
      let reader = leaf.reader();
      let mut stored_fields = reader.stored_fields()?;
      let max_doc = reader.max_doc()?;
      let mut doc_values = match reader.get_sorted_set_doc_values(dv_field)? {
        Some(values) => values,
        None => {
          for doc in 0..max_doc {
            assert!(
              stored_fields
                .document(doc)?
                .get_values(stored_field)?
                .is_empty()
            );
          }
          continue;
        },
      };

      for doc in 0..max_doc {
        let stored_doc = stored_fields.document(doc)?;
        let stored_values = stored_doc.get_values(stored_field)?;
        if stored_values.is_empty() {
          assert!(!doc_values.advance_exact(doc)?);
          continue;
        }
        match random.random_range(0..3) {
          0 => assert_eq!(doc, doc_values.next_doc()?),
          1 => assert_eq!(doc, doc_values.advance(doc)?),
          _ => assert!(doc_values.advance_exact(doc)?),
        }
        assert_eq!(doc, doc_values.doc_id());
        assert_eq!(stored_values.len() as i32, doc_values.doc_value_count()?);
        let repeats = 1 + random.random_range(0..3);
        for r in 0..repeats {
          if r > 0 || random.random_bool(0.5) {
            assert!(doc_values.advance_exact(doc)?);
          }
          let count = doc_values.doc_value_count()?;
          for expected in stored_values.iter().take(count as usize) {
            let ord = doc_values.next_ord()?;
            assert_eq!(
              expected.as_ref(),
              doc_values.lookup_ord(ord)?.utf8_to_string()?
            );
          }
        }
      }

      let iters = 1 + random.random_range(0..3);
      for _ in 0..iters {
        let mut doc_values = reader.get_sorted_set_doc_values(dv_field)?.unwrap();
        let mut doc = random.random_range(0..max_doc);
        while doc < max_doc {
          let stored_doc = stored_fields.document(doc)?;
          let stored_values = stored_doc.get_values(stored_field)?;
          if doc_values.advance_exact(doc)? {
            assert_eq!(doc, doc_values.doc_id());
            assert_eq!(stored_values.len() as i32, doc_values.doc_value_count()?);
            let repeats = 1 + random.random_range(0..3);
            for r in 0..repeats {
              if r > 0 || random.random_bool(0.5) {
                assert!(doc_values.advance_exact(doc)?);
              }
              let count = doc_values.doc_value_count()?;
              for expected in stored_values.iter().take(count as usize) {
                let ord = doc_values.next_ord()?;
                assert_eq!(
                  expected.as_ref(),
                  doc_values.lookup_ord(ord)?.utf8_to_string()?
                );
              }
            }
          } else {
            assert!(stored_values.is_empty());
          }
          doc += 1 + random.random_range(0..5);
        }
      }

      for _ in 0..iters {
        let mut doc_values = reader.get_sorted_set_doc_values(dv_field)?.unwrap();
        let mut doc = random.random_range(0..max_doc);
        while doc != NO_MORE_DOCS {
          let next_doc = doc_values.advance(doc)?;
          for d in doc..if next_doc == NO_MORE_DOCS {
            max_doc
          } else {
            next_doc
          } {
            assert!(
              stored_fields
                .document(d)?
                .get_values(stored_field)?
                .is_empty()
            );
          }
          doc = next_doc;
          if doc != NO_MORE_DOCS {
            let stored_doc = stored_fields.document(doc)?;
            let stored_values = stored_doc.get_values(stored_field)?;
            let repeats = 1 + random.random_range(0..3);
            for r in 0..repeats {
              if r > 0 || random.random_bool(0.5) {
                assert!(doc_values.advance_exact(doc)?);
              }
              let count = doc_values.doc_value_count()?;
              for expected in stored_values.iter().take(count as usize) {
                let ord = doc_values.next_ord()?;
                assert_eq!(
                  expected.as_ref(),
                  doc_values.lookup_ord(ord)?.utf8_to_string()?
                );
              }
            }
            let step = 1 + random.random_range(0..5);
            doc = if next_doc >= max_doc - step {
              NO_MORE_DOCS
            } else {
              next_doc + step
            };
          }
        }
      }
    }
    Ok(())
  }

  fn do_test_sorted_set_vs_stored_fields<R>(
    &self,
    random: &mut R,
    num_docs: i32,
    min_length: usize,
    max_length: usize,
    max_values_per_doc: usize,
    max_unique_values: usize,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_fs_directory(random, create_temp_dir_with_prefix("dvduel")?)?;
    let analyzer = MockAnalyzer::new(random);
    let conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    let writer = RandomIndexWriter::with_config(random, dir.clone(), conf);

    let mut value_set = HashSet::new();
    for _ in 0..10_000 {
      if value_set.len() >= max_unique_values {
        break;
      }
      let length = TestUtil::next_usize(random, min_length, max_length);
      value_set.insert(TestUtil::random_simple_string_range(random, length, length));
    }
    let unique_values = value_set.into_iter().collect::<Vec<_>>();

    let mut field_to_type = HashMap::new();
    for i in 0..num_docs {
      let mut doc = Document::new();
      doc.add(new_string_field(
        random,
        "id",
        i.to_string(),
        Store::No,
        &mut field_to_type,
      )?);
      let num_values = TestUtil::next_usize(random, 0, max_values_per_doc);
      let mut values = BTreeSet::new();
      for _ in 0..num_values {
        values.insert(unique_values.choose(random).unwrap().clone());
      }

      for value in &values {
        doc.add(StoredField::from_string("stored", value)?);
      }

      let mut unordered = values.into_iter().collect::<Vec<_>>();
      unordered.shuffle(random);
      for value in unordered {
        doc.add(SortedSetDocValuesField::new(
          "dv",
          new_bytes_ref_from_string(random, &value)?,
        ));
      }

      writer.add_document(random, doc)?;
      if random.random_range(0..31) == 0 {
        writer.commit(random)?;
      }
    }

    let num_deletions = random.random_range(0..=(num_docs / 10).max(0));
    for _ in 0..num_deletions {
      let id = random.random_range(0..num_docs);
      writer.delete_documents_with_terms(random, vec![Term::from_text("id", id.to_string())])?;
    }

    {
      let reader = writer.get_reader(random)?;
      TestUtil::check_reader(&reader)?;
      self.compare_stored_field_with_sorted_set_dv(random, &reader, "stored", "dv")?;
    }
    writer.force_merge(random, 1)?;
    {
      let reader = writer.get_reader(random)?;
      TestUtil::check_reader(&reader)?;
      self.compare_stored_field_with_sorted_set_dv(random, &reader, "stored", "dv")?;
    }
    writer.close(random)?;
    Ok(())
  }

  fn test_sorted_set_fixed_length_vs_stored_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_iterations = at_least(random, 1);
    for _ in 0..num_iterations {
      let fixed_length = TestUtil::next_usize(random, 1, 10);
      let num_docs = at_least(random, 300);
      self.do_test_sorted_set_vs_stored_fields(
        random,
        num_docs,
        fixed_length,
        fixed_length,
        16,
        100,
      )?;
    }
    Ok(())
  }

  fn test_sorted_numerics_single_valued_vs_stored_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_iterations = at_least(random, 1);
    for _ in 0..num_iterations {
      self
        .do_test_sorted_numerics_vs_stored_fields(random, &mut |_| 1, &mut |r| r.random::<i64>())?;
    }
    Ok(())
  }

  fn test_sorted_numerics_single_valued_missing_vs_stored_fields<R>(
    &self,
    random: &mut R,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_iterations = at_least(random, 1);
    for _ in 0..num_iterations {
      self.do_test_sorted_numerics_vs_stored_fields(
        random,
        &mut |r| if r.random_bool(0.5) { 0 } else { 1 },
        &mut |r| r.random::<i64>(),
      )?;
    }
    Ok(())
  }

  fn test_sorted_numerics_multiple_values_vs_stored_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_iterations = at_least(random, 1);
    for _ in 0..num_iterations {
      self.do_test_sorted_numerics_vs_stored_fields(
        random,
        &mut |r| TestUtil::next_usize(r, 0, 50),
        &mut |r| r.random::<i64>(),
      )?;
    }
    Ok(())
  }

  fn test_sorted_numerics_few_unique_sets_vs_stored_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let values_len = TestUtil::next_usize(random, 2, 6);
    let mut values = vec![0_i64; values_len];
    for value in &mut values {
      *value = random.random::<i64>();
    }
    let num_iterations = at_least(random, 1);
    for _ in 0..num_iterations {
      self.do_test_sorted_numerics_vs_stored_fields(
        random,
        &mut |r| TestUtil::next_usize(r, 0, 6),
        &mut |r| values[r.random_range(0..values.len())],
      )?;
    }
    Ok(())
  }

  fn test_sorted_set_variable_length_vs_stored_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_iterations = at_least(random, 1);
    for _ in 0..num_iterations {
      let num_docs = at_least(random, 300);
      self.do_test_sorted_set_vs_stored_fields(random, num_docs, 1, 10, 16, 100)?;
    }
    Ok(())
  }

  fn test_sorted_set_fixed_length_single_valued_vs_stored_fields<R>(
    &self,
    random: &mut R,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_iterations = at_least(random, 1);
    for _ in 0..num_iterations {
      let fixed_length = TestUtil::next_usize(random, 1, 10);
      let num_docs = at_least(random, 300);
      self.do_test_sorted_set_vs_stored_fields(
        random,
        num_docs,
        fixed_length,
        fixed_length,
        1,
        100,
      )?;
    }
    Ok(())
  }

  fn test_sorted_set_variable_length_single_valued_vs_stored_fields<R>(
    &self,
    random: &mut R,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_iterations = at_least(random, 1);
    for _ in 0..num_iterations {
      let num_docs = at_least(random, 300);
      self.do_test_sorted_set_vs_stored_fields(random, num_docs, 1, 10, 1, 100)?;
    }
    Ok(())
  }

  fn test_sorted_set_fixed_length_few_unique_sets_vs_stored_fields<R>(
    &self,
    random: &mut R,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_iterations = at_least(random, 1);
    for _ in 0..num_iterations {
      let num_docs = at_least(random, 300);
      self.do_test_sorted_set_vs_stored_fields(random, num_docs, 10, 10, 6, 6)?;
    }
    Ok(())
  }

  fn test_sorted_set_variable_length_few_unique_sets_vs_stored_fields<R>(
    &self,
    random: &mut R,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_iterations = at_least(random, 1);
    for _ in 0..num_iterations {
      let num_docs = at_least(random, 300);
      self.do_test_sorted_set_vs_stored_fields(random, num_docs, 1, 10, 6, 6)?;
    }
    Ok(())
  }

  fn test_sorted_set_variable_length_many_values_per_doc_vs_stored_fields<R>(
    &self,
    random: &mut R,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_iterations = at_least(random, 1);
    for _ in 0..num_iterations {
      let num_docs = at_least(random, 20);
      self.do_test_sorted_set_vs_stored_fields(random, num_docs, 1, 10, 500, 1000)?;
    }
    Ok(())
  }

  fn test_sorted_set_fixed_length_many_values_per_doc_vs_stored_fields<R>(
    &self,
    random: &mut R,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_iterations = at_least(random, 1);
    for _ in 0..num_iterations {
      let num_docs = at_least(random, 20);
      self.do_test_sorted_set_vs_stored_fields(random, num_docs, 10, 10, 500, 1000)?;
    }
    Ok(())
  }

  fn test_gcd_compression<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    self.do_test_gcd_compression(random, 1.0)
  }

  fn test_sparse_gcd_compression<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let density = random.random::<f64>();
    self.do_test_gcd_compression(random, density)
  }
  fn do_test_gcd_compression<R>(&self, random: &mut R, density: f64) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_iterations = at_least(random, 1);
    for _ in 0..num_iterations {
      let min = -((random.random_range(0..(1_i32 << 30)) as i64) << 32);
      let mul = (random.random::<u32>()) as i64;
      self.do_test_numerics_vs_stored_fields(random, density, &mut |r| {
        min + mul * (r.random_range(0..(1_i32 << 20)) as i64)
      })?;
    }
    Ok(())
  }

  fn test_zeros<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    self.do_test_numerics_vs_stored_fields(random, 1.0, &mut |_| 0)
  }

  fn test_sparse_zeros<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let density = random.random::<f64>();
    self.do_test_numerics_vs_stored_fields(random, density, &mut |_| 0)
  }

  fn test_zero_or_min<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_iterations = at_least(random, 1);
    for _ in 0..num_iterations {
      self.do_test_numerics_vs_stored_fields(random, 1.0, &mut |r| {
        if r.random_bool(0.5) { 0 } else { i64::MIN }
      })?;
    }
    Ok(())
  }

  fn test_two_numbers_one_missing<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    conf.set_merge_policy(new_log_merge_policy(random)?);
    let writer = RandomIndexWriter::with_config(random, directory.clone(), conf);
    let mut field_to_type = HashMap::new();

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "0",
      Store::Yes,
      &mut field_to_type,
    )?);
    doc.add(NumericDocValuesField::new("dv1", 0));
    writer.add_document(random, doc)?;

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "1",
      Store::Yes,
      &mut field_to_type,
    )?);
    writer.add_document(random, doc)?;
    writer.force_merge(random, 1)?;
    writer.close(random)?;

    let reader = self.maybe_wrap_with_merging_reader(directory_reader::open(directory)?)?;
    assert_eq!(1, (&reader).get_context()?.leaves()?.len());
    let context = (&reader).get_context()?;
    let leaf = context.leaves()?[0].reader();
    let mut dv = leaf.get_numeric_doc_values("dv1")?.unwrap();
    assert_eq!(0, dv.next_doc()?);
    assert_eq!(0, dv.long_value()?);
    assert_eq!(NO_MORE_DOCS, dv.next_doc()?);
    Ok(())
  }

  fn test_two_numbers_one_missing_with_merging<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    conf.set_merge_policy(new_log_merge_policy(random)?);
    let writer = RandomIndexWriter::with_config(random, directory.clone(), conf);
    let mut field_to_type = HashMap::new();

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "0",
      Store::Yes,
      &mut field_to_type,
    )?);
    doc.add(NumericDocValuesField::new("dv1", 0));
    writer.add_document(random, doc)?;
    writer.commit(random)?;

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "1",
      Store::Yes,
      &mut field_to_type,
    )?);
    writer.add_document(random, doc)?;
    writer.force_merge(random, 1)?;
    writer.close(random)?;

    let reader = self.maybe_wrap_with_merging_reader(directory_reader::open(directory)?)?;
    assert_eq!(1, (&reader).get_context()?.leaves()?.len());
    let context = (&reader).get_context()?;
    let leaf = context.leaves()?[0].reader();
    let mut dv = leaf.get_numeric_doc_values("dv1")?.unwrap();
    assert_eq!(0, dv.next_doc()?);
    assert_eq!(0, dv.long_value()?);
    assert_eq!(NO_MORE_DOCS, dv.next_doc()?);
    Ok(())
  }

  fn test_three_numbers_one_missing_with_merging<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    conf.set_merge_policy(new_log_merge_policy(random)?);
    let writer = RandomIndexWriter::with_config(random, directory.clone(), conf);
    let mut field_to_type = HashMap::new();

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "0",
      Store::Yes,
      &mut field_to_type,
    )?);
    doc.add(NumericDocValuesField::new("dv1", 0));
    writer.add_document(random, doc)?;

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "1",
      Store::Yes,
      &mut field_to_type,
    )?);
    writer.add_document(random, doc)?;
    writer.commit(random)?;

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "2",
      Store::Yes,
      &mut field_to_type,
    )?);
    doc.add(NumericDocValuesField::new("dv1", 5));
    writer.add_document(random, doc)?;
    writer.force_merge(random, 1)?;
    writer.close(random)?;

    let reader = self.maybe_wrap_with_merging_reader(directory_reader::open(directory)?)?;
    assert_eq!(1, (&reader).get_context()?.leaves()?.len());
    let context = (&reader).get_context()?;
    let leaf = context.leaves()?[0].reader();
    let mut dv = leaf.get_numeric_doc_values("dv1")?.unwrap();
    assert_eq!(0, dv.next_doc()?);
    assert_eq!(0, dv.long_value()?);
    assert_eq!(2, dv.next_doc()?);
    assert_eq!(5, dv.long_value()?);
    Ok(())
  }

  fn test_two_bytes_one_missing<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    conf.set_merge_policy(new_log_merge_policy(random)?);
    let writer = RandomIndexWriter::with_config(random, directory.clone(), conf);
    let mut field_to_type = HashMap::new();

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "0",
      Store::Yes,
      &mut field_to_type,
    )?);
    doc.add(BinaryDocValuesField::new(
      "dv1",
      BytesRef::from_slice(vec![], 0, 0),
    ));
    writer.add_document(random, doc)?;

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "1",
      Store::Yes,
      &mut field_to_type,
    )?);
    writer.add_document(random, doc)?;
    writer.force_merge(random, 1)?;
    writer.close(random)?;

    let reader = self.maybe_wrap_with_merging_reader(directory_reader::open(directory)?)?;
    assert_eq!(1, (&reader).get_context()?.leaves()?.len());
    let context = (&reader).get_context()?;
    let leaf = context.leaves()?[0].reader();
    let mut dv = leaf.get_binary_doc_values("dv1")?.unwrap();
    assert_eq!(0, dv.next_doc()?);
    assert_eq!(b"".as_slice(), dv.binary_value()?.as_bytes());
    assert_eq!(NO_MORE_DOCS, dv.next_doc()?);
    Ok(())
  }

  fn test_two_bytes_one_missing_with_merging<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    conf.set_merge_policy(new_log_merge_policy(random)?);
    let writer = RandomIndexWriter::with_config(random, directory.clone(), conf);
    let mut field_to_type = HashMap::new();

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "0",
      Store::Yes,
      &mut field_to_type,
    )?);
    doc.add(BinaryDocValuesField::new(
      "dv1",
      BytesRef::from_slice(vec![], 0, 0),
    ));
    writer.add_document(random, doc)?;
    writer.commit(random)?;

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "1",
      Store::Yes,
      &mut field_to_type,
    )?);
    writer.add_document(random, doc)?;
    writer.force_merge(random, 1)?;
    writer.close(random)?;

    let reader = self.maybe_wrap_with_merging_reader(directory_reader::open(directory)?)?;
    assert_eq!(1, (&reader).get_context()?.leaves()?.len());
    let context = (&reader).get_context()?;
    let leaf = context.leaves()?[0].reader();
    let mut dv = leaf.get_binary_doc_values("dv1")?.unwrap();
    assert_eq!(0, dv.next_doc()?);
    assert_eq!(b"".as_slice(), dv.binary_value()?.as_bytes());
    assert_eq!(NO_MORE_DOCS, dv.next_doc()?);
    Ok(())
  }

  fn test_three_bytes_one_missing_with_merging<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    conf.set_merge_policy(new_log_merge_policy(random)?);
    let writer = RandomIndexWriter::with_config(random, directory.clone(), conf);
    let mut field_to_type = HashMap::new();

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "0",
      Store::Yes,
      &mut field_to_type,
    )?);
    doc.add(BinaryDocValuesField::new(
      "dv1",
      BytesRef::from_slice(vec![], 0, 0),
    ));
    writer.add_document(random, doc)?;

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "1",
      Store::Yes,
      &mut field_to_type,
    )?);
    writer.add_document(random, doc)?;
    writer.commit(random)?;

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "2",
      Store::Yes,
      &mut field_to_type,
    )?);
    doc.add(BinaryDocValuesField::new(
      "dv1",
      new_bytes_ref_from_string(random, "boo")?,
    ));
    writer.add_document(random, doc)?;
    writer.force_merge(random, 1)?;
    writer.close(random)?;

    let reader = self.maybe_wrap_with_merging_reader(directory_reader::open(directory)?)?;
    assert_eq!(1, (&reader).get_context()?.leaves()?.len());
    let context = (&reader).get_context()?;
    let leaf = context.leaves()?[0].reader();
    let mut dv = leaf.get_binary_doc_values("dv1")?.unwrap();
    assert_eq!(0, dv.next_doc()?);
    assert_eq!(b"".as_slice(), dv.binary_value()?.as_bytes());
    assert_eq!(2, dv.next_doc()?);
    assert_eq!(
      new_bytes_ref_from_string::<_, Vec<u8>>(random, "boo")?.as_bytes(),
      dv.binary_value()?.as_bytes()
    );
    assert_eq!(NO_MORE_DOCS, dv.next_doc()?);
    Ok(())
  }
  fn test_threads<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    let writer = RandomIndexWriter::with_config(random, dir.clone(), conf);

    let num_docs = at_least(random, 300);
    for i in 0..num_docs {
      let mut doc = Document::new();
      doc.add(StringField::from_string("id", i.to_string(), Store::No)?);

      let length = TestUtil::next_usize(random, 0, 8);
      let mut buffer = vec![0u8; length];
      random.fill(&mut buffer[..]);
      let bytes_ref = if buffer.is_empty() {
        new_bytes_ref_empty(random)?
      } else {
        new_bytes_ref_from_bytes(random, &buffer)?
      };
      let numeric_value = random.random::<i64>();

      doc.add(StoredField::from_binary("storedBin", buffer.clone())?);
      doc.add(BinaryDocValuesField::new("dvBin", bytes_ref.clone()));
      doc.add(SortedDocValuesField::new("dvSorted", bytes_ref));
      doc.add(StoredField::from_string(
        "storedNum",
        numeric_value.to_string(),
      )?);
      doc.add(NumericDocValuesField::new("dvNum", numeric_value));
      writer.add_document(random, doc)?;

      if random.random_range(0..31) == 0 {
        writer.commit(random)?;
      }
    }

    let num_deletions = random.random_range(0..(num_docs / 10).max(1));
    for _ in 0..num_deletions {
      let id = random.random_range(0..num_docs);
      writer.delete_documents_with_terms(random, vec![Term::from_text("id", id.to_string())])?;
    }
    writer.close(random)?;

    let reader =
      Arc::new(self.maybe_wrap_with_merging_reader(directory_reader::open(dir.clone())?)?);
    let num_threads = random.random_range(2..7);
    let starting_gun = Arc::new(Barrier::new(num_threads + 1));

    thread::scope(|scope| -> Result<()> {
      let mut handles = Vec::new();
      for _ in 0..num_threads {
        let reader = reader.clone();
        let starting_gun = starting_gun.clone();
        handles.push(scope.spawn(move || -> Result<()> {
          starting_gun.wait();
          let context = (&reader).get_context()?;
          for leaf in context.leaves()? {
            let leaf_reader = leaf.reader();
            let mut stored_fields = leaf_reader.stored_fields()?;
            let mut binaries = leaf_reader.get_binary_doc_values("dvBin")?.unwrap();
            let mut sorted = leaf_reader.get_sorted_doc_values("dvSorted")?.unwrap();
            let mut numerics = leaf_reader.get_numeric_doc_values("dvNum")?.unwrap();
            let max_doc = leaf_reader.max_doc()?;

            for j in 0..max_doc {
              let stored_doc = stored_fields.document(j)?;
              let binary_value = stored_doc
                .get_binary_value("storedBin")?
                .expect("storedBin should exist");
              assert_eq!(j, binaries.next_doc()?);
              assert_eq!(binary_value.as_bytes(), binaries.binary_value()?.as_bytes());

              assert_eq!(j, sorted.next_doc()?);
              let ord = sorted.ord_value()?;
              assert_eq!(binary_value.as_bytes(), sorted.lookup_ord(ord)?.as_bytes());

              let expected = stored_doc
                .get("storedNum")?
                .expect("storedNum should exist");
              assert_eq!(j, numerics.next_doc()?);
              assert_eq!(expected.parse::<i64>().unwrap(), numerics.long_value()?);
            }
          }
          TestUtil::check_reader(reader.as_ref())?;
          Ok(())
        }));
      }

      starting_gun.wait();
      for handle in handles {
        handle.join().map_err(|_| {
          crate::core::util::error::lucene_error::LuceneError::illegal_state(
            "doc values thread panicked",
          )
        })??;
      }
      Ok(())
    })?;

    reader.close()?;
    Ok(())
  }
  #[cfg_attr(not(feature = "nightly"), allow(dead_code))]
  fn test_threads2<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    let writer = RandomIndexWriter::with_config(random, dir.clone(), conf);

    let num_docs = TestUtil::next_int(random, 1025, 2047);
    for i in 0..num_docs {
      let length = TestUtil::next_usize(random, 0, 8);
      let mut buffer = vec![0u8; length];
      random.fill(&mut buffer[..]);
      let bytes_ref = if buffer.is_empty() {
        new_bytes_ref_empty(random)?
      } else {
        new_bytes_ref_from_bytes(random, &buffer)?
      };
      let numeric_value = random.random::<i64>();
      let mut doc = Document::new();
      doc.add(StringField::from_string("id", i.to_string(), Store::No)?);

      if random.random_range(0..4) > 0 {
        doc.add(StoredField::from_binary("storedBin", buffer.clone())?);
        doc.add(BinaryDocValuesField::new("dvBin", bytes_ref.clone()));
        doc.add(SortedDocValuesField::new("dvSorted", bytes_ref));
      }
      if random.random_range(0..4) > 0 {
        doc.add(StoredField::from_string(
          "storedNum",
          numeric_value.to_string(),
        )?);
        doc.add(NumericDocValuesField::new("dvNum", numeric_value));
      }

      let num_sorted_set_fields = random.random_range(0..3);
      let mut values = BTreeSet::new();
      for _ in 0..num_sorted_set_fields {
        values.insert(TestUtil::random_simple_string(random));
      }
      for value in values {
        doc.add(SortedSetDocValuesField::new(
          "dvSortedSet",
          new_bytes_ref_from_string(random, &value)?,
        ));
        doc.add(StoredField::from_string("storedSortedSet", value)?);
      }

      let num_sorted_numeric_fields = random.random_range(0..3);
      let mut num_values = BTreeSet::new();
      for _ in 0..num_sorted_numeric_fields {
        num_values.insert(TestUtil::next_long(random, i64::MIN, i64::MAX));
      }
      for value in num_values {
        doc.add(SortedNumericDocValuesField::new("dvSortedNumeric", value));
        doc.add(StoredField::from_string(
          "storedSortedNumeric",
          value.to_string(),
        )?);
      }

      writer.add_document(random, doc)?;
      if random.random_range(0..31) == 0 {
        writer.commit(random)?;
      }
    }

    let num_deletions = random.random_range(0..(num_docs / 10).max(1));
    for _ in 0..num_deletions {
      let id = random.random_range(0..num_docs);
      writer.delete_documents_with_terms(random, vec![Term::from_text("id", id.to_string())])?;
    }
    writer.close(random)?;

    let reader =
      Arc::new(self.maybe_wrap_with_merging_reader(directory_reader::open(dir.clone())?)?);
    let num_threads = random.random_range(2..7);
    let starting_gun = Arc::new(Barrier::new(num_threads + 1));

    thread::scope(|scope| -> Result<()> {
      let mut handles = Vec::new();
      for _ in 0..num_threads {
        let reader = reader.clone();
        let starting_gun = starting_gun.clone();
        handles.push(scope.spawn(move || -> Result<()> {
          starting_gun.wait();
          let context = (&reader).get_context()?;
          for leaf in context.leaves()? {
            let leaf_reader = leaf.reader();
            let mut stored_fields = leaf_reader.stored_fields()?;
            let mut binaries = leaf_reader.get_binary_doc_values("dvBin")?;
            let mut sorted = leaf_reader.get_sorted_doc_values("dvSorted")?;
            let mut numerics = leaf_reader.get_numeric_doc_values("dvNum")?;
            let mut sorted_set = leaf_reader.get_sorted_set_doc_values("dvSortedSet")?;
            let mut sorted_numeric =
              leaf_reader.get_sorted_numeric_doc_values("dvSortedNumeric")?;
            let max_doc = leaf_reader.max_doc()?;

            for j in 0..max_doc {
              let stored_doc = stored_fields.document(j)?;
              let binary_value = stored_doc.get_binary_value("storedBin")?;
              if let Some(binary_value) = binary_value
                && let Some(ref mut binaries) = binaries
              {
                assert_eq!(j, binaries.next_doc()?);
                assert_eq!(
                  binary_value.as_bytes_ref().as_byte_slice(),
                  binaries.binary_value()?.as_bytes()
                );
                let sorted = sorted.as_mut().expect("dvSorted should exist");
                assert_eq!(j, sorted.next_doc()?);
                let ord = sorted.ord_value()?;
                assert_eq!(binary_value.as_bytes(), sorted.lookup_ord(ord)?.as_bytes());
              }

              let number = stored_doc.get("storedNum")?;
              if let Some(number) = number
                && let Some(ref mut numerics) = numerics
              {
                assert_eq!(j, numerics.advance(j)?);
                assert_eq!(number.parse::<i64>().unwrap(), numerics.long_value()?);
              }

              let values = stored_doc.get_values("storedSortedSet")?;
              if !values.is_empty() {
                let sorted_set = sorted_set.as_mut().expect("dvSortedSet should exist");
                assert_eq!(j, sorted_set.next_doc()?);
                assert_eq!(values.len() as i32, sorted_set.doc_value_count()?);
                for value in values {
                  let ord = sorted_set.next_ord()?;
                  let actual = sorted_set.lookup_ord(ord)?;
                  assert_eq!(value.as_ref(), actual.utf8_to_string()?);
                }
              }

              let num_values = stored_doc.get_values("storedSortedNumeric")?;
              if !num_values.is_empty() {
                let sorted_numeric = sorted_numeric
                  .as_mut()
                  .expect("dvSortedNumeric should exist");
                assert_eq!(j, sorted_numeric.next_doc()?);
                assert_eq!(num_values.len() as i32, sorted_numeric.doc_value_count()?);
                for num_value in num_values {
                  let value = sorted_numeric.next_value()?;
                  assert_eq!(num_value.as_ref(), value.to_string());
                }
              }
            }
          }
          TestUtil::check_reader(reader.as_ref())?;
          Ok(())
        }));
      }

      starting_gun.wait();
      for handle in handles {
        handle.join().map_err(|_| {
          crate::core::util::error::lucene_error::LuceneError::illegal_state(
            "doc values thread panicked",
          )
        })??;
      }
      Ok(())
    })?;

    reader.close()?;
    Ok(())
  }
  #[cfg_attr(not(feature = "nightly"), allow(dead_code))]
  fn test_threads3<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_fs_directory(random, create_temp_dir()?)?;
    let analyzer = MockAnalyzer::new(random);
    let conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    let writer = RandomIndexWriter::with_config(random, dir.clone(), conf);

    let num_sorted_sets = random.random_range(0..21);
    let num_binaries = random.random_range(0..21);
    let num_sorted_nums = random.random_range(0..21);

    let num_docs = TestUtil::next_int(random, 2025, 2047);
    for _ in 0..num_docs {
      let mut doc = Document::new();
      for j in 0..num_sorted_sets {
        let value = TestUtil::random_simple_string(random);
        doc.add(SortedSetDocValuesField::new(
          format!("ss{}", j),
          new_bytes_ref_from_string(random, &value)?,
        ));
        let value = TestUtil::random_simple_string(random);
        doc.add(SortedSetDocValuesField::new(
          format!("ss{}", j),
          new_bytes_ref_from_string(random, &value)?,
        ));
      }
      for j in 0..num_binaries {
        let value = TestUtil::random_simple_string(random);
        doc.add(BinaryDocValuesField::new(
          format!("b{}", j),
          new_bytes_ref_from_string(random, &value)?,
        ));
      }
      for j in 0..num_sorted_nums {
        doc.add(SortedNumericDocValuesField::new(
          format!("sn{}", j),
          TestUtil::next_long(random, i64::MIN, i64::MAX),
        ));
        doc.add(SortedNumericDocValuesField::new(
          format!("sn{}", j),
          TestUtil::next_long(random, i64::MIN, i64::MAX),
        ));
      }
      writer.add_document(random, doc)?;
    }
    writer.close(random)?;

    for _ in 0..10 {
      let reader =
        Arc::new(self.maybe_wrap_with_merging_reader(directory_reader::open(dir.clone())?)?);
      let num_threads = random.random_range(4..10);
      let starting_gun = Arc::new(Barrier::new(num_threads + 1));

      thread::scope(|scope| -> Result<()> {
        let mut handles = Vec::new();
        for _ in 0..num_threads {
          let reader = reader.clone();
          let starting_gun = starting_gun.clone();
          handles.push(scope.spawn(move || -> Result<()> {
            starting_gun.wait();
            let context = (&reader).get_context()?;
            let mut output = Vec::with_capacity(1024);
            for leaf in context.leaves()? {
              let status = CheckIndex::test_doc_values(leaf.reader(), Some(&mut output), true)?;
              if let Some(error) = status.error {
                return IOUtils::rethrow_always(error);
              }
            }
            Ok(())
          }));
        }

        starting_gun.wait();
        for handle in handles {
          handle.join().map_err(|_| {
            crate::core::util::error::lucene_error::LuceneError::illegal_state(
              "doc values thread panicked",
            )
          })??;
        }
        Ok(())
      })?;

      reader.close()?;
    }
    Ok(())
  }
  fn test_empty_binary_value_on_page_sizes<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    for i in 0..20 {
      if i > 14 && !self.codec_accepts_huge_binary_values("field") {
        break;
      }
      let directory = new_directory_shared(random)?;
      let writer = RandomIndexWriter::new(random, directory.clone())?;
      let bytes = BytesRef::from_slice(vec![0u8; 1usize << i], 0, 1usize << i);

      for _ in 0..4 {
        let mut doc = Document::new();
        doc.add(BinaryDocValuesField::new("field", bytes.clone()));
        writer.add_document(random, doc)?;
      }

      let mut doc = Document::new();
      doc.add(StoredField::from_string("id", "5")?);
      doc.add(BinaryDocValuesField::new("field", BytesRef::new()));
      writer.add_document(random, doc)?;

      let reader = writer.get_reader(random)?;
      writer.close(random)?;

      let mut values = MultiDocValues::get_binary_values(&reader, "field")?.unwrap();
      for j in 0..5 {
        assert_eq!(j, values.next_doc()?);
        let result = values.binary_value()?;
        let length = result.as_bytes().len();
        assert!(length == 0 || length == (1usize << i));
      }
      reader.close()?;
      directory.close()?;
    }
    Ok(())
  }

  fn test_one_sorted_number<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let writer = RandomIndexWriter::new(random, directory.clone())?;

    let mut doc = Document::new();
    doc.add(SortedNumericDocValuesField::new("dv", 5));
    writer.add_document(random, doc)?;
    writer.close(random)?;

    let reader = self.maybe_wrap_with_merging_reader(directory_reader::open(directory)?)?;
    let context = (&reader).get_context()?;
    assert_eq!(1, context.leaves()?.len());
    let mut dv = context.leaves()?[0]
      .reader()
      .get_sorted_numeric_doc_values("dv")?
      .unwrap();
    assert_eq!(0, dv.next_doc()?);
    assert_eq!(1, dv.doc_value_count()?);
    assert_eq!(5, dv.next_value()?);
    Ok(())
  }

  fn test_one_sorted_number_one_missing<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    let writer = IndexWriter::new(directory.clone(), conf)?;

    let mut doc = Document::new();
    doc.add(SortedNumericDocValuesField::new("dv", 5));
    writer.add_document(doc)?;
    writer.add_document(Document::new())?;
    writer.close()?;

    let reader = self.maybe_wrap_with_merging_reader(directory_reader::open(directory)?)?;
    let context = (&reader).get_context()?;
    assert_eq!(1, context.leaves()?.len());
    let mut dv = context.leaves()?[0]
      .reader()
      .get_sorted_numeric_doc_values("dv")?
      .unwrap();
    assert_eq!(0, dv.next_doc()?);
    assert_eq!(1, dv.doc_value_count()?);
    assert_eq!(5, dv.next_value()?);
    assert_eq!(NO_MORE_DOCS, dv.next_doc()?);
    Ok(())
  }

  fn test_number_merge_away_all_values<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwconfig = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwconfig.set_merge_policy(new_log_merge_policy(random)?);
    let writer = RandomIndexWriter::with_config(random, directory.clone(), iwconfig);
    let mut field_to_type = HashMap::new();

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "0",
      Store::No,
      &mut field_to_type,
    )?);
    writer.add_document(random, doc)?;

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "1",
      Store::No,
      &mut field_to_type,
    )?);
    doc.add(NumericDocValuesField::new("field", 5));
    writer.add_document(random, doc)?;
    writer.commit(random)?;
    writer.delete_documents_with_terms(random, vec![Term::from_text("id", "1")])?;
    writer.force_merge(random, 1)?;

    let reader = writer.get_reader(random)?;
    writer.close(random)?;

    let mut dv = get_only_leaf_reader(&reader)?
      .get_numeric_doc_values("field")?
      .unwrap();
    assert_eq!(NO_MORE_DOCS, dv.next_doc()?);
    Ok(())
  }

  fn test_two_sorted_number<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let writer = RandomIndexWriter::new(random, directory.clone())?;

    let mut doc = Document::new();
    doc.add(SortedNumericDocValuesField::new("dv", 11));
    doc.add(SortedNumericDocValuesField::new("dv", -5));
    writer.add_document(random, doc)?;
    writer.close(random)?;

    let reader = self.maybe_wrap_with_merging_reader(directory_reader::open(directory)?)?;
    let context = (&reader).get_context()?;
    assert_eq!(1, context.leaves()?.len());
    let mut dv = context.leaves()?[0]
      .reader()
      .get_sorted_numeric_doc_values("dv")?
      .unwrap();
    assert_eq!(0, dv.next_doc()?);
    assert_eq!(2, dv.doc_value_count()?);
    assert_eq!(-5, dv.next_value()?);
    assert_eq!(11, dv.next_value()?);
    Ok(())
  }

  fn test_two_sorted_number_same_value<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let writer = RandomIndexWriter::new(random, directory.clone())?;

    let mut doc = Document::new();
    doc.add(SortedNumericDocValuesField::new("dv", 11));
    doc.add(SortedNumericDocValuesField::new("dv", 11));
    writer.add_document(random, doc)?;
    writer.close(random)?;

    let reader = self.maybe_wrap_with_merging_reader(directory_reader::open(directory)?)?;
    let context = (&reader).get_context()?;
    assert_eq!(1, context.leaves()?.len());
    let mut dv = context.leaves()?[0]
      .reader()
      .get_sorted_numeric_doc_values("dv")?
      .unwrap();
    assert_eq!(0, dv.next_doc()?);
    assert_eq!(2, dv.doc_value_count()?);
    assert_eq!(11, dv.next_value()?);
    assert_eq!(11, dv.next_value()?);
    Ok(())
  }

  fn test_two_sorted_number_one_missing<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    let writer = IndexWriter::new(directory.clone(), conf)?;

    let mut doc = Document::new();
    doc.add(SortedNumericDocValuesField::new("dv", 11));
    doc.add(SortedNumericDocValuesField::new("dv", -5));
    writer.add_document(doc)?;
    writer.add_document(Document::new())?;
    writer.close()?;

    let reader = self.maybe_wrap_with_merging_reader(directory_reader::open(directory)?)?;
    let context = (&reader).get_context()?;
    assert_eq!(1, context.leaves()?.len());
    let mut dv = context.leaves()?[0]
      .reader()
      .get_sorted_numeric_doc_values("dv")?
      .unwrap();
    assert_eq!(0, dv.next_doc()?);
    assert_eq!(2, dv.doc_value_count()?);
    assert_eq!(-5, dv.next_value()?);
    assert_eq!(11, dv.next_value()?);
    assert_eq!(NO_MORE_DOCS, dv.next_doc()?);
    Ok(())
  }

  fn test_sorted_number_merge<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwconfig = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwconfig.set_merge_policy(new_log_merge_policy(random)?);
    let writer = IndexWriter::new(directory.clone(), iwconfig)?;

    let mut doc = Document::new();
    doc.add(SortedNumericDocValuesField::new("dv", 11));
    writer.add_document(doc)?;
    writer.commit()?;

    let mut doc = Document::new();
    doc.add(SortedNumericDocValuesField::new("dv", -5));
    writer.add_document(doc)?;
    writer.force_merge_with_wait(1, true)?;
    writer.close()?;

    let reader = self.maybe_wrap_with_merging_reader(directory_reader::open(directory)?)?;
    let context = (&reader).get_context()?;
    assert_eq!(1, context.leaves()?.len());
    let mut dv = context.leaves()?[0]
      .reader()
      .get_sorted_numeric_doc_values("dv")?
      .unwrap();
    assert_eq!(0, dv.next_doc()?);
    assert_eq!(1, dv.doc_value_count()?);
    assert_eq!(11, dv.next_value()?);
    assert_eq!(1, dv.next_doc()?);
    assert_eq!(1, dv.doc_value_count()?);
    assert_eq!(-5, dv.next_value()?);
    Ok(())
  }

  fn test_sorted_number_merge_away_all_values<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwconfig = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwconfig.set_merge_policy(new_log_merge_policy(random)?);
    let writer = RandomIndexWriter::with_config(random, directory.clone(), iwconfig);
    let mut field_to_type = HashMap::new();

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "0",
      Store::No,
      &mut field_to_type,
    )?);
    writer.add_document(random, doc)?;

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "1",
      Store::No,
      &mut field_to_type,
    )?);
    doc.add(SortedNumericDocValuesField::new("field", 5));
    writer.add_document(random, doc)?;
    writer.commit(random)?;
    writer.delete_documents_with_terms(random, vec![Term::from_text("id", "1")])?;
    writer.force_merge(random, 1)?;

    let reader = writer.get_reader(random)?;
    writer.close(random)?;

    let mut dv = get_only_leaf_reader(&reader)?
      .get_sorted_numeric_doc_values("field")?
      .unwrap();
    assert_eq!(NO_MORE_DOCS, dv.next_doc()?);
    Ok(())
  }

  fn test_sorted_enum_advance_independently<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwconfig = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwconfig.set_merge_policy(new_log_merge_policy(random)?);
    let writer = RandomIndexWriter::with_config(random, directory.clone(), iwconfig);

    let mut doc = Document::new();
    doc.add(SortedDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "2")?,
    ));
    writer.add_document(random, doc)?;

    let mut doc = Document::new();
    doc.add(SortedDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "1")?,
    ));
    writer.add_document(random, doc)?;

    let mut doc = Document::new();
    doc.add(SortedDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "3")?,
    ));
    writer.add_document(random, doc)?;

    writer.commit(random)?;
    writer.force_merge(random, 1)?;

    let reader = writer.get_reader(random)?;
    writer.close(random)?;

    let leaf = get_only_leaf_reader(&reader)?;
    self.do_test_sorted_set_enum_advance_independently(&mut || {
      let dv = leaf.get_sorted_doc_values("field")?.unwrap();
      DocValues::singleton_sorted(dv)
    })?;
    reader.close()?;
    directory.close()?;
    Ok(())
  }

  fn test_sorted_set_enum_advance_independently<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwconfig = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwconfig.set_merge_policy(new_log_merge_policy(random)?);
    let writer = RandomIndexWriter::with_config(random, directory.clone(), iwconfig);

    let mut doc = Document::new();
    doc.add(SortedSetDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "2")?,
    ));
    doc.add(SortedSetDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "3")?,
    ));
    writer.add_document(random, doc)?;

    let mut doc = Document::new();
    doc.add(SortedSetDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "1")?,
    ));
    doc.add(SortedSetDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "3")?,
    ));
    writer.add_document(random, doc)?;

    let mut doc = Document::new();
    doc.add(SortedSetDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "1")?,
    ));
    doc.add(SortedSetDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "2")?,
    ));
    writer.add_document(random, doc)?;

    writer.commit(random)?;
    writer.force_merge(random, 1)?;

    let reader = writer.get_reader(random)?;
    writer.close(random)?;

    let leaf = get_only_leaf_reader(&reader)?;
    self.do_test_sorted_set_enum_advance_independently(&mut || {
      leaf.get_sorted_set_doc_values("field")?.ok_or_else(|| {
        crate::core::util::error::lucene_error::LuceneError::illegal_state(
          "sorted set doc values should exist",
        )
      })
    })?;
    Ok(())
  }

  fn do_test_sorted_set_enum_advance_independently<DV, F>(&self, make_dv: &mut F) -> Result<()>
  where
    DV: SortedSetDocValues,
    F: FnMut() -> Result<DV>,
  {
    if make_dv()?.get_value_count()? < 2 {
      return Ok(());
    }

    let mut terms = Vec::new();
    {
      let mut dv = make_dv()?;
      let mut te = dv.terms_enum()?;
      terms.push(te.next()?.unwrap().into_owned());
      terms.push(te.next()?.unwrap().into_owned());
    }

    {
      let mut dv1 = make_dv()?;
      let mut dv2 = make_dv()?;
      let mut enum1 = dv1.terms_enum()?;
      let mut enum2 = dv2.terms_enum()?;
      let _ = enum1.next()?;
      let term2 = enum2.next()?.unwrap().into_owned();
      let term1 = enum1.next()?.unwrap().into_owned();
      assert_eq!(term1.as_bytes(), enum1.term()?.as_bytes());
      assert_eq!(term2.as_bytes(), enum2.term()?.as_bytes());
    }

    {
      let mut dv1 = make_dv()?;
      let mut dv2 = make_dv()?;
      let mut enum1 = dv1.terms_enum()?;
      let mut enum2 = dv2.terms_enum()?;
      let term2 = enum2.next()?.unwrap().into_owned();
      let mut seek_term_bytes =
        terms[0].bytes[terms[0].offset..(terms[0].offset + terms[0].length)].to_vec();
      seek_term_bytes.push(0);
      let seek_term = BytesRef::from_bytes(seek_term_bytes);
      enum1.seek_ceil(&seek_term)?;
      let term1 = enum1.term()?.into_owned();
      assert_eq!(term1.as_bytes(), enum1.term()?.as_bytes());
      assert_eq!(term2.as_bytes(), enum2.term()?.as_bytes());
    }

    {
      let mut dv1 = make_dv()?;
      let mut dv2 = make_dv()?;
      let mut enum1 = dv1.terms_enum()?;
      let mut enum2 = dv2.terms_enum()?;
      let term2 = enum2.next()?.unwrap().into_owned();
      enum1.seek_ceil(&terms[1])?;
      let term1 = enum1.term()?.into_owned();
      assert_eq!(term1.as_bytes(), enum1.term()?.as_bytes());
      assert_eq!(term2.as_bytes(), enum2.term()?.as_bytes());
    }

    {
      let mut dv1 = make_dv()?;
      let mut dv2 = make_dv()?;
      let mut enum1 = dv1.terms_enum()?;
      let mut enum2 = dv2.terms_enum()?;
      let term2 = enum2.next()?.unwrap().into_owned();
      assert!(enum1.seek_exact(&terms[1])?);
      let term1 = enum1.term()?.into_owned();
      assert_eq!(term1.as_bytes(), enum1.term()?.as_bytes());
      assert_eq!(term2.as_bytes(), enum2.term()?.as_bytes());
    }

    {
      let mut dv1 = make_dv()?;
      let mut dv2 = make_dv()?;
      let mut enum1 = dv1.terms_enum()?;
      let mut enum2 = dv2.terms_enum()?;
      let term2 = enum2.next()?.unwrap().into_owned();
      enum1.seek_exact_with_ord(1)?;
      let term1 = enum1.term()?.into_owned();
      assert_eq!(term1.as_bytes(), enum1.term()?.as_bytes());
      assert_eq!(term2.as_bytes(), enum2.term()?.as_bytes());
    }
    Ok(())
  }

  fn test_sorted_merge_away_all_values_large_segment<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwconfig = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwconfig.set_merge_policy(new_log_merge_policy(random)?);
    let writer = RandomIndexWriter::with_config(random, directory.clone(), iwconfig);
    let mut field_to_type = HashMap::new();

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "1",
      Store::No,
      &mut field_to_type,
    )?);
    doc.add(SortedDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "hello")?,
    ));
    writer.add_document(random, doc)?;

    let num_empty_docs = at_least(random, 1024);
    for _ in 0..num_empty_docs {
      writer.add_document(random, Document::new())?;
    }

    writer.commit(random)?;
    writer.delete_documents_with_terms(random, vec![Term::from_text("id", "1")])?;
    writer.force_merge(random, 1)?;

    let reader = writer.get_reader(random)?;
    writer.close(random)?;

    let mut dv = get_only_leaf_reader(&reader)?
      .get_sorted_doc_values("field")?
      .unwrap();
    assert_eq!(NO_MORE_DOCS, dv.next_doc()?);

    let mut terms_enum = dv.terms_enum()?;
    let lucene = new_bytes_ref_from_string::<_, Vec<u8>>(random, "lucene")?;
    assert!(!terms_enum.seek_exact(&lucene)?);
    assert_eq!(SeekStatus::End, terms_enum.seek_ceil(&lucene)?);
    assert_eq!(-1, dv.lookup_term(&lucene)?);
    reader.close()?;
    directory.close()?;
    Ok(())
  }

  fn test_sorted_set_merge_away_all_values_large_segment<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwconfig = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwconfig.set_merge_policy(new_log_merge_policy(random)?);
    let writer = RandomIndexWriter::with_config(random, directory.clone(), iwconfig);
    let mut field_to_type = HashMap::new();

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "1",
      Store::No,
      &mut field_to_type,
    )?);
    doc.add(SortedSetDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "hello")?,
    ));
    writer.add_document(random, doc)?;

    let num_empty_docs = at_least(random, 1024);
    for _ in 0..num_empty_docs {
      writer.add_document(random, Document::new())?;
    }

    writer.commit(random)?;
    writer.delete_documents_with_terms(random, vec![Term::from_text("id", "1")])?;
    writer.force_merge(random, 1)?;

    let reader = writer.get_reader(random)?;
    writer.close(random)?;

    let mut dv = get_only_leaf_reader(&reader)?
      .get_sorted_set_doc_values("field")?
      .unwrap();
    assert_eq!(NO_MORE_DOCS, dv.next_doc()?);

    let mut terms_enum = dv.terms_enum()?;
    let lucene = new_bytes_ref_from_string::<_, Vec<u8>>(random, "lucene")?;
    assert!(!terms_enum.seek_exact(&lucene)?);
    assert_eq!(SeekStatus::End, terms_enum.seek_ceil(&lucene)?);
    assert_eq!(-1, dv.lookup_term(&lucene)?);
    Ok(())
  }

  fn test_numeric_merge_away_all_values_large_segment<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwconfig = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwconfig.set_merge_policy(new_log_merge_policy(random)?);
    let writer = RandomIndexWriter::with_config(random, directory.clone(), iwconfig);
    let mut field_to_type = HashMap::new();

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "1",
      Store::No,
      &mut field_to_type,
    )?);
    doc.add(NumericDocValuesField::new("field", 42));
    writer.add_document(random, doc)?;

    let num_empty_docs = at_least(random, 1024);
    for _ in 0..num_empty_docs {
      writer.add_document(random, Document::new())?;
    }

    writer.commit(random)?;
    writer.delete_documents_with_terms(random, vec![Term::from_text("id", "1")])?;
    writer.force_merge(random, 1)?;

    let reader = writer.get_reader(random)?;
    writer.close(random)?;

    let mut dv = get_only_leaf_reader(&reader)?
      .get_numeric_doc_values("field")?
      .unwrap();
    assert_eq!(NO_MORE_DOCS, dv.next_doc()?);
    Ok(())
  }

  fn test_sorted_numeric_merge_away_all_values_large_segment<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwconfig = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwconfig.set_merge_policy(new_log_merge_policy(random)?);
    let writer = RandomIndexWriter::with_config(random, directory.clone(), iwconfig);
    let mut field_to_type = HashMap::new();

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "1",
      Store::No,
      &mut field_to_type,
    )?);
    doc.add(SortedNumericDocValuesField::new("field", 42));
    writer.add_document(random, doc)?;

    let num_empty_docs = at_least(random, 1024);
    for _ in 0..num_empty_docs {
      writer.add_document(random, Document::new())?;
    }

    writer.commit(random)?;
    writer.delete_documents_with_terms(random, vec![Term::from_text("id", "1")])?;
    writer.force_merge(random, 1)?;

    let reader = writer.get_reader(random)?;
    writer.close(random)?;

    let mut dv = get_only_leaf_reader(&reader)?
      .get_sorted_numeric_doc_values("field")?
      .unwrap();
    assert_eq!(NO_MORE_DOCS, dv.next_doc()?);
    Ok(())
  }

  fn test_binary_merge_away_all_values_large_segment<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwconfig = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwconfig.set_merge_policy(new_log_merge_policy(random)?);
    let writer = RandomIndexWriter::with_config(random, directory.clone(), iwconfig);
    let mut field_to_type = HashMap::new();

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "1",
      Store::No,
      &mut field_to_type,
    )?);
    doc.add(BinaryDocValuesField::new(
      "field",
      new_bytes_ref_from_string(random, "hello")?,
    ));
    writer.add_document(random, doc)?;

    let num_empty_docs = at_least(random, 1024);
    for _ in 0..num_empty_docs {
      writer.add_document(random, Document::new())?;
    }

    writer.commit(random)?;
    writer.delete_documents_with_terms(random, vec![Term::from_text("id", "1")])?;
    writer.force_merge(random, 1)?;

    let reader = writer.get_reader(random)?;
    writer.close(random)?;

    let mut dv = get_only_leaf_reader(&reader)?
      .get_binary_doc_values("field")?
      .unwrap();
    assert_eq!(NO_MORE_DOCS, dv.next_doc()?);
    Ok(())
  }

  fn test_random_advance_numeric<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let long_range = if random.random_bool(0.5) {
      TestUtil::next_int(random, 1, 1024) as i64
    } else {
      TestUtil::next_long(random, 1, i64::MAX)
    };
    let fc = FieldCreatorNumeric::new(long_range);
    Self::do_test_random_advance(random, fc)
  }

  fn test_random_advance_binary<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let fc = FieldCreatorBinary;
    Self::do_test_random_advance(random, fc)
  }

  #[cfg_attr(not(feature = "nightly"), allow(dead_code))]
  fn test_high_ords_sorted_set_dv<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwc = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwc.set_ram_buffer_size_mb((8 + random.random_range(0..64)) as f64);
    let writer = IndexWriter::new(directory, iwc)?;

    let num_docs = 20_000 + random.random_range(0..10_000);
    for _ in 1..num_docs {
      let num_ords = if random.random_range(0..100) <= 5 {
        1000 + random.random_range(0..500)
      } else {
        random.random_range(0..10)
      };

      let mut doc = Document::new();
      for _ in 0..num_ords {
        doc.add(SortedSetDocValuesField::new(
          "sorted_set_dv",
          TestUtil::random_binary_term_with_len(random, 2),
        ));
      }
      writer.add_document(doc)?;
    }

    writer.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&writer)?;
    TestUtil::check_reader(&reader)?;
    let leaf = get_only_leaf_reader(&reader)?;
    let mut dv = leaf
      .get_sorted_set_doc_values("sorted_set_dv")?
      .expect("sorted set doc values should exist");

    let mut visited_docs = 0_i32;
    while dv.next_doc()? != NO_MORE_DOCS {
      visited_docs += 1;
      let count = dv.doc_value_count()?;
      assert!(count > 0);
      let mut previous = None;
      for _ in 0..count {
        let ord = dv.next_ord()?;
        if let Some(prev) = previous {
          assert!(ord > prev);
        }
        previous = Some(ord);
      }
    }

    assert!(visited_docs > 0);
    assert!(dv.get_value_count()? > 0);

    reader.close()?;
    writer.close()?;
    Ok(())
  }
  fn do_test_random_advance<T, R>(random: &mut R, field_creator: T) -> Result<()>
  where
    R: Rng + ?Sized,
    T: FieldCreator,
  {
    let analyzer = MockAnalyzer::new(random);
    let directory = new_directory_shared(random)?;
    let mut conf = new_index_writer_config_with_analyzer(random, analyzer)?;
    conf.set_merge_policy(new_log_merge_policy(random)?);
    let writer = RandomIndexWriter::with_config(random, directory, conf);

    let num_chunks = at_least(random, 10);
    let mut id = 0_i64;
    let mut missing_ids = HashSet::new();

    for _ in 0..num_chunks {
      let sparse_chance = random.random::<f64>();
      let doc_count = at_least(random, 1000);
      for _ in 0..doc_count {
        let mut doc = Document::new();
        doc.add(StoredField::from_i64("id", id)?);
        if random.random::<f64>() > sparse_chance {
          doc.add(field_creator.next(random)?);
        } else {
          missing_ids.insert(id);
        }
        id += 1;
        writer.add_document(random, doc)?;
      }
    }

    if random.random_bool(0.5) {
      writer.force_merge(random, 1)?;
    }

    let reader = writer.get_reader(random)?;
    let max_doc = reader.max_doc()?;
    let mut stored_fields = reader.stored_fields()?;
    let mut missing = vec![false; max_doc as usize];
    for doc_id in 0..max_doc {
      let doc = stored_fields.document(doc_id)?;
      let stored_id = doc
        .get_field("id")
        .expect("stored id should exist")
        .numeric_value()?
        .expect("stored id should exist")
        .to_i64()
        .expect("stored id should be i64");
      if missing_ids.contains(&stored_id) {
        missing[doc_id as usize] = true;
      }
    }

    let num_iters = at_least(random, 10);
    for _ in 0..num_iters {
      let mut values = field_creator.iterator(&reader, random)?;

      assert_eq!(-1, values.doc_id());

      loop {
        let doc_id = if random.random_bool(0.5) {
          values.next_doc()?
        } else {
          let current = values.doc_id();
          let range = if random.random_range(0..10) == 7 {
            max_doc - current
          } else {
            25
          };
          let inc = TestUtil::next_int(random, 1, range);
          values.advance(current + inc)?
        };

        if doc_id == NO_MORE_DOCS {
          break;
        }
        assert!(!missing[doc_id as usize]);
      }
    }

    reader.close()?;
    writer.close(random)?;
    Ok(())
  }
  fn codec_accepts_huge_binary_values(&self, _field: &str) -> bool {
    true
  }
}

pub trait FieldCreator {
  fn next<R>(&self, random: &mut R) -> Result<Fields>
  where
    R: Rng + ?Sized;
  type DocIdSetIterator<IR>: DocIdSetIterator
  where
    IR: IndexReader;
  fn iterator<R, IR>(&self, reader: IR, random: &mut R) -> Result<Self::DocIdSetIterator<IR>>
  where
    R: Rng + ?Sized,
    IR: IndexReader;
}
pub struct FieldCreatorBinary;
impl FieldCreator for FieldCreatorBinary {
  fn next<R>(&self, random: &mut R) -> Result<Fields>
  where
    R: Rng + ?Sized,
  {
    let mut bytes = vec![0_u8; random.random_range(0..10)];
    random.fill(&mut bytes[..]);
    Ok(BinaryDocValuesField::new("field", new_bytes_ref_from_bytes(random, &bytes)?).into())
  }

  type DocIdSetIterator<IR>
    = MultiBinaryDocValues<IR>
  where
    IR: IndexReader;

  fn iterator<R, IR>(&self, reader: IR, _random: &mut R) -> Result<Self::DocIdSetIterator<IR>>
  where
    R: Rng + ?Sized,
    IR: IndexReader,
  {
    Ok(MultiDocValues::get_binary_values(reader, "field")?.expect("doc values should exist"))
  }
}

pub struct FieldCreatorNumeric {
  long_range: i64,
}
impl FieldCreatorNumeric {
  fn new(long_range: i64) -> FieldCreatorNumeric {
    Self { long_range }
  }
}
impl FieldCreator for FieldCreatorNumeric {
  fn next<R>(&self, random: &mut R) -> Result<Fields>
  where
    R: Rng + ?Sized,
  {
    Ok(NumericDocValuesField::new("field", TestUtil::next_long(random, 0, self.long_range)).into())
  }

  type DocIdSetIterator<IR>
    = MultiNumericDocValues<IR>
  where
    IR: IndexReader;

  fn iterator<R, IR>(&self, reader: IR, _random: &mut R) -> Result<Self::DocIdSetIterator<IR>>
  where
    R: Rng + ?Sized,
    IR: IndexReader,
  {
    Ok(MultiDocValues::get_numeric_values(reader, "field")?.expect("doc values should exist"))
  }
}
