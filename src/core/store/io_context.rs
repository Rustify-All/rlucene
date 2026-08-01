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
use std::sync::LazyLock;

use crate::core::store::ReadAdvice;
use crate::core::store::flush_info::FlushInfo;
use crate::core::store::merge_info::MergeInfo;
use crate::core::util::error::lucene_error::{LuceneError, Result};

/// A default context for normal reads/writes. Use
/// `with_read_advice` to specify
/// another [`ReadAdvice`].
///
/// # Note
/// It will use [`ReadAdvice::Random`] by default, unless set by the system
/// property `defaultReadAdvice`.
pub static IO_CONTEXT_DEFAULT: LazyLock<IOContext> =
  LazyLock::new(|| IOContext::default_io_context().unwrap());
/// A default context for reads with [`ReadAdvice::Sequential`].
///
/// # Note
/// This context should only be used when the read operations will be performed
/// in the same thread as the thread that opens the underlying storage.
pub static IO_CONTEXT_READ_ONCE: LazyLock<IOContext> =
  LazyLock::new(|| IOContext::read_once_io_context().unwrap());
/// `IOContext` holds additional details on the merge/search context. An
/// `IOContext` object can never be passed as a `None` parameter to either
/// [`Directory::open_input`](crate::core::store::directory::Directory::open_input) or
/// [`Directory::create_output`](crate::core::store::directory::Directory::create_output).
///
///
/// # Arguments
/// * `context` - An object of an enumerator `Context` type.
/// * `merge_info` - Must be provided when `context == MERGE`.
/// * `flush_info` - Must be provided when `context == FLUSH`.
/// * `read_advice` - Advice regarding the read access pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IOContext {
  pub(crate) context: Context,
  read_advice: ReadAdvice,
  pub merge_info: Option<MergeInfo>,
  pub flush_info: Option<FlushInfo>,
}

impl IOContext {
  pub fn new(
    context: Option<Context>,
    read_advice: Option<ReadAdvice>,
    merge_info: Option<MergeInfo>,
    flush_info: Option<FlushInfo>,
  ) -> Result<IOContext> {
    let context = context
      .ok_or_else(|| LuceneError::illegal_argument("context must not be None".to_string()))?;
    let read_advice = read_advice
      .ok_or_else(|| LuceneError::illegal_argument("read_advice must not be None".to_string()))?;
    if matches!(context, Context::Merge) && merge_info.is_none() {
      return Err(LuceneError::illegal_argument(
        "merge_info must not be None if context is MERGE".to_string(),
      ));
    }
    if matches!(context, Context::Flush) && flush_info.is_none() {
      return Err(LuceneError::illegal_argument(
        "flush_info must not be None if context is FLUSH".to_string(),
      ));
    }
    if (matches!(context, Context::Flush) || matches!(context, Context::Merge))
      && !matches!(read_advice, ReadAdvice::Sequential)
    {
      return Err(LuceneError::illegal_argument(
        "The FLUSH and MERGE contexts must use the SEQUENTIAL read access advice".to_string(),
      ));
    }
    Ok(Self {
      context,
      read_advice,
      merge_info,
      flush_info,
    })
  }

  pub fn get_context(&self) -> &Context {
    &self.context
  }

  pub fn get_read_advice(&self) -> &ReadAdvice {
    &self.read_advice
  }

  pub fn get_merge_info(&self) -> &Option<MergeInfo> {
    &self.merge_info
  }

  pub fn get_flush_info(&self) -> &Option<FlushInfo> {
    &self.flush_info
  }

  pub fn with_read_advice(read_advice: ReadAdvice) -> Result<IOContext> {
    Self::new(Some(Context::Default), Some(read_advice), None, None)
  }

  ///  Creates an `IOContext` for flushing.
  pub fn with_flush(flush_info: FlushInfo) -> Result<IOContext> {
    Self::new(
      Some(Context::Flush),
      Some(ReadAdvice::Sequential),
      None,
      Some(flush_info),
    )
  }
  ///  Creates an `IOContext` for merging.
  pub fn with_merge(merge_info: MergeInfo) -> Result<IOContext> {
    Self::new(
      Some(Context::Merge),
      Some(ReadAdvice::Sequential),
      Some(merge_info),
      None,
    )
  }

  pub fn with_read_advice_self(&self, read_advice: ReadAdvice) -> Result<IOContext> {
    if matches!(self.context, Context::Default) {
      // TODO: maybe should statically define all types of context
      Self::with_read_advice(read_advice)
    } else {
      Ok(self.clone())
    }
  }
  pub fn default_io_context() -> Result<IOContext> {
    Self::with_read_advice(ReadAdvice::default_read_advice())
  }
  pub fn read_once_io_context() -> Result<IOContext> {
    Self::with_read_advice(ReadAdvice::Sequential)
  }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Context {
  /// Context for reads and writes that are associated with a merge.  */
  Merge,
  /// Context for writes that are associated with a segment flush.  */
  Flush,
  /// Default context can be used for reading or writing.  */
  Default,
}
