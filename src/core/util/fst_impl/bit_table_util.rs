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
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::fst_impl::fst::BytesReader;
/// Static helper methods for `FST::Arc::BitTable`.
///
/// # Experimental
pub(crate) struct BitTableUtil;
impl BitTableUtil {
    /// Returns whether the bit at the given zero-based index is set.
    ///
    /// # Example
    /// A `bit_index` of 10 refers to the third bit on the right of the second
    /// byte.
    ///
    /// # Parameters
    /// - `bit_index`: The zero-based index of the bit. It must be greater than
    ///   or equal to 0 and strictly less than `number of bit-table bytes *
    ///   Byte::SIZE`.
    /// - `reader`: The [`FST::BytesReader`](BytesReader) used for reading. It
    ///   must be positioned at the beginning of the bit-table.
    pub fn is_bit_set(bit_index: i32, reader: &mut impl BytesReader) -> Result<bool> {
        debug_assert!(bit_index >= 0, "bitIndex={bit_index}");
        reader.skip_bytes((bit_index >> 3) as i64)?;
        let b = Self::read_byte(reader)?;
        let mask = 1u64 << (bit_index as u32 & (u8::BITS - 1));
        Ok((b & mask) != 0)
    }
    /// Counts all bits set in the bit-table.
    ///
    /// # Parameters
    /// - `bit_table_bytes`: The number of bytes in the bit-table.
    /// - `reader`: The [`FST::BytesReader`](BytesReader) used for reading. It
    ///   must be positioned at the beginning of the bit-table.
    pub fn count_bits(bit_table_bytes: i32, reader: &mut impl BytesReader) -> Result<i32> {
        debug_assert!(bit_table_bytes >= 0, "bitTableBytes={bit_table_bytes}");
        let mut bit_count = 0;
        let num_long_blocks = bit_table_bytes >> 3;
        for _ in 0..num_long_blocks {
            bit_count += Self::bit_count_8_bytes(reader)?;
        }
        let num_remaining_bytes = bit_table_bytes & (BitUtil::LONG_BYTES - 1) as i32;
        if num_remaining_bytes != 0 {
            bit_count += Self::read_upto_8_bytes(num_remaining_bytes, reader)?.count_ones() as i32;
        }
        Ok(bit_count)
    }
    /// Counts the bits set up to the given bit zero-based index, exclusive.
    ///
    /// In other words, counts how many `1`s there are up to (but excluding) the
    /// given `bit_index`.
    ///
    /// # Example
    /// A `bit_index` of 10 refers to the third bit on the right of the second
    /// byte.
    ///
    /// # Parameters
    /// - `bit_index`: The zero-based index, exclusive. It must be greater than
    ///   or equal to 0 and less than or equal to `number of bit-table bytes *
    ///   Byte::SIZE`.
    /// - `reader`: The [`FST::BytesReader`](BytesReader) used for reading. It
    ///   must be positioned at the beginning of the bit-table.
    pub fn count_bits_upto(bit_index: i32, reader: &mut impl BytesReader) -> Result<i32> {
        debug_assert!(bit_index >= 0, "bitIndex={bit_index}");
        let mut bit_count = 0;
        let num_long_blocks = bit_index >> 6;
        for _ in 0..num_long_blocks {
            // Count the bits set for all plain longs.
            bit_count += Self::bit_count_8_bytes(reader)?;
        }
        let remaining_bits = bit_index & (i64::BITS - 1) as i32;
        if remaining_bits != 0 {
            let num_remaining_bytes = (remaining_bits + (i8::BITS - 1) as i32) >> 3;
            // Prepare a mask with 1s on the right up to bitIndex exclusive.
            let mask = (1u64 << bit_index) - 1; // Shifts are mod 64.
            // Count the bits set only within the mask part, so up to bitIndex
            // exclusive.
            let l = Self::read_upto_8_bytes(num_remaining_bytes, reader)?;
            bit_count += (l & mask).count_ones() as i32;
        }
        Ok(bit_count)
    }
    /// Returns the index of the next set bit following the given zero-based
    /// index.
    ///
    /// # Example
    /// Given the bit sequence `100011`:
    /// - The next set bit after `index = -1` is at `index = 0`.
    /// - The next set bit after `index = 0` is at `index = 1`.
    /// - The next set bit after `index = 1` is at `index = 5`.
    /// - There is no next set bit after `index = 5`.
    ///
    /// # Parameters
    /// - `bit_index`: The zero-based index of the bit. It must be greater than
    ///   or equal to -1 and strictly less than `number of bit-table bytes *
    ///   Byte::SIZE`.
    /// - `bit_table_bytes`: The number of bytes in the bit-table.
    /// - `reader`: The [`FST::BytesReader`](BytesReader) used for reading. It
    ///   must be positioned at the beginning of the bit-table.
    ///
    /// # Returns
    /// The zero-based index of the next set bit after `bit_index`, or `-1` if
    /// none exist.
    pub fn next_bit_set(
        bit_index: i32,
        bit_table_bytes: i32,
        reader: &mut impl BytesReader,
    ) -> Result<i32> {
        debug_assert!(
            bit_index >= -1 && bit_index < bit_table_bytes * i8::BITS as i32,
            "bitIndex={bit_index} bitTableBytes={bit_table_bytes}"
        );
        let mut byte_index = bit_index / i8::BITS as i32;
        let mask: i32 = -1 << ((bit_index + 1) & (i8::BITS as i32 - 1));
        let mut i: i32;
        if mask == -1 && bit_index != -1 {
            reader.skip_bytes((byte_index + 1) as i64)?;
            i = 0;
        } else {
            reader.skip_bytes(byte_index as i64)?;
            i = (reader.read_byte()? as i32 & 0xFF) & mask;
        }
        while i == 0 {
            byte_index += 1;
            if byte_index == bit_table_bytes {
                return Ok(-1);
            }
            i = reader.read_byte()? as i32 & 0xFF;
        }
        Ok(i.trailing_zeros() as i32 + (byte_index << 3))
    }
    /// Returns the index of the previous set bit preceding the given zero-based
    /// index.
    ///
    /// # Example
    /// Given the bit sequence `100011`:
    /// - There is no previous set bit before `index = 0`.
    /// - The previous set bit before `index = 1` is at `index = 0`.
    /// - The previous set bit before `index = 5` is at `index = 1`.
    /// - The previous set bit before `index = 64` is at `index = 5`.
    ///
    /// # Parameters
    /// - `bit_index`: The zero-based index of the bit. It must be greater than
    ///   or equal to 0 and less than or equal to `number of bit-table bytes *
    ///   Byte::SIZE`.
    /// - `reader`: The [`FST::BytesReader`](BytesReader) used for reading. It
    ///   must be positioned at the beginning of the bit-table.
    ///
    /// # Returns
    /// The zero-based index of the previous set bit before `bit_index`, or `-1`
    /// if none exist.
    pub fn previous_bit_set(bit_index: i32, reader: &mut impl BytesReader) -> Result<i32> {
        debug_assert!(bit_index >= 0, "bitIndex={bit_index}");
        let mut byte_index = bit_index >> 3;
        reader.skip_bytes(byte_index as i64)?;
        let mask: i32 = (1 << (bit_index & (i8::BITS - 1) as i32)) - 1;
        let mut i = reader.read_byte()? as i32 & 0xFF;
        i &= mask;
        while i == 0 {
            if byte_index == 0 {
                return Ok(-1);
            }
            byte_index -= 1;
            reader.skip_bytes(-2)?;
            i = reader.read_byte()? as i32 & 0xFF;
        }
        Ok(((i32::BITS - 1) as i32 - i.leading_zeros() as i32) + (byte_index << 3))
    }

