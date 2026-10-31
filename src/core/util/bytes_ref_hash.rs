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
use std::sync::{Arc, LazyLock};

use crate::core::index::{BytesRef, BytesRefBuilder};
use crate::core::util::access::ByteSource;
use crate::core::util::accountable::Accountable;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bytes_ref_block_pool::{BytesRefBlockPool, BytesRefBlockPoolPosition};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::most_significant_bit_radix_sort::LENGTH_THRESHOLD;
use crate::core::util::ram_usage_estimator::size_of_vec;
use crate::core::util::{
  AtomicCounter, ByteBlockPool, BytesRefComparator, Counter, GOOD_FAST_HASH_SEED, HISTOGRAM_SIZE,
  LEVEL_THRESHOLD, MSBRadixSorter, MSBRadixSorterBase, Natural, SharedCounter, Sorter,
  StringHelper, StringSorter, StringSorterBase,
};

/// [`BytesRefHash`] is a special purpose hash-map like data structure optimized
/// for [`BytesRef`] instances. [`BytesRefHash`] maintains mappings of byte arrays
/// to `i32` IDs, storing the hashed bytes efficiently in
/// continuous storage. The mapping to the ID is encapsulated inside
/// [`BytesRefHash`] and is guaranteed to be increased for each added [`BytesRef`].
///
/// # Note
/// - The maximum capacity [`BytesRef`] instance passed to
///   [`add`](BytesRefHash::add) must not be longer than
///   [`BYTE_BLOCK_SIZE`](crate::core::util::byte_block_pool::BYTE_BLOCK_SIZE) - 2.
/// - The internal storage is limited to 2GB total byte storage.
///
pub struct BytesRefHash<BSA> {
  pool: BytesRefBlockPool,
  hash_size: usize,
  hash_half_size: usize,
  hash_mask: usize,
  pub(crate) count: usize,
  last_count: Option<usize>,
  pub ids: Vec<i32>,
  pub(crate) bytes_start_array: BSA,
  bytes_used: SharedCounter,
}
impl BytesRefHash<DirectBytesStartArray> {
  pub fn new() -> Result<Self> {
    let bytes_start_array = DirectBytesStartArray::new(DEFAULT_CAPACITY);
    BytesRefHash::from_bytes_start_array(16, bytes_start_array)
  }
}
// The process-wide hash seed never changes. Cache the same MurmurHash values
// for every possible one-byte key without adding state to individual hashes.
static SINGLE_BYTE_HASHES: LazyLock<[i32; 256]> = LazyLock::new(|| {
  let seed = *GOOD_FAST_HASH_SEED;
  std::array::from_fn(|byte| StringHelper::murmurhash3_x86_32_with_byte(&[byte as u8], 0, 1, seed))
});

