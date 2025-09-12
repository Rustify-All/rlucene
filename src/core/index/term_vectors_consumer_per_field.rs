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
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_invert_state::FieldInvertState;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::indexable_field::IndexableField;
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::index::parallel_postings_array::{
    ParallelPostingsArray, PostingsArrayBase, PostingsArrayEnum,
};
use crate::core::index::term_vectors_consumer::TermVectorsConsumer;
use crate::core::index::terms_hash_per_field::{
    PostingsArrayWrapper, TermsHashPerField, TermsHashPerFieldBase, TermsHashPerFieldType,
};
use crate::core::store::directory::Directory;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::attribute_source::AttributeSource;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bytes_ref_block_pool::BytesRefBlockPool;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::{ByteBlockPoolLock, CounterEnumLock};
use std::cmp::Ordering;
use std::sync::Arc;

pub(crate) struct TermVectorsConsumerPerField {
    field_info: Arc<FieldInfo>,
    do_vectors: bool,
    do_vector_positions: bool,
    do_vector_offsets: bool,
    do_vector_payloads: bool,
    term_byte_pool: BytesRefBlockPool<CounterEnumLock, ByteBlockPoolLock>,
    has_payloads: bool,
    field_name: String,
    base: TermsHashPerField,
}
impl Default for TermVectorsConsumerPerField {
    fn default() -> Self {
        TermVectorsConsumerPerField {
            field_info: Arc::new(FieldInfo::default()),
            do_vectors: false,
            do_vector_positions: false,
            do_vector_offsets: false,
            do_vector_payloads: false,
            term_byte_pool: BytesRefBlockPool::default(),
            has_payloads: false,
            field_name: String::new(),
            base: TermsHashPerField::default(),
        }
    }
}
impl Clone for TermVectorsConsumerPerField {
    // for padding
    fn clone(&self) -> Self {
        TermVectorsConsumerPerField {
            field_info: self.field_info.clone(),
            do_vectors: self.do_vectors,
            do_vector_positions: self.do_vector_positions,
            do_vector_offsets: self.do_vector_offsets,
            do_vector_payloads: self.do_vector_payloads,
            term_byte_pool: BytesRefBlockPool::default(),
            has_payloads: self.has_payloads,
            field_name: self.field_name.clone(),
            base: TermsHashPerField::default(),
        }
    }
}

impl TermVectorsConsumerPerField {
    pub(crate) fn new<D>(
        terms_hash: &mut TermVectorsConsumer<D>,
        field_info: Arc<FieldInfo>,
    ) -> Self
    where
        D: Directory,
    {
        let postings_array_wrapper = PostingsArrayWrapper::new(TermsHashPerFieldType::TermVectors);
        let base = TermsHashPerField::new(
            2,
            terms_hash.base.int_pool.clone(),
            terms_hash.base.byte_pool.clone(),
            terms_hash.base.term_byte_pool.as_mut().unwrap().clone(),
            terms_hash.base.bytes_used.clone(),
            postings_array_wrapper,
            field_info.name.clone(),
            field_info.index_options,
        );
        let field_name = field_info.name.clone();
        Self {
            field_info,
            do_vectors: false,
            do_vector_positions: false,
            do_vector_offsets: false,
            do_vector_payloads: false,
            term_byte_pool: BytesRefBlockPool::from_byte_block_pool(
                terms_hash.base.term_byte_pool.as_mut().unwrap().clone(),
            ),
            has_payloads: false,
            field_name,
            base,
        }
    }

