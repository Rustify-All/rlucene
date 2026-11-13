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
use crate::core::search::query::QueryRef;

use crate::core::document::document::Document;
use crate::core::document::field::Store;
use crate::core::document::fields::Fields;
use crate::core::document::int_field::IntField;
use crate::core::index::directory_reader;
use crate::core::index::index_reader::Identity;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::query_timeout::{QueryTimeout, QueryTimeoutEnum};
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term::Term;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::search::abstract_vector_similarity_query::AbstractVectorSimilarityQuery;
use crate::core::search::boost_query::BoostQuery;
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, NO_MORE_DOCS};
use crate::core::search::explanation::Explanation;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::query::{Query, QueryBase, QueryWeight, QueryWeightSs};
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::term_query::TermQuery;
use crate::core::search::weight::Weight;
use crate::core::store::directory::{DirEnum, Directory};
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind, Result};
use crate::core::util::hnsw::hnsw_util::HnswUtil;
use crate::core::util::{CoreHelper, HasIdentity};
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::util::lucene_test_case::{
  at_least_usize, new_directory_shared, new_index_writer_config, new_searcher_with_reader,
};
use crate::test_framework::f32_equals;
use rand::{Rng, RngExt};
use std::collections::{HashMap, HashSet};
use std::fmt::Debug;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct BaseVectorSimilarityQueryBase {
  pub(crate) vector_field: String,
  pub(crate) id_field: String,
  pub function: VectorSimilarityFunction,
  pub(crate) num_docs: usize,
  pub(crate) dim: usize,
}
impl BaseVectorSimilarityQueryBase {
  pub fn new(
    vector_field: String,
    id_field: String,
    function: VectorSimilarityFunction,
    num_docs: usize,
    dim: usize,
  ) -> Self {
    BaseVectorSimilarityQueryBase {
      vector_field,
      id_field,
      function,
      num_docs,
      dim,
    }
  }
}
pub trait BaseVectorSimilarityQueryTestCase {
  type Vector: Clone + Debug;
  type VectorQuery: AbstractVectorSimilarityQuery + Clone + PartialEq + Eq + Debug + Into<Query>;
  type Directory: Directory + Clone + Into<Arc<DirEnum>>;

  fn get_base(&self) -> &BaseVectorSimilarityQueryBase;
  fn get_base_mut(&mut self) -> &mut BaseVectorSimilarityQueryBase;

  fn get_random_vector<R>(&self, random: &mut R, dim: usize) -> Self::Vector
  where
    R: Rng + ?Sized;

  fn compare(&self, vector1: &Self::Vector, vector2: &Self::Vector) -> Result<f32>;

  fn check_equals(&self, vector1: &Self::Vector, vector2: &Self::Vector) -> bool;

  fn get_vector_field(
    &self,
    name: &str,
    vector: Self::Vector,
    function: VectorSimilarityFunction,
  ) -> Result<Fields>;

  fn get_vector_query(
    &self,
    field: &str,
    vector: Self::Vector,
    traversal_similarity: f32,
    result_similarity: f32,
    filter: Option<Query>,
  ) -> Result<Self::VectorQuery>;

  fn new_directory_for_test<R>(&self, random: &mut R) -> Result<Self::Directory>
  where
    R: Rng + ?Sized;

  fn default_new_directory_for_test<R>(&self, random: &mut R) -> Result<Arc<DirEnum>>
  where
    R: Rng + ?Sized,
  {
    new_directory_shared(random)
  }

  fn get_throwing_vector_query(
    &self,
    field: &str,
    vector: Self::Vector,
    traversal_similarity: f32,
    result_similarity: f32,
    filter: Option<Query>,
  ) -> Result<Self::VectorQuery>;

  fn test_equals<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dim = self.get_base().dim;
    let field1 = "f1";
    let field2 = "f2";

    let vector1 = self.get_random_vector(random, dim);
    let mut vector2;
    loop {
      vector2 = self.get_random_vector(random, dim);
      if !self.check_equals(&vector1, &vector2) {
        break;
      }
    }

    let traversal_similarity1 = 0.3;
    let traversal_similarity2 = 0.4;
    let result_similarity1 = 0.4;
    let result_similarity2 = 0.5;

    let filter1: Query = TermQuery::new(Term::from_text("t1", "v1")).into();
    let filter2: Query = TermQuery::new(Term::from_text("t2", "v2")).into();