pub fn do_hash(bytes: &[u8], offset: usize, length: usize) -> i32 {
  if length == 1 {
    SINGLE_BYTE_HASHES[bytes[offset] as usize]
  } else {
    StringHelper::murmurhash3_x86_32_with_byte(bytes, offset, length, *GOOD_FAST_HASH_SEED)
  }
}
impl<BSA> BytesRefHash<BSA>
where
  BSA: BytesStartArray,
{
  pub fn from_bytes_start_array(capacity: usize, mut bytes_start_array: BSA) -> Result<Self> {
    bytes_start_array.init()?;
    let bytes_used = bytes_start_array.bytes_used();
    let ref_pool = BytesRefBlockPool::new();
    let ids = vec![-1; capacity];
    bytes_used.add_and_get(size_of_vec(&ids));
    Ok(BytesRefHash {
      pool: ref_pool,
      hash_size: capacity,
      hash_half_size: capacity >> 1,
      hash_mask: capacity - 1,
      count: 0,
      last_count: None,
      ids,
      bytes_start_array,
      bytes_used,
    })
  }
  /// Returns the number of [`BytesRef`] values in this [`BytesRefHash`].
  ///
  /// # Returns
  /// The number of [`BytesRef`] values in this [`BytesRefHash`].
  pub fn size(&self) -> usize {
    self.count
  }
  /// Returns the pool position of the bytes for the given `bytes_id` without
  /// copying them.
  ///
  /// # Note
  /// The given `bytes_id` must be a non-negative integer less than the current
  /// size (`size()`).
  ///
  /// # Arguments
  /// - `bytes_id`: The ID.
  /// - `pool`: The same byte pool used to add the value.
  ///
  /// # Returns
  /// A position in `pool`. Borrow its bytes while reading, or copy them when
  /// independent ownership is needed. The position does not keep the pool alive
  /// and must not be reused after its contents are reset or changed.
  pub fn get(&self, bytes_id: i32, pool: &ByteBlockPool) -> Result<BytesRefBlockPoolPosition> {
    debug_assert!(
      !self.bytes_start_array.need_init(),
      "bytes_start is null - not initialized"
    );
    let bytes_start_len = self.bytes_start_array.len()?;
    let bytes_id = bytes_id as usize;
    debug_assert!(
      bytes_id < bytes_start_len,
      "bytesID exceeds bytes_start len"
    );
    let value = self.bytes_start_array.get_value(bytes_id)?;
    Ok(self.pool.fill_bytes_ref(value, pool))
  }

  /// Returns the id array in arbitrary order. Valid ids start at offset 0 and
  /// end at a limit of `size()` - 1.
  ///
  /// # Note
  /// This is a destructive operation. `Clear()` must be called to reuse this
  /// [`BytesRefHash`] instance.
  pub fn compact(&mut self) -> Result<&Vec<i32>> {
    debug_assert!(
      !self.bytes_start_array.need_init(),
      "bytes_start is null - not initialized"
    );

    let mut upto = 0;
    if let Some(ids) = self.ids.get_mut(..self.hash_size) {
      for i in 0..ids.len() {
        if ids[i] != -1 {
          if upto < i {
            ids[upto] = ids[i];
            ids[i] = -1;
          }
          upto += 1;
        }
      }
    } else {
      for i in 0..self.hash_size {
        if self.ids[i] != -1 {
          if upto < i {
            self.ids[upto] = self.ids[i];
            self.ids[i] = -1;
          }
          upto += 1;
        }
      }
    }
    debug_assert!(upto == self.count);
    self.last_count = Some(self.count);

    Ok(&self.ids)
  }
  /// Returns the values array sorted by the referenced byte values.
  pub fn sort(&mut self, byte_block_pool: &ByteBlockPool) -> Result<()> {
    let compact = self.compact()?;
    let mut length = compact.len();
    debug_assert!(
      self.count * 2 <= length,
      "We need load factor <= 0.5f to speed up this sort"
    );
    let tmp_offset = self.count;
    let sub_sorter = StringSorterImpl::new(
      tmp_offset,
      &mut self.ids,
      &mut self.pool,
      byte_block_pool,
      &self.bytes_start_array,
    );
    let mut sorter = StringSorter::new(sub_sorter, Natural::default());
    sorter.sort(0, tmp_offset)?;

    length = self.ids.len();
    for i in tmp_offset..length {
      self.ids[i] = -1;
    }
    Ok(())
  }
  fn shrink(&mut self, target_size: usize) -> bool {
    // Cannot use ArrayUtil.shrink because we require power of 2:
    let mut new_size = self.hash_size;

    while new_size >= 8 && new_size / 4 > target_size {
      new_size /= 2;
    }

    if new_size != self.hash_size {
      let old_size = size_of_vec(&self.ids);
      self.hash_size = new_size;
      self.ids = vec![-1; self.hash_size];
      self
        .bytes_used
        .add_and_get(size_of_vec(&self.ids).saturating_sub(old_size));
      self.hash_half_size = new_size / 2;
      self.hash_mask = new_size - 1;
      true
    } else {
      false
    }
  }
  /// Clears the [`BytesRef`] which maps to the given [`BytesRef`].
  pub fn clear_with_reset_pool(&mut self, reset_pool: bool, byte_block_pool: &mut ByteBlockPool) {
    self.last_count = Some(self.count);
    self.count = 0;

    if reset_pool {
      self.pool.reset(byte_block_pool);
    }

    self.bytes_start_array.clear();

    if self.last_count.is_some_and(|count| self.shrink(count)) {
      // shrink clears the hash entries
      return;
    }
    self.ids.fill(-1);
  }
  pub fn clear(&mut self, byte_block_pool: &mut ByteBlockPool) {
    self.clear_with_reset_pool(true, byte_block_pool)
  }

  /// Closes the [`BytesRefHash`] and releases all internally used memory.
  pub fn close(&mut self, byte_block_pool: &mut ByteBlockPool) {
    self.clear_with_reset_pool(true, byte_block_pool);
    self.bytes_used.add_and_get(-size_of_vec(&self.ids));
    self.ids = vec![];
  }
  /// Adds a new [`BytesRef`].
  ///
  /// # Arguments
  /// - `bytes`: The bytes to hash.
  ///
  /// # Returns
  /// The id the given bytes are hashed to if there was no mapping for the
  /// given bytes, otherwise `(-(id) - 1)`. This guarantees that the
  /// return value will always be >= 0 if the given bytes haven't been
  /// hashed before.
  ///
  /// # Errors
  /// Returns
  /// [`LuceneErrorKind::MaxBytesLengthExceeded`]
  /// if the given bytes are greater than 2 +
  /// [`BYTE_BLOCK_SIZE`](crate::core::util::byte_block_pool::BYTE_BLOCK_SIZE).
  pub fn add<BS>(
    &mut self,
    bytes: &BytesRef<BS>,
    byte_block_pool: &mut ByteBlockPool,
  ) -> Result<i32>
  where
    BS: ByteSource,
  {
    debug_assert!(
      !self.bytes_start_array.need_init(),
      "Bytesstart is null - not initialized"
    );

    // final position
    let hash_pos = self.find_hash(bytes, byte_block_pool)?;
    let mut e = self.ids[hash_pos];
    if e == -1 {
      {
        let length = self.bytes_start_array.len()?;
        let index = self.count;
        // new entry
        if index >= length {
          self.bytes_start_array.grow()?;
          let grown_length = self.bytes_start_array.len()?;
          debug_assert!(
            index < grown_length + 1,
            "count: {} len: {}",
            self.count,
            grown_length
          );
        }

        let v = self.pool.add_bytes_ref(bytes, byte_block_pool)?;
        self.bytes_start_array.set_value(index, v)?;
        e = self.count as i32;
        self.count += 1;
        debug_assert_eq!(self.ids[hash_pos], -1);
        self.ids[hash_pos] = e;
      }

      if self.count == self.hash_half_size {
        self.rehash(2 * self.hash_size, true, byte_block_pool)?;
      }

      return Ok(e);
    }
    Ok(-(e + 1))
  }
  /// Returns the id of the given [`BytesRef`].
  ///
  /// # Arguments
  /// - `bytes`: The [`BytesRef`] to look for.
  ///
  /// # Returns
  /// The id of the given bytes, or `-1` if there is no mapping for the given
  /// bytes.
  pub fn find(&self, bytes: &BytesRef<Vec<u8>>, byte_block_pool: &ByteBlockPool) -> Result<i32> {
    Ok(self.ids[self.find_hash(bytes, byte_block_pool)?])
  }
  fn find_hash<BS>(&self, bytes: &BytesRef<BS>, byte_block_pool: &ByteBlockPool) -> Result<usize>
  where
    BS: ByteSource,
  {
    debug_assert!(
      !self.bytes_start_array.need_init(),
      "bytesStart is null - not initialized"
    );

    let bytes_slice = bytes.as_byte_slice();
    let mut code = do_hash(bytes_slice, 0, bytes_slice.len());

    // final position
    let mut hash_pos = (code as usize) & self.hash_mask;
    let mut e = self.ids[hash_pos];
    if e != -1
      && !self.pool.equals(
        self.bytes_start_array.get_value(e as usize)?,
        bytes,
        byte_block_pool,
      )
    {
      loop {
        code = code.wrapping_add(1);
        hash_pos = (code as usize) & self.hash_mask;
        e = self.ids[hash_pos];
        if e == -1
          || self.pool.equals(
            self.bytes_start_array.get_value(e as usize)?,
            bytes,
            byte_block_pool,
          )
        {
          break;
        }
      }
    }
    Ok(hash_pos)
  }
  /// Adds an "arbitrary" integer offset instead of a [`BytesRef`] term.
  ///
  /// This is used in the indexer to hold the hash for term vectors, because
  /// they do not redundantly store the term bytes directly and instead
  /// reference the term bytes already stored by the postings
  /// [`BytesRefHash`].
  pub fn add_by_pool_offset(
    &mut self,
    offset: i32,
    byte_block_pool: &mut ByteBlockPool,
  ) -> Result<i32> {
    debug_assert!(
      !self.bytes_start_array.need_init(),
      "Bytesstart is null - not initialized"
    );

    // Final position
    let mut code = offset;
    let mut hash_pos = (offset as usize) & self.hash_mask;
    let mut e = self.ids[hash_pos];
    let length = self.bytes_start_array.len()?;
    // Resolve hash conflicts
    while e != -1 && self.bytes_start_array.get_value(e as usize)? != offset {
      code = code.wrapping_add(1);
      hash_pos = (code as usize) & self.hash_mask;
      e = self.ids[hash_pos];
    }

    if e == -1 {
      // New entry
      let index = self.count;
      if index >= length {
        self.bytes_start_array.grow()?;
        let grown_length = self.bytes_start_array.len()?;
        debug_assert!(
          self.count < grown_length + 1,
          "count: {}, len: {}",
          self.count,
          grown_length
        );
      }

      e = self.count as i32;
      self.count += 1;
      self.bytes_start_array.set_value(index, offset)?;

      debug_assert_eq!(self.ids[hash_pos], -1);
      self.ids[hash_pos] = e;

      if self.count == self.hash_half_size {
        self.rehash(2 * self.hash_size, false, byte_block_pool)?;
      }

      return Ok(e);
    }

    Ok(-(e + 1))
  }
  /// Called when hash is too small (> 50% occupied) or too large (< 20%
  /// occupied).
  fn rehash(
    &mut self,
    new_size: usize,
    hash_on_data: bool,
    byte_block_pool: &mut ByteBlockPool,
  ) -> Result<()> {
    let new_mask = new_size - 1;
    let old_size = size_of_vec(&self.ids);
    let mut new_hash = vec![-1; new_size];
    for i in 0..self.hash_size {
      let e0 = self.ids[i];
      if e0 != -1 {
        let mut code = if hash_on_data {
          self.pool.hash(
            self.bytes_start_array.get_value(e0 as usize)?,
            byte_block_pool,
          )
        } else {
          self.bytes_start_array.get_value(e0 as usize)?
        };

        let mut hash_pos = (code as usize) & new_mask;
        if new_hash[hash_pos] != -1 {
          loop {
            code = code.wrapping_add(1);
            hash_pos = (code as usize) & new_mask;
            if new_hash[hash_pos] == -1 {
              break;
            }
          }
        }
        new_hash[hash_pos] = e0;
      }
    }
    self.hash_mask = new_mask;
    self
      .bytes_used
      .add_and_get(size_of_vec(&new_hash).saturating_sub(old_size));
    self.ids = new_hash;
    self.hash_size = new_size;
    self.hash_half_size = new_size / 2;
    Ok(())
  }

  /// Reinitializes the [`BytesRefHash`] after a previous `clear()` call.
  /// If `clear()` has not been called previously, this method has no effect.
  pub fn reinit(&mut self) -> Result<()> {
    if self.bytes_start_array.need_init() {
      self.bytes_start_array.init()?;
    }

    if self.ids.is_empty() {
      self.ids = vec![-1; self.hash_size];
      self.bytes_used.add_and_get(size_of_vec(&self.ids));
    }
    Ok(())
  }
  /// Returns the `bytesStart` offset into the internally used
  /// `SingleThreadedByteBlockPool` for the given `bytes_id`.
  ///
  /// # Arguments
  /// * `bytes_id` - The ID to look up.
  ///
  /// # Returns
  /// The `bytesStart` offset into the internally used
  /// `SingleThreadedByteBlockPool` for the given ID.
  pub fn byte_start(&self, bytes_id: i32) -> Result<i32> {
    debug_assert!(
      !self.bytes_start_array.need_init(),
      "bytes_start is null - not initialized"
    );
    let bytes_index = bytes_id as usize;
    debug_assert!(bytes_id >= 0 && bytes_index < self.count);
    self.bytes_start_array.get_value(bytes_index)
  }

  /// Returns the retained heap used by this hash and the byte pool that stores
  /// its terms.
  ///
  /// The [`ByteBlockPool`] is passed separately because the Rust port keeps it
  /// outside [`BytesRefHash`]. Callers should only use this when this hash is the
  /// accounting owner of `byte_block_pool`.
  pub fn ram_bytes_used_with_pool(&self, byte_block_pool: &ByteBlockPool) -> Result<i64> {
    Ok(
      self
        .ram_bytes_used()?
        .saturating_add(byte_block_pool.ram_bytes_used()?),
    )
  }
}
impl<BSA> Accountable for BytesRefHash<BSA>
where
  BSA: BytesStartArray,
{
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(
      self
        .pool
        .ram_bytes_used()?
        .saturating_add(size_of_vec(&self.ids))
        .saturating_add(self.bytes_start_array.ram_bytes_used()?),
    )
  }
}

