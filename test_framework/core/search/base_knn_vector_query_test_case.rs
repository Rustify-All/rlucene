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
use crate::core::document::fields::Fields;
use crate::core::document::int_point::IntPoint;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::string_field::StringField;
use crate::core::index::base_composite_reader::{
  BCRStoredFieldsImpl, BCRTermVectorsImpl, BaseCompositeReader, BaseCompositeReaderBase,
};
use crate::core::index::composite_reader::CompositeReader;
use crate::core::index::directory_reader;
use crate::core::index::directory_reader::{DirectoryReader, DirectoryReaderBase};
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::filter_directory_reader::{FilterDirectoryReader, SubReaderWrapper};
use crate::core::index::filter_leaf_reader::FilterLeafReader;
use crate::core::index::index_commit::IndexCommit;
use crate::core::index::index_reader::{
  CompositeReaderContextKind, IndexReader, IndexReaderBase, LeafReaderContextKind,
};
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::knn_vector_values::KnnVectorValues;
use crate::core::index::leaf_metadata::LeafMetaData;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::multi_reader::MultiReader;
use crate::core::index::query_timeout::{QueryTimeout, QueryTimeoutEnum};
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term::Term;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::search::abstract_knn_vector_query::AbstractKnnVectorQuery;
use crate::core::search::boost_query::BoostQuery;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::field_value_hit_queue::TopFieldScoreDoc;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::knn::knn_collector_manager::KnnCollectorManager;
use crate::core::search::knn::top_knn_collector_manager::TopKnnCollectorManager;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::search::match_all_docs_query::MatchAllDocsQuery;
use crate::core::search::match_no_docs_query::MatchNoDocsQuery;
use crate::core::search::query::{Query, QueryBase};
use crate::core::search::scorable::Scorable;
use crate::core::search::score_doc::ScoreDoc;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer::Scorer;
use crate::core::search::sort::Sort;
use crate::core::search::sort_field::{SortField, SortFieldType};
use crate::core::search::term_query::TermQuery;
use crate::core::search::time_limiting_knn_collector_manager::TimeLimitingKnnCollectorManager;
use crate::core::search::total_hits::Relation::{EqualTo, GreaterThanOrEqualTo};
use crate::core::store::directory::{DirEnum, Directory};
use crate::core::util::bits::{Bits, MatchNoBits};
use crate::core::util::close::CloseableRef;
use crate::core::util::dummy::dummy_comparator::DummyComparator;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind};
use crate::core::util::io_utils::IOUtils;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::util::lucene_test_case::{
  at_least_usize, get_only_leaf_reader, new_directory_shared, new_searcher_with_reader,
  random_vector_format,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::Rng;
use rand::RngExt;
use std::collections::HashSet;
use std::fmt::{Debug, Display, Formatter};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

pub trait BaseKnnVectorQueryTestCase {
  type KnnVectorQuery: AbstractKnnVectorQuery + Clone + PartialEq + Eq + Debug + Into<Query>;

  fn get_knn_vector_query(
    &self,
    field: &str,
    query: Vec<f32>,
    k: usize,
    query_filter: Option<Query>,
  ) -> Result<Self::KnnVectorQuery>;

  fn get_throwing_knn_vector_query(
    &self,
    field: &str,
    query: Vec<f32>,
    k: usize,
    query_filter: Option<Query>,
  ) -> Result<Self::KnnVectorQuery>;

  fn get_knn_vector_query_no_filter(
    &self,
    field: &str,
    query: Vec<f32>,
    k: usize,
  ) -> Result<Self::KnnVectorQuery> {
    self.get_knn_vector_query(field, query, k, None)
  }

  fn random_vector<R>(&self, random: &mut R, dim: usize) -> Vec<f32>
  where
    R: Rng + ?Sized;

  fn get_knn_vector_field_with_similarity(
    &self,
    name: &str,
    vector: Vec<f32>,
    similarity_function: VectorSimilarityFunction,
  ) -> Result<Fields>;

  fn get_knn_vector_field(&self, name: &str, vector: Vec<f32>) -> Result<Fields>;

  type Directory: Directory + Clone + Into<Arc<DirEnum>>;
  fn new_directory_for_test<R>(&self, random: &mut R) -> Result<Self::Directory>
  where
    R: Rng + ?Sized;
  fn default_new_directory_for_test<R>(&self, random: &mut R) -> Result<Arc<DirEnum>>
  where
    R: Rng + ?Sized,
  {
    new_directory_shared(random)
  }

  fn test_equals(&self) -> Result<()> {
    let q1 = self.get_knn_vector_query_no_filter("f1", vec![0.0, 1.0], 10)?;
    let filter1 = TermQuery::new(Term::from_text("id", "id1"));
    let q2 = self.get_knn_vector_query("f1", vec![0.0, 1.0], 10, Some(filter1.clone().into()))?;

    assert_ne!(q2, q1);
    assert_ne!(q1, q2);
    assert_eq!(
      q2,
      self.get_knn_vector_query("f1", vec![0.0, 1.0], 10, Some(filter1.into()))?
    );

    let filter2 = TermQuery::new(Term::from_text("id", "id2"));
    assert_ne!(
      q2,
      self.get_knn_vector_query("f1", vec![0.0, 1.0], 10, Some(filter2.into()))?
    );

    assert_eq!(
      q1,
      self.get_knn_vector_query_no_filter("f1", vec![0.0, 1.0], 10)?
    );

    assert_ne!(Some(&q1), None);

    let term_query: Query = TermQuery::new(Term::from_text("f1", "x")).into();
    let q1_query: Query = q1.clone().into();
    assert_ne!(q1_query, term_query);

    assert_ne!(
      q1,
      self.get_knn_vector_query_no_filter("f2", vec![0.0, 1.0], 10)?
    );
    assert_ne!(
      q1,
      self.get_knn_vector_query_no_filter("f1", vec![1.0, 1.0], 10)?
    );
    assert_ne!(
      q1,
      self.get_knn_vector_query_no_filter("f1", vec![0.0, 1.0], 2)?
    );
    assert_ne!(
      q1,
      self.get_knn_vector_query_no_filter("f1", vec![0.0], 10)?
    );

    Ok(())
  }

  fn test_get_field(&self) -> Result<()> {
    let q1 = self.get_knn_vector_query_no_filter("f1", vec![0.0, 1.0], 10)?;
    let filter1 = TermQuery::new(Term::from_text("id", "id1"));
    let q2 = self.get_knn_vector_query("f2", vec![0.0, 1.0], 10, Some(filter1.into()))?;

    assert_eq!("f1", q1.base().field);
    assert_eq!("f2", q2.base().field);
    Ok(())
  }

  fn test_get_k(&self) -> Result<()> {
    let q1 = self.get_knn_vector_query_no_filter("f1", vec![0.0, 1.0], 6)?;
    let filter1 = TermQuery::new(Term::from_text("id", "id1"));
    let q2 = self.get_knn_vector_query("f2", vec![0.0, 1.0], 7, Some(filter1.into()))?;

    assert_eq!(6, q1.base().k);
    assert_eq!(7, q2.base().k);
    Ok(())
  }

  fn test_get_filter(&self) -> Result<()> {
    let q1 = self.get_knn_vector_query_no_filter("f1", vec![0.0, 1.0], 6)?;
    let filter1 = TermQuery::new(Term::from_text("id", "id1"));
    let filter1_query: Query = filter1.into();
    let q2 = self.get_knn_vector_query("f2", vec![0.0, 1.0], 7, Some(filter1_query.clone()))?;

    assert!(q1.base().filter.is_none());
    assert_eq!(Some(&filter1_query), q2.base().filter.as_deref());
    Ok(())
  }

  fn test_empty_index(&self) -> Result<()> {
    let searcher = new_searcher_with_reader(MultiReader::empty()?)?;
    let kvq = self.get_knn_vector_query_no_filter("field", vec![1.0, 2.0], 10)?;

    let hits = searcher.search(kvq.clone(), 10)?;
    assert_eq!(0, hits.score_docs.len());

    let rewritten = searcher.rewrite(kvq)?;
    assert!(matches!(
      rewritten,
      Query::MatchNoDocs(MatchNoDocsQuery { .. })
    ));
    Ok(())
  }

  fn test_find_all<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let index_store = self.get_index_store(
      random,
      "field",
      &[vec![0.0, 1.0], vec![1.0, 2.0], vec![0.0, 0.0]],
    )?;
    let reader = directory_reader::open(index_store.into())?;
    let searcher = new_searcher_with_reader(reader)?;
    let kvq = self.get_knn_vector_query_no_filter("field", vec![0.0, 0.0], 10)?;

    self.assert_matches(&searcher, kvq.clone(), 3)?;
    let score_docs = searcher.search(kvq, 3)?.score_docs;
    self.assert_id_matches(searcher.get_index_reader(), "id2", &score_docs[0])?;
    self.assert_id_matches(searcher.get_index_reader(), "id0", &score_docs[1])?;
    self.assert_id_matches(searcher.get_index_reader(), "id1", &score_docs[2])?;
    Ok(())
  }

  fn test_find_fewer<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let index_store = self.get_index_store(
      random,
      "field",
      &[vec![0.0, 1.0], vec![1.0, 2.0], vec![0.0, 0.0]],
    )?;
    let reader = directory_reader::open(index_store.into())?;
    let searcher = new_searcher_with_reader(reader)?;
    let kvq = self.get_knn_vector_query_no_filter("field", vec![0.0, 0.0], 2)?;

    self.assert_matches(&searcher, kvq.clone(), 2)?;
    let score_docs = searcher.search(kvq, 3)?.score_docs;
    assert_eq!(2, score_docs.len());
    self.assert_id_matches(searcher.get_index_reader(), "id2", &score_docs[0])?;
    self.assert_id_matches(searcher.get_index_reader(), "id0", &score_docs[1])?;
    Ok(())
  }

  fn test_search_boost<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let index_store = self.get_index_store(
      random,
      "field",
      &[vec![0.0, 1.0], vec![1.0, 2.0], vec![0.0, 0.0]],
    )?;
    let reader = directory_reader::open(index_store.into())?;
    let searcher = new_searcher_with_reader(reader)?;

    let vector_query: Query = self
      .get_knn_vector_query_no_filter("field", vec![0.0, 0.0], 10)?
      .into();
    let score_docs = searcher.search(vector_query.clone(), 3)?.score_docs;

    let boost_query = BoostQuery::new(vector_query, 3.0)?;
    let boost_score_docs = searcher.search(boost_query, 3)?.score_docs;
    assert_eq!(score_docs.len(), boost_score_docs.len());

    for (score_doc, boost_score_doc) in score_docs.iter().zip(boost_score_docs.iter()) {
      assert_eq!(score_doc.doc, boost_score_doc.doc);
      assert!((score_doc.score * 3.0 - boost_score_doc.score).abs() <= 0.001);
    }
    Ok(())
  }

  fn test_simple_filter<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let index_store = self.get_index_store(
      random,
      "field",
      &[vec![0.0, 1.0], vec![1.0, 2.0], vec![0.0, 0.0]],
    )?;
    let reader = directory_reader::open(index_store.into())?;
    let searcher = new_searcher_with_reader(reader)?;
    let filter: Query = TermQuery::new(Term::from_text("id", "id2")).into();
    let kvq: Query = self
      .get_knn_vector_query("field", vec![0.0, 0.0], 10, Some(filter))?
      .into();
    let top_docs = searcher.search(kvq, 3)?;

    assert_eq!(1, top_docs.total_hits.value());
    self.assert_id_matches(searcher.get_index_reader(), "id2", &top_docs.score_docs[0])?;
    Ok(())
  }

  fn test_filter_with_no_vector_matches<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let index_store = self.get_index_store(
      random,
      "field",
      &[vec![0.0, 1.0], vec![1.0, 2.0], vec![0.0, 0.0]],
    )?;
    let reader = directory_reader::open(index_store.into())?;
    let searcher = new_searcher_with_reader(reader)?;

    let filter: Query = TermQuery::new(Term::from_text("other", "value")).into();
    let kvq = self.get_knn_vector_query("field", vec![0.0, 0.0], 10, Some(filter))?;
    let top_docs = searcher.search(kvq, 3)?;
    assert_eq!(0, top_docs.total_hits.value());
    Ok(())
  }

  fn test_dimension_mismatch<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let index_store = self.get_index_store(
      random,
      "field",
      &[vec![0.0, 1.0], vec![1.0, 2.0], vec![0.0, 0.0]],
    )?;
    let reader = directory_reader::open(index_store.into())?;
    let searcher = new_searcher_with_reader(reader)?;

    let kvq = self.get_knn_vector_query_no_filter("field", vec![0.0], 1)?;
    match (searcher.search(kvq, 10)).map_err(LuceneError::into_kind) {
      Err(LuceneErrorKind::IllegalArgument(msg)) => {
        assert_eq!(
          "vector query dimension: 1 differs from field dimension: 2",
          msg.to_string()
        );
      },
      _ => unreachable!("expected IllegalArgument error, got successful search result"),
    }
    Ok(())
  }

  fn test_non_vector_field<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let index_store = self.get_index_store(
      random,
      "field",
      &[vec![0.0, 1.0], vec![1.0, 2.0], vec![0.0, 0.0]],
    )?;
    let reader = directory_reader::open(index_store.into())?;
    let searcher = new_searcher_with_reader(reader)?;

    self.assert_matches(
      &searcher,
      self.get_knn_vector_query_no_filter("xyzzy", vec![0.0], 10)?,
      0,
    )?;
    self.assert_matches(
      &searcher,
      self.get_knn_vector_query_no_filter("id", vec![0.0], 10)?,
      0,
    )?;
    Ok(())
  }

  fn test_illegal_arguments(&self) -> Result<()> {
    let err = self
      .get_knn_vector_query_no_filter("xx", vec![1.0], 0)
      .unwrap_err();
    assert!(matches!(err.kind(), LuceneErrorKind::IllegalArgument(_)));
    Ok(())
  }

  fn test_different_reader<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let index_store = self.get_index_store(
      random,
      "field",
      &[vec![0.0, 1.0], vec![1.0, 2.0], vec![0.0, 0.0]],
    )?;
    let directory_result = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
      let reader = directory_reader::open(index_store.clone().into())?;
      let searcher = new_searcher_with_reader(reader)?;
      let reader_result = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
        let query = self.get_knn_vector_query_no_filter("field", vec![2.0, 3.0], 3)?;
        let rewritten = searcher.rewrite(query)?;
        let leaf_reader = searcher.get_leaf_contexts()?[0].reader().clone();
        let leaf_searcher = new_searcher_with_reader(leaf_reader)?;

        assert!(matches!(
          leaf_searcher.create_weight(rewritten, ScoreMode::Complete, 1.0),
          Err(error) if error.is_illegal_state_error()
        ));
        Ok(())
      }));
      let close_result = catch_unwind(AssertUnwindSafe(|| searcher.get_index_reader().close()));
      IOUtils::use_or_suppress_caught_result(reader_result, close_result)
    }));
    let close_result = catch_unwind(AssertUnwindSafe(|| index_store.close()));
    IOUtils::use_or_suppress_caught_result(directory_result, close_result)
  }

  fn test_score_euclidean<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let vectors = [
      vec![0.0, 0.0],
      vec![1.0, 1.0],
      vec![2.0, 2.0],
      vec![3.0, 3.0],
      vec![4.0, 4.0],
    ];
    let directory = self.get_stable_index_store(random, "field", &vectors)?;
    let reader = directory_reader::open(directory.into())?;
    let searcher = new_searcher_with_reader(reader)?;

    let query = self.get_knn_vector_query_no_filter("field", vec![2.0, 3.0], 3)?;
    let rewritten = searcher.rewrite(query.into())?;
    let weight = searcher.create_weight(rewritten, ScoreMode::Complete, 1.0)?;
    let leaf = &searcher.get_leaf_contexts()?[0];
    let mut scorer = weight.scorer(leaf, &searcher)?.unwrap();

    assert_eq!(-1, (scorer).scoring_doc_id()?);
    assert!(matches!(
      (scorer.score()).as_ref().map_err(LuceneError::kind),
      Err(LuceneErrorKind::ArrayIndexOutOfBounds(_))
    ));

    assert_eq!(1.0 / 2.0, scorer.get_max_score(2)?);
    assert_eq!(1.0 / 2.0, scorer.get_max_score(i32::MAX)?);

    assert_eq!(
      3,
      crate::core::search::doc_id_set_iterator::DocIdSetIterator::cost(&scorer)?
    );
    let first_doc = scorer.next_doc()?;
    if first_doc == 1 {
      assert_eq!(1.0 / 6.0, scorer.score()?);
      assert_eq!(3, scorer.advance(3)?);
      assert_eq!(1.0 / 2.0, scorer.score()?);
      assert_eq!(NO_MORE_DOCS, scorer.advance(4)?);
    } else {
      assert_eq!(2, first_doc);
      assert_eq!(1.0 / 2.0, scorer.score()?);
      assert_eq!(4, scorer.advance(4)?);
      assert_eq!(1.0 / 6.0, scorer.score()?);
      assert_eq!(NO_MORE_DOCS, scorer.advance(5)?);
    }

    assert!(matches!(
      (scorer.score()).as_ref().map_err(LuceneError::kind),
      Err(LuceneErrorKind::ArrayIndexOutOfBounds(_))
    ));
    Ok(())
  }

  fn test_score_cosine<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = self.new_directory_for_test(random)?;
    let writer = IndexWriter::new(directory.clone().into(), IndexWriterConfig::new()?)?;
    for j in 1..=5 {
      let mut doc = Document::new();
      doc.add(self.get_knn_vector_field_with_similarity(
        "field",
        vec![j as f32, (j * j) as f32],
        VectorSimilarityFunction::Cosine,
      )?);
      writer.add_document(doc)?;
    }
    writer.close()?;

    let reader = directory_reader::open(directory.into())?;
    let searcher = new_searcher_with_reader(reader)?;
    assert_eq!(1, searcher.get_leaf_contexts()?.len());
    let query = self.get_knn_vector_query_no_filter("field", vec![2.0, 3.0], 3)?;
    let rewritten = searcher.rewrite(query.into())?;
    let weight = searcher.create_weight(rewritten, ScoreMode::Complete, 1.0)?;
    let leaf = &searcher.get_leaf_contexts()?[0];
    let mut scorer = weight.scorer(leaf, &searcher)?.unwrap();

    assert_eq!(-1, (scorer).scoring_doc_id()?);
    assert!(matches!(
      (scorer.score()).as_ref().map_err(LuceneError::kind),
      Err(LuceneErrorKind::ArrayIndexOutOfBounds(_))
    ));

    let score0 = (1.0 + (2.0 * 1.0 + 3.0 * 1.0) / ((13.0_f32 * 2.0_f32).sqrt())) / 2.0;
    let score1 = (1.0 + (2.0 * 2.0 + 3.0 * 4.0) / ((13.0_f32 * 20.0_f32).sqrt())) / 2.0;

    assert!((score1 - scorer.get_max_score(2)?).abs() <= 0.0001);
    assert!((score1 - scorer.get_max_score(i32::MAX)?).abs() <= 0.0001);

    assert_eq!(
      3,
      crate::core::search::doc_id_set_iterator::DocIdSetIterator::cost(&scorer)?
    );
    assert_eq!(0, scorer.next_doc()?);
    assert!((score0 - scorer.score()?).abs() <= 0.0001);
    assert_eq!(1, scorer.advance(1)?);
    assert!((score1 - scorer.score()?).abs() <= 0.0001);
    assert_eq!(NO_MORE_DOCS, scorer.advance(4)?);
    assert!(matches!(
      (scorer.score()).as_ref().map_err(LuceneError::kind),
      Err(LuceneErrorKind::ArrayIndexOutOfBounds(_))
    ));
    Ok(())
  }

  fn test_score_mip<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let index_store = self.get_index_store_with_similarity(
      random,
      "field",
      VectorSimilarityFunction::MaximumInnerProduct,
      &[vec![0.0, 1.0], vec![1.0, 2.0], vec![0.0, 0.0]],
    )?;
    let reader = directory_reader::open(index_store.into())?;
    let searcher = new_searcher_with_reader(reader)?;
    let kvq = self.get_knn_vector_query_no_filter("field", vec![0.0, -1.0], 10)?;

    self.assert_matches(&searcher, kvq.clone(), 3)?;
    let score_docs = searcher.search(kvq, 3)?.score_docs;
    self.assert_id_matches(searcher.get_index_reader(), "id2", &score_docs[0])?;
    self.assert_id_matches(searcher.get_index_reader(), "id0", &score_docs[1])?;
    self.assert_id_matches(searcher.get_index_reader(), "id1", &score_docs[2])?;

    assert!((1.0 - score_docs[0].score).abs() <= 1e-7);
    assert!(((1.0 / 2.0) - score_docs[1].score).abs() <= 1e-7);
    assert!(((1.0 / 3.0) - score_docs[2].score).abs() <= 1e-7);
    Ok(())
  }

  fn test_explain<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = self.new_directory_for_test(random)?;
    let writer = IndexWriter::new(directory.clone().into(), IndexWriterConfig::new()?)?;
    for j in 0..5 {
      let mut doc = Document::new();
      doc.add(self.get_knn_vector_field("field", vec![j as f32, j as f32])?);
      writer.add_document(doc)?;
    }
    writer.close()?;

    let reader = directory_reader::open(directory.into())?;
    let searcher = new_searcher_with_reader(reader)?;
    let query = self.get_knn_vector_query_no_filter("field", vec![2.0, 3.0], 3)?;

    let matched = searcher.explain(query.clone(), 2)?;
    assert!(matched.is_match());
    assert_eq!(Some(1.0 / 2.0), matched.get_value().to_f32());
    assert_eq!(0, matched.get_details().len());
    assert_eq!("within top 3 docs", matched.get_description());

    let nomatch = searcher.explain(query, 5)?;
    assert!(!nomatch.is_match());
    assert_eq!(Some(0.0), nomatch.get_value().to_f32());
    assert_eq!(0, nomatch.get_details().len());
    assert_eq!("not in top 3 docs", nomatch.get_description());
    Ok(())
  }

  fn test_explain_multiple_segments<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = self.new_directory_for_test(random)?;
    let writer = IndexWriter::new(directory.clone().into(), IndexWriterConfig::new()?)?;
    for j in 0..5 {
      let mut doc = Document::new();
      doc.add(self.get_knn_vector_field("field", vec![j as f32, j as f32])?);
      writer.add_document(doc)?;
      writer.commit()?;
    }
    writer.close()?;

    let reader = directory_reader::open(directory.into())?;
    let searcher = new_searcher_with_reader(reader)?;
    let query = self.get_knn_vector_query_no_filter("field", vec![2.0, 3.0], 3)?;

    let matched = searcher.explain(query.clone(), 2)?;
    assert!(matched.is_match());
    assert_eq!(Some(1.0 / 2.0), matched.get_value().to_f32());
    assert_eq!(0, matched.get_details().len());
    assert_eq!("within top 3 docs", matched.get_description());

    let nomatch = searcher.explain(query, 4)?;
    assert!(!nomatch.is_match());
    assert_eq!(Some(0.0), nomatch.get_value().to_f32());
    assert_eq!(0, nomatch.get_details().len());
    assert_eq!("not in top 3 docs", nomatch.get_description());
    Ok(())
  }

  fn test_skewed_index<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = self.new_directory_for_test(random)?;
    let writer = IndexWriter::new(directory.clone().into(), IndexWriterConfig::new()?)?;
    let mut r = 0;
    for _ in 0..5 {
      for _ in 0..5 {
        let mut doc = Document::new();
        doc.add(self.get_knn_vector_field("field", vec![r as f32, r as f32])?);
        doc.add(StringField::from_string(
          "id",
          format!("id{r}"),
          Store::Yes,
        )?);
        writer.add_document(doc)?;
        r += 1;
      }
      writer.flush()?;
    }
    writer.close()?;

    let reader = directory_reader::open(directory.into())?;
    let searcher = new_searcher_with_reader(reader)?;

    let mut results = searcher.search(
      self.get_knn_vector_query_no_filter("field", vec![0.0, 0.0], 8)?,
      10,
    )?;
    assert_eq!(8, results.score_docs.len());
    self.assert_id_matches(searcher.get_index_reader(), "id0", &results.score_docs[0])?;
    self.assert_id_matches(searcher.get_index_reader(), "id7", &results.score_docs[7])?;

    results = searcher.search(
      self.get_knn_vector_query_no_filter("field", vec![10.0, 10.0], 8)?,
      10,
    )?;
    assert_eq!(8, results.score_docs.len());
    self.assert_id_matches(searcher.get_index_reader(), "id10", &results.score_docs[0])?;
    self.assert_id_matches(searcher.get_index_reader(), "id6", &results.score_docs[7])?;
    Ok(())
  }

  fn test_random<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_docs = at_least_usize(random, 100);
    let dimension = at_least_usize(random, 5);
    let num_iters = at_least_usize(random, 10);
    let every_doc_has_a_vector = random.random_bool(0.5);

    let directory = self.new_directory_for_test(random)?;
    let writer = RandomIndexWriter::new(random, directory.clone().into())?;
    let mut num_docs_with_vectors = 0usize;
    for _ in 0..num_docs {
      let mut doc = Document::new();
      if every_doc_has_a_vector || random.random_range(0..10) != 2 {
        doc.add(self.get_knn_vector_field("field", self.random_vector(random, dimension))?);
        num_docs_with_vectors += 1;
      }
      writer.add_document(random, doc)?;
    }
    writer.close(random)?;

    let reader = directory_reader::open(directory.into())?;
    let searcher = new_searcher_with_reader(reader)?;
    for _ in 0..num_iters {
      let k = random.random_range(1..=80);
      let query =
        self.get_knn_vector_query_no_filter("field", self.random_vector(random, dimension), k)?;
      let n = random.random_range(1..=100);
      let results = searcher.search(query, n)?;
      let expected = n.min(k).min(num_docs_with_vectors);
      assert!(!searcher.get_index_reader().has_deletions()?);
      assert_eq!(expected, results.score_docs.len());
      assert!(results.total_hits.value() >= results.score_docs.len());

      let mut last = f32::MAX;
      for score_doc in results.score_docs {
        assert!(score_doc.score <= last);
        last = score_doc.score;
      }
    }

    Ok(())
  }

  fn test_random_with_filter<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
    Self::Directory: 'static,
  {
    let num_docs = 1000usize;
    let dimension = at_least_usize(random, 5);
    let num_iters = at_least_usize(random, 10);
    let directory = self.new_directory_for_test(random)?;
    // Always use the default kNN format to have predictable behavior around when it hits
    // visitedLimit. This is fine since the test targets AbstractKnnVectorQuery logic, not the kNN
    // format implementation.
    let mut iwc = IndexWriterConfig::new()?;
    iwc.set_codec(TestUtil::get_default_codec());
    let writer = RandomIndexWriter::with_config(random, Arc::new(directory.clone()), iwc);
    for i in 0..num_docs {
      let mut doc = Document::new();
      doc.add(self.get_knn_vector_field("field", self.random_vector(random, dimension))?);
      doc.add(NumericDocValuesField::new("tag", i as i64));
      doc.add(IntPoint::new("tag", [i as i32])?);
      writer.add_document(random, doc)?;
    }
    writer.force_merge(random, 1)?;
    writer.close(random)?;

    let reader = directory_reader::open(Arc::new(directory.clone()))?;
    let searcher = new_searcher_with_reader(reader)?;
    for _ in 0..num_iters {
      let lower = random.random_range(0..500);

      // Test a filter with cost less than k and check we use exact search.
      let filter1: Query = IntPoint::new_range_query("tag", lower, lower + 8)?.into();
      let results = searcher.search(
        self.get_knn_vector_query(
          "field",
          self.random_vector(random, dimension),
          10,
          Some(filter1.clone()),
        )?,
        num_docs,
      )?;
      assert_eq!(9, results.total_hits.value());
      assert_eq!(results.total_hits.value(), results.score_docs.len());
      match searcher.search(
        self.get_throwing_knn_vector_query(
          "field",
          self.random_vector(random, dimension),
          10,
          Some(filter1),
        )?,
        num_docs,
      ) {
        Err(error) if matches!(error.kind(), LuceneErrorKind::UnsupportedOperation(_)) => {},
        Err(error) => return Err(error),
        Ok(_) => panic!("exact search should not be supported"),
      }

      // Test a restrictive filter and check we use exact search.
      let filter2: Query = IntPoint::new_range_query("tag", lower, lower + 6)?.into();
      let results = searcher.search(
        self.get_knn_vector_query(
          "field",
          self.random_vector(random, dimension),
          5,
          Some(filter2.clone()),
        )?,
        num_docs,
      )?;
      assert_eq!(5, results.total_hits.value());
      assert_eq!(results.total_hits.value(), results.score_docs.len());
      match searcher.search(
        self.get_throwing_knn_vector_query(
          "field",
          self.random_vector(random, dimension),
          5,
          Some(filter2),
        )?,
        num_docs,
      ) {
        Err(error) if matches!(error.kind(), LuceneErrorKind::UnsupportedOperation(_)) => {},
        Err(error) => return Err(error),
        Ok(_) => panic!("exact search should not be supported"),
      }

      // Test an unrestrictive filter and check we use approximate search.
      let filter3: Query = IntPoint::new_range_query("tag", lower, num_docs as i32)?.into();
      let sort = Sort::with_fields(vec![SortField::new(Some("tag"), SortFieldType::Int)?])?;
      let results = searcher.search_with_sort(
        self.get_throwing_knn_vector_query(
          "field",
          self.random_vector(random, dimension),
          5,
          Some(filter3),
        )?,
        num_docs,
        sort,
      )?;
      assert_eq!(5, results.base.total_hits.value());
      assert_eq!(
        results.base.total_hits.value(),
        results.base.score_docs.len()
      );
      for score_doc in &results.base.score_docs {
        let TopFieldScoreDoc::Field(field_doc) = score_doc else {
          panic!("sorted search should return field docs");
        };
        assert_eq!(1, field_doc.fields.len());
        let tag = *field_doc.fields[0]
          .as_i32()
          .expect("tag sort value should be an i32");
        assert!(lower <= tag && tag <= num_docs as i32);
      }

      // Test a filter that exhausts visitedLimit in upper levels, and switches to exact search.
      let filter4: Query = IntPoint::new_range_query("tag", lower, lower + 2)?.into();
      match searcher.search(
        self.get_throwing_knn_vector_query(
          "field",
          self.random_vector(random, dimension),
          1,
          Some(filter4),
        )?,
        num_docs,
      ) {
        Err(error) if matches!(error.kind(), LuceneErrorKind::UnsupportedOperation(_)) => {},
        Err(error) => return Err(error),
        Ok(_) => panic!("exact search should not be supported"),
      }
    }

    searcher.get_index_reader().close()?;
    directory.close()?;
    Ok(())
  }

  fn test_filter_with_same_score<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_docs = 100usize;
    let dimension = at_least_usize(random, 5);
    let size = 5usize;
    let directory = self.new_directory_for_test(random)?;
    let mut iwc = IndexWriterConfig::new()?;
    iwc.set_codec(TestUtil::get_default_codec());
    let writer = IndexWriter::new(directory.clone().into(), iwc)?;
    let vector = self.random_vector(random, dimension);

    for i in 0..num_docs {
      let mut doc = Document::new();
      doc.add(self.get_knn_vector_field("field", vector.clone())?);
      doc.add(IntPoint::new("tag", [i as i32])?);
      writer.add_document(doc)?;
    }
    writer.force_merge(1)?;
    writer.close()?;

    let reader = directory_reader::open(directory.into())?;
    let searcher = new_searcher_with_reader(reader)?;
    let lower = random.random_range(0..50);

    let filter1: Query = IntPoint::new_range_query("tag", lower, lower + 6)?.into();
    let results = searcher.search(
      self.get_knn_vector_query(
        "field",
        self.random_vector(random, dimension),
        size,
        Some(filter1),
      )?,
      size,
    )?;
    assert_eq!(size, results.score_docs.len());

    let filter2: Query = IntPoint::new_range_query("tag", lower, num_docs as i32)?.into();
    let results = searcher.search(
      self.get_knn_vector_query(
        "field",
        self.random_vector(random, dimension),
        size,
        Some(filter2),
      )?,
      size,
    )?;
    assert_eq!(size, results.score_docs.len());
    Ok(())
  }

  fn test_deletes<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = self.new_directory_for_test(random)?;
    let writer = IndexWriter::new(directory.clone().into(), IndexWriterConfig::new()?)?;
    let num_docs = at_least_usize(random, 120);
    let dim = 30usize;
    for i in 0..num_docs {
      let mut doc = Document::new();
      doc.add(StringField::from_string(
        "index",
        i.to_string(),
        Store::Yes,
      )?);
      if i % 5 != 0 {
        doc.add(self.get_knn_vector_field("vector", self.random_vector(random, dim))?);
      }
      writer.add_document(doc)?;
    }
    writer.commit()?;

    let mut to_delete = HashSet::new();
    for _ in 0..25 {
      let index = random.random_range(0..num_docs);
      to_delete.insert(index.to_string());
    }
    let delete_terms: Vec<Term> = to_delete
      .iter()
      .map(|index| Term::from_text("index", index.as_str()))
      .collect();
    writer.delete_documents_with_terms(delete_terms)?;
    writer.commit()?;
    writer.close()?;

    let hits = 50usize;
    let reader = directory_reader::open(directory.into())?;
    let searcher = new_searcher_with_reader(reader)?;
    let query =
      self.get_knn_vector_query_no_filter("vector", self.random_vector(random, dim), hits)?;
    let top_docs = searcher.search(query, num_docs)?;
    let mut stored_fields = searcher.get_index_reader().stored_fields()?;
    let mut all_ids = HashSet::with_capacity(top_docs.score_docs.len());
    for score_doc in top_docs.score_docs {
      let doc = stored_fields.document(score_doc.doc)?;
      let index = doc
        .get("index")?
        .map(|value| value.into_owned())
        .expect("stored index should exist");
      assert!(
        !to_delete.contains(&index),
        "search returned a deleted document: {index}"
      );
      all_ids.insert(index);
    }
    assert_eq!(hits, all_ids.len(), "search missed some documents");
    Ok(())
  }

  fn test_all_deletes<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = self.new_directory_for_test(random)?;
    let writer = IndexWriter::new(directory.clone().into(), IndexWriterConfig::new()?)?;
    let num_docs = at_least_usize(random, 100);
    let dim = 30usize;
    for _ in 0..num_docs {
      let mut doc = Document::new();
      doc.add(self.get_knn_vector_field("vector", self.random_vector(random, dim))?);
      writer.add_document(doc)?;
    }
    writer.commit()?;
    writer.delete_documents_with_queries(vec![MatchAllDocsQuery::new().into()])?;
    writer.commit()?;
    writer.close()?;

    let reader = directory_reader::open(directory.into())?;
    let searcher = new_searcher_with_reader(reader)?;
    let query =
      self.get_knn_vector_query_no_filter("vector", self.random_vector(random, dim), num_docs)?;
    let top_docs = searcher.search(query, num_docs)?;
    assert_eq!(0, top_docs.score_docs.len());
    Ok(())
  }

  fn test_merge_away_all_values<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dim = 30usize;
    let directory = self.new_directory_for_test(random)?;
    let writer = IndexWriter::new(directory.clone().into(), IndexWriterConfig::new()?)?;

    let mut doc = Document::new();
    doc.add(StringField::from_string("id", "0", Store::No)?);
    writer.add_document(doc)?;

    let mut doc = Document::new();
    doc.add(StringField::from_string("id", "1", Store::No)?);
    doc.add(self.get_knn_vector_field("field", self.random_vector(random, dim))?);
    writer.add_document(doc)?;
    writer.commit()?;
    writer.delete_documents_with_terms(vec![Term::from_text("id", "1")])?;
    writer.force_merge(1)?;
    writer.close()?;

    let reader = directory_reader::open(directory.into())?;
    let leaf_reader = get_only_leaf_reader(&reader)?;
    let field_info = leaf_reader
      .get_field_infos()?
      .field_info_by_name("field")?
      .cloned();
    assert!(field_info.is_some());
    let field_info = field_info.unwrap();

    match field_info.get_vector_encoding() {
      crate::core::index::vector_encoding::VectorEncoding::BYTE(_) => {
        let vector_values = leaf_reader
          .get_byte_vector_values("field")?
          .expect("vector values");
        assert_eq!(NO_MORE_DOCS, vector_values.iterator()?.next_doc()?);
      },
      crate::core::index::vector_encoding::VectorEncoding::FLOAT32(_) => {
        let vector_values = leaf_reader
          .get_float_vector_values("field")?
          .expect("vector values");
        assert_eq!(NO_MORE_DOCS, vector_values.iterator()?.next_doc()?);
      },
    }
    Ok(())
  }

  /// Check that the query behaves reasonably when using a custom filter reader where there are no
  /// live docs.
  fn test_no_live_docs_reader<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = self.new_directory_for_test(random)?;
    let directory_result = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
      let writer = IndexWriter::new(directory.clone().into(), IndexWriterConfig::new()?)?;
      let writer_result = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
        let num_docs = 10usize;
        let dim = 30usize;
        for i in 0..num_docs {
          let mut doc = Document::new();
          doc.add(StringField::from_string("index", i.to_string(), Store::No)?);
          doc.add(self.get_knn_vector_field("vector", self.random_vector(random, dim))?);
          writer.add_document(doc)?;
        }
        writer.commit()?;

        let reader = directory_reader::open(directory.clone().into())?;
        let wrapped_reader = NoLiveDocsDirectoryReader::new(reader)?;
        let searcher = new_searcher_with_reader(wrapped_reader)?;
        let reader_result = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
          let query = self.get_knn_vector_query_no_filter(
            "vector",
            self.random_vector(random, dim),
            num_docs,
          )?;
          let top_docs = searcher.search(query, num_docs)?;
          assert_eq!(0, top_docs.score_docs.len());
          Ok(())
        }));
        let close_result = catch_unwind(AssertUnwindSafe(|| searcher.get_index_reader().close()));
        IOUtils::use_or_suppress_caught_result(reader_result, close_result)
      }));
      let close_result = catch_unwind(AssertUnwindSafe(|| writer.close()));
      IOUtils::use_or_suppress_caught_result(writer_result, close_result)
    }));
    let close_result = catch_unwind(AssertUnwindSafe(|| directory.close()));
    IOUtils::use_or_suppress_caught_result(directory_result, close_result)
  }

  fn test_bit_set_query<R>(&self, _random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    // TODO IMPORTANT BitSet filter reuse and ThrowingBitSetQuery are not implemented.
    Ok(())
  }

  fn test_time_limiting_knn_collector_manager<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let index_store = self.get_index_store(
      random,
      "field",
      &[vec![0.0, 1.0], vec![1.0, 2.0], vec![0.0, 0.0]],
    )?;
    let reader = directory_reader::open(index_store.into())?;
    let searcher = new_searcher_with_reader(reader)?;
    let leaf = &searcher.get_leaf_contexts()?[0];

    let delegate = TopKnnCollectorManager::new(3, &searcher)?;
    let no_timeout_manager =
      TimeLimitingKnnCollectorManager::new(delegate, None::<QueryTimeoutEnum>);
    let mut no_timeout_collector = no_timeout_manager.new_collector(usize::MAX, leaf)?;
    no_timeout_collector.collect(0, 0.0)?;
    assert!(!no_timeout_collector.early_terminated());
    let no_timeout_top_docs = no_timeout_collector.top_docs()?;
    assert_eq!(EqualTo, no_timeout_top_docs.total_hits.relation());
    assert_eq!(1, no_timeout_top_docs.score_docs.len());

    let delegate = TopKnnCollectorManager::new(3, &searcher)?;
    let timeout_manager =
      TimeLimitingKnnCollectorManager::new(delegate, Some(QueryTimeoutEnum::custom(AlwaysTimeout)));
    let mut timeout_collector = timeout_manager.new_collector(usize::MAX, leaf)?;
    timeout_collector.collect(0, 0.0)?;
    assert!(timeout_collector.early_terminated());
    let timeout_top_docs = timeout_collector.top_docs()?;
    assert_eq!(GreaterThanOrEqualTo, timeout_top_docs.total_hits.relation());
    assert_eq!(1, timeout_top_docs.score_docs.len());
    Ok(())
  }

  fn test_timeout<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let index_store = self.get_index_store(
      random,
      "field",
      &[vec![0.0, 1.0], vec![1.0, 2.0], vec![0.0, 0.0]],
    )?;
    let reader = directory_reader::open(index_store.into())?;
    let mut searcher = new_searcher_with_reader(reader)?;

    let query = self.get_knn_vector_query_no_filter("field", vec![0.0, 1.0], 2)?;
    let exact_query = self.get_knn_vector_query(
      "field",
      vec![0.0, 1.0],
      10,
      Some(MatchAllDocsQuery::new().into()),
    )?;

    assert_eq!(2, searcher.count(query.clone())?);
    assert_eq!(3, searcher.count(exact_query.clone())?);

    searcher.set_timeout(QueryTimeoutEnum::custom(AlwaysTimeout));
    assert_eq!(0, searcher.count(query.clone())?);
    assert_eq!(0, searcher.count(exact_query.clone())?);

    searcher.set_timeout(QueryTimeoutEnum::custom(CountingQueryTimeout::new(1)));
    assert!(searcher.count(query)? <= 1);

    searcher.set_timeout(QueryTimeoutEnum::custom(CountingQueryTimeout::new(1)));
    assert_eq!(1, searcher.count(exact_query)?);
    Ok(())
  }

  fn test_same_field_different_formats<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = self.new_directory_for_test(random)?;
    let format1 = random_vector_format(
      random,
      &crate::core::index::vector_encoding::VectorEncoding::FLOAT32(4),
    )?;
    let format2 = random_vector_format(
      random,
      &crate::core::index::vector_encoding::VectorEncoding::FLOAT32(4),
    )?;

    let mut config = IndexWriterConfig::new()?;
    config.set_codec(TestUtil::always_knn_vectors_format(format1));
    let writer = IndexWriter::new(directory.clone().into(), config)?;
    let mut doc = Document::new();
    doc.add(self.get_knn_vector_field("field1", vec![1.0, 1.0, 1.0])?);
    writer.add_document(&mut doc)?;
    doc.clear();
    doc.add(self.get_knn_vector_field("field1", vec![1.0, 2.0, 3.0])?);
    writer.add_document(&mut doc)?;
    writer.commit()?;
    writer.close()?;

    let mut config = IndexWriterConfig::new()?;
    config.set_codec(TestUtil::always_knn_vectors_format(format2));
    let writer = IndexWriter::new(directory.clone().into(), config)?;
    let mut doc = Document::new();
    doc.clear();
    doc.add(self.get_knn_vector_field("field1", vec![1.0, 1.0, 2.0])?);
    writer.add_document(&mut doc)?;
    doc.clear();
    doc.add(self.get_knn_vector_field("field1", vec![4.0, 5.0, 6.0])?);
    writer.add_document(&mut doc)?;
    writer.commit()?;
    writer.close()?;

    let reader = directory_reader::open(directory.into())?;
    let searcher = new_searcher_with_reader(reader)?;
    let query = self.get_knn_vector_query_no_filter("field1", vec![1.0, 2.0, 3.0], 10)?;
    let hits = searcher.search(query, 4)?;
    assert_eq!(4, hits.score_docs.len());
    Ok(())
  }

  fn get_index_store<R>(
    &self,
    random: &mut R,
    field: &str,
    contents: &[Vec<f32>],
  ) -> Result<Self::Directory>
  where
    R: Rng + ?Sized,
  {
    self.get_index_store_with_similarity(
      random,
      field,
      VectorSimilarityFunction::Euclidean,
      contents,
    )
  }

  fn get_index_store_with_similarity<R>(
    &self,
    random: &mut R,
    field: &str,
    vector_similarity_function: VectorSimilarityFunction,
    contents: &[Vec<f32>],
  ) -> Result<Self::Directory>
  where
    R: Rng + ?Sized,
  {
    let index_store = self.new_directory_for_test(random)?;
    let writer = RandomIndexWriter::new(random, index_store.clone().into())?;

    for (i, vector) in contents.iter().enumerate() {
      let mut doc = Document::new();
      doc.add(self.get_knn_vector_field_with_similarity(
        field,
        vector.clone(),
        vector_similarity_function,
      )?);
      doc.add(StringField::from_string(
        "id",
        format!("id{i}"),
        Store::Yes,
      )?);
      writer.add_document(random, doc)?;
      if random.random_bool(0.5) {
        for j in 0..TestUtil::next_usize(random, 1, 5) {
          let mut doc = Document::new();
          doc.add(StringField::from_string("other", "value", Store::No)?);
          doc.add(StringField::from_string(
            "id",
            format!("id{j}"),
            Store::Yes,
          )?);
          writer.add_document(random, doc)?;
        }
      }
    }

    for _ in 0..5 {
      let mut doc = Document::new();
      doc.add(StringField::from_string("other", "value", Store::No)?);
      writer.add_document(random, doc)?;
    }

    writer.close(random)?;
    Ok(index_store)
  }

  fn get_stable_index_store<R>(
    &self,
    random: &mut R,
    field: &str,
    contents: &[Vec<f32>],
  ) -> Result<Self::Directory>
  where
    R: Rng + ?Sized,
  {
    let index_store = self.new_directory_for_test(random)?;
    let writer = IndexWriter::new(index_store.clone().into(), IndexWriterConfig::new()?)?;

    for (i, vector) in contents.iter().enumerate() {
      let mut doc = Document::new();
      doc.add(self.get_knn_vector_field(field, vector.clone())?);
      doc.add(StringField::from_string(
        "id",
        format!("id{i}"),
        Store::Yes,
      )?);
      writer.add_document(doc)?;
    }

    for _ in 0..5 {
      let mut doc = Document::new();
      doc.add(StringField::from_string("other", "value", Store::No)?);
      writer.add_document(doc)?;
    }

    writer.close()?;
    Ok(index_store)
  }

  fn assert_matches<IRC, T, SIM>(
    &self,
    searcher: &IndexSearcher<IRC, SIM>,
    q: T,
    expected_matches: usize,
  ) -> Result<()>
  where
    SIM: crate::core::search::similarities_impl::similarities::Similarity,
    IRC: crate::core::index::index_reader_context::IndexReaderContext + Sync,
    T: Into<Query>,
    crate::core::search::index_searcher::IndexSearcher<IRC, SIM>: Sync,
  {
    let result = searcher.search(q, 1000)?.score_docs;
    assert_eq!(expected_matches, result.len());
    Ok(())
  }

  fn assert_id_matches<IR>(
    &self,
    reader: &IR,
    expected_id: &str,
    score_doc: &ScoreDoc,
  ) -> Result<()>
  where
    IR: IndexReader,
  {
    let document = reader.stored_fields()?.document(score_doc.doc)?;
    let actual_id = document.get("id")?;
    assert_eq!(Some(expected_id), actual_id.as_deref());
    Ok(())
  }

  fn assert_doc_score_query_to_string(&self, query: &Query) -> Result<()> {
    let query_string = query.to_string("ignored")?;
    assert!(query_string.starts_with("DocAndScoreQuery["));
    assert!(query_string.contains(",...]["));
    assert!(query_string.contains(",...],"));
    assert!(
      query_string.ends_with(",1") || query_string.ends_with(",1.0"),
      "unexpected query string: {query_string}"
    );
    Ok(())
  }
}

