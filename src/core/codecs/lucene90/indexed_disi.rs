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
use crate::core::index::knn_vector_values::DocIndexIterator;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::store::random_access_input::RandomAccessInput;
use crate::core::store::{IndexInput, IndexOutput};
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use parking_lot::Mutex;
use std::sync::Arc;

/// Disk-based implementation of a [`DocIdSetIterator`] which can return the
/// index of the current document, i.e. the ordinal of the current document
/// among the list of documents that this iterator can return. This is useful to
/// implement sparse doc values by only having to encode values for
/// documents that actually have a value.
///
/// Implementation-wise, this [`DocIdSetIterator`] is inspired by
/// [`RoaringDocIdSet`](crate::core::util::roaring_doc_id_set::RoaringDocIdSet)
/// (roaring bitmaps) and encodes ranges of `65536` documents independently,
/// picking between 3 encodings depending on the density of the range:
///
/// - `ALL`: if the range contains exactly 65536 documents,
/// - `DENSE`: if the range contains 4096 documents or more; in that case
///   documents are stored in a bit set,
/// - `SPARSE`: otherwise, and the lower 16 bits of the doc IDs are stored as
///   [`DataInput::read_short`].
///
/// Only ranges that contain at least one value are encoded.
///
/// This implementation uses 6 bytes per document in the worst-case scenario,
/// which happens when all ranges contain exactly one document.
///
/// To avoid O(n) lookup time complexity (where n is the number of documents),
/// two lookup tables are used: a lookup table for block offset and index, and a
/// rank structure for DENSE block index lookups.
///
/// The lookup table is an array of `int` pairs, one pair per block. It allows
/// for direct jumping to the block, rather than iterating forward from the
/// current position block-by-block.
///
/// Each `int` pair entry consists of two logical parts:
///
/// - The first 32-bit `int` holds the index (number of set bits in the blocks)
///   up to just before the wanted block. The maximum number of set bits is the
///   maximum number of documents, which is less than 2^31.
/// - The second `int` holds the byte offset into the underlying slice. Since
///   there is a maximum of 2^16 blocks, the maximum size of any block must not
///   exceed 2^15 bytes to avoid overflow (2^16 if treated as unsigned). This is
///   currently safe, with the largest block being DENSE and using 2^13 + 36
///   bytes.
///
/// The cache overhead is `num_docs / 1024` bytes.
///
/// Note: There are 4 types of blocks: `ALL`, `DENSE`, `SPARSE`, and
/// non-existing (0 set bits). In the case of non-existing blocks, the entry in
/// the lookup table has the same index as the previous entry and the offset
/// points to the next non-empty block.
///
/// The block lookup table is stored at the end of the total block structure.
///
/// The rank structure for `DENSE` blocks is an array of byte pairs with one
/// entry per sub-block (default 512 bits) out of the 65536 bits in the outer
/// `DENSE` block.
///
/// Each rank entry states the number of set bits within the block up to the bit
/// before the bit positioned at the start of the sub-block. Note that the rank
/// entry of the first sub-block is always 0, and the last entry can at most be
/// 65536 - 2 = 65534, so it will always fit into a byte pair (16 bits).
///
/// The rank structure for a given `DENSE` block is stored at the beginning of
/// the block itself, ensuring locality and simplifying block logistics.
///
/// # Lucene Internal
pub struct IndexedDISI<I, P>
where
    I: IndexInput,
    P: IndexedDISIPolicy<I>,
{
    slice: P::Slice,
    jump_table: Option<P::JumpTable>,
    jump_table_entry_count: i32,
    dense_rank_power: i8,
    dense_rank_table: Option<Vec<u8>>,
    cost: i64,
    block: i32,
    block_end: i64,
    // Only used for DENSE blocks
    dense_bitmap_offset: i64,
    next_block_index: i32,
    method: Method,
    doc: i32,
    index: i32,
    // SPARSE variables
    exists: bool,
    next_exist_doc_in_block: i32,
    // DENSE variables
    word: i64,
    word_index: i32,
    // number of one bits encountered so far, including those of `word`
    number_of_ones: i32,
    // Used with rank for jumps inside of DENSE as they are absolute instead of
    // relative
    dense_origo_index: i32,
    // ALL variables
    gap: i32,
}
impl<I> IndexedDISI<I, Owned>
where
    I: IndexInput,
{
    /// Constructs a new `IndexedDISI` instance by reading from the backing
    /// input data.
    ///
    /// This constructor always creates a new `block_slice` and a new
    /// `jump_table` from the input, to ensure that operations are
    /// independent from the caller. For re-using existing slices and
    /// tables, see [`IndexedDISI::from_components`] (if implemented).
    ///
    /// # Parameters
    /// - `input`: The backing input data.
    /// - `offset`: The starting byte offset of the blocks in the input.
    /// - `length`: The number of bytes that hold both blocks and the jump
    ///   table.
    /// - `jump_table_entry_count`: The number of blocks covered by the jump
    ///   table. This must match the number returned by `write_bit_set`
    /// - `dense_rank_power`: The number of doc IDs covered by each rank entry
    ///   in DENSE blocks, expressed as `2^dense_rank_power`. This must match
    ///   the value passed to `write_bit_set` when writing.
    /// - `cost`: Typically the number of logical doc IDs.
    pub fn new(
        index_input: &I,
        offset: usize,
        length: usize,
        jump_table_entry_count: i32,
        dense_rank_power: i8,
        cost: i64,
    ) -> Result<Self> {
        let block_slice =
            create_block_slice(index_input, "docs", offset, length, jump_table_entry_count)?;
        let jump_table = create_jump_table(index_input, offset, length, jump_table_entry_count)?;

        Self::from_components(
            block_slice,
            jump_table,
            jump_table_entry_count,
            dense_rank_power,
            cost,
        )
    }
}

