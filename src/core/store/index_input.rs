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
use crate::core::store::byte_buffers_index_input::ByteBuffersIndexInputOwned;
use crate::core::store::memory_segment_index_input::MemorySegmentIndexInput;
use crate::core::store::nio_fs_directory::NIOFSIndexInput;
use crate::core::store::random_access_input::{
  BoxRandomAccessInput, RandomAccessInput, RandomAccessInputEnum2, RandomAccessInputEnum3,
};
use crate::core::store::{BufferedIndexInput, DataInput, ReadAdvice};
use crate::core::util::TryIntoInt;
use crate::core::util::clone::TryClone;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

/// Provides random-access input operations for files within a
/// [`Directory`](crate::core::store::directory::Directory).
///
/// `IndexInput` supports reading data from a file and maintains its own
/// internal state, such as the current file position.
///
/// # Thread Safety
///
/// `IndexInput` is **not thread-safe**. If you need to use it in multiple
/// threads, you must **clone** the `IndexInput` instance. Each clone operates
/// on the same underlying resource but maintains an independent position.
///
///
/// # See Also
/// - [`Directory`](crate::core::store::directory::Directory) for file-based
///   operations.
pub trait IndexInput: DataInput + TryClone + CloseableRef {
  /// The index input type returned by slicing operations.
  type IndexInput: IndexInput;
  /// Returns the current position in this file, where the next read will
  /// occur.
  ///
  /// # See Also
  /// [`seek`](IndexInput::seek)
  fn get_file_pointer(&self) -> Result<usize>;

  /// Sets the current position in this file, where the next read will occur.
  /// If this position is beyond the end of the file, it will return an
  /// `EOFError`, and the stream will be in an undetermined state.
  ///
  /// # See Also
  /// [`get_file_pointer`](IndexInput::get_file_pointer)
  fn seek(&mut self, pos: usize) -> Result<()>;
  /// Inherits documentation from the parent implementation.
  ///
  /// # Behavior
  /// This is functionally equivalent to seeking to `get_file_pointer() +
  /// num_bytes`.
  ///
  /// # See Also
  /// [`get_file_pointer`](IndexInput::get_file_pointer)
  ///
  /// [`seek`](IndexInput::seek)
  fn skip_bytes(&mut self, num_bytes: i64) -> Result<()> {
    if num_bytes < 0 {
      return Err(LuceneError::illegal_argument(format!(
        "num_bytes must be >= 0, got {num_bytes}"
      )));
    }
    let num_bytes: usize = num_bytes.try_convert()?;
    let skip_to = self.get_file_pointer()? + num_bytes;
    self.seek(skip_to)?;
    Ok(())
  }
  /// The number of bytes in the file.
  fn length(&self) -> Result<usize>;

  /// Creates a slice of this index input, with the given description, offset,
  /// and length. The slice is positioned at the beginning.
  fn slice(
    &self,
    _slice_description: &str,
    _offset: usize,
    _length: usize,
  ) -> Result<Self::IndexInput> {
    Err(LuceneError::unsupported_operation("not support slicing"))
  }
  /// Creates a slice with a specific [`ReadAdvice`]. This is typically used
  /// by [`CompoundFormat`](crate::core::codecs::compound_format)
  /// implementations to honor the [`ReadAdvice`] of each file within the
  /// compound file.
  ///
  /// # Note
  /// It is only legal to call this method if this `IndexInput` has been
  /// opened with `ReadAdvice::NORMAL`. However, this method accepts any
  /// `ReadAdvice` value except `None` for the slice.
  ///
  /// The default implementation delegates to [`slice`](IndexInput::slice) and
  /// ignores the `ReadAdvice`.
  fn slice_with_read_advice(
    &self,
    description: &str,
    offset: usize,
    length: usize,
    _read_advice: &ReadAdvice,
  ) -> Result<Self::IndexInput> {
    self.slice(description, offset, length)
  }
  type RandomAccessSlice: RandomAccessInput;
  /// Creates a random-access slice of this index input, with the given offset
  /// and length.
  ///
  /// # Note
  /// The default implementation calls [`slice`](IndexInput::slice), and if it
  /// doesn't support random access. Wrap with [`RandomAccessInputWrapper`](crate::core::store::random_access_input::RandomAccessInputWrapper) It implements absolute reads as
  /// seek+read.
  fn random_access_slice(&self, offset: usize, length: usize) -> Result<Self::RandomAccessSlice>;

