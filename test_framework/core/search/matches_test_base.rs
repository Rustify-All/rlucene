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
use crate::core::document::field::Field;
use crate::core::document::field_type::FieldType;
use crate::core::document::int_point::IntPoint;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::text_field::TYPE_STORED;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::reader_util::ReaderUtil;
use crate::core::search::matches::Matches;
use crate::core::search::named_matches::NamedMatches;
use crate::core::search::query::Query;
use crate::core::search::score_mode::ScoreMode;
use crate::core::store::directory::DirEnum;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::util::lucene_test_case::{
  get_only_leaf_reader, new_directory_shared, new_index_writer_config_with_analyzer,
  new_log_merge_policy, new_searcher,
};
use crate::test_framework::core::util::{DefaultCRReaderShared, DefaultIndexSearchLR};
use rand::Rng;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, LazyLock};

/// Base trait for tests checking the `Weight::matches` implementations.
pub trait MatchesTestBase {
  fn context(&self) -> &MatchesTestContext;

  /// For a given query and field, check that expected matches are retrieved.
  fn check_matches(&self, q: Query, field: &str, expected: &[&[i32]]) -> Result<()> {
    let searcher = &self.context().searcher;
    let rewritten = searcher.rewrite(q)?;
    let weight = searcher.create_weight(rewritten, ScoreMode::Complete, 1.0)?;
    for expected in expected {
      let leaf_contexts = searcher.get_leaf_contexts()?;
      let context = &leaf_contexts[ReaderUtil::sub_index_with_leaves(expected[0], leaf_contexts)];
      let doc = expected[0] - context.doc_base as i32;
      let Some(matches) = weight.matches(context, doc, searcher)? else {
        assert_eq!(1, expected.len());
        continue;
      };
      let iterator = matches.get_matches(field)?;
      if expected.len() == 1 {
        assert!(iterator.is_none());
        continue;
      }
      let iterator = iterator.expect("expected matches iterator");
      self.check_field_matches(iterator, expected)?;
      let iterator = matches
        .get_matches(field)?
        .expect("matches iterator should be repeatable");
      self.check_field_matches(iterator, expected)?;
    }
    Ok(())
  }

  /// For a given query and field, check the expected numbers of query labels.
  fn check_label_count(&self, q: Query, field: &str, expected: &[usize]) -> Result<()> {
    let searcher = &self.context().searcher;
    let rewritten = searcher.rewrite(q)?;
    let weight = searcher.create_weight(rewritten, ScoreMode::Complete, 1.0)?;
    for (doc, expected) in expected.iter().enumerate() {
      let leaf_contexts = searcher.get_leaf_contexts()?;
      let doc_id = doc as i32;
      let context = &leaf_contexts[ReaderUtil::sub_index_with_leaves(doc_id, leaf_contexts)];
      let leaf_doc = doc_id - context.doc_base as i32;
      let Some(matches) = weight.matches(context, leaf_doc, searcher)? else {
        assert_eq!(0, *expected, "Expected to get matches on document {doc}");
        continue;
      };
      let iterator = matches.get_matches(field)?;
      if *expected == 0 {
        assert!(iterator.is_none());
        continue;
      }
      let mut iterator = iterator.expect("expected matches iterator");
      let mut labels = HashSet::new();
      while iterator.next()? {
        let query = iterator.get_query()?;
        labels.insert(Arc::as_ptr(query));
      }
      assert_eq!(*expected, labels.len());
    }
    Ok(())
  }

  /// Given a matches iterator, check its start/end positions and offsets.
  fn check_field_matches<M: crate::core::search::matches_iterator::MatchesIterator>(
    &self,
    mut iterator: M,
    expected: &[i32],
  ) -> Result<()> {
    let mut pos = 1;
    while iterator.next()? {
      assert_eq!(
        expected[pos],
        iterator.start_position()?,
        "Wrong start position"
      );
      assert_eq!(
        expected[pos + 1],
        iterator.end_position()?,
        "Wrong end position"
      );
      assert_eq!(
        expected[pos + 2],
        iterator.start_offset()?,
        "Wrong start offset"
      );
      assert_eq!(
        expected[pos + 3],
        iterator.end_offset()?,
        "Wrong end offset"
      );
      pos += 4;
    }
    assert_eq!(expected.len(), pos);
    Ok(())
  }

