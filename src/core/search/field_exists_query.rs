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
use crate::core::index::doc_values::DocValues;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_reader::{Identity, IndexReader};
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::knn_vector_values::KnnVectorValues;
use crate::core::index::leaf_reader::{
  IRCByteVectorIter, IRCFloatVectorIter, LRDisis, LRNormNumericDocValues, LeafReader,
};
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::point_values::PointValues;
use crate::core::index::terms::Terms;
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::search::constant_score_scorer::ConstantScoreDisiScorer;
use crate::core::search::constant_score_scorer::ConstantScoreScorer;
use crate::core::search::constant_score_weight::ConstantScoreWeight;
use crate::core::search::doc_id_set_iterator::{DocIdSetIteratorEnum4, DocIdSetIteratorEnum5};
use crate::core::search::explanation::Explanation;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::match_all_docs_query::MatchAllDocsQuery;
use crate::core::search::query::QueryRef;
use crate::core::search::query::{Query, QueryBase};
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::weight::{DefaultScorerSupplier, Weight};
use crate::core::util::TryIntoInt;
use crate::core::util::core_helper::HasIdentity;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::fmt::Debug;
use std::hash::{Hash, Hasher};

/// A [`Query`] that matches documents that contain either a [`KnnFloatVectorField`](crate::core::document::knn_float_vector_field::KnnFloatVectorField),
/// [`KnnByteVectorField`](crate::core::document::knn_byte_vector_field::KnnByteVectorField), or a field that indexes norms
/// or doc values.
#[derive(Debug, Clone)]
pub struct FieldExistsQuery {
  id: Identity,
  field: String,
}
impl FieldExistsQuery {
  /// Create a query that will match that have a value for the given `field`.
  pub fn new<T>(field: T) -> Self
  where
    T: Into<String>,
  {
    let field = field.into();
    Self {
      id: Identity::new(),
      field,
    }
  }
  pub fn get_field(&self) -> &str {
    &self.field
  }
  fn build_error_msg(&self, field_info: &FieldInfo) -> String {
    format!(
      "FieldExistsQuery requires that the field indexes doc values, norms or vectors, but field '{}' exists and indexes neither of these data structures",
      field_info.name
    )
  }
  fn get_vector_values_size<LR>(&self, fi: &FieldInfo, reader: &LR) -> Result<usize>
  where
    LR: LeafReader,
  {
    debug_assert_eq!(fi.name, self.field);
    match fi.get_vector_encoding() {
      VectorEncoding::FLOAT32(_) => match reader.get_float_vector_values(&self.field)? {
        Some(float_vector_values) => Ok(float_vector_values.size()),
        None => Err(LuceneError::illegal_state(
          "unexpected null float vector values",
        )),
      },
      VectorEncoding::BYTE(_) => match reader.get_byte_vector_values(&self.field)? {
        Some(byte_vector_values) => Ok(byte_vector_values.size()),
        None => Err(LuceneError::illegal_state(
          "unexpected null byte vector values",
        )),
      },
    }
  }
}

impl PartialEq for FieldExistsQuery {
  fn eq(&self, other: &Self) -> bool {
    self.field == other.field
  }
}
impl Eq for FieldExistsQuery {}

impl Hash for FieldExistsQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.field.hash(state);
  }
}

impl HasIdentity for FieldExistsQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl QueryBase for FieldExistsQuery {
  type Weight<IRC, SIM>
    = FieldExistsWeight
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static;

  fn to_string(&self, _field: &str) -> Result<String> {
    Ok(format!("FieldExistsQuery [field={}]", self.field))
  }

  fn create_weight<IRC, SIM>(
    self,
    _searcher: &IndexSearcher<IRC, SIM>,
    score_mode: &ScoreMode,
    boost: f32,
  ) -> Result<Self::Weight<IRC, SIM>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    Self: Sized,
    IndexSearcher<IRC, SIM>: Sync,
  {
    Ok(FieldExistsWeight::new(boost, self, *score_mode))
  }

