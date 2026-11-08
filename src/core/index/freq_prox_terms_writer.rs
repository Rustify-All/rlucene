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
use crate::core::codecs::block_term_state::TermStateEnum;
use crate::core::codecs::fields_consumer::FieldsConsumer;
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::codecs::postings_format::PostingsFormat;
use crate::core::index::BytesRef;
use crate::core::index::buffered_updates::BufferedUpdates;
use crate::core::index::bytes_ref::{BytesRefValue, BytesRefValueEnum};
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::fields::Fields;
use crate::core::index::freq_prox_fields::FreqProxFields;
use crate::core::index::freq_prox_terms_writer_per_field::FreqProxTermsWriterPerField;
use crate::core::index::frozen_buffered_updates::{TermDocsIterator, TermsProviderImpl1};
use crate::core::index::index_options::IndexOptions;
use crate::core::index::indexing_chain::PerField;
use crate::core::index::postings_enum::PostingsEnum;
use crate::core::index::postings_enum::{FREQS, feature_requested};
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::sorter::DocMap;
use crate::core::index::term_vectors_consumer::TermVectorsConsumer;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::index::terms_hash::TermsHash;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::store::byte_buffers_data_input::ByteBuffersDataInputOwned;
use crate::core::store::directory::Directory;
use crate::core::store::{ByteBuffersDataOutput, DataInput, DataOutput};
use crate::core::util::access::ByteSource;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::automation::compiled_automaton::CompiledAutomaton;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::close::Closeable;
use crate::core::util::collection_util::CollectionUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::int_block_pool::IntBlockPool;
use crate::core::util::lsb_radix_sorter::LSBRadixSorter;
use crate::core::util::packed::PackedInts;
use crate::core::util::{
  ByteBlockPool, IOUtils, SharedCounter, Sorter, TimSorter, TimSorterBase, ToInt,
};
use std::borrow::Cow;
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
  D: Directory + Clone,
{
  pub(crate) fn new(bytes_used: SharedCounter, next_terms_hash: TermVectorsConsumer<D>) -> Self {
    let base = TermsHash::new(bytes_used);

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
    seg_updates: Option<&mut BufferedUpdates>,
  ) -> Result<()> {
    if let Some(seg_updates) = seg_updates {
      let seg_deletes = &mut seg_updates.delete_terms;

      if seg_deletes.size() == 0 {
        return Ok(());
      }

      let mut iterator = TermDocsIterator::new(TermsProviderImpl1::new(&fields), true);
      seg_deletes.for_each_ordered(&mut |field: &str, bytes: &BytesRef<&[u8]>, doc_id: i32| {
        if let Some(postings) = iterator.next_term(field, bytes)? {
          debug_assert!(doc_id < NO_MORE_DOCS);

          loop {
            let doc = postings.next_doc()?;
            if doc >= doc_id {
              break;
            }

            let max_doc = segment_info.max_doc()?;
            let live_docs = state.live_docs.get_or_insert_with(|| {
              Arc::new(FixedBitSet::with_set_prefix(
                max_doc as usize,
                max_doc as usize,
              ))
            });

            let live_docs = Arc::get_mut(live_docs).ok_or_else(|| {
              LuceneError::illegal_state(
                "live docs must not be shared while applying flush deletes",
              )
            })?;
            if live_docs.get_and_clear(doc as usize) {
              state.del_count_on_flush += 1;
            }
          }
        }
        Ok(())
      })?;
    }
    Ok(())
  }

  pub(crate) fn abort(
    &mut self,
    int_pool: &mut IntBlockPool,
    byte_pool: &mut ByteBlockPool,
    next_int_pool: &mut IntBlockPool,
    next_byte_pool: &mut ByteBlockPool,
  ) -> Result<()> {
    let reset_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      int_pool.reset(false, false);
      byte_pool.reset(false, false);
      Ok(())
    }));
    let next_abort_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      self.next_terms_hash.abort(next_int_pool, next_byte_pool)
    }));
    IOUtils::finally_caught_result(reset_result, next_abort_result)
  }

  #[allow(clippy::too_many_arguments)]
  pub(crate) fn flush<N, DM, D1, T>(
    &mut self,
    fields_to_flush: Vec<FreqProxTermsWriterPerField>,
    state: &mut SegmentWriteState<D>,
    sort_map: Option<&DM>,
    norms: Option<&N>,
    codec: &T,
    info: &SegmentInfo<D1>,
    seg_updates: Option<&mut BufferedUpdates>,
    int_pool: IntBlockPool,
    byte_pool: ByteBlockPool,
  ) -> Result<()>
  where
    N: NormsProducer,
    DM: DocMap + Clone,
    T: Codec,
  {
    self.next_terms_hash.flush(state, sort_map, info)?;
    if !state.field_infos.has_postings() {
      return Ok(());
    }
    // Gather all fields that saw any postings:
    let mut all_fields = fields_to_flush;
    for per_field in &mut all_fields {
      per_field.base.sort_terms(&byte_pool)?;
      debug_assert!(per_field.base.index_options != IndexOptions::None);
    }
    // Sort by field name
    CollectionUtil::intro_sort(&mut all_fields)?;
    let fields = FreqProxFields::new(all_fields, int_pool, byte_pool);
    self.apply_deletes(state, &fields, info, seg_updates)?;

    let mut consumer = codec.postings_format().fields_consumer(state, info)?;
    let write_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      let owned_fields = fields;
      let mut field_view = &owned_fields;
      if let Some(doc_map) = sort_map {
        let mut filter_fields =
          FilterFieldsImpl::new(field_view, state.field_infos.as_ref(), doc_map);
        consumer.write(state, info, &mut filter_fields, norms)?;
      } else {
        consumer.write(state, info, &mut field_view, norms)?;
      }
      Ok(())
    }));
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| consumer.close()));
    IOUtils::use_or_suppress_caught_result(write_result, close_result)
  }
  pub(crate) fn finish_document<D1>(
    &mut self,
    doc_id: i32,
    info: &SegmentInfo<D1>,
    per_fields: &mut [PerField],
    int_pool: &mut IntBlockPool,
    next_byte_pool: &mut ByteBlockPool,
    byte_pool: &mut ByteBlockPool,
  ) -> Result<()> {
    self.next_terms_hash.finish_document(
      doc_id,
      info,
      per_fields,
      int_pool,
      next_byte_pool,
      byte_pool,
    )?;
    Ok(())
  }
  pub(crate) fn start_document(&mut self) -> Result<()> {
    self.next_terms_hash.start_document()?;
    Ok(())
  }
  pub(crate) fn add_field(
    &self,
    field_info: Arc<FieldInfo>,
  ) -> Result<FreqProxTermsWriterPerField> {
    let next_per_field = self.next_terms_hash.add_field(field_info.clone())?;
    FreqProxTermsWriterPerField::new(self, field_info, Some(next_per_field))
  }
}

