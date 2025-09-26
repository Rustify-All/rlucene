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
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::point_values::{
    IntersectVisitor, PointValues, Relation, is_estimated_point_count_greater_than_or_equal_to,
};
use crate::core::search::doc_id_set::DocIdSet;
use crate::core::search::doc_id_set_iterator::{
    AllDISI, DocIdSetIterator, Either3DocIdSetIterator,
};
use crate::core::search::pruning::Pruning;
use crate::core::search::scorable::{Scorable, ScorerEnum};
use crate::core::search::scorer::Scorer;
use crate::core::util::ToInt;
use crate::core::util::doc_id_set_builder::{DocIdSetBuilder, DocIdSetBuilderIterator};
use crate::core::util::error::lucene_error::{LuceneError, Result};

const MIN_SKIP_INTERVAL: i32 = 32;
const MAX_SKIP_INTERVAL: i32 = 8192;
/// Base numeric comparator for comparing numeric values.
/// This comparator provides a skipping functionality – an iterator that can skip over
/// non-competitive documents.
///
/// The parameter `field` provided in the constructor is used as a field name in the default
/// implementations of the methods `get_numeric_doc_values` and `get_point_values` to retrieve
/// doc values and points.
///
/// You can pass a dummy value for a field name (e.g. when sorting by script),
/// but in this case you must override both of these methods.
#[derive(Default)]
pub struct NumericComparator<V>
where
    V: PartialOrd + Copy,
{
    pub(crate) field: String,
    missing_value_as_long: i64,
    pub(crate) reverse: bool,
    bytes_count: i32, // how many bytes are used to encode this number
    pub(crate) missing_value: V,
    pub(crate) top_value_set: bool,
    pub(crate) single_sort: bool, // true if sort is based on a single sort field
    pub(crate) hits_threshold_reached: bool,
    pub(crate) queue_full: bool,
    pub(crate) pruning: Pruning,
}

