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
use crate::core::codecs::fields_consumer::FieldsConsumer;
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::codecs::postings_format::PostingsFormat;
use crate::core::codecs::{Codec, get_default_code};
use crate::core::index::BytesRef;
use crate::core::index::automaton_terms_enum::AutomatonTermsEnum;
use crate::core::index::buffered_updates::MTBufferedUpdates;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::fields::Fields;
use crate::core::index::filter_leaf_reader::{FilterFields, FilterTerms, FilterTermsEnum};
use crate::core::index::filtered_terms_enum::FilteredTermsEnum;
use crate::core::index::freq_prox_fields::FreqProxFields;
use crate::core::index::freq_prox_terms_writer_per_field::FreqProxTermsWriterPerField;
use crate::core::index::frozen_buffered_updates::{TermDocsIterator, TermsProviderImpl1};
use crate::core::index::index_options::IndexOptions;
use crate::core::index::indexing_chain::PerField;
use crate::core::index::postings_enum::PostingsEnum;
use crate::core::index::postings_enum::{Either2PostingsEnum, FREQS, feature_requested};
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::sorter::DocMap;
use crate::core::index::term::Term;
use crate::core::index::term_vectors_consumer::TermVectorsConsumer;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::index::terms_hash::TermsHash;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::store::byte_buffers_data_input::ByteBuffersDataInputOwned;
use crate::core::store::directory::Directory;
use crate::core::store::{ByteBuffersDataOutput, DataInput, DataOutput};
use crate::core::util::allocator_byte::AllocatorByteEnum;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::automation::compiled_automaton::CompiledAutomaton;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::collection_util::CollectionUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::int_block_pool::AllocatorIntEnum;
use crate::core::util::lsb_radix_sorter::LSBRadixSorter;
use crate::core::util::packed::PackedInts;
use crate::core::util::{CounterEnumLock, SliceCopyOps, Sorter, TimSorter, TimSorterBase, ToInt};
use std::borrow::Cow;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

