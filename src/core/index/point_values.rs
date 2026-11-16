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
use crate::core::codecs::mutable_point_tree::MutablePointTree;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::util::TryIntoInt;
use crate::core::util::array_util::{ArrayUtil, ByteArrayComparator};
use crate::core::util::bkd::bkd_config::BKDConfig;
use crate::core::util::clone::TryClone;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::ints_ref::IntsRef;
use std::borrow::Cow;
use std::sync::Arc;

pub trait PointValues {
  /// Returns minimum value for each dimension, packed, or None if `size()` is
  /// `0`
  fn get_min_packed_value(&self) -> Result<Option<Cow<'_, [u8]>>>;

  /// Returns maximum value for each dimension, packed, or None if `size()` is
  /// `0`
  fn get_max_packed_value(&self) -> Result<Option<Cow<'_, [u8]>>>;

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
  fn get_point_tree(&self) -> Result<Self::PointTree>;

  /// Finds all documents and points matching the provided visitor.
  /// This method does not enforce live documents, so it's up to the caller
  /// to test whether each document is deleted, if necessary.
  fn intersect<V>(&self, visitor: &mut V) -> Result<()>
  where
    V: IntersectVisitor,
  {
    let mut point_tree = self.get_point_tree()?;
    intersect_with_point_tree(visitor, &mut point_tree)?;
    debug_assert!(!point_tree.move_to_parent()?);
    Ok(())
  }

  /// Estimate the number of points that would be visited by `intersect`
  /// with the given [`IntersectVisitor`]. This should run many times faster
  /// than `intersect(IntersectVisitor)`.
  fn estimate_point_count<V>(&self, visitor: &V) -> Result<i64>
  where
    V: IntersectVisitor,
  {
    let mut point_tree = self.get_point_tree()?;
    let count = estimate_point_count_with_point_tree(visitor, &mut point_tree, i64::MAX)?;
    debug_assert!(!point_tree.move_to_parent()?);
    Ok(count)
  }

  /// Estimate the number of documents that would be matched by `intersect`
  /// with the given [`IntersectVisitor`]. This should run many times faster
  /// than `intersect(IntersectVisitor)`.
  ///
  /// See also: [`DocIdSetIterator::cost`]
  fn estimate_doc_count<V>(&self, visitor: &V) -> Result<i64>
  where
    V: IntersectVisitor,
  {
    let estimated_point_count = self.estimate_point_count(visitor)?;
    let doc_count = self.get_doc_count()? as i64;
    let size: i64 = self.size()?.try_convert()?;

    if estimated_point_count >= size {
      // math all docs
      Ok(doc_count)
    } else if size == doc_count || estimated_point_count == 0 {
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
            .powf(size as f64 / doc_count as f64))) as i64;
      Ok(if doc_estimate == 0 { 1 } else { doc_estimate })
    }
  }
}
/// Return the cumulated number of points across all leaves of the given [`IndexReader`].
/// Leaves that do not have points for the given field are ignored.
///
/// See [`PointValues::size`].
pub fn size<IR>(reader: IR, field: &str) -> Result<i64>
where
  IR: IndexReader,
{
  let leaves = reader.get_context()?;
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
/// [`IndexReader`].
/// Leaves that do not have points for the given field are ignored.
///
/// See [`PointValues::get_doc_count`].
pub fn get_doc_count<IR>(reader: IR, field: &str) -> Result<i32>
where
  IR: IndexReader,
{
  let leaves = reader.get_context()?;
  let leaves = leaves.leaves()?;
  let mut count = 0;

  for leaf in leaves.iter() {
    if let Some(values) = leaf.reader().get_point_values(field)? {
      count += values.get_doc_count()?;
    }
  }

  Ok(count)
}

/// Return the minimum packed values across all leaves of the given [`IndexReader`].
/// Leaves that do not have points for the given field are ignored.
///
/// See [`PointValues::get_min_packed_value`].
pub fn get_min_packed_value<IR>(reader: IR, field: &str) -> Result<Option<Vec<u8>>>
where
  IR: IndexReader,
{
  let leaves = reader.get_context()?;
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

    let min_value_ref = match min_value.as_mut() {
      Some(min_value) => min_value,
      None => {
        min_value = Some(leaf_min_value.into_owned());
        continue;
      },
    };

    let num_dimensions = values.get_num_index_dimensions()?;
    let num_bytes_per_dimension = values.get_bytes_per_dimension()?;
    let comparator = ArrayUtil::get_unsigned_comparator(num_bytes_per_dimension);
    for i in 0..num_dimensions {
      let offset = i * num_bytes_per_dimension;
      if comparator.compare(&leaf_min_value, offset, min_value_ref, offset) < 0 {
        let copy_source = &leaf_min_value[offset..offset + num_bytes_per_dimension];
        min_value_ref[offset..][..copy_source.len()].copy_from_slice(copy_source);
      }
    }
  }

  Ok(min_value)
}
/// Return the maximum packed values across all leaves of the given [`IndexReader`].
/// Leaves that do not have points for the given field are ignored.
///
/// See [`PointValues::get_max_packed_value`].
pub fn get_max_packed_value<IR>(reader: IR, field: &str) -> Result<Option<Vec<u8>>>
where
  IR: IndexReader,
{
  let ctx = reader.get_context()?;
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

    let max_value_ref = match max_value.as_mut() {
      Some(max_value) => max_value,
      None => {
        max_value = Some(leaf_max_value.into_owned());
        continue;
      },
    };

    let num_dimensions = values.get_num_index_dimensions()?;
    let num_bytes_per_dimension = values.get_bytes_per_dimension()?;
    let comparator = ArrayUtil::get_unsigned_comparator(num_bytes_per_dimension);

    for dim in 0..num_dimensions {
      let offset = dim * num_bytes_per_dimension;
      if comparator.compare(&leaf_max_value, offset, max_value_ref, offset) > 0 {
        let copy_source = &leaf_max_value[offset..offset + num_bytes_per_dimension];
        max_value_ref[offset..][..copy_source.len()].copy_from_slice(copy_source);
      }
    }
  }

  Ok(max_value)
}

