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
use parking_lot::RwLock;
use std::sync::LazyLock;
use std::{fmt, sync::Arc};

use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::Result;

/// Debugging API for Lucene components such as
/// [`IndexWriter`](crate::core::index::index_writer::IndexWriter)
/// and [`SegmentInfos`](crate::core::index::segment_infos::SegmentInfos).
pub trait InfoStream: Send + Sync + CloseableRef {
  /// Prints a message.
  fn message(&self, component: &str, message: &str) -> Result<()>;

  /// Returns true if messages are enabled and should be posted to [`Self::message`].
  fn is_enabled(&self, component: &str) -> bool;
}

impl<T> InfoStream for Arc<T>
where
  T: InfoStream + ?Sized,
{
  fn message(&self, component: &str, message: &str) -> Result<()> {
    self.as_ref().message(component, message)
  }

  fn is_enabled(&self, component: &str) -> bool {
    self.as_ref().is_enabled(component)
  }
}

pub(crate) static NO_OUTPUT_INFO_STREAM: LazyLock<InfoStreamMT> =
  LazyLock::new(|| Arc::new(InfoStreamEnum::NoOutput(NoOutput)));

/// A global, thread-safe reference to the default [`InfoStream`](crate::core::util::info_stream::InfoStream).
static DEFAULT_INFO_STREAM: LazyLock<RwLock<InfoStreamMT>> =
  LazyLock::new(|| RwLock::new(Arc::clone(&NO_OUTPUT_INFO_STREAM)));

/// Instance of InfoStream that does no logging at all.
#[derive(Clone, Debug, Default)]
pub struct NoOutput;

impl CloseableRef for NoOutput {
  fn close(&self) -> Result<()> {
    // Nothing to do.
    Ok(())
  }
}

impl InfoStream for NoOutput {
  fn message(&self, _component: &str, _message: &str) -> Result<()> {
    debug_assert!(
      false,
      "this method should never be called when is_enabled returns false"
    );
    Ok(())
  }

  fn is_enabled(&self, _component: &str) -> bool {
    false
  }
}

/// The default [`InfoStream`] used by newly created types.
pub fn get_default_info_stream() -> Arc<InfoStreamEnum> {
  DEFAULT_INFO_STREAM.read().clone()
}

/// Sets the default [`InfoStream`] used by newly created types.
pub fn set_default<T>(info_stream: T)
where
  T: Into<InfoStreamMT>,
{
  let info_stream = info_stream.into();
  let previous = std::mem::replace(&mut *DEFAULT_INFO_STREAM.write(), info_stream);
  // Release the old Rust owner after unlocking: a custom destructor may use InfoStream.
  drop(previous);
}
pub enum InfoStreamEnum {
  NoOutput(NoOutput),
  Custom(Box<dyn InfoStream>),
}
impl fmt::Debug for InfoStreamEnum {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      InfoStreamEnum::NoOutput(output) => f.debug_tuple("NoOutput").field(output).finish(),
      InfoStreamEnum::Custom(_) => f.write_str("CustomInfoStream"),
    }
  }
}
impl Default for InfoStreamEnum {
  fn default() -> Self {
    InfoStreamEnum::NoOutput(NoOutput)
  }
}
impl CloseableRef for InfoStreamEnum {
  fn close(&self) -> Result<()> {
    match self {
      InfoStreamEnum::NoOutput(output) => output.close(),
      InfoStreamEnum::Custom(output) => CloseableRef::close(output.as_ref()),
    }
  }
}
impl InfoStream for InfoStreamEnum {
  fn message(&self, component: &str, message: &str) -> Result<()> {
    match self {
      InfoStreamEnum::NoOutput(output) => output.message(component, message),
      InfoStreamEnum::Custom(output) => output.message(component, message),
    }
  }

  fn is_enabled(&self, component: &str) -> bool {
    match self {
      InfoStreamEnum::NoOutput(output) => output.is_enabled(component),
      InfoStreamEnum::Custom(output) => output.is_enabled(component),
    }
  }
}
pub type InfoStreamMT = Arc<InfoStreamEnum>;
