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
use crate::core::index::automaton_terms_enum::AutomatonTermsEnum;
use crate::core::index::base_terms_enum::BaseTermsEnum;
use crate::core::index::byte_slice_reader::ByteSliceReader;
use crate::core::index::dummy::dummy_impacts_enum::DummyImpactsEnum;
use crate::core::index::dummy::dummy_term_state_type::DummyTermState;
use crate::core::index::fields::Fields;
use crate::core::index::filtered_terms_enum::FilteredTermsEnum;
use crate::core::index::freq_prox_terms_writer_per_field::FreqProxTermsWriterPerField;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::parallel_postings_array::PostingsArrayEnum;
use crate::core::index::postings_enum::PostingsEnum;
use crate::core::index::postings_enum::{
    Either2PostingsEnum, FREQS, OFFSETS, POSITIONS, feature_requested,
};
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::{ReadyPreparedSeekExact, SeekStatus, TermsEnum};
use crate::core::index::{BytesRef, BytesRefBuilder};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::store::DataInput;
use crate::core::util::automation::compiled_automaton::CompiledAutomaton;
use crate::core::util::bytes_ref_block_pool::BytesRefBlockPool;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::dummy::dummy_attribute_source::DummyAttributeSource;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::{ByteBlockPoolLock, CounterEnumLock, ToInt};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::rc::Rc;

/// Implements limited (iterators only, no stats) [`Fields`](Fields) interface over the in-RAM buffered
/// fields/terms/postings, to flush postings through the PostingsFormat.
pub(crate) struct FreqProxFields {
    fields: BTreeMap<String, Rc<FreqProxTermsWriterPerField>>,
}
impl FreqProxFields {
    pub fn new(field_list: Vec<Rc<FreqProxTermsWriterPerField>>) -> Self {
        // NOTE: fields are already sorted by field name
        let mut fields = BTreeMap::new();
        for field in field_list {
            let field_name = field.base.get_field_name().to_string();
            fields.insert(field_name, field);
        }
        Self { fields }
    }
}
impl Fields for FreqProxFields {
    type FieldIter<'a> =
        std::collections::btree_map::Keys<'a, String, Rc<FreqProxTermsWriterPerField>>;

    fn iterator(&self) -> Self::FieldIter<'_> {
        self.fields.keys()
    }

    type Terms = FreqProxTerms;

    fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
        let per_filed = self.fields.get(field);
        match per_filed {
            Some(terms) => Ok(Some(FreqProxTerms::new(Rc::clone(terms)))),
            None => Ok(None),
        }
    }

    fn size(&self) -> Result<i32> {
        Err(LuceneError::unsupported_operation(""))
    }
}

impl Clone for FreqProxFields {
    fn clone(&self) -> Self {
        Self {
            fields: self.fields.clone(),
        }
    }
}

pub(crate) struct FreqProxTerms {
    terms: Rc<FreqProxTermsWriterPerField>,
}
impl FreqProxTerms {
    pub fn new(terms: Rc<FreqProxTermsWriterPerField>) -> Self {
        Self { terms }
    }
}
impl Terms for FreqProxTerms {
    type TermsEnum = BaseTermsEnum<FreqProxTermsEnum>;

    fn iterator(&self) -> Result<Self::TermsEnum> {
        let mut v = FreqProxTermsEnum::new(self.terms.clone());
        v.reset();
        Ok(v.into())
    }

    type IntersectIter = FilteredTermsEnum<Self::TermsEnum, AutomatonTermsEnum>;

    fn intersect(
        &self,
        compiled: &mut CompiledAutomaton,
        start_term: Option<BytesRef<Vec<u8>>>,
    ) -> Result<Self::IntersectIter> {
        self.default_intersect(compiled, start_term)
    }

    fn size(&self) -> Result<i64> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn get_sum_total_term_freq(&self) -> Result<i64> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn get_sum_doc_freq(&self) -> Result<i64> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn get_doc_count(&self) -> Result<i32> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn has_freqs(&self) -> bool {
        self.terms
            .base
            .index_options
            .cmp(&IndexOptions::DocsAndFreqs)
            .to_int()
            >= 0
    }

