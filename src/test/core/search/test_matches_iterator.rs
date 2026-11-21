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
use crate::core::codecs::block_term_state::TermStateEnum;
use crate::core::document::int_point::IntPoint;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::index::BytesRef;
use crate::core::index::index_reader::{IndexReader, IndexReaderBase, LeafReaderContextKind};
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_metadata::LeafMetaData;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::term::Term;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::search::boolean_clause::Occur;
use crate::core::search::boolean_query::Builder as BooleanQueryBuilder;
use crate::core::search::disjunction_matches_iterator::from_sub_iterators;
use crate::core::search::index_or_doc_values_query::IndexOrDocValuesQuery;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::search::matches::Matches;
use crate::core::search::matches_iterator::MatchesIterator;
use crate::core::search::multi_phrase_query::Builder as MultiPhraseQueryBuilder;
use crate::core::search::named_matches::NamedMatches;
use crate::core::search::phrase_query::{Builder as PhraseQueryBuilder, PhraseQuery};
use crate::core::search::prefix_query::PrefixQuery;
use crate::core::search::query::{IntoQuery, Query, QueryBase, QueryWeightMatchesIterator};
use crate::core::search::regexp_query::RegexpQuery;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::term_query::TermQuery;
use crate::core::util::access::ByteSource;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::io_utils::IOUtils;
use crate::core::util::{bits::Bits, bytes_ref_iterator::BytesRefIterator};
use crate::test_framework::core::search::matches_test_base::{
  FIELD_DOCS_ONLY, FIELD_FREQS, FIELD_NO_OFFSETS, FIELD_POINT, FIELD_WITH_OFFSETS, MatchesTestBase,
  MatchesTestContext,
};
use crate::test_framework::core::util::lucene_test_case::random;
use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

struct TestMatchesIterator {
  context: MatchesTestContext,
}

impl TestMatchesIterator {
  fn new<R>(random: &mut R) -> Result<Self>
  where
    R: rand::Rng + ?Sized,
  {
    Ok(Self {
      context: MatchesTestContext::new(random, &Self::get_documents())?,
    })
  }

  fn get_documents() -> [&'static str; 6] {
    [
      "w1 w2 w3 w4 w5",
      "w1 w3 w2 w3 zz",
      "w1 xx w2 yy w4",
      "w1 w2 w1 w4 w2 w3",
      "a phrase sentence with many phrase sentence iterations of a phrase sentence",
      "nothing matches this document",
    ]
  }

  fn test_term_query(&self) -> Result<()> {
    let term = Term::from_text(FIELD_WITH_OFFSETS, "w1");
    let query = NamedMatches::wrap_query("q", TermQuery::new(term));
    self.check_matches(
      query.clone(),
      FIELD_WITH_OFFSETS,
      &[
        &[0, 0, 0, 0, 2],
        &[1, 0, 0, 0, 2],
        &[2, 0, 0, 0, 2],
        &[3, 0, 0, 0, 2, 2, 2, 6, 8],
        &[4],
      ],
    )?;
    self.check_label_count(query.clone(), FIELD_WITH_OFFSETS, &[1, 1, 1, 1, 0, 0])?;
    self.assert_is_leaf_match(query.clone(), FIELD_WITH_OFFSETS)?;
    self.check_sub_matches(query, &[&["q"], &["q"], &["q"], &["q"], &[], &[]])
  }

  fn test_term_query_no_stored_offsets(&self) -> Result<()> {
    let query = TermQuery::new(Term::from_text(FIELD_NO_OFFSETS, "w1")).into();
    self.check_matches(
      query,
      FIELD_NO_OFFSETS,
      &[
        &[0, 0, 0, -1, -1],
        &[1, 0, 0, -1, -1],
        &[2, 0, 0, -1, -1],
        &[3, 0, 0, -1, -1, 2, 2, -1, -1],
        &[4],
      ],
    )
  }

  fn test_term_query_no_positions(&self) -> Result<()> {
    for field in [FIELD_DOCS_ONLY, FIELD_FREQS] {
      let query = TermQuery::new(Term::from_text(field, "w1")).into();
      self.check_no_positions_matches(query, field, &[true, true, true, true, false])?;
    }
    Ok(())
  }

  fn test_disjunction(&self) -> Result<()> {
    let w1 = NamedMatches::wrap_query(
      "w1",
      TermQuery::new(Term::from_text(FIELD_WITH_OFFSETS, "w1")),
    );
    let w3 = NamedMatches::wrap_query(
      "w3",
      TermQuery::new(Term::from_text(FIELD_WITH_OFFSETS, "w3")),
    );
    let mut query = BooleanQueryBuilder::new();
    query.add(w1, Occur::Should)?;
    query.add(w3, Occur::Should)?;
    let query: Query = query.build().into();
    self.check_matches(
      query.clone(),
      FIELD_WITH_OFFSETS,
      &[
        &[0, 0, 0, 0, 2, 2, 2, 6, 8],
        &[1, 0, 0, 0, 2, 1, 1, 3, 5, 3, 3, 9, 11],
        &[2, 0, 0, 0, 2],
        &[3, 0, 0, 0, 2, 2, 2, 6, 8, 5, 5, 15, 17],
        &[4],
      ],
    )?;
    self.check_label_count(query.clone(), FIELD_WITH_OFFSETS, &[2, 2, 1, 2, 0, 0])?;
    self.assert_is_leaf_match(query.clone(), FIELD_WITH_OFFSETS)?;
    self.check_sub_matches(
      query,
      &[
        &["w1", "w3"],
        &["w1", "w3"],
        &["w1"],
        &["w1", "w3"],
        &[],
        &[],
      ],
    )
  }

