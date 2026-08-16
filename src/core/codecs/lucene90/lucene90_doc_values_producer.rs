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
use crate::core::codecs::block_term_state::TermStateEnum;
use crate::core::codecs::doc_values_producer::DocValuesProducer;
use crate::core::codecs::dummy::dummy_numeric_doc_values::DummyNumericDocValues;
use crate::core::codecs::dummy::dummy_sorted_doc_values::DummySortedDocValues;
use crate::core::codecs::indexed_disi::IndexedDISIImpl;
use crate::core::codecs::lucene90::dov_values_inner_enum::{
  BaseSortedDocValuesEnum, BaseSortedSetDocValuesEnum, DenseBinaryDocValuesBaseEnum,
  DenseNumericDocValuesSubEnum,
  LongValuesEnums, SparseBinaryDocValuesBaseEnum, SparseNumericDocValuesSubEnum,
};
use crate::core::codecs::lucene90_doc_values_format::{
  Lucene90DocValuesFormat, SKIP_INDEX_JUMP_LENGTH_PER_LEVEL,
};
use crate::core::index::base_terms_enum::BaseTermsEnumTermStateImpl;
use crate::core::index::binary_doc_values::BinaryDocValues;
use crate::core::index::doc_values::{DocValues, EmptyBinary, EmptyNumeric};
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::doc_values_skip_index_type::DocValuesSkipIndexType;
use crate::core::index::doc_values_skipper::DocValuesSkipper;
use crate::core::index::dummy::dummy_impacts_enum::DummyImpactsEnum;
use crate::core::index::dummy::dummy_postings_enum::DummyPostingsEnum;
use crate::core::index::dummy::dummy_terms_enum::DummyTermsEnum;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::singleton_sorted_numeric_doc_values::SingletonSortedNumericDocValues;
use crate::core::index::singleton_sorted_set_doc_values::SingletonSortedSetDocValues;
use crate::core::index::sorted_doc_values::SortedDocValues;
use crate::core::index::sorted_numeric_doc_values::SortedNumericDocValues;
use crate::core::index::sorted_set_doc_values::SortedSetDocValues;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum, TermsEnumEnum2};
use crate::core::index::{BytesRef, IndexFileNames};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::store::directory::Directory;
use crate::core::store::random_access_input::RandomAccessInput;
use crate::core::store::{ByteArrayDataInput, DataInput, IndexInput, ReadAdvice};
use crate::core::util::IOUtils;
use crate::core::util::access::{SharedAccessVec, WritableVec};
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::close::CloseableRef;
use crate::core::util::compress::lz4::LZ4;
use crate::core::util::dummy::dummy_attribute_source::DummyAttributeSource;
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::long_values::{LongValues, Zeroes};
use crate::core::util::packed::direct_monotonic_reader::Meta;
use crate::core::util::packed::direct_monotonic_reader::{DirectMonotonicReader, load_meta};
use crate::core::util::packed::direct_reader::{DirectPackedEnum, DirectReader, FromSlice};
use crate::core::util::{SliceCopyOps, ToInt, TryIntoInt};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

pub struct Lucene90DocValuesProducer<I> {
  numerics: HashMap<i32, Arc<NumericEntry>>,
  binaries: HashMap<i32, Arc<BinaryEntry>>,
  sorted: HashMap<i32, Arc<SortedEntry>>,
  sorted_sets: HashMap<i32, Arc<SortedSetEntry>>,
  sorted_numerics: HashMap<i32, Arc<SortedNumericEntry>>,
  skippers: HashMap<i32, Arc<DocValuesSkipperEntry>>,
  data: Arc<I>,
  max_doc: i32,
  version: i32,
  merging: bool,
}