  /// Optional method: Gives a hint to this input that some bytes will be read
  /// soon. `IndexInput` implementations may take advantage of this hint
  /// to start fetching pages of data immediately from storage.
  ///
  /// # Arguments
  /// * `offset` - The starting offset.
  /// * `length` - The number of bytes to prefetch.
  ///
  /// # Note
  /// The default implementation is a no-op.
  fn prefetch(&mut self, _pos: usize, _len: usize) -> Result<()> {
    Ok(())
  }

  fn update_read_advice(&self, _read_advice: ReadAdvice) -> Result<()> {
    Ok(())
  }

  /// Returns a hint whether all the contents of this input are resident in physical memory. It's a
  /// hint because the operating system may have paged out some of the data by the time this method
  /// returns. If the optional is true, then it's likely that the contents of this input are resident
  /// in physical memory. A value of false does not imply that the contents are not resident in
  /// physical memory. [`None`] is returned if it is not possible to determine.
  ///
  /// This runs in linear time with the [`length`](IndexInput::length) of this input / page size.
  ///
  /// The default implementation returns [`None`].
  fn is_loaded(&self) -> Result<Option<bool>> {
    Ok(None)
  }
}
pub trait ErasedIndexInput:
  IndexInput<RandomAccessSlice = BoxRandomAccessInput, IndexInput = IndexInputEnum>
{
  fn try_clone_index_input(&self) -> Result<IndexInputEnum>;
}

pub type DynIndexInput = dyn ErasedIndexInput + Send + Sync;
pub type CustomIndexInput = Box<DynIndexInput>;

pub type IndexInputEnumRandomAccessSlice =
  RandomAccessInputEnum2<BufferedIndexInput<NIOFSIndexInput>, BoxRandomAccessInput>;

pub enum IndexInputEnum {
  Fs(BufferedIndexInput<NIOFSIndexInput>),
  Custom(CustomIndexInput),
}

impl IndexInputEnum {
  pub fn custom<I>(input: I) -> Self
  where
    I: ErasedIndexInput + Send + Sync + 'static,
  {
    Self::Custom(Box::new(input))
  }
}

impl CloseableRef for IndexInputEnum {
  fn close(&self) -> Result<()> {
    match self {
      IndexInputEnum::Fs(inner) => inner.close(),
      IndexInputEnum::Custom(inner) => inner.close(),
    }
  }
}

impl DataInput for IndexInputEnum {
  fn read_byte(&mut self) -> Result<u8> {
    match self {
      IndexInputEnum::Fs(inner) => DataInput::read_byte(inner),
      IndexInputEnum::Custom(inner) => inner.read_byte(),
    }
  }

  fn read_bytes(&mut self, b: &mut [u8], offset: usize, len: usize) -> Result<()> {
    match self {
      IndexInputEnum::Fs(inner) => DataInput::read_bytes(inner, b, offset, len),
      IndexInputEnum::Custom(inner) => inner.read_bytes(b, offset, len),
    }
  }

  fn read_bytes_with_buffer(
    &mut self,
    b: &mut [u8],
    offset: usize,
    len: usize,
    _use_buffer: bool,
  ) -> Result<()> {
    match self {
      IndexInputEnum::Fs(inner) => inner.read_bytes_with_buffer(b, offset, len, _use_buffer),
      IndexInputEnum::Custom(inner) => inner.read_bytes_with_buffer(b, offset, len, _use_buffer),
    }
  }

