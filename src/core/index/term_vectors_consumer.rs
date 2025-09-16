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
use crate::core::codecs::Codec;
use crate::core::codecs::term_vectors_format::TermVectorsFormat;
use crate::core::codecs::term_vectors_writer::{TermVectorsWriter, TermVectorsWriterEnum};
use crate::core::index::BytesRef;
use crate::core::index::byte_slice_reader::ByteSliceReader;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::indexing_chain::PerField;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::sorter::DocMap;
use crate::core::index::sorting_term_vectors_consumer::SortingTermVectorsConsumer;
use crate::core::index::term_vectors_consumer_per_field::TermVectorsConsumerPerField;
use crate::core::index::terms_hash::TermsHash;
use crate::core::store::IOContext;
use crate::core::store::directory::Directory;
#[cfg(test)]
use crate::core::store::dummy::dummy_directory::DummyDirectory;
use crate::core::store::flush_info::FlushInfo;
use crate::core::util::allocator_byte::AllocatorByteEnum;
#[cfg(test)]
use crate::core::util::allocator_byte::DirectAllocatorByte;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::int_block_pool::AllocatorIntEnum;
#[cfg(test)]
use crate::core::util::int_block_pool::DirectAllocatorI32;
use crate::core::util::{Counter, CounterEnum, CounterEnumLock};
use parking_lot::Mutex;
use std::cmp::Ordering;
use std::sync::Arc;

pub(crate) struct TermVectorsConsumer<D>
where
    D: Directory,
{
    directory: Arc<D>,
    pub(crate) writer: Option<TermVectorsWriterEnum<D>>,
    // Scratch term used by TermVectorsConsumerPerField.finishDocument.
    pub(crate) flush_term: BytesRef<Vec<u8>>,
    // Used by TermVectorsConsumerPerField when serializing the term vectors.
    pub(crate) vector_slice_reader_pos: ByteSliceReader,
    pub(crate) vector_slice_reader_off: ByteSliceReader,
    has_vectors: bool,
    num_vector_fields: i32,
    pub(crate) last_doc_id: i32,
    per_fields_idxs: Vec<PerFieldMeta>,
    sub: Option<SortingTermVectorsConsumer<D>>,
    pub(crate) base: TermsHash,
}

/// Parameter `idx` is the index of the [`PerField`] where the [`TermVectorsConsumerPerField`] resides.
/// [`PerField`] itself is located in the [`IndexingChain`](crate::core::index::indexing_chain::IndexingChain)'s `doc_fields` array.
///
/// Parameter `field_name` is the field name.
#[derive(Clone, Default)]
pub(crate) struct PerFieldMeta {
    pub(crate) idx: i32,
    pub(crate) field_name: String,
}

impl Eq for PerFieldMeta {}

