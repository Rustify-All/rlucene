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
use std::borrow::{Borrow, Cow};

use crate::core::index::{BytesRef, BytesRefBuilder, BytesRefValueEnum};
use crate::core::util::access::ByteSource;
use crate::core::util::accountable::Accountable;
use crate::core::util::allocator_byte::DirectTrackingAllocatorByte;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::byte_block_pool::{BYTE_BLOCK_MASK, BYTE_BLOCK_SHIFT, BYTE_BLOCK_SIZE};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::ram_usage_estimator::size_of_array_capacity;
use crate::core::util::sortable_bytes_ref_array::SortableBytesRefArray;
use crate::core::util::{
  ByteBlockPool, BytesRefComparator, Counter, MSBRadixSorterBase, SharedCounter, SliceCopyOps,
  Sorter, StableStringSorter, StableStringSorterBase, StringSorter, StringSorterBase, TryIntoInt,
};

/// A simple append-only random-access array that stores full copies of the
/// appended bytes in a [`ByteBlockPool`].
///
/// # Note
/// This struct is **not thread-safe!**
///
/// # Internal
/// This is an internal and experimental component.
#[derive(Debug)]
pub struct BytesRefArray {
  storage: ByteStorage,
  offsets: smallvec::SmallVec<[usize; 1]>,
  last_element: usize,
  current_offset: usize,
  byte_used: SharedCounter,
}

#[derive(Debug)]
enum ByteStorage {
  Blocks(ByteBlockPool),
  Compact(Vec<u8>),
}

impl BytesRefArray {
  pub fn new(byte_used: SharedCounter) -> Result<BytesRefArray> {
    let allocator = DirectTrackingAllocatorByte::new(byte_used.clone());
    let storage = ByteStorage::Blocks(ByteBlockPool::new(allocator));
    Ok(Self::with_storage(byte_used, storage))
  }

  pub(crate) fn new_compact(byte_used: SharedCounter) -> Self {
    Self::with_storage(byte_used, ByteStorage::Compact(Vec::new()))
  }

  fn with_storage(byte_used: SharedCounter, storage: ByteStorage) -> Self {
    let offsets = smallvec::SmallVec::with_capacity(1);
    byte_used.add_and_get(size_of_array_capacity::<usize>(offsets.capacity()));
    Self {
      storage,
      offsets,
      last_element: 0,
      current_offset: 0,
      byte_used,
    }
  }
  /// Returns the nth element of this [`BytesRefArray`].
  ///
  /// The returned bytes borrow the pool when the value is contained in one
  /// block. Values spanning blocks own a copy.
  ///
  /// # Parameters
  /// - `index`: The index of the element to retrieve.
  ///
  /// # Returns
  /// The nth element of this [`BytesRefArray`] as a [`BytesRefValueEnum`].
  ///
  /// # Errors
  /// Returns [`LuceneError::array_index_out_of_bounds`] if the index is
  /// invalid.
  pub fn get(&self, index: usize) -> Result<BytesRefValueEnum<'_>> {
    if index >= self.last_element {
      return Err(LuceneError::array_index_out_of_bounds(format!(
        "index: {}, last_element: {}",
        index, self.last_element
      )));
    }

    let offset = self.offsets[index];
    let length = if index == self.last_element - 1 {
      self.current_offset - offset
    } else {
      self.offsets[index + 1] - offset
    };

