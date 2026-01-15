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
use crate::core::codecs::mutable_point_tree::{MutablePointTree, MutablePointTreeEnum2};
use crate::core::index::composite_reader::{CompositeReader, get_context};
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::util::array_util::{ArrayUtil, ByteArrayComparator};
use crate::core::util::bkd::bkd_config::BKDConfig;
use crate::core::util::clone::TryClone;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::ints_ref::IntsRef;
use crate::core::util::{SliceCopyOps, TryIntoInt};
use std::borrow::Cow;
use std::sync::Arc;

pub trait PointValues: Clone {
    /// Returns minimum value for each dimension, packed, or None if `size()` is
    /// `0`
    fn get_min_packed_value(&self) -> Result<Option<Cow<'_, Vec<u8>>>>;

    /// Returns maximum value for each dimension, packed, or None if `size()` is
    /// `0`
    fn get_max_packed_value(&self) -> Result<Option<Cow<'_, Vec<u8>>>>;

    /// Returns how many dimensions are represented in the values
    fn get_num_dimensions(&self) -> Result<usize>;

    /// Returns how many dimensions are used for the index
    fn get_num_index_dimensions(&self) -> Result<usize>;
    /// Returns the number of bytes per dimension
    fn get_bytes_per_dimension(&self) -> Result<usize>;

    /// Returns the total number of indexed points across all documents.
    fn size(&self) -> Result<usize>;

    /// Returns the total number of documents that have indexed at least one
    /// point.
    fn get_doc_count(&self) -> Result<i32>;
    type PointTree: PointTree;
    type MutablePointTree: MutablePointTree;
    fn get_point_tree(&self) -> Result<PointTreeEnum<Self::MutablePointTree, Self::PointTree>>;

    /// Finds all documents and points matching the provided visitor.
    /// This method does not enforce live documents, so it's up to the caller
    /// to test whether each document is deleted, if necessary.
    fn intersect(&self, visitor: &mut impl IntersectVisitor) -> Result<()> {
        let mut point_tree = self.get_point_tree()?;
        intersect_with_point_tree(visitor, &mut point_tree)?;
        debug_assert!(!point_tree.move_to_parent()?);
        Ok(())
    }

    /// Estimate the number of points that would be visited by `intersect`
    /// with the given `IntersectVisitor`. This should run many times faster
    /// than `intersect(IntersectVisitor)`.
    fn estimate_point_count(&self, visitor: &impl IntersectVisitor) -> Result<i64> {
        let mut point_tree = self.get_point_tree()?;
        let count = estimate_point_count_with_point_tree(visitor, &mut point_tree, i64::MAX)?;
        debug_assert!(!point_tree.move_to_parent()?);
        Ok(count)
    }

    /// Estimate the number of documents that would be matched by `intersect`
    /// with the given `IntersectVisitor`. This should run many times faster
    /// than `intersect(IntersectVisitor)`.
    ///
    /// See also: `DocIdSetIterator::cost`
    fn estimate_doc_count(&self, visitor: &impl IntersectVisitor) -> Result<i64> {
        let estimated_point_count = self.estimate_point_count(visitor)?;
        let doc_count = self.get_doc_count()?;
        let size: i64 = self.size()?.try_convert()?;

        if estimated_point_count >= size {
            // math all docs
            Ok(doc_count as i64)
        } else if size == doc_count as i64 || estimated_point_count == 0 {
            Ok(estimated_point_count)
        } else {
            // in case of multi values estimate the number of docs using the
            // solution provided in https://math.stackexchange.com/questions/1175295/urn-problem-probability-of-drawing-balls-of
            //k-unique-colors
            // then approximate the solution for points per doc << size() which
            // results in the expression D * (1 - ((N - n) /
            // N)^(N/D)) where D is the total number of docs, N the
            // total number of points and n the estimated point
            // count
            let doc_estimate = (doc_count as f64
                * (1.0
                    - ((size - estimated_point_count) as f64 / size as f64)
                        .powf(size as f64 / doc_count as f64)))
                as i64;
            Ok(if doc_estimate == 0 { 1 } else { doc_estimate })
        }
    }
}
/// Return the cumulated number of points across all leaves of the given [`IndexReader`](crate::core::index::index_reader::IndexReader).
/// Leaves that do not have points for the given field are ignored.
///
/// See [`PointValues::size`].
pub fn size<CR>(reader: CR, field: &str) -> Result<i64>
where
    CR: CompositeReader,
{
    let leaves = get_context(reader)?;
    let leaves = leaves.leaves()?;
    let mut size = 0_i64;

    for leaf in leaves.iter() {
        if let Some(values) = leaf.reader().get_point_values(field)? {
            let v: i64 = values.size()?.try_convert()?;
            size += v;
        }
    }

    Ok(size)
}

/// Return the cumulated number of docs that have points across all leaves of the given
/// [`IndexReader`](crate::core::index::index_reader::IndexReader).
/// Leaves that do not have points for the given field are ignored.
///
/// See [`PointValues::get_doc_count`].
pub fn get_doc_count<CR>(reader: CR, field: &str) -> Result<i32>
where
    CR: CompositeReader,
{
    let leaves = get_context(reader)?;
    let leaves = leaves.leaves()?;
    let mut count = 0;

    for leaf in leaves.iter() {
        if let Some(values) = leaf.reader().get_point_values(field)? {
            count += values.get_doc_count()?;
        }
    }

    Ok(count)
}

/// Return the minimum packed values across all leaves of the given [`IndexReader`](crate::core::index::index_reader::IndexReader).
/// Leaves that do not have points for the given field are ignored.
///
/// See [`PointValues::get_min_packed_value`].
pub fn get_min_packed_value<CR>(reader: CR, field: &str) -> Result<Option<Vec<u8>>>
where
    CR: CompositeReader,
{
    let leaves = get_context(reader)?;
    let leaves = leaves.leaves()?;
    let mut min_value = None;

    for leaf in leaves.iter() {
        let values = match leaf.reader().get_point_values(field)? {
            Some(v) => v,
            None => continue,
        };

        let leaf_min_value = match values.get_min_packed_value()? {
            Some(v) => v,
            None => continue,
        };

        if min_value.is_none() {
            min_value = Some(leaf_min_value.into_owned());
            continue;
        }

        let min_value_ref = min_value.as_mut().unwrap();

        let num_dimensions = values.get_num_index_dimensions()?;
        let num_bytes_per_dimension = values.get_bytes_per_dimension()?;
        let comparator = ArrayUtil::get_unsigned_comparator(num_bytes_per_dimension);
        for i in 0..num_dimensions {
            let offset = i * num_bytes_per_dimension;
            if comparator.compare(&leaf_min_value, offset, min_value_ref, offset) < 0 {
                min_value_ref.copy_from(
                    &leaf_min_value[offset..offset + num_bytes_per_dimension],
                    offset,
                );
            }
        }
    }

    Ok(min_value)
}
/// Return the maximum packed values across all leaves of the given [`IndexReader`](crate::core::index::index_reader::IndexReader).
/// Leaves that do not have points for the given field are ignored.
///
/// See [`PointValues::get_max_packed_value`].
pub fn get_max_packed_value<CR>(reader: CR, field: &str) -> Result<Option<Vec<u8>>>
where
    CR: CompositeReader,
{
    let ctx = get_context(reader)?;
    let leaves = ctx.leaves()?;
    let mut max_value: Option<Vec<u8>> = None;

    for leaf in leaves.iter() {
        let values = match leaf.reader().get_point_values(field)? {
            Some(v) => v,
            None => continue,
        };

        let leaf_max_value = match values.get_max_packed_value()? {
            Some(v) => v,
            None => continue,
        };

        if max_value.is_none() {
            max_value = Some(leaf_max_value.into_owned());
            continue;
        }

        let max_value_ref = max_value.as_mut().unwrap();

        let num_dimensions = values.get_num_index_dimensions()?;
        let num_bytes_per_dimension = values.get_bytes_per_dimension()?;
        let comparator = ArrayUtil::get_unsigned_comparator(num_bytes_per_dimension);

        for dim in 0..num_dimensions {
            let offset = dim * num_bytes_per_dimension;
            if comparator.compare(&leaf_max_value, offset, max_value_ref, offset) > 0 {
                max_value_ref.copy_from(
                    &leaf_max_value[offset..offset + num_bytes_per_dimension],
                    offset,
                );
            }
        }
    }

    Ok(max_value)
}