    fn read_byte(reader: &mut impl BytesReader) -> Result<u64> {
        let b = reader.read_byte()?;
        Ok((b as u64) & 0xFF)
    }

    fn read_upto_8_bytes(num_bytes: i32, reader: &mut impl BytesReader) -> Result<u64> {
        debug_assert!(num_bytes > 0 && num_bytes <= 8, "numBytes={num_bytes}");
        let mut l = Self::read_byte(reader)?;
        let mut shift = 0;
        let mut remaining = num_bytes - 1;
        while remaining != 0 {
            shift += 8;
            l |= Self::read_byte(reader)? << shift;
            remaining -= 1;
        }
        Ok(l)
    }

    fn bit_count_8_bytes(reader: &mut impl BytesReader) -> Result<i32> {
        let l = reader.read_long()?;
        Ok(l.count_ones() as i32)
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::{Display, Formatter};

    use rand::Rng;

    use crate::core::store::DataInput;
    use crate::core::util::error::lucene_error::{LuceneError, Result};
    use crate::core::util::fst_impl::bit_table_util::BitTableUtil;
    use crate::core::util::fst_impl::fst::BytesReader;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{at_least, random};

    #[test]
    fn test_next_bit_set() -> Result<()> {
        let mut random = random();
        let num_iterations = at_least(&mut random, 1000);

        for i in 0..num_iterations {
            let bits = build_random_bits(&mut random);
            assert!(bits.len() <= i32::MAX as usize);
            let num_bytes = (bits.len() - 1) as i32;
            let num_bits = num_bytes * i8::BITS as i32;

            // Verify next_bit_set with count_bits_upto for all bit indexes.
            for bit_index in -1..num_bits {
                let next_index =
                    BitTableUtil::next_bit_set(bit_index, num_bytes, &mut reader(&bits))?;

                if next_index == -1 {
                    assert_eq!(
                        BitTableUtil::count_bits_upto(bit_index + 1, &mut reader(&bits))?,
                        BitTableUtil::count_bits(num_bytes, &mut reader(&bits))?,
                        "No next bit set, so expected no bit count diff (i={} bitIndex={})",
                        i,
                        bit_index
                    );
                } else {
                    assert!(
                        BitTableUtil::is_bit_set(next_index, &mut reader(&bits))?,
                        "Expected next bit set at next_index={} (i={} bitIndex={})",
                        next_index,
                        i,
                        bit_index
                    );

                    assert_eq!(
                        BitTableUtil::count_bits_upto(bit_index + 1, &mut reader(&bits))? + 1,
                        BitTableUtil::count_bits_upto(next_index + 1, &mut reader(&bits))?,
                        "Next bit set at next_index={} so expected bit count diff of 1 (i={} bitIndex={})",
                        next_index,
                        i,
                        bit_index
                    );
                }
            }
        }

        Ok(())
    }
    #[test]
    fn test_previous_bit_set() -> Result<()> {
        let mut random = random();
        let num_iterations = at_least(&mut random, 1000);

        for i in 0..num_iterations {
            let bits = build_random_bits(&mut random);
            assert!(bits.len() <= i32::MAX as usize);
            let num_bytes = (bits.len() - 1) as i32;
            let num_bits = num_bytes * i8::BITS as i32;

            // Verify previous_bit_set with count_bits_upto for all bit
            // indexes.
            for bit_index in 0..=num_bits {
                let previous_index = BitTableUtil::previous_bit_set(bit_index, &mut reader(&bits))?;

                if previous_index == -1 {
                    assert_eq!(
                        0,
                        BitTableUtil::count_bits_upto(bit_index, &mut reader(&bits))?,
                        "No previous bit set, so expected bit count 0 (i={} bitIndex={})",
                        i,
                        bit_index
                    );
                } else {
                    assert!(
                        BitTableUtil::is_bit_set(previous_index, &mut reader(&bits))?,
                        "Expected previous bit set at previous_index={} (i={} bitIndex={})",
                        previous_index,
                        i,
                        bit_index
                    );

                    let bit_count = BitTableUtil::count_bits_upto(
                        bit_index.saturating_add(1).min(num_bits),
                        &mut reader(&bits),
                    )?;
                    let expected_previous_bit_count = if bit_index < num_bits
                        && BitTableUtil::is_bit_set(bit_index, &mut reader(&bits))?
                    {
                        bit_count - 1
                    } else {
                        bit_count
                    };

                    assert_eq!(
                        expected_previous_bit_count,
                        BitTableUtil::count_bits_upto(previous_index + 1, &mut reader(&bits))?,
                        "Previous bit set at previous_index={} with current bitCount={} so expected previousBitCount={} (i={} bitIndex={})",
                        previous_index,
                        bit_count,
                        expected_previous_bit_count,
                        i,
                        bit_index
                    );
                }
            }
        }

        Ok(())
    }

    fn build_random_bits<R: Rng + ?Sized>(random: &mut R) -> Vec<u8> {
        let len = random.random_range(2..26);
        let mut bits = vec![0; len];

        for byte in &mut bits {
            // Bias towards zeros which require special logic.
            *byte = if random.random_range(0..4) == 0 {
                0
            } else {
                random.random()
            };
        }

        bits
    }

    /// Creates a `BytesReader` for the given byte slice.
    fn reader(bits: &[u8]) -> BytesReaderImpl<'_> {
        BytesReaderImpl::new(bits)
    }

    struct BytesReaderImpl<'a> {
        bits: &'a [u8],
        position: i32,
    }
    impl<'a> BytesReaderImpl<'a> {
        fn new(bits: &'a [u8]) -> Self {
            Self { bits, position: 0 }
        }
    }

    impl DataInput for BytesReaderImpl<'_> {
        fn read_byte(&mut self) -> Result<u8> {
            let v = self.bits[self.position as usize];
            self.position += 1;
            Ok(v)
        }

        fn read_bytes(&mut self, _b: &mut [u8], _offset: i32, _len: i32) -> Result<()> {
            Err(LuceneError::unsupported_operation("Not implemented"))
        }

        fn skip_bytes(&mut self, num_bytes: i64) -> Result<()> {
            self.position += num_bytes as i32;
            Ok(())
        }
    }

    impl Display for BytesReaderImpl<'_> {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", std::any::type_name::<Self>())
        }
    }

    impl BytesReader for BytesReaderImpl<'_> {
        fn get_position(&self) -> i64 {
            self.position as i64
        }
        fn set_position(&mut self, pos: i64) {
            self.position = pos as i32;
        }
    }
}
