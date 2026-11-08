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
use crate::core::codecs::CodecUtil;
use crate::core::codecs::doc_values_consumer::DocValuesConsumer;
use crate::core::codecs::doc_values_producer::DocValuesProducer;
use crate::core::codecs::dummy::dummy_binary_doc_values::DummyBinaryDocValues;
use crate::core::codecs::dummy::dummy_doc_values_skipper::DummyDocValuesSkipper;
use crate::core::codecs::dummy::dummy_numeric_doc_values::DummyNumericDocValues;
use crate::core::codecs::dummy::dummy_sorted_doc_values::DummySortedDocValues;
use crate::core::codecs::dummy::dummy_sorted_numeric_doc_values::DummySortedNumericDocValues;
use crate::core::codecs::dummy::dummy_sorted_set_doc_values::DummySortedSetDocValues;
use crate::core::codecs::indexed_disi::{
  DEFAULT_DENSE_RANK_POWER, write_bitset_with_dense_rank_power,
};
use crate::core::codecs::lucene90_doc_values_format::Lucene90DocValuesFormat;
use crate::core::index::binary_doc_values::BinaryDocValues;
use crate::core::index::doc_values::DocValues;
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::doc_values_skip_index_type::DocValuesSkipIndexType;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::singleton_sorted_numeric_doc_values::SingletonSortedNumericDocValues;
use crate::core::index::sorted_doc_values::SortedDocValues;
use crate::core::index::sorted_numeric_doc_values::SortedNumericDocValues;
use crate::core::index::sorted_set_doc_values::SortedSetDocValues;
use crate::core::index::{BytesRefBuilder, BytesRefValue};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::sorted_set_selector::{
  SortedDocValuesWrap, SortedSetSelector, SortedSetSelectorType,
};
use crate::core::store::directory::Directory;
use crate::core::store::{
  ByteArrayDataOutput, ByteBuffersDataOutput, ByteBuffersIndexOutput, DataOutput, IndexOutput,
};
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::compress::lz4::{FastCompressionHashTable, HashTableEnum, LZ4};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::math_util::MathUtil;
use crate::core::util::packed::direct_monotonic_writer::DirectMonotonicWriter;
use crate::core::util::packed::direct_writer::{DirectWriter, unsigned_bits_required};
use crate::core::util::{IOUtils, StringHelper, TryIntoInt};
use std::collections::HashMap;
use std::sync::Arc;