  fn read_short(&mut self) -> Result<i16> {
    match self {
      IndexInputEnum::Fs(inner) => DataInput::read_short(inner),
      IndexInputEnum::Custom(inner) => inner.read_short(),
    }
  }

  fn read_int(&mut self) -> Result<i32> {
    match self {
      IndexInputEnum::Fs(inner) => DataInput::read_int(inner),
      IndexInputEnum::Custom(inner) => inner.read_int(),
    }
  }

  fn read_group_vint(&mut self, dst: &mut [i32], offset: usize) -> Result<()> {
    match self {
      IndexInputEnum::Fs(inner) => inner.read_group_vint(dst, offset),
      IndexInputEnum::Custom(inner) => inner.read_group_vint(dst, offset),
    }
  }

  fn read_vint(&mut self) -> Result<i32> {
    match self {
      IndexInputEnum::Fs(inner) => DataInput::read_vint(inner),
      IndexInputEnum::Custom(inner) => inner.read_vint(),
    }
  }

  fn read_zint(&mut self) -> Result<i32> {
    match self {
      IndexInputEnum::Fs(inner) => DataInput::read_zint(inner),
      IndexInputEnum::Custom(inner) => inner.read_zint(),
    }
  }

  fn read_long(&mut self) -> Result<i64> {
    match self {
      IndexInputEnum::Fs(inner) => DataInput::read_long(inner),
      IndexInputEnum::Custom(inner) => inner.read_long(),
    }
  }

  fn read_longs(&mut self, dst: &mut [i64], offset: usize, len: usize) -> Result<()> {
    match self {
      IndexInputEnum::Fs(inner) => inner.read_longs(dst, offset, len),
      IndexInputEnum::Custom(inner) => inner.read_longs(dst, offset, len),
    }
  }

  fn read_ints(&mut self, dst: &mut [i32], offset: usize, len: usize) -> Result<()> {
    match self {
      IndexInputEnum::Fs(inner) => inner.read_ints(dst, offset, len),
      IndexInputEnum::Custom(inner) => inner.read_ints(dst, offset, len),
    }
  }

  fn read_floats(&mut self, dst: &mut [f32], offset: usize, len: usize) -> Result<()> {
    match self {
      IndexInputEnum::Fs(inner) => inner.read_floats(dst, offset, len),
      IndexInputEnum::Custom(inner) => inner.read_floats(dst, offset, len),
    }
  }

  fn read_vlong(&mut self) -> Result<i64> {
    match self {
      IndexInputEnum::Fs(inner) => DataInput::read_vlong(inner),
      IndexInputEnum::Custom(inner) => inner.read_vlong(),
    }
  }

  fn read_zlong(&mut self) -> Result<i64> {
    match self {
      IndexInputEnum::Fs(inner) => DataInput::read_zlong(inner),
      IndexInputEnum::Custom(inner) => inner.read_zlong(),
    }
  }

  fn read_string(&mut self) -> Result<String> {
    match self {
      IndexInputEnum::Fs(inner) => DataInput::read_string(inner),
      IndexInputEnum::Custom(inner) => inner.read_string(),
    }
  }

  fn read_map_of_strings(&mut self) -> Result<HashMap<String, String>> {
    match self {
      IndexInputEnum::Fs(inner) => DataInput::read_map_of_strings(inner),
      IndexInputEnum::Custom(inner) => inner.read_map_of_strings(),
    }
  }

  fn read_set_of_strings(&mut self) -> Result<HashSet<String>> {
    match self {
      IndexInputEnum::Fs(inner) => DataInput::read_set_of_strings(inner),
      IndexInputEnum::Custom(inner) => inner.read_set_of_strings(),
    }
  }