    if length == 0 {
      return Ok(BytesRefValueEnum::Slice(BytesRef {
        bytes: &[],
        offset: 0,
        length: 0,
      }));
    }
    let pool = match &self.storage {
      ByteStorage::Compact(bytes) => {
        return Ok(BytesRefValueEnum::Slice(BytesRef {
          bytes: &bytes[offset..offset + length],
          offset: 0,
          length,
        }));
      },
      ByteStorage::Blocks(pool) => pool,
    };
    let block_index = offset >> BYTE_BLOCK_SHIFT;
    let block_offset = offset & BYTE_BLOCK_MASK as usize;
    if length <= BYTE_BLOCK_SIZE as usize - block_offset {
      return Ok(BytesRefValueEnum::Slice(BytesRef {
        bytes: &pool.get_buffer(block_index)[block_offset..block_offset + length],
        offset: 0,
        length,
      }));
    }
    // Keep Lucene's length validation, but this owned snapshot will not grow.
    ArrayUtil::oversize(length, size_of::<u8>())?;
    let mut bytes = Vec::with_capacity(length);
    let buffer_index: i32 = ((offset as i64) >> BYTE_BLOCK_SHIFT).try_convert()?;
    let mut buffer_index = buffer_index as usize;
    let mut pos = (offset as i64 & BYTE_BLOCK_MASK as i64) as usize;
    let mut remaining = length;
    while remaining > 0 {
      let chunk = std::cmp::min(BYTE_BLOCK_SIZE as usize - pos, remaining);
      bytes.extend_from_slice(&pool.get_buffer(buffer_index)[pos..pos + chunk]);
      remaining -= chunk;
      buffer_index += 1;
      pos = 0;
    }
    Ok(BytesRefValueEnum::Buffer(Cow::Owned(BytesRef {
      bytes,
      offset: 0,
      length,
    })))
  }

  /// Used only by the sorting function below to set a [`BytesRef`] with the
  /// specified slice, avoiding copying bytes in the common case when the
  /// slice is contained in a single block in the byte block pool.
  #[allow(
    clippy::owned_cow,
    reason = "Matches ByteBlockPool's Vec carrier, which preserves reusable cross-block capacity"
  )]
  fn set_bytes_ref<'a>(
    &'a self,
    spare: &mut BytesRefBuilder<Vec<u8>>,
    result: &mut BytesRef<Cow<'a, Vec<u8>>>,
    index: usize,
  ) -> Result<()> {
    if index >= self.last_element {
      return Err(LuceneError::array_index_out_of_bounds(format!(
        "index: {}, last_element: {}",
        index, self.last_element
      )));
    }

    let offset = self.offsets[index];
    let length = if index == self.last_element - 1 {
      self.current_offset - offset
    } else {
      self.offsets[index + 1] - offset
    };

    match &self.storage {
      ByteStorage::Compact(bytes) => {
        result.bytes = Cow::Borrowed(bytes);
        result.offset = offset;
        result.length = length;
      },
      ByteStorage::Blocks(pool) => pool.set_bytes_ref(spare, result, offset as i64, length)?,
    }
    Ok(())
  }

  /// Returns a [`SortState`] representing the order of elements in this
  /// array. This is a non-destructive operation.
  ///
  /// # Parameters
  /// - `comp`: The comparator to compare [`BytesRef`]s. A radix sort
  ///   optimization is available if the comparator implements
  ///   [`BytesRefComparator`].
  /// - `stable`: Indicates if the sort needs to be stable.
  ///
  /// # Returns
  /// A [`SortState`] that can be used in
  /// [`BytesRefArray::iterator_with_state`] with the given sort state.
  pub fn sort<'a, C>(&'a self, comp: C, stable: bool) -> Result<SortState>
  where
    C: BytesRefComparator<Cow<'a, Vec<u8>>>,
  {
    let size = self.size();
    let mut indices = (size > 1).then(|| (0..size).collect::<smallvec::SmallVec<_>>());
    let mut identity = [0];
    let ordered_entries = indices
      .as_deref_mut()
      .unwrap_or(&mut identity[..size.min(1)]);
    if stable {
      let delegate = StableStringSorterImpl {
        tmp: Vec::new(),
        ordered_entries,
        bytes_ref_array: self,
      };
      let stable_string_sorter = StableStringSorter::new(delegate);
      let mut string_sorter = StringSorter::new(stable_string_sorter, comp);
      string_sorter.sort(0, size)?;
    } else {
      let delegate = StringSorterImpl {
        ordered_entries,
        bytes_ref_array: self,
      };
      let mut string_sorter = StringSorter::new(delegate, comp);
      string_sorter.sort(0, size)?;
    }
    Ok(SortState::new(indices))
  }
  pub fn iterator(&'_ self) -> IndexedBytesRefIteratorImpl<'_> {
    IndexedBytesRefIteratorImpl::new(None, self)
  }
  /// Returns an [`IndexedBytesRefIteratorImpl`] with point-in-time semantics.
  /// The iterator provides access to all [`BytesRef`] instances appended so
  /// far.
  ///
  /// # Parameters
  /// - `sort_state`:  the iterator will iterate the byte values in the order
  ///   defined by the `sort_state`.
  ///
  /// # Note
  /// - This is a non-destructive operation.
  /// # See Also
  /// [`IndexedBytesRefIterator`]
  ///
  /// [`BytesRef`]
  pub fn iterator_with_state<S: Borrow<SortState>>(
    &self,
    sort_state: S,
  ) -> IndexedBytesRefIteratorImpl<'_, S> {
    IndexedBytesRefIteratorImpl::new(Some(sort_state), self)
  }
}
/// Appends a copy of the given [`BytesRef`] to this [`BytesRefArray`].
///
/// # Parameters
/// - `bytes`: The [`BytesRef`] to append.
///
/// # Returns
/// The index of the appended bytes.
///
/// [`BytesRef`]
///
/// [`BytesRefArray`]
impl<'a> SortableBytesRefArray<'a> for BytesRefArray {
  fn append<AV: ByteSource>(&mut self, bytes: &BytesRef<AV>) -> Result<usize> {
    let view = bytes.as_byte_slice();
    if self.offsets.len() == self.offsets.capacity() {
      let old_size = size_of_array_capacity::<usize>(self.offsets.capacity());
      let min_size = self.offsets.len() + 1;
      ArrayUtil::oversize(min_size, size_of::<usize>())?;
      // Match Vec's minimum nonzero growth capacity for word-sized elements.
      let capacity = min_size.max(self.offsets.capacity() * 2).max(4);
      self.offsets.reserve_exact(capacity - self.offsets.len());
      self
        .byte_used
        .add_and_get(size_of_array_capacity::<usize>(self.offsets.capacity()) - old_size);
    }
    match &mut self.storage {
      ByteStorage::Compact(bytes) => {
        // Keep the same representable end offsets as the block pool.
        let end = bytes
          .len()
          .checked_add(view.len())
          .filter(|&end| i32::try_from(end).is_ok());
        if end.is_none() {
          return Err(LuceneError::number_overflow(
            "Overflow when calculating byte offset.",
          ));
        }
        let old_capacity = bytes.capacity();
        bytes.extend_from_slice(view);
        if bytes.capacity() != old_capacity {
          self.byte_used.add_and_get(
            size_of_array_capacity::<u8>(bytes.capacity())
              - if old_capacity == 0 {
                0
              } else {
                size_of_array_capacity::<u8>(old_capacity)
              },
          );
        }
      },
      ByteStorage::Blocks(pool) => {
        if pool.buffer_upto.is_none() {
          pool.next_buffer()?;
        }
        pool.append(view)?;
      },
    }
    self.offsets.push(self.current_offset);
    self.last_element += 1;
    self.current_offset += view.len();
    Ok(self.last_element - 1)
  }