struct NoLiveDocsSubReaderWrapper;

impl<LR> SubReaderWrapper<LR> for NoLiveDocsSubReaderWrapper
where
  LR: LeafReader + Clone,
{
  type LeafReader1 = Self::LeafReader2;

  fn wrap_readers(&self, readers: &[LR]) -> Result<Vec<Self::LeafReader1>> {
    self.default_wrap_readers(readers.to_vec())
  }

  type LeafReader2 = NoLiveDocsLeafReader<LR>;

  fn wrap(&self, reader: LR) -> Result<Self::LeafReader2> {
    NoLiveDocsLeafReader::new(reader)
  }
}

/// A version of [`AbstractKnnVectorQuery`] that returns an error when an exact search is run.
/// This allows us to check what search strategy is being used.
struct NoLiveDocsDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  in_: DR,
  base: BaseCompositeReaderBase<NoLiveDocsLeafReader<DR::LeafReader>>,
  index_base: IndexReaderBase,
}

impl<DR> NoLiveDocsDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  fn new(in_: DR) -> Result<Self> {
    let result = catch_unwind(AssertUnwindSafe(|| -> Result<_> {
      let wrapper = NoLiveDocsSubReaderWrapper;
      let readers = wrapper.wrap_readers(in_.get_sequential_sub_readers())?;
      let index_base = IndexReaderBase::new();
      let base = BaseCompositeReaderBase::new::<DummyComparator>(readers, None, &index_base)?;
      Ok((base, index_base))
    }));
    match result {
      Ok(Ok((base, index_base))) => Ok(Self {
        in_,
        base,
        index_base,
      }),
      result => {
        let close_result = catch_unwind(AssertUnwindSafe(|| in_.close()));
        IOUtils::use_or_suppress_caught_result(result, close_result)?;
        unreachable!()
      },
    }
  }
}

