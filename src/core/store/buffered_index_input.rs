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

use crate::core::store::data_input_ext::DataInputExt;
use parking_lot::Mutex;
use std::fmt::{Display, Formatter};
use std::io::Cursor;
use std::sync::Arc;

use byteorder::{ByteOrder, LE};

use crate::core::store::index_input::IndexInput;
use crate::core::store::random_access_input::RandomAccessInput;
use crate::core::store::{BufferedIndexInputBase, Context, DataInput, IOContext};
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::group_vint_util::{GroupVIntUtil, IntReader};
use crate::core::util::{ReadableCursorExt, SliceCopyOps, TryIntoInt};
/// Base implementation struct for buffered [`IndexInput`].  */
pub struct BufferedIndexInput<T> {
  buffer_size: usize,
  resource_desc: Arc<str>,
  sub_index_input: T,
  state: Mutex<BufferedIndexInputState>,
}

// These fields describe one cache snapshot and must change together.
struct BufferedIndexInputState {
  buffer: Cursor<Vec<u8>>,
  buffer_start: usize,
  /// File pointer for sequential reads; random cache repositioning also updates it.
  pos: usize,
  /// valid data length in the buffer
  length: usize,
  bulk_scratch: Vec<u8>,
}

struct BufferedIndexInputAccess<'a, T> {
  buffer_size: usize,
  resource_desc: &'a Arc<str>,
  sub_index_input: &'a T,
  state: &'a mut BufferedIndexInputState,
}

impl<T> std::ops::Deref for BufferedIndexInputAccess<'_, T> {
  type Target = BufferedIndexInputState;

  fn deref(&self) -> &Self::Target {
    self.state
  }
}

impl<T> std::ops::DerefMut for BufferedIndexInputAccess<'_, T> {
  fn deref_mut(&mut self) -> &mut Self::Target {
    self.state
  }
}

impl<T> Display for BufferedIndexInputAccess<'_, T> {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "BufferedIndexInput({})", self.resource_desc)
  }
}

impl<T> BufferedIndexInput<T> {
  pub fn with_buffer_size(
    sub_index_input: T,
    resource_desc: &str,
    buffer_size: usize,
  ) -> Result<BufferedIndexInput<T>> {
    Self::with_description(sub_index_input, resource_desc, buffer_size)
  }

  pub(crate) fn with_description(
    sub_index_input: T,
    resource_desc: impl Into<Arc<str>>,
    buffer_size: usize,
  ) -> Result<BufferedIndexInput<T>> {
    let buffer = Cursor::new(vec![0u8; buffer_size]);
    check_buffer_size(buffer_size)?;
    Ok(BufferedIndexInput {
      buffer_size,
      resource_desc: resource_desc.into(),
      sub_index_input,
      state: Mutex::new(BufferedIndexInputState {
        buffer,
        buffer_start: 0,
        pos: 0,
        length: 0,
        bulk_scratch: Vec::new(),
      }),
    })
  }
  pub fn with_resource_desc(
    sub_index_input: T,
    resource_desc: &str,
  ) -> Result<BufferedIndexInput<T>> {
    Self::with_buffer_size(sub_index_input, resource_desc, BUFFER_SIZE)
  }

  pub fn with_io_context(
    sub_index_input: T,
    resource_desc: &str,
    context: &IOContext,
  ) -> Result<BufferedIndexInput<T>> {
    Self::with_buffer_size(sub_index_input, resource_desc, buffer_size(context))
  }

  fn access_mut(&mut self) -> BufferedIndexInputAccess<'_, T> {
    BufferedIndexInputAccess {
      buffer_size: self.buffer_size,
      resource_desc: &self.resource_desc,
      sub_index_input: &self.sub_index_input,
      state: self.state.get_mut(),
    }
  }

  fn access_state<'a>(
    &'a self,
    state: &'a mut BufferedIndexInputState,
  ) -> BufferedIndexInputAccess<'a, T> {
    BufferedIndexInputAccess {
      buffer_size: self.buffer_size,
      resource_desc: &self.resource_desc,
      sub_index_input: &self.sub_index_input,
      state,
    }
  }
}

/// Returns default buffer sizes for the given [`IOContext`].
pub fn buffer_size(io_context: &IOContext) -> usize {
  match io_context.get_context() {
    Context::Merge => MERGE_BUFFER_SIZE,
    Context::Default | Context::Flush => BUFFER_SIZE,
  }
}

fn check_buffer_size(buffer_size: usize) -> Result<()> {
  if buffer_size < MIN_BUFFER_SIZE {
    return Err(LuceneError::illegal_argument(format!(
      "bufferSize must be at least MIN_BUFFER_SIZE (got {buffer_size})"
    )));
  }
  Ok(())
}

