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
use crate::core::codecs::term_vectors_writer::TermVectorsWriter;
use crate::core::index::BytesRef;
use crate::core::index::byte_slice_reader::ByteSliceReader;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_invert_state::FieldInvertState;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::index::parallel_postings_array::{
  ParallelPostingsArray, PostingsArray, PostingsArrayBase,
};
use crate::core::index::term_vectors_consumer::{PerFieldMeta, TermVectorsConsumer};
use crate::core::index::terms_hash_per_field::{
  PostingsArrayWrapper, TermsHashPerField, TermsHashPerFieldBase,
};
use crate::core::store::directory::Directory;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::attribute_source::AttributeSource;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bytes_ref_block_pool::BytesRefBlockPool;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::int_block_pool::IntBlockPool;
use crate::core::util::{ByteBlockPool, SharedCounter};

pub(crate) struct TermVectorOptions {
  vectors: bool,
  positions: bool,
  offsets: bool,
  payloads: bool,
}

impl TermVectorOptions {
  pub(crate) fn from_field_type(field_type: &impl IndexableFieldType) -> Self {
    Self {
      vectors: field_type.store_term_vectors(),
      positions: field_type.store_term_vector_positions(),
      offsets: field_type.store_term_vector_offsets(),
      payloads: field_type.store_term_vector_payloads(),
    }
  }
}

pub(crate) struct TermVectorsConsumerPerField {
  pub(super) do_vectors: bool,
  do_vector_positions: bool,
  do_vector_offsets: bool,
  do_vector_payloads: bool,
  term_byte_pool: BytesRefBlockPool,
  has_payloads: bool,
  base: Option<Box<TermsHashPerField<TermVectorsPostingsArray>>>,
}
impl TermVectorsConsumerPerField {
  fn missing_postings_state() -> LuceneError {
    LuceneError::illegal_state("term vector postings state is missing")
  }

  fn new_postings_state(
    field_info: &FieldInfo,
    bytes_used: &SharedCounter,
  ) -> Result<Box<TermsHashPerField<TermVectorsPostingsArray>>> {
    Ok(Box::new(TermsHashPerField::new(
      2,
      bytes_used.clone(),
      PostingsArrayWrapper::new(field_info.index_options),
      field_info.index_options,
      true,
    )?))
  }
  pub(crate) fn new<D>(terms_hash: &TermVectorsConsumer<D>, field_info: &FieldInfo) -> Result<Self>
  where
    D: Directory,
  {
    let bytes_used = &terms_hash.base.bytes_used;
    // Keep validation state for every indexed field, but allocate the postings
    // state only for a field that stores term vectors.
    let base = if field_info.has_term_vectors() {
      Some(Self::new_postings_state(field_info, bytes_used)?)
    } else {
      None
    };
    Ok(Self {
      do_vectors: false,
      do_vector_positions: false,
      do_vector_offsets: false,
      do_vector_payloads: false,
      term_byte_pool: BytesRefBlockPool::new(),
      has_payloads: false,
      base,
    })
  }

  pub(crate) fn finish_document<D>(
    &mut self,
    term_vectors_consumer: &mut TermVectorsConsumer<D>,
    field_info: &FieldInfo,
    int_pool: &mut IntBlockPool,
    byte_pool: &ByteBlockPool,
    term_byte_pool: &ByteBlockPool,
  ) -> Result<()>
  where
    D: Directory + Clone,
  {
    if !self.do_vectors {
      return Ok(());
    }
    self.do_vectors = false;

    term_vectors_consumer.write_per_field(self, field_info, int_pool, byte_pool, term_byte_pool)
  }

