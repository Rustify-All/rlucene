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
use crate::core::index::doc_values::{DocValues, Numeric, SortedNumeric};
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::point_values::{
  IntersectVisitor, PointValues, Relation, is_estimated_point_count_greater_than_or_equal_to,
};
use crate::core::search::doc_id_set_iterator::{AllDISI, DocIdSetIterator, DocIdSetIteratorEnum3};
use crate::core::search::pruning::Pruning;
use crate::core::search::scorable::Scorable;
use crate::core::search::sort_field::SortFieldType;
use crate::core::search::sorted_numeric_selector::{
  SortedNumericSelector, SortedNumericSelectorType, SortedNumericSelectorWrap,
};
use crate::core::util::ToInt;
use crate::core::util::doc_id_set_builder::{DocIdSetBuilder, OwnedDocIdSetBuilderIterator};
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind, Result};

const MIN_SKIP_INTERVAL: i32 = 32;
const MAX_SKIP_INTERVAL: i32 = 8192;
/// Base numeric comparator for comparing numeric values.
/// This comparator provides a skipping functionality – an iterator that can skip over
/// non-competitive documents.
///
/// The `field` parameter supplied at creation is used as a field name in the default
/// implementations of the methods `get_numeric_doc_values` and `get_point_values` to retrieve
/// doc values and points.
///
/// You can pass a dummy value for a field name (e.g. when sorting by script),
/// but in this case both methods must be provided.
#[derive(Default)]
pub struct NumericComparator<V> {
  pub(crate) field: String,
  pub(crate) selector: Option<(SortedNumericSelectorType, SortFieldType)>,
  missing_value_as_long: i64,
  pub(crate) reverse: bool,
  bytes_count: usize, // how many bytes are used to encode this number
  pub(crate) missing_value: V,
  pub(crate) top_value_set: bool,
  pub(crate) single_sort: bool, // true if sort is based on a single sort field
  pub(crate) hits_threshold_reached: bool,
  pub(crate) queue_full: bool,
  pub(crate) pruning: Pruning,
}

impl<V> NumericComparator<V> {
  pub fn new<FName>(
    field: FName,
    missing_value: V,
    reverse: bool,
    pruning: Pruning,
    bytes_count: usize,
    missing_value_as_long: i64,
  ) -> Self
  where
    FName: Into<String>,
  {
    let field = field.into();
    Self {
      field,
      selector: None,
      missing_value_as_long,
      reverse,
      bytes_count,
      missing_value,
      top_value_set: false,
      single_sort: false,
      hits_threshold_reached: false,
      queue_full: false,
      pruning,
    }
  }
  pub(crate) fn get_numeric_doc_values<LR: LeafReader>(
    &self,
    reader: Option<&LR>,
  ) -> Result<NumericLeafComparatorDocValues<LR>> {
    let reader =
      reader.ok_or_else(|| LuceneError::illegal_state("skip doc values are not initialized"))?;
    match self.selector {
      Some((selector, type_)) => Ok(NumericLeafComparatorDocValues::A(
        SortedNumericSelector::wrap(
          DocValues::get_sorted_numeric(reader, &self.field)?,
          selector,
          type_,
        )?,
      )),
      None => Ok(NumericLeafComparatorDocValues::B(DocValues::get_numeric(
        reader,
        &self.field,
      )?)),
    }
  }

  pub(crate) fn set_top_value(&mut self) {
    self.top_value_set = true;
  }
  pub(crate) fn set_single_sort(&mut self) {
    self.single_sort = true;
  }

  pub(crate) fn disable_skipping(&mut self) {
    self.pruning = Pruning::None;
  }
}

// Keep the shared cursor preparation in the concrete constructors that consume it.
macro_rules! prepare_numeric_doc_values {
  ($parent:expr, $context:expr, $doc_values:expr, $candidate:expr) => {{
    match ($doc_values, $candidate) {
      (Some(doc_value), Some(candidate)) => (doc_value, Some(candidate), None),
      (None, None) => {
        let doc_value = ($parent).get_numeric_doc_values(Some(($context).reader()))?;
        let context_reader = ($context).clone_reader();
        let candidate = if context_reader.is_none() {
          Some(($parent).get_numeric_doc_values(Some(($context).reader()))?)
        } else {
          None
        };
        (doc_value, candidate, context_reader)
      },
      _ => {
        return Err(
          $crate::core::util::error::lucene_error::LuceneError::illegal_state(
            "doc_values and candidate must be both Some or None",
          ),
        );
      },
    }
  }};
}
pub(crate) use prepare_numeric_doc_values;

