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
use crate::core::store::IndexInput;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use parking_lot::Mutex;
use std::borrow::Cow;
use std::fmt::Display;

/// Random Access Index API. Unlike [`IndexInput`],
/// this has no concept of file position; all reads are absolute.
/// Implementations may keep internal caches. Sharing across threads requires
/// an implementation that is also `Sync`.
pub trait RandomAccessInput {
  /// The number of bytes in the file.
  fn length(&self) -> Result<usize>;
  /// Reads a byte at the given position in the file
  fn read_byte(&self, pos: usize) -> Result<u8>;
  /// Returns bytes borrowed from the input or owned by the result.
  /// Borrowed bytes must remain valid and unchanged for the lifetime of the result,
  /// including across other shared reads. Mutable caches return owned snapshots.
  fn read_bytes(&self, pos: usize, len: usize) -> Result<Cow<'_, [u8]>> {
    let end = pos
      .checked_add(len)
      .ok_or_else(|| LuceneError::eof(format!("read past EOF at {pos} length {len}")))?;
    if end > self.length()? {
      return Err(LuceneError::eof(format!(
        "read past EOF at {pos} length {len}"
      )));
    }
    let mut bytes = Vec::with_capacity(len);
    for i in 0..len {
      bytes.push(self.read_byte(pos + i)?);
    }
    Ok(Cow::Owned(bytes))
  }
  /// Reads an `i16` (little-endian byte order) at the given file position.
  fn read_short(&self, pos: usize) -> Result<i16>;
  /// Reads an `i32` (little-endian byte order) at the given file position.
  fn read_int(&self, pos: usize) -> Result<i32>;
  /// Reads an `i64` (little-endian byte order) at the given file position.
  fn read_long(&self, pos: usize) -> Result<i64>;
  ///  Prefetch data in the background.
  fn prefetch(&self, pos: usize, len: usize) -> Result<()>;

  /// Returns a hint whether all the contents of this input are resident in physical memory.
  ///
  /// See [`IndexInput::is_loaded`].
  fn is_loaded(&self) -> Result<Option<bool>> {
    Ok(None)
  }
}

pub struct RandomAccessInputWrapper<I> {
  slice: Mutex<I>,
}

impl<I> RandomAccessInputWrapper<I> {
  pub fn new(inner: I) -> Self {
    Self {
      slice: Mutex::new(inner),
    }
  }
}

impl<I> RandomAccessInput for RandomAccessInputWrapper<I>
where
  I: IndexInput,
{
  fn length(&self) -> Result<usize> {
    self.slice.lock().length()
  }

  fn read_byte(&self, pos: usize) -> Result<u8> {
    let mut slice = self.slice.lock();
    slice.seek(pos)?;
    slice.read_byte()
  }

  fn read_bytes(&self, pos: usize, len: usize) -> Result<Cow<'_, [u8]>> {
    let mut slice = self.slice.lock();
    slice.seek(pos)?;
    let mut bytes = vec![0; len];
    slice.read_bytes(&mut bytes, 0, len)?;
    Ok(Cow::Owned(bytes))
  }

  fn read_short(&self, pos: usize) -> Result<i16> {
    let mut slice = self.slice.lock();
    slice.seek(pos)?;
    slice.read_short()
  }

  fn read_int(&self, pos: usize) -> Result<i32> {
    let mut slice = self.slice.lock();
    slice.seek(pos)?;
    slice.read_int()
  }

  fn read_long(&self, pos: usize) -> Result<i64> {
    let mut slice = self.slice.lock();
    slice.seek(pos)?;
    slice.read_long()
  }

  fn prefetch(&self, pos: usize, len: usize) -> Result<()> {
    self.slice.lock().prefetch(pos, len)
  }

  fn is_loaded(&self) -> Result<Option<bool>> {
    IndexInput::is_loaded(&*self.slice.lock())
  }
}
impl<I> Display for RandomAccessInputWrapper<I>
where
  I: Display,
{
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "RandomAccessInput({})", *self.slice.lock())
  }
}

pub type DynRandomAccessInput = dyn RandomAccessInput + Send + Sync;
pub type BoxRandomAccessInput = Box<DynRandomAccessInput>;

macro_rules! either_random_access_input {
    ($vis:vis $name:ident { $( $Variant:ident : $T:ident ),+ $(,)? }) => {
        $vis enum $name<$( $T ),+> {
            $( $Variant($T), )+
        }

        impl<$( $T ),+> RandomAccessInput for $name<$( $T ),+>
        where
            $( $T: RandomAccessInput ),+
        {
            fn length(&self) -> Result<usize>{
                match self {
                    $( Self::$Variant(inner) => inner.length(), )+
                }
            }

            fn read_byte(&self, pos: usize) -> Result<u8> {
                match self {
                    $( Self::$Variant(inner) => inner.read_byte(pos), )+
                }
            }

            fn read_bytes(&self, pos: usize, len: usize) -> Result<Cow<'_, [u8]>> {
                match self {
                    $( Self::$Variant(inner) => inner.read_bytes(pos, len), )+
                }
            }

            fn read_short(&self, pos: usize) -> Result<i16> {
                match self {
                    $( Self::$Variant(inner) => inner.read_short(pos), )+
                }
            }

            fn read_int(&self, pos: usize) -> Result<i32> {
                match self {
                    $( Self::$Variant(inner) => inner.read_int(pos), )+
                }
            }

            fn read_long(&self, pos: usize) -> Result<i64> {
                match self {
                    $( Self::$Variant(inner) => inner.read_long(pos), )+
                }
            }

            fn prefetch(&self, pos: usize, len: usize) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.prefetch(pos, len), )+
                }
            }

            fn is_loaded(&self) -> Result<Option<bool>> {
                match self {
                    $( Self::$Variant(inner) => RandomAccessInput::is_loaded(inner), )+
                }
            }
        }
    };
}
either_random_access_input!(pub RandomAccessInputEnum2 { A: A, B: B });
either_random_access_input!(pub RandomAccessInputEnum3 { A: A, B: B, C: C });
impl<T: ?Sized + RandomAccessInput> RandomAccessInput for Box<T> {
  fn length(&self) -> Result<usize> {
    (**self).length()
  }

  fn read_byte(&self, pos: usize) -> Result<u8> {
    (**self).read_byte(pos)
  }

  fn read_bytes(&self, pos: usize, len: usize) -> Result<Cow<'_, [u8]>> {
    (**self).read_bytes(pos, len)
  }

  fn read_short(&self, pos: usize) -> Result<i16> {
    (**self).read_short(pos)
  }

  fn read_int(&self, pos: usize) -> Result<i32> {
    (**self).read_int(pos)
  }

  fn read_long(&self, pos: usize) -> Result<i64> {
    (**self).read_long(pos)
  }

  fn prefetch(&self, pos: usize, len: usize) -> Result<()> {
    (**self).prefetch(pos, len)
  }

  fn is_loaded(&self) -> Result<Option<bool>> {
    RandomAccessInput::is_loaded(&**self)
  }
}