impl<I, P> IndexedDISI<I, P>
where
    I: IndexInput,
    P: IndexedDISIPolicy<I>,
{
    /// Constructs an `IndexedDISI` using the provided block slice and jump
    /// table, allowing reuse of existing data structures.  
    ///
    /// This is useful in cases like Lucene80 norms producer's merge instance,
    /// where reuse improves performance and avoids unnecessary allocations.
    ///
    /// # Parameters
    /// - `block_slice`: The data blocks, typically created using
    ///   [`create_block_slice`].
    /// - `jump_table`: The table holding jump data for block skips, typically
    ///   created using
    ///   [`create_jump_table`].
    /// - `jump_table_entry_count`: The number of blocks covered by the jump
    ///   table. This must match the number returned by `write_bit_set`
    /// - `dense_rank_power`: The number of doc IDs covered by each rank entry
    ///   in DENSE blocks, expressed as `2^dense_rank_power`. This must match
    ///   the value used in `write_bit_set`.
    /// - `cost`: Typically the number of logical doc IDs.
    pub fn from_components(
        mut block_slice: P::Slice,
        mut jump_table: Option<P::JumpTable>,
        jump_table_entry_count: i32,
        dense_rank_power: i8,
        cost: i64,
    ) -> Result<Self> {
        if !(7..=15).contains(&dense_rank_power) && dense_rank_power != -1 {
            return Err(LuceneError::illegal_argument(format!(
                "Acceptable values for denseRankPower are 7-15 (every 128-32768 docIDs). \
                     The provided power was {} (every {} docIDs).",
                dense_rank_power,
                1 << dense_rank_power
            )));
        }

        block_slice.with_mut(|index_input| {
            if index_input.length() > 0 {
                index_input.prefetch(0, 1)?;
            }
            Ok::<(), LuceneError>(())
        })?;

        if let Some(jump_table_rc) = &mut jump_table {
            jump_table_rc.with_mut(|jump_table| {
                if jump_table.length() > 0 {
                    jump_table.prefetch(0, 1)?;
                }
                Ok::<(), LuceneError>(())
            })?
        }

        let dense_rank_table = if dense_rank_power == -1 {
            None
        } else {
            let rank_index_shift = dense_rank_power - 7;
            Some(vec![0u8; (DENSE_BLOCK_LONGS >> rank_index_shift) as usize])
        };

        Ok(Self {
            slice: block_slice,
            jump_table,
            jump_table_entry_count,
            dense_rank_power,
            dense_rank_table,
            cost,
            block: -1,
            block_end: 0,
            dense_bitmap_offset: -1,
            next_block_index: -1,
            method: Method::Sparse,
            doc: -1,
            index: -1,
            exists: false,
            next_exist_doc_in_block: 0,
            word: 0,
            word_index: -1,
            number_of_ones: 0,
            dense_origo_index: 0,
            gap: 0,
        })
    }

    fn advance_block(&mut self, target_block: i32) -> Result<()> {
        let block_index = target_block >> 16;
        // If the destination block is 2 blocks or more ahead, we use the
        // jump-table.
        if let Some(jump_table_rc) = &mut self.jump_table
            && block_index >= (self.block >> 16) + 2
        {
            // If the jumpTableEntryCount is exceeded, there are no further
            // bits. Last entry is always NO_MORE_DOCS
            let in_range_block_index = if block_index < self.jump_table_entry_count {
                block_index
            } else {
                self.jump_table_entry_count - 1
            };

            let jump_pos = in_range_block_index.try_convert()? * BitUtil::INT_BYTES * 2;
            jump_table_rc.with_mut(|jump_table| {
                let index = jump_table.read_int(jump_pos)?;
                let offset = jump_table.read_int(jump_pos + BitUtil::INT_BYTES)?;
                // -1 to compensate for the always-added 1 in readBlockHeader
                self.next_block_index = index - 1;
                self.slice.with_mut(|slice| slice.seek(offset as usize))?;
                Ok::<(), LuceneError>(())
            })?;
            self.read_block_header()?;
            return Ok(());
        }
        // Fallback to iteration of blocks
        loop {
            self.slice
                .with_mut(|slice| slice.seek(self.block_end as usize))?;
            self.read_block_header()?;
            if self.block < target_block {
                continue;
            }
            break;
        }

        Ok(())
    }

    fn read_block_header(&mut self) -> Result<()> {
        self.slice.with_mut(|slice| {
            self.block = (slice.read_short()? as u16 as i32) << 16;
            debug_assert!(self.block >= 0);
            let num_values = 1 + slice.read_short()? as u16 as i32;

            self.index = self.next_block_index;
            self.next_block_index = self.index + num_values;

            if num_values <= MAX_ARRAY_LENGTH {
                self.method = Method::Sparse;
                self.block_end = slice.get_file_pointer()? as i64 + (num_values << 1) as i64;
                self.next_exist_doc_in_block = -1;
            } else if num_values == BLOCK_SIZE {
                self.method = Method::ALL;
                self.block_end = slice.get_file_pointer()? as i64;
                self.gap = self.block - self.index - 1;
            } else {
                self.method = Method::Dense;
                self.dense_bitmap_offset = slice.get_file_pointer()? as i64
                    + self.dense_rank_table.as_ref().map(|v| v.len()).unwrap_or(0) as i64;
                self.block_end = self.dense_bitmap_offset + (1 << 13);
                // Performance consideration: All rank (default 128 * 16 bits) are
                // loaded up front. This should be fast with the
                // reusable byte[] buffer, but it is still wasted if the DENSE block
                // is iterated in small steps.
                // If this results in too great a performance regression, a
                // heuristic strategy might work where the rank data
                // are loaded on first in-block advance, if said advance is > X
                // docIDs. The hope being that a small first
                // advance means that subsequent advances will be small too.
                // Another alternative is to maintain an extra slice for DENSE rank,
                // but IndexedDISI is already slice-heavy.
                if self.dense_rank_power != -1 {
                    let rank_table_len = self.dense_rank_table.as_ref().unwrap().len();
                    debug_assert!(rank_table_len <= i32::MAX as usize);
                    if let Some(rank_table) = self.dense_rank_table.as_mut() {
                        slice.read_bytes(rank_table, 0, rank_table_len)?;
                    }
                }

                self.word_index = -1;
                self.number_of_ones = self.index + 1;
                self.dense_origo_index = self.number_of_ones;
            }
            Ok::<(), LuceneError>(())
        })?;
        Ok(())
    }
    pub fn advance_exact(&mut self, target: i32) -> Result<bool> {
        let target_block = ((target as u32) & 0xFFFF_0000) as i32;

        if self.block < target_block {
            self.advance_block(target_block)?;
        }

        let found = self.block == target_block && {
            match self.method {
                Method::Sparse => SparseMethod.advance_exact_within_block(self, target)?,
                Method::Dense => DenseMethod.advance_exact_within_block(self, target)?,
                Method::ALL => All.advance_exact_within_block(self, target)?,
            }
        };
        self.doc = target;
        Ok(found)
    }
    pub fn index(&self) -> i32 {
        self.index
    }
    pub(crate) fn index_u(&self) -> usize {
        debug_assert!(self.index >= 0);
        self.index as usize
    }
}

