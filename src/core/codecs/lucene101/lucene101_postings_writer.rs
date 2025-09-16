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
use crate::core::codecs::CodecUtil;
use crate::core::codecs::block_term_state::BlockTermStateEnum;
use crate::core::codecs::competitive_impact_accumulator::CompetitiveImpactAccumulator;
use crate::core::codecs::lucene101::for_delta_util::ForDeltaUtil;
use crate::core::codecs::lucene101::lucene101_postings_format::{
    IntBlockTermState, Lucene101PostingsFormat,
};
use crate::core::codecs::lucene101::pfor_util::PForUtil;
use crate::core::codecs::lucene101::postings_util::PostingsUtil;
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::codecs::postings_writer_base::PostingsWriterBase;
use crate::core::codecs::push_postings_writer_base::{
    FieldWriteOptions, PushPostingsWriterBaseAbstract,
};
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::field_info::FieldInfo;

use crate::core::index::index_writer::MAX_POSITION;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::postings_enum::PostingsEnum;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::index::{BytesRef, IndexFileNames};
use crate::core::store::directory::Directory;
use crate::core::store::{ByteBuffersDataOutput, DataOutput, IndexOutput};
use crate::core::util::SliceCopyOps;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use std::borrow::Cow;
use std::default::Default;
use std::sync::Arc;

