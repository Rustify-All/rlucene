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

/// Enumerates all input ([`IntsRef`]) + output pairs in an FST.
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
    let mut result_input: IntsRef<Vec<i32>> = IntsRef::with_capacity(10)?;
    result_input.offset = 1;
    let base = FSTEnum::new(fst)?;
    Ok(Self {
      result: InputOutput {
        input: result_input,
        output: None,
      },
      base,
    })
  }

  pub fn current(&self) -> &IOIntRef<O> {
    &self.result
  }

  pub fn next_value(&mut self) -> Result<Option<&IOIntRef<O>>> {
    let (base, result) = (&mut self.base, &mut self.result);
    let mut ctx = IntsRefFSTEnumContext { result };
    base.do_next(&mut ctx)?;
    self.set_result()
  }
  /// Seeks to smallest term that's &gt;= target.
  pub fn seek_ceil(&mut self, target: &IntsRef<Vec<i32>>) -> Result<Option<&IOIntRef<O>>> {
    debug_assert!(target.length <= i32::MAX as usize);
    let (base, result) = (&mut self.base, &mut self.result);
    base.target_length = target.length;
    let mut ctx = IntsRefFSTEnumContext { result };
    base.do_seek_ceil(&mut ctx, target)?;
    self.set_result()
  }

  ///  Seeks to biggest term that's &lt;= target.
  pub fn seek_floor(&mut self, target: &IntsRef<Vec<i32>>) -> Result<Option<&IOIntRef<O>>> {
    debug_assert!(target.length <= i32::MAX as usize);
    let (base, result) = (&mut self.base, &mut self.result);
    base.target_length = target.length;
    let mut ctx = IntsRefFSTEnumContext { result };
    base.do_seek_floor(&mut ctx, target)?;
    self.set_result()
  }
  /// Seeks to the exact target term and returns `None` if the term does not
  /// exist. This is faster than using [`Self::seek_floor`] or
  /// [`Self::seek_ceil`] because it short-circuits as soon as a mismatch
  /// is detected.
  pub fn seek_exact(&mut self, target: &IntsRef<Vec<i32>>) -> Result<Option<&IOIntRef<O>>> {
    debug_assert!(target.length <= i32::MAX as usize);
    let (base, result) = (&mut self.base, &mut self.result);
    base.target_length = target.length;
    let mut ctx = IntsRefFSTEnumContext { result };
    let found = base.do_seek_exact(&mut ctx, target)?;
    if found {
      debug_assert_eq!(base.upto, 1 + target.length);
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
      // The END_LABEL output is never a prefix: the next traversal overwrites
      // this slot before descending. Transfer its owner into the result.
      std::mem::swap(&mut self.result.output, &mut self.base.output[base_upto]);
      self.result.input.length = base_upto - 1;
      Ok(Some(&self.result))
    }
  }
}

struct IntsRefFSTEnumContext<'a, O: Outputs> {
  result: &'a mut IOIntRef<O>,
}

impl<'a, O, F> FSTEnumBase<O, F> for IntsRefFSTEnumContext<'a, O>
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
    if self.result.input.ints.len() <= base.upto {
      ArrayUtil::grow_with(&mut self.result.input.ints, base.upto + 1, || 0)?;
    }
    Ok(())
  }
}

type IOIntRef<O> = InputOutput<<O as Outputs>::V, IntsRef<Vec<i32>>>;