impl<I, P> DocIdSetIterator for IndexedDISI<I, P>
where
    I: IndexInput,
    P: IndexedDISIPolicy<I>,
{
    fn doc_id(&self) -> i32 {
        self.doc
    }

    fn next_doc(&mut self) -> Result<i32> {
        self.advance(self.doc + 1)
    }

    fn advance(&mut self, target: i32) -> Result<i32> {
        let target_block = ((target as u32) & 0xFFFF_0000) as i32;

        if self.block < target_block {
            self.advance_block(target_block)?;
        }

        if self.block == target_block {
            let advanced = match self.method {
                Method::Sparse => SparseMethod.advance_within_block(self, target)?,
                Method::Dense => DenseMethod.advance_within_block(self, target)?,
                Method::ALL => All.advance_within_block(self, target)?,
            };
            if advanced {
                return Ok(self.doc);
            }
            self.read_block_header()?;
        }

        let found = match self.method {
            Method::Sparse => SparseMethod.advance_within_block(self, self.block)?,
            Method::Dense => DenseMethod.advance_within_block(self, self.block)?,
            Method::ALL => All.advance_within_block(self, self.block)?,
        };
        debug_assert!(found);
        Ok(self.doc)
    }

    fn cost(&self) -> Result<i64> {
        Ok(self.cost)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Sparse,
    Dense,
    ALL,
}
impl MethodBehavior for Method {
    fn advance_within_block<I, P>(&self, disi: &mut IndexedDISI<I, P>, target: i32) -> Result<bool>
    where
        I: IndexInput,
        P: IndexedDISIPolicy<I>,
    {
        match self {
            Method::Sparse => SparseMethod.advance_within_block(disi, target),
            Method::Dense => DenseMethod.advance_within_block(disi, target),
            Method::ALL => All.advance_within_block(disi, target),
        }
    }

    fn advance_exact_within_block<I, P>(
        &self,
        disi: &mut IndexedDISI<I, P>,
        target: i32,
    ) -> Result<bool>
    where
        I: IndexInput,
        P: IndexedDISIPolicy<I>,
    {
        match self {
            Method::Sparse => SparseMethod.advance_exact_within_block(disi, target),
            Method::Dense => DenseMethod.advance_exact_within_block(disi, target),
            Method::ALL => All.advance_exact_within_block(disi, target),
        }
    }
}
trait MethodBehavior {
    /// Advance to the first doc from the block that is equal to or greater than
    /// `target`. Return true if there is such a doc and false otherwise.
    fn advance_within_block<I, P>(&self, disi: &mut IndexedDISI<I, P>, target: i32) -> Result<bool>
    where
        I: IndexInput,
        P: IndexedDISIPolicy<I>;
    /// Advance the iterator exactly to the position corresponding to the given
    /// `target` and return whether this document exists.
    fn advance_exact_within_block<I, P>(
        &self,
        disi: &mut IndexedDISI<I, P>,
        target: i32,
    ) -> Result<bool>
    where
        I: IndexInput,
        P: IndexedDISIPolicy<I>;
}

struct SparseMethod;
impl MethodBehavior for SparseMethod {
    fn advance_within_block<I, P>(&self, disi: &mut IndexedDISI<I, P>, target: i32) -> Result<bool>
    where
        I: IndexInput,
        P: IndexedDISIPolicy<I>,
    {
        let target_in_block = target & 0xFFFF;

        disi.slice.with_mut(|slice| {
            while disi.index < disi.next_block_index {
                let doc = slice.read_short()? as u16 as i32;
                disi.index += 1;

                if doc >= target_in_block {
                    disi.doc = disi.block | doc;
                    disi.exists = true;
                    disi.next_exist_doc_in_block = doc;
                    return Ok(true);
                }
            }
            Ok(false)
        })
    }

    fn advance_exact_within_block<I, P>(
        &self,
        disi: &mut IndexedDISI<I, P>,
        target: i32,
    ) -> Result<bool>
    where
        I: IndexInput,
        P: IndexedDISIPolicy<I>,
    {
        let target_in_block = target & 0xFFFF;
        // TODO: binary search
        if disi.next_exist_doc_in_block > target_in_block {
            debug_assert!(!disi.exists);
            return Ok(false);
        }

        if disi.doc == target {
            return Ok(disi.exists);
        }
        disi.slice.with_mut(|slice| {
            while disi.index < disi.next_block_index {
                let doc = slice.read_short()? as u16 as i32;
                disi.index += 1;

                if doc >= target_in_block {
                    disi.next_exist_doc_in_block = doc;

                    if doc != target_in_block {
                        disi.index -= 1;
                        let fp = slice.get_file_pointer()?;
                        slice.seek(fp - 2)?;
                        break;
                    }

                    disi.exists = true;
                    return Ok(true);
                }
            }
            disi.exists = false;
            Ok(false)
        })
    }
}
struct DenseMethod;
impl MethodBehavior for DenseMethod {
    fn advance_within_block<I, P>(&self, disi: &mut IndexedDISI<I, P>, target: i32) -> Result<bool>
    where
        I: IndexInput,
        P: IndexedDISIPolicy<I>,
    {
        let target_in_block = target & 0xFFFF;
        let target_word_index = (target_in_block as u32 >> 6) as i32;
        // If possible, skip ahead using the rank cache
        // If the distance between the current position and the target is <
        // rank-longs there is no sense in using rank
        if disi.dense_rank_power != -1
            && target_word_index - disi.word_index >= (1 << (disi.dense_rank_power - 6))
        {
            rank_skip(disi, target_in_block)?;
        }

        disi.slice.with_mut(|slice| {
            for _ in disi.word_index + 1..=target_word_index {
                disi.word = slice.read_long()?;
                disi.number_of_ones += disi.word.count_ones() as i32;
            }
            disi.word_index = target_word_index;

            let left_bits = (disi.word as u64) >> (target_in_block & 63);
            if left_bits != 0 {
                disi.doc = target + left_bits.trailing_zeros() as i32;
                disi.index = disi.number_of_ones - left_bits.count_ones() as i32;
                return Ok(true);
            }

            while {
                disi.word_index += 1;
                disi.word_index < 1024
            } {
                disi.word = slice.read_long()?;
                if disi.word != 0 {
                    disi.index = disi.number_of_ones;
                    disi.number_of_ones += disi.word.count_ones() as i32;
                    disi.doc =
                        disi.block | ((disi.word_index << 6) | disi.word.trailing_zeros() as i32);
                    return Ok(true);
                }
            }
            Ok(false)
        })
    }

    fn advance_exact_within_block<I, P>(
        &self,
        disi: &mut IndexedDISI<I, P>,
        target: i32,
    ) -> Result<bool>
    where
        I: IndexInput,
        P: IndexedDISIPolicy<I>,
    {
        let target_in_block = target & 0xFFFF;
        let target_word_index = ((target_in_block as u32) >> 6) as i32;
        // If possible, skip ahead using the rank cache
        // If the distance between the current position and the target is <
        // rank-longs there is no sense in using rank
        if disi.dense_rank_power != -1
            && target_word_index - disi.word_index >= (1 << (disi.dense_rank_power - 6))
        {
            rank_skip(disi, target_in_block)?;
        }

        disi.slice.with_mut(|slice| {
            for _ in (disi.word_index + 1)..=target_word_index {
                disi.word = slice.read_long()?;
                disi.number_of_ones += disi.word.count_ones() as i32;
            }
            Ok::<(), LuceneError>(())
        })?;

        disi.word_index = target_word_index;

        let left_bits = (disi.word as u64 >> (target_in_block & 63)) as i64;
        disi.index = disi.number_of_ones - left_bits.count_ones() as i32;

        Ok((left_bits & 1) != 0)
    }
}
struct All;
impl MethodBehavior for All {
    fn advance_within_block<I, P>(&self, disi: &mut IndexedDISI<I, P>, target: i32) -> Result<bool>
    where
        I: IndexInput,
        P: IndexedDISIPolicy<I>,
    {
        disi.doc = target;
        disi.index = target - disi.gap;
        Ok(true)
    }

    fn advance_exact_within_block<I, P>(
        &self,
        disi: &mut IndexedDISI<I, P>,
        target: i32,
    ) -> Result<bool>
    where
        I: IndexInput,
        P: IndexedDISIPolicy<I>,
    {
        disi.index = target - disi.gap;
        Ok(true)
    }
}

pub struct DocIndexIteratorImpl<'a, I, P>
where
    I: IndexInput,
    P: IndexedDISIPolicy<I>,
{
    disi: &'a mut IndexedDISI<I, P>,
}
impl<'a, I, P> DocIndexIteratorImpl<'a, I, P>
where
    I: IndexInput,
    P: IndexedDISIPolicy<I>,
{
    pub fn new(disi: &'a mut IndexedDISI<I, P>) -> Self {
        DocIndexIteratorImpl { disi }
    }
}
impl<I, P> DocIdSetIterator for DocIndexIteratorImpl<'_, I, P>
where
    I: IndexInput,
    P: IndexedDISIPolicy<I>,
{
    fn doc_id(&self) -> i32 {
        self.disi.doc_id()
    }

    fn next_doc(&mut self) -> Result<i32> {
        self.disi.next_doc()
    }

    fn advance(&mut self, target: i32) -> Result<i32> {
        self.disi.advance(target)
    }

    fn cost(&self) -> Result<i64> {
        self.disi.cost()
    }
}
impl<I, P> DocIndexIterator for DocIndexIteratorImpl<'_, I, P>
where
    I: IndexInput,
    P: IndexedDISIPolicy<I>,
{
    fn index(&self) -> Result<i32> {
        Ok(self.disi.index())
    }
}
pub trait IndexedDISIPolicy<I>
where
    I: IndexInput,
{
    type Slice: MutAccess<I>;
    type JumpTable: MutAccess<I::RandomAccessSlice>;
}

pub struct Owned;
pub struct Shared;
impl<I> IndexedDISIPolicy<I> for Owned
where
    I: IndexInput,
{
    type Slice = I;
    type JumpTable = I::RandomAccessSlice;
}

impl<I> IndexedDISIPolicy<I> for Shared
where
    I: IndexInput,
{
    type Slice = Arc<Mutex<I>>;
    type JumpTable = Arc<Mutex<I::RandomAccessSlice>>;
}

use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::util::TryIntoInt;
use crate::core::util::access::MutAccess;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bit_set_iterator::BitSetIterator;
use crate::core::util::fixed_bit_set::FixedBitSet;

// jump-table time/space trade-offs to consider:
// The block offsets and the block indexes could be stored in more
// compressed form with two PackedInts or two MonotonicDirectReaders.
// The DENSE ranks (default 128 shorts = 256 bytes) could likewise be
// compressed. But as there is at least 4096 set bits in DENSE blocks,
// there will be at least one rank with 2^12 bits, so it is doubtful if
// there is much to gain here. The number of docIDs that a single block
// represents
const BLOCK_SIZE: i32 = 65536;
// Long.SIZE = 64 bits
const DENSE_BLOCK_LONGS: i32 = BLOCK_SIZE / i64::BITS as i32;
// Every 512 docIDs / 8 longs
pub const DEFAULT_DENSE_RANK_POWER: i8 = 9;
pub(crate) const MAX_ARRAY_LENGTH: i32 = (1 << 12) - 1;
/// Writes the doc IDs from the iterator into the output in logical blocks,
/// with one block for every 65,536 doc IDs, in monotonically increasing and
/// gap-less order.
///
/// DENSE blocks use
/// [`DEFAULT_DENSE_RANK_POWER`] of 9,
/// meaning a rank is written every 512 doc IDs (8 longs).
///
/// The caller must keep track of:
/// - The number of jump-table entries (returned by this method),
/// - The `dense_rank_power` (9 in this method), and provide them when
///   constructing an `IndexedDISI` for reading.
///
/// # Parameters
/// - `it`: the document ID iterator (monotonically increasing, no gaps).
/// - `out`: the output target for writing the encoded blocks.
///
/// # Returns
/// The number of jump-table entries that follow the blocks, or `-1` if
/// there are no entries. This value should be stored in metadata and
/// used when creating an `IndexedDISI` instance.
///
/// # Errors
/// Returns an error if writing to the output fails.
pub(crate) fn write_bitset<O>(it: &mut impl DocIdSetIterator, out: &mut O) -> Result<i16>
where
    O: IndexOutput,
{
    write_bitset_with_dense_rank_power(it, out, DEFAULT_DENSE_RANK_POWER)
}
/// Writes the doc IDs from the iterator into the output in logical blocks,
/// with one block for every 65,536 doc IDs, in monotonically increasing and
/// gap-less order.
///
/// The caller must keep track of:
/// - The number of jump-table entries (returned by this method),
/// - The `dense_rank_power`, and provide them when constructing an
///   `IndexedDISI` for reading.
///
/// # Parameters
/// - `it`: The iterator over document IDs (must be sorted, gap-less,
///   increasing).
/// - `out`: The destination where blocks will be written.
/// - `dense_rank_power`: For `DENSE` blocks, a rank will be written every
///   `2^dense_rank_power` doc IDs.
///     - Values `< 7` (every 128 doc IDs) or `> 15` (every 32,768 doc IDs)
///       disable DENSE rank.
///     - Recommended values: 8–12 (every 256–4096 doc IDs, or 4–64 longs).
///     - The default value is `DEFAULT_DENSE_RANK_POWER = 9`, which means
///       every 512 doc IDs.
///
/// # Returns
/// The number of jump-table entries that follow the blocks, or `-1` if
/// there are no entries. This value should be stored in metadata and
/// used when creating an instance of `IndexedDISI`.
///
/// # Errors
/// Returns an error if writing to the output fails.
pub fn write_bitset_with_dense_rank_power<O>(
    it: &mut impl DocIdSetIterator,
    out: &mut O,
    dense_rank_power: i8,
) -> Result<i16>
where
    O: IndexOutput,
{
    let origo = out.get_file_pointer();
    if !(7..=15).contains(&dense_rank_power) && dense_rank_power != -1 {
        return Err(LuceneError::illegal_argument(format!(
            "Acceptable values for denseRankPower are 7-15 (every 128-32768 docIDs). \
             The provided power was {} (every {} docIDs)",
            dense_rank_power,
            1 << dense_rank_power
        )));
    }

    let mut total_cardinality = 0;
    let mut block_cardinality = 0;
    let mut buffer = FixedBitSet::new(1 << 16);
    let jumps_len = ArrayUtil::oversize(1, BitUtil::INT_BYTES * 2);
    let mut jumps: Vec<i32> = vec![0; jumps_len];
    let mut prev_block = -1;
    let mut jump_block_index = 0;

    let mut doc = it.next_doc()?;
    while doc != NO_MORE_DOCS {
        let block = doc >> 16;

        if prev_block != -1 && block != prev_block {
            add_jumps(
                &mut jumps,
                out.get_file_pointer() - origo,
                total_cardinality,
                jump_block_index,
                (prev_block + 1) as usize,
            )?;
            jump_block_index = (prev_block + 1) as usize;
            buffer = flush(prev_block, buffer, block_cardinality, dense_rank_power, out)?;
            buffer.clear();
            total_cardinality += block_cardinality;
            block_cardinality = 0;
        }

        buffer.set((doc & 0xFFFF).try_convert()?);
        block_cardinality += 1;
        prev_block = block;

        doc = it.next_doc()?;
    }

    if block_cardinality > 0 {
        add_jumps(
            &mut jumps,
            out.get_file_pointer() - origo,
            total_cardinality,
            jump_block_index,
            (prev_block + 1) as usize,
        )?;
        total_cardinality += block_cardinality;
        buffer = flush(prev_block, buffer, block_cardinality, dense_rank_power, out)?;
        buffer.clear();
        prev_block += 1;
    }

    let last_block: usize = if prev_block == -1 {
        0
    } else {
        prev_block as usize
    };
    // There will always be at least 1 block (NO_MORE_DOCS)
    // Last entry is a SPARSE with blockIndex == 32767 and the single entry
    // 65535, which becomes the docID NO_MORE_DOCS
    // To avoid creating 65K jump-table entries, only a single entry is
    // created pointing to the offset of the
    // NO_MORE_DOCS block, with the jumpBlockIndex set to the logical EMPTY
    // block after all real blocks.
    add_jumps(
        &mut jumps,
        out.get_file_pointer() - origo,
        total_cardinality,
        last_block,
        last_block + 1,
    )?;

    buffer.set((NO_MORE_DOCS & 0xFFFF) as usize);
    let _ = flush(NO_MORE_DOCS >> 16, buffer, 1, dense_rank_power, out)?;

    flush_block_jumps(&jumps, last_block + 1, out)
}
/// Helper method for using [`IndexedDISI::from_components`].
/// Creates a `disi_slice` for the `IndexedDISI` data blocks, excluding the
/// jump table.
///
/// # Parameters
/// - `slice`: Backing data containing both blocks and the jump table.
/// - `slice_description`: Human-readable description of the slice.
/// - `offset`: Offset relative to the backing data.
/// - `length`: Total length of the `IndexedDISI`, including blocks and
///   jump-table data.
/// - `jump_table_entry_count`: Number of blocks covered by the jump table.
///
/// # Returns
/// A jump table containing the block skip data, or `None` if no such table
/// exists.
///
/// # Errors
/// Returns an error if a `RandomAccessInput` could not be created from the
/// slice.
pub fn create_block_slice<I: IndexInput>(
    slice: &I,
    slice_description: &str,
    offset: usize,
    length: usize,
    jump_table_entry_count: i32,
) -> Result<I> {
    let jump_table_bytes = if jump_table_entry_count < 0 {
        0
    } else {
        jump_table_entry_count as usize * BitUtil::INT_BYTES * 2
    };
    slice.slice(slice_description, offset, length - jump_table_bytes)
}
/// Helper method for using [`IndexedDISI::from_components`].
/// Creates a `RandomAccessInput` covering only the jump-table data, or
/// returns `None` if no such table exists.
///
/// # Parameters
/// - `slice`: Backing data containing both blocks and the jump table.
/// - `offset`: Offset relative to the backing data.
/// - `length`: Total length of the `IndexedDISI`, including blocks and
///   jump-table data.
/// - `jump_table_entry_count`: Number of blocks covered by the jump table.
///
/// # Returns
/// A `RandomAccessInput` covering the jump-table section, or `None` if the
/// table doesn't exist.
///
/// # Errors
/// Returns an error if a `RandomAccessInput` could not be created from the
/// slice.
pub fn create_jump_table<I: IndexInput>(
    slice: &I,
    offset: usize,
    length: usize,
    jump_table_entry_count: i32,
) -> Result<Option<I::RandomAccessSlice>> {
    if jump_table_entry_count <= 0 {
        Ok(None)
    } else {
        let jump_table_bytes = jump_table_entry_count as usize * BitUtil::INT_BYTES * 2;
        slice
            .random_access_slice(offset + length - jump_table_bytes, jump_table_bytes)
            .map(Some)
    }
}

fn flush<O>(
    block: i32,
    mut buffer: FixedBitSet,
    cardinality: i32,
    dense_rank_power: i8,
    out: &mut O,
) -> Result<FixedBitSet>
where
    O: IndexOutput,
{
    debug_assert!((0..BLOCK_SIZE).contains(&block));
    out.write_short(block as i16)?;
    debug_assert!(cardinality > 0 && cardinality <= BLOCK_SIZE);
    out.write_short((cardinality - 1) as i16)?;

    if cardinality > MAX_ARRAY_LENGTH {
        if cardinality != BLOCK_SIZE {
            if dense_rank_power != -1 {
                let rank = create_rank(&buffer, dense_rank_power as u8);
                let rank_len = rank.len();
                debug_assert!(rank_len <= i32::MAX as usize);
                out.write_bytes_with_len(&rank, rank_len)?;
            }
            for word in buffer.get_bits() {
                out.write_long(*word)?;
            }
        }
    } else {
        let mut iter = BitSetIterator::new(buffer, cardinality as i64)?;
        let mut doc;
        while {
            doc = iter.next_doc()?;
            doc != NO_MORE_DOCS
        } {
            out.write_short(doc as i16)?;
        }
        buffer = iter.bits
    }

    Ok(buffer)
}

// Creates a DENSE rank-entry (the number of set bits up to a given point)
// for the buffer. One rank-entry for every {@code 2^denseRankPower}
// bits, with each rank-entry using 2 bytes. Represented as a byte[] for
// fast flushing and mirroring of the retrieval representation.
fn create_rank(buffer: &FixedBitSet, dense_rank_power: u8) -> Vec<u8> {
    let longs_per_rank = 1 << (dense_rank_power - 6);
    let rank_mark = longs_per_rank - 1;
    // 6 for the long (2^6) + 1 for 2 bytes/entry
    let rank_index_shift = dense_rank_power - 7;
    let rank = (DENSE_BLOCK_LONGS >> rank_index_shift) as usize;
    let mut rank = vec![0u8; rank];
    let bits = buffer.get_bits();
    let mut bit_count = 0;
    #[allow(clippy::needless_range_loop)]
    for word in 0..DENSE_BLOCK_LONGS as usize {
        // Every longsPerRank longs
        if (word & rank_mark) == 0 {
            let rank_index = word >> rank_index_shift;
            rank[rank_index] = (bit_count >> 8) as u8;
            rank[rank_index + 1] = (bit_count & 0xFF) as u8;
        }
        bit_count += bits[word].count_ones() as i32;
    }
    rank
}

// Adds entries to the offset & index jump-table for blocks
fn add_jumps(
    jumps: &mut Vec<i32>,
    offset: usize,
    index: i32,
    start_block: usize,
    end_block: usize,
) -> Result<()> {
    debug_assert!(
        offset < i32::MAX as usize,
        "Logically the offset should not exceed 2^30 but was >= i32::MAX"
    );
    ArrayUtil::grow_i32(jumps, (end_block + 1) * 2)?;
    let offset = offset as i32;
    for b in start_block..end_block {
        let i = b * 2;
        jumps[i] = index;
        jumps[i + 1] = offset;
    }
    Ok(())
}
// Flushes the offset & index jump-table for blocks. This should be the last
// data written to out This method returns the blockCount for the blocks
// reachable for the jump_table or -1 for no jump-table
fn flush_block_jumps<O: IndexOutput>(
    jumps: &[i32],
    mut block_count: usize,
    out: &mut O,
) -> Result<i16> {
    // Jumps with a single real entry + NO_MORE_DOCS is just wasted space so
    // we ignore that
    if block_count == 2 {
        block_count = 0;
    }

    for i in 0..block_count {
        out.write_int(jumps[i * 2])?;
        out.write_int(jumps[i * 2 + 1])?;
    }
    // As there are at most 32k blocks, the count is a short
    // The jumpTableOffset will be at lastPos - (blockCount * Long.BYTES)
    Ok(block_count as i16)
}
/// If the distance between the current position and the target is greater
/// than 8 words, the rank cache will be used to guarantee a worst-case
/// of one rank lookup and up to seven word-read-and-bit-count
/// operations.
///
/// **Note**: This does *not* guarantee a skip up to the target, only up to
/// the nearest rank boundary. It is the caller’s responsibility to
/// continue iterating to reach the actual target.
///
/// # Parameters
/// - `disi`: The standard `DISI` instance.
/// - `target_in_block`: The lower 16 bits of the target document ID.
///
/// # Errors
/// Returns an error if seeking in the `DISI` failed.
fn rank_skip<I, P>(disi: &mut IndexedDISI<I, P>, target_in_block: i32) -> Result<()>
where
    I: IndexInput,
    P: IndexedDISIPolicy<I>,
{
    debug_assert!(
        disi.dense_rank_power >= 0,
        "dense_rank_power = {}",
        disi.dense_rank_power
    );
    // Resolve the rank as close to targetInBlock as possible (maximum
    // distance is 8 longs) Note: rankOrigoOffset is tracked on
    // block open, so it is absolute (e.g. don't add origo)
    let rank_index = target_in_block >> disi.dense_rank_power; // Default is 9 (8 longs: 2^3 * 2^6 = 512 docIDs)
    let byte_index = (rank_index << 1) as usize;
    let mut rank = 0;
    match &disi.dense_rank_table {
        None => {
            Err::<(), LuceneError>(LuceneError::unreachable("should not be here"))?;
        },
        Some(rank_table) => {
            let high = rank_table[byte_index] as u16;
            let low = rank_table[byte_index + 1] as u16;
            rank = ((high << 8) | low) as i32;
        },
    }
    // Position the counting logic just after the rank point
    let rank_aligned_word_index = (rank_index << disi.dense_rank_power) >> 6;
    let offset =
        disi.dense_bitmap_offset + (rank_aligned_word_index as i64) * BitUtil::LONG_BYTES as i64;
    let rank_word = disi.slice.with_mut(|slice| {
        slice.seek(offset as usize)?;
        slice.read_long()
    })?;
    let dense_noo = rank + rank_word.count_ones() as i32;

    disi.word_index = rank_aligned_word_index;
    disi.word = rank_word;
    disi.number_of_ones = disi.dense_origo_index + dense_noo;

    Ok(())
}
///  Returns an iterator that delegates to the IndexedDISI. Advancing this
/// iterator will advance the underlying IndexedDISI, and vice-versa.
pub fn get_doc_index_iterator<D, I, P>(
    disi: &mut IndexedDISI<I, P>,
) -> DocIndexIteratorImpl<'_, I, P>
where
    I: IndexInput,
    P: IndexedDISIPolicy<I>,
{
    DocIndexIteratorImpl::new(disi)
}

pub enum IndexedDISIEnum<I>
where
    I: IndexInput,
{
    Owned(IndexedDISI<I, Owned>),
    Shared(IndexedDISI<I, Shared>),
}
impl<I> IndexedDISIEnum<I>
where
    I: IndexInput,
{
    pub fn advance_exact(&mut self, target: i32) -> Result<bool> {
        match self {
            IndexedDISIEnum::Owned(disi) => disi.advance_exact(target),
            IndexedDISIEnum::Shared(disi) => disi.advance_exact(target),
        }
    }
    pub fn index(&self) -> i32 {
        match self {
            IndexedDISIEnum::Owned(disi) => disi.index(),
            IndexedDISIEnum::Shared(disi) => disi.index(),
        }
    }
}
impl<I> DocIdSetIterator for IndexedDISIEnum<I>
where
    I: IndexInput,
{
    fn doc_id(&self) -> i32 {
        match self {
            IndexedDISIEnum::Owned(disi) => disi.doc_id(),
            IndexedDISIEnum::Shared(disi) => disi.doc_id(),
        }
    }

    fn next_doc(&mut self) -> Result<i32> {
        match self {
            IndexedDISIEnum::Owned(disi) => disi.next_doc(),
            IndexedDISIEnum::Shared(disi) => disi.next_doc(),
        }
    }

    fn advance(&mut self, target: i32) -> Result<i32> {
        match self {
            IndexedDISIEnum::Owned(disi) => disi.advance(target),
            IndexedDISIEnum::Shared(disi) => disi.advance(target),
        }
    }

    fn slow_advance(&mut self, target: i32) -> Result<i32> {
        match self {
            IndexedDISIEnum::Owned(disi) => disi.slow_advance(target),
            IndexedDISIEnum::Shared(disi) => disi.slow_advance(target),
        }
    }

    fn cost(&self) -> Result<i64> {
        match self {
            IndexedDISIEnum::Owned(disi) => disi.cost(),
            IndexedDISIEnum::Shared(disi) => disi.cost(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::core::codecs::indexed_disi::{
        MAX_ARRAY_LENGTH, Owned, create_block_slice, create_jump_table,
        write_bitset_with_dense_rank_power,
    };
    use crate::core::codecs::lucene90::indexed_disi::{IndexedDISI, Method};
    use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
    use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
    use crate::core::store::directory::Directory;
    use crate::core::store::{IOContext, IndexInput, IndexOutput};

    use crate::core::util::bit_set::{BitSet, of};
    use crate::core::util::bit_set_iterator::BitSetIterator;
    use crate::core::util::error::lucene_error::{LuceneError, Result};
    use crate::core::util::fixed_bit_set::FixedBitSet;
    use crate::core::util::sparse_fixed_bit_set::SparseFixedBitSet;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{
        at_least, new_directory, random, rarely,
    };
    use crate::test::util::test_util::TestUtil;

    use crate::core::util::TryIntoInt;
    use rand::Rng;

    #[allow(dead_code)] // for quick search
    struct TestIndexedDISI;

    #[test]
    fn test_empty() -> Result<()> {
        let mut random = random();
        let max_doc = TestUtil::next_usize(&mut random, 1, 100_000);
        let set = SparseFixedBitSet::new(max_doc)?;
        let dir = new_directory(&mut random)?;
        let _ = do_test(set, &dir, &mut random);
        Ok(())
    }

    #[test]
    #[ignore]
    fn test_empty_blocks() -> Result<()> {
        const B: usize = 65536;
        let mut random = random();
        let max_doc = B * 11;
        let mut set = SparseFixedBitSet::new(max_doc)?;
        set.set(B + 5);
        set.set(B * 4 + 5);
        for i in 0..B {
            set.set(B * 6 + i);
        }
        for i in (0..B).step_by(3) {
            set.set(B * 7 + i);
        }
        for i in 0..B {
            if i != 32768 {
                set.set(B * 8 + i);
            }
        }
        {
            let dir = new_directory(&mut random)?;
            set = do_test_all_single_jump(&mut random, set, &dir)?;
        }
        set.set(0);
        {
            let dir = new_directory(&mut random)?;
            let _ = do_test_all_single_jump(&mut random, set, &dir)?;
        }
        Ok(())
    }

    #[test]
    fn test_last_empty_blocks() -> Result<()> {
        let mut random = random();
        let dir = new_directory(&mut random)?;
        const B: usize = 65536;
        let max_doc = B * 3;
        let mut set = SparseFixedBitSet::new(max_doc)?;
        for i in 0..(B * 2) {
            set.set(i);
        }
        set = do_test_all_single_jump(&mut random, set, &dir)?;
        assert_advance_beyond_end(set, &dir)
    }

    fn assert_advance_beyond_end<B: BitSet>(set: B, dir: &impl Directory) -> Result<()> {
        let cardinality = set.cardinality();
        let dense_rank_power = 9;
        let mut out = dir.create_output("bar", &IOContext::default_io_context()?)?;
        let mut v = BitSetIterator::new(set, cardinality as i64)?;
        let jump_count =
            write_bitset_with_dense_rank_power(&mut v, &mut out, dense_rank_power)? as i32;
        let length = out.get_file_pointer();
        drop(out);

        let mut disi2 = BitSetIterator::new(v.bits, cardinality as i64)?;
        let mut doc = disi2.doc_id();
        let mut index = 0;
        while doc < cardinality as i32 {
            doc = disi2.next_doc()?;
            index += 1;
        }

        let input = dir.open_input("bar", &IOContext::default_io_context()?)?;
        let mut disi = IndexedDISI::new(
            &input,
            0,
            length,
            jump_count,
            dense_rank_power,
            cardinality as i64,
        )?;
        assert!(
            !disi.advance_exact(disi2.bits.length().try_convert()?)?,
            "There should be no set bit beyond the valid docID range"
        );
        disi.advance(doc)?;
        assert_eq!(
            index,
            disi.index_u() + 1,
            "The index when advancing beyond the last defined docID should be correct"
        );
        Ok(())
    }

    #[test]
    #[ignore]
    fn test_random_blocks() -> Result<()> {
        let mut random = random();
        let dir = new_directory(&mut random)?;
        let set = create_set_with_random_blocks(&mut random, 5)?;
        let _ = do_test_all_single_jump(&mut random, set, &dir)?;
        Ok(())
    }

    #[test]
    fn test_position_not_zero() -> Result<()> {
        let mut random = random();
        let dir = new_directory(&mut random)?;
        const BLOCKS: usize = 10;
        let dense_rank_power = if rarely(&mut random) {
            -1
        } else {
            (random.random_range(0..7) + 7) as i8
        };
        let set = create_set_with_random_blocks(&mut random, BLOCKS)?;
        let cardinality = set.cardinality();
        let mut out = dir.create_output("foo", &IOContext::default_io_context()?)?;
        let jump_table_entry_count = write_bitset_with_dense_rank_power(
            &mut BitSetIterator::new(set, cardinality as i64)?,
            &mut out,
            dense_rank_power,
        )? as i32;
        let length = out.get_file_pointer();
        drop(out);

        let full_input = dir.open_input("foo", &IOContext::default_io_context()?)?;
        test_position_not_zero_extra(
            &mut random,
            &full_input,
            dense_rank_power,
            length,
            jump_table_entry_count,
            cardinality as i64,
            BLOCKS as i32,
        )
    }
    fn test_position_not_zero_extra<I: IndexInput, R: Rng + ?Sized>(
        random: &mut R,
        full_input: &I,
        dense_rank_power: i8,
        length: usize,
        jump_table_entry_count: i32,
        cardinality: i64,
        blocks: i32,
    ) -> Result<()> {
        let mut block_data =
            create_block_slice(full_input, "blocks", 0, length, jump_table_entry_count)?;
        block_data.seek(random.random_range(0..block_data.length()))?;
        let jump_table = create_jump_table(full_input, 0, length, jump_table_entry_count)?;
        let mut disi: IndexedDISI<I, Owned> = IndexedDISI::from_components(
            block_data,
            jump_table,
            jump_table_entry_count,
            dense_rank_power,
            cardinality,
        )?;
        disi.advance_exact(blocks * 65536 - 1)?;
        Ok(())
    }

    fn create_set_with_random_blocks<R: Rng + ?Sized>(
        random: &mut R,
        block_count: usize,
    ) -> Result<SparseFixedBitSet> {
        const B: usize = 65536;
        let mut set = SparseFixedBitSet::new(block_count * B)?;
        for block in 0..block_count {
            match random.random_range(0..4) {
                0 => {},
                1 => {
                    for doc_id in (block * B)..((block + 1) * B) {
                        set.set(doc_id);
                    }
                },
                2 => {
                    for doc_id in (block * B..(block + 1) * B).step_by(101) {
                        set.set(doc_id);
                    }
                },
                3 => {
                    for doc_id in (block * B..(block + 1) * B).step_by(3) {
                        set.set(doc_id);
                    }
                },
                _ => unreachable!(),
            }
        }
        Ok(set)
    }

    fn do_test_all_single_jump<R: Rng + ?Sized, B: BitSet>(
        random: &mut R,
        set: B,
        dir: &impl Directory,
    ) -> Result<B> {
        let cardinality = set.cardinality();
        let dense_rank_power = if rarely(random) {
            -1
        } else {
            (random.random_range(0..7) + 7) as i8
        };
        let mut out = dir.create_output("foo", &IOContext::default_io_context()?)?;
        let mut v = BitSetIterator::new(set, cardinality as i64)?;
        let jump_table_entry_count =
            { write_bitset_with_dense_rank_power(&mut v, &mut out, dense_rank_power)? as i32 };

        let length = out.get_file_pointer();
        drop(out);

        let input = dir.open_input("foo", &IOContext::default_io_context()?)?;
        for i in 0..v.bits.length() {
            let mut disi = IndexedDISI::new(
                &input,
                0,
                length,
                jump_table_entry_count,
                dense_rank_power,
                cardinality as i64,
            )?;
            assert_eq!(v.bits.get(i)?, disi.advance_exact(i as i32)?);

            let mut disi2 = IndexedDISI::new(
                &input,
                0,
                length,
                jump_table_entry_count,
                dense_rank_power,
                cardinality as i64,
            )?;
            let doc = disi2.advance(i as i32)? as usize;
            assert!(i <= doc);
            if v.bits.get(i)? {
                assert_eq!(i, doc);
            } else {
                assert_ne!(i, doc);
            }
        }
        let set = v.bits;
        Ok(set)
    }
    #[test]
    fn test_one_doc() -> Result<()> {
        let mut random = random();
        let max_doc = TestUtil::next_usize(&mut random, 1, 100_000);
        let mut set = SparseFixedBitSet::new(max_doc)?;
        set.set(random.random_range(0..max_doc));
        let dir = new_directory(&mut random)?;
        let _ = do_test(set, &dir, &mut random)?;
        Ok(())
    }

    #[test]
    fn test_two_docs() -> Result<()> {
        let mut random = random();
        let max_doc = TestUtil::next_usize(&mut random, 1, 100_000);
        let mut set = SparseFixedBitSet::new(max_doc)?;
        set.set(random.random_range(0..max_doc));
        set.set(random.random_range(0..max_doc));
        let dir = new_directory(&mut random)?;
        let _ = do_test(set, &dir, &mut random)?;
        Ok(())
    }

    #[test]
    fn test_all_docs() -> Result<()> {
        let mut random = random();
        let max_doc = TestUtil::next_usize(&mut random, 1, 100_000);
        let mut set = FixedBitSet::new(max_doc);
        set.set_with_range(1, max_doc);
        let dir = new_directory(&mut random)?;
        let _ = do_test(set, &dir, &mut random)?;
        Ok(())
    }

    #[test]
    fn test_half_full() -> Result<()> {
        let mut random = random();
        let max_doc = TestUtil::next_usize(&mut random, 1, 100_000);
        let mut set = SparseFixedBitSet::new(max_doc)?;
        let mut i = random.random_range(0..2);
        while i < max_doc {
            set.set(i);
            i += TestUtil::next_usize(&mut random, 1, 3);
        }
        let dir = new_directory(&mut random)?;
        let _ = do_test(set, &dir, &mut random)?;
        Ok(())
    }

    #[test]
    fn test_doc_range() -> Result<()> {
        let mut random = random();
        let dir = new_directory(&mut random)?;

        for _ in 0..10 {
            let max_doc = TestUtil::next_usize(&mut random, 1, 1_000_000);
            let mut set = FixedBitSet::new(max_doc);
            let start = random.random_range(0..max_doc);
            let end = TestUtil::next_usize(&mut random, start + 1, max_doc);
            set.set_with_range(start, end);
            let _ = do_test(set, &dir, &mut random)?;
        }

        Ok(())
    }

    #[test]
    fn test_sparse_dense_boundary() -> Result<()> {
        let mut random = random();
        let dir = new_directory(&mut random)?;
        let mut set = FixedBitSet::new(200_000);
        let start = 65536 + random.random_range(0..100);
        let dense_rank_power = if rarely(&mut random) {
            -1
        } else {
            (random.random_range(0..7) + 7) as i8
        };

        set.set_with_range(start, start + MAX_ARRAY_LENGTH as usize);
        let mut out = dir.create_output("sparse", &IOContext::default_io_context()?)?;
        let mut v = BitSetIterator::new(set, MAX_ARRAY_LENGTH as i64)?;
        let jump_table_entry_count =
            { write_bitset_with_dense_rank_power(&mut v, &mut out, dense_rank_power)? as i32 };
        let length = out.get_file_pointer();
        drop(out);

        let mut set = v.bits;

        {
            let input = dir.open_input("sparse", &IOContext::default_io_context()?)?;
            let mut disi = IndexedDISI::new(
                &input,
                0,
                length,
                jump_table_entry_count,
                dense_rank_power,
                MAX_ARRAY_LENGTH as i64,
            )?;
            assert_eq!(start, disi.next_doc()? as usize);
            assert_eq!(Method::Sparse, disi.method);
        }

        set = do_test(set, &dir, &mut random)?;

        set.set(start + MAX_ARRAY_LENGTH as usize + random.random_range(0..100));
        let mut out = dir.create_output("bar", &IOContext::default_io_context()?)?;
        let mut v = BitSetIterator::new(set.clone(), (MAX_ARRAY_LENGTH + 1) as i64)?;
        write_bitset_with_dense_rank_power(&mut v, &mut out, dense_rank_power)?;
        let set = v.bits;
        let length = out.get_file_pointer();
        drop(out);

        {
            let input = dir.open_input("bar", &IOContext::default_io_context()?)?;
            let mut disi = IndexedDISI::new(
                &input,
                0,
                length,
                jump_table_entry_count,
                dense_rank_power,
                (MAX_ARRAY_LENGTH + 1) as i64,
            )?;
            assert_eq!(start, disi.next_doc()? as usize);
            assert_eq!(Method::Dense, disi.method);
        }

        let _ = do_test(set, &dir, &mut random)?;
        Ok(())
    }

    #[test]
    fn test_one_doc_missing() -> Result<()> {
        let mut random = random();
        let max_doc = TestUtil::next_usize(&mut random, 1, 1_000_000);
        let mut set = FixedBitSet::new(max_doc);
        set.set_with_range(0, max_doc);
        set.clear_with_index(random.random_range(0..max_doc));
        let dir = new_directory(&mut random)?;
        let _ = do_test(set, &dir, &mut random)?;
        Ok(())
    }

    #[test]
    fn test_few_missing_docs() -> Result<()> {
        let mut random = random();
        let dir = new_directory(&mut random)?;
        let num_iters = at_least(&mut random, 10);

        for _ in 0..num_iters {
            let max_doc = TestUtil::next_usize(&mut random, 1, 100_000);
            let mut set = FixedBitSet::new(max_doc);
            set.set_with_range(0, max_doc);
            let num_missing = TestUtil::next_int(&mut random, 2, 1000);
            for _ in 0..num_missing {
                set.clear_with_index(random.random_range(0..max_doc));
            }
            let _ = do_test(set, &dir, &mut random)?;
        }

        Ok(())
    }

    #[test]
    fn test_dense_multi_block() -> Result<()> {
        let mut random = random();
        let dir = new_directory(&mut random)?;
        let max_doc = 10 * 65536;
        let mut set = FixedBitSet::new(max_doc);
        for i in (0..max_doc).step_by(2) {
            set.set(i);
        }
        let _ = do_test(set, &dir, &mut random)?;
        Ok(())
    }

    #[test]
    fn test_illegal_dense_rank_power() -> Result<()> {
        for &power in &[-1, 7, 8, 9, 10, 11, 12, 13, 14, 15] {
            create_and_open_disi(power, power)?;
        }

        for &power in &[-2, 0, 1, 6, 16] {
            assert!(matches!(
                create_and_open_disi(power, 8),
                Err(LuceneError::IllegalArgument(_))
            ));

            assert!(matches!(
                create_and_open_disi(8, power),
                Err(LuceneError::IllegalArgument(_))
            ));
        }

        Ok(())
    }

    fn create_and_open_disi(write_power: i8, read_power: i8) -> Result<()> {
        let mut set = FixedBitSet::new(10);
        set.set(9);
        let mut random = random();
        let dir = new_directory(&mut random)?;
        let mut out = dir.create_output("foo", &IOContext::default_io_context()?)?;
        let mut v = BitSetIterator::new(set.clone(), set.cardinality() as i64)?;
        let jump_count = write_bitset_with_dense_rank_power(&mut v, &mut out, write_power)? as i32;
        let length = out.get_file_pointer();
        drop(out);

        let input = dir.open_input("foo", &IOContext::default_io_context()?)?;
        let _ = IndexedDISI::new(
            &input,
            0,
            length,
            jump_count,
            read_power,
            set.cardinality() as i64,
        )?;
        Ok(())
    }

    #[test]
    fn test_one_doc_missing_fixed() -> Result<()> {
        let mut random = random();
        let max_doc = 9699;
        let dense_rank_power = if rarely(&mut random) {
            -1
        } else {
            (random.random_range(0..7) + 7) as i8
        };

        let mut set = FixedBitSet::new(max_doc);
        set.set_with_range(0, max_doc);
        set.clear_with_index(1345);
        let cardinality = set.cardinality() as i64;

        let dir = new_directory(&mut random)?;
        let mut out = dir.create_output("foo", &IOContext::default_io_context()?)?;
        let mut v = BitSetIterator::new(set, cardinality)?;
        let jump_table_entry_count =
            write_bitset_with_dense_rank_power(&mut v, &mut out, dense_rank_power)? as i32;
        let length = out.get_file_pointer();
        drop(out);

        let mut disi2 = BitSetIterator::new(v.bits, cardinality)?;
        let input = dir.open_input("foo", &IOContext::default_io_context()?)?;
        let mut disi = IndexedDISI::new(
            &input,
            0,
            length,
            jump_table_entry_count,
            dense_rank_power,
            cardinality,
        )?;
        assert_advance_equality(&mut disi, &mut disi2, 16000)
    }

    #[test]
    fn test_random() -> Result<()> {
        let mut random = random();
        let dir = new_directory(&mut random)?;
        let num_iters = at_least(&mut random, 3);

        for _ in 0..num_iters {
            do_test_random(&dir, &mut random)?;
        }

        Ok(())
    }

    fn do_test_random<R: Rng + ?Sized>(dir: &impl Directory, random: &mut R) -> Result<()> {
        let end = TestUtil::next_int(random, 2, 20);
        let max_step = TestUtil::next_int(random, 1, 1 << end);
        let num_docs =
            TestUtil::next_int(random, 1, std::cmp::min(100_000, (i32::MAX - 1) / max_step));

        let mut docs = SparseFixedBitSet::new((num_docs * max_step + 1) as usize)?;
        let mut last_doc = -1;

        let mut doc = -1;
        for _ in 0..num_docs {
            doc += TestUtil::next_int(random, 1, max_step);
            docs.set(doc as usize);
            last_doc = doc;
        }

        let max_doc = last_doc + TestUtil::next_int(random, 1, 100);
        let cardinality = docs.approximate_cardinality();
        let mut bit_set_iterator = BitSetIterator::new(docs, cardinality as i64)?;
        let set = of(&mut bit_set_iterator, max_doc as usize)?;

        let _ = do_test(set, dir, random)?;
        Ok(())
    }

    fn do_test<R: Rng + ?Sized, B: BitSet>(
        set: B,
        dir: &impl Directory,
        random: &mut R,
    ) -> Result<B> {
        let cardinality = set.cardinality() as i64;
        let dense_rank_power = if rarely(random) {
            -1
        } else {
            (random.random_range(0..7) + 7) as i8
        };

        let length;
        let jump_table_entry_count;

        let mut set = {
            let mut out = dir.create_output("foo", &IOContext::default_io_context()?)?;
            let mut v = BitSetIterator::new(set, cardinality)?;
            jump_table_entry_count =
                write_bitset_with_dense_rank_power(&mut v, &mut out, dense_rank_power)? as i32;
            length = out.get_file_pointer();
            v.bits
        };

        set = {
            let input = dir.open_input("foo", &IOContext::default_io_context()?)?;
            let mut disi = IndexedDISI::new(
                &input,
                0,
                length,
                jump_table_entry_count,
                dense_rank_power,
                cardinality,
            )?;
            let mut disi2 = BitSetIterator::new(set, cardinality)?;
            assert_single_step_equality(&mut disi, &mut disi2)?;
            disi2.bits
        };

        for &step in &[1, 10, 100, 1000, 10000, 100000] {
            let input = dir.open_input("foo", &IOContext::default_io_context()?)?;
            let mut disi = IndexedDISI::new(
                &input,
                0,
                length,
                jump_table_entry_count,
                dense_rank_power,
                cardinality,
            )?;
            let mut disi2 = BitSetIterator::new(set, cardinality)?;
            assert_advance_equality(&mut disi, &mut disi2, step)?;
            set = disi2.bits
        }

        for &step in &[10, 100, 1000, 10000, 100000] {
            let input = dir.open_input("foo", &IOContext::default_io_context()?)?;
            let mut disi = IndexedDISI::new(
                &input,
                0,
                length,
                jump_table_entry_count,
                dense_rank_power,
                cardinality,
            )?;
            let disi2_length = set.length();
            let mut disi2 = BitSetIterator::new(set, cardinality)?;
            assert_advance_exact_randomized(
                random,
                &mut disi,
                &mut disi2,
                disi2_length as i32,
                step,
            )?;
            set = disi2.bits
        }

        dir.delete_file("foo")?;
        Ok(set)
    }

    fn assert_advance_exact_randomized<I: IndexInput, T: BitSet, R: Rng + ?Sized>(
        random: &mut R,
        disi: &mut IndexedDISI<I, Owned>,
        disi2: &mut BitSetIterator<T>,
        disi2_length: i32,
        step: i32,
    ) -> Result<()> {
        let mut index = -1;
        let mut target = 0;

        while target < disi2_length {
            target += TestUtil::next_int(random, 0, step);
            let mut doc = disi2.doc_id();
            while doc < target {
                doc = disi2.next_doc()?;
                index += 1;
            }

            let exists = disi.advance_exact(target)?;
            assert_eq!(doc == target, exists);
            if exists {
                assert_eq!(index, disi.index());
            } else if random.random_bool(0.5) {
                let advanced_doc = disi.next_doc()?;
                assert_eq!(doc, advanced_doc);
                // This is a bit strange when doc == NO_MORE_DOCS as the index
                // overcounts in the disi2 while-loop
                assert_eq!(index, disi.index());
                target = doc;
            }
        }

        Ok(())
    }
    fn assert_single_step_equality<I: IndexInput, T: BitSet>(
        disi: &mut IndexedDISI<I, Owned>,
        disi2: &mut BitSetIterator<T>,
    ) -> Result<()> {
        let mut i = 0;
        let mut doc = disi2.next_doc()?;

        while doc != NO_MORE_DOCS {
            assert_eq!(doc, disi.next_doc()?);
            assert_eq!(i, disi.index_u());
            i += 1;
            doc = disi2.next_doc()?;
        }

        assert_eq!(NO_MORE_DOCS, disi.next_doc()?);
        Ok(())
    }
    fn assert_advance_equality<I: IndexInput, T: BitSet>(
        disi: &mut IndexedDISI<I, Owned>,
        disi2: &mut BitSetIterator<T>,
        step: i32,
    ) -> Result<()> {
        let mut index = -1;

        loop {
            let target = disi2.doc_id() + step;
            let mut doc;

            loop {
                doc = disi2.next_doc()?;
                index += 1;
                if doc >= target {
                    break;
                }
            }

            let advanced = disi.advance(target)?;
            assert_eq!(doc, advanced);

            if doc == NO_MORE_DOCS {
                break;
            }

            assert_eq!(
                index,
                disi.index(),
                "Expected equality using step {} at docID {}",
                step,
                doc
            );
        }

        Ok(())
    }
}