/// writer for [`Lucene90DocValuesFormat`].
pub struct Lucene90DocValuesConsumer<O>
where
  O: IndexOutput,
{
  data: O,
  meta: O,
  max_doc: i32,
  skip_index_interval_size: i32,
  closed: bool,
  skip_index_accumulators:
    [smallvec::SmallVec<[SkipAccumulator; 1]>; Lucene90DocValuesFormat::SKIP_INDEX_MAX_LEVEL],
  numeric_table_values: Vec<i64>,
  numeric_packed_scratch: Vec<i64>,
  numeric_unique_values: HashMap<i64, u8>,
  terms_dict_previous: BytesRefBuilder<Vec<u8>>,
  terms_dict_buffer: ByteArrayDataOutput<Vec<u8>>,
  terms_dict_hash_table: HashTableEnum,
}
impl<O: IndexOutput> Lucene90DocValuesConsumer<O> {
  /// expert: Creates a new writer
  pub fn new<D1, D2>(
    state: &SegmentWriteState<D1>,
    skip_index_interval_size: i32,
    data_codec: &str,
    data_extension: &str,
    meta_codec: &str,
    meta_extension: &str,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self>
  where
    D1: Directory<IndexOutput = O>,
  {
    let mut data = None;
    let mut meta = None;
    let mut success = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<i32> {
      data = Some(state.create_segment_output(&segment_info.name, data_extension)?);
      CodecUtil::write_index_header(
        data
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("data output is missing"))?,
        data_codec,
        Lucene90DocValuesFormat::VERSION_CURRENT,
        segment_info.get_id(),
        &state.segment_suffix,
      )?;

      meta = Some(state.create_segment_output(&segment_info.name, meta_extension)?);
      CodecUtil::write_index_header(
        meta
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("metadata output is missing"))?,
        meta_codec,
        Lucene90DocValuesFormat::VERSION_CURRENT,
        segment_info.get_id(),
        &state.segment_suffix,
      )?;

      let max_doc = segment_info.max_doc()?;
      success = true;
      Ok(max_doc)
    }));

    if !success {
      let close_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
          if let Some(meta) = meta.as_mut() {
            meta.write_int(-1)?;
            CodecUtil::write_footer(meta)?;
          }
          if let Some(data) = data.as_mut() {
            CodecUtil::write_footer(data)?;
          }
          Ok(())
        }));
      IOUtils::close_while_handling_exception((data.as_mut(), meta.as_mut()));
      resume_caught_panic!(close_result);
    }
    let max_doc = unwrap_caught_result!(result)?;
    let (data, meta) = match (data, meta) {
      (Some(data), Some(meta)) => (data, meta),
      (mut data, mut meta) => {
        IOUtils::close_while_handling_exception((data.as_mut(), meta.as_mut()));
        return Err(LuceneError::illegal_state(
          "doc values outputs are missing after successful construction",
        ));
      },
    };

    Ok(Lucene90DocValuesConsumer {
      data,
      meta,
      max_doc,
      skip_index_interval_size,
      closed: false,
      skip_index_accumulators: std::array::from_fn(|_| smallvec::SmallVec::new()),
      numeric_table_values: Vec::new(),
      numeric_packed_scratch: Vec::new(),
      numeric_unique_values: HashMap::new(),
      terms_dict_previous: BytesRefBuilder::new(),
      terms_dict_buffer: ByteArrayDataOutput::default(),
      terms_dict_hash_table: HashTableEnum::Fast(FastCompressionHashTable::default()),
    })
  }
  fn write_skip_index<T>(&mut self, field: &Arc<FieldInfo>, values_producer: &T) -> Result<()>
  where
    T: DocValuesProducer,
  {
    debug_assert!(*field.doc_values_skip_index_type() != DocValuesSkipIndexType::None);
    let start = self.data.get_file_pointer()?;
    let mut values = values_producer.get_sorted_numeric(field)?;
    let mut global_max_value = i64::MIN;
    let mut global_min_value = i64::MAX;
    let mut global_doc_count = 0;
    let mut max_doc_id = -1;

    let accumulators = &mut self.skip_index_accumulators;
    for level in accumulators.iter_mut() {
      level.clear();
    }
    let max_accumulators = 1
      << (Lucene90DocValuesFormat::SKIP_INDEX_LEVEL_SHIFT
        * (Lucene90DocValuesFormat::SKIP_INDEX_MAX_LEVEL - 1));
    let mut accumulator: Option<SkipAccumulator> = None;
    let mut doc = values.next_doc()?;
    while doc != NO_MORE_DOCS {
      let first_value = values.next_value()?;
      let completed_accumulator = match accumulator.as_ref() {
        Some(acc)
          if acc.is_done(
            self.skip_index_interval_size,
            values.doc_value_count()?,
            first_value,
            doc,
          ) =>
        {
          accumulator.take()
        },
        _ => None,
      };

      if let Some(acc) = completed_accumulator {
        global_max_value = global_max_value.max(acc.max_value);
        global_min_value = global_min_value.min(acc.min_value);
        global_doc_count += acc.doc_count;
        max_doc_id = acc.max_doc_id;
        accumulators[0].push(acc);
        if accumulators[0].len() == max_accumulators {
          Self::write_levels(&mut self.data, accumulators)?;
        }
      }

      let acc = accumulator.get_or_insert_with(|| SkipAccumulator::new(doc));
      acc.next_doc(doc);
      acc.accumulate_value(first_value);
      for _ in 1..values.doc_value_count()? {
        let v = values.next_value()?;
        acc.accumulate_value(v);
      }

      doc = values.next_doc()?;
    }

    if let Some(acc) = accumulator {
      global_max_value = global_max_value.max(acc.max_value);
      global_min_value = global_min_value.min(acc.min_value);
      global_doc_count += acc.doc_count;
      max_doc_id = acc.max_doc_id;
      accumulators[0].push(acc)
    }

    if !accumulators[0].is_empty() {
      Self::write_levels(&mut self.data, accumulators)?;
    }

    self.meta.write_long(start as i64)?; // record the start in meta
    self
      .meta
      .write_long((self.data.get_file_pointer()? - start) as i64)?; // record the length
    debug_assert!(global_doc_count == 0 || global_max_value >= global_min_value);
    self.meta.write_long(global_max_value)?;
    self.meta.write_long(global_min_value)?;
    debug_assert!(global_doc_count <= max_doc_id + 1);
    self.meta.write_int(global_doc_count)?;
    self.meta.write_int(max_doc_id)?;

    Ok(())
  }

  fn write_levels(
    data: &mut O,
    accumulators_levels: &mut [smallvec::SmallVec<[SkipAccumulator; 1]>;
           Lucene90DocValuesFormat::SKIP_INDEX_MAX_LEVEL],
  ) -> Result<()> {
    for i in 1..accumulators_levels.len() {
      let (lower, higher) = accumulators_levels.split_at_mut(i);
      Self::build_level(&lower[i - 1], &mut higher[0]);
    }

    let total = accumulators_levels[0].len();
    for index in 0..total {
      // compute how many levels we need to write for the current
      // accumulator
      let levels = Self::get_levels(index, total);
      // write the number of levels
      data.write_byte(levels as u8)?;
      // write intervals in reverse order. This is done so we don't
      // need to read all of them in case of slipping
      for level in (0..levels).rev() {
        let idx = index >> (Lucene90DocValuesFormat::SKIP_INDEX_LEVEL_SHIFT * level);
        let acc = &accumulators_levels[level][idx];
        data.write_int(acc.max_doc_id)?;
        data.write_int(acc.min_doc_id)?;
        data.write_long(acc.max_value)?;
        data.write_long(acc.min_value)?;
        data.write_int(acc.doc_count)?;
      }
    }

    accumulators_levels[0].clear();
    Ok(())
  }

  fn build_level(
    accumulators: &[SkipAccumulator],
    collector: &mut smallvec::SmallVec<[SkipAccumulator; 1]>,
  ) {
    let level_size = 1usize << Lucene90DocValuesFormat::SKIP_INDEX_LEVEL_SHIFT;
    let count = accumulators.len() / level_size;
    collector.clear();
    collector.reserve_exact(count);
    for group in 0..count {
      collector.push(SkipAccumulator::merge(
        accumulators,
        group * level_size,
        level_size,
      ));
    }
  }

  fn get_levels(index: usize, size: usize) -> usize {
    if index.trailing_zeros() >= Lucene90DocValuesFormat::SKIP_INDEX_LEVEL_SHIFT as u32 {
      let left = size - index;
      for level in (1..Lucene90DocValuesFormat::SKIP_INDEX_MAX_LEVEL).rev() {
        let intervals = 1 << (Lucene90DocValuesFormat::SKIP_INDEX_LEVEL_SHIFT * level);
        if left >= intervals && index.is_multiple_of(intervals) {
          return level + 1;
        }
      }
    }
    1
  }
  fn write_values<T>(
    &mut self,
    field: &Arc<FieldInfo>,
    values_producer: &T,
    ords: bool,
  ) -> Result<(i32, i64)>
  where
    T: DocValuesProducer,
  {
    let mut values = values_producer.get_sorted_numeric(field)?;
    let first_value = if values.next_doc()? != NO_MORE_DOCS {
      values.next_value()?
    } else {
      0
    };

    values = values_producer.get_sorted_numeric(field)?;
    let mut num_docs_with_value = 0;
    let mut min_max = MinMaxTracker::new();
    let mut block_min_max = MinMaxTracker::new();
    let mut gcd = 0i64;
    let mut unique_values = (!ords).then(|| {
      let mut values = std::mem::take(&mut self.numeric_unique_values);
      values.clear();
      values
    });
    let mut first_distinct = None;

    let mut doc = values.next_doc()?;
    while doc != NO_MORE_DOCS {
      for _ in 0..values.doc_value_count()? {
        let v = values.next_value()?;

        if gcd != 1 {
          if !(i64::MIN / 2..=i64::MAX / 2).contains(&v) {
            // in that case v - minValue might overflow and make the
            // GCD computation return
            // wrong results. Since these extreme values are
            // unlikely, we just discard GCD
            // computation for them
            gcd = 1;
          } else {
            gcd = MathUtil::gcd(gcd, v.wrapping_sub(first_value));
          }
        }

        block_min_max.update_value(v);

        if block_min_max.num_values == Lucene90DocValuesFormat::NUMERIC_BLOCK_SIZE as i64 {
          min_max.update_from(&block_min_max);
          block_min_max.next_block();
        }

        if let Some(map) = unique_values.as_mut() {
          if map.is_empty() {
            let first = *first_distinct.get_or_insert(v);
            if v != first {
              map.insert(first, 0);
              map.insert(v, 0);
            }
          } else if map.insert(v, 0).is_none()
            && map.len() > 256
            && let Some(values) = unique_values.take()
          {
            self.numeric_unique_values = values;
          }
        }
      }

      num_docs_with_value += 1;
      doc = values.next_doc()?;
    }

    min_max.update_from(&block_min_max);
    min_max.finish();
    block_min_max.finish();

    if ords && min_max.num_values > 0 {
      if min_max.min != 0 {
        return Err(LuceneError::illegal_state(format!(
          "The min value for ordinals should always be 0, got {}",
          min_max.min
        )));
      }
      if min_max.max != 0 && gcd != 1 {
        return Err(LuceneError::illegal_state(format!(
          "GCD compression should never be used on ordinals, found gcd={gcd}"
        )));
      }
    }

    let num_values = min_max.num_values;
    let mut min = min_max.min;
    let max = min_max.max;
    debug_assert!(block_min_max.space_in_bits <= min_max.space_in_bits);

    if num_docs_with_value == 0 {
      // meta[-2, 0]: No documents with values
      self.meta.write_long(-2)?; // docsWithFieldOffset
      self.meta.write_long(0)?; // docsWithFieldLength
      self.meta.write_short(-1)?; // jumpTableEntryCount
      self.meta.write_byte(-1i8 as u8)?; // denseRankPower
    } else if num_docs_with_value == self.max_doc {
      // meta[-1, 0]: All documents has values
      self.meta.write_long(-1)?; // docsWithFieldOffset
      self.meta.write_long(0)?; // docsWithFieldLength
      self.meta.write_short(-1)?; // jumpTableEntryCount
      self.meta.write_byte(-1i8 as u8)?; // denseRankPower
    } else {
      // meta[data.offset, data.length]: IndexedDISI structure
      let offset = self.data.get_file_pointer()?;
      self.meta.write_long(offset as i64)?; // docsWithFieldOffset

      let mut values = values_producer.get_sorted_numeric(field)?;
      let jump_table_entry_count =
        write_bitset_with_dense_rank_power(&mut values, &mut self.data, DEFAULT_DENSE_RANK_POWER)?;

      self
        .meta
        .write_long((self.data.get_file_pointer()? - offset) as i64)?;
      self.meta.write_short(jump_table_entry_count)?;
      self.meta.write_byte(DEFAULT_DENSE_RANK_POWER as u8)?;
    }

    self.meta.write_long(num_values)?;

    let num_bits_per_value;
    let mut do_blocks = false;
    let mut encode: Option<HashMap<i64, u8>> = None;

    if min >= max {
      // meta[-1]: All values are 0
      num_bits_per_value = 0;
      self.meta.write_int(-1)?;
    } else {
      let use_table = unique_values.as_ref().is_some_and(|set| {
        set.len() > 1
          && unsigned_bits_required(set.len() as i64 - 1)
            < unsigned_bits_required(max.wrapping_sub(min) / gcd)
      });
      let table_values = if use_table {
        unique_values.take()
      } else {
        if let Some(values) = unique_values.take() {
          self.numeric_unique_values = values;
        }
        None
      };
      if let Some(mut map) = table_values {
        let mut sorted: smallvec::SmallVec<[i64; 32]> = if map.len() > 32 {
          let mut sorted = std::mem::take(&mut self.numeric_table_values);
          sorted.clear();
          sorted.extend(map.keys().copied());
          smallvec::SmallVec::from_vec(sorted)
        } else {
          map.keys().copied().collect()
        };
        sorted.sort_unstable();
        debug_assert!(sorted.len() <= i32::MAX as usize);
        let set_len = sorted.len() as i32;
        num_bits_per_value = unsigned_bits_required(set_len as i64 - 1);
        self.meta.write_int(set_len)?;
        for v in &sorted {
          self.meta.write_long(*v)?;
        }
        for (i, &value) in sorted.iter().enumerate() {
          map.insert(value, i as u8);
        }
        if sorted.spilled() {
          self.numeric_table_values = sorted.into_vec();
        }
        encode = Some(map);
        min = 0;
        gcd = 1;
      } else {
        // we do blocks if that appears to save 10+% storage
        do_blocks = min_max.space_in_bits > 0
          && (block_min_max.space_in_bits as f64 / min_max.space_in_bits as f64) <= 0.9;
        if do_blocks {
          num_bits_per_value = 0xFF;
          self
            .meta
            .write_int(-2 - Lucene90DocValuesFormat::NUMERIC_BLOCK_SHIFT)?;
        } else {
          num_bits_per_value = unsigned_bits_required(max.wrapping_sub(min) / gcd);
          if gcd == 1
            && min > 0
            && unsigned_bits_required(max) == unsigned_bits_required(max.wrapping_sub(min))
          {
            min = 0;
          }
          self.meta.write_int(-1)?;
        }
      }
    }

    self.meta.write_byte(num_bits_per_value as u8)?;
    self.meta.write_long(min)?;
    self.meta.write_long(gcd)?;

    let start_offset = self.data.get_file_pointer()?;
    self.meta.write_long(start_offset as i64)?;
    let mut jump_table_offset = -1;

    if do_blocks {
      let mut values = values_producer.get_sorted_numeric(field)?;
      jump_table_offset = self.write_values_multiple_blocks(&mut values, gcd, num_values)?;
    } else if num_bits_per_value != 0 {
      let mut values = values_producer.get_sorted_numeric(field)?;
      self.write_values_single_block(
        &mut values,
        num_values,
        num_bits_per_value,
        min,
        gcd,
        encode.as_ref(),
      )?;
    }

    self
      .meta
      .write_long((self.data.get_file_pointer()? - start_offset) as i64)?;
    self.meta.write_long(jump_table_offset)?;

    if let Some(values) = encode.or(unique_values) {
      self.numeric_unique_values = values;
    }
    Ok((num_docs_with_value, num_values))
  }

  fn write_values_single_block<T>(
    &mut self,
    values: &mut T,
    num_values: i64,
    num_bits_per_value: i32,
    min: i64,
    gcd: i64,
    encode: Option<&HashMap<i64, u8>>,
  ) -> Result<()>
  where
    T: SortedNumericDocValues,
  {
    DirectWriter::write_with_scratch(
      &mut self.data,
      num_values,
      num_bits_per_value,
      &mut self.numeric_packed_scratch,
      |writer| match encode.as_ref() {
        Some(map) => {
          Self::write_values_with_encoder(values, writer, |v| i64::from(*map.get(&v).unwrap_or(&0)))
        },
        None => Self::write_values_with_encoder(values, writer, |v| v.wrapping_sub(min) / gcd),
      },
    )
  }

  fn write_values_with_encoder<T, E>(
    values: &mut T,
    writer: &mut DirectWriter<'_, O>,
    mut encode_value: E,
  ) -> Result<()>
  where
    T: SortedNumericDocValues,
    E: FnMut(i64) -> i64,
  {
    let mut doc = values.next_doc()?;
    while doc != NO_MORE_DOCS {
      for _ in 0..values.doc_value_count()? {
        let v = values.next_value()?;
        let encoded = encode_value(v);
        writer.add(encoded)?;
      }
      doc = values.next_doc()?;
    }

    Ok(())
  }

  fn write_values_multiple_blocks<T>(
    &mut self,
    values: &mut T,
    gcd: i64,
    num_values: i64,
  ) -> Result<i64>
  where
    T: SortedNumericDocValues,
  {
    const NUMERIC_BLOCK_SIZE: usize = Lucene90DocValuesFormat::NUMERIC_BLOCK_SIZE as usize;
    let block_count = usize::try_from(num_values)
      .ok()
      .map(|count| count.div_ceil(NUMERIC_BLOCK_SIZE))
      .filter(|&count| ArrayUtil::oversize(count, BitUtil::LONG_BYTES).is_ok())
      .unwrap_or(ArrayUtil::oversize(1, BitUtil::LONG_BYTES)?);
    let mut offsets = smallvec::SmallVec::<[i64; 16]>::with_capacity(block_count);
    let mut buffer = [0i64; NUMERIC_BLOCK_SIZE];
    let mut encode_buffer = ByteBuffersDataOutput::new_resettable_instance();
    let mut upto = 0;

    let mut doc = values.next_doc()?;
    while doc != NO_MORE_DOCS {
      for _ in 0..values.doc_value_count()? {
        buffer[upto] = values.next_value()?;
        upto += 1;

        if upto == NUMERIC_BLOCK_SIZE {
          if offsets.len() == offsets.capacity() {
            let min_size = offsets.len() + 1;
            ArrayUtil::oversize(min_size, BitUtil::LONG_BYTES)?;
            offsets.reserve(min_size.saturating_sub(offsets.len()));
          }
          offsets.push(self.data.get_file_pointer()? as i64);
          Self::write_block(
            &mut self.data,
            &buffer,
            gcd,
            &mut encode_buffer,
            &mut self.numeric_packed_scratch,
          )?;
          upto = 0;
        }
      }
      doc = values.next_doc()?;
    }
    if upto > 0 {
      if offsets.len() == offsets.capacity() {
        let min_size = offsets.len() + 1;
        ArrayUtil::oversize(min_size, BitUtil::LONG_BYTES)?;
        offsets.reserve(min_size.saturating_sub(offsets.len()));
      }
      offsets.push(self.data.get_file_pointer()? as i64);
      Self::write_block(
        &mut self.data,
        &buffer[..upto],
        gcd,
        &mut encode_buffer,
        &mut self.numeric_packed_scratch,
      )?;
    }
    // All blocks has been written. Flush the offset jump-table
    let offsets_origo = self.data.get_file_pointer()?.try_convert()?;
    for &offset in &offsets {
      self.data.write_long(offset)?;
    }
    self.data.write_long(offsets_origo)?;
    Ok(offsets_origo)
  }

  fn write_block(
    data: &mut O,
    values: &[i64],
    gcd: i64,
    buffer: &mut ByteBuffersDataOutput,
    packed_scratch: &mut Vec<i64>,
  ) -> Result<()> {
    let length = values.len();
    debug_assert!(length > 0);

    let mut min = values[0];
    let mut max = values[0];

    for &v in &values[1..] {
      debug_assert_eq!(v.wrapping_sub(min).rem_euclid(gcd), 0);
      min = min.min(v);
      max = max.max(v);
    }

    if min == max {
      data.write_byte(0)?;
      data.write_long(min)?;
    } else {
      let bits_per_value = unsigned_bits_required(max.wrapping_sub(min) / gcd);

      buffer.reset();
      debug_assert_eq!(buffer.size(), 0);
      DirectWriter::write_with_scratch(
        buffer,
        length as i64,
        bits_per_value,
        packed_scratch,
        |writer| {
          for &v in values {
            writer.add(v.wrapping_sub(min) / gcd)?;
          }
          Ok(())
        },
      )?;
      data.write_byte(bits_per_value as u8)?;
      data.write_long(min)?;
      data.write_int(buffer.size().try_convert()?)?;
      buffer.copy_to(data)?;
    }

    Ok(())
  }
  fn do_add_sorted_field<D>(
    &mut self,
    field: &Arc<FieldInfo>,
    values_producer: &D,
    add_type_byte: bool,
  ) -> Result<()>
  where
    D: DocValuesProducer,
  {
    let producer = EmptyDocValuesProducerSub2 { values_producer };

    if *field.doc_values_skip_index_type() != DocValuesSkipIndexType::None {
      self.write_skip_index(field, &producer)?;
    }

    if add_type_byte {
      self.meta.write_byte(0)?; // multiValued (0 = singleValued)
    }

    self.write_values(field, &producer, true)?;
    let mut sorted = DocValues::singleton_sorted(values_producer.get_sorted(field)?)?;
    self.add_terms_dict(&mut sorted)?;
    Ok(())
  }

  fn add_terms_dict<T>(&mut self, values: &mut T) -> Result<()>
  where
    T: SortedSetDocValues,
  {
    let size = values.get_value_count()?;
    let meta = &mut self.meta;
    meta.write_vlong(size)?;
    let data = &mut self.data;
    let block_mask = Lucene90DocValuesFormat::TERMS_DICT_BLOCK_LZ4_MASK as i64;
    let shift = Lucene90DocValuesFormat::TERMS_DICT_BLOCK_LZ4_SHIFT;
    meta.write_int(Lucene90DocValuesFormat::DIRECT_MONOTONIC_BLOCK_SHIFT)?;

    let mut address_output = ByteBuffersIndexOutput::new(
      ByteBuffersDataOutput::new_resettable_instance(),
      "temp",
      "temp",
    );
    let num_blocks = (size + block_mask) >> shift;
    let mut writer = DirectMonotonicWriter::get_instance(
      meta,
      &mut address_output,
      num_blocks,
      Lucene90DocValuesFormat::DIRECT_MONOTONIC_BLOCK_SHIFT,
    )?;

    let previous = &mut self.terms_dict_previous;
    previous.clear();
    let mut ord: i64 = 0;
    let mut start = data.get_file_pointer()?;
    let mut max_length = 0;
    let mut max_block_length = 0;

    {
      let mut iterator = values.terms_enum()?;

      let ht = &mut self.terms_dict_hash_table;
      if size > 1
        && let HashTableEnum::Fast(table) = ht
      {
        table.clear();
      }
      let buffered_output = &mut self.terms_dict_buffer;
      buffered_output.reset()?;
      let mut dict_length: usize = 0;
      while let Some(term) = iterator.next()? {
        let term = term.as_bytes_ref();
        let length = term.length;
        let offset = term.offset;
        let prefix_length;
        if (ord & block_mask) == 0 {
          prefix_length = 0;
          if ord != 0 {
            let uncompressed_length = Self::compress_and_get_terms_dict_block_length(
              buffered_output,
              dict_length,
              ht,
              data,
            )?;
            max_block_length = max_block_length.max(uncompressed_length);
            buffered_output.reset()?;
          }

          writer.add((data.get_file_pointer()? - start) as i64)?;
          // Write the first term both to the index output, and to the
          // buffer where we'll use it as a
          // dictionary for compression
          data.write_vint(length as i32)?;
          data.write_bytes_range(term.bytes, offset, length)?;
          Self::maybe_grow_buffer(buffered_output, length)?;
          buffered_output.write_bytes_range(term.bytes, offset, length)?;
          dict_length = length;
        } else {
          prefix_length = StringHelper::bytes_difference(previous.get_bytes_mut_ref(), &term)?;
          let suffix_length = length - prefix_length;
          // Will write (suffixLength + 1 byte + 2 vint) bytes. Grow
          // the buffer in need.
          Self::maybe_grow_buffer(buffered_output, suffix_length + 11)?;
          buffered_output
            .write_byte(((prefix_length.min(15)) | ((suffix_length - 1).min(15) << 4)) as u8)?;
          if prefix_length >= 15 {
            buffered_output.write_vint((prefix_length - 15) as i32)?;
          }
          if suffix_length >= 16 {
            buffered_output.write_vint((suffix_length - 16) as i32)?;
          }
          buffered_output.write_bytes_range(term.bytes, offset + prefix_length, suffix_length)?;
        }

        max_length = max_length.max(length);
        if previous.bytes().bytes.len() >= length {
          previous.set_length(prefix_length);
          previous.append_with_range(term.bytes, offset + prefix_length, length - prefix_length)?;
        } else {
          previous.copy_bytes_from_ref(&term)?;
        }
        ord += 1;
      }
      // Compress and write out the last block
      if buffered_output.get_position() > dict_length {
        let uncompressed_length =
          Self::compress_and_get_terms_dict_block_length(buffered_output, dict_length, ht, data)?;
        max_block_length = max_block_length.max(uncompressed_length);
      }
    }

    writer.finish()?;
    drop(writer);
    meta.write_int(max_length as i32)?;
    // Write one more int for storing max block length.
    meta.write_int(max_block_length)?;
    meta.write_long(start as i64)?;
    meta.write_long((data.get_file_pointer()? - start) as i64)?;
    start = data.get_file_pointer()?;
    address_output.delegate()?.copy_to(data)?;
    meta.write_long(start as i64)?;
    meta.write_long((data.get_file_pointer()? - start) as i64)?;
    address_output.delegate_mut()?.reset();
    Self::write_terms_index(data, meta, values, address_output, previous)?;
    Ok(())
  }

  fn compress_and_get_terms_dict_block_length(
    buffered_output: &mut ByteArrayDataOutput<Vec<u8>>,
    dict_length: usize,
    ht: &mut HashTableEnum,
    data: &mut O,
  ) -> Result<i32> {
    debug_assert!(buffered_output.get_position() <= i32::MAX as usize);
    let uncompressed_length = (buffered_output.get_position() - dict_length) as i32;
    data.write_vint(uncompressed_length)?;
    LZ4::compress_with_dictionary(
      buffered_output.bytes.as_slice(),
      0,
      dict_length as i32,
      uncompressed_length,
      data,
      ht,
    )?;
    Ok(uncompressed_length)
  }

  fn maybe_grow_buffer(
    buffered_output: &mut ByteArrayDataOutput<Vec<u8>>,
    term_length: usize,
  ) -> Result<()> {
    let pos = buffered_output.get_position();
    let terms_dict_buffer = &mut buffered_output.bytes;
    debug_assert!(terms_dict_buffer.len() <= i32::MAX as usize);
    let required = pos + term_length;
    if required > terms_dict_buffer.len() {
      ArrayUtil::grow_with(terms_dict_buffer, required, || 0)?;
      debug_assert!(terms_dict_buffer.len() <= i32::MAX as usize);
      let terms_dict_buffer_len = terms_dict_buffer.len();
      buffered_output.reset_with_range(pos, terms_dict_buffer_len - pos)?;
    }
    Ok(())
  }

  fn write_terms_index<T>(
    data: &mut O,
    meta: &mut O,
    values: &mut T,
    mut address_output: ByteBuffersIndexOutput,
    previous: &mut BytesRefBuilder<Vec<u8>>,
  ) -> Result<()>
  where
    T: SortedSetDocValues,
  {
    let size = values.get_value_count()?;
    meta.write_int(Lucene90DocValuesFormat::TERMS_DICT_REVERSE_INDEX_SHIFT)?;
    let start = data.get_file_pointer()?;

    let num_blocks = 1
      + ((size + Lucene90DocValuesFormat::TERMS_DICT_REVERSE_INDEX_MASK as i64)
        >> Lucene90DocValuesFormat::TERMS_DICT_REVERSE_INDEX_SHIFT);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      let mut writer = DirectMonotonicWriter::get_instance(
        meta,
        &mut address_output,
        num_blocks,
        Lucene90DocValuesFormat::DIRECT_MONOTONIC_BLOCK_SHIFT,
      )?;

      let mut iterator = values.terms_enum()?;
      previous.clear();
      let mut offset: i64 = 0;
      let mut ord: i64 = 0;

      while let Some(term) = iterator.next()? {
        let term = term.as_bytes_ref();
        if (ord & Lucene90DocValuesFormat::TERMS_DICT_REVERSE_INDEX_MASK as i64) == 0 {
          writer.add(offset)?;
          let sort_key_length = if ord == 0 {
            0
          } else {
            StringHelper::sort_key_length(previous.get_bytes_mut_ref(), &term)?
          };
          offset += sort_key_length as i64;
          data.write_bytes_range(term.bytes, term.offset, sort_key_length)?;
        } else if (ord & Lucene90DocValuesFormat::TERMS_DICT_REVERSE_INDEX_MASK as i64)
          == Lucene90DocValuesFormat::TERMS_DICT_REVERSE_INDEX_MASK as i64
        {
          previous.copy_bytes_from_ref(&term)?;
        }
        ord += 1;
      }

      writer.add(offset)?;
      writer.finish()?;
      drop(writer);

      meta.write_long(start as i64)?;
      meta.write_long((data.get_file_pointer()? - start) as i64)?;

      let start = data.get_file_pointer()?;
      let addresses = address_output.delegate()?;
      if addresses.size() != 0 {
        addresses.copy_to(data)?;
      }
      meta.write_long(start as i64)?;
      meta.write_long((data.get_file_pointer()? - start) as i64)?;
      Ok(())
    }));
    let close_result =
      std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| address_output.close()));
    IOUtils::use_or_suppress_caught_result(result, close_result)
  }
  fn do_add_sorted_numeric_field<T>(
    &mut self,
    field: &Arc<FieldInfo>,
    values_producer: &T,
    ords: bool,
  ) -> Result<()>
  where
    T: DocValuesProducer,
  {
    if *field.doc_values_skip_index_type() != DocValuesSkipIndexType::None {
      self.write_skip_index(field, values_producer)?;
    }

    if ords {
      self.meta.write_byte(1)?; // multiValued (1 = multiValued)
    }

    let (num_docs_with_field, num_values) = self.write_values(field, values_producer, ords)?;
    debug_assert!(num_values >= num_docs_with_field as i64);

    self.meta.write_int(num_docs_with_field)?;
    if num_values > num_docs_with_field as i64 {
      let start = self.data.get_file_pointer()?;
      self.meta.write_long(start as i64)?;
      self
        .meta
        .write_vint(Lucene90DocValuesFormat::DIRECT_MONOTONIC_BLOCK_SHIFT)?;

      let count = num_docs_with_field as i64 + 1;
      let mut addresses_writer = DirectMonotonicWriter::get_instance(
        &mut self.meta,
        &mut self.data,
        count,
        Lucene90DocValuesFormat::DIRECT_MONOTONIC_BLOCK_SHIFT,
      )?;

      let mut addr = 0i64;
      addresses_writer.add(addr)?;

      let mut values = values_producer.get_sorted_numeric(field)?;
      let mut doc = values.next_doc()?;
      while doc != NO_MORE_DOCS {
        addr += values.doc_value_count()? as i64;
        addresses_writer.add(addr)?;
        doc = values.next_doc()?;
      }

      addresses_writer.finish()?;
      self
        .meta
        .write_long((self.data.get_file_pointer()? - start) as i64)?;
    }

    Ok(())
  }
  fn is_single_valued<S>(values: &mut S) -> Result<bool>
  where
    S: SortedSetDocValues,
  {
    if values.is_single_valued() {
      return Ok(true);
    }
    debug_assert_eq!(values.doc_id(), -1);

    let mut doc = values.next_doc()?;
    while doc != NO_MORE_DOCS {
      let count = values.doc_value_count()?;
      debug_assert!(count > 0);
      if count > 1 {
        return Ok(false);
      }
      doc = values.next_doc()?;
    }

    Ok(true)
  }
}
impl<O> Closeable for Lucene90DocValuesConsumer<O>
where
  O: IndexOutput,
{
  fn close(&mut self) -> Result<()> {
    if self.closed {
      return Ok(());
    }
    self.closed = true;

    let mut success = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      self.meta.write_int(-1)?; // write EOF marker
      CodecUtil::write_footer(&mut self.meta)?;
      CodecUtil::write_footer(&mut self.data)?;
      success = true;
      Ok(())
    }));
    if success {
      IOUtils::close([&mut self.data, &mut self.meta])?;
    } else {
      IOUtils::close_while_handling_exception((&mut self.data, &mut self.meta));
    }
    unwrap_caught_result!(result)
  }
}
impl<O> DocValuesConsumer for Lucene90DocValuesConsumer<O>
where
  O: IndexOutput,
{
  type IndexOutput = O;

  fn add_numeric_field<D1, D2, D>(
    &mut self,
    _write_state: &SegmentWriteState<D1>,
    _segment_info: &SegmentInfo<D2>,
    field: &Arc<FieldInfo>,
    values_producer: &D,
  ) -> Result<()>
  where
    D1: Directory<IndexOutput = Self::IndexOutput>,
    D: DocValuesProducer,
  {
    self.meta.write_int(field.number)?;
    self.meta.write_byte(Lucene90DocValuesFormat::NUMERIC)?;

    let producer = EmptyDocValuesProducerSub1 { values_producer };

    if *field.doc_values_skip_index_type() != DocValuesSkipIndexType::None {
      self.write_skip_index(field, &producer)?;
    }

    self.write_values(field, &producer, false)?;
    Ok(())
  }

  fn add_binary_field<D1, D2, D>(
    &mut self,
    _write_state: &SegmentWriteState<D1>,
    _segment_info: &SegmentInfo<D2>,
    field: &Arc<FieldInfo>,
    values_producer: &D,
  ) -> Result<()>
  where
    D1: Directory<IndexOutput = Self::IndexOutput>,
    D: DocValuesProducer,
  {
    self.meta.write_int(field.number)?;
    self.meta.write_byte(Lucene90DocValuesFormat::BINARY)?;

    let (start, num_docs_with_field, min_length, max_length) = {
      let mut values = values_producer.get_binary(field)?;
      let start = self.data.get_file_pointer()?;
      self.meta.write_long(start as i64)?; // dataOffset
      let mut num_docs_with_field = 0;
      let mut min_length = i32::MAX as usize;
      let mut max_length = 0;
      let mut doc = values.next_doc()?;
      while doc != NO_MORE_DOCS {
        num_docs_with_field += 1;
        let length = {
          let value = values.binary_value()?;
          let bytes = value.as_bytes();
          self.data.write_bytes_range(bytes, 0, bytes.len())?;
          bytes.len()
        };
        min_length = min_length.min(length);
        max_length = max_length.max(length);
        doc = values.next_doc()?;
      }
      (start, num_docs_with_field, min_length, max_length)
    };

    debug_assert!(num_docs_with_field <= self.max_doc);
    self
      .meta
      .write_long((self.data.get_file_pointer()? - start) as i64)?; // dataLength

    if num_docs_with_field == 0 {
      self.meta.write_long(-2)?; // docsWithFieldOffset
      self.meta.write_long(0)?; // docsWithFieldLength
      self.meta.write_short(-1)?; // jumpTableEntryCount
      self.meta.write_byte(-1i8 as u8)?; // denseRankPower
    } else if num_docs_with_field == self.max_doc {
      self.meta.write_long(-1)?; // docsWithFieldOffset
      self.meta.write_long(0)?; // docsWithFieldLength
      self.meta.write_short(-1)?; // jumpTableEntryCount
      self.meta.write_byte(-1i8 as u8)?; // denseRankPower
    } else {
      let offset = self.data.get_file_pointer()?;
      self.meta.write_long(offset as i64)?; // docsWithFieldOffset
      let mut values = values_producer.get_binary(field)?;
      let jump_table_entry_count =
        write_bitset_with_dense_rank_power(&mut values, &mut self.data, DEFAULT_DENSE_RANK_POWER)?;
      self
        .meta
        .write_long((self.data.get_file_pointer()? - offset) as i64)?; //docsWithFieldLength
      self.meta.write_short(jump_table_entry_count)?;
      self.meta.write_byte(DEFAULT_DENSE_RANK_POWER as u8)?;
    }

    self.meta.write_int(num_docs_with_field)?;
    self.meta.write_int(min_length as i32)?;
    self.meta.write_int(max_length as i32)?;

    if max_length > min_length {
      let start = self.data.get_file_pointer()?;
      self.meta.write_long(start as i64)?;
      self
        .meta
        .write_vint(Lucene90DocValuesFormat::DIRECT_MONOTONIC_BLOCK_SHIFT)?;

      let mut writer = DirectMonotonicWriter::get_instance(
        &mut self.meta,
        &mut self.data,
        num_docs_with_field as i64 + 1,
        Lucene90DocValuesFormat::DIRECT_MONOTONIC_BLOCK_SHIFT,
      )?;

      let mut addr = 0i64;
      writer.add(addr)?;

      let mut values = values_producer.get_binary(field)?;
      let mut doc = values.next_doc()?;
      while doc != NO_MORE_DOCS {
        addr += {
          let value = values.binary_value()?;
          value.as_bytes().len() as i64
        };
        writer.add(addr)?;
        doc = values.next_doc()?;
      }

      writer.finish()?;
      self
        .meta
        .write_long((self.data.get_file_pointer()? - start) as i64)?;
    }

    Ok(())
  }

  fn add_sorted_field<D1, D2, D>(
    &mut self,
    _write_state: &SegmentWriteState<D1>,
    _segment_info: &SegmentInfo<D2>,
    field: &Arc<FieldInfo>,
    values_producer: &D,
  ) -> Result<()>
  where
    D1: Directory<IndexOutput = Self::IndexOutput>,
    D: DocValuesProducer,
  {
    self.meta.write_int(field.number)?;
    self.meta.write_byte(Lucene90DocValuesFormat::SORTED)?;
    self.do_add_sorted_field(field, values_producer, false)?;
    Ok(())
  }

  fn add_sorted_numeric_field<D1, D2, D>(
    &mut self,
    _write_state: &SegmentWriteState<D1>,
    _segment_info: &SegmentInfo<D2>,
    field: &Arc<FieldInfo>,
    values_producer: &D,
  ) -> Result<()>
  where
    D1: Directory<IndexOutput = Self::IndexOutput>,
    D: DocValuesProducer,
  {
    self.meta.write_int(field.number)?;
    self
      .meta
      .write_byte(Lucene90DocValuesFormat::SORTED_NUMERIC)?;
    self.do_add_sorted_numeric_field(field, values_producer, false)?;
    Ok(())
  }

  fn add_sorted_set_field<D1, D2, D>(
    &mut self,
    _write_state: &SegmentWriteState<D1>,
    _segment_info: &SegmentInfo<D2>,
    field: &Arc<FieldInfo>,
    values_producer: &D,
  ) -> Result<()>
  where
    D1: Directory<IndexOutput = Self::IndexOutput>,
    D: DocValuesProducer,
  {
    self.meta.write_int(field.number)?;
    self.meta.write_byte(Lucene90DocValuesFormat::SORTED_SET)?;

    let mut sorted_set = values_producer.get_sorted_set(field)?;
    if Self::is_single_valued(&mut sorted_set)? {
      let producer = EmptyDocValuesProducerSub3 { values_producer };
      self.do_add_sorted_field(field, &producer, true)?;
      return Ok(());
    }

    let producer = EmptyDocValuesProducerSub4 { values_producer };
    self.do_add_sorted_numeric_field(field, &producer, true)?;
    self.add_terms_dict(&mut values_producer.get_sorted_set(field)?)?;
    Ok(())
  }
}
impl<O> Drop for Lucene90DocValuesConsumer<O>
where
  O: IndexOutput,
{
  fn drop(&mut self) {
    let _ = Closeable::close(self);
  }
}