impl<I> Lucene90DocValuesProducer<I>
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
    let meta_name =
      IndexFileNames::segment_file_name(&segment_info.name, &state.segment_suffix, meta_extension);

    let max_doc = segment_info.max_doc()?;
    let mut version = -1;

    let mut numerics = HashMap::new();
    let mut binaries = HashMap::new();
    let mut sorted = HashMap::new();
    let mut sorted_sets = HashMap::new();
    let mut sorted_numerics = HashMap::new();
    let mut skippers = HashMap::new();

    {
      let mut input = state.directory.open_checksum_input(&meta_name)?;
      let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        let prior_result =
          std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
            version = CodecUtil::check_index_header(
              &mut input,
              meta_codec,
              Lucene90DocValuesFormat::VERSION_START,
              Lucene90DocValuesFormat::VERSION_CURRENT,
              segment_info.get_id(),
              &state.segment_suffix,
            )?;
            Self::read_fields(
              &mut input,
              &state.field_infos,
              &mut numerics,
              &mut binaries,
              &mut sorted,
              &mut sorted_sets,
              &mut sorted_numerics,
              &mut skippers,
            )
          }));
        let prior_result = match prior_result {
          Ok(Ok(())) => None,
          prior_result => Some(prior_result),
        };
        CodecUtil::check_footer_with_error(&mut input, prior_result)
      }));
      let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| input.close()));
      IOUtils::use_or_suppress_caught_result(result, close_result)?;
    }

    let data_name =
      IndexFileNames::segment_file_name(&segment_info.name, &state.segment_suffix, data_extension);
    // Doc-values have a forward-only access pattern, so pass
    // ReadAdvice.NORMAL to perform readahead.
    let mut data = Some(state.directory.open_input(
      &data_name,
      &state.context.with_read_advice_self(ReadAdvice::Normal)?,
    )?);
    let mut success = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<Self> {
      let data_ref = data
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("doc values data input is missing"))?;
      let version2 = CodecUtil::check_index_header(
        data_ref,
        data_codec,
        Lucene90DocValuesFormat::VERSION_START,
        Lucene90DocValuesFormat::VERSION_CURRENT,
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

      let data = Arc::new(
        data
          .take()
          .ok_or_else(|| LuceneError::illegal_state("doc values data input is missing"))?,
      );
      success = true;
      Ok(Self {
        numerics,
        binaries,
        sorted,
        sorted_sets,
        sorted_numerics,
        skippers,
        data,
        max_doc,
        version,
        merging: false,
      })
    }));
    if !success {
      IOUtils::close_while_handling_exception(data.as_ref());
    }
    unwrap_caught_result!(result)
  }
  #[allow(clippy::too_many_arguments)]
  fn with_merging(
    numerics: HashMap<i32, Arc<NumericEntry>>,
    binaries: HashMap<i32, Arc<BinaryEntry>>,
    sorted: HashMap<i32, Arc<SortedEntry>>,
    sorted_sets: HashMap<i32, Arc<SortedSetEntry>>,
    sorted_numerics: HashMap<i32, Arc<SortedNumericEntry>>,
    skippers: HashMap<i32, Arc<DocValuesSkipperEntry>>,
    data: Arc<I>,
    max_doc: i32,
    version: i32,
  ) -> Result<Self> {
    Ok(Self {
      numerics,
      binaries,
      sorted,
      sorted_sets,
      sorted_numerics,
      skippers,
      data: Arc::new((*data).try_clone()?),
      max_doc,
      version,
      merging: true,
    })
  }

  #[allow(clippy::too_many_arguments)]
  fn read_fields(
    meta: &mut impl IndexInput,
    infos: &Arc<FieldInfos>,
    numerics: &mut HashMap<i32, Arc<NumericEntry>>,
    binaries: &mut HashMap<i32, Arc<BinaryEntry>>,
    sorted: &mut HashMap<i32, Arc<SortedEntry>>,
    sorted_sets: &mut HashMap<i32, Arc<SortedSetEntry>>,
    sorted_numerics: &mut HashMap<i32, Arc<SortedNumericEntry>>,
    skippers: &mut HashMap<i32, Arc<DocValuesSkipperEntry>>,
  ) -> Result<()> {
    loop {
      let field_number = meta.read_int()?;
      if field_number == -1 {
        break;
      }
      let info = infos.field_info_by_number(field_number)?;
      let Some(ref info) = info else {
        return Err(LuceneError::corrupt_index(format!(
          "Field number {field_number} not found in field infos, resource {meta}"
        )));
      };
      let type_byte = meta.read_byte()?;

      if info.doc_values_skip_index_type() != &DocValuesSkipIndexType::None {
        let skipper = Arc::new(Self::read_doc_value_skipper_meta(meta)?);
        skippers.insert(info.number, skipper);
      }

      match type_byte {
        t if t == Lucene90DocValuesFormat::NUMERIC => {
          let entry = Arc::new(Self::read_numeric(meta)?);
          numerics.insert(info.number, entry);
        },
        t if t == Lucene90DocValuesFormat::BINARY => {
          let entry = Self::read_binary(meta)?;
          binaries.insert(info.number, Arc::new(entry));
        },
        t if t == Lucene90DocValuesFormat::SORTED => {
          let entry = Arc::new(Self::read_sorted(meta)?);
          sorted.insert(info.number, entry);
        },
        t if t == Lucene90DocValuesFormat::SORTED_SET => {
          let entry = Arc::new(Self::read_sorted_set(meta)?);
          sorted_sets.insert(info.number, entry);
        },
        t if t == Lucene90DocValuesFormat::SORTED_NUMERIC => {
          let entry = Arc::new(Self::read_sorted_numeric(meta)?);
          sorted_numerics.insert(info.number, entry);
        },
        _ => {
          return Err(LuceneError::corrupt_index(format!(
            "Invalid doc values type: {type_byte}"
          )));
        },
      }
    }
    Ok(())
  }
  fn read_numeric(meta: &mut impl IndexInput) -> Result<NumericEntry> {
    let mut entry = NumericEntry::default();
    Self::read_numeric_with_entry(meta, &mut entry)?;
    Ok(entry)
  }
  fn read_doc_value_skipper_meta(meta: &mut impl IndexInput) -> Result<DocValuesSkipperEntry> {
    let offset = meta.read_long()?;
    let length = meta.read_long()?;
    let max_value = meta.read_long()?;
    let min_value = meta.read_long()?;
    let doc_count = meta.read_int()?;
    let max_doc_id = meta.read_int()?;

    Ok(DocValuesSkipperEntry {
      offset,
      length,
      min_value,
      max_value,
      doc_count,
      max_doc_id,
    })
  }
  fn read_numeric_with_entry(meta: &mut impl IndexInput, entry: &mut NumericEntry) -> Result<()> {
    entry.docs_with_field_offset = meta.read_long()?;
    entry.docs_with_field_length = meta.read_long()? as usize;
    entry.jump_table_entry_count = meta.read_short()?;
    entry.dense_rank_power = meta.read_byte()? as i8;
    entry.num_values = meta.read_long()? as usize;

    let table_size = meta.read_int()?;
    if table_size > 256 {
      return Err(LuceneError::corrupt_index(format!(
        "invalid table size: {table_size} resource {meta}"
      )));
    }

    entry.table = if table_size >= 0 {
      let mut table = Vec::with_capacity(table_size as usize);
      for _ in 0..table_size {
        table.push(meta.read_long()?);
      }
      Option::from(Arc::new(table))
    } else {
      None
    };

    entry.block_shift = if table_size < -1 { -2 - table_size } else { -1 };

    entry.bits_per_value = meta.read_byte()? as i8;
    entry.min_value = meta.read_long()?;
    entry.gcd = meta.read_long()?;
    entry.values_offset = meta.read_long()? as usize;
    entry.values_length = meta.read_long()? as usize;
    entry.value_jump_table_offset = meta.read_long()?;
    Ok(())
  }
  fn read_binary(meta: &mut impl IndexInput) -> Result<BinaryEntry> {
    let data_offset = meta.read_long()? as usize;
    let data_length = meta.read_long()? as usize;
    let docs_with_field_offset = meta.read_long()?;
    let docs_with_field_length = meta.read_long()? as usize;
    let jump_table_entry_count = meta.read_short()?;

    let dense_rank_power = meta.read_byte()?;
    let num_docs_with_field = meta.read_int()?;
    let min_length = meta.read_int()?;
    let max_length = meta.read_int()?;

    let mut addresses_offset = 0;
    let mut addresses_meta = None;
    let mut addresses_length = 0;

    if min_length < max_length {
      addresses_offset = meta.read_long()? as usize;
      // Old count of uncompressed addresses
      let num_addresses = num_docs_with_field as i64 + 1;
      let block_shift = meta.read_vint()?;
      addresses_meta = Some(load_meta(meta, num_addresses, block_shift)?);
      addresses_length = meta.read_long()? as usize;
    }

    Ok(BinaryEntry {
      data_offset,
      data_length,
      docs_with_field_offset,
      docs_with_field_length,
      jump_table_entry_count,
      dense_rank_power,
      num_docs_with_field,
      min_length,
      max_length,
      addresses_offset,
      addresses_length,
      addresses_meta,
    })
  }
  fn read_sorted(meta: &mut impl IndexInput) -> Result<SortedEntry> {
    let mut ords_entry = NumericEntry::default();
    Self::read_numeric_with_entry(meta, &mut ords_entry)?;
    let mut terms_dict_entry = TermsDictEntry::default();
    Self::read_term_dict_with_entry(meta, &mut terms_dict_entry)?;
    Ok(SortedEntry {
      ords_entry: Arc::new(ords_entry),
      terms_dict_entry: Arc::new(terms_dict_entry),
    })
  }
  fn read_sorted_set(meta: &mut impl IndexInput) -> Result<SortedSetEntry> {
    let multi_valued = meta.read_byte()?;
    let mut entry = SortedSetEntry::default();
    match multi_valued {
      0 => {
        entry.single_value_entry = Some(Arc::new(Self::read_sorted(meta)?));
        return Ok(entry);
      },
      1 => {},
      _ => {
        return Err(LuceneError::corrupt_index(format!(
          "Invalid multiValued flag: {multi_valued} resource {meta}"
        )));
      },
    };
    let mut ords_entry = SortedNumericEntry::default();
    Self::read_sorted_numeric_with_entry(meta, &mut ords_entry)?;
    let mut terms_dict_entry = TermsDictEntry::default();
    Self::read_term_dict_with_entry(meta, &mut terms_dict_entry)?;

    entry.ords_entry = Some(Arc::new(ords_entry));
    entry.terms_dict_entry = Some(Arc::new(terms_dict_entry));
    Ok(entry)
  }
  fn read_term_dict_with_entry(
    meta: &mut impl IndexInput,
    entry: &mut TermsDictEntry,
  ) -> Result<()> {
    entry.terms_dict_size = meta.read_vlong()?;
    let block_shift = meta.read_int()?;

    let addresses_size =
      (entry.terms_dict_size + (1 << Lucene90DocValuesFormat::TERMS_DICT_BLOCK_LZ4_SHIFT) - 1)
        >> Lucene90DocValuesFormat::TERMS_DICT_BLOCK_LZ4_SHIFT;

    entry.terms_addresses_meta = Some(load_meta(meta, addresses_size, block_shift)?);

    entry.max_term_length = meta.read_int()?;
    entry.max_block_length = meta.read_int()?;
    entry.terms_data_offset = meta.read_long()? as usize;
    entry.terms_data_length = meta.read_long()? as usize;
    entry.terms_addresses_offset = meta.read_long()? as usize;
    entry.terms_addresses_length = meta.read_long()? as usize;
    entry.terms_dict_index_shift = meta.read_int()?;

    let index_size = (entry.terms_dict_size + (1 << entry.terms_dict_index_shift) - 1)
      >> entry.terms_dict_index_shift;

    entry.terms_index_addresses_meta = Some(load_meta(meta, 1 + index_size, block_shift)?);
    entry.terms_index_offset = meta.read_long()? as usize;
    entry.terms_index_length = meta.read_long()? as usize;
    entry.terms_index_addresses_offset = meta.read_long()? as usize;
    entry.terms_index_addresses_length = meta.read_long()? as usize;

    Ok(())
  }
  fn read_sorted_numeric(meta: &mut impl IndexInput) -> Result<SortedNumericEntry> {
    let mut entry = SortedNumericEntry::default();
    Self::read_sorted_numeric_with_entry(meta, &mut entry)?;
    Ok(entry)
  }
  fn read_sorted_numeric_with_entry(
    meta: &mut impl IndexInput,
    entry: &mut SortedNumericEntry,
  ) -> Result<()> {
    debug_assert!(*entry.base == NumericEntry::default());
    let mut numeric_entry = NumericEntry::default();
    Self::read_numeric_with_entry(meta, &mut numeric_entry)?;
    entry.base = Arc::new(numeric_entry);
    entry.num_docs_with_field = meta.read_int()?;

    if entry.num_docs_with_field as usize != entry.base.num_values {
      entry.addresses_offset = meta.read_long()? as usize;
      let block_shift = meta.read_vint()?;
      entry.addresses_meta = Some(load_meta(
        meta,
        entry.num_docs_with_field as i64 + 1,
        block_shift,
      )?);
      entry.addresses_length = meta.read_long()? as usize;
    }
    Ok(())
  }

  fn get_numeric(&self, entry: Arc<NumericEntry>) -> Result<Lucene90NumericDocValuesEnum<I>> {
    if entry.docs_with_field_offset == -2 {
      // empty
      Ok(Lucene90NumericDocValuesEnum::C(
        DocValues::empty_numeric(),
      ))
    } else if entry.docs_with_field_offset == -1 {
      // dense
      let dense_numeric_doc_values_base_enum = if entry.bits_per_value == 0 {
        DenseNumericDocValuesSubEnum::Dense(DenseNumericDocValuesBaseImpl {
          min_values: entry.min_value,
        })
      } else {
        let mut slice = self
          .data
          .random_access_slice(entry.values_offset, entry.values_length)?;
        // Prefetch the first page of data. Following pages are expected
        // to get prefetched through read-ahead.
        if slice.length()? > 0 {
          slice.prefetch(0, 1)?
        }
        if entry.block_shift >= 0 {
          let vbpv_reader =
            VaryingBPVReader::new(entry.clone(), slice, self.data.as_ref(), self.merging)?;
          DenseNumericDocValuesSubEnum::Dense1(DenseNumericDocValuesBaseImpl1 { vbpv_reader })
        } else {
          let values = get_direct_reader_instance(
            self.merging,
            Some(slice),
            entry.bits_per_value as i32,
            0,
            entry.num_values,
          )?;
          match entry.table {
            Some(ref table) => {
              DenseNumericDocValuesSubEnum::Dense2(DenseNumericDocValuesBaseImpl2 {
                table: table.clone(),
                values,
              })
            },
            None => {
              if entry.gcd == 1 && entry.min_value == 0 {
                DenseNumericDocValuesSubEnum::Dense3(DenseNumericDocValuesBaseImpl3 { values })
              } else {
                DenseNumericDocValuesSubEnum::Dense4(DenseNumericDocValuesBaseImpl4 {
                  values,
                  mul: entry.gcd,
                  delta: entry.min_value,
                })
              }
            },
          }
        }
      };
      Ok(Lucene90NumericDocValuesEnum::A(DenseNumericDocValues::new(
        dense_numeric_doc_values_base_enum,
        self.max_doc,
      )))
    } else {
      let disi = IndexedDISIImpl::new(
        self.data.as_ref(),
        entry.docs_with_field_offset as usize,
        entry.docs_with_field_length,
        entry.jump_table_entry_count as i32,
        entry.dense_rank_power,
        entry.num_values as i64,
      )?;
      let sparse_numeric_doc_values_base_enum = if entry.bits_per_value == 0 {
        SparseNumericDocValuesSubEnum::Sparse(SparseNumericDocValuesBaseImpl {
          min_values: entry.min_value,
        })
      } else {
        let mut slice = self
          .data
          .random_access_slice(entry.values_offset, entry.values_length)?;
        // Prefetch the first page of data. Following pages are expected
        // to get prefetched through read-ahead.
        if slice.length()? > 0 {
          slice.prefetch(0, 1)?
        }
        if entry.block_shift >= 0 {
          SparseNumericDocValuesSubEnum::Sparse1(SparseNumericDocValuesBaseImpl1 {
            vbpv_reader: VaryingBPVReader::new(
              entry.clone(),
              slice,
              self.data.as_ref(),
              self.merging,
            )?,
          })
        } else {
          let values = get_direct_reader_instance(
            self.merging,
            Some(slice),
            entry.bits_per_value as i32,
            0,
            entry.num_values,
          )?;
          match entry.table {
            Some(ref table) => {
              SparseNumericDocValuesSubEnum::Sparse2(SparseNumericDocValuesBaseImpl2 {
                table: table.clone(),
                values,
              })
            },
            None => {
              if entry.gcd == 1 && entry.min_value == 0 {
                SparseNumericDocValuesSubEnum::Sparse3(SparseNumericDocValuesBaseImpl3 { values })
              } else {
                SparseNumericDocValuesSubEnum::Sparse4(SparseNumericDocValuesBaseImpl4 {
                  values,
                  mul: entry.gcd,
                  delta: entry.min_value,
                })
              }
            },
          }
        }
      };
      Ok(Lucene90NumericDocValuesEnum::B(
        SparseNumericDocValues::new(sparse_numeric_doc_values_base_enum, disi),
      ))
    }
  }
  fn get_numeric_values(
    &self,
    entry: &Arc<NumericEntry>,
  ) -> Result<LongValuesEnums<I::RandomAccessSlice>>
  where
    I: IndexInput,
  {
    let long_values = if entry.bits_per_value == 0 {
      LongValuesEnums::Constant(LongValuesImpl {
        min_values: entry.min_value,
      })
    } else {
      let mut slice = self
        .data
        .random_access_slice(entry.values_offset, entry.values_length)?;
      if slice.length()? > 0 {
        slice.prefetch(0, 1)?
      }
      if entry.block_shift >= 0 {
        LongValuesEnums::Block(LongValuesImpl1 {
          vbpv_reader: VaryingBPVReader::new(
            entry.clone(),
            slice,
            self.data.as_ref(),
            self.merging,
          )?,
        })
      } else {
        let values = get_direct_reader_instance(
          self.merging,
          Some(slice),
          entry.bits_per_value as i32,
          0,
          entry.num_values,
        )?;
        match entry.table {
          Some(ref table) => LongValuesEnums::Table(LongValuesImpl2 {
            table: table.clone(),
            values,
          }),
          None => {
            if entry.gcd != 1 {
              LongValuesEnums::Gcd(LongValuesImpl3 {
                values,
                gcd: entry.gcd,
                min_value: entry.min_value,
              })
            } else if entry.min_value != 0 {
              LongValuesEnums::Delta(LongValuesImpl4 {
                values,
                min_value: entry.min_value,
              })
            } else {
              LongValuesEnums::Direct(values)
            }
          },
        }
      }
    };
    Ok(long_values)
  }

  fn get_sorted(&self, entry: Arc<SortedEntry>) -> Result<BaseSortedDocValues<I>> {
    let ords_entry = &entry.ords_entry;

    if ords_entry.block_shift < 0 && ords_entry.bits_per_value > 0 {
      if ords_entry.gcd != 1 || ords_entry.min_value != 0 || ords_entry.table.is_some() {
        return Err(LuceneError::illegal_state(
          "Ordinals shouldn't use GCD, offset or table compression",
        ));
      }

      let mut slice = self
        .data
        .random_access_slice(ords_entry.values_offset, ords_entry.values_length)?;
      if slice.length()? > 0 {
        slice.prefetch(0, 1)?;
      }

      let values = get_direct_reader_instance(
        self.merging,
        Some(slice),
        ords_entry.bits_per_value as i32,
        0,
        ords_entry.num_values,
      )?;

      let sub = if ords_entry.docs_with_field_offset == -1 {
        //dense
        BaseSortedDocValuesEnum::Dense(DenseBaseSortedDocValues::new(self.max_doc, values))
      } else if ords_entry.docs_with_field_offset >= 0 {
        let disi = IndexedDISIImpl::new(
          self.data.as_ref(),
          ords_entry.docs_with_field_offset as usize,
          ords_entry.docs_with_field_length,
          ords_entry.jump_table_entry_count as i32,
          ords_entry.dense_rank_power,
          ords_entry.num_values as i64,
        )?;
        BaseSortedDocValuesEnum::Sparse(SparseBaseSortedDocValues::new(disi, values))
      } else {
        let ords = self.get_numeric(ords_entry.clone())?;
        BaseSortedDocValuesEnum::Impl(BaseSortedDocValuesOrdinals::new(ords))
      };
      return BaseSortedDocValues::new(entry.clone(), self.data.clone(), sub, self.merging);
    }

    let ords = self.get_numeric(ords_entry.clone())?;
    let sub = BaseSortedDocValuesEnum::Impl(BaseSortedDocValuesOrdinals::new(ords));
    BaseSortedDocValues::new(entry.clone(), self.data.clone(), sub, self.merging)
  }

  fn get_sorted_numeric(
    &self,
    entry: &SortedNumericEntry,
  ) -> Result<Lucene90SortedNumericDocValuesEnum<I>>
  where
    I: IndexInput,
  {
    if entry.base.num_values == entry.num_docs_with_field as usize {
      return Ok(Lucene90SortedNumericDocValuesEnum::C(
        DocValues::singleton_numeric(self.get_numeric(entry.base.clone())?)?,
      ));
    }

    let mut addresses_input = self
      .data
      .random_access_slice(entry.addresses_offset, entry.addresses_length)?;
    // Prefetch the first page of data. Following pages are expected to get
    // prefetched through read-ahead.
    if addresses_input.length()? > 0 {
      addresses_input.prefetch(0, 1)?;
    }

    let Some(ref meta) = entry.addresses_meta else {
      return Err(LuceneError::illegal_state("addresses_meta is None"));
    };

    let addresses =
      DirectMonotonicReader::get_instance_with_merging(meta, addresses_input, self.merging)?;

    let values = self.get_numeric_values(&entry.base)?;

    if entry.base.docs_with_field_offset == -1 {
      // dense
      Ok(Lucene90SortedNumericDocValuesEnum::A(
        DenseSortedNumericDocValues::new(self.max_doc, values, addresses),
      ))
    } else {
      // sparse
      let disi = IndexedDISIImpl::new(
        self.data.as_ref(),
        entry.base.docs_with_field_offset as usize,
        entry.base.docs_with_field_length,
        entry.base.jump_table_entry_count as i32,
        entry.base.dense_rank_power,
        entry.num_docs_with_field as i64,
      )?;

      Ok(Lucene90SortedNumericDocValuesEnum::B(
        SpareSortedNumericDocValues::new(disi, values, addresses),
      ))
    }
  }
}