pub(crate) struct FreqProxTermsWriter<D>
where
    D: Directory,
{
    pub(crate) next_terms_hash: TermVectorsConsumer<D>,
    pub(crate) base: TermsHash,
}
impl<D> FreqProxTermsWriter<D>
where
    D: Directory,
{
    pub(crate) fn new(
        int_block_allocator: AllocatorIntEnum<CounterEnumLock>,
        byte_block_allocator: AllocatorByteEnum<CounterEnumLock>,
        bytes_used: CounterEnumLock,
        mut next_terms_hash: TermVectorsConsumer<D>,
    ) -> Self {
        let mut base = TermsHash::new(int_block_allocator, byte_block_allocator, bytes_used);
        base.term_byte_pool = Some(base.byte_pool.clone());
        next_terms_hash.base.term_byte_pool = Some(base.byte_pool.clone());

        Self {
            next_terms_hash,
            base,
        }
    }
    fn apply_deletes<D1>(
        &self,
        state: &mut SegmentWriteState<D>,
        fields: &FreqProxFields,
        segment_info: &SegmentInfo<D1>,
        seg_updates: Option<&mut MTBufferedUpdates>,
    ) -> Result<()>
    where
        D1: Directory,
    {
        if let Some(seg_updates) = seg_updates {
            let seg_deletes = &mut seg_updates.delete_terms;

            if seg_deletes.size() == 0 {
                return Ok(());
            }

            let mut iterator = TermDocsIterator::new(TermsProviderImpl1::new(fields), true);

            seg_deletes.for_each_ordered(&mut |term: &Term, doc_id: i32| {
                if let Some(postings) = iterator.next_term(term.field(), &term.bytes)? {
                    debug_assert!(doc_id < NO_MORE_DOCS);

                    while let Ok(doc) = postings.next_doc() {
                        if doc >= doc_id {
                            break;
                        }

                        let max_doc = segment_info.max_doc()?;
                        let live_docs = state.live_docs.get_or_insert_with(|| {
                            let mut bits = FixedBitSet::new(max_doc);
                            bits.set_with_range(0, max_doc);
                            bits
                        });

                        if live_docs.get_and_clear(doc) {
                            state.del_count_on_flush += 1;
                        }
                    }
                }
                Ok(())
            })?;
        }
        Ok(())
    }

    pub(crate) fn abort(&mut self) -> Result<()> {
        self.base.reset();
        self.next_terms_hash.abort()
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn flush<N, DM, D1>(
        &mut self,
        fields_to_flush: HashMap<String, FreqProxTermsWriterPerField>,
        state: &mut SegmentWriteState<D>,
        sort_map: Option<Arc<DM>>,
        norms: Option<N>,
        codec: &impl Codec,
        info: &SegmentInfo<D1>,
        seg_updates: Option<&mut MTBufferedUpdates>,
    ) -> Result<()>
    where
        N: NormsProducer,
        DM: DocMap,
        D1: Directory,
    {
        self.next_terms_hash.flush(state, &sort_map, codec, info)?;
        if !state.field_infos.has_postings() {
            return Ok(());
        }
        // Gather all fields that saw any postings:
        let mut all_fields = Vec::new();
        for mut per_field in fields_to_flush.into_values() {
            if per_field.base.get_num_terms() > 0 {
                per_field.base.sort_terms()?;
                debug_assert!(per_field.base.index_options != IndexOptions::None);
                all_fields.push(Rc::new(per_field));
            }
        }
        // Sort by field name
        CollectionUtil::intro_sort(&mut all_fields)?;
        let mut fields = FreqProxFields::new(all_fields);
        self.apply_deletes(state, &fields, info, seg_updates)?;

        let mut consumer = get_default_code()
            .postings_format()
            .fields_consumer(state, info)?;
        if let Some(doc_map) = &sort_map {
            let mut filter_fields = FilterFieldsImpl::new(
                FilterFields::new(fields),
                state.field_infos.clone(),
                doc_map.clone(),
            );
            consumer.write(&mut filter_fields, &norms)?;
        } else {
            consumer.write(&mut fields, &norms)?;
        }
        Ok(())
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
        self.next_terms_hash
            .finish_document(doc_id, codec, info, per_fields)?;
        Ok(())
    }
    pub(crate) fn start_document(&mut self) -> Result<()> {
        self.next_terms_hash.start_document()?;
        Ok(())
    }
    pub(crate) fn add_field(&mut self, field_info: Arc<FieldInfo>) -> FreqProxTermsWriterPerField {
        let next_per_field = self.next_terms_hash.add_field(field_info.clone());
        FreqProxTermsWriterPerField::new(self, field_info, Some(next_per_field))
    }
}

pub(crate) struct FilterFieldsImpl<F, D>
where
    F: Fields,
    D: DocMap,
{
    base: FilterFields<F>,
    field_infos: Arc<FieldInfos>,
    doc_map: Arc<D>,
}
impl<F, D> FilterFieldsImpl<F, D>
where
    F: Fields,
    D: DocMap,
{
    pub(crate) fn new(
        base: FilterFields<F>,
        field_infos: Arc<FieldInfos>,
        doc_map: Arc<D>,
    ) -> Self {
        Self {
            base,
            field_infos,
            doc_map,
        }
    }
}
impl<F, D> Fields for FilterFieldsImpl<F, D>
where
    F: Fields,
    D: DocMap,
{
    type FieldIter<'a>
        = F::FieldIter<'a>
    where
        F: 'a,
        D: 'a;

    fn iterator(&self) -> Self::FieldIter<'_> {
        self.base.iterator()
    }

    type Terms = SortingTerms<F::Terms, D>;

    fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
        match self.base.terms(field)? {
            Some(terms) => {
                let index_options = self.field_infos.field_info_by_name(field);
                if index_options.is_none() {
                    return Err(LuceneError::illegal_state(format!(
                        "Field '{field}' not found in field infos"
                    )));
                }
                let base = FilterTerms::new(terms);
                Ok(Some(SortingTerms::new(
                    base,
                    *index_options.as_ref().unwrap().get_index_options(),
                    self.doc_map.clone(),
                )))
            },
            None => Ok(None),
        }
    }

    fn size(&self) -> Result<i32> {
        self.base.size()
    }
}

