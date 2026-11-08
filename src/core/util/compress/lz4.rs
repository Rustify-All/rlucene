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
use crate::core::store::{DataInput, DataOutput};
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::{CoreHelper, TryIntoInt};

// Share Java hash-table calculations between the public HashTable methods
// and the concrete table borrows used during compression.
// Keep the same mutations and lookup order for each table implementation.
macro_rules! compression_hash_table {
  (init_dictionary, $base:expr, $hash_log:expr, $last_off:expr, $dict_len:expr, $bytes:expr, $set:expr) => {{
    let base = $base;
    let hash_log = $hash_log;
    for i in 0..$dict_len {
      let v = LZ4::read_int($bytes, base + i);
      let h = LZ4::hash(v, hash_log);
      ($set)(h, i);
    }
    $last_off += $dict_len;
  }};
  (get, $base:expr, $hash_log:expr, $last_off:expr, $end:expr, $off:expr, $bytes:expr, $get_and_set:expr) => {{
    let off = $off;
    debug_assert!(off > $last_off);
    debug_assert!(off < $end);
    let v = LZ4::read_int($bytes, off);
    let h = LZ4::hash(v, $hash_log);
    let ref_ = $base + ($get_and_set)(h, off - $base)?;
    $last_off = off;
    Ok::<Option<i32>, LuceneError>(
      if ref_ < off && off - ref_ < LZ4::MAX_DISTANCE && LZ4::read_int($bytes, ref_) == v {
        Some(ref_)
      } else {
        None
      },
    )
  }};
  (high_add_hash, $table:expr, $off:expr, $bytes:expr) => {{
    let off = $off;
    let bytes = $bytes;
    let table = &mut *($table);
    let v = LZ4::read_int(bytes, off);
    let h = LZ4::hash_hc(v);
    let mut delta = off - table.hash_table[h];
    if delta <= 0 || delta >= LZ4::MAX_DISTANCE {
      delta = LZ4::MAX_DISTANCE - 1;
    }
    table.chain_table[(off & HighCompressionHashTable::MASK) as usize] = delta as u16;
    table.hash_table[h] = off;
  }};
  (high_get, $table:expr, $off:expr, $bytes:expr) => {{
    let off = $off;
    let bytes = $bytes;
    let table = &mut *($table);
    'high_get: {
      debug_assert!(off >= table.next);
      debug_assert!(off < table.end);

      while table.next < off {
        compression_hash_table!(high_add_hash, table, table.next, bytes);
        table.next += 1;
      }
      let v = LZ4::read_int(bytes, off);
      let h = LZ4::hash_hc(v);

      table.attempts = 0;
      let mut ref_idx = table.hash_table[h];
      if ref_idx >= off {
        // remainder from a previous call to compress()
        break 'high_get Ok::<Option<i32>, LuceneError>(None);
      }
      let min = std::cmp::max(table.base, off - LZ4::MAX_DISTANCE + 1);
      while ref_idx >= min && table.attempts < HighCompressionHashTable::MAX_ATTEMPTS {
        if LZ4::read_int(bytes, ref_idx) == v {
          break 'high_get Ok::<Option<i32>, LuceneError>(Some(ref_idx));
        }
        ref_idx -= table.chain_table[(ref_idx & HighCompressionHashTable::MASK) as usize] as i32;
        table.attempts += 1;
      }
      Ok(None)
    }
  }};
}

/// LZ4 compression and decompression routines.
///
/// <https://github.com/lz4/lz4/tree/dev/lib> <http://fastcompression.blogspot.fr/p/lz4.html>
///
/// The high-compression option is a simpler version of the one of the original
/// algorithm, and only retains a better hash table that remembers about more
/// occurrences of a previous 4-bytes sequence, and removes all the logic about
/// handling of the case when overlapping matches are found.
pub struct LZ4;

impl LZ4 {
  /// Window size: this is the maximum supported distance between two strings
  /// so that LZ4 can replace the second one by a reference to the first
  /// one.
  pub const MAX_DISTANCE: i32 = 1 << 16; // maximum distance of a reference

