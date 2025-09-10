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
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::fst_impl::fst::{END_LABEL, FST};
use crate::core::util::fst_impl::fst_enum::{FSTEnum, FSTEnumBase, InputOutput};
use crate::core::util::fst_impl::fst_reader::FstReader;
use crate::core::util::fst_impl::outputs::Outputs;
use crate::core::util::ints_ref::IntsRef;

/// Enumerates all input (`IntsRef`) + output pairs in an FST.
pub struct IntsRefFSTEnum<O, F>
where
    O: Outputs,
    F: FstReader,
{
    pub(crate) result: IOIntRef<O>,
    pub base: FSTEnum<O, F>,
}

impl<O, F> IntsRefFSTEnum<O, F>
where
    O: Outputs,
    F: FstReader,
{
    /// `do_floor` controls the behavior of advance: if it's true,
    /// `advance` positions to the biggest term before target.
    pub fn new(fst: FST<O, F>) -> Result<Self> {
        let mut result_input: IntsRef<Vec<i32>> = IntsRef::with_capacity(10);
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

    pub fn current(&self) -> &IOIntRef<O> {
        &self.result
    }

    pub fn next_value(&mut self) -> Result<Option<&IOIntRef<O>>> {
        unsafe {
            let this: *mut Self = self;
            let base_ptr = &mut (*this).base as *mut FSTEnum<O, F>;
            (*base_ptr).do_next(&mut *this)?;
        }
        self.set_result()
    }
    /// Seeks to smallest term that's &gt;= target.
    pub fn seek_ceil(&mut self, target: &IntsRef<Vec<i32>>) -> Result<Option<&IOIntRef<O>>> {
        debug_assert!(target.length <= i32::MAX as usize);
        unsafe {
            let this: *mut Self = self;
            let base_ptr = &mut (*this).base as *mut FSTEnum<O, F>;
            (*base_ptr).target_length = target.length as i32;
            (*base_ptr).do_seek_ceil(&mut *this, target)?;
        }
        self.set_result()
    }

    ///  Seeks to biggest term that's &lt;= target.
    pub fn seek_floor(&mut self, target: &IntsRef<Vec<i32>>) -> Result<Option<&IOIntRef<O>>> {
        debug_assert!(target.length <= i32::MAX as usize);
        unsafe {
            let this: *mut Self = self;
            let base_ptr = &mut (*this).base as *mut FSTEnum<O, F>;
            (*base_ptr).target_length = target.length as i32;
            (*base_ptr).do_seek_floor(&mut *this, target)?;
        }
        self.set_result()
    }
    /// Seeks to the exact target term and returns `None` if the term does not
    /// exist. This is faster than using [`Self::seek_floor`] or
    /// [`Self::seek_ceil`] because it short-circuits as soon as a mismatch
    /// is detected.
    pub fn seek_exact(&mut self, target: &IntsRef<Vec<i32>>) -> Result<Option<&IOIntRef<O>>> {
        debug_assert!(target.length <= i32::MAX as usize);
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

    fn set_result(&mut self) -> Result<Option<&IOIntRef<O>>> {
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

impl<O, F> FSTEnumBase<O, F> for IntsRefFSTEnum<O, F>
where
    O: Outputs,
    F: FstReader,
{
    type V = IntsRef<Vec<i32>>;

    fn get_target_label(&self, base: &FSTEnum<O, F>, target: &Self::V) -> Result<i32> {
        if base.upto - 1 == target.length {
            Ok(END_LABEL)
        } else {
            Ok(target.ints[target.offset + base.upto - 1])
        }
    }

    fn get_current_label(&self, base: &FSTEnum<O, F>) -> Result<i32> {
        Ok(self.result.input.ints[base.upto])
    }

    fn set_current_label(&mut self, label: i32, base: &FSTEnum<O, F>) -> Result<()> {
        self.result.input.ints[base.upto] = label;
        Ok(())
    }

    fn grow(&mut self, base: &FSTEnum<O, F>) -> Result<()> {
        ArrayUtil::grow_with_len(&mut self.result.input.ints, base.upto + 1);
        Ok(())
    }
}
type IOIntRef<O> = InputOutput<<O as Outputs>::V, IntsRef<Vec<i32>>>;
