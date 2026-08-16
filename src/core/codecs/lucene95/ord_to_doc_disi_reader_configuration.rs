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
use crate::core::codecs::indexed_disi::{
  DEFAULT_DENSE_RANK_POWER, IndexedDISI, IndexedDISIImpl, Owned,
  write_bitset_with_dense_rank_power,
};
use crate::core::index::docs_with_field_set::DocsWithFieldSet;
use crate::core::search::doc_id_set::DocIdSet;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::store::{IndexInput, IndexOutput};
use crate::core::util::TryIntoInt;
use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::packed::direct_monotonic_reader::Meta;
use crate::core::util::packed::direct_monotonic_reader::{DirectMonotonicReader, load_meta};
use crate::core::util::packed::direct_monotonic_writer::DirectMonotonicWriter;
/// Configuration for [`DirectMonotonicReader`] and [`IndexedDISI`] for reading sparse
/// vectors. The format in the static writing methods adheres to the
/// Lucene95HnswVectorsFormat
pub struct OrdToDocDISIReaderConfiguration {
  pub size: i32,

  // the following four variables used to read docIds encoded by IndexDISI
  // special values of docsWithFieldOffset are -1 and -2
  // -1 : dense
  // -2 : empty
  // other: sparse
  pub jump_table_entry_count: i16,
  pub docs_with_field_offset: i64,
  pub docs_with_field_length: usize,
  pub dense_rank_power: i8,

  // the following four variables used to read ordToDoc encoded by DirectMonotonicWriter
  // note that only spare case needs to store ordToDoc
  pub addresses_offset: usize,
  pub addresses_length: usize,
  pub meta: Option<Meta>,
}
impl OrdToDocDISIReaderConfiguration {
  /// Writes out the docsWithField and ordToDoc mapping to the outputMeta and vectorData
  /// respectively. This is in adherence to the Lucene95HnswVectorsFormat.
  ///
  /// Within outputMeta the format is as follows:
  ///
  /// - **`int8`** if equals to -2, empty - no vector values. If equals to -1, dense – all
  ///   documents have values for a field. If equals to 0, sparse – some documents missing
  ///   values.
  /// - DocIds were encoded by `IndexedDISI::writeBitSet`
  /// - OrdToDoc was encoded by [`DirectMonotonicWriter`], note
  ///   that only in sparse case
  ///
  /// Within the vectorData the format is as follows:
  ///
  /// - DocIds encoded by `IndexedDISI::writeBitSet`,
  ///   note that only in sparse case
  /// - OrdToDoc was encoded by [`DirectMonotonicWriter`],
  ///   note that only in sparse case
  ///
  /// # Arguments
  ///
  /// - `outputMeta`: the outputMeta
  /// - `vectorData`: the vectorData
  /// - `count`: the count of docs with vectors
  /// - `maxDoc`: the maxDoc for the index
  /// - `docsWithField`: the docs contaiting a vector field
  ///
  /// # Errors
  ///
  /// Returns an error when writing data fails to either output.
  pub fn write_stored_meta(
    direct_monotonic_block_shift: i32,
    output_meta: &mut impl IndexOutput,
    vector_data: &mut impl IndexOutput,
    count: i32,
    max_doc: i32,
    docs_with_field: &DocsWithFieldSet,
  ) -> Result<()> {
    if count == 0 {
      output_meta.write_long(-2)?; // docsWithFieldOffset
      output_meta.write_long(0)?; // docsWithFieldLength
      output_meta.write_short(-1)?; // jumpTableEntryCount
      output_meta.write_byte(-1i8 as u8)?; // denseRankPower
    } else if count == max_doc {
      output_meta.write_long(-1)?; // docsWithFieldOffset
      output_meta.write_long(0)?; // docsWithFieldLength
      output_meta.write_short(-1)?; // jumpTableEntryCount
      output_meta.write_byte(-1i8 as u8)?; // denseRankPower
    } else {
      let offset = vector_data.get_file_pointer()?.try_convert()?;
      output_meta.write_long(offset)?; // docsWithFieldOffset

      let jump_table_entry_count = write_bitset_with_dense_rank_power(
        &mut docs_with_field.iterator()?,
        vector_data,
        DEFAULT_DENSE_RANK_POWER,
      )?;

      let fp: i64 = vector_data.get_file_pointer()?.try_convert()?;
      output_meta.write_long(fp - offset)?; // docsWithFieldLength
      output_meta.write_short(jump_table_entry_count)?;
      output_meta.write_byte(DEFAULT_DENSE_RANK_POWER as u8)?;

      // write ordToDoc mapping
      let start = vector_data.get_file_pointer()?.try_convert()?;
      output_meta.write_long(start)?;
      output_meta.write_vint(direct_monotonic_block_shift)?;

      // dense case and empty case do not need to store ordToMap mapping
      let mut ord_to_doc_writer = DirectMonotonicWriter::get_instance(
        output_meta,
        vector_data,
        count as i64,
        direct_monotonic_block_shift,
      )?;

      let mut iterator = docs_with_field.iterator()?;
      let mut doc = iterator.next_doc()?;
      while doc != NO_MORE_DOCS {
        ord_to_doc_writer.add(doc as i64)?;
        doc = iterator.next_doc()?;
      }

      ord_to_doc_writer.finish()?;
      let fp: i64 = vector_data.get_file_pointer()?.try_convert()?;
      output_meta.write_long(fp - start)?;
    }

    Ok(())
  }
  /// Reads in the necessary fields stored in the outputMeta to configure [`DirectMonotonicReader`]
  /// and [`IndexedDISI`].
  ///
  /// # Arguments
  ///
  /// - `input_meta`: the inputMeta, previously written to via `write_stored_meta`
  /// - `size`: The number of vectors
  ///
  /// # Returns
  ///
  /// the configuration required to read sparse vectors
  ///
  /// # Errors
  ///
  /// Returns an error when reading data fails
  pub fn from_stored_meta(input_meta: &mut impl IndexInput, size: i32) -> Result<Self> {
    let docs_with_field_offset = input_meta.read_long()?;
    let docs_with_field_length = input_meta.read_long()?.try_convert()?;
    let jump_table_entry_count = input_meta.read_short()?;
    let dense_rank_power = input_meta.read_byte()? as i8;

    let mut addresses_offset = 0;
    let mut meta = None;
    let mut addresses_length = 0;

    if docs_with_field_offset > -1 {
      addresses_offset = input_meta.read_long()?.try_convert()?;
      let block_shift = input_meta.read_vint()?;
      meta = Some(load_meta(input_meta, size as i64, block_shift)?);
      addresses_length = input_meta.read_long()?.try_convert()?;
    }

    Ok(Self::new(
      size,
      jump_table_entry_count,
      addresses_offset,
      addresses_length,
      docs_with_field_offset,
      docs_with_field_length,
      dense_rank_power,
      meta,
    ))
  }
  #[allow(clippy::too_many_arguments)]
  pub fn new(
    size: i32,
    jump_table_entry_count: i16,
    addresses_offset: usize,
    addresses_length: usize,
    docs_with_field_offset: i64,
    docs_with_field_length: usize,
    dense_rank_power: i8,
    meta: Option<Meta>,
  ) -> Self {
    Self {
      size,
      jump_table_entry_count,
      addresses_offset,
      addresses_length,
      docs_with_field_offset,
      docs_with_field_length,
      dense_rank_power,
      meta,
    }
  }
  /// # Arguments
  ///
  /// - `data_in`: the dataIn
  ///
  /// # Returns
  ///
  /// the IndexedDISI for sparse values
  ///
  /// # Errors
  ///
  /// Returns an error when reading data fails
  pub fn get_indexed_disi<I>(&self, data_in: &I) -> Result<IndexedDISI<I, Owned>>
  where
    I: IndexInput,
  {
    debug_assert!(self.docs_with_field_offset > -1);

    IndexedDISIImpl::new(
      data_in,
      self.docs_with_field_offset as usize,
      self.docs_with_field_length,
      self.jump_table_entry_count as i32,
      self.dense_rank_power,
      self.size as i64,
    )
  }