  pub const MEMORY_USAGE: i32 = 14;
  pub const MIN_MATCH: i32 = 4; // minimum length of a match
  pub const LAST_LITERALS: i32 = 5; // the last 5 bytes must be encoded as literals
  pub const HASH_LOG_HC: i32 = 15; // log size of the dictionary for compressHC
  pub const HASH_TABLE_SIZE_HC: i32 = 1 << LZ4::HASH_LOG_HC;

  fn hash(i: i32, hash_bits: i32) -> usize {
    ((i.wrapping_mul(-1640531535) as u32) >> (32 - hash_bits)) as usize
  }

  fn hash_hc(i: i32) -> usize {
    Self::hash(i, LZ4::HASH_LOG_HC)
  }

  /// Note: This method expects the data to be read in little-endian byte
  /// order. Ensure that the input data is in little-endian format, or it
  /// may result in incorrect parsing.
  #[inline]
  fn read_int(buf: &[u8], i: i32) -> i32 {
    debug_assert!(i >= 0);
    // According to LZ4's algorithm the endianness does not matter at all:
    BitUtil::get_i32_le(buf, i as usize)
  }

  #[inline]
  fn common_bytes(b: &[u8], o1: i32, o2: i32, limit: i32) -> i32 {
    debug_assert!(o1 < o2);
    CoreHelper::miss_match_u8(
      &b[o1 as usize..limit as usize],
      &b[o2 as usize..limit as usize],
    )
  }

  /// Copies the non-incremental match using the rounded-up `fast_len`.
  #[inline]
  fn copy_fast_match(dest: &mut [u8], d_off: i32, match_dec: i32, fast_len: i32) {
    let start = (d_off - match_dec) as usize;
    let end = d_off as usize;
    let fast_len = fast_len as usize;

    // match_dec >= match_len does not rule out overlap after rounding to fast_len.
    // Use the fixed-size copy only for disjoint, valid 16-byte ranges.
    if fast_len == 16
      && match_dec >= 16
      && d_off >= match_dec
      && end <= dest.len().saturating_sub(16)
    {
      let (before, after) = dest.split_at_mut(end);
      after[..16].copy_from_slice(&before[start..start + 16]);
    } else {
      dest.copy_within(start..start + fast_len, end);
    }
  }

  /// Decompress at least `decompressed_len` bytes into `dest[d_off..]`.
  /// Please note that `dest` must be large enough to hold **all**
  /// decompressed data (meaning that you need to know the total
  /// decompressed length). If the given bytes were compressed using a
  /// preset dictionary, the same dictionary must be provided in
  /// `dest[d_off-dict_len..d_off]`.
  pub fn decompress<DI>(
    compressed: &mut DI,
    decompressed_len: i32,
    dest: &mut [u8],
    mut d_off: i32,
  ) -> Result<i32>
  where
    DI: DataInput,
  {
    let dest_end = d_off + decompressed_len;
    loop {
      let token = compressed.read_byte()? as i32;
      let mut literal_len = (token as u32 >> 4) as i32;

      if literal_len != 0 {
        if literal_len == 0x0F {
          loop {
            let len = compressed.read_byte()?;
            if len != 0xFF {
              literal_len += len as i32;
              break;
            }
            literal_len += 0xFF;
          }
        }
        compressed.read_bytes(dest, d_off as usize, literal_len as usize)?;
        d_off += literal_len;
      }

      if d_off >= dest_end {
        break;
      }

      // Read matches
      let match_dec = compressed.read_short()? as u16 as i32;

      debug_assert!(match_dec > 0);

      let mut match_len = token & 0x0F;
      if match_len == 0x0F {
        loop {
          let len = compressed.read_byte()?;
          if len != 0xFF {
            match_len += len as i32;
            break;
          }
          match_len += 0xFF;
        }
      }
      match_len += LZ4::MIN_MATCH;

      // copying a multiple of 8 bytes can make decompression from 5% to
      // 10% faster
      let fast_len = (match_len + 7) & !7;

      if match_dec < match_len || d_off + fast_len > dest_end {
        let mut start: usize = (d_off - match_dec).try_convert()?;
        let end = d_off + match_len;

        if match_dec == 1
          && let Some(&value) = dest.get(start)
          && let Some(output) = dest.get_mut(d_off as usize..end as usize)
        {
          output.fill(value);
          d_off = end;
        } else {
          while d_off < end {
            dest[d_off as usize] = dest[start];
            start += 1;
            d_off += 1;
          }
        }
      } else {
        Self::copy_fast_match(dest, d_off, match_dec, fast_len);
        d_off += match_len;
      }

      if d_off >= dest_end {
        break;
      }
    }
    Ok(d_off)
  }
  fn encode_len<DO>(mut l: i32, out: &mut DO) -> Result<()>
  where
    DO: DataOutput,
  {
    while l >= 0xFF {
      out.write_byte(0xFF)?;
      l -= 0xFF;
    }
    out.write_byte(l as u8)?;
    Ok(())
  }
  fn encode_literals<DO>(
    bytes: &[u8],
    token: i32,
    anchor: i32,
    literal_len: i32,
    out: &mut DO,
  ) -> Result<()>
  where
    DO: DataOutput,
  {
    out.write_byte(token as u8)?;

    // encode literal length
    if literal_len >= 0x0F {
      Self::encode_len(literal_len - 0x0F, out)?;
    }

    // encode literals
    out.write_bytes_range(bytes, anchor as usize, literal_len as usize)?;

    Ok(())
  }
  fn encode_last_literals<DO>(
    bytes: &[u8],
    anchor: i32,
    literal_len: i32,
    out: &mut DO,
  ) -> Result<()>
  where
    DO: DataOutput,
  {
    let token = std::cmp::min(literal_len, 0x0F) << 4;
    Self::encode_literals(bytes, token, anchor, literal_len, out)
  }