/// Estimate if the point count that would be matched by `intersect`
/// with the given [`IntersectVisitor`](crate::core::index::point_values::IntersectVisitor) is greater than or equal to the
/// `upper_bound`.
pub(crate) fn is_estimated_point_count_greater_than_or_equal_to<V, T>(
  visitor: &V,
  point_tree: &mut T,
  upper_bound: i64,
) -> Result<bool>
where
  V: IntersectVisitor,
  T: PointTree,
{
  Ok(estimate_point_count_with_point_tree(visitor, point_tree, upper_bound)? >= upper_bound)
}
fn intersect_with_point_tree<V, T>(visitor: &mut V, point_tree: &mut T) -> Result<()>
where
  V: IntersectVisitor,
  T: PointTree,
{
  let relation = visitor.compare(
    point_tree.get_min_packed_value()?.as_ref(),
    point_tree.get_max_packed_value()?.as_ref(),
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

fn estimate_point_count_with_point_tree<V, T>(
  visitor: &V,
  point_tree: &mut T,
  upper_bound: i64,
) -> Result<i64>
where
  V: IntersectVisitor,
  T: PointTree,
{
  let relation = visitor.compare(
    point_tree.get_min_packed_value()?.as_ref(),
    point_tree.get_max_packed_value()?.as_ref(),
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
        loop {
          cost += estimate_point_count_with_point_tree(visitor, point_tree, upper_bound - cost)?;
          if cost >= upper_bound || !point_tree.move_to_sibling()? {
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
impl Relation {
  pub fn ordinal(&self) -> i32 {
    match self {
      Relation::CellInsideQuery => 0,
      Relation::CellOutsideQuery => 1,
      Relation::CellCrossesQuery => 2,
    }
  }
}
/// Basic operations to read the KD-tree.
pub trait PointTree: TryClone {
  /// Concrete mutable capability exposed by this tree.
  type MutablePointTree: MutablePointTree;

  /// Move to the first child node and return `true` upon success.
  /// Returns `false` for leaf nodes and `true` otherwise.
  fn move_to_child(&mut self) -> Result<bool> {
    Err(LuceneError::need_implemented("move_to_child not implement"))
  }

  /// Move to the next sibling node and return `true` upon success.
  /// Returns `false` if the current node has no more siblings.
  fn move_to_sibling(&mut self) -> Result<bool> {
    Err(LuceneError::need_implemented(
      "move_to_sibling not implement",
    ))
  }

  /// Move to the parent node and return `true` upon success.
  /// Returns `false` for the root node and `true` otherwise.
  fn move_to_parent(&mut self) -> Result<bool> {
    Err(LuceneError::need_implemented(
      "move_to_parent not implement",
    ))
  }

  /// Return the minimum packed value of the current node.
  fn get_min_packed_value(&self) -> Result<Cow<'_, [u8]>> {
    Err(LuceneError::need_implemented(
      "get_min_packed_value not implement",
    ))
  }

  /// Return the maximum packed value of the current node.
  fn get_max_packed_value(&self) -> Result<Cow<'_, [u8]>> {
    Err(LuceneError::need_implemented(
      "get_max_packed_value not implement",
    ))
  }

  /// Return the number of points below the current node.
  fn size(&self) -> Result<usize> {
    Err(LuceneError::need_implemented("size not implement"))
  }

  /// Visit all the docs below the current node.
  fn visit_doc_ids<IV>(&mut self, _visitor: &mut IV) -> Result<()>
  where
    IV: IntersectVisitor,
  {
    Err(LuceneError::need_implemented("visit_doc_ids not implement"))
  }

  /// Visit all the docs and values below the current node.
  fn visit_doc_values<IV>(&mut self, _visitor: &mut IV) -> Result<()>
  where
    IV: IntersectVisitor,
  {
    Err(LuceneError::need_implemented(
      "visit_doc_values not implement",
    ))
  }

  /// Select the mutable capability of this already acquired tree.
  ///
  /// This is the owned Rust counterpart of Java's `instanceof MutablePointTree`
  /// and cast. The conversion must only move the existing tree: it must not fetch
  /// another tree, allocate, clone, visit points, or invoke user callbacks.
  /// Mutable trees return `PointTreeEnum::Mutable(self)` with `Self` as their
  /// associated mutable type. Wrappers preserve their own exposed capability.
  fn into_mutable(self) -> PointTreeEnum<Self::MutablePointTree, Self>
  where
    Self: Sized,
  {
    PointTreeEnum::Other(self)
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
  /// the provided [`DocIdSetIterator`].
  fn visit_with_iterator<I>(&mut self, iterator: &mut I) -> Result<()>
  where
    I: DocIdSetIterator,
  {
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
  fn visit_iterator_with_packed_value<I>(
    &mut self,
    iterator: &mut I,
    packed_value: &[u8],
  ) -> Result<()>
  where
    I: DocIdSetIterator,
  {
    self.default_visit_iterator_with_packed_value_(iterator, packed_value)
  }
  fn default_visit_iterator_with_packed_value_<I>(
    &mut self,
    iterator: &mut I,
    packed_value: &[u8],
  ) -> Result<()>
  where
    I: DocIdSetIterator,
  {
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

pub enum PointTreeEnum<M, O> {
  Mutable(M),
  Other(O),
}

impl<M, O> TryClone for PointTreeEnum<M, O>
where
  M: MutablePointTree,
  O: PointTree,
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

impl<M, O> PointTree for PointTreeEnum<M, O>
where
  M: MutablePointTree,
  O: PointTree,
{
  type MutablePointTree = M;

  fn into_mutable(self) -> PointTreeEnum<Self::MutablePointTree, Self> {
    match self {
      Self::Mutable(tree) => PointTreeEnum::Mutable(tree),
      Self::Other(tree) => PointTreeEnum::Other(Self::Other(tree)),
    }
  }

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

  fn get_min_packed_value(&self) -> Result<Cow<'_, [u8]>> {
    match self {
      PointTreeEnum::Mutable(mpt) => mpt.get_min_packed_value(),
      PointTreeEnum::Other(pt) => pt.get_min_packed_value(),
    }
  }

  fn get_max_packed_value(&self) -> Result<Cow<'_, [u8]>> {
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
  type MutablePointTree =
    crate::core::codecs::dummy::dummy_mutable_point_tree::DummyMutablePointTree;

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

  fn get_min_packed_value(&self) -> Result<Cow<'_, [u8]>> {
    match self {
      PointTreeEnum2::A(tree) => tree.get_min_packed_value(),
      PointTreeEnum2::B(tree) => tree.get_min_packed_value(),
    }
  }

  fn get_max_packed_value(&self) -> Result<Cow<'_, [u8]>> {
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

impl<T> PointValues for Arc<T>
where
  T: PointValues,
{
  fn get_min_packed_value(&self) -> Result<Option<Cow<'_, [u8]>>> {
    (**self).get_min_packed_value()
  }

  fn get_max_packed_value(&self) -> Result<Option<Cow<'_, [u8]>>> {
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

  fn get_point_tree(&self) -> Result<Self::PointTree> {
    (**self).get_point_tree()
  }

  fn intersect<V>(&self, visitor: &mut V) -> Result<()>
  where
    V: IntersectVisitor,
  {
    (**self).intersect(visitor)
  }

  fn estimate_point_count<V>(&self, visitor: &V) -> Result<i64>
  where
    V: IntersectVisitor,
  {
    (**self).estimate_point_count(visitor)
  }

  fn estimate_doc_count<V>(&self, visitor: &V) -> Result<i64>
  where
    V: IntersectVisitor,
  {
    (**self).estimate_doc_count(visitor)
  }
}