    fn has_offsets(&self) -> bool {
        // NOTE: the in-memory buffer may have indexed offsets
        // because that's what FieldInfo said when we started,
        // but during indexing this may have been downgraded:
        self.terms
            .base
            .index_options
            .cmp(&IndexOptions::DocsAndFreqsAndPositionsAndOffsets)
            .to_int()
            >= 0
    }

    fn has_positions(&self) -> bool {
        // NOTE: the in-memory buffer may have indexed positions
        // because that's what FieldInfo said when we started,
        // but during indexing this may have been downgraded:
        self.terms
            .base
            .index_options
            .cmp(&IndexOptions::DocsAndFreqsAndPositions)
            .to_int()
            >= 0
    }

    fn has_payloads(&self) -> bool {
        self.terms.saw_payloads
    }
}

pub(crate) struct FreqProxTermsEnum {
    terms: Rc<FreqProxTermsWriterPerField>,
    terms_pool: BytesRefBlockPool<CounterEnumLock, ByteBlockPoolLock>,
    scratch: BytesRef<Vec<u8>>,
    num_terms: i32,
    ord: i32,
}
impl FreqProxTermsEnum {
    fn new(terms: Rc<FreqProxTermsWriterPerField>) -> Self {
        let (num_terms, terms_pool) = {
            let num_terms = terms.base.get_num_terms();
            let terms_pool = BytesRefBlockPool::from_byte_block_pool(terms.base.byte_pool.clone());
            (num_terms, terms_pool)
        };
        Self {
            terms,
            terms_pool,
            scratch: BytesRef::new(),
            num_terms,
            ord: 0,
        }
    }
    pub fn reset(&mut self) {
        self.ord = -1;
    }
}

impl BytesRefIterator for FreqProxTermsEnum {
    fn next(&mut self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
        self.ord += 1;
        if self.ord >= self.num_terms {
            return Ok(None);
        }

        let term_id = self.terms.base.get_sorted_term_ids()[self.ord as usize];

        let postings_array_enum = &self
            .terms
            .base
            .bytes_hash
            .bytes_start_array
            .per_field
            .postings_array;

        let Some(PostingsArrayEnum::FreqProx(p)) = postings_array_enum else {
            return Err(LuceneError::illegal_state(
                "Expected FreqProx postings array",
            ));
        };

        let text_start = p.parent.text_starts[term_id as usize];
        self.terms_pool
            .fill_bytes_ref(&mut self.scratch, text_start);

        Ok(Some(Cow::Borrowed(&self.scratch)))
    }
}

impl TermsEnum for FreqProxTermsEnum {
    type AttributeSource = DummyAttributeSource;
    type PreparedSeekExact<'a> = ReadyPreparedSeekExact;