// SortingTerms
pub(crate) struct SortingTerms<T, D>
where
    T: Terms,
    D: DocMap,
{
    base: FilterTerms<T>,
    index_options: IndexOptions,
    doc_map: Arc<D>,
}
impl<T, D> SortingTerms<T, D>
where
    T: Terms,
    D: DocMap,
{
    pub(crate) fn new(base: FilterTerms<T>, index_options: IndexOptions, doc_map: Arc<D>) -> Self {
        Self {
            base,
            index_options,
            doc_map,
        }
    }
}
impl<T, D> Terms for SortingTerms<T, D>
where
    T: Terms,
    D: DocMap,
{
    type TermsEnum = SortingTermsEnum<T::TermsEnum, D>;

    fn iterator(&self) -> Result<Self::TermsEnum> {
        let base = FilterTermsEnum::new(self.base.iterator()?);
        Ok(SortingTermsEnum::new(
            base,
            self.index_options,
            self.doc_map.clone(),
        ))
    }

    type IntersectIter = SortingTermsEnum<FilteredTermsEnum<T::TermsEnum, AutomatonTermsEnum>, D>;

    fn intersect(
        &self,
        compiled: &mut CompiledAutomaton,
        start_term: Option<BytesRef<Vec<u8>>>,
    ) -> Result<Self::IntersectIter> {
        let base = FilterTermsEnum::new(self.base.intersect(compiled, start_term)?);
        Ok(SortingTermsEnum::new(
            base,
            self.index_options,
            self.doc_map.clone(),
        ))
    }

    fn size(&self) -> Result<i64> {
        self.base.size()
    }

    fn get_sum_total_term_freq(&self) -> Result<i64> {
        self.base.get_sum_total_term_freq()
    }

    fn get_sum_doc_freq(&self) -> Result<i64> {
        self.base.get_sum_doc_freq()
    }

    fn get_doc_count(&self) -> Result<i32> {
        self.base.get_doc_count()
    }

    fn has_freqs(&self) -> bool {
        self.base.has_freqs()
    }

    fn has_offsets(&self) -> bool {
        self.base.has_offsets()
    }

    fn has_positions(&self) -> bool {
        self.base.has_positions()
    }

    fn has_payloads(&self) -> bool {
        self.base.has_payloads()
    }

    fn get_min<'a, T1>(&'a self, iterator: &'a mut T1) -> Result<Option<Cow<'a, BytesRef<Vec<u8>>>>>
    where
        T1: TermsEnum,
    {
        self.base.get_min(iterator)
    }

    fn get_max<'a, T1>(&'a self, iterator: &'a mut T1) -> Result<Option<Cow<'a, BytesRef<Vec<u8>>>>>
    where
        T1: TermsEnum,
    {
        self.base.get_max(iterator)
    }

    fn get_stats(&self) -> Result<String> {
        self.base.get_stats()
    }
}

// SortingTermsEnum
pub(crate) struct SortingTermsEnum<T, D>
where
    T: TermsEnum,
    D: DocMap,
{
    base: FilterTermsEnum<T>,
    index_options: IndexOptions,
    doc_map: Arc<D>,
}
impl<T, D> SortingTermsEnum<T, D>
where
    T: TermsEnum,
    D: DocMap,
{
    pub(crate) fn new(
        base: FilterTermsEnum<T>,
        index_options: IndexOptions,
        doc_map: Arc<D>,
    ) -> Self {
        Self {
            base,
            index_options,
            doc_map,
        }
    }
}

impl<T, D> BytesRefIterator for SortingTermsEnum<T, D>
where
    D: DocMap,
    T: TermsEnum,
{
}