    pub(crate) fn finish_document<D>(
        &mut self,
        term_vectors_consumer: &mut TermVectorsConsumer<D>,
    ) -> Result<()>
    where
        D: Directory,
    {
        if !self.do_vectors {
            return Ok(());
        }
        self.do_vectors = false;

        let num_postings = self.base.get_num_terms();
        debug_assert!(num_postings >= 0);

        let tv = term_vectors_consumer.writer.as_mut().unwrap();

        self.base.sort_terms()?;
        let term_ids = self.base.get_sorted_term_ids();

        tv.start_field(
            &self.field_info,
            num_postings as usize,
            self.do_vector_positions,
            self.do_vector_offsets,
            self.has_payloads,
        )?;

        let mut pos_reader = if self.do_vector_positions {
            Some(
                term_vectors_consumer
                    .vector_slice_reader_pos
                    .take()
                    .unwrap(),
            )
        } else {
            None
        };
        let mut off_reader = if self.do_vector_offsets {
            Some(
                term_vectors_consumer
                    .vector_slice_reader_off
                    .take()
                    .unwrap(),
            )
        } else {
            None
        };

        let postings_array_enum = self
            .base
            .bytes_hash
            .bytes_start_array
            .per_field
            .postings_array
            .as_ref()
            .expect("postings_array must be Some");
        match postings_array_enum {
            PostingsArrayEnum::TermVectors(postings) => {
                for &term_id in term_ids {
                    let freq = postings.freqs[term_id as usize];
                    self.term_byte_pool.fill_bytes_ref(
                        &mut term_vectors_consumer.flush_term,
                        postings.parent.text_starts[term_id as usize],
                    );

                    tv.start_term(&term_vectors_consumer.flush_term, freq)?;

                    if self.do_vector_positions || self.do_vector_offsets {
                        if let Some(reader) = pos_reader.as_mut() {
                            self.base.init_reader(reader, term_id, 0);
                        }
                        if let Some(reader) = off_reader.as_mut() {
                            self.base.init_reader(reader, term_id, 1);
                        }
                        tv.add_prox(freq as usize, &mut pos_reader, &mut off_reader)?;
                    }

                    tv.finish_term()?;
                }
            },
            _ => unreachable!("Expected TermVectors postings"),
        }

        tv.finish_field()?;

        self.reset();
        self.field_info.set_store_term_vectors()?;
        term_vectors_consumer.vector_slice_reader_off = off_reader;
        term_vectors_consumer.vector_slice_reader_pos = pos_reader;
        Ok(())
    }
    pub(crate) fn reset(&mut self) {
        self.base.reset();
    }
    // Secondary entry point (for 2nd & subsequent TermsHash),
    // because token text has already been "interned" into
    // textStart, so we hash by textStart.  term vectors use
    // this API.
    pub(crate) fn add_with_text_start(
        &mut self,
        text_start: i32,
        doc_id: i32,
        state: &mut FieldInvertState,
        attribute_source: &impl AttributeSource,
    ) -> Result<()> {
        let term_id = self.base.bytes_hash.add_by_pool_offset(text_start)?;
        if term_id >= 0 {
            // First time we are seeing this token since we last
            // flushed the hash.
            self.base.init_stream_slices(term_id, doc_id)?;
            self.new_term(term_id, doc_id, state, attribute_source)?;
        } else {
            self.base.position_stream_slice(term_id, doc_id)?;
            self.add_term(term_id, doc_id, state, attribute_source)?;
        }
        Ok(())
    }
    pub(crate) fn start<F>(&mut self, field: &F, first: bool) -> Result<bool>
    where
        F: IndexableField,
    {
        debug_assert!(*field.field_type().index_options() != IndexOptions::None);

        if first {
            if self.base.get_num_terms() != 0 {
                // Only necessary if previous doc hit a
                // non-aborting exception while writing vectors in
                // this field:
                self.base.reset();
            }

            self.base.reinit_hash();

            self.has_payloads = false;

            self.do_vectors = field.field_type().store_term_vectors();

            if self.do_vectors {
                self.do_vector_positions = field.field_type().store_term_vector_positions();
                // Somewhat confusingly, unlike postings, you are
                // allowed to index TV offsets without TV positions:
                self.do_vector_offsets = field.field_type().store_term_vector_offsets();

                if self.do_vector_positions {
                    self.do_vector_payloads = field.field_type().store_term_vector_payloads();
                } else {
                    self.do_vector_payloads = false;
                    if field.field_type().store_term_vector_payloads() {
                        return Err(LuceneError::illegal_argument(format!(
                            "cannot index term vector payloads without term vector positions (field=\"{}\")",
                            field.name()
                        )));
                    }
                }
            } else {
                if field.field_type().store_term_vector_offsets() {
                    return Err(LuceneError::illegal_argument(format!(
                        "cannot index term vector offsets when term vectors are not indexed (field=\"{}\")",
                        field.name()
                    )));
                }
                if field.field_type().store_term_vector_positions() {
                    return Err(LuceneError::illegal_argument(format!(
                        "cannot index term vector positions when term vectors are not indexed (field=\"{}\")",
                        field.name()
                    )));
                }
                if field.field_type().store_term_vector_payloads() {
                    return Err(LuceneError::illegal_argument(format!(
                        "cannot index term vector payloads when term vectors are not indexed (field=\"{}\")",
                        field.name()
                    )));
                }
            }
        } else {
            if self.do_vectors != field.field_type().store_term_vectors() {
                return Err(LuceneError::illegal_argument(format!(
                    "all instances of a given field name must have the same term vectors settings (storeTermVectors changed for field=\"{}\")",
                    field.name()
                )));
            }
            if self.do_vector_positions != field.field_type().store_term_vector_positions() {
                return Err(LuceneError::illegal_argument(format!(
                    "all instances of a given field name must have the same term vectors settings (storeTermVectorPositions changed for field=\"{}\")",
                    field.name()
                )));
            }
            if self.do_vector_offsets != field.field_type().store_term_vector_offsets() {
                return Err(LuceneError::illegal_argument(format!(
                    "all instances of a given field name must have the same term vectors settings (storeTermVectorOffsets changed for field=\"{}\")",
                    field.name()
                )));
            }
            if self.do_vector_payloads != field.field_type().store_term_vector_payloads() {
                return Err(LuceneError::illegal_argument(format!(
                    "all instances of a given field name must have the same term vectors settings (storeTermVectorPayloads changed for field=\"{}\")",
                    field.name()
                )));
            }
        }
        Ok(self.do_vectors)
    }
    pub(crate) fn write_prox(
        &mut self,
        term_id: usize,
        field_state: &FieldInvertState,
        attribute_source: &impl AttributeSource,
    ) -> Result<()> {
        let postings = self
            .base
            .bytes_hash
            .bytes_start_array
            .per_field
            .postings_array
            .as_ref()
            .unwrap();
        let mut last_offset = None;
        let mut last_position = None;
        match postings {
            PostingsArrayEnum::TermVectors(postings) => {
                if self.do_vector_offsets {
                    let (start, end) = attribute_source
                        .start_offset()
                        .zip(attribute_source.end_offset())
                        .ok_or_else(|| {
                            LuceneError::illegal_state(
                                "missing start or end offset in attribute_source",
                            )
                        })?;

                    let start_offset = field_state.offset + start;
                    let end_offset = field_state.offset + end;

                    self.base
                        .write_vint(1, start_offset - postings.last_offsets[term_id])?;
                    self.base.write_vint(1, end_offset - start_offset)?;
                    last_offset = Some(end_offset);
                }

                if self.do_vector_positions {
                    let pos = field_state.position - postings.last_positions[term_id];

                    if let Some(payload) = attribute_source.get_payload() {
                        if payload.length > 0 {
                            self.base.write_vint(0, (pos << 1) | 1)?;
                            self.base.write_vint(0, payload.length as i32)?;
                            self.base.write_bytes(
                                0,
                                &payload.bytes,
                                payload.offset,
                                payload.length,
                            )?;
                            self.has_payloads = true;
                        } else {
                            self.base.write_vint(0, pos << 1)?;
                        }
                    } else {
                        self.base.write_vint(0, pos << 1)?;
                    }

                    last_position = Some(field_state.position);
                }
            },
            _ => unreachable!("should not be here"),
        }
        let postings = self
            .base
            .bytes_hash
            .bytes_start_array
            .per_field
            .postings_array
            .as_mut()
            .unwrap();
        match postings {
            PostingsArrayEnum::TermVectors(postings) => {
                if let Some(offset) = last_offset {
                    postings.last_offsets[term_id] = offset;
                }
                if let Some(pos) = last_position {
                    postings.last_positions[term_id] = pos;
                }
            },
            _ => unreachable!("should not be here"),
        }

        Ok(())
    }
    pub(crate) fn get_term_freq(&self, attribute_source: &impl AttributeSource) -> Result<i32> {
        let freq = if let Some(att) = attribute_source.get_term_frequency() {
            att
        } else {
            return Ok(1);
        };

        if freq != 1 {
            if self.do_vector_positions {
                return Err(LuceneError::illegal_argument(format!(
                    "field \"{}\": cannot index term vector positions while using custom TermFrequencyAttribute",
                    self.field_name
                )));
            }
            if self.do_vector_offsets {
                return Err(LuceneError::illegal_argument(format!(
                    "field \"{}\": cannot index term vector offsets while using custom TermFrequencyAttribute",
                    self.field_name
                )));
            }
        }

        Ok(freq)
    }
    pub(crate) fn finish<D>(self, term_vectors_consumer: &mut TermVectorsConsumer<D>)
    where
        D: Directory,
    {
        if !self.do_vectors || self.base.get_num_terms() == 0 {
            return;
        }
        term_vectors_consumer.add_field_to_flush(self)
    }
}

