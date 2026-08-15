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

use crate::core::index::index_reader::Identity;
use crate::core::store::buffered_checksum_index_input::BufferedChecksumIndexInput;
use crate::core::store::data_input::DataInput;
use crate::core::store::directory::Directory;
use crate::core::store::index_input::IndexInput;
use crate::core::store::random_access_input::RandomAccessInputWrapper;
use crate::core::store::{IOContext, IndexInputEnum2, ReadAdvice};
use crate::core::util::HasIdentity;
use crate::core::util::clone::TryClone;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::group_vint_util::GroupVIntUtil;
use std::cell::Cell;
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use thread_local::ThreadLocal;

const PAGE_SHIFT: usize = 12; // 4096 bytes per page
// Assumed number of pages that are read ahead
const PAGE_READAHEAD: i64 = 4;

struct SerialIOState {
  counter: AtomicI64,
  pending_fetch: ThreadLocal<Cell<bool>>,
}

/// A [`Directory`] wrapper that counts the number of times that Lucene may wait for I/O to
/// return serially. Lower counts mean that Lucene better takes advantage of I/O parallelism.
pub struct SerialIOCountingDirectory<D> {
  in_: D,
  state: Arc<SerialIOState>,
  id: Identity,
}

impl<D> SerialIOCountingDirectory<D>
where
  D: Directory,
{
  /// Sole constructor.
  pub fn new(in_: D) -> Self {
    Self {
      in_,
      state: Arc::new(SerialIOState {
        counter: AtomicI64::new(0),
        pending_fetch: ThreadLocal::new(),
      }),
      id: Identity::new(),
    }
  }

  /// Return the number of I/O requests performed serially.
  pub fn count(&self) -> i64 {
    self.state.counter.load(Ordering::Relaxed)
  }

  /// Returns the wrapped [`Directory`].
  pub fn get_delegate_mut(&mut self) -> &mut D {
    &mut self.in_
  }
}

impl<D> Display for SerialIOCountingDirectory<D>
where
  D: Directory,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "SerialIOCountingDirectory({})", self.in_)
  }
}

impl<D> CloseableRef for SerialIOCountingDirectory<D>
where
  D: Directory,
{
  fn close(&self) -> Result<()> {
    self.in_.close()
  }
}

impl<D> HasIdentity for SerialIOCountingDirectory<D>
where
  D: Directory,
{
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl<D> Directory for SerialIOCountingDirectory<D>
where
  D: Directory,
{
  fn list_all(&self) -> Result<Vec<String>> {
    self.in_.list_all()
  }

  fn delete_file(&self, name: &str) -> Result<()> {
    self.in_.delete_file(name)
  }

  fn file_length(&self, name: &str) -> Result<usize> {
    self.in_.file_length(name)
  }

  type IndexOutput = D::IndexOutput;

  fn create_output(&self, name: &str, context: &IOContext) -> Result<Self::IndexOutput> {
    self.in_.create_output(name, context)
  }

  fn create_temp_output(
    &self,
    prefix: &str,
    suffix: &str,
    context: &IOContext,
  ) -> Result<Self::IndexOutput> {
    self.in_.create_temp_output(prefix, suffix, context)
  }

  fn sync(&self, names: &[String]) -> Result<()> {
    self.in_.sync(names)
  }

  fn sync_metadata(&self) -> Result<()> {
    self.in_.sync_metadata()
  }

  fn rename(&self, source: &str, dest: &str) -> Result<()> {
    self.in_.rename(source, dest)
  }

  type IndexInput = IndexInputEnum2<D::IndexInput, SerializedIOCountingIndexInput<D::IndexInput>>;

  fn open_input(&self, name: &str, context: &IOContext) -> Result<Self::IndexInput> {
    let input = self.in_.open_input(name, context)?;
    if *context.get_read_advice() == ReadAdvice::RandomPreload {
      // expected to be loaded in memory, only count 1 at open time
      self.state.counter.fetch_add(1, Ordering::Relaxed);
      Ok(IndexInputEnum2::A(input))
    } else {
      Ok(IndexInputEnum2::B(SerializedIOCountingIndexInput::new(
        input,
        *context.get_read_advice(),
        self.state.clone(),
      )?))
    }
  }

  fn open_checksum_input(
    &self,
    name: &str,
  ) -> Result<BufferedChecksumIndexInput<Self::IndexInput>> {
    // sequential access, count 1 for the whole file
    self.state.counter.fetch_add(1, Ordering::Relaxed);
    let context = IOContext::read_once_io_context()?;
    Ok(BufferedChecksumIndexInput::new(
      self.open_input(name, &context)?,
    ))
  }

  type Lock = D::Lock;

  fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
    self.in_.obtain_lock(name)
  }

  fn copy_from<T>(&self, from: &T, src: &str, dest: &str, context: &IOContext) -> Result<()>
  where
    T: Directory + ?Sized,
  {
    self.in_.copy_from(from, src, dest, context)
  }

  fn get_pending_deletions(&self) -> Result<HashSet<String>> {
    self.in_.get_pending_deletions()
  }

  #[cfg(debug_assertions)]
  fn is_fs_directory(&self) -> bool {
    self.in_.is_fs_directory()
  }

  fn ensure_open(&self) -> Result<()> {
    self.in_.ensure_open()
  }
}