impl PartialEq<Self> for PerFieldMeta {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl PartialOrd<Self> for PerFieldMeta {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PerFieldMeta {
    fn cmp(&self, other: &Self) -> Ordering {
        self.field_name.cmp(&other.field_name)
    }
}

#[cfg(test)]
impl Default for TermVectorsConsumer<DummyDirectory> {
    fn default() -> Self {
        let int_block_allocator = AllocatorIntEnum::DA(DirectAllocatorI32::new());
        let byte_block_allocator = AllocatorByteEnum::DA(DirectAllocatorByte::new());
        let directory = Arc::new(DummyDirectory);
        TermVectorsConsumer::new(int_block_allocator, byte_block_allocator, directory, None)
    }
}

impl<D> TermVectorsConsumer<D>
where
    D: Directory,
{
    pub(crate) fn new(
        int_block_allocator: AllocatorIntEnum<CounterEnumLock>,
        byte_block_allocator: AllocatorByteEnum<CounterEnumLock>,
        directory: Arc<D>,
        sub: Option<SortingTermVectorsConsumer<D>>,
    ) -> Self {
        let base = TermsHash::new(
            int_block_allocator,
            byte_block_allocator,
            Arc::new(Mutex::new(CounterEnum::new_counter(false))),
        );

        let per_fields = vec![PerFieldMeta::default(); 1];

        TermVectorsConsumer {
            directory,
            writer: None,
            flush_term: BytesRef::default(),
            vector_slice_reader_pos: ByteSliceReader::new(),
            vector_slice_reader_off: ByteSliceReader::new(),
            has_vectors: false,
            num_vector_fields: 0,
            last_doc_id: 0,
            per_fields_idxs: per_fields,
            base,
            sub,
        }
    }
    fn reset_fields(&mut self) {
        self.per_fields_idxs.clear();
        self.num_vector_fields = 0;
    }
    fn fill(&mut self, doc_id: i32) -> Result<()> {
        while self.last_doc_id < doc_id {
            if let Some(ref mut w) = self.writer {
                w.start_document(0)?;
                w.finish_document()?;
            } else {
                Err(LuceneError::illegal_state(
                    "TermVectorsConsumer writer is not initialized",
                ))?;
            }
            self.last_doc_id += 1;
        }
        Ok(())
    }

    pub(crate) fn set_has_vectors(&mut self) {
        self.has_vectors = true;
    }
    pub(crate) fn finish_document<D1>(
        &mut self,
        doc_id: i32,
        codec: &impl Codec,
        info: &SegmentInfo<D1>,
        per_fields: &mut [Option<PerField>],
    ) -> Result<()>
    where
        D1: Directory,
    {
        if !self.has_vectors {
            return Ok(());
        }

        ArrayUtil::intro_sort_with_range(&mut self.per_fields_idxs, 0, self.num_vector_fields)?;

        self.init_term_vectors_writer(codec, info)?;
        self.fill(doc_id)?;
        // Append term vectors to the real outputs:
        match self.sub {
            Some(ref mut sub) => {
                if sub.writer.is_none() {
                    sub.writer
                        .as_mut()
                        .unwrap()
                        .start_document(self.num_vector_fields)?;
                }
            },
            None => {
                self.writer
                    .as_mut()
                    .unwrap()
                    .start_document(self.num_vector_fields)?;
            },
        }
        let idxs = std::mem::take(&mut self.per_fields_idxs);
        for per_field_idx in idxs.into_iter().take(self.num_vector_fields as usize) {
            let v = per_fields[per_field_idx.idx as usize].as_mut().unwrap();
            v.terms_hash_per_field
                .as_mut()
                .unwrap()
                .next_per_field
                .as_mut()
                .unwrap()
                .finish_document(self)?;
        }

        match self.sub {
            Some(ref mut sub) => {
                sub.writer.as_mut().unwrap().finish_document()?;
            },
            None => {
                self.writer.as_mut().unwrap().finish_document()?;
            },
        }
        debug_assert_eq!(
            self.last_doc_id, doc_id,
            "last_doc_id = {}, doc_id = {}",
            self.last_doc_id, doc_id
        );

        self.last_doc_id += 1;
        self.reset_fields();
        Ok(())
    }
    pub(crate) fn start_document(&mut self) -> Result<()> {
        self.reset_fields();
        self.num_vector_fields = 0;
        Ok(())
    }
    pub(crate) fn add_field(&mut self, field_info: Arc<FieldInfo>) -> TermVectorsConsumerPerField {
        TermVectorsConsumerPerField::new(self, field_info)
    }
    pub(crate) fn add_field_to_flush(&mut self, meta: PerFieldMeta) {
        let num_vector_fields = self.num_vector_fields as usize;
        if num_vector_fields == self.per_fields_idxs.len() {
            let new_size = ArrayUtil::oversize(num_vector_fields + 1, 0);
            ArrayUtil::grow_with_len(&mut self.per_fields_idxs, new_size);
        }

        self.per_fields_idxs[num_vector_fields] = meta;
        self.num_vector_fields += 1;
    }
    pub(crate) fn flush<DM, D1>(
        &mut self,
        state: &SegmentWriteState<D>,
        sort_map: &Option<Arc<DM>>,
        codec: &impl Codec,
        info: &SegmentInfo<D1>,
    ) -> Result<()>
    where
        DM: DocMap,
        D1: Directory,
    {
        if self.writer.is_some()
            || (self.sub.is_some() && self.sub.as_ref().unwrap().writer.is_some())
        {
            let num_docs = info.max_doc()?;
            debug_assert!(num_docs > 0);
            // At least one doc in this run had term vectors enabled
            self.fill(num_docs)?;
            match self.sub {
                Some(ref mut sub) => {
                    sub.writer
                        .as_mut()
                        .unwrap()
                        .finish(num_docs, state.directory)?;
                    let _ = sub.writer.take();
                },
                None => {
                    self.writer
                        .as_mut()
                        .unwrap()
                        .finish(num_docs, state.directory)?;
                    let _ = self.writer.take();
                },
            }

            if let Some(ref mut sub) = self.sub {
                sub.flush(state, sort_map, codec, info)?;
            }
        }

        Ok(())
    }

    fn init_term_vectors_writer<D1>(
        &mut self,
        codec: &impl Codec,
        info: &SegmentInfo<D1>,
    ) -> Result<()>
    where
        D1: Directory,
    {
        match self.sub {
            Some(ref mut sub) => {
                if sub.writer.is_none() {
                    sub.init_term_vectors_writer(
                        self.last_doc_id,
                        info,
                        self.base.bytes_used.lock().get(),
                    )?;
                }
            },
            None => {
                if self.writer.is_none() {
                    let flush_info =
                        FlushInfo::new(self.last_doc_id, self.base.bytes_used.lock().get());
                    let context = IOContext::with_flush(flush_info)?;

                    self.writer = Option::from(codec.term_vectors_format().vectors_writer(
                        self.directory.as_ref(),
                        info,
                        &context,
                    )?)
                }
            },
        }

        self.last_doc_id = 0;
        Ok(())
    }

    pub(crate) fn abort(&mut self) -> Result<()> {
        self.base.reset();
        if let Some(ref mut sub) = self.sub {
            sub.abort()?;
        }
        Ok(())
    }
}

pub(crate) trait TermVectorsConsumerBase {
    type Directory: Directory;
    fn flush<DM, D1>(
        &mut self,
        state: &SegmentWriteState<Self::Directory>,
        sort_map: &Option<Arc<DM>>,
        codec: &impl Codec,
        info: &SegmentInfo<D1>,
    ) -> Result<()>
    where
        DM: DocMap,
        D1: Directory;
    fn init_term_vectors_writer<D1>(
        &mut self,
        last_doc_id: i32,
        info: &SegmentInfo<D1>,
        bytes_used: i64,
    ) -> Result<()>
    where
        D1: Directory;
    fn abort(&mut self) -> Result<()>;
}