pub(crate) struct StringSorterImpl<'a, BSA> {
  tmp_offset: usize,
  compact: &'a mut Vec<i32>,
  pool: &'a mut BytesRefBlockPool,
  byte_block_pool: &'a ByteBlockPool,
  bytes_start_array: &'a BSA,
  k: usize,
  cmp: Natural,
  scratch_bytes: BytesRef<&'a [u8]>,
}
impl<'a, BSA> StringSorterImpl<'a, BSA>
where
  BSA: BytesStartArray,
{
  pub fn new(
    tmp_offset: usize,
    compact: &'a mut Vec<i32>,
    pool: &'a mut BytesRefBlockPool,
    byte_block_pool: &'a ByteBlockPool,
    bytes_start_array: &'a BSA,
  ) -> Self {
    StringSorterImpl {
      tmp_offset,
      compact,
      pool,
      byte_block_pool,
      bytes_start_array,
      k: 0,
      cmp: Natural::default(),
      scratch_bytes: BytesRef::default(),
    }
  }
  fn swap_bucket_cache(&mut self, i: usize, j: usize) -> Result<()> {
    self.swap(i, j)?;
    self.compact.swap(self.tmp_offset + i, self.tmp_offset + j);
    Ok(())
  }
}
impl<BSA> MSBRadixSorterBase for StringSorterImpl<'_, BSA>
where
  BSA: BytesStartArray,
{
  fn byte_at(&mut self, i: usize, k: usize) -> Result<i32> {
    let start = self.bytes_start_array.get_value(self.compact[i] as usize)?;
    let position = self.pool.fill_bytes_ref(start, self.byte_block_pool);
    let block = self.byte_block_pool.get_buffer(position.block_index);
    self.scratch_bytes.bytes = &block[position.offset..position.offset + position.length];
    self.scratch_bytes.offset = 0;
    self.scratch_bytes.length = position.length;
    self.cmp.byte_at(&self.scratch_bytes, k)
  }

  fn reorder(
    &mut self,
    from: usize,
    _to: usize,
    start_offsets: &mut [usize],
    end_offsets: &mut [usize],
    k: usize,
  ) -> Result<()> {
    debug_assert_eq!(self.k, k);
    for i in 0..HISTOGRAM_SIZE {
      let limit = end_offsets[i];
      loop {
        let h1 = start_offsets[i];
        if h1 >= limit {
          break;
        }

        let idx = self.tmp_offset + from + h1;
        let b = self.compact[idx] as usize;

        let h2 = start_offsets[b];
        start_offsets[b] += 1;

        self.swap_bucket_cache(from + h1, from + h2)?;
      }
    }
    Ok(())
  }

  fn build_histogram(
    &mut self,
    prefix_common_bucket: usize,
    prefix_common_len: usize,
    from: usize,
    to: usize,
    k: usize,
    histogram: &mut [usize],
  ) -> Result<()> {
    self.k = k;
    histogram[prefix_common_bucket] = prefix_common_len;
    self.compact[(self.tmp_offset + from - prefix_common_len)..(self.tmp_offset + from)]
      .fill(prefix_common_bucket as i32);
    for i in from..to {
      let b = self.get_bucket(i, k)?;
      self.compact[self.tmp_offset + i] = b as i32;
      histogram[b] += 1;
    }
    Ok(())
  }

  fn should_fallback(&self, from: usize, to: usize, l: usize) -> bool {
    // We lower the fallback threshold because the bucket cache speeds up
    // the reorder
    to - from <= (LENGTH_THRESHOLD / 2) || l >= LEVEL_THRESHOLD
  }
}
impl<BSA> Sorter for StringSorterImpl<'_, BSA>
where
  BSA: BytesStartArray,
{
  fn swap(&mut self, i: usize, j: usize) -> Result<()> {
    self.compact.swap(i, j);
    Ok(())
  }
}
impl<'a, BSA> StringSorterBase for StringSorterImpl<'a, BSA>
where
  BSA: BytesStartArray,
{
  type Bytes = &'a [u8];

  fn get(
    &mut self,
    _builder: &mut BytesRefBuilder<Vec<u8>>,
    result: &mut BytesRef<Self::Bytes>,
    i: usize,
  ) -> Result<()> {
    let start = self.bytes_start_array.get_value(self.compact[i] as usize)?;
    let position = self.pool.fill_bytes_ref(start, self.byte_block_pool);
    let block = self.byte_block_pool.get_buffer(position.block_index);
    result.bytes = &block[position.offset..position.offset + position.length];
    result.offset = 0;
    result.length = position.length;
    Ok(())
  }

  fn radix_sorter<'b, C1>(&'b mut self, cmp: &'b mut C1) -> impl Sorter + 'b
  where
    C1: BytesRefComparator<Self::Bytes>,
    Self: Sorter + Sized,
  {
    let length = cmp.compared_bytes_count();
    let delegate = MSBStringHashRadixSorter::new(cmp, self);
    MSBRadixSorter::new(length, delegate)
  }
}