pub struct MinMaxTracker {
  min: i64,
  max: i64,
  num_values: i64,
  pub space_in_bits: i64,
}

impl Default for MinMaxTracker {
  fn default() -> Self {
    Self::new()
  }
}

impl MinMaxTracker {
  pub fn new() -> Self {
    let mut result = Self {
      min: 0,
      max: 0,
      num_values: 0,
      space_in_bits: 0,
    };
    result.reset();
    result
  }

  fn reset(&mut self) {
    self.min = i64::MAX;
    self.max = i64::MIN;
    self.num_values = 0;
  }

  /// Accumulate a new value
  pub fn update_value(&mut self, v: i64) {
    self.min = self.min.min(v);
    self.max = self.max.max(v);
    self.num_values += 1;
  }

  /// Accumulate state from another tracker
  pub fn update_from(&mut self, other: &MinMaxTracker) {
    self.min = self.min.min(other.min);
    self.max = self.max.max(other.max);
    self.num_values += other.num_values;
  }

  /// Update the required space
  pub fn finish(&mut self) {
    if self.max > self.min {
      let bits = unsigned_bits_required(self.max.wrapping_sub(self.min));
      self.space_in_bits = self
        .space_in_bits
        .wrapping_add((bits as i64).wrapping_mul(self.num_values));
    }
  }