pub(crate) struct FilterFieldsImpl<'a, F, DM> {
  inner: F,
  field_infos: &'a FieldInfos,
  doc_map: &'a DM,
}
impl<'a, F, DM> FilterFieldsImpl<'a, F, DM> {
  pub(crate) fn new(base: F, field_infos: &'a FieldInfos, doc_map: &'a DM) -> Self {
    Self {
      inner: base,
      field_infos,
      doc_map,
    }
  }
}
impl<'dm, F, DM> Fields for FilterFieldsImpl<'dm, F, DM>
where
  F: Fields,
  DM: DocMap,
{
  type FieldIter<'a>
    = F::FieldIter<'a>
  where
    Self: 'a;

  fn iterator(&self) -> Result<Self::FieldIter<'_>> {
    self.inner.iterator()
  }

  type Terms = SortingTerms<F::Terms, &'dm DM>;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    match self.inner.terms(field)? {
      Some(terms) => {
        let Some(index_options) = self.field_infos.field_info_by_name(field)? else {
          return Err(LuceneError::illegal_state(format!(
            "Field '{field}' not found in field infos"
          )));
        };
        Ok(Some(SortingTerms::new(
          terms,
          *index_options.get_index_options(),
          self.doc_map,
        )))
      },
      None => Ok(None),
    }
  }

  fn size(&self) -> Result<i32> {
    self.inner.size()
  }
}

