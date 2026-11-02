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
use crate::core::store::{DataInput, DataOutput};
use crate::core::util::CoreHelper;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::fst_impl::outputs::Outputs;
use crate::core::util::ram_usage_estimator::size_of_vec;
use crate::core::util::{StringHelper, TryIntoInt};
use std::borrow::Cow;
use std::fmt::{Display, Formatter};
use std::sync::{Arc, LazyLock};

pub static SINGLETON: LazyLock<ByteSequenceOutputs> = LazyLock::new(ByteSequenceOutputs::default);

/// FST outputs consisting of u8 sequences. `None` represents an empty output.
#[derive(Clone, Default)]
pub struct ByteSequenceOutputs;

impl ByteSequenceOutputs {
  pub fn get_singleton() -> &'static ByteSequenceOutputs {
    &SINGLETON
  }
}

impl Outputs for ByteSequenceOutputs {
  type V = BytesRef<Arc<Vec<u8>>>;

  #[inline]
  fn common(&self, output1: &Option<Self::V>, output2: &Option<Self::V>) -> Option<Self::V> {
    let (Some(a), Some(b)) = (output1, output2) else {
      return None;
    };
    let left = &a.bytes[a.offset..a.offset + a.length];
    let right = &b.bytes[b.offset..b.offset + b.length];
    let n = match CoreHelper::miss_match_u8(left, right) {
      -1 => return output1.clone(),
      0 => return None,
      n => n as usize,
    };
    if n == a.length {
      output1.clone()
    } else if n == b.length {
      output2.clone()
    } else {
      Some(BytesRef::from_slice(a.bytes.clone(), a.offset, n))
    }
  }

  #[inline]
  fn subtract(&self, output: &Option<Self::V>, inc: &Option<Self::V>) -> Option<Self::V> {
    let Some(inc) = inc else {
      return output.clone();
    };
    let Some(value) = output.as_ref() else {
      debug_assert_eq!(inc.length, 0);
      return None;
    };
    debug_assert!(StringHelper::starts_with(
      &value.bytes,
      value.offset,
      value.length,
      &inc.bytes,
      inc.offset,
      inc.length
    ));
    if inc.length == value.length {
      return None;
    }
    debug_assert!(inc.length < value.length);
    Some(BytesRef::from_slice(
      value.bytes.clone(),
      value.offset + inc.length,
      value.length - inc.length,
    ))
  }

  #[inline]
  fn subtract_assign(&self, output: &mut Option<Self::V>, inc: &Option<Self::V>) {
    let Some(inc) = inc else {
      return;
    };
    let Some(value) = output.as_mut() else {
      debug_assert_eq!(inc.length, 0);
      return;
    };
    debug_assert!(StringHelper::starts_with(
      &value.bytes,
      value.offset,
      value.length,
      &inc.bytes,
      inc.offset,
      inc.length
    ));
    if inc.length == value.length {
      *output = None;
    } else {
      debug_assert!(inc.length < value.length);
      debug_assert!(inc.length > 0);
      value.offset += inc.length;
      value.length -= inc.length;
    }
  }

  #[inline]
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
        buf.extend_from_slice(&prefix.bytes[prefix.offset..prefix.offset + prefix.length]);
        buf.extend_from_slice(&output.bytes[output.offset..output.offset + output.length]);
        Some(Cow::Owned(BytesRef::from_slice(Arc::new(buf), 0, len)))
      },
    }
  }

  fn add_assign(&self, prefix: &mut Option<Self::V>, output: &mut Option<Self::V>) {
    if prefix.is_none() {
      std::mem::swap(prefix, output);
      return;
    }
    let Some(value) = output.as_ref() else {
      return;
    };
    if let Some(prefix) = prefix.as_mut()
      && prefix.offset == 0
      && prefix.length == prefix.bytes.len()
      && let Some(bytes) = Arc::get_mut(&mut prefix.bytes)
    {
      bytes.extend_from_slice(&value.bytes[value.offset..value.offset + value.length]);
      prefix.length += value.length;
      return;
    }
    *prefix = self.add(prefix, output).map(Cow::into_owned);
  }

  #[inline]
  fn write<DO>(&self, output: &Option<Self::V>, out: &mut DO) -> Result<()>
  where
    DO: DataOutput,
  {
    if let Some(output) = output {
      out.write_vint(output.length as i32)?;
      out.write_bytes_range(&output.bytes, output.offset, output.length)
    } else {
      out.write_vint(0)
    }
  }

  fn read<DI>(&self, input: &mut DI) -> Result<Option<Self::V>>
  where
    DI: DataInput,
  {
    let len = input.read_vint()?.try_convert()?;
    if len == 0 {
      return Ok(None);
    }
    let output = input.get_bytes(len)?.into_owned();
    Ok(Some(BytesRef::from_slice(Arc::new(output), 0, len)))
  }

  fn skip_output<DI>(&self, input: &mut DI) -> Result<()>
  where
    DI: DataInput,
  {
    let len = input.read_vint()?;
    if len != 0 {
      input.skip_bytes(len as i64)?;
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
      (std::mem::size_of_val(output.bytes.as_ref()) as i64)
        .saturating_add(size_of_vec(output.bytes.as_ref()))
    })
  }
}

impl Display for ByteSequenceOutputs {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}