/// Manages allocation of the per-term addresses.
#[allow(clippy::len_without_is_empty)]
pub trait BytesStartArray {
  /// Initializes the BytesStartArray. This call will allocate memory.
  ///
  /// # Returns
  /// The initialized bytes start array.
  fn init(&mut self) -> Result<()>;

  /// Grows the [`BytesStartArray`].
  ///
  /// # Returns
  /// The grown array.
  fn grow(&mut self) -> Result<()>;

  /// Clears the [`BytesStartArray`] and returns the cleared instance.
  ///
  /// # Returns
  /// The cleared instance, this might be `None`.
  fn clear(&mut self);

  /// A reference holding the number of bytes used by this [`BytesStartArray`].
  /// The [`BytesRefHash`] uses this reference to track its memory usage.
  ///
  /// # Returns
  /// A reference holding the number of bytes used by this [`BytesStartArray`].
  fn bytes_used(&mut self) -> SharedCounter;
  fn get_value(&self, index: usize) -> Result<i32>;
  fn set_value(&mut self, index: usize, value: i32) -> Result<()>;
  fn len(&self) -> Result<usize>;
  fn need_init(&self) -> bool;
  fn ram_bytes_used(&self) -> Result<i64>;
}
/// A simple [`BytesStartArray`] that tracks memory allocation using a private
/// [`Counter`] instance.
pub struct DirectBytesStartArray {
  init_size: usize,
  pub(crate) bytes_start: Vec<i32>,
  bytes_used: SharedCounter,
  init: bool,
}
impl DirectBytesStartArray {
  pub fn new(init_size: usize) -> Self {
    DirectBytesStartArray::with_counter(init_size, Arc::new(AtomicCounter::new()))
  }
  pub fn with_counter(init_size: usize, counter: SharedCounter) -> Self {
    DirectBytesStartArray {
      init_size,
      bytes_start: vec![],
      bytes_used: counter,
      init: false,
    }
  }
}

