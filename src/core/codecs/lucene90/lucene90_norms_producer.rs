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
use crate::core::codecs::doc_values_enum::norms::Lucene90NormNumericDocValuesEnum;

use crate::core::codecs::indexed_disi::{
  IndexedDISIEnum, create_block_slice, create_jump_table,
};
use crate::core::codecs::lucene90::indexed_disi::{IndexInputImpl, IndexedDISIImpl};
use crate::core::codecs::lucene90_norms_format::Lucene90NormsFormat;
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::index::IndexFileNames;
use crate::core::index::doc_values::DocValues;
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::store::directory::Directory;
use crate::core::store::random_access_input::RandomAccessInput;
use crate::core::store::{IndexInput, ReadAdvice};
use crate::core::util::IOUtils;
use crate::core::util::TryIntoInt;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

/// Reader for [`Lucene90NormsFormat`]
pub struct Lucene90NormsProducer<I>
where
  I: IndexInput,
{
  // metadata maps (just file pointers and minimal stuff)
  norms: HashMap<i32, NormsEntry>,
  max_doc: i32,
  data: I,
  merging: bool,

  // reused slice while merging
  disi_inputs: Mutex<HashMap<i32, Arc<Mutex<I::IndexInput>>>>,
  #[allow(clippy::type_complexity)]
  disi_jump_tables: Mutex<HashMap<i32, Option<Arc<Mutex<I::RandomAccessSlice>>>>>,
  data_inputs: Mutex<HashMap<i32, Arc<Mutex<I::RandomAccessSlice>>>>,
}

