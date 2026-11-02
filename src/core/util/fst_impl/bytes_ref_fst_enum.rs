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

/// Enumerates all input ([`BytesRef`]) + output pairs in an FST.
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
    let mut result_input = BytesRef::with_capacity(10)?;
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

  pub fn current(&self) -> &IOBytesRef<O> {
    &self.result
  }

  pub fn next_value(&mut self) -> Result<Option<&IOBytesRef<O>>> {
    let (base, result) = (&mut self.base, &mut self.result);
    let mut ctx = BytesRefFSTEnumContext { result };
    base.do_next(&mut ctx)?;
    self.set_result()
  }

  pub fn seek_ceil(&mut self, target: &BytesRef<Vec<u8>>) -> Result<Option<&IOBytesRef<O>>> {
    let (base, result) = (&mut self.base, &mut self.result);
    base.target_length = target.length;
    let mut ctx = BytesRefFSTEnumContext { result };
    base.do_seek_ceil(&mut ctx, target)?;
    self.set_result()
  }

  pub fn seek_floor(&mut self, target: &BytesRef<Vec<u8>>) -> Result<Option<&IOBytesRef<O>>> {
    let (base, result) = (&mut self.base, &mut self.result);
    base.target_length = target.length;
    let mut ctx = BytesRefFSTEnumContext { result };
    base.do_seek_floor(&mut ctx, target)?;
    self.set_result()
  }

  pub fn seek_exact(&mut self, target: &BytesRef<Vec<u8>>) -> Result<Option<&IOBytesRef<O>>> {
    let (base, result) = (&mut self.base, &mut self.result);
    base.target_length = target.length;
    let mut ctx = BytesRefFSTEnumContext { result };
    let found = base.do_seek_exact(&mut ctx, target)?;
    if found {
      debug_assert_eq!(base.upto, 1 + target.length);
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
      // The END_LABEL output is never a prefix: the next traversal overwrites
      // this slot before descending. Transfer its owner into the result.
      std::mem::swap(&mut self.result.output, &mut self.base.output[base_upto]);
      self.result.input.length = base_upto - 1;
      Ok(Some(&self.result))
    }
  }
}

struct BytesRefFSTEnumContext<'a, O: Outputs> {
  result: &'a mut IOBytesRef<O>,
}

impl<'a, O, F> FSTEnumBase<O, F> for BytesRefFSTEnumContext<'a, O>
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
    if self.result.input.bytes.len() <= base.upto {
      ArrayUtil::grow_with(&mut self.result.input.bytes, base.upto + 1, || 0)?;
    }
    Ok(())
  }
}

type IOBytesRef<O> = InputOutput<<O as Outputs>::V, BytesRef<Vec<u8>>>;