  /// # Arguments
  ///
  /// - `data_in`: the dataIn
  ///
  /// # Returns
  ///
  /// the DirectMonotonicReader for sparse values
  ///
  /// # Errors
  ///
  /// Returns an error when reading data fails
  pub fn get_direct_monotonic_reader<I>(
    &self,
    data_in: &mut I,
  ) -> Result<DirectMonotonicReader<I::RandomAccessSlice>>
  where
    I: IndexInput,
  {
    debug_assert!(self.docs_with_field_offset > -1);

    let addresses_data =
      data_in.random_access_slice(self.addresses_offset, self.addresses_length)?;
    match self.meta.as_ref() {
      Some(meta) => DirectMonotonicReader::get_instance(meta, addresses_data),
      None => Err(LuceneError::illegal_state(
        "docs_with_field_offset > -1 but meta is None",
      )),
    }
  }
  /// # Returns
  ///
  /// If true, the field is empty, no vector values. If false, the field is either dense or
  /// sparse.
  pub fn is_empty(&self) -> bool {
    self.docs_with_field_offset == -2
  }

  /// # Returns
  ///
  /// If true, the field is dense, all documents have values for a field. If false, the field
  /// is sparse, some documents missing values.
  pub fn is_dense(&self) -> bool {
    self.docs_with_field_offset == -1
  }
}

impl Accountable for OrdToDocDISIReaderConfiguration {
  fn ram_bytes_used(&self) -> Result<i64> {
    match self.meta.as_ref() {
      Some(meta) => meta.ram_bytes_used(),
      None => Ok(0),
    }
  }
}
