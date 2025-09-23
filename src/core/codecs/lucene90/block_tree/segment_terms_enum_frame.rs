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
use crate::core::codecs::block_tree::field_reader::FieldReader;
use crate::core::codecs::block_tree::segment_terms_enum::SegmentTermsEnum;
use crate::core::codecs::lucene90::block_tree::compression_algorithm::CompressionAlgorithm;
use crate::core::codecs::lucene90::block_tree::segment_terms_enum::OutputAccumulator;
use crate::core::codecs::postings_reader_base::PostingsReaderBase;
use crate::core::index::BytesRef;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::terms_enum::SeekStatus;
use crate::core::store::{ByteArrayDataInput, DataInput, IndexInput};
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::{SliceCopyOps, ToInt};
use std::sync::Arc;

pub struct SegmentTermsEnumFrame {
    /// Our index in stack[]
    pub(crate) ord: i32,

    pub(crate) has_terms: bool,
    pub(crate) has_terms_orig: bool,
    pub(crate) is_floor: bool,

    pub(crate) arc: Option<usize>,

    /// File pointer where this block was loaded from
    pub(crate) fp: i64,
    pub(crate) fp_orig: i64,
    pub(crate) fp_end: i64,
    pub(crate) total_suffix_bytes: i64, // for stats
    pub(crate) suffix_bytes: Vec<u8>,
    pub(crate) suffixes_reader: ByteArrayDataInput<Vec<u8>>,

    pub(crate) suffix_length_bytes: Vec<u8>,
    pub(crate) suffix_lengths_reader: ByteArrayDataInput<Vec<u8>>,
    pub(crate) stat_bytes: Vec<u8>,
    pub(crate) stats_singleton_run_length: i32,
    pub(crate) stats_reader: ByteArrayDataInput<Vec<u8>>,

    pub(crate) rewind_pos: i32,
    pub(crate) floor_data_reader: ByteArrayDataInput<Arc<Vec<u8>>>,

    // Length of prefix shared by all terms in this block
    pub(crate) prefix_length: i32,

    // Number of entries (term or sub-block) in this block
    pub(crate) ent_count: i32,

    // Which term we will next read, or -1 if the block isn't loaded yet
    pub(crate) next_ent: i32,

    // True if this block is either not a floor block, or it's the last sub-block of a floor block
    pub(crate) is_last_in_floor: bool,

    // True if all entries are terms
    pub(crate) is_leaf_block: bool,

    // True if all entries have the same length.
    pub(crate) all_equal: bool,

    pub(crate) last_sub_fp: i64,

    pub(crate) next_floor_label: i32,
    pub(crate) num_follow_floor_blocks: i32,

    // Next term to decode metaData; we decode metaData
    // lazily so that scanning to find the matching term is
    // fast and only if you find a match and app wants the
    // stats or docs/positions enums, will we decode the
    // metaData
    pub(crate) meta_data_upto: i32,

    pub(crate) state: BlockTermStateEnum,

    // metadata buffer
    pub(crate) bytes: Vec<u8>,
    pub(crate) bytes_reader: ByteArrayDataInput<Vec<u8>>,