  fn test_disjunction_no_positions(&self) -> Result<()> {
    for field in [FIELD_DOCS_ONLY, FIELD_FREQS] {
      let mut query = BooleanQueryBuilder::new();
      query.add(TermQuery::new(Term::from_text(field, "w1")), Occur::Should)?;
      query.add(TermQuery::new(Term::from_text(field, "w3")), Occur::Should)?;
      self.check_no_positions_matches(
        query.build().into(),
        field,
        &[true, true, true, true, false],
      )?;
    }
    Ok(())
  }

  fn test_req_opt(&self) -> Result<()> {
    let mut query = BooleanQueryBuilder::new();
    query.add(
      TermQuery::new(Term::from_text(FIELD_WITH_OFFSETS, "w1")),
      Occur::Should,
    )?;
    query.add(
      TermQuery::new(Term::from_text(FIELD_WITH_OFFSETS, "w3")),
      Occur::Must,
    )?;
    let query: Query = query.build().into();
    self.check_matches(
      query.clone(),
      FIELD_WITH_OFFSETS,
      &[
        &[0, 0, 0, 0, 2, 2, 2, 6, 8],
        &[1, 0, 0, 0, 2, 1, 1, 3, 5, 3, 3, 9, 11],
        &[2],
        &[3, 0, 0, 0, 2, 2, 2, 6, 8, 5, 5, 15, 17],
        &[4],
      ],
    )?;
    self.check_label_count(query, FIELD_WITH_OFFSETS, &[2, 2, 0, 2, 0, 0])
  }

  fn test_req_opt_no_positions(&self) -> Result<()> {
    for field in [FIELD_DOCS_ONLY, FIELD_FREQS] {
      let mut query = BooleanQueryBuilder::new();
      query.add(TermQuery::new(Term::from_text(field, "w1")), Occur::Should)?;
      query.add(TermQuery::new(Term::from_text(field, "w3")), Occur::Must)?;
      self.check_no_positions_matches(
        query.build().into(),
        field,
        &[true, true, false, true, false],
      )?;
    }
    Ok(())
  }

  fn test_min_should_match(&self) -> Result<()> {
    let w1 = NamedMatches::wrap_query(
      "w1",
      TermQuery::new(Term::from_text(FIELD_WITH_OFFSETS, "w1")),
    );
    let w3 = NamedMatches::wrap_query(
      "w3",
      TermQuery::new(Term::from_text(FIELD_WITH_OFFSETS, "w3")),
    );
    let w4 = TermQuery::new(Term::from_text(FIELD_WITH_OFFSETS, "w4"));
    let xx = NamedMatches::wrap_query(
      "xx",
      TermQuery::new(Term::from_text(FIELD_WITH_OFFSETS, "xx")),
    );
    let mut inner = BooleanQueryBuilder::new();
    inner.add(w1, Occur::Should)?;
    inner.add(w4, Occur::Should)?;
    inner.add(xx, Occur::Should)?;
    inner.set_minimum_number_should_match(2);
    let mut query = BooleanQueryBuilder::new();
    query.add(w3, Occur::Should)?;
    query.add(inner.build(), Occur::Should)?;
    let query: Query = query.build().into();
    self.check_matches(
      query.clone(),
      FIELD_WITH_OFFSETS,
      &[
        &[0, 0, 0, 0, 2, 2, 2, 6, 8, 3, 3, 9, 11],
        &[1, 1, 1, 3, 5, 3, 3, 9, 11],
        &[2, 0, 0, 0, 2, 1, 1, 3, 5, 4, 4, 12, 14],
        &[3, 0, 0, 0, 2, 2, 2, 6, 8, 3, 3, 9, 11, 5, 5, 15, 17],
        &[4],
      ],
    )?;
    self.check_label_count(query.clone(), FIELD_WITH_OFFSETS, &[3, 1, 3, 3, 0, 0])?;
    self.assert_is_leaf_match(query.clone(), FIELD_WITH_OFFSETS)?;
    self.check_sub_matches(
      query,
      &[
        &["w1", "w3"],
        &["w3"],
        &["w1", "xx"],
        &["w1", "w3"],
        &[],
        &[],
      ],
    )
  }

  fn test_min_should_match_no_positions(&self) -> Result<()> {
    for field in [FIELD_FREQS, FIELD_DOCS_ONLY] {
      let mut inner = BooleanQueryBuilder::new();
      inner.add(TermQuery::new(Term::from_text(field, "w1")), Occur::Should)?;
      inner.add(TermQuery::new(Term::from_text(field, "w4")), Occur::Should)?;
      inner.add(TermQuery::new(Term::from_text(field, "xx")), Occur::Should)?;
      inner.set_minimum_number_should_match(2);
      let mut query = BooleanQueryBuilder::new();
      query.add(TermQuery::new(Term::from_text(field, "w3")), Occur::Should)?;
      query.add(inner.build(), Occur::Should)?;
      self.check_no_positions_matches(
        query.build().into(),
        field,
        &[true, true, true, true, false],
      )?;
    }
    Ok(())
  }