  /// Update space usage and get ready for next block
  pub fn next_block(&mut self) {
    self.finish();
    self.reset();
  }
}

pub struct SkipAccumulator {
  min_doc_id: i32,
  max_doc_id: i32,
  doc_count: i32,
  min_value: i64,
  max_value: i64,
}

impl SkipAccumulator {
  pub fn new(doc_id: i32) -> Self {
    Self {
      min_doc_id: doc_id,
      max_doc_id: doc_id,
      doc_count: 0,
      min_value: i64::MAX,
      max_value: i64::MIN,
    }
  }

  pub fn is_done(
    &self,
    skip_index_interval_size: i32,
    value_count: i32,
    next_value: i64,
    next_doc: i32,
  ) -> bool {
    if self.doc_count < skip_index_interval_size {
      return false;
    }
    // Once we reach the interval size, we will keep accepting documents if
    // - next doc value is not a multi-value
    // - current accumulator only contains a single value and next value is the same
    //   value
    // - the accumulator is dense and the next doc keeps the density (no gaps)
    value_count > 1
      || self.min_value != self.max_value
      || self.min_value != next_value
      || self.doc_count != next_doc - self.min_doc_id
  }

  pub fn accumulate_value(&mut self, value: i64) {
    self.min_value = self.min_value.min(value);
    self.max_value = self.max_value.max(value);
  }