impl<T, D> TermsEnum for SortingTermsEnum<T, D>
where
    T: TermsEnum,
    D: DocMap,
{
    type AttributeSource = <FilterTermsEnum<T> as TermsEnum>::AttributeSource;

    fn attributes(&self) -> Result<Self::AttributeSource> {
        self.base.attributes()
    }

    fn seek_exact(&mut self, term: &BytesRef<Vec<u8>>) -> Result<bool> {
        self.base.seek_exact(term)
    }

    fn prepare_seek_exact(&mut self, text: &BytesRef<Vec<u8>>) -> Result<Option<()>> {
        self.base.prepare_seek_exact(text)
    }

    fn seek_ceil(&mut self, term: &BytesRef<Vec<u8>>) -> Result<SeekStatus> {
        self.base.seek_ceil(term)
    }

    fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
        self.base.seek_exact_with_ord(ord)
    }

    fn seek_exact_with_state(
        &mut self,
        term: &BytesRef<Vec<u8>>,
        state: &Self::TermState,
    ) -> Result<()> {
        self.base.seek_exact_with_state(term, state)
    }

    fn term(&self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
        self.base.term()
    }

    fn ord(&self) -> Result<i64> {
        self.base.ord()
    }

    fn doc_freq(&mut self) -> Result<i32> {
        self.base.doc_freq()
    }

    fn total_term_freq(&mut self) -> Result<i64> {
        self.base.total_term_freq()
    }

    type PostingsEnum =
        Either2PostingsEnum<SortingPostingsEnum<T::PostingsEnum>, SortingDocsEnum<T::PostingsEnum>>;

    fn postings_with_flags(
        &mut self,
        reuse: Option<Self::PostingsEnum>,
        flags: i32,
    ) -> Result<Self::PostingsEnum> {
        let feature_freqs = feature_requested(flags, FREQS);

        if self.index_options >= IndexOptions::DocsAndFreqs && feature_freqs {
            let mut wrap_reuse = match reuse {
                Some(Either2PostingsEnum::A(sorting_enum)) => sorting_enum,
                _ => SortingPostingsEnum::new(),
            };
            let in_reuse = wrap_reuse.postings_enum.take();

            let in_docs_and_positions = self.base.postings_with_flags(in_reuse, flags)?;
            // we ignore the fact that positions/offsets may be stored but not asked for,
            // since this code is expected to be used during addIndexes which will
            // ask for everything. if that assumption changes in the future, we can
            // factor in whether 'flags' says offsets are not required.
            let store_positions = self
                .index_options
                .cmp(&IndexOptions::DocsAndFreqsAndPositions)
                .to_int()
                >= 0;
            let store_offsets = self
                .index_options
                .cmp(&IndexOptions::DocsAndFreqsAndPositionsAndOffsets)
                .to_int()
                >= 0;

            wrap_reuse.reset(
                self.doc_map.as_ref(),
                in_docs_and_positions,
                store_positions,
                store_offsets,
            )?;
            return Ok(Either2PostingsEnum::A(wrap_reuse));
        }

        let mut wrap_reuse = match reuse {
            Some(Either2PostingsEnum::B(sorting_enum)) => sorting_enum,
            _ => SortingDocsEnum::new(),
        };
        let in_reuse = wrap_reuse.postings_enum.take();
        let in_docs = self.base.postings_with_flags(in_reuse, flags)?;
        wrap_reuse.reset(self.doc_map.as_ref(), in_docs)?;
        Ok(Either2PostingsEnum::B(wrap_reuse))
    }

    type ImpactsEnum = T::ImpactsEnum;

    fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
        self.base.impacts(flags)
    }

    type TermState = T::TermState;

    fn term_state(&mut self) -> Result<Self::TermState> {
        self.base.term_state()
    }
}
// SortingDocsEnum
pub(crate) struct SortingDocsEnum<P>
where
    P: PostingsEnum,
{
    sorter: LSBRadixSorter,
    postings_enum: Option<P>,
    docs: Vec<i32>,
    doc_it: i32,
    upto: i32,
}
impl<P> SortingDocsEnum<P>
where
    P: PostingsEnum,
{
    pub(crate) fn new() -> Self {
        Self {
            sorter: LSBRadixSorter::new(),
            postings_enum: None,
            docs: Vec::new(),
            doc_it: -1,
            upto: 0,
        }
    }
    pub(crate) fn reset(&mut self, doc_map: &impl DocMap, mut postings_enum: P) -> Result<()> {
        let mut i = 0;
        loop {
            let doc = postings_enum.next_doc()?;
            if doc == NO_MORE_DOCS {
                break;
            }
            if self.docs.len() <= i {
                ArrayUtil::grow(&mut self.docs)?;
            }
            self.docs[i] = doc_map.old_to_new(doc);
            i += 1;
        }

        self.upto = i as i32;
        if self.docs.len() == self.upto as usize {
            ArrayUtil::grow(&mut self.docs)?;
        }
        self.docs[self.upto as usize] = NO_MORE_DOCS;

        let max_doc = doc_map.size();
        let num_bits = PackedInts::bits_required(std::cmp::max(0, (max_doc - 1) as i64))? as usize;
        // Even though LSBRadixSorter cannot take advantage of partial ordering like
        // TimSorter it is often still faster for nearly-sorted inputs.
        self.sorter
            .sort(num_bits, &mut self.docs, self.upto as usize);
        self.doc_it = -1;
        self.postings_enum = Some(postings_enum);
        Ok(())
    }
}