impl<I> CloseableRef for Lucene90DocValuesProducer<I>
where
  I: IndexInput,
{
  fn close(&self) -> Result<()> {
    CloseableRef::close(&self.data)
  }
}

impl<I> DocValuesProducer for Lucene90DocValuesProducer<I>
where
  I: IndexInput,
{
  type NumericDocValues = Lucene90NumericDocValuesEnum<I>;

  fn get_numeric(&self, field: &Arc<FieldInfo>) -> Result<Lucene90NumericDocValuesEnum<I>> {
    let entry = self.numerics.get(&field.number).ok_or_else(|| {
      LuceneError::illegal_state(format!("Missing numeric entry for field {}", field.number))
    })?;
    self.get_numeric(entry.clone())
  }

  type BinaryDocValues = Lucene90BinaryDocValuesEnum<I>;

  fn get_binary(&self, field: &Arc<FieldInfo>) -> Result<Self::BinaryDocValues> {
    let entry = self.binaries.get(&field.number);
    let Some(entry) = entry else {
      return Err(LuceneError::illegal_state(format!(
        "Missing sorted set entry for field {}",
        field.number
      )));
    };
    if entry.docs_with_field_offset == -2 {
      return Ok(Lucene90BinaryDocValuesEnum::Empty(
        DocValues::empty_binary(),
      ));
    }
    let mut bytes_slice = self
      .data
      .random_access_slice(entry.data_offset, entry.data_length)?;
    // Prefetch the first page of data. Following pages are expected
    // to get prefetched through read-ahead.
    if bytes_slice.length()? > 0 {
      bytes_slice.prefetch(0, 1)?;
    }
    if entry.docs_with_field_offset == -1 {
      let dense = if entry.min_length == entry.max_length {
        // fixed length
        let vec = vec![0u8; entry.max_length as usize];
        let base = DenseBinaryDocValuesBaseImpl {
          bytes_slice,
          length: entry.max_length,
          bytes: BytesRef::from_slice(vec, 0, entry.max_length as usize),
        };
        DenseBinaryDocValuesBaseEnum::Dense(base)
      } else {
        let mut addresses_data = self
          .data
          .random_access_slice(entry.addresses_offset, entry.addresses_length)?;
        // Prefetch the first page of data. Following pages are
        // expected to get prefetched through
        // read-ahead.
        if addresses_data.length()? > 0 {
          addresses_data.prefetch(0, 1)?;
        }
        let Some(ref meta) = entry.addresses_meta else {
          return Err(LuceneError::illegal_state("addresses_meta is None"))?;
        };
        let addresses = DirectMonotonicReader::get_instance(meta, addresses_data)?;
        let vec = vec![0u8; entry.max_length as usize];
        let base = DenseBinaryDocValuesBaseImpl1 {
          bytes_slice,
          bytes: BytesRef::from_slice(vec, 0, entry.max_length as usize),
          addresses,
        };
        DenseBinaryDocValuesBaseEnum::Dense1(base)
      };
      Ok(Lucene90BinaryDocValuesEnum::Dense(
        DenseBinaryDocValues::new(dense, self.max_doc),
      ))
    } else {
      let disi = IndexedDISIImpl::new(
        self.data.as_ref(),
        entry.docs_with_field_offset as usize,
        entry.docs_with_field_length,
        entry.jump_table_entry_count as i32,
        entry.dense_rank_power as i8,
        entry.num_docs_with_field as i64,
      )?;

      let sub = if entry.min_length == entry.max_length {
        // fixed-length
        let length = entry.max_length;
        SparseBinaryDocValuesBaseEnum::Sparse(SparseBinaryDocValuesBaseImpl {
          bytes_slice,
          bytes: BytesRef::from_slice(vec![0u8; length as usize], 0, length as usize),
          length,
        })
      } else {
        // variable-length
        let mut addresses_data = self
          .data
          .random_access_slice(entry.addresses_offset, entry.addresses_length)?;
        if addresses_data.length()? > 0 {
          addresses_data.prefetch(0, 1)?;
        }
        let Some(ref meta) = entry.addresses_meta else {
          return Err(LuceneError::illegal_state("addresses_meta is None"));
        };

        let addresses = DirectMonotonicReader::get_instance(meta, addresses_data)?;
        SparseBinaryDocValuesBaseEnum::Sparse1(SparseBinaryDocValuesBaseImpl1 {
          bytes_slice,
          bytes: BytesRef::from_slice(
            vec![0u8; entry.max_length as usize],
            0,
            entry.max_length as usize,
          ),
          addresses,
        })
      };
      Ok(Lucene90BinaryDocValuesEnum::Sparse(
        SparseBinaryDocValues::new(sub, disi),
      ))
    }
  }

  type SortedDocValues = Lucene90SortedDocValuesEnum<I>;

  fn get_sorted(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedDocValues> {
    let entry = self.sorted.get(&field.number).ok_or_else(|| {
      LuceneError::illegal_state(format!("Missing sorted entry for field {}", field.number))
    })?;
    self.get_sorted(entry.clone())
  }

  type SortedNumericDocValues = Lucene90SortedNumericDocValuesEnum<I>;

  fn get_sorted_numeric(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedNumericDocValues> {
    let entry = self.sorted_numerics.get(&field.number).ok_or_else(|| {
      LuceneError::illegal_state(format!(
        "Missing sorted numeric entry for field {}",
        field.number
      ))
    })?;
    self.get_sorted_numeric(entry.as_ref())
  }

  type SortedSetDocValues = Lucene90SortedSetDocValuesEnum<I>;

  fn get_sorted_set(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedSetDocValues> {
    let field_number = field.number;
    let entry = self.sorted_sets.get(&field_number);
    let Some(entry) = entry else {
      return Err(LuceneError::illegal_state(format!(
        "Missing sorted set entry for field {field_number}"
      )));
    };
    if let Some(ref single_value_entry) = entry.single_value_entry {
      let singleton = DocValues::singleton_sorted(self.get_sorted(single_value_entry.clone())?)?;
      return Ok(Lucene90SortedSetDocValuesEnum::Single(singleton));
    }
    // Specialize the common case for ordinals: single block of
    // packed integers.
    let Some(ref ords_entry) = entry.ords_entry else {
      return Err(LuceneError::illegal_state("ords_entry is None"))?;
    };
    if ords_entry.base.block_shift < 0 && ords_entry.base.bits_per_value > 0 {
      if ords_entry.base.gcd != 1
        || ords_entry.base.min_value != 0
        || ords_entry.base.table.is_some()
      {
        return Err(LuceneError::illegal_state(
          "Ordinals shouldn't use GCD, offset or table compression",
        ));
      }

      let mut addresses_input = self
        .data
        .random_access_slice(ords_entry.addresses_offset, ords_entry.addresses_length)?;
      if addresses_input.length()? > 0 {
        addresses_input.prefetch(0, 1)?;
      }
      let Some(ref meta) = ords_entry.addresses_meta else {
        return Err(LuceneError::illegal_state("addresses_meta is None"));
      };

      let addresses = DirectMonotonicReader::get_instance(meta, addresses_input)?;

      let mut slice = self
        .data
        .random_access_slice(ords_entry.base.values_offset, ords_entry.base.values_length)?;
      if slice.length()? > 0 {
        slice.prefetch(0, 1)?;
      }
      let values = DirectReader::get_instance(slice, ords_entry.base.bits_per_value as i32)?;

      let sub = if ords_entry.base.docs_with_field_offset == -1 {
        BaseSortedSetDocValuesEnum::Dense(DenseBaseSortedSetDocValues::new(
          self.max_doc,
          values,
          addresses,
        ))
      } else if ords_entry.base.docs_with_field_offset >= 0 {
        //sparse
        let disi = IndexedDISIImpl::new(
          self.data.as_ref(),
          ords_entry.base.docs_with_field_offset as usize,
          ords_entry.base.docs_with_field_length,
          ords_entry.base.jump_table_entry_count as i32,
          ords_entry.base.dense_rank_power,
          ords_entry.base.num_values as i64,
        )?;
        BaseSortedSetDocValuesEnum::Sparse(SparseBaseSortedSetDocValues::new(
          disi, values, addresses,
        ))
      } else {
        let ords = self.get_sorted_numeric(ords_entry)?;
        BaseSortedSetDocValuesEnum::Impl(BaseSortedSetDocValuesOrdinals::new(ords))
      };
      return Ok(Lucene90SortedSetDocValuesEnum::Multi(
        BaseSortedSetDocValues::new(entry.clone(), self.data.clone(), sub, self.merging)?,
      ));
    }

    let ords = self.get_sorted_numeric(ords_entry)?;
    let sub = BaseSortedSetDocValuesEnum::Impl(BaseSortedSetDocValuesOrdinals::new(ords));
    Ok(Lucene90SortedSetDocValuesEnum::Multi(
      BaseSortedSetDocValues::new(entry.clone(), self.data.clone(), sub, self.merging)?,
    ))
  }

  type DocValuesSkipper = Lucene90Skipper<I::IndexInput>;

  fn get_skipper(&self, field: &Arc<FieldInfo>) -> Result<Option<Self::DocValuesSkipper>> {
    let entry = self.skippers.get(&field.number).ok_or_else(|| {
      LuceneError::illegal_state(format!("Missing skipper entry for field {}", field.number))
    })?;

    let mut input = self.data.slice(
      "doc value skipper",
      entry.offset as usize,
      entry.length as usize,
    )?;

    if input.length()? > 0 {
      input.prefetch(0, 1)?;
    }
    Ok(Some(DocValuesSkipperImpl::new(input, entry.clone())))
  }

  fn check_integrity(&self) -> Result<()> {
    CodecUtil::checksum_entire_file(self.data.as_ref())?;
    Ok(())
  }

  fn get_merge_instance(&self) -> Result<Option<Self>>
  where
    Self: Sized,
  {
    Ok(Some(Lucene90DocValuesProducer::with_merging(
      self.numerics.clone(),
      self.binaries.clone(),
      self.sorted.clone(),
      self.sorted_sets.clone(),
      self.sorted_numerics.clone(),
      self.skippers.clone(),
      self.data.clone(),
      self.max_doc,
      self.version,
    )?))
  }
}
#[derive(Debug, Clone, Copy)]
pub struct DocValuesSkipperEntry {
  pub offset: i64,
  pub length: i64,
  pub min_value: i64,
  pub max_value: i64,
  pub doc_count: i32,
  pub max_doc_id: i32,
}
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct NumericEntry {
  pub table: Option<Arc<Vec<i64>>>,
  pub block_shift: i32,
  pub bits_per_value: i8,
  pub docs_with_field_offset: i64,
  pub docs_with_field_length: usize,
  pub jump_table_entry_count: i16,
  pub dense_rank_power: i8,
  pub num_values: usize,
  pub min_value: i64,
  pub gcd: i64,
  pub values_offset: usize,
  pub values_length: usize,
  pub value_jump_table_offset: i64, // -1 if no jump-table
}

pub struct BinaryEntry {
  pub data_offset: usize,
  pub data_length: usize,
  pub docs_with_field_offset: i64,
  pub docs_with_field_length: usize,
  pub jump_table_entry_count: i16,
  pub dense_rank_power: u8,
  pub num_docs_with_field: i32,
  pub min_length: i32,
  pub max_length: i32,
  pub addresses_offset: usize,
  pub addresses_length: usize,
  pub addresses_meta: Option<Meta>,
}

#[derive(Default)]
pub struct TermsDictEntry {
  pub terms_dict_size: i64,
  pub terms_addresses_meta: Option<Meta>,
  pub max_term_length: i32,
  pub terms_data_offset: usize,
  pub terms_data_length: usize,
  pub terms_addresses_offset: usize,
  pub terms_addresses_length: usize,
  pub terms_dict_index_shift: i32,
  pub terms_index_addresses_meta: Option<Meta>,
  pub terms_index_offset: usize,
  pub terms_index_length: usize,
  pub terms_index_addresses_offset: usize,
  pub terms_index_addresses_length: usize,
  pub max_block_length: i32,
}

pub struct SortedEntry {
  pub ords_entry: Arc<NumericEntry>,
  pub terms_dict_entry: Arc<TermsDictEntry>,
}

#[derive(Default)]
pub struct SortedSetEntry {
  pub single_value_entry: Option<Arc<SortedEntry>>,
  pub ords_entry: Option<Arc<SortedNumericEntry>>,
  pub terms_dict_entry: Option<Arc<TermsDictEntry>>,
}
#[derive(Default)]
pub struct SortedNumericEntry {
  pub base: Arc<NumericEntry>,
  pub num_docs_with_field: i32,
  pub addresses_meta: Option<Meta>,
  pub addresses_offset: usize,
  pub addresses_length: usize,
}
pub struct DenseNumericDocValues<R> {
  sub: DenseNumericDocValuesSubEnum<R>,
  max_doc: i32,
  doc: i32,
}
impl<R> DenseNumericDocValues<R> {
  fn new(base: DenseNumericDocValuesSubEnum<R>, max_doc: i32) -> Self {
    Self {
      sub: base,
      max_doc,
      doc: -1,
    }
  }
}

impl<R> DocValuesIterator for DenseNumericDocValues<R>
where
  R: RandomAccessInput,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    self.doc = target;
    Ok(true)
  }
}

impl<R> DocIdSetIterator for DenseNumericDocValues<R>
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
    } else {
      self.doc = target;
    }
    Ok(self.doc)
  }

  fn cost(&self) -> Result<i64> {
    Ok(self.max_doc as i64)
  }
}