impl<I> Lucene90NormsProducer<I>
where
  I: IndexInput,
{
  pub fn new<D1, D2>(
    state: &SegmentReadState<D1>,
    data_codec: &str,
    data_extension: &str,
    meta_codec: &str,
    meta_extension: &str,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self>
  where
    D1: Directory<IndexInput = I>,
  {
    let max_doc = segment_info.max_doc()?;
    let meta_name =
      IndexFileNames::segment_file_name(&segment_info.name, &state.segment_suffix, meta_extension);
    let mut version = -1;

    // Read in the entries from the metadata file
    let mut norms = HashMap::new();
    let mut input = state.directory.open_checksum_input(&meta_name)?;

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      let prior_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
          version = CodecUtil::check_index_header(
            &mut input,
            meta_codec,
            Lucene90NormsFormat::VERSION_START,
            Lucene90NormsFormat::VERSION_CURRENT,
            segment_info.get_id(),
            &state.segment_suffix,
          )?;
          norms = Self::read_fields(&mut input, &state.field_infos)?;
          Ok(())
        }));
      let prior_result = match prior_result {
        Ok(Ok(())) => None,
        prior_result => Some(prior_result),
      };
      CodecUtil::check_footer_with_error(&mut input, prior_result)
    }));
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| input.close()));
    IOUtils::use_or_suppress_caught_result(result, close_result)?;

    let data_name =
      IndexFileNames::segment_file_name(&segment_info.name, &state.segment_suffix, data_extension);

    // Norms have a forward-only access pattern, so pass ReadAdvice::Normal
    // to perform readahead
    let mut data = Some(state.directory.open_input(
      &data_name,
      &state.context.with_read_advice_self(ReadAdvice::Normal)?,
    )?);

    // Check header again in the data file
    let mut success = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<Self> {
      let data_ref = data
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("norms data input is missing"))?;
      let version2 = CodecUtil::check_index_header(
        data_ref,
        data_codec,
        Lucene90NormsFormat::VERSION_START,
        Lucene90NormsFormat::VERSION_CURRENT,
        segment_info.get_id(),
        &state.segment_suffix,
      )?;

      if version != version2 {
        return Err(LuceneError::corrupt_index(format!(
          "Format versions mismatch: meta={version}, data={version2} (resource={data_ref})"
        )));
      }
      // NOTE: data file is too costly to verify checksum against all the
      // bytes on open, but for now we at least verify proper
      // structure of the checksum footer: which looks
      // for FOOTER_MAGIC + algorithmID. This is cheap and can detect some
      // forms of corruption such as file truncation.
      CodecUtil::retrieve_checksum(data_ref)?;

      let data = data
        .take()
        .ok_or_else(|| LuceneError::illegal_state("norms data input is missing"))?;
      success = true;
      Ok(Self {
        norms,
        max_doc,
        data,
        merging: false,
        disi_inputs: HashMap::new().into(),
        disi_jump_tables: HashMap::new().into(),
        data_inputs: HashMap::new().into(),
      })
    }));
    if !success {
      IOUtils::close_while_handling_exception(data.as_ref());
    }
    unwrap_caught_result!(result)
  }
  fn read_fields(
    meta: &mut impl IndexInput,
    field_infos: &Arc<FieldInfos>,
  ) -> Result<HashMap<i32, NormsEntry>> {
    let mut norms = HashMap::new();
    loop {
      let field_number = meta.read_int()?;
      if field_number == -1 {
        break;
      }

      let info = field_infos.field_info_by_number(field_number)?;
      let info_number;
      match &info {
        None => {
          return Err(LuceneError::corrupt_index(format!(
            "invalid field number: {field_number} (resource={meta})"
          )));
        },
        Some(info) => {
          info_number = info.number;
          if !info.has_norms() {
            return Err(LuceneError::corrupt_index(format!(
              "Invalid field (no norms): {}",
              info.name
            )));
          }
        },
      }
      let docs_with_field_offset = meta.read_long()?;
      let docs_with_field_length = meta.read_long()?;
      let jump_table_entry_count = meta.read_short()?;

      let dense_rank_power = meta.read_byte()? as i8;
      let num_docs_with_field = meta.read_int()?.try_convert()?;
      let bytes_per_norm = meta.read_byte()? as i8;

      match bytes_per_norm {
        0 | 1 | 2 | 4 | 8 => {},
        _ => {
          return Err(LuceneError::corrupt_index(format!(
            "Invalid bytesPerValue: {}, field: {}(resource={})",
            bytes_per_norm,
            info.as_ref().unwrap().name,
            meta
          )));
        },
      }

      let norms_offset = meta.read_long()?;

      norms.insert(
        info_number,
        NormsEntry {
          dense_rank_power,
          bytes_per_norm,
          docs_with_field_offset,
          docs_with_field_length,
          jump_table_entry_count,
          num_docs_with_field,
          norms_offset,
        },
      );
    }
    Ok(norms)
  }
  fn get_data_input(
    &self,
    field: &FieldInfo,
    entry: &NormsEntry,
  ) -> Result<RandomAccessSliceEnum<I::RandomAccessSlice>> {
    if self.merging
      && let Some(existing) = self.data_inputs.lock().get(&field.number)
    {
      return Ok(RandomAccessSliceEnum::Shared(Arc::clone(existing)));
    }

    let length = entry.num_docs_with_field * entry.bytes_per_norm as usize;
    let mut slice = self
      .data
      .random_access_slice(entry.norms_offset.try_convert()?, length)?;
    // Prefetch the first page of data. Following pages are expected to get
    // prefetched through read-ahead.
    if slice.length()? > 0 {
      slice.prefetch(0, 1)?;
    }

    if self.merging {
      let slice_rc = Arc::new(Mutex::new(slice));
      if self.merging {
        self
          .data_inputs
          .lock()
          .insert(field.number, Arc::clone(&slice_rc));
      }

      Ok(RandomAccessSliceEnum::Shared(slice_rc))
    } else {
      Ok(RandomAccessSliceEnum::Owned(slice))
    }
  }
  fn get_disi_jump_table(
    &self,
    field: &Arc<FieldInfo>,
    entry: &NormsEntry,
  ) -> Result<Option<RandomAccessSliceEnum<I::RandomAccessSlice>>> {
    if self.merging {
      if let Some(cached) = {
        let map = self.disi_jump_tables.lock();
        map.get(&field.number).cloned()
      } {
        return Ok(cached.map(RandomAccessSliceEnum::Shared));
      }

      let created = create_jump_table(
        &self.data,
        entry.docs_with_field_offset as usize,
        entry.docs_with_field_length as usize,
        entry.jump_table_entry_count as i32,
      )?;

      let final_value = {
        let mut map = self.disi_jump_tables.lock();
        map
          .entry(field.number)
          .or_insert_with(|| created.map(|jt| Arc::new(Mutex::new(jt))))
          .clone()
      };

      Ok(final_value.map(RandomAccessSliceEnum::Shared))
    } else {
      let jump_table = create_jump_table(
        &self.data,
        entry.docs_with_field_offset as usize,
        entry.docs_with_field_length as usize,
        entry.jump_table_entry_count as i32,
      )?;
      Ok(jump_table.map(RandomAccessSliceEnum::Owned))
    }
  }
}
pub enum RandomAccessSliceEnum<R> {
  Shared(Arc<Mutex<R>>),
  Owned(R),
}
impl<I> Lucene90NormsProducer<I>
where
  I: IndexInput,
{
  fn get_disi_input(
    &self,
    field: &FieldInfo,
    entry: &NormsEntry,
  ) -> Result<SliceEnum<I::IndexInput>> {
    if self.merging {
      if let Some(existing) = {
        let map = self.disi_inputs.lock();
        map.get(&field.number).cloned()
      } {
        return Ok(SliceEnum::Shared(IndexInputImpl::new(existing)));
      }

      let new_input = Arc::new(Mutex::new(create_block_slice(
        &self.data,
        "docs",
        entry.docs_with_field_offset as usize,
        entry.docs_with_field_length as usize,
        entry.jump_table_entry_count as i32,
      )?));

      let input = {
        let mut map = self.disi_inputs.lock();
        map
          .entry(field.number)
          .or_insert_with(|| new_input.clone())
          .clone()
      };
      Ok(SliceEnum::Shared(IndexInputImpl::new(input)))
    } else {
      let input = create_block_slice(
        &self.data,
        "docs",
        entry.docs_with_field_offset as usize,
        entry.docs_with_field_length as usize,
        entry.jump_table_entry_count as i32,
      )?;
      Ok(SliceEnum::Owned(input))
    }
  }
}
pub enum SliceEnum<I> {
  Shared(IndexInputImpl<I>),
  Owned(I),
}
impl<I> Display for Lucene90NormsProducer<I>
where
  I: IndexInput,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "Lucene90NormsProducer(fields={})", self.norms.len())
  }
}

