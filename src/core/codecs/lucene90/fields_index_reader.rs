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
use crate::core::codecs::lucene90::fields_index::FieldsIndex;
use crate::core::codecs::lucene90::fields_index_writer::fields_index_writer_const;
use crate::core::index::IndexFileNames;
use crate::core::store::directory::Directory;
use crate::core::store::{IOContext, IndexInput, ReadAdvice};
use crate::core::util::StringHelper;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::long_values::LongValues;
use crate::core::util::packed::direct_monotonic_reader::direct_monotonic::Meta;
use crate::core::util::packed::direct_monotonic_reader::{DirectMonotonicReader, load_meta};
use parking_lot::Mutex;
use std::sync::Arc;

pub(crate) struct FieldsIndexReader<I>
where
    I: IndexInput,
{
    max_doc: i32,
    block_shift: i32,
    num_chunks: i32,
    docs_meta: Meta,
    start_pointers_meta: Meta,
    index_input: I,
    docs_start_pointer: i64,
    docs_end_pointer: i64,
    start_pointers_start_pointer: i64,
    start_pointers_end_pointer: i64,
    docs: DirectMonotonicReader<I::RandomAccessSlice>,
    start_pointers: DirectMonotonicReader<I::RandomAccessSlice>,
    max_pointer: i64,
}
impl<I> FieldsIndexReader<I>
where
    I: IndexInput,
{
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new<D>(
        dir: &D,
        name: String,
        suffix: &str,
        extension: &str,
        codec_name: &str,
        id: &[u8; StringHelper::ID_LENGTH],
        meta_in: &mut impl IndexInput,
        context: &IOContext,
    ) -> Result<Self>
    where
        D: Directory<IndexInput = I>,
    {
        let max_doc = meta_in.read_int()?;
        let block_shift = meta_in.read_int()?;
        let num_chunks = meta_in.read_int()?;
        let docs_start_pointer = meta_in.read_long()?;
        let docs_meta = load_meta(meta_in, num_chunks as i64, block_shift)?;
        let (docs_end_pointer, start_pointers_start_pointer) = {
            let v = meta_in.read_long()?;
            (v, v)
        };
        let start_pointers_meta = load_meta(meta_in, num_chunks as i64, block_shift)?;
        let start_pointers_end_pointer = meta_in.read_long()?;
        let max_pointer = meta_in.read_long()?;

        let mut index_input = dir.open_input(
            &IndexFileNames::segment_file_name(&name, suffix, extension),
            &context.with_read_advice_self(ReadAdvice::RandomPreload)?,
        )?;

        CodecUtil::check_index_header(
            &mut index_input,
            &format!("{codec_name}Idx"),
            fields_index_writer_const::VERSION_START,
            fields_index_writer_const::VERSION_CURRENT,
            id,
            suffix,
        )?;
        CodecUtil::retrieve_checksum(&mut index_input)?;

        let docs_slice = index_input
            .random_access_slice(docs_start_pointer, docs_end_pointer - docs_start_pointer)?;
        let start_pointers_slice = index_input.random_access_slice(
            start_pointers_start_pointer,
            start_pointers_end_pointer - start_pointers_start_pointer,
        )?;
        let docs =
            DirectMonotonicReader::get_instance(&docs_meta, Arc::new(Mutex::new(docs_slice)))?;
        let start_pointers = DirectMonotonicReader::get_instance(
            &start_pointers_meta,
            Arc::new(Mutex::new(start_pointers_slice)),
        )?;

        Ok(FieldsIndexReader {
            max_doc,
            block_shift,
            num_chunks,
            docs_meta,
            start_pointers_meta,
            index_input,
            docs_start_pointer,
            docs_end_pointer,
            start_pointers_start_pointer,
            start_pointers_end_pointer,
            max_pointer,
            docs,
            start_pointers,
        })
    }
    fn with_other(other: &FieldsIndexReader<I>) -> Result<Self> {
        let docs_meta = other.docs_meta.clone();
        let start_pointers_meta = other.start_pointers_meta.clone();
        let docs_slice = Arc::new(Mutex::new(other.index_input.random_access_slice(
            other.docs_start_pointer,
            other.docs_end_pointer - other.docs_start_pointer,
        )?));
        let start_pointers_slice = Arc::new(Mutex::new(other.index_input.random_access_slice(
            other.start_pointers_start_pointer,
            other.start_pointers_end_pointer - other.start_pointers_start_pointer,
        )?));
        let docs = DirectMonotonicReader::get_instance(&docs_meta, docs_slice)?;
        let start_pointers =
            DirectMonotonicReader::get_instance(&start_pointers_meta, start_pointers_slice)?;
        Ok(FieldsIndexReader {
            max_doc: other.max_doc,
            block_shift: other.block_shift,
            num_chunks: other.num_chunks,
            docs_meta,
            start_pointers_meta,
            index_input: other.index_input.try_clone()?,
            docs_start_pointer: other.docs_start_pointer,
            docs_end_pointer: other.docs_end_pointer,
            start_pointers_start_pointer: other.start_pointers_start_pointer,
            start_pointers_end_pointer: other.start_pointers_end_pointer,
            max_pointer: other.max_pointer,
            docs,
            start_pointers,
        })
    }
    pub(crate) fn get_max_pointer(&self) -> i64 {
        self.max_pointer
    }
}

impl<I> crate::core::util::clone::TryClone for FieldsIndexReader<I>
where
    I: IndexInput,
{
    fn try_clone(&self) -> Result<Self>
    where
        Self: Sized,
    {
        FieldsIndexReader::with_other(self)
    }
}

impl<I> FieldsIndex for FieldsIndexReader<I>
where
    I: IndexInput,
{
    fn get_block_id(&self, doc_id: i32) -> Result<i64> {
        assert!(doc_id >= 0 && doc_id < self.max_doc);
        let block_index = self
            .docs
            .binary_search(0, self.num_chunks as i64, doc_id as i64)?;
        let block_index = if block_index < 0 {
            -(2 + block_index)
        } else {
            block_index
        };
        Ok(block_index)
    }

    fn get_block_start_pointer(&self, block_id: i64) -> Result<i64> {
        self.start_pointers.get(block_id)
    }

    fn get_block_length(&self, block_id: i64) -> Result<i64> {
        let end_pointer = if block_id == (self.num_chunks - 1) as i64 {
            self.max_pointer
        } else {
            self.start_pointers.get(block_id + 1)?
        };
        Ok(end_pointer - self.get_block_start_pointer(block_id)?)
    }

    fn check_integrity(&self) -> Result<()> {
        CodecUtil::checksum_entire_file(&self.index_input)?;
        Ok(())
    }
}