  pub fn accumulate_other(&mut self, other: &SkipAccumulator) {
    debug_assert!(self.min_doc_id <= other.min_doc_id && self.max_doc_id < other.max_doc_id);
    self.max_doc_id = other.max_doc_id;
    self.min_value = self.min_value.min(other.min_value);
    self.max_value = self.max_value.max(other.max_value);
    self.doc_count += other.doc_count;
  }

  pub fn next_doc(&mut self, doc_id: i32) {
    self.max_doc_id = doc_id;
    self.doc_count += 1;
  }

  pub fn merge(list: &[SkipAccumulator], index: usize, length: usize) -> Self {
    let mut acc = SkipAccumulator::new(list[index].min_doc_id);
    for i in 0..length {
      acc.accumulate_other(&list[index + i]);
    }
    acc
  }
}
struct EmptyDocValuesProducerSub1<'a, D> {
  values_producer: &'a D,
}

impl<D> CloseableRef for EmptyDocValuesProducerSub1<'_, D> {}

impl<D> DocValuesProducer for EmptyDocValuesProducerSub1<'_, D>
where
  D: DocValuesProducer,
{
  type NumericDocValues = DummyNumericDocValues;
  type BinaryDocValues = DummyBinaryDocValues;
  type SortedDocValues = DummySortedDocValues;
  type SortedNumericDocValues = SingletonSortedNumericDocValues<D::NumericDocValues>;

  fn get_sorted_numeric(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedNumericDocValues> {
    let v = self.values_producer.get_numeric(field)?;
    DocValues::singleton_numeric(v)
  }
  type SortedSetDocValues = DummySortedSetDocValues;
  type DocValuesSkipper = DummyDocValuesSkipper;
}
struct EmptyDocValuesProducerSub2<'a, D> {
  values_producer: &'a D,
}

