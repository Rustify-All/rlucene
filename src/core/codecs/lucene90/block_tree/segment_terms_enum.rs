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
use std::borrow::Cow;
use std::fmt::{Display, Formatter};

use crate::core::codecs::block_term_state::BlockTermStateEnum;
use crate::core::codecs::block_tree::lucene90_block_tree_terms_reader::{
    NO_OUTPUT, OUTPUT_FLAG_HAS_TERMS, OUTPUT_FLAG_IS_FLOOR, OUTPUT_FLAGS_NUM_BITS,
};
use crate::core::codecs::lucene90::block_tree::field_reader::FieldReader;
use crate::core::codecs::lucene90::block_tree::segment_terms_enum_frame::SegmentTermsEnumFrame;
use crate::core::codecs::postings_reader_base::PostingsReaderBase;
use crate::core::index::base_terms_enum::BaseTermsEnum;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::{PrepareSeekStatus, SeekStatus, TermsEnum};
use crate::core::index::{BytesRef, BytesRefBuilder};
use crate::core::store::{ByteArrayDataInput, DataInput, IndexInput};
use crate::core::util::ToInt;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::dummy::dummy_attribute_source::DummyAttributeSource;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fst_impl::fst::Arc;
use crate::core::util::fst_impl::reverse_random_access_reader::ReverseRandomAccessReader;

/// Iterates through terms in this field.
pub struct SegmentTermsEnum<I, P>
where
    I: IndexInput,
    P: PostingsReaderBase,
{
    // Lazy init: input stream
    pub(crate) input: Option<I>,
    pub(crate) stack: Vec<SegmentTermsEnumFrame>,
    pub(crate) static_frame: SegmentTermsEnumFrame,
    // TODO: 由于不确定使用static_frame_idx还是stack
    // 代码中存在很多根据current_frame_idx的重复代码需要优化
    pub(crate) current_frame_idx: usize,
    pub(crate) static_frame_idx: usize,
    pub(crate) term_exists: bool,
    pub(crate) fr: FieldReader<I, P>,
    target_before_current_length: i32,
    output_accumulator: OutputAccumulator,
    valid_index_prefix: i32,
    eof: bool,
    pub(crate) term: BytesRefBuilder<Vec<u8>>,
    fst_reader: Option<ReverseRandomAccessReader<I::RandomAccessSlice>>,
    arcs: Vec<Arc<BytesRef<std::sync::Arc<Vec<u8>>>>>,
    prepare_seek_status: PrepareSeekStatus,
}

