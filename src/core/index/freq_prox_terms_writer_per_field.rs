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
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_invert_state::FieldInvertState;
use crate::core::index::freq_prox_terms_writer::FreqProxTermsWriter;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::indexing_chain::IndexContext;
use crate::core::index::parallel_postings_array::{
  ParallelPostingsArray, PostingsArray, PostingsArrayBase,
};
use crate::core::index::term_vectors_consumer::TermVectorsConsumer;
use crate::core::index::term_vectors_consumer_per_field::{
  TermVectorOptions, TermVectorsConsumerPerField,
};
use crate::core::index::terms_hash_per_field::{
  PostingsArrayWrapper, TermsHashPerField, TermsHashPerFieldBase,
};
use crate::core::index::{BytesRef, BytesRefValue};
use crate::core::store::directory::Directory;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::attribute_source::AttributeSource;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::int_block_pool::IntBlockPool;
use crate::core::util::{ByteBlockPool, SharedCounter, ToInt};
#[cfg(test)]
use crate::test_framework::core::index::test_terms_hash_per_field::TermsHashPerFieldMock;
use std::cmp::Ordering;
use std::sync::Arc;

pub(crate) struct FreqProxTermsWriterPerField {
  pub(super) field_info: Arc<FieldInfo>,
  pub(crate) has_freq: bool,
  pub(crate) has_prox: bool,
  pub(crate) has_offsets: bool,
  // Set to true if any token had a payload in the current segment.
  pub(crate) saw_payloads: bool,

  pub(crate) next_per_field: Option<TermVectorsConsumerPerField>,