    let query = self.get_vector_query(
      field1,
      vector1.clone(),
      traversal_similarity1,
      result_similarity1,
      Some(filter1.clone()),
    )?;

    assert_eq!(
      query,
      self.get_vector_query(
        field1,
        vector1.clone(),
        traversal_similarity1,
        result_similarity1,
        Some(filter1.clone()),
      )?
    );

    assert_ne!(Some(&query), None);

    assert_ne!(
      query,
      self.get_vector_query(
        field2,
        vector1.clone(),
        traversal_similarity1,
        result_similarity1,
        Some(filter1.clone()),
      )?
    );

    assert_ne!(
      query,
      self.get_vector_query(
        field1,
        vector2,
        traversal_similarity1,
        result_similarity1,
        Some(filter1.clone()),
      )?
    );

    assert_ne!(
      query,
      self.get_vector_query(
        field1,
        vector1.clone(),
        traversal_similarity2,
        result_similarity1,
        Some(filter1.clone()),
      )?
    );

    assert_ne!(
      query,
      self.get_vector_query(
        field1,
        vector1.clone(),
        traversal_similarity1,
        result_similarity2,
        Some(filter1.clone()),
      )?
    );

    assert_ne!(
      query,
      self.get_vector_query(
        field1,
        vector1,
        traversal_similarity1,
        result_similarity1,
        Some(filter2),
      )?
    );