impl BytesStartArray for DirectBytesStartArray {
  fn init(&mut self) -> Result<()> {
    self.bytes_start = vec![0; ArrayUtil::oversize(self.init_size, BitUtil::INT_BYTES)?];
    self.init = true;
    Ok(())
  }

  fn grow(&mut self) -> Result<()> {
    debug_assert!(self.init);
    let length = self.bytes_start.len();
    ArrayUtil::grow_with(&mut self.bytes_start, length + 1, || 0)?;
    Ok(())
  }

  fn clear(&mut self) {
    self.init = false;
    self.bytes_start = vec![];
  }

  fn bytes_used(&mut self) -> SharedCounter {
    self.bytes_used.clone()
  }

  fn get_value(&self, index: usize) -> Result<i32> {
    self.bytes_start.get(index).copied().ok_or_else(|| {
      LuceneError::illegal_argument(format!("bytes start index {index} is out of bounds"))
    })
  }

  fn set_value(&mut self, index: usize, value: i32) -> Result<()> {
    let slot = self.bytes_start.get_mut(index).ok_or_else(|| {
      LuceneError::illegal_argument(format!("bytes start index {index} is out of bounds"))
    })?;
    *slot = value;
    Ok(())
  }

  fn len(&self) -> Result<usize> {
    Ok(self.bytes_start.len())
  }