  fn test_exclusion(&self) -> Result<()> {
    let mut query = BooleanQueryBuilder::new();
    query.add(
      TermQuery::new(Term::from_text(FIELD_WITH_OFFSETS, "w3")),
      Occur::Should,
    )?;
    query.add(
      TermQuery::new(Term::from_text(FIELD_WITH_OFFSETS, "zz")),
      Occur::MustNot,
    )?;
    self.check_matches(
      query.build().into(),
      FIELD_WITH_OFFSETS,
      &[&[0, 2, 2, 6, 8], &[1], &[2], &[3, 5, 5, 15, 17], &[4]],
    )
  }

  fn test_exclusion_no_positions(&self) -> Result<()> {
    for field in [FIELD_FREQS, FIELD_DOCS_ONLY] {
      let mut query = BooleanQueryBuilder::new();
      query.add(TermQuery::new(Term::from_text(field, "w3")), Occur::Should)?;
      query.add(TermQuery::new(Term::from_text(field, "zz")), Occur::MustNot)?;
      self.check_no_positions_matches(
        query.build().into(),
        field,
        &[true, false, false, true, false],
      )?;
    }
    Ok(())
  }

  fn test_conjunction(&self) -> Result<()> {
    let mut query = BooleanQueryBuilder::new();
    query.add(
      TermQuery::new(Term::from_text(FIELD_WITH_OFFSETS, "w3")),
      Occur::Must,
    )?;
    query.add(
      TermQuery::new(Term::from_text(FIELD_WITH_OFFSETS, "w4")),
      Occur::Must,
    )?;
    self.check_matches(
      query.build().into(),
      FIELD_WITH_OFFSETS,
      &[
        &[0, 2, 2, 6, 8, 3, 3, 9, 11],
        &[1],
        &[2],
        &[3, 3, 3, 9, 11, 5, 5, 15, 17],
        &[4],
      ],
    )
  }

  fn test_conjunction_no_positions(&self) -> Result<()> {
    for field in [FIELD_FREQS, FIELD_DOCS_ONLY] {
      let mut query = BooleanQueryBuilder::new();
      query.add(TermQuery::new(Term::from_text(field, "w3")), Occur::Must)?;
      query.add(TermQuery::new(Term::from_text(field, "w4")), Occur::Must)?;
      self.check_no_positions_matches(
        query.build().into(),
        field,
        &[true, false, false, true, false],
      )?;
    }
    Ok(())
  }

  fn test_wildcards(&self) -> Result<()> {
    let query = PrefixQuery::new(Term::from_text(FIELD_WITH_OFFSETS, "x"))?.into_query();
    self.check_matches(
      query,
      FIELD_WITH_OFFSETS,
      &[&[0], &[1], &[2, 1, 1, 3, 5], &[3], &[4]],
    )?;
    let query = RegexpQuery::new(Term::from_text(FIELD_WITH_OFFSETS, "w[1-2]"))?.into_query();
    self.check_matches(
      query.clone(),
      FIELD_WITH_OFFSETS,
      &[
        &[0, 0, 0, 0, 2, 1, 1, 3, 5],
        &[1, 0, 0, 0, 2, 2, 2, 6, 8],
        &[2, 0, 0, 0, 2, 2, 2, 6, 8],
        &[3, 0, 0, 0, 2, 1, 1, 3, 5, 2, 2, 6, 8, 4, 4, 12, 14],
        &[4],
      ],
    )?;
    self.check_label_count(query.clone(), FIELD_WITH_OFFSETS, &[1, 1, 1, 1, 0])?;
    self.assert_is_leaf_match(query, FIELD_WITH_OFFSETS)
  }

  fn test_no_match_wildcards(&self) -> Result<()> {
    let searcher = &self.context.searcher;
    let query = PrefixQuery::new(Term::from_text(FIELD_WITH_OFFSETS, "wibble"))?.into_query();
    let rewritten = searcher.rewrite(query)?;
    let weight = searcher.create_weight(rewritten, ScoreMode::CompleteNoScores, 1.0)?;
    assert!(
      weight
        .matches(&searcher.get_leaf_contexts()?[0], 0, searcher)?
        .is_none()
    );
    Ok(())
  }

  fn test_wildcards_no_positions(&self) -> Result<()> {
    for field in [FIELD_FREQS, FIELD_DOCS_ONLY] {
      let query = PrefixQuery::new(Term::from_text(field, "x"))?.into_query();
      self.check_no_positions_matches(query, field, &[false, false, true, false, false])?;
    }
    Ok(())
  }

  fn test_synonym_query(&self) -> Result<()> {
    // TODO IMPORTANT SynonymQuery未实现
    Ok(())
  }

  fn test_synonym_query_no_positions(&self) -> Result<()> {
    // TODO IMPORTANT SynonymQuery未实现
    Ok(())
  }