impl<R> NumericDocValues for DenseNumericDocValues<R>
where
  R: RandomAccessInput,
{
  fn long_value(&mut self) -> Result<i64> {
    self.sub.long_value(self.doc)
  }
}

pub struct SparseNumericDocValues<I>
where
  I: IndexInput,
{
  sub: SparseNumericDocValuesSubEnum<I::RandomAccessSlice>,
  disi: IndexedDISIImpl<I::IndexInput, I::RandomAccessSlice>,
}

impl<I> SparseNumericDocValues<I>
where
  I: IndexInput,
{
  fn new(
    sub: SparseNumericDocValuesSubEnum<I::RandomAccessSlice>,
    disi: IndexedDISIImpl<I::IndexInput, I::RandomAccessSlice>,
  ) -> Self {
    Self { sub, disi }
  }
}

impl<I> DocValuesIterator for SparseNumericDocValues<I>
where
  I: IndexInput,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    self.disi.advance_exact(target)
  }
}

impl<I> DocIdSetIterator for SparseNumericDocValues<I>
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

impl<I> NumericDocValues for SparseNumericDocValues<I>
where
  I: IndexInput,
{
  fn long_value(&mut self) -> Result<i64> {
    <SparseNumericDocValuesSubEnum<I::RandomAccessSlice> as SparseNumericDocValuesBase<I>>::long_value(
      &mut self.sub,
      &mut self.disi,
    )
  }
}

pub struct DenseBinaryDocValues<R> {
  sub: DenseBinaryDocValuesBaseEnum<R>,
  max_doc: i32,
  doc: i32,
}
impl<R> DenseBinaryDocValues<R> {
  fn new(sub: DenseBinaryDocValuesBaseEnum<R>, max_doc: i32) -> Self {
    Self {
      sub,
      max_doc,
      doc: -1,
    }
  }
}

impl<R> DocValuesIterator for DenseBinaryDocValues<R>
where
  R: RandomAccessInput,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    self.doc = target;
    Ok(true)
  }
}

impl<R> DocIdSetIterator for DenseBinaryDocValues<R>
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
    } else {
      self.doc = target;
    }
    Ok(self.doc)
  }

  fn cost(&self) -> Result<i64> {
    Ok(self.max_doc as i64)
  }
}

impl<R> BinaryDocValues for DenseBinaryDocValues<R>
where
  R: RandomAccessInput,
{
  fn binary_value(&mut self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
    self.sub.binary_value(self.doc)
  }
}

pub struct SparseBinaryDocValues<I>
where
  I: IndexInput,
{
  sub: SparseBinaryDocValuesBaseEnum<I::RandomAccessSlice>,
  disi: IndexedDISIImpl<I::IndexInput, I::RandomAccessSlice>,
}

impl<I> SparseBinaryDocValues<I>
where
  I: IndexInput,
{
  fn new(
    sub: SparseBinaryDocValuesBaseEnum<I::RandomAccessSlice>,
    disi: IndexedDISIImpl<I::IndexInput, I::RandomAccessSlice>,
  ) -> Self {
    Self { sub, disi }
  }
}

impl<I> DocValuesIterator for SparseBinaryDocValues<I>
where
  I: IndexInput,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    self.disi.advance_exact(target)
  }
}

impl<I> DocIdSetIterator for SparseBinaryDocValues<I>
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

impl<I> BinaryDocValues for SparseBinaryDocValues<I>
where
  I: IndexInput,
{
  fn binary_value(&mut self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
    self.sub.binary_value(&mut self.disi)
  }
}

/// Reader for longs split into blocks of different bits per value.
/// The longs are requested by index and must be accessed in monotonically
/// increasing order.
///
/// Note: The order requirement could be removed as the jump tables allow for
/// backwards iteration.
///
/// Note 2: The `rank_slice` is only used if an advance of more than one block
/// is called. Its construction could be lazy.
struct VaryingBPVReader<R> {
  // 2 slices to avoid cache thrashing when using rank
  slice: R,
  rank_slice: Option<R>,
  entry: Arc<NumericEntry>,

  shift: usize,
  mul: i64,
  mask: usize,

  block: i32,
  delta: i64,
  offset: usize,
  block_end_offset: usize,
  merging: bool,
  values: Option<DirectPackedEnum<R>>,
}

impl<R> VaryingBPVReader<R>
where
  R: RandomAccessInput,
{
  fn new<I>(
    entry: Arc<NumericEntry>,
    slice: I::RandomAccessSlice,
    data: &I,
    merging: bool,
  ) -> Result<Self>
  where
    I: IndexInput<RandomAccessSlice = R>,
  {
    let rank_slice = if entry.value_jump_table_offset == -1 {
      None
    } else {
      let mut slice = data.random_access_slice(
        entry.value_jump_table_offset as usize,
        data.length()? - entry.value_jump_table_offset as usize,
      )?;
      if slice.length()? > 0 {
        slice.prefetch(0, 1)?;
      }
      Some(slice)
    };
    debug_assert!(entry.block_shift >= 0);
    let shift = entry.block_shift as usize;
    let mul = entry.gcd;
    let mask = (1 << shift) - 1;

    Ok(Self {
      slice,
      rank_slice,
      entry,
      shift,
      mul,
      mask: mask as usize,
      block: -1,
      delta: 0,
      offset: 0,
      block_end_offset: 0,
      merging,
      values: None,
    })
  }

  fn get_long_value(&mut self, index: usize) -> Result<i64> {
    let block = index >> self.shift;

    if self.block < 0 || self.block as usize != block {
      let mut bits_per_value;
      loop {
        if let Some(ref mut rank_slice) = self.rank_slice
          && block != (self.block + 1) as usize
        {
          self.block_end_offset = (rank_slice.read_long(block * BitUtil::LONG_BYTES)? as usize)
            .checked_sub(self.entry.values_offset)
            .ok_or_else(|| LuceneError::illegal_state("underflow?"))?;
          self.block = match block.checked_sub(1) {
            Some(v) => v.try_convert()?,
            None => -1,
          }
        }

        self.offset = self.block_end_offset;
        bits_per_value = self.slice.read_byte(self.offset)? as i32;
        self.offset += 1;

        self.delta = self.slice.read_long(self.offset)?;
        self.offset += BitUtil::LONG_BYTES;

        if bits_per_value == 0 {
          self.block_end_offset = self.offset;
        } else {
          let length = self.slice.read_int(self.offset)? as usize;
          self.offset += BitUtil::INT_BYTES;
          self.block_end_offset = self.offset + length;
        }

        self.block += 1;
        if self.block as usize == block {
          break;
        }
      }
      let num_values = std::cmp::min(
        1 << self.shift,
        self.entry.num_values - (block << self.shift),
      );

      self.values = if bits_per_value == 0 {
        Some(DirectPackedEnum::Zeroes(Zeroes))
      } else {
        Some(get_direct_reader_instance(
          self.merging,
          None,
          bits_per_value,
          self.offset,
          num_values,
        )?)
      };
    }
    let v = self
      .values
      .as_mut()
      .ok_or_else(|| LuceneError::illegal_state("values is None"))?;
    Ok(
      self
        .mul
        .wrapping_mul(v.read_from_slice(index & self.mask, Some(&mut self.slice))?)
        .wrapping_add(self.delta),
    )
  }
}

pub struct DocValuesSkipperImpl<I> {
  min_doc_id: [i32; Lucene90DocValuesFormat::SKIP_INDEX_MAX_LEVEL],
  max_doc_id: [i32; Lucene90DocValuesFormat::SKIP_INDEX_MAX_LEVEL],
  min_value: [i64; Lucene90DocValuesFormat::SKIP_INDEX_MAX_LEVEL],
  max_value: [i64; Lucene90DocValuesFormat::SKIP_INDEX_MAX_LEVEL],
  doc_count: [i32; Lucene90DocValuesFormat::SKIP_INDEX_MAX_LEVEL],
  levels: usize,
  input: I,
  entry: Arc<DocValuesSkipperEntry>,
}
impl<I> DocValuesSkipperImpl<I> {
  pub fn new(input: I, entry: Arc<DocValuesSkipperEntry>) -> Self {
    Self {
      min_doc_id: [-1; Lucene90DocValuesFormat::SKIP_INDEX_MAX_LEVEL],
      max_doc_id: [-1; Lucene90DocValuesFormat::SKIP_INDEX_MAX_LEVEL],
      min_value: [0; Lucene90DocValuesFormat::SKIP_INDEX_MAX_LEVEL],
      max_value: [0; Lucene90DocValuesFormat::SKIP_INDEX_MAX_LEVEL],
      doc_count: [0; Lucene90DocValuesFormat::SKIP_INDEX_MAX_LEVEL],
      levels: 1,
      input,
      entry,
    }
  }
}
impl<I> DocValuesSkipper for DocValuesSkipperImpl<I>
where
  I: IndexInput,
{
  fn advance(&mut self, target: i32) -> Result<()> {
    if target > self.entry.max_doc_id {
      // skipper is exhausted
      for i in 0..Lucene90DocValuesFormat::SKIP_INDEX_MAX_LEVEL {
        self.min_doc_id[i] = NO_MORE_DOCS;
        self.max_doc_id[i] = NO_MORE_DOCS;
      }
    } else {
      // find next interval
      debug_assert!(
        target > self.max_doc_id[0],
        "target must be bigger than current interval"
      );

      loop {
        self.levels = self.input.read_byte()? as usize;

        debug_assert!(
          self.levels <= Lucene90DocValuesFormat::SKIP_INDEX_MAX_LEVEL && self.levels > 0,
          "level out of range [{}]",
          self.levels
        );

        let mut valid = true;

        // check if current interval is competitive or we can jump to
        // the next position
        for level in (0..self.levels).rev() {
          let max_doc = self.input.read_int()?;
          self.max_doc_id[level] = max_doc;
          if max_doc < target {
            IndexInput::skip_bytes(&mut self.input, SKIP_INDEX_JUMP_LENGTH_PER_LEVEL[level])?;
            valid = false;
            break;
          }
          self.min_doc_id[level] = self.input.read_int()?;
          self.max_value[level] = self.input.read_long()?;
          self.min_value[level] = self.input.read_long()?;
          self.doc_count[level] = self.input.read_int()?;
        }

        if valid {
          // adjust levels
          while self.levels < Lucene90DocValuesFormat::SKIP_INDEX_MAX_LEVEL
            && self.max_doc_id[self.levels] >= target
          {
            self.levels += 1;
          }
          break;
        }
      }
    }
    Ok(())
  }

  fn num_levels(&self) -> usize {
    self.levels
  }

  fn min_doc_id_with_level(&self, level: usize) -> i32 {
    self.min_doc_id[level]
  }

  fn max_doc_id_with_level(&self, level: usize) -> i32 {
    self.max_doc_id[level]
  }

  fn min_value_with_level(&self, level: usize) -> i64 {
    self.min_value[level]
  }

  fn max_value_with_level(&self, level: usize) -> i64 {
    self.max_value[level]
  }

  fn doc_count_with_level(&self, level: usize) -> i32 {
    self.doc_count[level]
  }

  fn min_value(&self) -> i64 {
    self.entry.min_value
  }

  fn max_value(&self) -> i64 {
    self.entry.max_value
  }

  fn doc_count(&self) -> i32 {
    self.entry.doc_count
  }
}

pub trait DenseNumericDocValuesBase {
  fn long_value(&mut self, doc: i32) -> Result<i64>;
}
pub struct DenseNumericDocValuesBaseImpl {
  min_values: i64,
}
impl DenseNumericDocValuesBase for DenseNumericDocValuesBaseImpl {
  fn long_value(&mut self, _doc: i32) -> Result<i64> {
    Ok(self.min_values)
  }
}
pub struct DenseNumericDocValuesBaseImpl1<R> {
  vbpv_reader: VaryingBPVReader<R>,
}
impl<R> DenseNumericDocValuesBase for DenseNumericDocValuesBaseImpl1<R>
where
  R: RandomAccessInput,
{
  fn long_value(&mut self, doc: i32) -> Result<i64> {
    self.vbpv_reader.get_long_value(doc.try_convert()?)
  }
}
pub struct DenseNumericDocValuesBaseImpl2<R> {
  table: Arc<Vec<i64>>,
  values: DirectPackedEnum<R>,
}
impl<R> DenseNumericDocValuesBase for DenseNumericDocValuesBaseImpl2<R>
where
  R: RandomAccessInput,
{
  fn long_value(&mut self, doc: i32) -> Result<i64> {
    Ok(self.table[self.values.get_mut(doc as usize)? as usize])
  }
}
pub struct DenseNumericDocValuesBaseImpl3<R> {
  values: DirectPackedEnum<R>,
}
impl<R> DenseNumericDocValuesBase for DenseNumericDocValuesBaseImpl3<R>
where
  R: RandomAccessInput,
{
  fn long_value(&mut self, doc: i32) -> Result<i64> {
    self.values.get_mut(doc as usize)
  }
}
pub struct DenseNumericDocValuesBaseImpl4<R> {
  values: DirectPackedEnum<R>,
  mul: i64,
  delta: i64,
}
impl<R> DenseNumericDocValuesBase for DenseNumericDocValuesBaseImpl4<R>
where
  R: RandomAccessInput,
{
  fn long_value(&mut self, doc: i32) -> Result<i64> {
    Ok(
      self
        .mul
        .wrapping_mul(self.values.get_mut(doc as usize)?)
        .wrapping_add(self.delta),
    )
  }
}

