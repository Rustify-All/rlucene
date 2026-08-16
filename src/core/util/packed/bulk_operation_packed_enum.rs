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
use crate::core::util::packed::bulk_operation::BulkOperation;
use crate::core::util::packed::bulk_operation_packed::BulkOperationPacked;
use crate::core::util::packed::bulk_operation_packed_dummy::BulkOperationPackedDummy;
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

pub(crate) enum BulkOperationPackedEnum {
  Packed1(BulkOperationPacked<BulkOperationPacked1>),
  Packed2(BulkOperationPacked<BulkOperationPacked2>),
  Packed3(BulkOperationPacked<BulkOperationPacked3>),
  Packed4(BulkOperationPacked<BulkOperationPacked4>),
  Packed5(BulkOperationPacked<BulkOperationPacked5>),
  Packed6(BulkOperationPacked<BulkOperationPacked6>),
  Packed7(BulkOperationPacked<BulkOperationPacked7>),
  Packed8(BulkOperationPacked<BulkOperationPacked8>),
  Packed9(BulkOperationPacked<BulkOperationPacked9>),
  Packed10(BulkOperationPacked<BulkOperationPacked10>),
  Packed11(BulkOperationPacked<BulkOperationPacked11>),
  Packed12(BulkOperationPacked<BulkOperationPacked12>),
  Packed13(BulkOperationPacked<BulkOperationPacked13>),
  Packed14(BulkOperationPacked<BulkOperationPacked14>),
  Packed15(BulkOperationPacked<BulkOperationPacked15>),
  Packed16(BulkOperationPacked<BulkOperationPacked16>),
  Packed17(BulkOperationPacked<BulkOperationPacked17>),
  Packed18(BulkOperationPacked<BulkOperationPacked18>),
  Packed19(BulkOperationPacked<BulkOperationPacked19>),
  Packed20(BulkOperationPacked<BulkOperationPacked20>),
  Packed21(BulkOperationPacked<BulkOperationPacked21>),
  Packed22(BulkOperationPacked<BulkOperationPacked22>),
  Packed23(BulkOperationPacked<BulkOperationPacked23>),
  Packed24(BulkOperationPacked<BulkOperationPacked24>),
  Packed(BulkOperationPacked<BulkOperationPackedDummy>),
  SinglePacked(BulkOperationPackedSingleBlock),
  Dummy(BulkOperationPackedDummy),
}

impl Decoder for BulkOperationPackedEnum {
  fn long_block_count(&self) -> i32 {
    match self {
      BulkOperationPackedEnum::Packed1(op) => Decoder::long_block_count(op),
      BulkOperationPackedEnum::Packed2(op) => Decoder::long_block_count(op),
      BulkOperationPackedEnum::Packed3(op) => Decoder::long_block_count(op),
      BulkOperationPackedEnum::Packed4(op) => Decoder::long_block_count(op),
      BulkOperationPackedEnum::Packed5(op) => Decoder::long_block_count(op),
      BulkOperationPackedEnum::Packed6(op) => Decoder::long_block_count(op),
      BulkOperationPackedEnum::Packed7(op) => Decoder::long_block_count(op),
      BulkOperationPackedEnum::Packed8(op) => Decoder::long_block_count(op),
      BulkOperationPackedEnum::Packed9(op) => Decoder::long_block_count(op),
      BulkOperationPackedEnum::Packed10(op) => Decoder::long_block_count(op),
      BulkOperationPackedEnum::Packed11(op) => Decoder::long_block_count(op),
      BulkOperationPackedEnum::Packed12(op) => Decoder::long_block_count(op),
      BulkOperationPackedEnum::Packed13(op) => Decoder::long_block_count(op),
      BulkOperationPackedEnum::Packed14(op) => Decoder::long_block_count(op),
      BulkOperationPackedEnum::Packed15(op) => Decoder::long_block_count(op),
      BulkOperationPackedEnum::Packed16(op) => Decoder::long_block_count(op),
      BulkOperationPackedEnum::Packed17(op) => Decoder::long_block_count(op),
      BulkOperationPackedEnum::Packed18(op) => Decoder::long_block_count(op),
      BulkOperationPackedEnum::Packed19(op) => Decoder::long_block_count(op),
      BulkOperationPackedEnum::Packed20(op) => Decoder::long_block_count(op),
      BulkOperationPackedEnum::Packed21(op) => Decoder::long_block_count(op),
      BulkOperationPackedEnum::Packed22(op) => Decoder::long_block_count(op),
      BulkOperationPackedEnum::Packed23(op) => Decoder::long_block_count(op),
      BulkOperationPackedEnum::Packed24(op) => Decoder::long_block_count(op),
      BulkOperationPackedEnum::Packed(op) => Decoder::long_block_count(op),
      BulkOperationPackedEnum::SinglePacked(op) => Decoder::long_block_count(op),
      BulkOperationPackedEnum::Dummy(op) => Decoder::long_block_count(op),
    }
  }

