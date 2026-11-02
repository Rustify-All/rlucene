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
use crate::core::store::{DataInput, DataOutput};
use crate::core::util::error::lucene_error::Result;
use crate::core::util::fst_impl::outputs::Outputs;
use std::borrow::Cow;
use std::fmt::{Display, Formatter};
use std::sync::LazyLock;

pub static SINGLETON: LazyLock<PositiveIntOutputs> = LazyLock::new(PositiveIntOutputs::default);

/// Nonnegative integer outputs. Zero is represented by `None`.
#[derive(Clone, Default)]
pub struct PositiveIntOutputs;

impl PositiveIntOutputs {
  pub fn get_singleton() -> &'static PositiveIntOutputs {
    &SINGLETON
  }

  #[cfg(debug_assertions)]
  fn valid(&self, output: &Option<i64>) -> bool {
    debug_assert!(output.is_none_or(|value| value > 0), "output={output:?}");
    true
  }
}

impl Outputs for PositiveIntOutputs {
  type V = i64;

  fn common(&self, output1: &Option<i64>, output2: &Option<i64>) -> Option<i64> {
    #[cfg(debug_assertions)]
    {
      debug_assert!(self.valid(output1));
      debug_assert!(self.valid(output2));
    }
    match (output1, output2) {
      (Some(a), Some(b)) => Some((*a).min(*b)),
      _ => None,
    }
  }

  fn subtract(&self, output: &Option<i64>, inc: &Option<i64>) -> Option<i64> {
    #[cfg(debug_assertions)]
    {
      debug_assert!(self.valid(output));
      debug_assert!(self.valid(inc));
    }
    debug_assert!(output.unwrap_or(0) >= inc.unwrap_or(0));
    match (output, inc) {
      (output, None) => *output,
      (Some(a), Some(b)) if a == b => None,
      (Some(a), Some(b)) => Some(a.wrapping_sub(*b)),
      (None, Some(value)) => Some(0i64.wrapping_sub(*value)),
    }
  }

  fn add<'a>(&self, prefix: &'a Option<i64>, output: &'a Option<i64>) -> Option<Cow<'a, i64>> {
    #[cfg(debug_assertions)]
    {
      debug_assert!(self.valid(prefix));
      debug_assert!(self.valid(output));
    }
    match (prefix, output) {
      (None, output) => output.as_ref().map(Cow::Borrowed),
      (prefix, None) => prefix.as_ref().map(Cow::Borrowed),
      (Some(a), Some(b)) => {
        let sum = a.wrapping_add(*b);
        if sum == 0 {
          None
        } else {
          Some(Cow::Owned(sum))
        }
      },
    }
  }

  fn write<DO>(&self, output: &Option<i64>, out: &mut DO) -> Result<()>
  where
    DO: DataOutput,
  {
    #[cfg(debug_assertions)]
    debug_assert!(self.valid(output));
    out.write_vlong(output.unwrap_or(0))
  }

  fn read<DI>(&self, input: &mut DI) -> Result<Option<i64>>
  where
    DI: DataInput,
  {
    let value = input.read_vlong()?;
    Ok(if value == 0 { None } else { Some(value) })
  }

  fn skip_output<DI>(&self, input: &mut DI) -> Result<()>
  where
    DI: DataInput,
  {
    input.read_vlong()?;
    Ok(())
  }

  fn output_to_string(&self, output: &Option<i64>) -> String {
    output.unwrap_or(0).to_string()
  }
  fn ram_bytes_used(&self, _output: &Option<i64>) -> i64 {
    std::mem::size_of::<Option<i64>>() as i64
  }
}

impl Display for PositiveIntOutputs {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}