impl<T> BufferedIndexInputAccess<'_, T>
where
  T: BufferedIndexInputBase<Slice = BufferedIndexInput<T>>,
{
  /// Refills the buffer with data from the underlying input, preserving
  /// unaligned bytes.
  ///
  /// This method handles cases where a previous read left some unaligned
  /// bytes in the buffer. It copies these unaligned bytes to the start of
  /// the buffer and refills the remaining space with new data from the
  /// underlying input.
  ///
  /// # Arguments
  /// * `remain_unaligned_bytes` - The number of unaligned bytes remaining in
  ///   the buffer from the previous read.
  /// * `start` - The starting position in the underlying input to begin
  ///   reading data.
  ///
  /// # Returns
  /// * `Ok(())` - If the buffer is successfully refilled.
  /// * `Err(LuceneError)` - If an error occurs during the refill operation,
  ///   such as reaching EOF.
  ///
  /// # Behavior
  /// 1. Calculate the range `[start, end)` for data to be read from the
  ///    underlying input.
  /// 2. Ensures that the read operation does not exceed the end of the file
  ///    (EOF).
  /// 3. Copies the unaligned bytes to the start of the buffer.
  /// 4. Reads new data into the remaining space in the buffer.
  /// 5. Updates the buffer's position and the valid data length
  ///    (`self.length`).
  ///
  /// # Note
  /// - The `buffer_start` is adjusted to include the unaligned bytes.
  /// - The new valid data length is the sum of the unaligned bytes and the
  ///   newly read bytes.
  ///
  /// # Errors
  /// * Returns [`LuceneError::eof`](crate::core::util::error::lucene_error::LuceneError::eof) if no new data can be read from the
  ///   underlying input.
  fn refill(&mut self, remain_unaligned_bytes: usize, start: usize) -> Result<()> {
    // After the last read, some unaligned bytes remain in the buffer.
    let mut end = start + (self.buffer_size - remain_unaligned_bytes);

    // Don't read past EOF
    let length = self.sub_index_input.length();
    if end > length {
      end = length;
    }

    let new_length = end - start;
    if new_length == 0 {
      return Err(LuceneError::eof(format!("read past EOF: {self}")));
    }

    if self.buffer.get_ref().is_empty() {
      let buffer_size = self.buffer_size;
      self.buffer.get_mut().resize(buffer_size, 0);
    }
    // valid data length in buffer
    self.length = new_length + remain_unaligned_bytes;
    // Set the buffer position to the remaining unaligned bytes
    // so that the next write within `read_internal` starts from remaining
    // unaligned bytes
    self
      .buffer
      .set_position(remain_unaligned_bytes.try_convert()?);
    self
      .sub_index_input
      .read_internal(&mut self.buffer, new_length, start)?;
    // Adjust buffer_start to include unaligned bytes
    self.buffer_start = start - remain_unaligned_bytes;
    Ok(())
  }
  fn read_longs(
    &mut self,
    pos: usize,
    len: usize,
    output: &mut [i64],
    use_buffer: bool,
  ) -> Result<()> {
    self.read_buffer(
      pos,
      len,
      output,
      BitUtil::LONG_BYTES,
      LE::read_i64,
      use_buffer,
    )
  }
  fn read_bytes(
    &mut self,
    pos: usize,
    len: usize,
    output: &mut [u8],
    use_buffer: bool,
  ) -> Result<()> {
    // `read_buffer` copies byte-sized values directly, so this converter is
    // only a fallback for the generic path. Keeping it total avoids turning
    // an internal dispatch invariant into a panic if that path changes.
    self.read_buffer(pos, len, output, 1, |bytes| bytes[0], use_buffer)
  }
  fn read_ints(
    &mut self,
    pos: usize,
    len: usize,
    output: &mut [i32],
    use_buffer: bool,
  ) -> Result<()> {
    self.read_buffer(
      pos,
      len,
      output,
      BitUtil::INT_BYTES,
      LE::read_i32,
      use_buffer,
    )
  }
  fn read_shorts(
    &mut self,
    pos: usize,
    len: usize,
    output: &mut [i16],
    use_buffer: bool,
  ) -> Result<()> {
    self.read_buffer(
      pos,
      len,
      output,
      BitUtil::SHORT_BYTES,
      LE::read_i16,
      use_buffer,
    )
  }
  fn read_floats(
    &mut self,
    pos: usize,
    len: usize,
    output: &mut [f32],
    use_buffer: bool,
  ) -> Result<()> {
    self.read_buffer(
      pos,
      len,
      output,
      BitUtil::FLOAT_BYTES,
      LE::read_f32,
      use_buffer,
    )
  }
  /// Reads and converts a specified number of elements starting at a given
  /// position into a target buffer.
  ///
  /// This method reads a specified number of bytes, starting at the position
  /// (`pos`), and converts them into elements of type `D` using the
  /// provided `converter` function. The converted elements are written
  /// into the `target` buffer.
  ///
  /// The method prioritizes reading from the internal buffer if the data is
  /// available. If the data is not in the buffer, it will either refill
  /// the buffer or read directly from the underlying input depending on the
  /// `use_buffer` flag.
  ///
  /// # Arguments
  /// * `pos` - The starting position in the file or stream to read from.
  /// * `target` - The target buffer where the converted data will be written.
  /// * `len` - The number of elements to read and convert.
  /// * `type_size` - The size in bytes of each element to be read.
  /// * `use_buffer` - Whether to use the internal buffer for reading. If
  ///   `false`, data will be read directly.
  /// * `converter` - A closure that processes a chunk of raw bytes into the
  ///   target buffer format.
  ///
  /// # Returns
  /// * `Ok(())` - If the requested data is successfully read and converted.
  /// * `Err(LuceneError)` - If an error occurs during reading, such as
  ///   reaching EOF.
  ///
  /// # Behavior
  /// - If `use_buffer` is `true` and the requested data fits in the buffer,
  ///   it will be read directly from the buffer.
  /// - If `use_buffer` is `true` but the requested data exceeds the buffer,
  ///   the method will refill the buffer and continue reading.
  /// - If `use_buffer` is `false`, data will be read directly from the
  ///   underlying input, bypassing the buffer.
  /// - The provided `converter` processes chunks of raw bytes to populate the
  ///   target buffer.
  ///
  /// # Errors
  /// This method may return the following errors:
  /// * [`LuceneError::eof`](crate::core::util::error::lucene_error::LuceneError::eof) - If attempting to read beyond the end of the file
  ///   or stream.
  ///
  /// # Note
  /// - The method assumes that the buffer's `refill` method ensures enough
  ///   data is available for reading, eliminating the need for additional
  ///   checks.
  /// - When unaligned data remains in the buffer (e.g., when the available
  ///   bytes are not a multiple of `type_size`), the method copies the
  ///   remaining bytes to the start of the buffer for further processing.
  ///   This ensures that subsequent reads start with aligned data. The
  ///   maximum amount of data copied is `type_size - 1` bytes, which is
  ///   minimal. For example, if the largest type being read is `i64` (8
  ///   bytes), at most 7 bytes are copied. Such small amounts of data copying
  ///   have negligible performance impact.
  fn read_buffer<D, F>(
    &mut self,
    pos: usize,
    len: usize,
    target: &mut [D],
    type_size: usize,
    converter: F,
    use_buffer: bool,
  ) -> Result<()>
  where
    D: Copy,
    F: Fn(&[u8]) -> D,
  {
    if len == 0 {
      return Ok(());
    }
    // Calculate the total bytes to read based on the number of elements and
    // the type size.
    let total_bytes = len * type_size;
    let mut elements_read = 0; // Tracks the number of elements read so far.
    let mut unaligned_bytes = 0; // Tracks bytes that cannot form a complete element.
    // Check if the position is within the current buffer range
    if pos >= self.buffer_start && pos < self.buffer_start + self.length {
      let buffer_offset = pos - self.buffer_start;
      // Determine the number of bytes available in the buffer from the
      // requested position.
      let available = self.buffer.remain_between(buffer_offset, self.length);
      // If the buffer contains all the data required for the request:
      if available >= total_bytes {
        let src = &self.buffer.get_ref()[buffer_offset..(buffer_offset + total_bytes)];
        Self::process_data(src, &mut target[0..len], len, type_size, &converter);
        return Ok(());
      }
      // Calculate the number of aligned bytes and elements that can be
      // fully read.
      let aligned_bytes = (available / type_size) * type_size;
      let aligned_elements = aligned_bytes / type_size;
      // Process aligned elements from the buffer.
      if aligned_elements > 0 {
        let src = &self.buffer.get_ref()[buffer_offset..(buffer_offset + aligned_bytes)];
        Self::process_data(
          src,
          &mut target[elements_read..elements_read + aligned_elements],
          aligned_elements,
          type_size,
          &converter,
        );
        elements_read += aligned_elements;
      }
      // Handle unaligned bytes that cannot form a complete element.
      unaligned_bytes = available - aligned_bytes;
      if unaligned_bytes > 0 {
        let buffer = self.buffer.get_mut();
        let unaligned_start = buffer_offset + aligned_bytes;
        // Copy unaligned bytes to the start of the buffer, we would
        // read these bytes later when buffer/temp_buffer was refilled
        // again
        buffer.copy_within(unaligned_start..unaligned_start + unaligned_bytes, 0);
      }
    }

    debug_assert!(self.buffer.position() <= u32::MAX as u64);
    // Calculate the remaining elements and bytes to read.
    let remaining_len = len - elements_read;
    let remaining_bytes = remaining_len * type_size;
    // If the buffer is used and the remaining bytes are less than the
    // buffer size, refill the buffer.
    if use_buffer && remaining_bytes < self.buffer_size {
      let start = self.buffer_start + self.length;
      self.refill(unaligned_bytes, start)?;

      let available = self.length;
      let readable_elements = (available / type_size).min(remaining_len);

      if readable_elements > 0 {
        let src = &self.buffer.get_ref()[0..readable_elements * type_size];
        Self::process_data(
          src,
          &mut target[elements_read..elements_read + readable_elements],
          readable_elements,
          type_size,
          &converter,
        );
      }
      // If there are still remaining elements, report EOF
      let remaining_elements = remaining_len - readable_elements;
      if remaining_elements > 0 {
        return Err(LuceneError::eof(format!(
          "read past EOF: expected {remaining_len}, got {readable_elements}"
        )));
      }

      return Ok(());
    }
    // If the buffer is not used or the remaining data exceeds the buffer
    // size, read directly from the underlying input
    let after = self.buffer_start + (self.length + remaining_bytes - unaligned_bytes);
    if after > self.sub_index_input.length() {
      return Err(LuceneError::eof(format!("read past EOF: {self}")));
    }
    // Prepare a temporary buffer to handle unaligned and remaining bytes.
    let scratch_len = remaining_bytes + unaligned_bytes;
    let reuse_scratch = scratch_len <= 8192;
    let mut temp_vec = if reuse_scratch {
      let mut bytes = std::mem::take(&mut self.bulk_scratch);
      bytes.clear();
      if bytes.capacity() < scratch_len {
        bytes.reserve_exact(scratch_len);
      }
      bytes.resize(scratch_len, 0);
      bytes
    } else {
      vec![0; scratch_len]
    };
    if unaligned_bytes > 0 {
      // If there are unaligned bytes left from the previous buffer,
      // copy them into the beginning of the temporary vector
      // (`temp_vec`).
      //
      // These unaligned bytes are those that could not form a complete
      // element (e.g., a full integer or floating-point
      // value) in the previous buffer. They were left
      // unprocessed and must be handled before reading new data
      // from the underlying input.
      //
      // The unaligned bytes are located at the end of the current buffer.
      // We copy them into the start of the temporary vector to ensure
      // they are preserved when the buffer is refilled with
      // new data.
      temp_vec.copy_from(
        &self.buffer.get_ref()[(self.buffer_size - unaligned_bytes)..],
        0,
      );
    }
    let mut temp_buffer = Cursor::new(temp_vec);
    temp_buffer.set_position(unaligned_bytes as u64);
    let read_result = self.sub_index_input.read_internal(
      &mut temp_buffer,
      remaining_bytes - unaligned_bytes,
      self.buffer_start + self.length,
    );
    if let Err(error) = read_result {
      if reuse_scratch && temp_buffer.get_ref().capacity() <= 8192 {
        self.bulk_scratch = temp_buffer.into_inner();
      }
      return Err(error);
    }

    debug_assert!(temp_buffer.position() == remaining_bytes as u64);
    let src = temp_buffer.get_ref();
    Self::process_data(
      src,
      &mut target[elements_read..elements_read + remaining_len],
      remaining_len,
      type_size,
      &converter,
    );

    if reuse_scratch && temp_buffer.get_ref().capacity() <= 8192 {
      self.bulk_scratch = temp_buffer.into_inner();
    }
    self.buffer_start = after;
    // we use temp_buffer to read underling data, so self.buffer is empty
    self.length = 0;
    Ok(())
  }
  /// Processes data by converting a source byte slice (`src`) into a
  /// destination slice (`dst`) of type `D`.
  ///
  /// # Parameters
  /// - `src`: A byte slice containing the source data to be processed.
  /// - `dst`: A mutable slice of type `D` where the converted data will be
  ///   stored.
  /// - `len`: The number of elements to process.
  /// - `type_size`: The size of each element in bytes in the source data.
  /// - `converter`: A conversion function that takes a byte slice and
  ///   converts it into type `D`.
  ///
  /// # Behavior
  /// - If `type_size` is 1, the function directly copies the data from `src`
  ///   to `dst` as bytes using unsafe code.
  /// - Otherwise, it iterates over `src` in chunks of `type_size`, applies
  ///   the `converter` function to each chunk, and stores the resulting value
  ///   in `dst`.
  ///
  /// # Safety
  /// - Unsafe code is used to cast `dst` to a mutable byte slice when
  ///   `type_size == 1`. The caller must ensure that `dst` has sufficient
  ///   capacity and correct alignment to avoid undefined behavior.
  fn process_data<D, F>(src: &[u8], dst: &mut [D], len: usize, type_size: usize, converter: &F)
  where
    D: Copy,
    F: Fn(&[u8]) -> D,
  {
    if type_size == 1 {
      unsafe {
        let dst_u8 = std::slice::from_raw_parts_mut(dst.as_mut_ptr() as *mut u8, len);
        dst_u8.copy_from(src, 0);
      }
    } else {
      for (i, dst_item) in dst.iter_mut().enumerate().take(len) {
        let chunk_start = i * type_size;
        *dst_item = converter(&src[chunk_start..chunk_start + type_size]);
      }
    }
  }
  /// Resolves the position within the buffer, optimizing for backward random
  /// reads.
  ///
  /// When performing random reads at position `pos`, this function checks if
  /// the requested position can be directly resolved within the current
  /// buffer. If not, it adjusts the buffer range dynamically to optimize
  /// subsequent backward reads:
  ///
  /// - For backward random reads, instead of always starting the buffer from
  ///   `pos`, the buffer is adjusted to include the range `[pos + width -
  ///   buffer_size, pos + width]`, if possible. This approach minimizes
  ///   redundant loading of the same data when successive backward reads
  ///   occur.
  ///
  /// - For forward random reads, the buffer starts directly at `pos` to
  ///   ensure data availability.
  ///
  /// # Arguments
  /// * `pos` - The target position to resolve.
  /// * `width` - The number of bytes needed for the read operation.
  ///
  /// # Returns
  /// * `Ok(pos)` - If the position is resolved successfully.
  /// * `Err(LuceneError)` - If an error occurs while seeking or refilling the
  ///   buffer.
  ///
  /// # Behavior
  /// - If the position `pos` is within the current buffer and there is enough
  ///   data for `width` bytes, the function returns `pos` directly.
  /// - If the position is outside the current buffer, it dynamically adjusts
  ///   `buffer_start`:
  ///   - For backward reads, it calculates a new `buffer_start` such that the
  ///     buffer includes the desired position and minimizes redundant
  ///     reloading of the same data.
  ///   - For forward reads, it directly sets `buffer_start` to `pos`.
  /// - The buffer is refilled as needed to ensure the requested data is
  ///   available.
  ///
  /// # Efficiency
  /// This method is particularly efficient for scenarios involving frequent
  /// backward random reads, as it reduces redundant I/O operations by
  /// aligning the buffer with anticipated access patterns.
  fn resolve_position_in_buffer(&mut self, pos: usize, width: usize) -> Result<()> {
    let index_opt = pos.checked_sub(self.buffer_start);
    if let Some(index) = index_opt
      && self.length.checked_sub(width).is_some_and(|v| index <= v)
    {
      return Ok(());
    }

    if index_opt.is_none() {
      // If we're moving backwards, then try and fill up the previous page
      // rather than starting again at the current pos, to
      // avoid successive backwards reads reloading
      // the same data over and over again.  We also check that we can
      // read `width` bytes without going over the end of the
      // buffer
      let v = self.buffer_start.max(pos + width);
      self.buffer_start = v.saturating_sub(self.buffer_size);
      self.buffer_start = self.buffer_start.min(pos);
      self.pos = self.buffer_start;
    } else {
      self.buffer_start = pos;
      self.pos = pos;
    }
    self.length = 0;
    self.sub_index_input.seek_internal(self.buffer_start)?;
    self.refill(0, self.buffer_start)?;
    Ok(())
  }
}