/// Writer for
/// [`Lucene101PostingsFormat`](crate::core::codecs::lucene101::lucene101_postings_format)
pub struct Lucene101PostingsWriter<O>
where
    O: IndexOutput,
{
    pub(crate) meta_out: O,
    pub(crate) doc_out: O,
    pub(crate) pos_out: Option<O>,
    pub(crate) pay_out: Option<O>,
    pub(crate) last_state: IntBlockTermState,
    /// Holds starting file pointers for current term:
    doc_start_fp: i64,
    pos_start_fp: i64,
    pay_start_fp: i64,

    pub(crate) doc_delta_buffer: Vec<i32>,
    pub(crate) freq_buffer: Vec<i32>,
    doc_buffer_upto: i32,

    pub(crate) pos_delta_buffer: Vec<i32>,
    pub(crate) payload_length_buffer: Vec<i32>,
    pub(crate) offset_start_delta_buffer: Vec<i32>,
    pub(crate) offset_length_buffer: Vec<i32>,
    pos_buffer_upto: i32,

    payload_bytes: Vec<u8>,
    payload_byte_upto: i32,

    level0_last_doc_id: i32,
    level0_last_pos_fp: i64,
    level0_last_pay_fp: i64,

    level1_last_doc_id: i32,
    level1_last_pos_fp: i64,
    level1_last_pay_fp: i64,

    doc_id: i32,
    last_doc_id: i32,
    last_position: i32,
    last_start_offset: i32,
    doc_count: i32,

    pfor_util: PForUtil,
    for_delta_util: ForDeltaUtil,

    field_has_norms: bool,
    level0_freq_norm_accumulator: CompetitiveImpactAccumulator,
    level1_competitive_freq_norm_accumulator: CompetitiveImpactAccumulator,

    max_num_impacts_at_level0: i32,
    max_impact_num_bytes_at_level0: i32,
    max_num_impacts_at_level1: i32,
    max_impact_num_bytes_at_level1: i32,
    /// Scratch output that we use to be able to prepend the encoded length,
    /// e.g. impacts.
    scratch_output: ByteBuffersDataOutput,
    /// Output for a single block. This is useful to be able to prepend skip
    /// data before each block, which can only be computed once the block
    /// is encoded. The content is then typically copied to
    /// `level1Output`.
    level0_output: ByteBuffersDataOutput,
    /// Output for groups of 32 blocks. This is useful to prepend skip data for
    /// these 32 blocks, which can only be done once we have encoded these
    /// 32 blocks. The content is then typically copied to `docCount`.
    level1_output: ByteBuffersDataOutput,
}
impl<O> Lucene101PostingsWriter<O>
where
    O: IndexOutput,
{
    pub fn new<D1, D2>(
        state: &SegmentWriteState<D1>,
        segment_info: &SegmentInfo<D2>,
    ) -> Result<Self>
    where
        D1: Directory<IndexOutput = O>,
        D2: Directory,
    {
        let meta_file = IndexFileNames::segment_file_name(
            &segment_info.name,
            &state.segment_suffix,
            Lucene101PostingsFormat::META_EXTENSION,
        );
        let doc_file = IndexFileNames::segment_file_name(
            &segment_info.name,
            &state.segment_suffix,
            Lucene101PostingsFormat::DOC_EXTENSION,
        );

        let mut meta_out = state.directory.create_output(&meta_file, state.context)?;
        let mut doc_out = state.directory.create_output(&doc_file, state.context)?;
        CodecUtil::write_index_header(
            &mut meta_out,
            Lucene101PostingsFormat::META_CODEC,
            Lucene101PostingsFormat::VERSION_CURRENT,
            segment_info.get_id(),
            &state.segment_suffix,
        )?;
        CodecUtil::write_index_header(
            &mut doc_out,
            Lucene101PostingsFormat::DOC_CODEC,
            Lucene101PostingsFormat::VERSION_CURRENT,
            segment_info.get_id(),
            &state.segment_suffix,
        )?;

        let for_delta_util = ForDeltaUtil::new();
        let pfor_util = PForUtil::new();

        let mut pos_out: Option<O> = None;
        let mut pay_out: Option<O> = None;

        let mut pos_delta_buffer = Vec::new();
        let mut payload_length_buffer = Vec::new();
        let mut offset_start_delta_buffer = Vec::new();
        let mut offset_length_buffer = Vec::new();
        let mut payload_bytes = Vec::new();

        if state.field_infos.has_prox() {
            let pos_file = IndexFileNames::segment_file_name(
                &segment_info.name,
                &state.segment_suffix,
                Lucene101PostingsFormat::POS_EXTENSION,
            );
            let mut pos_out_opt = state.directory.create_output(&pos_file, state.context)?;
            CodecUtil::write_index_header(
                &mut pos_out_opt,
                Lucene101PostingsFormat::POS_CODEC,
                Lucene101PostingsFormat::VERSION_CURRENT,
                segment_info.get_id(),
                &state.segment_suffix,
            )?;
            pos_out = Some(pos_out_opt);
            pos_delta_buffer = vec![0; Lucene101PostingsFormat::BLOCK_SIZE];

            if state.field_infos.has_payloads() {
                payload_bytes = vec![0; 128];
                payload_length_buffer = vec![0; Lucene101PostingsFormat::BLOCK_SIZE];
            }

            if state.field_infos.has_offsets() {
                offset_start_delta_buffer = vec![0; Lucene101PostingsFormat::BLOCK_SIZE];
                offset_length_buffer = vec![0; Lucene101PostingsFormat::BLOCK_SIZE];
            }

            if state.field_infos.has_payloads() || state.field_infos.has_offsets() {
                let pay_file = IndexFileNames::segment_file_name(
                    &segment_info.name,
                    &state.segment_suffix,
                    Lucene101PostingsFormat::PAY_EXTENSION,
                );
                let mut pay_out_opt = state.directory.create_output(&pay_file, state.context)?;
                CodecUtil::write_index_header(
                    &mut pay_out_opt,
                    Lucene101PostingsFormat::PAY_CODEC,
                    Lucene101PostingsFormat::VERSION_CURRENT,
                    segment_info.get_id(),
                    &state.segment_suffix,
                )?;
                pay_out = Some(pay_out_opt);
            }
        }

        let doc_delta_buffer = vec![0; Lucene101PostingsFormat::BLOCK_SIZE];
        let freq_buffer = vec![0; Lucene101PostingsFormat::BLOCK_SIZE];

        Ok(Self {
            meta_out,
            doc_out,
            pos_out,
            pay_out,
            last_state: IntBlockTermState::default(),
            doc_start_fp: 0,
            pos_start_fp: 0,
            pay_start_fp: 0,
            doc_delta_buffer,
            freq_buffer,
            doc_buffer_upto: 0,
            pos_delta_buffer,
            payload_length_buffer,
            offset_start_delta_buffer,
            offset_length_buffer,
            pos_buffer_upto: 0,
            payload_bytes,
            payload_byte_upto: 0,
            level0_last_doc_id: 0,
            level0_last_pos_fp: 0,
            level0_last_pay_fp: 0,
            level1_last_doc_id: 0,
            level1_last_pos_fp: 0,
            level1_last_pay_fp: 0,
            doc_id: 0,
            last_doc_id: 0,
            last_position: 0,
            last_start_offset: 0,
            doc_count: 0,
            pfor_util,
            for_delta_util,
            field_has_norms: false,
            level0_freq_norm_accumulator: CompetitiveImpactAccumulator::new(),
            level1_competitive_freq_norm_accumulator: CompetitiveImpactAccumulator::new(),
            max_num_impacts_at_level0: 0,
            max_impact_num_bytes_at_level0: 0,
            max_num_impacts_at_level1: 0,
            max_impact_num_bytes_at_level1: 0,
            scratch_output: ByteBuffersDataOutput::new_resettable_instance(),
            level0_output: ByteBuffersDataOutput::new_resettable_instance(),
            level1_output: ByteBuffersDataOutput::new_resettable_instance(),
        })
    }
    fn flush_doc_block(&mut self, finish_term: bool, options: &FieldWriteOptions) -> Result<()> {
        debug_assert!(self.doc_buffer_upto != 0);

        if (self.doc_buffer_upto as usize) < Lucene101PostingsFormat::BLOCK_SIZE {
            debug_assert!(finish_term);
            PostingsUtil::write_vint_block(
                &mut self.level0_output,
                &mut self.doc_delta_buffer,
                &self.freq_buffer,
                self.doc_buffer_upto,
                options.write_freqs,
            )?;
        } else {
            if options.write_freqs {
                let impacts = self
                    .level0_freq_norm_accumulator
                    .get_competitive_freq_norm_pairs();
                let n = impacts.len() as i32;
                if n > self.max_num_impacts_at_level0 {
                    self.max_num_impacts_at_level0 = n;
                }
                write_impacts(&impacts, &mut self.scratch_output)?;
                debug_assert!(self.level0_output.size() == 0);
                let scratch_len = self.scratch_output.size();
                if scratch_len > self.max_impact_num_bytes_at_level0 as i64 {
                    self.max_impact_num_bytes_at_level0 = scratch_len.try_into()?;
                }
                self.level0_output.write_vlong(scratch_len)?;
                self.scratch_output.copy_to(&mut self.level0_output)?;
                self.scratch_output.reset();

                if options.write_positions {
                    let pos_out = self.pos_out.as_ref().unwrap();
                    self.level0_output
                        .write_vlong(pos_out.get_file_pointer() - self.level0_last_pos_fp)?;
                    self.level0_output.write_byte(self.pos_buffer_upto as u8)?;
                    self.level0_last_pos_fp = pos_out.get_file_pointer();

                    if options.write_offsets || options.write_payloads {
                        let pay_out = self.pay_out.as_ref().unwrap();
                        self.level0_output
                            .write_vlong(pay_out.get_file_pointer() - self.level0_last_pay_fp)?;
                        self.level0_output.write_vint(self.payload_byte_upto)?;
                        self.level0_last_pay_fp = pay_out.get_file_pointer();
                    }
                }
            }

            let mut num_skip_bytes = self.level0_output.size();
            self.for_delta_util
                .encode_deltas(&mut self.doc_delta_buffer, &mut self.level0_output)?;
            if options.write_freqs {
                self.pfor_util
                    .encode(&mut self.freq_buffer, &mut self.level0_output)?;
            }
            // docID - lastBlockDocID is at least 128, so it can never fit a
            // single byte with a vint Even if we subtracted 128,
            // only extremely dense blocks would be eligible to a single byte
            // so let's go with 2 bytes right away
            write_vint15(
                &mut self.scratch_output,
                self.doc_id - self.level0_last_doc_id,
            )?;
            write_vlong15(&mut self.scratch_output, self.level0_output.size())?;
            num_skip_bytes += self.scratch_output.size();

            self.level1_output.write_vlong(num_skip_bytes)?;
            self.scratch_output.copy_to(&mut self.level1_output)?;
            self.scratch_output.reset();
        }

        self.level0_output.copy_to(&mut self.level1_output)?;
        self.level0_output.reset();

        self.level0_last_doc_id = self.doc_id;
        if options.write_freqs {
            self.level1_competitive_freq_norm_accumulator
                .add_all(&self.level0_freq_norm_accumulator);
            self.level0_freq_norm_accumulator.clear();
        }

        if (self.doc_count & Lucene101PostingsFormat::LEVEL1_MASK) == 0 {
            // true every 32 blocks (4,096 docs)
            self.write_level1_skip_data(options)?;
            self.level1_last_doc_id = self.doc_id;
            self.level1_competitive_freq_norm_accumulator.clear();
        } else if finish_term {
            self.level1_output.copy_to(&mut self.doc_out)?;
            self.level1_output.reset();
            self.level1_competitive_freq_norm_accumulator.clear();
        }

        Ok(())
    }
    fn write_level1_skip_data(&mut self, options: &FieldWriteOptions) -> Result<()> {
        self.doc_out
            .write_vint(self.doc_id - self.level1_last_doc_id)?;
        let level1_end: i64;

        if options.write_freqs {
            let impacts = self
                .level1_competitive_freq_norm_accumulator
                .get_competitive_freq_norm_pairs();
            let n = impacts.len() as i32;
            if n > self.max_num_impacts_at_level1 {
                self.max_num_impacts_at_level1 = n;
            }
            write_impacts(&impacts, &mut self.scratch_output)?;
            let num_impact_bytes = self.scratch_output.size();
            if num_impact_bytes > self.max_impact_num_bytes_at_level1 as i64 {
                self.max_impact_num_bytes_at_level1 = num_impact_bytes.try_into()?;
            }
            if options.write_positions {
                let pos_fp = self.pos_out.as_ref().unwrap().get_file_pointer();
                self.scratch_output
                    .write_vlong(pos_fp - self.level1_last_pos_fp)?;
                self.scratch_output.write_byte(self.pos_buffer_upto as u8)?;
                self.level1_last_pos_fp = pos_fp;
                if options.write_offsets || options.write_payloads {
                    let pay_fp = self.pay_out.as_ref().unwrap().get_file_pointer();
                    self.scratch_output
                        .write_vlong(pay_fp - self.level1_last_pay_fp)?;
                    self.scratch_output.write_vint(self.payload_byte_upto)?;
                    self.level1_last_pay_fp = pay_fp;
                }
            }
            let level1_len = 2 * BitUtil::SHORT_BYTES as i64
                + self.scratch_output.size()
                + self.level1_output.size();
            self.doc_out.write_vlong(level1_len)?;
            level1_end = self.doc_out.get_file_pointer() + level1_len;
            // There are at most 128 impacts, that require at most 2 bytes each
            debug_assert!(self.scratch_output.size() <= i16::MAX as i64);
            // Like impacts plus a few vlongs, still way under the max short
            // value
            debug_assert!(
                (self.scratch_output.size() + BitUtil::SHORT_BYTES as i64) <= i16::MAX as i64
            );
            self.doc_out
                .write_short((self.scratch_output.size() + BitUtil::SHORT_BYTES as i64) as i16)?;
            self.doc_out
                .write_short(self.scratch_output.size() as i16)?;
            self.scratch_output.copy_to(&mut self.doc_out)?;
            self.scratch_output.reset();
        } else {
            self.doc_out.write_vlong(self.level1_output.size())?;
            level1_end = self.doc_out.get_file_pointer() + self.level1_output.size();
        }

        self.level1_output.copy_to(&mut self.doc_out)?;
        self.level1_output.reset();
        debug_assert_eq!(self.doc_out.get_file_pointer(), level1_end);
        Ok(())
    }
    pub fn close(&mut self) {
        let result = (|| -> Result<()> {
            CodecUtil::write_footer(&mut self.doc_out)?;
            if let Some(ref mut po) = self.pos_out {
                CodecUtil::write_footer(po)?;
            }
            if let Some(ref mut pay) = self.pay_out {
                CodecUtil::write_footer(pay)?;
            }
            self.meta_out.write_int(self.max_num_impacts_at_level0)?;
            self.meta_out
                .write_int(self.max_impact_num_bytes_at_level0)?;
            self.meta_out.write_int(self.max_num_impacts_at_level1)?;
            self.meta_out
                .write_int(self.max_impact_num_bytes_at_level1)?;
            self.meta_out.write_long(self.doc_out.get_file_pointer())?;
            if let Some(ref po) = self.pos_out {
                self.meta_out.write_long(po.get_file_pointer())?;
                if let Some(ref pay) = self.pay_out {
                    self.meta_out.write_long(pay.get_file_pointer())?;
                }
            }
            CodecUtil::write_footer(&mut self.meta_out)?;
            Ok(())
        })();
        match result {
            Ok(_) => {},
            Err(e) => {
                eprintln!("Failed to close: {e}");
            },
        }
    }
}
impl<O> Drop for Lucene101PostingsWriter<O>
where
    O: IndexOutput,
{
    fn drop(&mut self) {
        self.close();
    }
}
impl<O> PostingsWriterBase for Lucene101PostingsWriter<O>
where
    O: IndexOutput,
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
        CodecUtil::write_index_header(
            terms_out,
            Lucene101PostingsFormat::TERMS_CODEC,
            Lucene101PostingsFormat::VERSION_CURRENT,
            segment_info.get_id(),
            &state.segment_suffix,
        )?;
        terms_out.write_vint(Lucene101PostingsFormat::BLOCK_SIZE as i32)?;
        Ok(())
    }

    fn write_term<N: NormsProducer, PE: PostingsEnum>(
        &mut self,
        _term: &BytesRef<Vec<u8>>,
        _terms_enum: &mut impl TermsEnum<PostingsEnum = PE>,
        _docs_seen: &mut FixedBitSet,
        _norms: &Option<N>,
        _postings_enum: Option<PE>,
    ) -> Result<(Option<PE>, Option<BlockTermStateEnum>)> {
        Err(LuceneError::not_implemented(""))
    }

    fn encode_term(
        &mut self,
        _out: &mut impl DataOutput,
        _field_info: &FieldInfo,
        _state: Cow<BlockTermStateEnum>,
        _absolute: bool,
    ) -> Result<()> {
        Err(LuceneError::unreachable("should not be called"))
    }

    fn set_field(&mut self, field_info: Arc<FieldInfo>) {
        self.last_state = IntBlockTermState::default();
        self.field_has_norms = field_info.has_norms();
    }
}
impl<O> PushPostingsWriterBaseAbstract for Lucene101PostingsWriter<O>
where
    O: IndexOutput,
{
    fn new_term_state(&mut self) -> Result<BlockTermStateEnum> {
        Ok(BlockTermStateEnum::Int(IntBlockTermState::default()))
    }

    fn start_term(&mut self, options: &FieldWriteOptions) -> Result<()> {
        self.doc_start_fp = self.doc_out.get_file_pointer();
        if options.write_positions
            && let Some(ref pos_out) = self.pos_out
        {
            self.pos_start_fp = pos_out.get_file_pointer();
            self.level0_last_pos_fp = self.pos_start_fp;
            self.level1_last_pos_fp = self.pos_start_fp;
            if options.write_payloads || options.write_offsets {
                let pay_fp = self.pay_out.as_ref().unwrap().get_file_pointer();
                self.pay_start_fp = pay_fp;
                self.level0_last_pay_fp = pay_fp;
                self.level1_last_pay_fp = pay_fp;
            }
        }
        self.last_doc_id = -1;
        self.level0_last_doc_id = -1;
        self.level1_last_doc_id = -1;
        if options.write_freqs {
            self.level0_freq_norm_accumulator.clear();
        }
        Ok(())
    }

    fn finish_term(
        &mut self,
        state: &mut BlockTermStateEnum,
        options: &FieldWriteOptions,
    ) -> Result<()> {
        let state = match state {
            BlockTermStateEnum::Int(state) => state,
            _ => {
                return Err(LuceneError::illegal_state(
                    "not IntBlockTermState".to_string(),
                ));
            },
        };
        debug_assert!(state.base.doc_freq > 0);
        debug_assert_eq!(state.base.doc_freq, self.doc_count);

        let singleton_doc_id = if state.base.doc_freq == 1 {
            self.doc_delta_buffer[0] - 1
        } else {
            self.flush_doc_block(true, options)?;
            -1
        };

        let last_pos_block_offset = if options.write_positions {
            // totalTermFreq is just total number of positions(or payloads, or
            // offsets) associated with current term.
            debug_assert!(state.base.total_term_freq != -1);
            let offset =
                if state.base.total_term_freq as usize > Lucene101PostingsFormat::BLOCK_SIZE {
                    // record file offset for last pos in last block
                    self.pos_out.as_ref().unwrap().get_file_pointer() - self.pos_start_fp
                } else {
                    -1
                };
            if self.pos_buffer_upto > 0 {
                debug_assert!(
                    (self.pos_buffer_upto as usize) < Lucene101PostingsFormat::BLOCK_SIZE
                );
                // TODO: should we send offsets/payloads to
                // .pay...?  seems wasteful (have to store extra
                // vLong for low (< BLOCK_SIZE) DF terms = vast vast
                // majority)

                // vInt encode the remaining positions/payloads/offsets:
                let mut last_payload_length = -1;
                let mut last_offset_length = -1;
                let mut payload_bytes_read_upto = 0;
                let po_out = self.pos_out.as_mut().unwrap();
                for i in 0..self.pos_buffer_upto as usize {
                    let pos_delta = self.pos_delta_buffer[i];
                    if options.write_payloads {
                        let payload_length = self.payload_length_buffer[i];
                        if payload_length != last_payload_length {
                            last_payload_length = payload_length;
                            po_out.write_vint((pos_delta << 1) | 1)?;
                            po_out.write_vint(payload_length)?;
                        } else {
                            po_out.write_vint(pos_delta << 1)?;
                        }
                        if payload_length != 0 {
                            po_out.write_bytes_range(
                                &self.payload_bytes,
                                payload_bytes_read_upto,
                                payload_length,
                            )?;
                            payload_bytes_read_upto += payload_length;
                        }
                    } else {
                        po_out.write_vint(pos_delta)?;
                    }
                    if options.write_offsets {
                        let delta = self.offset_start_delta_buffer[i];
                        let length = self.offset_length_buffer[i];
                        if length == last_offset_length {
                            po_out.write_vint(delta << 1)?;
                        } else {
                            po_out.write_vint((delta << 1) | 1)?;
                            po_out.write_vint(length)?;
                            last_offset_length = length;
                        }
                    }
                }
                if options.write_payloads {
                    debug_assert_eq!(payload_bytes_read_upto, self.payload_byte_upto);
                    self.payload_byte_upto = 0;
                }
            }
            offset
        } else {
            -1
        };

        state.doc_start_fp = self.doc_start_fp;
        state.pos_start_fp = self.pos_start_fp;
        state.pay_start_fp = self.pay_start_fp;
        state.singleton_doc_id = singleton_doc_id;
        state.last_pos_block_offset = last_pos_block_offset;

        self.doc_buffer_upto = 0;
        self.pos_buffer_upto = 0;
        self.last_doc_id = -1;
        self.doc_count = 0;
        Ok(())
    }

    fn start_doc<N: NormsProducer>(
        &mut self,
        norms: &mut Option<N::NumericDocValues>,
        doc_id: i32,
        term_doc_freq: i32,
        options: &FieldWriteOptions,
    ) -> Result<()> {
        if self.doc_buffer_upto as usize == Lucene101PostingsFormat::BLOCK_SIZE {
            self.flush_doc_block(false, options)?;
            self.doc_buffer_upto = 0;
        }

        let doc_delta = doc_id - self.last_doc_id;
        if doc_id < 0 || doc_delta <= 0 {
            return Err(LuceneError::corrupt_index(format!(
                "docs out of order ({} <= {}) resource {}",
                doc_id, self.last_doc_id, self.doc_out
            )));
        }

        let idx = self.doc_buffer_upto as usize;
        self.doc_delta_buffer[idx] = doc_delta;
        if options.write_freqs {
            self.freq_buffer[idx] = term_doc_freq;
        }

        self.doc_id = doc_id;
        self.last_position = 0;
        self.last_start_offset = 0;

        if options.write_freqs {
            let norm = if self.field_has_norms {
                debug_assert!(norms.is_some(), "norms should not be None");
                let found = norms.as_mut().unwrap().advance_exact(doc_id)?;
                if !found {
                    1
                } else {
                    let n = norms.as_mut().unwrap().long_value()?;
                    debug_assert!(n != 0, "norm for doc {doc_id} is zero");
                    n
                }
            } else {
                1
            };
            self.level0_freq_norm_accumulator.add(term_doc_freq, norm);
        }

        Ok(())
    }

    fn add_position(
        &mut self,
        position: i32,
        payload: Option<Cow<'_, BytesRef<Vec<u8>>>>,
        start_offset: i32,
        end_offset: i32,
        options: &FieldWriteOptions,
    ) -> Result<()> {
        if position > MAX_POSITION {
            return Err(LuceneError::corrupt_index(format!(
                "position={} is too large (> IndexWriter.MAX_POSITION={})  resource {}",
                position, MAX_POSITION, self.doc_out
            )));
        }
        if position < 0 {
            return Err(LuceneError::corrupt_index(format!(
                "position={} is < 0  resource {}",
                position, self.doc_out
            )));
        }

        let idx = self.pos_buffer_upto as usize;
        self.pos_delta_buffer[idx] = position - self.last_position;

        if let Some(p) = payload.as_ref() {
            if p.length == 0 {
                self.payload_length_buffer[idx] = 0;
            } else {
                self.payload_length_buffer[idx] = p.length as i32;
                if self.payload_byte_upto as usize + p.length > self.payload_bytes.len() {
                    ArrayUtil::grow_with_len(
                        &mut self.payload_bytes,
                        self.payload_byte_upto as usize + p.length,
                    );
                }
                let start = p.offset;
                self.payload_bytes.copy_from(
                    &p.bytes[start..start + p.length],
                    self.payload_byte_upto as usize,
                );
                self.payload_byte_upto += p.length as i32;
            }
        } else {
            self.payload_length_buffer[idx] = 0;
        }

        if options.write_offsets {
            debug_assert!(start_offset >= self.last_start_offset);
            debug_assert!(end_offset >= start_offset);
            self.offset_start_delta_buffer[idx] = start_offset - self.last_start_offset;
            self.offset_length_buffer[idx] = end_offset - start_offset;
            self.last_start_offset = start_offset;
        }

        self.pos_buffer_upto += 1;
        self.last_position = position;

        if self.pos_buffer_upto as usize == Lucene101PostingsFormat::BLOCK_SIZE {
            let po = self.pos_out.as_mut().unwrap();
            self.pfor_util.encode(&mut self.pos_delta_buffer, po)?;
            if options.write_payloads {
                let pay_out = self.pay_out.as_mut().unwrap();
                self.pfor_util
                    .encode(&mut self.payload_length_buffer, pay_out)?;
                pay_out.write_vint(self.payload_byte_upto)?;
                pay_out.write_bytes_range(&self.payload_bytes, 0, self.payload_byte_upto)?;
                self.payload_byte_upto = 0;
            }
            if options.write_offsets {
                let pay_out = self.pay_out.as_mut().unwrap();
                self.pfor_util
                    .encode(&mut self.offset_start_delta_buffer, pay_out)?;
                self.pfor_util
                    .encode(&mut self.offset_length_buffer, pay_out)?;
            }
            self.pos_buffer_upto = 0;
        }

        Ok(())
    }

    fn finish_doc(&mut self) -> Result<()> {
        self.doc_buffer_upto += 1;
        self.doc_count += 1;
        self.last_doc_id = self.doc_id;
        Ok(())
    }

    fn encode_term_with_option(
        &mut self,
        out: &mut impl DataOutput,
        _field_info: &FieldInfo,
        state: Cow<BlockTermStateEnum>,
        absolute: bool,
        options: &FieldWriteOptions,
    ) -> Result<()> {
        let state = match state {
            Cow::Borrowed(b) => b.clone(),
            Cow::Owned(o) => o,
        };
        let state = match state {
            BlockTermStateEnum::Int(state) => state,
            _ => {
                return Err(LuceneError::illegal_state(
                    "not IntBlockTermState".to_string(),
                ));
            },
        };
        if absolute {
            self.last_state = IntBlockTermState::default();
            debug_assert_eq!(self.last_state.doc_start_fp, 0);
        }

        if self.last_state.singleton_doc_id != -1
            && state.singleton_doc_id != -1
            && state.doc_start_fp == self.last_state.doc_start_fp
        {
            // With runs of rare values such as ID fields, the increment of
            // pointers in the docs file is often 0.
            // Furthermore some ID schemes like auto-increment IDs or Flake IDs
            // are monotonic, so we encode the delta
            // between consecutive doc IDs to save space.
            let delta = (state.singleton_doc_id - self.last_state.singleton_doc_id) as i64;
            out.write_vlong((BitUtil::zig_zag_encode_i64(delta) << 1) | 1)?;
        } else {
            out.write_vlong((state.doc_start_fp - self.last_state.doc_start_fp) << 1)?;
            if state.singleton_doc_id != -1 {
                out.write_vint(state.singleton_doc_id)?;
            }
        }

        if options.write_positions {
            out.write_vlong(state.pos_start_fp - self.last_state.pos_start_fp)?;
            if options.write_payloads || options.write_offsets {
                out.write_vlong(state.pay_start_fp - self.last_state.pay_start_fp)?;
            }
            if state.last_pos_block_offset != -1 {
                out.write_vlong(state.last_pos_block_offset)?;
            }
        }

        self.last_state = state;
        Ok(())
    }
}