  fn clear(&mut self) {
    self.last_element = 0;
    self.current_offset = 0;
    self.offsets.clear();
    match &mut self.storage {
      ByteStorage::Compact(bytes) => bytes.clear(),
      ByteStorage::Blocks(pool) => pool.reset(false, true),
    }
  }

  fn size(&self) -> usize {
    self.last_element
  }

  /// Returns an [`IndexedBytesRefIterator`] with point-in-time semantics. The
  /// iterator provides access to all [`BytesRef`] instances appended so
  /// far.
  ///
  /// # Parameters
  /// - `comp`: An optional [`Comparator`](crate::core::util::comparator::Comparator) to specify the order of iteration.
  ///   the iterator will iterate the byte values in the order specified by
  ///   the comparator.
  ///
  /// # Note
  /// - This is a non-destructive operation.
  type Iter = IndexedBytesRefIteratorImpl<'a>;

  fn iterator<C>(&'a self, comp: C) -> Result<Self::Iter>
  where
    C: BytesRefComparator<Cow<'a, Vec<u8>>>,
  {
    let ords = self.sort(comp, false)?;
    Ok(self.iterator_with_state(ords))
  }
}

#[derive(Clone, Debug)]
pub struct SortState {
  pub(crate) indices: Option<smallvec::SmallVec<[usize; 4]>>,
}
impl SortState {
  fn new(indices: Option<smallvec::SmallVec<[usize; 4]>>) -> SortState {
    SortState { indices }
  }
}
impl Accountable for SortState {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(
      self
        .indices
        .as_ref()
        .filter(|indices| indices.spilled())
        .map_or(0, |indices| {
          size_of_array_capacity::<usize>(indices.capacity())
        }),
    )
  }
}