  fn rewrite<IRC, SIM>(&self, searcher: &IndexSearcher<IRC, SIM>) -> Result<Option<Query>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    Self: Sized,
    IndexSearcher<IRC, SIM>: Sync,
  {
    let reader = searcher.get_index_reader();

    for context in searcher.get_leaf_contexts()? {
      let leaf = context.reader();
      let field_infos = leaf.get_field_infos()?;
      let Some(field_info) = field_infos.field_info_by_name(&self.field)? else {
        return Ok(None);
      };

      if field_info.has_norms() {
        // The field indexes norms.
        if reader.get_doc_count(&self.field)? != reader.max_doc()? {
          return Ok(None);
        }
      } else if field_info.get_vector_dimension() != 0 {
        if self.get_vector_values_size(field_info, leaf)? != leaf.max_doc()? as usize {
          return Ok(None);
        }
      } else if *field_info.get_doc_values_type() != DocValuesType::None {
        // This optimization is possible due to LUCENE-9334 enforcing a field to always uses the
        // same data structures (all or nothing). Since there's no index statistic to detect when
        // all documents have doc values for a specific field, FieldExistsQuery can only be
        // rewritten to MatchAllDocsQuery for doc values field, when that same field also indexes
        // terms or point values which do have index statistics, and those statistics confirm that
        // all documents in this segment have values terms or point values.
        let terms = leaf.terms(&self.field)?;
        let point_values = leaf.get_point_values(&self.field)?;

        let mut terms_bad = true;
        if let Some(t) = terms.as_ref()
          && t.get_doc_count()? == leaf.max_doc()?
        {
          terms_bad = false;
        }

        if terms_bad {
          let mut points_bad = true;
          if let Some(p) = point_values.as_ref()
            && p.get_doc_count()? == leaf.max_doc()?
          {
            points_bad = false;
          }

          if points_bad {
            return Ok(None);
          }
        }
      } else {
        return Err(LuceneError::illegal_state(
          self.build_error_msg(field_info.as_ref()),
        ));
      }
    }

    Ok(Some(MatchAllDocsQuery::new().into()))
  }

  fn visit<QV>(&self, visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    let query = self.into();
    if visitor.accept_field(&self.field) {
      visitor.visit_leaf(query)?;
    }
    Ok(())
  }
}

pub struct FieldExistsWeight {
  base: ConstantScoreWeight,
  parent_query: FieldExistsQuery,
  score_mode: ScoreMode,
  score: f32,
}
impl FieldExistsWeight {
  fn new(score: f32, query: FieldExistsQuery, score_mode: ScoreMode) -> Self {
    let parent_query = query;
    Self {
      base: ConstantScoreWeight::new(score),
      parent_query,
      score_mode,
      score,
    }
  }

  fn query(&self) -> Result<&FieldExistsQuery> {
    Ok(&self.parent_query)
  }
}

impl<IRC> SegmentCacheable<IRC> for FieldExistsWeight
where
  IRC: IndexReaderContext + 'static,
{
  fn is_cacheable(&self, ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    let field_infos = ctx.reader().get_field_infos()?;
    let query = self.query()?;
    let field_info = field_infos.field_info_by_name(&query.field)?;

    if let Some(fi) = field_info
      && *fi.get_doc_values_type() != DocValuesType::None
    {
      let field = [query.field.as_str()];
      return DocValues::is_cacheable(ctx, field);
    }
    Ok(true)
  }
}
pub type Disi<LR> = DocIdSetIteratorEnum4<
  IRCByteVectorIter<LR>,
  IRCFloatVectorIter<LR>,
  LRNormNumericDocValues<LR>,
  LRDisis<LR>,
