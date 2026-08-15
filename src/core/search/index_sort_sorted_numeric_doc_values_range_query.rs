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
use crate::core::document::int_point::IntPoint;
use crate::core::document::long_point::LongPoint;
use crate::core::index::doc_values::{DocValues, SortedNumeric};
use crate::core::index::index_reader::{Identity, IndexReader};
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::point_values::{IntersectVisitor, PointTree, PointValues, Relation};
use crate::core::index::sorted_numeric_doc_values::SortedNumericDocValues;
use crate::core::search::constant_score_scorer::ConstantScoreScorer;
use crate::core::search::constant_score_weight::ConstantScoreWeight;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::doc_id_set_iterator::{
  AllDISI, DocIdSetIterator, DocIdSetIteratorEnum4, EmptyDISI, RangeDISI,
};
use crate::core::search::dummy::dummy_scorer::DummyScorer;
use crate::core::search::explanation::Explanation;
use crate::core::search::field_comparator::{FieldComparator, FieldComparatorEnum};
use crate::core::search::field_exists_query::FieldExistsQuery;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::leaf_field_comparator::{LeafFieldComparator, LeafFieldComparatorEnum};
use crate::core::search::match_all_docs_query::MatchAllDocsQuery;
use crate::core::search::pruning::Pruning;
use crate::core::search::query::{
  IntoBoxQuery, Query, QueryBase, QueryWeight, QueryWeightSs, QueryWeightSsBulkScorer,
  QueryWeightSsScorer,
};
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer_supplier::ScorerSupplier;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::sort_field::{MissingValueEnum, SortFieldType, SortFiledBase};
use crate::core::search::sort_field_enum::SortFieldEnum;
use crate::core::search::weight::Weight;
use crate::core::util::TryIntoInt;
use crate::core::util::array_util::{ArrayUtil, ByteArrayComparator, ByteArrayComparatorEnum};
use crate::core::util::bit_util::BitUtil;
use crate::core::util::core_helper::HasIdentity;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct IndexSortSortedNumericDocValuesRangeQuery {
  id: Identity,
  field: String,
  lower_value: i64,
  upper_value: i64,
  pub(crate) fallback_query: Box<Query>,
}
impl IndexSortSortedNumericDocValuesRangeQuery {
  pub fn new<S, T>(field: S, lower_value: i64, upper_value: i64, fallback_query: T) -> Self
  where
    S: Into<String>,
    T: IntoBoxQuery,
  {
    let fallback_query = fallback_query.into_box_query();
    Self {
      id: Identity::new(),
      field: field.into(),
      lower_value,
      upper_value,
      fallback_query,
    }
  }
}
impl PartialEq for IndexSortSortedNumericDocValuesRangeQuery {
  fn eq(&self, other: &Self) -> bool {
    self.lower_value == other.lower_value
      && self.upper_value == other.upper_value
      && self.field == other.field
      && self.fallback_query == other.fallback_query
  }
}

impl Eq for IndexSortSortedNumericDocValuesRangeQuery {}

impl Hash for IndexSortSortedNumericDocValuesRangeQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.field.hash(state);
    self.lower_value.hash(state);
    self.upper_value.hash(state);
    self.fallback_query.hash(state);
  }
}

impl HasIdentity for IndexSortSortedNumericDocValuesRangeQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl QueryBase for IndexSortSortedNumericDocValuesRangeQuery {
  fn to_string(&self, field: &str) -> Result<String> {
    let mut s = String::new();

    if self.field != field {
      s.push_str(&self.field);
      s.push(':');
    }

    s.push('[');
    s.push_str(&self.lower_value.to_string());
    s.push_str(" TO ");
    s.push_str(&self.upper_value.to_string());
    s.push(']');
    Ok(s)
  }

  fn create_weight<IRC>(
    self,
    searcher: &IndexSearcher<IRC>,
    score_mode: &ScoreMode,
    boost: f32,
  ) -> Result<QueryWeight<IRC>>
  where
    IRC: IndexReaderContext,
    Self: Sized,
  {
    let query = self.clone();
    let fallback_query = *self.fallback_query;
    let fallback_query_weight = fallback_query.create_weight(searcher, score_mode, boost)?;
    Ok(Box::new(
      IndexSortSortedNumericDocValuesRangeQueryWeight::new(
        query,
        ConstantScoreWeight::new(boost),
        *score_mode,
        fallback_query_weight,
      ),
    ))
  }