impl<DR> BaseCompositeReader for NoLiveDocsDirectoryReader<DR> where DR: DirectoryReader {}

impl<DR> CompositeReader for NoLiveDocsDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  type LeafReader = NoLiveDocsLeafReader<DR::LeafReader>;
  type SubReader = Self::LeafReader;

  fn get_sequential_sub_readers(&self) -> &[Self::SubReader] {
    self.base.get_sequential_sub_readers()
  }

  fn visit_leaves<F>(&self, visitor: &mut F) -> Result<()>
  where
    F: FnMut(&Self::LeafReader) -> Result<()>,
  {
    for reader in self.get_sequential_sub_readers() {
      visitor(reader)?;
    }
    Ok(())
  }

  fn to_string(&self) -> String {
    format!("NoLiveDocsDirectoryReader({})", self.in_.to_string())
  }
}

impl<DR> IndexReader for NoLiveDocsDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  type ContextKind = CompositeReaderContextKind;

  type TermVectors = BCRTermVectorsImpl<<Self as CompositeReader>::LeafReader>;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    self.base.term_vector(self)
  }

  fn max_doc(&self) -> Result<i32> {
    Ok(self.base.max_doc())
  }

  fn num_docs(&self) -> Result<i32> {
    self.base.num_docs()
  }

  type StoredFields = BCRStoredFieldsImpl<<Self as CompositeReader>::LeafReader>;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    self.base.stored_fields(self)
  }

  fn do_close(&self) -> Result<()> {
    self.in_.close()
  }

  type ReaderCacheHelper = DR::ReaderCacheHelper;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    self.in_.get_reader_cache_helper()
  }

  fn doc_freq(&self, term: &Term) -> Result<i32> {
    self.base.doc_freq(term, self)
  }

  fn total_term_freq(&self, term: &Term) -> Result<i64> {
    self.base.total_term_freq(term, self)
  }

  fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
    self.base.get_sum_doc_freq(field, self)
  }

  fn get_doc_count(&self, field: &str) -> Result<i32> {
    self.base.get_doc_count(field, self)
  }

  fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
    self.base.get_sum_total_term_freq(field, self)
  }

  fn index_base(&self) -> &IndexReaderBase {
    &self.index_base
  }
}