/// Estimate if the point count that would be matched by `intersect`
/// with the given `IntersectVisitor` is greater than or equal to the
/// `upper_bound`.
pub(crate) fn is_estimated_point_count_greater_than_or_equal_to(
    visitor: &impl IntersectVisitor,
    point_tree: &mut impl PointTree,
    upper_bound: i64,
) -> Result<bool> {
    Ok(estimate_point_count_with_point_tree(visitor, point_tree, upper_bound)? >= upper_bound)
}
fn intersect_with_point_tree(
    visitor: &mut impl IntersectVisitor,
    point_tree: &mut impl PointTree,
) -> Result<()> {
    let relation = visitor.compare(
        point_tree.get_min_packed_value()?,
        point_tree.get_max_packed_value()?,
    )?;

    match relation {
        Relation::CellOutsideQuery => {
            // This cell is fully outside the query shape: stop recursing
        },
        Relation::CellInsideQuery => {
            // This cell is fully inside the query shape: recursively add
            // all points in this cell without filtering
            point_tree.visit_doc_ids(visitor)?;
        },
        Relation::CellCrossesQuery => {
            // The cell crosses the shape boundary, or the cell fully
            // contains the query, so we fall through and do
            // full filtering:
            if point_tree.move_to_child()? {
                loop {
                    intersect_with_point_tree(visitor, point_tree)?;
                    if !point_tree.move_to_sibling()? {
                        break;
                    }
                }
                point_tree.move_to_parent()?;
            } else {
                // Leaf node; scan and filter all points in this block:
                point_tree.visit_doc_values(visitor)?;
            }
        },
    }

    Ok(())
}

fn estimate_point_count_with_point_tree(
    visitor: &impl IntersectVisitor,
    point_tree: &mut impl PointTree,
    upper_bound: i64,
) -> Result<i64> {
    let relation = visitor.compare(
        point_tree.get_min_packed_value()?,
        point_tree.get_max_packed_value()?,
    )?;

    match relation {
        Relation::CellOutsideQuery => {
            // This cell is fully outside the query shape: no points added
            Ok(0)
        },
        Relation::CellInsideQuery => {
            // This cell is fully inside the query shape: add all points
            Ok(point_tree.size()?.try_convert()?)
        },
        Relation::CellCrossesQuery => {
            // The cell crosses the shape boundary: keep recursing
            if point_tree.move_to_child()? {
                let mut cost = 0;
                while cost < upper_bound {
                    cost += estimate_point_count_with_point_tree(
                        visitor,
                        point_tree,
                        upper_bound - cost,
                    )?;
                    if !point_tree.move_to_sibling()? {
                        break;
                    }
                }
                point_tree.move_to_parent()?;
                Ok(cost)
            } else {
                // Assume half the points matched
                let v: i64 = point_tree.size()?.try_convert()?;
                Ok((v + 1) / 2)
            }
        },
    }
}

/// Used by `intersect` to check how each recursive cell corresponds to the
/// query.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Relation {
    /// Return this if the query fully contains the cell.
    CellInsideQuery,
    /// Return this if the cell and query do not overlap.
    CellOutsideQuery,
    /// Return this if the cell partially overlaps the query.
    CellCrossesQuery,
}
/// Basic operations to read the KD-tree.
pub trait PointTree: TryClone {
    /// Move to the first child node and return `true` upon success.
    /// Returns `false` for leaf nodes and `true` otherwise.
    fn move_to_child(&mut self) -> Result<bool> {
        Err(LuceneError::need_implemented(
            "move_to_child is not implemented",
        ))
    }

    /// Move to the next sibling node and return `true` upon success.
    /// Returns `false` if the current node has no more siblings.
    fn move_to_sibling(&mut self) -> Result<bool> {
        Err(LuceneError::need_implemented(
            "move_to_sibling is not implemented",
        ))
    }

    /// Move to the parent node and return `true` upon success.
    /// Returns `false` for the root node and `true` otherwise.
    fn move_to_parent(&mut self) -> Result<bool> {
        Err(LuceneError::need_implemented(
            "move_to_parent is not implemented",
        ))
    }

    /// Return the minimum packed value of the current node.
    fn get_min_packed_value(&self) -> Result<&[u8]> {
        Err(LuceneError::need_implemented(
            "get_min_packed_value is not implemented",
        ))
    }

    /// Return the maximum packed value of the current node.
    fn get_max_packed_value(&self) -> Result<&[u8]> {
        Err(LuceneError::need_implemented(
            "get_max_packed_value is not implemented",
        ))
    }

    /// Return the number of points below the current node.
    fn size(&self) -> Result<usize> {
        Err(LuceneError::need_implemented("size is not implemented"))
    }

    /// Visit all the docs below the current node.
    fn visit_doc_ids<IV>(&mut self, _visitor: &mut IV) -> Result<()>
    where
        IV: IntersectVisitor,
    {
        Err(LuceneError::need_implemented(
            "visit_doc_ids is not implemented",
        ))
    }

    /// Visit all the docs and values below the current node.
    fn visit_doc_values<IV>(&mut self, _visitor: &mut IV) -> Result<()>
    where
        IV: IntersectVisitor,
    {
        Err(LuceneError::need_implemented(
            "visit_doc_values is not implemented",
        ))
    }
}
/// We recurse the [PointTree], using a provided instance of this to guide the
/// recursion.
pub trait IntersectVisitor {
    /// Called for all documents in a leaf cell that's fully contained by the
    /// query. The consumer should blindly accept the docID.
    fn visit(&mut self, doc_id: i32) -> Result<()>;