  fn rewrite<IRC>(mut self, searcher: &IndexSearcher<IRC>) -> Result<Query>
  where
    IRC: IndexReaderContext,
    Self: Sized,
  {
    if self.lower_value == i64::MIN && self.upper_value == i64::MAX {
      return Ok(FieldExistsQuery::new(self.field).into());
    }

    let fallback_id = self.fallback_query.identity().clone();
    let rewritten_fallback = self.fallback_query.clone().rewrite(searcher)?;

    if matches!(rewritten_fallback, Query::MatchAllDocs(_)) {
      return Ok(MatchAllDocsQuery::new().into());
    }

    if rewritten_fallback.identity() == &fallback_id {
      self.fallback_query = Box::new(rewritten_fallback);
      return Ok(self.into());
    }

    Ok(
      IndexSortSortedNumericDocValuesRangeQuery::new(
        self.field,
        self.lower_value,
        self.upper_value,
        Box::new(rewritten_fallback),
      )
      .into(),
    )
  }

  fn visit<QV>(&self, visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    if visitor.accept_field(&self.field) {
      visitor.visit_leaf(self.into())?;
      self.fallback_query.visit(visitor)?;
    }
    Ok(())
  }
}

pub struct IndexSortSortedNumericDocValuesRangeQueryWeight<IRC> {
  query: IndexSortSortedNumericDocValuesRangeQuery,
  base: ConstantScoreWeight,
  score_mode: ScoreMode,
  fallback_query_weight: QueryWeight<IRC>,
  parent_query: Arc<Query>,
}
impl<IRC> IndexSortSortedNumericDocValuesRangeQueryWeight<IRC>
where
  IRC: IndexReaderContext,
{
  pub fn new(
    query: IndexSortSortedNumericDocValuesRangeQuery,
    base: ConstantScoreWeight,
    score_mode: ScoreMode,
    fallback_query_weight: QueryWeight<IRC>,
  ) -> Self {
    let query_clone = query.clone();
    Self {
      query: query_clone,
      base,
      score_mode,
      fallback_query_weight,
      parent_query: Arc::new(query.into()),
    }
  }
}
pub type Disi<LR> = <SortedNumeric<LR> as SortedNumericDocValues>::NumericDocValues;

impl<IRC> SegmentCacheable<IRC> for IndexSortSortedNumericDocValuesRangeQueryWeight<IRC>
where
  IRC: IndexReaderContext,
{
  fn is_cacheable(&self, ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    // Both queries should always return the same values, so we can just check
    // if the fallback query is cacheable.
    self.fallback_query_weight.is_cacheable(ctx)
  }
}