impl<I, P> SegmentTermsEnum<I, P>
where
    I: IndexInput,
    P: PostingsReaderBase,
{
    pub fn new(fr: FieldReader<I, P>) -> Result<BaseTermsEnum<Self>> {
        // Construct SegmentTerms first
        let fst_reader = match &fr.index {
            Some(index) => Some(index.get_bytes_reader()?),
            None => None,
        };

        let v = Arc::default();
        let mut arcs = vec![v; 1];
        {
            if fr.index.is_some() {
                fr.index.as_ref().unwrap().get_first_arc(&mut arcs[0]);
                debug_assert!(arcs[0].is_final())
            }
        }
        // Create static_frame
        let static_frame = SegmentTermsEnumFrame::new(-1, &fr)?;

        // Build Frame
        let stack = Vec::new();
        let stack_len = stack.len();
        let sub = Self {
            input: None,
            stack,
            static_frame,
            current_frame_idx: stack_len + 1,
            // The value of static_frame_idx is always stack.len() + 1.
            static_frame_idx: stack_len + 1,
            term_exists: false,
            fr,
            target_before_current_length: 0,
            output_accumulator: OutputAccumulator::new(),
            valid_index_prefix: 0,
            eof: false,
            term: BytesRefBuilder::new(),
            fst_reader,
            arcs,
            prepare_seek_status: PrepareSeekStatus::NotFound,
        };
        Ok(sub.into())
    }
    pub(crate) fn init_index_input(&mut self) -> Result<()> {
        if self.input.is_none() {
            self.input = Some(self.fr.parent.terms_in.try_clone()?);
        }
        Ok(())
    }
    fn get_frame(&mut self, ord: usize) -> Result<()> {
        if ord >= self.stack.len() {
            let new_len = ArrayUtil::oversize(ord + 1, 0);

            for i in self.stack.len()..new_len {
                let frame = SegmentTermsEnumFrame::new(i as i32, &self.fr)?;
                self.stack.push(frame);
            }
            if self.current_frame_idx == self.static_frame_idx {
                self.current_frame_idx = self.stack.len() + 1;
            }
            // The value of static_frame_idx is always stack.len() + 1.
            self.static_frame_idx = self.stack.len() + 1;
        }

        debug_assert_eq!(self.stack[ord].ord, ord as i32, "Frame ord mismatch");
        Ok(())
    }
    pub(crate) fn get_arc(&mut self, ord: usize) -> usize {
        if ord >= self.arcs.len() {
            let new_len = ArrayUtil::oversize(ord + 1, 0);
            for _ in self.arcs.len()..new_len {
                self.arcs.push(Arc::default())
            }
        }
        ord
    }
    pub(crate) fn push_frame_with_data(
        &mut self,
        arc: Option<usize>,
        frame_data: BytesRef<std::sync::Arc<Vec<u8>>>,
        length: i32,
    ) -> Result<usize> {
        self.output_accumulator.reset();
        self.output_accumulator.push(frame_data);
        self.push_frame_with_length(arc, length)
    }
    pub(crate) fn push_frame_with_length(
        &mut self,
        arc: Option<usize>,
        length: i32,
    ) -> Result<usize> {
        self.output_accumulator.prepare_read();
        let code = self.fr.read_vlong_output(&mut self.output_accumulator)?;
        let fp_seek = ((code as u64) >> OUTPUT_FLAGS_NUM_BITS) as i64;
        let current_ord = if self.current_frame_idx == self.static_frame_idx {
            -1
        } else {
            self.stack[self.current_frame_idx].ord
        };
        let ord = (current_ord + 1) as usize;
        self.get_frame(ord)?;
        let f = &mut self.stack[ord];
        f.has_terms = (code & OUTPUT_FLAG_HAS_TERMS as i64) != 0;
        f.has_terms_orig = f.has_terms;
        f.is_floor = (code & OUTPUT_FLAG_IS_FLOOR as i64) != 0;

        if f.is_floor {
            f.set_floor_data(&self.output_accumulator)?;
        }
        self.push_frame(arc, fp_seek, length)?;
        Ok(ord)
    }
    pub(crate) fn push_frame(&mut self, arc: Option<usize>, fp: i64, length: i32) -> Result<usize> {
        let current_frame = if self.current_frame_idx == self.static_frame_idx {
            &mut self.static_frame
        } else {
            &mut self.stack[self.current_frame_idx]
        };
        let ord = (current_frame.ord + 1) as usize;
        self.get_frame(ord)?;
        let f = &mut self.stack[ord];
        f.arc = arc;

        if f.fp_orig == fp && f.next_ent != -1 {
            if f.ord > self.target_before_current_length {
                f.rewind()?;
            }
            debug_assert_eq!(length, f.prefix_length);
        } else {
            f.next_ent = -1;
            f.prefix_length = length;
            f.state.get_block_term_state().term_block_ord = 0;
            f.fp_orig = fp;
            f.fp = fp;
            f.last_sub_fp = -1;
        }

        Ok(ord)
    }

    fn clear_eof(&mut self) -> bool {
        self.eof = false;
        true
    }
    fn set_eof(&mut self) -> bool {
        self.eof = true;
        true
    }
    pub fn prepare_seek_exact(
        &mut self,
        target: &BytesRef<Vec<u8>>,
        prefetch: bool,
    ) -> Result<Option<()>> {
        if self.fr.index.is_none() {
            return Err(LuceneError::illegal_state("terms index was not loaded"));
        }
        if self.fr.size()? > 0 {
            let mut iter = self.fr.iterator()?;
            let left = target
                .cmp(self.fr.get_min(&mut iter)?.as_ref().unwrap())
                .to_int();
            let right = target
                .cmp(self.fr.get_max(&mut iter)?.as_ref().unwrap())
                .to_int();
            if left < 0 || right > 0 {
                self.prepare_seek_status = PrepareSeekStatus::NotFound;
                return Ok(None);
            }
        }
        self.term.grow(1 + target.length);
        debug_assert!(self.clear_eof());
        let mut target_upto;
        let target_before_current_length = {
            let current_frame = if self.current_frame_idx == self.static_frame_idx {
                &mut self.static_frame
            } else {
                &mut self.stack[self.current_frame_idx]
            };
            current_frame.ord
        };
        self.target_before_current_length = target_before_current_length;
        self.output_accumulator.reset();

        // -1 means equal to staticFrame
        let mut arc_index;
        if self.current_frame_idx != self.static_frame_idx {
            let mut arc;
            // We are already seek'd; find the common
            // prefix of new seek term vs current term and
            // re-use the corresponding seek state.  For
            // example, if app first seeks to foobar, then
            // seeks to foobaz, we can re-use the seek state
            // for the first 5 bytes.
            arc_index = 0;
            arc = &mut self.arcs[arc_index];
            debug_assert!(arc.is_final());
            self.output_accumulator.push(arc.output());

            target_upto = 0;
            let mut last_frame: usize = 0;
            debug_assert!(self.valid_index_prefix <= self.term.length() as i32);

            let target_limit = std::cmp::min(target.length, self.valid_index_prefix as usize);

            let mut cmp = 0;

            while target_upto < target_limit {
                let term_byte = self.term.byte_at(target_upto) as i32;
                let target_byte = target.bytes[target.offset + target_upto] as i32;
                cmp = term_byte - target_byte;
                if cmp != 0 {
                    break;
                }
                arc_index = 1 + target_upto;
                arc = &mut self.arcs[arc_index];
                debug_assert_eq!(
                    arc.label(),
                    target.bytes[target.offset + target_upto] as i32
                );
                self.output_accumulator.push(arc.output());

                if arc.is_final() {
                    last_frame = (1 + self.stack[last_frame].ord + 1) as usize;
                }

                target_upto += 1;
            }

            if cmp == 0 {
                // Second compare the rest of the term, but
                // don't save arc/output/frame; we only do this
                // to find out if the target term is before,
                // equal or after the current term
                let a = &self.term.bytes_ref.bytes[target_upto..self.term.length()];
                let b = &target.bytes[target.offset + target_upto..target.offset + target.length];
                cmp = a.cmp(b).to_int();
            }

            if cmp < 0 {
                // Common case: target term is after current
                // term, ie, app is seeking multiple terms
                // in sorted order
                // if (DEBUG) {
                //   System.out.println("  target is after current (shares prefixLen=" +
                // targetUpto + "); frame.ord=" + lastFrame.ord);
                // }
                self.current_frame_idx = last_frame;
            } else if cmp > 0 {
                // Uncommon case: target term
                // is before current term; this means we can
                // keep the currentFrame, but we must rewind it
                // (so we scan from the start)
                self.target_before_current_length = self.stack[last_frame].ord;
                self.current_frame_idx = last_frame;
                self.stack[last_frame].rewind()?;
            } else {
                debug_assert_eq!(self.term.length(), target.length);
                if self.term_exists {
                    self.prepare_seek_status = PrepareSeekStatus::Found;
                    return Ok(Some(()));
                }
            }
        } else {
            let next_final_output = {
                arc_index = 0;
                let arc = &mut self.arcs[0];
                self.target_before_current_length = -1;

                self.fr.index.as_ref().unwrap().get_first_arc(arc);
                debug_assert!(arc.is_final());

                self.output_accumulator.push(arc.output());

                self.current_frame_idx = self.static_frame_idx;

                target_upto = 0;
                let next_final_output = arc.next_final_output();
                self.output_accumulator.push(next_final_output.clone());
                next_final_output
            };
            self.current_frame_idx = self.push_frame_with_length(Some(0), 0)?;
            self.output_accumulator.pop(&next_final_output);
        }
        // We are done sharing the common prefix with the incoming target and where we
        // are currently seek'd; now continue walking the index:
        while target_upto < target.length {
            let target_label = target.bytes[target.offset + target_upto] as i32;

            let next_arc_idx = self.get_arc(1 + target_upto);
            let v = {
                let fr_index = self.fr.index.as_ref().unwrap();
                let reader = self.fst_reader.as_mut().unwrap();

                fr_index.find_target_arc(
                    target_label,
                    // clone here is acceptable
                    &self.arcs[arc_index].clone(),
                    &mut self.arcs[next_arc_idx],
                    reader,
                )?
            };

            if v.is_none() {
                // index exhausted
                {
                    let current_frame = if self.current_frame_idx == self.static_frame_idx {
                        &mut self.static_frame
                    } else {
                        &mut self.stack[self.current_frame_idx]
                    };
                    self.valid_index_prefix = current_frame.prefix_length;
                }
                SegmentTermsEnumFrame::scan_to_floor_frame_with_target(
                    self.current_frame_idx,
                    target,
                    self,
                    true,
                )?;
                let current_frame = if self.current_frame_idx == self.static_frame_idx {
                    &mut self.static_frame
                } else {
                    &mut self.stack[self.current_frame_idx]
                };
                if !current_frame.has_terms {
                    self.term_exists = false;
                    self.term.set_byte_at(target_upto, target_label as u8);
                    self.term.set_length(target_upto + 1);
                    self.prepare_seek_status = PrepareSeekStatus::NotFound;
                    return Ok(None);
                }

                if prefetch {
                    SegmentTermsEnumFrame::prefetch_block(self.current_frame_idx, self)?;
                }
                self.prepare_seek_status = PrepareSeekStatus::Pending;
                return Ok(Some(()));
            } else {
                arc_index = next_arc_idx;

                let arc = &mut self.arcs[next_arc_idx];
                self.term.set_byte_at(target_upto, target_label as u8);
                self.output_accumulator.push(arc.output());
                target_upto += 1;
                if arc.is_final() {
                    self.output_accumulator.push(arc.next_final_output());
                    let v = arc.next_final_output();
                    self.current_frame_idx =
                        self.push_frame_with_length(Some(next_arc_idx), target_upto as i32)?;
                    self.output_accumulator.pop(&v);
                }
            }
        }
        {
            let current_frame = if self.current_frame_idx == self.static_frame_idx {
                &mut self.static_frame
            } else {
                &mut self.stack[self.current_frame_idx]
            };
            self.valid_index_prefix = current_frame.prefix_length;
        }
        SegmentTermsEnumFrame::scan_to_floor_frame_with_target(
            self.current_frame_idx,
            target,
            self,
            true,
        )?;
        let current_frame = if self.current_frame_idx == self.static_frame_idx {
            &mut self.static_frame
        } else {
            &mut self.stack[self.current_frame_idx]
        };
        if !current_frame.has_terms {
            self.term_exists = false;
            self.term.set_length(target_upto);
            self.prepare_seek_status = PrepareSeekStatus::NotFound;
            return Ok(None);
        }

        if prefetch {
            SegmentTermsEnumFrame::prefetch_block(self.current_frame_idx, self)?;
        }
        self.prepare_seek_status = PrepareSeekStatus::Pending;
        Ok(Some(()))
    }
}