// SortingTerms
pub struct SortingTerms<T, DM> {
  in_: T,
  index_options: IndexOptions,
  doc_map: DM,
}
impl<T, DM> SortingTerms<T, DM> {
  pub(crate) fn new(base: T, index_options: IndexOptions, doc_map: DM) -> Self {
    Self {
      in_: base,
      index_options,
      doc_map,
    }
  }
}
impl<T, DM> Terms for SortingTerms<T, DM>
where
  T: Terms,
  DM: DocMap + Clone,
{
  type TermsEnum = SortingTermsEnum<T::TermsEnum, DM>;

  fn iterator(&self) -> Result<Self::TermsEnum> {
    Ok(SortingTermsEnum::new(
      self.in_.iterator()?,
      self.index_options,
      self.doc_map.clone(),
    ))
  }

  type IntersectIter = SortingTermsEnum<T::IntersectIter, DM>;

  fn intersect(
    &self,
    compiled: &CompiledAutomaton,
    start_term: Option<&BytesRef<Vec<u8>>>,
  ) -> Result<Self::IntersectIter> {
    let v = self.in_.intersect(compiled, start_term)?;
    Ok(SortingTermsEnum::new(
      v,
      self.index_options,
      self.doc_map.clone(),
    ))
  }

  fn size(&self) -> Result<i64> {
    self.in_.size()
  }

  fn get_sum_total_term_freq(&self) -> Result<i64> {
    self.in_.get_sum_total_term_freq()
  }

  fn get_sum_doc_freq(&self) -> Result<i64> {
    self.in_.get_sum_doc_freq()
  }

  fn get_doc_count(&self) -> Result<i32> {
    self.in_.get_doc_count()
  }

  fn has_freqs(&self) -> bool {
    self.in_.has_freqs()
  }

  fn has_offsets(&self) -> bool {
    self.in_.has_offsets()
  }

  fn has_positions(&self) -> bool {
    self.in_.has_positions()
  }

  fn has_payloads(&self) -> bool {
    self.in_.has_payloads()
  }

  fn get_min(&self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
    self.in_.get_min()
  }

  fn get_max(&'_ self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
    self.in_.get_max()
  }

  fn get_stats(&self) -> Result<String> {
    self.in_.get_stats()
  }
}

// SortingTermsEnum
pub struct SortingTermsEnum<T, DM> {
  in_: T,
  index_options: IndexOptions,
  doc_map: DM,
}
impl<T, DM> SortingTermsEnum<T, DM> {
  pub(crate) fn new(in_: T, index_options: IndexOptions, doc_map: DM) -> Self {
    Self {
      in_,
      index_options,
      doc_map,
    }
  }
}

impl<T, DM> BytesRefIterator for SortingTermsEnum<T, DM>
where
  DM: DocMap,
  T: TermsEnum,
{
  type Value<'a>
    = BytesRefValueEnum<'a>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    self
      .in_
      .next()
      .map(|value| value.map(BytesRefValue::into_value))
  }
}

impl<T, DM> TermsEnum for SortingTermsEnum<T, DM>
where
  T: TermsEnum,
  DM: DocMap,
{
  type AttributeSource<'a>
    = T::AttributeSource<'a>
  where
    Self: 'a;
  type AttributeSourceMut<'a>
    = T::AttributeSourceMut<'a>
  where
    Self: 'a;

  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    self.in_.attributes()
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    self.in_.attributes_mut()
  }

  fn seek_exact<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<bool> {
    self.in_.seek_exact(term)
  }

  fn prepare_seek_exact<BS: ByteSource>(&mut self, text: &BytesRef<BS>) -> Result<Option<()>> {
    self.in_.prepare_seek_exact(text)
  }

  fn get_prepare_seek_exact_status<BS: ByteSource>(
    &mut self,
    target: &BytesRef<BS>,
  ) -> Result<bool> {
    self.in_.get_prepare_seek_exact_status(target)
  }

  fn seek_ceil<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<SeekStatus> {
    self.in_.seek_ceil(term)
  }

  fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
    self.in_.seek_exact_with_ord(ord)
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    term: &BytesRef<BS>,
    state: &TermStateEnum,
  ) -> Result<()> {
    self.in_.seek_exact_with_state(term, state)
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    self.in_.term().map(BytesRefValue::into_value)
  }

  fn ord(&self) -> Result<i64> {
    self.in_.ord()
  }

  fn doc_freq(&mut self) -> Result<i32> {
    self.in_.doc_freq()
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    self.in_.total_term_freq()
  }

  type PostingsEnum = SortingPostingsEnumType<T::PostingsEnum>;

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    let feature_freqs = feature_requested(flags, FREQS);

    if self.index_options >= IndexOptions::DocsAndFreqs && feature_freqs {
      let mut wrap_reuse = match reuse {
        Some(SortingPostingsEnumType::A(sorting_enum)) => sorting_enum,
        _ => SortingPostingsEnum::new(),
      };
      let in_reuse = wrap_reuse.postings_enum.take();

      let in_docs_and_positions = self.in_.postings_with_flags(in_reuse, flags)?;
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
        &self.doc_map,
        in_docs_and_positions,
        store_positions,
        store_offsets,
      )?;
      return Ok(SortingPostingsEnumType::A(wrap_reuse));
    }

    let mut wrap_reuse = match reuse {
      Some(SortingPostingsEnumType::B(sorting_enum)) => sorting_enum,
      _ => SortingDocsEnum::new(),
    };
    let in_reuse = wrap_reuse.postings_enum.take();
    let in_docs = self.in_.postings_with_flags(in_reuse, flags)?;
    wrap_reuse.reset(&self.doc_map, in_docs)?;
    Ok(SortingPostingsEnumType::B(wrap_reuse))
  }

  type ImpactsEnum = T::ImpactsEnum;

  fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
    self.in_.impacts(flags)
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    self.in_.term_state()
  }
}

