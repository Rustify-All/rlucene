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
use crate::core::util::packed::bulk_operation_packed::BulkOperationPacked;
use crate::core::util::packed::bulk_operation_packed_dummy::BulkOperationPackedDummy;
use crate::core::util::packed::bulk_operation_packed_enum::BulkOperationPackedEnum;
use crate::core::util::packed::bulk_operation_packed_single_block::BulkOperationPackedSingleBlock;
use crate::core::util::packed::bulk_operation_packed1::BulkOperationPacked1;
use crate::core::util::packed::bulk_operation_packed2::BulkOperationPacked2;
use crate::core::util::packed::bulk_operation_packed3::BulkOperationPacked3;
use crate::core::util::packed::bulk_operation_packed4::BulkOperationPacked4;
use crate::core::util::packed::bulk_operation_packed5::BulkOperationPacked5;
use crate::core::util::packed::bulk_operation_packed6::BulkOperationPacked6;
use crate::core::util::packed::bulk_operation_packed7::BulkOperationPacked7;
use crate::core::util::packed::bulk_operation_packed8::BulkOperationPacked8;
use crate::core::util::packed::bulk_operation_packed9::BulkOperationPacked9;
use crate::core::util::packed::bulk_operation_packed10::BulkOperationPacked10;
use crate::core::util::packed::bulk_operation_packed11::BulkOperationPacked11;
use crate::core::util::packed::bulk_operation_packed12::BulkOperationPacked12;
use crate::core::util::packed::bulk_operation_packed13::BulkOperationPacked13;
use crate::core::util::packed::bulk_operation_packed14::BulkOperationPacked14;
use crate::core::util::packed::bulk_operation_packed15::BulkOperationPacked15;
use crate::core::util::packed::bulk_operation_packed16::BulkOperationPacked16;
use crate::core::util::packed::bulk_operation_packed17::BulkOperationPacked17;
use crate::core::util::packed::bulk_operation_packed18::BulkOperationPacked18;
use crate::core::util::packed::bulk_operation_packed19::BulkOperationPacked19;
use crate::core::util::packed::bulk_operation_packed20::BulkOperationPacked20;
use crate::core::util::packed::bulk_operation_packed21::BulkOperationPacked21;
use crate::core::util::packed::bulk_operation_packed22::BulkOperationPacked22;
use crate::core::util::packed::bulk_operation_packed23::BulkOperationPacked23;
use crate::core::util::packed::bulk_operation_packed24::BulkOperationPacked24;
use crate::core::util::packed::{Decoder, Encoder};
/// Padding Value to make compiler happy
pub(crate) const PACKED_DUMMY: BulkOperationPackedEnum =
  BulkOperationPackedEnum::Dummy(BulkOperationPackedDummy::new());
