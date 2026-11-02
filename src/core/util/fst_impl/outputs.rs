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
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::ints_ref::IntsRef;
use crate::core::util::{HashCode, OutputIdentity};
use std::borrow::Cow;
use std::fmt::Display;
use std::hash::Hash;
use std::sync::Arc;

/// Output algebra for building and traversing an FST.
/// `None` is the empty output; `V` describes a payload, without an empty singleton.
/// Callers represent every empty output as `None`. `Some` must contain a nonempty
/// payload; the FST does not normalize empty payloads supplied inside `Some`.
/// An accepted key with an empty output is distinct from a missing key.
pub trait Outputs: Display + Clone + Default {
  type V: OutputsBound;

  /// Returns the shared output prefix, or `None` when the prefix is empty.
  fn common(&self, output1: &Option<Self::V>, output2: &Option<Self::V>) -> Option<Self::V>;

  /// Removes `inc` from `output`, returning an owned payload or `None`.
  fn subtract(&self, output: &Option<Self::V>, inc: &Option<Self::V>) -> Option<Self::V>;

  /// Updates a disposable output without changing bytes shared with other values.
  fn subtract_assign(&self, output: &mut Option<Self::V>, inc: &Option<Self::V>) {
    if inc.is_none() {
      return;
    }
    *output = self.subtract(output, inc);
  }

  /// Combines two outputs, borrowing an existing nonempty payload when possible.
  fn add<'a>(
    &self,
    prefix: &'a Option<Self::V>,
    output: &'a Option<Self::V>,
  ) -> Option<Cow<'a, Self::V>>;

  /// `output` is a disposable slot. Its value may be moved into `prefix`.
  fn add_assign(&self, prefix: &mut Option<Self::V>, output: &mut Option<Self::V>) {
    let take_output = match self.add(prefix, output) {
      None => {
        *prefix = None;
        false
      },
      Some(Cow::Borrowed(value))
        if prefix
          .as_ref()
          .is_some_and(|prefix| std::ptr::eq(value, prefix)) =>
      {
        false
      },
      Some(Cow::Borrowed(value))
        if output
          .as_ref()
          .is_some_and(|output| std::ptr::eq(value, output)) =>
      {
        true
      },
      Some(Cow::Borrowed(value)) => {
        *prefix = Some(value.clone());
        false
      },
      Some(Cow::Owned(value)) => {
        *prefix = Some(value);
        false
      },
    };
    if take_output {
      std::mem::swap(prefix, output);
    }
  }

  /// Encodes an output, including the existing wire encoding for an empty value.
  fn write<DO>(&self, output: &Option<Self::V>, out: &mut DO) -> Result<()>
  where
    DO: DataOutput;

  fn write_final_output<DO>(&self, output: &Option<Self::V>, out: &mut DO) -> Result<()>
  where
    DO: DataOutput,
  {
    self.write(output, out)
  }

  /// Decodes a payload, or `None` for an encoded empty output. Errors remain `Err`.
  fn read<DI>(&self, input: &mut DI) -> Result<Option<Self::V>>
  where
    DI: DataInput;

  fn skip_output<DI>(&self, input: &mut DI) -> Result<()>
  where
    DI: DataInput,
  {
    let _ = self.read(input)?;
    Ok(())
  }

  fn read_final_output<DI>(&self, input: &mut DI) -> Result<Option<Self::V>>
  where
    DI: DataInput,
  {
    self.read(input)
  }

  fn skip_final_output<DI>(&self, input: &mut DI) -> Result<()>
  where
    DI: DataInput,
  {
    self.skip_output(input)
  }

  fn output_to_string(&self, output: &Option<Self::V>) -> String;

  fn merge(&self, _first: &Option<Self::V>, _second: &Option<Self::V>) -> Result<Option<Self::V>> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn ram_bytes_used(&self, output: &Option<Self::V>) -> i64;
}

/// Payload operations. Stored empty outputs use `None`.
pub trait OutputsBound: Clone + PartialEq + Default + HashCode + Hash {
  /// Hash of the empty value for the existing node-deduplication scheme,
  /// computed without constructing an empty payload owner.
  const EMPTY_HASH: fn() -> i32 = || 0;
  fn assign_from(&mut self, output: &Self);
}

impl OutputsBound for i64 {
  fn assign_from(&mut self, output: &Self) {
    *self = *output;
  }
}

impl OutputsBound for Arc<i64> {
  #[inline]
  fn assign_from(&mut self, output: &Self) {
    if !self.is_same_reference(output) {
      *self = output.clone();
    }
  }
}

impl OutputsBound for BytesRef<Arc<Vec<u8>>> {
  const EMPTY_HASH: fn() -> i32 = || {
    crate::core::util::StringHelper::murmurhash3_x86_32(
      &BytesRef::<Vec<u8>>::new(),
      *crate::core::util::GOOD_FAST_HASH_SEED,
    )
  };
  #[inline]
  fn assign_from(&mut self, output: &Self) {
    if !self.is_same_reference(output) {
      if Arc::ptr_eq(&self.bytes, &output.bytes) {
        debug_assert!(output.is_valid().is_ok());
        self.offset = output.offset;
        self.length = output.length;
      } else {
        *self = output.clone();
      }
    }
  }
}

impl OutputsBound for IntsRef<Arc<Vec<i32>>> {
  fn assign_from(&mut self, output: &Self) {
    if !self.is_same_reference(output) {
      if Arc::ptr_eq(&self.ints, &output.ints) {
        self.offset = output.offset;
        self.length = output.length;
      } else {
        *self = output.clone();
      }
    }
  }
}

impl OutputsBound for () {
  fn assign_from(&mut self, _output: &Self) {}
}

impl HashCode for () {
  fn hash_code(&self) -> i32 {
    0
  }
}

impl<T: OutputsBound> OutputsBound for Option<T> {
  fn assign_from(&mut self, output: &Self) {
    match (self.as_mut(), output.as_ref()) {
      (Some(current), Some(next)) => current.assign_from(next),
      (_, None) => *self = None,
      (None, Some(next)) => *self = Some(next.clone()),
    }
  }
}

impl<T: OutputsBound> HashCode for Option<T> {
  fn hash_code(&self) -> i32 {
    self
      .as_ref()
      .map_or_else(T::EMPTY_HASH, HashCode::hash_code)
  }
}