  pub(crate) base: TermsHashPerField<FreqProxPostingsArray>,
}
impl FreqProxTermsWriterPerField {
  pub fn new<D>(
    terms_hash: &FreqProxTermsWriter<D>,
    field_info: Arc<FieldInfo>,
    next_per_field: Option<TermVectorsConsumerPerField>,
  ) -> Result<FreqProxTermsWriterPerField>
  where
    D: Directory,
  {
    let index_options = *field_info.get_index_options();

    let has_freq = index_options >= IndexOptions::DocsAndFreqs;
    let has_prox = index_options >= IndexOptions::DocsAndFreqsAndPositions;
    let has_offsets = index_options >= IndexOptions::DocsAndFreqsAndPositionsAndOffsets;

    let saw_payloads = false;

    let stream_count = if index_options
      .cmp(&IndexOptions::DocsAndFreqsAndPositions)
      .to_int()
      >= 0
    {
      2
    } else {
      1
    };
    let postings_array_wrapper = PostingsArrayWrapper::new(index_options);
    let base = TermsHashPerField::new(
      stream_count,
      terms_hash.base.bytes_used.clone(),
      postings_array_wrapper,
      index_options,
      true,
    )?;
    Ok(FreqProxTermsWriterPerField {
      field_info,
      has_freq,
      has_prox,
      has_offsets,
      saw_payloads,
      next_per_field,
      base,
    })
  }
  pub(crate) fn write_prox<AS>(
    &mut self,
    term_id: usize,
    prox_code: i32,
    field_state: &FieldInvertState,
    attribute_source: &AS,
    int_pool: &mut IntBlockPool,
    byte_pool: &mut ByteBlockPool,
  ) -> Result<()>
  where
    AS: AttributeSource,
  {
    if let Some(payload) = attribute_source.get_payload()? {
      if payload.length > 0 {
        self
          .base
          .write_vint(1, (prox_code << 1) | 1, int_pool, byte_pool)?;
        self
          .base
          .write_vint(1, payload.length as i32, int_pool, byte_pool)?;
        self.base.write_bytes(
          1,
          &payload.bytes,
          payload.offset,
          payload.length,
          int_pool,
          byte_pool,
        )?;
        self.saw_payloads = true;
      } else {
        self
          .base
          .write_vint(1, prox_code << 1, int_pool, byte_pool)?;
      }
    } else {
      self
        .base
        .write_vint(1, prox_code << 1, int_pool, byte_pool)?;
    }
    let postings_array = self
      .base
      .postings_array_mut()
      .ok_or_else(|| LuceneError::illegal_state("postings_array not initialized"))?;
    postings_array
      .last_positions
      .as_mut()
      .ok_or_else(|| LuceneError::illegal_state("last_positions not initialized"))?[term_id] =
      field_state.position();

    Ok(())
  }
  pub(crate) fn write_offsets<AS>(
    &mut self,
    term_id: usize,
    offset_accum: i32,
    attribute_source: &AS,
    int_pool: &mut IntBlockPool,
    byte_pool: &mut ByteBlockPool,
  ) -> Result<()>
  where
    AS: AttributeSource,
  {
    let start = attribute_source.start_offset()?;
    let end = attribute_source.end_offset()?;

    let start_offset = offset_accum + start;
    let end_offset = offset_accum + end;

    let postings_array = self
      .base
      .postings_array_mut()
      .ok_or_else(|| LuceneError::illegal_state("postings_array not initialized"))?;

    let last_offsets = postings_array
      .last_offsets
      .as_mut()
      .ok_or_else(|| LuceneError::illegal_state("last_offsets not available"))?;
    let last_offset = last_offsets[term_id];

    debug_assert!(
      start_offset - last_offset >= 0,
      "start_offset must not go backwards"
    );
    let v1 = start_offset - last_offset;
    let v2 = end_offset - start_offset;

    last_offsets[term_id] = start_offset;

    self.base.write_vint(1, v1, int_pool, byte_pool)?;
    self.base.write_vint(1, v2, int_pool, byte_pool)?;

    Ok(())
  }
  fn get_term_freq<AS>(&self, attribute_source: &AS) -> Result<i32>
  where
    AS: AttributeSource,
  {
    let freq = attribute_source.get_term_frequency().unwrap_or(1);

    if freq != 1 && self.has_prox {
      return Err(LuceneError::illegal_state(format!(
        "field \"{}\": cannot index positions while using custom TermFrequencyAttribute",
        self.field_info.name
      )));
    }

    Ok(freq)
  }
  pub(crate) fn finish<D>(
    &mut self,
    term_vectors_consumer: &mut TermVectorsConsumer<D>,
    field_index: usize,
  ) -> Result<()>
  where
    D: Directory + Clone,
  {
    if let Some(next_per_field) = self.next_per_field.as_mut() {
      next_per_field.finish(term_vectors_consumer, field_index)?;
    }

    if self.saw_payloads {
      self.field_info.set_store_payloads()?;
    }
    Ok(())
  }
  #[allow(dead_code)]
  pub(crate) fn reset(&mut self, byte_pool: &mut ByteBlockPool) {
    self.base.reset(byte_pool);
    if let Some(next_per_field) = self.next_per_field.as_mut() {
      next_per_field.reset(byte_pool);
    }
  }
  /// Called once per inverted token. This is the primary entry point (for
  /// first TermsHash); postings use this API.
  pub(crate) fn add_with_bytes_ref<AS>(
    &mut self,
    term_bytes: Option<&BytesRef<&[u8]>>,
    doc_id: i32,
    field_state: &mut FieldInvertState,
    attribute_source: &mut AS,
    context: &mut IndexContext,
  ) -> Result<()>
  where
    AS: AttributeSource,
  {
    self.add_with_bytes_ref_mode::<true, AS>(
      term_bytes,
      doc_id,
      field_state,
      attribute_source,
      context,
    )
  }
  // The false mode is selected only after start has disabled vectors for this value.
  pub(crate) fn add_with_bytes_ref_mode<const VECTORS: bool, AS>(
    &mut self,
    term_bytes: Option<&BytesRef<&[u8]>>,
    doc_id: i32,
    field_state: &mut FieldInvertState,
    attribute_source: &mut AS,
    context: &mut IndexContext,
  ) -> Result<()>
  where
    AS: AttributeSource,
  {
    #[cfg(debug_assertions)]
    debug_assert!(self.base.assert_doc_id(doc_id));
    // We are first in the chain so we must "intern" the
    // term text into textStart address
    // Get the text & hash of this term.
    let bytes = attribute_source.get_bytes_ref()?;
    let fallback;
    let term_bytes = if let Some(t) = term_bytes {
      t
    } else {
      fallback = bytes
        .ok_or_else(|| {
          LuceneError::illegal_state("term bytes and attribute source bytes are both None")
        })?
        .as_bytes_ref();
      &fallback
    };

    let term_id = self
      .base
      .bytes_hash
      .add(term_bytes, &mut context.byte_pool)?;
    let term_id = if term_id >= 0 {
      let term_id = term_id as usize;
      self.base.init_stream_slices(
        term_id,
        doc_id,
        &mut context.freq_prox_term_int_pool,
        &mut context.byte_pool,
      )?;
      self.new_term(
        term_id,
        doc_id,
        field_state,
        attribute_source,
        &mut context.freq_prox_term_int_pool,
        &mut context.byte_pool,
      )?;
      term_id
    } else {
      let term_id = self.base.position_stream_slice(term_id, doc_id)?;
      self.add_term(
        term_id,
        doc_id,
        field_state,
        attribute_source,
        &mut context.freq_prox_term_int_pool,
        &mut context.byte_pool,
      )?;
      term_id
    };

    if VECTORS
      && let Some(ref mut next_per_field) = self.next_per_field
      && next_per_field.do_vectors
    {
      let text_start = self
        .base
        .postings_array()
        .ok_or_else(|| LuceneError::illegal_state("postings_array not initialized"))?
        .get_text_starts()[term_id][0];
      next_per_field.add_with_text_start(
        text_start,
        doc_id,
        field_state,
        attribute_source,
        &mut context.term_vectors_int_pool,
        &mut context.term_vectors_byte_pool,
        &mut context.byte_pool,
      )?;
    }
    Ok(())
  }
  #[cfg(test)]
  #[allow(clippy::too_many_arguments)]
  pub(crate) fn add_with_bytes_ref_with_test<AS>(
    &mut self,
    term_bytes: &BytesRef<Vec<u8>>,
    doc_id: i32,
    sub: &mut TermsHashPerFieldMock,
    attribute_source: &AS,
    int_pool: &mut IntBlockPool,
    byte_pool: &mut ByteBlockPool,
    next_byte_pool: &mut ByteBlockPool,
  ) -> Result<()>
  where
    AS: AttributeSource,
  {
    #[cfg(debug_assertions)]
    debug_assert!(self.base.assert_doc_id(doc_id));
    // We are first in the chain so we must "intern" the
    // term text into textStart address
    // Get the text & hash of this term.
    let term_id = self.base.bytes_hash.add(term_bytes, byte_pool)?;
    let term_id = if term_id >= 0 {
      let term_id = term_id as usize;
      self
        .base
        .init_stream_slices(term_id, doc_id, int_pool, byte_pool)?;
      sub.new_term(term_id, doc_id, &mut self.base)?;
      term_id
    } else {
      let term_id = self.base.position_stream_slice(term_id, doc_id)?;
      sub.add_term(term_id, doc_id, &mut self.base, int_pool, byte_pool)?;
      term_id
    };

    if let Some(ref mut next_per_field) = self.next_per_field
      && next_per_field.do_vectors
    {
      let text_start = self
        .base
        .postings_array()
        .ok_or_else(|| LuceneError::illegal_state("postings_array not initialized"))?
        .get_text_starts()[term_id][0];
      next_per_field.add_with_text_start(
        text_start,
        doc_id,
        &mut sub.field_state,
        attribute_source,
        int_pool,
        next_byte_pool,
        byte_pool,
      )?;
    }
    Ok(())
  }
  pub(crate) fn start(
    &mut self,
    vector_options: TermVectorOptions,
    first: bool,
    bytes_used: &SharedCounter,
    byte_pool: &mut ByteBlockPool,
  ) -> Result<bool> {
    match self.next_per_field {
      Some(ref mut next_per_field) => next_per_field.start(
        self.field_info.as_ref(),
        vector_options,
        first,
        bytes_used,
        byte_pool,
      ),
      None => Ok(false),
    }
  }
}
impl TermsHashPerFieldBase for FreqProxTermsWriterPerField {
  fn new_term<AS>(
    &mut self,
    term_id: usize,
    doc_id: i32,
    field_state: &mut FieldInvertState,
    attribute_source: &AS,
    int_pool: &mut IntBlockPool,
    byte_pool: &mut ByteBlockPool,
  ) -> Result<()>
  where
    AS: AttributeSource,
  {
    // First time we're seeing this term since the last
    // flush
    let tf = self.get_term_freq(attribute_source)?;
    let postings = self
      .base
      .postings_array_mut()
      .ok_or_else(|| LuceneError::illegal_state("postings_array not initialized"))?;

    postings.last_docs[term_id][0] = doc_id;

    if !self.has_freq {
      debug_assert!(postings.term_freqs.is_none());
      postings.last_docs[term_id][1] = doc_id;
      field_state.max_term_frequency = field_state.max_term_frequency.max(1);
    } else {
      postings.last_docs[term_id][1] = doc_id << 1;
      postings
        .term_freqs
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("term_freqs not initialized"))?[term_id] = tf;

      if self.has_prox {
        self.write_prox(
          term_id,
          field_state.position,
          field_state,
          attribute_source,
          int_pool,
          byte_pool,
        )?;
        if self.has_offsets {
          self.write_offsets(
            term_id,
            field_state.offset,
            attribute_source,
            int_pool,
            byte_pool,
          )?;
        }
      } else {
        debug_assert!(!self.has_offsets);
      }

      field_state.max_term_frequency = field_state.max_term_frequency.max(tf);
    }
    field_state.unique_term_count += 1;

