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
use crate::core::codecs::block_tree::compression_algorithm::CompressionAlgorithm;
use crate::core::codecs::block_tree::field_reader::FieldReader;
use crate::core::codecs::block_tree::intersect_terms_enum::IntersectTermsEnum;
use crate::core::codecs::block_tree::lucene90_block_tree_terms_reader::OUTPUT_FLAG_IS_FLOOR;
use crate::core::codecs::postings_reader_base::PostingsReaderBase;
use crate::core::index::BytesRef;
use crate::core::index::index_options::IndexOptions::Docs;
use crate::core::store::{ByteArrayDataInput, DataInput, IndexInput};

use crate::core::util::TryIntoInt;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::automation::byte_runnable::ByteRunnable;
use crate::core::util::automation::transition::Transition;
use crate::core::util::automation::transition_accessor::{
    TransitionAccessor, TransitionAccessorEnum,
};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::sync::Arc;

pub(crate) struct IntersectTermsEnumFrame {
    pub(crate) ord: i32,
    pub(crate) fp: i64,
    pub(crate) fp_orig: i64,
    fp_end: i64,
    pub(crate) last_sub_fp: i64,

    /// State in automaton
    pub(crate) state: i32,
    /// State just before the last label
    pub(crate) last_state: i32,
    meta_data_upto: i32,
    pub(crate) suffixes_reader: ByteArrayDataInput<Vec<u8>>,
    pub(crate) suffix_lengths_reader: ByteArrayDataInput<Vec<u8>>,
    stats_singleton_run_length: i32,
    stats_reader: ByteArrayDataInput<Vec<u8>>,
    floor_data_reader: ByteArrayDataInput<Arc<Vec<u8>>>,
    /// Length of prefix shared by all terms in this block
    pub(crate) prefix: usize,
    /// Number of entries (term or sub-block) in this block
    pub(crate) ent_count: i32,
    /// Which term we will next read
    pub(crate) next_ent: i32,
    /// True if this block is either not a floor block,
    /// or, it's the last sub-block of a floor block
    pub(crate) is_last_in_floor: bool,
    /// True if all entries are terms
    is_leaf_block: bool,
    num_follow_floor_blocks: i32,
    next_floor_label: i32,
    pub(crate) transition: Transition,
    pub(crate) transition_index: i32,
    pub(crate) transition_count: i32,
    pub(crate) arc: usize,
    // arc: FstArcBytesRef,
    pub(crate) term_state: BlockTermStateEnum,
    /// metadata buffer
    bytes_reader: ByteArrayDataInput<Vec<u8>>,
    pub(crate) output_num: i32,
    pub(crate) start_byte_pos: usize,
    pub(crate) suffix: usize,
}
impl IntersectTermsEnumFrame {
    pub fn new<I, P>(ord: i32, fr: &FieldReader<I, P>) -> Result<Self>
    where
        I: IndexInput,
        P: PostingsReaderBase<TermState = BlockTermStateEnum>,
    {
        // newTermState()
        let mut term_state = fr
            .parent
            .as_ref()
            .unwrap()
            .postings_reader
            .new_term_state()?;

        term_state.get_block_term_state_mut().total_term_freq = -1;
        let suffix_length_bytes = vec![0u8; 32];
        let suffix_bytes = vec![0u8; 128];
        let stat_bytes = vec![0u8; 64];
        let bytes = vec![0u8; 32];

        Ok(Self {
            ord,
            fp: 0,
            fp_orig: 0,
            fp_end: 0,
            last_sub_fp: 0,

            state: 0,
            last_state: 0,
            meta_data_upto: 0,

            suffixes_reader: ByteArrayDataInput::with_bytes(suffix_bytes),
            suffix_lengths_reader: ByteArrayDataInput::with_bytes(suffix_length_bytes),

            stats_singleton_run_length: 0,
            stats_reader: ByteArrayDataInput::with_bytes(stat_bytes),
            floor_data_reader: ByteArrayDataInput::new(),

            prefix: 0,
            ent_count: 0,
            next_ent: 0,
            is_last_in_floor: false,
            is_leaf_block: false,

            num_follow_floor_blocks: 0,
            next_floor_label: 0,

            transition: Transition::default(),
            transition_index: 0,
            transition_count: 0,

            arc: 0,

            term_state,

            bytes_reader: ByteArrayDataInput::with_bytes(bytes),

            output_num: 0,
            start_byte_pos: 0,
            suffix: 0,
        })
    }
    pub(crate) fn load_next_floor_block<I, P>(
        ite: &mut IntersectTermsEnum<I, P>,
        frame_idx: usize,
    ) -> Result<()>
    where
        I: IndexInput,
        P: PostingsReaderBase<TermState = BlockTermStateEnum>,
    {
        let frame = &mut ite.stack[frame_idx];
        debug_assert!(
            frame.num_follow_floor_blocks > 0,
            "next_floor_label={}",
            frame.next_floor_label
        );

        loop {
            let delta: i64 = (frame.floor_data_reader.read_vlong()? as u64 >> 1).try_convert()?;
            frame.fp = frame.fp_orig + delta;
            frame.num_follow_floor_blocks -= 1;
            if frame.num_follow_floor_blocks != 0 {
                frame.next_floor_label = (frame.floor_data_reader.read_byte()?) as i32;
            } else {
                frame.next_floor_label = 256;
            }

            if frame.num_follow_floor_blocks == 0 || frame.next_floor_label > frame.transition.min {
                break;
            }
        }
        Self::load(ite, frame_idx, None)
    }
    pub(crate) fn set_state(
        automaton: &TransitionAccessorEnum,
        frame: &mut IntersectTermsEnumFrame,
        state: i32,
    ) -> Result<()> {
        frame.state = state;
        frame.transition_index = 0;
        frame.transition_count = automaton.get_num_transitions_with_state(state);
        if frame.transition_count != 0 {
            automaton.init_transition(state, &mut frame.transition);
            automaton.get_next_transition(&mut frame.transition);
        } else {
            // Must set min to -1 so the "label < min" check never falsely triggers:
            frame.transition.min = -1;

            // Must set max to -1 so we immediately realize we need to step
            // to the next transition and then pop this frame:
            frame.transition.max = -1;
        }

        Ok(())
    }
    pub(crate) fn load_from_frame_index_data<I, P>(
        ite: &mut IntersectTermsEnum<I, P>,
        frame_index_data: BytesRef<Arc<Vec<u8>>>,
        frame_idx: usize,
    ) -> Result<()>
    where
        I: IndexInput,
        P: PostingsReaderBase<TermState = BlockTermStateEnum>,
    {
        let frame = &mut ite.stack[frame_idx];
        frame.floor_data_reader.reset_with_range(
            frame_index_data.bytes,
            frame_index_data.offset,
            frame_index_data.length,
        );
        let code = ite.fr.read_vlong_output(&mut frame.floor_data_reader)?;
        Self::load(ite, frame_idx, Some(code))
    }
    pub(crate) fn load_from_output_accumulator<I, P>(
        ite: &mut IntersectTermsEnum<I, P>,
        frame_idx: usize,
    ) -> Result<()>
    where
        I: IndexInput,
        P: PostingsReaderBase<TermState = BlockTermStateEnum>,
    {
        let frame = &mut ite.stack[frame_idx];
        ite.output_accumulator.prepare_read();
        let code = ite.fr.read_vlong_output(&mut ite.output_accumulator)?;
        ite.output_accumulator
            .set_floor_data(&mut frame.floor_data_reader);
        Self::load(ite, frame_idx, Some(code))
    }
    fn load<I, P>(
        ite: &mut IntersectTermsEnum<I, P>,
        frame_idx: usize,
        block_code: Option<i64>,
    ) -> Result<()>
    where
        I: IndexInput,
        P: PostingsReaderBase<TermState = BlockTermStateEnum>,
    {
        let frame = &mut ite.stack[frame_idx];
        if let Some(block_code) = block_code {
            // This block is the first one in a possible sequence of floor blocks corresponding to a
            // single seek point from the FST terms index
            if (block_code & OUTPUT_FLAG_IS_FLOOR as i64) != 0 {
                // Floor frame
                frame.num_follow_floor_blocks = frame.floor_data_reader.read_vint()?;
                frame.next_floor_label = (frame.floor_data_reader.read_byte()?) as i32;

                // If current state is not accept and has transitions,
                // process first block in case it has empty suffix
                if !ite.run_automation.is_accept(frame.state)? && frame.transition_count != 0 {
                    // Maybe skip floor blocks:
                    debug_assert!(
                        frame.transition_index == 0,
                        "transitionIndex={}",
                        frame.transition_index
                    );

                    while frame.num_follow_floor_blocks != 0
                        && frame.next_floor_label <= frame.transition.min
                    {
                        let delta: i64 =
                            (frame.floor_data_reader.read_vlong()? as u64 >> 1).try_convert()?;
                        frame.fp = frame.fp_orig + delta;
                        frame.num_follow_floor_blocks -= 1;
                        if frame.num_follow_floor_blocks != 0 {
                            frame.next_floor_label = (frame.floor_data_reader.read_byte()?) as i32;
                        } else {
                            frame.next_floor_label = 256;
                        }
                    }
                }
            }
        }
        let input = ite
            .input
            .as_mut()
            .ok_or_else(|| LuceneError::number_format("input not init yey"))?;
        input.seek(frame.fp as usize)?;

        let code = input.read_vint()?;
        frame.ent_count = (code as u32 >> 1).try_convert()?;
        debug_assert!(frame.ent_count > 0);
        frame.is_last_in_floor = (code & 1) != 0;

        // term suffixed
        let code_l = input.read_vlong()?;

        frame.is_leaf_block = (code_l & 0x04) != 0;
        let num_suffix_bytes = (code_l as u64 >> 3) as usize;

        if frame.suffixes_reader.bytes.len() < num_suffix_bytes {
            let new_len = ArrayUtil::oversize(num_suffix_bytes, 1);
            frame.suffixes_reader.bytes = vec![0u8; new_len];
        }

        let compression_alg = match CompressionAlgorithm::by_code((code_l & 0x03).try_convert()?) {
            Ok(alg) => alg,
            Err(e) => {
                return Err(LuceneError::corrupt_index(format!(
                    "Corrupted suffix compression algorithm: {}",
                    e
                )));
            },
        };

        compression_alg.read(
            input,
            &mut frame.suffixes_reader.bytes,
            num_suffix_bytes.try_convert()?,
        )?;

        frame.suffixes_reader.reset_meta(0, num_suffix_bytes);

        let mut num_suffix_len_bytes = input.read_vint()?;
        let all_equal = (num_suffix_len_bytes & 1) != 0;
        debug_assert!(num_suffix_len_bytes >= 0);
        num_suffix_len_bytes >>= 1;

        let num_suffix_len_bytes = num_suffix_len_bytes as usize;

        if frame.suffix_lengths_reader.bytes.len() < num_suffix_len_bytes {
            let new_len = ArrayUtil::oversize(num_suffix_len_bytes, 1);
            frame.suffix_lengths_reader.bytes = vec![0u8; new_len];
        }

        if all_equal {
            let b = input.read_byte()?;
            frame.suffix_lengths_reader.bytes[..num_suffix_len_bytes].fill(b);
        } else {
            input.read_bytes(
                &mut frame.suffix_lengths_reader.bytes,
                0,
                num_suffix_len_bytes,
            )?;
        }

        frame
            .suffix_lengths_reader
            .reset_meta(0, num_suffix_len_bytes);

        // stats
        let num_bytes = input.read_vint()? as usize;
        if frame.stats_reader.bytes.len() < num_bytes {
            let new_len = ArrayUtil::oversize(num_bytes, 1);
            frame.stats_reader.bytes = vec![0u8; new_len];
        }
        input.read_bytes(&mut frame.stats_reader.bytes, 0, num_bytes)?;

        frame.stats_reader.reset_meta(0, num_bytes);
        frame.stats_singleton_run_length = 0;
        frame.meta_data_upto = 0;
        frame.term_state.get_block_term_state_mut().term_block_ord = 0;
        frame.next_ent = 0;

        // metadata
        let num_bytes = input.read_vint()? as usize;
        if frame.bytes_reader.bytes.len() < num_bytes {
            let new_len = ArrayUtil::oversize(num_bytes, 1);
            frame.bytes_reader.bytes = vec![0u8; new_len];
        }
        input.read_bytes(&mut frame.bytes_reader.bytes, 0, num_bytes)?;

        frame.bytes_reader.reset_meta(0, num_bytes);

        if !frame.is_last_in_floor {
            // Sub-blocks of a single floor block are always
            // written one after another -- tail recurse:
            // tail recursion boundary for floor blocks
            frame.fp_end = input.get_file_pointer()? as i64;
        }

        Ok(())
    }
    /// Decodes next entry; returns true if it's a sub-block.
    pub(crate) fn next(&mut self) -> Result<bool> {
        if self.is_leaf_block {
            self.next_leaf()?;
            Ok(false)
        } else {
            self.next_non_leaf()
        }
    }
    pub(crate) fn next_leaf(&mut self) -> Result<()> {
        debug_assert!(
            self.next_ent != -1 && self.next_ent < self.ent_count,
            "nextEnt={} entCount={} fp={}",
            self.next_ent,
            self.ent_count,
            self.fp
        );
        self.next_ent += 1;
        self.suffix = self.suffix_lengths_reader.read_vint()?.try_convert()?;
        self.start_byte_pos = self.suffixes_reader.get_position();
        self.suffixes_reader.skip_bytes(self.suffix as i64)?;
        Ok(())
    }
    pub(crate) fn next_non_leaf(&mut self) -> Result<bool> {
        debug_assert!(
            self.next_ent != -1 && self.next_ent < self.ent_count,
            "nextEnt={} entCount={} fp={}",
            self.next_ent,
            self.ent_count,
            self.fp
        );
        self.next_ent += 1;
        let code = self.suffix_lengths_reader.read_vint()?;
        self.suffix = (code as u32 >> 1) as usize;
        self.start_byte_pos = self.suffixes_reader.get_position();
        self.suffixes_reader.skip_bytes(self.suffix as i64)?;
        if (code & 1) == 0 {
            self.term_state.get_block_term_state_mut().term_block_ord += 1;
            Ok(false)
        } else {
            let delta = self.suffix_lengths_reader.read_vlong()?;
            self.last_sub_fp = self.fp - delta;
            Ok(true)
        }
    }
    pub(crate) fn get_term_block_ord(&self) -> i32 {
        if self.is_leaf_block {
            self.next_ent
        } else {
            self.term_state.get_block_term_state().term_block_ord
        }
    }
    pub(crate) fn decode_meta_data<I, P>(
        ite: &mut IntersectTermsEnum<I, P>,
        frame_idx: usize,
    ) -> Result<()>
    where
        I: IndexInput,
        P: PostingsReaderBase<TermState = BlockTermStateEnum>,
    {
        let frame = &mut ite.stack[frame_idx];
        // lazily catch up on metadata decode:
        let limit = frame.get_term_block_ord();
        let mut absolute = frame.meta_data_upto == 0;
        debug_assert!(limit > 0);
        while frame.meta_data_upto < limit {
            let term_state = frame.term_state.get_block_term_state_mut();

            if frame.stats_singleton_run_length > 0 {
                term_state.doc_freq = 1;
                term_state.total_term_freq = 1;
                frame.stats_singleton_run_length -= 1;
            } else {
                let token = frame.stats_reader.read_vint()?;

                if (token & 1) == 1 {
                    // singleton run
                    term_state.doc_freq = 1;
                    term_state.total_term_freq = 1;
                    frame.stats_singleton_run_length = (token as u32 >> 1).try_convert()?;
                } else {
                    term_state.doc_freq = (token as u32 >> 1).try_convert()?;

                    if *ite.fr.field_info.get_index_options() == Docs {
                        term_state.total_term_freq = term_state.doc_freq as i64;
                    } else {
                        let delta = frame.stats_reader.read_vlong()?;
                        term_state.total_term_freq = term_state.doc_freq as i64 + delta;
                    }
                }
            }
            // metadata
            ite.fr
                .parent
                .as_ref()
                .unwrap()
                .postings_reader
                .decode_term(
                    &mut frame.bytes_reader,
                    &ite.fr.field_info,
                    &mut frame.term_state,
                    absolute,
                )?;

            frame.meta_data_upto += 1;
            absolute = false;
        }

        frame.term_state.get_block_term_state_mut().term_block_ord = frame.meta_data_upto;

        Ok(())
    }
}