pub trait SparseNumericDocValuesBase<I>
where
  I: IndexInput,
{
  fn long_value(
    &mut self,
    disi: &mut IndexedDISIImpl<I::IndexInput, I::RandomAccessSlice>,
  ) -> Result<i64>;
}
pub struct SparseNumericDocValuesBaseImpl {
  min_values: i64,
}
impl<I> SparseNumericDocValuesBase<I> for SparseNumericDocValuesBaseImpl
where
  I: IndexInput,
{
  fn long_value(
    &mut self,
    _disi: &mut IndexedDISIImpl<I::IndexInput, I::RandomAccessSlice>,
  ) -> Result<i64> {
    Ok(self.min_values)
  }
}
pub struct SparseNumericDocValuesBaseImpl1<R> {
  vbpv_reader: VaryingBPVReader<R>,
}
impl<I> SparseNumericDocValuesBase<I>
  for SparseNumericDocValuesBaseImpl1<I::RandomAccessSlice>
where
  I: IndexInput,
{
  fn long_value(
    &mut self,
    disi: &mut IndexedDISIImpl<I::IndexInput, I::RandomAccessSlice>,
  ) -> Result<i64> {
    let index = disi.index_u();
    self.vbpv_reader.get_long_value(index)
  }
}
pub struct SparseNumericDocValuesBaseImpl2<R> {
  table: Arc<Vec<i64>>,
  values: DirectPackedEnum<R>,
}
impl<I> SparseNumericDocValuesBase<I>
  for SparseNumericDocValuesBaseImpl2<I::RandomAccessSlice>
where
  I: IndexInput,
{
  fn long_value(
    &mut self,
    disi: &mut IndexedDISIImpl<I::IndexInput, I::RandomAccessSlice>,
  ) -> Result<i64> {
    Ok(self.table[self.values.get_mut(disi.index_u())? as usize])
  }
}
pub struct SparseNumericDocValuesBaseImpl3<R> {
  values: DirectPackedEnum<R>,
}
impl<I> SparseNumericDocValuesBase<I>
  for SparseNumericDocValuesBaseImpl3<I::RandomAccessSlice>
where
  I: IndexInput,
{
  fn long_value(
    &mut self,
    disi: &mut IndexedDISIImpl<I::IndexInput, I::RandomAccessSlice>,
  ) -> Result<i64> {
    self.values.get_mut(disi.index_u())
  }
}
pub struct SparseNumericDocValuesBaseImpl4<R> {
  values: DirectPackedEnum<R>,
  mul: i64,
  delta: i64,
}
impl<I> SparseNumericDocValuesBase<I>
  for SparseNumericDocValuesBaseImpl4<I::RandomAccessSlice>
where
  I: IndexInput,
{
  fn long_value(
    &mut self,
    disi: &mut IndexedDISIImpl<I::IndexInput, I::RandomAccessSlice>,
  ) -> Result<i64> {
    Ok(
      self
        .mul
        .wrapping_mul(self.values.get_mut(disi.index_u())?)
        .wrapping_add(self.delta),
    )
  }
}

pub struct LongValuesImpl {
  min_values: i64,
}
impl LongValues for LongValuesImpl {
  fn get_mut(&mut self, index: usize) -> Result<i64> {
    self.get(index)
  }

  fn get(&self, _index: usize) -> Result<i64> {
    Ok(self.min_values)
  }
}
pub struct LongValuesImpl1<R> {
  vbpv_reader: VaryingBPVReader<R>,
}
impl<R> LongValues for LongValuesImpl1<R>
where
  R: RandomAccessInput,
{
  fn get_mut(&mut self, index: usize) -> Result<i64> {
    self.vbpv_reader.get_long_value(index)
  }
}
pub struct LongValuesImpl2<R> {
  table: Arc<Vec<i64>>,
  values: DirectPackedEnum<R>,
}
impl<R> LongValues for LongValuesImpl2<R>
where
  R: RandomAccessInput,
{
  fn get_mut(&mut self, index: usize) -> Result<i64> {
    Ok(self.table[self.values.get_mut(index)? as usize])
  }
}
pub struct LongValuesImpl3<R> {
  values: DirectPackedEnum<R>,
  gcd: i64,
  min_value: i64,
}
impl<R> LongValues for LongValuesImpl3<R>
where
  R: RandomAccessInput,
{
  fn get_mut(&mut self, index: usize) -> Result<i64> {
    Ok(
      self
        .gcd
        .wrapping_mul(self.values.get_mut(index)?)
        .wrapping_add(self.min_value),
    )
  }
}
pub struct LongValuesImpl4<R> {
  values: DirectPackedEnum<R>,
  min_value: i64,
}
impl<R> LongValues for LongValuesImpl4<R>
where
  R: RandomAccessInput,
{
  fn get_mut(&mut self, index: usize) -> Result<i64> {
    Ok(self.values.get_mut(index)?.wrapping_add(self.min_value))
  }
}

pub trait DenseBinaryDocValuesBase {
  fn binary_value(&mut self, doc: i32) -> Result<Cow<'_, BytesRef<Vec<u8>>>>;
}

pub struct DenseBinaryDocValuesBaseImpl<R> {
  bytes_slice: R,
  length: i32,
  bytes: BytesRef<Vec<u8>>,
}
impl<R> DenseBinaryDocValuesBase for DenseBinaryDocValuesBaseImpl<R>
where
  R: RandomAccessInput,
{
  fn binary_value(&mut self, doc: i32) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
    self.bytes_slice.read_bytes(
      (doc * self.length) as usize,
      &mut self.bytes.bytes,
      0,
      self.length as usize,
    )?;
    Ok(Cow::Borrowed(&self.bytes))
  }
}
pub struct DenseBinaryDocValuesBaseImpl1<R> {
  bytes_slice: R,
  bytes: BytesRef<Vec<u8>>,
  addresses: DirectMonotonicReader<R>,
}
impl<R> DenseBinaryDocValuesBase for DenseBinaryDocValuesBaseImpl1<R>
where
  R: RandomAccessInput,
{
  fn binary_value(&mut self, doc: i32) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
    let start_offset = self.addresses.get_mut(doc as usize)?;
    self.bytes.length = (self.addresses.get_mut((doc + 1) as usize)? - start_offset) as usize;
    self.bytes_slice.read_bytes(
      start_offset as usize,
      &mut self.bytes.bytes,
      0,
      self.bytes.length,
    )?;
    Ok(Cow::Borrowed(&self.bytes))
  }
}

pub trait SparseBinaryDocValuesBase<I, R>
where
  I: IndexInput,
  R: RandomAccessInput,
{
  fn binary_value(
    &mut self,
    disi: &mut IndexedDISIImpl<I, R>,
  ) -> Result<Cow<'_, BytesRef<Vec<u8>>>>;
}
pub struct SparseBinaryDocValuesBaseImpl<R> {
  bytes_slice: R,
  bytes: BytesRef<Vec<u8>>,
  length: i32,
}
impl<I, R> SparseBinaryDocValuesBase<I, R> for SparseBinaryDocValuesBaseImpl<R>
where
  I: IndexInput,
  R: RandomAccessInput,
{
  fn binary_value(
    &mut self,
    disi: &mut IndexedDISIImpl<I, R>,
  ) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
    let length = self.length as usize;
    let pos = disi.index_u() * length;
    self
      .bytes_slice
      .read_bytes(pos, &mut self.bytes.bytes, 0, length)?;
    Ok(Cow::Borrowed(&self.bytes))
  }
}
pub struct SparseBinaryDocValuesBaseImpl1<R> {
  bytes_slice: R,
  bytes: BytesRef<Vec<u8>>,
  addresses: DirectMonotonicReader<R>,
}
impl<I, R> SparseBinaryDocValuesBase<I, R> for SparseBinaryDocValuesBaseImpl1<R>
where
  I: IndexInput,
  R: RandomAccessInput,
{
  fn binary_value(
    &mut self,
    disi: &mut IndexedDISIImpl<I, R>,
  ) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
    let index = disi.index() as usize;
    let start_offset = self.addresses.get_mut(index)?;
    self.bytes.length = (self.addresses.get_mut(index + 1)? - start_offset) as usize;
    self.bytes_slice.read_bytes(
      start_offset as usize,
      &mut self.bytes.bytes,
      0,
      self.bytes.length,
    )?;
    Ok(Cow::Borrowed(&self.bytes))
  }
}

pub struct DenseBaseSortedDocValues<R> {
  doc: i32,
  max_doc: i32,
  value: DirectPackedEnum<R>,
}
impl<R> DenseBaseSortedDocValues<R> {
  fn new(max_doc: i32, value: DirectPackedEnum<R>) -> Self {
    Self {
      doc: -1,
      max_doc,
      value,
    }
  }
}

impl<R> DocValuesIterator for DenseBaseSortedDocValues<R>
where
  R: RandomAccessInput,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    self.doc = target;
    Ok(true)
  }
}

impl<R> DocIdSetIterator for DenseBaseSortedDocValues<R>
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
    } else {
      self.doc = target;
    }
    Ok(self.doc)
  }

  fn cost(&self) -> Result<i64> {
    Ok(self.max_doc as i64)
  }
}

impl<R> SortedDocValues for DenseBaseSortedDocValues<R>
where
  R: RandomAccessInput,
{
  fn ord_value(&mut self) -> Result<i32> {
    Ok(self.value.get_mut(self.doc as usize)? as i32)
  }

  type TermsEnum<'a>
    = DummyTermsEnum
  where
    R: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    Err(LuceneError::unsupported_operation(""))
  }
}

pub struct SparseBaseSortedDocValues<I>
where
  I: IndexInput,
{
  disi: IndexedDISIImpl<I::IndexInput, I::RandomAccessSlice>,
  value: DirectPackedEnum<I::RandomAccessSlice>,
}

impl<I> SparseBaseSortedDocValues<I>
where
  I: IndexInput,
{
  fn new(
    disi: IndexedDISIImpl<I::IndexInput, I::RandomAccessSlice>,
    value: DirectPackedEnum<I::RandomAccessSlice>,
  ) -> Self {
    Self { disi, value }
  }
}

impl<I> DocValuesIterator for SparseBaseSortedDocValues<I>
where
  I: IndexInput,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    self.disi.advance_exact(target)
  }
}

impl<I> DocIdSetIterator for SparseBaseSortedDocValues<I>
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

impl<I> SortedDocValues for SparseBaseSortedDocValues<I>
where
  I: IndexInput,
{
  fn ord_value(&mut self) -> Result<i32> {
    Ok(self.value.get_mut(self.disi.index_u())? as i32)
  }

  type TermsEnum<'a>
    = DummyTermsEnum
  where
    I: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    Err(LuceneError::unsupported_operation(""))
  }
}
pub struct BaseSortedDocValuesOrdinals<I>
where
  I: IndexInput,
{
  ords: Lucene90NumericDocValuesEnum<I>,
}

impl<I> BaseSortedDocValuesOrdinals<I>
where
  I: IndexInput,
{
  fn new(ords: Lucene90NumericDocValuesEnum<I>) -> Self {
    Self { ords }
  }
}

impl<I> DocValuesIterator for BaseSortedDocValuesOrdinals<I>
where
  I: IndexInput,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    self.ords.advance_exact(target)
  }
}

impl<I> DocIdSetIterator for BaseSortedDocValuesOrdinals<I>
where
  I: IndexInput,
{
  fn doc_id(&self) -> i32 {
    self.ords.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.ords.next_doc()
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.ords.advance(target)
  }

  fn cost(&self) -> Result<i64> {
    self.ords.cost()
  }
}

impl<I> SortedDocValues for BaseSortedDocValuesOrdinals<I>
where
  I: IndexInput,
{
  fn ord_value(&mut self) -> Result<i32> {
    Ok(self.ords.long_value()? as i32)
  }

  type TermsEnum<'a>
    = DummyTermsEnum
  where
    I: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    Err(LuceneError::unsupported_operation(""))
  }
}

pub struct BaseSortedDocValues<I>
where
  I: IndexInput,
{
  entry: Arc<SortedEntry>,
  terms_enum: TermsDict<I>,
  sub: BaseSortedDocValuesEnum<I>,
  data: Arc<I>,
  merging: bool,
}

impl<I> BaseSortedDocValues<I>
where
  I: IndexInput,
{
  fn new(
    entry: Arc<SortedEntry>,
    data: Arc<I>,
    sub: BaseSortedDocValuesEnum<I>,
    merging: bool,
  ) -> Result<Self> {
    let terms_enum = TermsDict::new(entry.terms_dict_entry.clone(), data.as_ref(), merging)?;
    Ok(Self {
      entry,
      terms_enum,
      sub,
      data,
      merging,
    })
  }
}

