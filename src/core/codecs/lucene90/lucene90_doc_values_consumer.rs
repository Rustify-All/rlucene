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
use crate::core::index::{BytesRefBuilder, IndexFileNames};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::search::sorted_set_selector::{
    SortedDocValuesWrapEnum, SortedSetSelector, SortedSetSelectorType,
};
use crate::core::store::directory::Directory;
use crate::core::store::{
    ByteArrayDataOutput, ByteBuffersDataOutput, ByteBuffersIndexOutput, DataOutput, IndexOutput,
};
use crate::core::util::access::SharedAccessVec;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::compress::lz4::{FastCompressionHashTable, HashTableEnum, LZ4};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::math_util::MathUtil;
use crate::core::util::packed::direct_monotonic_writer::DirectMonotonicWriter;
use crate::core::util::packed::direct_writer::{DirectWriter, unsigned_bits_required};
use crate::core::util::{CoreHelper, StringHelper};
use std::collections::{HashMap, HashSet};
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
        D2: Directory,
    {
        let data_name = IndexFileNames::segment_file_name(
            &segment_info.name,
            &state.segment_suffix,
            data_extension,
        );
        let mut data = state.directory.create_output(&data_name, state.context)?;
        CodecUtil::write_index_header(
            &mut data,
            data_codec,
            Lucene90DocValuesFormat::VERSION_CURRENT,
            segment_info.get_id(),
            &state.segment_suffix,
        )?;

        let meta_name = IndexFileNames::segment_file_name(
            &segment_info.name,
            &state.segment_suffix,
            meta_extension,
        );
        let mut meta = state.directory.create_output(&meta_name, state.context)?;
        CodecUtil::write_index_header(
            &mut meta,
            meta_codec,
            Lucene90DocValuesFormat::VERSION_CURRENT,
            segment_info.get_id(),
            &state.segment_suffix,
        )?;

        let max_doc = segment_info.max_doc()?;
        Ok(Lucene90DocValuesConsumer {
            data,
            meta,
            max_doc,
            skip_index_interval_size,
            closed: false,
        })
    }
    fn write_skip_index(
        &mut self,
        field: &Arc<FieldInfo>,
        values_producer: &impl DocValuesProducer,
    ) -> Result<()> {
        debug_assert!(*field.doc_values_skip_index_type() != DocValuesSkipIndexType::None);
        let start = self.data.get_file_pointer();
        let mut values = values_producer.get_sorted_numeric(field)?;
        let mut global_max_value = i64::MIN;
        let mut global_min_value = i64::MAX;
        let mut global_doc_count = 0;
        let mut max_doc_id = -1;

        let mut accumulators: Vec<SkipAccumulator> = Vec::new();
        let max_accumulators = 1
            << (Lucene90DocValuesFormat::SKIP_INDEX_LEVEL_SHIFT
                * (Lucene90DocValuesFormat::SKIP_INDEX_MAX_LEVEL as i32 - 1));
        let mut accumulator: Option<SkipAccumulator> = None;
        let mut doc = values.next_doc()?;
        while doc != NO_MORE_DOCS {
            let first_value = values.next_value()?;
            let done = if let Some(ref acc) = accumulator {
                acc.is_done(
                    self.skip_index_interval_size,
                    values.doc_value_count()?,
                    first_value,
                    doc,
                )
            } else {
                false
            };

            if done {
                let acc = accumulator.take().unwrap();
                global_max_value = global_max_value.max(acc.max_value);
                global_min_value = global_min_value.min(acc.min_value);
                global_doc_count += acc.doc_count;
                max_doc_id = acc.max_doc_id;
                accumulators.push(acc);
                if accumulators.len() == max_accumulators {
                    self.write_levels(std::mem::take(&mut accumulators))?;
                }
            }

            if accumulator.is_none() {
                accumulator = Some(SkipAccumulator::new(doc));
            }
            if let Some(ref mut acc) = accumulator {
                acc.next_doc(doc);
                acc.accumulate_value(first_value);
            }
            for _ in 1..values.doc_value_count()? {
                let v = values.next_value()?;
                accumulator.as_mut().unwrap().accumulate_value(v);
            }

            doc = values.next_doc()?;
        }

        if let Some(acc) = accumulator {
            global_max_value = global_max_value.max(acc.max_value);
            global_min_value = global_min_value.min(acc.min_value);
            global_doc_count += acc.doc_count;
            max_doc_id = acc.max_doc_id;
            accumulators.push(acc)
        }

        if !accumulators.is_empty() {
            self.write_levels(accumulators)?;
        }

        self.meta.write_long(start)?; // record the start in meta
        self.meta.write_long(self.data.get_file_pointer() - start)?; // record the length
        debug_assert!(global_doc_count == 0 || global_max_value >= global_min_value);
        self.meta.write_long(global_max_value)?;
        self.meta.write_long(global_min_value)?;
        debug_assert!(global_doc_count <= max_doc_id + 1);
        self.meta.write_int(global_doc_count)?;
        self.meta.write_int(max_doc_id)?;

        Ok(())
    }

    fn write_levels(&mut self, accumulators: Vec<SkipAccumulator>) -> Result<()> {
        let mut accumulators_levels: Vec<Vec<SkipAccumulator>> =
            Vec::with_capacity(Lucene90DocValuesFormat::SKIP_INDEX_MAX_LEVEL);
        let accumulators_len = accumulators.len();
        accumulators_levels.push(accumulators);

        for i in 0..(Lucene90DocValuesFormat::SKIP_INDEX_MAX_LEVEL - 1) {
            let next_level = Self::build_level(&accumulators_levels[i]);
            accumulators_levels.push(next_level);
        }

        let total = accumulators_len as i32;
        for index in 0..total {
            // compute how many levels we need to write for the current
            // accumulator
            let levels = Self::get_levels(index, total);
            // write the number of levels
            self.data.write_byte(levels as u8)?;
            // write intervals in reverse order. This is done so we don't
            // need to read all of them in case of slipping
            for level in (0..levels as usize).rev() {
                let idx =
                    index >> (Lucene90DocValuesFormat::SKIP_INDEX_LEVEL_SHIFT as usize * level);
                let acc = &accumulators_levels[level][idx as usize];
                self.data.write_int(acc.max_doc_id)?;
                self.data.write_int(acc.min_doc_id)?;
                self.data.write_long(acc.max_value)?;
                self.data.write_long(acc.min_value)?;
                self.data.write_int(acc.doc_count)?;
            }
        }

        Ok(())
    }

    fn build_level(accumulators: &[SkipAccumulator]) -> Vec<SkipAccumulator> {
        let level_size = 1 << Lucene90DocValuesFormat::SKIP_INDEX_LEVEL_SHIFT;
        let mut collector = Vec::new();

        let end = accumulators.len() as i32 - level_size + 1;
        let mut i = 0;
        while i < end {
            let merged = SkipAccumulator::merge(accumulators, i, level_size);
            collector.push(merged);
            i += level_size;
        }

        collector
    }

    fn get_levels(index: i32, size: i32) -> i32 {
        if index.trailing_zeros() >= Lucene90DocValuesFormat::SKIP_INDEX_LEVEL_SHIFT as u32 {
            let left = size - index;
            for level in (1..Lucene90DocValuesFormat::SKIP_INDEX_MAX_LEVEL as i32).rev() {
                let intervals = 1 << (Lucene90DocValuesFormat::SKIP_INDEX_LEVEL_SHIFT * level);
                if left >= intervals && index % intervals == 0 {
                    return level + 1;
                }
            }
        }
        1
    }
    fn write_values(
        &mut self,
        field: &Arc<FieldInfo>,
        values_producer: &impl DocValuesProducer,
        ords: bool,
    ) -> Result<(i32, i64)> {
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
        let mut unique_values = if !ords { Some(HashSet::new()) } else { None };

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
                        gcd = MathUtil::gcd(gcd, v - first_value);
                    }
                }

                block_min_max.update_value(v);

                if block_min_max.num_values == Lucene90DocValuesFormat::NUMERIC_BLOCK_SIZE as i64 {
                    min_max.update_from(&block_min_max);
                    block_min_max.next_block();
                }

                if let Some(set) = unique_values.as_mut()
                    && set.insert(v)
                    && set.len() > 256
                {
                    unique_values = None;
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
            let offset = self.data.get_file_pointer();
            self.meta.write_long(offset)?; // docsWithFieldOffset

            let mut values = values_producer.get_sorted_numeric(field)?;
            let jump_table_entry_count = write_bitset_with_dense_rank_power(
                &mut values,
                &mut self.data,
                DEFAULT_DENSE_RANK_POWER,
            )?;

            self.meta
                .write_long(self.data.get_file_pointer() - offset)?;
            self.meta.write_short(jump_table_entry_count)?;
            self.meta.write_byte(DEFAULT_DENSE_RANK_POWER as u8)?;
        }

        self.meta.write_long(num_values)?;

        let mut num_bits_per_value = 0;
        let mut do_blocks = false;
        let mut encode: Option<HashMap<i64, i32>> = None;

        if min >= max {
            // meta[-1]: All values are 0
            num_bits_per_value = 0;
            self.meta.write_int(-1)?;
        } else if let Some(set) = unique_values.as_ref() {
            if set.len() > 1
                && unsigned_bits_required(set.len() as i64 - 1)
                    < unsigned_bits_required((max - min) / gcd)
            {
                let mut sorted: Vec<i64> = set.iter().cloned().collect();
                sorted.sort_unstable();
                debug_assert!(sorted.len() <= i32::MAX as usize);
                let set_len = sorted.len() as i32;
                num_bits_per_value = unsigned_bits_required(set_len as i64 - 1);
                self.meta.write_int(set_len)?;
                for v in &sorted {
                    self.meta.write_long(*v)?;
                }
                encode = Some(
                    sorted
                        .iter()
                        .enumerate()
                        .map(|(i, &v)| (v, i as i32))
                        .collect::<HashMap<_, _>>(),
                );
                min = 0;
                gcd = 1;
            } else {
                // we do blocks if that appears to save 10+% storage
                do_blocks = min_max.space_in_bits > 0
                    && (block_min_max.space_in_bits as f64 / min_max.space_in_bits as f64) <= 0.9;
                if do_blocks {
                    num_bits_per_value = 0xFF;
                    self.meta
                        .write_int(-2 - Lucene90DocValuesFormat::NUMERIC_BLOCK_SHIFT)?;
                } else {
                    num_bits_per_value = unsigned_bits_required((max - min) / gcd);
                    if gcd == 1
                        && min > 0
                        && unsigned_bits_required(max) == unsigned_bits_required(max - min)
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

        let start_offset = self.data.get_file_pointer();
        self.meta.write_long(start_offset)?;
        let mut jump_table_offset = -1;

        if do_blocks {
            let mut values = values_producer.get_sorted_numeric(field)?;
            jump_table_offset = self.write_values_multiple_blocks(&mut values, gcd)?;
        } else if num_bits_per_value != 0 {
            let mut values = values_producer.get_sorted_numeric(field)?;
            self.write_values_single_block(
                &mut values,
                num_values,
                num_bits_per_value,
                min,
                gcd,
                encode,
            )?;
        }

        self.meta
            .write_long(self.data.get_file_pointer() - start_offset)?;
        self.meta.write_long(jump_table_offset)?;

        Ok((num_docs_with_value, num_values))
    }

    fn write_values_single_block(
        &mut self,
        values: &mut impl SortedNumericDocValues,
        num_values: i64,
        num_bits_per_value: i32,
        min: i64,
        gcd: i64,
        encode: Option<HashMap<i64, i32>>,
    ) -> Result<()> {
        let mut writer =
            DirectWriter::get_instance(&mut self.data, num_values, num_bits_per_value)?;

        let mut doc = values.next_doc()?;
        while doc != NO_MORE_DOCS {
            for _ in 0..values.doc_value_count()? {
                let v = values.next_value()?;
                let encoded = if let Some(map) = &encode {
                    *map.get(&v).unwrap_or(&0) as i64
                } else {
                    (v - min) / gcd
                };
                writer.add(encoded)?;
            }
            doc = values.next_doc()?;
        }

        writer.finish()?;
        Ok(())
    }

    fn write_values_multiple_blocks(
        &mut self,
        values: &mut impl SortedNumericDocValues,
        gcd: i64,
    ) -> Result<i64> {
        let mut offsets: Vec<i64> = vec![0; ArrayUtil::oversize(1, BitUtil::LONG_BYTES)];
        let mut offsets_index: usize = 0;
        let mut buffer = [0i64; Lucene90DocValuesFormat::NUMERIC_BLOCK_SIZE as usize];
        let mut encode_buffer = ByteBuffersDataOutput::new_resettable_instance();
        let mut upto = 0;

        let mut doc = values.next_doc()?;
        while doc != NO_MORE_DOCS {
            for _ in 0..values.doc_value_count()? {
                buffer[upto] = values.next_value()?;
                upto += 1;

                if upto == Lucene90DocValuesFormat::NUMERIC_BLOCK_SIZE as usize {
                    offsets.push(self.data.get_file_pointer());
                    offsets_index += 1;
                    self.write_block(&buffer, gcd, &mut encode_buffer)?;
                    upto = 0;
                }
            }
            doc = values.next_doc()?;
        }
        if upto > 0 {
            ArrayUtil::grow_with_len(&mut offsets, offsets_index);
            offsets[offsets_index] = self.data.get_file_pointer();
            offsets_index += 1;
            self.write_block(&buffer[..upto], gcd, &mut encode_buffer)?;
        }
        // All blocks has been written. Flush the offset jump-table
        let offsets_origo = self.data.get_file_pointer();
        for &offset in offsets.iter().take(offsets_index) {
            self.data.write_long(offset)?;
        }
        self.data.write_long(offsets_origo)?;
        Ok(offsets_origo)
    }

    fn write_block(
        &mut self,
        values: &[i64],
        gcd: i64,
        buffer: &mut ByteBuffersDataOutput,
    ) -> Result<()> {
        debug_assert!(!values.is_empty());

        let mut min = values[0];
        let mut max = values[0];

        for &v in &values[1..] {
            debug_assert!(((v - min).rem_euclid(gcd)) == 0);
            min = min.min(v);
            max = max.max(v);
        }

        if min == max {
            self.data.write_byte(0)?;
            self.data.write_long(min)?;
        } else {
            let bits_per_value = unsigned_bits_required((max - min) / gcd);

            buffer.reset();
            assert_eq!(buffer.size(), 0);

            let mut w = DirectWriter::get_instance(buffer, values.len() as i64, bits_per_value)?;
            for &v in values {
                w.add((v - min) / gcd)?;
            }
            w.finish()?;

            self.data.write_byte(bits_per_value as u8)?;
            self.data.write_long(min)?;
            self.data.write_int(buffer.size() as i32)?;
            buffer.copy_to(&mut self.data)?;
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

    fn add_terms_dict(&mut self, values: &mut impl SortedSetDocValues) -> Result<()> {
        let size = values.get_value_count()?;
        let meta = &mut self.meta;
        meta.write_vlong(size)?;
        let data = &mut self.data;
        let block_mask = Lucene90DocValuesFormat::TERMS_DICT_BLOCK_LZ4_MASK as i64;
        let shift = Lucene90DocValuesFormat::TERMS_DICT_BLOCK_LZ4_SHIFT;
        meta.write_int(Lucene90DocValuesFormat::DIRECT_MONOTONIC_BLOCK_SHIFT)?;

        let mut address_buffer = ByteBuffersDataOutput::new();
        let mut address_output = ByteBuffersIndexOutput::new(&mut address_buffer, "temp", "temp");
        let num_blocks = (size + block_mask) >> shift;
        let mut writer = DirectMonotonicWriter::get_instance(
            meta,
            &mut address_output,
            num_blocks,
            Lucene90DocValuesFormat::DIRECT_MONOTONIC_BLOCK_SHIFT,
        )?;

        let mut previous = BytesRefBuilder::new();
        let mut ord: i64 = 0;
        let mut start = data.get_file_pointer();
        let mut max_length = 0;
        let mut max_block_length = 0;
        {
            let mut iterator = values.terms_enum()?;

            let mut ht = HashTableEnum::Fast(FastCompressionHashTable::default());
            let terms_dict_buffer = vec![0u8; 1 << 14];
            let mut buffered_output = ByteArrayDataOutput::with_bytes(terms_dict_buffer);
            let mut dict_length = 0;
            while let Some(term) = iterator.next()? {
                let length = term.length as i32;
                let offset = term.offset as i32;
                if (ord & block_mask) == 0 {
                    if ord != 0 {
                        let uncompressed_length = Self::compress_and_get_terms_dict_block_length(
                            &mut buffered_output,
                            dict_length,
                            &mut ht,
                            data,
                        )?;
                        max_block_length = max_block_length.max(uncompressed_length);
                        buffered_output.reset()?;
                    }

                    writer.add(data.get_file_pointer() - start)?;
                    // Write the first term both to the index output, and to the
                    // buffer where we'll use it as a
                    // dictionary for compression
                    data.write_vint(length)?;
                    term.bytes.access(|bytes| {
                        data.write_bytes_range(bytes, offset, length)?;
                        Self::maybe_grow_buffer(&mut buffered_output, length)?;
                        buffered_output.write_bytes_range(bytes, offset, length)?;
                        // Help the compiler infer types.
                        Ok::<(), LuceneError>(())
                    })?;
                    dict_length = length;
                } else {
                    let prefix_length = StringHelper::bytes_difference(
                        previous.get_bytes_mut_ref(),
                        term.as_ref(),
                    )?;
                    let suffix_length = length - prefix_length;
                    debug_assert!(suffix_length > 0);
                    // Will write (suffixLength + 1 byte + 2 vint) bytes. Grow
                    // the buffer in need.
                    Self::maybe_grow_buffer(&mut buffered_output, suffix_length + 11)?;
                    buffered_output.write_byte(
                        ((prefix_length.min(15)) | ((suffix_length - 1).min(15) << 4)) as u8,
                    )?;
                    if prefix_length >= 15 {
                        buffered_output.write_vint(prefix_length - 15)?;
                    }
                    if suffix_length >= 16 {
                        buffered_output.write_vint(suffix_length - 16)?;
                    }
                    term.bytes.access(|bytes| {
                        buffered_output.write_bytes_range(
                            bytes,
                            offset + prefix_length,
                            suffix_length,
                        )?;
                        // Help the compiler infer types.
                        Ok::<(), LuceneError>(())
                    })?;
                }

                max_length = max_length.max(length);
                previous.copy_bytes_with_ref(term.as_ref());
                ord += 1;
            }
            // Compress and write out the last block
            if buffered_output.get_position() > dict_length as usize {
                let uncompressed_length = Self::compress_and_get_terms_dict_block_length(
                    &mut buffered_output,
                    dict_length,
                    &mut ht,
                    data,
                )?;
                max_block_length = max_block_length.max(uncompressed_length);
            }

            writer.finish()?;
            meta.write_int(max_length)?;
            // Write one more int for storing max block length.
            meta.write_int(max_block_length)?;
            meta.write_long(start)?;
            meta.write_long(data.get_file_pointer() - start)?;
            start = data.get_file_pointer();
            address_buffer.copy_to(data)?;
            meta.write_long(start)?;
            meta.write_long(data.get_file_pointer() - start)?;
        }
        self.write_terms_index(values)?;
        Ok(())
    }

    fn compress_and_get_terms_dict_block_length(
        buffered_output: &mut ByteArrayDataOutput<Vec<u8>>,
        dict_length: i32,
        ht: &mut HashTableEnum,
        data: &mut O,
    ) -> Result<i32> {
        debug_assert!(buffered_output.get_position() <= i32::MAX as usize);
        let uncompressed_length = buffered_output.get_position() as i32 - dict_length;
        data.write_vint(uncompressed_length)?;
        LZ4::compress_with_dictionary(
            buffered_output.bytes.as_slice(),
            0,
            dict_length,
            uncompressed_length,
            data,
            ht,
        )?;
        Ok(uncompressed_length)
    }

    fn maybe_grow_buffer(
        buffered_output: &mut ByteArrayDataOutput<Vec<u8>>,
        term_length: i32,
    ) -> Result<()> {
        let pos = buffered_output.get_position();
        let terms_dict_buffer = &mut buffered_output.bytes;
        debug_assert!(terms_dict_buffer.len() <= i32::MAX as usize);
        let original_length = terms_dict_buffer.len();
        if pos + term_length as usize >= original_length - 1 {
            ArrayUtil::grow_with_len(terms_dict_buffer, original_length + term_length as usize);
            debug_assert!(terms_dict_buffer.len() <= i32::MAX as usize);
            let terms_dict_buffer_len = terms_dict_buffer.len();
            buffered_output.reset_with_range(pos, terms_dict_buffer_len - pos)?;
        }
        Ok(())
    }

    fn write_terms_index(&mut self, values: &mut impl SortedSetDocValues) -> Result<()> {
        let size = values.get_value_count()?;
        self.meta
            .write_int(Lucene90DocValuesFormat::TERMS_DICT_REVERSE_INDEX_SHIFT)?;
        let start = self.data.get_file_pointer();

        let num_blocks = 1
            + ((size + Lucene90DocValuesFormat::TERMS_DICT_REVERSE_INDEX_MASK as i64)
                >> Lucene90DocValuesFormat::TERMS_DICT_REVERSE_INDEX_SHIFT);

        let mut address_buffer = ByteBuffersDataOutput::new();
        let mut writer;

        {
            let mut address_output =
                ByteBuffersIndexOutput::new(&mut address_buffer, "temp", "temp");
            writer = DirectMonotonicWriter::get_instance(
                &mut self.meta,
                &mut address_output,
                num_blocks,
                Lucene90DocValuesFormat::DIRECT_MONOTONIC_BLOCK_SHIFT,
            )?;

            let mut iterator = values.terms_enum()?;
            let mut previous = BytesRefBuilder::new();
            let mut offset: i64 = 0;
            let mut ord: i64 = 0;

            while let Some(term) = iterator.next()? {
                if (ord & Lucene90DocValuesFormat::TERMS_DICT_REVERSE_INDEX_MASK as i64) == 0 {
                    writer.add(offset)?;
                    let sort_key_length = if ord == 0 {
                        0
                    } else {
                        StringHelper::sort_key_length(previous.get_bytes_mut_ref(), &term)?
                    };
                    offset += sort_key_length as i64;
                    term.bytes.access(|bytes| {
                        self.data
                            .write_bytes_range(bytes, term.offset as i32, sort_key_length)?;
                        // Help the compiler infer types.
                        Ok::<(), LuceneError>(())
                    })?;
                } else if (ord & Lucene90DocValuesFormat::TERMS_DICT_REVERSE_INDEX_MASK as i64)
                    == Lucene90DocValuesFormat::TERMS_DICT_REVERSE_INDEX_MASK as i64
                {
                    previous.copy_bytes_with_ref(&term);
                }
                ord += 1;
            }

            writer.add(offset)?;
            writer.finish()?;

            self.meta.write_long(start)?;
            self.meta.write_long(self.data.get_file_pointer() - start)?;

            let start = self.data.get_file_pointer();
            address_buffer.copy_to(&mut self.data)?;
            self.meta.write_long(start)?;
            self.meta.write_long(self.data.get_file_pointer() - start)?;
        }
        Ok(())
    }
    fn do_add_sorted_numeric_field(
        &mut self,
        field: &Arc<FieldInfo>,
        values_producer: &impl DocValuesProducer,
        ords: bool,
    ) -> Result<()> {
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
            let start = self.data.get_file_pointer();
            self.meta.write_long(start)?;
            self.meta
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
            self.meta.write_long(self.data.get_file_pointer() - start)?;
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
    pub fn close(&mut self) -> Result<()> {
        if !self.closed {
            self.closed = true;
            self.meta.write_int(-1)?; // write EOF marker
            CodecUtil::write_footer(&mut self.meta)?;
            CodecUtil::write_footer(&mut self.data)?;
        }
        Ok(())
    }
}
impl<O> DocValuesConsumer for Lucene90DocValuesConsumer<O>
where
    O: IndexOutput,
{
    fn add_numeric_field<D>(&mut self, field: &Arc<FieldInfo>, values_producer: &D) -> Result<()>
    where
        D: DocValuesProducer,
    {
        self.meta.write_int(field.number)?;
        self.meta.write_byte(Lucene90DocValuesFormat::NUMERIC)?;

        let producer = EmptyDocValuesProducerSub1 {
            values_producer: Some(values_producer),
        };

        if *field.doc_values_skip_index_type() != DocValuesSkipIndexType::None {
            self.write_skip_index(field, &producer)?;
        }

        self.write_values(field, &producer, false)?;
        Ok(())
    }

    fn add_binary_field<D>(&mut self, field: &Arc<FieldInfo>, values_producer: &D) -> Result<()>
    where
        D: DocValuesProducer,
    {
        self.meta.write_int(field.number)?;
        self.meta.write_byte(Lucene90DocValuesFormat::BINARY)?;

        let mut values = values_producer.get_binary(field)?;
        let start = self.data.get_file_pointer();
        self.meta.write_long(start)?; // dataOffset
        let mut num_docs_with_field = 0;
        let mut min_length = i32::MAX;
        let mut max_length = 0;
        let mut doc = values.next_doc()?;
        while doc != NO_MORE_DOCS {
            num_docs_with_field += 1;
            let value = values.binary_value()?;
            let v = value.as_ref();
            let length = v.length as i32;
            self.data
                .write_bytes_range(&v.bytes, v.offset as i32, length)?;
            min_length = min_length.min(length);
            max_length = max_length.max(length);
            doc = values.next_doc()?;
        }

        debug_assert!(num_docs_with_field <= self.max_doc);
        self.meta.write_long(self.data.get_file_pointer() - start)?; // dataLength

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
            let offset = self.data.get_file_pointer();
            self.meta.write_long(offset)?; // docsWithFieldOffset
            let mut values = values_producer.get_binary(field)?;
            let jump_table_entry_count = write_bitset_with_dense_rank_power(
                &mut values,
                &mut self.data,
                DEFAULT_DENSE_RANK_POWER,
            )?;
            self.meta
                .write_long(self.data.get_file_pointer() - offset)?; //docsWithFieldLength
            self.meta.write_short(jump_table_entry_count)?;
            self.meta.write_byte(DEFAULT_DENSE_RANK_POWER as u8)?;
        }

        self.meta.write_int(num_docs_with_field)?;
        self.meta.write_int(min_length)?;
        self.meta.write_int(max_length)?;

        if max_length > min_length {
            let start = self.data.get_file_pointer();
            self.meta.write_long(start)?;
            self.meta
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
                let value = values.binary_value()?;
                addr += value.as_ref().length as i64;
                writer.add(addr)?;
                doc = values.next_doc()?;
            }

            writer.finish()?;
            self.meta.write_long(self.data.get_file_pointer() - start)?;
        }

        Ok(())
    }

    fn add_sorted_field<D>(&mut self, field: &Arc<FieldInfo>, values_producer: &D) -> Result<()>
    where
        D: DocValuesProducer,
    {
        self.meta.write_int(field.number)?;
        self.meta.write_byte(Lucene90DocValuesFormat::SORTED)?;
        self.do_add_sorted_field(field, values_producer, false)?;
        Ok(())
    }

    fn add_sorted_numeric_field<D>(
        &mut self,
        field: &Arc<FieldInfo>,
        values_producer: &D,
    ) -> Result<()>
    where
        D: DocValuesProducer,
    {
        self.meta.write_int(field.number)?;
        self.meta
            .write_byte(Lucene90DocValuesFormat::SORTED_NUMERIC)?;
        self.do_add_sorted_numeric_field(field, values_producer, false)?;
        Ok(())
    }

    fn add_sorted_set_field<D>(&mut self, field: &Arc<FieldInfo>, values_producer: &D) -> Result<()>
    where
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
        let result = self.close();
        match result {
            Ok(_) => (),
            Err(e) => {
                eprintln!("Failed to close Lucene90DocValuesConsumer: {e:?}")
            },
        }
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
            let bits = unsigned_bits_required(self.max - self.min);
            self.space_in_bits += bits as i64 * self.num_values;
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

    pub fn merge(list: &[SkipAccumulator], index: i32, length: i32) -> Self {
        let index = index as usize;
        let mut acc = SkipAccumulator::new(list[index].min_doc_id);
        for i in 0..length as usize {
            acc.accumulate_other(&list[index + i]);
        }
        acc
    }
}
struct EmptyDocValuesProducerSub1<'a, D>
where
    D: DocValuesProducer,
{
    // wrap with `Option` for std::mem::take
    values_producer: Option<&'a D>,
}

impl<D> Clone for EmptyDocValuesProducerSub1<'_, D>
where
    D: DocValuesProducer,
{
    fn clone(&self) -> Self {
        unreachable!(
            "{} {}",
            std::any::type_name::<Self>(),
            CoreHelper::CLONE_WARRING
        )
    }
}

impl<D> DocValuesProducer for EmptyDocValuesProducerSub1<'_, D>
where
    D: DocValuesProducer,
{
    type NumericDocValues = DummyNumericDocValues;
    type BinaryDocValues = DummyBinaryDocValues;
    type SortedDocValues = DummySortedDocValues;
    type SortedNumericDocValues = SingletonSortedNumericDocValues<D::NumericDocValues>;

    fn get_sorted_numeric(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedNumericDocValues> {
        let v = self.values_producer.as_ref().unwrap().get_numeric(field)?;
        DocValues::singleton_numeric(v)
    }
    type SortedSetDocValues = DummySortedSetDocValues;
    type DocValuesSkipper = DummyDocValuesSkipper;
}
struct EmptyDocValuesProducerSub2<'a, D>
where
    D: DocValuesProducer,
{
    values_producer: &'a D,
}

impl<D> Clone for EmptyDocValuesProducerSub2<'_, D>
where
    D: DocValuesProducer,
{
    fn clone(&self) -> Self {
        unreachable!(
            "{} {}",
            std::any::type_name::<Self>(),
            CoreHelper::CLONE_WARRING
        )
    }
}

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

pub struct EmptyDocValuesProducerSub3<'a, D>
where
    D: DocValuesProducer,
{
    values_producer: &'a D,
}

impl<D> Clone for EmptyDocValuesProducerSub3<'_, D>
where
    D: DocValuesProducer,
{
    fn clone(&self) -> Self {
        unreachable!(
            "{} {}",
            std::any::type_name::<Self>(),
            CoreHelper::CLONE_WARRING
        )
    }
}

impl<D> DocValuesProducer for EmptyDocValuesProducerSub3<'_, D>
where
    D: DocValuesProducer,
{
    type NumericDocValues = DummyNumericDocValues;
    type BinaryDocValues = DummyBinaryDocValues;
    type SortedDocValues = SortedDocValuesWrapEnum<D::SortedSetDocValues>;

    fn get_sorted(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedDocValues> {
        let sorted_set = self.values_producer.get_sorted_set(field)?;
        SortedSetSelector::wrap(sorted_set, SortedSetSelectorType::Min)
    }

    type SortedNumericDocValues = DummySortedNumericDocValues;
    type SortedSetDocValues = DummySortedSetDocValues;
    type DocValuesSkipper = DummyDocValuesSkipper;
}

pub struct EmptyDocValuesProducerSub4<'a, D>
where
    D: DocValuesProducer,
{
    values_producer: &'a D,
}

impl<D> Clone for EmptyDocValuesProducerSub4<'_, D>
where
    D: DocValuesProducer,
{
    fn clone(&self) -> Self {
        unreachable!(
            "{} {}",
            std::any::type_name::<Self>(),
            CoreHelper::CLONE_WARRING
        )
    }
}

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

pub struct NumericDocValuesImpl<S>
where
    S: SortedDocValues,
{
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
    i: i32,
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
            ArrayUtil::grow_with_len(&mut self.ords, self.doc_value_count as usize);
            for i in 0..self.doc_value_count {
                self.ords[i as usize] = self.value.next_ord()?;
            }
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
        let value = self.ords[self.i as usize];
        self.i += 1;
        Ok(value)
    }

    fn doc_value_count(&mut self) -> Result<i32> {
        Ok(self.doc_value_count)
    }

    type NumericDocValues = DummyNumericDocValues;
}