  fn encode_sequence<DO>(
    bytes: &[u8],
    anchor: i32,
    match_ref: i32,
    match_off: i32,
    match_len: i32,
    out: &mut DO,
  ) -> Result<()>
  where
    DO: DataOutput,
  {
    let literal_len = match_off - anchor;
    debug_assert!(match_len >= 4);
    // Encode token
    let token = (i32::min(literal_len, 0x0F) << 4) | i32::min(match_len - 4, 0x0F);
    Self::encode_literals(bytes, token, anchor, literal_len, out)?;

    // Encode match dec
    let match_dec = match_off - match_ref;
    debug_assert!(match_dec > 0 && match_dec < (1 << 16));
    out.write_short(match_dec as i16)?;

    // Encode match len
    if match_len >= Self::MIN_MATCH + 0x0F {
      Self::encode_len(match_len - 0x0F - Self::MIN_MATCH, out)?;
    }

    Ok(())
  }
  /// Compress `bytes[off:off+len]` into `out` using at most 16kB of memory.
  /// `ht` shouldn't be shared across threads but can safely be reused.
  pub fn compress<DO>(
    bytes: &[u8],
    off: i32,
    len: i32,
    out: &mut DO,
    ht: &mut HashTableEnum,
  ) -> Result<()>
  where
    DO: DataOutput,
  {
    Self::compress_with_dictionary(bytes, off, 0, len, out, ht)
  }
  /// Compress `[dictOff+dictLen:dictOff+dictLen+len]` into `out` using at
  /// most 16kB of memory. `[dictOff:dictOff+dictLen]` will be used as a
  /// dictionary. `dictLen` must not be greater than `MAX_DISTANCE 64kB`,
  /// the maximum window size. `ht` shouldn't be shared across threads but
  /// can safely be reused.
  pub fn compress_with_dictionary<DO>(
    bytes: &[u8],
    dict_off: i32,
    dict_len: i32,
    len: i32,
    out: &mut DO,
    ht: &mut HashTableEnum,
  ) -> Result<()>
  where
    DO: DataOutput,
  {
    // Ensure the indices are valid
    CoreHelper::check_from_index_size(dict_off as usize, dict_len as usize, bytes.len())?;
    CoreHelper::check_from_index_size((dict_off + dict_len) as usize, len as usize, bytes.len())?;

    if dict_len > LZ4::MAX_DISTANCE {
      return Err(LuceneError::illegal_argument(format!(
        "dictLen must not be greater than 64kB, but got {dict_len}"
      )));
    }

    let end = dict_off + dict_len + len;
    let mut off = dict_off + dict_len;
    let mut anchor = off;
    if len > LZ4::LAST_LITERALS + LZ4::MIN_MATCH {
      let limit = end - LZ4::LAST_LITERALS;
      let match_limit = limit - LZ4::MIN_MATCH;
      CoreHelper::check_from_index_size(dict_off as usize, (dict_len + len) as usize, bytes.len())?;
      ht.reset(dict_off, dict_len + len);
      macro_rules! compress_loop {
        ($get:ident, $previous:ident) => {{
          'main: while off <= limit {
            // find a match
            let mut ref_;

            loop {
              if off >= match_limit {
                break 'main;
              }

              if let Some(index) = $get!(off)? {
                ref_ = index;
                debug_assert!(ref_ >= dict_off && ref_ < off);
                debug_assert_eq!(LZ4::read_int(bytes, ref_), LZ4::read_int(bytes, off));
                break;
              }

              off += 1;
            }

            // Compute match length
            let mut match_len = LZ4::MIN_MATCH
              + LZ4::common_bytes(bytes, ref_ + LZ4::MIN_MATCH, off + LZ4::MIN_MATCH, limit);

            // Try to find a better match
            let min = (off - LZ4::MAX_DISTANCE + 1).max(dict_off);
            let mut r = $previous!(ref_);
            while let Some(r_index) = r.filter(|&r| r >= min) {
              debug_assert_eq!(LZ4::read_int(bytes, r_index), LZ4::read_int(bytes, off));
              let r_match_len = LZ4::MIN_MATCH
                + LZ4::common_bytes(bytes, r_index + LZ4::MIN_MATCH, off + LZ4::MIN_MATCH, limit);
              if r_match_len > match_len {
                ref_ = r_index;
                match_len = r_match_len;
              }

              r = $previous!(r_index);
            }

            // Encode match
            LZ4::encode_sequence(bytes, anchor, ref_, off, match_len, out)?;
            off += match_len;
            anchor = off;
          }
        }};
      }
      match ht {
        HashTableEnum::Fast(table) => {
          table.init_dictionary(dict_len, bytes);
          let FastCompressionHashTable {
            base,
            last_off,
            end,
            hash_log,
            hash_table,
          } = table;
          let base = *base;
          let end = *end;
          let hash_log = *hash_log;
          // Dictionary initialization selects its table before scanning.
          // Keep the concrete table borrow throughout the compression scan.
          macro_rules! compress_fast {
            ($table:ident) => {{
              let bits_per_value = $table.get_bits_per_value();
              let table = $table.table.as_mut_slice();
              macro_rules! get {
                ($offset:expr) => {{
                  compression_hash_table!(
                    get,
                    base,
                    hash_log,
                    *last_off,
                    end,
                    $offset,
                    bytes,
                    |h: usize, value: i32| {
                      let previous = i32::from(table[h]);
                      debug_assert!(bits_per_value != 16 || (0..(1 << 16)).contains(&value));
                      table[h] = value as _;
                      Ok::<i32, LuceneError>(previous)
                    }
                  )
                }};
              }
              macro_rules! previous {
                ($offset:expr) => {
                  None::<i32>
                };
              }
              compress_loop!(get, previous);
            }};
          }
          match hash_table.as_mut() {
            Some(TableEnum::Table16(table)) => compress_fast!(table),
            Some(TableEnum::Table32(table)) => compress_fast!(table),
            None => return Err(LuceneError::illegal_state("hash_table not initialized")),
          }
        },
        HashTableEnum::High(table) => {
          table.init_dictionary(dict_len, bytes);
          macro_rules! get {
            ($offset:expr) => {
              compression_hash_table!(high_get, table, $offset, bytes)
            };
          }
          macro_rules! previous {
            ($offset:expr) => {
              table.previous($offset, bytes)
            };
          }
          compress_loop!(get, previous);
        },
      }
    }

