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

use crate::core::codecs::indexed_disi::{create_block_slice, create_jump_table};
use crate::core::codecs::lucene90::indexed_disi::IndexedDISI;
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
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::store::directory::Directory;
use crate::core::store::dummy::dummy_index_input::DummyIndexInput;
use crate::core::store::dummy::dummy_random_access_input::DummyRandomAccessInput;
use crate::core::store::random_access_input::RandomAccessInput;
use crate::core::store::{DataInput, IndexInput, ReadAdvice};
use crate::core::util::CoreHelper;
use crate::core::util::bit_util::BitUtil;
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

    disi_inputs: Mutex<HashMap<i32, Arc<Mutex<I::Slice>>>>,
    disi_jump_tables: Mutex<HashMap<i32, Arc<Mutex<I::RandomAccessSlice>>>>,
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
        D2: Directory,
    {
        let max_doc = segment_info.max_doc()?;
        let meta_name = IndexFileNames::segment_file_name(
            &segment_info.name,
            &state.segment_suffix,
            meta_extension,
        );
        let mut version = -1;

        // Read in the entries from the metadata file
        let mut norms = HashMap::new();
        {
            let mut input = state.directory.open_checksum_input(&meta_name)?;

            let mut prior_error = None;

            match CodecUtil::check_index_header(
                &mut input,
                meta_codec,
                Lucene90NormsFormat::VERSION_START,
                Lucene90NormsFormat::VERSION_CURRENT,
                segment_info.get_id(),
                &state.segment_suffix,
            ) {
                Ok(v) => {
                    version = v;
                    norms = Self::read_fields(&mut input, &state.field_infos)?;
                },
                Err(e) => {
                    prior_error = Some(e);
                },
            }
            if let Some(e) = prior_error {
                return Err(CodecUtil::check_footer_with_error(&mut input, e));
            } else {
                CodecUtil::check_footer(&mut input)?;
            }
        }

        let data_name = IndexFileNames::segment_file_name(
            &segment_info.name,
            &state.segment_suffix,
            data_extension,
        );

        // Norms have a forward-only access pattern, so pass ReadAdvice::Normal
        // to perform readahead
        let mut data = state.directory.open_input(
            &data_name,
            &state.context.with_read_advice_self(ReadAdvice::Normal)?,
        )?;

        // Check header again in the data file

        let version2 = CodecUtil::check_index_header(
            &mut data,
            data_codec,
            Lucene90NormsFormat::VERSION_START,
            Lucene90NormsFormat::VERSION_CURRENT,
            segment_info.get_id(),
            &state.segment_suffix,
        )?;

        if version != version2 {
            return Err(LuceneError::corrupt_index(format!(
                "Format versions mismatch: meta={version}, data={version2} (resource={data})"
            )));
        }
        // NOTE: data file is too costly to verify checksum against all the
        // bytes on open, but for now we at least verify proper
        // structure of the checksum footer: which looks
        // for FOOTER_MAGIC + algorithmID. This is cheap and can detect some
        // forms of corruption such as file truncation.
        CodecUtil::retrieve_checksum(&mut data)?;

        Ok(Self {
            norms,
            max_doc,
            data,
            merging: false,
            disi_inputs: HashMap::new().into(),
            disi_jump_tables: HashMap::new().into(),
            data_inputs: HashMap::new().into(),
        })
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
            let num_docs_with_field = meta.read_int()?;
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
    ) -> Result<Arc<Mutex<I::RandomAccessSlice>>> {
        if self.merging
            && let Some(existing) = self.data_inputs.lock().get(&field.number)
        {
            return Ok(Arc::clone(existing));
        }
        let length = entry.num_docs_with_field as i64 * entry.bytes_per_norm as i64;
        let mut slice = self.data.random_access_slice(entry.norms_offset, length)?;
        // Prefetch the first page of data. Following pages are expected to get
        // prefetched through read-ahead.
        if slice.length() > 0 {
            slice.prefetch(0, 1)?;
        }

        let slice_rc = Arc::new(Mutex::new(slice));
        if self.merging {
            self.data_inputs
                .lock()
                .insert(field.number, Arc::clone(&slice_rc));
        }

        Ok(slice_rc)
    }
    fn get_disi_jump_table(
        &self,
        field: &Arc<FieldInfo>,
        entry: &NormsEntry,
    ) -> Result<Arc<Mutex<I::RandomAccessSlice>>> {
        if self.merging
            && let Some(jump_table) = self.disi_jump_tables.lock().get(&field.number)
        {
            return Ok(Arc::clone(jump_table));
        }

        let jump_table = create_jump_table(
            &self.data,
            entry.docs_with_field_offset,
            entry.docs_with_field_length,
            entry.jump_table_entry_count as i32,
        )?;
        debug_assert!(entry.jump_table_entry_count > 0);
        debug_assert!(jump_table.is_some());
        let jump_table_rc = Arc::new(Mutex::new(jump_table.unwrap()));
        if self.merging {
            self.disi_jump_tables
                .lock()
                .insert(field.number, Arc::clone(&jump_table_rc));
        }

        Ok(jump_table_rc)
    }
}
impl<I> Lucene90NormsProducer<I>
where
    I: IndexInput,
{
    fn get_disi_input(
        &self,
        _field: &FieldInfo,
        entry: &NormsEntry,
    ) -> Result<Arc<Mutex<I::Slice>>> {
        // TODO: Due to the generic constraints, following the Java Lucene
        // implementation currently makes it impossible to cache the Slice.
        let input = create_block_slice(
            &self.data,
            "docs",
            entry.docs_with_field_offset,
            entry.docs_with_field_length,
            entry.jump_table_entry_count as i32,
        )?;
        Ok(Arc::new(Mutex::new(input)))

        // if !self.merging {
        //     let input = create_block_slice(
        //         &mut self.data,
        //         "docs",
        //         entry.docs_with_field_offset,
        //         entry.docs_with_field_length,
        //         entry.jump_table_entry_count as i32,
        //     )?;
        //     return Ok(Arc::new(Mutex::new(input)));
        // }
        //
        // if let Some(existing) = self.disi_inputs.get(&field.number) {
        //     return Ok(Arc::clone(existing));
        // }
        //
        // let input = create_block_slice(
        //     &mut self.data,
        //     "docs",
        //     entry.docs_with_field_offset,
        //     entry.docs_with_field_length,
        //     entry.jump_table_entry_count as i32,
        // )?;
        // let input = Arc::new(Mutex::new(input));
        // self.disi_inputs.insert(field.number, input.clone());
        // Wrap so that reads can be interleaved from the same thread if two
        // norms instances are pulled and consumed in parallel. Merging usually
        // doesn't need this feature but CheckIndex might, plus we need merge
        // instances to behave well and not be trappy.
        // let index_input = IndexInputImpl {
        //     inf: Arc::clone(&in_f),
        //     offset: 0,
        // };
        // Ok(Arc::new(Mutex::new(index_input)))
    }
}
impl<I> Display for Lucene90NormsProducer<I>
where
    I: IndexInput,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "Lucene90NormsProducer(fields={})", self.norms.len())
    }
}

