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
use crate::core::store::data_input_ext::DataInputExt;
use crate::core::store::{DataInput, DataOutput, IndexInput};
use crate::core::util::SliceCopyOps;
use crate::core::util::accountable::Accountable;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::group_vint_util::GroupVIntUtil;
use crate::core::util::ram_usage_estimator::size_of_vec;
use std::fmt;
use std::fmt::{Display, Formatter};
use std::mem;
use std::sync::Arc;

/// Represents a logical byte buffer as a series of pages. You can write once into
/// the logical buffer (append only), using copy, and then retrieve slices
/// (BytesRef) into it using fill_slice.
#[derive(Default)]
pub struct PagedBytes {
  blocks: Vec<Vec<u8>>,
  // Share the immutable page directory across readers and retained page views.
  frozen_blocks: Option<Arc<Vec<Vec<u8>>>>,
  num_blocks: usize,
  block_size: usize,
  block_bits: usize,
  block_mask: usize,
  did_skip_bytes: bool,
  frozen: bool,
  upto: usize,
  current_block: Option<Vec<u8>>,
  bytes_used_per_block: i64,
}
impl PagedBytes {
  pub fn new(block_bits: usize) -> Self {
    debug_assert!(
      block_bits > 0 && block_bits <= 31,
      "blockBits: {block_bits}"
    );
    let block_size = 1 << block_bits;
    let block_mask = block_size - 1;
    let upto = block_size;
    let bytes_used_per_block = block_size as i64;

    PagedBytes {
      blocks: Vec::with_capacity(16),
      frozen_blocks: None,
      num_blocks: 0,
      block_size,
      block_bits,
      block_mask,
      did_skip_bytes: false,
      frozen: false,
      upto,
      current_block: None,
      bytes_used_per_block,
    }
  }
  fn add_block(&mut self, block: Vec<u8>) -> Result<()> {
    if self.blocks.len() == self.blocks.capacity() {
      ArrayUtil::grow_capacity(&mut self.blocks, self.num_blocks + 1)?;
    }
    self.blocks.push(block);
    self.num_blocks += 1;
    Ok(())
  }
  /// Read this many bytes from in
  pub fn copy_with_input<II>(&mut self, input: &mut II, mut byte_count: usize) -> Result<()>
  where
    II: IndexInput,
  {
    while byte_count > 0 {
      let mut left = self.block_size - self.upto;
      if left == 0 {
        if let Some(block) = self.current_block.take() {
          self.add_block(block)?;
        }
        self.current_block = Some(vec![0u8; self.block_size]);
        self.upto = 0;
        left = self.block_size;
      }
      let current_block = self
        .current_block
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("current_block not initialized"))?;
      if left < byte_count {
        input.read_bytes_with_buffer(current_block, self.upto, left, false)?;
        self.upto = self.block_size;
        byte_count -= left;
      } else {
        input.read_bytes_with_buffer(current_block, self.upto, byte_count, false)?;
        self.upto += byte_count;
        break;
      }
    }
    Ok(())
  }
  /// Copy [`BytesRef`] into the pool, setting the output [`BytesRef`] to the
  /// result.
  ///
  /// Do **not** use this method if `freeze(true)` will be called afterward.
  ///
  /// This only supports `bytes.len() <= block_size`.
  /// The output borrows the current page; finish using it before appending again.
  pub fn copy_with_bytes_ref<'a>(
    &'a mut self,
    bytes: &BytesRef<Vec<u8>>,
    out: &mut BytesRef<&'a mut [u8]>,
  ) -> Result<()> {
    let left = self.block_size - self.upto;
    if bytes.length > left || self.current_block.is_none() {
      if let Some(block) = self.current_block.take() {
        self.add_block(block)?;
        self.did_skip_bytes = true;
      }
      self.current_block = Some(vec![0u8; self.block_size]);
      self.upto = 0;
      debug_assert!(bytes.length <= self.block_size);
    }

    let current_block = self
      .current_block
      .as_mut()
      .ok_or_else(|| LuceneError::illegal_state("current_block not initialized"))?;
    out.offset = self.upto;
    out.length = bytes.length;
    current_block.copy_from(
      &bytes.bytes[bytes.offset..bytes.offset + bytes.length],
      self.upto,
    );
    self.upto += bytes.length;
    out.bytes = current_block;
    Ok(())
  }
  /// Commits the final byte buffer, trimming it when requested.
  pub fn freeze(&mut self, trim: bool) -> Result<Reader> {
    if self.frozen {
      return Err(LuceneError::illegal_state("already frozen"));
    }
    if self.did_skip_bytes {
      return Err(LuceneError::illegal_state(
        "cannot freeze when copy(BytesRef, BytesRef) was used",
      ));
    }

    if let Some(mut block) = self.current_block.take() {
      if trim && self.upto < self.block_size {
        block.truncate(self.upto);
        block.shrink_to_fit();
      }
      self.add_block(block)?;
    } else {
      self.add_block(Vec::new())?;
    }

    self.frozen = true;
    self.current_block = None;

    // Transfer the whole directory, including its spare capacity, to the readers.
    // No writer can append after freeze, so rebuilding the outer Vec is unnecessary.
    self.frozen_blocks = Some(Arc::new(std::mem::take(&mut self.blocks)));

    Reader::new(self)
  }
  pub fn get_pointer(&self) -> i64 {
    if self.current_block.is_none() {
      0
    } else {
      (self.num_blocks as i64 * self.block_size as i64) + self.upto as i64
    }
  }
  /// Copy bytes in, writing the length as a 1 or 2 byte vInt prefix.
  pub fn copy_using_length_prefix(&mut self, bytes: &BytesRef<Vec<u8>>) -> Result<i64> {
    if bytes.length >= 32768 {
      return Err(LuceneError::illegal_argument(format!(
        "max length is 32767 (got {})",
        bytes.length
      )));
    }

    if self.upto + bytes.length + 2 > self.block_size {
      if bytes.length + 2 > self.block_size {
        return Err(LuceneError::illegal_argument(format!(
          "block size {} is too small to store length {} bytes",
          self.block_size, bytes.length
        )));
      }
      if let Some(block) = self.current_block.take() {
        self.add_block(block)?;
      }
      self.current_block = Some(vec![0u8; self.block_size]);
      self.upto = 0;
    }

    let pointer = self.get_pointer();
    let current_block = self
      .current_block
      .as_mut()
      .ok_or_else(|| LuceneError::illegal_state("current_block not initialized"))?;

    if bytes.length < 128 {
      current_block[self.upto] = bytes.length as u8;
      self.upto += 1;
    } else {
      let length = (bytes.length | 0x8000) as u16;
      current_block[self.upto] = (length >> 8) as u8;
      current_block[self.upto + 1] = length as u8;
      self.upto += 2;
    }
    current_block.copy_from(
      &bytes.bytes[bytes.offset..bytes.offset + bytes.length],
      self.upto,
    );
    self.upto += bytes.length;

    Ok(pointer)
  }
}
impl Accountable for PagedBytes {
  fn ram_bytes_used(&self) -> Result<i64> {
    let mut bytes = size_of_vec(&self.blocks);
    if let Some(frozen_blocks) = &self.frozen_blocks {
      // The shared directory retains its Vec control value and Arc reference counts.
      bytes = bytes
        .saturating_add(size_of_vec(frozen_blocks))
        .saturating_add(mem::size_of::<Vec<Vec<u8>>>() as i64)
        .saturating_add((2 * mem::size_of::<usize>()) as i64);
      if let Some(last_block) = frozen_blocks.last() {
        bytes = bytes
          .saturating_add(
            ((frozen_blocks.len() - 1) as i64).saturating_mul(self.bytes_used_per_block),
          )
          .saturating_add(size_of_vec(last_block));
      }
    } else {
      if self.num_blocks > 0 {
        bytes = bytes
          .saturating_add(((self.num_blocks - 1) as i64).saturating_mul(self.bytes_used_per_block))
          .saturating_add(size_of_vec(&self.blocks[self.num_blocks - 1]));
      }
      if let Some(current_block) = &self.current_block {
        bytes = bytes.saturating_add(size_of_vec(current_block));
      }
    }
    Ok(bytes)
  }
}
/// An independent byte view that retains the frozen page directory.
///
/// Its default is an empty byte source. A populated view retains all pages,
/// including after the originating Reader or PagedBytes has been dropped.
#[derive(Clone, Default)]
pub struct PagedBytesPage {
  source: Option<(Arc<Vec<Vec<u8>>>, usize)>,
}

