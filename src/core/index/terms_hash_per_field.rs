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
use crate::core::index::byte_slice_pool::ByteSlicePool;
use crate::core::index::byte_slice_reader::ByteSliceReader;
use crate::core::index::field_invert_state::FieldInvertState;
use crate::core::index::freq_prox_terms_writer_per_field::FreqProxPostingsArray;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::parallel_postings_array::PostingsArray;
use crate::core::util::attribute_source::AttributeSource;
use crate::core::util::bytes_ref_hash::{BytesRefHash, BytesStartArray};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::int_block_pool::{
  INT_BLOCK_MASK, INT_BLOCK_SHIFT, INT_BLOCK_SIZE, IntBlockPool,
};
use crate::core::util::{
  BYTE_BLOCK_MASK, BYTE_BLOCK_SHIFT, BYTE_BLOCK_SIZE, ByteBlockPool, Counter, SharedCounter,
  SliceCopyOps, size_of_slice,
};

/// This struct stores streams of information per term without knowing the size
/// of the stream ahead of time. Each stream typically encodes one level of
/// information, like term frequency per document or term proximity.
///
/// Internally, this struct allocates a linked list of slices that can be read
/// by a [`ByteSliceReader`] for each term. Terms are first deduplicated in a
/// [`BytesRefHash`]. Once this is done, internal data structures point to the
/// current offset of each stream that can be written to.
pub struct TermsHashPerField<PA: PostingsArray = FreqProxPostingsArray> {
  slice_pool: ByteSlicePool,
  // for each term we store an integer per stream that points into the
  // bytePool above the address is updated once data is written to the
  // stream to point to the next free offset in the terms stream. The
  // start address for the stream is stored in postingsArray.
  // `byte_starts[term_id]` is initialized in `add_term`,
  // either to a brand new per term stream if the term is new or
  // to the addresses where the term stream was written to when we saw it the
  // last time.    term_stream_address_buffer: Vec<i32>,
  term_stream_address_buffer_index: usize,
  stream_address_offset: i32,
  stream_count: i32,
  // This stores the actual term bytes for postings and offsets into the
  // parent hash in the case that this TermsHashPerField is hashing term
  // vectors.
  pub(crate) bytes_hash: BytesRefHash<PostingsBytesStartArray<PA>>,
  // Sorted term IDs stay in bytes_hash.ids; track whether they are available separately.
  #[cfg(debug_assertions)]
  is_sorted: bool,
  last_doc_id: i32, // only used with debug/asserts
  pub(crate) index_options: IndexOptions,
}
impl<PA: PostingsArray> TermsHashPerField<PA> {
  ///  streamCount: how many streams this field stores per term. E.g.
  /// doc(+freq) is 1 stream, prox+offset is a second.
  pub(crate) fn new(
    stream_count: i32,
    bytes_used: SharedCounter,
    postings_array_wrapper: PostingsArrayWrapper<PA>,
    index_options: IndexOptions,
  ) -> Result<Self> {
    debug_assert!(index_options != IndexOptions::None);
    let slice_pool = ByteSlicePool;
    let byte_starts = PostingsBytesStartArray::new(postings_array_wrapper, bytes_used);

    let bytes_hash = BytesRefHash::from_bytes_start_array(HASH_INIT_SIZE, byte_starts)?;
    Ok(TermsHashPerField {
      slice_pool,
      term_stream_address_buffer_index: 0,
      stream_address_offset: 0,
      stream_count,
      bytes_hash,
      #[cfg(debug_assertions)]
      is_sorted: false,
      last_doc_id: 0,
      index_options,
    })
  }
  pub(crate) fn init_reader(
    &self,
    reader: &mut ByteSliceReader<'_>,
    term_id: usize,
    stream: i32,
    int_pool: &IntBlockPool,
  ) -> Result<()> {
    debug_assert!(stream < self.stream_count);
    let postings_array_wrapper = &self.bytes_hash.bytes_start_array.per_field;
    let stream_start_offset = postings_array_wrapper
      .postings_array
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("postings array is missing"))?
      .get_address_offset()[term_id];
    let buffer_index = (stream_start_offset >> INT_BLOCK_SHIFT) as usize;
    let offset_in_address_buffer = stream_start_offset & INT_BLOCK_MASK;
    let addr;
    {
      let stream_address_buffer = int_pool.get_buffer(buffer_index);
      addr = stream_address_buffer[(offset_in_address_buffer + stream) as usize];
    }
    let init_offset = postings_array_wrapper
      .postings_array
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("postings array is missing"))?
      .get_byte_starts()[term_id]
      + stream * ByteSlicePool::FIRST_LEVEL_SIZE;
    reader.init(init_offset as usize, addr as usize);
    Ok(())
  }
  /// Collapse the hash table and sort in-place; also sets this.sortedTermIDs
  /// to the results. This method must not be called twice unless
  /// [`reset()`](Self::reset) or
  /// [`reinit_hash()`](Self::reinit_hash) was called.
  pub(crate) fn sort_terms(&mut self, byte_pool: &ByteBlockPool) -> Result<()> {
    #[cfg(debug_assertions)]
    debug_assert!(!self.is_sorted);
    self.bytes_hash.sort(byte_pool)?;
    #[cfg(debug_assertions)]
    {
      self.is_sorted = true;
    }
    Ok(())
  }
  /// Returns the sorted term IDs.
  /// [`sort_terms()`](TermsHashPerField::sort_terms) must be called before.
  pub(crate) fn get_sorted_term_ids(&self) -> &[i32] {
    #[cfg(debug_assertions)]
    debug_assert!(self.is_sorted);
    self.bytes_hash.ids.as_slice()
  }

  fn write_byte(
    &self,
    stream_address: &mut i32,
    b: u8,
    byte_pool: &mut ByteBlockPool,
  ) -> Result<()> {
    let upto = *stream_address;
    let block_index = (upto >> BYTE_BLOCK_SHIFT) as usize;
    debug_assert!(block_index <= byte_pool.buffer_upto()?);
    let offset = (upto & BYTE_BLOCK_MASK) as usize;
    let slot = &mut byte_pool.get_buffer_mut(block_index)[offset];
    if *slot == 0 {
      *slot = b;
    } else {
      // End of slice; allocate a new one
      let offset = self
        .slice_pool
        .alloc_slice(block_index, offset, byte_pool)?;
      *stream_address = offset as i32 + byte_pool.byte_offset;
      let block_index = byte_pool.buffer_upto()?;
      byte_pool.get_buffer_mut(block_index)[offset] = b;
    }
    *stream_address += 1;
    Ok(())
  }
  pub(crate) fn write_bytes(
    &self,
    stream: i32,
    b: &[u8],
    mut offset: usize,
    len: usize,
    int_pool: &mut IntBlockPool,
    byte_pool: &mut ByteBlockPool,
  ) -> Result<()> {
    let end = offset + len;
    let stream_address = (self.stream_address_offset + stream) as usize;

    let term_stream_address_buffer = int_pool.get_buffer_mut(self.term_stream_address_buffer_index);
    let upto = term_stream_address_buffer[stream_address];
    {
      let mut block_index = (upto >> BYTE_BLOCK_SHIFT) as usize;
      debug_assert!(block_index <= byte_pool.buffer_upto()?);
      let slice = byte_pool.get_buffer_mut(block_index);
      let mut slice_offset = (upto & BYTE_BLOCK_MASK) as usize;

      while offset < end && slice[slice_offset] == 0 {
        slice[slice_offset] = b[offset];
        slice_offset += 1;
        offset += 1;
        term_stream_address_buffer[stream_address] += 1;
      }

      while offset < end {
        let offset_and_length =
          self
            .slice_pool
            .alloc_known_size_slice(block_index, slice_offset, byte_pool)?;
        slice_offset = (offset_and_length >> 8) as usize;
        let slice_length = offset_and_length & 0xff;
        let buffer_upto = byte_pool.buffer_upto()?;
        block_index = buffer_upto;
        let slice = byte_pool.get_buffer_mut(buffer_upto);
        let write_length = std::cmp::min(slice_length as usize - 1, end - offset);
        slice.copy_from(&b[offset..offset + write_length], slice_offset);
        slice_offset += write_length;
        offset += write_length;
        debug_assert!(slice_offset <= i32::MAX as usize);
        term_stream_address_buffer[stream_address] = slice_offset as i32 + byte_pool.byte_offset;
      }
    }
    Ok(())
  }
  pub(crate) fn write_vint(
    &self,
    stream: i32,
    mut i: i32,
    int_pool: &mut IntBlockPool,
    byte_pool: &mut ByteBlockPool,
  ) -> Result<()> {
    debug_assert!(stream < self.stream_count);
    let stream_address = (self.stream_address_offset + stream) as usize;
    let term_stream_address_buffer = int_pool.get_buffer_mut(self.term_stream_address_buffer_index);
    let stream_address = &mut term_stream_address_buffer[stream_address];
    while (i & !0x7F) != 0 {
      self.write_byte(stream_address, ((i & 0x7F) | 0x80) as u8, byte_pool)?;
      i = ((i as u32) >> 7) as i32;
    }
    self.write_byte(stream_address, i as u8, byte_pool)
  }

  pub(crate) fn get_num_terms(&self) -> usize {
    self.bytes_hash.size()
  }

  pub(crate) fn postings_array(&self) -> Option<&PA> {
    self
      .bytes_hash
      .bytes_start_array
      .per_field
      .postings_array
      .as_ref()
  }

  pub(crate) fn postings_array_mut(&mut self) -> Option<&mut PA> {
    self
      .bytes_hash
      .bytes_start_array
      .per_field
      .postings_array
      .as_mut()
  }

  pub(crate) fn reset(&mut self, byte_pool: &mut ByteBlockPool) {
    self.bytes_hash.clear_with_reset_pool(false, byte_pool);
    #[cfg(debug_assertions)]
    {
      self.is_sorted = false;
    }
  }

  pub(crate) fn reinit_hash(&mut self) -> Result<()> {
    #[cfg(debug_assertions)]
    {
      self.is_sorted = false;
    }
    self.bytes_hash.reinit()
  }

  /// Called when we first encounter a new term. We must allocate slices to
  /// store the postings (vInt compressed doc/freq/prox), and also the int
  /// pointers to where (in our [`ByteBlockPool`] storage) the postings
  /// for this term begin.
  pub(crate) fn init_stream_slices(
    &mut self,
    term_id: usize,
    _doc_id: i32,
    int_pool: &mut IntBlockPool,
    byte_pool: &mut ByteBlockPool,
  ) -> Result<()> {
    {
      if self.stream_count + int_pool.int_upto > INT_BLOCK_SIZE {
        // not enough space remaining in this buffer -- jump to next buffer and lose this remaining
        // piece
        int_pool.next_buffer()?;
      }

      if BYTE_BLOCK_SIZE - byte_pool.byte_upto
        < 2 * self.stream_count * ByteSlicePool::FIRST_LEVEL_SIZE
      {
        // can we fit at least one byte per stream in the current
        // buffer, if not allocate a new one
        byte_pool.next_buffer()?;
      }
      let byte_offset = byte_pool.byte_offset;
      self.term_stream_address_buffer_index = int_pool
        .buffer_upto
        .ok_or_else(|| LuceneError::illegal_state("term stream has no current int buffer"))?;
      self.stream_address_offset = int_pool.int_upto;
      let stream_address_index = self.stream_address_offset as usize;
      int_pool.int_upto += self.stream_count;
      let postings_array_wrapper = &mut self.bytes_hash.bytes_start_array.per_field;
      debug_assert!(postings_array_wrapper.postings_array.is_some());
      postings_array_wrapper
        .postings_array
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("postings array is missing"))?
        .set_address_offset(term_id, self.stream_address_offset + int_pool.int_offset);

      let term_stream_address_buffer =
        int_pool.get_buffer_mut(self.term_stream_address_buffer_index);
      if let Some(addresses) = term_stream_address_buffer
        .get_mut(stream_address_index..)
        .and_then(|buffer| buffer.get_mut(..self.stream_count as usize))
      {
        for address in addresses {
          let upto = self
            .slice_pool
            .new_slice(ByteSlicePool::FIRST_LEVEL_SIZE, byte_pool)?;
          *address = upto + byte_offset;
        }
      } else {
        for i in 0..self.stream_count as usize {
          let upto = self
            .slice_pool
            .new_slice(ByteSlicePool::FIRST_LEVEL_SIZE, byte_pool)?;
          term_stream_address_buffer[stream_address_index + i] = upto + byte_offset;
        }
      }
      postings_array_wrapper
        .postings_array
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("postings array is missing"))?
        .set_byte_starts(term_id, term_stream_address_buffer[stream_address_index]);
    }
    Ok(())
  }

  pub(crate) fn assert_doc_id(&mut self, doc_id: i32) -> bool {
    debug_assert!(
      doc_id >= self.last_doc_id,
      "docID must be >= {} but was: {}",
      self.last_doc_id,
      doc_id
    );
    self.last_doc_id = doc_id;
    true
  }

  pub(crate) fn position_stream_slice(&mut self, term_id: i32, _doc_id: i32) -> Result<usize> {
    let term_id = (-term_id) - 1;
    let term_id = term_id as usize;
    let postings_array_wrapper = &self.bytes_hash.bytes_start_array.per_field;
    debug_assert!(postings_array_wrapper.postings_array.is_some());
    let int_start = postings_array_wrapper
      .postings_array
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("postings array is missing"))?
      .get_address_offset()[term_id];
    self.term_stream_address_buffer_index = (int_start >> INT_BLOCK_SHIFT) as usize;
    self.stream_address_offset = int_start & INT_BLOCK_MASK;
    Ok(term_id)
  }
}