  fn skip_bytes(&mut self, num_bytes: i64) -> Result<()> {
    match self {
      IndexInputEnum::Fs(inner) => DataInput::skip_bytes(inner, num_bytes),
      IndexInputEnum::Custom(inner) => DataInput::skip_bytes(inner, num_bytes),
    }
  }

  fn is_index_input(&self) -> bool {
    match self {
      IndexInputEnum::Fs(inner) => inner.is_index_input(),
      IndexInputEnum::Custom(inner) => inner.is_index_input(),
    }
  }

  fn seek_in_data_input(&mut self, _pos: usize) -> Result<()> {
    match self {
      IndexInputEnum::Fs(inner) => inner.seek_in_data_input(_pos),
      IndexInputEnum::Custom(inner) => inner.seek_in_data_input(_pos),
    }
  }

  fn get_file_pointer_in_data_input(&self) -> Result<usize> {
    match self {
      IndexInputEnum::Fs(inner) => inner.get_file_pointer_in_data_input(),
      IndexInputEnum::Custom(inner) => inner.get_file_pointer_in_data_input(),
    }
  }
}

impl Display for IndexInputEnum {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self {
      IndexInputEnum::Fs(inner) => inner.fmt(f),
      IndexInputEnum::Custom(inner) => inner.fmt(f),
    }
  }
}
impl TryClone for IndexInputEnum {
  fn try_clone(&self) -> Result<Self>
  where
    Self: Sized,
  {
    match self {
      IndexInputEnum::Fs(inner) => Ok(IndexInputEnum::Fs(inner.try_clone()?)),
      IndexInputEnum::Custom(inner) => inner.try_clone_index_input(),
    }
  }
}

impl IndexInput for IndexInputEnum {
  type IndexInput = IndexInputEnum;

  fn get_file_pointer(&self) -> Result<usize> {
    match self {
      IndexInputEnum::Fs(inner) => inner.get_file_pointer(),
      IndexInputEnum::Custom(inner) => inner.get_file_pointer(),
    }
  }

  fn seek(&mut self, pos: usize) -> Result<()> {
    match self {
      IndexInputEnum::Fs(inner) => inner.seek(pos),
      IndexInputEnum::Custom(inner) => inner.seek(pos),
    }
  }

  fn skip_bytes(&mut self, num_bytes: i64) -> Result<()> {
    match self {
      IndexInputEnum::Fs(inner) => IndexInput::skip_bytes(inner, num_bytes),
      IndexInputEnum::Custom(inner) => inner.skip_bytes(num_bytes),
    }
  }

  fn length(&self) -> Result<usize> {
    match self {
      IndexInputEnum::Fs(inner) => IndexInput::length(inner),
      IndexInputEnum::Custom(inner) => inner.length(),
    }
  }

  fn slice(
    &self,
    slice_description: &str,
    offset: usize,
    length: usize,
  ) -> Result<Self::IndexInput> {
    match self {
      IndexInputEnum::Fs(inner) => Ok(IndexInputEnum::Fs(inner.slice(
        slice_description,
        offset,
        length,
      )?)),
      IndexInputEnum::Custom(inner) => inner.slice(slice_description, offset, length),
    }
  }

  fn slice_with_read_advice(
    &self,
    description: &str,
    offset: usize,
    length: usize,
    _read_advice: &ReadAdvice,
  ) -> Result<Self::IndexInput> {
    match self {
      IndexInputEnum::Fs(inner) => Ok(IndexInputEnum::Fs(inner.slice_with_read_advice(
        description,
        offset,
        length,
        _read_advice,
      )?)),
      IndexInputEnum::Custom(inner) => {
        inner.slice_with_read_advice(description, offset, length, _read_advice)
      },
    }
  }

  type RandomAccessSlice = IndexInputEnumRandomAccessSlice;