impl Clone for BytesRef<PagedBytesPage> {
  fn clone(&self) -> Self {
    let bytes_ref = Self {
      bytes: self.bytes.clone(),
      offset: self.offset,
      length: self.length,
    };
    debug_assert!({
      let bytes = crate::core::util::access::ByteSource::as_slice(&bytes_ref.bytes);
      bytes_ref.offset <= bytes.len()
        && bytes_ref.length <= bytes.len()
        && bytes_ref.offset + bytes_ref.length <= bytes.len()
    });
    bytes_ref
  }
}

impl crate::core::util::access::ByteSource for PagedBytesPage {
  fn as_slice(&self) -> &[u8] {
    match &self.source {
      Some((blocks, index)) => &blocks[*index],
      None => &[],
    }
  }
}

impl PagedBytesPage {
  fn retains(&self, blocks: &Arc<Vec<Vec<u8>>>, index: usize) -> bool {
    self
      .source
      .as_ref()
      .is_some_and(|(retained, page)| *page == index && Arc::ptr_eq(retained, blocks))
  }
}

/// Provides methods to read BytesRefs from a frozen PagedBytes.
#[derive(Clone, Default)]
pub struct Reader {
  blocks: Arc<Vec<Vec<u8>>>,
  block_bits: usize,
  block_mask: usize,
  block_size: usize,
  bytes_used_per_block: i64,
}

