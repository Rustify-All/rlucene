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
use crate::core::codecs::block_term_state::BlockTermStateEnum;
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::codecs::postings_writer_base::PostingsWriterBase;
use crate::core::index::BytesRef;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::postings_enum::{FREQS, OFFSETS, PAYLOADS, POSITIONS, PostingsEnum};
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::store::directory::Directory;
use crate::core::store::{DataOutput, IndexOutput};
use crate::core::util::bit_set::BitSet;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::fixed_bit_set::FixedBitSet;
use std::borrow::Cow;
use std::sync::Arc;

/// Extension of [`PostingsWriterBase`], adding a push API for writing each
/// element of the postings. This API is somewhat analogous to an XML SAX API,
/// while [`PostingsWriterBase`] is more like an XML DOM API.
///
/// @see [`PostingsWriterBase`]
// TODO: find a better name; this defines the API that the
// terms dict impls use to talk to a postings impl.
/// TermsDict + PostingsReader/WriterBase == PostingsConsumer/Producer
pub struct PushPostingsWriterBase<S>
where
    S: PushPostingsWriterBaseAbstract + PostingsWriterBase,
{
    enum_flags: i32,

    /// `FieldInfo` of current field being written.
    pub(crate) field_info: Option<Arc<FieldInfo>>,

    /// `IndexOptions` of current field being written.
    pub(crate) index_options: IndexOptions,

    options: FieldWriteOptions,

    sub: S,
}
pub struct FieldWriteOptions {
    /// True if the current field writes freqs.
    pub(crate) write_freqs: bool,
    /// True if the current field writes positions.
    pub(crate) write_positions: bool,
    /// True if the current field writes payloads.
    pub(crate) write_payloads: bool,
    /// True if the current field writes offsets.
    pub(crate) write_offsets: bool,
}

impl<S> PushPostingsWriterBase<S>
where
    S: PushPostingsWriterBaseAbstract + PostingsWriterBase,
{
    #[allow(clippy::too_many_arguments)]
    /// # Parameters
    /// - `field_info`: It is just a placeholder value; it should be initialized
    ///   as None, but I don't want to add extra wrapping around it. It would be
    ///   set in [`set_field`](Self::set_field) before used
    pub fn new(sub: S) -> Self {
        let options = FieldWriteOptions {
            write_freqs: false,
            write_positions: false,
            write_payloads: false,
            write_offsets: false,
        };
        Self {
            enum_flags: 0,
            field_info: None,
            index_options: Default::default(),
            options,
            sub,
        }
    }
}
impl<S> PostingsWriterBase for PushPostingsWriterBase<S>
where
    S: PushPostingsWriterBaseAbstract + PostingsWriterBase,
{
    fn init<D1, D2>(
        &mut self,
        terms_out: &mut impl IndexOutput,
        state: &SegmentWriteState<D1>,
        segment_info: &SegmentInfo<D2>,
    ) -> Result<()>
    where
        D1: Directory,
        D2: Directory,
    {
        self.sub.init(terms_out, state, segment_info)
    }

    fn write_term<N: NormsProducer, PE: PostingsEnum>(
        &mut self,
        _term: &BytesRef<Vec<u8>>,
        terms_enum: &mut impl TermsEnum<PostingsEnum = PE>,
        docs_seen: &mut FixedBitSet,
        norms: &Option<N>,
        postings_enum: Option<PE>,
    ) -> Result<(Option<PE>, Option<BlockTermStateEnum>)> {
        let mut norm_values = if self.field_info.as_ref().unwrap().has_norms() {
            Some(
                norms
                    .as_ref()
                    .unwrap()
                    .get_norms(self.field_info.as_ref().unwrap())?,
            )
        } else {
            None
        };

        self.sub.start_term(&self.options)?;

        let mut postings_enum = terms_enum.postings_with_flags(postings_enum, self.enum_flags)?;

        let mut doc_freq = 0;
        let mut total_term_freq = 0i64;
        loop {
            let doc_id = postings_enum.next_doc()?;
            if doc_id == NO_MORE_DOCS {
                break;
            }
            doc_freq += 1;
            docs_seen.set(doc_id);

            let freq = if self.options.write_freqs {
                let f = postings_enum.freq()?;
                total_term_freq += f as i64;
                f
            } else {
                -1
            };

            self.sub
                .start_doc::<N>(&mut norm_values, doc_id, freq, &self.options)?;

            if self.options.write_positions {
                for _ in 0..freq {
                    let pos = postings_enum.next_position()?;
                    let payload = if self.options.write_payloads {
                        postings_enum.get_payload()?
                    } else {
                        None
                    };
                    let (start_offset, end_offset) = if self.options.write_offsets {
                        (postings_enum.start_offset()?, postings_enum.end_offset()?)
                    } else {
                        (-1, -1)
                    };
                    self.sub
                        .add_position(pos, payload, start_offset, end_offset, &self.options)?;
                }
            }

            self.sub.finish_doc()?;
        }

        if doc_freq == 0 {
            return Ok((Some(postings_enum), None));
        }

        let mut upper = self.sub.new_term_state()?;
        let state = upper.get_block_term_state();
        state.doc_freq = doc_freq;
        state.total_term_freq = if self.options.write_freqs {
            total_term_freq
        } else {
            -1
        };
        self.sub.finish_term(&mut upper, &self.options)?;
        Ok((Some(postings_enum), Some(upper)))
    }

    fn encode_term(
        &mut self,
        out: &mut impl DataOutput,
        field_info: &FieldInfo,
        state: Cow<BlockTermStateEnum>,
        absolute: bool,
    ) -> Result<()> {
        self.sub
            .encode_term_with_option(out, field_info, state, absolute, &self.options)
    }
    /// Sets the current field for writing, and returns the fixed length of
    /// `&[i64]` metadata (which is fixed per field), called when the
    /// writing switches to another field.
    fn set_field(&mut self, field_info: Arc<FieldInfo>) {
        self.index_options = *field_info.get_index_options();
        let options = &mut self.options;
        options.write_freqs = self.index_options >= IndexOptions::DocsAndFreqs;
        options.write_positions = self.index_options >= IndexOptions::DocsAndFreqsAndPositions;
        options.write_offsets =
            self.index_options >= IndexOptions::DocsAndFreqsAndPositionsAndOffsets;
        options.write_payloads = field_info.has_payloads();
        self.field_info = Option::from(field_info.clone());

        self.enum_flags = if !options.write_freqs {
            0
        } else if !options.write_positions {
            FREQS as i32
        } else if !options.write_offsets {
            if options.write_payloads {
                PAYLOADS as i32
            } else {
                POSITIONS as i32
            }
        } else if options.write_payloads {
            (PAYLOADS | OFFSETS) as i32
        } else {
            OFFSETS as i32
        };
        self.sub.set_field(field_info)
    }
}
pub trait PushPostingsWriterBaseAbstract {
    /// Return a newly created empty TermState
    fn new_term_state(&mut self) -> Result<BlockTermStateEnum>;