impl<T> crate::core::util::close::CloseableRef for BufferedIndexInput<T>
where
  T: BufferedIndexInputBase<Slice = BufferedIndexInput<T>>,
{
  fn close(&self) -> Result<()> {
    let _state = self.state.lock();
    self.sub_index_input.close()
  }
}

impl<T> DataInput for BufferedIndexInput<T>
where
  T: BufferedIndexInputBase<Slice = BufferedIndexInput<T>>,
{
  fn read_byte(&mut self) -> Result<u8> {
    let mut access = self.access_mut();
    let mut bytes = [0; 1];
    access.read_bytes(access.pos, 1, &mut bytes, true)?;
    access.pos += 1;
    Ok(bytes[0])
  }

  fn get_bytes<'a>(&'a mut self, len: usize) -> Result<std::borrow::Cow<'a, [u8]>> {
    let offset = {
      let state = self.state.get_mut();
      state.pos.checked_sub(state.buffer_start).filter(|&offset| {
        state
          .length
          .checked_sub(offset)
          .is_some_and(|remaining| len <= remaining)
      })
    };
    if let Some(offset) = offset {
      let state = self.state.get_mut();
      state.pos += len;
      return Ok(std::borrow::Cow::Borrowed(
        &state.buffer.get_ref()[offset..offset + len],
      ));
    }
    let refill = {
      let state = self.state.get_mut();
      len > 0 && len < self.buffer_size && state.pos == state.buffer_start + state.length
    };
    if refill {
      let mut access = self.access_mut();
      debug_assert!(access.buffer.position() <= u32::MAX as u64);
      let start = access.buffer_start + access.length;
      access.refill(0, start)?;
      if len > access.length {
        return Err(LuceneError::eof(format!(
          "read past EOF: expected {len}, got {}",
          access.length
        )));
      }
      access.pos += len;
      let state = self.state.get_mut();
      return Ok(std::borrow::Cow::Borrowed(&state.buffer.get_ref()[..len]));
    }
    let mut bytes = vec![0; len];
    let state = self.state.get_mut();
    let available =
      if state.pos >= state.buffer_start && state.pos < state.buffer_start + state.length {
        state.length - (state.pos - state.buffer_start)
      } else {
        0
      };
    let remaining = len - available;
    if len > 0 && remaining >= self.buffer_size {
      debug_assert!(state.buffer.position() <= u32::MAX as u64);
      let after = state.buffer_start + (state.length + remaining);
      if after > self.sub_index_input.length() {
        return Err(LuceneError::eof(format!(
          "read past EOF: BufferedIndexInput({})",
          self.resource_desc
        )));
      }
      // Read the suffix at its final offset so the cached prefix does not
      // require moving the newly read bytes after the input succeeds.
      let mut output = Cursor::new(bytes);
      output.set_position(available as u64);
      self.sub_index_input.read_internal(
        &mut output,
        remaining,
        state.buffer_start + state.length,
      )?;
      debug_assert!(output.position() == len as u64);
      let mut bytes = output.into_inner();
      bytes.resize(len, 0);
      if available > 0 {
        let offset = state.pos - state.buffer_start;
        bytes[..available].copy_from_slice(&state.buffer.get_ref()[offset..offset + available]);
      }
      state.buffer_start = after;
      state.length = 0;
      state.pos += len;
      return Ok(std::borrow::Cow::Owned(bytes));
    }
    self.read_bytes(&mut bytes, 0, len)?;
    Ok(std::borrow::Cow::Owned(bytes))
  }

  fn read_bytes(&mut self, b: &mut [u8], offset: usize, len: usize) -> Result<()> {
    self.read_bytes_with_buffer(b, offset, len, true)
  }

  fn read_bytes_with_buffer(
    &mut self,
    b: &mut [u8],
    offset: usize,
    len: usize,
    use_buffer: bool,
  ) -> Result<()> {
    let mut access = self.access_mut();
    access.read_bytes(access.pos, len, &mut b[offset..(offset + len)], use_buffer)?;
    access.pos += len;
    Ok(())
  }

  fn read_short(&mut self) -> Result<i16> {
    let mut access = self.access_mut();
    let mut output = [0; 1];
    access.read_shorts(access.pos, 1, &mut output, true)?;
    access.pos += BitUtil::SHORT_BYTES;
    Ok(output[0])
  }

  fn read_int(&mut self) -> Result<i32> {
    let mut access = self.access_mut();
    let mut output = [0; 1];
    access.read_ints(access.pos, 1, &mut output, true)?;
    access.pos += BitUtil::INT_BYTES;
    Ok(output[0])
  }

  fn read_group_vint(&mut self, dst: &mut [i32], offset: usize) -> Result<()> {
    let (pos, remain) = {
      let state = self.state.get_mut();
      let pos = state.buffer.position().try_convert()?;
      let remain = state.buffer.remain_between(pos, state.length);
      debug_assert!(state.buffer.position() <= i64::MAX as u64);
      (pos, remain)
    };
    let len = GroupVIntUtil::read_group_vint_i32_with_reader(self, remain, pos, dst, offset)?;
    self.state.get_mut().pos += len;
    Ok(())
  }

  fn read_long(&mut self) -> Result<i64> {
    let mut access = self.access_mut();
    let mut output = [0; 1];
    access.read_longs(access.pos, 1, &mut output, true)?;
    access.pos += BitUtil::LONG_BYTES;
    Ok(output[0])
  }

  fn read_longs(&mut self, dst: &mut [i64], offset: usize, len: usize) -> Result<()> {
    let mut access = self.access_mut();
    access.read_longs(access.pos, len, &mut dst[offset..(offset + len)], true)?;
    access.pos += len * BitUtil::LONG_BYTES;
    Ok(())
  }

  fn read_ints(&mut self, dst: &mut [i32], offset: usize, len: usize) -> Result<()> {
    let mut access = self.access_mut();
    access.read_ints(access.pos, len, &mut dst[offset..(offset + len)], true)?;
    access.pos += len * BitUtil::INT_BYTES;
    Ok(())
  }

  fn read_floats(&mut self, dst: &mut [f32], offset: usize, len: usize) -> Result<()> {
    let mut access = self.access_mut();
    access.read_floats(access.pos, len, &mut dst[offset..(offset + len)], true)?;
    access.pos += len * BitUtil::FLOAT_BYTES;
    Ok(())
  }

  fn skip_bytes(&mut self, num_bytes: i64) -> Result<()> {
    IndexInput::skip_bytes(self, num_bytes)
  }
}