impl<P> DocIdSetIterator for SortingDocsEnum<P>
where
    P: PostingsEnum,
{
    fn doc_id(&self) -> i32 {
        if self.doc_it < 0 {
            -1
        } else {
            self.docs[self.doc_it as usize]
        }
    }

    fn next_doc(&mut self) -> Result<i32> {
        self.doc_it += 1;
        Ok(self.docs[self.doc_it as usize])
    }

    fn advance(&mut self, target: i32) -> Result<i32> {
        self.slow_advance(target)
    }

    fn cost(&self) -> Result<i64> {
        Ok(self.upto as i64)
    }
}

impl<P> PostingsEnum for SortingDocsEnum<P>
where
    P: PostingsEnum,
{
    fn freq(&mut self) -> Result<i32> {
        Ok(1)
    }

    fn next_position(&mut self) -> Result<i32> {
        Ok(-1)
    }

    fn start_offset(&self) -> Result<i32> {
        Ok(-1)
    }

    fn end_offset(&self) -> Result<i32> {
        Ok(-1)
    }

    fn get_payload(&self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
        Ok(None)
    }
}

struct DocOffsetSorter<'a> {
    docs: &'a mut [i32],
    offsets: &'a mut [i64],
    tmp_docs: Vec<i32>,
    tmp_offsets: Vec<i64>,
    pivot_index: i32,
}

impl<'a> DocOffsetSorter<'a> {
    pub fn new(
        docs: &'a mut [i32],
        offsets: &'a mut [i64],
        max_temp_slots: usize,
    ) -> TimSorter<DocOffsetSorter<'a>> {
        let tmp_docs = Vec::new();
        let tmp_offsets = Vec::new();
        let sorter = DocOffsetSorter {
            docs,
            offsets,
            tmp_docs,
            tmp_offsets,
            pivot_index: 0,
        };
        TimSorter::new(max_temp_slots as i32, sorter)
    }
}

impl Sorter for DocOffsetSorter<'_> {
    fn compare(&mut self, i: i32, j: i32) -> Result<i32> {
        Ok(self.docs[i as usize] - self.docs[j as usize])
    }

    fn swap(&mut self, i: i32, j: i32) -> Result<()> {
        let i = i as usize;
        let j = j as usize;
        self.docs.swap(i, j);
        self.offsets.swap(i, j);
        Ok(())
    }

    fn set_pivot(&mut self, i: i32) -> Result<()> {
        self.pivot_index = i;
        Ok(())
    }

    fn compare_pivot(&mut self, j: i32) -> Result<i32> {
        self.compare(self.pivot_index, j)
    }
}