impl Eq for TermVectorsConsumerPerField {}

impl PartialEq<Self> for TermVectorsConsumerPerField {
    fn eq(&self, _other: &Self) -> bool {
        todo!()
    }
}

impl PartialOrd<Self> for TermVectorsConsumerPerField {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TermVectorsConsumerPerField {
    fn cmp(&self, other: &Self) -> Ordering {
        self.field_name.cmp(&other.field_name)
    }
}
impl TermsHashPerFieldBase for TermVectorsConsumerPerField {
    fn new_term(
        &mut self,
        term_id: i32,
        _doc_id: i32,
        field_state: &mut FieldInvertState,
        attribute_source: &impl AttributeSource,
    ) -> Result<()> {
        let term_id = term_id as usize;
        let freq = self.get_term_freq(attribute_source)?;
        let postings_enum = self
            .base
            .bytes_hash
            .bytes_start_array
            .per_field
            .postings_array
            .as_mut()
            .unwrap();
        if let PostingsArrayEnum::TermVectors(postings) = postings_enum {
            postings.freqs[term_id] = freq;
            postings.last_offsets[term_id] = 0;
            postings.last_positions[term_id] = 0;

            self.write_prox(term_id, field_state, attribute_source)?;
        } else {
            unreachable!("Expected TermVectors postings");
        }
        Ok(())
    }

