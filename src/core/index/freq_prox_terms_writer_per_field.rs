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
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_invert_state::FieldInvertState;
use crate::core::index::freq_prox_terms_writer::FreqProxTermsWriter;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::indexable_field::IndexableField;
use crate::core::index::parallel_postings_array::{
    ParallelPostingsArray, PostingsArrayBase, PostingsArrayEnum,
};
use crate::core::index::term_vectors_consumer::{PerFieldMeta, TermVectorsConsumer};
use crate::core::index::term_vectors_consumer_per_field::TermVectorsConsumerPerField;
#[cfg(test)]
use crate::core::index::terms_hash_per_field::tests::TermsHashPerFieldMock;
use crate::core::index::terms_hash_per_field::{
    PostingsArrayWrapper, TermsHashPerField, TermsHashPerFieldBase, TermsHashPerFieldType,
};
use crate::core::store::directory::Directory;
use crate::core::util::ToInt;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::attribute_source::AttributeSource;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::cmp::Ordering;
use std::sync::Arc;

// TODO: break into separate freq and prox writers as
// codecs; make separate container (tii/tis/skip/*) that can
// be configured as any number of files 1..N
pub(crate) struct FreqProxTermsWriterPerField {
    field_info: Arc<FieldInfo>,
    pub(crate) has_freq: bool,
    pub(crate) has_prox: bool,
    pub(crate) has_offsets: bool,
    // Set to true if any token had a payload in the current segment.
    pub(crate) saw_payloads: bool,

    pub(crate) next_per_field: Option<TermVectorsConsumerPerField>,

