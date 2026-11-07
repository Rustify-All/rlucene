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
use crate::core::util::bit_util::BitUtil;
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::{CoreHelper, TryIntoInt};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt::Display;

// Reserve for medium-sized metadata collections, where the benchmarks show
// a benefit. Small and unusually large collections keep ordinary growth.
// The entry count comes from the input file, so never reserve without a bound.
const MAX_INITIAL_STRING_COLLECTION_CAPACITY: i32 = 1024;

// Keep the extra reservation logic out of the ordinary decoding methods, so
// their small and large collection loops can retain their original code shape.
#[inline(never)]
fn read_medium_string_map<DI: DataInput + ?Sized>(
  input: &mut DI,
  count: i32,
) -> Result<HashMap<String, String>> {
  let mut map = HashMap::new();
  for i in 0..4 {
    let key = input.read_string()?.into_owned();
    let value = input.read_string()?.into_owned();
    // Only reserve if the initial entries actually need a larger table.
    if i == 3 && map.len() == 3 && !map.contains_key(&key) {
      map.reserve(count as usize - map.len());
    }
    map.insert(key, value);
  }
  for _ in 4..count {
    map.insert(
      input.read_string()?.into_owned(),
      input.read_string()?.into_owned(),
    );
  }
  Ok(map)
}

#[inline(never)]
fn read_medium_string_set<DI: DataInput + ?Sized>(
  input: &mut DI,
  count: i32,
) -> Result<HashSet<String>> {
  let mut set = HashSet::new();
  for i in 0..4 {
    let value = input.read_string()?.into_owned();
    if i == 3 && set.len() == 3 && !set.contains(&value) {
      set.reserve(count as usize - set.len());
    }
    set.insert(value);
  }
  for _ in 4..count {
    set.insert(input.read_string()?.into_owned());
  }
  Ok(set)
}

/// Base trait for performing read operations on Lucene's low-level data types.
///
/// # Note
/// [`DataInput`] is not thread-safe as it maintains internal state (e.g., file
/// position).
pub trait DataInput: Display + DataInputExt {
  /// Reads and returns a single byte.
  ///
  /// # See Also
  /// [`DataOutput::write_byte`](crate::core::store::data_output::DataOutput::write_byte)
  fn read_byte(&mut self) -> Result<u8>;
  /// Reads the next `len` bytes, borrowing a contiguous input range when possible.
  ///
  /// The result borrows this input until its last use. Callers that need to keep
  /// bytes across another input operation must obtain ownership. This entry uses
  /// the ordinary three-argument Java read path, independently of buffer policy.
  /// Implementations may borrow their own reusable scratch buffer when the
  /// input cannot provide a contiguous view. Borrowed alone does not imply
  /// zero copying. A result that needs to outlive the borrow must be owned.
  /// On success the input advances by `len`. On failure it retains the state
  /// changes of the ordinary read path; reads are not rolled back.
  fn get_bytes(&mut self, len: usize) -> Result<Cow<'_, [u8]>>;
  /// Reads a specified number of bytes into an array at the specified offset.
  ///
  /// # Arguments
  /// * `b` - The array to read bytes into.
  /// * `offset` - The offset in the array to start storing bytes.
  /// * `len` - The number of bytes to read.
  ///
  /// # See Also
  /// [`DataOutput::write_bytes_range`](crate::core::store::data_output::DataOutput::write_bytes_range)
  fn read_bytes(&mut self, b: &mut [u8], offset: usize, len: usize) -> Result<()>;
  /// Reads a specified number of bytes into an array at the specified offset,
  /// with control over whether the read should be buffered. Callers who
  /// have their own buffer should pass `false` for `use_buffer`.
  /// Currently, only [`BufferedIndexInput`](crate::core::store::buffered_index_input::BufferedIndexInput) respects this parameter.
  ///
  /// # Arguments
  /// * `b` - The array to read bytes into.
  /// * `offset` - The offset in the array to start storing bytes.
  /// * `len` - The number of bytes to read.
  /// * `use_buffer` - Set to `false` if the caller handles buffering.
  ///
  /// # See Also
  /// [`DataOutput::write_bytes_with_len`](crate::core::store::data_output::DataOutput::write_bytes_with_len)
  fn read_bytes_with_buffer(
    &mut self,
    b: &mut [u8],
    offset: usize,
    len: usize,
    _use_buffer: bool,
  ) -> Result<()> {
    self.read_bytes(b, offset, len)
  }