pub(crate) const PACKED_BULK_OPS: [BulkOperationPackedEnum; 64] = [
  BulkOperationPackedEnum::Packed1(BulkOperationPacked::new(1, Some(BulkOperationPacked1))),
  BulkOperationPackedEnum::Packed2(BulkOperationPacked::new(2, Some(BulkOperationPacked2))),
  BulkOperationPackedEnum::Packed3(BulkOperationPacked::new(3, Some(BulkOperationPacked3))),
  BulkOperationPackedEnum::Packed4(BulkOperationPacked::new(4, Some(BulkOperationPacked4))),
  BulkOperationPackedEnum::Packed5(BulkOperationPacked::new(5, Some(BulkOperationPacked5))),
  BulkOperationPackedEnum::Packed6(BulkOperationPacked::new(6, Some(BulkOperationPacked6))),
  BulkOperationPackedEnum::Packed7(BulkOperationPacked::new(7, Some(BulkOperationPacked7))),
  BulkOperationPackedEnum::Packed8(BulkOperationPacked::new(8, Some(BulkOperationPacked8))),
  BulkOperationPackedEnum::Packed9(BulkOperationPacked::new(9, Some(BulkOperationPacked9))),
  BulkOperationPackedEnum::Packed10(BulkOperationPacked::new(10, Some(BulkOperationPacked10))),
  BulkOperationPackedEnum::Packed11(BulkOperationPacked::new(11, Some(BulkOperationPacked11))),
  BulkOperationPackedEnum::Packed12(BulkOperationPacked::new(12, Some(BulkOperationPacked12))),
  BulkOperationPackedEnum::Packed13(BulkOperationPacked::new(13, Some(BulkOperationPacked13))),
  BulkOperationPackedEnum::Packed14(BulkOperationPacked::new(14, Some(BulkOperationPacked14))),
  BulkOperationPackedEnum::Packed15(BulkOperationPacked::new(15, Some(BulkOperationPacked15))),
  BulkOperationPackedEnum::Packed16(BulkOperationPacked::new(16, Some(BulkOperationPacked16))),
  BulkOperationPackedEnum::Packed17(BulkOperationPacked::new(17, Some(BulkOperationPacked17))),
  BulkOperationPackedEnum::Packed18(BulkOperationPacked::new(18, Some(BulkOperationPacked18))),
  BulkOperationPackedEnum::Packed19(BulkOperationPacked::new(19, Some(BulkOperationPacked19))),
  BulkOperationPackedEnum::Packed20(BulkOperationPacked::new(20, Some(BulkOperationPacked20))),
  BulkOperationPackedEnum::Packed21(BulkOperationPacked::new(21, Some(BulkOperationPacked21))),
  BulkOperationPackedEnum::Packed22(BulkOperationPacked::new(22, Some(BulkOperationPacked22))),
  BulkOperationPackedEnum::Packed23(BulkOperationPacked::new(23, Some(BulkOperationPacked23))),
  BulkOperationPackedEnum::Packed24(BulkOperationPacked::new(24, Some(BulkOperationPacked24))),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(25, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(26, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(27, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(28, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(29, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(30, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(31, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(32, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(33, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(34, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(35, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(36, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(37, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(38, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(39, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(40, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(41, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(42, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(43, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(44, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(45, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(46, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(47, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(48, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(49, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(50, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(51, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(52, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(53, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(54, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(55, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(56, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(57, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(58, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(59, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(60, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(61, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(62, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(63, None)),
  BulkOperationPackedEnum::Packed(BulkOperationPacked::new(64, None)),
];
pub(crate) const PACKED_SINGLE_BLOCK_BULK_OPS: [BulkOperationPackedEnum; 32] = [
  BulkOperationPackedEnum::SinglePacked(BulkOperationPackedSingleBlock::new(1)),
  BulkOperationPackedEnum::SinglePacked(BulkOperationPackedSingleBlock::new(2)),
  BulkOperationPackedEnum::SinglePacked(BulkOperationPackedSingleBlock::new(3)),
  BulkOperationPackedEnum::SinglePacked(BulkOperationPackedSingleBlock::new(4)),
  BulkOperationPackedEnum::SinglePacked(BulkOperationPackedSingleBlock::new(5)),
  BulkOperationPackedEnum::SinglePacked(BulkOperationPackedSingleBlock::new(6)),
  BulkOperationPackedEnum::SinglePacked(BulkOperationPackedSingleBlock::new(7)),
  BulkOperationPackedEnum::SinglePacked(BulkOperationPackedSingleBlock::new(8)),
  BulkOperationPackedEnum::SinglePacked(BulkOperationPackedSingleBlock::new(9)),
  BulkOperationPackedEnum::SinglePacked(BulkOperationPackedSingleBlock::new(10)),
  BulkOperationPackedEnum::Dummy(BulkOperationPackedDummy::new()),
  BulkOperationPackedEnum::SinglePacked(BulkOperationPackedSingleBlock::new(12)),
  BulkOperationPackedEnum::Dummy(BulkOperationPackedDummy::new()),
  BulkOperationPackedEnum::Dummy(BulkOperationPackedDummy::new()),
  BulkOperationPackedEnum::Dummy(BulkOperationPackedDummy::new()),
  BulkOperationPackedEnum::SinglePacked(BulkOperationPackedSingleBlock::new(16)),
  BulkOperationPackedEnum::Dummy(BulkOperationPackedDummy::new()),
  BulkOperationPackedEnum::Dummy(BulkOperationPackedDummy::new()),
  BulkOperationPackedEnum::Dummy(BulkOperationPackedDummy::new()),
  BulkOperationPackedEnum::Dummy(BulkOperationPackedDummy::new()),
  BulkOperationPackedEnum::SinglePacked(BulkOperationPackedSingleBlock::new(21)),
  BulkOperationPackedEnum::Dummy(BulkOperationPackedDummy::new()),
  BulkOperationPackedEnum::Dummy(BulkOperationPackedDummy::new()),
  BulkOperationPackedEnum::Dummy(BulkOperationPackedDummy::new()),
  BulkOperationPackedEnum::Dummy(BulkOperationPackedDummy::new()),
  BulkOperationPackedEnum::Dummy(BulkOperationPackedDummy::new()),
  BulkOperationPackedEnum::Dummy(BulkOperationPackedDummy::new()),
  BulkOperationPackedEnum::Dummy(BulkOperationPackedDummy::new()),
  BulkOperationPackedEnum::Dummy(BulkOperationPackedDummy::new()),
  BulkOperationPackedEnum::Dummy(BulkOperationPackedDummy::new()),
  BulkOperationPackedEnum::Dummy(BulkOperationPackedDummy::new()),
  BulkOperationPackedEnum::SinglePacked(BulkOperationPackedSingleBlock::new(32)),
];
pub(crate) trait BulkOperation: Decoder + Encoder {
  fn write_long(&self, block: u64, blocks: &mut [u8], mut blocks_offset: usize) -> usize {
    for j in 1..=8 {
      blocks[blocks_offset] = (block >> (64 - (j << 3))) as u8;
      blocks_offset += 1;
    }
    blocks_offset
  }
  /// For every number of bits per value, there is a minimum number of blocks
  /// (b) / values (v) you need to write in order to reach the next block
  /// boundary:
  ///
  /// - 16 bits per value -> b=2, v=1
  /// - 24 bits per value -> b=3, v=1
  /// - 50 bits per value -> b=25, v=4
  /// - 63 bits per value -> b=63, v=8
  ///
  /// A bulk read consists of copying `iterations * v` values that are
  /// contained in `iterations * b` blocks into a `Vec<i64>` (higher
  /// values of `iterations` are likely to yield a better throughput):
  /// this requires `iterations * (b + 8v)` bytes of memory.
  ///
  /// This method computes `iterations` as `ram_budget / (b + 8v)` (since an
  /// i64 is 8 bytes).
  ///
  /// # Arguments
  /// - `value_count`: The total number of values.
  /// - `ram_budget`: The available RAM budget in bytes.
  ///
  /// # Returns
  /// The number of iterations to perform.
  fn compute_iterations(&self, value_count: i32, ram_budget: i32) -> i32 {
    let byte_value_count = Decoder::byte_value_count(self);
    let iterations = ram_budget / (Decoder::byte_block_count(self) + 8 * byte_value_count);
    if iterations == 0 {
      // At least 1 iteration is required
      1
    } else if (iterations - 1) * byte_value_count >= value_count {
      // Don't allocate for more than the size of the reader
      (value_count as f64 / byte_value_count as f64).ceil() as i32
    } else {
      iterations
    }
  }
}
use crate::core::util::packed::Format;

pub(crate) fn of(format: Format, bits_per_value: i32) -> &'static BulkOperationPackedEnum {
  match format {
    Format::Packed(..) => {
      debug_assert!(
        bits_per_value > 0 && bits_per_value <= 64,
        "bits_per_value must be between 1 and 64"
      );
      &PACKED_BULK_OPS[bits_per_value as usize - 1]
    },
    Format::PackedSingleBlock(..) => {
      debug_assert!(
        bits_per_value > 0 && bits_per_value <= 32,
        "bits_per_value must be between 1 and 32"
      );

      let operation = &PACKED_SINGLE_BLOCK_BULK_OPS[bits_per_value as usize - 1];

      debug_assert!(
        !matches!(operation, BulkOperationPackedEnum::Dummy(_)),
        "BulkOperationPackedDummy is not a valid operation"
      );
      operation
    },
  }
}