    /// Similar to `visit(doc_id)`, but a bulk visit and implementations may
    /// have their optimizations. Default implementation that iterates over
    /// the provided `DocIdSetIterator`.
    fn visit_with_iterator(&mut self, iterator: &mut impl DocIdSetIterator) -> Result<()> {
        loop {
            let doc_id = iterator.next_doc()?;
            if doc_id == NO_MORE_DOCS {
                break;
            }
            self.visit(doc_id)?;
        }
        Ok(())
    }

    /// Similar to `visit(doc_id)`, but a bulk visit and implementations may
    /// have their optimizations. Even if the implementation does the same
    /// thing as this method, this may be a speed improvement due to fewer
    /// virtual calls.
    fn visit_with_ints_ref(&mut self, ints_ref: &IntsRef<Vec<i32>>) -> Result<()> {
        self.default_visit_with_ints_ref(ints_ref)
    }
    fn default_visit_with_ints_ref(&mut self, ints_ref: &IntsRef<Vec<i32>>) -> Result<()> {
        for i in ints_ref.offset..(ints_ref.offset + ints_ref.length) {
            self.visit(ints_ref.ints[i])?;
        }
        Ok(())
    }

    /// Called for all documents in a leaf cell that crosses the query.
    /// The consumer should scrutinize the `packed_value` to decide whether to
    /// accept it. In the 1D case, values are visited in increasing order,
    /// and in the case of ties, in increasing docID order.
    fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()>;

    /// Similar to `visit_with_packed_value(doc_id, packed_value)` but in this
    /// case the `packed_value` can have more than one docID associated to
    /// it. The provided iterator should not escape the scope of this method
    /// so that implementations of PointValues are free to reuse it.
    fn visit_iterator_with_packed_value(
        &mut self,
        iterator: &mut impl DocIdSetIterator,
        packed_value: &[u8],
    ) -> Result<()> {
        self.default_visit_iterator_with_packed_value_(iterator, packed_value)
    }
    fn default_visit_iterator_with_packed_value_(
        &mut self,
        iterator: &mut impl DocIdSetIterator,
        packed_value: &[u8],
    ) -> Result<()> {
        loop {
            let doc_id = iterator.next_doc()?;
            if doc_id == NO_MORE_DOCS {
                break;
            }
            self.visit_with_packed_value(doc_id, packed_value)?;
        }
        Ok(())
    }

    /// Called for non-leaf cells to test how the cell relates to the query,
    /// to determine how to further recurse down the tree.
    fn compare(&self, min_packed_value: &[u8], max_packed_value: &[u8]) -> Result<Relation>;

    /// Notifies the caller that this many documents are about to be visited.
    fn grow(&mut self, _count: usize) -> Result<()> {
        Ok(())
    }
}

pub const MAX_NUM_BYTES: usize = 16;
pub const MAX_DIMENSIONS: usize = BKDConfig::MAX_DIMS;
pub const MAX_INDEX_DIMENSIONS: usize = BKDConfig::MAX_INDEX_DIMS;

pub enum PointTreeEnum<MPT, PT>
where
    MPT: MutablePointTree,
    PT: PointTree,
{
    Mutable(MPT),
    Other(PT),
}

impl<MPT, PT> TryClone for PointTreeEnum<MPT, PT>
where
    MPT: MutablePointTree,
    PT: PointTree,
{
    fn try_clone(&self) -> Result<Self>
    where
        Self: Sized,
    {
        match self {
            PointTreeEnum::Mutable(mpt) => Ok(PointTreeEnum::Mutable(mpt.try_clone()?)),
            PointTreeEnum::Other(pt) => Ok(PointTreeEnum::Other(pt.try_clone()?)),
        }
    }
}

impl<MPT, PT> PointTree for PointTreeEnum<MPT, PT>
where
    MPT: MutablePointTree,
    PT: PointTree,
{
    fn move_to_child(&mut self) -> Result<bool> {
        match self {
            PointTreeEnum::Mutable(mpt) => mpt.move_to_child(),
            PointTreeEnum::Other(pt) => pt.move_to_child(),
        }
    }

    fn move_to_sibling(&mut self) -> Result<bool> {
        match self {
            PointTreeEnum::Mutable(mpt) => mpt.move_to_sibling(),
            PointTreeEnum::Other(pt) => pt.move_to_sibling(),
        }
    }

    fn move_to_parent(&mut self) -> Result<bool> {
        match self {
            PointTreeEnum::Mutable(mpt) => mpt.move_to_parent(),
            PointTreeEnum::Other(pt) => pt.move_to_parent(),
        }
    }

    fn get_min_packed_value(&self) -> Result<&[u8]> {
        match self {
            PointTreeEnum::Mutable(mpt) => mpt.get_min_packed_value(),
            PointTreeEnum::Other(pt) => pt.get_min_packed_value(),
        }
    }

    fn get_max_packed_value(&self) -> Result<&[u8]> {
        match self {
            PointTreeEnum::Mutable(mpt) => mpt.get_max_packed_value(),
            PointTreeEnum::Other(pt) => pt.get_max_packed_value(),
        }
    }

    fn size(&self) -> Result<usize> {
        match self {
            PointTreeEnum::Mutable(mpt) => mpt.size(),
            PointTreeEnum::Other(pt) => pt.size(),
        }
    }

    fn visit_doc_ids<IV>(&mut self, _visitor: &mut IV) -> Result<()>
    where
        IV: IntersectVisitor,
    {
        match self {
            PointTreeEnum::Mutable(mpt) => mpt.visit_doc_ids(_visitor),
            PointTreeEnum::Other(pt) => pt.visit_doc_ids(_visitor),
        }
    }

    fn visit_doc_values<IV>(&mut self, _visitor: &mut IV) -> Result<()>
    where
        IV: IntersectVisitor,
    {
        match self {
            PointTreeEnum::Mutable(mpt) => mpt.visit_doc_values(_visitor),
            PointTreeEnum::Other(pt) => pt.visit_doc_values(_visitor),
        }
    }
}

pub enum PointTreeEnum2<A, B> {
    A(A),
    B(B),
}

impl<A, B> TryClone for PointTreeEnum2<A, B>
where
    A: PointTree,
    B: PointTree,
{
    fn try_clone(&self) -> Result<Self>
    where
        Self: Sized,
    {
        match self {
            PointTreeEnum2::A(tree) => Ok(PointTreeEnum2::A(tree.try_clone()?)),
            PointTreeEnum2::B(tree) => Ok(PointTreeEnum2::B(tree.try_clone()?)),
        }
    }
}