#[allow(clippy::large_enum_variant)] // Keep postings reuse allocation-free.
pub enum SortingPostingsEnumType<P> {
  A(SortingPostingsEnum<P>),
  B(SortingDocsEnum<P>),
}

impl<P> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SortingPostingsEnumType<P>
where
  P: PostingsEnum,
{
}
impl<P> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for SortingPostingsEnumType<P>
where
  P: PostingsEnum,
{
}

impl<P> DocIdSetIterator for SortingPostingsEnumType<P>
where
  P: PostingsEnum,
{
  fn doc_id(&self) -> i32 {
    match self {
      Self::A(postings) => postings.doc_id(),
      Self::B(postings) => postings.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::A(postings) => postings.next_doc(),
      Self::B(postings) => postings.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::A(postings) => postings.advance(target),
      Self::B(postings) => postings.advance(target),
    }
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::A(postings) => postings.slow_advance(target),
      Self::B(postings) => postings.slow_advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::A(postings) => postings.cost(),
      Self::B(postings) => postings.cost(),
    }
  }
}

impl<P> PostingsEnum for SortingPostingsEnumType<P>
where
  P: PostingsEnum,
{
  fn freq(&mut self) -> Result<i32> {
    match self {
      Self::A(postings) => postings.freq(),
      Self::B(postings) => postings.freq(),
    }
  }

  fn next_position(&mut self) -> Result<i32> {
    match self {
      Self::A(postings) => postings.next_position(),
      Self::B(postings) => postings.next_position(),
    }
  }

  fn start_offset(&self) -> Result<i32> {
    match self {
      Self::A(postings) => postings.start_offset(),
      Self::B(postings) => postings.start_offset(),
    }
  }

  fn end_offset(&self) -> Result<i32> {
    match self {
      Self::A(postings) => postings.end_offset(),
      Self::B(postings) => postings.end_offset(),
    }
  }

  fn get_payload(&self) -> Result<Option<BytesRefValueEnum<'_>>> {
    match self {
      Self::A(postings) => postings.get_payload(),
      Self::B(postings) => postings.get_payload(),
    }
  }
}

// SortingDocsEnum
pub struct SortingDocsEnum<P> {
  sorter: LSBRadixSorter,
  postings_enum: Option<P>,
  docs: smallvec::SmallVec<[i32; 8]>,
  doc_it: Option<usize>,
  upto: usize,
}
impl<P> SortingDocsEnum<P> {
  pub(crate) fn new() -> Self {
    Self {
      sorter: LSBRadixSorter::new(),
      postings_enum: None,
      docs: smallvec::SmallVec::new(),
      doc_it: None,
      upto: 0,
    }
  }
  pub(crate) fn reset<T>(&mut self, doc_map: &T, mut postings_enum: P) -> Result<()>
  where
    P: PostingsEnum,
    T: DocMap,
  {
    self.docs.clear();
    loop {
      let doc = postings_enum.next_doc()?;
      if doc == NO_MORE_DOCS {
        break;
      }
      if self.docs.len() == self.docs.capacity() {
        let min_size = self.docs.len() + 1;
        ArrayUtil::oversize(min_size, size_of::<i32>())?;
        self.docs.reserve(1);
      }
      self.docs.push(doc_map.old_to_new(doc)?);
    }

    self.upto = self.docs.len();

    let max_doc = doc_map.size();
    let num_bits = PackedInts::bits_required(std::cmp::max(0, (max_doc - 1) as i64))? as usize;
    // Even though LSBRadixSorter cannot take advantage of partial ordering like
    // TimSorter it is often still faster for nearly-sorted inputs.
    self.sorter.sort(num_bits, &mut self.docs, self.upto)?;
    self.doc_it = None;
    self.postings_enum = Some(postings_enum);
    Ok(())
  }
}