pub(crate) trait TermsHashPerFieldBase {
  /// Called when a term is seen for the first time.
  fn new_term<AS>(
    &mut self,
    term_id: usize,
    doc_id: i32,
    state: &mut FieldInvertState,
    attribute_source: &AS,
    int_pool: &mut IntBlockPool,
    byte_pool: &mut ByteBlockPool,
  ) -> Result<()>
  where
    AS: AttributeSource;
  /// Called when a previously seen term is seen again.
  fn add_term<AS>(
    &mut self,
    term_id: usize,
    doc_id: i32,
    state: &mut FieldInvertState,
    attribute_source: &AS,
    int_pool: &mut IntBlockPool,
    byte_pool: &mut ByteBlockPool,
  ) -> Result<()>
  where
    AS: AttributeSource;
  /// Called when the postings array is initialized or resized.
  #[allow(dead_code)]
  fn new_postings_array(&mut self) -> Result<()> {
    Err(LuceneError::not_implemented("should nerve called"))
  }
}
pub(crate) struct PostingsArrayWrapper<PA: PostingsArray> {
  pub(crate) postings_array: Option<PA>,
  pub(crate) index_options: IndexOptions,
}

impl<PA: PostingsArray> PostingsArrayWrapper<PA> {
  pub fn new(index_options: IndexOptions) -> Self {
    Self {
      postings_array: None,
      index_options,
    }
  }
}