  fn test_multiple_fields(&self) -> Result<()> {
    let mut query = BooleanQueryBuilder::new();
    query.add(TermQuery::new(Term::from_text("id", "1")), Occur::Should)?;
    query.add(
      TermQuery::new(Term::from_text(FIELD_WITH_OFFSETS, "w3")),
      Occur::Must,
    )?;
    let searcher = &self.context.searcher;
    let rewritten = searcher.rewrite(query.build())?;
    let weight = searcher.create_weight(rewritten, ScoreMode::Complete, 1.0)?;
    let context = &searcher.get_leaf_contexts()?[0];
    let matches = weight
      .matches(context, 1 - context.doc_base as i32, searcher)?
      .expect("expected matches");
    self.check_field_matches(
      matches.get_matches("id")?.expect("expected id matches"),
      &[-1, 0, 0, -1, -1],
    )?;
    self.check_field_matches(
      matches
        .get_matches(FIELD_WITH_OFFSETS)?
        .expect("expected field matches"),
      &[-1, 1, 1, 3, 5, 3, 3, 9, 11],
    )?;
    assert!(matches.get_matches("bogus")?.is_none());
    let fields: HashSet<_> = matches.field().iter().map(String::as_str).collect();
    assert_eq!(2, fields.len());
    assert!(fields.contains(FIELD_WITH_OFFSETS));
    assert!(fields.contains("id"));
    assert_eq!(2, matches.get_sub_matches().len());
    Ok(())
  }

  //  0         1         2         3         4         5         6         7
  // "a phrase sentence with many phrase sentence iterations of a phrase sentence",
  fn test_sloppy_phrase_query_with_repeats(&self) -> Result<()> {
    let query =
      PhraseQuery::from_terms(10, FIELD_WITH_OFFSETS, ["phrase", "sentence", "sentence"])?;
    self.check_matches(
      query.clone().into(),
      FIELD_WITH_OFFSETS,
      &[
        &[0],
        &[1],
        &[2],
        &[3],
        &[4, 1, 6, 2, 43, 2, 11, 9, 75, 5, 11, 28, 75, 6, 11, 35, 75],
      ],
    )?;
    self.check_label_count(query.clone().into(), FIELD_WITH_OFFSETS, &[0, 0, 0, 0, 1])?;
    self.assert_is_leaf_match(query.into(), FIELD_WITH_OFFSETS)
  }

  fn test_sloppy_phrase_query(&self) -> Result<()> {
    let query = PhraseQuery::from_terms(4, FIELD_WITH_OFFSETS, ["a", "sentence"])?;
    self.check_matches(
      query.clone().into(),
      FIELD_WITH_OFFSETS,
      &[
        &[0],
        &[1],
        &[2],
        &[3],
        &[4, 0, 2, 0, 17, 6, 9, 35, 59, 9, 11, 58, 75],
      ],
    )?;
    self.assert_is_leaf_match(query.into(), FIELD_WITH_OFFSETS)
  }

  fn test_exact_phrase_query(&self) -> Result<()> {
    let query = PhraseQuery::from_terms_no_slop(FIELD_WITH_OFFSETS, ["phrase", "sentence"])?;
    self.check_matches(
      query.into(),
      FIELD_WITH_OFFSETS,
      &[
        &[0],
        &[1],
        &[2],
        &[3],
        &[4, 1, 2, 2, 17, 5, 6, 28, 43, 10, 11, 60, 75],
      ],
    )?;
    let mut builder = PhraseQueryBuilder::new();
    builder.add_term(Term::from_text(FIELD_WITH_OFFSETS, "a"))?;
    builder.add(Term::from_text(FIELD_WITH_OFFSETS, "sentence"), 2)?;
    let query = builder.build()?;
    self.check_matches(
      query.clone().into(),
      FIELD_WITH_OFFSETS,
      &[&[0], &[1], &[2], &[3], &[4, 0, 2, 0, 17, 9, 11, 58, 75]],
    )?;
    self.assert_is_leaf_match(query.into(), FIELD_WITH_OFFSETS)
  }

  //  0         1         2         3         4         5         6         7
  // "a phrase sentence with many phrase sentence iterations of a phrase sentence",
  fn test_sloppy_multi_phrase_query(&self) -> Result<()> {
    let phrase = Term::from_text(FIELD_WITH_OFFSETS, "phrase");
    let sentence = Term::from_text(FIELD_WITH_OFFSETS, "sentence");
    let iterations = Term::from_text(FIELD_WITH_OFFSETS, "iterations");
    let mut builder = MultiPhraseQueryBuilder::new();
    builder.add_term(phrase)?;
    builder.add_terms([sentence, iterations])?;
    builder.set_slop(4)?;
    let query = builder.build();
    self.check_matches(
      query.clone().into(),
      FIELD_WITH_OFFSETS,
      &[
        &[0],
        &[1],
        &[2],
        &[3],
        &[4, 1, 2, 2, 17, 5, 6, 28, 43, 5, 7, 28, 54, 10, 11, 60, 75],
      ],
    )?;
    self.assert_is_leaf_match(query.into(), FIELD_WITH_OFFSETS)
  }