impl<I> DocValuesIterator for BaseSortedDocValues<I>
where
  I: IndexInput,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    self.sub.advance_exact(target)
  }
}

impl<I> DocIdSetIterator for BaseSortedDocValues<I>
where
  I: IndexInput,
{
  fn doc_id(&self) -> i32 {
    self.sub.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.sub.next_doc()
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.sub.advance(target)
  }

  fn cost(&self) -> Result<i64> {
    self.sub.cost()
  }
}

impl<I> SortedDocValues for BaseSortedDocValues<I>
where
  I: IndexInput,
{
  fn ord_value(&mut self) -> Result<i32> {
    self.sub.ord_value()
  }

  fn lookup_ord(&mut self, ord: i32) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
    self.terms_enum.seek_exact_with_ord(ord as i64)?;
    self.terms_enum.term()
  }

  fn get_value_count(&self) -> Result<i32> {
    let v: i32 = self.entry.terms_dict_entry.terms_dict_size.try_convert()?;
    Ok(v)
  }

  fn lookup_term(&mut self, key: &BytesRef<Vec<u8>>) -> Result<i32> {
    match self.terms_enum.seek_ceil(key)? {
      SeekStatus::Found => {
        let v = self.terms_enum.ord()?.try_convert()?;
        Ok(v)
      },
      SeekStatus::NotFound | SeekStatus::End => {
        let v = (-1 - self.terms_enum.ord()?).try_convert()?;
        Ok(v)
      },
    }
  }
  type TermsEnum<'a>
    = TermsDict<I>
  where
    I: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    TermsDict::new(
      self.entry.terms_dict_entry.clone(),
      self.data.as_ref(),
      self.merging,
    )
  }
}
pub struct DenseBaseSortedSetDocValues<R> {
  max_doc: i32,
  doc: i32,
  curr: i64,
  count: i32,
  value: DirectPackedEnum<R>,
  addresses: DirectMonotonicReader<R>,
}
impl<R> DenseBaseSortedSetDocValues<R> {
  fn new(max_doc: i32, value: DirectPackedEnum<R>, addresses: DirectMonotonicReader<R>) -> Self {
    Self {
      max_doc,
      doc: -1,
      curr: 0,
      count: 0,
      value,
      addresses,
    }
  }
}

impl<R> DocValuesIterator for DenseBaseSortedSetDocValues<R>
where
  R: RandomAccessInput,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    self.curr = self.addresses.get_mut(target as usize)?;
    let end = self.addresses.get_mut((target as usize) + 1)?;
    self.count = (end - self.curr) as i32;
    self.doc = target;
    Ok(true)
  }
}

impl<R> DocIdSetIterator for DenseBaseSortedSetDocValues<R>
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
      return Ok(NO_MORE_DOCS);
    }
    let target_u = target as usize;
    self.curr = self.addresses.get_mut(target_u)?;
    let end = self.addresses.get_mut((target_u) + 1)?;
    self.count = (end - self.curr) as i32;
    self.doc = target;

    Ok(self.doc)
  }

  fn cost(&self) -> Result<i64> {
    Ok(self.max_doc as i64)
  }
}

impl<R> SortedSetDocValues for DenseBaseSortedSetDocValues<R>
where
  R: RandomAccessInput,
{
  fn next_ord(&mut self) -> Result<i64> {
    let ord = self.value.get_mut(self.curr as usize)?;
    self.curr += 1;
    Ok(ord)
  }

  fn doc_value_count(&mut self) -> Result<i32> {
    Ok(self.count)
  }

  type TermsEnum<'a>
    = DummyTermsEnum
  where
    R: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    Err(LuceneError::unsupported_operation(""))
  }

  type SortedDocValues = DummySortedDocValues;
}

pub struct SparseBaseSortedSetDocValues<I>
where
  I: IndexInput,
{
  disi: IndexedDISIImpl<I::IndexInput, I::RandomAccessSlice>,
  set: bool,
  curr: i64,
  count: i32,
  value: DirectPackedEnum<I::RandomAccessSlice>,
  addresses: DirectMonotonicReader<I::RandomAccessSlice>,
}

impl<I> SparseBaseSortedSetDocValues<I>
where
  I: IndexInput,
{
  fn new(
    disi: IndexedDISIImpl<I::IndexInput, I::RandomAccessSlice>,
    value: DirectPackedEnum<I::RandomAccessSlice>,
    addresses: DirectMonotonicReader<I::RandomAccessSlice>,
  ) -> Self {
    Self {
      disi,
      set: false,
      curr: 0,
      count: 0,
      value,
      addresses,
    }
  }
  fn set(&mut self) -> Result<()> {
    if !self.set {
      let index = self.disi.index_u();
      self.curr = self.addresses.get_mut(index)?;
      let end = self.addresses.get_mut(index + 1)?;
      self.count = (end - self.curr) as i32;
      self.set = true;
    }
    Ok(())
  }
}

impl<I> DocValuesIterator for SparseBaseSortedSetDocValues<I>
where
  I: IndexInput,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    self.set = false;
    self.disi.advance_exact(target)
  }
}

impl<I> DocIdSetIterator for SparseBaseSortedSetDocValues<I>
where
  I: IndexInput,
{
  fn doc_id(&self) -> i32 {
    self.disi.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.set = false;
    self.disi.next_doc()
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.set = false;
    self.disi.advance(target)
  }

  fn cost(&self) -> Result<i64> {
    self.disi.cost()
  }
}

impl<I> SortedSetDocValues for SparseBaseSortedSetDocValues<I>
where
  I: IndexInput,
{
  fn next_ord(&mut self) -> Result<i64> {
    self.set()?;
    let ord = self.value.get_mut(self.curr as usize)?;
    self.curr += 1;
    Ok(ord)
  }

  fn doc_value_count(&mut self) -> Result<i32> {
    self.set()?;
    Ok(self.count)
  }

  type TermsEnum<'a>
    = DummyTermsEnum
  where
    I: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    Err(LuceneError::unsupported_operation(""))
  }

  type SortedDocValues = DummySortedDocValues;
}

pub struct BaseSortedSetDocValuesOrdinals<I>
where
  I: IndexInput,
{
  ords: Lucene90SortedNumericDocValuesEnum<I>,
}

impl<I> BaseSortedSetDocValuesOrdinals<I>
where
  I: IndexInput,
{
  fn new(ords: Lucene90SortedNumericDocValuesEnum<I>) -> Self {
    Self { ords }
  }
}

impl<I> DocValuesIterator for BaseSortedSetDocValuesOrdinals<I>
where
  I: IndexInput,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    self.ords.advance_exact(target)
  }
}

impl<I> DocIdSetIterator for BaseSortedSetDocValuesOrdinals<I>
where
  I: IndexInput,
{
  fn doc_id(&self) -> i32 {
    self.ords.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.ords.next_doc()
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.ords.advance(target)
  }

  fn cost(&self) -> Result<i64> {
    self.ords.cost()
  }
}

impl<I> SortedSetDocValues for BaseSortedSetDocValuesOrdinals<I>
where
  I: IndexInput,
{
  fn next_ord(&mut self) -> Result<i64> {
    self.ords.next_value()
  }

  fn doc_value_count(&mut self) -> Result<i32> {
    self.ords.doc_value_count()
  }
  type TermsEnum<'a>
    = DummyTermsEnum
  where
    I: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    Err(LuceneError::unsupported_operation(""))
  }

  type SortedDocValues = DummySortedDocValues;
}

pub struct BaseSortedSetDocValues<I>
where
  I: IndexInput,
{
  entry: Arc<SortedSetEntry>,
  terms_enum: TermsDict<I>,
  sub: BaseSortedSetDocValuesEnum<I>,
  data: Arc<I>,
  merging: bool,
}

impl<I> BaseSortedSetDocValues<I>
where
  I: IndexInput,
{
  fn new(
    entry: Arc<SortedSetEntry>,
    data: Arc<I>,
    sub: BaseSortedSetDocValuesEnum<I>,
    merging: bool,
  ) -> Result<Self> {
    let terms_dict_entry = entry
      .terms_dict_entry
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("TermsDictEntry's terms_dict_entry is None"))?
      .clone();
    let terms_enum = TermsDict::new(terms_dict_entry, data.as_ref(), merging)?;
    Ok(Self {
      entry,
      terms_enum,
      sub,
      data,
      merging,
    })
  }
}

impl<I> DocValuesIterator for BaseSortedSetDocValues<I>
where
  I: IndexInput,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    self.sub.advance_exact(target)
  }
}

impl<I> DocIdSetIterator for BaseSortedSetDocValues<I>
where
  I: IndexInput,
{
  fn doc_id(&self) -> i32 {
    self.sub.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.sub.next_doc()
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.sub.advance(target)
  }

  fn cost(&self) -> Result<i64> {
    self.sub.cost()
  }
}

impl<I> SortedSetDocValues for BaseSortedSetDocValues<I>
where
  I: IndexInput,
{
  fn next_ord(&mut self) -> Result<i64> {
    self.sub.next_ord()
  }

  fn doc_value_count(&mut self) -> Result<i32> {
    self.sub.doc_value_count()
  }

  fn lookup_ord(&mut self, ord: i64) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
    self.terms_enum.seek_exact_with_ord(ord)?;
    self.terms_enum.term()
  }

  fn get_value_count(&self) -> Result<i64> {
    let entry = self
      .entry
      .terms_dict_entry
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("TermsDictEntry's terms_dict_entry is None"))?;
    Ok(entry.terms_dict_size)
  }

  fn lookup_term(&mut self, key: &BytesRef<Vec<u8>>) -> Result<i64> {
    match self.terms_enum.seek_ceil(key)? {
      SeekStatus::Found => Ok(self.terms_enum.ord()?),
      SeekStatus::NotFound | SeekStatus::End => {
        let ord = self.terms_enum.ord()?;
        Ok(-1 - ord)
      },
    }
  }

  type TermsEnum<'a>
    = TermsDict<I>
  where
    I: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    let terms_dict_entry = self
      .entry
      .terms_dict_entry
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("TermsDictEntry's terms_dict_entry is None"))?
      .clone();
    TermsDict::new(terms_dict_entry, self.data.as_ref(), self.merging)
  }

  type SortedDocValues = DummySortedDocValues;
}

pub struct TermsDict<I>
where
  I: IndexInput,
{
  entry: Arc<TermsDictEntry>,
  block_addresses: DirectMonotonicReader<I::RandomAccessSlice>,
  bytes: I::IndexInput,
  block_mask: u64,
  index_addresses: DirectMonotonicReader<I::RandomAccessSlice>,
  index_bytes: I::RandomAccessSlice,
  term: BytesRef<Vec<u8>>,
  ord: i64,
  block_input: ByteArrayDataInput<Vec<u8>>,
  block_buffer_offset: usize,
  block_buffer_length: usize,
  current_compressed_block_start: Option<usize>,
  current_compressed_block_end: Option<usize>,
}