    fn prepare_seek_exact<'a>(
        &'a mut self,
        _text: &'a BytesRef<Vec<u8>>,
    ) -> Result<Option<Self::PreparedSeekExact<'a>>> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn seek_ceil(&mut self, text: &BytesRef<Vec<u8>>) -> Result<SeekStatus> {
        let postings_array_enum = &self
            .terms
            .base
            .bytes_hash
            .bytes_start_array
            .per_field
            .postings_array;
        let Some(postings_array) = postings_array_enum else {
            return Err(LuceneError::illegal_state("Postings array is none"));
        };

        let PostingsArrayEnum::FreqProx(postings_array) = postings_array else {
            return Err(LuceneError::illegal_state("Unexpected postings array type"));
        };

        let sorted_term_ids = self.terms.base.get_sorted_term_ids();

        let mut lo = 0;
        let mut hi = self.num_terms - 1;

        while hi >= lo {
            let mid = (lo + hi) >> 1;
            let term_id = sorted_term_ids[mid as usize];
            let text_start = postings_array.parent.text_starts[term_id as usize];

            self.terms_pool
                .fill_bytes_ref(&mut self.scratch, text_start);
            let cmp = self.scratch.cmp(text).to_int();

            if cmp < 0 {
                lo = mid + 1;
            } else if cmp > 0 {
                hi = mid - 1;
            } else {
                // found
                self.ord = mid;
                debug_assert_eq!((*self.term()?).cmp(text).to_int(), 0);
                return Ok(SeekStatus::Found);
            }
        }

        // not found
        self.ord = lo;
        if self.ord >= self.num_terms {
            Ok(SeekStatus::End)
        } else {
            let term_id = sorted_term_ids[self.ord as usize];
            let text_start = postings_array.parent.text_starts[term_id as usize];
            self.terms_pool
                .fill_bytes_ref(&mut self.scratch, text_start);
            debug_assert!((*self.term()?).cmp(text).to_int() > 0);
            Ok(SeekStatus::NotFound)
        }
    }

    fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
        let ord = ord as i32;
        self.ord = ord;

        let term_id = self.terms.base.get_sorted_term_ids()[ord as usize];

        let postings_array_enum = &self
            .terms
            .base
            .bytes_hash
            .bytes_start_array
            .per_field
            .postings_array;

        let Some(PostingsArrayEnum::FreqProx(p)) = postings_array_enum else {
            return Err(LuceneError::illegal_state(
                "Expected FreqProx postings array",
            ));
        };

        let text_start = p.parent.text_starts[term_id as usize];
        self.terms_pool
            .fill_bytes_ref(&mut self.scratch, text_start);

        Ok(())
    }

    fn term(&self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
        Ok(Cow::Borrowed(&self.scratch))
    }

    fn ord(&self) -> Result<i64> {
        Ok(self.ord as i64)
    }

    fn doc_freq(&mut self) -> Result<i32> {
        // We do not store this per-term, and we cannot
        // implement this at merge time w/o an added pass
        // through the postings:
        Err(LuceneError::unsupported_operation(""))
    }

    fn total_term_freq(&mut self) -> Result<i64> {
        // We do not store this per-term, and we cannot
        // implement this at merge time w/o an added pass
        // through the postings:
        Err(LuceneError::unsupported_operation(""))
    }

    type PostingsEnum = Either2PostingsEnum<FreqProxPostingsEnum, FreqProxDocsEnum>;

    fn postings_with_flags(
        &mut self,
        reuse: Option<Self::PostingsEnum>,
        flags: i32,
    ) -> Result<Self::PostingsEnum> {
        let sorted_term_ids = self.terms.base.get_sorted_term_ids();
        let (has_prox, has_offsets, has_freq) = {
            (
                self.terms.has_prox,
                self.terms.has_offsets,
                self.terms.has_freq,
            )
        };
        if feature_requested(flags, POSITIONS) {
            if !has_prox {
                // Caller wants positions but we didn't index them;
                // don't lie:
                return Err(LuceneError::illegal_state("did not index positions"));
            }
            if !has_offsets && feature_requested(flags, OFFSETS) {
                // Caller wants offsets but we didn't index them;
                // don't lie:
                return Err(LuceneError::illegal_state("did not index offsets"));
            }

            let mut pos_enum = match reuse {
                Some(Either2PostingsEnum::A(p)) => p,
                _ => FreqProxPostingsEnum::new(self.terms.clone()),
            };
            pos_enum.reset(sorted_term_ids[self.ord as usize]);
            return Ok(Either2PostingsEnum::A(pos_enum));
        }

        if has_freq && !feature_requested(flags, FREQS) {
            // Caller wants offsets but we didn't index them;
            // don't lie:
            return Err(LuceneError::illegal_state("did not index freq"));
        };
        let mut docs_enum = match reuse {
            Some(Either2PostingsEnum::B(p)) => p,
            Some(Either2PostingsEnum::A(_)) => FreqProxDocsEnum::new(self.terms.clone()),
            None => return Err(LuceneError::illegal_state("reuse is none")),
        };
        docs_enum.reset(sorted_term_ids[self.ord as usize]);
        Ok(Either2PostingsEnum::B(docs_enum))
    }

    type ImpactsEnum = DummyImpactsEnum;

    fn impacts(&mut self, _flags: i32) -> Result<Self::ImpactsEnum> {
        Err(LuceneError::unsupported_operation(""))
    }

    type TermState = DummyTermState;
}