>;
impl<IRC, SIM> Weight<IRC, SIM> for FieldExistsWeight
where
  SIM: Similarity,
  IRC: IndexReaderContext + 'static,
{
  fn explain(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Explanation>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    let scorer = self.scorer(context, searcher)?;
    self
      .base
      .explain(scorer, doc, self.parent_query.to_string("")?)
  }

  fn get_query(&self) -> QueryRef<'_> {
    QueryRef::FieldExists(&self.parent_query)
  }

  type ScorerSupplier = DefaultScorerSupplier<
    ConstantScoreDisiScorer<
      DocIdSetIteratorEnum4<
        IRCByteVectorIter<IRCLeafReader<IRC>>,
        IRCFloatVectorIter<IRCLeafReader<IRC>>,
        LRNormNumericDocValues<IRCLeafReader<IRC>>,
        LRDisis<IRCLeafReader<IRC>>,
      >,
    >,
  >;

  fn scorer_supplier(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Option<Self::ScorerSupplier>>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    let reader = context.reader();
    let query = self.query()?;
    let field = query.get_field();
    let field_infos = reader.get_field_infos()?;
    let field_info = field_infos.field_info_by_name(field)?;

    let Some(fi) = field_info else {
      return Ok(None);
    };
    let disi_opt = if fi.has_norms() {
      // the field indexes norms
      reader.get_norm_values(field)?.map(DocIdSetIteratorEnum4::C)
    } else if fi.get_vector_dimension() != 0 {
      match fi.get_vector_encoding() {
        VectorEncoding::BYTE(_) => Some(DocIdSetIteratorEnum4::A(
          context
            .reader()
            .get_byte_vector_values(field)?
            .ok_or_else(|| LuceneError::illegal_state("unexpected null byte vector values"))?
            .iterator()?,
        )),
        VectorEncoding::FLOAT32(_) => Some(DocIdSetIteratorEnum4::B(
          context
            .reader()
            .get_float_vector_values(field)?
            .ok_or_else(|| LuceneError::illegal_state("unexpected null float vector values"))?
            .iterator()?,
        )),
      }
    } else if *fi.get_doc_values_type() != DocValuesType::None {
      match *fi.get_doc_values_type() {
        DocValuesType::Numeric => reader
          .get_numeric_doc_values(field)?
          .map(|numeric| DocIdSetIteratorEnum4::D(DocIdSetIteratorEnum5::A(numeric))),

        DocValuesType::Binary => reader
          .get_binary_doc_values(field)?
          .map(|binary| DocIdSetIteratorEnum4::D(DocIdSetIteratorEnum5::B(binary))),

        DocValuesType::Sorted => reader
          .get_sorted_doc_values(field)?
          .map(|sorted| DocIdSetIteratorEnum4::D(DocIdSetIteratorEnum5::C(sorted))),

        DocValuesType::SortedNumeric => reader
          .get_sorted_numeric_doc_values(field)?
          .map(|sorted_numeric| DocIdSetIteratorEnum4::D(DocIdSetIteratorEnum5::D(sorted_numeric))),

        DocValuesType::SortedSet => reader
          .get_sorted_set_doc_values(field)?
          .map(|sorted_set| DocIdSetIteratorEnum4::D(DocIdSetIteratorEnum5::E(sorted_set))),
        DocValuesType::None => None,
      }
    } else {
      return Err(LuceneError::illegal_argument(
        query.build_error_msg(fi.as_ref()),
      ));
    };
    match disi_opt {
      Some(disi) => Ok(Some(DefaultScorerSupplier::new(
        ConstantScoreScorer::from_disi(self.score, self.score_mode, disi),
      ))),
      None => Ok(None),
    }
  }

  fn count(
    &self,
    ctx: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<i32>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    let reader = ctx.reader();

    let field_infos = reader.get_field_infos()?;
    let query = self.query()?;
    let field_info = field_infos.field_info_by_name(query.get_field())?;

    let Some(fi) = field_info else {
      return Ok(0);
    };

    if fi.has_norms() {
      // the field indexes norms
      // If every field has a value then we can shortcut
      let doc_count = LeafReader::get_doc_count(reader, query.get_field())?;
      if doc_count == reader.max_doc()? {
        return reader.num_docs();
      }
      return <Self as Weight<IRC, SIM>>::default_count(self, ctx);
    }

    if fi.has_vector_values() {
      // the field indexes vectors
      if !reader.has_deletions()? {
        return query
          .get_vector_values_size(fi.as_ref(), reader)?
          .try_convert();
      }
      return <Self as Weight<IRC, SIM>>::default_count(self, ctx);
    }

    if *fi.get_doc_values_type() != DocValuesType::None {
      // the field indexes doc values
      if !reader.has_deletions()? {
        if fi.get_point_dimension_count() > 0 {
          if let Some(point_values) = reader.get_point_values(query.get_field())? {
            return point_values.get_doc_count();
          } else {
            return Ok(0);
          }
        }

        if *fi.get_index_options() != IndexOptions::None {
          if let Some(terms) = reader.terms(query.get_field())? {
            return terms.get_doc_count();
          } else {
            return Ok(0);
          }
        }
      }

      return <Self as Weight<IRC, SIM>>::default_count(self, ctx);
    }

    Err(LuceneError::illegal_argument(
      query.build_error_msg(fi.as_ref()),
    ))
  }
}

/// Returns a DocIdSetIterator from the given field or None if the field doesn't
/// exist in the reader or if the reader has no doc values for the field.
pub fn get_doc_values_doc_id_set_iterator<LR>(
  field: &str,
  reader: &LR,
) -> Result<Option<LRDisis<LR>>>
where
  LR: LeafReader,
{
  let field_infos = reader.get_field_infos()?;
  let field_info = field_infos.field_info_by_name(field)?;

  let Some(fi) = field_info else {
    return Ok(None);
  };
  let doc_value_type = *fi.get_doc_values_type();
  match doc_value_type {
    DocValuesType::Numeric => match reader.get_numeric_doc_values(field)? {
      Some(numeric) => Ok(Some(DocIdSetIteratorEnum5::A(numeric))),
      None => Ok(None),
    },

    DocValuesType::Binary => match reader.get_binary_doc_values(field)? {
      Some(binary) => Ok(Some(DocIdSetIteratorEnum5::B(binary))),
      None => Ok(None),
    },

    DocValuesType::Sorted => match reader.get_sorted_doc_values(field)? {
      Some(sorted) => Ok(Some(DocIdSetIteratorEnum5::C(sorted))),
      None => Ok(None),
    },

    DocValuesType::SortedNumeric => match reader.get_sorted_numeric_doc_values(field)? {
      Some(sorted_numeric) => Ok(Some(DocIdSetIteratorEnum5::D(sorted_numeric))),
      None => Ok(None),
    },

    DocValuesType::SortedSet => match reader.get_sorted_set_doc_values(field)? {
      Some(sorted_set) => Ok(Some(DocIdSetIteratorEnum5::E(sorted_set))),
      None => Ok(None),
    },
    DocValuesType::None => Ok(None),
  }
}

impl crate::core::util::accountable::Accountable for FieldExistsQuery {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(crate::core::util::ram_usage_estimator::QUERY_DEFAULT_RAM_BYTES_USED)
  }
}