impl<D> CloseableRef for EmptyDocValuesProducerSub2<'_, D> {}

impl<D> DocValuesProducer for EmptyDocValuesProducerSub2<'_, D>
where
  D: DocValuesProducer,
{
  type NumericDocValues = DummyNumericDocValues;
  type BinaryDocValues = DummyBinaryDocValues;
  type SortedDocValues = DummySortedDocValues;
  type SortedNumericDocValues =
    SingletonSortedNumericDocValues<NumericDocValuesImpl<D::SortedDocValues>>;

  fn get_sorted_numeric(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedNumericDocValues> {
    let sorted = self.values_producer.get_sorted(field)?;
    let sorted_ords = NumericDocValuesImpl { sorted };
    DocValues::singleton_numeric(sorted_ords)
  }

  type SortedSetDocValues = DummySortedSetDocValues;
  type DocValuesSkipper = DummyDocValuesSkipper;
}

pub struct EmptyDocValuesProducerSub3<'a, D> {
  values_producer: &'a D,
}

impl<D> CloseableRef for EmptyDocValuesProducerSub3<'_, D> {}

impl<D> DocValuesProducer for EmptyDocValuesProducerSub3<'_, D>
where
  D: DocValuesProducer,
{
  type NumericDocValues = DummyNumericDocValues;
  type BinaryDocValues = DummyBinaryDocValues;
  type SortedDocValues = SortedDocValuesWrap<D::SortedSetDocValues>;

  fn get_sorted(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedDocValues> {
    let sorted_set = self.values_producer.get_sorted_set(field)?;
    SortedSetSelector::wrap(sorted_set, SortedSetSelectorType::Min)
  }

  type SortedNumericDocValues = DummySortedNumericDocValues;
  type SortedSetDocValues = DummySortedSetDocValues;
  type DocValuesSkipper = DummyDocValuesSkipper;
}

pub struct EmptyDocValuesProducerSub4<'a, D> {
  values_producer: &'a D,
}