pub(crate) struct FreqProxDocsEnum {
    pub terms: Rc<FreqProxTermsWriterPerField>,
    pub reader: ByteSliceReader,
    pub read_term_freq: bool,
    pub doc_id: i32,
    pub freq: i32,
    pub ended: bool,
    pub term_id: i32,
}
impl FreqProxDocsEnum {
    pub fn new(terms: Rc<FreqProxTermsWriterPerField>) -> Self {
        let read_term_freq = terms.has_freq;
        Self {
            terms,
            reader: ByteSliceReader::new(),
            read_term_freq,
            doc_id: -1,
            freq: 0,
            ended: false,
            term_id: -1,
        }
    }
    pub fn reset(&mut self, term_id: i32) {
        self.term_id = term_id;
        self.terms.base.init_reader(&mut self.reader, term_id, 0);
        self.ended = false;
        self.doc_id = -1;
    }
}

impl DocIdSetIterator for FreqProxDocsEnum {
    fn doc_id(&self) -> i32 {
        self.doc_id
    }

    fn next_doc(&mut self) -> Result<i32> {
        if self.doc_id == -1 {
            self.doc_id = 0;
        }

        if self.reader.eof() {
            if self.ended {
                return Ok(NO_MORE_DOCS);
            } else {
                self.ended = true;
                {
                    let postings_array_enum = &self
                        .terms
                        .base
                        .bytes_hash
                        .bytes_start_array
                        .per_field
                        .postings_array;
                    let Some(postings_array) = postings_array_enum else {
                        return Err(LuceneError::illegal_state("Postings array is none"));
                    };

                    let PostingsArrayEnum::FreqProx(p) = postings_array else {
                        return Err(LuceneError::illegal_state("Unexpected postings array type"));
                    };
                    self.doc_id = p.last_doc_ids[self.term_id as usize];
                    if self.read_term_freq {
                        self.freq = p.term_freqs.as_ref().expect("term_freqs must exist")
                            [self.term_id as usize];
                    }
                }
            }
        } else {
            let code = self.reader.read_vint()?;
            if !self.read_term_freq {
                self.doc_id += code;
            } else {
                self.doc_id += (code as u32 >> 1) as i32;
                if (code & 1) != 0 {
                    self.freq = 1;
                } else {
                    self.freq = self.reader.read_vint()?;
                }
            }
        }

        Ok(self.doc_id)
    }

    fn advance(&mut self, _target: i32) -> Result<i32> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn cost(&self) -> Result<i64> {
        Err(LuceneError::unsupported_operation(""))
    }
}

impl PostingsEnum for FreqProxDocsEnum {
    fn freq(&mut self) -> Result<i32> {
        // Don't lie here ... don't want codecs writings lots
        // of wasted 1s into the index:
        if !self.read_term_freq {
            return Err(LuceneError::illegal_state("freq was not indexed"));
        }
        Ok(self.freq)
    }

    fn next_position(&mut self) -> Result<i32> {
        Ok(-1)
    }

    fn start_offset(&self) -> Result<i32> {
        Ok(-1)
    }

    fn end_offset(&self) -> Result<i32> {
        Ok(-1)
    }

    fn get_payload(&self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
        Ok(None)
    }
}

pub(crate) struct FreqProxPostingsEnum {
    terms: Rc<FreqProxTermsWriterPerField>,
    reader: ByteSliceReader,
    pos_reader: ByteSliceReader,
    read_offsets: bool,
    doc_id: i32,
    freq: i32,
    pos: i32,
    start_offset: i32,
    end_offset: i32,
    pos_left: i32,
    term_id: i32,
    ended: bool,
    has_payload: bool,
    payload: BytesRefBuilder<Vec<u8>>,
}
impl FreqProxPostingsEnum {
    pub fn new(terms: Rc<FreqProxTermsWriterPerField>) -> Self {
        let has_offsets = terms.has_offsets;
        Self {
            terms,
            reader: ByteSliceReader::new(),
            pos_reader: ByteSliceReader::new(),
            read_offsets: has_offsets,
            doc_id: -1,
            freq: 0,
            pos: 0,
            start_offset: 0,
            end_offset: 0,
            pos_left: 0,
            term_id: 0,
            ended: false,
            has_payload: false,
            payload: BytesRefBuilder::new(),
        }
    }
    pub fn reset(&mut self, term_id: i32) {
        self.term_id = term_id;
        self.terms.base.init_reader(&mut self.reader, term_id, 0);
        self.terms
            .base
            .init_reader(&mut self.pos_reader, term_id, 1);
        self.ended = false;
        self.doc_id = -1;
        self.pos_left = 0;
    }
}

