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
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::fst_impl::fst::{END_LABEL, FST};
use crate::core::util::fst_impl::fst_enum::{FSTEnum, FSTEnumBase, InputOutput};
use crate::core::util::fst_impl::fst_reader::FstReader;
use crate::core::util::fst_impl::outputs::Outputs;

/// Enumerates all input (`BytesRef`) + output pairs in an FST.
pub struct BytesRefFSTEnum<O, F>
where
    O: Outputs,
    F: FstReader,
{
    pub(crate) result: IOBytesRef<O>,
    base: FSTEnum<O, F>,
}

impl<O, F> BytesRefFSTEnum<O, F>
where
    O: Outputs,
    F: FstReader,
{
    /// `do_floor` controls the behavior of advance: if it's true,
    /// `advance` positions to the biggest term before target.
    pub fn new(fst: FST<O, F>) -> Result<Self> {
        let mut result_input = BytesRef::with_capacity(10);
        result_input.offset = 1;
        let base = FSTEnum::new(fst)?;
        Ok(Self {
            result: InputOutput {
                input: result_input,
                output: O::V::default(),
            },
            base,
        })
    }

    pub fn current(&self) -> &IOBytesRef<O> {
        &self.result
    }

    pub fn next_value(&mut self) -> Result<Option<&IOBytesRef<O>>> {
        unsafe {
            let this: *mut Self = self;
            let base_ptr = &mut (*this).base as *mut FSTEnum<O, F>;
            (*base_ptr).do_next(&mut *this)?;
        }
        self.set_result()
    }

    pub fn seek_ceil(&mut self, target: &BytesRef<Vec<u8>>) -> Result<Option<&IOBytesRef<O>>> {
        unsafe {
            let this: *mut Self = self;
            let base_ptr = &mut (*this).base as *mut FSTEnum<O, F>;
            (*base_ptr).target_length = target.length as i32;
            (*base_ptr).do_seek_ceil(&mut *this, target)?;
        }
        self.set_result()
    }

    pub fn seek_floor(&mut self, target: &BytesRef<Vec<u8>>) -> Result<Option<&IOBytesRef<O>>> {
        unsafe {
            let this: *mut Self = self;
            let base_ptr = &mut (*this).base as *mut FSTEnum<O, F>;
            (*base_ptr).target_length = target.length as i32;
            (*base_ptr).do_seek_floor(&mut *this, target)?;
        }
        self.set_result()
    }

    pub fn seek_exact(&mut self, target: &BytesRef<Vec<u8>>) -> Result<Option<&IOBytesRef<O>>> {
        let found = unsafe {
            let this: *mut Self = self;
            let base_ptr = &mut (*this).base as *mut FSTEnum<O, F>;
            (*base_ptr).target_length = target.length as i32;
            (*base_ptr).do_seek_exact(&mut *this, target)?
        };
        if found {
            debug_assert_eq!(self.base.upto, 1 + target.length);
            self.set_result()
        } else {
            Ok(None)
        }
    }

    fn set_result(&mut self) -> Result<Option<&IOBytesRef<O>>> {
        let base_upto = self.base.upto;
        if base_upto == 0 {
            Ok(None)
        } else {
            let output = self.base.output[base_upto].clone();
            self.result.input.length = base_upto - 1;
            self.result.output = output;
            Ok(Some(&self.result))
        }
    }
}
impl<O, F> FSTEnumBase<O, F> for BytesRefFSTEnum<O, F>
where
    O: Outputs,
    F: FstReader,
{
    type V = BytesRef<Vec<u8>>;

    fn get_target_label(&self, base: &FSTEnum<O, F>, target: &Self::V) -> Result<i32> {
        if base.upto - 1 == target.length {
            Ok(END_LABEL)
        } else {
            Ok(target.bytes[target.offset + base.upto - 1] as i32 & 0xFF)
        }
    }

    fn get_current_label(&self, base: &FSTEnum<O, F>) -> Result<i32> {
        Ok(self.result.input.bytes[base.upto] as i32 & 0xFF)
    }

    fn set_current_label(&mut self, label: i32, base: &FSTEnum<O, F>) -> Result<()> {
        self.result.input.bytes[base.upto] = label as u8;
        Ok(())
    }

    fn grow(&mut self, base: &FSTEnum<O, F>) -> Result<()> {
        ArrayUtil::grow_with_len(&mut self.result.input.bytes, base.upto + 1);
        Ok(())
    }
}
type IOBytesRef<O> = InputOutput<<O as Outputs>::V, BytesRef<Vec<u8>>>;