  /// Check that matches are returned from expected documents without positions.
  fn check_no_positions_matches(&self, q: Query, field: &str, expected: &[bool]) -> Result<()> {
    let searcher = &self.context().searcher;
    let rewritten = searcher.rewrite(q)?;
    let weight = searcher.create_weight(rewritten, ScoreMode::Complete, 1.0)?;
    for (doc, expected) in expected.iter().enumerate() {
      let leaf_contexts = searcher.get_leaf_contexts()?;
      let doc_id = doc as i32;
      let context = &leaf_contexts[ReaderUtil::sub_index_with_leaves(doc_id, leaf_contexts)];
      let leaf_doc = doc_id - context.doc_base as i32;
      let matches = weight.matches(context, leaf_doc, searcher)?;
      if *expected {
        let matches = matches.expect("expected matches");
        let mut iterator = matches
          .get_matches(field)?
          .expect("expected matches iterator");
        assert!(iterator.next()?);
        assert_eq!(-1, iterator.start_position()?);
        while iterator.next()? {
          assert_eq!(-1, iterator.start_position()?);
        }
      } else {
        assert!(matches.is_none());
      }
    }
    Ok(())
  }

  /// Check that matches contain the expected named-query wrapper names.
  fn check_sub_matches(&self, q: Query, expected_names: &[&[&str]]) -> Result<()> {
    let searcher = &self.context().searcher;
    let rewritten = searcher.rewrite(q)?;
    let weight = searcher.create_weight(rewritten, ScoreMode::CompleteNoScores, 1.0)?;
    for (doc, expected_names) in expected_names.iter().enumerate() {
      let leaf_contexts = searcher.get_leaf_contexts()?;
      let doc_id = doc as i32;
      let context = &leaf_contexts[ReaderUtil::sub_index_with_leaves(doc_id, leaf_contexts)];
      let leaf_doc = doc_id - context.doc_base as i32;
      let Some(matches) = weight.matches(context, leaf_doc, searcher)? else {
        assert!(
          expected_names.is_empty(),
          "Expected to get no matches on document {doc}"
        );
        continue;
      };
      let expected_queries: HashSet<_> = expected_names.iter().copied().collect();
      let actual_queries: HashSet<_> = NamedMatches::find_named_matches(&matches)
        .into_iter()
        .map(NamedMatches::get_name)
        .collect();
      let unexpected: Vec<_> = actual_queries.difference(&expected_queries).collect();
      assert!(
        unexpected.is_empty(),
        "Unexpected matching leaf queries: {unexpected:?}"
      );
      let missing: Vec<_> = expected_queries.difference(&actual_queries).collect();
      assert!(
        missing.is_empty(),
        "Missing matching leaf queries: {missing:?}"
      );
    }
    Ok(())
  }

  /// Assert that query matches from a field are leaf matches.
  fn assert_is_leaf_match(&self, q: Query, field: &str) -> Result<()> {
    let searcher = &self.context().searcher;
    let rewritten = searcher.rewrite(q)?;
    let weight = searcher.create_weight(rewritten, ScoreMode::Complete, 1.0)?;
    for doc in 0..searcher.get_index_reader().max_doc()? {
      let leaf_contexts = searcher.get_leaf_contexts()?;
      let context = &leaf_contexts[ReaderUtil::sub_index_with_leaves(doc, leaf_contexts)];
      let leaf_doc = doc - context.doc_base as i32;
      let Some(matches) = weight.matches(context, leaf_doc, searcher)? else {
        return Ok(());
      };
      let Some(mut iterator) = matches.get_matches(field)? else {
        return Ok(());
      };
      while iterator.next()? {
        assert!(iterator.get_sub_matches()?.is_none());
      }
    }
    Ok(())
  }

  /// Check each document's term submatches against the expected matches.
  #[allow(unused)]
  fn check_term_matches<M: crate::core::search::matches_iterator::MatchesIterator>(
    &self,
    q: Query,
    field: &str,
    expected: &[&[&[TermMatch]]],
  ) -> Result<()> {
    let searcher = &self.context().searcher;
    let rewritten = searcher.rewrite(q)?;
    let weight = searcher.create_weight(rewritten, ScoreMode::Complete, 1.0)?;
    for (doc, expected) in expected.iter().enumerate() {
      let leaf_contexts = searcher.get_leaf_contexts()?;
      let doc_id = doc as i32;
      let context = &leaf_contexts[ReaderUtil::sub_index_with_leaves(doc_id, leaf_contexts)];
      let leaf_doc = doc_id - context.doc_base as i32;
      let Some(matches) = weight.matches(context, leaf_doc, searcher)? else {
        assert!(expected.is_empty());
        continue;
      };
      let iterator = matches.get_matches(field)?;
      if expected.is_empty() {
        assert!(iterator.is_none());
        continue;
      }
      let iterator = iterator.expect("expected matches iterator");
      self.check_terms(expected, iterator)?;
    }
    Ok(())
  }