impl<D> CloseableRef for EmptyDocValuesProducerSub4<'_, D> {}

impl<D> DocValuesProducer for EmptyDocValuesProducerSub4<'_, D>
where
  D: DocValuesProducer,
{
  type NumericDocValues = DummyNumericDocValues;
  type BinaryDocValues = DummyBinaryDocValues;
  type SortedDocValues = DummySortedDocValues;
  type SortedNumericDocValues = SortedNumericDocValuesImpl<D>;

  fn get_sorted_numeric(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedNumericDocValues> {
    let value = self.values_producer.get_sorted_set(field)?;
    Ok(SortedNumericDocValuesImpl {
      ords: vec![],
      i: 0,
      doc_value_count: 0,
      value,
    })
  }

  type SortedSetDocValues = DummySortedSetDocValues;
  type DocValuesSkipper = DummyDocValuesSkipper;
}

pub struct NumericDocValuesImpl<S> {
  sorted: S,
}

impl<S> DocValuesIterator for NumericDocValuesImpl<S>
where
  S: SortedDocValues,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    self.sorted.advance_exact(target)
  }
}

impl<S> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for NumericDocValuesImpl<S>
where
  S: SortedDocValues,
{
}
impl<S> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for NumericDocValuesImpl<S> where
  S: SortedDocValues
{
}