  /// Reads two bytes and returns an `i16` (little-endian byte order).
  ///
  /// # See Also
  /// [`DataOutput::write_short`](crate::core::store::data_output::DataOutput::write_short)
  /// [`BitUtil::get_i16_le`](BitUtil::get_i16_le)
  fn read_short(&mut self) -> Result<i16> {
    let b1 = self.read_byte()?;
    let b2 = self.read_byte()?;
    Ok(i16::from_le_bytes([b1, b2]))
  }
  /// Reads four bytes and returns an `i32` (little-endian byte order).
  ///
  /// # See Also
  /// [`DataOutput::write_int`](crate::core::store::data_output::DataOutput::write_int)
  /// [`BitUtil::get_i32_le`](BitUtil::get_i32_le)
  fn read_int(&mut self) -> Result<i32> {
    let b1 = self.read_byte()?;
    let b2 = self.read_byte()?;
    let b3 = self.read_byte()?;
    let b4 = self.read_byte()?;
    Ok(i32::from_le_bytes([b1, b2, b3, b4]))
  }

  /// Implement this method when a more efficient implementation is available.
  /// In general, this is when the input supports
  /// random access.
  fn read_group_vint(&mut self, dst: &mut [i32], offset: usize) -> Result<()>;
  /// Reads an `i32` stored in a variable-length format. Reads between one and
  /// five bytes, with smaller values taking fewer bytes. Negative numbers
  /// are supported but should be avoided.
  ///
  /// # Format
  /// The format is described further in
  /// [`DataOutput::write_vint`](crate::core::store::data_output::DataOutput::write_vint).
  ///
  ///
  /// # See Also
  /// [`DataOutput::write_vint`](crate::core::store::data_output::DataOutput::write_vint)
  #[inline]
  fn read_vint(&mut self) -> Result<i32> {
    let mut b = self.read_byte()? as i32;
    let mut i = b & 0x7F;
    let mut shift = 7;

    while (b & 0x80) != 0 {
      b = self.read_byte()? as i32;
      i |= (b & 0x7F).wrapping_shl(shift);
      shift += 7;
    }
    Ok(i)
  }
  /// Reads a [`zig-zag`](BitUtil::zig_zag_decode_i32)-encoded
  /// [`read_vint`](Self::read_vint) variable-length integer.
  ///
  /// # See Also
  /// [`DataOutput::write_zint`](crate::core::store::data_output::DataOutput::write_zint)
  fn read_zint(&mut self) -> Result<i32> {
    Ok(BitUtil::zig_zag_decode_i32(self.read_vint()? as u32))
  }

  /// Reads eight bytes and returns an `i64` (little-endian byte order).
  ///
  /// # See Also
  /// [`DataOutput::write_long`](crate::core::store::data_output::DataOutput::write_long)
  /// [`BitUtil::get_i64_le`](BitUtil::get_i64_le)
  fn read_long(&mut self) -> Result<i64> {
    let b1 = self.read_int()? as u64 & 0xFFFFFFFF;
    let b2 = (self.read_int()? as u64) << 32;
    Ok((b2 | b1) as i64)
  }
  /// Reads a specified number of `i64` values.
  ///
  /// # Note
  /// This is an experimental API.
  fn read_longs(&mut self, dst: &mut [i64], offset: usize, len: usize) -> Result<()> {
    debug_assert!(dst.len() <= i32::MAX as usize);
    CoreHelper::check_from_index_size(offset, len, dst.len())?;
    let mut i = 0;
    while i < len {
      dst[i + offset] = self.read_long()?;
      i += 1;
    }
    Ok(())
  }
  /// Reads a specified number of `i32` values into a slice at the specified
  /// offset.
  ///
  /// # Arguments
  /// * `dst` - The slice to read values into.
  /// * `offset` - The offset in the slice to start storing `i32` values.
  /// * `length` - The number of `i32` values to read.
  fn read_ints(&mut self, dst: &mut [i32], offset: usize, len: usize) -> Result<()> {
    CoreHelper::check_from_index_size(offset, len, dst.len())?;
    let mut i = 0;
    while i < len {
      dst[i + offset] = self.read_int()?;
      i += 1;
    }
    Ok(())
  }