impl<DR> Display for NoLiveDocsDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", CompositeReader::to_string(self))
  }
}

impl<DR> DirectoryReader for NoLiveDocsDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  type DirectoryReader = NoLiveDocsDirectoryReader<DR::DirectoryReader>;

  fn do_open_if_changed(&self) -> Result<Option<Self::DirectoryReader>> {
    self.wrap_directory_reader(self.in_.do_open_if_changed()?)
  }

  fn do_open_if_changed_with_commit<IC>(
    &self,
    commit: Option<&IC>,
  ) -> Result<Option<Self::DirectoryReader>>
  where
    IC: IndexCommit<Directory = Arc<Self::Directory>>,
  {
    self.wrap_directory_reader(self.in_.do_open_if_changed_with_commit(commit)?)
  }

  fn do_open_if_changed_with_deletes(
    &self,
    writer: &Arc<IndexWriter<Self::Directory>>,
    apply_deletes: bool,
  ) -> Result<Option<Self::DirectoryReader>> {
    self.wrap_directory_reader(
      self
        .in_
        .do_open_if_changed_with_deletes(writer, apply_deletes)?,
    )
  }

  fn get_version(&self) -> Result<i64> {
    self.in_.get_version()
  }

  fn is_current(&self) -> Result<bool> {
    self.in_.is_current()
  }

  type IndexCommit = DR::IndexCommit;

  fn get_index_commit(&self) -> Result<Self::IndexCommit> {
    self.in_.get_index_commit()
  }

  type Directory = DR::Directory;

  fn directory(&self) -> &DirectoryReaderBase<Self::Directory> {
    self.in_.directory()
  }
}