pub(crate) struct PostingsBytesStartArray<PA: PostingsArray> {
  pub(crate) per_field: PostingsArrayWrapper<PA>,
  bytes_used: SharedCounter,
}

impl<PA: PostingsArray> PostingsBytesStartArray<PA> {
  pub(crate) fn new(per_field: PostingsArrayWrapper<PA>, bytes_used: SharedCounter) -> Self {
    Self {
      per_field,
      bytes_used,
    }
  }

  fn postings_array(&self) -> Result<&PA> {
    debug_assert!(self.per_field.postings_array.is_some());
    self
      .per_field
      .postings_array
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("postings array is missing"))
  }

  fn postings_array_mut(&mut self) -> Result<&mut PA> {
    debug_assert!(self.per_field.postings_array.is_some());
    self
      .per_field
      .postings_array
      .as_mut()
      .ok_or_else(|| LuceneError::illegal_state("postings array is missing"))
  }
}
impl<PA: PostingsArray> BytesStartArray for PostingsBytesStartArray<PA> {
  fn init(&mut self) -> Result<()> {
    if self.per_field.postings_array.is_none() {
      self.per_field.postings_array = Some(PA::new_for_index(2, self.per_field.index_options));
      if let Some(ref mut postings_array) = self.per_field.postings_array {
        let byte_used = postings_array.bytes_per_posting() * postings_array.get_size();
        let _ = self.bytes_used.add_and_get(byte_used as i64);
      }
    }
    Ok(())
  }