impl<I> CloseableRef for Lucene90NormsProducer<I>
where
  I: IndexInput,
{
  fn close(&self) -> Result<()> {
    self.data.close()
  }
}

impl<I> NormsProducer for Lucene90NormsProducer<I>
where
  I: IndexInput,
{
  type NumericDocValues = Lucene90NormNumericDocValuesEnum<I>;

  fn get_norms(&self, field: &Arc<FieldInfo>) -> Result<Lucene90NormNumericDocValuesEnum<I>> {
    // copy on stack is acceptable, of course we could have a better way
    let entry = self.norms.get(&field.number).unwrap().clone();
    if entry.docs_with_field_offset == -2 {
      // empty
      return Ok(Lucene90NormNumericDocValuesEnum::Empty(
        DocValues::empty_numeric(),
      ));
    }

    if entry.docs_with_field_offset == -1 {
      // dense
      if entry.bytes_per_norm == 0 {
        let sub_dense_norms = DenseNormsIteratorBaseEnum::Dense(DenseNormsIteratorBaseImpl {
          norms_offset: entry.norms_offset,
        });
        let dense_norms_iterator = DenseNormsIterator::new(self.max_doc, sub_dense_norms);
        return Ok(Lucene90NormNumericDocValuesEnum::Dense(
          dense_norms_iterator,
        ));
      }
      let slice = self.get_data_input(field, &entry)?;

      return match entry.bytes_per_norm {
        1 => {
          let sub_dense_norms =
            DenseNormsIteratorBaseEnum::Dense1(DenseNormsIteratorBaseImpl1 { slice });
          let dense_norms_iterator = DenseNormsIterator::new(self.max_doc, sub_dense_norms);
          Ok(Lucene90NormNumericDocValuesEnum::Dense(
            dense_norms_iterator,
          ))
        },
        2 => {
          let sub_dense_norms =
            DenseNormsIteratorBaseEnum::Dense2(DenseNormsIteratorBaseImpl2 { slice });
          let dense_norms_iterator = DenseNormsIterator::new(self.max_doc, sub_dense_norms);
          Ok(Lucene90NormNumericDocValuesEnum::Dense(
            dense_norms_iterator,
          ))
        },
        4 => {
          let sub_dense_norms =
            DenseNormsIteratorBaseEnum::Dense3(DenseNormsIteratorBaseImpl4 { slice });
          let dense_norms_iterator = DenseNormsIterator::new(self.max_doc, sub_dense_norms);
          Ok(Lucene90NormNumericDocValuesEnum::Dense(
            dense_norms_iterator,
          ))
        },
        8 => {
          let sub_dense_norms =
            DenseNormsIteratorBaseEnum::Dense4(DenseNormsIteratorBaseImpl8 { slice });
          let dense_norms_iterator = DenseNormsIterator::new(self.max_doc, sub_dense_norms);
          Ok(Lucene90NormNumericDocValuesEnum::Dense(
            dense_norms_iterator,
          ))
        },
        _ => Err(LuceneError::unreachable("invalid bytes_per_norm")),
      };
    }
    // sparse
    let disi = match (
      self.get_disi_jump_table(field, &entry)?,
      self.get_disi_input(field, &entry)?,
    ) {
      (Some(RandomAccessSliceEnum::Shared(jt)), SliceEnum::Shared(input)) => {
        IndexedDISIEnum::<I>::Shared(IndexedDISIImpl::from_components(
          input,
          Some(jt),
          entry.jump_table_entry_count as i32,
          entry.dense_rank_power,
          entry.num_docs_with_field as i64,
        )?)
      },

      (Some(RandomAccessSliceEnum::Owned(jt)), SliceEnum::Owned(input)) => {
        IndexedDISIEnum::<I>::Owned(IndexedDISIImpl::from_components(
          input,
          Some(jt),
          entry.jump_table_entry_count as i32,
          entry.dense_rank_power,
          entry.num_docs_with_field as i64,
        )?)
      },
      (None, SliceEnum::Shared(input)) => {
        IndexedDISIEnum::<I>::Shared(IndexedDISIImpl::from_components(
          input,
          None,
          entry.jump_table_entry_count as i32,
          entry.dense_rank_power,
          entry.num_docs_with_field as i64,
        )?)
      },

      (None, SliceEnum::Owned(input)) => IndexedDISIEnum::<I>::Owned(IndexedDISIImpl::from_components(
        input,
        None,
        entry.jump_table_entry_count as i32,
        entry.dense_rank_power,
        entry.num_docs_with_field as i64,
      )?),
      _ => {
        return Err(LuceneError::illegal_state(
          "should have same ownership: Shared or Owned",
        ));
      },
    };

    if entry.bytes_per_norm == 0 {
      let sub_sparse_norms = SparseNormsIteratorBaseEnum::Sparse(SparseNormsIteratorBaseImpl {
        norms_offset: entry.norms_offset,
      });
      let sparse_norms_iterator = SparseNormsIterator::new(sub_sparse_norms, disi);
      return Ok(Lucene90NormNumericDocValuesEnum::Sparse(
        sparse_norms_iterator,
      ));
    }

    let slice = self.get_data_input(field, &entry)?;

    match entry.bytes_per_norm {
      1 => {
        let sub_sparse_norms =
          SparseNormsIteratorBaseEnum::Sparse1(SparseNormsIteratorBaseImpl1 { slice });
        let sparse_norms_iterator = SparseNormsIterator::new(sub_sparse_norms, disi);
        Ok(Lucene90NormNumericDocValuesEnum::Sparse(
          sparse_norms_iterator,
        ))
      },
      2 => {
        let sub_sparse_norms =
          SparseNormsIteratorBaseEnum::Sparse2(SparseNormsIteratorBaseImpl2 { slice });
        let sparse_norms_iterator = SparseNormsIterator::new(sub_sparse_norms, disi);
        Ok(Lucene90NormNumericDocValuesEnum::Sparse(
          sparse_norms_iterator,
        ))
      },
      4 => {
        let sub_sparse_norms =
          SparseNormsIteratorBaseEnum::Sparse3(SparseNormsIteratorBaseImpl4 { slice });
        let sparse_norms_iterator = SparseNormsIterator::new(sub_sparse_norms, disi);
        Ok(Lucene90NormNumericDocValuesEnum::Sparse(
          sparse_norms_iterator,
        ))
      },
      8 => {
        let sub_sparse_norms =
          SparseNormsIteratorBaseEnum::Sparse4(SparseNormsIteratorBaseImpl8 { slice });
        let sparse_norms_iterator = SparseNormsIterator::new(sub_sparse_norms, disi);
        Ok(Lucene90NormNumericDocValuesEnum::Sparse(
          sparse_norms_iterator,
        ))
      },
      _ => Err(LuceneError::unreachable("invalid bytes_per_norm")),
    }
  }

  fn check_integrity(&self) -> Result<()> {
    CodecUtil::checksum_entire_file(&self.data)?;
    Ok(())
  }

  fn get_merge_instance(&self) -> Result<Option<Self>>
  where
    Self: Sized,
  {
    Ok(Some(Self {
      norms: self.norms.clone(),
      max_doc: self.max_doc,
      data: self.data.try_clone()?,
      merging: true,
      disi_inputs: HashMap::new().into(),
      disi_jump_tables: HashMap::new().into(),
      data_inputs: HashMap::new().into(),
    }))
  }
}

