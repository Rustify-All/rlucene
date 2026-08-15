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
use crate::core::store::data_input::DataInput;
use crate::core::store::data_output::DataOutput;
use crate::core::store::directory::Directory;
use crate::core::store::index_output::IndexOutput;
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::io_utils::IOUtils;
use crate::test_framework::core::store::mock_directory_wrapper::MockDirectoryWrapper;
use parking_lot::Mutex;
use rand::RngExt;
use std::fmt::{Display, Formatter};
use std::io::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

static NEXT_HANDLE_ID: AtomicUsize = AtomicUsize::new(0);

struct MockIndexOutputState<O> {
  out: O,
  closed: bool,
}

pub(crate) struct MockIndexOutputHandle<O> {
  state: Arc<Mutex<MockIndexOutputState<O>>>,
  name: String,
}

impl<O> Clone for MockIndexOutputHandle<O>
where
  O: IndexOutput,
{
  fn clone(&self) -> Self {
    Self {
      state: Arc::clone(&self.state),
      name: self.name.clone(),
    }
  }
}

/// Used to create an output stream that will throw an IOException on fake disk
/// full, track max disk space actually used, and maybe throw random
/// IOExceptions.
pub(crate) struct MockIndexOutputWrapper<D>
where
  D: Directory,
{
  dir: MockDirectoryWrapper<D>,
  first: bool,
  pub(crate) name: String,

  single_byte: [u8; 1],

  handle: MockIndexOutputHandle<D::IndexOutput>,
  pub(crate) handle_id: usize,
}

impl<D> MockIndexOutputWrapper<D>
where
  D: Directory,
{
  /// Construct an empty output buffer.
  pub(crate) fn new(
    dir: MockDirectoryWrapper<D>,
    out: D::IndexOutput,
    name: impl Into<String>,
  ) -> Self {
    let name = name.into();
    Self {
      dir,
      first: true,
      name: name.clone(),
      single_byte: [0],
      handle: MockIndexOutputHandle {
        state: Arc::new(Mutex::new(MockIndexOutputState { out, closed: false })),
        name,
      },
      handle_id: NEXT_HANDLE_ID.fetch_add(1, Ordering::SeqCst),
    }
  }

  pub(crate) fn output_handle(&self) -> MockIndexOutputHandle<D::IndexOutput> {
    self.handle.clone()
  }

  pub(crate) fn force_close(
    dir: &MockDirectoryWrapper<D>,
    handle_id: usize,
    handle: &MockIndexOutputHandle<D::IndexOutput>,
  ) -> Result<()> {
    Self::close_handle(dir, handle_id, handle)
  }

  fn close_handle(
    dir: &MockDirectoryWrapper<D>,
    handle_id: usize,
    handle: &MockIndexOutputHandle<D::IndexOutput>,
  ) -> Result<()> {
    let (result, close_result) = {
      let mut state = handle.state.lock();
      if state.closed {
        state.out.close()?; // don't mask double-close bugs
        return Ok(());
      }
      state.closed = true;

      let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        dir.maybe_throw_deterministic_exception()
      }));
      let close_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| state.out.close()));
      (result, close_result)
    };

    dir.remove_index_output(handle_id, &handle.name);
    if dir.state.track_disk_usage.load(Ordering::SeqCst) {
      // Now compute actual disk usage & track the maxUsedSize
      // in the MockDirectoryWrapper:
      let size = dir.size_in_bytes()? as i64;
      if size > dir.state.max_used_size.load(Ordering::SeqCst) {
        dir.state.max_used_size.store(size, Ordering::SeqCst);
      }
    }

    match result {
      Err(payload) => std::panic::resume_unwind(payload),
      Ok(Err(error)) => match close_result {
        Ok(close_result) => IOUtils::use_or_suppress_result(Err(error), close_result),
        Err(_) => Err(error),
      },
      Ok(Ok(())) => match close_result {
        Ok(close_result) => close_result,
        Err(payload) => std::panic::resume_unwind(payload),
      },
    }
  }

  fn check_crashed(&self) -> Result<()> {
    // If crashed since we were opened, then don't write anything
    if self.dir.state.crashed.load(Ordering::SeqCst) {
      return Err(LuceneError::io_with_path(
        &self.name,
        Error::other(format!(
          "{} has crashed; cannot write to {}",
          std::any::type_name::<MockDirectoryWrapper<D>>()
            .rsplit("::")
            .next()
            .unwrap_or("MockDirectoryWrapper"),
          self.name
        )),
      ));
    }
    Ok(())
  }

  fn check_disk_full<F>(&mut self, len: usize, write_free_space: F) -> Result<()>
  where
    F: FnOnce(&mut Self, usize) -> Result<()>,
  {
    let max_size = self.dir.state.max_size.load(Ordering::SeqCst);
    let len = len as i64;
    let mut free_space = if max_size == 0 {
      0
    } else {
      max_size - self.dir.size_in_bytes()? as i64
    };
    let mut real_usage = 0;

    // Enforce disk full:
    if max_size != 0 && free_space <= len {
      // Compute the real disk free. This will greatly slow down our test but
      // makes it more accurate:
      real_usage = self.dir.size_in_bytes()? as i64;
      free_space = max_size - real_usage;
    }

    if max_size != 0 && free_space <= len {
      if free_space > 0 {
        let free_space = free_space as usize;
        real_usage += free_space as i64;
        write_free_space(self, free_space)?;
      }
      if real_usage > self.dir.state.max_used_size.load(Ordering::SeqCst) {
        self
          .dir
          .state
          .max_used_size
          .store(real_usage, Ordering::SeqCst);
      }
      let mut message = format!(
        "fake disk full at {} bytes when writing {} (file length={}",
        self.dir.size_in_bytes()?,
        self.name,
        self.handle.state.lock().out.get_file_pointer()?
      );
      if free_space > 0 {
        message.push_str(&format!("; wrote {free_space} of {len} bytes"));
      }
      message.push(')');
      if cfg!(feature = "test_log_verbose") {
        eprintln!("MDW: now throw fake disk full");
      }
      return Err(LuceneError::io_with_path(&self.name, Error::other(message)));
    }
    Ok(())
  }

  fn ensure_open(&self) -> Result<()> {
    if self.handle.state.lock().closed {
      return Err(LuceneError::already_closed(format!(
        "Already closed: {}",
        self
      )));
    }
    Ok(())
  }
}