impl<A, B> PointTree for PointTreeEnum2<A, B>
where
    A: PointTree,
    B: PointTree,
{
    fn move_to_child(&mut self) -> Result<bool> {
        match self {
            PointTreeEnum2::A(tree) => tree.move_to_child(),
            PointTreeEnum2::B(tree) => tree.move_to_child(),
        }
    }

    fn move_to_sibling(&mut self) -> Result<bool> {
        match self {
            PointTreeEnum2::A(tree) => tree.move_to_sibling(),
            PointTreeEnum2::B(tree) => tree.move_to_sibling(),
        }
    }

    fn move_to_parent(&mut self) -> Result<bool> {
        match self {
            PointTreeEnum2::A(tree) => tree.move_to_parent(),
            PointTreeEnum2::B(tree) => tree.move_to_parent(),
        }
    }

    fn get_min_packed_value(&self) -> Result<&[u8]> {
        match self {
            PointTreeEnum2::A(tree) => tree.get_min_packed_value(),
            PointTreeEnum2::B(tree) => tree.get_min_packed_value(),
        }
    }

    fn get_max_packed_value(&self) -> Result<&[u8]> {
        match self {
            PointTreeEnum2::A(tree) => tree.get_max_packed_value(),
            PointTreeEnum2::B(tree) => tree.get_max_packed_value(),
        }
    }

    fn size(&self) -> Result<usize> {
        match self {
            PointTreeEnum2::A(tree) => tree.size(),
            PointTreeEnum2::B(tree) => tree.size(),
        }
    }

    fn visit_doc_ids<IV>(&mut self, visitor: &mut IV) -> Result<()>
    where
        IV: IntersectVisitor,
    {
        match self {
            PointTreeEnum2::A(tree) => tree.visit_doc_ids(visitor),
            PointTreeEnum2::B(tree) => tree.visit_doc_ids(visitor),
        }
    }

    fn visit_doc_values<IV>(&mut self, visitor: &mut IV) -> Result<()>
    where
        IV: IntersectVisitor,
    {
        match self {
            PointTreeEnum2::A(tree) => tree.visit_doc_values(visitor),
            PointTreeEnum2::B(tree) => tree.visit_doc_values(visitor),
        }
    }
}

#[derive(Clone)]
pub enum PointValuesEnum2<A, B> {
    A(A),
    B(B),
}

impl<A, B> PointValues for PointValuesEnum2<A, B>
where
    A: PointValues,
    B: PointValues,
{
    fn get_min_packed_value(&self) -> Result<Option<Cow<'_, Vec<u8>>>> {
        match self {
            PointValuesEnum2::A(values) => values.get_min_packed_value(),
            PointValuesEnum2::B(values) => values.get_min_packed_value(),
        }
    }

    fn get_max_packed_value(&self) -> Result<Option<Cow<'_, Vec<u8>>>> {
        match self {
            PointValuesEnum2::A(values) => values.get_max_packed_value(),
            PointValuesEnum2::B(values) => values.get_max_packed_value(),
        }
    }

    fn get_num_dimensions(&self) -> Result<usize> {
        match self {
            PointValuesEnum2::A(values) => values.get_num_dimensions(),
            PointValuesEnum2::B(values) => values.get_num_dimensions(),
        }
    }

    fn get_num_index_dimensions(&self) -> Result<usize> {
        match self {
            PointValuesEnum2::A(values) => values.get_num_index_dimensions(),
            PointValuesEnum2::B(values) => values.get_num_index_dimensions(),
        }
    }

    fn get_bytes_per_dimension(&self) -> Result<usize> {
        match self {
            PointValuesEnum2::A(values) => values.get_bytes_per_dimension(),
            PointValuesEnum2::B(values) => values.get_bytes_per_dimension(),
        }
    }

    fn size(&self) -> Result<usize> {
        match self {
            PointValuesEnum2::A(values) => values.size(),
            PointValuesEnum2::B(values) => values.size(),
        }
    }

    fn get_doc_count(&self) -> Result<i32> {
        match self {
            PointValuesEnum2::A(values) => values.get_doc_count(),
            PointValuesEnum2::B(values) => values.get_doc_count(),
        }
    }

    type PointTree = PointTreeEnum2<A::PointTree, B::PointTree>;
    type MutablePointTree = MutablePointTreeEnum2<A::MutablePointTree, B::MutablePointTree>;

    fn get_point_tree(&self) -> Result<PointTreeEnum<Self::MutablePointTree, Self::PointTree>> {
        match self {
            PointValuesEnum2::A(values) => match values.get_point_tree()? {
                PointTreeEnum::Mutable(tree) => {
                    Ok(PointTreeEnum::Mutable(MutablePointTreeEnum2::A(tree)))
                },
                PointTreeEnum::Other(tree) => Ok(PointTreeEnum::Other(PointTreeEnum2::A(tree))),
            },
            PointValuesEnum2::B(values) => match values.get_point_tree()? {
                PointTreeEnum::Mutable(tree) => {
                    Ok(PointTreeEnum::Mutable(MutablePointTreeEnum2::B(tree)))
                },
                PointTreeEnum::Other(tree) => Ok(PointTreeEnum::Other(PointTreeEnum2::B(tree))),
            },
        }
    }
}

impl<T> PointValues for Arc<T>
where
    T: PointValues,
{
    fn get_min_packed_value(&self) -> Result<Option<Cow<'_, Vec<u8>>>> {
        (**self).get_min_packed_value()
    }

    fn get_max_packed_value(&self) -> Result<Option<Cow<'_, Vec<u8>>>> {
        (**self).get_max_packed_value()
    }

    fn get_num_dimensions(&self) -> Result<usize> {
        (**self).get_num_dimensions()
    }

    fn get_num_index_dimensions(&self) -> Result<usize> {
        (**self).get_num_index_dimensions()
    }

    fn get_bytes_per_dimension(&self) -> Result<usize> {
        (**self).get_bytes_per_dimension()
    }

    fn size(&self) -> Result<usize> {
        (**self).size()
    }

    fn get_doc_count(&self) -> Result<i32> {
        (**self).get_doc_count()
    }

    type PointTree = T::PointTree;
    type MutablePointTree = T::MutablePointTree;

    fn get_point_tree(&self) -> Result<PointTreeEnum<Self::MutablePointTree, Self::PointTree>> {
        (**self).get_point_tree()
    }

    fn intersect(&self, visitor: &mut impl IntersectVisitor) -> Result<()> {
        (**self).intersect(visitor)
    }

    fn estimate_point_count(&self, visitor: &impl IntersectVisitor) -> Result<i64> {
        (**self).estimate_point_count(visitor)
    }

    fn estimate_doc_count(&self, visitor: &impl IntersectVisitor) -> Result<i64> {
        (**self).estimate_doc_count(visitor)
    }
}

#[cfg(test)]
mod tests {
    use crate::core::document::binary_point::BinaryPoint;
    use crate::core::document::document::Document;
    use crate::core::document::double_point::DoublePoint;
    use crate::core::document::field::{FieldBase, Store};
    use crate::core::document::float_point::FloatPoint;
    use crate::core::document::int_point::IntPoint;
    use crate::core::document::long_point::LongPoint;
    use crate::core::index::composite_reader::get_context;
    use crate::core::index::directory_reader::directory_reader_util;