impl Reader {
  /// 1<<blockBits must be bigger than biggest single BytesRef slice that will
  /// be pulled
  pub fn new(paged_bytes: &PagedBytes) -> Result<Self> {
    Ok(Reader {
      blocks: paged_bytes
        .frozen_blocks
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("blocks_rc not initialized"))?
        .clone(),
      block_bits: paged_bytes.block_bits,
      block_mask: paged_bytes.block_mask,
      block_size: paged_bytes.block_size,
      bytes_used_per_block: paged_bytes.bytes_used_per_block,
    })
  }
  /// Gets a slice out of [`PagedBytes`] starting at `start` with the given
  /// `length`.
  ///
  /// If the slice spans across a block boundary, this method will allocate
  /// sufficient resources and copy the paged data.
  ///
  /// Slices spanning more than two blocks are **not supported**.
  pub fn fill_slice<'a>(
    &'a self,
    b: &'a mut BytesRef<Vec<u8>>,
    start: usize,
    length: usize,
  ) -> Result<BytesRef<&'a [u8]>> {
    debug_assert!(length <= self.block_size + 1, "length={length}");
    b.length = length;

    if length == 0 {
      return Ok(BytesRef {
        bytes: &b.bytes,
        offset: b.offset,
        length,
      });
    }

    let index = start >> self.block_bits;
    let offset = start & self.block_mask;

    if self.block_size - offset >= length {
      // Within block
      Ok(BytesRef {
        bytes: self.blocks[index].as_slice(),
        offset,
        length,
      })
    } else {
      // Split across two blocks
      ArrayUtil::grow_no_copy(&mut b.bytes, length, || 0)?;
      let first_len = self.block_size - offset;
      b.bytes
        .copy_from(&self.blocks[index][offset..offset + first_len], 0);
      b.bytes.copy_from(
        &self.blocks[index + 1][..length - first_len],
        self.block_size - offset,
      );
      b.offset = 0;
      Ok(BytesRef {
        bytes: &b.bytes,
        offset: b.offset,
        length,
      })
    }
  }
  /// Get the byte at the given offset.
  pub fn get_byte(&self, o: usize) -> u8 {
    let index = o >> self.block_bits;
    let offset = o & self.block_mask;
    self.blocks[index][offset]
  }
  /// Reads a one- or two-byte length prefix at `start` and fills `b`.
  /// The slice must be contained within a single block.
  pub fn fill(&self, b: &mut BytesRef<PagedBytesPage>, start: usize) -> Result<()> {
    let index = start >> self.block_bits;
    let offset = start & self.block_mask;
    let block = &self.blocks[index];
    if !b.bytes.retains(&self.blocks, index) {
      b.bytes = PagedBytesPage {
        source: Some((Arc::clone(&self.blocks), index)),
      };
    }
    if block[offset] & 128 == 0 {
      b.length = block[offset] as usize;
      b.offset = offset + 1;
    } else {
      b.length = (u16::from_be_bytes([block[offset], block[offset + 1]]) & 0x7fff) as usize;
      b.offset = offset + 2;
      debug_assert!(b.length > 0);
    }
    Ok(())
  }
}
impl Accountable for Reader {
  fn ram_bytes_used(&self) -> Result<i64> {
    // Report the complete retained shared directory, as for the shared page payloads.
    let mut bytes = size_of_vec(&self.blocks)
      .saturating_add(mem::size_of::<Vec<Vec<u8>>>() as i64)
      .saturating_add((2 * mem::size_of::<usize>()) as i64);
    if let Some(last_block) = self.blocks.last() {
      bytes = bytes
        .saturating_add(((self.blocks.len() - 1) as i64).saturating_mul(self.bytes_used_per_block))
        .saturating_add(size_of_vec(last_block));
    }
    Ok(bytes)
  }
}
impl Display for Reader {
  fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
    write!(f, "PagedBytes(blocksize={})", self.block_size)
  }
}
impl Display for PagedBytes {
  fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
    write!(
      f,
      "{}(blocksize={})",
      std::any::type_name::<Self>(),
      self.block_size
    )
  }
}
/// Input that transparently iterates over pages
#[derive(Clone)]
pub struct PagedBytesDataInput {
  current_block_index: usize,
  current_block_upto: usize,
  block_size: usize,
  block_bits: usize,
  block_mask: usize,
  blocks: Arc<Vec<Vec<u8>>>,
}