#[allow(clippy::type_complexity)]
pub struct NumericLeafComparator<PV, N, T, C = N>
where
  PV: PointValues,
  T: ToLong,
{
  pub(crate) doc_values: N,
  point_values: Option<PV>,
  // lazily constructed to avoid performance overhead when this is not used
  point_tree: Option<PV::PointTree>,
  // if skipping functionality should be enabled on this segment
  enable_skipping: bool,
  max_doc: i32,
  leaf_top_set: bool,
  min_value_as_long: i64,
  max_value_as_long: i64,
  competitive_iterator: Option<CompetitiveIterator<CompetitiveIteratorType<C>>>,
  iterator_cost: i64,
  max_doc_visited: i32,
  update_counter: i32,
  current_skip_interval: i32,
  // helps to be conservative about increasing the sampling interval
  try_update_fail_count: i32,
  // An explicit cursor supplied by a caller. Reader-based comparators open it on demand.
  skip_doc_values: Option<Box<C>>,
  convert: T,
}
impl<PV, N, T, C> NumericLeafComparator<PV, N, T, C>
where
  PV: PointValues,
  N: NumericDocValues,
  C: NumericDocValues,
  T: ToLong,
{
  pub fn new<LR>(
    context: &LeafReaderContext<LR>,
    parent: &mut NumericComparator<T::V>,
    doc_values: N,
    candidate: C,
    v_to_long: T,
    top: T::V,
  ) -> Result<Self>
  where
    LR: LeafReader<PointValues = PV>,
  {
    Self::with_candidate(context, parent, doc_values, Some(candidate), v_to_long, top)
  }

  pub(crate) fn with_candidate<LR>(
    context: &LeafReaderContext<LR>,
    parent: &mut NumericComparator<T::V>,
    doc_values: N,
    candidate: Option<C>,
    v_to_long: T,
    top: T::V,
  ) -> Result<Self>
  where
    LR: LeafReader<PointValues = PV>,
  {
    let field = &parent.field;

    let point_values = if parent.pruning != Pruning::None {
      context.reader().get_point_values(field)?
    } else {
      None
    };

    let (enable_skipping, max_doc, competitive_iterator) = if point_values.is_some() {
      if let Some(info) = context
        .reader()
        .get_field_infos()?
        .field_info_by_name(field)?
      {
        if info.get_point_dimension_count() == 0 {
          return Err(LuceneError::illegal_state(format!(
            "Field {} doesn't index points according to FieldInfos yet returns non-null PointValues",
            field
          )));
        } else if info.get_point_dimension_count() > 1 {
          return Err(LuceneError::illegal_argument(format!(
            "Field {} is indexed with multiple dimensions, sorting is not supported",
            field
          )));
        } else if info.get_point_num_bytes() != parent.bytes_count {
          return Err(LuceneError::illegal_argument(format!(
            "Field {} is indexed with {} bytes per dimension, but expected {}",
            field,
            info.get_point_num_bytes(),
            parent.bytes_count
          )));
        }
      } else {
        return Err(LuceneError::illegal_state(format!(
          "Field {} has no FieldInfo but returned non-null PointValues",
          field
        )));
      }

      let max_doc = context.reader().max_doc()?;
      let competitive_iterator = Some(CompetitiveIterator::new(CompetitiveIteratorType::A(
        AllDISI::new(max_doc),
      )));
      (true, max_doc, competitive_iterator)
    } else {
      (false, 0, None)
    };

    let mut v = Self {
      doc_values,
      point_values,
      point_tree: None,
      enable_skipping,
      max_doc,
      leaf_top_set: parent.top_value_set,
      min_value_as_long: i64::MIN,
      max_value_as_long: i64::MAX,
      competitive_iterator,
      iterator_cost: -1,
      max_doc_visited: -1,
      update_counter: 0,
      current_skip_interval: MIN_SKIP_INTERVAL,
      try_update_fail_count: 0,
      skip_doc_values: candidate.map(Box::new),
      convert: v_to_long,
    };
    if v.point_values.is_some() && v.leaf_top_set {
      v.encode_top(top, parent);
    }
    Ok(v)
  }
  fn update_competitive_iterator(
    &mut self,
    bottom: T::V,
    top: T::V,
    comparator: &mut NumericComparator<T::V>,
    open_doc_values: impl FnOnce(&NumericComparator<T::V>) -> Result<C>,
  ) -> Result<()> {
    if !self.enable_skipping
      || !comparator.hits_threshold_reached
      || (!self.leaf_top_set && !comparator.queue_full)
    {
      return Ok(());
    }
    debug_assert!(self.point_values.is_some());
    // if some documents have missing points, check that missing values prohibits optimization
    if let Some(ref pv) = self.point_values
      && pv.get_doc_count()? < self.max_doc
      && self.is_missing_value_competitive(bottom, top, comparator)
    {
      return Ok(()); // we can't filter out documents, as documents with missing values are competitive
    }

    self.update_counter += 1;

    // Start sampling if we get called too much
    if self.update_counter > 256
      && (self.update_counter & (self.current_skip_interval - 1)) != self.current_skip_interval - 1
    {
      return Ok(());
    }

    if comparator.queue_full {
      self.encode_bottom(bottom, comparator);
    }

    let result = DocIdSetBuilder::new(self.max_doc);

    self.init_point_tree()?;
    let mut visitor = IntersectVisitorImpl::new(
      result,
      self.max_doc_visited,
      self.min_value_as_long,
      self.max_value_as_long,
      &self.convert,
    );

    let threshold = ((self.iterator_cost as u64) >> 3) as i64;
    if self.point_values.is_some() {
      let point_tree = self
        .point_tree
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("point tree is not initialized"))?;
      if is_estimated_point_count_greater_than_or_equal_to(&visitor, point_tree, threshold)? {
        // the new range is not selective enough to be worth materializing, it doesn't reduce number
        // of docs at least 8x
        self.update_skip_interval(false);

        let pv = self
          .point_values
          .as_ref()
          .ok_or_else(|| LuceneError::illegal_state("point values are not initialized"))?;
        if (pv.get_doc_count()? as i64) < self.iterator_cost {
          let skip_doc_values = match self.skip_doc_values.take() {
            Some(doc_values) => *doc_values,
            None => open_doc_values(comparator)?,
          };
          self.competitive_iterator = Some(CompetitiveIterator::new(CompetitiveIteratorType::B(
            skip_doc_values,
          )));

          self.iterator_cost = pv.get_doc_count()? as i64
        }
        return Ok(());
      }
      self
        .point_values
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("point values are not initialized"))?
        .intersect(&mut visitor)?;
    } else {
      return Err(LuceneError::illegal_state("point_values is None"));
    }

    let it = visitor.result.build_iterator()?;
    self.iterator_cost = it.cost()?;
    self.competitive_iterator = Some(CompetitiveIterator::new(CompetitiveIteratorType::C(it)));
    self.update_skip_interval(true);
    Ok(())
  }
  fn init_point_tree(&mut self) -> Result<()> {
    if self.point_tree.is_none() {
      if let Some(ref mut pv) = self.point_values {
        self.point_tree = Some(pv.get_point_tree()?);
      } else {
        return Err(LuceneError::illegal_state(
          "point_values is None but get_point_tree() was called",
        ));
      }
    }
    Ok(())
  }
  fn update_skip_interval(&mut self, success: bool) {
    if self.update_counter > 256 {
      if success {
        self.current_skip_interval = (self.current_skip_interval / 2).max(MIN_SKIP_INTERVAL);
        self.try_update_fail_count = 0;
      } else if self.try_update_fail_count >= 3 {
        self.current_skip_interval = (self.current_skip_interval * 2).min(MAX_SKIP_INTERVAL);
        self.try_update_fail_count = 0;
      } else {
        self.try_update_fail_count += 1;
      }
    }
  }
  fn encode_bottom(&mut self, bottom: T::V, comparator: &mut NumericComparator<T::V>) {
    if !comparator.reverse {
      // ascending order
      self.max_value_as_long = self.convert.value_to_long(bottom);
      if comparator.pruning == Pruning::GreaterThanOrEqualTo && self.max_value_as_long != i64::MIN {
        self.max_value_as_long -= 1;
      }
    } else {
      // descending order
      self.min_value_as_long = self.convert.value_to_long(bottom);
      if comparator.pruning == Pruning::GreaterThanOrEqualTo && self.min_value_as_long != i64::MAX {
        self.min_value_as_long += 1;
      }
    }
  }
  fn encode_top(&mut self, top: T::V, comparator: &mut NumericComparator<T::V>) {
    if !comparator.reverse {
      self.min_value_as_long = self.convert.value_to_long(top);
      if comparator.single_sort
        && comparator.pruning == Pruning::GreaterThanOrEqualTo
        && comparator.queue_full
        && self.min_value_as_long != i64::MAX
      {
        self.min_value_as_long += 1;
      }
    } else {
      // descending order
      self.max_value_as_long = self.convert.value_to_long(top);
      if comparator.single_sort
        && comparator.pruning == Pruning::GreaterThanOrEqualTo
        && comparator.queue_full
        && self.max_value_as_long != i64::MIN
      {
        self.max_value_as_long -= 1;
      }
    }
  }
  #[allow(clippy::collapsible_else_if)]
  fn is_missing_value_competitive(
    &self,
    bottom: T::V,
    top: T::V,
    comparator: &mut NumericComparator<T::V>,
  ) -> bool {
    // if queue is full, compare with bottom first,
    // if competitive, then check if we can compare with topValue
    if comparator.queue_full {
      let result = comparator
        .missing_value_as_long
        .cmp(&self.convert.value_to_long(bottom))
        .to_int();
      // in reverse (desc) sort missingValue is competitive when it's greater or equal to bottom,
      // in asc sort missingValue is competitive when it's smaller or equal to bottom
      let competitive = if comparator.reverse {
        if comparator.pruning == Pruning::GreaterThanOrEqualTo {
          result > 0
        } else {
          result >= 0
        }
      } else {
        if comparator.pruning == Pruning::GreaterThanOrEqualTo {
          result < 0
        } else {
          result <= 0
        }
      };

      if !competitive {
        return false;
      }
    }

    if self.leaf_top_set {
      let result = comparator
        .missing_value_as_long
        .cmp(&self.convert.value_to_long(top))
        .to_int();
      // in reverse (desc) sort missingValue is competitive when it's smaller or equal to
      // topValue,
      // in asc sort missingValue is competitive when it's greater or equal to topValue

      return if comparator.reverse {
        result <= 0
      } else {
        result >= 0
      };
    }
    // by default competitive
    true
  }

  pub(crate) fn set_bottom(
    &mut self,
    bottom: T::V,
    top: T::V,
    comparator: &mut NumericComparator<T::V>,
    open_doc_values: impl FnOnce(&NumericComparator<T::V>) -> Result<C>,
  ) -> Result<()> {
    comparator.queue_full = true; // if we are setting bottom, it means that we have collected enough hits
    self.update_competitive_iterator(bottom, top, comparator, open_doc_values)?; // update an iterator if we set a new bottom
    Ok(())
  }

  pub(crate) fn copy(&mut self, doc: i32) -> Result<()> {
    self.max_doc_visited = doc;
    Ok(())
  }

  pub(crate) fn set_scorer<S>(
    &mut self,
    scorer: &mut S,
    bottom: T::V,
    top: T::V,
    comparator: &mut NumericComparator<T::V>,
    open_doc_values: impl FnOnce(&NumericComparator<T::V>) -> Result<C>,
  ) -> Result<()>
  where
    S: Scorable + ?Sized,
  {
    if self.iterator_cost == -1 {
      match scorer.cost() {
        Ok(c) => self.iterator_cost = c,
        Err(e) => match e.kind() {
          LuceneErrorKind::UnsupportedOperation(_) => {
            self.iterator_cost = self.max_doc as i64;
          },
          _ => return Err(e),
        },
      };
      self.update_competitive_iterator(bottom, top, comparator, open_doc_values)?;
    }
    Ok(())
  }

  pub(crate) fn competitive_iterator(
    &mut self,
  ) -> Option<&mut CompetitiveIterator<CompetitiveIteratorType<C>>> {
    self.competitive_iterator.as_mut()
  }

  pub(crate) fn set_hits_threshold_reached(
    &mut self,
    bottom: T::V,
    top: T::V,
    comparator: &mut NumericComparator<T::V>,
    open_doc_values: impl FnOnce(&NumericComparator<T::V>) -> Result<C>,
  ) -> Result<()> {
    comparator.hits_threshold_reached = true;
    self.update_competitive_iterator(bottom, top, comparator, open_doc_values)
  }
}
pub struct CompetitiveIterator<D> {
  competitive_iterator: D,
  doc_id: i32,
}
impl<D> CompetitiveIterator<D>
where
  D: DocIdSetIterator,
{
  pub fn new(competitive_iterator: D) -> Self {
    let doc_id = competitive_iterator.doc_id();
    Self {
      competitive_iterator,
      doc_id,
    }
  }
}
impl<D> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for CompetitiveIterator<D>
where
  D: DocIdSetIterator,
{
}
impl<D> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for CompetitiveIterator<D> where
  D: DocIdSetIterator
{
}