    Ok(())
  }

  fn add_term<AS>(
    &mut self,
    term_id: usize,
    doc_id: i32,
    field_state: &mut FieldInvertState,
    attribute_source: &AS,
    int_pool: &mut IntBlockPool,
    byte_pool: &mut ByteBlockPool,
  ) -> Result<()>
  where
    AS: AttributeSource,
  {
    let tf = self.get_term_freq(attribute_source)?;
    let postings = self
      .base
      .postings_array_mut()
      .ok_or_else(|| LuceneError::illegal_state("postings_array not initialized"))?;
    let mut v = [None; 2];
    if self.has_freq {
      debug_assert!(
        postings
          .term_freqs
          .as_ref()
          .ok_or_else(|| LuceneError::illegal_state("term_freqs not initialized"))?[term_id]
          > 0
      );
    }

    if !self.has_freq {
      debug_assert!(postings.term_freqs.is_none());

      if tf != 1 {
        return Err(LuceneError::illegal_state(format!(
          "field \"{}\": must index term freq while using custom TermFrequencyAttribute",
          self.field_info.name
        )));
      }

      if doc_id != postings.last_docs[term_id][0] {
        debug_assert!(doc_id > postings.last_docs[term_id][0]);
        v[0] = Some(postings.last_docs[term_id][1]);
        postings.last_docs[term_id][1] = doc_id - postings.last_docs[term_id][0];
        postings.last_docs[term_id][0] = doc_id;
        field_state.unique_term_count += 1;
      }
      // Due to borrow conflict, we add later before handle prox/offset
      for x in v.into_iter().flatten() {
        self.base.write_vint(0, x, int_pool, byte_pool)?
      }
    } else if doc_id != postings.last_docs[term_id][0] {
      debug_assert!(
        doc_id > postings.last_docs[term_id][0],
        "docID = {}, postingsID = {}, termID = {}",
        doc_id,
        postings.last_docs[term_id][0],
        term_id
      );

      let freq = postings
        .term_freqs
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("term_freqs not initialized"))?[term_id];
      // Term not yet seen in the current doc but previously
      // seen in other doc(s) since the last flush

      // Now that we know doc freq for previous doc,
      // write it & lastDocCode
      if freq == 1 {
        v[0] = Some(postings.last_docs[term_id][1] | 1);
      } else {
        v[0] = Some(postings.last_docs[term_id][1]);
        v[1] = Some(freq);
      }

      // Init freq for the current document
      postings
        .term_freqs
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("term_freqs not initialized"))?[term_id] = tf;

      field_state.max_term_frequency = field_state.max_term_frequency.max(tf);

      postings.last_docs[term_id][1] = (doc_id - postings.last_docs[term_id][0]) << 1;
      postings.last_docs[term_id][0] = doc_id;

      if self.has_prox && self.has_offsets {
        postings
          .last_offsets
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("last_offsets not initialized"))?[term_id] = 0;
      }
      // Due to borrow conflict, we add later before handle prox/offset
      for x in v.into_iter().flatten() {
        self.base.write_vint(0, x, int_pool, byte_pool)?
      }
      if self.has_prox {
        self.write_prox(
          term_id,
          field_state.position,
          field_state,
          attribute_source,
          int_pool,
          byte_pool,
        )?;
        if self.has_offsets {
          self.write_offsets(
            term_id,
            field_state.offset,
            attribute_source,
            int_pool,
            byte_pool,
          )?;
        }
      } else {
        debug_assert!(!self.has_offsets);
      }
      field_state.unique_term_count += 1;
    } else {
      let term_freqs = postings
        .term_freqs
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("term_freqs not initialized"))?;
      let term_freq = &mut term_freqs[term_id];
      let freq = term_freq
        .checked_add(tf)
        .ok_or_else(|| LuceneError::illegal_state("term frequency overflow"))?;
      *term_freq = freq;

      field_state.max_term_frequency = field_state.max_term_frequency.max(freq);

      if self.has_prox {
        let delta = field_state.position
          - postings
            .last_positions
            .as_ref()
            .ok_or_else(|| LuceneError::illegal_state("last_positions not initialized"))?[term_id];
        self.write_prox(
          term_id,
          delta,
          field_state,
          attribute_source,
          int_pool,
          byte_pool,
        )?;
        if self.has_offsets {
          self.write_offsets(
            term_id,
            field_state.offset,
            attribute_source,
            int_pool,
            byte_pool,
          )?;
        }
      }
    }
    Ok(())
  }
}