  /// Reads a specified number of `f32` values into a slice at the
  /// specified offset.
  ///
  /// # Arguments
  /// * `dst` - The slice to read values into.
  /// * `offset` - The offset in the slice to start storing `f32` values.
  /// * `len` - The number of `f32` values to read.
  fn read_floats(&mut self, dst: &mut [f32], offset: usize, len: usize) -> Result<()> {
    CoreHelper::check_from_index_size(offset, len, dst.len())?;
    let mut i = 0;
    while i < len {
      dst[i + offset] = f32::from_bits(self.read_int()? as u32);
      i += 1;
    }
    Ok(())
  }

  /// Reads an `i64` stored in a variable-length format. Reads between one and
  /// nine bytes, with smaller values taking fewer bytes. Negative numbers
  /// are not supported.
  ///
  /// # Format
  /// The format is described further in
  /// [`DataOutput::write_vint`](crate::core::store::data_output::DataOutput::write_vint).
  ///
  ///
  /// # See Also
  /// [`DataOutput::write_vlong`](crate::core::store::data_output::DataOutput::write_vlong)
  fn read_vlong(&mut self) -> Result<i64> {
    let mut b = self.read_byte()? as i64;
    let mut i = b & 0x7F;
    let mut shift = 7;
    while (b & 0x80) != 0 {
      b = self.read_byte()? as i64;
      i |= (b & 0x7F).wrapping_shl(shift);
      shift += 7;
    }
    Ok(i)
  }
  /// Reads a [`zig-zag`](BitUtil::zig_zag_decode_i64)-encoded
  /// [`read_vlong`](Self::read_vlong) variable-length integer. Reads
  /// between one and ten bytes.
  ///
  /// # See Also
  /// [`DataOutput::write_zlong`](crate::core::store::data_output::DataOutput::write_zlong)
  fn read_zlong(&mut self) -> Result<i64> {
    Ok(BitUtil::zig_zag_decode_i64(self.read_vlong()? as u64))
  }
  /// Reads a string, borrowing valid UTF-8 from the input or its scratch buffer.
  /// Callers that retain
  /// the result across another input operation must obtain ownership.
  ///
  /// # See Also
  /// [`DataOutput::write_string`](crate::core::store::data_output::DataOutput::write_string)
  fn read_string<'a>(&'a mut self) -> Result<std::borrow::Cow<'a, str>> {
    let length = self.read_vint()?.try_convert()?;
    Ok(match self.get_bytes(length)? {
      Cow::Borrowed(bytes) => match std::str::from_utf8(bytes) {
        Ok(value) => Cow::Borrowed(value),
        Err(_) => Cow::Owned(String::from_utf8_lossy(bytes).into_owned()),
      },
      Cow::Owned(bytes) => Cow::Owned(match String::from_utf8(bytes) {
        Ok(value) => value,
        Err(error) => String::from_utf8_lossy(error.as_bytes()).into_owned(),
      }),
    })
  }

  /// Reads a `HashMap<String, String>` previously written with
  /// [`DataOutput::write_map_of_strings`](crate::core::store::data_output::DataOutput::write_map_of_strings).
  ///
  /// Returns an owned map containing the decoded entries. The caller may
  /// mutate the returned map.
  fn read_map_of_strings(&mut self) -> Result<HashMap<String, String>> {
    let count = self.read_vint()?;
    if (8..=MAX_INITIAL_STRING_COLLECTION_CAPACITY).contains(&count) {
      return read_medium_string_map(self, count);
    }

    if count == 0 {
      Ok(HashMap::new())
    } else if count == 1 {
      let mut map = HashMap::new();
      map.insert(
        self.read_string()?.into_owned(),
        self.read_string()?.into_owned(),
      );
      Ok(map)
    } else {
      let mut map = HashMap::new();
      for _ in 0..count {
        map.insert(
          self.read_string()?.into_owned(),
          self.read_string()?.into_owned(),
        );
      }
      Ok(map)
    }
  }
  /// Reads a `HashSet<String>` previously written with
  /// [`DataOutput::write_set_of_strings`](crate::core::store::data_output::DataOutput::write_set_of_strings).
  ///
  /// Returns an owned set containing the decoded entries. The caller may
  /// mutate the returned set.
  fn read_set_of_strings(&mut self) -> Result<HashSet<String>> {
    let count = self.read_vint()?;
    if (8..=MAX_INITIAL_STRING_COLLECTION_CAPACITY).contains(&count) {
      return read_medium_string_set(self, count);
    }
    if count == 0 {
      Ok(HashSet::new())
    } else if count == 1 {
      let mut set = HashSet::new();
      set.insert(self.read_string()?.into_owned());
      Ok(set)
    } else {
      let mut set = HashSet::new();
      for _ in 0..count {
        set.insert(self.read_string()?.into_owned());
      }
      Ok(set)
    }
  }
  /// Skips over `num_bytes` bytes. This method may skip bytes in whatever way
  /// is most optimal, and may not behave the same as reading the skipped
  /// bytes.
  fn skip_bytes(&mut self, num_bytes: i64) -> Result<()>;
}