    use crate::core::index::index_reader_context::IndexReaderContext;
    use crate::core::index::index_writer::IndexWriter;
    use crate::core::index::index_writer_config::IndexWriterConfig;
    use crate::core::index::indexable_field::IndexableField;
    use crate::core::index::leaf_reader::LeafReader;
    use crate::core::index::multi_reader::MultiReader;
    use crate::core::index::point_values::{
        IntersectVisitor, MAX_INDEX_DIMENSIONS, MAX_NUM_BYTES, PointValues, Relation,
        get_doc_count, get_max_packed_value, get_min_packed_value, size,
    };
    use crate::core::index::term::Term;
    use crate::core::util::error::lucene_error::{LuceneError, Result};
    use crate::test::index::random_index_writer::RandomIndexWriter;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{
        at_least, create_temp_dir, get_only_leaf_reader, is_night_mode, new_directory_shared,
        new_fs_directory, new_index_writer_config, new_string_field, random,
    };
    use crate::test::util::test_util::TestUtil;
    use rand::Rng;
    use std::collections::HashMap;

    use crate::core::util::TryIntoInt;
    use std::vec;

    #[allow(dead_code)] // for quick search
    struct TestPointValues;
    // Suddenly add points to an existing field:
    #[test]
    fn test_upgrade_field_to_points() -> Result<()> {
        let mut random = random();

        let dir = new_directory_shared(&mut random)?;

        // TODO: 这里应该使用了带有分词器的构造方法
        let iwc = new_index_writer_config(&mut random);
        {
            let w = IndexWriter::new(dir.clone(), iwc)?;

            let mut field_types = HashMap::new();
            let mut doc = Document::new();
            doc.add(new_string_field("dim", "foo", Store::No, &mut field_types)?);
            w.add_document(doc)?;
            w.close()?;
        }

        // TODO: 这里应该使用了带有分词器的构造方法
        let iwc = new_index_writer_config(&mut random);
        let w = IndexWriter::new(dir.clone(), iwc)?;

        let mut doc = Document::new();
        let v = BinaryPoint::new("dim", vec![vec![0u8; 4]])?;
        doc.add(v);
        w.close()?;

        Ok(())
    }
    #[test]
    fn test_illegal_dim_change_one_doc() -> Result<()> {
        let mut random = random();

        let dir = new_directory_shared(&mut random)?;
        // TODO: 这里应该使用了带有分词器的构造方法
        let iwc = new_index_writer_config(&mut random);
        let w = IndexWriter::new(dir.clone(), iwc)?;

        let mut doc = Document::new();
        doc.add(BinaryPoint::new("dim", vec![vec![0u8; 4]])?);
        doc.add(BinaryPoint::new("dim", vec![vec![0u8; 4], vec![0u8; 4]])?);

        let err = w.add_document(doc).unwrap_err();
        match err {
            LuceneError::IllegalArgument(msg) => {
                assert_eq!(
                    msg.to_string(),
                    "Inconsistency of field data structures across documents for field [dim] of doc [0].\
 point dimension: expected '1', but it has '2'."
                );
            },
            _ => unreachable!("{:?}", err),
        }
        w.close()?;
        Ok(())
    }
    #[test]
    fn test_illegal_dim_change_two_docs() -> Result<()> {
        let mut random = random();

        let dir = new_directory_shared(&mut random)?;
        // TODO: 这里应该使用了带有分词器的构造方法
        let iwc = new_index_writer_config(&mut random);
        let w = IndexWriter::new(dir.clone(), iwc)?;

        let mut doc = Document::new();
        doc.add(BinaryPoint::new("dim", vec![vec![0u8; 4]])?);
        w.add_document(doc)?;

        let mut doc2 = Document::new();
        doc2.add(BinaryPoint::new("dim", vec![vec![0u8; 4], vec![0u8; 4]])?);

        let err = w.add_document(doc2).unwrap_err();
        match err {
            LuceneError::IllegalArgument(msg) => {
                assert_eq!(
                    msg.to_string(),
                    "Inconsistency of field data structures across documents for field [dim] of doc [1].\
 point dimension: expected '1', but it has '2'."
                );
            },
            _ => unreachable!("{:?}", err),
        }

        w.close()?;
        Ok(())
    }
    #[test]
    fn test_illegal_dim_change_two_segments() -> Result<()> {
        let mut random = random();

        let dir = new_directory_shared(&mut random)?;
        // TODO: 这里应该使用了带有分词器的构造方法

        let iwc = new_index_writer_config(&mut random);
        let w = IndexWriter::new(dir.clone(), iwc)?;

        {
            let mut doc = Document::new();
            doc.add(BinaryPoint::new("dim", vec![vec![0u8; 4]])?);
            w.add_document(doc)?;
            w.commit()?;
        }

        let mut doc2 = Document::new();
        doc2.add(BinaryPoint::new("dim", vec![vec![0u8; 4], vec![0u8; 4]])?);

        let err = w.add_document(doc2).unwrap_err();
        match err {
            LuceneError::IllegalArgument(msg) => {
                assert_eq!(
                    msg.to_string(),
                    "cannot change field \"dim\" from points dimensionCount=1, indexDimensionCount=1, numBytes=4 \
to inconsistent dimensionCount=2, indexDimensionCount=2, numBytes=4"
                );
            },
            _ => unreachable!("{:?}", err),
        }

        w.close()?;
        Ok(())
    }
    #[test]
    fn test_illegal_dim_change_two_writers() -> Result<()> {
        let mut random = random();

        let dir = new_directory_shared(&mut random)?;

        {
            // TODO: 这里应该使用了带有分词器的构造方法
            let iwc = new_index_writer_config(&mut random);
            let w = IndexWriter::new(dir.clone(), iwc)?;

            let mut doc = Document::new();
            doc.add(BinaryPoint::new("dim", vec![vec![0u8; 4]])?);
            w.add_document(doc)?;
            w.close()?;
        }

        {
            // TODO: 这里应该使用了带有分词器的构造方法

            let iwc = new_index_writer_config(&mut random);
            let w2 = IndexWriter::new(dir.clone(), iwc)?;

            let mut doc2 = Document::new();
            doc2.add(BinaryPoint::new("dim", vec![vec![0u8; 4], vec![0u8; 4]])?);

            let err = w2.add_document(doc2).unwrap_err();
            match err {
                LuceneError::IllegalArgument(msg) => {
                    assert_eq!(
                        msg.to_string(),
                        "cannot change field \"dim\" from points dimensionCount=1, indexDimensionCount=1, numBytes=4 \
to inconsistent dimensionCount=2, indexDimensionCount=2, numBytes=4"
                    );
                },
                _ => unreachable!("{:?}", err),
            }

            w2.close()?;
        }
        Ok(())
    }
    #[test]
    fn test_illegal_dim_change_via_add_indexes_directory() -> Result<()> {
        // TODO add_indexes未实现
        Ok(())
    }
    #[test]
    fn test_illegal_dim_change_via_add_indexes_codec_reader() -> Result<()> {
        // TODO add_indexes未实现
        Ok(())
    }
    #[test]
    fn test_illegal_dim_change_via_add_indexes_slow_codec_reader() -> Result<()> {
        // TODO add_indexes_slowly未实现
        Ok(())
    }
    #[test]
    fn test_illegal_num_bytes_change_one_doc() -> Result<()> {
        let mut random = random();

        let dir = new_directory_shared(&mut random)?;
        // TODO: 这里应该使用了带有分词器的构造方法
        let iwc = new_index_writer_config(&mut random);
        let w = IndexWriter::new(dir.clone(), iwc)?;

        let mut doc = Document::new();
        doc.add(BinaryPoint::new("dim", vec![vec![0u8; 4]])?);
        doc.add(BinaryPoint::new("dim", vec![vec![0u8; 6]])?);

        let err = w.add_document(doc).unwrap_err();
        match err {
            LuceneError::IllegalArgument(msg) => {
                assert_eq!(
                    msg.to_string(),
                    "Inconsistency of field data structures across documents for field [dim] of doc [0].\
 point num bytes: expected '4', but it has '6'."
                );
            },
            _ => unreachable!("{:?}", err),
        }

        w.close()?;
        Ok(())
    }