  fn random_access_slice(&self, offset: usize, length: usize) -> Result<Self::RandomAccessSlice> {
    match self {
      IndexInputEnum::Fs(inner) => Ok(IndexInputEnumRandomAccessSlice::A(
        inner.random_access_slice(offset, length)?,
      )),
      IndexInputEnum::Custom(inner) => Ok(IndexInputEnumRandomAccessSlice::B(
        inner.random_access_slice(offset, length)?,
      )),
    }
  }

  fn prefetch(&mut self, pos: usize, len: usize) -> Result<()> {
    match self {
      IndexInputEnum::Fs(inner) => IndexInput::prefetch(inner, len, pos),
      IndexInputEnum::Custom(inner) => inner.prefetch(pos, len),
    }
  }

  fn update_read_advice(&self, read_advice: ReadAdvice) -> Result<()> {
    match self {
      IndexInputEnum::Fs(inner) => inner.update_read_advice(read_advice),
      IndexInputEnum::Custom(inner) => inner.update_read_advice(read_advice),
    }
  }

  fn is_loaded(&self) -> Result<Option<bool>> {
    match self {
      IndexInputEnum::Fs(inner) => IndexInput::is_loaded(inner),
      IndexInputEnum::Custom(inner) => IndexInput::is_loaded(inner.as_ref()),
    }
  }
}

/// Implementations call this to build the resource description for a slice of
/// this `IndexInput`.
pub fn get_full_slice_description(slice_description: &str) -> String {
  format!(" [slice={slice_description}] ")
}