  pub(crate) fn write_to_writer<TW>(
    &mut self,
    tv: &mut TW,
    field_info: &FieldInfo,
    int_pool: &mut IntBlockPool,
    byte_pool: &ByteBlockPool,
    term_byte_pool: &ByteBlockPool,
  ) -> Result<()>
  where
    TW: TermVectorsWriter,
  {
    let base = self
      .base
      .as_deref_mut()
      .ok_or_else(Self::missing_postings_state)?;
    let num_postings = base.get_num_terms();

    base.sort_terms(term_byte_pool)?;
    let term_ids = base.get_sorted_term_ids();

    tv.start_field(
      field_info,
      num_postings,
      self.do_vector_positions,
      self.do_vector_offsets,
      self.has_payloads,
    )?;
    let postings = base
      .postings_array()
      .ok_or_else(|| LuceneError::illegal_state("postings_array not initialized"))?;
    let mut off_reader = ByteSliceReader::new(byte_pool);
    let mut pos_reader = ByteSliceReader::new(byte_pool);
    for &term_id in &term_ids[..num_postings] {
      let term_id = term_id as usize;
      let freq = postings.term_data[term_id][0];
      let position = self
        .term_byte_pool
        .fill_bytes_ref(postings.parent.starts[term_id][0], term_byte_pool);
      let block = term_byte_pool.get_buffer(position.block_index);
      let flush_term = BytesRef {
        bytes: &block[position.offset..position.offset + position.length],
        offset: 0,
        length: position.length,
      };
      tv.start_term(&flush_term, freq)?;

      if self.do_vector_positions || self.do_vector_offsets {
        let positions = if self.do_vector_positions {
          base.init_reader(&mut pos_reader, term_id, 0, int_pool)?;
          Some(&mut pos_reader)
        } else {
          None
        };
        let offsets = if self.do_vector_offsets {
          base.init_reader(&mut off_reader, term_id, 1, int_pool)?;
          Some(&mut off_reader)
        } else {
          None
        };
        tv.add_prox(freq as usize, positions, offsets)?;
      }

      tv.finish_term()?;
    }

    tv.finish_field()?;
    field_info.set_store_term_vectors()?;
    Ok(())
  }
  pub(crate) fn reset(&mut self, byte_pool: &mut ByteBlockPool) {
    if let Some(base) = self.base.as_deref_mut() {
      base.reset(byte_pool);
    }
  }
  // Secondary entry point (for 2nd & subsequent TermsHash),
  // because token text has already been "interned" into
  // textStart, so we hash by textStart.  term vectors use
  // this API.
  #[allow(clippy::too_many_arguments)]
  pub(crate) fn add_with_text_start<AS>(
    &mut self,
    text_start: i32,
    doc_id: i32,
    state: &mut FieldInvertState,
    attribute_source: &AS,
    int_pool: &mut IntBlockPool,
    byte_pool: &mut ByteBlockPool,
    term_byte_pool: &mut ByteBlockPool,
  ) -> Result<()>
  where
    AS: AttributeSource,
  {
    let base = self
      .base
      .as_deref_mut()
      .ok_or_else(Self::missing_postings_state)?;
    let term_id = base
      .bytes_hash
      .add_by_pool_offset(text_start, term_byte_pool)?;
    if term_id >= 0 {
      let term_id = term_id as usize;
      // First time we are seeing this token since we last
      // flushed the hash.
      base.init_stream_slices(term_id, doc_id, int_pool, byte_pool)?;
      self.new_term(
        term_id,
        doc_id,
        state,
        attribute_source,
        int_pool,
        byte_pool,
      )?;
    } else {
      let term_id = base.position_stream_slice(term_id, doc_id)?;
      self.add_term(
        term_id,
        doc_id,
        state,
        attribute_source,
        int_pool,
        byte_pool,
      )?;
    }
    Ok(())
  }
  #[inline(always)]
  pub(crate) fn start(
    &mut self,
    field_info: &FieldInfo,
    options: TermVectorOptions,
    first: bool,
    bytes_used: &SharedCounter,
    byte_pool: &mut ByteBlockPool,
  ) -> Result<bool> {
    let field_name = field_info.get_name();
    if first {
      if let Some(base) = self.base.as_deref_mut()
        && base.get_num_terms() != 0
      {
        // The previous document hit a non-aborting error writing these vectors.
        base.reset(byte_pool);
      }

      self.has_payloads = false;

      self.do_vectors = options.vectors;

      if self.do_vectors {
        if let Some(base) = self.base.as_deref_mut() {
          base.reinit_hash()?;
        } else {
          self.base = Some(Self::new_postings_state(field_info, bytes_used)?);
        }
        self.do_vector_positions = options.positions;
        // Somewhat confusingly, unlike postings, you are
        // allowed to index TV offsets without TV positions:
        self.do_vector_offsets = options.offsets;

        if self.do_vector_positions {
          self.do_vector_payloads = options.payloads;
        } else {
          self.do_vector_payloads = false;
          if options.payloads {
            return Err(LuceneError::illegal_argument(format!(
              "cannot index term vector payloads without term vector positions (field=\"{}\")",
              field_name
            )));
          }
        }
      } else {
        if options.offsets {
          return Err(LuceneError::illegal_argument(format!(
            "cannot index term vector offsets when term vectors are not indexed (field=\"{}\")",
            field_name
          )));
        }
        if options.positions {
          return Err(LuceneError::illegal_argument(format!(
            "cannot index term vector positions when term vectors are not indexed (field=\"{}\")",
            field_name
          )));
        }
        if options.payloads {
          return Err(LuceneError::illegal_argument(format!(
            "cannot index term vector payloads when term vectors are not indexed (field=\"{}\")",
            field_name
          )));
        }
      }
    } else {
      if self.do_vectors != options.vectors {
        return Err(LuceneError::illegal_argument(format!(
          "all instances of a given field name must have the same term vectors settings (storeTermVectors changed for field=\"{}\")",
          field_name
        )));
      }
      if self.do_vector_positions != options.positions {
        return Err(LuceneError::illegal_argument(format!(
          "all instances of a given field name must have the same term vectors settings (storeTermVectorPositions changed for field=\"{}\")",
          field_name
        )));
      }
      if self.do_vector_offsets != options.offsets {
        return Err(LuceneError::illegal_argument(format!(
          "all instances of a given field name must have the same term vectors settings (storeTermVectorOffsets changed for field=\"{}\")",
          field_name
        )));
      }
      if self.do_vector_payloads != options.payloads {
        return Err(LuceneError::illegal_argument(format!(
          "all instances of a given field name must have the same term vectors settings (storeTermVectorPayloads changed for field=\"{}\")",
          field_name
        )));
      }
    }
    Ok(self.do_vectors)
  }
  pub(crate) fn write_prox<AS>(
    &mut self,
    term_id: usize,
    field_state: &FieldInvertState,
    attribute_source: &AS,
    int_pool: &mut IntBlockPool,
    byte_pool: &mut ByteBlockPool,
  ) -> Result<()>
  where
    AS: AttributeSource,
  {
    let base = self
      .base
      .as_deref_mut()
      .ok_or_else(Self::missing_postings_state)?;
    let postings = base
      .postings_array()
      .ok_or_else(|| LuceneError::illegal_state("postings_array not initialized"))?;
    let mut last_offset = None;
    let mut last_position = None;
    if self.do_vector_offsets {
      let start = attribute_source.start_offset()?;
      let end = attribute_source.end_offset()?;

      let start_offset = field_state.offset + start;
      let end_offset = field_state.offset + end;

      base.write_vint(
        1,
        start_offset - postings.term_data[term_id][1],
        int_pool,
        byte_pool,
      )?;
      base.write_vint(1, end_offset - start_offset, int_pool, byte_pool)?;
      last_offset = Some(end_offset);
    }

    if self.do_vector_positions {
      let pos = field_state.position - postings.term_data[term_id][2];

      if self.do_vector_payloads
        && let Some(payload) = attribute_source.get_payload()?
      {
        if payload.length > 0 {
          base.write_vint(0, (pos << 1) | 1, int_pool, byte_pool)?;
          base.write_vint(0, payload.length as i32, int_pool, byte_pool)?;
          base.write_bytes(
            0,
            &payload.bytes,
            payload.offset,
            payload.length,
            int_pool,
            byte_pool,
          )?;
          self.has_payloads = true;
        } else {
          base.write_vint(0, pos << 1, int_pool, byte_pool)?;
        }
      } else {
        base.write_vint(0, pos << 1, int_pool, byte_pool)?;
      }

      last_position = Some(field_state.position);
    }
    let postings = base
      .postings_array_mut()
      .ok_or_else(|| LuceneError::illegal_state("postings_array not initialized"))?;
    if let Some(offset) = last_offset {
      postings.term_data[term_id][1] = offset;
    }
    if let Some(pos) = last_position {
      postings.term_data[term_id][2] = pos;
    }

    Ok(())
  }
  pub(crate) fn get_term_freq<AS>(&self, field_name: &str, attribute_source: &AS) -> Result<i32>
  where
    AS: AttributeSource,
  {
    let freq = attribute_source.get_term_frequency().unwrap_or(1);

    if freq != 1 {
      if self.do_vector_positions {
        return Err(LuceneError::illegal_argument(format!(
          "field \"{}\": cannot index term vector positions while using custom TermFrequencyAttribute",
          field_name
        )));
      }
      if self.do_vector_offsets {
        return Err(LuceneError::illegal_argument(format!(
          "field \"{}\": cannot index term vector offsets while using custom TermFrequencyAttribute",
          field_name
        )));
      }
    }

    Ok(freq)
  }
  pub(crate) fn finish<D>(
    &self,
    term_vectors_consumer: &mut TermVectorsConsumer<D>,
    field_index: usize,
  ) -> Result<()>
  where
    D: Directory + Clone,
  {
    if !self.do_vectors
      || self
        .base
        .as_ref()
        .is_none_or(|base| base.get_num_terms() == 0)
    {
      return Ok(());
    }
    term_vectors_consumer.add_field_to_flush(PerFieldMeta { idx: field_index })
  }
}
impl TermsHashPerFieldBase for TermVectorsConsumerPerField {
  fn new_term<AS>(
    &mut self,
    term_id: usize,
    _doc_id: i32,
    field_state: &mut FieldInvertState,
    attribute_source: &AS,
    int_pool: &mut IntBlockPool,
    byte_pool: &mut ByteBlockPool,
  ) -> Result<()>
  where
    AS: AttributeSource,
  {
    let freq = self.get_term_freq(field_state.get_name(), attribute_source)?;
    let base = self
      .base
      .as_deref_mut()
      .ok_or_else(Self::missing_postings_state)?;
    let postings = base
      .postings_array_mut()
      .ok_or_else(|| LuceneError::illegal_state("postings_array not initialized"))?;
    postings.term_data[term_id][0] = freq;
    postings.term_data[term_id][1] = 0;
    postings.term_data[term_id][2] = 0;

    self.write_prox(term_id, field_state, attribute_source, int_pool, byte_pool)?;
    Ok(())
  }

