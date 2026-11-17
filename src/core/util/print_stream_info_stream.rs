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
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::info_stream::{InfoStream, InfoStreamEnum};
use chrono::Utc;
use parking_lot::ReentrantMutex;
use std::any::TypeId;
use std::cell::RefCell;
use std::io::{self, Write};
use std::sync::atomic::{AtomicI32, Ordering};

static MESSAGE_ID: AtomicI32 = AtomicI32::new(0);

/// InfoStream implementation over a writable stream such as stdout.
pub struct PrintStreamInfoStream<W> {
  message_id: i32,
  // Write has no close operation. Taking the owned writer releases its resource.
  pub(crate) stream: ReentrantMutex<RefCell<Option<W>>>,
}

impl<W> PrintStreamInfoStream<W> {
  pub fn new(stream: W) -> Self {
    Self::with_message_id(stream, MESSAGE_ID.fetch_add(1, Ordering::SeqCst))
  }

  pub fn with_message_id(stream: W, message_id: i32) -> Self {
    Self {
      message_id,
      stream: ReentrantMutex::new(RefCell::new(Some(stream))),
    }
  }
}

impl<W> InfoStream for PrintStreamInfoStream<W>
where
  W: Write + Send + 'static,
{
  fn message(&self, component: &str, message: &str) -> Result<()> {
    let current_thread = std::thread::current();
    let thread_name = current_thread.name().unwrap_or("<unnamed>");
    let timestamp = self.get_timestamp();
    let stream_guard = self.stream.lock();
    let mut stream = stream_guard.borrow_mut();
    if let Some(stream) = stream.as_mut() {
      // Record I/O failures instead of propagating them from println.
      let _ = writeln!(
        stream,
        "{} {} [{}; {}]: {}",
        component, self.message_id, timestamp, thread_name, message
      );
      let _ = stream.flush();
    }
    Ok(())
  }

  fn is_enabled(&self, _component: &str) -> bool {
    true
  }
}

impl<W> CloseableRef for PrintStreamInfoStream<W>
where
  W: Write + Send + 'static,
{
  fn close(&self) -> Result<()> {
    if !self.is_system_stream() {
      let stream_guard = self.stream.lock();
      let stream = stream_guard.borrow_mut().take();
      if let Some(mut stream) = stream {
        // An I/O failure does not escape close.
        let _ = stream.flush();
      }
    }
    Ok(())
  }
}

impl<W> PrintStreamInfoStream<W> {
  pub fn is_system_stream(&self) -> bool
  where
    W: 'static,
  {
    TypeId::of::<W>() == TypeId::of::<io::Stdout>()
      || TypeId::of::<W>() == TypeId::of::<io::Stderr>()
  }

  /// Returns the current time as string for insertion into log messages.
  pub fn get_timestamp(&self) -> String {
    Utc::now().to_rfc3339()
  }
}

impl PrintStreamInfoStream<io::Stdout> {
  pub fn stdout() -> Self {
    Self::new(io::stdout())
  }
}

impl PrintStreamInfoStream<io::Stderr> {
  pub fn stderr() -> Self {
    Self::new(io::stderr())
  }
}

impl<W> From<PrintStreamInfoStream<W>> for InfoStreamEnum
where
  W: Write + Send + 'static,
{
  fn from(info_stream: PrintStreamInfoStream<W>) -> Self {
    InfoStreamEnum::Custom(Box::new(info_stream))
  }
}