impl<T> DataInputExt for BufferedIndexInput<T>
where
  T: BufferedIndexInputBase<Slice = BufferedIndexInput<T>>,
{
  crate::core::store::data_input_ext::impl_index_input_ext!();
}

impl<T> Display for BufferedIndexInput<T> {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "BufferedIndexInput({})", self.resource_desc)
  }
}

impl<T> crate::core::util::clone::TryClone for BufferedIndexInput<T>
where
  T: BufferedIndexInputBase<Slice = BufferedIndexInput<T>>,
{
  fn try_clone(&self) -> Result<Self> {
    let state = self.state.lock();
    let file_pointer = state.pos;
    let sub_index_input = self.sub_index_input.try_clone()?;
    Ok(Self {
      buffer_size: self.buffer_size,
      resource_desc: self.resource_desc.clone(),
      sub_index_input,
      state: Mutex::new(BufferedIndexInputState {
        buffer: Cursor::new(Vec::new()),
        buffer_start: file_pointer,
        pos: file_pointer,
        length: 0,
        bulk_scratch: Vec::new(),
      }),
    })
  }
}

impl<T> IndexInput for BufferedIndexInput<T>
where
  T: BufferedIndexInputBase<Slice = BufferedIndexInput<T>>,
{
  type IndexInput = Self;

  fn get_file_pointer(&self) -> Result<usize> {
    Ok(self.state.lock().pos)
  }

  fn seek(&mut self, pos: usize) -> Result<()> {
    let state = self.state.get_mut();
    if pos >= state.buffer_start && pos < (state.buffer_start + state.length) {
      state.pos = pos;
    } else {
      state.pos = pos;
      state.buffer_start = pos;
      state.length = 0;
      self.sub_index_input.seek_internal(pos)?;
    }
    Ok(())
  }

  fn length(&self) -> Result<usize> {
    Ok(self.sub_index_input.length())
  }

  fn slice(&self, slice_description: &str, offset: usize, length: usize) -> Result<Self> {
    let _state = self.state.lock();
    self
      .sub_index_input
      .slice(slice_description, offset, length)
  }

  type RandomAccessSlice = Self;

  fn random_access_slice(&self, offset: usize, length: usize) -> Result<Self> {
    self.slice("random_access_slice", offset, length)
  }
}