  fn need_init(&self) -> bool {
    !self.init
  }

  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(size_of_vec(&self.bytes_start))
  }
}

pub struct MSBStringHashRadixSorter<'a, T: StringSorterBase, C> {
  scratch1: BytesRefBuilder<Vec<u8>>,
  scratch2: BytesRefBuilder<Vec<u8>>,
  pivot_builder: BytesRefBuilder<Vec<u8>>,
  scratch_bytes1: BytesRef<T::Bytes>,
  scratch_bytes2: BytesRef<T::Bytes>,
  pivot: BytesRef<T::Bytes>,
  cmp: &'a mut C,
  delegate: &'a mut T,
}
impl<'a, T, C> MSBStringHashRadixSorter<'a, T, C>
where
  T: StringSorterBase,
  C: BytesRefComparator<T::Bytes>,
{
  pub fn new(cmp: &'a mut C, delegate: &'a mut T) -> MSBStringHashRadixSorter<'a, T, C> {
    MSBStringHashRadixSorter {
      scratch1: BytesRefBuilder::default(),
      scratch2: BytesRefBuilder::default(),
      pivot_builder: BytesRefBuilder::default(),
      scratch_bytes1: BytesRef::default(),
      scratch_bytes2: BytesRef::default(),
      pivot: BytesRef::default(),
      cmp,
      delegate,
    }
  }
}