    #[test]
    fn test_illegal_num_bytes_change_two_docs() -> Result<()> {
        let mut random = random();

        let dir = new_directory_shared(&mut random)?;
        // TODO: 这里应该使用了带有分词器的构造方法
        let iwc = new_index_writer_config(&mut random);
        let w = IndexWriter::new(dir.clone(), iwc)?;

        let mut doc = Document::new();
        doc.add(BinaryPoint::new("dim", vec![vec![0u8; 4]])?);
        w.add_document(doc)?;

        let mut doc2 = Document::new();
        doc2.add(BinaryPoint::new("dim", vec![vec![0u8; 6]])?);

        let err = w.add_document(doc2).unwrap_err();
        match err {
            LuceneError::IllegalArgument(msg) => {
                assert_eq!(
                    msg.to_string(),
                    "Inconsistency of field data structures across documents for field [dim] of doc [1].\
 point num bytes: expected '4', but it has '6'."
                );
            },
            _ => unreachable!("{:?}", err),
        }

        w.close()?;
        Ok(())
    }
    #[test]
    fn test_illegal_num_bytes_change_two_segments() -> Result<()> {
        let mut random = random();

        let dir = new_directory_shared(&mut random)?;
        // TODO: 这里应该使用了带有分词器的构造方法

        let iwc = new_index_writer_config(&mut random);
        let w = IndexWriter::new(dir.clone(), iwc)?;

        {
            let mut doc = Document::new();
            doc.add(BinaryPoint::new("dim", vec![vec![0u8; 4]])?);
            w.add_document(doc)?;
            w.commit()?;
        }

        let mut doc2 = Document::new();
        doc2.add(BinaryPoint::new("dim", vec![vec![0u8; 6]])?);

        let err = w.add_document(doc2).unwrap_err();
        match err {
            LuceneError::IllegalArgument(msg) => {
                assert_eq!(
                    msg.to_string(),
                    "cannot change field \"dim\" from points dimensionCount=1, indexDimensionCount=1, numBytes=4 \
to inconsistent dimensionCount=1, indexDimensionCount=1, numBytes=6"
                );
            },
            _ => unreachable!("{:?}", err),
        }

        w.close()?;
        Ok(())
    }

    #[test]
    fn test_illegal_num_bytes_change_two_writers() -> Result<()> {
        let mut random = random();

        let dir = new_directory_shared(&mut random)?;

        {
            let iwc = new_index_writer_config(&mut random);
            // TODO: 这里应该使用了带有分词器的构造方法
            let w = IndexWriter::new(dir.clone(), iwc)?;

            let mut doc = Document::new();
            doc.add(BinaryPoint::new("dim", vec![vec![0u8; 4]])?);
            w.add_document(doc)?;
            w.close()?;
        }

        {
            let iwc = new_index_writer_config(&mut random);
            let w2 = IndexWriter::new(dir.clone(), iwc)?;

            let mut doc2 = Document::new();
            doc2.add(BinaryPoint::new("dim", vec![vec![0u8; 6]])?);

            let err = w2.add_document(doc2).unwrap_err();
            match err {
                LuceneError::IllegalArgument(msg) => {
                    assert_eq!(
                        msg.to_string(),
                        "cannot change field \"dim\" from points dimensionCount=1, indexDimensionCount=1, numBytes=4 \
to inconsistent dimensionCount=1, indexDimensionCount=1, numBytes=6"
                    );
                },
                _ => unreachable!("{:?}", err),
            }

            w2.close()?;
        }

        Ok(())
    }
    #[test]
    fn test_illegal_num_bytes_change_via_add_indexes_directory() -> Result<()> {
        // TODO add_indexes未实现
        Ok(())
    }
    #[test]
    fn test_illegal_num_bytes_change_via_add_indexes_codec_reader() -> Result<()> {
        // TODO add_indexes未实现
        Ok(())
    }
    #[test]
    fn test_illegal_num_bytes_change_via_add_indexes_slow_codec_reader() -> Result<()> {
        // TODO add_indexes_slowly未实现
        Ok(())
    }
    #[test]
    fn test_illegal_too_many_bytes() -> Result<()> {
        let mut random = random();

        let dir = new_directory_shared(&mut random)?;
        // TODO: 这里应该使用了带有分词器的构造方法

        let iwc = new_index_writer_config(&mut random);
        let w = IndexWriter::new(dir.clone(), iwc)?;

        let err = BinaryPoint::new("dim", vec![vec![0u8; MAX_NUM_BYTES + 1]]);
        match err {
            Err(LuceneError::IllegalArgument(_)) => {},
            _ => unreachable!(""),
        }

        let mut doc2 = Document::new();
        doc2.add(IntPoint::new("dim", vec![17])?);
        w.add_document(doc2)?;

        w.close()?;
        Ok(())
    }

    #[test]
    fn test_illegal_too_many_dimensions() -> Result<()> {
        let mut random = random();

        let dir = new_directory_shared(&mut random)?;
        // TODO: 这里应该使用了带有分词器的构造方法

        let iwc = new_index_writer_config(&mut random);
        let w = IndexWriter::new(dir.clone(), iwc)?;

        let mut values: Vec<Vec<u8>> = Vec::with_capacity(MAX_INDEX_DIMENSIONS + 1);
        for _ in 0..(MAX_INDEX_DIMENSIONS + 1) {
            values.push(vec![0u8; 4]);
        }

        let bp = BinaryPoint::new("dim", values);
        match bp {
            Err(LuceneError::IllegalArgument(_)) => {},
            _ => unreachable!(""),
        }

        let mut doc2 = Document::new();
        doc2.add(IntPoint::new("dim", vec![17])?);
        w.add_document(doc2)?;

        w.close()?;
        Ok(())
    }
    #[test]
    fn test_different_codecs_1() -> Result<()> {
        // this test is not required in Rust Lucene
        Ok(())
    }