pub struct IndexInputImpl<I>
where
    I: IndexInput,
{
    inf: Arc<Mutex<I::Slice>>,
    offset: i64,
}

impl<I> DataInput for IndexInputImpl<I>
where
    I: IndexInput,
{
    fn read_byte(&mut self) -> Result<u8> {
        Err(LuceneError::unsupported_operation("Unused by IndexedDISI"))
    }

    fn read_bytes(&mut self, b: &mut [u8], off: i32, len: i32) -> Result<()> {
        let mut inf = self.inf.lock();
        inf.seek(self.offset)?;
        self.offset += len as i64;
        inf.read_bytes(b, off, len)?;
        Ok(())
    }

    fn read_short(&mut self) -> Result<i16> {
        let mut inf = self.inf.lock();
        inf.seek(self.offset)?;
        self.offset += BitUtil::SHORT_BYTES as i64;
        inf.read_short()
    }

    fn skip_bytes(&mut self, num_bytes: i64) -> Result<()> {
        IndexInput::skip_bytes(self, num_bytes)
    }
}

impl<I> Display for IndexInputImpl<I>
where
    I: IndexInput,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "docs")
    }
}

impl<I> crate::core::util::clone::TryClone for IndexInputImpl<I>
where
    I: IndexInput,
{
    fn try_clone(&self) -> Result<Self>
    where
        Self: Sized,
    {
        todo!()
    }
}