  fn add_term<AS>(
    &mut self,
    term_id: usize,
    _doc_id: i32,
    state: &mut FieldInvertState,
    attribute_source: &AS,
    int_pool: &mut IntBlockPool,
    byte_pool: &mut ByteBlockPool,
  ) -> Result<()>
  where
    AS: AttributeSource,
  {
    let freq = self.get_term_freq(state.get_name(), attribute_source)?;
    let base = self
      .base
      .as_deref_mut()
      .ok_or_else(Self::missing_postings_state)?;
    let postings = base
      .postings_array_mut()
      .ok_or_else(|| LuceneError::illegal_state("postings_array not initialized"))?;

    postings.term_data[term_id][0] += freq;
    self.write_prox(term_id, state, attribute_source, int_pool, byte_pool)?;

    Ok(())
  }
}

pub(crate) struct TermVectorsPostingsArray {
  // Per-term [frequency, last offset, last position], without element padding.
  term_data: Vec<[i32; 3]>,
  pub(crate) parent: ParallelPostingsArray,
}

impl TermVectorsPostingsArray {
  pub fn new(size: usize) -> Self {
    TermVectorsPostingsArray {
      term_data: vec![[0; 3]; size],
      parent: ParallelPostingsArray::new(size),
    }
  }
}

impl PostingsArrayBase for TermVectorsPostingsArray {
  fn bytes_per_posting(&self) -> usize {
    self.parent.bytes_per_posting() + 3 * BitUtil::INT_BYTES
  }
  fn copy_to(&mut self, new_size: usize) -> Result<()> {
    self.parent.copy_to(new_size)?;
    ArrayUtil::grow_exact(&mut self.term_data, new_size, || [0; 3])?;
    Ok(())
  }
}

impl PostingsArray for TermVectorsPostingsArray {
  fn new_for_index(size: usize, _index_options: IndexOptions) -> Self {
    Self::new(size)
  }
  fn get_address_offset(&self) -> &[[i32; 3]] {
    self.parent.get_address_offset()
  }
  fn set_address_offset(&mut self, index: usize, value: i32) {
    self.parent.set_address_offset(index, value);
  }
  fn get_text_starts(&self) -> &[[i32; 3]] {
    self.parent.get_text_starts()
  }
  fn set_text_starts(&mut self, index: usize, value: i32) {
    self.parent.set_text_starts(index, value);
  }
  fn get_byte_starts(&self) -> &[[i32; 3]] {
    self.parent.get_byte_starts()
  }
  fn set_byte_starts(&mut self, index: usize, value: i32) {
    self.parent.set_byte_starts(index, value);
  }
}