#[derive(Clone)]
struct NormsEntry {
  pub dense_rank_power: i8,
  pub bytes_per_norm: i8,
  pub docs_with_field_offset: i64,
  pub docs_with_field_length: i64,
  pub jump_table_entry_count: i16,
  pub num_docs_with_field: usize,
  pub norms_offset: i64,
}
pub struct DenseNormsIterator<R> {
  max_doc: i32,
  doc: i32,
  sub_dense_norms: DenseNormsIteratorBaseEnum<R>,
}
impl<R> DenseNormsIterator<R> {
  fn new(max_doc: i32, sub_dense_norms: DenseNormsIteratorBaseEnum<R>) -> Self {
    Self {
      max_doc,
      doc: -1,
      sub_dense_norms,
    }
  }
}

impl<R> DocValuesIterator for DenseNormsIterator<R>
where
  R: RandomAccessInput,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    self.doc = target;
    Ok(true)
  }
}

impl<R> DocIdSetIterator for DenseNormsIterator<R>
where
  R: RandomAccessInput,
{
  fn doc_id(&self) -> i32 {
    self.doc
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.advance(self.doc + 1)
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    if target >= self.max_doc {
      self.doc = NO_MORE_DOCS;
      return Ok(self.doc);
    }
    self.doc = target;
    Ok(self.doc)
  }

  fn cost(&self) -> Result<i64> {
    Ok(self.max_doc as i64)
  }
}