impl<T: ?Sized + Closeable> Closeable for Box<T> {
  fn close(&mut self) -> Result<()> {
    (**self).close()
  }
}

impl<T: ?Sized + DataInput> DataInput for &mut T {
  fn read_byte(&mut self) -> Result<u8> {
    (**self).read_byte()
  }

  fn get_bytes<'a>(&'a mut self, len: usize) -> Result<Cow<'a, [u8]>> {
    DataInput::get_bytes(&mut **self, len)
  }

  fn read_bytes(&mut self, b: &mut [u8], offset: usize, len: usize) -> Result<()> {
    (**self).read_bytes(b, offset, len)
  }

  fn read_bytes_with_buffer(
    &mut self,
    b: &mut [u8],
    offset: usize,
    len: usize,
    _use_buffer: bool,
  ) -> Result<()> {
    (**self).read_bytes_with_buffer(b, offset, len, _use_buffer)
  }

  fn read_short(&mut self) -> Result<i16> {
    (**self).read_short()
  }

  fn read_int(&mut self) -> Result<i32> {
    (**self).read_int()
  }

  fn read_group_vint(&mut self, dst: &mut [i32], offset: usize) -> Result<()> {
    (**self).read_group_vint(dst, offset)
  }

  fn read_vint(&mut self) -> Result<i32> {
    (**self).read_vint()
  }

  fn read_zint(&mut self) -> Result<i32> {
    (**self).read_zint()
  }

  fn read_long(&mut self) -> Result<i64> {
    (**self).read_long()
  }

  fn read_longs(&mut self, dst: &mut [i64], offset: usize, len: usize) -> Result<()> {
    (**self).read_longs(dst, offset, len)
  }

  fn read_ints(&mut self, dst: &mut [i32], offset: usize, len: usize) -> Result<()> {
    (**self).read_ints(dst, offset, len)
  }

  fn read_floats(&mut self, dst: &mut [f32], offset: usize, len: usize) -> Result<()> {
    (**self).read_floats(dst, offset, len)
  }

  fn read_vlong(&mut self) -> Result<i64> {
    (**self).read_vlong()
  }

  fn read_zlong(&mut self) -> Result<i64> {
    (**self).read_zlong()
  }

  fn read_string<'a>(&'a mut self) -> Result<std::borrow::Cow<'a, str>> {
    (**self).read_string()
  }

  fn read_map_of_strings(&mut self) -> Result<HashMap<String, String>> {
    (**self).read_map_of_strings()
  }

  fn read_set_of_strings(&mut self) -> Result<HashSet<String>> {
    (**self).read_set_of_strings()
  }

  fn skip_bytes(&mut self, num_bytes: i64) -> Result<()> {
    (**self).skip_bytes(num_bytes)
  }
}