impl<I, P> BytesRefIterator for SegmentTermsEnum<I, P>
where
    I: IndexInput,
    P: PostingsReaderBase,
{
    fn next(&mut self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
        let input_none = { self.input.is_none() };
        if input_none {
            let arc = if let Some(index) = self.fr.index.as_ref() {
                let arc = &mut self.arcs[0];
                index.get_first_arc(arc);
                debug_assert!(arc.is_final());
                Some(0)
            } else {
                None
            };
            self.current_frame_idx =
                self.push_frame_with_data(arc, self.fr.root_code.clone(), 0)?;
            SegmentTermsEnumFrame::load_block(self.current_frame_idx, self)?;
        }
        self.target_before_current_length = self.stack[self.current_frame_idx].ord;
        debug_assert!(!self.eof);

        if self.current_frame_idx == self.static_frame_idx {
            // If seek was previously called and the term was
            // cached, or seek(TermState) was called, usually
            // caller is just going to pull a D/&PEnum or get
            // docFreq, etc.  But, if they then call next(),
            // this method catches up all internal state so next()
            // works properly:
            let v = std::mem::take(&mut self.term.bytes_ref);
            let found = self.seek_exact(&v)?;
            self.term.bytes_ref = v;
            debug_assert!(found);
        }
        loop {
            let res = {
                let current_frame = if self.current_frame_idx == self.static_frame_idx {
                    &mut self.static_frame
                } else {
                    &mut self.stack[self.current_frame_idx]
                };
                current_frame.next_ent == current_frame.ent_count
            };
            if res {
                let is_last_in_floor = {
                    let current_frame = if self.current_frame_idx == self.static_frame_idx {
                        &mut self.static_frame
                    } else {
                        &mut self.stack[self.current_frame_idx]
                    };
                    current_frame.is_last_in_floor
                };
                if !is_last_in_floor {
                    SegmentTermsEnumFrame::load_next_floor_block(self.current_frame_idx, self)?;
                    break;
                } else {
                    let (next_ent, last_sub_fp, last_fp) = {
                        let mut current_frame = if self.current_frame_idx == self.static_frame_idx {
                            &mut self.static_frame
                        } else {
                            &mut self.stack[self.current_frame_idx]
                        };
                        if current_frame.ord == 0 {
                            self.eof = true;
                            self.term.clear();
                            self.valid_index_prefix = 0;
                            current_frame.rewind()?;
                            self.term_exists = false;
                            return Ok(None);
                        }

                        let last_fp = current_frame.fp_orig;
                        self.current_frame_idx = (current_frame.ord - 1) as usize;
                        current_frame = &mut self.stack[self.current_frame_idx];
                        (current_frame.next_ent, current_frame.last_sub_fp, last_fp)
                    };

                    if next_ent == -1 || last_sub_fp != last_fp {
                        // We popped into a frame that's not loaded
                        // yet or not scan'd to the right entry
                        SegmentTermsEnumFrame::scan_to_floor_frame(self.current_frame_idx, self)?;
                        SegmentTermsEnumFrame::load_block(self.current_frame_idx, self)?;
                        SegmentTermsEnumFrame::scan_to_sub_block(
                            self.current_frame_idx,
                            last_fp,
                            self,
                        )?;
                    }
                    let current_frame = if self.current_frame_idx == self.static_frame_idx {
                        &mut self.static_frame
                    } else {
                        &mut self.stack[self.current_frame_idx]
                    };
                    self.valid_index_prefix =
                        self.valid_index_prefix.min(current_frame.prefix_length);
                }
            } else {
                break;
            }
        }

        loop {
            let last_sub_fp = {
                let current_frame = if self.current_frame_idx == self.static_frame_idx {
                    &mut self.static_frame
                } else {
                    &mut self.stack[self.current_frame_idx]
                };
                current_frame.last_sub_fp
            };
            let has_next = SegmentTermsEnumFrame::next(self.current_frame_idx, self)?;
            if has_next {
                let length = { self.term.length() };
                self.current_frame_idx = self.push_frame(None, last_sub_fp, length as i32)?;
                SegmentTermsEnumFrame::load_block(self.current_frame_idx, self)?;
                continue;
            } else {
                let term = self.term.get_bytes_ref();
                Some(term)
            };
        }
    }
}