impl DocIdSetIterator for FreqProxPostingsEnum {
    fn doc_id(&self) -> i32 {
        self.doc_id
    }

    fn next_doc(&mut self) -> Result<i32> {
        if self.doc_id == -1 {
            self.doc_id = 0;
        }

        while self.pos_left != 0 {
            self.next_position()?;
        }

        if self.reader.eof() {
            if self.ended {
                return Ok(NO_MORE_DOCS);
            } else {
                self.ended = true;
                {
                    let postings_array_enum = &self
                        .terms
                        .base
                        .bytes_hash
                        .bytes_start_array
                        .per_field
                        .postings_array;
                    let Some(postings_array) = postings_array_enum else {
                        return Err(LuceneError::illegal_state("Postings array is none"));
                    };

                    let PostingsArrayEnum::FreqProx(p) = postings_array else {
                        return Err(LuceneError::illegal_state("Unexpected postings array type"));
                    };

                    self.doc_id = p.last_doc_ids[self.term_id as usize];
                    self.freq = p.term_freqs.as_ref().unwrap()[self.term_id as usize];
                }
            }
        } else {
            let code = self.reader.read_vint()?;
            self.doc_id += ((code as u32) >> 1) as i32;
            if (code & 1) != 0 {
                self.freq = 1;
            } else {
                self.freq = self.reader.read_vint()?;
            }
        }

        self.pos_left = self.freq;
        self.pos = 0;
        self.start_offset = 0;

        Ok(self.doc_id)
    }

    fn advance(&mut self, _target: i32) -> Result<i32> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn cost(&self) -> Result<i64> {
        Err(LuceneError::unsupported_operation(""))
    }
}

impl PostingsEnum for FreqProxPostingsEnum {
    fn freq(&mut self) -> Result<i32> {
        Ok(self.freq)
    }

    fn next_position(&mut self) -> Result<i32> {
        debug_assert!(self.pos_left > 0);
        self.pos_left -= 1;

        let code = self.pos_reader.read_vint()?;
        self.pos += (code as u32 >> 1) as i32;

        if (code & 1) != 0 {
            self.has_payload = true;
            // has a payload
            let payload_len = self.pos_reader.read_vint()? as usize;
            self.payload.set_length(payload_len);
            self.payload.grow_no_copy(payload_len);

            debug_assert!(payload_len <= i32::MAX as usize);
            self.pos_reader
                .read_bytes(&mut self.payload.bytes_ref.bytes, 0, payload_len as i32)?;
        } else {
            self.has_payload = false;
        }

        if self.read_offsets {
            self.start_offset += self.pos_reader.read_vint()?;
            self.end_offset = self.start_offset + self.pos_reader.read_vint()?;
        }
        Ok(self.pos)
    }

    fn start_offset(&self) -> Result<i32> {
        if !self.read_offsets {
            return Err(LuceneError::unsupported_operation(
                "Offsets not indexed".to_string(),
            ));
        }
        Ok(self.start_offset)
    }

    fn end_offset(&self) -> Result<i32> {
        if !self.read_offsets {
            return Err(LuceneError::unsupported_operation(
                "Offsets not indexed".to_string(),
            ));
        }
        Ok(self.end_offset)
    }

    fn get_payload(&self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
        if !self.has_payload {
            return Err(LuceneError::unsupported_operation(
                "Payloads not indexed".to_string(),
            ));
        }
        Ok(Some(Cow::Borrowed(&self.payload.bytes_ref)))
    }
}
