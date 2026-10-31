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
use crate::core::index::BytesRef;
use crate::core::store::IndexInput;
use crate::core::store::directory::Directory;
use crate::core::util::array_util::ByteArrayComparator;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bkd::bkd_bytes::BKDBytes;
use crate::core::util::bkd::bkd_config::BKDConfig;
use crate::core::util::bkd::heap_point_reader::HeapPointReader;
use crate::core::util::bkd::point_value::PointValue;
use crate::core::util::bkd::point_writer::PointWriter;
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::{CoreHelper, SliceCopyOps, ToInt};
use std::fmt;
use std::fmt::{Display, Formatter};

/// Utility struct to write new points into in-heap arrays.
pub struct HeapPointWriter<const N: usize = 0> {
  pub block: Vec<u8>,
  pub size: usize,
  pub config: BKDConfig,
  pub scratch: Vec<u8>,
  pub(crate) dim_comparator: BKDBytes<N>,
  // length is composed by the data dimensions plus the docID
  pub data_dims_and_doc_length: usize,
  pub next_write: usize,
  pub closed: bool,
  pub point_value: Option<HeapPointValue>,
}
impl<const N: usize> Default for HeapPointWriter<N> {
  fn default() -> Self {
    Self {
      block: vec![],
      size: 0,
      config: BKDConfig::default(),
      scratch: vec![],
      dim_comparator: BKDBytes::new(1),
      data_dims_and_doc_length: 0,
      next_write: 0,
      closed: false,
      point_value: None,
    }
  }
}

#[cfg(test)]
impl HeapPointWriter<0> {
  pub fn new(config: BKDConfig, size: usize) -> Self {
    Self::new_with_bytes(config, size)
  }
}
impl<const N: usize> HeapPointWriter<N> {
  pub fn new_with_bytes(config: BKDConfig, size: usize) -> Self {
    let data_dims_and_doc_length = config.bytes_per_doc() - config.packed_index_bytes_length();
    let bytes_per_doc = config.bytes_per_doc();
    let block = vec![0u8; bytes_per_doc * size];
    let point_value = if size > 0 {
      Some(HeapPointValue::new(&config, vec![]))
    } else {
      None
    };
    let bytes_per_dim = config.bytes_per_dim;
    Self {
      block,
      size,
      config,
      scratch: vec![0u8; bytes_per_doc],
      dim_comparator: BKDBytes::new(bytes_per_dim),
      data_dims_and_doc_length,
      next_write: 0,
      closed: false,
      point_value,
    }
  }
  pub fn get_packed_value_slice(&mut self, index: usize) -> Result<&HeapPointValue> {
    debug_assert!(self.closed);
    debug_assert!(
      index < self.next_write,
      "next_write={} vs index={}",
      self.next_write,
      index
    );

    let pv = self
      .point_value
      .as_mut()
      .ok_or_else(|| LuceneError::illegal_state("point_value not initialized"))?;

    pv.set_offset(index * self.config.bytes_per_doc());
    Ok(pv)
  }
  /// Swaps the point at point `i` with the point at position `j`
  pub(crate) fn swap(&mut self, i: usize, j: usize) -> Result<()> {
    debug_assert!(self.closed);
    let heap_value = match self.point_value {
      Some(ref mut v) => v,
      None => {
        return Err(LuceneError::illegal_state("point_value is None"));
      },
    };
    let bytes_per_doc = self.config.bytes_per_doc();
    let index_i = bytes_per_doc * i;
    let index_j = bytes_per_doc * j;
    self
      .scratch
      .copy_from(&heap_value.value[index_i..index_i + bytes_per_doc], 0);
    // Keep the existing scratch update when swapping a point with itself.
    if index_i != index_j || self.scratch.len() != bytes_per_doc {
      heap_value
        .value
        .copy_within(index_j..index_j + bytes_per_doc, index_i);
      heap_value.value.copy_from(&self.scratch, index_j);
    }
    Ok(())
  }

