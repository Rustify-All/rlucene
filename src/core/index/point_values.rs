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
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::store::IndexInput;
use crate::core::util::bkd::bkd_config::BKDConfig;
use crate::core::util::bkd::bkd_reader::BKDPointTree;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::ints_ref::IntsRef;
use std::borrow::Cow;

pub trait PointValues {
    /// Returns minimum value for each dimension, packed, or None if `size()` is
    /// `0`
    fn get_min_packed_value(&self) -> Result<Option<Cow<'_, Vec<u8>>>>;

    /// Returns maximum value for each dimension, packed, or None if `size()` is
    /// `0`
    fn get_max_packed_value(&self) -> Result<Option<Cow<'_, Vec<u8>>>>;

    /// Returns how many dimensions are represented in the values
    fn get_num_dimensions(&self) -> Result<i32>;

    /// Returns how many dimensions are used for the index
    fn get_num_index_dimensions(&self) -> Result<i32>;
    /// Returns the number of bytes per dimension
    fn get_bytes_per_dimension(&self) -> Result<i32>;

    /// Returns the total number of indexed points across all documents.
    fn size(&self) -> Result<i64>;

    /// Returns the total number of documents that have indexed at least one
    /// point.
    fn get_doc_count(&self) -> Result<i32>;
    type PointTree: PointTree;
    fn get_point_tree(&self) -> Result<Self::PointTree>;

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
    fn estimate_point_count(&self, visitor: &mut impl IntersectVisitor) -> Result<i64> {
        let mut point_tree = self.get_point_tree()?;
        let count = estimate_point_count_with_point_tree(visitor, &mut point_tree, i64::MAX)?;
        debug_assert!(!point_tree.move_to_parent()?);
        Ok(count)
    }

    /// Estimate if the point count that would be matched by `intersect`
    /// with the given `IntersectVisitor` is greater than or equal to the
    /// `upper_bound`.
    fn is_estimated_point_count_greater_than_or_equal_to(
        visitor: &mut impl IntersectVisitor,
        point_tree: &mut impl PointTree,
        upper_bound: i64,
    ) -> Result<bool> {
        Ok(estimate_point_count_with_point_tree(visitor, point_tree, upper_bound)? >= upper_bound)
    }

    /// Estimate the number of documents that would be matched by `intersect`
    /// with the given `IntersectVisitor`. This should run many times faster
    /// than `intersect(IntersectVisitor)`.
    ///
    /// See also: `DocIdSetIterator::cost`
    fn estimate_doc_count(&self, visitor: &mut impl IntersectVisitor) -> Result<i64> {
        let estimated_point_count = self.estimate_point_count(visitor)?;
        let doc_count = self.get_doc_count()?;
        let size = self.size()?;

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
    visitor: &mut impl IntersectVisitor,
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
            point_tree.size()
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
                Ok((point_tree.size()? + 1) / 2)
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
pub trait PointTree: Clone {
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
    fn size(&self) -> Result<i64> {
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
pub enum PointTreeEnum<I>
where
    I: IndexInput,
{
    BKD(BKDPointTree<I>),
}

impl<I> Clone for PointTreeEnum<I>
where
    I: IndexInput,
{
    fn clone(&self) -> Self {
        todo!()
    }
}

impl<I> PointTree for PointTreeEnum<I> where I: IndexInput {}
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
    fn compare(&mut self, min_packed_value: &[u8], max_packed_value: &[u8]) -> Result<Relation>;

    /// Notifies the caller that this many documents are about to be visited.
    fn grow(&mut self, _count: i32) -> Result<()> {
        Ok(())
    }
}

pub const MAX_NUM_BYTES: i32 = 16;
pub const MAX_DIMENSIONS: i32 = BKDConfig::MAX_DIMS;
pub const MAX_INDEX_DIMENSIONS: i32 = BKDConfig::MAX_INDEX_DIMS;