  fn test_exact_multi_phrase_query(&self) -> Result<()> {
    let mut builder = MultiPhraseQueryBuilder::new();
    builder.add_term(Term::from_text(FIELD_WITH_OFFSETS, "sentence"))?;
    builder.add_terms([
      Term::from_text(FIELD_WITH_OFFSETS, "with"),
      Term::from_text(FIELD_WITH_OFFSETS, "iterations"),
    ])?;
    let query = builder.build();
    self.check_matches(
      query.into(),
      FIELD_WITH_OFFSETS,
      &[&[0], &[1], &[2], &[3], &[4, 2, 3, 9, 22, 6, 7, 35, 54]],
    )?;
    let mut builder = MultiPhraseQueryBuilder::new();
    builder.add_terms([
      Term::from_text(FIELD_WITH_OFFSETS, "a"),
      Term::from_text(FIELD_WITH_OFFSETS, "many"),
    ])?;
    builder.add_term(Term::from_text(FIELD_WITH_OFFSETS, "phrase"))?;
    let query = builder.build();
    self.check_matches(
      query.clone().into(),
      FIELD_WITH_OFFSETS,
      &[
        &[0],
        &[1],
        &[2],
        &[3],
        &[4, 0, 1, 0, 8, 4, 5, 23, 34, 9, 10, 58, 66],
      ],
    )?;
    self.assert_is_leaf_match(query.into(), FIELD_WITH_OFFSETS)
  }

  fn test_point_query(&self) -> Result<()> {
    let mut point_query = IndexOrDocValuesQuery::new(
      IntPoint::new_exact_query(FIELD_POINT, 10)?,
      NumericDocValuesField::new_slow_range_query(FIELD_POINT, 10, 10),
    );
    let term = Term::from_text(FIELD_WITH_OFFSETS, "w1");
    let mut query = BooleanQueryBuilder::new();
    query.add(TermQuery::new(term.clone()), Occur::Must)?;
    query.add(point_query.clone(), Occur::Must)?;
    self.check_matches(point_query.into(), FIELD_WITH_OFFSETS, &[])?;
    self.check_matches(
      query.build().into(),
      FIELD_WITH_OFFSETS,
      &[
        &[0, 0, 0, 0, 2],
        &[1, 0, 0, 0, 2],
        &[2, 0, 0, 0, 2],
        &[3, 0, 0, 0, 2, 2, 2, 6, 8],
        &[4],
      ],
    )?;
    point_query = IndexOrDocValuesQuery::new(
      IntPoint::new_exact_query(FIELD_POINT, 11)?,
      NumericDocValuesField::new_slow_range_query(FIELD_POINT, 11, 11),
    );
    let mut query = BooleanQueryBuilder::new();
    query.add(TermQuery::new(term.clone()), Occur::Must)?;
    query.add(point_query.clone(), Occur::Must)?;
    self.check_matches(query.build().into(), FIELD_WITH_OFFSETS, &[])?;
    let mut query = BooleanQueryBuilder::new();
    query.add(TermQuery::new(term), Occur::Must)?;
    query.add(point_query, Occur::Should)?;
    self.check_matches(
      query.build().into(),
      FIELD_WITH_OFFSETS,
      &[
        &[0, 0, 0, 0, 2],
        &[1, 0, 0, 0, 2],
        &[2, 0, 0, 0, 2],
        &[3, 0, 0, 0, 2, 2, 2, 6, 8],
        &[4],
      ],
    )
  }

  fn test_minimal_seeking_with_wildcards(&self) -> Result<()> {
    let seeks = Arc::new(AtomicI32::new(0));
    let reader = SeekCountingLeafReader::new(
      self.context.searcher.get_index_reader().clone(),
      seeks.clone(),
    )?;
    let searcher = IndexSearcher::new(reader.get_context()?)?;
    let query = PrefixQuery::new(Term::from_text(FIELD_WITH_OFFSETS, "w"))?;
    let rewritten = searcher.rewrite(query)?;
    let weight = searcher.create_weight(rewritten, ScoreMode::Complete, 1.0)?;

    // docs 0-3 match several different terms here, but we only seek to the first term and
    // then short-cut return; other terms are ignored until we try and iterate over matches
    let expected_seeks = [1, 1, 1, 1, 6, 6];
    let mut index = 0;
    for context in searcher.get_leaf_contexts()? {
      for doc in 0..context.reader().max_doc()? {
        seeks.store(0, Ordering::Relaxed);
        weight.matches(context, doc, &searcher)?;
        assert_eq!(
          expected_seeks[index],
          seeks.load(Ordering::Relaxed),
          "Unexpected seek count on doc {doc}"
        );
        index += 1;
      }
    }
    Ok(())
  }

  fn test_from_sub_iterators_method(&self) -> Result<()> {
    struct CountIterator {
      count: i32,
      max: i32,
    }

    impl CountIterator {
      fn new(count: i32) -> Self {
        Self { count, max: count }
      }
    }

    impl MatchesIterator for CountIterator {
      type SubMatches<'a> =
        crate::core::search::dummy::dummy_matches_iterator::DummyMatchesIterator;
      fn next(&mut self) -> Result<bool> {
        if self.count == 0 {
          Ok(false)
        } else {
          self.count -= 1;
          Ok(true)
        }
      }

      fn start_position(&self) -> Result<i32> {
        Ok(self.max - self.count)
      }

      fn end_position(&self) -> Result<i32> {
        Ok(self.max - self.count)
      }

      fn start_offset(&self) -> Result<i32> {
        panic!("start_offset should not be called")
      }

      fn end_offset(&self) -> Result<i32> {
        panic!("end_offset should not be called")
      }

      fn get_sub_matches(&mut self) -> Result<Option<Self::SubMatches<'_>>> {
        panic!("get_sub_matches should not be called")
      }

      fn get_query(&self) -> Result<&Arc<Query>> {
        panic!("get_query should not be called")
      }
    }