  /// Return the byte at position `k` of the point at position `i`
  pub fn byte_at(&self, i: usize, k: usize) -> Result<i32> {
    debug_assert!(self.closed);
    let heap_value = match self.point_value {
      Some(ref v) => v,
      None => {
        return Err(LuceneError::illegal_state("point_value is None"));
      },
    };
    Ok(heap_value.value[i * self.config.bytes_per_doc() + k] as i32)
  }

  /// Copy the dimension `dim` of the point at position `i` in the provided
  /// `bytes` at the given offset
  pub fn copy_dim(&self, i: usize, dim: usize, bytes: &mut [u8], offset: usize) -> Result<()> {
    debug_assert!(self.closed);
    let heap_value = match self.point_value {
      Some(ref v) => v,
      None => {
        return Err(LuceneError::illegal_state("point_value is None"));
      },
    };
    let start = i * self.config.bytes_per_doc() + dim;
    let len = self.config.bytes_per_dim;
    bytes.copy_from(&heap_value.value[start..start + len], offset);
    Ok(())
  }

  /// Copy the data dimensions and doc value of the point at position `i` in
  /// the provided `bytes` at the given offset
  pub fn copy_data_dims_and_doc(&self, i: usize, bytes: &mut [u8], offset: usize) -> Result<()> {
    debug_assert!(self.closed);
    let heap_value = match self.point_value {
      Some(ref v) => v,
      None => {
        return Err(LuceneError::illegal_state("point_value is None"));
      },
    };
    let start = i * self.config.bytes_per_doc() + self.config.packed_index_bytes_length();
    let len = self.data_dims_and_doc_length;
    bytes.copy_from(&heap_value.value[start..start + len], offset);
    Ok(())
  }

  /// Compares the dimension `dim` value of the point at position `i` with the
  /// point at position `j`
  pub fn compare_dim(&self, i: usize, j: usize, dim: usize) -> Result<i32> {
    debug_assert!(self.closed);
    let heap_value = match self.point_value {
      Some(ref v) => v,
      None => {
        return Err(LuceneError::illegal_state("point_value is None"));
      },
    };

    let bytes_per_doc = self.config.bytes_per_doc();
    let i_offset = i * bytes_per_doc + dim;
    let j_offset = j * bytes_per_doc + dim;

    Ok(self.compare_dim_slice(&heap_value.value, i_offset, &heap_value.value, j_offset))
  }

  /// Compares the dimension `dim` value of the point at position `j` with the
  /// provided value
  pub fn compare_dim_with_scratch(
    &self,
    j: usize,
    dim_value: &[u8],
    offset: usize,
    dim: usize,
  ) -> Result<i32> {
    debug_assert!(self.closed);
    let heap_value = match self.point_value {
      Some(ref v) => v,
      None => {
        return Err(LuceneError::illegal_state("point_value is None"));
      },
    };
    let j_offset = j * self.config.bytes_per_doc() + dim;
    Ok(self.compare_dim_slice(dim_value, offset, &heap_value.value, j_offset))
  }

  fn compare_dim_slice(
    &self,
    block_i: &[u8],
    offset_i: usize,
    block_j: &[u8],
    offset_j: usize,
  ) -> i32 {
    self
      .dim_comparator
      .compare(block_i, offset_i, block_j, offset_j)
  }

  /// Compares the data dimensions and doc values of the point at position `i`
  /// with the point at position `j`
  #[inline]
  pub fn compare_data_dims_and_doc(&self, i: usize, j: usize) -> Result<i32> {
    debug_assert!(self.closed);
    let heap_value = match self.point_value {
      Some(ref v) => v,
      None => {
        return Err(LuceneError::illegal_state("point_value is None"));
      },
    };
    let i_offset = i * self.config.bytes_per_doc() + self.config.packed_index_bytes_length();
    let j_offset = j * self.config.bytes_per_doc() + self.config.packed_index_bytes_length();
    Ok(self.compare_data_dims_and_doc_slice(
      &heap_value.value,
      i_offset,
      &heap_value.value,
      j_offset,
    ))
  }