  #[allow(unused)]
  fn check_terms<M: crate::core::search::matches_iterator::MatchesIterator>(
    &self,
    expected: &[&[TermMatch]],
    mut iterator: M,
  ) -> Result<()> {
    let mut up_to = 0;
    while iterator.next()? {
      let mut expected_matches: HashSet<_> = expected[up_to].iter().cloned().collect();
      let mut sub_matches = iterator
        .get_sub_matches()?
        .expect("expected term submatches");
      while sub_matches.next()? {
        let term_match = TermMatch::new(
          sub_matches.start_position()?,
          sub_matches.start_offset()?,
          sub_matches.end_offset()?,
        );
        assert!(
          expected_matches.remove(&term_match),
          "Unexpected term match: {term_match:?}"
        );
      }
      assert!(
        expected_matches.is_empty(),
        "Missing term matches: {expected_matches:?}"
      );
      up_to += 1;
    }
    assert!(
      up_to >= expected.len().saturating_sub(1),
      "Missing expected match"
    );
    Ok(())
  }
}

pub const FIELD_WITH_OFFSETS: &str = "field_offsets";
pub const FIELD_NO_OFFSETS: &str = "field_no_offsets";
pub const FIELD_DOCS_ONLY: &str = "field_docs_only";
pub const FIELD_FREQS: &str = "field_freqs";
pub const FIELD_POINT: &str = "field_point";

static OFFSETS: LazyLock<FieldType> = LazyLock::new(|| {
  let mut field_type = FieldType::from_ref(&*TYPE_STORED).expect("copy text field type");
  field_type
    .set_index_options(IndexOptions::DocsAndFreqsAndPositionsAndOffsets)
    .expect("set offsets index options");
  field_type.freeze();
  field_type
});

static DOCS: LazyLock<FieldType> = LazyLock::new(|| {
  let mut field_type = FieldType::from_ref(&*TYPE_STORED).expect("copy text field type");
  field_type
    .set_index_options(IndexOptions::Docs)
    .expect("set docs index options");
  field_type.freeze();
  field_type
});

static DOCS_AND_FREQS: LazyLock<FieldType> = LazyLock::new(|| {
  let mut field_type = FieldType::from_ref(&*TYPE_STORED).expect("copy text field type");
  field_type
    .set_index_options(IndexOptions::DocsAndFreqs)
    .expect("set docs and freqs index options");
  field_type.freeze();
  field_type
});

pub struct MatchesTestContext {
  pub searcher: DefaultIndexSearchLR,
  reader: DefaultCRReaderShared,
  directory: Arc<DirEnum>,
}

impl MatchesTestContext {
  pub fn new<R>(random: &mut R, documents: &[&str]) -> Result<Self>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut config = new_index_writer_config_with_analyzer(random, analyzer)?;
    config.set_merge_policy(new_log_merge_policy(random)?);
    let writer = RandomIndexWriter::with_config(random, directory.clone(), config);
    for (i, text) in documents.iter().enumerate() {
      let mut document = Document::new();
      document.add(Field::from_string(
        FIELD_WITH_OFFSETS,
        *text,
        OFFSETS.clone(),
      )?);
      document.add(Field::from_string(
        FIELD_NO_OFFSETS,
        *text,
        TYPE_STORED.clone(),
      )?);
      document.add(Field::from_string(FIELD_DOCS_ONLY, *text, DOCS.clone())?);
      document.add(Field::from_string(
        FIELD_FREQS,
        *text,
        DOCS_AND_FREQS.clone(),
      )?);
      document.add(IntPoint::new(FIELD_POINT, [10])?);
      document.add(NumericDocValuesField::new(FIELD_POINT, 10));
      document.add(NumericDocValuesField::new("id", i as i64));
      document.add(Field::from_string(
        "id",
        i.to_string(),
        TYPE_STORED.clone(),
      )?);
      writer.add_document(random, document)?;
    }
    writer.force_merge(random, 1)?;
    let reader = Arc::new(writer.get_reader(random)?);
    writer.close(random)?;
    let searcher = new_searcher(random, get_only_leaf_reader(reader.clone())?)?;
    Ok(Self {
      searcher,
      reader,
      directory,
    })
  }

  pub fn close(self) -> Result<()> {
    self.reader.close()?;
    self.directory.close()
  }
}

/// Encapsulates a term position, start offset and end offset.
#[derive(Clone, Debug, Eq)]
#[allow(dead_code)]
pub struct TermMatch {
  pub position: i32,
  pub start_offset: i32,
  pub end_offset: i32,
}

impl TermMatch {
  #[allow(unused)]
  pub fn new(position: i32, start_offset: i32, end_offset: i32) -> Self {
    Self {
      position,
      start_offset,
      end_offset,
    }
  }
}

impl PartialEq for TermMatch {
  fn eq(&self, other: &Self) -> bool {
    self.position == other.position
      && self.start_offset == other.start_offset
      && self.end_offset == other.end_offset
  }
}

impl Hash for TermMatch {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.position.hash(state);
    self.start_offset.hash(state);
    self.end_offset.hash(state);
  }
}

use crate::core::search::weight::Weight;

use crate::core::search::matches_iterator::MatchesIterator;