    // Handle last literals
    let literal_len = end - anchor;
    debug_assert!(literal_len >= LZ4::LAST_LITERALS || literal_len == len);
    LZ4::encode_last_literals(bytes, anchor, literal_len, out)?;
    Ok(())
  }
}

/// A record of previous occurrences of sequences of 4 bytes.
pub trait HashTable {
  /// Reset this hash table in order to compress the given content.
  fn reset(&mut self, off: i32, len: i32);

  /// Init `dict_len` bytes to be used as a dictionary.
  fn init_dictionary(&mut self, dict_len: i32, bytes: &[u8]);

  /// Advance the cursor to `off` and return an index that stored the same 4
  /// bytes as `b[off:off+4]`. This may only be called on strictly
  /// increasing sequences of offsets. A return value of `None` indicates
  /// that no other index could be found.
  fn get(&mut self, off: i32, bytes: &[u8]) -> Result<Option<i32>>;

  /// Return an index that is less than `off` and stores the same 4 bytes.
  /// Unlike `get`, it doesn't need to be called on increasing offsets.
  /// A return value of `None` indicates that no other index could be found.
  fn previous(&mut self, off: i32, bytes: &[u8]) -> Option<i32>;

  /// For testing purposes.
  fn assert_reset(&self) -> bool;
}
trait Table {
  fn set(&mut self, offset: usize, value: i32);
  fn get_and_set(&mut self, offset: usize, value: i32) -> i32;
  fn get_bits_per_value(&self) -> i32;
  fn size(&self) -> usize;
}
/// 16 bits per offset. This is by far the most commonly used table since it
/// gets used whenever compressing inputs whose size is <= 64kB.
struct Table16 {
  table: Vec<u16>,
}