impl<DR> FilterDirectoryReader for NoLiveDocsDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  type Delegate = DR;

  fn get_delegate(&self) -> &Self::Delegate {
    &self.in_
  }

  type WrapDirectoryReader = NoLiveDocsDirectoryReader<DR::DirectoryReader>;

  fn do_wrap_directory_reader(
    &self,
    in_: Option<<Self::Delegate as DirectoryReader>::DirectoryReader>,
  ) -> Result<Option<Self::WrapDirectoryReader>> {
    in_.map(NoLiveDocsDirectoryReader::new).transpose()
  }
}

struct NoLiveDocsLeafReader<LR>
where
  LR: LeafReader,
{
  in_: LR,
  live_docs: MatchNoBits,
  index_base: IndexReaderBase,
}

impl<LR> NoLiveDocsLeafReader<LR>
where
  LR: LeafReader,
{
  fn new(in_: LR) -> Result<Self> {
    let index_base = IndexReaderBase::new();
    in_.register_parent_reader(&index_base)?;
    let live_docs = MatchNoBits::new(in_.max_doc()? as usize);
    Ok(Self {
      in_,
      live_docs,
      index_base,
    })
  }
}

impl<LR> Clone for NoLiveDocsLeafReader<LR>
where
  LR: LeafReader + Clone,
{
  fn clone(&self) -> Self {
    Self {
      in_: self.in_.clone(),
      live_docs: self.live_docs.clone(),
      index_base: self.index_base.clone(),
    }
  }
}