  /// Compares the data dimensions and doc values of the point at position `j`
  /// with the provided value
  pub fn compare_data_dims_and_doc_with(
    &self,
    j: usize,
    data_dims_and_docs: &[u8],
    offset: usize,
  ) -> Result<i32> {
    debug_assert!(self.closed);
    let heap_value = match self.point_value {
      Some(ref v) => v,
      None => {
        return Err(LuceneError::illegal_state("point_value is None"));
      },
    };
    let j_offset = j * self.config.bytes_per_doc() + self.config.packed_index_bytes_length();
    Ok(self.compare_data_dims_and_doc_slice(
      data_dims_and_docs,
      offset,
      &heap_value.value,
      j_offset,
    ))
  }

  fn compare_data_dims_and_doc_slice(
    &self,
    block_i: &[u8],
    offset_i: usize,
    block_j: &[u8],
    offset_j: usize,
  ) -> i32 {
    let len = self.data_dims_and_doc_length;
    let slice_i = &block_i[offset_i..offset_i + len];
    let slice_j = &block_j[offset_j..offset_j + len];
    slice_i.cmp(slice_j).to_int()
  }

  /// Computes the cardinality of the points between `from` tp `to`
  pub fn compute_cardinality(
    &self,
    from: usize,
    to: usize,
    common_prefix_lengths: &[usize],
  ) -> Result<usize> {
    debug_assert!(self.closed);

    let heap_value = match self.point_value {
      Some(ref v) => v,
      None => {
        return Err(LuceneError::illegal_state("point_value is None"));
      },
    };

    let bytes_per_doc = self.config.bytes_per_doc();
    let bytes_per_dim = self.config.bytes_per_dim;
    let num_dims = self.config.num_dims;

    let mut leaf_cardinality = 1;

    for i in (from + 1)..to {
      let point_offset = (i - 1) * bytes_per_doc;
      let next_point_offset = point_offset + bytes_per_doc;
      for (dim, &prefix_len) in common_prefix_lengths.iter().take(num_dims).enumerate() {
        let base = dim * bytes_per_dim;
        let start = base + prefix_len;
        let end = base + bytes_per_dim;

        if CoreHelper::miss_match_u8(
          &heap_value.value[next_point_offset + start..next_point_offset + end],
          &heap_value.value[point_offset + start..point_offset + end],
        ) != -1
        {
          leaf_cardinality += 1;
          break;
        }
      }
    }

    Ok(leaf_cardinality)
  }
  pub fn take_data(&mut self, data: Option<HeapPointValue>) {
    self.point_value = data
  }
}
impl<const N: usize> PointWriter for HeapPointWriter<N> {
  fn append_bytes(&mut self, packed_value: &[u8], doc_id: i32) -> Result<()> {
    debug_assert!(!self.closed, "point writer is already closed");
    debug_assert_eq!(
      packed_value.len(),
      self.config.packed_bytes_length(),
      "[packedValue] must have length {} but was {}",
      self.config.packed_bytes_length(),
      packed_value.len()
    );
    debug_assert!(
      self.next_write < self.size,
      "nextWrite={} vs size={}",
      self.next_write + 1,
      self.size
    );
    let position = self.next_write * self.config.bytes_per_doc();
    self.block.copy_from(
      &packed_value[0..self.config.packed_bytes_length()],
      position,
    );
    BitUtil::set_i32_be(
      &mut self.block,
      position + self.config.packed_bytes_length(),
      doc_id,
    );
    self.next_write += 1;
    Ok(())
  }