impl Table16 {
  pub fn new(size: usize) -> Self {
    Table16 {
      table: vec![0; size],
    }
  }
}

impl Table for Table16 {
  fn set(&mut self, index: usize, value: i32) {
    debug_assert!((0..(1 << 16)).contains(&value));
    self.table[index] = value as u16;
  }

  fn get_and_set(&mut self, index: usize, value: i32) -> i32 {
    let prev = self.table[index] as i32;
    self.set(index, value);
    prev
  }

  fn get_bits_per_value(&self) -> i32 {
    16
  }

  fn size(&self) -> usize {
    self.table.len()
  }
}
/// 32 bits per value, only used when inputs exceed 64kB, e.g. very large stored
/// fields.
pub struct Table32 {
  table: Vec<i32>,
}

impl Table32 {
  pub fn new(size: usize) -> Self {
    Table32 {
      table: vec![0; size],
    }
  }
}

impl Table for Table32 {
  fn set(&mut self, index: usize, value: i32) {
    self.table[index] = value;
  }

  fn get_and_set(&mut self, index: usize, value: i32) -> i32 {
    let prev = self.table[index];
    self.set(index, value);
    prev
  }

  fn get_bits_per_value(&self) -> i32 {
    32
  }

  fn size(&self) -> usize {
    self.table.len()
  }
}
enum TableEnum {
  Table16(Table16),
  Table32(Table32),
}
impl Table for TableEnum {
  fn set(&mut self, offset: usize, value: i32) {
    match self {
      TableEnum::Table16(table) => table.set(offset, value),
      TableEnum::Table32(table) => table.set(offset, value),
    }
  }

  fn get_and_set(&mut self, offset: usize, value: i32) -> i32 {
    match self {
      TableEnum::Table16(table) => table.get_and_set(offset, value),
      TableEnum::Table32(table) => table.get_and_set(offset, value),
    }
  }

  fn get_bits_per_value(&self) -> i32 {
    match self {
      TableEnum::Table16(table) => table.get_bits_per_value(),
      TableEnum::Table32(table) => table.get_bits_per_value(),
    }
  }

  fn size(&self) -> usize {
    match self {
      TableEnum::Table16(table) => table.size(),
      TableEnum::Table32(table) => table.size(),
    }
  }
}