impl<LR> Display for NoLiveDocsLeafReader<LR>
where
  LR: LeafReader,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "NoLiveDocsLeafReader({})", self.in_)
  }
}

impl<LR> IndexReader for NoLiveDocsLeafReader<LR>
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
    Ok(0)
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
    self.in_.get_reader_cache_helper()
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

impl<LR> LeafReader for NoLiveDocsLeafReader<LR>
where
  LR: LeafReader,
{
  type CacheHelper = LR::CacheHelper;

  fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
    self.in_.get_core_cache_helper()
  }

  type Terms = LR::Terms;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    self.in_.terms(field)
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

  fn get_field_infos(&self) -> Result<Arc<FieldInfos>> {
    self.in_.get_field_infos()
  }

  type Bits = MatchNoBits;

  fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
    Ok(Some(self.live_docs.clone()))
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

impl<LR> FilterLeafReader for NoLiveDocsLeafReader<LR> where LR: LeafReader {}

struct AlwaysTimeout;

impl QueryTimeout for AlwaysTimeout {
  fn should_exit(&self) -> bool {
    true
  }
}

struct CountingQueryTimeout {
  remaining: AtomicUsize,
}

impl CountingQueryTimeout {
  fn new(limit: usize) -> Self {
    Self {
      remaining: AtomicUsize::new(limit),
    }
  }
}

impl QueryTimeout for CountingQueryTimeout {
  fn should_exit(&self) -> bool {
    let previous = self
      .remaining
      .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
        if remaining > 0 {
          Some(remaining - 1)
        } else {
          None
        }
      })
      .unwrap_or_else(|remaining| remaining);
    previous == 0
  }
}

use crate::core::search::weight::Weight;
