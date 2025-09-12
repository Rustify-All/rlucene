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
use crate::core::store::DataOutput;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::packed::Format::Packed;
use crate::core::util::packed::{FormatBehavior, PackedImpl, PackedInts};
/// Writer for packed integers that can be directly read from a
/// [`Directory`](crate::core::store::directory::Directory) via
/// [`DirectReader`](crate::core::util::packed::direct_reader::DirectReader).
///
/// Unlike `PackedInts`, this optimizes for read I/O operations and supports
/// values exceeding 2^31 (2 billion).
///
///
/// See also: [`DirectReader`](crate::core::util::packed::direct_reader::DirectReader)
pub struct DirectWriter<'a, D>
where
    D: DataOutput,
{
    bits_per_value: i32,
    num_values: i64,
    output: &'a mut D,
    count: i64,
    finished: bool,
    off: i32,
    next_blocks: Vec<u8>,
    next_values: Vec<i64>,
}
impl<'a, D> DirectWriter<'a, D>
where
    D: DataOutput,
{
    pub fn new(output: &'a mut D, num_values: i64, bits_per_value: i32) -> Result<Self> {
        let memory_budget_in_bits = i8::BITS as i32 * PackedInts::DEFAULT_BUFFER_SIZE;
        // For every value we need 64 bits for the value and bitsPerValue for
        // the encoded value
        let mut buffer_size = memory_budget_in_bits / (u64::BITS as i32 + bits_per_value);
        debug_assert!(buffer_size > 0);
        // Round to the next multiple of 64
        buffer_size = ((buffer_size + 63) as u32 & 0xFFFFFFC0) as i32;
        let next_values = vec![0i64; buffer_size as usize];
        let next_blocks_size =
            (buffer_size * bits_per_value) as usize / i8::BITS as usize + BitUtil::LONG_BYTES - 1;
        let next_blocks = vec![0u8; next_blocks_size];

        Ok(DirectWriter {
            bits_per_value,
            num_values,
            output,
            count: 0,
            finished: false,
            off: 0,
            next_blocks,
            next_values,
        })
    }
    /// Adds a value to this writer.
    pub fn add(&mut self, l: i64) -> Result<()> {
        debug_assert!(
            self.bits_per_value == 64
                || (l >= 0 && l <= PackedInts::max_value(self.bits_per_value)),
            "{}",
            self.bits_per_value
        );
        debug_assert!(!self.finished);
        if self.count >= self.num_values {
            return Err(LuceneError::eof("Writing past end of stream"));
        }
        self.next_values[self.off as usize] = l;
        self.off += 1;
        if self.off as usize == self.next_values.len() {
            self.flush()?;
        }
        self.count += 1;
        Ok(())
    }
    fn flush(&mut self) -> Result<()> {
        if self.off == 0 {
            return Ok(());
        }
        // Avoid writing bits from values that are outside of the range we need
        // to encode
        for i in self.off as usize..self.next_values.len() {
            self.next_values[i] = 0;
        }
        Self::encode(
            &self.next_values,
            self.off as usize,
            &mut self.next_blocks,
            self.bits_per_value,
        );
        let block_count = Packed(PackedImpl::new(0)).byte_count(
            PackedInts::VERSION_CURRENT,
            self.off,
            self.bits_per_value,
        ) as i32;
        self.output
            .write_bytes_with_len(&self.next_blocks, block_count)?;
        self.off = 0;
        Ok(())
    }
    fn encode(next_values: &[i64], upto: usize, next_blocks: &mut [u8], bits_per_value: i32) {
        if bits_per_value & 7 == 0 {
            // bitsPerValue is a multiple of 8: 8, 16, 24, 32, 30, 48, 56, 64
            let bytes_per_value = bits_per_value / i8::BITS as i32;
            let mut o = 0;
            for &l in next_values.iter().take(upto) {
                if bits_per_value > i32::BITS as i32 {
                    BitUtil::set_i64_le(next_blocks, o, l);
                } else if bits_per_value > i16::BITS as i32 {
                    BitUtil::set_i32_le(next_blocks, o, l as i32);
                } else if bits_per_value > i8::BITS as i32 {
                    BitUtil::set_i16_le(next_blocks, o, l as i16);
                } else {
                    next_blocks[o] = l as u8;
                }
                o += bytes_per_value as usize;
            }
        } else if bits_per_value < 8 {
            // bitsPerValue is 1, 2 or 4
            let values_per_long = (u64::BITS as i32 / bits_per_value) as usize;
            let mut i = 0;
            let mut o = 0;
            while i < upto {
                let mut v = 0;
                for j in 0..values_per_long {
                    v |= next_values[i + j] << (bits_per_value as i64 * j as i64);
                }
                BitUtil::set_i64_le(next_blocks, o, v);
                o += BitUtil::LONG_BYTES;
                i += values_per_long;
            }
        } else {
            // bitsPerValue is 12, 20 or 28
            // Write values 2 by 2
            let num_bytes_for_2_values = ((bits_per_value * 2) as u32 / i8::BITS) as usize;
            let mut i = 0;
            let mut o = 0;
            while i < upto {
                let l1 = next_values[i];
                let l2 = next_values[i + 1];
                let merged = l1 | (l2 << bits_per_value);
                if bits_per_value <= (32 / 2) {
                    BitUtil::set_i32_le(next_blocks, o, merged as i32);
                } else {
                    BitUtil::set_i64_le(next_blocks, o, merged);
                }
                o += num_bytes_for_2_values;
                i += 2;
            }
        }
    }
    /// finishes writing.
    pub fn finish(&mut self) -> Result<()> {
        if self.count != self.num_values {
            return Err(LuceneError::illegal_state(format!(
                "Wrong number of values added, expected: {}, got: {}",
                self.num_values, self.count
            )));
        }
        debug_assert!(!self.finished);
        self.flush()?;

        // add padding bytes for fast io
        // for every number of bits per value, we want to be able to read the
        // entire value in a single read e.g. for 20 bits per value, we
        // want to be able to read values using ints so we need
        // 32 - 20 = 12 bits of padding
        let padding_bits_needed = if self.bits_per_value > u32::BITS as i32 {
            u64::BITS as i32 - self.bits_per_value
        } else if self.bits_per_value > i16::BITS as i32 {
            u32::BITS as i32 - self.bits_per_value
        } else if self.bits_per_value > i8::BITS as i32 {
            i16::BITS as i32 - self.bits_per_value
        } else {
            0
        };

        debug_assert!(padding_bits_needed >= 0);
        let padding_bytes_needed = (padding_bits_needed + i8::BITS as i32 - 1) / i8::BITS as i32;
        debug_assert!(padding_bytes_needed <= 3);

        for _ in 0..padding_bytes_needed {
            self.output.write_byte(0)?;
        }
        self.finished = true;
        Ok(())
    }
    /// Returns an instance suitable for encoding `numValues` using
    /// `bitsPerValue`.
    pub fn get_instance(output: &'a mut D, num_values: i64, bits_per_value: i32) -> Result<Self> {
        match SUPPORTED_BITS_PER_VALUE.binary_search(&bits_per_value) {
            Ok(_) => (),
            Err(_) => {
                return Err(LuceneError::illegal_argument(format!(
                    "Unsupported bitsPerValue {bits_per_value}. Did you use bits_required?"
                )));
            },
        }
        DirectWriter::new(output, num_values, bits_per_value)
    }
}