impl<V> NumericComparator<V>
where
    V: PartialOrd + Copy,
{
    pub fn new(
        field: String,
        missing_value: V,
        reverse: bool,
        pruning: Pruning,
        bytes_count: i32,
        missing_value_as_long: i64,
    ) -> Self {
        Self {
            field,
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
    pub(crate) fn set_top_value(&mut self) {
        self.top_value_set = true;
    }
    fn set_single_sort(&mut self) {
        self.single_sort = true;
    }

    fn disable_skipping(&mut self) {
        self.pruning = Pruning::None;
    }
}

pub struct NumericLeafComparator<LR, N, V, T>
where
    LR: LeafReader,
    N: NumericDocValues,
    V: PartialOrd + Copy,
    T: ToLong<V = V>,
{
    pub(crate) doc_values: N,
    point_values: Option<LR::PointValues>,
    // lazily constructed to avoid performance overhead when this is not used
    point_tree: Option<<LR::PointValues as PointValues>::PointTree>,
    // if skipping functionality should be enabled on this segment
    enable_skipping: bool,
    max_doc: i32,
    leaf_top_set: bool,
    min_value_as_long: i64,
    max_value_as_long: i64,
    competitive_iterator: Option<CompetitiveIteratorType<N>>,
    iterator_cost: i64,
    max_doc_visited: i32,
    update_counter: i32,
    current_skip_interval: i32,
    // helps to be conservative about increasing the sampling interval
    try_update_fail_count: i32,
    pub(crate) parent: NumericComparator<V>,
    skip_doc_values: Option<N>,
    convert: T,
}
impl<LR, N, V, T> NumericLeafComparator<LR, N, V, T>
where
    LR: LeafReader,
    N: NumericDocValues,
    V: PartialOrd + Copy,
    T: ToLong<V = V>,
{
    pub fn new(
        context: &LeafReaderContext<LR>,
        parent: NumericComparator<V>,
        doc_values: N,
        candidate: N,
        v_to_long: T,
        top: V,
    ) -> Result<Self> {
        let field = &parent.field;

        let point_values = if parent.pruning != Pruning::None {
            context.reader().get_point_values(field)?
        } else {
            None
        };

        let (enable_skipping, max_doc, competitive_iterator) = if let Some(_) = point_values {
            if let Some(info) = context
                .reader()
                .get_field_infos()?
                .field_info_by_name(field)
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
            let competitive_iterator = Some(CompetitiveIteratorType::A(AllDISI::new(max_doc)));
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
            parent,
            skip_doc_values: Some(candidate),
            convert: v_to_long,
        };
        if v.point_values.is_some() && v.leaf_top_set {
            v.encode_top(top);
        }
        Ok(v)
    }
    fn update_competitive_iterator(&mut self, bottom: V, top: V) -> Result<()> {
        if !self.enable_skipping
            || !self.parent.hits_threshold_reached
            || (!self.leaf_top_set && !self.parent.queue_full)
        {
            return Ok(());
        }
        // if some documents have missing points, check that missing values prohibits optimization
        if let Some(ref pv) = self.point_values
            && pv.get_doc_count()? < self.max_doc
            && self.is_missing_value_competitive(bottom, top)
        {
            return Ok(()); // we can't filter out documents, as documents with missing values are competitive
        }

        self.update_counter += 1;

        // Start sampling if we get called too much
        if self.update_counter > 256
            && (self.update_counter & (self.current_skip_interval - 1))
                != self.current_skip_interval - 1
        {
            return Ok(());
        }

        if self.parent.queue_full {
            self.encode_bottom(bottom);
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
            if is_estimated_point_count_greater_than_or_equal_to(
                &visitor,
                self.point_tree.as_mut().unwrap(),
                threshold,
            )? {
                // the new range is not selective enough to be worth materializing, it doesn't reduce number
                // of docs at least 8x
                self.update_skip_interval(false);

                let pv = self.point_values.as_ref().unwrap();
                if (pv.get_doc_count()? as i64) < self.iterator_cost {
                    debug_assert!(self.skip_doc_values.is_some());
                    self.competitive_iterator = Some(CompetitiveIteratorType::B(
                        self.skip_doc_values.take().unwrap(),
                    ));

                    self.iterator_cost = pv.get_doc_count()? as i64
                }
                return Ok(());
            }
            self.point_values
                .as_ref()
                .unwrap()
                .intersect(&mut visitor)?;
        }

        match visitor.result.build()?.iterator()? {
            Some(it) => {
                self.iterator_cost = it.cost()?;
                self.competitive_iterator = Some(CompetitiveIteratorType::C(it));
                self.update_skip_interval(true);
                Ok(())
            },
            None => Err(LuceneError::illegal_state(
                "DocIdSetBuilder returned None iterator",
            ))?,
        }
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
                self.current_skip_interval =
                    (self.current_skip_interval / 2).max(MIN_SKIP_INTERVAL);
                self.try_update_fail_count = 0;
            } else if self.try_update_fail_count >= 3 {
                self.current_skip_interval =
                    (self.current_skip_interval * 2).min(MAX_SKIP_INTERVAL);
                self.try_update_fail_count = 0;
            } else {
                self.try_update_fail_count += 1;
            }
        }
    }
    fn encode_bottom(&mut self, bottom: V) {
        if !self.parent.reverse {
            // ascending order
            self.max_value_as_long = self.convert.value_to_long(bottom);
            if self.parent.pruning == Pruning::GreaterThanOrEqualTo
                && self.max_value_as_long != i64::MIN
            {
                self.max_value_as_long -= 1;
            }
        } else {
            // descending order
            self.min_value_as_long = self.convert.value_to_long(bottom);
            if self.parent.pruning == Pruning::GreaterThanOrEqualTo
                && self.min_value_as_long != i64::MAX
            {
                self.min_value_as_long += 1;
            }
        }
    }
    fn encode_top(&mut self, top: V) {
        if !self.parent.reverse {
            self.min_value_as_long = self.convert.value_to_long(top);
            if self.parent.single_sort
                && self.parent.pruning == Pruning::GreaterThanOrEqualTo
                && self.parent.queue_full
                && self.min_value_as_long != i64::MAX
            {
                self.min_value_as_long += 1;
            }
        } else {
            // descending order
            self.max_value_as_long = self.convert.value_to_long(top);
            if self.parent.single_sort
                && self.parent.pruning == Pruning::GreaterThanOrEqualTo
                && self.parent.queue_full
                && self.max_value_as_long != i64::MIN
            {
                self.max_value_as_long -= 1;
            }
        }
    }
    #[allow(clippy::collapsible_else_if)]
    fn is_missing_value_competitive(&self, bottom: V, top: V) -> bool {
        // if queue is full, compare with bottom first,
        // if competitive, then check if we can compare with topValue
        if self.parent.queue_full {
            let result = self
                .parent
                .missing_value_as_long
                .cmp(&self.convert.value_to_long(bottom))
                .to_int();
            // in reverse (desc) sort missingValue is competitive when it's greater or equal to bottom,
            // in asc sort missingValue is competitive when it's smaller or equal to bottom
            let competitive = if self.parent.reverse {
                if self.parent.pruning == Pruning::GreaterThanOrEqualTo {
                    result > 0
                } else {
                    result >= 0
                }
            } else {
                if self.parent.pruning == Pruning::GreaterThanOrEqualTo {
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
            let result = self
                .parent
                .missing_value_as_long
                .cmp(&self.convert.value_to_long(top))
                .to_int();
            // in reverse (desc) sort missingValue is competitive when it's smaller or equal to
            // topValue,
            // in asc sort missingValue is competitive when it's greater or equal to topValue

            return if self.parent.reverse {
                result <= 0
            } else {
                result >= 0
            };
        }
        // by default competitive
        true
    }

    pub(crate) fn set_bottom(&mut self, bottom: V, top: V) -> Result<()> {
        self.parent.queue_full = true; // if we are setting bottom, it means that we have collected enough hits
        self.update_competitive_iterator(bottom, top)?; // update an iterator if we set a new bottom
        Ok(())
    }

    pub(crate) fn copy(&mut self, doc: i32) -> Result<()> {
        self.max_doc_visited = doc;
        Ok(())
    }

    pub(crate) fn set_scorer<S1, S2>(
        &mut self,
        _scorer: &ScorerEnum<S1, S2>,
        bottom: V,
        top: V,
    ) -> Result<()>
    where
        S1: Scorer,
        S2: Scorable,
    {
        if self.iterator_cost == -1 {
            self.iterator_cost = self.max_doc as i64;
            self.update_competitive_iterator(bottom, top)?;
        }
        Ok(())
    }

    pub(crate) fn competitive_iterator(
        &mut self,
    ) -> Option<CompetitiveIterator<CompetitiveIteratorType<N>>> {
        debug_assert!(self.competitive_iterator.is_some());
        match self.enable_skipping {
            true => Some(CompetitiveIterator::new(
                self.competitive_iterator.take().unwrap(),
            )),
            false => None,
        }
    }

    pub(crate) fn set_hits_threshold_reached(&mut self, bottom: V, top: V) -> Result<()> {
        self.parent.hits_threshold_reached = true;
        self.update_competitive_iterator(bottom, top)
    }
}
pub struct CompetitiveIterator<D>
where
    D: DocIdSetIterator,
{
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

struct IntersectVisitorImpl<'a, T>
where
    T: ToLong,
{
    result: DocIdSetBuilder,
    max_doc_visited: i32,
    min_value_as_long: i64,
    max_value_as_long: i64,
    as_long: &'a T,
}
impl<'a, T> IntersectVisitorImpl<'a, T>
where
    T: ToLong,
{
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
        self.result.add_doc(doc_id);
        Ok(())
    }

    fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()> {
        if doc_id <= self.max_doc_visited {
            return Ok(()); // Already visited or skipped
        }
        let l = self.as_long.bytes_to_long(packed_value);
        if l >= self.min_value_as_long && l <= self.max_value_as_long {
            self.result.add_doc(doc_id); // doc is competitive
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

    fn grow(&mut self, count: i32) -> Result<()> {
        self.result.grow(count);
        Ok(())
    }
}
pub type CompetitiveIteratorType<T> = Either3DocIdSetIterator<AllDISI, T, DocIdSetBuilderIterator>;
pub trait ToLong {
    type V: PartialOrd + Copy;
    fn value_to_long(&self, v: Self::V) -> i64;
    fn bytes_to_long(&self, bytes: &[u8]) -> i64;
}