impl<R> NumericDocValues for DenseNormsIterator<R>
where
  R: RandomAccessInput,
{
  fn long_value(&mut self) -> Result<i64> {
    self.sub_dense_norms.long_value(self.doc)
  }
}
trait DenseNormsIteratorBase {
  fn long_value(&mut self, doc: i32) -> Result<i64>;
}
struct DenseNormsIteratorBaseImpl {
  norms_offset: i64,
}
impl DenseNormsIteratorBase for DenseNormsIteratorBaseImpl {
  fn long_value(&mut self, _doc: i32) -> Result<i64> {
    Ok(self.norms_offset)
  }
}
// case 1
struct DenseNormsIteratorBaseImpl1<R> {
  slice: RandomAccessSliceEnum<R>,
}
impl<R> DenseNormsIteratorBase for DenseNormsIteratorBaseImpl1<R>
where
  R: RandomAccessInput,
{
  fn long_value(&mut self, doc: i32) -> Result<i64> {
    match self.slice {
      RandomAccessSliceEnum::Owned(ref mut v) => {
        Ok((v.read_byte(doc.try_convert()?)? as i8) as i64)
      },
      RandomAccessSliceEnum::Shared(ref v) => {
        Ok((v.lock().read_byte(doc.try_convert()?)? as i8) as i64)
      },
    }
  }
}
// case 2
struct DenseNormsIteratorBaseImpl2<R> {
  slice: RandomAccessSliceEnum<R>,
}
impl<R> DenseNormsIteratorBase for DenseNormsIteratorBaseImpl2<R>
where
  R: RandomAccessInput,
{
  fn long_value(&mut self, doc: i32) -> Result<i64> {
    match self.slice {
      RandomAccessSliceEnum::Owned(ref mut v) => {
        Ok(v.read_short((doc.try_convert()?) << 1)? as i64)
      },
      RandomAccessSliceEnum::Shared(ref v) => {
        Ok(v.lock().read_short((doc.try_convert()?) << 1)? as i64)
      },
    }
  }
}
// case 4
struct DenseNormsIteratorBaseImpl4<R> {
  slice: RandomAccessSliceEnum<R>,
}
impl<R> DenseNormsIteratorBase for DenseNormsIteratorBaseImpl4<R>
where
  R: RandomAccessInput,
{
  fn long_value(&mut self, doc: i32) -> Result<i64> {
    match self.slice {
      RandomAccessSliceEnum::Owned(ref mut v) => Ok(v.read_int((doc.try_convert()?) << 2)? as i64),
      RandomAccessSliceEnum::Shared(ref v) => {
        Ok(v.lock().read_int((doc.try_convert()?) << 2)? as i64)
      },
    }
  }
}
// case 8
struct DenseNormsIteratorBaseImpl8<R> {
  slice: RandomAccessSliceEnum<R>,
}
impl<R> DenseNormsIteratorBase for DenseNormsIteratorBaseImpl8<R>
where
  R: RandomAccessInput,
{
  fn long_value(&mut self, doc: i32) -> Result<i64> {
    match self.slice {
      RandomAccessSliceEnum::Owned(ref mut v) => Ok(v.read_long((doc.try_convert()?) << 3)?),
      RandomAccessSliceEnum::Shared(ref v) => Ok(v.lock().read_long((doc.try_convert()?) << 3)?),
    }
  }
}
enum DenseNormsIteratorBaseEnum<R> {
  Dense(DenseNormsIteratorBaseImpl),
  Dense1(DenseNormsIteratorBaseImpl1<R>),
  Dense2(DenseNormsIteratorBaseImpl2<R>),
  Dense3(DenseNormsIteratorBaseImpl4<R>),
  Dense4(DenseNormsIteratorBaseImpl8<R>),
}
impl<R> DenseNormsIteratorBase for DenseNormsIteratorBaseEnum<R>
where
  R: RandomAccessInput,
{
  fn long_value(&mut self, doc: i32) -> Result<i64> {
    match self {
      DenseNormsIteratorBaseEnum::Dense(inner) => inner.long_value(doc),
      DenseNormsIteratorBaseEnum::Dense1(inner) => inner.long_value(doc),
      DenseNormsIteratorBaseEnum::Dense2(inner) => inner.long_value(doc),
      DenseNormsIteratorBaseEnum::Dense3(inner) => inner.long_value(doc),
      DenseNormsIteratorBaseEnum::Dense4(inner) => inner.long_value(doc),
    }
  }
}