/// Round a number of bits per value to the next amount of bits per value
/// that is supported by this writer.
///
/// # Parameters
/// - `bitsRequired`: the amount of bits required
///
/// # Returns
/// The next number of bits per value that is greater than or equal to the
/// provided value and supported by this writer.
fn round_bits(bits_required: i32) -> i32 {
    match SUPPORTED_BITS_PER_VALUE.binary_search(&bits_required) {
        Ok(_) => bits_required,
        Err(index) => SUPPORTED_BITS_PER_VALUE[index],
    }
}
/// Returns how many bits are required to hold values up to and including
/// `max_value`.
///
/// # Parameters
/// - `max_value`: The maximum value that should be representable.
///
/// # Returns
/// The amount of bits needed to represent values from 0 to `max_value`.
///
/// # See also
/// `PackedInts::bits_required(long)`
pub fn bits_required(max_value: i64) -> Result<i32> {
    Ok(round_bits(PackedInts::bits_required(max_value)?))
}
pub fn unsigned_bits_required(max_value: i64) -> i32 {
    round_bits(PackedInts::unsigned_bits_required(max_value))
}
/// Returns how many bits are required to hold values up to and including
/// `max_value`, interpreted as an unsigned value.
///
/// # Parameters
/// - `max_value`: The maximum value that should be representable.
///
/// # Returns
/// The amount of bits needed to represent values from 0 to `max_value`.
///
/// # See also
/// `PackedInts::unsigned_bits_required(long)`
pub(crate) const SUPPORTED_BITS_PER_VALUE: [i32; 14] =
    [1, 2, 4, 8, 12, 16, 20, 24, 28, 32, 40, 48, 56, 64];