  fn grow(&mut self) -> Result<()> {
    let bytes_used = {
      let postings_array = self.postings_array_mut()?;
      let old_size = postings_array.get_size();
      postings_array.grow()?;
      postings_array.bytes_per_posting() * (postings_array.get_size() - old_size)
    };
    self.bytes_used.add_and_get(bytes_used as i64);
    Ok(())
  }

  fn clear(&mut self) {
    if let Some(postings_array) = self.per_field.postings_array.take() {
      let byte_used = postings_array.bytes_per_posting() * postings_array.get_size();
      debug_assert!(byte_used <= i64::MAX as usize);
      let _ = self.bytes_used.add_and_get(-(byte_used as i64));
    }
  }

  fn bytes_used(&mut self) -> SharedCounter {
    self.bytes_used.clone()
  }

  fn get_value(&self, index: usize) -> Result<i32> {
    let postings_array = self.postings_array()?;
    postings_array
      .get_text_starts()
      .get(index)
      .copied()
      .ok_or_else(|| {
        LuceneError::illegal_argument(format!("postings array index {index} is out of bounds"))
      })
  }

  fn set_value(&mut self, index: usize, value: i32) -> Result<()> {
    let postings_array = self.postings_array_mut()?;
    if index >= postings_array.get_text_starts().len() {
      return Err(LuceneError::illegal_argument(format!(
        "postings array index {index} is out of bounds"
      )));
    }
    postings_array.set_text_starts(index, value);
    Ok(())
  }

  fn len(&self) -> Result<usize> {
    let postings_array = self.postings_array()?;
    Ok(postings_array.get_text_starts().len())
  }

  fn need_init(&self) -> bool {
    self.per_field.postings_array.is_none()
  }

  fn ram_bytes_used(&self) -> Result<i64> {
    match self.per_field.postings_array.as_ref() {
      Some(postings_array) => Ok(size_of_slice(postings_array.get_text_starts())),
      None => Ok(0),
    }
  }
}

pub(crate) const HASH_INIT_SIZE: usize = 4;