macro_rules! either_index_input {
    (
        $vis:vis $name:ident<$( $G:ident ),+>,
        index_input = $index_input:ty,
        random_access = $random_access_type:ty,
        random_access_enum = $random_access:ident
        { $( $Variant:ident : $T:ty ),+ $(,)? }
    ) => {
        either_index_input!(@impl
            $vis $name [<$( $G ),+>],
            index_input = $index_input,
            random_access = $random_access_type,
            random_access_enum = $random_access
            { $( $Variant : $T ),+ }
        );
    };
    (
        $vis:vis $name:ident,
        index_input = $index_input:ty,
        random_access = $random_access_type:ty,
        random_access_enum = $random_access:ident
        { $( $Variant:ident : $T:ty ),+ $(,)? }
    ) => {
        either_index_input!(@impl
            $vis $name [],
            index_input = $index_input,
            random_access = $random_access_type,
            random_access_enum = $random_access
            { $( $Variant : $T ),+ }
        );
    };
    (@impl
        $vis:vis $name:ident [$($generics:tt)*],
        index_input = $index_input:ty,
        random_access = $random_access_type:ty,
        random_access_enum = $random_access:ident
        { $( $Variant:ident : $T:ty ),+ $(,)? }
    ) => {
        $vis enum $name $($generics)* {
            $( $Variant($T), )+
        }

        impl $($generics)* CloseableRef for $name $($generics)*
        where
            $( $T: CloseableRef ),+
        {
            fn close(&self) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.close(), )+
                }
            }
        }

        impl $($generics)* DataInput for $name $($generics)*
        where
            $( $T: DataInput ),+
        {
            fn read_byte(&mut self) -> Result<u8> {
                match self {
                    $( Self::$Variant(inner) => DataInput::read_byte(inner), )+
                }
            }

            fn read_bytes(&mut self, b: &mut [u8], offset: usize, len: usize) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => DataInput::read_bytes(inner, b, offset, len), )+
                }
            }

            fn read_bytes_with_buffer(
                &mut self,
                b: &mut [u8],
                offset: usize,
                len: usize,
                _use_buffer: bool,
            ) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.read_bytes_with_buffer(
                        b,
                        offset,
                        len,
                        _use_buffer
                    ), )+
                }
            }

            fn read_short(&mut self) -> Result<i16> {
                match self {
                    $( Self::$Variant(inner) => DataInput::read_short(inner), )+
                }
            }



            fn read_int(&mut self) -> Result<i32> {
                match self {
                    $( Self::$Variant(inner) => DataInput::read_int(inner), )+
                }
            }



            fn read_group_vint(&mut self, dst: &mut [i32], offset: usize) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.read_group_vint(dst, offset), )+
                }
            }



            fn read_vint(&mut self) -> Result<i32> {
                match self {
                    $( Self::$Variant(inner) => inner.read_vint(), )+
                }
            }

            fn read_zint(&mut self) -> Result<i32> {
                match self {
                    $( Self::$Variant(inner) => inner.read_zint(), )+
                }
            }

            fn read_long(&mut self) -> Result<i64> {
                match self {
                    $( Self::$Variant(inner) => DataInput::read_long(inner), )+
                }
            }



            fn read_longs(&mut self, dst: &mut [i64], offset: usize, len: usize) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.read_longs(dst, offset, len), )+
                }
            }

            fn read_ints(&mut self, dst: &mut [i32], offset: usize, len: usize) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.read_ints(dst, offset, len), )+
                }
            }

            fn read_floats(&mut self, dst: &mut [f32], offset: usize, len: usize) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.read_floats(dst, offset, len), )+
                }
            }

            fn read_vlong(&mut self) -> Result<i64> {
                match self {
                    $( Self::$Variant(inner) => inner.read_vlong(), )+
                }
            }

            fn read_zlong(&mut self) -> Result<i64> {
                match self {
                    $( Self::$Variant(inner) => inner.read_zlong(), )+
                }
            }

            fn read_string(&mut self) -> Result<String> {
                match self {
                    $( Self::$Variant(inner) => inner.read_string(), )+
                }
            }

            fn read_map_of_strings(&mut self) -> Result<HashMap<String, String>> {
                match self {
                    $( Self::$Variant(inner) => inner.read_map_of_strings(), )+
                }
            }

            fn read_set_of_strings(&mut self) -> Result<HashSet<String>> {
                match self {
                    $( Self::$Variant(inner) => inner.read_set_of_strings(), )+
                }
            }

            fn skip_bytes(&mut self, num_bytes: i64) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => DataInput::skip_bytes(inner, num_bytes), )+
                }
            }

            fn is_index_input(&self) -> bool {
                match self {
                    $( Self::$Variant(inner) => inner.is_index_input(), )+
                }
            }

            fn seek_in_data_input(&mut self, _pos: usize) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.seek_in_data_input(_pos), )+
                }
            }

            fn get_file_pointer_in_data_input(&self) -> Result<usize>{
                match self {
                    $( Self::$Variant(inner) => inner.get_file_pointer_in_data_input(), )+
                }
            }
        }

        impl $($generics)* Display for $name $($generics)*
        where
            $( $T: Display ),+
        {
            fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
                match self {
                    $( Self::$Variant(inner) => inner.fmt(f), )+
                }
            }
        }

        impl $($generics)* TryClone for $name $($generics)*
        where
            $( $T: TryClone ),+
        {
            fn try_clone(&self) -> Result<Self>
            where
                Self: Sized,
            {
                match self {
                    $( Self::$Variant(inner) => Ok(Self::$Variant(inner.try_clone()?)), )+
                }
            }
        }

        impl $($generics)* RandomAccessInput for $name $($generics)*
        where
            $( $T: RandomAccessInput ),+
        {
            fn length(&self) -> Result<usize> {
                match self {
                    $( Self::$Variant(inner) => RandomAccessInput::length(inner), )+
                }
            }

            fn read_byte(&mut self, pos: usize) -> Result<u8> {
                match self {
                    $( Self::$Variant(inner) => RandomAccessInput::read_byte(inner, pos), )+
                }
            }

            fn read_bytes(
                &mut self,
                pos: usize,
                buf: &mut [u8],
                offset: usize,
                len: usize,
            ) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => RandomAccessInput::read_bytes(inner, pos, buf, offset, len), )+
                }
            }

            fn read_short(&mut self, pos: usize) -> Result<i16> {
                match self {
                    $( Self::$Variant(inner) => RandomAccessInput::read_short(inner, pos), )+
                }
            }

            fn read_int(&mut self, pos: usize) -> Result<i32> {
                match self {
                    $( Self::$Variant(inner) => RandomAccessInput::read_int(inner, pos), )+
                }
            }

            fn read_long(&mut self, pos: usize) -> Result<i64> {
                match self {
                    $( Self::$Variant(inner) => RandomAccessInput::read_long(inner, pos), )+
                }
            }

            fn prefetch(&mut self, pos: usize, len: usize) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => RandomAccessInput::prefetch(inner, pos, len), )+
                }
            }
        }

        impl $($generics)* IndexInput for $name $($generics)*
        where
            $( $T: IndexInput ),+
        {
            type IndexInput = $index_input;

            fn get_file_pointer(&self) -> Result<usize>{
                match self {
                    $( Self::$Variant(inner) => inner.get_file_pointer(), )+
                }
            }

            fn seek(&mut self, pos: usize) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.seek(pos), )+
                }
            }

            fn skip_bytes(&mut self, num_bytes: i64) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => IndexInput::skip_bytes(inner, num_bytes), )+
                }
            }

            fn length(&self) -> Result<usize> {
                match self {
                    $( Self::$Variant(inner) => IndexInput::length(inner), )+
                }
            }

            fn slice(
                &self,
                slice_description: &str,
                offset: usize,
                length: usize,
            ) -> Result<Self::IndexInput> {
                match self {
                    $( Self::$Variant(inner) => Ok($name::$Variant(
                        inner.slice(slice_description, offset, length)?,
                    )), )+
                }
            }

            fn slice_with_read_advice(
                &self,
                description: &str,
                offset: usize,
                length: usize,
                read_advice: &ReadAdvice,
            ) -> Result<Self::IndexInput> {
                match self {
                    $( Self::$Variant(inner) => Ok($name::$Variant(inner.slice_with_read_advice(
                        description,
                        offset,
                        length,
                        read_advice,
                    )?)), )+
                }
            }

            type RandomAccessSlice = $random_access_type;

            fn random_access_slice(&self, offset: usize, length: usize) -> Result<Self::RandomAccessSlice> {
                match self {
                    $( Self::$Variant(inner) => Ok($random_access::$Variant(
                        inner.random_access_slice(offset, length)?,
                    )), )+
                }
            }

            fn prefetch(&mut self, pos: usize, len: usize) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => IndexInput::prefetch(inner, pos, len), )+
                }
            }

            fn update_read_advice(&self, read_advice: ReadAdvice) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.update_read_advice(read_advice), )+
                }
            }

            fn is_loaded(&self) -> Result<Option<bool>> {
                match self {
                    $( Self::$Variant(inner) => IndexInput::is_loaded(inner), )+
                }
            }

        }
    };
}