impl<IRC> Weight<IRC> for IndexSortSortedNumericDocValuesRangeQueryWeight<IRC>
where
  IRC: IndexReaderContext,
{
  fn explain(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Explanation> {
    let scorer = self.scorer(context, searcher)?;
    self
      .base
      .explain(scorer, doc, self.parent_query.to_string("")?)
  }

  fn get_query(&self) -> Arc<Query> {
    self.parent_query.clone()
  }

  type ScorerSupplier = QueryWeightSs<IRC>;

  fn scorer_supplier(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::ScorerSupplier>> {
    match get_doc_id_set_iterator_or_null(
      context,
      self.query.lower_value,
      self.query.upper_value,
      &self.query.field,
    )? {
      Some(it_and_count) => {
        let disi = it_and_count.it;
        let scorer_supplier = ScorerSupplierImpl::new(
          disi,
          self.score_mode,
          self.query.lower_value,
          self.query.upper_value,
          self.query.field.clone(),
          self.base.score(),
        )?;
        Ok(Some(Box::new(scorer_supplier)))
      },
      None => match self
        .fallback_query_weight
        .scorer_supplier(context, searcher)?
      {
        Some(v) => Ok(Some(v)),
        None => Ok(None),
      },
    }
  }

  fn count(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<i32>
where {
    let reader = context.reader();

    if !reader.has_deletions()? {
      if self.query.lower_value > self.query.upper_value {
        return Ok(0);
      }

      let mut sorted_numeric_values = DocValues::get_sorted_numeric(reader, &self.query.field)?;
      if !sorted_numeric_values.is_single_valued() {
        return self.fallback_query_weight.count(context, searcher);
      }
      let mut numeric_values = Some(DocValues::unwrap_singleton_numeric(
        &mut sorted_numeric_values,
      )?);

      let point_values = reader.get_point_values(&self.query.field)?;

      if let Some(ref points) = point_values
        && points.get_doc_count()? == reader.max_doc()?
      {
        let (opt_itc, remaining_numeric_values) = get_doc_id_set_iterator_or_null_from_bkd(
          context,
          numeric_values.take().unwrap(),
          self.query.lower_value,
          self.query.upper_value,
          &self.query.field,
        )?;
        numeric_values = remaining_numeric_values;

        if let Some(itc) = opt_itc
          && itc.count != -1
        {
          return Ok(itc.count);
        }
      }
      // use index sort optimization if possible
      let meta = reader.get_metadata()?;
      if let Some(index_sort) = meta.get_sort() {
        let sort_fields = index_sort.get_sort();

        if !sort_fields.is_empty() && sort_fields[0].get_field() == Some(&self.query.field) {
          let sort_field = &sort_fields[0];
          let sort_field_type = get_sort_field_type(sort_field);
          // The index sort optimization is only supported for Type.INT and Type.LONG
          if sort_field_type == SortFieldType::Int || sort_field_type == SortFieldType::Long {
            let missing_long_value = match sort_field.get_missing_value() {
              None => 0i64,
              Some(MissingValueEnum::Long(v)) => *v,
              Some(MissingValueEnum::Int(v)) => *v as i64,
              _ => {
                return Err(LuceneError::illegal_argument(
                  "Missing value for SortedNumericSortField must be Long/Int",
                ));
              },
            };

            let all_docs_have_values = match point_values {
              Some(ref pv) => pv.get_doc_count()? == reader.max_doc()?,
              None => false,
            };

            if (all_docs_have_values
              || (missing_long_value < self.query.lower_value
                || missing_long_value > self.query.upper_value))
              && let Some(numeric_values) = numeric_values
            {
              let itc = get_doc_id_set_iterator(
                sort_field,
                context,
                numeric_values,
                self.query.lower_value,
                self.query.upper_value,
                &self.query.field,
              )?;
              if itc.count != -1 {
                return Ok(itc.count);
              }
            }
          }
        }
      }
    }
    self.fallback_query_weight.count(context, searcher)
  }
}

pub struct ScorerSupplierImpl<D> {
  disi: Option<IteratorAndCountDisi<D>>,
  score_mode: ScoreMode,
  cost: i64,
  lower_value: i64,
  upper_value: i64,
  field: String,
  score: f32,
}
impl<D> ScorerSupplierImpl<D>
where
  D: DocIdSetIterator,
{
  pub fn new(
    disi: IteratorAndCountDisi<D>,
    score_mode: ScoreMode,
    lower_value: i64,
    upper_value: i64,
    field: String,
    score: f32,
  ) -> Result<Self> {
    let cost = disi.cost()?;
    Ok(Self {
      disi: Some(disi),
      score_mode,
      cost,
      lower_value,
      upper_value,
      field,
      score,
    })
  }
}
impl<IRC> ScorerSupplier<IRC> for ScorerSupplierImpl<Disi<IRCLeafReader<IRC>>>
where
  IRC: IndexReaderContext,
{
  type Scorer = QueryWeightSsScorer;
  type BulkScorer = QueryWeightSsBulkScorer;

  fn get(
    &mut self,
    _lead_cost: i64,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC>,
  ) -> Result<Self::Scorer> {
    let disi = match self.disi.take() {
      Some(disi) => disi,
      None => {
        match get_doc_id_set_iterator_or_null(
          context,
          self.lower_value,
          self.upper_value,
          &self.field,
        )? {
          Some(mut it_and_count) => std::mem::take(&mut it_and_count.it),
          None => return Err(LuceneError::illegal_state("should not be here")),
        }
      },
    };
    let v = ConstantScoreScorer::from_disi(self.score, self.score_mode, disi);
    Ok(Box::new(v))
  }

  fn bulk_scorer(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::BulkScorer>> {
    Ok(Some(Box::new(self.default_bulk_scorer(context, searcher)?)))
  }

  fn cost(
    &mut self,
    _context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC>,
  ) -> Result<i64> {
    Ok(self.cost)
  }
}
struct ValueAndDoc {
  value: Option<Vec<u8>>,
  doc_id: i32,
  done: bool,
}
impl ValueAndDoc {
  pub fn new() -> Self {
    Self {
      value: None,
      doc_id: 0,
      done: false,
    }
  }
}
fn find_next_value<P>(
  point_tree: &mut P,
  value: &[u8],
  allow_equal: bool,
  comparator: &ByteArrayComparatorEnum,
  last_doc: bool,
) -> Result<Option<ValueAndDoc>>
where
  P: PointTree,
{
  let cmp = comparator.compare(point_tree.get_max_packed_value()?.as_ref(), 0, value, 0);

  if cmp < 0 || (cmp == 0 && !allow_equal) {
    return Ok(None);
  }

  if !point_tree.move_to_child()? {
    let mut vd = ValueAndDoc::new();
    let mut visitor = IntersectVisitorImpl::new(&mut vd, comparator, value, last_doc, allow_equal);
    point_tree.visit_doc_values(&mut visitor)?;

    if vd.value.is_some() {
      return Ok(Some(vd));
    } else {
      return Ok(None);
    }
  }
  loop {
    if let Some(vd) = find_next_value(point_tree, value, allow_equal, comparator, last_doc)? {
      return Ok(Some(vd));
    }

    if !point_tree.move_to_sibling()? {
      break;
    }
  }

  let moved = point_tree.move_to_parent()?;
  debug_assert!(moved);
  Ok(None)
}
fn next_doc<P>(
  point_tree: &mut P,
  value: &[u8],
  allow_equal: bool,
  comparator: &ByteArrayComparatorEnum,
  last_doc_flag: bool,
) -> Result<i32>
where
  P: PointTree,
{
  let vd_opt = find_next_value(point_tree, value, allow_equal, comparator, last_doc_flag)?;

  let vd = match vd_opt {
    Some(v) => v,
    None => return Ok(-1),
  };

  if !last_doc_flag || vd.done {
    return Ok(vd.doc_id);
  }

  // We found the next value, now we need the last doc ID.
  let doc = last_doc(point_tree, vd.value.as_ref().unwrap(), comparator)?;

  if doc == -1 {
    // vd.docID was actually the last doc ID
    Ok(vd.doc_id)
  } else {
    Ok(doc)
  }
}
fn last_doc<P>(
  point_tree: &mut P,
  value: &[u8],
  comparator: &ByteArrayComparatorEnum,
) -> Result<i32>
where
  P: PointTree,
{
  // Create a stack of nodes that may contain value that we'll use to search for the last leaf
  // node that contains `value`.
  // While the logic looks a bit complicated due to the fact that the PointTree API doesn't allow
  // moving back to previous siblings, this effectively performs a binary search.
  let mut stack: Vec<P> = Vec::new();

  'outer: loop {
    // Move to the next node
    while !point_tree.move_to_sibling()? {
      if !point_tree.move_to_parent()? {
        // No next node
        break 'outer;
      }
    }

    let cmp = comparator.compare(point_tree.get_min_packed_value()?.as_ref(), 0, value, 0);
    if cmp > 0 {
      // This node doesn't have `value`, so next nodes can't either
      break;
    }

    stack.push(point_tree.try_clone()?);
  }

  // Now search stack nodes
  while let Some(mut next) = stack.pop() {
    if !next.move_to_child()? {
      let mut visitor = IntersectVisitorImpl1::new(value, comparator);
      next.visit_doc_values(&mut visitor)?;

      if visitor.last_doc != -1 {
        return Ok(visitor.last_doc);
      }
    } else {
      loop {
        let cmp = comparator.compare(next.get_min_packed_value()?.as_ref(), 0, value, 0);
        if cmp > 0 {
          // This node doesn't have `value`, so next nodes can't either
          break;
        }

        stack.push(next.try_clone()?);

        if !next.move_to_sibling()? {
          break;
        }
      }
    }
  }

  Ok(-1)
}

struct IntersectVisitorImpl1<'a> {
  value: &'a [u8],
  comparator: &'a ByteArrayComparatorEnum,
  last_doc: i32,
}
impl<'a> IntersectVisitorImpl1<'a> {
  pub fn new(value: &'a [u8], comparator: &'a ByteArrayComparatorEnum) -> Self {
    Self {
      value,
      comparator,
      last_doc: -1,
    }
  }
}
impl<'a> IntersectVisitor for IntersectVisitorImpl1<'a> {
  fn visit(&mut self, _doc_id: i32) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()> {
    let cmp = self.comparator.compare(self.value, 0, packed_value, 0);
    if cmp == 0 {
      self.last_doc = doc_id;
    }
    Ok(())
  }

  fn compare(&self, _min_packed_value: &[u8], _max_packed_value: &[u8]) -> Result<Relation> {
    Ok(Relation::CellCrossesQuery)
  }
}

struct IntersectVisitorImpl<'a> {
  vd: &'a mut ValueAndDoc,
  comparator: &'a ByteArrayComparatorEnum,
  value: &'a [u8],
  last_doc: bool,
  allow_equal: bool,
}
impl<'a> IntersectVisitorImpl<'a> {
  pub fn new(
    vd: &'a mut ValueAndDoc,
    comparator: &'a ByteArrayComparatorEnum,
    value: &'a [u8],
    last_doc: bool,
    allow_equal: bool,
  ) -> Self {
    Self {
      vd,
      comparator,
      value,
      last_doc,
      allow_equal,
    }
  }
}
impl<'a> IntersectVisitor for IntersectVisitorImpl<'a> {
  fn visit(&mut self, _doc_id: i32) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()> {
    match self.vd.value {
      Some(ref value) if self.last_doc && !self.vd.done => {
        let cmp = self.comparator.compare(packed_value, 0, value, 0);
        debug_assert!(cmp >= 0);
        if cmp > 0 {
          self.vd.done = true;
        } else {
          self.vd.doc_id = doc_id;
        }
      },
      None => {
        let cmp = self.comparator.compare(packed_value, 0, self.value, 0);

        if cmp > 0 || (cmp == 0 && self.allow_equal) {
          self.vd.value = Some(packed_value.to_vec());
          self.vd.doc_id = doc_id;
        }
      },
      _ => {},
    }
    Ok(())
  }

  fn compare(&self, _min_packed_value: &[u8], _max_packed_value: &[u8]) -> Result<Relation> {
    Ok(Relation::CellCrossesQuery)
  }
}
pub struct BoundedDocIdSetIterator<D> {
  first_doc: i32,
  last_doc: i32,
  delegate: D,
  doc_id: i32,
}

impl<D> BoundedDocIdSetIterator<D> {
  fn new(first_doc: i32, last_doc: i32, delegate: D) -> Self {
    Self {
      first_doc,
      last_doc,
      delegate,
      doc_id: -1,
    }
  }
}

impl<D> DocIdSetIterator for BoundedDocIdSetIterator<D>
where
  D: DocIdSetIterator,
{
  fn doc_id(&self) -> i32 {
    self.doc_id
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.advance(self.doc_id + 1)
  }

  fn advance(&mut self, mut target: i32) -> Result<i32> {
    if target < self.first_doc {
      target = self.first_doc;
    }

    let result = self.delegate.advance(target)?;

    if result < self.last_doc {
      self.doc_id = result;
    } else {
      self.doc_id = NO_MORE_DOCS;
    }

    Ok(self.doc_id)
  }

  fn cost(&self) -> Result<i64> {
    let delegate_cost = self.delegate.cost()?;
    let bound_cost = (self.last_doc - self.first_doc) as i64;
    Ok(delegate_cost.min(bound_cost))
  }
}

fn get_doc_id_set_iterator<LR>(
  sort_field: &SortFieldEnum,
  context: &LeafReaderContext<LR>,
  delegate: Disi<LR>,
  lower_value: i64,
  upper_value: i64,
  field: &str,
) -> Result<IteratorAndCount<Disi<LR>>>
where
  LR: LeafReader,
{
  let lower = if sort_field.get_reverse() {
    upper_value
  } else {
    lower_value
  };
  let upper = if sort_field.get_reverse() {
    lower_value
  } else {
    upper_value
  };

  let reader = context.reader();
  let max_doc = reader.max_doc()?;
  // Perform a binary search to find the first document with value >= lower.
  let mut comparator = load_comparator(sort_field, lower, context)?;
  let mut low: i32 = 0;
  let mut high: i32 = max_doc - 1;

  while low <= high {
    let mid = (low + high) >> 1;
    if comparator.compare(mid)? <= 0 {
      high = mid - 1;
      comparator = load_comparator(sort_field, lower, context)?;
    } else {
      low = mid + 1;
    }
  }

  let first_doc_id_inclusive = high + 1;
  // Perform a binary search to find the first document with value > upper.
  // Since we know that upper >= lower, we can initialize the lower bound
  // of the binary search to the result of the previous search.
  let mut comparator = load_comparator(sort_field, upper, context)?;
  low = first_doc_id_inclusive;
  high = max_doc - 1;

  while low <= high {
    let mid = (low + high) >> 1;

    if comparator.compare(mid)? < 0 {
      high = mid - 1;
      comparator = load_comparator(sort_field, upper, context)?;
    } else {
      low = mid + 1;
    }
  }

  let last_doc_id_exclusive = high + 1;

  if first_doc_id_inclusive == last_doc_id_exclusive {
    return Ok(IteratorAndCount::empty());
  }

  let missing_value = sort_field.get_missing_value();
  let missing_long_value = match missing_value {
    Some(MissingValueEnum::Long(mv)) => *mv,
    Some(MissingValueEnum::Int(mv)) => mv.to_owned() as i64,
    Some(_) => {
      return Err(LuceneError::illegal_argument(
        "Missing value for SortedNumericSortField must be Long/Int",
      ));
    },
    None => 0i64,
  };

  let point_values = reader.get_point_values(field)?;
  // all documents have docValues or missing value falls outside the range
  let all_docs_have_values = match point_values {
    Some(point_values) => point_values.get_doc_count()? == reader.max_doc()?,
    _ => false,
  };

  if all_docs_have_values || (missing_long_value < lower_value || missing_long_value > upper_value)
  {
    return IteratorAndCount::dense_range(first_doc_id_inclusive, last_doc_id_exclusive);
  }

  Ok(IteratorAndCount::sparse_range(
    first_doc_id_inclusive,
    last_doc_id_exclusive,
    delegate,
  ))
}
fn get_doc_id_set_iterator_or_null<LR>(
  context: &LeafReaderContext<LR>,
  lower_value: i64,
  upper_value: i64,
  field: &str,
) -> Result<Option<IteratorAndCount<Disi<LR>>>>
where
  LR: LeafReader,
{
  if lower_value > upper_value {
    return Ok(Some(IteratorAndCount::empty()));
  }

  let mut sorted_numeric_values = DocValues::get_sorted_numeric(context.reader(), field)?;

  if !sorted_numeric_values.is_single_valued() {
    return Ok(None);
  }
  let numeric_values = DocValues::unwrap_singleton_numeric(&mut sorted_numeric_values)?;
  let (it_and_count_opt, disi_opt) = get_doc_id_set_iterator_or_null_from_bkd(
    context,
    numeric_values,
    lower_value,
    upper_value,
    field,
  )?;
  match (it_and_count_opt, disi_opt) {
    (Some(itc), None) => Ok(Some(itc)),

    (None, Some(numeric_values)) => {
      let meta = context.reader().get_metadata()?;
      if let Some(index_sort) = meta.get_sort() {
        let sort_fields = index_sort.get_sort();

        if !sort_fields.is_empty() && sort_fields[0].get_field() == Some(field) {
          let sort_field = &sort_fields[0];
          let sort_field_type = get_sort_field_type(sort_field);

          // Only INT and LONG supported
          if sort_field_type == SortFieldType::Int || sort_field_type == SortFieldType::Long {
            let it_and_count = get_doc_id_set_iterator(
              sort_field,
              context,
              numeric_values,
              lower_value,
              upper_value,
              field,
            )?;
            return Ok(Some(it_and_count));
          }
        }
      }
      Ok(None)
    },
    _ => Err(LuceneError::illegal_state("should not be here")),
  }
}
fn match_none<P>(points: &P, query_lower_point: &[u8], query_upper_point: &[u8]) -> Result<bool>
where
  P: PointValues,
{
  debug_assert!(points.get_num_dimensions()? == 1);
  let comparator = ArrayUtil::get_unsigned_comparator(points.get_bytes_per_dimension()?);
  match points.get_min_packed_value()? {
    None => {
      return Err(LuceneError::illegal_state(
        "point values has no min packed value",
      ));
    },
    Some(v) => {
      let min_cmp = comparator.compare(v.as_ref(), 0, query_upper_point, 0);
      if min_cmp > 0 {
        return Ok(true);
      }
    },
  }
  match points.get_max_packed_value()? {
    None => {
      return Err(LuceneError::illegal_state(
        "point values has no max packed value",
      ));
    },
    Some(v) => {
      let max_cmp = comparator.compare(v.as_ref(), 0, query_lower_point, 0);
      if max_cmp < 0 {
        return Ok(true);
      }
    },
  }

  Ok(false)
}
fn match_all<P>(points: &P, query_lower_point: &[u8], query_upper_point: &[u8]) -> Result<bool>
where
  P: PointValues,
{
  debug_assert!(points.get_num_dimensions()? == 1);

  let comparator = ArrayUtil::get_unsigned_comparator(points.get_bytes_per_dimension()?);
  let min = points
    .get_min_packed_value()?
    .ok_or_else(|| LuceneError::illegal_state("point values has no min packed value"))?;
  let min_cmp = comparator.compare(min.as_ref(), 0, query_lower_point, 0);

  if min_cmp < 0 {
    return Ok(false);
  }
  let max = points
    .get_max_packed_value()?
    .ok_or_else(|| LuceneError::illegal_state("point values has no max packed value"))?;

  let max_cmp = comparator.compare(max.as_ref(), 0, query_upper_point, 0);
  Ok(max_cmp <= 0)
}
#[allow(clippy::type_complexity)]
fn get_doc_id_set_iterator_or_null_from_bkd<LR>(
  context: &LeafReaderContext<LR>,
  delegate: Disi<LR>,
  lower_value: i64,
  upper_value: i64,
  field: &str,
) -> Result<(Option<IteratorAndCount<Disi<LR>>>, Option<Disi<LR>>)>
where
  LR: LeafReader,
{
  let index_sort = context.reader().get_metadata()?.get_sort();
  if index_sort.is_none()
    || index_sort.as_ref().unwrap().get_sort().is_empty()
    || index_sort.as_ref().unwrap().get_sort()[0].get_field() != Some(field)
  {
    return Ok((None, Some(delegate)));
  }

  let points = context.reader().get_point_values(field)?;
  let points = match points {
    Some(p) => p,
    None => return Ok((None, Some(delegate))),
  };

  if points.get_num_dimensions()? != 1 {
    return Ok((None, Some(delegate)));
  }

  let bpd = points.get_bytes_per_dimension()?;
  if bpd != BitUtil::INT_BYTES && bpd != BitUtil::LONG_BYTES {
    return Ok((None, Some(delegate)));
  }

  if points.size()? != points.get_doc_count()?.try_convert()? {
    return Ok((None, Some(delegate)));
  }

  debug_assert!(lower_value <= upper_value);

  let (query_lower_point, query_upper_point) = if bpd == BitUtil::INT_BYTES {
    (
      IntPoint::pack([lower_value as i32])?,
      IntPoint::pack([upper_value as i32])?,
    )
  } else {
    (
      LongPoint::pack([lower_value])?,
      LongPoint::pack([upper_value])?,
    )
  };

  if match_none(&points, &query_lower_point.bytes, &query_upper_point.bytes)? {
    return Ok((Some(IteratorAndCount::empty()), None));
  }

  if match_all(&points, &query_lower_point.bytes, &query_upper_point.bytes)? {
    let max_doc = context.reader().max_doc()?;

    if points.get_doc_count()? == max_doc {
      return Ok((Some(IteratorAndCount::all(max_doc)), None));
    } else {
      return Ok((
        Some(IteratorAndCount::sparse_range(0, max_doc, delegate)),
        None,
      ));
    }
  }

  let reverse = index_sort.as_ref().unwrap().get_sort()[0].get_reverse();
  let comparator = ArrayUtil::get_unsigned_comparator(bpd);

  let min_doc_id;
  let mut max_doc_id;

  if reverse {
    min_doc_id = next_doc(
      &mut points.get_point_tree()?,
      &query_upper_point.bytes,
      false,
      &comparator,
      true,
    )? + 1;
  } else {
    min_doc_id = next_doc(
      &mut points.get_point_tree()?,
      &query_lower_point.bytes,
      true,
      &comparator,
      false,
    )?;
    if min_doc_id == -1 {
      return Ok((Some(IteratorAndCount::empty()), None));
    }
  }

  if reverse {
    max_doc_id = next_doc(
      &mut points.get_point_tree()?,
      &query_lower_point.bytes,
      true,
      &comparator,
      true,
    )? + 1;

    if max_doc_id == 0 {
      return Ok((Some(IteratorAndCount::empty()), None));
    }
  } else {
    max_doc_id = next_doc(
      &mut points.get_point_tree()?,
      &query_upper_point.bytes,
      false,
      &comparator,
      false,
    )?;

    if max_doc_id == -1 {
      max_doc_id = context.reader().max_doc()?;
    }
  }

  if min_doc_id == max_doc_id {
    return Ok((Some(IteratorAndCount::empty()), None));
  }

  if points.get_doc_count()? == context.reader().max_doc()? {
    Ok((
      Some(IteratorAndCount::dense_range(min_doc_id, max_doc_id)?),
      None,
    ))
  } else {
    Ok((
      Some(IteratorAndCount::sparse_range(
        min_doc_id, max_doc_id, delegate,
      )),
      None,
    ))
  }
}
trait ValueComparator {
  fn compare(&mut self, doc_id: i32) -> Result<i32>;
}
struct ValueComparatorImpl<LR>
where
  LR: LeafReader,
{
  field_comparator: FieldComparatorEnum,
  leaf_field_comparator: LeafFieldComparatorEnum<LR>,
  direction: i32,
}
impl<LR> ValueComparatorImpl<LR>
where
  LR: LeafReader,
{
  pub fn new(
    mut field_comparator: FieldComparatorEnum,
    direction: i32,
    context: &LeafReaderContext<LR>,
  ) -> Result<Self> {
    let leaf_field_comparator = field_comparator.get_leaf_comparator(context)?;
    Ok(Self {
      field_comparator,
      leaf_field_comparator,
      direction,
    })
  }
}
impl<LR> ValueComparator for ValueComparatorImpl<LR>
where
  LR: LeafReader,
{
  fn compare(&mut self, doc_id: i32) -> Result<i32> {
    let mut v = DummyScorer;
    let value =
      self
        .leaf_field_comparator
        .compare_top(doc_id, &mut v, &mut self.field_comparator)?;
    Ok(self.direction * value)
  }
}
fn load_comparator<LR>(
  sort_field: &SortFieldEnum,
  top_value: i64,
  context: &LeafReaderContext<LR>,
) -> Result<ValueComparatorImpl<LR>>
where
  LR: LeafReader,
{
  let mut field_comparator = sort_field.get_comparator(1, Pruning::None)?;
  match field_comparator {
    FieldComparatorEnum::Long(ref mut fc) => {
      fc.set_top_value(top_value)?;
    },
    FieldComparatorEnum::SortedNumericLong(ref mut fc) => {
      fc.set_top_value(top_value)?;
    },
    FieldComparatorEnum::Int(ref mut fc) => {
      fc.set_top_value(top_value as i32)?;
    },
    FieldComparatorEnum::SortedNumericInt(ref mut fc) => {
      fc.set_top_value(top_value as i32)?;
    },
    _ => {
      return Err(LuceneError::illegal_argument(
        "Expected Long or Int FieldComparator",
      ));
    },
  }

  let direction = if sort_field.get_reverse() { -1 } else { 1 };

  ValueComparatorImpl::new(field_comparator, direction, context)
}

fn get_sort_field_type(sort_field: &SortFieldEnum) -> SortFieldType {
  // We expect the sortField to be SortedNumericSortField
  match sort_field {
    SortFieldEnum::SortedNumeric(sf) => sf.get_numeric_type(),
    _ => sort_field.get_type(),
  }
}

struct IteratorAndCount<D> {
  it: IteratorAndCountDisi<D>,
  count: i32,
}

impl<D> IteratorAndCount<D> {
  fn new(it: IteratorAndCountDisi<D>, count: i32) -> Self {
    Self { it, count }
  }

  fn empty() -> Self {
    IteratorAndCount::new(DocIdSetIteratorEnum4::A(EmptyDISI::default()), 0)
  }

  fn all(max_doc: i32) -> Self {
    IteratorAndCount::new(DocIdSetIteratorEnum4::B(AllDISI::new(max_doc)), max_doc)
  }

  fn dense_range(min_doc: i32, max_doc: i32) -> Result<Self> {
    Ok(IteratorAndCount::new(
      DocIdSetIteratorEnum4::C(RangeDISI::new(min_doc, max_doc)?),
      max_doc - min_doc,
    ))
  }

  fn sparse_range(min_doc: i32, max_doc: i32, delegate: D) -> IteratorAndCount<D> {
    let v = BoundedDocIdSetIterator::new(min_doc, max_doc, delegate);
    IteratorAndCount::new(DocIdSetIteratorEnum4::D(v), -1)
  }
}

pub type IteratorAndCountDisi<D> =
  DocIdSetIteratorEnum4<EmptyDISI, AllDISI, RangeDISI, BoundedDocIdSetIterator<D>>;
// for std::mem::take
impl<D> Default for IteratorAndCountDisi<D> {
  fn default() -> Self {
    DocIdSetIteratorEnum4::A(EmptyDISI::default())
  }
}

impl crate::core::util::accountable::Accountable for IndexSortSortedNumericDocValuesRangeQuery {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(crate::core::util::ram_usage_estimator::QUERY_DEFAULT_RAM_BYTES_USED)
  }
}