impl<P> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions for SortingDocsEnum<P> where
  P: PostingsEnum
{
}
impl<P> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for SortingDocsEnum<P> where
  P: PostingsEnum
{
}

impl<P> DocIdSetIterator for SortingDocsEnum<P>
where
  P: PostingsEnum,
{
  fn doc_id(&self) -> i32 {
    self.doc_it.map_or(-1, |doc_it| {
      if doc_it == self.docs.len() {
        NO_MORE_DOCS
      } else {
        self.docs[doc_it]
      }
    })
  }

  fn next_doc(&mut self) -> Result<i32> {
    let doc_it = self.doc_it.map_or(0, |doc_it| doc_it + 1);
    self.doc_it = Some(doc_it);
    Ok(self.doc_id())
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

  fn get_payload(&self) -> Result<Option<BytesRefValueEnum<'_>>> {
    Ok(None)
  }
}

pub(crate) struct DocOffsetSorter<'a> {
  docs: &'a mut [i32],
  offsets: &'a mut [usize],
  tmp_docs: &'a mut Vec<i32>,
  tmp_offsets: &'a mut Vec<usize>,
  pivot_index: usize,
}

impl<'a> DocOffsetSorter<'a> {
  pub(crate) fn new(
    docs: &'a mut [i32],
    offsets: &'a mut [usize],
    max_temp_slots: usize,
    tmp_docs: &'a mut Vec<i32>,
    tmp_offsets: &'a mut Vec<usize>,
  ) -> TimSorter<DocOffsetSorter<'a>> {
    let sorter = DocOffsetSorter {
      docs,
      offsets,
      tmp_docs,
      tmp_offsets,
      pivot_index: 0,
    };
    TimSorter::new(max_temp_slots, sorter)
  }
}

impl Sorter for DocOffsetSorter<'_> {
  fn compare(&mut self, i: usize, j: usize) -> Result<i32> {
    Ok(self.docs[i] - self.docs[j])
  }

  fn swap(&mut self, i: usize, j: usize) -> Result<()> {
    self.docs.swap(i, j);
    self.offsets.swap(i, j);
    Ok(())
  }

  fn set_pivot(&mut self, i: usize) -> Result<()> {
    self.pivot_index = i;
    Ok(())
  }

  fn compare_pivot(&mut self, j: usize) -> Result<i32> {
    self.compare(self.pivot_index, j)
  }
}

impl TimSorterBase for DocOffsetSorter<'_> {
  fn copy(&mut self, src: usize, dest: usize) -> Result<()> {
    self.docs[dest] = self.docs[src];
    self.offsets[dest] = self.offsets[src];
    Ok(())
  }

  fn save(&mut self, i: usize, len: usize) -> Result<()> {
    ArrayUtil::grow_capacity(self.tmp_docs, len)?;
    ArrayUtil::grow_capacity(self.tmp_offsets, len)?;
    self.tmp_docs.clear();
    self.tmp_offsets.clear();
    self.tmp_docs.extend_from_slice(&self.docs[i..i + len]);
    self
      .tmp_offsets
      .extend_from_slice(&self.offsets[i..i + len]);
    Ok(())
  }

  fn restore(&mut self, i: usize, j: usize) {
    self.docs[j] = self.tmp_docs[i];
    self.offsets[j] = self.tmp_offsets[i];
  }

  fn compare_saved(&self, i: usize, j: usize) -> Result<i32> {
    Ok(self.tmp_docs[i] - self.docs[j])
  }
}
pub struct SortingPostingsEnum<P> {
  docs: Vec<i32>,
  offsets: Vec<usize>,
  tmp_docs: Vec<i32>,
  tmp_offsets: Vec<usize>,
  upto: usize,

  posting_input: Option<ByteBuffersDataInputOwned>,
  postings_enum: Option<P>,

  store_positions: bool,
  store_offsets: bool,

  doc_it: Option<usize>,
  pos: i32,
  start_offset: i32,
  end_offset: i32,

  payload: BytesRef<Vec<u8>>,
  curr_freq: i32,