impl<D> DocIdSetIterator for CompetitiveIterator<D>
where
  D: DocIdSetIterator,
{
  fn doc_id(&self) -> i32 {
    self.doc_id
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.advance(self.doc_id + 1)
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.doc_id = self.competitive_iterator.advance(target)?;
    Ok(self.doc_id)
  }

  fn cost(&self) -> Result<i64> {
    self.competitive_iterator.cost()
  }
}

struct IntersectVisitorImpl<'a, T> {
  result: DocIdSetBuilder,
  max_doc_visited: i32,
  min_value_as_long: i64,
  max_value_as_long: i64,
  as_long: &'a T,
}
impl<'a, T> IntersectVisitorImpl<'a, T> {
  fn new(
    result: DocIdSetBuilder,
    max_doc_visited: i32,
    min_value_as_long: i64,
    max_value_as_long: i64,
    as_long: &'a T,
  ) -> Self {
    Self {
      result,
      max_doc_visited,
      min_value_as_long,
      max_value_as_long,
      as_long,
    }
  }
}
impl<T> IntersectVisitor for IntersectVisitorImpl<'_, T>
where
  T: ToLong,
{
  fn visit(&mut self, doc_id: i32) -> Result<()> {
    if doc_id <= self.max_doc_visited {
      return Ok(()); // Already visited or skipped
    }
    self.result.add_doc(doc_id)?;
    Ok(())
  }

  fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()> {
    if doc_id <= self.max_doc_visited {
      return Ok(()); // Already visited or skipped
    }
    let l = self.as_long.bytes_to_long(packed_value);
    if l >= self.min_value_as_long && l <= self.max_value_as_long {
      self.result.add_doc(doc_id)?; // doc is competitive
    }
    Ok(())
  }

  fn compare(&self, min_packed_value: &[u8], max_packed_value: &[u8]) -> Result<Relation> {
    let min = self.as_long.bytes_to_long(min_packed_value);
    let max = self.as_long.bytes_to_long(max_packed_value);

    if min > self.max_value_as_long || max < self.min_value_as_long {
      // 1. cmp ==0 and pruning==Pruning.GREATER_THAN_OR_EQUAL_TO : if the sort is
      // ascending then maxValueAsLong is bottom's next less value, so it is competitive
      // 2. cmp ==0 and pruning==Pruning.GREATER_THAN: maxValueAsLong equals to
      // bottom, but there are multiple comparators, so it could be competitive
      Ok(Relation::CellOutsideQuery)
    } else if min < self.min_value_as_long || max > self.max_value_as_long {
      Ok(Relation::CellCrossesQuery)
    } else {
      Ok(Relation::CellInsideQuery)
    }
  }

  fn grow(&mut self, count: usize) -> Result<()> {
    self.result.grow(count)?;
    Ok(())
  }
}
pub type CompetitiveIteratorType<T> =
  DocIdSetIteratorEnum3<AllDISI, T, OwnedDocIdSetBuilderIterator>;