    pub(crate) base: TermsHashPerField,
}
impl FreqProxTermsWriterPerField {
    pub fn new<D>(
        terms_hash: &mut FreqProxTermsWriter<D>,
        field_info: Arc<FieldInfo>,
        next_per_field: Option<TermVectorsConsumerPerField>,
    ) -> FreqProxTermsWriterPerField
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
        let name = field_info.get_name().to_string();
        let postings_array_wrapper = PostingsArrayWrapper::new(TermsHashPerFieldType::FreqProx(
            FreqProx::new(index_options),
        ));
        let base = TermsHashPerField::new(
            stream_count,
            terms_hash.base.int_pool.clone(),
            terms_hash.base.byte_pool.clone(),
            terms_hash.base.term_byte_pool.as_mut().unwrap().clone(),
            terms_hash.base.bytes_used.clone(),
            postings_array_wrapper,
            name,
            index_options,
        );
        FreqProxTermsWriterPerField {
            field_info,
            has_freq,
            has_prox,
            has_offsets,
            saw_payloads,
            next_per_field,
            base,
        }
    }
    pub(crate) fn write_prox(
        &mut self,
        term_id: usize,
        prox_code: i32,
        field_state: &FieldInvertState,
        attribute_source: &impl AttributeSource,
    ) -> Result<()> {
        if let Some(payload) = attribute_source.get_payload() {
            if payload.length > 0 {
                self.base.write_vint(1, (prox_code << 1) | 1)?;
                self.base.write_vint(1, payload.length as i32)?;
                self.base
                    .write_bytes(1, &payload.bytes, payload.offset, payload.length)?;
                self.saw_payloads = true;
            } else {
                self.base.write_vint(1, prox_code << 1)?;
            }
        } else {
            self.base.write_vint(1, prox_code << 1)?;
        }
        let postings_array_enum = self
            .base
            .bytes_hash
            .bytes_start_array
            .per_field
            .postings_array
            .as_mut()
            .expect("postings_array must be Some");
        match postings_array_enum {
            PostingsArrayEnum::FreqProx(f) => {
                f.last_positions.as_mut().unwrap()[term_id] = field_state.position();
            },
            _ => unreachable!("should not be here"),
        }

        Ok(())
    }
    pub(crate) fn write_offsets(
        &mut self,
        term_id: usize,
        offset_accum: i32,
        attribute_source: &impl AttributeSource,
    ) -> Result<()> {
        let (start, end) = attribute_source
            .start_offset()
            .zip(attribute_source.end_offset())
            .ok_or_else(|| {
                LuceneError::illegal_state("missing start or end offset in attribute_source")
            })?;

        let start_offset = offset_accum + start;
        let end_offset = offset_accum + end;

        let postings_array = self
            .base
            .bytes_hash
            .bytes_start_array
            .per_field
            .postings_array
            .as_mut()
            .expect("postings_array must be Some");

        let (v1, v2) = match postings_array {
            PostingsArrayEnum::FreqProx(f) => {
                let last_offsets = f.last_offsets.as_mut().expect("last_offsets must be Some");
                let last_offset = last_offsets[term_id];

                debug_assert!(
                    start_offset - last_offset >= 0,
                    "start_offset must not go backwards"
                );
                let v1 = start_offset - last_offset;
                let v2 = end_offset - start_offset;

                last_offsets[term_id] = start_offset;

                (v1, v2)
            },
            _ => unreachable!("expected FreqProx posting array"),
        };
        self.base.write_vint(1, v1)?;
        self.base.write_vint(1, v2)?;

        Ok(())
    }
    fn get_term_freq(&self, attribute_source: &impl AttributeSource) -> Result<i32> {
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
        meta: PerFieldMeta,
    ) where
        D: Directory,
    {
        if self.next_per_field.is_some() {
            self.next_per_field
                .as_mut()
                .unwrap()
                .finish(term_vectors_consumer, meta);
        }
        if self.saw_payloads {
            self.field_info
                .set_store_payloads()
                .expect("should not fail")
        }
    }
    pub(crate) fn reset(&mut self) {
        self.base.reset();
        if self.next_per_field.is_some() {
            self.next_per_field.as_mut().unwrap().reset();
        }
    }
    /// Called once per inverted token. This is the primary entry point (for
    /// first TermsHash); postings use this API.
    pub(crate) fn add_with_bytes_ref(
        &mut self,
        term_bytes: Option<&BytesRef<Vec<u8>>>,
        doc_id: i32,
        field_state: &mut FieldInvertState,
        attribute_source: &mut impl AttributeSource,
    ) -> Result<()> {
        debug_assert!(self.base.assert_doc_id(doc_id));
        // We are first in the chain so we must "intern" the
        // term text into textStart address
        // Get the text & hash of this term.
        let bytes = attribute_source.get_bytes_ref();
        let term_bytes = match term_bytes {
            Some(t) => t,
            None => {
                if bytes.is_none() {
                    return Err(LuceneError::illegal_state(
                        "term bytes and attribute source bytes are both None",
                    ));
                }
                bytes.as_ref().unwrap()
            },
        };
        let mut term_id = self.base.bytes_hash.add(term_bytes)?;
        if term_id >= 0 {
            self.base.init_stream_slices(term_id, doc_id)?;
            self.new_term(term_id, doc_id, field_state, attribute_source)?;
        } else {
            term_id = self.base.position_stream_slice(term_id, doc_id)?;
            self.add_term(term_id, doc_id, field_state, attribute_source)?;
        }

        if let Some(ref mut next_per_field) = self.next_per_field {
            let postings_array_wrapper = &self.base.bytes_hash.bytes_start_array.per_field;
            debug_assert!(postings_array_wrapper.postings_array.is_some());
            let text_start = postings_array_wrapper
                .postings_array
                .as_ref()
                .unwrap()
                .get_text_starts()[term_id as usize];
            next_per_field.add_with_text_start(
                text_start,
                doc_id,
                field_state,
                attribute_source,
            )?;
        }
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn add_with_bytes_ref_with_test(
        &mut self,
        term_bytes: &BytesRef<Vec<u8>>,
        doc_id: i32,
        sub: &mut TermsHashPerFieldMock,
        attribute_source: &impl AttributeSource,
    ) -> Result<()> {
        debug_assert!(self.base.assert_doc_id(doc_id));
        // We are first in the chain so we must "intern" the
        // term text into textStart address
        // Get the text & hash of this term.
        let mut term_id = self.base.bytes_hash.add(term_bytes)?;
        if term_id >= 0 {
            self.base.init_stream_slices(term_id, doc_id)?;
            sub.new_term(term_id, doc_id, &mut self.base)?;
        } else {
            term_id = self.base.position_stream_slice(term_id, doc_id)?;
            sub.add_term(term_id, doc_id, &mut self.base)?;
        }

        if let Some(ref mut next_per_field) = self.next_per_field {
            let postings_array_wrapper = &self.base.bytes_hash.bytes_start_array.per_field;
            debug_assert!(postings_array_wrapper.postings_array.is_some());
            let text_start = postings_array_wrapper
                .postings_array
                .as_ref()
                .unwrap()
                .get_text_starts()[term_id as usize];
            next_per_field.add_with_text_start(
                text_start,
                doc_id,
                &mut sub.field_state,
                attribute_source,
            )?;
        }
        Ok(())
    }
    pub(crate) fn start<F>(&mut self, field: &F, first: bool) -> Result<bool>
    where
        F: IndexableField,
    {
        match self.next_per_field {
            Some(ref mut next_per_field) => next_per_field.start(field, first)?,
            None => true,
        };
        Ok(true)
    }
}
impl TermsHashPerFieldBase for FreqProxTermsWriterPerField {
    fn new_term(
        &mut self,
        term_id: i32,
        doc_id: i32,
        field_state: &mut FieldInvertState,
        attribute_source: &impl AttributeSource,
    ) -> Result<()> {
        let term_id = term_id as usize;
        // First time we're seeing this term since the last
        // flush
        let tf = self.get_term_freq(attribute_source)?;
        let postings_array_enum = self
            .base
            .bytes_hash
            .bytes_start_array
            .per_field
            .postings_array
            .as_mut()
            .expect("postings_array must be Some");

        match postings_array_enum {
            PostingsArrayEnum::FreqProx(postings) => {
                postings.last_doc_ids[term_id] = doc_id;

                if !self.has_freq {
                    debug_assert!(postings.term_freqs.is_none());
                    postings.last_doc_codes[term_id] = doc_id;
                    field_state.max_term_frequency = field_state.max_term_frequency.max(1);
                } else {
                    postings.last_doc_codes[term_id] = doc_id << 1;
                    postings.term_freqs.as_mut().unwrap()[term_id] = tf;

                    if self.has_prox {
                        self.write_prox(
                            term_id,
                            field_state.position,
                            field_state,
                            attribute_source,
                        )?;
                        if self.has_offsets {
                            self.write_offsets(term_id, field_state.offset, attribute_source)?;
                        }
                    } else {
                        debug_assert!(!self.has_offsets);
                    }

                    field_state.max_term_frequency = field_state.max_term_frequency.max(tf);
                }
                field_state.unique_term_count += 1;
            },
            _ => unreachable!("expected FreqProx posting array"),
        }

        Ok(())
    }