    Ok(())
  }

  fn test_empty_index<R>(&mut self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    self.get_base_mut().num_docs = 0;
    let (num_docs, dim, vector_field) = {
      let base = self.get_base();
      (base.num_docs, base.dim, &base.vector_field)
    };
    let vectors = self.get_random_vectors(random, num_docs, dim);
    let index_store = self.get_index_store(random, vectors)?;
    let reader = directory_reader::open(index_store.into())?;
    let searcher = new_searcher_with_reader(reader)?;
    let query = self.get_vector_query(
      vector_field,
      self.get_random_vector(random, dim),
      f32::NEG_INFINITY,
      f32::NEG_INFINITY,
      None,
    )?;

    assert_eq!(0, searcher.count(query)?);
    Ok(())
  }

  fn test_extremes<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let (num_docs, dim, vector_field, _id_field) = {
      let base = self.get_base();
      (base.num_docs, base.dim, &base.vector_field, &base.id_field)
    };
    let vectors = self.get_random_vectors(random, num_docs, dim);
    let index_store = self.get_index_store(random, vectors)?;
    let reader = directory_reader::open(index_store.into())?;
    if !HnswUtil::graph_is_rooted(&reader, vector_field)? {
      return Ok(());
    }
    let searcher = new_searcher_with_reader(reader)?;

    let query1 = self.get_vector_query(
      vector_field,
      self.get_random_vector(random, dim),
      f32::NEG_INFINITY,
      f32::NEG_INFINITY,
      None,
    )?;
    assert_eq!(num_docs as i32, searcher.count(query1)?);

    let query2 = self.get_vector_query(
      vector_field,
      self.get_random_vector(random, dim),
      f32::INFINITY,
      f32::INFINITY,
      None,
    )?;
    assert_eq!(0, searcher.count(query2)?);
    Ok(())
  }

  fn test_random_filter<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let (num_docs, dim, vector_field, id_field) = {
      let base = self.get_base();
      (base.num_docs, base.dim, &base.vector_field, &base.id_field)
    };
    let start_index = random.random_range(0..num_docs);
    let end_index = random.random_range(start_index..num_docs);
    let start_id = start_index as i32;
    let end_id = end_index as i32;
    let filter: Query = IntField::new_range_query(id_field, start_id, end_id)?.into();

    let vectors = self.get_random_vectors(random, num_docs, dim);
    let index_store = self.get_index_store(random, vectors)?;
    let reader = directory_reader::open(index_store.into())?;
    if !HnswUtil::graph_is_rooted(&reader, vector_field)? {
      return Ok(());
    }
    let searcher = new_searcher_with_reader(reader)?;
    let query = self.get_vector_query(
      vector_field,
      self.get_random_vector(random, dim),
      f32::NEG_INFINITY,
      f32::NEG_INFINITY,
      Some(filter),
    )?;

    let score_docs = searcher.search(query, num_docs)?.score_docs;
    for score_doc in &score_docs {
      let id = self.get_id(&searcher, id_field, score_doc.doc)?;
      assert!(id >= start_id && id <= end_id);
    }
    assert_eq!(end_index - start_index + 1, score_docs.len());
    Ok(())
  }

  fn test_filter_with_no_matches<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let (num_docs, dim, vector_field, id_field) = {
      let base = self.get_base();
      (base.num_docs, base.dim, &base.vector_field, &base.id_field)
    };
    let vectors = self.get_random_vectors(random, num_docs, dim);
    let index_store = self.get_index_store(random, vectors)?;
    let reader = directory_reader::open(index_store.into())?;
    let searcher = new_searcher_with_reader(reader)?;

    let filter1: Query = TermQuery::new(Term::from_text("random_field", "random_value")).into();
    let query1 = self.get_vector_query(
      vector_field,
      self.get_random_vector(random, dim),
      f32::NEG_INFINITY,
      f32::NEG_INFINITY,
      Some(filter1),
    )?;
    assert_eq!(0, searcher.count(query1)?);

    let filter2: Query = IntField::new_exact_query(id_field, -1)?.into();
    let query2 = self.get_vector_query(
      vector_field,
      self.get_random_vector(random, dim),
      f32::NEG_INFINITY,
      f32::NEG_INFINITY,
      Some(filter2),
    )?;
    assert_eq!(0, searcher.count(query2)?);
    Ok(())
  }

  fn test_dimension_mismatch<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let (num_docs, dim, vector_field, _id_field) = {
      let base = self.get_base();
      (base.num_docs, base.dim, &base.vector_field, &base.id_field)
    };
    let new_dim = at_least_usize(random, dim + 1);
    let vectors = self.get_random_vectors(random, num_docs, dim);
    let index_store = self.get_index_store(random, vectors)?;
    let reader = directory_reader::open(index_store.into())?;
    let searcher = new_searcher_with_reader(reader)?;
    let query = self.get_vector_query(
      vector_field,
      self.get_random_vector(random, new_dim),
      f32::NEG_INFINITY,
      f32::NEG_INFINITY,
      None,
    )?;

    let err = searcher.count(query).unwrap_err();
    assert_eq!(
      format!("vector query dimension: {new_dim} differs from field dimension: {dim}"),
      err.to_string()
    );
    Ok(())
  }

  fn test_non_vectors_field<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let (num_docs, dim, _vector_field, id_field) = {
      let base = self.get_base();
      (base.num_docs, base.dim, &base.vector_field, &base.id_field)
    };
    let vectors = self.get_random_vectors(random, num_docs, dim);
    let index_store = self.get_index_store(random, vectors)?;
    let reader = directory_reader::open(index_store.into())?;
    let searcher = new_searcher_with_reader(reader)?;

    let query1 = self.get_vector_query(
      "random_field",
      self.get_random_vector(random, dim),
      f32::NEG_INFINITY,
      f32::NEG_INFINITY,
      None,
    )?;
    assert_eq!(0, searcher.count(query1)?);

    let query2 = self.get_vector_query(
      id_field,
      self.get_random_vector(random, dim),
      f32::NEG_INFINITY,
      f32::NEG_INFINITY,
      None,
    )?;
    assert_eq!(0, searcher.count(query2)?);
    Ok(())
  }

  fn test_some_deletes<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    // Delete a sub-range from 0 to numDocs
    let (num_docs, dim, vector_field, id_field) = {
      let base = self.get_base();
      (base.num_docs, base.dim, &base.vector_field, &base.id_field)
    };
    let start_index = random.random_range(0..num_docs);
    let end_index = random.random_range(start_index..num_docs);
    let start_id = start_index as i32;
    let end_id = end_index as i32;
    let delete: Query = IntField::new_range_query(id_field, start_id, end_id)?.into();

    let vectors = self.get_random_vectors(random, num_docs, dim);
    let index_store = self.get_index_store(random, vectors)?;
    let writer = IndexWriter::new(index_store.clone().into(), new_index_writer_config(random)?)?;
    writer.delete_documents_with_queries(vec![delete])?;
    writer.commit()?;
    writer.close()?;

    let reader = directory_reader::open(index_store.into())?;
    if !HnswUtil::graph_is_rooted(&reader, vector_field)? {
      return Ok(());
    }
    let searcher = new_searcher_with_reader(reader)?;

    let query = self.get_vector_query(
      vector_field,
      self.get_random_vector(random, dim),
      f32::NEG_INFINITY,
      f32::NEG_INFINITY,
      None,
    )?;

    let score_docs = searcher.search(query, num_docs)?.score_docs;
    for score_doc in &score_docs {
      let id = self.get_id(&searcher, id_field, score_doc.doc)?;

      // Check that returned document is not deleted
      assert!(id < start_id || id > end_id);
    }
    // Check that all live docs are returned
    assert_eq!(num_docs - end_index + start_index - 1, score_docs.len());
    Ok(())
  }

  fn test_all_deletes<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let (num_docs, dim, vector_field, _id_field) = {
      let base = self.get_base();
      (base.num_docs, base.dim, &base.vector_field, &base.id_field)
    };
    let vectors = self.get_random_vectors(random, num_docs, dim);
    let index_store = self.get_index_store(random, vectors)?;
    let writer = IndexWriter::new(index_store.clone().into(), new_index_writer_config(random)?)?;
    // Delete all documents
    writer.delete_documents_with_queries(vec![
      crate::core::search::match_all_docs_query::MatchAllDocsQuery::new().into(),
    ])?;
    writer.commit()?;
    writer.close()?;

    let reader = directory_reader::open(index_store.into())?;
    let searcher = new_searcher_with_reader(reader)?;

    let query = self.get_vector_query(
      vector_field,
      self.get_random_vector(random, dim),
      f32::NEG_INFINITY,
      f32::NEG_INFINITY,
      None,
    )?;

    // Check that no vectors are found
    assert_eq!(0, searcher.count(query)?);
    Ok(())
  }

  fn test_boost_query<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let (num_docs, dim, vector_field, _id_field) = {
      let base = self.get_base();
      (base.num_docs, base.dim, &base.vector_field, &base.id_field)
    };
    let boost = 5.0 + random.random::<f32>() * 5.0;
    let delta = 1e-3;
    let vectors = self.get_random_vectors(random, num_docs, dim);
    let index_store = self.get_index_store(random, vectors)?;
    let reader = directory_reader::open(index_store.into())?;
    let searcher = new_searcher_with_reader(reader)?;

    let query1: Query = self
      .get_vector_query(
        vector_field,
        self.get_random_vector(random, dim),
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
        None,
      )?
      .into();
    let score_docs1 = searcher.search(query1.clone(), num_docs)?.score_docs;

    let query2 = BoostQuery::new(query1, boost)?;
    let score_docs2 = searcher.search(query2, num_docs)?.score_docs;
    assert_eq!(score_docs1.len(), score_docs2.len());

    for score_doc in &score_docs1 {
      let boosted_doc = score_docs2
        .iter()
        .find(|boosted| boosted.doc == score_doc.doc)
        .expect("boosted result should contain original doc");
      assert!((boost * score_doc.score - boosted_doc.score).abs() <= delta);
    }
    Ok(())
  }

  fn test_vectors_above_similarity<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let (num_docs, dim, vector_field, id_field) = {
      let base = self.get_base();
      (base.num_docs, base.dim, &base.vector_field, &base.id_field)
    };
    let num_accepted = random.random_range((num_docs / 3)..(num_docs / 2));
    let delta = 1e-3;
    let vectors = self.get_random_vectors(random, num_docs, dim);
    let query_vector = self.get_random_vector(random, dim);
    let result_similarity = self.get_similarity(&vectors, &query_vector, num_accepted)?;

    let mut scores = HashMap::new();
    assert_eq!(vectors.len(), num_docs);
    for (id, vector) in vectors.iter().enumerate() {
      let score = self.compare(&query_vector, vector)?;
      if score >= result_similarity {
        scores.insert(id, score);
      }
    }

    let index_store = self.get_index_store(random, vectors)?;
    let reader = directory_reader::open(index_store.into())?;
    let searcher = new_searcher_with_reader(reader)?;
    let query = self.get_vector_query(
      vector_field,
      query_vector,
      f32::NEG_INFINITY,
      result_similarity,
      None,
    )?;

    let score_docs = searcher.search(query, num_docs)?.score_docs;
    for score_doc in &score_docs {
      let id = self.get_id(&searcher, id_field, score_doc.doc)? as usize;
      assert!(scores.contains_key(&id));
      assert!(f32_equals(
        *scores.get(&id).unwrap(),
        score_doc.score,
        delta
      ));
    }
    assert_eq!(scores.len(), score_docs.len());
    Ok(())
  }

  fn test_fallback_to_exact<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let base = self.get_base();
    let num_docs = base.num_docs;
    let dim = base.dim;
    let vector_field = &base.vector_field;
    let id_field = &base.id_field;
    let num_filtered = random.random_range((num_docs / 10)..num_docs / 5);

    let vectors = self.get_random_vectors(random, num_docs, dim);
    let query_vector = self.get_random_vector(random, dim);
    let result_similarity = self.get_similarity(&vectors, &query_vector, num_docs)?;
    let filter: Query =
      IntField::new_set_query(id_field, self.get_filtered(random, num_filtered))?.into();

    let index_store = self.get_index_store(random, vectors)?;
    let reader = directory_reader::open(index_store.into())?;
    let searcher = new_searcher_with_reader(reader)?;

    let query = self.get_throwing_vector_query(
      vector_field,
      query_vector,
      result_similarity,
      result_similarity,
      Some(filter),
    )?;

    let result = searcher.search(query, num_docs);
    assert!(matches!(
      result.as_ref().map_err(LuceneError::kind),
      Err(LuceneErrorKind::UnsupportedOperation(_))
    ));

    Ok(())
  }

  fn test_approximate<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let base = self.get_base();
    let dim = base.dim;
    let vector_field = &base.vector_field;
    let id_field = &base.id_field;
    let num_filtered = base.num_docs - 1;
    let target_visited = random.random_range(1..(num_filtered / 10));

    let vectors = self.get_random_vectors(random, base.num_docs, dim);
    let query_vector = self.get_random_vector(random, dim);
    let result_similarity = self.get_similarity(&vectors, &query_vector, target_visited)?;
    let filter: Query =
      IntField::new_set_query(id_field, self.get_filtered(random, num_filtered))?.into();

    let index_store = self.get_index_store(random, vectors)?;
    let w = IndexWriter::new(index_store.clone().into(), new_index_writer_config(random)?)?;
    // Force merge because smaller segments have few filtered docs and often fall back to exact
    // search, making this test flaky
    w.force_merge(1)?;
    w.commit()?;
    let reader = directory_reader::open(index_store.into())?;
    let searcher = new_searcher_with_reader(reader)?;

    let query = self.get_throwing_vector_query(
      vector_field,
      query_vector,
      result_similarity,
      result_similarity,
      Some(filter),
    )?;

    // The filter restricts results
    assert!(searcher.count(query)? <= num_filtered as i32);

    Ok(())
  }

  fn test_timeout<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let (num_docs, dim, vector_field, id_field) = {
      let base = self.get_base();
      (base.num_docs, base.dim, &base.vector_field, &base.id_field)
    };
    let vectors = self.get_random_vectors(random, num_docs, dim);
    let query_vector = self.get_random_vector(random, dim);

    let index_store = self.get_index_store(random, vectors)?;
    let reader = directory_reader::open(index_store.into())?;
    let mut searcher = new_searcher_with_reader(reader)?;

    searcher.set_query_cache(None);

    let query: Query = CountingQuery::new(self.get_vector_query(
      vector_field,
      query_vector.clone(),
      f32::NEG_INFINITY,
      f32::NEG_INFINITY,
      None,
    )?)
    .into();

    let expected_count = num_docs as i32;
    assert_eq!(expected_count, searcher.count(query.clone())?);

    searcher.set_timeout(QueryTimeoutEnum::custom(AlwaysTimeout));
    assert_eq!(0, searcher.count(query.clone())?);

    searcher.set_timeout(QueryTimeoutEnum::custom(CountingQueryTimeout::new(
      num_docs - 1,
    )));
    let count = searcher.count(query)?;
    assert!(
      count > 0 && count < expected_count,
      "0 < count={count} < num_docs={num_docs}"
    );

    let num_filtered = random.random_range((num_docs / 2)..num_docs);
    let filter: Query =
      IntField::new_set_query(id_field, self.get_filtered(random, num_filtered))?.into();
    let filtered_query: Query = CountingQuery::new(self.get_vector_query(
      vector_field,
      query_vector,
      f32::NEG_INFINITY,
      f32::NEG_INFINITY,
      Some(filter),
    )?)
    .into();

    searcher.set_timeout(QueryTimeoutEnum::custom(NeverTimeout));
    let expected_filtered_count = num_filtered as i32;
    assert_eq!(
      expected_filtered_count,
      searcher.count(filtered_query.clone())?
    );

    searcher.set_timeout(QueryTimeoutEnum::custom(CountingQueryTimeout::new(
      num_filtered - 1,
    )));
    let filtered_count = searcher.count(filtered_query)?;
    assert!(
      filtered_count > 0 && filtered_count < expected_filtered_count,
      "0 < filtered_count={filtered_count} < num_filtered={num_filtered}"
    );
    Ok(())
  }

  fn get_similarity(
    &self,
    vectors: &[Self::Vector],
    query_vector: &Self::Vector,
    target_visited: usize,
  ) -> Result<f32> {
    let num_docs = self.get_base().num_docs;
    assert!(target_visited <= num_docs);

    if target_visited == 0 {
      return Ok(f32::INFINITY);
    }
    let mut scores = Vec::with_capacity(num_docs);
    for vector in vectors.iter().take(num_docs) {
      scores.push(self.compare(query_vector, vector)?);
    }

    scores.sort_by(|a, b| CoreHelper::compare_f32(*a, *b));

    Ok(scores[num_docs - target_visited])
  }
  fn get_filtered<R: Rng + ?Sized>(&self, random: &mut R, num_filtered: usize) -> Vec<i32> {
    let num_docs = self.get_base().num_docs;
    let mut accepted = HashSet::new();

    let mut i = 0;
    while i < num_filtered {
      let index = random.random_range(0..num_docs);
      if !accepted.contains(&index) {
        accepted.insert(index);
        i += 1;
      }
    }

    accepted.into_iter().map(|index| index as i32).collect()
  }
  fn get_id<IRC, SIM>(
    &self,
    searcher: &IndexSearcher<IRC, SIM>,
    id_field: &str,
    doc: i32,
  ) -> Result<i32>
  where
    SIM: crate::core::search::similarities_impl::similarities::Similarity,
    IRC: IndexReaderContext + 'static,
    crate::core::search::index_searcher::IndexSearcher<IRC, SIM>: Sync,
  {
    let document = searcher.get_index_reader().stored_fields()?.document(doc)?;
    let id = document.get(id_field)?.expect("id field should be stored");
    Ok(id.parse::<i32>().expect("stored id should be an i32"))
  }
  fn get_random_vectors<R>(&self, random: &mut R, num_docs: usize, dim: usize) -> Vec<Self::Vector>
  where
    R: Rng + ?Sized,
  {
    (0..num_docs)
      .map(|_| self.get_random_vector(random, dim))
      .collect()
  }

  fn get_index_store<R>(
    &self,
    random: &mut R,
    vectors: Vec<Self::Vector>,
  ) -> Result<Self::Directory>
  where
    R: Rng + ?Sized,
  {
    let index_store = self.new_directory_for_test(random)?;
    let writer = RandomIndexWriter::new(random, index_store.clone().into())?;
    let (vector_field, id_field, function) = {
      let base = self.get_base();
      (&base.vector_field, &base.id_field, base.function)
    };
    for (id, vector) in vectors.into_iter().enumerate() {
      let mut doc = Document::new();
      doc.add(self.get_vector_field(vector_field, vector, function)?);
      doc.add(IntField::new(id_field.clone(), id as i32, Store::Yes)?);
      writer.add_document(random, doc)?;
    }
    writer.close(random)?;
    Ok(index_store)
  }
}

