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
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};

use crate::core::store::DataInput;
use crate::core::store::data_output::DataOutput;
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::{LuceneError, Result};

/// A `DataOutput` for appending data to a file in a `Directory`.
///
/// # Note
/// Instances of this struct are **not** thread-safe.
///
/// # See Also
/// [`Directory`](crate::core::store::directory::Directory)
///
/// [`IndexInput`](crate::core::store::index_input::IndexInput)
pub trait IndexOutput: DataOutput + Display + Closeable {
    /// Returns the current position in this file, where the next write will
    /// occur.
    fn get_file_pointer(&self) -> usize;
    /// Returns the current checksum of bytes written so far.
    fn get_checksum(&mut self) -> u64;
    /// Returns the name used to create this `IndexOutput`. This is especially
    /// useful when using
    /// [`Directory::create_temp_output`](crate::core::store::directory::Directory::create_temp_output).
    fn get_name(&self) -> &str;
    /// Aligns the current file pointer to multiples of `alignment_bytes` bytes
    /// to improve reads with mmap. This will write between 0 and
    /// `(alignment_bytes - 1)` zero bytes using
    /// [`write_byte`](DataOutput::write_byte).
    ///
    /// # Arguments
    /// * `alignment_bytes` - The alignment to which it should forward the file
    ///   pointer (must be a power of 2).
    ///
    /// # Returns
    /// The new file pointer after alignment.
    ///
    /// # See Also
    /// [`align_offset`]
    fn align_file_pointer(&mut self, alignment_bytes: usize) -> Result<usize> {
        let offset = self.get_file_pointer();
        let aligned_offset = align_offset(offset, alignment_bytes)?;
        let count = (aligned_offset - offset) as usize;
        for _ in 0..count {
            self.write_byte(0)?;
        }
        Ok(aligned_offset)
    }
}
/// Aligns the given `offset` to multiples of `alignment_bytes` bytes by
/// rounding up. The alignment must be a power of 2.
///
/// # Arguments
/// * `offset` - The offset to be aligned.
/// * `alignment_bytes` - The alignment to which it should be rounded (must be a
///   power of 2).
pub fn align_offset(offset: usize, alignment_bytes: usize) -> Result<usize> {
    if alignment_bytes == 0 || alignment_bytes.count_ones() != 1 {
        return Err(LuceneError::illegal_argument(
            "Alignment must be a power of 2",
        ));
    }
    Ok((offset + alignment_bytes - 1) & !(alignment_bytes - 1))
}

macro_rules! either_index_output {
    ($vis:vis $name:ident { $( $Variant:ident : $T:ident ),+ $(,)? }) => {
        $vis enum $name<$( $T ),+> {
            $( $Variant($T), )+
        }

        impl<$( $T ),+> DataOutput for $name<$( $T ),+>
        where
            $( $T: IndexOutput ),+
        {
            fn write_byte(&mut self, b: u8) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.write_byte(b), )+
                }
            }

            fn write_bytes_with_len(&mut self, b: &[u8], len: usize) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.write_bytes_with_len(b, len), )+
                }
            }

            fn write_bytes_range(&mut self, b: &[u8], offset: usize, length: usize) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.write_bytes_range(b, offset, length), )+
                }
            }

            fn write_int(&mut self, i: i32) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.write_int(i), )+
                }
            }

            fn write_short(&mut self, i: i16) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.write_short(i), )+
                }
            }

            fn write_vint(&mut self, i: i32) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.write_vint(i), )+
                }
            }

            fn write_zint(&mut self, i: i32) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.write_zint(i), )+
                }
            }

            fn write_long(&mut self, i: i64) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.write_long(i), )+
                }
            }

            fn write_vlong(&mut self, i: i64) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.write_vlong(i), )+
                }
            }

            fn write_signed_vlong(&mut self, i: i64) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.write_signed_vlong(i), )+
                }
            }

            fn write_zlong(&mut self, i: i64) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.write_zlong(i), )+
                }
            }

            fn write_string(&mut self, s: &str) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.write_string(s), )+
                }
            }

            fn copy_bytes(&mut self, input: &mut impl DataInput, num_bytes: usize) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.copy_bytes(input, num_bytes), )+
                }
            }

            fn write_map_of_strings(&mut self, map: &HashMap<String, String>) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.write_map_of_strings(map), )+
                }
            }

            fn write_set_of_strings(&mut self, set: &HashSet<String>) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.write_set_of_strings(set), )+
                }
            }

        }

        impl<$( $T ),+> Display for $name<$( $T ),+>
        where
            $( $T: IndexOutput ),+
        {
            fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
                match self {
                    $( Self::$Variant(inner) => inner.fmt(f), )+
                }
            }
        }

        impl<$( $T ),+> Closeable for $name<$( $T ),+>
        where
            $( $T: IndexOutput ),+
        {
            fn close(&mut self) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.close(), )+
                }
            }
        }

        impl<$( $T ),+> IndexOutput for $name<$( $T ),+>
        where
            $( $T: IndexOutput ),+
        {
            fn get_file_pointer(&self) -> usize{
                match self {
                    $( Self::$Variant(inner) => inner.get_file_pointer(), )+
                }
            }

            fn get_checksum(&mut self) -> u64 {
                match self {
                    $( Self::$Variant(inner) => inner.get_checksum(), )+
                }
            }

            fn get_name(&self) -> &str {
                match self {
                    $( Self::$Variant(inner) => inner.get_name(), )+
                }
            }

            fn align_file_pointer(&mut self, alignment_bytes: usize) -> Result<usize> {
                match self {
                    $( Self::$Variant(inner) => inner.align_file_pointer(alignment_bytes), )+
                }
            }
        }
    };
}
either_index_output!(pub IndexOutputEnum2 { A: A, B: B });
either_index_output!(pub IndexOutputEnum3 { A: A, B: B, C: C });