impl PagedBytesDataInput {
  fn new(blocks: &PagedBytes) -> Result<Self> {
    debug_assert!(blocks.frozen_blocks.is_some());
    Ok(Self {
      current_block_index: 0,
      current_block_upto: 0,
      block_size: blocks.block_size,
      block_bits: blocks.block_bits,
      block_mask: blocks.block_mask,
      blocks: blocks
        .frozen_blocks
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("blocks_rc not initialized"))?
        .clone(),
    })
  }
  /// Returns the current byte position.
  pub fn get_position(&self) -> usize {
    (self.current_block_index * self.block_size) + self.current_block_upto
  }
  /// Seek to a position previously obtained from `get_position()`.
  pub fn set_position(&mut self, pos: usize) {
    self.current_block_index = pos >> self.block_bits;
    self.current_block_upto = pos & self.block_mask;
  }
  fn next_block(&mut self) {
    self.current_block_index += 1;
    self.current_block_upto = 0;
  }
}

impl Display for PagedBytesDataInput {
  fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
    write!(
      f,
      "{}(blocks={:?}, current_block_index={}, current_block_upto={})",
      std::any::type_name::<Self>(),
      self.blocks,
      self.current_block_index,
      self.current_block_upto
    )
  }
}

impl crate::core::util::close::Closeable for PagedBytesDataInput {}

impl DataInput for PagedBytesDataInput {
  fn read_byte(&mut self) -> Result<u8> {
    if self.current_block_upto == self.block_size {
      self.next_block();
    }

    let byte = self.blocks[self.current_block_index][self.current_block_upto];
    self.current_block_upto += 1;
    Ok(byte)
  }