    start_byte_pos: i32,
    suffix_length: i32,
    sub_code: i64,
    compression_alg: CompressionAlgorithm,
}
impl SegmentTermsEnumFrame {
    pub fn new<I, P>(ord: i32, fr: &FieldReader<I, P>) -> Result<Self>
    where
        I: IndexInput,
        P: PostingsReaderBase<TermState = BlockTermStateEnum>,
    {
        let mut state = fr.parent.postings_reader.new_term_state()?;
        state.get_block_term_state().total_term_freq = -1;
        Ok(Self {
            ord,
            state,

            arc: None,
            has_terms: false,
            has_terms_orig: false,
            is_floor: false,

            fp: 0,
            fp_orig: 0,
            fp_end: 0,
            total_suffix_bytes: 0,

            suffix_bytes: vec![0u8; 128],
            suffixes_reader: ByteArrayDataInput::new(),

            suffix_length_bytes: vec![0u8; 32],
            suffix_lengths_reader: ByteArrayDataInput::new(),

            stat_bytes: vec![0u8; 64],
            stats_singleton_run_length: 0,
            stats_reader: ByteArrayDataInput::new(),

            rewind_pos: 0,
            floor_data_reader: ByteArrayDataInput::new(),

            prefix_length: 0,
            ent_count: 0,
            next_ent: 0,

            is_last_in_floor: false,
            is_leaf_block: false,
            all_equal: false,

            last_sub_fp: 0,
            next_floor_label: 0,
            num_follow_floor_blocks: 0,

            meta_data_upto: 0,

            bytes: vec![0u8; 32],
            bytes_reader: ByteArrayDataInput::new(),
            start_byte_pos: 0,
            suffix_length: 0,
            sub_code: 0,
            compression_alg: CompressionAlgorithm::NoCompression,
        })
    }
    pub(crate) fn set_floor_data(&mut self, output_accumulator: &OutputAccumulator) -> Result<()> {
        output_accumulator.set_floor_data(&mut self.floor_data_reader);
        debug_assert!(self.floor_data_reader.get_position() <= i32::MAX as usize);
        self.rewind_pos = self.floor_data_reader.get_position() as i32;
        self.num_follow_floor_blocks = self.floor_data_reader.read_vint()?;
        self.next_floor_label = self.floor_data_reader.read_byte()? as i32;
        Ok(())
    }
    pub(crate) fn get_term_block_ord(&mut self) -> i32 {
        if self.is_leaf_block {
            self.next_ent
        } else {
            self.state.get_block_term_state().term_block_ord
        }
    }
    pub(crate) fn load_next_floor_block<I, P>(
        frame_idx: usize,
        ste: &mut SegmentTermsEnum<I, P>,
    ) -> Result<bool>
    where
        I: IndexInput,
        P: PostingsReaderBase<TermState = BlockTermStateEnum>,
    {
        let frame = if frame_idx == ste.static_frame_idx {
            &mut ste.static_frame
        } else {
            &mut ste.stack[frame_idx]
        };
        debug_assert!(
            frame.arc.is_none() || frame.is_floor,
            "arc= {:?} isFloor={}",
            frame.arc,
            frame.is_floor
        );

        frame.fp = frame.fp_end;
        frame.next_ent = -1;

        Self::load_block(frame_idx, ste)
    }
    pub(crate) fn prefetch_block<I, P>(
        frame_idx: usize,
        ste: &mut SegmentTermsEnum<I, P>,
    ) -> Result<()>
    where
        I: IndexInput,
        P: PostingsReaderBase<TermState = BlockTermStateEnum>,
    {
        let (next_ent, fp) = {
            let frame = if frame_idx == ste.static_frame_idx {
                &ste.static_frame
            } else {
                &ste.stack[frame_idx]
            };
            (frame.next_ent, frame.fp)
        };
        if next_ent != -1 {
            // Already loaded
            return Ok(());
        }

        // Clone the IndexInput lazily, so that consumers
        // that just pull a TermsEnum to
        // seekExact(TermState) don't pay this cost:
        ste.init_index_input()?;

        // TODO: Could we know the number of bytes to prefetch?
        ste.input.as_mut().unwrap().prefetch(fp, 1)?;
        Ok(())
    }
    /* Does initial decode of next block of terms; this
    doesn't actually decode the docFreq, totalTermFreq,
    postings details (frq/prx offset, etc.) metadata;
    it just loads them as byte[] blobs which are then
    decoded on-demand if the metadata is ever requested
    for any term in this block.  This enables terms-only
    intensive consumes (eg certain MTQs, respelling) to
    not pay the price of decoding metadata they won't
    use.  */
    pub(crate) fn load_block<I, P>(
        frame_index: usize,
        ste: &mut SegmentTermsEnum<I, P>,
    ) -> Result<bool>
    where
        I: IndexInput,
        P: PostingsReaderBase<TermState = BlockTermStateEnum>,
    {
        // Clone the IndexInput lazily, so that consumers
        // that just pull a TermsEnum to
        // seekExact(TermState) don't pay this cost:
        ste.init_index_input()?;
        let frame = if frame_index == ste.static_frame_idx {
            &mut ste.static_frame
        } else {
            &mut ste.stack[frame_index]
        };

        if frame.next_ent != -1 {
            return Ok(frame.is_leaf_block); // already loaded
        }

        let input = ste.input.as_mut().unwrap();

        input.seek(frame.fp)?;
        let code = input.read_vint()?;
        frame.ent_count = ((code as u32) >> 1) as i32;
        debug_assert!(frame.ent_count > 0);
        frame.is_last_in_floor = (code & 1) != 0;

        debug_assert!(
            frame.arc.is_none() || frame.is_last_in_floor || frame.is_floor,
            "fp={} arc={:?} is_floor={} is_last_in_floor={}",
            frame.fp,
            frame.arc,
            frame.is_floor,
            frame.is_last_in_floor
        );
        // TODO: if suffixes were stored in random-access
        // array structure, then we could do binary search
        // instead of linear scan to find target term; eg
        // we could have simple array of offsets
        let start_suffix_fp = input.get_file_pointer();
        // term suffixes:
        let code_l = input.read_vlong()?;
        frame.is_leaf_block = (code_l & 0x04) != 0;
        let num_suffix_bytes = ((code_l as u64) >> 3) as i32;

        if frame.suffix_bytes.len() < num_suffix_bytes as usize {
            let new_len = ArrayUtil::oversize(num_suffix_bytes as usize, 1);
            frame.suffix_bytes = vec![0u8; new_len];
        }

        let alg_code = (code_l & 0x03) as u8;
        frame.compression_alg = CompressionAlgorithm::by_code(alg_code)?;

        frame
            .compression_alg
            .read(input, &mut frame.suffix_bytes, num_suffix_bytes)?;
        frame.suffixes_reader.reset_with_range(
            std::mem::take(&mut frame.suffix_bytes),
            0,
            num_suffix_bytes as usize,
        );

        let num_suffix_length_bytes = input.read_vint()?;
        debug_assert!(num_suffix_length_bytes >= 0);
        let mut num_suffix_length_bytes = num_suffix_length_bytes as usize;
        frame.all_equal = (num_suffix_length_bytes & 0x01) != 0;
        num_suffix_length_bytes >>= 1;

        if frame.suffix_length_bytes.len() < num_suffix_length_bytes {
            let new_len = ArrayUtil::oversize(num_suffix_length_bytes, 1);
            frame.suffix_length_bytes = vec![0u8; new_len];
        }

        if frame.all_equal {
            let fill_byte = input.read_byte()?;
            for i in 0..num_suffix_length_bytes {
                frame.suffix_length_bytes[i] = fill_byte;
            }
        } else {
            input.read_bytes(
                &mut frame.suffix_length_bytes,
                0,
                num_suffix_length_bytes as i32,
            )?;
        }

        frame.suffix_lengths_reader.reset_with_range(
            std::mem::take(&mut frame.suffix_length_bytes),
            0,
            num_suffix_length_bytes,
        );
        frame.total_suffix_bytes = input.get_file_pointer() - start_suffix_fp;

        // stats
        let mut num_bytes = input.read_vint()?;
        debug_assert!(num_bytes >= 0);
        if frame.stat_bytes.len() < num_bytes as usize {
            let new_len = ArrayUtil::oversize(num_bytes as usize, 1);
            frame.stat_bytes = vec![0u8; new_len];
        }
        input.read_bytes(&mut frame.stat_bytes, 0, num_bytes)?;
        frame.stats_reader.reset_with_range(
            std::mem::take(&mut frame.stat_bytes),
            0,
            num_bytes as usize,
        );
        frame.stats_singleton_run_length = 0;
        frame.meta_data_upto = 0;

        frame.state.get_block_term_state().term_block_ord = 0;
        frame.next_ent = 0;
        frame.last_sub_fp = -1;
        // TODO: we could skip this if !hasTerms; but
        // that's rare so won't help much
        // metadata
        num_bytes = input.read_vint()?;
        if frame.bytes.len() < num_bytes as usize {
            let new_len = ArrayUtil::oversize(num_bytes as usize, 1);
            frame.bytes = vec![0u8; new_len];
        }
        input.read_bytes(&mut frame.bytes, 0, num_bytes)?;
        frame.bytes_reader.reset_with_range(
            std::mem::take(&mut frame.bytes),
            0,
            num_bytes as usize,
        );

        frame.fp_end = input.get_file_pointer();

        Ok(frame.is_leaf_block)
    }
    pub(crate) fn rewind(&mut self) -> Result<()> {
        // Force reload
        self.fp = self.fp_orig;
        self.next_ent = -1;
        self.has_terms = self.has_terms_orig;

        if self.is_floor {
            self.floor_data_reader
                .set_position(self.rewind_pos as usize);
            self.num_follow_floor_blocks = self.floor_data_reader.read_vint()?;
            debug_assert!(self.num_follow_floor_blocks > 0);
            self.next_floor_label = self.floor_data_reader.read_byte()? as i32;
        }

        Ok(())
    }
    pub fn next<I, P>(frame_idx: usize, ste: &mut SegmentTermsEnum<I, P>) -> Result<bool>
    where
        I: IndexInput,
        P: PostingsReaderBase<TermState = BlockTermStateEnum>,
    {
        let frame = if frame_idx == ste.static_frame_idx {
            &mut ste.static_frame
        } else {
            &mut ste.stack[frame_idx]
        };
        if frame.is_leaf_block {
            Self::next_leaf(frame_idx, ste)?;
            Ok(false)
        } else {
            Self::next_non_leaf(frame_idx, ste)
        }
    }
    pub fn next_leaf<I, P>(frame_idx: usize, ste: &mut SegmentTermsEnum<I, P>) -> Result<()>
    where
        I: IndexInput,
        P: PostingsReaderBase<TermState = BlockTermStateEnum>,
    {
        // TODO: 可以判断下是不是static 就可以避免这里的判断
        let frame = if frame_idx == ste.static_frame_idx {
            &mut ste.static_frame
        } else {
            &mut ste.stack[frame_idx]
        };
        debug_assert!(
            frame.next_ent != -1 && frame.next_ent < frame.ent_count,
            "next_ent={} ent_count={} fp={}",
            frame.next_ent,
            frame.ent_count,
            frame.fp
        );

        frame.next_ent += 1;
        frame.suffix_length = frame.suffix_lengths_reader.read_vint()?;
        debug_assert!(frame.suffixes_reader.get_position() <= i32::MAX as usize);
        frame.start_byte_pos = frame.suffixes_reader.get_position() as i32;

        let term_len = frame.prefix_length + frame.suffix_length;
        ste.term.set_length(term_len as usize);
        let len = ste.term.length();
        ste.term.grow(len);

        frame.suffixes_reader.read_bytes(
            ste.term.get_bytes_mut_ref().bytes.as_mut(),
            frame.prefix_length,
            frame.suffix_length,
        )?;

        ste.term_exists = true;
        Ok(())
    }
    pub(crate) fn next_non_leaf<I, P>(
        frame_idx: usize,
        ste: &mut SegmentTermsEnum<I, P>,
    ) -> Result<bool>
    where
        I: IndexInput,
        P: PostingsReaderBase<TermState = BlockTermStateEnum>,
    {
        loop {
            let v = {
                let frame = if frame_idx == ste.static_frame_idx {
                    &mut ste.static_frame
                } else {
                    &mut ste.stack[frame_idx]
                };
                frame.next_ent == frame.ent_count
            };

            if v {
                debug_assert!({
                    let frame = if frame_idx == ste.static_frame_idx {
                        &mut ste.static_frame
                    } else {
                        &mut ste.stack[frame_idx]
                    };
                    frame.arc.is_none() || (frame.is_floor && !frame.is_last_in_floor)
                });

                let is_leaf_block = Self::load_next_floor_block(frame_idx, ste)?;

                if is_leaf_block {
                    Self::next_leaf(frame_idx, ste)?;
                    return Ok(false);
                } else {
                    continue;
                }
            }
            let frame = if frame_idx == ste.static_frame_idx {
                &mut ste.static_frame
            } else {
                &mut ste.stack[frame_idx]
            };
            debug_assert!(
                frame.next_ent != -1 && frame.next_ent < frame.ent_count,
                "next_ent={} ent_count={} fp={}",
                frame.next_ent,
                frame.ent_count,
                frame.fp
            );

            frame.next_ent += 1;

            let code = frame.suffix_lengths_reader.read_vint()?;
            frame.suffix_length = ((code as u32) >> 1) as i32;
            debug_assert!(frame.suffixes_reader.get_position() <= i32::MAX as usize);
            frame.start_byte_pos = frame.suffixes_reader.get_position() as i32;

            let term_len = frame.prefix_length + frame.suffix_length;
            ste.term.set_length(term_len as usize);
            let len = ste.term.length();
            ste.term.grow(len);

            frame.suffixes_reader.read_bytes(
                ste.term.get_bytes_mut_ref().bytes.as_mut(),
                frame.prefix_length,
                frame.suffix_length,
            )?;

            return if (code & 1) == 0 {
                // Normal term
                ste.term_exists = true;
                frame.sub_code = 0;
                frame.state.get_block_term_state().term_block_ord += 1;
                Ok(false)
            } else {
                // A sub-block; make sub-FP absolute:
                ste.term_exists = false;
                frame.sub_code = frame.suffix_lengths_reader.read_vlong()?;
                frame.last_sub_fp = frame.fp - frame.sub_code;
                Ok(true)
            };
        }
    }
    pub fn scan_to_floor_frame<I, P>(
        frame_idx: usize,
        ste: &mut SegmentTermsEnum<I, P>,
    ) -> Result<()>
    where
        I: IndexInput,
        P: PostingsReaderBase<TermState = BlockTermStateEnum>,
    {
        Self::scan_to_floor_frame_with_target(frame_idx, &BytesRef::new(), ste, false)
    }
    pub fn scan_to_floor_frame_with_target<I, P>(
        frame_idx: usize,
        target: &BytesRef<Vec<u8>>,
        ste: &mut SegmentTermsEnum<I, P>,
        use_target: bool,
    ) -> Result<()>
    where
        I: IndexInput,
        P: PostingsReaderBase<TermState = BlockTermStateEnum>,
    {
        let frame = if frame_idx == ste.static_frame_idx {
            &mut ste.static_frame
        } else {
            &mut ste.stack[frame_idx]
        };
        let target = if use_target {
            target
        } else {
            ste.term.get_bytes_ref()
        };
        if !frame.is_floor || target.length <= frame.prefix_length as usize {
            return Ok(());
        }

        let target_label = target.bytes[target.offset + frame.prefix_length as usize] as i32;

        if target_label < frame.next_floor_label {
            return Ok(());
        }

        debug_assert!(frame.num_follow_floor_blocks != 0);

        let mut new_fp;

        loop {
            let code = frame.floor_data_reader.read_vlong()?;
            new_fp = frame.fp_orig + ((code as u64) >> 1) as i64;
            frame.has_terms = (code & 1) != 0;

            frame.is_last_in_floor = frame.num_follow_floor_blocks == 0;
            frame.num_follow_floor_blocks -= 1;

            if frame.is_last_in_floor {
                frame.next_floor_label = 256;
                break;
            } else {
                frame.next_floor_label = frame.floor_data_reader.read_byte()? as i32;
                if target_label < frame.next_floor_label {
                    break;
                }
            }
        }

        if new_fp != frame.fp {
            frame.next_ent = -1;
            frame.fp = new_fp;
        }

        Ok(())
    }
    pub fn decode_meta_data<I, P>(frame_idx: usize, ste: &mut SegmentTermsEnum<I, P>) -> Result<()>
    where
        I: IndexInput,
        P: PostingsReaderBase<TermState = BlockTermStateEnum>,
    {
        let frame = if frame_idx == ste.static_frame_idx {
            &mut ste.static_frame
        } else {
            &mut ste.stack[frame_idx]
        };
        let limit = frame.get_term_block_ord();
        let mut absolute = frame.meta_data_upto == 0;
        debug_assert!(limit > 0);

        while frame.meta_data_upto < limit {
            let state = frame.state.get_block_term_state();

            if frame.stats_singleton_run_length > 0 {
                state.doc_freq = 1;
                state.total_term_freq = 1;
                frame.stats_singleton_run_length -= 1;
            } else {
                let token = frame.stats_reader.read_vint()?;
                if (token & 1) == 1 {
                    state.doc_freq = 1;
                    state.total_term_freq = 1;
                    frame.stats_singleton_run_length = (token as u32 >> 1) as i32;
                } else {
                    state.doc_freq = (token as u32 >> 1) as i32;
                    if *ste.fr.field_info.get_index_options() == IndexOptions::Docs {
                        state.total_term_freq = state.doc_freq as i64;
                    } else {
                        state.total_term_freq =
                            state.doc_freq as i64 + frame.stats_reader.read_vlong()?;
                    }
                }
            }

            ste.fr.parent.postings_reader.decode_term(
                &mut frame.bytes_reader,
                &ste.fr.field_info,
                &mut frame.state,
                absolute,
            )?;

            frame.meta_data_upto += 1;
            absolute = false;
        }

        frame.state.get_block_term_state().term_block_ord = frame.meta_data_upto;

        Ok(())
    }
    /// Used only in debug assertions: does target prefix match the current
    /// term?
    fn prefix_matches<I, P>(
        frame_idx: usize,
        target: &BytesRef<Vec<u8>>,
        ste: &SegmentTermsEnum<I, P>,
    ) -> bool
    where
        I: IndexInput,
        P: PostingsReaderBase<TermState = BlockTermStateEnum>,
    {
        let frame = if frame_idx == ste.static_frame_idx {
            &ste.static_frame
        } else {
            &ste.stack[frame_idx]
        };
        for byte_pos in 0..frame.prefix_length as usize {
            if target.bytes[target.offset + byte_pos] != ste.term.byte_at(byte_pos) {
                return false;
            }
        }
        true
    }
    // Scans to sub-block that has this target fp; only
    // called by next(); NOTE: does not set
    // startBytePos/suffix as a side effect
    pub fn scan_to_sub_block<I, P>(
        frame_idx: usize,
        sub_fp: i64,
        ste: &mut SegmentTermsEnum<I, P>,
    ) -> Result<()>
    where
        I: IndexInput,
        P: PostingsReaderBase<TermState = BlockTermStateEnum>,
    {
        let frame = if frame_idx == ste.static_frame_idx {
            &mut ste.static_frame
        } else {
            &mut ste.stack[frame_idx]
        };
        debug_assert!(!frame.is_leaf_block);
        if frame.last_sub_fp == sub_fp {
            return Ok(());
        }

        debug_assert!(sub_fp < frame.fp, "fp={} sub_fp={}", frame.fp, sub_fp);
        let target_sub_code = frame.fp - sub_fp;

        loop {
            debug_assert!(frame.next_ent < frame.ent_count);
            frame.next_ent += 1;

            let code = frame.suffix_lengths_reader.read_vint()?;
            frame
                .suffixes_reader
                .skip_bytes((code as u64 >> 1) as i64)?;

            if (code & 1) != 0 {
                let sub_code = frame.suffix_lengths_reader.read_vlong()?;
                if target_sub_code == sub_code {
                    frame.last_sub_fp = sub_fp;
                    return Ok(());
                }
            } else {
                frame.state.get_block_term_state().term_block_ord += 1;
            }
        }
    }
    /// Scan to a specific target term within the block. May update
    /// suffix/startBytePos.
    pub(crate) fn scan_to_term<I, P>(
        frame_idx: usize,
        target: &BytesRef<Vec<u8>>,
        exact_only: bool,
        ste: &mut SegmentTermsEnum<I, P>,
    ) -> Result<SeekStatus>
    where
        I: IndexInput,
        P: PostingsReaderBase<TermState = BlockTermStateEnum>,
    {
        let frame = if frame_idx == ste.static_frame_idx {
            &mut ste.static_frame
        } else {
            &mut ste.stack[frame_idx]
        };
        if frame.is_leaf_block {
            if frame.all_equal {
                Self::binary_search_term_leaf(frame_idx, target, exact_only, ste)
            } else {
                Self::scan_to_term_leaf(frame_idx, target, exact_only, ste)
            }
        } else {
            Self::scan_to_term_non_leaf(frame_idx, target, exact_only, ste)
        }
    }
    // Target's prefix matches this block's prefix; we
    // scan the entries to check if the suffix matches.
    pub fn scan_to_term_leaf<I, P>(
        frame_idx: usize,
        target: &BytesRef<Vec<u8>>,
        exact_only: bool,
        ste: &mut SegmentTermsEnum<I, P>,
    ) -> Result<SeekStatus>
    where
        I: IndexInput,
        P: PostingsReaderBase<TermState = BlockTermStateEnum>,
    {
        {
            let frame = if frame_idx == ste.static_frame_idx {
                &mut ste.static_frame
            } else {
                &mut ste.stack[frame_idx]
            };
            debug_assert!(frame.next_ent != -1);
            ste.term_exists = true;
            frame.sub_code = 0;

            if frame.next_ent == frame.ent_count {
                if exact_only {
                    Self::fill_term(frame_idx, ste);
                }
                return Ok(SeekStatus::End);
            }
        }

        debug_assert!(Self::prefix_matches(frame_idx, target, ste));

        loop {
            let frame = if frame_idx == ste.static_frame_idx {
                &mut ste.static_frame
            } else {
                &mut ste.stack[frame_idx]
            };
            frame.next_ent += 1;
            frame.suffix_length = frame.suffix_lengths_reader.read_vint()?;
            debug_assert!(frame.suffixes_reader.get_position() <= i32::MAX as usize);
            frame.start_byte_pos = frame.suffixes_reader.get_position() as i32;
            frame
                .suffixes_reader
                .skip_bytes(frame.suffix_length as i64)?;

            let suffix_start = frame.start_byte_pos as usize;
            let suffix_end = suffix_start + frame.suffix_length as usize;

            let cmp = frame.suffix_bytes[suffix_start..suffix_end]
                .cmp(
                    &target.bytes[target.offset + frame.prefix_length as usize
                        ..target.offset + target.length],
                )
                .to_int();

            if cmp < 0 {
                // Current entry is still before the target;
                // keep scanning
            } else if cmp > 0 {
                // Done!  Current entry is after target --
                // return NOT_FOUND:
                Self::fill_term(frame_idx, ste);
                return Ok(SeekStatus::NotFound);
            } else {
                // Exact match!

                // This cannot be a sub-block because we
                // would have followed the index to this
                // sub-block from the start:
                Self::fill_term(frame_idx, ste);
                return Ok(SeekStatus::Found);
            }
            if frame.next_ent < frame.ent_count {
                break;
            }
        }
        // It is possible (and OK) that terms index pointed us
        // at this block, but, we scanned the entire block and
        // did not find the term to position to.  This happens
        // when the target is after the last term in the block
        // (but, before the next term in the index).  EG
        // target could be foozzz, and terms index pointed us
        // to the foo* block, but the last term in this block
        // was fooz (and, eg, first term in the next block will
        // bee fop).
        if exact_only {
            Self::fill_term(frame_idx, ste);
        }
        // TODO: not consistent that in the
        // not-exact case we don't next() into the next
        // frame here
        Ok(SeekStatus::End)
    }