  buffer: ByteBuffersDataOutput,
}
impl<P> SortingPostingsEnum<P> {
  pub fn new() -> Self {
    Self {
      docs: Vec::new(),
      offsets: Vec::new(),
      tmp_docs: Vec::new(),
      tmp_offsets: Vec::new(),
      upto: 0,
      posting_input: None,
      postings_enum: None,
      store_positions: false,
      store_offsets: false,
      doc_it: None,
      pos: 0,
      start_offset: 0,
      end_offset: 0,
      payload: BytesRef::new(),
      curr_freq: 0,
      buffer: ByteBuffersDataOutput::new_resettable_instance(),
    }
  }
  pub fn reset<DM>(
    &mut self,
    doc_map: &DM,
    mut postings_enum: P,
    store_positions: bool,
    store_offsets: bool,
  ) -> Result<()>
  where
    DM: DocMap,
    P: PostingsEnum,
  {
    self.store_positions = store_positions;
    self.store_offsets = store_offsets;

    self.doc_it = None;
    self.start_offset = -1;
    self.end_offset = -1;

    self.buffer.reset();

    self.docs.clear();
    self.offsets.clear();
    let mut i = 0;
    loop {
      let doc = postings_enum.next_doc()?;
      if doc == NO_MORE_DOCS {
        break;
      }
      if self.docs.len() == self.docs.capacity() {
        ArrayUtil::grow_capacity(&mut self.docs, i + 1)?;
      }
      if self.offsets.len() == self.offsets.capacity() {
        ArrayUtil::grow_capacity(&mut self.offsets, i + 1)?;
      }

      self.docs.push(doc_map.old_to_new(doc)?);
      self.offsets.push(self.buffer.size());

      self.add_positions(&mut postings_enum)?;
      i += 1;
    }
    self.postings_enum = Some(postings_enum);

    self.upto = i;

    let num_temp_slots = doc_map.size() / 8;
    let mut sorter = DocOffsetSorter::new(
      &mut self.docs,
      &mut self.offsets,
      num_temp_slots as usize,
      &mut self.tmp_docs,
      &mut self.tmp_offsets,
    );
    sorter.sort(0, self.upto)?;

    self.posting_input = Some(self.buffer.get_data_input_owner(true)?);

    Ok(())
  }
  fn add_positions<T>(&mut self, postings: &mut T) -> Result<()>
  where
    T: PostingsEnum,
  {
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
        let token = ((pos - previous_position) << 1) | if payload_opt.is_some() { 1 } else { 0 };
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
          let payload = payload.as_bytes_ref();
          self.buffer.write_vint(payload.length as i32)?;
          self
            .buffer
            .write_bytes_range(payload.bytes, payload.offset, payload.length)?;
        }
      }
    }
    Ok(())
  }
}

impl<P> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SortingPostingsEnum<P>
where
  P: PostingsEnum,
{
}
impl<P> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for SortingPostingsEnum<P> where
  P: PostingsEnum
{
}

impl<P> DocIdSetIterator for SortingPostingsEnum<P>
where
  P: PostingsEnum,
{
  fn doc_id(&self) -> i32 {
    match self.doc_it {
      None => -1,
      Some(doc_it) if doc_it >= self.upto => NO_MORE_DOCS,
      Some(doc_it) => self.docs[doc_it],
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    let doc_it = self.doc_it.map_or(0, |doc_it| doc_it + 1);
    self.doc_it = Some(doc_it);
    if doc_it >= self.upto {
      return Ok(NO_MORE_DOCS);
    }

    let offset = self.offsets[doc_it];
    let Some(posting_input) = self.posting_input.as_mut() else {
      return Err(LuceneError::illegal_state("posting_input not initialized"));
    };
    posting_input.seek(offset)?;

    self.curr_freq = posting_input.read_vint()?;

    self.pos = 0;
    self.end_offset = 0;

    Ok(self.docs[doc_it])
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    // need to support it for checkIndex, but in practice it won't be called, so
    // don't bother to implement efficiently for now.
    self.slow_advance(target)
  }

  fn cost(&self) -> Result<i64> {
    self
      .postings_enum
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("postings enum not initialized"))?
      .cost()
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

    let Some(posting_input) = self.posting_input.as_mut() else {
      return Err(LuceneError::illegal_state("posting_input not initialized"));
    };

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

      if self.payload.length > self.payload.bytes.len() {
        ArrayUtil::grow_no_copy(&mut self.payload.bytes, length, || 0)?;
      }

      posting_input.read_bytes(&mut self.payload.bytes, 0, self.payload.length)?;
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

  fn get_payload(&self) -> Result<Option<BytesRefValueEnum<'_>>> {
    if self.payload.length == 0 {
      Ok(None)
    } else {
      Ok(Some(Cow::Borrowed(&self.payload).into_value()))
    }
  }
}