impl TimSorterBase for DocOffsetSorter<'_> {
    fn copy(&mut self, src: i32, dest: i32) {
        let src = src as usize;
        let dest = dest as usize;
        self.docs[dest] = self.docs[src];
        self.offsets[dest] = self.offsets[src];
    }

    fn save(&mut self, i: i32, len: i32) {
        if self.tmp_docs.len() < len as usize {
            let new_len = ArrayUtil::oversize(len as usize, 0);
            self.tmp_docs = vec![0; new_len];
            self.tmp_offsets = vec![0; new_len];
        }
        let i = i as usize;
        let len = len as usize;

        self.tmp_docs.copy_from(&self.docs[i..i + len], 0);
        self.tmp_offsets.copy_from(&self.offsets[i..i + len], 0);
    }

    fn restore(&mut self, i: i32, j: i32) {
        let i = i as usize;
        let j = j as usize;
        self.docs[j] = self.tmp_docs[i];
        self.offsets[j] = self.tmp_offsets[i];
    }

    fn compare_saved(&self, i: i32, j: i32) -> i32 {
        self.tmp_docs[i as usize] - self.docs[j as usize]
    }
}
pub(crate) struct SortingPostingsEnum<P>
where
    P: PostingsEnum,
{
    docs: Vec<i32>,
    offsets: Vec<i64>,
    upto: i32,

    posting_input: Option<ByteBuffersDataInputOwned>,
    postings_enum: Option<P>,

    store_positions: bool,
    store_offsets: bool,

    doc_it: i32,
    pos: i32,
    start_offset: i32,
    end_offset: i32,

    payload: BytesRef<Vec<u8>>,
    curr_freq: i32,

    buffer: ByteBuffersDataOutput,
}
impl<P> SortingPostingsEnum<P>
where
    P: PostingsEnum,
{
    pub fn new() -> Self {
        Self {
            docs: Vec::new(),
            offsets: Vec::new(),
            upto: 0,
            posting_input: None,
            postings_enum: None,
            store_positions: false,
            store_offsets: false,
            doc_it: -1,
            pos: 0,
            start_offset: 0,
            end_offset: 0,
            payload: BytesRef::new(),
            curr_freq: 0,
            buffer: ByteBuffersDataOutput::new_resettable_instance(),
        }
    }
    pub fn reset<D>(
        &mut self,
        doc_map: &D,
        mut postings_enum: P,
        store_positions: bool,
        store_offsets: bool,
    ) -> Result<()>
    where
        D: DocMap,
    {
        self.store_positions = store_positions;
        self.store_offsets = store_offsets;

        self.doc_it = -1;
        self.start_offset = -1;
        self.end_offset = -1;

        self.buffer.reset();

        let mut i = 0;
        loop {
            let doc = postings_enum.next_doc()?;
            if doc == NO_MORE_DOCS {
                break;
            }
            if i == self.docs.len() {
                let new_length = ArrayUtil::oversize(i + 1, 4);
                ArrayUtil::grow_exact(&mut self.docs, new_length)?;
                ArrayUtil::grow_exact(&mut self.offsets, new_length)?;
            }

            self.docs[i] = doc_map.old_to_new(doc);
            self.offsets[i] = self.buffer.size();

            self.add_positions(&mut postings_enum)?;
            i += 1;
        }
        self.postings_enum = Some(postings_enum);

        debug_assert!(i <= i32::MAX as usize);
        self.upto = i as i32;

        let num_temp_slots = doc_map.size() / 8;
        let mut sorter =
            DocOffsetSorter::new(&mut self.docs, &mut self.offsets, num_temp_slots as usize);
        sorter.sort(0, self.upto)?;

        self.posting_input = Some(self.buffer.get_data_input_owner());

        Ok(())
    }
    fn add_positions(&mut self, postings: &mut impl PostingsEnum) -> Result<()> {
        let freq = postings.freq()?;
        self.buffer.write_vint(freq)?;

        if self.store_positions {
            let mut previous_position = 0;
            let mut previous_end_offset = 0;

            for _ in 0..freq {
                let pos = postings.next_position()?;
                let payload_opt = postings.get_payload()?;
                // The low-order bit of token is set only if there is a payload, the
                // previous bits are the delta-encoded position.
                let token =
                    ((pos - previous_position) << 1) | if payload_opt.is_some() { 1 } else { 0 };
                self.buffer.write_vint(token)?;
                previous_position = pos;

                if self.store_offsets {
                    // don't encode offsets if they are not stored
                    let start_offset = postings.start_offset()?;
                    let end_offset = postings.end_offset()?;
                    self.buffer.write_vint(start_offset - previous_end_offset)?;
                    self.buffer.write_vint(end_offset - start_offset)?;
                    previous_end_offset = end_offset;
                }

                if let Some(payload) = payload_opt {
                    self.buffer.write_vint(payload.length as i32)?;
                    self.buffer.write_bytes_range(
                        &payload.bytes,
                        payload.offset as i32,
                        payload.length as i32,
                    )?;
                }
            }
        }
        Ok(())
    }
}

impl<P> DocIdSetIterator for SortingPostingsEnum<P>
where
    P: PostingsEnum,
{
    fn doc_id(&self) -> i32 {
        if self.doc_it < 0 {
            -1
        } else if self.doc_it >= self.upto {
            NO_MORE_DOCS
        } else {
            self.docs[self.doc_it as usize]
        }
    }

    fn next_doc(&mut self) -> Result<i32> {
        self.doc_it += 1;
        if self.doc_it >= self.upto {
            return Ok(NO_MORE_DOCS);
        }

        let offset = self.offsets[self.doc_it as usize];
        let posting_input = self.posting_input.as_mut().unwrap();
        posting_input.seek(offset)?;

        posting_input.read_vint()?;

        self.pos = 0;
        self.end_offset = 0;

        Ok(self.docs[self.doc_it as usize])
    }

    fn advance(&mut self, target: i32) -> Result<i32> {
        // need to support it for checkIndex, but in practice it won't be called, so
        // don't bother to implement efficiently for now.
        self.slow_advance(target)
    }

    fn cost(&self) -> Result<i64> {
        self.postings_enum.as_ref().unwrap().cost()
    }
}