impl<I> DataInput for Arc<I>
where
  I: IndexInput,
{
  fn read_byte(&mut self) -> Result<u8> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn read_bytes(&mut self, _b: &mut [u8], _offset: usize, _len: usize) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn read_bytes_with_buffer(
    &mut self,
    _b: &mut [u8],
    _offset: usize,
    _len: usize,
    _use_buffer: bool,
  ) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn read_short(&mut self) -> Result<i16> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn read_int(&mut self) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn read_group_vint(&mut self, _dst: &mut [i32], _offset: usize) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn read_vint(&mut self) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn read_zint(&mut self) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn read_long(&mut self) -> Result<i64> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn read_longs(&mut self, _dst: &mut [i64], _offset: usize, _len: usize) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn read_ints(&mut self, _dst: &mut [i32], _offset: usize, _len: usize) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn read_floats(&mut self, _dst: &mut [f32], _offset: usize, _len: usize) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn read_vlong(&mut self) -> Result<i64> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn read_zlong(&mut self) -> Result<i64> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn read_string(&mut self) -> Result<String> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn read_map_of_strings(&mut self) -> Result<HashMap<String, String>> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn read_set_of_strings(&mut self) -> Result<HashSet<String>> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn skip_bytes(&mut self, _num_bytes: i64) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn is_index_input(&self) -> bool {
    (**self).is_index_input()
  }

  fn seek_in_data_input(&mut self, _pos: usize) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_file_pointer_in_data_input(&self) -> Result<usize> {
    (**self).get_file_pointer_in_data_input()
  }
}