    #[test]
    fn test_different_codecs_2() -> Result<()> {
        // this test is not required in Rust Lucene
        Ok(())
    }
    #[test]
    fn test_invalid_int_point_usage() -> Result<()> {
        let mut field = IntPoint::new("field", vec![17, 42])?;

        let err = field.set_int_value(14).unwrap_err();
        match err {
            LuceneError::IllegalArgument(_) => {},
            _ => unreachable!("{:?}", err),
        }

        let err = field.numeric_value().unwrap_err();
        match err {
            LuceneError::IllegalState(_) => {},
            _ => unreachable!("{:?}", err),
        }

        Ok(())
    }
    #[test]
    fn test_invalid_long_point_usage() -> Result<()> {
        let mut field = LongPoint::new("field", vec![17, 42])?;

        let err = field.set_long_value(14).unwrap_err();
        match err {
            LuceneError::IllegalArgument(_) => {},
            _ => unreachable!("{:?}", err),
        }

        let err = field.numeric_value().unwrap_err();
        match err {
            LuceneError::IllegalState(_) => {},
            _ => unreachable!("{:?}", err),
        }

        Ok(())
    }

    #[test]
    fn test_invalid_float_point_usage() -> Result<()> {
        let mut field = FloatPoint::new("field", vec![17.0_f32, 42.0_f32])?;

        let err = field.set_float_value(14.0).unwrap_err();
        match err {
            LuceneError::IllegalArgument(_) => {},
            _ => unreachable!("{:?}", err),
        }

        let err = field.numeric_value().unwrap_err();
        match err {
            LuceneError::IllegalState(_) => {},
            _ => unreachable!("{:?}", err),
        }

        Ok(())
    }

    #[test]
    fn test_invalid_double_point_usage() -> Result<()> {
        let mut field = DoublePoint::new("field", vec![17.0_f64, 42.0_f64])?;

        let err = field.set_double_value(14.0).unwrap_err();
        match err {
            LuceneError::IllegalArgument(_) => {},
            _ => unreachable!("{:?}", err),
        }

        let err = field.numeric_value().unwrap_err();
        match err {
            LuceneError::IllegalState(_) => {},
            _ => unreachable!("{:?}", err),
        }

        Ok(())
    }
    struct IntersectVisitorImpl {
        last_doc_id: i32,
    }
    impl IntersectVisitorImpl {
        fn new() -> Self {
            Self { last_doc_id: -1 }
        }
    }
    impl IntersectVisitor for IntersectVisitorImpl {
        fn visit(&mut self, doc_id: i32) -> Result<()> {
            if doc_id < self.last_doc_id {
                return Err(LuceneError::illegal_state(format!(
                    "docs out of order: docID={} but lastDocID={}",
                    doc_id, self.last_doc_id
                )));
            }
            self.last_doc_id = doc_id;
            Ok(())
        }

        fn visit_with_packed_value(&mut self, doc_id: i32, _packed_value: &[u8]) -> Result<()> {
            self.visit(doc_id)
        }

        fn compare(&self, _min_packed_value: &[u8], _max_packed_value: &[u8]) -> Result<Relation> {
            if random().random_bool(0.5) {
                Ok(Relation::CellCrossesQuery)
            } else {
                Ok(Relation::CellInsideQuery)
            }
        }
    }
    #[test]
    fn test_tie_break_by_doc_id() -> Result<()> {
        let mut random = random();

        let dir = new_fs_directory(&mut random, create_temp_dir()?)?;

        let iwc = new_index_writer_config(&mut random);
        let w = IndexWriter::new(dir.clone(), iwc)?;

        let mut doc = Document::new();
        doc.add(IntPoint::new("int", vec![17])?);

        let num_docs = if is_night_mode() {
            at_least(&mut random, 300_000)
        } else {
            at_least(&mut random, 3_000)
        };

        for _ in 0..num_docs {
            w.add_document(doc.clone())?;
            if random.random_range(0..1000) == 17 {
                w.commit()?;
            }
        }

        let reader = directory_reader_util::open_with_writer(&w)?;
        let reader = get_context(reader)?;

        for leaf in reader.leaves()? {
            let points = leaf.reader().get_point_values("int")?;
            if let Some(points) = points {
                let mut visitor = IntersectVisitorImpl { last_doc_id: -1 };
                points.intersect(&mut visitor)?;
            }
        }

        w.close()?;
        Ok(())
    }
    // TODO force_merge未实现 测试未通过
    fn test_delete_all_point_docs() -> Result<()> {
        let mut random = random();

        let dir = new_directory_shared(&mut random)?;
        let iwc = new_index_writer_config(&mut random);
        let w = IndexWriter::new(dir.clone(), iwc)?;

        let mut field_types = HashMap::new();
        let mut doc = Document::new();
        doc.add(new_string_field("id", "0", Store::No, &mut field_types)?);
        doc.add(IntPoint::new("int", vec![17])?);
        w.add_document(doc)?;

        w.add_document(Document::new())?;
        w.commit()?;

        w.delete_documents_with_terms(vec![Term::from_text("id", "0")])?;

        // TODO force_merge未实现
        // w.force_merge(1)?;
        let reader = directory_reader_util::open_with_writer(&w)?;

        let ctx = get_context(reader)?;
        let leaves = ctx.leaves()?;
        let leaf = &leaves[0];

        assert!(leaf.reader().get_point_values("int")?.is_none());

        w.close()?;
        Ok(())
    }

    #[test]
    fn test_points_field_missing_from_one_segment() -> Result<()> {
        let mut random = random();

        let dir = new_fs_directory(&mut random, create_temp_dir()?)?;

        let iwc = new_index_writer_config(&mut random);
        let w = IndexWriter::new(dir.clone(), iwc)?;

        let mut field_types = HashMap::new();
        let mut doc = Document::new();
        doc.add(new_string_field("id", "0", Store::No, &mut field_types)?);
        doc.add(IntPoint::new("int0", vec![0])?);
        w.add_document(doc)?;
        w.commit()?;

        let mut doc2 = Document::new();
        doc2.add(IntPoint::new("int1", vec![17])?);
        w.add_document(doc2)?;

        // TODO force_merge未实现
        // w.force_merge(1)?;

        w.close()?;
        Ok(())
    }
    #[test]
    fn test_sparse_points() -> Result<()> {
        let mut random = random();

        let dir = new_directory_shared(&mut random)?;
        let num_docs = at_least(&mut random, 1000);
        let num_fields = TestUtil::next_int(&mut random, 1, 10);

        let w = RandomIndexWriter::new(&mut random, dir.clone());

        let mut field_doc_counts = vec![0i32; num_fields as usize];
        let mut field_sizes = vec![0i32; num_fields as usize];

        for _ in 0..num_docs {
            let mut doc = Document::new();

            for field in 0..num_fields {
                let field_name = format!("int{}", field);

                if random.random_range(0..100) == 17 {
                    let v = random.random();
                    doc.add(IntPoint::new(&field_name, vec![v])?);
                    field_doc_counts[field as usize] += 1;
                    field_sizes[field as usize] += 1;

                    if random.random_range(0..10) == 5 {
                        let v2 = random.random();
                        doc.add(IntPoint::new(&field_name, vec![v2])?);
                        field_sizes[field as usize] += 1;
                    }
                }
            }

            w.add_document(doc)?;
        }

        let reader = w.get_reader()?;
        let ctx = get_context(reader)?;
        let leaves = ctx.leaves()?;

        for field in 0..num_fields {
            let mut doc_count = 0i32;
            let mut size = 0i32;
            let field_name = format!("int{}", field);

            for leaf in leaves.iter() {
                if let Some(points) = leaf.reader().get_point_values(&field_name)? {
                    doc_count += points.get_doc_count()?;
                    size += points.size()? as i32;
                }
            }

            assert_eq!(field_doc_counts[field as usize], doc_count);
            assert_eq!(field_sizes[field as usize], size);
        }

        w.close()?;

        Ok(())
    }
    #[test]
    fn test_check_index_includes_points() -> Result<()> {
        // TODO check_index未实现
        Ok(())
    }
    #[test]
    fn test_merged_stats_empty_reader() -> Result<()> {
        let reader = MultiReader::empty()?;

        assert!(get_min_packed_value(&reader, "field")?.is_none());
        assert!(get_max_packed_value(&reader, "field")?.is_none());
        assert_eq!(0, get_doc_count(&reader, "field")?);
        assert_eq!(0, size(&reader, "field")?);

        Ok(())
    }