    fn add_term(
        &mut self,
        term_id: i32,
        doc_id: i32,
        field_state: &mut FieldInvertState,
        attribute_source: &impl AttributeSource,
    ) -> Result<()> {
        let term_id = term_id as usize;

        let tf = self.get_term_freq(attribute_source)?;
        let postings_enum = self
            .base
            .bytes_hash
            .bytes_start_array
            .per_field
            .postings_array
            .as_mut()
            .expect("postings_array must be Some");
        let mut v = Vec::new();
        match postings_enum {
            PostingsArrayEnum::FreqProx(postings) => {
                if self.has_freq {
                    debug_assert!(postings.term_freqs.as_ref().unwrap()[term_id] > 0);
                }

                if !self.has_freq {
                    debug_assert!(postings.term_freqs.is_none());

                    if let Some(attr) = attribute_source.get_term_frequency()
                        && attr != 1
                    {
                        return Err(LuceneError::illegal_state(format!(
                            "field \"{}\": must index term freq while using custom TermFrequencyAttribute",
                            self.field_info.name
                        )));
                    }

                    if doc_id != postings.last_doc_ids[term_id] {
                        debug_assert!(doc_id > postings.last_doc_ids[term_id]);
                        v.push(postings.last_doc_codes[term_id]);
                        postings.last_doc_codes[term_id] = doc_id - postings.last_doc_ids[term_id];
                        postings.last_doc_ids[term_id] = doc_id;
                        field_state.unique_term_count += 1;
                    }
                } else if doc_id != postings.last_doc_ids[term_id] {
                    debug_assert!(
                        doc_id > postings.last_doc_ids[term_id],
                        "docID = {}, postingsID = {}, termID = {}",
                        doc_id,
                        postings.last_doc_ids[term_id],
                        term_id
                    );

                    let freq = postings.term_freqs.as_ref().unwrap()[term_id];
                    // Term not yet seen in the current doc but previously
                    // seen in other doc(s) since the last flush

                    // Now that we know doc freq for previous doc,
                    // write it & lastDocCode
                    if freq == 1 {
                        v.push(postings.last_doc_codes[term_id] | 1);
                    } else {
                        v.push(postings.last_doc_codes[term_id]);
                        v.push(freq);
                    }
                    // Init freq for the current document
                    postings.term_freqs.as_mut().unwrap()[term_id] = tf;

                    field_state.max_term_frequency = field_state.max_term_frequency.max(tf);

                    postings.last_doc_codes[term_id] =
                        (doc_id - postings.last_doc_ids[term_id]) << 1;
                    postings.last_doc_ids[term_id] = doc_id;

                    if self.has_prox && self.has_offsets {
                        postings.last_offsets.as_mut().unwrap()[term_id] = 0;
                    }
                    if self.has_prox {
                        self.write_prox(
                            term_id,
                            field_state.position,
                            field_state,
                            attribute_source,
                        )?;
                        if self.has_offsets {
                            self.write_offsets(term_id, field_state.offset, attribute_source)?;
                        }
                    } else {
                        debug_assert!(!self.has_offsets);
                    }
                    field_state.unique_term_count += 1;
                } else {
                    let term_freqs = postings.term_freqs.as_mut().unwrap();
                    term_freqs[term_id] = term_freqs[term_id]
                        .checked_add(tf)
                        .ok_or_else(|| LuceneError::illegal_state("term frequency overflow"))?;

                    field_state.max_term_frequency =
                        field_state.max_term_frequency.max(term_freqs[term_id]);

                    if self.has_prox {
                        let delta = field_state.position
                            - postings.last_positions.as_ref().unwrap()[term_id];
                        self.write_prox(term_id, delta, field_state, attribute_source)?;
                        if self.has_offsets {
                            self.write_offsets(term_id, field_state.offset, attribute_source)?;
                        }
                    }
                }
            },
            _ => unreachable!("expected FreqProx posting array"),
        }
        for x in v {
            self.base.write_vint(0, x)?
        }
        Ok(())
    }