impl<I> IndexInput for IndexInputImpl<I>
where
    I: IndexInput,
{
    fn get_file_pointer(&self) -> i64 {
        self.offset
    }

    fn seek(&mut self, pos: i64) -> Result<()> {
        self.offset = pos;
        Ok(())
    }

    fn length(&self) -> i64 {
        self.inf.lock().length()
    }

    type Slice = DummyIndexInput;

    fn slice(&self, _slice_description: &str, _offset: i64, _length: i64) -> Result<Self::Slice> {
        Err(LuceneError::unsupported_operation("Unused by IndexedDISI"))
    }

    type RandomAccessSlice = DummyRandomAccessInput;

    fn random_access_slice(&self, _offset: i64, _length: i64) -> Result<Self::RandomAccessSlice> {
        Err(LuceneError::unsupported_operation("Unused by IndexedDISI"))
    }

    fn prefetch(&mut self, _pos: i64, _len: i64) -> Result<()> {
        // Not delegating to the wrapped instance on purpose. This is only used
        // for merging.
        Ok(())
    }
}

impl<I> Clone for Lucene90NormsProducer<I>
where
    I: IndexInput,
{
    fn clone(&self) -> Self {
        unreachable!(
            "{} {}",
            std::any::type_name::<Self>(),
            CoreHelper::CLONE_WARRING
        )
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
            return Ok(Lucene90NormNumericDocValuesEnum::<I>::Empty(
                DocValues::empty_numeric(),
            ));
        }

        if entry.docs_with_field_offset == -1 {
            // dense
            if entry.bytes_per_norm == 0 {
                let sub_dense_norms =
                    DenseNormsIteratorBaseEnum::Dense(DenseNormsIteratorBaseImpl {
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
                    let dense_norms_iterator =
                        DenseNormsIterator::new(self.max_doc, sub_dense_norms);
                    Ok(Lucene90NormNumericDocValuesEnum::Dense(
                        dense_norms_iterator,
                    ))
                },
                2 => {
                    let sub_dense_norms =
                        DenseNormsIteratorBaseEnum::Dense2(DenseNormsIteratorBaseImpl2 { slice });
                    let dense_norms_iterator =
                        DenseNormsIterator::new(self.max_doc, sub_dense_norms);
                    Ok(Lucene90NormNumericDocValuesEnum::Dense(
                        dense_norms_iterator,
                    ))
                },
                4 => {
                    let sub_dense_norms =
                        DenseNormsIteratorBaseEnum::Dense3(DenseNormsIteratorBaseImpl4 { slice });
                    let dense_norms_iterator =
                        DenseNormsIterator::new(self.max_doc, sub_dense_norms);
                    Ok(Lucene90NormNumericDocValuesEnum::Dense(
                        dense_norms_iterator,
                    ))
                },
                8 => {
                    let sub_dense_norms =
                        DenseNormsIteratorBaseEnum::Dense4(DenseNormsIteratorBaseImpl8 { slice });
                    let dense_norms_iterator =
                        DenseNormsIterator::new(self.max_doc, sub_dense_norms);
                    Ok(Lucene90NormNumericDocValuesEnum::Dense(
                        dense_norms_iterator,
                    ))
                },
                _ => Err(LuceneError::unreachable("invalid bytes_per_norm")),
            };
        }
        // sparse
        let disi_input = self.get_disi_input(field, &entry)?;
        let disi_jump_table = self.get_disi_jump_table(field, &entry)?;
        let disi = IndexedDISI::from_components(
            disi_input,
            Some(disi_jump_table),
            entry.jump_table_entry_count as i32,
            entry.dense_rank_power,
            entry.num_docs_with_field as i64,
        )?;

        if entry.bytes_per_norm == 0 {
            let sub_sparse_norms =
                SparseNormsIteratorBaseEnum::Sparse(SparseNormsIteratorBaseImpl {
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
        let _ = CodecUtil::checksum_entire_file(&self.data);
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
    pub num_docs_with_field: i32,
    pub norms_offset: i64,
}
pub struct DenseNormsIterator<I>
where
    I: IndexInput,
{
    max_doc: i32,
    doc: i32,
    sub_dense_norms: DenseNormsIteratorBaseEnum<I>,
}
impl<I> DenseNormsIterator<I>
where
    I: IndexInput,
{
    fn new(max_doc: i32, sub_dense_norms: DenseNormsIteratorBaseEnum<I>) -> Self {
        Self {
            max_doc,
            doc: -1,
            sub_dense_norms,
        }
    }
}

impl<I> DocValuesIterator for DenseNormsIterator<I>
where
    I: IndexInput,
{
    fn advance_exact(&mut self, target: i32) -> Result<bool> {
        self.doc = target;
        Ok(true)
    }
}

impl<I> DocIdSetIterator for DenseNormsIterator<I>
where
    I: IndexInput,
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

impl<I> NumericDocValues for DenseNormsIterator<I>
where
    I: IndexInput,
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
struct DenseNormsIteratorBaseImpl1<I>
where
    I: IndexInput,
{
    slice: Arc<Mutex<I::RandomAccessSlice>>,
}
impl<I> DenseNormsIteratorBase for DenseNormsIteratorBaseImpl1<I>
where
    I: IndexInput,
{
    fn long_value(&mut self, doc: i32) -> Result<i64> {
        Ok(self.slice.lock().read_byte(doc as i64)? as i64)
    }
}
// case 2
struct DenseNormsIteratorBaseImpl2<I>
where
    I: IndexInput,
{
    slice: Arc<Mutex<I::RandomAccessSlice>>,
}
impl<I> DenseNormsIteratorBase for DenseNormsIteratorBaseImpl2<I>
where
    I: IndexInput,
{
    fn long_value(&mut self, doc: i32) -> Result<i64> {
        Ok(self.slice.lock().read_short((doc as i64) << 1)? as i64)
    }
}
// case 4
struct DenseNormsIteratorBaseImpl4<I>
where
    I: IndexInput,
{
    slice: Arc<Mutex<I::RandomAccessSlice>>,
}
impl<I> DenseNormsIteratorBase for DenseNormsIteratorBaseImpl4<I>
where
    I: IndexInput,
{
    fn long_value(&mut self, doc: i32) -> Result<i64> {
        Ok(self.slice.lock().read_int((doc as i64) << 2)? as i64)
    }
}
// case 8
struct DenseNormsIteratorBaseImpl8<I>
where
    I: IndexInput,
{
    slice: Arc<Mutex<I::RandomAccessSlice>>,
}
impl<I> DenseNormsIteratorBase for DenseNormsIteratorBaseImpl8<I>
where
    I: IndexInput,
{
    fn long_value(&mut self, doc: i32) -> Result<i64> {
        self.slice.lock().read_long((doc as i64) << 3)
    }
}
enum DenseNormsIteratorBaseEnum<I>
where
    I: IndexInput,
{
    Dense(DenseNormsIteratorBaseImpl),
    Dense1(DenseNormsIteratorBaseImpl1<I>),
    Dense2(DenseNormsIteratorBaseImpl2<I>),
    Dense3(DenseNormsIteratorBaseImpl4<I>),
    Dense4(DenseNormsIteratorBaseImpl8<I>),
}
impl<I> DenseNormsIteratorBase for DenseNormsIteratorBaseEnum<I>
where
    I: IndexInput,
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
    sub_sparse_norms: SparseNormsIteratorBaseEnum<I>,
    disi: IndexedDISI<I>,
}
impl<I> SparseNormsIterator<I>
where
    I: IndexInput,
{
    fn new(sub_sparse_norms: SparseNormsIteratorBaseEnum<I>, disi: IndexedDISI<I>) -> Self {
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
        self.sub_sparse_norms.long_value(&mut self.disi)
    }
}

trait SparseNormsIteratorBase<I>
where
    I: IndexInput,
{
    fn long_value(&mut self, disi: &mut IndexedDISI<I>) -> Result<i64>;
}
struct SparseNormsIteratorBaseImpl {
    norms_offset: i64,
}
impl SparseNormsIteratorBaseImpl {
    fn new(norms_offset: i64) -> Self {
        Self { norms_offset }
    }
}
impl<I> SparseNormsIteratorBase<I> for SparseNormsIteratorBaseImpl
where
    I: IndexInput,
{
    fn long_value(&mut self, _disi: &mut IndexedDISI<I>) -> Result<i64> {
        Ok(self.norms_offset)
    }
}
// case 1
struct SparseNormsIteratorBaseImpl1<I>
where
    I: IndexInput,
{
    slice: Arc<Mutex<I::RandomAccessSlice>>,
}
impl<I> SparseNormsIteratorBase<I> for SparseNormsIteratorBaseImpl1<I>
where
    I: IndexInput,
{
    fn long_value(&mut self, disi: &mut IndexedDISI<I>) -> Result<i64> {
        Ok(self.slice.lock().read_byte(disi.index() as i64)? as i64)
    }
}
// case 2
struct SparseNormsIteratorBaseImpl2<I>
where
    I: IndexInput,
{
    slice: Arc<Mutex<I::RandomAccessSlice>>,
}
impl<I> SparseNormsIteratorBase<I> for SparseNormsIteratorBaseImpl2<I>
where
    I: IndexInput,
{
    fn long_value(&mut self, disi: &mut IndexedDISI<I>) -> Result<i64> {
        Ok(self.slice.lock().read_short((disi.index() as i64) << 1)? as i64)
    }
}
// case 4
struct SparseNormsIteratorBaseImpl4<I>
where
    I: IndexInput,
{
    slice: Arc<Mutex<I::RandomAccessSlice>>,
}
impl<I> SparseNormsIteratorBase<I> for SparseNormsIteratorBaseImpl4<I>
where
    I: IndexInput,
{
    fn long_value(&mut self, disi: &mut IndexedDISI<I>) -> Result<i64> {
        Ok(self.slice.lock().read_int((disi.index() as i64) << 2)? as i64)
    }
}
// case 8
struct SparseNormsIteratorBaseImpl8<I>
where
    I: IndexInput,
{
    slice: Arc<Mutex<I::RandomAccessSlice>>,
}
impl<I> SparseNormsIteratorBase<I> for SparseNormsIteratorBaseImpl8<I>
where
    I: IndexInput,
{
    fn long_value(&mut self, disi: &mut IndexedDISI<I>) -> Result<i64> {
        self.slice.lock().read_long((disi.index() as i64) << 3)
    }
}
enum SparseNormsIteratorBaseEnum<I>
where
    I: IndexInput,
{
    Sparse(SparseNormsIteratorBaseImpl),
    Sparse1(SparseNormsIteratorBaseImpl1<I>),
    Sparse2(SparseNormsIteratorBaseImpl2<I>),
    Sparse3(SparseNormsIteratorBaseImpl4<I>),
    Sparse4(SparseNormsIteratorBaseImpl8<I>),
}
impl<I> SparseNormsIteratorBase<I> for SparseNormsIteratorBaseEnum<I>
where
    I: IndexInput,
{
    fn long_value(&mut self, disi: &mut IndexedDISI<I>) -> Result<i64> {
        match self {
            SparseNormsIteratorBaseEnum::Sparse(inner) => inner.long_value(disi),
            SparseNormsIteratorBaseEnum::Sparse1(inner) => inner.long_value(disi),
            SparseNormsIteratorBaseEnum::Sparse2(inner) => inner.long_value(disi),
            SparseNormsIteratorBaseEnum::Sparse3(inner) => inner.long_value(disi),
            SparseNormsIteratorBaseEnum::Sparse4(inner) => inner.long_value(disi),
        }
    }
}