pub struct IndexedBytesRefIteratorImpl<'a, S = SortState> {
  pos: usize,
  pub(crate) ord: usize,
  sort_state: Option<S>,
  size: usize,
  bytes_ref_array: &'a BytesRefArray,
  result: BytesRef<Vec<u8>>,
}
impl<'a, S: Borrow<SortState>> IndexedBytesRefIteratorImpl<'a, S> {
  pub(crate) fn new(
    sort_state: Option<S>,
    bytes_ref_array: &'a BytesRefArray,
  ) -> IndexedBytesRefIteratorImpl<'a, S> {
    if let Some(indices) = sort_state
      .as_ref()
      .and_then(|state| state.borrow().indices.as_ref())
    {
      debug_assert_eq!(indices.len(), bytes_ref_array.size());
    }
    Self {
      pos: 0,
      ord: 0,
      sort_state,
      size: bytes_ref_array.size(),
      bytes_ref_array,
      result: BytesRef::new(),
    }
  }
}
impl<S: Borrow<SortState>> IndexedBytesRefIterator for IndexedBytesRefIteratorImpl<'_, S> {
  fn next(&mut self) -> Result<Option<(usize, BytesRefValueEnum<'_>)>> {
    if self.pos < self.size {
      self.ord = match self
        .sort_state
        .as_ref()
        .and_then(|state| state.borrow().indices.as_ref())
      {
        None => self.pos,
        Some(indices) => indices[self.pos],
      };

      let array = self.bytes_ref_array;
      if self.ord >= array.last_element {
        return Err(LuceneError::array_index_out_of_bounds(format!(
          "index: {}, last_element: {}",
          self.ord, array.last_element
        )));
      }
      let offset = array.offsets[self.ord];
      let length = if self.ord == array.last_element - 1 {
        array.current_offset - offset
      } else {
        array.offsets[self.ord + 1] - offset
      };
      // Retain the growth error before updating the result or converting
      // the offset, even when a contiguous value no longer needs a copy.
      if self.result.bytes.len() < length {
        ArrayUtil::oversize(length, size_of::<u8>())?;
      }
      let pool = match &array.storage {
        ByteStorage::Compact(bytes) => {
          self.pos += 1;
          let value = BytesRef {
            bytes: &bytes[offset..offset + length],
            offset: 0,
            length,
          };
          return Ok(Some((self.ord, BytesRefValueEnum::Slice(value))));
        },
        ByteStorage::Blocks(pool) => pool,
      };
      let offset = offset as i64;
      let pos = (offset & BYTE_BLOCK_MASK as i64) as usize;
      if pos + length > BYTE_BLOCK_SIZE as usize {
        ArrayUtil::grow_no_copy(&mut self.result.bytes, length, || 0)?;
      }
      self.result.length = length;
      let buffer_index: i32 = (offset >> BYTE_BLOCK_SHIFT).try_convert()?;
      let value = if pos + length <= BYTE_BLOCK_SIZE as usize {
        let bytes = &pool.get_buffer(buffer_index as usize)[pos..pos + length];
        self.result.offset = 0;
        BytesRefValueEnum::Slice(BytesRef {
          bytes,
          offset: 0,
          length,
        })
      } else {
        self.result.offset = 0;
        pool.read_bytes(offset, &mut self.result.bytes, 0, length)?;
        BytesRefValueEnum::Buffer(Cow::Borrowed(&self.result))
      };
      self.pos += 1;
      Ok(Some((self.ord, value)))
    } else {
      Ok(None)
    }
  }
  fn ord(&self) -> usize {
    self.ord
  }
}