pub struct SparseNormsIterator<I>
where
  I: IndexInput,
{
  sub_sparse_norms: SparseNormsIteratorBaseEnum<I::RandomAccessSlice>,
  disi: IndexedDISIEnum<I>,
}

impl<I> SparseNormsIterator<I>
where
  I: IndexInput,
{
  fn new(
    sub_sparse_norms: SparseNormsIteratorBaseEnum<I::RandomAccessSlice>,
    disi: IndexedDISIEnum<I>,
  ) -> Self {
    Self {
      sub_sparse_norms,
      disi,
    }
  }
}

impl<I> DocValuesIterator for SparseNormsIterator<I>
where
  I: IndexInput,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    self.disi.advance_exact(target)
  }
}

impl<I> DocIdSetIterator for SparseNormsIterator<I>
where
  I: IndexInput,
{
  fn doc_id(&self) -> i32 {
    self.disi.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.disi.next_doc()
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.disi.advance(target)
  }

  fn cost(&self) -> Result<i64> {
    self.disi.cost()
  }
}

impl<I> NumericDocValues for SparseNormsIterator<I>
where
  I: IndexInput,
{
  fn long_value(&mut self) -> Result<i64> {
    self
      .sub_sparse_norms
      .long_value(self.disi.index().try_convert()?)
  }
}