  fn append_point_value<V: PointValue + ?Sized>(&mut self, point_value: &V) -> Result<()> {
    debug_assert!(!self.closed, "point writer is already closed");
    debug_assert!(
      self.next_write < self.size,
      "nextWrite={} vs size={}",
      self.next_write + 1,
      self.size
    );
    let packed_value = point_value.packed_value_doc_id_bytes();
    debug_assert_eq!(
      packed_value.length,
      self.config.bytes_per_doc(),
      "[packedValue] must have length {} but was {}",
      self.config.bytes_per_doc(),
      packed_value.length
    );
    let position = self.next_write * self.config.bytes_per_doc();
    self.block.copy_from(
      &packed_value.bytes[packed_value.offset..(packed_value.offset + self.config.bytes_per_doc())],
      position,
    );
    self.next_write += 1;
    Ok(())
  }

  type PointReader<I>
    = HeapPointReader
  where
    I: IndexInput;

  fn get_reader<D>(
    &mut self,
    start: usize,
    length: usize,
    _temp_dir: &D,
  ) -> Result<Self::PointReader<D::IndexInput>>
  where
    D: Directory,
  {
    debug_assert!(
      self.closed,
      "point writer is still open and trying to get a reader"
    );
    debug_assert!(
      start + length <= self.size,
      "start={} length={} docIDs.length={}",
      start,
      length,
      self.size
    );
    debug_assert!(
      start + length <= self.next_write,
      "start={} length={} nextWrite={}",
      start,
      length,
      self.next_write
    );
    let value = start + length;
    if value > i32::MAX as usize {
      return Err(LuceneError::illegal_argument(format!(
        "start + length must be <= {}",
        i32::MAX
      )));
    }
    Ok(HeapPointReader::new(
      self.point_value.take(),
      start,
      value,
      self.config.bytes_per_doc(),
    ))
  }

  fn count(&self) -> usize {
    self.next_write
  }

  fn destroy<D>(&mut self, _dir: &D) -> Result<()>
  where
    D: Directory,
  {
    Ok(())
  }
}
impl<const N: usize> Closeable for HeapPointWriter<N> {
  fn close(&mut self) -> Result<()> {
    self.closed = true;
    if let Some(ref mut point_value) = self.point_value {
      {
        // Since `block` is no longer updated, its ownership is now transferred to
        // `point_value`.
        point_value.value = std::mem::take(&mut self.block);
      }
    }
    Ok(())
  }
}
impl<const N: usize> Display for HeapPointWriter<N> {
  fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
    write!(
      f,
      "{}(count={} size={})",
      std::any::type_name::<Self>(),
      self.next_write,
      self.size
    )
  }
}
impl<const N: usize> Drop for HeapPointWriter<N> {
  fn drop(&mut self) {
    let _ = self.close();
  }
}

/// Reusable implementation for a point value on-heap.
#[derive(Debug, Clone)]
pub(crate) struct HeapPointValue {
  pub(crate) offset: usize,
  pub(crate) value: Vec<u8>,
  pub(crate) packed_value_length: usize,
  pub(crate) packed_value_doc_id_length: usize,
}

impl HeapPointValue {
  pub fn new(config: &BKDConfig, value: Vec<u8>) -> Self {
    Self {
      offset: 0,
      value,
      packed_value_length: config.packed_bytes_length(),
      packed_value_doc_id_length: config.bytes_per_doc(),
    }
  }
  pub fn set_offset(&mut self, offset: usize) {
    self.offset = offset;
  }
}
impl PointValue for HeapPointValue {
  fn packed_value(&self) -> BytesRef<&[u8]> {
    BytesRef {
      bytes: self.value.as_slice(),
      offset: self.offset,
      length: self.packed_value_length,
    }
  }

  fn doc_id(&self) -> i32 {
    let position = self.offset + self.packed_value_length;
    BitUtil::get_i32_be(&self.value[position..], 0)
  }

  fn packed_value_doc_id_bytes(&self) -> BytesRef<&[u8]> {
    BytesRef {
      bytes: self.value.as_slice(),
      offset: self.offset,
      length: self.packed_value_doc_id_length,
    }
  }
}