  fn long_value_count(&self) -> i32 {
    match self {
      BulkOperationPackedEnum::Packed1(op) => Decoder::long_value_count(op),
      BulkOperationPackedEnum::Packed2(op) => Decoder::long_value_count(op),
      BulkOperationPackedEnum::Packed3(op) => Decoder::long_value_count(op),
      BulkOperationPackedEnum::Packed4(op) => Decoder::long_value_count(op),
      BulkOperationPackedEnum::Packed5(op) => Decoder::long_value_count(op),
      BulkOperationPackedEnum::Packed6(op) => Decoder::long_value_count(op),
      BulkOperationPackedEnum::Packed7(op) => Decoder::long_value_count(op),
      BulkOperationPackedEnum::Packed8(op) => Decoder::long_value_count(op),
      BulkOperationPackedEnum::Packed9(op) => Decoder::long_value_count(op),
      BulkOperationPackedEnum::Packed10(op) => Decoder::long_value_count(op),
      BulkOperationPackedEnum::Packed11(op) => Decoder::long_value_count(op),
      BulkOperationPackedEnum::Packed12(op) => Decoder::long_value_count(op),
      BulkOperationPackedEnum::Packed13(op) => Decoder::long_value_count(op),
      BulkOperationPackedEnum::Packed14(op) => Decoder::long_value_count(op),
      BulkOperationPackedEnum::Packed15(op) => Decoder::long_value_count(op),
      BulkOperationPackedEnum::Packed16(op) => Decoder::long_value_count(op),
      BulkOperationPackedEnum::Packed17(op) => Decoder::long_value_count(op),
      BulkOperationPackedEnum::Packed18(op) => Decoder::long_value_count(op),
      BulkOperationPackedEnum::Packed19(op) => Decoder::long_value_count(op),
      BulkOperationPackedEnum::Packed20(op) => Decoder::long_value_count(op),
      BulkOperationPackedEnum::Packed21(op) => Decoder::long_value_count(op),
      BulkOperationPackedEnum::Packed22(op) => Decoder::long_value_count(op),
      BulkOperationPackedEnum::Packed23(op) => Decoder::long_value_count(op),
      BulkOperationPackedEnum::Packed24(op) => Decoder::long_value_count(op),
      BulkOperationPackedEnum::Packed(op) => Decoder::long_value_count(op),
      BulkOperationPackedEnum::SinglePacked(op) => Decoder::long_value_count(op),
      BulkOperationPackedEnum::Dummy(op) => Decoder::long_value_count(op),
    }
  }

  fn byte_block_count(&self) -> i32 {
    match self {
      BulkOperationPackedEnum::Packed1(op) => Decoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed2(op) => Decoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed3(op) => Decoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed4(op) => Decoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed5(op) => Decoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed6(op) => Decoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed7(op) => Decoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed8(op) => Decoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed9(op) => Decoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed10(op) => Decoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed11(op) => Decoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed12(op) => Decoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed13(op) => Decoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed14(op) => Decoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed15(op) => Decoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed16(op) => Decoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed17(op) => Decoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed18(op) => Decoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed19(op) => Decoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed20(op) => Decoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed21(op) => Decoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed22(op) => Decoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed23(op) => Decoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed24(op) => Decoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed(op) => Decoder::byte_block_count(op),
      BulkOperationPackedEnum::SinglePacked(op) => Decoder::byte_block_count(op),
      BulkOperationPackedEnum::Dummy(op) => Decoder::byte_block_count(op),
    }
  }