impl<P> PostingsEnum for SortingPostingsEnum<P>
where
    P: PostingsEnum,
{
    fn freq(&mut self) -> Result<i32> {
        Ok(self.curr_freq)
    }

    fn next_position(&mut self) -> Result<i32> {
        if !self.store_positions {
            return Ok(-1);
        }

        let posting_input = self.posting_input.as_mut().unwrap();

        let token = posting_input.read_vint()?;
        self.pos += ((token as u32) >> 1) as i32;

        if self.store_offsets {
            self.start_offset = self.end_offset + posting_input.read_vint()?;
            self.end_offset = self.start_offset + posting_input.read_vint()?;
        }

        if (token & 1) != 0 {
            self.payload.offset = 0;
            let length = posting_input.read_vint()? as usize;
            self.payload.length = length;

            if self.payload.bytes.len() < length {
                let new_length = ArrayUtil::oversize(length, 1);
                self.payload.bytes = vec![0; new_length];
            }

            posting_input.read_bytes(&mut self.payload.bytes, 0, self.payload.length as i32)?;
        } else {
            self.payload.length = 0;
        }

        Ok(self.pos)
    }

    fn start_offset(&self) -> Result<i32> {
        Ok(self.start_offset)
    }

    fn end_offset(&self) -> Result<i32> {
        Ok(self.end_offset)
    }

    fn get_payload(&self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
        if self.payload.length == 0 {
            Ok(None)
        } else {
            Ok(Some(Cow::Borrowed(&self.payload)))
        }
    }
}
#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use rand::Rng;
    use rand::prelude::SliceRandom;

    use crate::core::index::freq_prox_terms_writer::DocOffsetSorter;
    use crate::core::util::Sorter;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{is_night_mode, random};
    use crate::test::util::test_util::TestUtil;

    fn generate_doc_offset_data<R: Rng + ?Sized>(
        random: &mut R,
        len: usize,
    ) -> (Vec<i32>, Vec<i64>) {
        let mut docs = Vec::with_capacity(len);
        let mut offsets = Vec::with_capacity(len);

        let mut doc_id = 0;
        for _ in 0..len {
            doc_id += random.random_range(1..10);
            docs.push(doc_id);
            offsets.push(random.random_range(1000..10_000));
        }
        docs.shuffle(random);

        (docs, offsets)
    }

    fn assert_sorted_and_synced(docs: &[i32], offsets: &[i64], original_map: &HashMap<i32, i64>) {
        assert_eq!(docs.len(), offsets.len());

        for i in 0..docs.len() {
            if i > 0 {
                assert!(
                    docs[i - 1] <= docs[i],
                    "docs not sorted at index {}: {} > {}",
                    i,
                    docs[i - 1],
                    docs[i]
                );
            }

            let doc = docs[i];
            let expected_offset = original_map.get(&doc).expect("missing doc in map");

            assert_eq!(
                offsets[i], *expected_offset,
                "offset mismatch at index {}: doc={} expected={} actual={}",
                i, doc, expected_offset, offsets[i]
            );
        }
    }

    #[test]
    fn test_doc_offset_sorter_basic() {
        let mut random = random();
        let len = if is_night_mode() {
            random.random_range(1000..5000)
        } else {
            random.random_range(10000..20000)
        };

        let (mut docs, mut offsets) = generate_doc_offset_data(&mut random, len);
        assert_eq!(docs.len(), offsets.len());

        let mut original_map: HashMap<i32, i64> = HashMap::with_capacity(len);
        for (doc, offset) in docs.iter().cloned().zip(offsets.iter().cloned()) {
            original_map.insert(doc, offset);
        }

        let max_temp_slots = TestUtil::next_int(&mut random, 0, len as i32);
        let mut sorter = DocOffsetSorter::new(&mut docs, &mut offsets, max_temp_slots as usize);
        sorter.sort(0, len as i32).unwrap();

        assert_sorted_and_synced(&docs, &offsets, &original_map);
    }
}