impl<I, P> TermsEnum for SegmentTermsEnum<I, P>
where
    I: IndexInput,
    P: PostingsReaderBase,
{
    type AttributeSource = DummyAttributeSource;

    fn attributes(&self) -> Result<Self::AttributeSource> {
        Err(LuceneError::not_implemented(""))
    }

    fn seek_exact(&mut self, target: &BytesRef<Vec<u8>>) -> Result<bool> {
        match self.prepare_seek_exact(target, false)? {
            Some(_) => self.get_prepare_seek_exact_status(target),
            None => Ok(false),
        }
    }

    fn prepare_seek_exact(&mut self, target: &BytesRef<Vec<u8>>) -> Result<Option<()>> {
        self.prepare_seek_exact(target, true)
    }
    fn get_prepare_seek_exact_status(&mut self, target: &BytesRef<Vec<u8>>) -> Result<bool> {
        match self.prepare_seek_status {
            PrepareSeekStatus::NotFound => Ok(false),
            PrepareSeekStatus::Found => Ok(true),
            PrepareSeekStatus::Pending => {
                SegmentTermsEnumFrame::load_block(self.current_frame_idx, self)?;
                let result = SegmentTermsEnumFrame::scan_to_term(
                    self.current_frame_idx,
                    target,
                    false,
                    self,
                )?;
                Ok(result == SeekStatus::Found)
            },
        }
    }

    fn seek_ceil(&mut self, target: &BytesRef<Vec<u8>>) -> Result<SeekStatus> {
        if self.fr.index.is_none() {
            return Err(LuceneError::illegal_state("terms index was not loaded"));
        }

        self.term.grow(1 + target.length);
        debug_assert!(self.clear_eof());

        let mut target_upto;

        self.target_before_current_length = self.stack[self.current_frame_idx].ord;
        self.output_accumulator.reset();
        let mut arc_index;
        if self.current_frame_idx != self.static_frame_idx {
            let mut arc;
            // We are already seek'd; find the common
            // prefix of new seek term vs current term and
            // re-use the corresponding seek state.  For
            // example, if app first seeks to foobar, then
            // seeks to foobaz, we can re-use the seek state
            // for the first 5 bytes.
            arc_index = 0;
            arc = &self.arcs[arc_index];
            debug_assert!(arc.is_final());
            let v = arc.output();
            self.output_accumulator.push(v);
            target_upto = 0;

            let mut last_frame_index: usize = 0;
            let mut last_frame = &mut self.stack[last_frame_index];
            debug_assert!(self.valid_index_prefix <= self.term.length() as i32);

            let target_limit = std::cmp::min(target.length, self.valid_index_prefix as usize);
            let mut cmp = 0;

            while target_upto < target_limit {
                let term_byte = self.term.byte_at(target_upto) as i32;
                let target_byte = target.bytes[target.offset + target_upto] as i32;
                cmp = term_byte - target_byte;
                if cmp != 0 {
                    break;
                }
                arc_index = 1 + target_upto;
                arc = &self.arcs[arc_index];
                debug_assert_eq!(
                    arc.label(),
                    target.bytes[target.offset + target_upto] as i32
                );
                self.output_accumulator.push(arc.output());

                if arc.is_final() {
                    let idx = 1 + last_frame.ord;
                    last_frame_index = idx as usize;
                    last_frame = &mut self.stack[idx as usize];
                }

                target_upto += 1;
            }

            if cmp == 0 {
                cmp = self.term.bytes_ref.bytes[target_upto..self.term.length()]
                    .cmp(&target.bytes[target.offset + target_upto..target.offset + target.length])
                    .to_int();
            }

            if cmp < 0 {
                self.current_frame_idx = last_frame_index;
            } else if cmp > 0 {
                self.target_before_current_length = 0;
                last_frame.rewind()?;
                self.current_frame_idx = last_frame_index;
            } else {
                debug_assert_eq!(self.term.length(), target.length);
                if self.term_exists {
                    return Ok(SeekStatus::Found);
                }
            }
        } else {
            self.target_before_current_length = -1;
            arc_index = 0;
            let arc = &mut self.arcs[arc_index];
            self.fr.index.as_ref().unwrap().get_first_arc(arc);

            debug_assert!(arc.is_final());

            self.output_accumulator.push(arc.output());

            self.current_frame_idx = self.static_frame_idx;

            target_upto = 0;
            self.output_accumulator.push(arc.next_final_output());
            let v = arc.next_final_output();
            self.current_frame_idx = self.push_frame_with_length(Some(0), 0)?;
            self.output_accumulator.pop(&v);
        }
        let mut next_arc_idx;
        while target_upto < target.length {
            let target_label = target.bytes[target.offset + target_upto] as i32;

            next_arc_idx = self.get_arc(1 + target_upto);
            let v = {
                let fr_index = self.fr.index.as_ref().unwrap();

                let reader = self.fst_reader.as_mut().unwrap();

                fr_index.find_target_arc(
                    target_label,
                    // clone here is acceptable
                    &self.arcs[arc_index].clone(),
                    &mut self.arcs[next_arc_idx],
                    reader,
                )?
            };

            if v.is_none() {
                let current_frame = if self.current_frame_idx == self.static_frame_idx {
                    &mut self.static_frame
                } else {
                    &mut self.stack[self.current_frame_idx]
                };
                self.valid_index_prefix = current_frame.prefix_length;
                SegmentTermsEnumFrame::scan_to_floor_frame_with_target(
                    self.current_frame_idx,
                    target,
                    self,
                    true,
                )?;
                SegmentTermsEnumFrame::load_block(self.current_frame_idx, self)?;

                let result = SegmentTermsEnumFrame::scan_to_term(
                    self.current_frame_idx,
                    target,
                    false,
                    self,
                )?;
                return if result == SeekStatus::End {
                    {
                        self.term.copy_bytes_with_ref(target);
                        self.term_exists = false;
                    }

                    if self.next()?.is_some() {
                        Ok(SeekStatus::NotFound)
                    } else {
                        Ok(SeekStatus::End)
                    }
                } else {
                    Ok(result)
                };
            } else {
                arc_index = next_arc_idx;
                let arc = &self.arcs[arc_index];
                self.term.set_byte_at(target_upto, target_label as u8);
                self.output_accumulator.push(arc.output());

                target_upto += 1;
                if arc.is_final() {
                    self.output_accumulator.push(arc.next_final_output());
                    let v = arc.next_final_output();
                    self.current_frame_idx =
                        self.push_frame_with_length(Some(arc_index), target_upto as i32)?;
                    self.output_accumulator.pop(&v);
                }
            }
        }
        let current_frame = if self.current_frame_idx == self.static_frame_idx {
            &mut self.static_frame
        } else {
            &mut self.stack[self.current_frame_idx]
        };
        self.valid_index_prefix = current_frame.prefix_length;
        SegmentTermsEnumFrame::scan_to_floor_frame_with_target(
            self.current_frame_idx,
            target,
            self,
            true,
        )?;
        SegmentTermsEnumFrame::load_block(self.current_frame_idx, self)?;
        let result =
            SegmentTermsEnumFrame::scan_to_term(self.current_frame_idx, target, false, self)?;

        match result {
            SeekStatus::End => {
                {
                    self.term.copy_bytes_with_ref(target);
                    self.term_exists = false;
                }
                if self.next()?.is_some() {
                    Ok(SeekStatus::NotFound)
                } else {
                    Ok(SeekStatus::End)
                }
            },
            _ => Ok(result),
        }
    }

    fn seek_exact_with_ord(&mut self, _ord: i64) -> Result<()> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn seek_exact_with_state(
        &mut self,
        target: &BytesRef<Vec<u8>>,
        other_state: &Self::TermState,
    ) -> Result<()> {
        debug_assert!(self.clear_eof());
        if (target.cmp(self.term.get_bytes_mut_ref()).to_int() != 0 || !self.term_exists)
            && matches!(other_state, BlockTermStateEnum::Block(_))
        {
            self.static_frame.state = other_state.clone();
            self.current_frame_idx = self.static_frame_idx;
            self.term.copy_bytes_with_ref(target);
            self.static_frame.meta_data_upto = self.static_frame.get_term_block_ord();
            debug_assert!(self.static_frame.meta_data_upto > 0);
            self.valid_index_prefix = 0;
        }
        Ok(())
    }

    fn term(&self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
        debug_assert!(!self.eof);
        Ok(Cow::Borrowed(&self.term.bytes_ref))
    }

    fn ord(&self) -> Result<i64> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn doc_freq(&mut self) -> Result<i32> {
        debug_assert!(!self.eof);

        SegmentTermsEnumFrame::decode_meta_data(self.current_frame_idx, self)?;
        let current_frame = if self.current_frame_idx == self.static_frame_idx {
            &mut self.static_frame
        } else {
            &mut self.stack[self.current_frame_idx]
        };

        Ok(current_frame.state.get_block_term_state().doc_freq)
    }

    fn total_term_freq(&mut self) -> Result<i64> {
        debug_assert!(!self.eof);

        SegmentTermsEnumFrame::decode_meta_data(self.current_frame_idx, self)?;
        let current_frame = if self.current_frame_idx == self.static_frame_idx {
            &mut self.static_frame
        } else {
            &mut self.stack[self.current_frame_idx]
        };

        Ok(current_frame.state.get_block_term_state().total_term_freq)
    }

    type PostingsEnum = P::PostingsEnum;

    fn postings_with_flags(
        &mut self,
        reuse: Option<Self::PostingsEnum>,
        flags: i32,
    ) -> Result<Self::PostingsEnum> {
        debug_assert!(!self.eof);

        SegmentTermsEnumFrame::decode_meta_data(self.current_frame_idx, self)?;

        let field_info = &self.fr.field_info;
        let postings_reader = &self.fr.parent.postings_reader;
        let current_frame = if self.current_frame_idx == self.static_frame_idx {
            &mut self.static_frame
        } else {
            &mut self.stack[self.current_frame_idx]
        };

        let v = postings_reader
            .postings(field_info, &current_frame.state, reuse, flags)?
            .unwrap();
        Ok(v)
    }

    type ImpactsEnum = P::ImpactsEnum;

    fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
        debug_assert!(!self.eof);
        SegmentTermsEnumFrame::decode_meta_data(self.current_frame_idx, self)?;
        let current_frame = if self.current_frame_idx == self.static_frame_idx {
            &mut self.static_frame
        } else {
            &mut self.stack[self.current_frame_idx]
        };
        let field_info = &self.fr.field_info;
        let postings_reader = &self.fr.parent.postings_reader;

        let result = postings_reader.impacts(field_info, &current_frame.state, flags)?;
        Ok(result)
    }

    type TermState = BlockTermStateEnum;

    fn term_state(&mut self) -> Result<Self::TermState> {
        debug_assert!(!self.eof);

        SegmentTermsEnumFrame::decode_meta_data(self.current_frame_idx, self)?;
        let current_frame = if self.current_frame_idx == self.static_frame_idx {
            &mut self.static_frame
        } else {
            &mut self.stack[self.current_frame_idx]
        };

        let cloned_state = current_frame.state.clone();
        Ok(cloned_state)
    }
}