trait SparseNormsIteratorBase {
  fn long_value(&mut self, index: usize) -> Result<i64>;
}
struct SparseNormsIteratorBaseImpl {
  norms_offset: i64,
}
impl SparseNormsIteratorBaseImpl {
  fn new(norms_offset: i64) -> Self {
    Self { norms_offset }
  }
}
impl SparseNormsIteratorBase for SparseNormsIteratorBaseImpl {
  fn long_value(&mut self, _index: usize) -> Result<i64> {
    Ok(self.norms_offset)
  }
}
// case 1
struct SparseNormsIteratorBaseImpl1<R> {
  slice: RandomAccessSliceEnum<R>,
}
impl<R> SparseNormsIteratorBase for SparseNormsIteratorBaseImpl1<R>
where
  R: RandomAccessInput,
{
  fn long_value(&mut self, index: usize) -> Result<i64> {
    match self.slice {
      RandomAccessSliceEnum::Owned(ref mut v) => Ok((v.read_byte(index)? as i8) as i64),
      RandomAccessSliceEnum::Shared(ref v) => Ok((v.lock().read_byte(index)? as i8) as i64),
    }
  }
}
// case 2
struct SparseNormsIteratorBaseImpl2<R> {
  slice: RandomAccessSliceEnum<R>,
}
impl<R> SparseNormsIteratorBase for SparseNormsIteratorBaseImpl2<R>
where
  R: RandomAccessInput,
{
  fn long_value(&mut self, index: usize) -> Result<i64> {
    match self.slice {
      RandomAccessSliceEnum::Owned(ref mut v) => Ok(v.read_short(index << 1)? as i64),
      RandomAccessSliceEnum::Shared(ref v) => Ok(v.lock().read_short(index << 1)? as i64),
    }
  }
}
// case 4
struct SparseNormsIteratorBaseImpl4<R> {
  slice: RandomAccessSliceEnum<R>,
}
impl<R> SparseNormsIteratorBase for SparseNormsIteratorBaseImpl4<R>
where
  R: RandomAccessInput,
{
  fn long_value(&mut self, index: usize) -> Result<i64> {
    match self.slice {
      RandomAccessSliceEnum::Owned(ref mut v) => Ok(v.read_int(index << 2)? as i64),
      RandomAccessSliceEnum::Shared(ref v) => Ok(v.lock().read_int(index << 2)? as i64),
    }
  }
}
// case 8
struct SparseNormsIteratorBaseImpl8<R> {
  slice: RandomAccessSliceEnum<R>,
}
impl<R> SparseNormsIteratorBase for SparseNormsIteratorBaseImpl8<R>
where
  R: RandomAccessInput,
{
  fn long_value(&mut self, index: usize) -> Result<i64> {
    match self.slice {
      RandomAccessSliceEnum::Owned(ref mut v) => v.read_long(index << 3),
      RandomAccessSliceEnum::Shared(ref v) => v.lock().read_long(index << 3),
    }
  }
}
enum SparseNormsIteratorBaseEnum<R> {
  Sparse(SparseNormsIteratorBaseImpl),
  Sparse1(SparseNormsIteratorBaseImpl1<R>),
  Sparse2(SparseNormsIteratorBaseImpl2<R>),
  Sparse3(SparseNormsIteratorBaseImpl4<R>),
  Sparse4(SparseNormsIteratorBaseImpl8<R>),
}
impl<R> SparseNormsIteratorBase for SparseNormsIteratorBaseEnum<R>
where
  R: RandomAccessInput,
{
  fn long_value(&mut self, index: usize) -> Result<i64> {
    match self {
      SparseNormsIteratorBaseEnum::Sparse(inner) => inner.long_value(index),
      SparseNormsIteratorBaseEnum::Sparse1(inner) => inner.long_value(index),
      SparseNormsIteratorBaseEnum::Sparse2(inner) => inner.long_value(index),
      SparseNormsIteratorBaseEnum::Sparse3(inner) => inner.long_value(index),
      SparseNormsIteratorBaseEnum::Sparse4(inner) => inner.long_value(index),
    }
  }
}