impl<I> TryClone for Arc<I>
where
  I: IndexInput,
{
  fn try_clone(&self) -> Result<Self>
  where
    Self: Sized,
  {
    Ok(Arc::clone(self))
  }
}

impl<I> IndexInput for Arc<I>
where
  I: IndexInput,
{
  type IndexInput = I::IndexInput;

  fn get_file_pointer(&self) -> Result<usize> {
    (**self).get_file_pointer()
  }

  fn seek(&mut self, _pos: usize) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn skip_bytes(&mut self, _num_bytes: i64) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn length(&self) -> Result<usize> {
    (**self).length()
  }

  fn slice(
    &self,
    _slice_description: &str,
    _offset: usize,
    _length: usize,
  ) -> Result<Self::IndexInput> {
    (**self).slice(_slice_description, _offset, _length)
  }

  fn slice_with_read_advice(
    &self,
    description: &str,
    offset: usize,
    length: usize,
    _read_advice: &ReadAdvice,
  ) -> Result<Self::IndexInput> {
    self
      .as_ref()
      .slice_with_read_advice(description, offset, length, _read_advice)
  }

  type RandomAccessSlice = I::RandomAccessSlice;

  fn random_access_slice(&self, offset: usize, length: usize) -> Result<Self::RandomAccessSlice> {
    (**self).random_access_slice(offset, length)
  }

  fn prefetch(&mut self, _pos: usize, _len: usize) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn update_read_advice(&self, read_advice: ReadAdvice) -> Result<()> {
    self.as_ref().update_read_advice(read_advice)
  }

  fn is_loaded(&self) -> Result<Option<bool>> {
    IndexInput::is_loaded(self.as_ref())
  }
}
either_index_input!(
    pub IndexInputEnum2<A, B>,
    index_input = IndexInputEnum2<A::IndexInput, B::IndexInput>,
    random_access = RandomAccessInputEnum2<A::RandomAccessSlice, B::RandomAccessSlice>,
    random_access_enum = RandomAccessInputEnum2
    { A: A, B: B }
);
either_index_input!(
    pub IndexInputEnum3<A, B, C>,
    index_input = IndexInputEnum3<A::IndexInput, B::IndexInput, C::IndexInput>,
    random_access = RandomAccessInputEnum3<
        A::RandomAccessSlice,
        B::RandomAccessSlice,
        C::RandomAccessSlice,
    >,
    random_access_enum = RandomAccessInputEnum3
    { A: A, B: B, C: C }
);
either_index_input!(
    pub NRTCachingIndexInput<I>,
    index_input = NRTCachingIndexInput<I::IndexInput>,
    random_access = RandomAccessInputEnum2<
        <ByteBuffersIndexInputOwned as IndexInput>::RandomAccessSlice,
        I::RandomAccessSlice,
    >,
    random_access_enum = RandomAccessInputEnum2
    { A: ByteBuffersIndexInputOwned, B: I }
);
either_index_input!(
    pub BuiltInFSIndexInput,
    index_input = BuiltInFSIndexInput,
    random_access = RandomAccessInputEnum2<
        MemorySegmentIndexInput,
        BufferedIndexInput<NIOFSIndexInput>,
    >,
    random_access_enum = RandomAccessInputEnum2
    {
        A: MemorySegmentIndexInput,
        B: BufferedIndexInput<NIOFSIndexInput>,
    }
);