    fn add_term(
        &mut self,
        term_id: i32,
        _doc_id: i32,
        state: &mut FieldInvertState,
        attribute_source: &impl AttributeSource,
    ) -> Result<()> {
        let term_id = term_id as usize;
        let freq = self.get_term_freq(attribute_source)?;
        let postings_enum = self
            .base
            .bytes_hash
            .bytes_start_array
            .per_field
            .postings_array
            .as_mut()
            .unwrap();

        if let PostingsArrayEnum::TermVectors(postings) = postings_enum {
            postings.freqs[term_id] += freq;
            self.write_prox(term_id, state, attribute_source)?;
        } else {
            unreachable!("Expected TermVectors postings");
        }

        Ok(())
    }

    fn get_field_name(&self) -> &str {
        self.field_name.as_str()
    }
}

pub(crate) struct TermVectorsPostingsArray {
    pub(crate) size: usize,
    freqs: Vec<i32>,          // How many times this term occurred in the current doc
    last_offsets: Vec<i32>,   // Last offset we saw
    last_positions: Vec<i32>, // Last position where this term occurred
    pub(crate) parent: ParallelPostingsArray,
}

impl TermVectorsPostingsArray {
    pub fn new(size: usize) -> Self {
        TermVectorsPostingsArray {
            size,
            freqs: vec![0; size],
            last_offsets: vec![0; size],
            last_positions: vec![0; size],
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
        self.size = new_size;
        ArrayUtil::grow_exact(&mut self.freqs, new_size)?;
        ArrayUtil::grow_exact(&mut self.last_offsets, new_size)?;
        ArrayUtil::grow_exact(&mut self.last_positions, new_size)?;
        Ok(())
    }
}