pub trait ToLong {
  type V: PartialOrd + Copy;
  fn value_to_long(&self, v: Self::V) -> i64;
  fn bytes_to_long(&self, bytes: &[u8]) -> i64;
}

pub enum NumericLeafComparatorDocValues<LR>
where
  LR: LeafReader,
{
  A(SortedNumericSelectorWrap<SortedNumeric<LR>>),
  B(Numeric<LR>),
}

impl<LR> DocValuesIterator for NumericLeafComparatorDocValues<LR>
where
  LR: LeafReader,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      Self::A(values) => values.advance_exact(target),
      Self::B(values) => values.advance_exact(target),
    }
  }
}

impl<LR> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for NumericLeafComparatorDocValues<LR>
where
  LR: LeafReader,
{
}
impl<LR> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for NumericLeafComparatorDocValues<LR>
where
  LR: LeafReader,
{
}

impl<LR> DocIdSetIterator for NumericLeafComparatorDocValues<LR>
where
  LR: LeafReader,
{
  fn doc_id(&self) -> i32 {
    match self {
      Self::A(values) => values.doc_id(),
      Self::B(values) => values.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::A(values) => values.next_doc(),
      Self::B(values) => values.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::A(values) => values.advance(target),
      Self::B(values) => values.advance(target),
    }
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::A(values) => values.slow_advance(target),
      Self::B(values) => values.slow_advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::A(values) => values.cost(),
      Self::B(values) => values.cost(),
    }
  }
}

impl<LR> NumericDocValues for NumericLeafComparatorDocValues<LR>
where
  LR: LeafReader,
{
  fn long_value(&mut self) -> Result<i64> {
    match self {
      Self::A(values) => values.long_value(),
      Self::B(values) => values.long_value(),
    }
  }
}
pub type NumericCompetitiveIterator<LR> =
  CompetitiveIterator<CompetitiveIteratorType<NumericLeafComparatorDocValues<LR>>>;
