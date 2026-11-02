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
use std::fmt::Display;
use std::hash::Hash;
use std::sync::Arc;

/// Represents the outputs for an FST, providing the basic algebra required for
/// building and traversing the FST.
///
/// Reference-backed outputs must return the same `NO_OUTPUT` singleton from
/// [`get_no_output`](Outputs::get_no_output). Value outputs use their canonical
/// no-output value instead; for [`i64`] this is zero.
pub trait Outputs: Display + Clone + Default {
  type V: OutputsBound;

  /// Eg. `common("foobar", "food") -> "foo"`
  fn common(&self, output1: &Self::V, output2: &Self::V) -> Self::V;

  /// Eg. `subtract("foobar", "foo") -> "bar"`
  fn subtract<'a>(&self, output: &'a Self::V, inc: &Self::V) -> std::borrow::Cow<'a, Self::V>;

  /// Stores `subtract(output, inc)` in `output`.
  ///
  /// The old value of `output` is disposable. An implementation may update its
  /// slice metadata while retaining the backing owner, but must not change data
  /// visible through other output values.
  fn subtract_assign(&self, output: &mut Self::V, inc: &Self::V) {
    match self.subtract(output, inc) {
      std::borrow::Cow::Borrowed(next) if std::ptr::eq(next, output) => {},
      std::borrow::Cow::Borrowed(next) => *output = next.clone(),
      std::borrow::Cow::Owned(next) => *output = next,
    }
  }

  /// Eg. `add("foo", "bar") -> "foobar"`
  fn add<'a>(&self, prefix: &'a Self::V, output: &'a Self::V) -> std::borrow::Cow<'a, Self::V>;

  /// Stores `add(prefix, output)` in `prefix`.
  ///
  /// `output` is a disposable scratch slot: an implementation may move its value
  /// into `prefix` and leave the old prefix in `output`. Callers must not rely on
  /// the value of `output` after this call. Use [`Self::add`] when both inputs
  /// must remain unchanged.
  fn add_assign(&self, prefix: &mut Self::V, output: &mut Self::V) {
    let take_output = match self.add(prefix, output) {
      std::borrow::Cow::Borrowed(existing) if std::ptr::eq(existing, prefix) => false,
      std::borrow::Cow::Borrowed(existing) if std::ptr::eq(existing, output) => true,
      std::borrow::Cow::Borrowed(existing) => {
        *prefix = existing.clone();
        false
      },
      std::borrow::Cow::Owned(next) => {
        *prefix = next;
        false
      },
    };
    if take_output {
      std::mem::swap(prefix, output);
    }
  }

  /// Encode an output value into a `Write` stream.
  fn write<DO>(&self, output: &Self::V, out: &mut DO) -> Result<()>
  where
    DO: DataOutput;

  /// Encode a final node output value into a `Write` stream.
  /// By default this just calls [`Self::write`].
  fn write_final_output<DO>(&self, output: &Self::V, out: &mut DO) -> Result<()>
  where
    DO: DataOutput,
  {
    self.write(output, out)
  }

  /// Decode an output value previously written with [`Self::write`].
  fn read<DI>(&self, input: &mut DI) -> Result<std::borrow::Cow<'_, Self::V>>
  where
    DI: DataInput;

  /// Skip the output; defaults to just calling [`Self::read`] and discarding the
  /// result.
  fn skip_output<DI>(&self, input: &mut DI) -> Result<()>
  where
    DI: DataInput,
  {
    let _ = self.read(input)?;
    Ok(())
  }

  /// Decode an output value previously written with [`Self::write_final_output`].
  /// By default this just calls [`Self::read`].
  fn read_final_output<DI>(&self, input: &mut DI) -> Result<std::borrow::Cow<'_, Self::V>>
  where
    DI: DataInput,
  {
    self.read(input)
  }

  /// Skip the output previously written with [`Self::write_final_output`];
  /// defaults to just calling [`Self::read_final_output`] and discarding the
  /// result.
  fn skip_final_output<DI>(&self, input: &mut DI) -> Result<()>
  where
    DI: DataInput,
  {
    self.skip_output(input)?;
    Ok(())
  }

  /// Returns the canonical no-output value. Reference-backed outputs must use
  /// the same singleton object whenever an operation returns no output.
  /// Value outputs are recognized by [`OutputsBound::is_no_output`].
  fn get_no_output(&self) -> &Self::V;

  fn output_to_string(&self, output: &Self::V) -> String;

  fn merge(&self, _first: &Self::V, _second: &Self::V) -> Result<Self::V> {
    Err(LuceneError::unsupported_operation(""))
  }

  /// Return memory usage for the provided output.
  ///
  /// See also: [`Accountable`](crate::core::util::accountable::Accountable)
  fn ram_bytes_used(&self, output: &Self::V) -> i64;
}

/// Output values used by the FST algorithms.
///
/// Recognizing no output and updating an existing slot are separate from
/// physical reference identity, so scalar values need no shared allocation.
pub trait OutputsBound: Clone + PartialEq + Default + HashCode + Hash + Display {
  /// Whether this is the canonical no-output representation. Reference-backed
  /// values compare singleton identity; scalar values compare their value.
  fn is_no_output(&self, no_output: &Self) -> bool;

  /// Copies `output` into this slot. Reference-backed implementations may
  /// retain the slot when it already refers to exactly the same output.
  fn assign_from(&mut self, output: &Self);
}

impl OutputsBound for i64 {
  fn is_no_output(&self, no_output: &Self) -> bool {
    self == no_output
  }

  fn assign_from(&mut self, output: &Self) {
    *self = *output;
  }
}

impl OutputsBound for Arc<i64> {
  fn is_no_output(&self, no_output: &Self) -> bool {
    self.is_same_reference(no_output)
  }

  #[inline]
  fn assign_from(&mut self, output: &Self) {
    if !self.is_same_reference(output) {
      *self = output.clone();
    }
  }
}

impl OutputsBound for BytesRef<Arc<Vec<u8>>> {
  fn is_no_output(&self, no_output: &Self) -> bool {
    self.is_same_reference(no_output)
  }

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
  fn is_no_output(&self, no_output: &Self) -> bool {
    self.is_same_reference(no_output)
  }

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