    fn get_field_name(&self) -> &str {
        &self.field_info.name
    }
}

impl Eq for FreqProxTermsWriterPerField {}

impl PartialEq<Self> for FreqProxTermsWriterPerField {
    fn eq(&self, _other: &Self) -> bool {
        todo!()
    }
}

impl PartialOrd<Self> for FreqProxTermsWriterPerField {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FreqProxTermsWriterPerField {
    fn cmp(&self, other: &Self) -> Ordering {
        self.base.field_name.cmp(&other.base.field_name)
    }
}

pub(crate) struct FreqProxPostingsArray {
    pub(crate) size: usize,
    pub(crate) term_freqs: Option<Vec<i32>>, /* # times this term occurs in
                                              * the current doc  */
    pub(crate) last_doc_ids: Vec<i32>, // Last docID where this term occurred
    pub(crate) last_doc_codes: Vec<i32>, // Code for prior doc
    pub(crate) last_positions: Option<Vec<i32>>, /* Last position where this term
                                        * occurred  */
    pub(crate) last_offsets: Option<Vec<i32>>, // Last endOffset where this term occurred
    pub(crate) parent: ParallelPostingsArray,
}
impl FreqProxPostingsArray {
    // Constructor for FreqProxPostingsArray
    pub(crate) fn new(
        size: usize,
        write_freqs: bool,
        write_prox: bool,
        write_offsets: bool,
    ) -> Self {
        let mut term_freqs = None;
        if write_freqs {
            term_freqs = Some(vec![0; size]);
        }
        let last_positions = if write_prox {
            Some(vec![0; size])
        } else {
            None
        };
        let last_offsets = if write_offsets {
            Some(vec![0; size])
        } else {
            None
        };
        FreqProxPostingsArray {
            size,
            term_freqs,
            last_doc_ids: vec![0; size],
            last_doc_codes: vec![0; size],
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
        self.size = new_size;
        ArrayUtil::grow_exact(&mut self.last_doc_ids, new_size)?;
        ArrayUtil::grow_exact(&mut self.last_doc_codes, new_size)?;
        if self.last_positions.is_some() {
            ArrayUtil::grow_exact(self.last_positions.as_mut().unwrap(), new_size)?;
        }
        if self.last_offsets.is_some() {
            ArrayUtil::grow_exact(self.last_offsets.as_mut().unwrap(), new_size)?;
        }
        if self.term_freqs.is_some() {
            ArrayUtil::grow_exact(self.term_freqs.as_mut().unwrap(), new_size)?;
        }
        Ok(())
    }
}

pub(crate) struct FreqProx {
    pub(crate) index_options: IndexOptions,
}
impl FreqProx {
    pub fn new(index_options: IndexOptions) -> Self {
        FreqProx { index_options }
    }
}