pub struct SerializedIOCountingIndexInput<I> {
  in_: I,
  slice_offset: usize,
  slice_length: usize,
  read_advice: ReadAdvice,
  pending_pages: HashSet<i64>,
  current_page: i64,
  state: Arc<SerialIOState>,
}

impl<I> SerializedIOCountingIndexInput<I>
where
  I: IndexInput<IndexInput = I>,
{
  fn new(in_: I, read_advice: ReadAdvice, state: Arc<SerialIOState>) -> Result<Self> {
    let length = in_.length()?;
    Ok(Self::new_with_slice(in_, read_advice, 0, length, state))
  }

  fn new_with_slice(
    in_: I,
    read_advice: ReadAdvice,
    slice_offset: usize,
    slice_length: usize,
    state: Arc<SerialIOState>,
  ) -> Self {
    Self {
      in_,
      slice_offset,
      slice_length,
      read_advice,
      pending_pages: HashSet::new(),
      current_page: i64::MIN,
      state,
    }
  }

  fn on_read(&mut self, offset: usize, len: usize) {
    if len == 0 {
      return;
    }
    let first_page = ((self.slice_offset + offset) >> PAGE_SHIFT) as i64;
    let last_page = ((self.slice_offset + offset + len - 1) >> PAGE_SHIFT) as i64;

    for page in first_page..=last_page {
      let read_ahead_upto = if self.read_advice == ReadAdvice::Random {
        self.current_page
      } else {
        // Assume that the next few pages are always free to read thanks to read-ahead.
        self.current_page + PAGE_READAHEAD
      };

      if !self.pending_pages.contains(&page) && (page < self.current_page || page > read_ahead_upto)
      {
        self.state.counter.fetch_add(1, Ordering::Relaxed);
      }
      self.current_page = page;
    }
    self
      .state
      .pending_fetch
      .get_or(|| Cell::new(false))
      .set(false);
  }
}

impl<I> Display for SerializedIOCountingIndexInput<I>
where
  I: IndexInput<IndexInput = I>,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", self.in_)
  }
}

impl<I> CloseableRef for SerializedIOCountingIndexInput<I>
where
  I: IndexInput<IndexInput = I>,
{
  fn close(&self) -> Result<()> {
    self.in_.close()
  }
}

impl<I> TryClone for SerializedIOCountingIndexInput<I>
where
  I: IndexInput<IndexInput = I>,
{
  fn try_clone(&self) -> Result<Self> {
    Ok(Self {
      in_: self.in_.try_clone()?,
      slice_offset: self.slice_offset,
      slice_length: self.slice_length,
      read_advice: self.read_advice,
      pending_pages: HashSet::new(),
      current_page: i64::MIN,
      state: self.state.clone(),
    })
  }
}