impl<T> RandomAccessInput for BufferedIndexInput<T>
where
  T: BufferedIndexInputBase<Slice = BufferedIndexInput<T>>,
{
  fn length(&self) -> Result<usize> {
    Ok(self.sub_index_input.length())
  }

  fn read_byte(&self, pos: usize) -> Result<u8> {
    let mut guard = self.state.lock();
    let mut state = self.access_state(&mut guard);
    let mut bytes = [0; 1];
    state.resolve_position_in_buffer(pos, 1)?;
    state.read_bytes(pos, 1, &mut bytes, true)?;
    Ok(bytes[0])
  }

  fn read_bytes(&self, pos: usize, len: usize) -> Result<std::borrow::Cow<'_, [u8]>> {
    let mut guard = self.state.lock();
    if len == 0 {
      return Ok(std::borrow::Cow::Borrowed(&[]));
    }
    if let Some(offset) = pos.checked_sub(guard.buffer_start)
      && guard
        .length
        .checked_sub(len)
        .is_some_and(|max| offset <= max)
    {
      return Ok(std::borrow::Cow::Owned(
        guard.buffer.get_ref()[offset..offset + len].to_vec(),
      ));
    }
    let mut state = self.access_state(&mut guard);
    state.resolve_position_in_buffer(pos, len)?;
    if let Some(offset) = pos.checked_sub(state.buffer_start)
      && state
        .length
        .checked_sub(len)
        .is_some_and(|max| offset <= max)
    {
      return Ok(std::borrow::Cow::Owned(
        state.buffer.get_ref()[offset..offset + len].to_vec(),
      ));
    }
    let mut bytes = vec![0; len];
    if len > state.buffer_size && len - state.length >= state.buffer_size {
      // A request wider than the cache resolves its start exactly to pos.
      debug_assert_eq!(pos, state.buffer_start);
      debug_assert!(state.buffer.position() <= u32::MAX as u64);
      let remaining = len - state.length;
      let after = state.buffer_start + (state.length + remaining);
      if after > state.sub_index_input.length() {
        return Err(LuceneError::eof(format!("read past EOF: {state}")));
      }
      // Keep the original underlying target shape, using the final owner.
      bytes.truncate(remaining);
      let mut output = Cursor::new(bytes);
      state.sub_index_input.read_internal(
        &mut output,
        remaining,
        state.buffer_start + state.length,
      )?;
      debug_assert!(output.position() == (len - state.length) as u64);
      let mut bytes = output.into_inner();
      bytes.resize(len, 0);
      let available = state.length;
      if available > 0 {
        bytes.copy_within(..len - available, available);
        bytes[..available].copy_from_slice(&state.buffer.get_ref()[..available]);
      }
      state.buffer_start += len;
      state.length = 0;
      return Ok(std::borrow::Cow::Owned(bytes));
    }
    state.read_bytes(pos, len, &mut bytes, true)?;
    Ok(std::borrow::Cow::Owned(bytes))
  }

  fn read_short(&self, pos: usize) -> Result<i16> {
    let mut guard = self.state.lock();
    let mut state = self.access_state(&mut guard);
    let mut bytes = [0; BitUtil::SHORT_BYTES];
    state.resolve_position_in_buffer(pos, BitUtil::SHORT_BYTES)?;
    state.read_shorts(pos, 1, &mut bytes, true)?;
    Ok(bytes[0])
  }

  fn read_int(&self, pos: usize) -> Result<i32> {
    let mut guard = self.state.lock();
    let mut state = self.access_state(&mut guard);
    let mut bytes = [0; BitUtil::INT_BYTES];
    state.resolve_position_in_buffer(pos, BitUtil::INT_BYTES)?;
    state.read_ints(pos, 1, &mut bytes, true)?;
    Ok(bytes[0])
  }

  fn read_long(&self, pos: usize) -> Result<i64> {
    let mut guard = self.state.lock();
    let mut state = self.access_state(&mut guard);
    let mut bytes = [0; BitUtil::LONG_BYTES];
    state.resolve_position_in_buffer(pos, BitUtil::LONG_BYTES)?;
    state.read_longs(pos, 1, &mut bytes, true)?;
    Ok(bytes[0])
  }

  fn prefetch(&self, _pos: usize, _len: usize) -> Result<()> {
    Ok(())
  }
}