  fn byte_value_count(&self) -> i32 {
    match self {
      BulkOperationPackedEnum::Packed1(op) => Decoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed2(op) => Decoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed3(op) => Decoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed4(op) => Decoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed5(op) => Decoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed6(op) => Decoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed7(op) => Decoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed8(op) => Decoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed9(op) => Decoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed10(op) => Decoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed11(op) => Decoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed12(op) => Decoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed13(op) => Decoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed14(op) => Decoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed15(op) => Decoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed16(op) => Decoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed17(op) => Decoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed18(op) => Decoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed19(op) => Decoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed20(op) => Decoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed21(op) => Decoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed22(op) => Decoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed23(op) => Decoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed24(op) => Decoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed(op) => Decoder::byte_value_count(op),
      BulkOperationPackedEnum::SinglePacked(op) => Decoder::byte_value_count(op),
      BulkOperationPackedEnum::Dummy(op) => Decoder::byte_value_count(op),
    }
  }

  fn decode_u64_to_i64(
    &self,
    blocks: &[u64],
    blocks_offset: usize,
    values: &mut [i64],
    values_offset: usize,
    iterations: i32,
  ) {
    match self {
      BulkOperationPackedEnum::Packed1(op) => {
        op.decode_u64_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed2(op) => {
        op.decode_u64_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed3(op) => {
        op.decode_u64_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed4(op) => {
        op.decode_u64_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed5(op) => {
        op.decode_u64_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed6(op) => {
        op.decode_u64_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed7(op) => {
        op.decode_u64_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed8(op) => {
        op.decode_u64_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed9(op) => {
        op.decode_u64_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed10(op) => {
        op.decode_u64_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed11(op) => {
        op.decode_u64_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed12(op) => {
        op.decode_u64_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed13(op) => {
        op.decode_u64_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed14(op) => {
        op.decode_u64_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed15(op) => {
        op.decode_u64_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed16(op) => {
        op.decode_u64_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed17(op) => {
        op.decode_u64_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed18(op) => {
        op.decode_u64_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed19(op) => {
        op.decode_u64_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed20(op) => {
        op.decode_u64_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed21(op) => {
        op.decode_u64_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed22(op) => {
        op.decode_u64_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed23(op) => {
        op.decode_u64_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed24(op) => {
        op.decode_u64_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed(op) => {
        op.decode_u64_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::SinglePacked(op) => {
        op.decode_u64_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Dummy(op) => {
        op.decode_u64_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
    }
  }

  fn decode_u8_to_i64(
    &self,
    blocks: &[u8],
    blocks_offset: usize,
    values: &mut [i64],
    values_offset: usize,
    iterations: i32,
  ) {
    match self {
      BulkOperationPackedEnum::Packed1(op) => {
        op.decode_u8_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed2(op) => {
        op.decode_u8_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed3(op) => {
        op.decode_u8_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed4(op) => {
        op.decode_u8_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed5(op) => {
        op.decode_u8_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed6(op) => {
        op.decode_u8_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed7(op) => {
        op.decode_u8_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed8(op) => {
        op.decode_u8_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed9(op) => {
        op.decode_u8_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed10(op) => {
        op.decode_u8_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed11(op) => {
        op.decode_u8_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed12(op) => {
        op.decode_u8_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed13(op) => {
        op.decode_u8_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed14(op) => {
        op.decode_u8_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed15(op) => {
        op.decode_u8_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed16(op) => {
        op.decode_u8_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed17(op) => {
        op.decode_u8_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed18(op) => {
        op.decode_u8_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed19(op) => {
        op.decode_u8_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed20(op) => {
        op.decode_u8_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed21(op) => {
        op.decode_u8_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed22(op) => {
        op.decode_u8_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed23(op) => {
        op.decode_u8_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed24(op) => {
        op.decode_u8_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed(op) => {
        op.decode_u8_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },

      BulkOperationPackedEnum::SinglePacked(op) => {
        op.decode_u8_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Dummy(op) => {
        op.decode_u8_to_i64(blocks, blocks_offset, values, values_offset, iterations)
      },
    }
  }

  fn decode_u64_to_i32(
    &self,
    blocks: &[u64],
    blocks_offset: usize,
    values: &mut [i32],
    values_offset: usize,
    iterations: i32,
  ) {
    match self {
      BulkOperationPackedEnum::Packed1(op) => {
        op.decode_u64_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed2(op) => {
        op.decode_u64_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed3(op) => {
        op.decode_u64_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed4(op) => {
        op.decode_u64_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed5(op) => {
        op.decode_u64_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed6(op) => {
        op.decode_u64_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed7(op) => {
        op.decode_u64_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed8(op) => {
        op.decode_u64_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed9(op) => {
        op.decode_u64_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed10(op) => {
        op.decode_u64_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed11(op) => {
        op.decode_u64_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed12(op) => {
        op.decode_u64_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed13(op) => {
        op.decode_u64_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed14(op) => {
        op.decode_u64_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed15(op) => {
        op.decode_u64_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed16(op) => {
        op.decode_u64_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed17(op) => {
        op.decode_u64_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed18(op) => {
        op.decode_u64_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed19(op) => {
        op.decode_u64_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed20(op) => {
        op.decode_u64_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed21(op) => {
        op.decode_u64_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },

      BulkOperationPackedEnum::Packed22(op) => {
        op.decode_u64_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed23(op) => {
        op.decode_u64_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed24(op) => {
        op.decode_u64_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed(op) => {
        op.decode_u64_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::SinglePacked(op) => {
        op.decode_u64_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Dummy(op) => {
        op.decode_u64_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
    }
  }

  fn decode_u8_to_i32(
    &self,
    blocks: &[u8],
    blocks_offset: usize,
    values: &mut [i32],
    values_offset: usize,
    iterations: i32,
  ) {
    match self {
      BulkOperationPackedEnum::Packed1(op) => {
        op.decode_u8_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed2(op) => {
        op.decode_u8_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed3(op) => {
        op.decode_u8_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed4(op) => {
        op.decode_u8_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed5(op) => {
        op.decode_u8_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed6(op) => {
        op.decode_u8_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed7(op) => {
        op.decode_u8_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed8(op) => {
        op.decode_u8_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed9(op) => {
        op.decode_u8_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed10(op) => {
        op.decode_u8_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed11(op) => {
        op.decode_u8_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed12(op) => {
        op.decode_u8_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed13(op) => {
        op.decode_u8_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed14(op) => {
        op.decode_u8_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed15(op) => {
        op.decode_u8_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed16(op) => {
        op.decode_u8_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed17(op) => {
        op.decode_u8_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed18(op) => {
        op.decode_u8_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed19(op) => {
        op.decode_u8_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed20(op) => {
        op.decode_u8_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed21(op) => {
        op.decode_u8_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed22(op) => {
        op.decode_u8_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed23(op) => {
        op.decode_u8_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed24(op) => {
        op.decode_u8_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Packed(op) => {
        op.decode_u8_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::SinglePacked(op) => {
        op.decode_u8_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
      BulkOperationPackedEnum::Dummy(op) => {
        op.decode_u8_to_i32(blocks, blocks_offset, values, values_offset, iterations)
      },
    }
  }
}
impl Encoder for BulkOperationPackedEnum {
  fn long_block_count(&self) -> i32 {
    match self {
      BulkOperationPackedEnum::Packed1(op) => Encoder::long_block_count(op),
      BulkOperationPackedEnum::Packed2(op) => Encoder::long_block_count(op),
      BulkOperationPackedEnum::Packed3(op) => Encoder::long_block_count(op),
      BulkOperationPackedEnum::Packed4(op) => Encoder::long_block_count(op),
      BulkOperationPackedEnum::Packed5(op) => Encoder::long_block_count(op),
      BulkOperationPackedEnum::Packed6(op) => Encoder::long_block_count(op),
      BulkOperationPackedEnum::Packed7(op) => Encoder::long_block_count(op),
      BulkOperationPackedEnum::Packed8(op) => Encoder::long_block_count(op),
      BulkOperationPackedEnum::Packed9(op) => Encoder::long_block_count(op),
      BulkOperationPackedEnum::Packed10(op) => Encoder::long_block_count(op),
      BulkOperationPackedEnum::Packed11(op) => Encoder::long_block_count(op),
      BulkOperationPackedEnum::Packed12(op) => Encoder::long_block_count(op),
      BulkOperationPackedEnum::Packed13(op) => Encoder::long_block_count(op),
      BulkOperationPackedEnum::Packed14(op) => Encoder::long_block_count(op),
      BulkOperationPackedEnum::Packed15(op) => Encoder::long_block_count(op),
      BulkOperationPackedEnum::Packed16(op) => Encoder::long_block_count(op),
      BulkOperationPackedEnum::Packed17(op) => Encoder::long_block_count(op),
      BulkOperationPackedEnum::Packed18(op) => Encoder::long_block_count(op),
      BulkOperationPackedEnum::Packed19(op) => Encoder::long_block_count(op),
      BulkOperationPackedEnum::Packed20(op) => Encoder::long_block_count(op),
      BulkOperationPackedEnum::Packed21(op) => Encoder::long_block_count(op),
      BulkOperationPackedEnum::Packed22(op) => Encoder::long_block_count(op),
      BulkOperationPackedEnum::Packed23(op) => Encoder::long_block_count(op),
      BulkOperationPackedEnum::Packed24(op) => Encoder::long_block_count(op),
      BulkOperationPackedEnum::Packed(op) => Encoder::long_block_count(op),
      BulkOperationPackedEnum::SinglePacked(op) => Encoder::long_block_count(op),
      BulkOperationPackedEnum::Dummy(op) => Encoder::long_block_count(op),
    }
  }

  fn long_value_count(&self) -> i32 {
    match self {
      BulkOperationPackedEnum::Packed1(op) => Encoder::long_value_count(op),
      BulkOperationPackedEnum::Packed2(op) => Encoder::long_value_count(op),
      BulkOperationPackedEnum::Packed3(op) => Encoder::long_value_count(op),
      BulkOperationPackedEnum::Packed4(op) => Encoder::long_value_count(op),
      BulkOperationPackedEnum::Packed5(op) => Encoder::long_value_count(op),
      BulkOperationPackedEnum::Packed6(op) => Encoder::long_value_count(op),
      BulkOperationPackedEnum::Packed7(op) => Encoder::long_value_count(op),
      BulkOperationPackedEnum::Packed8(op) => Encoder::long_value_count(op),
      BulkOperationPackedEnum::Packed9(op) => Encoder::long_value_count(op),
      BulkOperationPackedEnum::Packed10(op) => Encoder::long_value_count(op),
      BulkOperationPackedEnum::Packed11(op) => Encoder::long_value_count(op),
      BulkOperationPackedEnum::Packed12(op) => Encoder::long_value_count(op),
      BulkOperationPackedEnum::Packed13(op) => Encoder::long_value_count(op),
      BulkOperationPackedEnum::Packed14(op) => Encoder::long_value_count(op),
      BulkOperationPackedEnum::Packed15(op) => Encoder::long_value_count(op),
      BulkOperationPackedEnum::Packed16(op) => Encoder::long_value_count(op),
      BulkOperationPackedEnum::Packed17(op) => Encoder::long_value_count(op),
      BulkOperationPackedEnum::Packed18(op) => Encoder::long_value_count(op),
      BulkOperationPackedEnum::Packed19(op) => Encoder::long_value_count(op),
      BulkOperationPackedEnum::Packed20(op) => Encoder::long_value_count(op),
      BulkOperationPackedEnum::Packed21(op) => Encoder::long_value_count(op),
      BulkOperationPackedEnum::Packed22(op) => Encoder::long_value_count(op),
      BulkOperationPackedEnum::Packed23(op) => Encoder::long_value_count(op),
      BulkOperationPackedEnum::Packed24(op) => Encoder::long_value_count(op),
      BulkOperationPackedEnum::Packed(op) => Encoder::long_value_count(op),
      BulkOperationPackedEnum::SinglePacked(op) => Encoder::long_value_count(op),
      BulkOperationPackedEnum::Dummy(op) => Encoder::long_value_count(op),
    }
  }

  fn byte_block_count(&self) -> i32 {
    match self {
      BulkOperationPackedEnum::Packed1(op) => Encoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed2(op) => Encoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed3(op) => Encoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed4(op) => Encoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed5(op) => Encoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed6(op) => Encoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed7(op) => Encoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed8(op) => Encoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed9(op) => Encoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed10(op) => Encoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed11(op) => Encoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed12(op) => Encoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed13(op) => Encoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed14(op) => Encoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed15(op) => Encoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed16(op) => Encoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed17(op) => Encoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed18(op) => Encoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed19(op) => Encoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed20(op) => Encoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed21(op) => Encoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed22(op) => Encoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed23(op) => Encoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed24(op) => Encoder::byte_block_count(op),
      BulkOperationPackedEnum::Packed(op) => Encoder::byte_block_count(op),
      BulkOperationPackedEnum::SinglePacked(op) => Encoder::byte_block_count(op),
      BulkOperationPackedEnum::Dummy(op) => Encoder::byte_block_count(op),
    }
  }

  fn byte_value_count(&self) -> i32 {
    match self {
      BulkOperationPackedEnum::Packed1(op) => Encoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed2(op) => Encoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed3(op) => Encoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed4(op) => Encoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed5(op) => Encoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed6(op) => Encoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed7(op) => Encoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed8(op) => Encoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed9(op) => Encoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed10(op) => Encoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed11(op) => Encoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed12(op) => Encoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed13(op) => Encoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed14(op) => Encoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed15(op) => Encoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed16(op) => Encoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed17(op) => Encoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed18(op) => Encoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed19(op) => Encoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed20(op) => Encoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed21(op) => Encoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed22(op) => Encoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed23(op) => Encoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed24(op) => Encoder::byte_value_count(op),
      BulkOperationPackedEnum::Packed(op) => Encoder::byte_value_count(op),
      BulkOperationPackedEnum::SinglePacked(op) => Encoder::byte_value_count(op),
      BulkOperationPackedEnum::Dummy(op) => Encoder::byte_value_count(op),
    }
  }

  fn encode_i64_to_u64(
    &self,
    values: &[i64],
    values_offset: usize,
    blocks: &mut [u64],
    blocks_offset: usize,
    iterations: i32,
  ) {
    match self {
      BulkOperationPackedEnum::Packed1(op) => {
        op.encode_i64_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed2(op) => {
        op.encode_i64_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed3(op) => {
        op.encode_i64_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed4(op) => {
        op.encode_i64_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed5(op) => {
        op.encode_i64_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed6(op) => {
        op.encode_i64_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed7(op) => {
        op.encode_i64_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed8(op) => {
        op.encode_i64_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed9(op) => {
        op.encode_i64_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed10(op) => {
        op.encode_i64_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed11(op) => {
        op.encode_i64_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed12(op) => {
        op.encode_i64_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed13(op) => {
        op.encode_i64_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed14(op) => {
        op.encode_i64_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed15(op) => {
        op.encode_i64_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed16(op) => {
        op.encode_i64_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed17(op) => {
        op.encode_i64_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed18(op) => {
        op.encode_i64_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed19(op) => {
        op.encode_i64_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed20(op) => {
        op.encode_i64_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed21(op) => {
        op.encode_i64_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed22(op) => {
        op.encode_i64_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed23(op) => {
        op.encode_i64_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed24(op) => {
        op.encode_i64_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed(op) => {
        op.encode_i64_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::SinglePacked(op) => {
        op.encode_i64_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Dummy(op) => {
        op.encode_i64_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
    }
  }

  fn encode_i64_to_u8(
    &self,
    values: &[i64],
    values_offset: usize,
    blocks: &mut [u8],
    blocks_offset: usize,
    iterations: i32,
  ) {
    match self {
      BulkOperationPackedEnum::Packed1(op) => {
        op.encode_i64_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed2(op) => {
        op.encode_i64_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed3(op) => {
        op.encode_i64_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed4(op) => {
        op.encode_i64_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed5(op) => {
        op.encode_i64_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed6(op) => {
        op.encode_i64_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed7(op) => {
        op.encode_i64_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed8(op) => {
        op.encode_i64_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed9(op) => {
        op.encode_i64_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed10(op) => {
        op.encode_i64_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed11(op) => {
        op.encode_i64_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed12(op) => {
        op.encode_i64_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed13(op) => {
        op.encode_i64_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed14(op) => {
        op.encode_i64_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed15(op) => {
        op.encode_i64_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed16(op) => {
        op.encode_i64_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed17(op) => {
        op.encode_i64_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed18(op) => {
        op.encode_i64_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed19(op) => {
        op.encode_i64_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed20(op) => {
        op.encode_i64_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed21(op) => {
        op.encode_i64_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed22(op) => {
        op.encode_i64_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed23(op) => {
        op.encode_i64_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed24(op) => {
        op.encode_i64_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed(op) => {
        op.encode_i64_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::SinglePacked(op) => {
        op.encode_i64_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Dummy(op) => {
        op.encode_i64_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
    }
  }

  fn encode_i32_to_u64(
    &self,
    values: &[i32],
    values_offset: usize,
    blocks: &mut [u64],
    blocks_offset: usize,
    iterations: i32,
  ) {
    match self {
      BulkOperationPackedEnum::Packed1(op) => {
        op.encode_i32_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed2(op) => {
        op.encode_i32_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed3(op) => {
        op.encode_i32_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed4(op) => {
        op.encode_i32_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed5(op) => {
        op.encode_i32_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed6(op) => {
        op.encode_i32_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed7(op) => {
        op.encode_i32_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed8(op) => {
        op.encode_i32_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed9(op) => {
        op.encode_i32_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed10(op) => {
        op.encode_i32_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed11(op) => {
        op.encode_i32_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed12(op) => {
        op.encode_i32_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed13(op) => {
        op.encode_i32_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed14(op) => {
        op.encode_i32_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed15(op) => {
        op.encode_i32_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed16(op) => {
        op.encode_i32_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed17(op) => {
        op.encode_i32_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed18(op) => {
        op.encode_i32_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed19(op) => {
        op.encode_i32_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed20(op) => {
        op.encode_i32_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed21(op) => {
        op.encode_i32_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed22(op) => {
        op.encode_i32_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed23(op) => {
        op.encode_i32_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed24(op) => {
        op.encode_i32_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed(op) => {
        op.encode_i32_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::SinglePacked(op) => {
        op.encode_i32_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Dummy(op) => {
        op.encode_i32_to_u64(values, values_offset, blocks, blocks_offset, iterations)
      },
    }
  }

  fn encode_i32_to_u8(
    &self,
    values: &[i32],
    values_offset: usize,
    blocks: &mut [u8],
    blocks_offset: usize,
    iterations: i32,
  ) {
    match self {
      BulkOperationPackedEnum::Packed1(op) => {
        op.encode_i32_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed2(op) => {
        op.encode_i32_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed3(op) => {
        op.encode_i32_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed4(op) => {
        op.encode_i32_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed5(op) => {
        op.encode_i32_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed6(op) => {
        op.encode_i32_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed7(op) => {
        op.encode_i32_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed8(op) => {
        op.encode_i32_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed9(op) => {
        op.encode_i32_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed10(op) => {
        op.encode_i32_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed11(op) => {
        op.encode_i32_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed12(op) => {
        op.encode_i32_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed13(op) => {
        op.encode_i32_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed14(op) => {
        op.encode_i32_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed15(op) => {
        op.encode_i32_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed16(op) => {
        op.encode_i32_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed17(op) => {
        op.encode_i32_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed18(op) => {
        op.encode_i32_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed19(op) => {
        op.encode_i32_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed20(op) => {
        op.encode_i32_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed21(op) => {
        op.encode_i32_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed22(op) => {
        op.encode_i32_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed23(op) => {
        op.encode_i32_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed24(op) => {
        op.encode_i32_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Packed(op) => {
        op.encode_i32_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::SinglePacked(op) => {
        op.encode_i32_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
      BulkOperationPackedEnum::Dummy(op) => {
        op.encode_i32_to_u8(values, values_offset, blocks, blocks_offset, iterations)
      },
    }
  }
}
impl BulkOperation for BulkOperationPackedEnum {}