impl<I> TermsDict<I>
where
  I: IndexInput,
{
  const LZ4_DECOMPRESSOR_PADDING: i32 = 7;

  pub fn new(entry: Arc<TermsDictEntry>, data: &I, merging: bool) -> Result<Self> {
    let addresses_slice =
      data.random_access_slice(entry.terms_addresses_offset, entry.terms_addresses_length)?;

    let Some(ref meta) = entry.terms_addresses_meta else {
      return Err(LuceneError::illegal_state(
        "TermsDictEntry's terms_addresses_meta is None",
      ));
    };

    let block_addresses =
      DirectMonotonicReader::get_instance_with_merging(meta, addresses_slice, merging)?;

    let bytes = data.slice("terms", entry.terms_data_offset, entry.terms_data_length)?;

    let block_mask = (1u64 << Lucene90DocValuesFormat::TERMS_DICT_BLOCK_LZ4_SHIFT) - 1;

    let index_addresses_slice = data.random_access_slice(
      entry.terms_index_addresses_offset,
      entry.terms_index_addresses_length,
    )?;

    let Some(ref meta) = entry.terms_index_addresses_meta else {
      return Err(LuceneError::illegal_state(
        "TermsDictEntry's terms_index_addresses_meta is None",
      ));
    };

    let index_addresses =
      DirectMonotonicReader::get_instance_with_merging(meta, index_addresses_slice, merging)?;

    let index_bytes =
      data.random_access_slice(entry.terms_index_offset, entry.terms_index_length)?;

    let term = BytesRef::with_capacity(entry.max_term_length as usize)?;
    // add the max term length for the dictionary
    // add 7 padding bytes can help decompression run faster.
    let buffer_size =
      entry.max_block_length + entry.max_term_length + Self::LZ4_DECOMPRESSOR_PADDING;

    let block_buffer = vec![0u8; buffer_size as usize];
    let block_input = ByteArrayDataInput::with_bytes(block_buffer);

    let sub = Self {
      entry,
      block_addresses,
      bytes,
      block_mask,
      index_addresses,
      index_bytes,
      term,
      ord: -1,
      block_input,
      block_buffer_offset: 0,
      block_buffer_length: buffer_size as usize,
      current_compressed_block_start: None,
      current_compressed_block_end: None,
    };
    Ok(sub)
  }

  fn get_term_from_index(&mut self, index: usize) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
    debug_assert!(
      index <= ((self.entry.terms_dict_size - 1) as usize >> self.entry.terms_dict_index_shift),
      "index {index} out of range"
    );

    let start = self.index_addresses.get_mut(index)? as usize;
    let end = self.index_addresses.get_mut(index + 1)? as usize;
    let len = end - start;
    self.term.length = len;

    self.term.bytes.access_mut(|bytes| {
      self.index_bytes.read_bytes(start, bytes, 0, len)?;
      // Help the compiler infer types.
      Ok::<(), LuceneError>(())
    })?;

    Ok(Cow::Borrowed(&self.term))
  }
  fn seek_terms_index(&mut self, text: &BytesRef<Vec<u8>>) -> Result<i64> {
    let mut lo: i64 = 0;
    let mut hi: i64 = (self.entry.terms_dict_size - 1) >> self.entry.terms_dict_index_shift;

    while lo <= hi {
      let mid = (lo + hi) >> 1;
      let term = self.get_term_from_index(mid as usize)?;
      let cmp = term.as_ref().cmp(text).to_int();
      if cmp <= 0 {
        lo = mid + 1;
      } else {
        hi = mid - 1;
      }
    }

    debug_assert!(
      hi < 0
        || self
          .get_term_from_index(hi as usize)?
          .as_ref()
          .cmp(text)
          .to_int()
          <= 0,
      "hi check failed"
    );
    debug_assert!(
      hi == ((self.entry.terms_dict_size - 1) >> self.entry.terms_dict_index_shift)
        || self
          .get_term_from_index((hi + 1) as usize)?
          .as_ref()
          .cmp(text)
          .to_int()
          > 0,
      "hi+1 check failed"
    );
    // return -1 iff empty term dict
    debug_assert!(
      (hi < 0) ^ (self.entry.terms_dict_size > 0),
      "empty term dict assertion failed"
    );
    Ok(hi)
  }
  fn get_first_term_from_block(&mut self, block: usize) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
    debug_assert!(
      block
        <= (((self.entry.terms_dict_size - 1) as usize)
          >> Lucene90DocValuesFormat::TERMS_DICT_BLOCK_LZ4_SHIFT)
    );

    let block_address = self.block_addresses.get_mut(block)?;
    self.bytes.seek(block_address as usize)?;

    let len = self.bytes.read_vint()?.try_convert()?;
    self.term.length = len;
    self.term.bytes.access_mut(|bytes| {
      self.bytes.read_bytes(bytes, 0, len)?;
      // Help the compiler infer types.
      Ok::<(), LuceneError>(())
    })?;

    Ok(Cow::Borrowed(&self.term))
  }
  fn seek_block(&mut self, text: &BytesRef<Vec<u8>>) -> Result<i64> {
    let index = self.seek_terms_index(text)?;

    if index == -1 {
      // Empty term dictionary
      self.ord = 0;
      return Ok(-2);
    }

    let ord_lo = index << self.entry.terms_dict_index_shift;
    let ord_hi = std::cmp::min(
      self.entry.terms_dict_size,
      ord_lo + (1 << self.entry.terms_dict_index_shift),
    ) - 1;

    let mut block_lo =
      (ord_lo as u64 >> Lucene90DocValuesFormat::TERMS_DICT_BLOCK_LZ4_SHIFT) as i64;
    let mut block_hi =
      (ord_hi as u64 >> Lucene90DocValuesFormat::TERMS_DICT_BLOCK_LZ4_SHIFT) as i64;

    while block_lo <= block_hi {
      let block_mid = ((block_lo + block_hi) as u64 >> 1) as i64;
      let term = self.get_first_term_from_block(block_mid as usize)?;
      let cmp = term.as_ref().cmp(text).to_int();
      if cmp <= 0 {
        block_lo = block_mid + 1;
      } else {
        block_hi = block_mid - 1;
      }
    }

    debug_assert!(
      block_hi < 0
        || self
          .get_first_term_from_block(block_hi as usize)?
          .as_ref()
          .cmp(text)
          .to_int()
          <= 0
    );
    debug_assert!(
      block_hi
        == ((self.entry.terms_dict_size - 1) as u64
          >> Lucene90DocValuesFormat::TERMS_DICT_BLOCK_LZ4_SHIFT) as i64
        || self
          .get_first_term_from_block((block_hi + 1) as usize)?
          .as_ref()
          .cmp(text)
          .to_int()
          > 0
    );
    // read the block only if term dict is not empty
    debug_assert!(self.entry.terms_dict_size > 0);
    // reset ord and bytes to the ceiling block even if
    // text is before the first term (blockHi == -1)
    let block = std::cmp::max(block_hi, 0);
    let block_address = self.block_addresses.get_mut(block as usize)?;
    self.ord = block << Lucene90DocValuesFormat::TERMS_DICT_BLOCK_LZ4_SHIFT;
    self.bytes.seek(block_address as usize)?;
    self.decompress_block()?;

    Ok(block_hi)
  }
  fn decompress_block(&mut self) -> Result<()> {
    // The first term is kept uncompressed, so no need to decompress block
    // if only look up the first term when doing seek block.
    self.term.length = self.bytes.read_vint()? as usize;
    self.term.bytes.access_mut(|bytes| {
      self.bytes.read_bytes(bytes, 0, self.term.length)?;
      // Help the compiler infer types.
      Ok::<(), LuceneError>(())
    })?;
    let offset = self.bytes.get_file_pointer()?;
    if offset + 1 < self.entry.terms_data_length {
      // Avoid decompressing again if reading the same block
      if self.current_compressed_block_start != Some(offset) {
        let block_buffer_offset = self.term.length;
        let block_buffer_len = self.bytes.read_vint()?;

        self.block_buffer_offset = block_buffer_offset;
        self.block_buffer_length = block_buffer_len as usize;
        // Decompress the remaining of current block, using the first
        // term as a dictionary
        self.block_input.bytes.access_mut(|buffer_bytes| {
          self.term.bytes.access(|term_bytes| {
            buffer_bytes.copy_from(&term_bytes[..block_buffer_offset], 0);
          })
        });
        self.block_input.bytes.access_mut(|buffer_bytes| {
          LZ4::decompress(
            &mut self.bytes,
            block_buffer_len,
            buffer_bytes,
            block_buffer_offset as i32,
          )?;
          // Help the compiler infer types.
          Ok::<(), LuceneError>(())
        })?;

        self.current_compressed_block_start = Some(offset);
        self.current_compressed_block_end = Some(self.bytes.get_file_pointer()?);
      } else {
        // Seek to block end if already decompressed
        self
          .bytes
          .seek(self.current_compressed_block_end.ok_or_else(|| {
            LuceneError::illegal_argument("current_compressed_block_end not initialized yet")
          })?)?;
      }

      // Reset buffer reader
      self
        .block_input
        .reset_meta(self.block_buffer_offset, self.block_buffer_length);
    }

    Ok(())
  }
}

impl<I> BytesRefIterator for TermsDict<I>
where
  I: IndexInput,
{
  fn next(&mut self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
    self.ord += 1;
    if self.ord >= self.entry.terms_dict_size {
      return Ok(None);
    }

    if (self.ord & self.block_mask as i64) == 0 {
      self.decompress_block()?;
    } else {
      let input = &mut self.block_input;
      let token = input.read_byte()? as i32;
      let mut prefix_length = token & 0x0F;
      let mut suffix_length = 1 + (token as usize >> 4) as i32;

      if prefix_length == 15 {
        prefix_length += input.read_vint()?;
      }
      if suffix_length == 16 {
        suffix_length += input.read_vint()?;
      }

      self.term.length = (prefix_length + suffix_length) as usize;
      self.term.bytes.access_mut(|bytes| {
        input.read_bytes(bytes, prefix_length as usize, suffix_length as usize)?;
        // Help the compiler infer types.
        Ok::<(), LuceneError>(())
      })?;
    }
    Ok(Some(Cow::Borrowed(&self.term)))
  }
}

impl<I> TermsEnum for TermsDict<I>
where
  I: IndexInput,
{
  type AttributeSource<'a>
    = &'a DummyAttributeSource
  where
    Self: 'a;
  type AttributeSourceMut<'a>
    = &'a mut DummyAttributeSource
  where
    Self: 'a;

  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn seek_exact(&mut self, term: &BytesRef<Vec<u8>>) -> Result<bool> {
    Ok(self.seek_ceil(term)? == SeekStatus::Found)
  }

  fn prepare_seek_exact(&mut self, _text: &BytesRef<Vec<u8>>) -> Result<Option<()>> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_prepare_seek_exact_status(&mut self, _target: &BytesRef<Vec<u8>>) -> Result<bool> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn seek_ceil(&mut self, text: &BytesRef<Vec<u8>>) -> Result<SeekStatus> {
    let block = self.seek_block(text)?;
    if block == -2 {
      // empty terms dict
      debug_assert!(self.entry.terms_dict_size == 0);
      return Ok(SeekStatus::End);
    } else if block == -1 {
      // before the first term
      return Ok(SeekStatus::NotFound);
    }

    loop {
      let cmp = self.term.cmp(text).to_int();
      if cmp == 0 {
        return Ok(SeekStatus::Found);
      } else if cmp > 0 {
        return Ok(SeekStatus::NotFound);
      }

      if self.next()?.is_none() {
        return Ok(SeekStatus::End);
      }
    }
  }

  fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
    if ord < 0 || ord >= self.entry.terms_dict_size {
      return Err(LuceneError::illegal_state(format!(
        "Invalid ordinal {} (max {})",
        ord, self.entry.terms_dict_size
      )));
    }

    // Signed shift since ord is -1 when unpositioned
    let current_block_index = self.ord >> Lucene90DocValuesFormat::TERMS_DICT_BLOCK_LZ4_SHIFT;
    let block_index = ord >> Lucene90DocValuesFormat::TERMS_DICT_BLOCK_LZ4_SHIFT;

    if ord < self.ord || block_index != current_block_index {
      // The looked up ord is before the current ord or belongs to a
      // different block, seek again
      let block_address = self.block_addresses.get_mut(block_index as usize)?;
      self.bytes.seek(block_address as usize)?;
      self.ord = (block_index << Lucene90DocValuesFormat::TERMS_DICT_BLOCK_LZ4_SHIFT) - 1;
    }
    // Scan forward to the desired ordinal
    while self.ord < ord {
      self.next()?;
    }
    Ok(())
  }

  fn seek_exact_with_state(
    &mut self,
    term: &BytesRef<Vec<u8>>,
    _state: &TermStateEnum,
  ) -> Result<()> {
    if !self.seek_exact(term)? {
      return Err(LuceneError::illegal_state(format!(
        "term {} does not exist",
        term
      )));
    }
    Ok(())
  }

  fn term(&self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
    Ok(Cow::Borrowed(&self.term))
  }

  fn ord(&self) -> Result<i64> {
    Ok(self.ord)
  }

  fn doc_freq(&mut self) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    Ok(-1)
  }

  type PostingsEnum = DummyPostingsEnum;

  fn postings_with_flags(
    &mut self,
    _reuse: Option<Self::PostingsEnum>,
    _flags: i32,
  ) -> Result<Self::PostingsEnum> {
    Err(LuceneError::unsupported_operation(""))
  }

  type ImpactsEnum = DummyImpactsEnum;

  fn impacts(&mut self, _flags: i32) -> Result<Self::ImpactsEnum> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    Ok(BaseTermsEnumTermStateImpl.into())
  }
}

pub struct DenseSortedNumericDocValues<R> {
  max_doc: i32,
  start: i64,
  end: i64,
  doc: i32,
  count: i32,
  values: LongValuesEnums<R>,
  addresses: DirectMonotonicReader<R>,
}
impl<R> DenseSortedNumericDocValues<R> {
  fn new(max_doc: i32, values: LongValuesEnums<R>, addresses: DirectMonotonicReader<R>) -> Self {
    Self {
      max_doc,
      start: 0,
      end: 0,
      doc: -1,
      count: 0,
      values,
      addresses,
    }
  }
}

impl<R> DocValuesIterator for DenseSortedNumericDocValues<R>
where
  R: RandomAccessInput,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    self.start = self.addresses.get_mut(target as usize)?;
    self.end = self.addresses.get_mut((target as usize) + 1)?;
    self.count = (self.end - self.start) as i32;
    self.doc = target;
    Ok(true)
  }
}

impl<R> DocIdSetIterator for DenseSortedNumericDocValues<R>
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
      return Ok(NO_MORE_DOCS);
    }

    self.start = self.addresses.get_mut(target as usize)?;
    self.end = self.addresses.get_mut((target + 1) as usize)?;
    self.count = (self.end - self.start) as i32;
    self.doc = target;

    Ok(self.doc)
  }

  fn cost(&self) -> Result<i64> {
    Ok(self.max_doc as i64)
  }
}