  fn get_bytes<'a>(&'a mut self, len: usize) -> Result<std::borrow::Cow<'a, [u8]>> {
    let offset = self.current_block_upto;
    let contiguous = self
      .block_size
      .checked_sub(offset)
      .is_some_and(|left| len <= left)
      && self
        .blocks
        .get(self.current_block_index)
        .is_some_and(|block| {
          block
            .len()
            .checked_sub(offset)
            .is_some_and(|left| len <= left)
        });
    if contiguous {
      self.current_block_upto += len;
      return Ok(std::borrow::Cow::Borrowed(
        &self.blocks[self.current_block_index][offset..offset + len],
      ));
    }
    // The current page may be exhausted while the next request is still
    // wholly contained in the following page. Preserve the ordinary read
    // transition, then borrow that page instead of copying through a Vec.
    if offset == self.block_size
      && len > 0
      && len <= self.block_size
      && self
        .blocks
        .get(self.current_block_index)
        .is_some_and(|block| block.len() >= offset)
      && self
        .current_block_index
        .checked_add(1)
        .and_then(|index| self.blocks.get(index))
        .is_some_and(|block| len <= block.len())
    {
      self.next_block();
      self.current_block_upto = len;
      return Ok(std::borrow::Cow::Borrowed(
        &self.blocks[self.current_block_index][..len],
      ));
    }
    if let Some(block_left) = self.block_size.checked_sub(offset)
      && block_left < len
      && len - block_left <= self.block_size
      && let Some(first) = self
        .blocks
        .get(self.current_block_index)
        .and_then(|block| block.get(offset..self.block_size))
      && self
        .current_block_index
        .checked_add(1)
        .and_then(|index| self.blocks.get(index))
        .is_some_and(|block| len - block_left <= block.len())
    {
      // A cross-page result needs its own bytes. Fill that owner directly
      // instead of initializing bytes that the successful read overwrites.
      let second_len = len - block_left;
      let second = &self.blocks[self.current_block_index + 1][..second_len];
      let bytes = [first, second].concat();
      self.next_block();
      self.current_block_upto = second_len;
      return Ok(std::borrow::Cow::Owned(bytes));
    }
    // Longer reads still need an independent owner. Append each page's
    // bytes directly, retaining the read_bytes page and failure transitions.
    let mut bytes = Vec::with_capacity(len);
    let mut left = len;
    loop {
      let block = &self.blocks[self.current_block_index];
      let block_left = self.block_size - self.current_block_upto;
      if block_left < left {
        bytes
          .extend_from_slice(&block[self.current_block_upto..self.current_block_upto + block_left]);
        self.next_block();
        left -= block_left;
      } else {
        bytes.extend_from_slice(&block[self.current_block_upto..self.current_block_upto + left]);
        self.current_block_upto += left;
        break;
      }
    }
    Ok(std::borrow::Cow::Owned(bytes))
  }

  fn read_bytes(&mut self, b: &mut [u8], mut offset: usize, len: usize) -> Result<()> {
    debug_assert!(
      b.len() >= (offset + len),
      "b.len()={}, offset={}, len={}",
      b.len(),
      offset,
      len
    );
    let offset_end = offset + len;

    loop {
      let block = &self.blocks[self.current_block_index];
      let block_left = self.block_size - self.current_block_upto;
      let left = offset_end - offset;

      if block_left < left {
        let source = &block[self.current_block_upto..self.current_block_upto + block_left];
        b[offset..offset + block_left].copy_from_slice(source);
        self.next_block();
        offset += block_left;
      } else {
        let source = &block[self.current_block_upto..self.current_block_upto + left];
        b[offset..offset + left].copy_from_slice(source);
        self.current_block_upto += left;
        break;
      }
    }
    Ok(())
  }

  fn read_group_vint(&mut self, dst: &mut [i32], offset: usize) -> Result<()> {
    GroupVIntUtil::read_group_vint_i32(self, dst, offset)
  }

  fn skip_bytes(&mut self, num_bytes: i64) -> Result<()> {
    if num_bytes < 0 {
      return Err(LuceneError::illegal_argument(format!(
        "num_bytes must be >= 0, got {num_bytes}"
      )));
    }
    let skip_to = self.get_position() + num_bytes as usize;
    self.set_position(skip_to);
    Ok(())
  }
}