pub struct OutputAccumulator {
    pub(crate) outputs: Vec<BytesRef<std::sync::Arc<Vec<u8>>>>,
    pub(crate) current: BytesRef<std::sync::Arc<Vec<u8>>>,
    pub(crate) num: usize,
    pub(crate) output_index: usize,
    pub(crate) index: usize,
}
impl OutputAccumulator {
    pub(crate) fn new() -> Self {
        Self {
            outputs: Vec::with_capacity(16),
            current: BytesRef::new(),
            num: 0,
            output_index: 0,
            index: 0,
        }
    }
    pub(crate) fn push(&mut self, output: BytesRef<std::sync::Arc<Vec<u8>>>) {
        if !NO_OUTPUT.with(|rc| BytesRef::equals(&output, rc)) {
            debug_assert!(output.length > 0);
            if self.outputs.len() == self.num {
                self.outputs.resize(self.num + 1, BytesRef::new());
            }
            self.outputs[self.num] = output;
            self.num += 1;
        }
    }

    pub(crate) fn pop(&mut self, output: &BytesRef<std::sync::Arc<Vec<u8>>>) {
        if !NO_OUTPUT.with(|rc| BytesRef::equals(output, rc)) {
            debug_assert!(self.num > 0);
            debug_assert!(&self.outputs[self.num - 1] == output);
            self.num -= 1;
        }
    }
    pub(crate) fn pop_n(&mut self, cnt: usize) {
        debug_assert!(self.num >= cnt);
        self.num -= cnt;
    }

