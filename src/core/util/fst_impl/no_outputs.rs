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

pub static SINGLETON: LazyLock<NoOutputs> = LazyLock::new(NoOutputs::default);

/// Outputs for an automaton without payloads. Every stored output is `None`.
#[derive(Clone, Default)]
pub struct NoOutputs;

impl NoOutputs {
  pub fn get_singleton() -> &'static NoOutputs {
    &SINGLETON
  }
}

impl Display for NoOutputs {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

impl Outputs for NoOutputs {
  type V = ();

  fn common(&self, a: &Option<()>, b: &Option<()>) -> Option<()> {
    debug_assert!(a.is_none() && b.is_none());
    None
  }
  fn subtract(&self, output: &Option<()>, inc: &Option<()>) -> Option<()> {
    debug_assert!(output.is_none() && inc.is_none());
    None
  }
  fn add<'a>(&self, prefix: &'a Option<()>, output: &'a Option<()>) -> Option<Cow<'a, ()>> {
    debug_assert!(prefix.is_none() && output.is_none());
    None
  }
  fn write<DO>(&self, _output: &Option<()>, _out: &mut DO) -> Result<()>
  where
    DO: DataOutput,
  {
    Ok(())
  }
  fn read<DI>(&self, _input: &mut DI) -> Result<Option<()>>
  where
    DI: DataInput,
  {
    Ok(None)
  }
  fn output_to_string(&self, _output: &Option<()>) -> String {
    String::new()
  }
  fn merge(&self, first: &Option<()>, second: &Option<()>) -> Result<Option<()>> {
    debug_assert!(first.is_none() && second.is_none());
    Ok(None)
  }
  fn ram_bytes_used(&self, _output: &Option<()>) -> i64 {
    0
  }
}