impl<S> DocIdSetIterator for NumericDocValuesImpl<S>
where
  S: SortedDocValues,
{
  fn doc_id(&self) -> i32 {
    self.sorted.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.sorted.next_doc()
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.sorted.advance(target)
  }

  fn cost(&self) -> Result<i64> {
    self.sorted.cost()
  }
}

impl<S> NumericDocValues for NumericDocValuesImpl<S>
where
  S: SortedDocValues,
{
  fn long_value(&mut self) -> Result<i64> {
    Ok(self.sorted.ord_value()? as i64)
  }
}
pub struct SortedNumericDocValuesImpl<D>
where
  D: DocValuesProducer,
{
  ords: Vec<i64>,
  i: usize,
  doc_value_count: i32,
  value: D::SortedSetDocValues,
}

impl<D> DocValuesIterator for SortedNumericDocValuesImpl<D>
where
  D: DocValuesProducer,
{
  fn advance_exact(&mut self, _target: i32) -> Result<bool> {
    Err(LuceneError::unsupported_operation(""))
  }
}

impl<D> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SortedNumericDocValuesImpl<D>
where
  D: DocValuesProducer,
{
}
impl<D> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for SortedNumericDocValuesImpl<D>
where
  D: DocValuesProducer,
{
}

impl<D> DocIdSetIterator for SortedNumericDocValuesImpl<D>
where
  D: DocValuesProducer,
{
  fn doc_id(&self) -> i32 {
    self.value.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    let doc = self.value.next_doc()?;
    if doc != NO_MORE_DOCS {
      self.doc_value_count = self.value.doc_value_count()?;
      let count = self.doc_value_count as usize;
      ArrayUtil::grow_capacity(&mut self.ords, count)?;
      self.ords.clear();
      self.value.append_ords(&mut self.ords, count)?;
      self.i = 0;
    }
    Ok(doc)
  }

  fn advance(&mut self, _target: i32) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn cost(&self) -> Result<i64> {
    self.value.cost()
  }
}

impl<D> SortedNumericDocValues for SortedNumericDocValuesImpl<D>
where
  D: DocValuesProducer,
{
  fn next_value(&mut self) -> Result<i64> {
    let value = self.ords[self.i];
    self.i += 1;
    Ok(value)
  }

  fn doc_value_count(&mut self) -> Result<i32> {
    Ok(self.doc_value_count)
  }

  type NumericDocValues = DummyNumericDocValues;
}