    let checks: &[&[i32]] = &[
      &[0],
      &[1],
      &[0, 0],
      &[0, 1],
      &[1, 0],
      &[1, 1],
      &[0, 0, 0],
      &[0, 0, 1],
      &[0, 1, 0],
      &[1, 0, 0],
      &[1, 0, 1],
      &[1, 1, 0],
      &[1, 1, 1],
    ];
    for counts in checks {
      let iterators = counts
        .iter()
        .map(|count| {
          QueryWeightMatchesIterator::<
              crate::core::index::dummy::dummy_leaf_reader::DummyLeafReader,
            >::from(Box::new(CountIterator::new(*count))
              as Box<dyn crate::core::search::query_matches::TestMatchesIterator>)
        })
        .collect();
      let expected_count: i32 = counts.iter().sum();
      let mut merged = from_sub_iterators(iterators)?.expect("expected merged iterator");
      let mut actual_count = 0;
      while merged.next()? {
        actual_count += 1;
      }
      assert_eq!(
        expected_count, actual_count,
        "Sub-iterator count is not right for: {counts:?}"
      );
    }
    Ok(())
  }
}

struct SeekCountingLeafReader<LR>
where
  LR: LeafReader,
{
  in_: LR,
  seeks: Arc<AtomicI32>,
  index_base: IndexReaderBase,
}

impl<LR> SeekCountingLeafReader<LR>
where
  LR: LeafReader,
{
  fn new(in_: LR, seeks: Arc<AtomicI32>) -> Result<Self> {
    let index_base = IndexReaderBase::new();
    in_.register_parent_reader(&index_base)?;
    Ok(Self {
      in_,
      seeks,
      index_base,
    })
  }
}

impl<LR> Clone for SeekCountingLeafReader<LR>
where
  LR: LeafReader + Clone,
{
  fn clone(&self) -> Self {
    Self {
      in_: self.in_.clone(),
      seeks: self.seeks.clone(),
      index_base: self.index_base.clone(),
    }
  }
}

impl<LR> Display for SeekCountingLeafReader<LR>
where
  LR: LeafReader,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "SeekCountingLeafReader({})", self.in_)
  }
}

impl<LR> IndexReader for SeekCountingLeafReader<LR>
where
  LR: LeafReader,
{
  type ContextKind = LeafReaderContextKind;
  type TermVectors = LR::TermVectors;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    self.in_.term_vectors()
  }

  fn max_doc(&self) -> Result<i32> {
    self.in_.max_doc()
  }

  fn num_docs(&self) -> Result<i32> {
    self.in_.num_docs()
  }

  type StoredFields = LR::StoredFields;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    self.in_.stored_fields()
  }

  fn do_close(&self) -> Result<()> {
    self.in_.close()
  }

  type ReaderCacheHelper = LR::ReaderCacheHelper;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    Ok(None)
  }

  fn doc_freq(&self, term: &Term) -> Result<i32> {
    IndexReader::doc_freq(&self.in_, term)
  }

  fn total_term_freq(&self, term: &Term) -> Result<i64> {
    self.in_.total_term_freq(term)
  }

  fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
    IndexReader::get_sum_doc_freq(&self.in_, field)
  }

  fn get_doc_count(&self, field: &str) -> Result<i32> {
    IndexReader::get_doc_count(&self.in_, field)
  }

  fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
    IndexReader::get_sum_total_term_freq(&self.in_, field)
  }

  fn index_base(&self) -> &IndexReaderBase {
    &self.index_base
  }
}