/// Simple lossy [`HashTable`] that only stores the last occurrence for each hash
/// on `2^14` bytes of memory.
pub struct FastCompressionHashTable {
  base: i32,
  last_off: i32,
  end: i32,
  hash_log: i32,
  hash_table: Option<TableEnum>,
}

impl Default for FastCompressionHashTable {
  fn default() -> Self {
    Self::new()
  }
}

impl FastCompressionHashTable {
  /// Creates a new instance.
  pub fn new() -> Self {
    FastCompressionHashTable {
      base: 0,
      last_off: 0,
      end: 0,
      hash_log: 0,
      hash_table: None,
    }
  }
  pub(crate) fn clear(&mut self) {
    match self.hash_table.as_mut() {
      Some(TableEnum::Table16(table)) => table.table.fill(0),
      Some(TableEnum::Table32(table)) => table.table.fill(0),
      None => {},
    }
  }
}
impl HashTable for FastCompressionHashTable {
  fn reset(&mut self, off: i32, len: i32) {
    self.base = off;
    self.end = off + len;

    let bits_per_offset: i32 = if (len - LZ4::LAST_LITERALS) < (1 << 16) {
      16
    } else {
      32
    };

    let bits_per_offset_log = 32 - (bits_per_offset - 1).leading_zeros() as i32;
    self.hash_log = LZ4::MEMORY_USAGE + 3 - bits_per_offset_log;

    let need_new_table = match &self.hash_table {
      None => true,
      Some(table) => {
        table.size() < (1 << self.hash_log) || table.get_bits_per_value() < bits_per_offset
      },
    };

    if need_new_table {
      self.hash_table = if bits_per_offset > 16 {
        debug_assert_eq!(bits_per_offset, 32);
        Some(TableEnum::Table32(Table32::new(1 << self.hash_log)))
      } else {
        debug_assert_eq!(bits_per_offset, 16);
        Some(TableEnum::Table16(Table16::new(1 << self.hash_log)))
      };
    } else {
      // Avoid calling hashTable.clear(), this makes it costly to compress
      // many short sequences otherwise.
      // Instead, get() checks that references are less than the current
      // offset.
    }

    self.last_off = off - 1;
  }

  fn init_dictionary(&mut self, dict_len: i32, bytes: &[u8]) {
    compression_hash_table!(
      init_dictionary,
      self.base,
      self.hash_log,
      self.last_off,
      dict_len,
      bytes,
      |h: usize, i: i32| {
        debug_assert!(self.hash_table.is_some());
        if let Some(table) = &mut self.hash_table {
          table.set(h, i);
        }
      }
    );
  }

  fn get(&mut self, off: i32, bytes: &[u8]) -> Result<Option<i32>> {
    compression_hash_table!(
      get,
      self.base,
      self.hash_log,
      self.last_off,
      self.end,
      off,
      bytes,
      |h: usize, value: i32| {
        Ok::<i32, LuceneError>(
          self
            .hash_table
            .as_mut()
            .ok_or_else(|| LuceneError::illegal_state("hash_table not initialized"))?
            .get_and_set(h, value),
        )
      }
    )
  }

  fn previous(&mut self, _off: i32, _bytes: &[u8]) -> Option<i32> {
    None
  }

  fn assert_reset(&self) -> bool {
    true
  }
}
/// A higher-precision [`HashTable`]. It stores up to 256 occurrences of 4-bytes
/// sequences in the last 2^16 bytes, which makes it much more likely to find
/// matches than FastCompressionHashTable.
pub struct HighCompressionHashTable {
  base: i32,
  next: i32,
  end: i32,
  hash_table: Vec<i32>,
  chain_table: Vec<u16>,
  attempts: i32,
}

impl Default for HighCompressionHashTable {
  fn default() -> Self {
    Self::new()
  }
}