use crate::core::index::impact::Impact;

/// Special vints that are encoded on 2 bytes if they require 15 bits or
/// less. VInt becomes especially slow when the number of bytes is
/// variable, so this special layout helps in the case when the number
/// likely requires 15 bits or less.
pub(crate) fn write_vint15(out: &mut impl DataOutput, v: i32) -> Result<()> {
    debug_assert!(v >= 0);
    write_vlong15(out, v as i64)
}

/// @see [`write_vint15`]
pub(crate) fn write_vlong15(out: &mut impl DataOutput, v: i64) -> Result<()> {
    debug_assert!(v >= 0);
    if v & !0x7FFF == 0 {
        out.write_short(v as i16)?;
    } else {
        let prefix = 0x8000 | (v & 0x7FFF);
        out.write_short(prefix as i16)?;
        out.write_vlong(v >> 15)?;
    }
    Ok(())
}
pub(crate) fn write_impacts(impacts: &[Impact], out: &mut impl DataOutput) -> Result<()> {
    let mut previous = Impact { freq: 0, norm: 0 };
    for impact in impacts {
        debug_assert!(impact.freq > previous.freq);
        debug_assert!((impact.norm as u64) > (previous.norm as u64));
        let freq_delta = impact.freq - previous.freq - 1;
        let norm_delta = impact.norm - previous.norm - 1;
        if norm_delta == 0 {
            // most of time, norm only increases by 1, so we can fold
            // everything in a single byte
            out.write_vint(freq_delta << 1)?;
        } else {
            out.write_vint((freq_delta << 1) | 1)?;
            out.write_zlong(norm_delta)?;
        }
        previous = impact.clone();
    }
    Ok(())
}
