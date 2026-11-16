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
use rand::Rng;
use rand::RngExt;

use crate::core::store::{ByteArrayDataInput, ByteBuffersDataOutput, DataOutput};
use crate::core::util::compress::lz4::LZ4;
use crate::core::util::compress::lz4::{HashTable, HashTableEnum};
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::util::test_util::TestUtil;
pub(crate) trait LZ4TestCase {
  fn new_hash_table(&self) -> AssertingHashTable;

  fn do_test<R>(random: &mut R, data: &[u8], hash_table: &mut AssertingHashTable) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    // this triggers special reset logic for high compression
    let offset = if data.len() >= (1 << 16) || random.random_bool(0.5) {
      random.random_range(0..10)
    } else {
      (1 << 16) - data.len() / 2
    };

    let mut copy = vec![0; data.len() + offset + random.random_range(0..10)];
    copy[offset..][..data.len()].copy_from_slice(data);
    Self::do_test_with_offset(random, copy.as_slice(), offset, data.len(), hash_table)
  }

  fn do_test_with_offset<R>(
    random: &mut R,
    data: &[u8],
    offset: usize,
    length: usize,
    hash_table: &mut AssertingHashTable,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let offset_i32 = offset as i32;
    let length_i32 = length as i32;
    let mut out = ByteBuffersDataOutput::new();
    LZ4::compress(data, offset_i32, length_i32, &mut out, &mut hash_table.ht)?;

    let compressed = out.try_get_array_ownership();
    let mut off = 0;
    let mut decompressed_off = 0;

    loop {
      let token = compressed[off];
      off += 1;
      let mut literal_len = (token >> 4) as usize;

      if literal_len == 0x0F {
        while compressed[off] == 0xFF {
          literal_len += 0xFF;
          off += 1;
        }
        literal_len += compressed[off] as usize;
        off += 1;
      }
      // skip literals
      off += literal_len;
      decompressed_off += literal_len;
      // check that the stream ends with literals and that there are
      // at least 5 of them
      if off == compressed.len() {
        assert_eq!(length, decompressed_off);
        assert!(literal_len >= LZ4::LAST_LITERALS as usize || literal_len == length);
        break;
      }

      let match_dec = (compressed[off] as usize) | ((compressed[off + 1] as usize) << 8);
      off += 2;

      assert!(match_dec > 0 && match_dec <= decompressed_off);

      let mut match_len = token as usize & 0x0F;
      if match_len == 0x0F {
        while compressed[off] == 0xFF {
          match_len += 0xFF;
          off += 1;
        }
        match_len += compressed[off] as usize;
        off += 1;
      }
      match_len += LZ4::MIN_MATCH as usize;
      {
        // if the match ends prematurely, the next sequence should
        // not have literals or this means we
        // are wasting space
        if decompressed_off + match_len + (LZ4::LAST_LITERALS as usize) < length {
          let more_common_bytes = data[offset + decompressed_off + match_len]
            == data[offset + decompressed_off - match_dec + match_len];
          let next_sequence_has_literals = compressed[off] >> 4 != 0;
          assert!(!(more_common_bytes && next_sequence_has_literals));
        }
      }

      decompressed_off += match_len;
    }

    assert_eq!(length, decompressed_off);

    // Compress once again with the same hash table to test reuse
    let mut out2 = ByteBuffersDataOutput::new();
    LZ4::compress(data, offset_i32, length_i32, &mut out2, &mut hash_table.ht)?;
    assert_eq!(compressed, out2.try_get_array_ownership());

    // Now restore and compare bytes
    let mut restored = vec![0; length + random.random_range(0..10)];
    let mut input = ByteArrayDataInput::with_bytes(compressed.as_slice());
    LZ4::decompress(&mut input, length_i32, &mut restored, 0)?;

    assert!(off <= i32::MAX as usize);
    debug_assert!(offset + length >= offset && offset + length <= data.len());
    let left = &data[offset..offset + length];
    debug_assert!(length <= restored.len());
    let right = &restored[..length];
    assert_eq!(left, right);

    // Now restore with an offset
    let restore_offset = TestUtil::next_usize(random, 1, 10);
    restored.clear();
    restored.resize(restore_offset + length + random.random_range(0..10), 0);
    let mut input = ByteArrayDataInput::with_bytes(compressed.as_slice());
    LZ4::decompress(&mut input, length_i32, &mut restored, restore_offset as i32)?;

    debug_assert!(offset + length >= offset && offset + length <= data.len());
    let left = &data[offset..offset + length];
    debug_assert!(
      restore_offset + length >= restore_offset && restore_offset + length <= restored.len()
    );
    let right = &restored[restore_offset..restore_offset + length];
    assert_eq!(left, right);

    Ok(())
  }

  fn do_test_with_dictionary<R>(
    random: &mut R,
    data: &[u8],
    hash_table: &mut AssertingHashTable,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let mut copy = ByteBuffersDataOutput::new();
    let dict_off = TestUtil::next_usize(random, 0, 10);
    let padding = [0u8; 10];
    copy.write_bytes(&padding[..dict_off])?;

    // Create a dictionary from substrings of the input to compress
    let max_distance = LZ4::MAX_DISTANCE as usize;
    let mut dict_len = 0;
    let mut i = TestUtil::next_usize(random, 0, data.len());
    while i < data.len() && dict_len < max_distance {
      let l = std::cmp::min(data.len() - i, TestUtil::next_usize(random, 1, 32));
      let l = std::cmp::min(l, max_distance - dict_len);
      assert!(l <= i32::MAX as usize);
      copy.write_bytes_range(data, i, l)?;
      dict_len += l;
      i += l;
      i += TestUtil::next_usize(random, 1, 32);
    }

    let data_length = data.len();
    assert!(data_length <= i32::MAX as usize);
    copy.write_bytes(data)?;
    copy.write_bytes(&padding[..random.random_range(0..10)])?;

    let copy_bytes = copy.try_get_array_ownership();
    Self::do_test_with_dictionary_inner(
      random,
      copy_bytes.as_slice(),
      dict_off,
      dict_len,
      data_length,
      hash_table,
    )
  }

  fn do_test_with_dictionary_inner<R>(
    random: &mut R,
    data: &[u8],
    dict_off: usize,
    dict_len: usize,
    length: usize,
    hash_table: &mut AssertingHashTable,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dict_off_i32 = dict_off as i32;
    let dict_len_i32 = dict_len as i32;
    let length_i32 = length as i32;
    let mut out = ByteBuffersDataOutput::new();
    LZ4::compress_with_dictionary(
      data,
      dict_off_i32,
      dict_len_i32,
      length_i32,
      &mut out,
      &mut hash_table.ht,
    )?;
    let compressed = out.try_get_array_ownership();

    // Compress once again with the same hash table to test reuse
    let mut out2 = ByteBuffersDataOutput::new();
    LZ4::compress_with_dictionary(
      data,
      dict_off_i32,
      dict_len_i32,
      length_i32,
      &mut out2,
      &mut hash_table.ht,
    )?;
    assert_eq!(compressed, out2.try_get_array_ownership());

    // Now restore and compare bytes
    let restore_offset = TestUtil::next_usize(random, 1, 10);
    let mut restored = vec![0; restore_offset + dict_len + length + random.random_range(0..10)];
    let copy_source = &data[dict_off..dict_off + dict_len];
    restored[restore_offset..][..copy_source.len()].copy_from_slice(copy_source);

    let mut input = ByteArrayDataInput::with_bytes(compressed.as_slice());
    LZ4::decompress(
      &mut input,
      length_i32,
      &mut restored,
      (dict_len + restore_offset) as i32,
    )?;

    debug_assert!(
      dict_off + dict_len + length >= dict_off + dict_len
        && dict_off + dict_len + length <= data.len()
    );
    let left = &data[dict_off + dict_len..dict_off + dict_len + length];
    debug_assert!(
      dict_len + restore_offset + length >= dict_len + restore_offset
        && dict_len + restore_offset + length <= restored.len()
    );
    let right = &restored[dict_len + restore_offset..dict_len + restore_offset + length];
    assert_eq!(left, right);

    Ok(())
  }
  fn test_empty<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    // literals and match lengths <= 15
    let data: Vec<u8> = "".to_string().into_bytes();
    Self::do_test(random, &data, &mut self.new_hash_table())
  }

  fn test_short_literals_and_matches<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    // literals and match lengths <= 15
    let data = b"1234562345673456745678910123";
    Self::do_test(random, data, &mut self.new_hash_table())?;
    Self::do_test_with_dictionary(random, data, &mut self.new_hash_table())?;
    Ok(())
  }

  fn test_long_matches<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    // match length >= 20
    let len = TestUtil::next_usize(random, 300, 1024);
    let mut data = vec![0u8; len];
    for (index, element) in data.iter_mut().enumerate() {
      *element = index as u8;
    }
    Self::do_test(random, data.as_slice(), &mut self.new_hash_table())?;
    Ok(())
  }
  fn test_long_literals<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    // long literals (length >= 16) which are not the last literals
    let len = TestUtil::next_usize(random, 400, 1024);
    let mut data = vec![0u8; len];
    random.fill_bytes(&mut data);
    let match_ref = random.random_range(0..30);
    let match_off = TestUtil::next_usize(random, len - 40, len - 20);
    let match_length = TestUtil::next_usize(random, 4, 10);
    data.copy_within(match_ref..match_ref + match_length, match_off);
    Self::do_test(random, data.as_slice(), &mut self.new_hash_table())?;
    Ok(())
  }

  fn test_match_right_before_last_literals<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let data = [1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 5];
    Self::do_test(random, data.as_slice(), &mut self.new_hash_table())?;
    Ok(())
  }

  fn test_incompressible_random<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let len = TestUtil::next_usize(random, 1, 1 << 18);
    let mut b = vec![0u8; len];
    random.fill_bytes(&mut b);
    Self::do_test(random, &b, &mut self.new_hash_table())?;
    Self::do_test_with_dictionary(random, &b, &mut self.new_hash_table())?;
    Ok(())
  }

  fn test_compressible_random<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let len = TestUtil::next_usize(random, 1, 1 << 18);
    let mut b = vec![0u8; len];
    let base = random.random_range(0..256);
    let max_delta = 1 + random.random_range(0..8);
    for elem in b.iter_mut() {
      *elem = (base + random.random_range(0..max_delta)) as u8;
    }
    Self::do_test(random, b.as_slice(), &mut self.new_hash_table())?;
    Self::do_test_with_dictionary(random, b.as_slice(), &mut self.new_hash_table())?;
    Ok(())
  }
  fn test_lucene5201<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let data: Vec<i8> = vec![
      14, 72, 14, 85, 3, 72, 14, 85, 3, 72, 14, 72, 14, 72, 14, 85, 3, 72, 14, 72, 14, 72, 14, 72,
      14, 72, 14, 72, 14, 85, 3, 72, 14, 85, 3, 72, 14, 85, 3, 72, 14, 85, 3, 72, 14, 85, 3, 72,
      14, 85, 3, 72, 14, 50, 64, 0, 46, -1, 0, 0, 0, 29, 3, 85, 8, -113, 0, 68, -97, 3, 0, 2, 3,
      -97, 6, 0, 68, -113, 0, 2, 3, -97, 6, 0, 68, -113, 0, 2, 3, -97, 6, 0, 68, -113, 0, 2, 3,
      -97, 6, 0, 68, -113, 0, 2, 3, -97, 6, 0, 68, -113, 0, 50, 64, 0, 47, -105, 0, 0, 0, 30, 3,
      -97, 6, 0, 68, -113, 0, 2, 3, -97, 6, 0, 68, -113, 0, 2, 3, 85, 8, -113, 0, 68, -97, 3, 0, 2,
      3, 85, 8, -113, 0, 68, -113, 0, 2, 3, -97, 6, 0, 68, -113, 0, 2, 3, -97, 6, 0, 68, -113, 0,
      2, 3, -97, 6, 0, 68, -113, 0, 2, 3, 85, 8, -113, 0, 68, -97, 3, 0, 2, 3, -97, 6, 0, 68, -113,
      0, 2, 3, -97, 6, 0, 68, -113, 0, 2, 3, -97, 6, 0, 68, -113, 0, 2, 3, -97, 6, 0, 68, -113, 0,
      2, 3, -97, 6, 0, 68, -113, 0, 2, 3, -97, 6, 0, 68, -113, 0, 2, 3, -97, 6, 0, 68, -113, 0, 2,
      3, 85, 8, -113, 0, 68, -113, 0, 2, 3, -97, 6, 0, 68, -113, 0, 2, 3, 85, 8, -113, 0, 68, -97,
      3, 0, 2, 3, -97, 6, 0, 68, -113, 0, 2, 3, -97, 6, 0, 68, -113, 0, 2, 3, -97, 6, 0, 68, -113,
      0, 2, 3, -97, 6, 0, 50, 64, 0, 50, 53, 0, 0, 0, 34, 3, 85, 8, -113, 0, 68, -97, 3, 0, 2, 3,
      -97, 6, 0, 68, -113, 0, 2, 3, -97, 6, 0, 68, -113, 0, 2, 3, -97, 6, 0, 68, -113, 0, 2, 3,
      -97, 6, 0, 68, -113, 0, 2, 3, -97, 6, 0, 68, -113, 0, 2, 3, 85, 8, -113, 0, 68, -113, 0, 2,
      3, -97, 6, 0, 68, -113, 0, 2, 3, 85, 8, -113, 0, 68, -97, 3, 0, 2, 3, 85, 8, -113, 0, 68,
      -97, 3, 0, 120, 64, 0, 52, -88, 0, 0, 0, 39, 13, 85, 5, 72, 13, 85, 5, 72, 13, 85, 5, 72, 13,
      72, 13, 85, 5, 72, 13, 85, 5, 72, 13, 85, 5, 72, 13, 85, 5, 72, 13, 85, 5, 72, 13, 72, 13,
      85, 5, 72, 13, 85, 5, 72, 13, 72, 13, 72, 13, 85, 5, 72, 13, 85, 5, 72, 13, 85, 5, 72, 13,
      85, 5, 72, 13, 85, 5, 72, 13, 85, 5, 72, 13, 85, 5, 72, 13, 85, 5, 72, 13, 85, 5, 72, 13, 85,
      5, 72, 13, 85, 5, 72, 13, 85, 5, 72, 13, 85, 5, 72, 13, 85, 5, 72, 13, 72, 13, 72, 13, 72,
      13, 85, 5, 72, 13, 85, 5, 72, 13, 72, 13, 85, 5, 72, 13, 85, 5, 72, 13, -19, -24, -101, -35,
    ];
    let len = data.len();
    let data_u8: Vec<u8> = data.into_iter().map(|x| x as u8).collect();
    Self::do_test_with_offset(
      random,
      data_u8.as_slice(),
      9,
      len - 9,
      &mut self.new_hash_table(),
    )
  }

  fn test_use_dictionary<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let b: Vec<i8> = vec![1, 2, 3, 4, 5, 6, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
    let dict_off = 0;
    let dict_len = 6;
    let len = b.len() - dict_len;
    let byte: Vec<u8> = b.into_iter().map(|x| x as u8).collect();

    Self::do_test_with_dictionary_inner(
      random,
      byte.as_slice(),
      dict_off,
      dict_len,
      len,
      &mut self.new_hash_table(),
    )?;
    let mut out = ByteBuffersDataOutput::new();
    LZ4::compress_with_dictionary(
      byte.as_slice(),
      dict_off as i32,
      dict_len as i32,
      len as i32,
      &mut out,
      &mut self.new_hash_table().ht,
    )?;

    // The compressed output is smaller than the original input despite
    // being incompressible on its own
    assert!(out.size() < len);
    Ok(())
  }
}

pub(crate) struct AssertingHashTable {
  ht: HashTableEnum,
}
impl AssertingHashTable {
  pub(crate) fn new(ht: HashTableEnum) -> Self {
    AssertingHashTable { ht }
  }
}
impl HashTable for AssertingHashTable {
  fn reset(&mut self, off: i32, len: i32) {
    self.ht.reset(off, len);
    assert!(self.ht.assert_reset());
  }

  fn init_dictionary(&mut self, dict_len: i32, bytes: &[u8]) {
    assert!(self.ht.assert_reset());
    self.ht.init_dictionary(dict_len, bytes)
  }

  fn get(&mut self, off: i32, bytes: &[u8]) -> Result<Option<i32>> {
    self.ht.get(off, bytes)
  }

  fn previous(&mut self, off: i32, bytes: &[u8]) -> Option<i32> {
    self.ht.previous(off, bytes)
  }

  fn assert_reset(&self) -> bool {
    unreachable!()
  }
}