impl Eq for FreqProxTermsWriterPerField {}

impl PartialEq for FreqProxTermsWriterPerField {
  fn eq(&self, _other: &Self) -> bool {
    self.cmp(_other) == Ordering::Equal
  }
}

impl PartialOrd<Self> for FreqProxTermsWriterPerField {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

impl Ord for FreqProxTermsWriterPerField {
  fn cmp(&self, other: &Self) -> Ordering {
    self.field_info.name.cmp(&other.field_info.name)
  }
}

pub(crate) struct FreqProxPostingsArray {
  pub(crate) term_freqs: Option<Vec<i32>>, /* # times this term occurs in
                                            * the current doc  */
  // Per-term [last document ID, prior document code], without element padding.
  pub(crate) last_docs: Vec<[i32; 2]>,
  pub(crate) last_positions: Option<Vec<i32>>, /* Last position where this term
                                                * occurred  */
  pub(crate) last_offsets: Option<Vec<i32>>, // Last endOffset where this term occurred
  pub(crate) parent: ParallelPostingsArray,
}
impl FreqProxPostingsArray {
  // Creates FreqProxPostingsArray
  pub(crate) fn new(size: usize, write_freqs: bool, write_prox: bool, write_offsets: bool) -> Self {
    let mut term_freqs = None;
    if write_freqs {
      term_freqs = Some(vec![0; size]);
    }
    let last_positions = if write_prox {
      Some(vec![0; size])
    } else {
      debug_assert!(!write_offsets);
      None
    };
    let last_offsets = if write_prox && write_offsets {
      Some(vec![0; size])
    } else {
      None
    };
    FreqProxPostingsArray {
      term_freqs,
      last_docs: vec![[0; 2]; size],
      last_positions,
      last_offsets,
      parent: ParallelPostingsArray::new(size),
    }
  }
}

impl PostingsArrayBase for FreqProxPostingsArray {
  fn bytes_per_posting(&self) -> usize {
    let i32_bytes = BitUtil::INT_BYTES;
    let mut bytes = ParallelPostingsArray::BYTES_PER_POSTING + 2 * i32_bytes;

    if self.last_positions.is_some() {
      bytes += i32_bytes;
    }
    if self.last_offsets.is_some() {
      bytes += i32_bytes;
    }
    if self.term_freqs.is_some() {
      bytes += i32_bytes;
    }
    bytes
  }

  fn copy_to(&mut self, new_size: usize) -> Result<()> {
    self.parent.copy_to(new_size)?;
    ArrayUtil::grow_exact(&mut self.last_docs, new_size, || [0; 2])?;
    if let Some(last_positions) = self.last_positions.as_mut() {
      ArrayUtil::grow_exact(last_positions, new_size, || 0)?;
    }
    if let Some(last_offsets) = self.last_offsets.as_mut() {
      ArrayUtil::grow_exact(last_offsets, new_size, || 0)?;
    }
    if let Some(term_freqs) = self.term_freqs.as_mut() {
      ArrayUtil::grow_exact(term_freqs, new_size, || 0)?;
    }

    Ok(())
  }
}

impl PostingsArray for FreqProxPostingsArray {
  fn new_for_index(size: usize, index_options: IndexOptions) -> Self {
    let has_freq = index_options >= IndexOptions::DocsAndFreqs;
    let has_prox = index_options >= IndexOptions::DocsAndFreqsAndPositions;
    let has_offsets = index_options >= IndexOptions::DocsAndFreqsAndPositionsAndOffsets;
    Self::new(size, has_freq, has_prox, has_offsets)
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