    pub(crate) fn output_count(&self) -> usize {
        self.num
    }

    pub(crate) fn reset(&mut self) {
        self.num = 0;
    }

    pub(crate) fn prepare_read(&mut self) {
        self.index = 0;
        self.output_index = 0;
        self.current = self.outputs[0].clone();
    }
    /// Set the last arc as the source of the floorData.  
    /// This won't change the reading position of this [`OutputAccumulator`].
    pub(crate) fn set_floor_data(
        &self,
        floor_data: &mut ByteArrayDataInput<std::sync::Arc<Vec<u8>>>,
    ) {
        debug_assert!(
            self.output_index == self.num - 1,
            "floor data should be stored in last arc, got output_index={}, num={}",
            self.output_index,
            self.num
        );

        let output = self.outputs[self.output_index].clone();
        let start = output.offset + self.index;
        let length = output.length - self.index;

        floor_data.reset_with_range(output.bytes, start, length);
    }
}

impl Display for OutputAccumulator {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "OutputAccumulator")
    }
}

impl DataInput for OutputAccumulator {
    fn read_byte(&mut self) -> Result<u8> {
        if self.index >= self.current.length {
            self.output_index += 1;
            self.current = self.outputs[self.output_index].clone();
            self.index = 0;
        }
        let byte = self.current.bytes[self.current.offset + self.index];
        self.index += 1;
        Ok(byte)
    }

    fn read_bytes(&mut self, _b: &mut [u8], _offset: i32, _len: i32) -> Result<()> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn skip_bytes(&mut self, _num_bytes: i64) -> Result<()> {
        Err(LuceneError::unsupported_operation(""))
    }
}