impl HighCompressionHashTable {
  const MAX_ATTEMPTS: i32 = 256;
  pub const MASK: i32 = LZ4::MAX_DISTANCE - 1;
  /// Creates a new instance.
  pub fn new() -> Self {
    HighCompressionHashTable {
      base: 0,
      next: 0,
      end: 0,
      hash_table: vec![-1; LZ4::HASH_TABLE_SIZE_HC as usize],
      chain_table: vec![0xFFFF; LZ4::MAX_DISTANCE as usize],
      attempts: 0,
    }
  }
  fn add_hash(&mut self, off: i32, bytes: &[u8]) {
    compression_hash_table!(high_add_hash, self, off, bytes);
  }
}
impl HashTable for HighCompressionHashTable {
  fn reset(&mut self, off: i32, len: i32) {
    if self.end - self.base < self.chain_table.len() as i32 {
      // The last call to compress was done on less than 64kB, let's not
      // reset the hashTable and only reset the relevant parts
      // of the chainTable. This helps avoid slowing down
      // calling compress() many times on short inputs.
      let start_offset = (self.base & Self::MASK) as usize;
      let end_offset = if self.end == 0 {
        0
      } else {
        (((self.end - 1) & Self::MASK) + 1) as usize
      };

      if start_offset < end_offset {
        self.chain_table[start_offset..end_offset]
          .iter_mut()
          .for_each(|x| *x = 0xFFFF);
      } else {
        self.chain_table[0..end_offset]
          .iter_mut()
          .for_each(|x| *x = 0xFFFF);
        self.chain_table[start_offset..]
          .iter_mut()
          .for_each(|x| *x = 0xFFFF);
      }
    } else {
      // The last call to compress was done on a large enough amount of
      // data that it's fine to reset both tables
      self.hash_table.fill(-1);
      self.chain_table.fill(0xFFFF);
    }
    self.base = off;
    self.next = off;
    self.end = off + len;
  }

  fn init_dictionary(&mut self, dict_len: i32, bytes: &[u8]) {
    debug_assert!(self.next == self.base);
    for i in 0..dict_len {
      self.add_hash(self.base + i, bytes);
    }
    self.next += dict_len;
  }

  fn get(&mut self, off: i32, bytes: &[u8]) -> Result<Option<i32>> {
    compression_hash_table!(high_get, self, off, bytes)
  }

  fn previous(&mut self, off: i32, bytes: &[u8]) -> Option<i32> {
    let v = LZ4::read_int(bytes, off);
    let mut ref_idx = off - ((self.chain_table[(off & Self::MASK) as usize] as i32) & 0xFFFF);
    while ref_idx >= self.base && self.attempts < Self::MAX_ATTEMPTS {
      if LZ4::read_int(bytes, ref_idx) == v {
        return Some(ref_idx);
      }
      ref_idx -= self.chain_table[(ref_idx & Self::MASK) as usize] as i32 & 0xFFFF;
      self.attempts += 1;
    }
    None
  }

  fn assert_reset(&self) -> bool {
    for i in 0..self.chain_table.len() {
      debug_assert!(self.chain_table[i] == 0xFFFF);
    }
    true
  }
}

pub enum HashTableEnum {
  Fast(FastCompressionHashTable),
  High(HighCompressionHashTable),
}
impl HashTable for HashTableEnum {
  fn reset(&mut self, off: i32, len: i32) {
    match self {
      HashTableEnum::Fast(table) => table.reset(off, len),
      HashTableEnum::High(table) => table.reset(off, len),
    }
  }

  fn init_dictionary(&mut self, dict_len: i32, bytes: &[u8]) {
    match self {
      HashTableEnum::Fast(table) => table.init_dictionary(dict_len, bytes),
      HashTableEnum::High(table) => table.init_dictionary(dict_len, bytes),
    }
  }

  fn get(&mut self, off: i32, bytes: &[u8]) -> Result<Option<i32>> {
    match self {
      HashTableEnum::Fast(table) => table.get(off, bytes),
      HashTableEnum::High(table) => table.get(off, bytes),
    }
  }

  fn previous(&mut self, off: i32, bytes: &[u8]) -> Option<i32> {
    match self {
      HashTableEnum::Fast(table) => table.previous(off, bytes),
      HashTableEnum::High(table) => table.previous(off, bytes),
    }
  }

  fn assert_reset(&self) -> bool {
    match self {
      HashTableEnum::Fast(table) => table.assert_reset(),
      HashTableEnum::High(table) => table.assert_reset(),
    }
  }
}