impl<T: ?Sized + DataInput> DataInput for Box<T> {
  fn read_byte(&mut self) -> Result<u8> {
    (**self).read_byte()
  }

  fn get_bytes<'a>(&'a mut self, len: usize) -> Result<Cow<'a, [u8]>> {
    DataInput::get_bytes(&mut **self, len)
  }

  fn read_bytes(&mut self, b: &mut [u8], offset: usize, len: usize) -> Result<()> {
    (**self).read_bytes(b, offset, len)
  }

  fn read_bytes_with_buffer(
    &mut self,
    b: &mut [u8],
    offset: usize,
    len: usize,
    _use_buffer: bool,
  ) -> Result<()> {
    (**self).read_bytes_with_buffer(b, offset, len, _use_buffer)
  }

  fn read_short(&mut self) -> Result<i16> {
    (**self).read_short()
  }

  fn read_int(&mut self) -> Result<i32> {
    (**self).read_int()
  }

  fn read_group_vint(&mut self, dst: &mut [i32], offset: usize) -> Result<()> {
    (**self).read_group_vint(dst, offset)
  }

  fn read_vint(&mut self) -> Result<i32> {
    (**self).read_vint()
  }

  fn read_zint(&mut self) -> Result<i32> {
    (**self).read_zint()
  }

  fn read_long(&mut self) -> Result<i64> {
    (**self).read_long()
  }

  fn read_longs(&mut self, dst: &mut [i64], offset: usize, len: usize) -> Result<()> {
    (**self).read_longs(dst, offset, len)
  }

  fn read_ints(&mut self, dst: &mut [i32], offset: usize, len: usize) -> Result<()> {
    (**self).read_ints(dst, offset, len)
  }

  fn read_floats(&mut self, dst: &mut [f32], offset: usize, len: usize) -> Result<()> {
    (**self).read_floats(dst, offset, len)
  }

  fn read_vlong(&mut self) -> Result<i64> {
    (**self).read_vlong()
  }

  fn read_zlong(&mut self) -> Result<i64> {
    (**self).read_zlong()
  }

  fn read_string<'a>(&'a mut self) -> Result<std::borrow::Cow<'a, str>> {
    (**self).read_string()
  }

  fn read_map_of_strings(&mut self) -> Result<HashMap<String, String>> {
    (**self).read_map_of_strings()
  }

  fn read_set_of_strings(&mut self) -> Result<HashSet<String>> {
    (**self).read_set_of_strings()
  }

  fn skip_bytes(&mut self, num_bytes: i64) -> Result<()> {
    (**self).skip_bytes(num_bytes)
  }
}

