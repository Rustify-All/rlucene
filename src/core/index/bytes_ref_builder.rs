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
use crate::core::index::bytes_ref::BytesRef;
use crate::core::util::CoreHelper;
use crate::core::util::access::{ByteSource, SharedAccessVec, WritableVec};
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::error::lucene_error::Result;

/// A builder for [`BytesRef`] instances.
pub struct BytesRefBuilder<AV> {
  pub(crate) bytes_ref: BytesRef<AV>,
}
impl<AV> Default for BytesRefBuilder<AV>
where
  AV: WritableVec<u8>,
{
  fn default() -> Self {
    Self::new()
  }
}

impl<AV> BytesRefBuilder<AV>
where
  AV: SharedAccessVec<u8>,
{
  pub fn new() -> BytesRefBuilder<AV>
  where
    AV: WritableVec<u8>,
  {
    BytesRefBuilder {
      bytes_ref: BytesRef::new(),
    }
  }
  /// Return a reference to the bytes of this builder.
  pub fn bytes(&self) -> &BytesRef<AV> {
    &self.bytes_ref
  }
  pub fn bytes_mut(&mut self) -> &mut BytesRef<AV> {
    &mut self.bytes_ref
  }

  /// Return the number of bytes in this buffer.
  pub fn length(&self) -> usize {
    self.bytes_ref.length
  }

  /// Set the length.
  pub fn set_length(&mut self, length: usize) {
    self.bytes_ref.length = length;
  }

  /// Return the byte at the given offset.
  pub fn byte_at(&self, index: usize) -> u8 {
    self.bytes_ref.bytes.access(|bytes| bytes[index])
  }

  /// Set a byte.
  pub fn set_byte_at(&mut self, offset: usize, value: u8)
  where
    AV: WritableVec<u8>,
  {
    self.bytes_ref.bytes.access_mut(|bytes| {
      bytes[offset] = value;
    })
  }
  pub fn grow(&mut self, capacity: usize) -> Result<()>
  where
    AV: WritableVec<u8>,
  {
    self
      .bytes_ref
      .bytes
      .access_mut(|bytes| ArrayUtil::grow_with(bytes, capacity, || 0))
  }
  pub fn grow_no_copy(&mut self, capacity: usize) -> Result<()>
  where
    AV: WritableVec<u8>,
  {
    self
      .bytes_ref
      .bytes
      .access_mut(|bytes| ArrayUtil::grow_no_copy(bytes, capacity, || 0))
  }

  /// Append a single byte to this builder.
  pub fn append_byte(&mut self, b: u8) -> Result<()>
  where
    AV: WritableVec<u8>,
  {
    self.grow(self.bytes_ref.length + 1)?;
    let idx = self.bytes_ref.length;
    self.bytes_ref.bytes.access_mut(|bytes| {
      bytes[idx] = b;
    });
    self.bytes_ref.length += 1;
    Ok(())
  }

  /// Append the provided bytes to this builder.
  pub fn append_with_range(&mut self, b: &[u8], off: usize, len: usize) -> Result<()>
  where
    AV: WritableVec<u8>,
  {
    self.grow(self.bytes_ref.length + len)?;
    let pos = self.bytes_ref.length;
    CoreHelper::check_from_index_size(off, len, b.len())?;
    let source = &b[off..off + len];
    self
      .bytes_mut()
      .bytes
      .access_mut(|bytes| bytes[pos..][..source.len()].copy_from_slice(source));
    self.bytes_ref.length += len;
    Ok(())
  }

  /// Append the provided bytes to this builder.
  pub fn append(&mut self, b: &BytesRef<AV>) -> Result<()>
  where
    AV: WritableVec<u8>,
  {
    b.bytes
      .access(|bytes| self.append_with_range(bytes, b.offset, b.length))
  }

  /// Reset this builder to the empty state.
  pub fn append_builder(&mut self, b: &mut BytesRefBuilder<AV>) -> Result<()>
  where
    AV: WritableVec<u8>,
  {
    self.append(b.get_bytes_mut_ref())
  }
  pub fn clear(&mut self) {
    self.set_length(0);
  }

  /// Replaces the content of this builder with the provided bytes.
  ///
  /// This is equivalent to calling [`clear`](BytesRefBuilder::clear) and then
  /// [`append`](BytesRefBuilder::append_with_range) with the specified
  /// `Vec<u8>` and range parameters (`start` and `end`).
  ///
  /// # Parameters
  /// - `bytes`: The byte vector to replace the current content.
  /// - `start`: The starting index of the byte slice to append.
  /// - `End`: The ending index of the byte slice to append.
  ///
  /// # See Also
  /// - [`clear`](BytesRefBuilder::clear)
  /// - [`append`](BytesRefBuilder::append_with_range)
  pub fn copy_bytes_from_vec(&mut self, b: &[u8], off: usize, len: usize) -> Result<()>
  where
    AV: WritableVec<u8>,
  {
    debug_assert_eq!(self.bytes_ref.offset, 0);
    self.bytes_ref.length = len;
    self.grow_no_copy(len)?;
    CoreHelper::check_from_index_size(off, len, b.len())?;
    let source = &b[off..off + len];
    self
      .bytes_mut()
      .bytes
      .access_mut(|bytes| bytes[..source.len()].copy_from_slice(source));
    Ok(())
  }
  pub fn copy_bytes_from_ref<B: ByteSource>(&mut self, b: &BytesRef<B>) -> Result<()>
  where
    AV: WritableVec<u8>,
  {
    self.copy_bytes_from_vec(b.bytes.as_slice(), b.offset, b.length)
  }
  pub fn copy_bytes_from_builder(&mut self, b: &mut BytesRefBuilder<AV>) -> Result<()>
  where
    AV: WritableVec<u8> + ByteSource,
  {
    self.copy_bytes_from_ref(b.get_bytes_mut_ref())
  }
  pub fn copy_chars_from_string(&mut self, s: &str) -> Result<()>
  where
    AV: WritableVec<u8>,
  {
    self.copy_chars_range(s, 0, s.len())
  }
  pub fn copy_chars_range(&mut self, s: &str, off: usize, len: usize) -> Result<()>
  where
    AV: WritableVec<u8>,
  {
    CoreHelper::check_from_index_size(off, len, s.len())?;
    let sub_bytes = &s.as_bytes()[off..off + len];
    self.copy_chars_from_vec(sub_bytes, 0, sub_bytes.len())
  }
  pub fn copy_chars_from_vec(&mut self, s: &[u8], off: usize, len: usize) -> Result<()>
  where
    AV: WritableVec<u8>,
  {
    self.grow(len)?;
    CoreHelper::check_from_index_size(off, len, s.len())?;
    let source = &s[off..off + len];
    self
      .bytes_ref
      .bytes
      .access_mut(|bytes| bytes[..source.len()].copy_from_slice(source));
    self.bytes_ref.length = len;
    self.bytes_ref.offset = 0;
    Ok(())
  }
  pub fn copy_chars_from_chars(&mut self, s: &[char], off: usize, len: usize) -> Result<()>
  where
    AV: WritableVec<u8>,
  {
    CoreHelper::check_from_index_size(off, len, s.len())?;
    let source = &s[off..off + len];
    self.bytes_ref.length = self.bytes_ref.bytes.access_mut(|bytes| {
      bytes.clear();
      let (groups, tail) = source.as_chunks::<8>();
      let ascii = groups.iter().all(|g| {
        ((g[0] as u32)
          | (g[1] as u32)
          | (g[2] as u32)
          | (g[3] as u32)
          | (g[4] as u32)
          | (g[5] as u32)
          | (g[6] as u32)
          | (g[7] as u32))
          < 0x80
      }) && tail.iter().all(|c| c.is_ascii());
      if bytes.capacity() < source.len() {
        let encoded_len = if ascii {
          source.len()
        } else {
          source.iter().map(|c| c.len_utf8()).sum()
        };
        bytes.reserve_exact(encoded_len);
      }
      if ascii {
        bytes.extend(source.iter().map(|c| *c as u8));
      } else if source.len() == 1 && source[0].len_utf8() == 4 {
        let mut buf = [0u8; 4];
        let encoded_str = source[0].encode_utf8(&mut buf);
        bytes.extend_from_slice(encoded_str.as_bytes());
      } else {
        for &c in source {
          if c.is_ascii() {
            bytes.push(c as u8);
          } else {
            let mut buf = [0u8; 4];
            let encoded_str = c.encode_utf8(&mut buf);
            match encoded_str.len() {
              2 => bytes.extend_from_slice(&encoded_str.as_bytes()[..2]),
              3 => bytes.extend_from_slice(&encoded_str.as_bytes()[..3]),
              4 => bytes.extend_from_slice(&encoded_str.as_bytes()[..4]),
              _ => bytes.extend_from_slice(encoded_str.as_bytes()),
            }
          }
        }
      }
      bytes.len()
    });
    self.bytes_ref.offset = 0;
    Ok(())
  }

  /// Return a BytesRef that points to the internal content of this builder.
  /// Any update to  the content of this builder might invalidate the
  /// provided bytes_ref and vice versa.
  pub fn get_bytes_mut_ref(&mut self) -> &mut BytesRef<AV> {
    debug_assert_eq!(
      self.bytes_ref.offset, 0,
      "Modifying the offset of the returned ref is illegal"
    );
    &mut self.bytes_ref
  }
  pub fn get_bytes_ref(&self) -> &BytesRef<AV> {
    debug_assert_eq!(
      self.bytes_ref.offset, 0,
      "Modifying the offset of the returned ref is illegal"
    );
    &self.bytes_ref
  }
  /// # Note
  /// This method should be only called with `BytesRef<Vec<u8>>`
  pub fn get_bytes_owner(&mut self) -> BytesRef<AV>
  where
    AV: WritableVec<u8>,
  {
    std::mem::take(&mut self.bytes_ref)
  }
  /// Build a new BytesRef that has the same content as this buffer.
  pub fn get_bytes_ref_copy(&self) -> Result<BytesRef<AV>> {
    self.bytes_ref.bytes.access(|bytes| {
      CoreHelper::check_from_index_size(0, self.bytes_ref.length, bytes.len())?;
      let slice = &bytes[..self.bytes_ref.length];
      Ok(BytesRef::from_bytes(AV::from_vec(slice.to_vec())))
    })
  }
}