    // Target's prefix matches this block's prefix;
    // And all suffixes have the same length in this block,
    // we binary search the entries to check if the suffix matches.
    pub fn binary_search_term_leaf<I, P>(
        frame_idx: usize,
        target: &BytesRef<Vec<u8>>,
        exact_only: bool,
        ste: &mut SegmentTermsEnum<I, P>,
    ) -> Result<SeekStatus>
    where
        I: IndexInput,
        P: PostingsReaderBase<TermState = BlockTermStateEnum>,
    {
        {
            let frame = if frame_idx == ste.static_frame_idx {
                &mut ste.static_frame
            } else {
                &mut ste.stack[frame_idx]
            };
            debug_assert!(frame.next_ent != -1);
            ste.term_exists = true;
            frame.sub_code = 0;

            if frame.next_ent == frame.ent_count {
                if exact_only {
                    Self::fill_term(frame_idx, ste);
                }
                return Ok(SeekStatus::End);
            }
        }

        debug_assert!(Self::prefix_matches(frame_idx, target, ste));
        let frame = if frame_idx == ste.static_frame_idx {
            &mut ste.static_frame
        } else {
            &mut ste.stack[frame_idx]
        };

        frame.suffix_length = frame.suffix_lengths_reader.read_vint()?;

        let mut start = frame.next_ent;
        let mut end = frame.ent_count - 1;
        let mut cmp = 0;

        while start <= end {
            let mid = ((start + end) as u32 >> 1) as i32;
            frame.next_ent = mid + 1;
            frame.start_byte_pos = mid * frame.suffix_length;

            let suffix_start = frame.start_byte_pos as usize;
            let suffix_end = suffix_start + frame.suffix_length as usize;

            cmp = frame.suffix_bytes[suffix_start..suffix_end]
                .cmp(
                    &target.bytes[target.offset + frame.prefix_length as usize
                        ..target.offset + target.length],
                )
                .to_int();

            if cmp < 0 {
                start = mid + 1;
            } else if cmp > 0 {
                end = mid - 1;
            } else {
                // match
                frame
                    .suffixes_reader
                    .set_position((frame.start_byte_pos + frame.suffix_length) as usize);
                Self::fill_term(frame_idx, ste);
                return Ok(SeekStatus::Found);
            }
        }
        // It is possible (and OK) that terms index pointed us
        // at this block, but, we searched the entire block and
        // did not find the term to position to.  This happens
        // when the target is after the last term in the block
        // (but, before the next term in the index).  EG
        // target could be foozzz, and terms index pointed us
        // to the foo* block, but the last term in this block
        // was fooz (and, eg, first term in the next block will
        // bee fop).
        let seek_status;

        if end < frame.ent_count - 1 {
            seek_status = SeekStatus::NotFound;
            if cmp < 0 {
                frame.start_byte_pos += frame.suffix_length;
                frame.next_ent += 1;
            }
            frame
                .suffixes_reader
                .set_position((frame.start_byte_pos + frame.suffix_length) as usize);
            Self::fill_term(frame_idx, ste);
        } else {
            seek_status = SeekStatus::End;
            frame
                .suffixes_reader
                .set_position((frame.start_byte_pos + frame.suffix_length) as usize);
            if exact_only {
                Self::fill_term(frame_idx, ste);
            }
        }

        Ok(seek_status)
    }
    // Target's prefix matches this block's prefix; we
    // scan the entries to check if the suffix matches.
    pub fn scan_to_term_non_leaf<I, P>(
        frame_idx: usize,
        target: &BytesRef<Vec<u8>>,
        exact_only: bool,
        ste: &mut SegmentTermsEnum<I, P>,
    ) -> Result<SeekStatus>
    where
        I: IndexInput,
        P: PostingsReaderBase<TermState = BlockTermStateEnum>,
    {
        debug_assert!({
            let frame = if frame_idx == ste.static_frame_idx {
                &mut ste.static_frame
            } else {
                &mut ste.stack[frame_idx]
            };
            frame.next_ent != -1
        });

        let v = {
            let frame = if frame_idx == ste.static_frame_idx {
                &mut ste.static_frame
            } else {
                &mut ste.stack[frame_idx]
            };
            frame.next_ent == frame.ent_count
        };
        if v {
            if exact_only {
                Self::fill_term(frame_idx, ste);
                let frame = if frame_idx == ste.static_frame_idx {
                    &mut ste.static_frame
                } else {
                    &mut ste.stack[frame_idx]
                };
                ste.term_exists = frame.sub_code == 0;
            }
            return Ok(SeekStatus::End);
        }
        debug_assert!(Self::prefix_matches(frame_idx, target, ste));
        while {
            let frame = if frame_idx == ste.static_frame_idx {
                &mut ste.static_frame
            } else {
                &mut ste.stack[frame_idx]
            };
            frame.next_ent < frame.ent_count
        } {
            let cmp = {
                let frame = if frame_idx == ste.static_frame_idx {
                    &mut ste.static_frame
                } else {
                    &mut ste.stack[frame_idx]
                };
                frame.next_ent += 1;
                let code = frame.suffix_lengths_reader.read_vint()?;
                frame.suffix_length = (code as u32 >> 1) as i32;
                debug_assert!(frame.suffixes_reader.get_position() <= i32::MAX as usize);
                frame.start_byte_pos = frame.suffixes_reader.get_position() as i32;
                frame
                    .suffixes_reader
                    .skip_bytes(frame.suffix_length as i64)?;

                let exists = {
                    ste.term_exists = (code & 1) == 0;
                    ste.term_exists
                };
                if exists {
                    frame.state.get_block_term_state().term_block_ord += 1;
                    frame.sub_code = 0;
                } else {
                    frame.sub_code = frame.suffix_lengths_reader.read_vlong()?;
                    frame.last_sub_fp = frame.fp - frame.sub_code;
                }

                let suffix_start = frame.start_byte_pos as usize;
                let suffix_end = suffix_start + frame.suffix_length as usize;

                frame.suffix_bytes[suffix_start..suffix_end]
                    .cmp(
                        &target.bytes[target.offset + frame.prefix_length as usize
                            ..target.offset + target.length],
                    )
                    .to_int()
            };

            if cmp < 0 {
                // Current entry is still before the target;
                // keep scanning
            } else if cmp > 0 {
                Self::fill_term(frame_idx, ste);
                if !exact_only && !ste.term_exists {
                    // TODO this
                    // We are on a sub-block, and caller wants
                    // us to position to the next term after
                    // the target, so we must recurse into the
                    // sub-frame(s):
                    let last_sub_fp = {
                        let current_frame = if ste.current_frame_idx == ste.static_frame_idx {
                            &mut ste.static_frame
                        } else {
                            &mut ste.stack[ste.current_frame_idx]
                        };
                        current_frame.last_sub_fp
                    };
                    let frame = if frame_idx == ste.static_frame_idx {
                        &mut ste.static_frame
                    } else {
                        &mut ste.stack[frame_idx]
                    };
                    let prefix_len = frame.prefix_length + frame.suffix_length;
                    let mut current_frame_idx = ste.push_frame(None, last_sub_fp, prefix_len)?;
                    ste.current_frame_idx = current_frame_idx;

                    Self::load_block(current_frame_idx, ste)?;
                    while Self::next(current_frame_idx, ste)? {
                        let last_sub_fp = {
                            let current_frame = if ste.current_frame_idx == ste.static_frame_idx {
                                &mut ste.static_frame
                            } else {
                                &mut ste.stack[ste.current_frame_idx]
                            };
                            current_frame.last_sub_fp
                        };

                        let next_prefix = ste.term.length();
                        current_frame_idx =
                            ste.push_frame(None, last_sub_fp, next_prefix as i32)?;
                        Self::load_block(current_frame_idx, ste)?;
                    }
                }

                return Ok(SeekStatus::NotFound);
            } else {
                debug_assert!(ste.term_exists);
                Self::fill_term(frame_idx, ste);
                return Ok(SeekStatus::Found);
            }
        }
        // It is possible (and OK) that terms index pointed us
        // at this block, but, we scanned the entire block and
        // did not find the term to position to.  This happens
        // when the target is after the last term in the block
        // (but, before the next term in the index).  EG
        // target could be foozzz, and terms index pointed us
        // to the foo* block, but the last term in this block
        // was fooz (and, eg, first term in the next block will
        // bee fop).
        if exact_only {
            Self::fill_term(frame_idx, ste);
        }

        Ok(SeekStatus::End)
    }
    pub(crate) fn fill_term<I, P>(frame_idx: usize, ste: &mut SegmentTermsEnum<I, P>)
    where
        I: IndexInput,
        P: PostingsReaderBase<TermState = BlockTermStateEnum>,
    {
        let frame = if frame_idx == ste.static_frame_idx {
            &mut ste.static_frame
        } else {
            &mut ste.stack[frame_idx]
        };
        let term_length = frame.prefix_length + frame.suffix_length;
        ste.term.set_length(term_length as usize);
        ste.term.grow(term_length as usize);

        let dest: &mut [u8] = ste.term.get_bytes_mut_ref().bytes.as_mut();
        let src = &frame.suffix_bytes;
        let start = frame.start_byte_pos as usize;
        let len = start + frame.suffix_length as usize;
        dest.copy_from(&src[start..start + len], frame.prefix_length as usize);
    }
}