struct AlwaysTimeout;

impl QueryTimeout for AlwaysTimeout {
  fn should_exit(&self) -> bool {
    true
  }
}

struct NeverTimeout;

impl QueryTimeout for NeverTimeout {
  fn should_exit(&self) -> bool {
    false
  }
}

struct CountingQueryTimeout {
  remaining: AtomicUsize,
}

impl CountingQueryTimeout {
  fn new(count: usize) -> Self {
    Self {
      remaining: AtomicUsize::new(count),
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

#[derive(Clone, Debug)]
pub struct CountingQuery {
  id: Identity,
  delegate: Box<Query>,
}

impl CountingQuery {
  pub fn new<T>(delegate: T) -> Self
  where
    T: Into<Query>,
  {
    Self {
      id: Identity::new(),
      delegate: Box::new(delegate.into()),
    }
  }
}

impl PartialEq for CountingQuery {
  fn eq(&self, other: &Self) -> bool {
    self.delegate == other.delegate
  }
}

impl Eq for CountingQuery {}

impl std::hash::Hash for CountingQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: std::hash::Hasher,
  {
    self.delegate.hash(state);
  }
}

impl HasIdentity for CountingQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl QueryBase for CountingQuery {
  type Weight<IRC, SIM>
    = CountingWeight<IRC, SIM>
  where
    SIM: crate::core::search::similarities_impl::similarities::Similarity,
    IRC: IndexReaderContext + 'static;

  fn to_string(&self, field: &str) -> Result<String> {
    Ok(format!(
      "CountingQuery[{}]",
      self.delegate.to_string(field)?
    ))
  }

  fn create_weight<IRC, SIM>(
    self,
    searcher: &IndexSearcher<IRC, SIM>,
    score_mode: &ScoreMode,
    boost: f32,
  ) -> Result<Self::Weight<IRC, SIM>>
  where
    SIM: crate::core::search::similarities_impl::similarities::Similarity,
    IRC: IndexReaderContext + 'static,
    crate::core::search::index_searcher::IndexSearcher<IRC, SIM>: Sync,
    Self: Sized,
  {
    let delegate_weight = self
      .delegate
      .as_ref()
      .clone()
      .create_weight(searcher, score_mode, boost)?;
    Ok(CountingWeight::<_, SIM> {
      parent_query: Arc::new(self.into()),
      delegate_weight,
    })
  }

  fn visit<QV>(&self, visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    visitor.visit_leaf(self.into())
  }
}

pub struct CountingWeight<
  IRC: IndexReaderContext + 'static,
  SIM = crate::core::search::similarities_impl::bm25_similarity::BM25Similarity,
> where
  SIM: crate::core::search::similarities_impl::similarities::Similarity,
  IRC: IndexReaderContext + 'static,
{
  parent_query: Arc<Query>,
  delegate_weight: QueryWeight<IRC, SIM>,
}

impl<IRC, SIM> SegmentCacheable<IRC> for CountingWeight<IRC, SIM>
where
  SIM: crate::core::search::similarities_impl::similarities::Similarity,
  IRC: IndexReaderContext + 'static,
{
  fn is_cacheable(&self, ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    self.delegate_weight.is_cacheable(ctx)
  }
}

impl<IRC, SIM> Weight<IRC, SIM> for CountingWeight<IRC, SIM>
where
  SIM: crate::core::search::similarities_impl::similarities::Similarity,
  IRC: IndexReaderContext + 'static,
{
  type ScorerSupplier = QueryWeightSs<IRC, SIM>;

  fn matches<'a>(
    &'a self,
    context: &'a LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &'a IndexSearcher<IRC, SIM>,
  ) -> Result<Option<crate::core::search::query::QueryWeightMatches<'a, IRC, SIM>>>
  where
    crate::core::search::index_searcher::IndexSearcher<IRC, SIM>: Sync,
  {
    self.delegate_weight.matches(context, doc, searcher)
  }

  fn explain(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Explanation>
  where
    crate::core::search::index_searcher::IndexSearcher<IRC, SIM>: Sync,
  {
    self.delegate_weight.explain(context, doc, searcher)
  }

  fn get_query(&self) -> QueryRef<'_> {
    self.parent_query.as_ref().into()
  }

  fn scorer_supplier(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Option<Self::ScorerSupplier>>
  where
    crate::core::search::index_searcher::IndexSearcher<IRC, SIM>: Sync,
  {
    self.delegate_weight.scorer_supplier(context, searcher)
  }

  fn count(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<i32>
  where
    crate::core::search::index_searcher::IndexSearcher<IRC, SIM>: Sync,
  {
    let Some(mut scorer) = self.delegate_weight.scorer(context, searcher)? else {
      return Ok(0);
    };

    let mut count = 0;
    while scorer.next_doc()? != NO_MORE_DOCS {
      count += 1;
    }
    Ok(count)
  }
}

impl crate::core::util::accountable::Accountable for CountingQuery {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(crate::core::util::ram_usage_estimator::QUERY_DEFAULT_RAM_BYTES_USED)
  }
}