    /// Start a new term.
    /// A matching call to [`finish_term`](Self::finish_term) will be done only
    /// if the term has at least one document.
    fn start_term(&mut self, options: &FieldWriteOptions) -> Result<()>;

    /// Finishes the current term. The provided [`BlockTermState`](crate::core::codecs::block_term_state::BlockTermState) contains
    /// the term's summary statistics and will hold metadata from PBF when
    /// returned.
    fn finish_term(
        &mut self,
        state: &mut BlockTermStateEnum,
        options: &FieldWriteOptions,
    ) -> Result<()>;

    /// Adds a new doc in this term. `freq` will be -1 when term
    /// frequencies are omitted for the field.
    fn start_doc<N: NormsProducer>(
        &mut self,
        norms: &mut Option<N::NumericDocValues>,
        doc_id: i32,
        freq: i32,
        options: &FieldWriteOptions,
    ) -> Result<()>;

    /// Add a new position and payload, and start/end offset.
    /// A null payload means no payload; a non-null payload with zero length
    /// also means no payload. Caller may reuse the [`BytesRef`] for the payload
    /// between calls (method must fully consume the payload).
    /// `start_offset` and `end_offset` will be -1 when offsets are not indexed.
    fn add_position(
        &mut self,
        position: i32,
        payload: Option<Cow<'_, BytesRef<Vec<u8>>>>,
        start_offset: i32,
        end_offset: i32,
        options: &FieldWriteOptions,
    ) -> Result<()>;

    /// Called when we are done adding positions and payloads for each doc.
    fn finish_doc(&mut self) -> Result<()>;
    fn encode_term_with_option(
        &mut self,
        out: &mut impl DataOutput,
        field_info: &FieldInfo,
        state: Cow<BlockTermStateEnum>,
        absolute: bool,
        options: &FieldWriteOptions,
    ) -> Result<()>;
}