impl<LR> LeafReader for SeekCountingLeafReader<LR>
where
  LR: LeafReader,
{
  type CacheHelper = LR::CacheHelper;

  fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
    Ok(None)
  }

  type Terms = SeekCountingTerms<LR::Terms>;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    Ok(
      self
        .in_
        .terms(field)?
        .map(|terms| SeekCountingTerms::new(terms, self.seeks.clone())),
    )
  }

  type NumericDocValues = LR::NumericDocValues;

  fn get_numeric_doc_values(&self, field: &str) -> Result<Option<Self::NumericDocValues>> {
    self.in_.get_numeric_doc_values(field)
  }

  type BinaryDocValues = LR::BinaryDocValues;

  fn get_binary_doc_values(&self, field: &str) -> Result<Option<Self::BinaryDocValues>> {
    self.in_.get_binary_doc_values(field)
  }

  type SortedDocValues = LR::SortedDocValues;

  fn get_sorted_doc_values(&self, field: &str) -> Result<Option<Self::SortedDocValues>> {
    self.in_.get_sorted_doc_values(field)
  }

  type SortedNumericDocValues = LR::SortedNumericDocValues;

  fn get_sorted_numeric_doc_values(
    &self,
    field: &str,
  ) -> Result<Option<Self::SortedNumericDocValues>> {
    self.in_.get_sorted_numeric_doc_values(field)
  }

  type SortedSetDocValues = LR::SortedSetDocValues;

  fn get_sorted_set_doc_values(&self, field: &str) -> Result<Option<Self::SortedSetDocValues>> {
    self.in_.get_sorted_set_doc_values(field)
  }

  type NormNumericDocValues = LR::NormNumericDocValues;

  fn get_norm_values(&self, field: &str) -> Result<Option<Self::NormNumericDocValues>> {
    self.in_.get_norm_values(field)
  }

  type DocValuesSkipper = LR::DocValuesSkipper;

  fn get_doc_values_skipper(&self, field: &str) -> Result<Option<Self::DocValuesSkipper>> {
    self.in_.get_doc_values_skipper(field)
  }

  type FloatVectorValues = LR::FloatVectorValues;

  fn get_float_vector_values(&self, field: &str) -> Result<Option<Self::FloatVectorValues>> {
    self.in_.get_float_vector_values(field)
  }

  type ByteVectorValues = LR::ByteVectorValues;

  fn get_byte_vector_values(&self, field: &str) -> Result<Option<Self::ByteVectorValues>> {
    self.in_.get_byte_vector_values(field)
  }

  fn search_nearest_vectors_f32<B, K, TV>(
    &self,
    field: &str,
    target: TV,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
    TV: Into<Vec<f32>>,
  {
    let target = target.into();
    self
      .in_
      .search_nearest_vectors_f32(field, target, knn_collector, accept_docs)
  }

  fn search_nearest_vectors_u8<B, K, TV>(
    &self,
    field: &str,
    target: TV,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
    TV: Into<Vec<u8>>,
  {
    let target = target.into();
    self
      .in_
      .search_nearest_vectors_u8(field, target, knn_collector, accept_docs)
  }

  fn get_field_infos(&self) -> Result<Arc<crate::core::index::field_infos::FieldInfos>> {
    self.in_.get_field_infos()
  }

  type Bits = LR::Bits;

  fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
    self.in_.get_live_docs()
  }

  type PointValues = LR::PointValues;

  fn get_point_values(&self, field: &str) -> Result<Option<Self::PointValues>> {
    self.in_.get_point_values(field)
  }

  fn check_integrity(&self) -> Result<()> {
    self.in_.check_integrity()
  }

  fn get_metadata(&self) -> Result<&LeafMetaData> {
    self.in_.get_metadata()
  }
}

struct SeekCountingTerms<T>
where
  T: Terms,
{
  in_: T,
  seeks: Arc<AtomicI32>,
}

impl<T> SeekCountingTerms<T>
where
  T: Terms,
{
  fn new(in_: T, seeks: Arc<AtomicI32>) -> Self {
    Self { in_, seeks }
  }
}

impl<T> Terms for SeekCountingTerms<T>
where
  T: Terms,
{
  type TermsEnum = SeekCountingTermsEnum<T::TermsEnum>;

  fn iterator(&self) -> Result<Self::TermsEnum> {
    Ok(SeekCountingTermsEnum::new(
      self.in_.iterator()?,
      self.seeks.clone(),
    ))
  }

  type IntersectIter = T::IntersectIter;

  fn intersect(
    &self,
    compiled: &crate::core::util::automation::compiled_automaton::CompiledAutomaton,
    start_term: Option<&BytesRef<Vec<u8>>>,
  ) -> Result<Self::IntersectIter> {
    self.in_.intersect(compiled, start_term)
  }

  fn size(&self) -> Result<i64> {
    self.in_.size()
  }

  fn get_sum_total_term_freq(&self) -> Result<i64> {
    self.in_.get_sum_total_term_freq()
  }

  fn get_sum_doc_freq(&self) -> Result<i64> {
    self.in_.get_sum_doc_freq()
  }

  fn get_doc_count(&self) -> Result<i32> {
    self.in_.get_doc_count()
  }

  fn has_freqs(&self) -> bool {
    self.in_.has_freqs()
  }

  fn has_offsets(&self) -> bool {
    self.in_.has_offsets()
  }

  fn has_positions(&self) -> bool {
    self.in_.has_positions()
  }

  fn has_payloads(&self) -> bool {
    self.in_.has_payloads()
  }

  fn get_stats(&self) -> Result<String> {
    self.in_.get_stats()
  }
}

struct SeekCountingTermsEnum<TE>
where
  TE: TermsEnum,
{
  in_: TE,
  seeks: Arc<AtomicI32>,
}

impl<TE> SeekCountingTermsEnum<TE>
where
  TE: TermsEnum,
{
  fn new(in_: TE, seeks: Arc<AtomicI32>) -> Self {
    Self { in_, seeks }
  }
}

impl<TE> BytesRefIterator for SeekCountingTermsEnum<TE>
where
  TE: TermsEnum,
{
  type Value<'a>
    = TE::Value<'a>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    self.in_.next()
  }
}