impl<D> DataOutput for MockIndexOutputWrapper<D>
where
  D: Directory,
{
  fn write_byte(&mut self, b: u8) -> Result<()> {
    self.single_byte[0] = b;
    let single_byte = self.single_byte;
    self.write_bytes_range(&single_byte, 0, 1)
  }

  fn write_bytes_range(&mut self, b: &[u8], offset: usize, len: usize) -> Result<()> {
    self.ensure_open()?;
    self.check_crashed()?;
    self.check_disk_full(len, |this, free_space| {
      this
        .handle
        .state
        .lock()
        .out
        .write_bytes_range(b, offset, free_space)
    })?;

    if self.dir.state.random_state.lock().random_range(0..200) == 0 {
      let half = len / 2;
      self
        .handle
        .state
        .lock()
        .out
        .write_bytes_range(b, offset, half)?;
      thread::yield_now();
      self
        .handle
        .state
        .lock()
        .out
        .write_bytes_range(b, offset + half, len - half)?;
    } else {
      self
        .handle
        .state
        .lock()
        .out
        .write_bytes_range(b, offset, len)?;
    }

    self.dir.maybe_throw_deterministic_exception()?;

    if self.first {
      // Maybe throw random exception; only do this on first write to a new
      // file:
      self.first = false;
      self.dir.maybe_throw_io_exception(Some(&self.name))?;
    }
    Ok(())
  }

  fn copy_bytes<I>(&mut self, input: &mut I, num_bytes: usize) -> Result<()>
  where
    Self: Sized,
    I: DataInput + ?Sized,
  {
    self.ensure_open()?;
    self.check_crashed()?;
    self.check_disk_full(num_bytes, |this, free_space| {
      let mut buffer = vec![0u8; 16384];
      let mut left = free_space;
      while left > 0 {
        let to_copy = left.min(buffer.len());
        input.read_bytes(&mut buffer, 0, to_copy)?;
        this
          .handle
          .state
          .lock()
          .out
          .write_bytes_with_len(&buffer, to_copy)?;
        left -= to_copy;
      }
      Ok(())
    })?;

    self.handle.state.lock().out.copy_bytes(input, num_bytes)?;
    self.dir.maybe_throw_deterministic_exception()
  }
}

impl<D> Display for MockIndexOutputWrapper<D>
where
  D: Directory,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "MockIndexOutputWrapper({})",
      self.handle.state.lock().out
    )
  }
}

impl<D> Closeable for MockIndexOutputWrapper<D>
where
  D: Directory,
{
  fn close(&mut self) -> Result<()> {
    Self::close_handle(&self.dir, self.handle_id, &self.handle)
  }
}

impl<D> Drop for MockIndexOutputWrapper<D>
where
  D: Directory,
{
  fn drop(&mut self) {
    if !self.handle.state.lock().closed {
      let _ = self.close();
    }
  }
}

impl<D> IndexOutput for MockIndexOutputWrapper<D>
where
  D: Directory,
{
  fn get_file_pointer(&self) -> Result<usize> {
    self.handle.state.lock().out.get_file_pointer()
  }

  fn get_checksum(&mut self) -> Result<u64> {
    self.handle.state.lock().out.get_checksum()
  }

  fn get_name(&self) -> &str {
    &self.name
  }
}
