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
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::sync::LazyLock;

use crate::core::store::{DataInput, DataOutput};
use crate::core::util::CoreHelper;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::fst_impl::outputs::Outputs;
use crate::core::util::ints_ref::IntsRef;
use crate::core::util::ram_usage_estimator::size_of_vec;

/// Global NO_OUTPUT singleton shared by all threads.
static NO_OUTPUT: LazyLock<IntsRef<Arc<Vec<i32>>>> = LazyLock::new(IntsRef::new);

pub static SINGLETON: LazyLock<IntSequenceOutputs> = LazyLock::new(|| IntSequenceOutputs {
  no_output: &NO_OUTPUT,
});

/// An FST [`Outputs`] implementation where each output is a sequence of ints.
pub struct IntSequenceOutputs {
  no_output: &'static IntsRef<Arc<Vec<i32>>>,
}

impl Clone for IntSequenceOutputs {
  fn clone(&self) -> Self {
    Self {
      no_output: self.no_output,
    }
  }
}

impl Default for IntSequenceOutputs {
  fn default() -> Self {
    Self {
      no_output: &NO_OUTPUT,
    }
  }
}

impl IntSequenceOutputs {
  pub fn get_singleton() -> &'static IntSequenceOutputs {
    &SINGLETON
  }
}

impl Outputs for IntSequenceOutputs {
  type V = IntsRef<Arc<Vec<i32>>>;

  fn common(&self, output1: &Self::V, output2: &Self::V) -> Self::V {
    let a = &output1.ints[output1.offset..output1.offset + output1.length];
    let b = &output2.ints[output2.offset..output2.offset + output2.length];

    let mismatch = match CoreHelper::miss_match_i32(a, b) {
      -1 => return output1.clone(),       // exactly equals
      0 => return self.no_output.clone(), // no common prefix
      n => n as usize,
    };

    if mismatch == output1.length {
      output1.clone()
    } else if mismatch == output2.length {
      output2.clone()
    } else {
      IntsRef::from_slice(output1.ints.clone(), output1.offset, mismatch)
    }
  }

  fn subtract<'a>(&self, output: &'a Self::V, inc: &Self::V) -> std::borrow::Cow<'a, Self::V> {
    if IntsRef::equals(inc, self.no_output) {
      return std::borrow::Cow::Borrowed(output);
    } else if inc.length == output.length {
      return std::borrow::Cow::Owned(self.no_output.clone());
    }

    debug_assert!(inc.length < output.length);

    std::borrow::Cow::Owned(IntsRef::from_slice(
      output.ints.clone(),
      output.offset + inc.length,
      output.length - inc.length,
    ))
  }

  fn subtract_assign(&self, output: &mut Self::V, inc: &Self::V) {
    if IntsRef::equals(inc, self.no_output) {
      return;
    }
    if inc.length == output.length {
      *output = self.no_output.clone();
      return;
    }
    debug_assert!(inc.length < output.length);
    let offset = output.offset + inc.length;
    let length = output.length - inc.length;
    // Retain the constructor's validation before changing either field in debug builds.
    debug_assert!(matches!(
      IntsRef::from_slice(output.ints.clone(), offset, length).is_valid(),
      Ok(true)
    ));
    output.offset = offset;
    output.length = length;
  }

  fn add<'a>(&self, prefix: &'a Self::V, output: &'a Self::V) -> std::borrow::Cow<'a, Self::V> {
    let no_output = self.no_output;
    if IntsRef::equals(prefix, no_output) {
      return std::borrow::Cow::Borrowed(output);
    } else if IntsRef::equals(output, no_output) {
      return std::borrow::Cow::Borrowed(prefix);
    }
    debug_assert!(prefix.length > 0);
    debug_assert!(output.length > 0);
    let mut buf = Vec::with_capacity(prefix.length + output.length);
    buf.extend_from_slice(&prefix.ints[prefix.offset..prefix.offset + prefix.length]);
    buf.extend_from_slice(&output.ints[output.offset..output.offset + output.length]);
    std::borrow::Cow::Owned(IntsRef::from_slice(
      Arc::new(buf),
      0,
      prefix.length + output.length,
    ))
  }

  fn add_assign(&self, prefix: &mut Self::V, output: &mut Self::V) {
    if IntsRef::equals(prefix, self.no_output) {
      std::mem::swap(prefix, output);
      return;
    }
    if IntsRef::equals(output, self.no_output) {
      return;
    }
    debug_assert!(prefix.length > 0);
    debug_assert!(output.length > 0);

    if prefix.offset == 0
      && prefix.length == prefix.ints.len()
      && let Some(ints) = Arc::get_mut(&mut prefix.ints)
    {
      ints.extend_from_slice(&output.ints[output.offset..output.offset + output.length]);
      prefix.length += output.length;
      return;
    }
    *prefix = self.add(prefix, output).into_owned();
  }

  fn write<DO>(&self, output: &Self::V, out: &mut DO) -> Result<()>
  where
    DO: DataOutput,
  {
    out.write_vint(output.length as i32)?;
    for i in 0..output.length {
      out.write_vint(output.ints[output.offset + i])?;
    }
    Ok(())
  }

  fn read<DI>(&self, input: &mut DI) -> Result<std::borrow::Cow<'_, Self::V>>
  where
    DI: DataInput,
  {
    let len = input.read_vint()? as usize;
    if len == 0 {
      Ok(std::borrow::Cow::Borrowed(self.no_output))
    } else {
      let mut buf = Vec::with_capacity(len);
      for _ in 0..len {
        buf.push(input.read_vint()?);
      }
      Ok(std::borrow::Cow::Owned(IntsRef::from_slice(
        Arc::new(buf),
        0,
        len,
      )))
    }
  }

  fn skip_output<DI>(&self, input: &mut DI) -> Result<()>
  where
    DI: DataInput,
  {
    let len = input.read_vint()?;
    if len == 0 {
      return Ok(());
    }
    for _ in 0..len {
      input.read_vint()?;
    }
    Ok(())
  }

  fn get_no_output(&self) -> &Self::V {
    self.no_output
  }

  fn output_to_string(&self, output: &Self::V) -> String {
    output.to_string()
  }

  fn ram_bytes_used(&self, output: &Self::V) -> i64 {
    (std::mem::size_of_val(output.ints.as_ref()) as i64)
      .saturating_add(size_of_vec(output.ints.as_ref()))
  }
}

impl Display for IntSequenceOutputs {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}