impl<T, C> Sorter for MSBStringHashRadixSorter<'_, T, C>
where
  T: StringSorterBase + MSBRadixSorterBase,
  C: BytesRefComparator<T::Bytes>,
{
  fn swap(&mut self, i: usize, j: usize) -> Result<()> {
    self.delegate.swap(i, j)
  }
}

impl<T, C> MSBRadixSorterBase for MSBStringHashRadixSorter<'_, T, C>
where
  T: StringSorterBase + MSBRadixSorterBase,
  C: BytesRefComparator<T::Bytes>,
{
  fn byte_at(&mut self, i: usize, k: usize) -> Result<i32> {
    self.delegate.byte_at(i, k)
  }

  fn get_fallback_sorter(&mut self, k: usize, _length: usize) -> impl Sorter {
    self.delegate.fall_back_sorter(
      self.cmp,
      Some(k),
      &mut self.scratch1,
      &mut self.scratch2,
      &mut self.pivot_builder,
      &mut self.scratch_bytes1,
      &mut self.scratch_bytes2,
      &mut self.pivot,
    )
  }

  fn reorder(
    &mut self,
    from: usize,
    to: usize,
    start_offsets: &mut [usize],
    end_offsets: &mut [usize],
    k: usize,
  ) -> Result<()> {
    self
      .delegate
      .reorder(from, to, start_offsets, end_offsets, k)
  }

  fn build_histogram(
    &mut self,
    prefix_common_bucket: usize,
    prefix_common_len: usize,
    from: usize,
    to: usize,
    k: usize,
    histogram: &mut [usize],
  ) -> Result<()> {
    self.delegate.build_histogram(
      prefix_common_bucket,
      prefix_common_len,
      from,
      to,
      k,
      histogram,
    )
  }

  fn should_fallback(&self, from: usize, to: usize, l: usize) -> bool {
    self.delegate.should_fallback(from, to, l)
  }
}

pub const DEFAULT_CAPACITY: usize = 16;
pub(crate) type DirectBytesRefHash = BytesRefHash<DirectBytesStartArray>;