impl<T> IntReader for BufferedIndexInput<T>
where
  T: BufferedIndexInputBase<Slice = BufferedIndexInput<T>>,
{
  fn read(&mut self, pos: usize) -> Result<i32> {
    let mut access = self.access_mut();
    let mut bytes = [0; BitUtil::INT_BYTES];
    access.resolve_position_in_buffer(pos, BitUtil::INT_BYTES)?;
    access.read_ints(pos, 1, &mut bytes, true)?;
    Ok(bytes[0])
  }
}

/// Default buffer size set to [`BUFFER_SIZE`].
pub const BUFFER_SIZE: usize = 1024;
/// Minimum buffer size allowed
pub const MIN_BUFFER_SIZE: usize = 8;

/// A buffer size for merges set to [`MERGE_BUFFER_SIZE`].  */
/// The normal read buffer size defaults to 1024, but
/// increasing this during merging seems to yield
/// performance gains.  However, we don't want to increase
/// it too much because there are quite a few
/// BufferedIndexInputs created during merging.  See
/// LUCENE-888 for details.
pub const MERGE_BUFFER_SIZE: usize = 4096;

#[cfg(test)]
impl<T> BufferedIndexInput<T>
where
  T: BufferedIndexInputBase<Slice = BufferedIndexInput<T>>,
{
  pub fn get_sub_index_input(&self) -> &T {
    &self.sub_index_input
  }
}