pub trait IndexedBytesRefIterator {
  /// Returns the next ordinal and value, borrowing a pool block when contiguous.
  fn next(&mut self) -> Result<Option<(usize, BytesRefValueEnum<'_>)>>;

  /// Returns the ordinal position of the element that was returned in the
  /// latest call to [`next`](Self::next).
  ///
  /// # Warning
  /// This method must not be called if [`next`](Self::next) has
  /// not been called yet, or if the last call to
  /// [`next`](Self::next) returned `None`.
  fn ord(&self) -> usize;
}

struct StableStringSorterImpl<'a, 'o> {
  tmp: Vec<usize>,
  ordered_entries: &'o mut [usize],
  bytes_ref_array: &'a BytesRefArray,
}
impl Sorter for StableStringSorterImpl<'_, '_> {
  fn swap(&mut self, i: usize, j: usize) -> Result<()> {
    self.ordered_entries.swap(i, j);
    Ok(())
  }
}

impl<'a> StringSorterBase for StableStringSorterImpl<'a, '_> {
  type Bytes = Cow<'a, Vec<u8>>;

  fn get(
    &mut self,
    builder: &mut BytesRefBuilder<Vec<u8>>,
    result: &mut BytesRef<Cow<'a, Vec<u8>>>,
    i: usize,
  ) -> Result<()> {
    self
      .bytes_ref_array
      .set_bytes_ref(builder, result, self.ordered_entries[i])
  }
}

impl StableStringSorterBase for StableStringSorterImpl<'_, '_> {
  fn save(&mut self, i: usize, j: usize) {
    let value = self.ordered_entries[i];
    if self.tmp.is_empty() {
      self.tmp = vec![0; self.ordered_entries.len()];
    }
    self.tmp[j] = value;
  }
  fn restore(&mut self, i: usize, j: usize) {
    self.ordered_entries.copy_from(&self.tmp[i..j], i);
  }
}
impl MSBRadixSorterBase for StableStringSorterImpl<'_, '_> {}

struct StringSorterImpl<'a, 'o> {
  ordered_entries: &'o mut [usize],
  bytes_ref_array: &'a BytesRefArray,
}
impl Sorter for StringSorterImpl<'_, '_> {
  fn swap(&mut self, i: usize, j: usize) -> Result<()> {
    self.ordered_entries.swap(i, j);
    Ok(())
  }
}
impl<'a> StringSorterBase for StringSorterImpl<'a, '_> {
  type Bytes = Cow<'a, Vec<u8>>;

  fn get(
    &mut self,
    builder: &mut BytesRefBuilder<Vec<u8>>,
    result: &mut BytesRef<Cow<'a, Vec<u8>>>,
    i: usize,
  ) -> Result<()> {
    self
      .bytes_ref_array
      .set_bytes_ref(builder, result, self.ordered_entries[i])
  }
}