macro_rules! define_data_input_enum {
    (
        $vis:vis enum $name:ident < $( $T:ident => $V:ident ),+ $(,)? >
    ) => {
        $vis enum $name<$( $T ),+> {
            $(
                $V($T),
            )+
        }

        impl<$( $T ),+> std::fmt::Display for $name<$( $T ),+>
        where
            $( $T: std::fmt::Display ),+
        {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match self {
                    $(
                        Self::$V(inner) => inner.fmt(f),
                    )+
                }
            }
        }

        impl<$( $T ),+> Closeable for $name<$( $T ),+> {}

        impl<$( $T ),+> DataInput for $name<$( $T ),+>
        where
            $( $T: DataInput ),+
        {
            fn read_byte(&mut self) -> Result<u8> {
                match self {
                    $(
                        Self::$V(inner) => inner.read_byte(),
                    )+
                }
            }

            fn get_bytes<'a>(&'a mut self, len: usize) -> Result<std::borrow::Cow<'a, [u8]>> {
                match self {
                    $( Self::$V(inner) => DataInput::get_bytes(inner, len), )+
                }
            }

            fn read_bytes(&mut self, b: &mut [u8], offset: usize, len: usize) -> Result<()> {
                match self {
                    $(
                        Self::$V(inner) => inner.read_bytes(b, offset, len),
                    )+
                }
            }

            fn read_bytes_with_buffer(
                &mut self,
                b: &mut [u8],
                offset: usize,
                len: usize,
                use_buffer: bool,
            ) -> Result<()> {
                match self {
                    $(
                        Self::$V(inner) => inner.read_bytes_with_buffer(b, offset, len, use_buffer),
                    )+
                }
            }

            fn read_short(&mut self) -> Result<i16> {
                match self {
                    $(
                        Self::$V(inner) => inner.read_short(),
                    )+
                }
            }

            fn read_int(&mut self) -> Result<i32> {
                match self {
                    $(
                        Self::$V(inner) => inner.read_int(),
                    )+
                }
            }

            fn read_group_vint(&mut self, dst: &mut [i32], offset: usize) -> Result<()> {
                match self {
                    $(
                        Self::$V(inner) => inner.read_group_vint(dst, offset),
                    )+
                }
            }

            fn read_vint(&mut self) -> Result<i32> {
                match self {
                    $(
                        Self::$V(inner) => inner.read_vint(),
                    )+
                }
            }

            fn read_zint(&mut self) -> Result<i32> {
                match self {
                    $(
                        Self::$V(inner) => inner.read_zint(),
                    )+
                }
            }

            fn read_long(&mut self) -> Result<i64> {
                match self {
                    $(
                        Self::$V(inner) => inner.read_long(),
                    )+
                }
            }

            fn read_longs(&mut self, dst: &mut [i64], offset: usize, len: usize) -> Result<()> {
                match self {
                    $(
                        Self::$V(inner) => inner.read_longs(dst, offset, len),
                    )+
                }
            }

            fn read_ints(&mut self, dst: &mut [i32], offset: usize, len: usize) -> Result<()> {
                match self {
                    $(
                        Self::$V(inner) => inner.read_ints(dst, offset, len),
                    )+
                }
            }

            fn read_floats(&mut self, dst: &mut [f32], offset: usize, len: usize) -> Result<()> {
                match self {
                    $(
                        Self::$V(inner) => inner.read_floats(dst, offset, len),
                    )+
                }
            }

            fn read_vlong(&mut self) -> Result<i64> {
                match self {
                    $(
                        Self::$V(inner) => inner.read_vlong(),
                    )+
                }
            }

            fn read_zlong(&mut self) -> Result<i64> {
                match self {
                    $(
                        Self::$V(inner) => inner.read_zlong(),
                    )+
                }
            }

            fn read_string<'a>(&'a mut self) -> Result<std::borrow::Cow<'a, str>> {
                match self {
                    $(
                        Self::$V(inner) => inner.read_string(),
                    )+
                }
            }

            fn read_map_of_strings(&mut self) -> Result<std::collections::HashMap<String, String>> {
                match self {
                    $(
                        Self::$V(inner) => inner.read_map_of_strings(),
                    )+
                }
            }

            fn read_set_of_strings(&mut self) -> Result<std::collections::HashSet<String>> {
                match self {
                    $(
                        Self::$V(inner) => inner.read_set_of_strings(),
                    )+
                }
            }

            fn skip_bytes(&mut self, num_bytes: i64) -> Result<()> {
                match self {
                    $(
                        Self::$V(inner) => inner.skip_bytes(num_bytes),
                    )+
                }
            }
        }

        impl<$( $T ),+> $crate::core::store::data_input_ext::DataInputExt for $name<$( $T ),+>
        where
            $( $T: DataInput ),+
        {
            fn is_index_input(&self) -> bool {
                match self {
                    $(
                        Self::$V(inner) => inner.is_index_input(),
                    )+
                }
            }

            fn seek_in_data_input(&mut self, pos: usize) -> Result<()> {
                match self {
                    $(
                        Self::$V(inner) => inner.seek_in_data_input(pos),
                    )+
                }
            }

            fn get_file_pointer_in_data_input(&self) -> Result<usize> {
                match self {
                    $(
                        Self::$V(inner) => inner.get_file_pointer_in_data_input(),
                    )+
                }
            }
        }
    };
}
define_data_input_enum!(
    pub enum DataInputEnum3<
        A => A,
        B => B,
        C => C,
    >
);