impl DataInputExt for PagedBytesDataInput {}

pub struct PagedBytesDataOutput {
  pub paged_bytes: PagedBytes,
}
impl PagedBytesDataOutput {
  fn new(paged_bytes: PagedBytes) -> Self {
    PagedBytesDataOutput { paged_bytes }
  }
  /// Return the current byte position.
  pub fn get_position(&self) -> i64 {
    self.paged_bytes.get_pointer()
  }
}
impl DataOutput for PagedBytesDataOutput {
  fn write_byte(&mut self, b: u8) -> Result<()> {
    if self.paged_bytes.upto == self.paged_bytes.block_size {
      if let Some(block) = self.paged_bytes.current_block.take() {
        self.paged_bytes.add_block(block)?;
      }
      self.paged_bytes.current_block = Some(vec![0u8; self.paged_bytes.block_size]);
      self.paged_bytes.upto = 0;
    }

    let block = self
      .paged_bytes
      .current_block
      .as_mut()
      .ok_or_else(|| LuceneError::illegal_state("current_block not initialized"))?;
    block[self.paged_bytes.upto] = b;
    self.paged_bytes.upto += 1;
    Ok(())
  }

  fn write_bytes_range(&mut self, b: &[u8], mut offset: usize, length: usize) -> Result<()> {
    debug_assert!(
      b.len() >= (offset + length),
      "b.len={} offset={} length={}",
      b.len(),
      offset,
      length
    );
    if length == 0 {
      return Ok(());
    }

    if self.paged_bytes.upto < self.paged_bytes.block_size
      && length <= self.paged_bytes.block_size - self.paged_bytes.upto
    {
      let offset_end = offset + length;
      let current_block = self
        .paged_bytes
        .current_block
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("current_block not initialized"))?;
      current_block.copy_from(&b[offset..offset_end], self.paged_bytes.upto);
      self.paged_bytes.upto += length;
      return Ok(());
    }

    if self.paged_bytes.upto == self.paged_bytes.block_size {
      if let Some(block) = self.paged_bytes.current_block.take() {
        self.paged_bytes.add_block(block)?;
      }
      self.paged_bytes.current_block = Some(vec![0u8; self.paged_bytes.block_size]);
      self.paged_bytes.upto = 0;
    }
    let offset_end = offset + length;

    loop {
      let left = offset_end - offset;
      let block_left = self.paged_bytes.block_size - self.paged_bytes.upto;

      let current_block = self
        .paged_bytes
        .current_block
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("current_block not initialized"))?;
      if block_left < left {
        current_block.copy_from(&b[offset..offset + block_left], self.paged_bytes.upto);
        let old_block = std::mem::replace(current_block, vec![0u8; self.paged_bytes.block_size]);
        self.paged_bytes.add_block(old_block)?;

        self.paged_bytes.upto = 0;
        offset += block_left;
      } else {
        current_block.copy_from(&b[offset..offset + left], self.paged_bytes.upto);
        self.paged_bytes.upto += left;
        break;
      }
    }
    Ok(())
  }
}

/// Returns a DataInput to read values from this PagedBytes instance.
pub fn get_data_input(paged_bytes: &PagedBytes) -> Result<PagedBytesDataInput> {
  if !paged_bytes.frozen {
    return Err(LuceneError::illegal_state(
      "must call freeze() before get_data_input()",
    ));
  }

  PagedBytesDataInput::new(paged_bytes)
}
/// Returns a DataOutput that you may use to write into this PagedBytes
/// instance. If you do this,  you should not call the other writing methods
/// (eg, copy); results are undefined.
pub fn get_data_output(paged_bytes: PagedBytes) -> Result<PagedBytesDataOutput> {
  if paged_bytes.frozen {
    return Err(LuceneError::illegal_state(
      "cannot get DataOutput after freeze()",
    ));
  }

  Ok(PagedBytesDataOutput::new(paged_bytes))
}