impl<TE> TermsEnum for SeekCountingTermsEnum<TE>
where
  TE: TermsEnum,
{
  type AttributeSource<'a>
    = TE::AttributeSource<'a>
  where
    Self: 'a;
  type AttributeSourceMut<'a>
    = TE::AttributeSourceMut<'a>
  where
    Self: 'a;

  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    self.in_.attributes()
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    self.in_.attributes_mut()
  }

  fn seek_exact<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<bool> {
    self.seeks.fetch_add(1, Ordering::Relaxed);
    self.in_.seek_exact(term)
  }

  crate::core::index::terms_enum::prepare_seek_exact_delegate!(TE => in_);

  fn seek_ceil<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<SeekStatus> {
    self.in_.seek_ceil(term)
  }

  fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
    self.in_.seek_exact_with_ord(ord)
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    term: &BytesRef<BS>,
    state: &TermStateEnum,
  ) -> Result<()> {
    self.in_.seek_exact_with_state(term, state)
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    self.in_.term()
  }

  fn ord(&self) -> Result<i64> {
    self.in_.ord()
  }

  fn doc_freq(&mut self) -> Result<i32> {
    self.in_.doc_freq()
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    self.in_.total_term_freq()
  }

  type PostingsEnum = TE::PostingsEnum;

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    self.in_.postings_with_flags(reuse, flags)
  }

  type ImpactsEnum = TE::ImpactsEnum;

  fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
    self.in_.impacts(flags)
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    self.in_.term_state()
  }
}

impl MatchesTestBase for TestMatchesIterator {
  fn context(&self) -> &MatchesTestContext {
    &self.context
  }
}

fn run_case<F>(test: F) -> Result<()>
where
  F: FnOnce(&TestMatchesIterator) -> Result<()>,
{
  let mut random = random();
  let case = TestMatchesIterator::new(&mut random)?;
  let result = test(&case);
  IOUtils::use_or_suppress_result(result, case.context.close())
}

#[test]
fn test_term_query() -> Result<()> {
  run_case(TestMatchesIterator::test_term_query)
}

#[test]
fn test_term_query_no_stored_offsets() -> Result<()> {
  run_case(TestMatchesIterator::test_term_query_no_stored_offsets)
}

#[test]
fn test_term_query_no_positions() -> Result<()> {
  run_case(TestMatchesIterator::test_term_query_no_positions)
}

#[test]
fn test_disjunction() -> Result<()> {
  run_case(TestMatchesIterator::test_disjunction)
}

#[test]
fn test_disjunction_no_positions() -> Result<()> {
  run_case(TestMatchesIterator::test_disjunction_no_positions)
}

#[test]
fn test_req_opt() -> Result<()> {
  run_case(TestMatchesIterator::test_req_opt)
}

#[test]
fn test_req_opt_no_positions() -> Result<()> {
  run_case(TestMatchesIterator::test_req_opt_no_positions)
}

#[test]
fn test_min_should_match() -> Result<()> {
  run_case(TestMatchesIterator::test_min_should_match)
}

#[test]
fn test_min_should_match_no_positions() -> Result<()> {
  run_case(TestMatchesIterator::test_min_should_match_no_positions)
}

#[test]
fn test_exclusion() -> Result<()> {
  run_case(TestMatchesIterator::test_exclusion)
}

#[test]
fn test_exclusion_no_positions() -> Result<()> {
  run_case(TestMatchesIterator::test_exclusion_no_positions)
}

#[test]
fn test_conjunction() -> Result<()> {
  run_case(TestMatchesIterator::test_conjunction)
}

#[test]
fn test_conjunction_no_positions() -> Result<()> {
  run_case(TestMatchesIterator::test_conjunction_no_positions)
}

#[test]
fn test_wildcards() -> Result<()> {
  run_case(TestMatchesIterator::test_wildcards)
}

#[test]
fn test_no_match_wildcards() -> Result<()> {
  run_case(TestMatchesIterator::test_no_match_wildcards)
}

#[test]
fn test_wildcards_no_positions() -> Result<()> {
  run_case(TestMatchesIterator::test_wildcards_no_positions)
}

#[test]
#[ignore = "SynonymQuery has a known bug"]
fn test_synonym_query() -> Result<()> {
  run_case(TestMatchesIterator::test_synonym_query)
}

#[test]
#[ignore = "SynonymQuery has a known bug"]
fn test_synonym_query_no_positions() -> Result<()> {
  run_case(TestMatchesIterator::test_synonym_query_no_positions)
}

#[test]
fn test_multiple_fields() -> Result<()> {
  run_case(TestMatchesIterator::test_multiple_fields)
}

#[test]
fn test_sloppy_phrase_query_with_repeats() -> Result<()> {
  run_case(TestMatchesIterator::test_sloppy_phrase_query_with_repeats)
}

#[test]
fn test_sloppy_phrase_query() -> Result<()> {
  run_case(TestMatchesIterator::test_sloppy_phrase_query)
}

#[test]
fn test_exact_phrase_query() -> Result<()> {
  run_case(TestMatchesIterator::test_exact_phrase_query)
}

#[test]
fn test_sloppy_multi_phrase_query() -> Result<()> {
  run_case(TestMatchesIterator::test_sloppy_multi_phrase_query)
}

#[test]
fn test_exact_multi_phrase_query() -> Result<()> {
  run_case(TestMatchesIterator::test_exact_multi_phrase_query)
}

#[test]
fn test_point_query() -> Result<()> {
  run_case(TestMatchesIterator::test_point_query)
}

#[test]
fn test_minimal_seeking_with_wildcards() -> Result<()> {
  run_case(TestMatchesIterator::test_minimal_seeking_with_wildcards)
}

#[test]
fn test_from_sub_iterators_method() -> Result<()> {
  run_case(TestMatchesIterator::test_from_sub_iterators_method)
}

use crate::core::search::weight::Weight;