impl<R> SortedNumericDocValues for DenseSortedNumericDocValues<R>
where
  R: RandomAccessInput,
{
  fn next_value(&mut self) -> Result<i64> {
    let value = self.values.get_mut(self.start as usize)?;
    self.start += 1;
    Ok(value)
  }

  fn doc_value_count(&mut self) -> Result<i32> {
    Ok(self.count)
  }
  type NumericDocValues = DummyNumericDocValues;
}
pub struct SpareSortedNumericDocValues<I>
where
  I: IndexInput,
{
  disi: IndexedDISIImpl<I::IndexInput, I::RandomAccessSlice>,
  values: LongValuesEnums<I::RandomAccessSlice>,
  addresses: DirectMonotonicReader<I::RandomAccessSlice>,
  set: bool,
  start: i64,
  end: i64,
  count: i32,
}
impl<I> SpareSortedNumericDocValues<I>
where
  I: IndexInput,
{
  pub fn new(
    disi: IndexedDISIImpl<I::IndexInput, I::RandomAccessSlice>,
    values: LongValuesEnums<I::RandomAccessSlice>,
    addresses: DirectMonotonicReader<I::RandomAccessSlice>,
  ) -> Self {
    Self {
      disi,
      values,
      addresses,
      set: false,
      start: 0,
      end: 0,
      count: 0,
    }
  }

  fn set(&mut self) -> Result<()> {
    if !self.set {
      let index = self.disi.index_u();
      self.start = self.addresses.get_mut(index)?;
      self.end = self.addresses.get_mut((index) + 1)?;
      self.count = (self.end - self.start) as i32;
      self.set = true;
    }
    Ok(())
  }
}
impl<I> DocIdSetIterator for SpareSortedNumericDocValues<I>
where
  I: IndexInput,
{
  fn doc_id(&self) -> i32 {
    self.disi.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.set = false;
    self.disi.next_doc()
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.set = false;
    self.disi.advance(target)
  }

  fn cost(&self) -> Result<i64> {
    self.disi.cost()
  }
}

impl<I> DocValuesIterator for SpareSortedNumericDocValues<I>
where
  I: IndexInput,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    self.set = false;
    self.disi.advance_exact(target)
  }
}
impl<I> SortedNumericDocValues for SpareSortedNumericDocValues<I>
where
  I: IndexInput,
{
  fn next_value(&mut self) -> Result<i64> {
    self.set()?;
    let value = self.values.get_mut(self.start as usize)?;
    self.start += 1;
    Ok(value)
  }

  fn doc_value_count(&mut self) -> Result<i32> {
    self.set()?;
    Ok(self.count)
  }

  type NumericDocValues = DummyNumericDocValues;
}

// 1. NumericDocValues
pub enum Lucene90NumericDocValuesEnum<I>
where
  I: IndexInput,
{
  A(DenseNumericDocValues<I::RandomAccessSlice>),
  B(SparseNumericDocValues<I>),
  C(EmptyNumeric),
}

impl<I> DocValuesIterator for Lucene90NumericDocValuesEnum<I>
where
  I: IndexInput,
{
  #[inline]
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      Self::A(values) => values.advance_exact(target),
      Self::B(values) => values.advance_exact(target),
      Self::C(values) => values.advance_exact(target),
    }
  }
}

impl<I> DocIdSetIterator for Lucene90NumericDocValuesEnum<I>
where
  I: IndexInput,
{
  #[inline]
  fn doc_id(&self) -> i32 {
    match self {
      Self::A(values) => values.doc_id(),
      Self::B(values) => values.doc_id(),
      Self::C(values) => values.doc_id(),
    }
  }

  #[inline]
  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::A(values) => values.next_doc(),
      Self::B(values) => values.next_doc(),
      Self::C(values) => values.next_doc(),
    }
  }

  #[inline]
  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::A(values) => values.advance(target),
      Self::B(values) => values.advance(target),
      Self::C(values) => values.advance(target),
    }
  }

  #[inline]
  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::A(values) => values.slow_advance(target),
      Self::B(values) => values.slow_advance(target),
      Self::C(values) => values.slow_advance(target),
    }
  }

  #[inline]
  fn cost(&self) -> Result<i64> {
    match self {
      Self::A(values) => values.cost(),
      Self::B(values) => values.cost(),
      Self::C(values) => values.cost(),
    }
  }
}

impl<I> NumericDocValues for Lucene90NumericDocValuesEnum<I>
where
  I: IndexInput,
{
  #[inline]
  fn long_value(&mut self) -> Result<i64> {
    match self {
      Self::A(values) => values.long_value(),
      Self::B(values) => values.long_value(),
      Self::C(values) => values.long_value(),
    }
  }
}

// 2.SortedNumericDocValues
pub enum Lucene90SortedNumericDocValuesEnum<I>
where
  I: IndexInput,
{
  A(DenseSortedNumericDocValues<I::RandomAccessSlice>),
  B(SpareSortedNumericDocValues<I>),
  C(SingletonSortedNumericDocValues<Lucene90NumericDocValuesEnum<I>>),
}

impl<I> DocValuesIterator for Lucene90SortedNumericDocValuesEnum<I>
where
  I: IndexInput,
{
  #[inline]
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      Self::A(values) => values.advance_exact(target),
      Self::B(values) => values.advance_exact(target),
      Self::C(values) => values.advance_exact(target),
    }
  }
}

impl<I> DocIdSetIterator for Lucene90SortedNumericDocValuesEnum<I>
where
  I: IndexInput,
{
  #[inline]
  fn doc_id(&self) -> i32 {
    match self {
      Self::A(values) => values.doc_id(),
      Self::B(values) => values.doc_id(),
      Self::C(values) => values.doc_id(),
    }
  }

  #[inline]
  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::A(values) => values.next_doc(),
      Self::B(values) => values.next_doc(),
      Self::C(values) => values.next_doc(),
    }
  }

  #[inline]
  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::A(values) => values.advance(target),
      Self::B(values) => values.advance(target),
      Self::C(values) => values.advance(target),
    }
  }

  #[inline]
  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::A(values) => values.slow_advance(target),
      Self::B(values) => values.slow_advance(target),
      Self::C(values) => values.slow_advance(target),
    }
  }

  #[inline]
  fn cost(&self) -> Result<i64> {
    match self {
      Self::A(values) => values.cost(),
      Self::B(values) => values.cost(),
      Self::C(values) => values.cost(),
    }
  }
}

impl<I> SortedNumericDocValues for Lucene90SortedNumericDocValuesEnum<I>
where
  I: IndexInput,
{
  #[inline]
  fn next_value(&mut self) -> Result<i64> {
    match self {
      Self::A(values) => values.next_value(),
      Self::B(values) => values.next_value(),
      Self::C(values) => values.next_value(),
    }
  }

  #[inline]
  fn doc_value_count(&mut self) -> Result<i32> {
    match self {
      Self::A(values) => values.doc_value_count(),
      Self::B(values) => values.doc_value_count(),
      Self::C(values) => values.doc_value_count(),
    }
  }

  #[inline]
  fn is_single_valued(&self) -> bool {
    match self {
      Self::A(values) => values.is_single_valued(),
      Self::B(values) => values.is_single_valued(),
      Self::C(values) => values.is_single_valued(),
    }
  }

  type NumericDocValues = Lucene90NumericDocValuesEnum<I>;

  #[inline]
  fn get_numeric_doc_values(&mut self) -> Result<Self::NumericDocValues> {
    match self {
      Self::A(_) | Self::B(_) => Err(LuceneError::unsupported_operation("")),
      Self::C(values) => values.get_numeric_doc_values(),
    }
  }
}

// 3. BinaryDocValues
pub enum Lucene90BinaryDocValuesEnum<I>
where
  I: IndexInput,
{
  Dense(DenseBinaryDocValues<I::RandomAccessSlice>),
  Sparse(SparseBinaryDocValues<I>),
  Empty(EmptyBinary),
}

impl<I> DocValuesIterator for Lucene90BinaryDocValuesEnum<I>
where
  I: IndexInput,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      Self::Dense(values) => values.advance_exact(target),
      Self::Sparse(values) => values.advance_exact(target),
      Self::Empty(values) => values.advance_exact(target),
    }
  }
}

impl<I> DocIdSetIterator for Lucene90BinaryDocValuesEnum<I>
where
  I: IndexInput,
{
  fn doc_id(&self) -> i32 {
    match self {
      Self::Dense(values) => values.doc_id(),
      Self::Sparse(values) => values.doc_id(),
      Self::Empty(values) => values.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::Dense(values) => values.next_doc(),
      Self::Sparse(values) => values.next_doc(),
      Self::Empty(values) => values.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Dense(values) => values.advance(target),
      Self::Sparse(values) => values.advance(target),
      Self::Empty(values) => values.advance(target),
    }
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Dense(values) => values.slow_advance(target),
      Self::Sparse(values) => values.slow_advance(target),
      Self::Empty(values) => values.slow_advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::Dense(values) => values.cost(),
      Self::Sparse(values) => values.cost(),
      Self::Empty(values) => values.cost(),
    }
  }
}

impl<I> BinaryDocValues for Lucene90BinaryDocValuesEnum<I>
where
  I: IndexInput,
{
  fn binary_value(&mut self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
    match self {
      Self::Dense(values) => values.binary_value(),
      Self::Sparse(values) => values.binary_value(),
      Self::Empty(values) => values.binary_value(),
    }
  }
}

// 4. SortedSetDocValues
pub enum Lucene90SortedSetDocValuesEnum<I>
where
  I: IndexInput,
{
  Single(SingletonSortedSetDocValues<BaseSortedDocValues<I>>),
  Multi(BaseSortedSetDocValues<I>),
}

impl<I> DocValuesIterator for Lucene90SortedSetDocValuesEnum<I>
where
  I: IndexInput,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      Self::Single(values) => values.advance_exact(target),
      Self::Multi(values) => values.advance_exact(target),
    }
  }
}

impl<I> DocIdSetIterator for Lucene90SortedSetDocValuesEnum<I>
where
  I: IndexInput,
{
  fn doc_id(&self) -> i32 {
    match self {
      Self::Single(values) => values.doc_id(),
      Self::Multi(values) => values.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::Single(values) => values.next_doc(),
      Self::Multi(values) => values.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Single(values) => values.advance(target),
      Self::Multi(values) => values.advance(target),
    }
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Single(values) => values.slow_advance(target),
      Self::Multi(values) => values.slow_advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::Single(values) => values.cost(),
      Self::Multi(values) => values.cost(),
    }
  }
}

impl<I> SortedSetDocValues for Lucene90SortedSetDocValuesEnum<I>
where
  I: IndexInput,
{
  fn next_ord(&mut self) -> Result<i64> {
    match self {
      Self::Single(values) => values.next_ord(),
      Self::Multi(values) => values.next_ord(),
    }
  }

  fn doc_value_count(&mut self) -> Result<i32> {
    match self {
      Self::Single(values) => values.doc_value_count(),
      Self::Multi(values) => values.doc_value_count(),
    }
  }

  fn lookup_ord(&mut self, ord: i64) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
    match self {
      Self::Single(values) => values.lookup_ord(ord),
      Self::Multi(values) => values.lookup_ord(ord),
    }
  }

  fn get_value_count(&self) -> Result<i64> {
    match self {
      Self::Single(values) => values.get_value_count(),
      Self::Multi(values) => values.get_value_count(),
    }
  }

  fn lookup_term(&mut self, key: &BytesRef<Vec<u8>>) -> Result<i64> {
    match self {
      Self::Single(values) => values.lookup_term(key),
      Self::Multi(values) => values.lookup_term(key),
    }
  }

  type TermsEnum<'a>
    = TermsEnumEnum2<
    <SingletonSortedSetDocValues<BaseSortedDocValues<I>> as SortedSetDocValues>::TermsEnum<'a>,
    <BaseSortedSetDocValues<I> as SortedSetDocValues>::TermsEnum<'a>,
  >
  where
    I: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    match self {
      Self::Single(values) => values.terms_enum().map(TermsEnumEnum2::A),
      Self::Multi(values) => values.terms_enum().map(TermsEnumEnum2::B),
    }
  }

  fn is_single_valued(&self) -> bool {
    match self {
      Self::Single(values) => values.is_single_valued(),
      Self::Multi(values) => values.is_single_valued(),
    }
  }

  type SortedDocValues = BaseSortedDocValues<I>;

  fn get_sorted_doc_values(&mut self) -> Result<Self::SortedDocValues> {
    match self {
      Self::Single(values) => values.get_sorted_doc_values(),
      Self::Multi(_) => Err(LuceneError::unsupported_operation("")),
    }
  }
}
// 5. SortedDocValues
pub type Lucene90SortedDocValuesEnum<I> = BaseSortedDocValues<I>;

// 6. skipper
pub type Lucene90Skipper<I> = DocValuesSkipperImpl<I>;

fn get_direct_reader_instance<R>(
  merging: bool,
  slice: Option<R>,
  bits_per_value: i32,
  offset: usize,
  num_values: usize,
) -> Result<DirectPackedEnum<R>>
where
  R: RandomAccessInput,
{
  if merging {
    Ok(DirectReader::get_merge_instance_with_base_offset(
      slice,
      bits_per_value,
      offset,
      num_values,
    ))
  } else {
    DirectReader::get_instance_with_offset(slice, bits_per_value, offset)
  }
}