    #[test]
    fn test_merged_stats_one_segment_without_points() -> Result<()> {
        let mut random = random();
        // TODO ByteBuffersDirectory未实现
        let dir = new_directory_shared(&mut random)?;
        // TODO NoMergePolicy未实现
        let iwc = IndexWriterConfig::new();

        let w = IndexWriter::new(dir.clone(), iwc)?;

        w.add_document(Document::new())?;

        {
            directory_reader_util::open_with_writer(&w)?;
        }

        let mut doc = Document::new();
        doc.add(IntPoint::new("field", vec![i32::MIN])?);
        w.add_document(doc)?;

        let reader = directory_reader_util::open_with_writer(&w)?;

        assert_eq!(get_min_packed_value(&reader, "field")?, Some(vec![0u8; 4]));

        assert_eq!(get_max_packed_value(&reader, "field")?, Some(vec![0u8; 4]));

        assert_eq!(get_doc_count(&reader, "field")?, 1);

        assert_eq!(size(&reader, "field")?, 1);

        assert_eq!(get_min_packed_value(&reader, "field2")?, None);
        assert_eq!(get_max_packed_value(&reader, "field2")?, None);
        assert_eq!(get_doc_count(&reader, "field2")?, 0);
        assert_eq!(size(&reader, "field2")?, 0);

        Ok(())
    }
    // TODO force_merge未实现 测试未通过
    fn test_merged_stats_all_points_deleted() -> Result<()> {
        let mut random = random();

        // TODO ByteBuffersDirectory 未实现
        let dir = new_directory_shared(&mut random)?;

        let iwc = IndexWriterConfig::new();
        let w = IndexWriter::new(dir.clone(), iwc)?;

        w.add_document(Document::new())?;

        {
            let mut field_types = HashMap::new();
            let mut doc = Document::new();
            doc.add(IntPoint::new("field", vec![i32::MIN])?);
            doc.add(new_string_field(
                "delete",
                "yes",
                Store::No,
                &mut field_types,
            )?);
            w.add_document(doc)?;
        }

        // TODO force_merge未实现
        // w.force_merge(1)?;

        w.delete_documents_with_terms(vec![Term::from_text("delete", "yes")])?;

        w.add_document(Document::new())?;

        // TODO force_merge未实现
        // w.force_merge(1)?;

        let reader = directory_reader_util::open_with_writer(&w)?;

        assert_eq!(get_min_packed_value(&reader, "field")?, None);
        assert_eq!(get_max_packed_value(&reader, "field")?, None);
        assert_eq!(get_doc_count(&reader, "field")?, 0);
        assert_eq!(size(&reader, "field")?, 0);

        Ok(())
    }
    // TODO force_merge未实现 测试未通过
    fn test_merged_stats() -> Result<()> {
        let mut random = random();

        let iters = at_least(&mut random, 3);

        for _ in 0..iters {
            do_test_merged_stats(&mut random)?;
        }

        Ok(())
    }

    fn random_binary_value<R: Rng + ?Sized>(
        random: &mut R,
        num_dims: usize,
        num_bytes_per_dim: usize,
    ) -> Vec<Vec<u8>> {
        let mut values = Vec::with_capacity(num_dims);
        for _ in 0..num_dims {
            let mut bytes = vec![0u8; num_bytes_per_dim];
            random.fill(bytes.as_mut_slice());
            values.push(bytes);
        }
        values
    }
    fn do_test_merged_stats<R: Rng + ?Sized>(random: &mut R) -> Result<()> {
        let num_dims = TestUtil::next_int(random, 1, 8);
        let num_bytes_per_dim = TestUtil::next_int(random, 1, 16);

        // TODO ByteBuffersDirectory 未实现
        let dir = new_directory_shared(random)?;

        let iwc = IndexWriterConfig::new();
        let w = IndexWriter::new(dir.clone(), iwc)?;

        let num_docs = TestUtil::next_int(random, 10, 20);

        for _ in 0..num_docs {
            let mut doc = Document::new();
            let num_points = random.random_range(0..3);

            for _ in 0..num_points {
                let v = random_binary_value(random, num_dims as usize, num_bytes_per_dim as usize);
                doc.add(BinaryPoint::new("field", v)?);
            }

            w.add_document(doc)?;

            if random.random_bool(0.5) {
                directory_reader_util::open_with_writer(&w)?;
            }
        }

        let reader1 = directory_reader_util::open_with_writer(&w)?;
        // TODO force_merge未实现
        // w.force_merge(1)?;

        let reader2 = directory_reader_util::open_with_writer(&w)?;
        let leaf = get_only_leaf_reader(&reader2)?;
        let expected_opt = leaf.get_point_values("field")?;
        match expected_opt {
            Some(expected) => {
                assert_eq!(
                    Some(expected.get_min_packed_value()?.unwrap().into_owned()),
                    get_min_packed_value(&reader1, "field")?
                );

                assert_eq!(
                    Some(expected.get_max_packed_value()?.unwrap().into_owned()),
                    get_max_packed_value(&reader1, "field")?
                );

                assert_eq!(expected.get_doc_count()?, get_doc_count(&reader1, "field")?);

                assert_eq!(expected.size()?, size(&reader1, "field")?.try_convert()?);
            },
            None => {
                assert_eq!(get_min_packed_value(&reader1, "field")?, None);
                assert_eq!(get_max_packed_value(&reader1, "field")?, None);
                assert_eq!(get_doc_count(&reader1, "field")?, 0);
                assert_eq!(size(&reader1, "field")?, 0);
            },
        }

        Ok(())
    }
}
