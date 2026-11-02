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
use crate::core::util::CoreHelper;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::fst_impl::outputs::Outputs;
use crate::core::util::ints_ref::IntsRef;
use crate::core::util::ram_usage_estimator::size_of_vec;
use std::borrow::Cow;
use std::fmt::{Display, Formatter};
use std::sync::{Arc, LazyLock};

pub static SINGLETON: LazyLock<IntSequenceOutputs> = LazyLock::new(IntSequenceOutputs::default);

/// FST outputs consisting of i32 sequences. `None` represents an empty output.
#[derive(Clone, Default)]
pub struct IntSequenceOutputs;

impl IntSequenceOutputs {
  pub fn get_singleton() -> &'static IntSequenceOutputs {
    &SINGLETON
  }
}

impl Outputs for IntSequenceOutputs {
  type V = IntsRef<Arc<Vec<i32>>>;

  fn common(&self, output1: &Option<Self::V>, output2: &Option<Self::V>) -> Option<Self::V> {
    let (Some(a), Some(b)) = (output1, output2) else {
      return None;
    };
    let left = &a.ints[a.offset..a.offset + a.length];
    let right = &b.ints[b.offset..b.offset + b.length];
    let n = match CoreHelper::miss_match_i32(left, right) {
      -1 => return output1.clone(),
      0 => return None,
      n => n as usize,
    };
    if n == a.length {
      output1.clone()
    } else if n == b.length {
      output2.clone()
    } else {
      Some(IntsRef::from_slice(a.ints.clone(), a.offset, n))
    }
  }

  fn subtract(&self, output: &Option<Self::V>, inc: &Option<Self::V>) -> Option<Self::V> {
    let Some(inc) = inc else {
      return output.clone();
    };
    let Some(value) = output.as_ref() else {
      debug_assert_eq!(inc.length, 0);
      return None;
    };
    if inc.length == value.length {
      return None;
    }
    debug_assert!(inc.length < value.length);
    Some(IntsRef::from_slice(
      value.ints.clone(),
      value.offset + inc.length,
      value.length - inc.length,
    ))
  }

  fn subtract_assign(&self, output: &mut Option<Self::V>, inc: &Option<Self::V>) {
    let Some(inc) = inc else {
      return;
    };
    let Some(value) = output.as_mut() else {
      debug_assert_eq!(inc.length, 0);
      return;
    };
    if inc.length == value.length {
      *output = None;
      return;
    }
    debug_assert!(inc.length < value.length);
    let offset = value.offset + inc.length;
    let length = value.length - inc.length;
    // Retain the constructor's validation before changing either field in debug builds.
    debug_assert!(matches!(
      IntsRef::from_slice(value.ints.clone(), offset, length).is_valid(),
      Ok(true)
    ));
    value.offset = offset;
    value.length = length;
  }

  fn add<'a>(
    &self,
    prefix: &'a Option<Self::V>,
    output: &'a Option<Self::V>,
  ) -> Option<Cow<'a, Self::V>> {
    match (prefix, output) {
      (None, output) => output.as_ref().map(Cow::Borrowed),
      (prefix, None) => prefix.as_ref().map(Cow::Borrowed),
      (Some(prefix), Some(output)) => {
        debug_assert!(prefix.length > 0);
        debug_assert!(output.length > 0);
        let len = prefix.length + output.length;
        let mut buf = Vec::with_capacity(len);
        buf.extend_from_slice(&prefix.ints[prefix.offset..prefix.offset + prefix.length]);
        buf.extend_from_slice(&output.ints[output.offset..output.offset + output.length]);
        Some(Cow::Owned(IntsRef::from_slice(Arc::new(buf), 0, len)))
      },
    }
  }

  fn add_assign(&self, prefix: &mut Option<Self::V>, output: &mut Option<Self::V>) {
    if prefix.is_none() {
      std::mem::swap(prefix, output);
      return;
    }
    let (Some(value), Some(output_value)) = (prefix.as_mut(), output.as_ref()) else {
      return;
    };
    debug_assert!(value.length > 0);
    debug_assert!(output_value.length > 0);

    if value.offset == 0
      && value.length == value.ints.len()
      && let Some(ints) = Arc::get_mut(&mut value.ints)
    {
      ints.extend_from_slice(
        &output_value.ints[output_value.offset..output_value.offset + output_value.length],
      );
      value.length += output_value.length;
      return;
    }
    *prefix = self.add(prefix, output).map(Cow::into_owned);
  }

  fn write<DO>(&self, output: &Option<Self::V>, out: &mut DO) -> Result<()>
  where
    DO: DataOutput,
  {
    let Some(output) = output else {
      return out.write_vint(0);
    };
    out.write_vint(output.length as i32)?;
    for i in 0..output.length {
      out.write_vint(output.ints[output.offset + i])?;
    }
    Ok(())
  }

  fn read<DI>(&self, input: &mut DI) -> Result<Option<Self::V>>
  where
    DI: DataInput,
  {
    let len = input.read_vint()? as usize;
    if len == 0 {
      return Ok(None);
    }
    let mut buf = Vec::with_capacity(len);
    for _ in 0..len {
      buf.push(input.read_vint()?);
    }
    Ok(Some(IntsRef::from_slice(Arc::new(buf), 0, len)))
  }

  fn skip_output<DI>(&self, input: &mut DI) -> Result<()>
  where
    DI: DataInput,
  {
    let len = input.read_vint()?;
    for _ in 0..len {
      input.read_vint()?;
    }
    Ok(())
  }

  fn output_to_string(&self, output: &Option<Self::V>) -> String {
    output
      .as_ref()
      .map_or_else(|| "[]".to_owned(), ToString::to_string)
  }

  fn ram_bytes_used(&self, output: &Option<Self::V>) -> i64 {
    output.as_ref().map_or(0, |output| {
      (std::mem::size_of_val(output.ints.as_ref()) as i64)
        .saturating_add(size_of_vec(output.ints.as_ref()))
    })
  }
}

impl Display for IntSequenceOutputs {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}