impl<I> DataInput for SerializedIOCountingIndexInput<I>
where
  I: IndexInput<IndexInput = I>,
{
  fn read_byte(&mut self) -> Result<u8> {
    self.on_read(self.get_file_pointer()?, 1);
    self.in_.read_byte()
  }

  fn read_bytes(&mut self, b: &mut [u8], offset: usize, len: usize) -> Result<()> {
    self.on_read(self.get_file_pointer()?, len);
    self.in_.read_bytes(b, offset, len)
  }

  fn read_group_vint(&mut self, dst: &mut [i32], offset: usize) -> Result<()> {
    GroupVIntUtil::read_group_vint_i32(self, dst, offset)
  }

  fn skip_bytes(&mut self, num_bytes: i64) -> Result<()> {
    IndexInput::skip_bytes(self, num_bytes)
  }

  fn is_index_input(&self) -> bool {
    true
  }

  fn seek_in_data_input(&mut self, pos: usize) -> Result<()> {
    self.seek(pos)
  }

  fn get_file_pointer_in_data_input(&self) -> Result<usize> {
    self.get_file_pointer()
  }
}

impl<I> IndexInput for SerializedIOCountingIndexInput<I>
where
  I: IndexInput<IndexInput = I>,
{
  type IndexInput = Self;

  fn get_file_pointer(&self) -> Result<usize> {
    Ok(self.in_.get_file_pointer()? - self.slice_offset)
  }

  fn seek(&mut self, pos: usize) -> Result<()> {
    self.in_.seek(self.slice_offset + pos)
  }

  fn length(&self) -> Result<usize> {
    Ok(self.slice_length)
  }

  fn slice(
    &self,
    slice_description: &str,
    offset: usize,
    length: usize,
  ) -> Result<Self::IndexInput> {
    self.slice_with_read_advice(slice_description, offset, length, &self.read_advice)
  }

  fn slice_with_read_advice(
    &self,
    _slice_description: &str,
    offset: usize,
    length: usize,
    read_advice: &ReadAdvice,
  ) -> Result<Self::IndexInput> {
    if offset > self.slice_length || length > self.slice_length - offset {
      return Err(LuceneError::illegal_argument("slice is out of bounds"));
    }
    let mut clone = self.in_.try_clone()?;
    clone.seek(self.slice_offset + offset)?;
    Ok(Self {
      in_: clone,
      slice_offset: self.slice_offset + offset,
      slice_length: length,
      read_advice: *read_advice,
      pending_pages: HashSet::new(),
      current_page: i64::MIN,
      state: self.state.clone(),
    })
  }

  type RandomAccessSlice = RandomAccessInputWrapper<Self>;

  fn random_access_slice(&self, offset: usize, length: usize) -> Result<Self::RandomAccessSlice> {
    Ok(RandomAccessInputWrapper::new(self.slice(
      "randomaccess",
      offset,
      length,
    )?))
  }

  fn prefetch(&mut self, offset: usize, length: usize) -> Result<()> {
    let first_page = ((self.slice_offset + offset) >> PAGE_SHIFT) as i64;
    let last_page = ((self.slice_offset + offset + length - 1) >> PAGE_SHIFT) as i64;

    let read_ahead_upto = if self.read_advice == ReadAdvice::Random {
      self.current_page
    } else {
      // Assume that the next few pages are always free to read thanks to read-ahead.
      self.current_page + PAGE_READAHEAD
    };

    if first_page >= self.current_page && last_page <= read_ahead_upto {
      // seeking within the current (or next page if ReadAdvice::Normal) doesn't increment the
      // counter
    } else if !self.state.pending_fetch.get_or(|| Cell::new(false)).get() {
      // If multiple prefetch calls are performed without a readXXX() call in-between, count a
      // single increment as these I/O requests can be performed in parallel.
      self.state.counter.fetch_add(1, Ordering::Relaxed);
      self.pending_pages.clear();
      self
        .state
        .pending_fetch
        .get_or(|| Cell::new(false))
        .set(true);
    }

    for page in first_page..=last_page {
      self.pending_pages.insert(page);
    }
    Ok(())
  }

  fn is_loaded(&self) -> Result<Option<bool>> {
    IndexInput::is_loaded(&self.in_)
  }
}
