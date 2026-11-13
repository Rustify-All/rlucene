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
use crate::core::analysis::analyzer::Analyzer;
use crate::core::analysis::token_stream::{AnalyzerTokenStreams, TokenStream};
use crate::core::codecs::Codec;
use crate::core::codecs::doc_values_format::DocValuesFormat;
use crate::core::codecs::field_infos_format::FieldInfosFormat;
use crate::core::codecs::norms_format::NormsFormat;
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::codecs::points_format::PointsFormat;
use crate::core::codecs::points_writer::PointsWriter;
use crate::core::document::fields::{FieldTokenStreamEnum, Fields, dispatch_fields};
use crate::core::document::invertable_field::InvertableType;
use crate::core::index::BinaryValueEnum;
use crate::core::index::BytesRef;
use crate::core::index::binary_doc_values_writer::{
  BinaryDocValuesWriter, BufferedBinaryDocValues,
};
use crate::core::index::buffered_updates::BufferedUpdates;
use crate::core::index::doc_values::{
  DocValues, EmptyBinary, EmptyNumeric, EmptySorted, EmptySortedSet,
};
use crate::core::index::doc_values_leaf_reader::DocValuesLeafReader;
use crate::core::index::doc_values_skip_index_type::DocValuesSkipIndexType;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::doc_values_writer::{
  DocValuesWriter, DocValuesWriterDISI, DocValuesWriterEnum,
};
use crate::core::index::docs_with_field_set::DocsWithFieldSetDISI;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::Builder;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::field_invert_state::FieldInvertState;
use crate::core::index::freq_prox_terms_writer::FreqProxTermsWriter;
use crate::core::index::freq_prox_terms_writer_per_field::FreqProxTermsWriterPerField;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_reader::{IndexReader, IndexReaderBase, LeafReaderContextKind};
use crate::core::index::index_sorter::DocComparatorEnum2;
use crate::core::index::index_sorter::{DocComparator, IndexSorter};
use crate::core::store::IO_CONTEXT_DEFAULT;
use std::borrow::{Borrow, BorrowMut, Cow};
use std::num::NonZeroU32;

use crate::core::analysis::reader::ReaderEnum;
use crate::core::codecs::knn_vectors_format::KnnVectorsFormat;
use crate::core::codecs::knn_vectors_writer::KnnVectorsWriter;
use crate::core::document::field::{FieldDataEnum, IndexingTokenStreamEnum3};
use crate::core::index::BytesRefValue;
use crate::core::index::index_writer::{
  IndexingDocument, IndexingFields, MAX_POSITION, MAX_STORED_STRING_LENGTH, MAX_TERM_LENGTH,
};
use crate::core::index::indexable_field::{
  IndexableField, IndexingTokenStream, ReusedIndexingTokenStream,
};
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::index::leaf_metadata::LeafMetaData;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::norm_values_writer::NormValuesWriter;
use crate::core::index::numeric_doc_values_writer::{
  BufferedNumericDocValues, NumericDocValuesWriter,
};
use crate::core::index::point_values_writer::PointValuesWriter;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::singleton_sorted_numeric_doc_values::SingletonSortedNumericDocValues;
use crate::core::index::sorted_doc_values_writer::{
  BufferedSortedDocValues, SortedDocValuesWriter,
};
use crate::core::index::sorted_numeric_doc_values_writer::{
  SortedNumericDocValuesWriter, SortedNumericDocValuesWriterValues,
};
use crate::core::index::sorted_set_doc_values_writer::{
  SortedSetDocValuesWriter, SortedSetDocValuesWriterDocIdSetIterator,
};
use crate::core::index::sorter::{DocMap, DocMapImpl, Sorter};
use crate::core::index::sorting_stored_fields_consumer::SortingStoredFieldsConsumer;
use crate::core::index::sorting_term_vectors_consumer::SortingTermVectorsConsumer;
use crate::core::index::stored_fields_consumer::{StoredFieldsConsumer, StoredFieldsConsumerHook};
use crate::core::index::term::Term;
use crate::core::index::term_vectors_consumer::{TermVectorsConsumer, TermVectorsConsumerHook};
use crate::core::index::term_vectors_consumer_per_field::TermVectorOptions;
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::index::vector_values_consumer::VectorValuesConsumer;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::sort::Sort;
use crate::core::search::sort_field::SortFiledBase;

use crate::core::store::directory::Directory;
use crate::core::util::accountable::Accountable;
use crate::core::util::allocator_byte::DirectTrackingAllocatorByte;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::attribute_source::{AttributeSource, Attributes, EmptyAttributeSource};
use crate::core::util::bit_set::of;
use crate::core::util::bit_set::{BitSet, SparseFixedBitSetBitSet};
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bits::Bits;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{
  CaughtResult, CaughtResultExt, LuceneError, LuceneErrorKind, Result,
};
use crate::core::util::info_stream::{InfoStream, InfoStreamEnum, InfoStreamMT};
use crate::core::util::int_block_pool::{
  AllocatorI32, AllocatorIntEnum, INT_BLOCK_SIZE, IntBlockPool,
};
use crate::core::util::number::Number;
use crate::core::util::paged_bytes::PagedBytesDataInput;
use crate::core::util::{
  AtomicCounter, ByteBlockPool, CoreHelper, Counter, IOUtils, LUCENE_10_0_0, SharedCounter,
  TryIntoInt,
};
#[cfg(test)]
use crate::test_framework::core::util::failure_context::{
  ExecutionMethod, ExecutionOwner, ExecutionScope,
};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use std::vec;

macro_rules! catch_aborting_exception {
  ($has_hit_aborting_exception:expr, $aborting_exception:expr, $panic_message:expr, $operation:expr) => {{
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| $operation));
    if let Some(failure) = result.clone_caught_failure($panic_message) {
      *$has_hit_aborting_exception = true;
      let _ = $aborting_exception.set(failure);
    }
    unwrap_caught_result!(result)
  }};
}

/// Default general purpose indexing chain, which handles indexing all types of fields.
pub(crate) struct IndexingChain<D>
where
  D: Directory,
{
  bytes_used: SharedCounter,
  terms_hash: FreqProxTermsWriter<D>,
  doc_values_byte_pool: ByteBlockPool,
  stored_fields_consumer: StoredFieldsConsumer<D>,
  vector_values_consumer: VectorValuesConsumer<D>,
  field_hash: smallvec::SmallVec<[Option<NonZeroU32>; 2]>,
  hash_mask: usize,
  total_field_count: usize,
  // Holds fields seen in each document
  fields: smallvec::SmallVec<[u32; 1]>,
  per_fields: Vec<PerField>,
  doc_fields: smallvec::SmallVec<[u32; 2]>,
  field_buffer: Vec<Fields>,
  info_stream: InfoStreamMT,
  index_created_version_major: i32,
  has_hit_aborting_exception: bool,
  context: IndexContext,
}
pub(crate) struct IndexContext {
  term_vectors_bytes_used: SharedCounter,
  pub(crate) term_vectors_int_pool: IntBlockPool,
  pub(crate) freq_prox_term_int_pool: IntBlockPool,
  pub(crate) term_vectors_byte_pool: ByteBlockPool,
  pub(crate) byte_pool: ByteBlockPool,
}

impl<D> IndexingChain<D>
where
  D: Directory + Clone,
{
  pub(crate) fn new<D1, T>(
    index_created_version_major: i32,
    segment_info: &SegmentInfo<D1>,
    directory: D,
    index_writer_config: &T,
  ) -> Result<Self>
  where
    D: Clone,
    T: LiveIndexWriterConfig,
  {
    let bytes_used = Arc::new(AtomicCounter::new());
    let codec = index_writer_config.get_codec().clone();
    let (stored_fields_consumer, term_vectors_writer) = if segment_info.index_sort_ref().is_none() {
      (
        StoredFieldsConsumer::new(
          codec.clone(),
          directory.clone(),
          StoredFieldsConsumerHook::default(),
        ),
        TermVectorsConsumer::new(
          codec.clone(),
          directory.clone(),
          TermVectorsConsumerHook::default(),
        ),
      )
    } else {
      let stored_fields_consumer_sub = SortingStoredFieldsConsumer::new(directory.clone())?;
      let term_vector_consumer_sub = SortingTermVectorsConsumer::new(directory.clone())?;
      (
        StoredFieldsConsumer::new(
          codec.clone(),
          directory.clone(),
          StoredFieldsConsumerHook::Sorting(stored_fields_consumer_sub),
        ),
        TermVectorsConsumer::new(
          codec.clone(),
          directory.clone(),
          TermVectorsConsumerHook::Sorting(term_vector_consumer_sub),
        ),
      )
    };

    let terms_hash = FreqProxTermsWriter::new(bytes_used.clone(), term_vectors_writer);
    let doc_values_byte_pool = ByteBlockPool::new(DirectTrackingAllocatorByte::allocator_enum(
      bytes_used.clone(),
    ));
    let info_stream = index_writer_config.get_info_stream();
    let term_vectors_int_pool =
      IntBlockPool::with_allocator(IntBlockAllocator::allocator_enum(bytes_used.clone()));
    let freq_prox_term_int_pool =
      IntBlockPool::with_allocator(IntBlockAllocator::allocator_enum(bytes_used.clone()));
    let term_vectors_byte_pool = ByteBlockPool::new(DirectTrackingAllocatorByte::allocator_enum(
      bytes_used.clone(),
    ));
    let allocator = DirectTrackingAllocatorByte::allocator_enum(bytes_used.clone());
    let byte_pool = ByteBlockPool::new(allocator);
    let context = IndexContext {
      term_vectors_bytes_used: Arc::clone(&terms_hash.next_terms_hash.base.bytes_used),
      term_vectors_int_pool,
      freq_prox_term_int_pool,
      term_vectors_byte_pool,
      byte_pool,
    };
    let vector_values_consumer = VectorValuesConsumer::new(codec, directory, info_stream.clone())?;
    Ok(IndexingChain {
      bytes_used,
      terms_hash,
      doc_values_byte_pool,
      stored_fields_consumer,
      vector_values_consumer,
      field_hash: smallvec::smallvec![None; 2],
      hash_mask: 1,
      total_field_count: 0,
      fields: smallvec::SmallVec::new(),
      per_fields: vec![],
      doc_fields: smallvec::SmallVec::new(),
      field_buffer: Vec::new(),
      info_stream,
      index_created_version_major,
      has_hit_aborting_exception: false,
      context,
    })
  }
  pub(crate) fn maybe_sort_segment<D1>(
    &mut self,
    state: &SegmentWriteState<D>,
    segment_info: &SegmentInfo<D1>,
  ) -> Result<Option<Arc<DocMapImpl>>>
  where
    D: Directory,
  {
    let index_created_version_major = self.index_created_version_major;
    let Some(index_sort) = segment_info.index_sort_ref() else {
      return Ok(None);
    };

    let doc_values_reader = DocValuesLeafReaderImpl1::new(self, &state.field_infos);
    let max_doc = segment_info.max_doc()?;
    let has_blocks = segment_info.get_has_blocks();
    let parent_field = state.field_infos.get_parent_field();

    let parent_bit_set = if has_blocks {
      if let Some(parent_field) = parent_field {
        match doc_values_reader.get_numeric_doc_values(parent_field)? {
          Some(ref mut reader_values) => Some(of(reader_values, max_doc.try_convert()?)?),
          None => {
            return Err(LuceneError::corrupt_index(format!(
              "missing doc values for parent field {parent_field} IndexingChain"
            )));
          },
        }
      } else {
        None
      }
    } else {
      None
    };

    if has_blocks && parent_field.is_none() && index_created_version_major >= LUCENE_10_0_0.major {
      return Err(LuceneError::corrupt_index(format!(
        "parent field is not set but the index has blocks and uses index sorting. indexCreatedVersionMajor: {} \"IndexingChain\"",
        self.index_created_version_major
      )));
    }
    let mut comparators = smallvec::SmallVec::<[_; 1]>::with_capacity(index_sort.fields.len());
    for sort_field in &index_sort.fields {
      let sorter = sort_field.get_index_sorter()?.ok_or_else(|| {
        LuceneError::unsupported_operation(format!(
          "Cannot sort index using sort field {sort_field}"
        ))
      })?;
      let doc_comparator = sorter.get_doc_comparator(&doc_values_reader, max_doc)?;
      let v = match &parent_bit_set {
        Some(parent_bit_set) => {
          DocComparatorEnum2::A(DocComparatorImpl::new(parent_bit_set, doc_comparator))
        },
        None => DocComparatorEnum2::B(doc_comparator),
      };
      comparators.push(v);
    }
    // returns None if the documents are already sorted
    match Sorter::sort(max_doc, &comparators)? {
      Some(doc_map) => Ok(Some(Arc::new(doc_map))),
      None => Ok(None),
    }
  }
  pub(crate) fn flush<D1, T>(
    &mut self,
    state: &mut SegmentWriteState<D>,
    segment_info: &mut SegmentInfo<D1>,
    seg_updates: Option<&mut BufferedUpdates>,
    index_writer_config: &T,
  ) -> Result<Option<Arc<DocMapImpl>>>
  where
    T: LiveIndexWriterConfig,
  {
    #[cfg(test)]
    let _execution_scope =
      ExecutionScope::enter(ExecutionOwner::IndexingChain, ExecutionMethod::Flush);
    // NOTE: caller (DocumentsWriterPerThread) handles
    // aborting on any error from this method
    let sort_map = self.maybe_sort_segment(state, segment_info)?;
    let max_doc = segment_info.max_doc()?;

    // write norms
    let t0 = Instant::now();
    self.write_norms(state, sort_map.as_ref(), segment_info, index_writer_config)?;
    if self.info_stream.is_enabled("IW") {
      self.info_stream.message(
        "IW",
        &format!("{} ms to write norms", t0.elapsed().as_millis()),
      )?;
    }

    // write doc-values
    let t0 = Instant::now();
    self.write_doc_values(state, sort_map.as_ref(), segment_info, index_writer_config)?;
    if self.info_stream.is_enabled("IW") {
      self.info_stream.message(
        "IW",
        &format!("{} ms to write docValues", t0.elapsed().as_millis()),
      )?;
    }

    // write points
    let t0 = Instant::now();
    self.write_points(state, sort_map.as_ref(), index_writer_config, segment_info)?;
    if self.info_stream.is_enabled("IW") {
      self.info_stream.message(
        "IW",
        &format!("{} ms to write points", t0.elapsed().as_millis()),
      )?;
    }

    let t0 = Instant::now();
    self
      .vector_values_consumer
      .flush(segment_info, sort_map.as_ref())?;
    if self.info_stream.is_enabled("IW") {
      self.info_stream.message(
        "IW",
        &format!("{} ms to write vectors", t0.elapsed().as_millis()),
      )?;
    }

    // finish & flush stored fields
    let t0 = Instant::now();
    self.stored_fields_consumer.finish(max_doc, segment_info)?;
    self
      .stored_fields_consumer
      .flush(state, sort_map.as_ref(), segment_info)?;
    if self.info_stream.is_enabled("IW") {
      self.info_stream.message(
        "IW",
        &format!("{} ms to finish stored fields", t0.elapsed().as_millis()),
      )?;
    }

    let field_count = self
      .per_fields
      .iter()
      .filter(|pf| {
        pf.invert_state
          .as_ref()
          .and_then(|inversion| inversion.terms_hash.as_ref())
          .is_some_and(|field| field.base.get_num_terms() > 0)
      })
      .count();
    let mut fields_to_flush = Vec::with_capacity(field_count);
    for &idx in &self.field_hash {
      let mut fp_idx = idx;
      while let Some(index) = fp_idx {
        let index = index.get() as usize - 1;
        let pf = &mut self.per_fields[index];
        if let Some(inversion) = pf.invert_state.as_mut() {
          pf.schema
            .field_info()
            .ok_or_else(|| LuceneError::illegal_state("field info is missing"))?;
          let terms_hash = inversion
            .terms_hash
            .take()
            .ok_or_else(|| LuceneError::illegal_state("terms hash is missing"))?;
          if terms_hash.base.get_num_terms() > 0 {
            fields_to_flush.push(*terms_hash);
          }
        }
        fp_idx = pf.next;
      }
    }
    // Postings now own the field data they need; this segment will not index more documents.
    self.per_fields = Vec::new();
    self.field_hash.fill(None);
    let io_context = IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?;
    let read_state = SegmentReadState::with_suffix(
      state.directory,
      state.field_infos.clone(),
      io_context,
      &state.segment_suffix,
    );
    let mut norms = if read_state.field_infos.has_norms() {
      Some(
        index_writer_config
          .get_codec()
          .norms_format()
          .norms_producer(&read_state, segment_info)?,
      )
    } else {
      None
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      let norms_merge_instance = match norms.as_ref() {
        // Use the merge instance in order to reuse the same IndexInput for all terms
        Some(norms) => norms.get_merge_instance()?,
        None => None,
      };
      // Keep the selected producer concrete throughout postings consumption.
      macro_rules! flush_with_norms {
        ($norms:expr) => {{
          let norms_for_flush = $norms;
          let int_pool = std::mem::take(&mut self.context.freq_prox_term_int_pool);
          let byte_pool = std::mem::take(&mut self.context.byte_pool);
          self.terms_hash.flush(
            fields_to_flush,
            state,
            sort_map.as_ref(),
            norms_for_flush,
            index_writer_config.get_codec(),
            segment_info,
            seg_updates,
            int_pool,
            byte_pool,
          )
        }};
      }
      match norms_merge_instance.as_ref() {
        Some(merge_norms) => flush_with_norms!(Some(merge_norms)),
        None => flush_with_norms!(norms.as_ref()),
      }
    }));
    let close_result =
      std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match norms.as_mut() {
        Some(norms) => norms.close(),
        None => Ok(()),
      }));
    IOUtils::use_or_suppress_caught_result(result, close_result)?;
    if self.info_stream.is_enabled("IW") {
      self.info_stream.message(
        "IW",
        &format!(
          "{} ms to write postings and finish vectors",
          t0.elapsed().as_millis()
        ),
      )?;
    }
    // Important to save after asking consumer to flush so
    // consumer can alter the FieldInfo* if necessary.  EG,
    // FreqProxTermsWriter does this with
    // FieldInfo.storePayload.
    let t0 = Instant::now();
    index_writer_config.get_codec().field_infos_format().write(
      state.directory,
      segment_info,
      "",
      &state.field_infos,
      IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?,
    )?;
    if self.info_stream.is_enabled("IW") {
      self.info_stream.message(
        "IW",
        &format!("{} ms to write fieldInfos", t0.elapsed().as_millis()),
      )?;
    }

    Ok(sort_map)
  }
  ///  Writes all buffered points.
  pub fn write_points<DM, D1, T>(
    &mut self,
    state: &SegmentWriteState<D>,
    sort_map: Option<&DM>,
    index_writer_config: &T,
    info: &SegmentInfo<D1>,
  ) -> Result<()>
  where
    DM: DocMap + Clone,
    T: LiveIndexWriterConfig,
  {
    let mut points_writer = None;
    let mut success = false;
    debug_assert!(self.field_hash.len() <= i32::MAX as usize);

    let body_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      for bucket in 0..self.field_hash.len() {
        let mut per_field_index = self.field_hash[bucket];
        while let Some(index) = per_field_index {
          let index = index.get() as usize - 1;
          let per_field = &mut self.per_fields[index];
          if let Some(point_values_writer) = per_field.point_values_writer.as_mut() {
            let field_info = per_field
              .schema
              .field_info()
              .ok_or_else(|| LuceneError::illegal_state("field info is missing"))?;
            // We could have initialized pointValuesWriter, but failed to write even a single doc
            if field_info.get_point_dimension_count() > 0 {
              if points_writer.is_none() {
                // lazy init
                let fmt = index_writer_config.get_codec().points_format();
                points_writer = Some(fmt.fields_writer(state, info)?);
              }
              let points_writer = points_writer
                .as_mut()
                .ok_or_else(|| LuceneError::illegal_state("points writer is missing"))?;
              point_values_writer.flush(state.directory, sort_map, points_writer, info)?;
            }
          }

          per_field.point_values_writer = None;
          per_field_index = per_field.next;
        }
      }

      if let Some(w) = points_writer.as_mut() {
        w.finish()?;
      }
      success = true;
      Ok(())
    }));
    if success {
      if let Some(w) = points_writer.as_mut() {
        w.close()?;
      }
    } else {
      IOUtils::close_while_handling_exception(points_writer.as_mut());
    }
    unwrap_caught_result!(body_result)
  }
  // Finishes all doc values writers.
  pub(crate) fn finish_doc_values_writer(&mut self) -> Result<()> {
    let mut bytes = std::mem::take(&mut self.doc_values_byte_pool);
    let mut pool = None;
    let mut per_field_index;
    for i in 0..self.field_hash.len() {
      per_field_index = self.field_hash[i];
      while let Some(index) = per_field_index {
        let index = index.get() as usize - 1;
        let per_field = &mut self.per_fields[index];
        if let Some(ref mut writer) = per_field.doc_values_writer {
          let pool = pool.get_or_insert_with(|| Arc::new(std::mem::take(&mut bytes)));
          writer.finish(pool)?;
        }
        per_field_index = per_field.next;
      }
    }
    Ok(())
  }

  /// Writes all buffered doc values.
  fn write_doc_values<DM, D1, T>(
    &mut self,
    state: &SegmentWriteState<D>,
    sort_map: Option<&DM>,
    segment_info: &SegmentInfo<D1>,
    index_writer_config: &T,
  ) -> Result<()>
  where
    DM: DocMap,
    T: LiveIndexWriterConfig,
  {
    let mut dv_consumer = None;
    let mut success = false;

    let body_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      // iterate hash buckets
      let mut per_field_index;
      debug_assert!(self.field_hash.len() <= i32::MAX as usize);
      for bucket in 0..self.field_hash.len() {
        per_field_index = self.field_hash[bucket];
        while let Some(index) = per_field_index {
          let index = index.get() as usize - 1;
          let per_field = &mut self.per_fields[index];
          if let Some(ref mut writer) = per_field.doc_values_writer {
            let field_info = per_field
              .schema
              .field_info()
              .ok_or_else(|| LuceneError::illegal_state("field info is missing"))?;
            if *field_info.get_doc_values_type() == DocValuesType::None {
              return Err(LuceneError::illegal_state(format!(
                "segment= {}: field={} has no docvalues but wrote them",
                segment_info, field_info.name
              )));
            }
            let consumer = match dv_consumer {
              Some(ref mut consumer) => consumer,
              None => {
                // lazy init
                let fmt = index_writer_config.get_codec().doc_values_format();
                dv_consumer.insert(fmt.fields_consumer(state, segment_info)?)
              },
            };
            // Since it’s only ever called once globally, we didn’t implement the DocValuesWriter trait for DocValuesWriterEnum.
            writer.flush(state, sort_map, consumer, segment_info)?;
          } else if let Some(field_info) = per_field.schema.field_info()
            && field_info.get_doc_values_type() != &DocValuesType::None
          {
            return Err(LuceneError::illegal_state(format!(
              "segment={segment_info}: fieldInfos has docValues but did not write them"
            )));
          }
          per_field.doc_values_writer = None;
          per_field_index = per_field.next;
        }
      }
      success = true;
      Ok(())
    }));

    let has_dv_consumer = dv_consumer.is_some();
    if success {
      if let Some(consumer) = dv_consumer.as_mut() {
        consumer.close()?;
      }
    } else {
      IOUtils::close_while_handling_exception(dv_consumer.as_mut());
    }
    unwrap_caught_result!(body_result)?;

    if !state.field_infos.has_doc_values() {
      if has_dv_consumer {
        return Err(LuceneError::illegal_state(format!(
          "segment= {segment_info}: fieldInfos has no docValues but wrote them "
        )));
      }
    } else if !has_dv_consumer {
      return Err(LuceneError::illegal_state(format!(
        "segment= {segment_info}: fieldInfos has docValues but did not wrote them "
      )));
    }

    Ok(())
  }

  fn write_norms<DM, D1, T>(
    &mut self,
    state: &SegmentWriteState<D>,
    sort_map: Option<&DM>,
    segment_info: &SegmentInfo<D1>,
    index_writer_config: &T,
  ) -> Result<()>
  where
    DM: DocMap,
    T: LiveIndexWriterConfig,
  {
    if !state.field_infos.has_norms() {
      return Ok(());
    }

    let mut norms_consumer = {
      let norm_format = index_writer_config.get_codec().norms_format();
      norm_format.norms_consumer(state, segment_info)?
    };
    let mut success = false;

    let body_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      segment_info.max_doc()?;
      for fi in state.field_infos.iter() {
        let per_field_index = self.get_per_field(&fi.name);
        debug_assert!(per_field_index.is_some());
        // we must check the final value of omitNorms for the fieldinfo: it could have
        // changed for this field since the first time we added it.
        if !fi.omits_norms() && *fi.get_index_options() != IndexOptions::None {
          let per_field_index = per_field_index
            .ok_or_else(|| LuceneError::illegal_state("per-field entry is missing"))?;
          let per_field = self
            .per_fields
            .get_mut(per_field_index)
            .ok_or_else(|| LuceneError::illegal_state("per-field index is out of range"))?;
          let norms = per_field
            .invert_state
            .as_mut()
            .and_then(|inversion| inversion.norms.as_mut())
            .ok_or_else(|| LuceneError::illegal_state("norms writer is missing"))?;
          norms.flush(sort_map, &mut norms_consumer, segment_info)?;
        }
      }
      success = true;
      Ok(())
    }));
    if success {
      norms_consumer.close()?;
    } else {
      IOUtils::close_while_handling_exception(&mut norms_consumer);
    }
    unwrap_caught_result!(body_result)
  }

  pub(crate) fn abort(&mut self) -> Result<()> {
    #[cfg(test)]
    let _execution_scope =
      ExecutionScope::enter(ExecutionOwner::IndexingChain, ExecutionMethod::Abort);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      self.stored_fields_consumer.abort()?;
      self.vector_values_consumer.abort();
      Ok(())
    }));
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      self.terms_hash.abort(
        &mut self.context.freq_prox_term_int_pool,
        &mut self.context.byte_pool,
        &mut self.context.term_vectors_int_pool,
        &mut self.context.term_vectors_byte_pool,
      )
    }));
    self.field_hash.fill(None);
    self.doc_fields.clear();
    IOUtils::use_or_suppress_caught_result(result, close_result)
  }

  fn rehash(&mut self) {
    let old_size = self.field_hash.len();
    let new_size = old_size * 2;
    debug_assert!(new_size > old_size);
    self.field_hash.resize(new_size, None);
    let (low_buckets, high_buckets) = self.field_hash.split_at_mut(old_size);

    for (low, high) in low_buckets.iter_mut().zip(high_buckets) {
      let mut next = low.take();
      while let Some(index) = next {
        let field = &mut self.per_fields[index.get() as usize - 1];
        next = field.next;
        let hash = CoreHelper::calculate_hash(field.schema.name());
        let target = if hash & old_size as u64 == 0 {
          &mut *low
        } else {
          &mut *high
        };
        field.next = target.replace(index);
      }
    }
    self.hash_mask = new_size - 1;
  }

  /// Calls `start_document` on the stored fields consumer, aborting the segment on error.
  pub(crate) fn start_stored_fields<D1>(
    &mut self,
    doc_id: i32,
    info: &mut SegmentInfo<D1>,
    aborting_exception: &OnceLock<CaughtResult>,
  ) -> Result<()> {
    catch_aborting_exception!(
      &mut self.has_hit_aborting_exception,
      aborting_exception,
      "panic while starting stored fields",
      self.stored_fields_consumer.start_document(doc_id, info)
    )?;
    Ok(())
  }
  ///  Calls StoredFieldsWriter.finishDocument, aborting the segment if it hits any error .
  pub(crate) fn finish_stored_fields(
    &mut self,
    aborting_exception: &OnceLock<CaughtResult>,
  ) -> Result<()> {
    catch_aborting_exception!(
      &mut self.has_hit_aborting_exception,
      aborting_exception,
      "panic while finishing stored fields",
      self.stored_fields_consumer.finish_document()
    )
  }
  #[expect(
    clippy::too_many_arguments,
    reason = "The per-operation similarity is borrowed separately from the persistent writer configuration."
  )]
  pub(crate) fn process_document<DF, D1, T, S>(
    &mut self,
    doc_id: i32,
    document: DF,
    mut parent_field: Option<&mut Fields>,
    info: &mut SegmentInfo<D1>,
    field_infos: &mut Builder,
    index_writer_config: &T,
    aborting_exception_consumer: &OnceLock<CaughtResult>,
    similarity: &S,
  ) -> Result<()>
  where
    S: Similarity,
    DF: IndexingDocument,
    T: LiveIndexWriterConfig,
  {
    let mut document_fields = document.into_indexing_fields();
    // number of unique fields by names (collapses multiple field instances by the same name)
    let mut field_count = 0;
    // number of unique fields indexed with postings
    let mut indexed_field_count = 0;
    let mut doc_field_idx = 0;
    self.fields.clear();
    // NOTE: we need two passes here, in case there are
    // multi-valued fields, because we must process all
    // instances of a given field at once, since the
    // analyzer is free to reuse TokenStream across fields
    // (i.e., we cannot have more than one TokenStream
    // running "at once"):
    self.terms_hash.start_document()?;
    self.start_stored_fields(doc_id, info, aborting_exception_consumer)?;

    let mut document = DF::take_field_buffer(&mut self.field_buffer);
    // Keep the first field inline while retaining both schema and indexing passes.
    let mut first_field = None;
    // 1st pass over doc fields – verify that doc schema matches the index schema
    // build schema for each unique doc field

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      // Preserve the iterable's size_hint callback before inspecting any fields.
      if let IndexingFields::Iterable(fields) = &mut document_fields {
        document.reserve(
          fields
            .size_hint()
            .0
            .saturating_add(usize::from(parent_field.is_some()))
            .saturating_sub(1),
        );
      }
      macro_rules! check_schema {
        ($field:expr) => {
          dispatch_fields!($field, |field| self.process_field_schema(
            (field.field_type(), field.is_reserved(), field.name()),
            doc_id,
            &mut field_count,
            &mut doc_field_idx,
          ))?
        };
      }
      if let Some(field) = parent_field.as_deref() {
        check_schema!(field);
      }
      match &mut document_fields {
        IndexingFields::Borrowed(fields) => {
          for field in fields.iter() {
            check_schema!(field);
          }
        },
        IndexingFields::Iterable(fields) => {
          while let Some(field_value) = fields.next().transpose()? {
            check_schema!(field_value.borrow());
            if first_field.is_none() && parent_field.is_none() {
              first_field = Some(field_value);
            } else {
              document.push(field_value);
            }
          }
        },
      }

      self.doc_fields.truncate(doc_field_idx);

      // For each field, if it's the first time we see this field in this segment,
      // initialize its FieldInfo.
      // If we have already seen this field, verify that its schema
      // within the current doc matches its schema in the index.
      for i in 0..field_count {
        let idx = self.fields[i] as usize;
        let pf = &mut self.per_fields[idx];
        if let Some(field_info) = pf.schema.field_info() {
          pf.schema.assert_same_schema(field_info)?;
        } else {
          self.initialize_field_info(
            idx,
            field_infos,
            index_writer_config,
            info,
            aborting_exception_consumer,
          )?;
        }
      }

      // 2nd pass over doc fields – index each field
      // also count the number of unique fields indexed with postings
      doc_field_idx = 0;
      self.fields.clear();

      macro_rules! index_fields {
        ($fields:expr) => {
          for field in $fields {
            let field: &mut Fields = field.borrow_mut();
            let per_field_idx = self.doc_fields[doc_field_idx] as usize;
            if dispatch_fields!(field, |field| self.process_field(
              doc_id,
              field,
              per_field_idx,
              index_writer_config,
              aborting_exception_consumer,
            ))? {
              self.fields.push(self.doc_fields[doc_field_idx]);
              indexed_field_count += 1;
            }
            doc_field_idx += 1;
          }
        };
      }
      index_fields!(parent_field.iter_mut());
      match &mut document_fields {
        IndexingFields::Borrowed(fields) => index_fields!(fields.iter_mut()),
        IndexingFields::Iterable(_) => {
          index_fields!(first_field.iter_mut().chain(document.iter_mut()));
        },
      }
      Ok(())
    }));
    if !matches!(&result, Ok(Ok(()))) {
      self.doc_fields.clear();
    }
    if !self.has_hit_aborting_exception {
      // Finish each indexed field name seen in the document:
      for i in 0..indexed_field_count {
        let idx = self.fields[i] as usize;
        let pf = &mut self.per_fields[idx];
        debug_assert!(pf.schema.field_info().is_some());
        pf.finish(
          doc_id,
          idx,
          &mut self.terms_hash.next_terms_hash,
          similarity,
        )?;
      }
      self.finish_stored_fields(aborting_exception_consumer)?;
      catch_aborting_exception!(
        &mut self.has_hit_aborting_exception,
        aborting_exception_consumer,
        "panic while finishing terms hash document",
        self.terms_hash.finish_document(
          doc_id,
          info,
          &mut self.per_fields,
          &mut self.context.term_vectors_int_pool,
          &mut self.context.term_vectors_byte_pool,
          &mut self.context.byte_pool,
        )
      )?;
    }
    let result = unwrap_caught_result!(result);
    // Keep owners alive through all finish callbacks and preserve their drop order.
    drop(first_field);
    document.clear();
    DF::recycle_field_buffer(&mut self.field_buffer, document);
    drop(document_fields);
    result
  }
  pub(crate) fn initialize_field_info<D1, T>(
    &mut self,
    per_field_index: usize,
    field_infos: &mut Builder,
    index_writer_config: &T,
    segment_info: &SegmentInfo<D1>,
    aborting_exception: &OnceLock<CaughtResult>,
  ) -> Result<()>
  where
    T: LiveIndexWriterConfig,
  {
    // Create and add a new fieldInfo to fieldInfos for this segment.
    // During the creation of FieldInfo there is also verification of the correctness of all its
    // parameters.

    // If the fieldInfo doesn't exist in globalFieldNumbers for the whole index,
    // it will be added there.
    // If the field already exists in globalFieldNumbers (i.e. field present in other segments),
    // we check consistency of its schema with schema for the whole index.
    let pf = &mut self.per_fields[per_field_index];
    let s = &mut pf.schema;

    // validate sort DV type
    if let Some(index_sort) = index_writer_config.get_index_sort()
      && s.doc_values_type != DocValuesType::None
    {
      Self::validate_index_sort_dv_type(index_sort, s.name(), &s.doc_values_type)?;
    }
    let vectors = s.vectors();
    if vectors.dimension != 0 {
      let max_dim = index_writer_config
        .get_codec()
        .knn_vectors_format()?
        .get_max_dimensions(s.name())?;
      Self::validate_max_vector_dimension(s.name(), vectors.dimension, max_dim)?;
    }
    let soft_deletes_field = field_infos.is_soft_deletes_field_name(s.name());
    let is_parent_field = field_infos.is_parent_field_name(s.name());
    let field_info = FieldInfo::new(
      s.name().to_owned(),
      -1,
      s.store_term_vector,
      s.omit_norms,
      false, // storePayloads is set up during indexing, if payloads were seen
      s.index_options,
      s.doc_values_type,
      s.doc_values_skip_index,
      -1,
      s.attributes.clone(),
      s.point_dimension_count as usize,
      s.point_index_dimension_count as usize,
      s.point_num_bytes as usize,
      vectors.dimension as i32,
      vectors.encoding,
      vectors.similarity,
      soft_deletes_field,
      is_parent_field,
    )?;

    let fi = field_infos.add_owned(field_info)?;
    let indexed = *fi.get_index_options() != IndexOptions::None;
    pf.set_field_info(fi);

    if indexed {
      pf.set_invert_state(
        self.index_created_version_major,
        &mut self.terms_hash,
        &self.bytes_used,
      )?;
    }
    let fi = pf
      .schema
      .field_info()
      .ok_or_else(|| LuceneError::illegal_state("field info is missing"))?;

    pf.doc_values_writer = match fi.get_doc_values_type() {
      DocValuesType::None => None,
      DocValuesType::Numeric => Some(DocValuesWriterEnum::Numeric(Box::new(
        NumericDocValuesWriter::new(fi.clone(), self.bytes_used.clone())?,
      ))),
      DocValuesType::Binary => Some(DocValuesWriterEnum::Binary(Box::new(
        BinaryDocValuesWriter::new(fi.clone(), self.bytes_used.clone())?,
      ))),
      DocValuesType::Sorted => Some(DocValuesWriterEnum::Sorted(Box::new(
        SortedDocValuesWriter::new(fi.clone(), self.bytes_used.clone())?,
      ))),
      DocValuesType::SortedNumeric => Some(DocValuesWriterEnum::SortedNumeric(Box::new(
        SortedNumericDocValuesWriter::new(fi.clone(), self.bytes_used.clone())?,
      ))),
      DocValuesType::SortedSet => Some(DocValuesWriterEnum::SortedSet(Box::new(
        SortedSetDocValuesWriter::new(fi.clone(), self.bytes_used.clone())?,
      ))),
    };

    if fi.get_point_dimension_count() != 0 {
      pf.point_values_writer = Some(Box::new(PointValuesWriter::new(
        self.bytes_used.clone(),
        fi.clone(),
      )?));
    }

    if fi.get_vector_dimension() != 0 {
      let writer = catch_aborting_exception!(
        &mut self.has_hit_aborting_exception,
        aborting_exception,
        "panic while adding vector field",
        self
          .vector_values_consumer
          .add_field(Arc::clone(fi), segment_info)
      )?;
      let vectors = pf
        .schema
        .vectors
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("vector field state is missing"))?;
      vectors.writer_index = Some(writer);
    }

    Ok(())
  }

  fn process_field<T, T2>(
    &mut self,
    doc_id: i32,
    field: &mut T,
    per_field_index: usize,
    index_writer_config: &T2,
    aborting_exception: &OnceLock<CaughtResult>,
  ) -> Result<bool>
  where
    T: IndexableField,
    T2: LiveIndexWriterConfig,
  {
    let pf = &mut self.per_fields[per_field_index];
    let mut indexed_field = false;

    // Invert indexed fields
    if *field.field_type().index_options() != IndexOptions::None {
      // first time we see this field in this doc
      if pf.first {
        pf.invert(
          doc_id,
          field,
          true,
          index_writer_config.get_analyzer(),
          self.info_stream.as_ref(),
          &mut self.context,
          &mut self.has_hit_aborting_exception,
          aborting_exception,
        )?;
        pf.first = false;
        indexed_field = true;
      } else {
        pf.invert(
          doc_id,
          field,
          false,
          index_writer_config.get_analyzer(),
          self.info_stream.as_ref(),
          &mut self.context,
          &mut self.has_hit_aborting_exception,
          aborting_exception,
        )?;
      }
    }
    // Add stored fields
    if field.field_type().stored() {
      let stored_value = field
        .stored_value()?
        .ok_or_else(|| LuceneError::illegal_argument("Cannot store a null value"))?;
      let stored_value: &FieldDataEnum = stored_value.borrow();
      macro_rules! write_stored_value {
        ($value:expr) => {
          catch_aborting_exception!(
            &mut self.has_hit_aborting_exception,
            aborting_exception,
            "panic while writing stored field",
            self.stored_fields_consumer.write_field(
              pf.schema
                .field_info()
                .ok_or_else(|| LuceneError::illegal_state("field info is missing"))?,
              $value,
            )
          )
        };
      }
      match stored_value {
        FieldDataEnum::Binary(bytes) => write_stored_value!(bytes)?,
        value => {
          let text = match value {
            FieldDataEnum::String(s) => Some(s.as_str()),
            FieldDataEnum::SharedString(s) => Some(s.as_str()),
            FieldDataEnum::SharedText(s) => Some(s.as_ref()),
            _ => None,
          };
          if let Some(s) = text
            && s.len() > MAX_STORED_STRING_LENGTH
          {
            // UTF-8 byte length bounds UTF-16 length from above. Only count units
            // when the byte length alone cannot prove that the string fits.
            let length: usize = s.chars().map(char::len_utf16).sum();
            if length > MAX_STORED_STRING_LENGTH {
              return Err(LuceneError::illegal_argument(format!(
                "stored field \"{}\" is too large ({} characters) to store",
                field.name(),
                length
              )));
            }
          }
          write_stored_value!(value)?;
        },
      }
    }

    let dv_type = *field.field_type().doc_values_type();
    if dv_type != DocValuesType::None {
      Self::index_doc_value(doc_id, pf, dv_type, field, &mut self.doc_values_byte_pool)?;
    }

    // points
    if field.field_type().point_dimension_count() != 0 {
      let binary_value = field
        .binary_value()?
        .ok_or_else(|| LuceneError::illegal_argument("point field missing binary value"))?;
      let point_values_writer = pf
        .point_values_writer
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("point values writer is missing"))?;
      let value = binary_value.as_bytes_ref();
      point_values_writer.add_packed_value(doc_id, value.bytes, value.offset, value.length)?;
    }

    if field.field_type().vector_dimension() != 0 {
      let field_writer_idx = pf
        .schema
        .vectors
        .as_ref()
        .and_then(|vectors| vectors.writer_index)
        .ok_or_else(|| LuceneError::illegal_state("field writer idx not initialized"))?;
      self.index_vector_value(doc_id, field_writer_idx, field)?;
    }

    Ok(indexed_field)
  }
  // Keep the concrete field and field type through the first schema pass.
  fn process_field_schema<IFT: IndexableFieldType>(
    &mut self,
    (field_type, is_reserved, field_name): (IFT, bool, &str),
    doc_id: i32,
    field_count: &mut usize,
    doc_field_idx: &mut usize,
  ) -> Result<()> {
    // The previous document's slot is a hint. Validate the name before
    // reusing its per-segment index, since documents can reorder fields.
    let slot = match self.doc_fields.get(*doc_field_idx).copied() {
      Some(slot) if self.per_fields[slot as usize].schema.name() == field_name => slot,
      _ => self
        .get_or_add_per_field(field_name, is_reserved)?
        .try_convert()?,
    };
    let pf_idx = slot as usize;
    {
      let pf = &mut self.per_fields[pf_idx];
      if pf.reserved != is_reserved {
        return Err(LuceneError::illegal_argument(format!(
          "\"{}\" is a reserved field and should not be added to any document",
          field_name
        )));
      }

      if pf.schema.doc_id != doc_id {
        // The DWPT reserves a new doc ID before every indexing attempt.
        if self.fields.len() == self.fields.capacity() {
          let min_size = self.fields.len() + 1;
          ArrayUtil::oversize(min_size, std::mem::size_of::<u32>())?;
          self.fields.reserve(min_size - self.fields.len());
        }
        self.fields.push(slot);
        *field_count += 1;
        pf.reset(doc_id);
      }
    }
    if let Some(previous) = self.doc_fields.get_mut(*doc_field_idx) {
      *previous = slot;
    } else {
      if self.doc_fields.len() == self.doc_fields.capacity() {
        let min_size = self.doc_fields.len() + 1;
        ArrayUtil::oversize(min_size, std::mem::size_of::<u32>())?;
        self.doc_fields.reserve(min_size - self.doc_fields.len());
      }
      self.doc_fields.push(slot);
    }
    *doc_field_idx += 1;
    let pf = &mut self.per_fields[pf_idx];
    Self::update_doc_field_schema(field_name, &mut pf.schema, &field_type)?;
    if let Some(attrs) = field_type.get_attributes()
      && pf.field_info().is_none()
      && !attrs.is_empty()
    {
      pf.schema.update_attributes(attrs);
    }
    Ok(())
  }
  /// Returns a previously created [`PerField`], absorbing the type information from
  /// [`FieldType`](crate::core::document::field_type::FieldType), and creates a new [`PerField`] if this field name wasn't seen yet.
  #[inline(always)]
  pub(crate) fn get_or_add_per_field(&mut self, field_name: &str, reserved: bool) -> Result<usize> {
    let hash_pos = CoreHelper::calculate_hash(field_name) as usize & self.hash_mask;
    let mut per_field_index = self.field_hash[hash_pos];
    let mut conflict = false;
    while let Some(index) = per_field_index {
      let index = index.get() as usize - 1;
      conflict = true;
      debug_assert!(self.per_fields.get(index).is_some());
      let pf = &mut self.per_fields[index];
      if pf.schema.name() != field_name {
        per_field_index = pf.next;
      } else {
        return Ok(index);
      }
    }

    let index = self.per_fields.len();
    // Store a one-based slot so zero represents a missing link.
    let slot: u32 = (index + 1).try_convert()?;
    let schema = FieldSchema::new(field_name);
    let mut pf = PerField::new(schema, reserved);
    // filed_name's hash conflict happened, and could not find existing PerField with the same name in next chain
    if conflict {
      let old_pos = self.field_hash[hash_pos];
      pf.next = old_pos;
    }
    self.per_fields.push(pf);
    self.field_hash[hash_pos] = NonZeroU32::new(slot);

    self.total_field_count += 1;

    if self.total_field_count >= (self.field_hash.len() >> 1) {
      self.rehash();
    }
    Ok(index)
  }
  // update schema for field as seen in a particular document
  fn update_doc_field_schema<IFT>(
    field_name: &str,
    schema: &mut FieldSchema,
    field_type: &IFT,
  ) -> Result<()>
  where
    IFT: IndexableFieldType + ?Sized,
  {
    if *field_type.index_options() != IndexOptions::None {
      schema.set_index_options(
        *field_type.index_options(),
        field_type.omit_norms(),
        field_type.store_term_vectors(),
      )?;
    } else {
      Self::verify_unindexed_field_type(field_name, field_type)?;
    }

    if *field_type.doc_values_type() != DocValuesType::None {
      schema.set_doc_values(
        *field_type.doc_values_type(),
        *field_type.doc_values_skip_index_type(),
      )?;
    } else if *field_type.doc_values_skip_index_type() != DocValuesSkipIndexType::None {
      return Err(LuceneError::illegal_argument(format!(
        "field '{}' cannot have docValuesSkipIndexType={} without doc values",
        schema.name(),
        field_type.doc_values_skip_index_type()
      )));
    }

    if field_type.point_dimension_count() != 0 {
      schema.set_points(
        field_type.point_dimension_count(),
        field_type.point_index_dimension_count(),
        field_type.point_num_bytes(),
      )?;
    }

    if field_type.vector_dimension() != 0 {
      schema.set_vectors(
        *field_type.vector_encoding(),
        *field_type.vector_similarity_function(),
        field_type.vector_dimension() as usize,
      )?;
    }

    Ok(())
  }

  fn verify_unindexed_field_type<IFT>(name: &str, ft: &IFT) -> Result<()>
  where
    IFT: IndexableFieldType + ?Sized,
  {
    if ft.store_term_vectors() {
      return Err(LuceneError::illegal_argument(format!(
        "cannot store term vectors for a field that is not indexed (field=\"{name}\")"
      )));
    }
    if ft.store_term_vector_positions() {
      return Err(LuceneError::illegal_argument(format!(
        "cannot store term vector positions for a field that is not indexed (field=\"{name}\")"
      )));
    }
    if ft.store_term_vector_offsets() {
      return Err(LuceneError::illegal_argument(format!(
        "cannot store term vector offsets for a field that is not indexed (field=\"{name}\")"
      )));
    }
    if ft.store_term_vector_payloads() {
      return Err(LuceneError::illegal_argument(format!(
        "cannot store term vector payloads for a field that is not indexed (field=\"{name}\")"
      )));
    }
    Ok(())
  }

  fn validate_max_vector_dimension(
    field_name: &str,
    vector_dim: usize,
    max_vector_dim: usize,
  ) -> Result<()> {
    if vector_dim > max_vector_dim {
      return Err(LuceneError::illegal_argument(format!(
        "Field [{field_name}] vector's dimensions must be <= [{max_vector_dim}]; got {vector_dim}"
      )));
    }
    Ok(())
  }

  fn validate_index_sort_dv_type(
    index_sort: &Sort,
    field_to_validate: &str,
    dv_type: &DocValuesType,
  ) -> Result<()> {
    for sort_field in index_sort.get_sort() {
      let sorter = sort_field.get_index_sorter()?.ok_or_else(|| {
        LuceneError::illegal_state(format!("Cannot sort index with sort order {sort_field}"))
      })?;
      let doc_values_leaf_reader =
        DocValuesLeafReaderImpl2::new(field_to_validate, dv_type, sort_field);
      sorter.get_doc_comparator(&doc_values_leaf_reader, 0)?;
    }
    Ok(())
  }

  pub fn index_doc_value<T>(
    doc_id: i32,
    fp: &mut PerField,
    dv_type: DocValuesType,
    field: &T,
    pool: &mut ByteBlockPool,
  ) -> Result<()>
  where
    T: IndexableField,
  {
    let field_name = &fp
      .schema
      .field_info()
      .ok_or_else(|| LuceneError::illegal_state("field info is missing"))?
      .name;
    match fp.doc_values_writer.as_mut() {
      Some(DocValuesWriterEnum::Numeric(writer)) => {
        debug_assert_eq!(dv_type, DocValuesType::Numeric);
        let value = field.numeric_value()?.ok_or_else(|| {
          LuceneError::illegal_argument(format!("field=\"{field_name}\": null value not allowed"))
        })?;
        let num = match value {
          Number::I64(value) => Some(value),
          other => other.to_i64(),
        };
        match num {
          Some(num) => {
            writer.add_value(doc_id, num)?;
          },
          _ => {
            return Err(LuceneError::illegal_argument(format!(
              "field=\"{field_name}\": numeric value out of range: {num:?}"
            )));
          },
        }
      },
      Some(DocValuesWriterEnum::Binary(writer)) => {
        debug_assert_eq!(dv_type, DocValuesType::Binary);
        let bytes = field.binary_value()?.ok_or_else(|| {
          LuceneError::illegal_argument(format!("field=\"{field_name}\": null value not allowed"))
        })?;
        writer.add_value(doc_id, &bytes.as_bytes_ref())?;
      },
      Some(DocValuesWriterEnum::Sorted(writer)) => {
        debug_assert_eq!(dv_type, DocValuesType::Sorted);
        let bytes = field.binary_value()?.ok_or_else(|| {
          LuceneError::illegal_argument(format!("field=\"{field_name}\": null value not allowed"))
        })?;
        writer.add_value(doc_id, &bytes.as_bytes_ref(), pool)?;
      },
      Some(DocValuesWriterEnum::SortedNumeric(writer)) => {
        debug_assert_eq!(dv_type, DocValuesType::SortedNumeric);
        let value = field.numeric_value()?.ok_or_else(|| {
          LuceneError::illegal_argument(format!("field=\"{field_name}\": null value not allowed"))
        })?;
        let num = match value {
          Number::I64(value) => Some(value),
          other => other.to_i64(),
        };

        match num {
          Some(num) => {
            writer.add_value(doc_id, num)?;
          },
          _ => {
            return Err(LuceneError::illegal_argument(format!(
              "field=\"{field_name}\": numeric value out of range: {num:?}"
            )));
          },
        }
      },
      Some(DocValuesWriterEnum::SortedSet(writer)) => {
        debug_assert_eq!(dv_type, DocValuesType::SortedSet);
        let bytes = field.binary_value()?.ok_or_else(|| {
          LuceneError::illegal_argument(format!("field=\"{field_name}\": null value not allowed"))
        })?;
        writer.add_value(doc_id, &bytes.as_bytes_ref(), pool)?;
      },
      None => {
        return Err(LuceneError::illegal_state(format!(
          "field=\"{field_name}\": no DocValuesWriter for type {dv_type:?}"
        )));
      },
    }
    Ok(())
  }

  fn index_vector_value<T>(&mut self, doc_id: i32, field_writer_idx: usize, field: &T) -> Result<()>
  where
    T: IndexableField,
  {
    let writer = self
      .vector_values_consumer
      .writer
      .as_mut()
      .ok_or_else(|| LuceneError::illegal_state("writer not initialized"))?;
    match field.field_type().vector_encoding() {
      VectorEncoding::FLOAT32(_) => {
        writer.add_value(doc_id, field.float_vector_value()?, field_writer_idx)
      },
      VectorEncoding::BYTE(_) => {
        writer.add_value(doc_id, field.byte_vector_value()?, field_writer_idx)
      },
    }
  }

  fn get_per_field(&self, name: &str) -> Option<usize> {
    let hash_pos = CoreHelper::calculate_hash(name) as usize & self.hash_mask;
    let mut per_field_index = self.field_hash[hash_pos];
    while let Some(index) = per_field_index {
      let index = index.get() as usize - 1;
      let pf = &self.per_fields[index];
      if pf.schema.name() == name {
        return Some(index);
      }
      per_field_index = pf.next;
    }
    None
  }
  pub(crate) fn mark_as_reserved(&mut self, field: &Fields) -> Result<()> {
    self.get_or_add_per_field(field.name(), true).map(|_| ())
  }

  pub(crate) fn get_has_doc_values(&mut self, field: &str) -> Result<Option<DocValuesWriterDISI>> {
    if let Some(idx) = self.get_per_field(field) {
      let pf = self
        .per_fields
        .get_mut(idx)
        .ok_or_else(|| LuceneError::illegal_state("per-field index is out of range"))?;
      if let Some(ref writer_enum) = pf.doc_values_writer {
        let field_info = pf
          .schema
          .field_info()
          .ok_or_else(|| LuceneError::illegal_state("field info is missing"))?;
        if *field_info.get_doc_values_type() == DocValuesType::None {
          return Ok(None);
        }
        return Ok(Some(writer_enum.get_doc_values()?));
      }
    }
    Ok(None)
  }
}
impl<D> Accountable for IndexingChain<D>
where
  D: Directory,
  StoredFieldsConsumer<D>: Accountable,
{
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(
      self
        .bytes_used
        .get()
        .saturating_add((self.field_buffer.capacity() * std::mem::size_of::<Fields>()) as i64)
        .saturating_add(self.stored_fields_consumer.ram_bytes_used()?)
        .saturating_add(self.terms_hash.next_terms_hash.ram_bytes_used()?)
        .saturating_add(
          self
            .vector_values_consumer
            .get_accountable()
            .ram_bytes_used()?,
        ),
    )
  }

  fn get_child_resources(&self) -> Vec<&dyn Accountable> {
    vec![
      &self.stored_fields_consumer,
      &self.terms_hash.next_terms_hash,
      self.vector_values_consumer.get_accountable(),
    ]
  }
}

pub(crate) struct PerFieldInversion {
  state: FieldInvertState,
  token_stream: Option<ReusedIndexingTokenStream>,
  pub(super) terms_hash: Option<Box<FreqProxTermsWriterPerField>>,
  norms: Option<Box<NormValuesWriter>>,
}

pub(crate) struct PerField {
  pub(crate) schema: FieldSchema,
  pub(crate) reserved: bool,
  pub(super) invert_state: Option<Box<PerFieldInversion>>,
  pub(crate) doc_values_writer: Option<DocValuesWriterEnum>,
  pub(crate) point_values_writer: Option<Box<PointValuesWriter>>,
  next: Option<NonZeroU32>,
  pub(crate) first: bool,
}
impl PerField {
  fn on_aborting_exception(
    has_hit_aborting_exception: &mut bool,
    aborting_exception: &OnceLock<CaughtResult>,
    throwable: CaughtResult,
  ) {
    *has_hit_aborting_exception = true;
    let _ = aborting_exception.set(throwable);
  }

  pub(crate) fn new(schema: FieldSchema, reserved: bool) -> Self {
    PerField {
      schema,
      reserved,
      invert_state: None,
      doc_values_writer: None,
      point_values_writer: None,
      next: None,
      first: false,
    }
  }
  pub(crate) fn reset(&mut self, doc_id: i32) {
    self.first = true;
    self.schema.reset(doc_id);
  }

  pub(crate) fn field_info(&self) -> Option<&Arc<FieldInfo>> {
    self.schema.field_info()
  }

  pub(crate) fn set_field_info(&mut self, field_info: Arc<FieldInfo>) {
    debug_assert!(self.schema.field_info().is_none());
    self.schema.field = SchemaField::Registered(field_info);
  }
  pub(crate) fn set_invert_state<D>(
    &mut self,
    index_created_version_major: i32,
    terms_hash: &mut FreqProxTermsWriter<D>,
    bytes_used: &SharedCounter,
  ) -> Result<()>
  where
    D: Directory + Clone,
  {
    let fi = self
      .schema
      .field_info()
      .ok_or_else(|| LuceneError::illegal_state("field info is missing"))?;
    let state = FieldInvertState::from_field_info(index_created_version_major, Arc::clone(fi));
    let inversion = self.invert_state.insert(Box::new(PerFieldInversion {
      state,
      token_stream: None,
      terms_hash: None,
      norms: None,
    }));
    inversion.terms_hash = Some(Box::new(terms_hash.add_field(fi.clone())?));

    if !fi.omits_norms() {
      // Even if no documents actually succeed in setting a norm, we still write norms for this
      // segment
      debug_assert!(inversion.norms.is_none());
      inversion.norms = Some(Box::new(NormValuesWriter::new(
        fi.clone(),
        bytes_used.clone(),
      )?));
    }

    if fi.has_term_vectors() {
      terms_hash.next_terms_hash.set_has_vectors();
    }
    Ok(())
  }
  pub(crate) fn finish<D, S>(
    &mut self,
    doc_id: i32,
    field_slot: usize,
    term_vectors_consumer: &mut TermVectorsConsumer<D>,
    similarity: &S,
  ) -> Result<()>
  where
    D: Directory + Clone,
    S: Similarity,
  {
    let field_info = self
      .schema
      .field_info()
      .ok_or_else(|| LuceneError::illegal_state("field info is missing"))?;
    if !field_info.omits_norms() {
      let norm_value = {
        let state = self
          .invert_state
          .as_ref()
          .map(|inversion| &inversion.state)
          .ok_or_else(|| LuceneError::illegal_state("invert state is missing"))?;
        if state.length == 0 {
          // the field exists in this document, but it did not have
          // any indexed tokens, so we assign a default value of zero
          // to the norm
          0
        } else {
          let nv = similarity.compute_norm(state)?;
          if nv == 0 {
            return Err(LuceneError::illegal_state(format!(
              "Similarity {similarity} returned 0 for non-empty field"
            )));
          }
          nv
        }
      };
      self
        .invert_state
        .as_mut()
        .and_then(|inversion| inversion.norms.as_mut())
        .ok_or_else(|| LuceneError::illegal_state("norms writer is missing"))?
        .add_value(doc_id, norm_value)?;
    }
    self
      .invert_state
      .as_mut()
      .and_then(|inversion| inversion.terms_hash.as_mut())
      .ok_or_else(|| LuceneError::illegal_state("terms hash is missing"))?
      .finish(term_vectors_consumer, field_slot)
  }
  /// Inverts one field for one document; first is true if this is the first time we are seeing
  /// this field name in this document.
  #[allow(clippy::too_many_arguments)]
  pub(crate) fn invert<A, T>(
    &mut self,
    doc_id: i32,
    field: &mut T,
    first: bool,
    analyzer: &A,
    info_stream: &InfoStreamEnum,
    context: &mut IndexContext,
    has_hit_aborting_exception: &mut bool,
    aborting_exception: &OnceLock<CaughtResult>,
  ) -> Result<()>
  where
    A: Analyzer,
    T: IndexableField,
  {
    debug_assert!(
      *field.field_type().index_options() >= IndexOptions::Docs,
      "field must be indexed with at least Docs"
    );

    if first {
      match &mut self.invert_state {
        Some(invert_state) => {
          // First time we're seeing this field (indexed) in this document
          invert_state.state.reset()
        },
        None => {
          return Err(LuceneError::illegal_state("invert_state not initialized"));
        },
      }
    }

    match field.invertable_type() {
      InvertableType::BINARY => {
        self.invert_term(doc_id, field, first, context)?;
      },
      InvertableType::TokenStream => {
        self.invert_token_stream(
          doc_id,
          field,
          first,
          analyzer,
          info_stream,
          context,
          has_hit_aborting_exception,
          aborting_exception,
        )?;
      },
    }

    Ok(())
  }
  #[allow(clippy::too_many_arguments)]
  fn invert_token_stream<A, T>(
    &mut self,
    doc_id: i32,
    field: &mut T,
    first: bool,
    analyzer: &A,
    info_stream: &InfoStreamEnum,
    context: &mut IndexContext,
    has_hit_aborting_exception: &mut bool,
    aborting_exception: &OnceLock<CaughtResult>,
  ) -> Result<()>
  where
    A: Analyzer,
    T: IndexableField,
  {
    let (analyzed, vector_options) = {
      // The stream can borrow the field, so copy its settings before obtaining it.
      let field_type = field.field_type();
      (
        field_type.tokenized(),
        TermVectorOptions::from_field_type(&field_type),
      )
    };
    /*
     * To assist people in tracking down problems in analysis components, we wish to write the field name to the infostream
     * when we fail. We expect some caller to eventually deal with the real error, so we don't want any 'catch' clauses,
     * but rather a finally that takes note of the problem.
     */

    let field_name = self.schema.name();

    let mut succeeded_in_processing_field = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      let PerFieldInversion {
        state: invert_state,
        token_stream,
        terms_hash,
        ..
      } = self
        .invert_state
        .as_deref_mut()
        .ok_or_else(|| LuceneError::illegal_state("terms hash is missing"))?;
      let terms_hash_per_field = terms_hash
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("terms hash is missing"))?;
      let mut stream = field
        .token_stream(analyzer, token_stream)?
        .ok_or_else(|| LuceneError::illegal_state("Analyzer token_stream is not initialized"))?;
      // Keep the owner (including the analyzer's RefMut) alive while a concrete
      // stream is borrowed for the entire reset/increment/end/close sequence.
      macro_rules! consume_tokens {
        ($stream:expr, $vectors:literal) => {{
          while $stream.increment_token()? {
            // If we hit an error in stream.next below
            // (which is fairly common, e.g. if analyzer
            // chokes on a given document), then it's
            // non-aborting and (above) this one document
            // will be marked as deleted, but still
            // consume a docID
            let attribute_source = $stream.get_attribute_source_mut();
            // A custom stream may replace its attributes between tokens. Select
            // once for this token, then keep the inversion and postings concrete.
            macro_rules! invert_token {
              ($attribute_source:expr) => {{
                let attribute_source = $attribute_source;
            let pos_incr = attribute_source.get_position_increment()?;
            invert_state.position += pos_incr;
            if invert_state.position < invert_state.last_position {
              return if pos_incr == 0 {
                Err(LuceneError::illegal_argument(format!(
                  "first position increment must be > 0 (got 0) for field '{}'",
                  field_name
                )))
              } else if pos_incr < 0 {
                // position increment must be > 0
                Err(LuceneError::illegal_argument(format!(
                  "position increment must be > 0 (got {}) for field '{}'",
                  pos_incr, field_name
                )))
              } else {
                Err(LuceneError::illegal_argument(format!(
                  "position overflowed i32::MAX (got pos_incr={} last_position={} position={}) for field '{}'",
                  pos_incr, invert_state.last_position, invert_state.position, field_name
                )))
              };
            } else if invert_state.position > MAX_POSITION {
              return Err(LuceneError::illegal_argument(format!(
                "position {} too large for field {}",
                invert_state.position, field_name
              )));
            }
            if pos_incr == 0 {
              invert_state.num_overlap += 1;
            }
            invert_state.last_position = invert_state.position;
            let start = attribute_source.start_offset()?;
            let end = attribute_source.end_offset()?;
            let start_offset = invert_state.offset + start;
            let end_offset = invert_state.offset + end;
            if start_offset < invert_state.last_start_offset || end_offset < start_offset {
              return Err(LuceneError::illegal_argument(format!(
                "startOffset must be non-negative, and endOffset must be >= startOffset, and offsets must not go backwards offsets: start={} end={} last_start={} for field {}",
                start_offset, end_offset, invert_state.last_start_offset, field_name
              )));
            }
            invert_state.last_start_offset = start_offset;
            // update length
            let tf = attribute_source.get_term_frequency()?;
            invert_state.length = invert_state.length.checked_add(tf).ok_or_else(|| {
              LuceneError::illegal_argument(format!("too many tokens for field {}", field_name))
            })?;
            // If we hit an error in here, we abort
            // all buffered documents since the last
            // flush, on the likelihood that the
            // internal state of the terms hash is now
            // corrupt and should not be flushed to a
            // new segment:
            let add_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
              terms_hash_per_field.add_with_bytes_ref_mode::<$vectors, _>(
                // use attribute_source's bytes_ref
                None,
                doc_id,
                invert_state,
                attribute_source,
                context,
              )
            }));
            match add_result {
              Ok(Ok(())) => {},
              Ok(Err(e)) if matches!(e.kind(), LuceneErrorKind::MaxBytesLengthExceeded(_)) => {
                let bytes_ref = attribute_source.get_bytes_ref()?.ok_or_else(|| {
                  LuceneError::illegal_state("BytesRef is None in attribute_source")
                })?;
                let mut prefix = [0u8; 30];
                let len = 30.min(bytes_ref.length);
                prefix[..len]
                  .copy_from_slice(&bytes_ref.bytes[bytes_ref.offset..bytes_ref.offset + len]);
                let mut ia = LuceneError::illegal_argument(format!(
                  "Document contains at least one immense term in field=\"{}\" (whose UTF8 encoding is longer than the max length {}), all of which were skipped. Please correct the analyzer to not produce such terms. The prefix of the first immense term is: '{:?}...', original message: {}",
                  field_name, MAX_TERM_LENGTH, prefix, e
                ));
                ia.add_suppressed(e);
                return Err(ia);
              },
              add_result => {
                let failure = add_result
                  .clone_caught_failure("panic while adding term")
                  .ok_or_else(|| LuceneError::illegal_state("term add result must have failed"))?;
                Self::on_aborting_exception(
                  has_hit_aborting_exception,
                  aborting_exception,
                  failure,
                );
                return unwrap_caught_result!(add_result);
              },
            }
              }};
            }
            match attribute_source {
              Attributes::PackedToken(attributes) => invert_token!(attributes),
              Attributes::BytesTerm(attributes) => invert_token!(attributes),
              Attributes::BinaryTokenStream(attributes) => invert_token!(attributes),
              #[cfg(test)]
              Attributes::RandomTokenStream(attributes) => invert_token!(attributes),
            }
          }
        }};
      }
      macro_rules! consume_stream {
        ($stream:expr) => {{
          let stream = $stream;
          let stream_result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
              stream.reset()?;
              let do_vectors = terms_hash_per_field.start(
                vector_options,
                first,
                &context.term_vectors_bytes_used,
                &mut context.byte_pool,
              )?;
              if do_vectors {
                consume_tokens!(stream, true);
              } else {
                consume_tokens!(stream, false);
              }
              stream.end()?;
              invert_state.position += stream.get_attribute_source().get_position_increment()?;
              invert_state.offset += stream.get_attribute_source().end_offset()?;
              Ok(())
            }));
          succeeded_in_processing_field = matches!(&stream_result, Ok(Ok(())));
          let close_result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| stream.close()));
          IOUtils::use_or_suppress_caught_result(stream_result, close_result)
        }};
      }
      match &mut stream {
        IndexingTokenStreamEnum3::AnalyzerTokenStream(stream) => match &mut **stream {
          AnalyzerTokenStreams::Whitespace(stream) => consume_stream!(stream),
          AnalyzerTokenStreams::Standard(stream) => consume_stream!(stream),
          AnalyzerTokenStreams::Dummy(stream) => consume_stream!(stream),
          AnalyzerTokenStreams::Custom(stream) => consume_stream!(stream),
        },
        IndexingTokenStreamEnum3::Reused(stream) => match &mut **stream {
          ReusedIndexingTokenStream::A(stream) => consume_stream!(stream),
          ReusedIndexingTokenStream::B(stream) => consume_stream!(stream),
        },
        IndexingTokenStreamEnum3::FieldTokenStream(stream) => match &mut **stream {
          FieldTokenStreamEnum::Dummy(stream) => consume_stream!(stream),
          FieldTokenStreamEnum::Custom(stream) => consume_stream!(stream),
        },
      }
    }));

    if !succeeded_in_processing_field && info_stream.is_enabled("DW") {
      info_stream.message(
        "DW",
        &format!("error in invert_token_stream for {}", field_name),
      )?;
    }
    unwrap_caught_result!(result)?;

    if analyzed {
      let field_name = &self
        .schema
        .field_info()
        .ok_or_else(|| LuceneError::illegal_state("field info is missing"))?
        .name;
      let invert_state = self
        .invert_state
        .as_mut()
        .map(|inversion| &mut inversion.state)
        .ok_or_else(|| LuceneError::illegal_state("invert state is missing"))?;
      invert_state.position += analyzer.get_position_increment_gap(field_name);
      invert_state.offset += analyzer.get_offset_gap(field_name);
    }
    Ok(())
  }

  fn invert_term<F>(
    &mut self,
    doc_id: i32,
    field: &F,
    first: bool,
    context: &mut IndexContext,
  ) -> Result<()>
  where
    F: IndexableField,
  {
    let binary_value = field.binary_value()?.ok_or_else(|| {
      LuceneError::illegal_argument(format!(
        "Field {} returns TERM for invertable_type() and null for binary_value(), which is illegal",
        field.name()
      ))
    })?;

    let field_type = field.field_type();
    if field_type.tokenized()
      || *field_type.index_options() > IndexOptions::DocsAndFreqs
      || field_type.store_term_vector_positions()
      || field_type.store_term_vector_offsets()
      || field_type.store_term_vector_payloads()
    {
      return Err(LuceneError::illegal_argument(format!(
        "Fields that are tokenized or index proximity data must produce a non-null TokenStream, but {} did not",
        field.name()
      )));
    }
    let PerFieldInversion {
      state, terms_hash, ..
    } = self
      .invert_state
      .as_deref_mut()
      .ok_or_else(|| LuceneError::illegal_state("invert state is missing"))?;
    // TODO
    // state.set_attribute_source();
    state.position += 1;
    state.length += 1;
    let terms_hash_per_field = terms_hash
      .as_mut()
      .ok_or_else(|| LuceneError::illegal_state("terms hash is missing"))?;
    terms_hash_per_field.start(
      TermVectorOptions::from_field_type(&field_type),
      first,
      &context.term_vectors_bytes_used,
      &mut context.byte_pool,
    )?;
    match state.length.checked_add(1) {
      Some(new_length) => {
        state.length = new_length;
      },
      None => {
        return Err(LuceneError::number_overflow("Field length overflowed"));
      },
    }
    let mut attribute_source = EmptyAttributeSource;
    match terms_hash_per_field.add_with_bytes_ref(
      Some(&binary_value.as_bytes_ref()),
      doc_id,
      state,
      &mut attribute_source,
      context,
    ) {
      Ok(()) => {},
      Err(e) if matches!(e.kind(), LuceneErrorKind::MaxBytesLengthExceeded(_)) => {
        let mut prefix = [0u8; 30];
        prefix.copy_from_slice(&binary_value.as_bytes()[..30]);
        let msg = format!(
          "Document contains at least one immense term in field=\"{}\" (whose length is longer than the max length {}), all of which were skipped. The prefix of the first immense term is: '{:?}...'",
          self.schema.name(),
          MAX_TERM_LENGTH,
          prefix
        );
        let mut ia = LuceneError::illegal_argument(msg);
        ia.add_suppressed(e);
        return Err(ia);
      },
      Err(error) => return Err(error),
    }
    Ok(())
  }
}

impl PartialEq for PerField {
  fn eq(&self, other: &Self) -> bool {
    self.schema.name() == other.schema.name()
  }
}
impl Eq for PerField {}

impl PartialOrd for PerField {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}
impl Ord for PerField {
  fn cmp(&self, other: &Self) -> Ordering {
    self.schema.name().cmp(other.schema.name())
  }
}

pub struct IntBlockAllocator {
  block_size: usize,
  pub byte_used: SharedCounter,
}
impl IntBlockAllocator {
  fn new(byte_used: SharedCounter) -> Self {
    IntBlockAllocator {
      block_size: INT_BLOCK_SIZE as usize,
      byte_used,
    }
  }
  pub(crate) fn allocator_enum(byte_used: SharedCounter) -> AllocatorIntEnum {
    AllocatorIntEnum::IBA(IntBlockAllocator::new(byte_used))
  }
}
impl AllocatorI32 for IntBlockAllocator {
  fn recycle_int_blocks(&mut self, _blocks: &[Vec<i32>], _offset: usize, length: usize) {
    let delta = length as i64 * (self.block_size as i64 * BitUtil::INT_BYTES as i64);
    self.byte_used.add_and_get(-delta);
  }

  fn get_byte_block(&mut self) -> Vec<i32> {
    let b = vec![0; INT_BLOCK_SIZE as usize];
    self
      .byte_used
      .add_and_get(INT_BLOCK_SIZE as i64 * BitUtil::INT_BYTES as i64);
    b
  }

  fn get_block_size(&self) -> usize {
    self.block_size
  }
}

/// A schema of the field in the current document. With every new document this schema is reset.
/// As the document’s fields are processed, we update the schema with any options encountered in
/// this document. Once processing for the document is complete, we compare the built schema of
/// the current document with the corresponding [`FieldInfo`](crate::core::index::field_info::FieldInfo) (constructed from the first document
/// in the segment where this field appeared). If there is any inconsistency, we return an error.
/// This ensures that a field’s data structures remain consistent across all documents.
pub(crate) struct FieldSchema {
  field: SchemaField,
  doc_id: i32,
  attributes: Arc<HashMap<String, String>>,
  omit_norms: bool,
  store_term_vector: bool,
  index_options: IndexOptions,
  doc_values_type: DocValuesType,
  doc_values_skip_index: DocValuesSkipIndexType,
  point_dimension_count: u32,
  point_index_dimension_count: u32,
  point_num_bytes: u32,
  vectors: Option<Box<FieldVectorState>>,
}

struct FieldVectorState {
  schema: FieldVectorSchema,
  writer_index: Option<usize>,
}

struct FieldVectorSchema {
  dimension: usize,
  encoding: VectorEncoding,
  similarity: VectorSimilarityFunction,
}

const EMPTY_VECTOR_SCHEMA: FieldVectorSchema = FieldVectorSchema {
  dimension: 0,
  encoding: VectorEncoding::FLOAT32(4),
  similarity: VectorSimilarityFunction::Euclidean,
};

// The pending name is only needed until the segment creates its FieldInfo.
enum SchemaField {
  Pending(Box<str>),
  Registered(Arc<FieldInfo>),
}

impl FieldSchema {
  fn vectors(&self) -> &FieldVectorSchema {
    self
      .vectors
      .as_ref()
      .map_or(&EMPTY_VECTOR_SCHEMA, |vectors| &vectors.schema)
  }

  fn name(&self) -> &str {
    match &self.field {
      SchemaField::Pending(name) => name,
      SchemaField::Registered(field_info) => field_info.get_name(),
    }
  }

  fn field_info(&self) -> Option<&Arc<FieldInfo>> {
    match &self.field {
      SchemaField::Pending(_) => None,
      SchemaField::Registered(field_info) => Some(field_info),
    }
  }

  const ERR_MSG: &'static str =
    "Inconsistency of field data structures across documents for field ";
  pub(crate) fn new(name: &str) -> Self {
    FieldSchema {
      field: SchemaField::Pending(name.into()),
      doc_id: -1,
      attributes: Arc::clone(&crate::core::index::field_info::EMPTY_FIELD_ATTRIBUTES),
      omit_norms: false,
      store_term_vector: false,
      index_options: IndexOptions::None,
      doc_values_type: DocValuesType::None,
      doc_values_skip_index: DocValuesSkipIndexType::None,
      point_dimension_count: 0,
      point_index_dimension_count: 0,
      point_num_bytes: 0,
      vectors: None,
    }
  }
  #[inline]
  pub(crate) fn assert_same<T>(&self, label: &str, expected: &T, given: &T) -> Result<()>
  where
    T: PartialEq + Display,
  {
    if expected != given {
      return Err(LuceneError::illegal_argument(format!(
        "{}[{}] of doc [{}]. {}: expected '{}', but it has '{}'.",
        Self::ERR_MSG,
        self.name(),
        self.doc_id,
        label,
        expected,
        given
      )));
    }
    Ok(())
  }
  pub(crate) fn update_attributes(&mut self, attrs: &HashMap<String, String>) {
    if attrs
      .iter()
      .all(|(key, value)| self.attributes.get(key) == Some(value))
    {
      return;
    }
    let attributes = Arc::make_mut(&mut self.attributes);
    for (key, value) in attrs {
      if let Some(current) = attributes.get_mut(key) {
        current.clone_from(value);
      } else {
        attributes.insert(key.clone(), value.clone());
      }
    }
  }

  pub(crate) fn set_index_options(
    &mut self,
    new_index_options: IndexOptions,
    new_omit_norms: bool,
    new_store_term_vector: bool,
  ) -> Result<()> {
    if self.index_options == IndexOptions::None {
      self.index_options = new_index_options;
      self.omit_norms = new_omit_norms;
      self.store_term_vector = new_store_term_vector;
    } else {
      self.assert_same("index options", &self.index_options, &new_index_options)?;
      self.assert_same("omit norms", &self.omit_norms, &new_omit_norms)?;
      self.assert_same(
        "store term vector",
        &self.store_term_vector,
        &new_store_term_vector,
      )?;
    }
    Ok(())
  }
  pub(crate) fn set_doc_values(
    &mut self,
    new_doc_values_type: DocValuesType,
    new_doc_values_skip_index: DocValuesSkipIndexType,
  ) -> Result<()> {
    if self.doc_values_type == DocValuesType::None {
      self.doc_values_type = new_doc_values_type;
      self.doc_values_skip_index = new_doc_values_skip_index;
    } else {
      self.assert_same(
        "doc values type",
        &self.doc_values_type,
        &new_doc_values_type,
      )?;
      self.assert_same(
        "doc values skip index type",
        &self.doc_values_skip_index,
        &new_doc_values_skip_index,
      )?;
    }
    Ok(())
  }

  pub(crate) fn set_points(
    &mut self,
    dimension_count: usize,
    index_dimension_count: usize,
    num_bytes: usize,
  ) -> Result<()> {
    if self.point_index_dimension_count == 0 {
      let dimension_count = dimension_count.try_convert()?;
      let index_dimension_count = index_dimension_count.try_convert()?;
      let num_bytes = num_bytes.try_convert()?;
      self.point_dimension_count = dimension_count;
      self.point_index_dimension_count = index_dimension_count;
      self.point_num_bytes = num_bytes;
    } else {
      self.assert_same(
        "point dimension",
        &(self.point_dimension_count as usize),
        &dimension_count,
      )?;
      self.assert_same(
        "point index dimension",
        &(self.point_index_dimension_count as usize),
        &index_dimension_count,
      )?;
      self.assert_same(
        "point num bytes",
        &(self.point_num_bytes as usize),
        &num_bytes,
      )?;
    }
    Ok(())
  }

  pub(crate) fn set_vectors(
    &mut self,
    encoding: VectorEncoding,
    similarity_function: VectorSimilarityFunction,
    dimension: usize,
  ) -> Result<()> {
    let previous = self.vectors();
    if previous.dimension == 0 {
      let vectors = self.vectors.get_or_insert_with(|| {
        Box::new(FieldVectorState {
          schema: EMPTY_VECTOR_SCHEMA,
          writer_index: None,
        })
      });
      vectors.schema = FieldVectorSchema {
        dimension,
        encoding,
        similarity: similarity_function,
      };
    } else {
      self.assert_same("vector encoding", &previous.encoding, &encoding)?;
      self.assert_same(
        "vector similarity function",
        &previous.similarity,
        &similarity_function,
      )?;
      self.assert_same("vector dimension", &previous.dimension, &dimension)?;
    }
    Ok(())
  }
  pub(crate) fn reset(&mut self, doc: i32) {
    self.doc_id = doc;
    self.omit_norms = false;
    self.store_term_vector = false;
    self.index_options = IndexOptions::None;
    self.doc_values_type = DocValuesType::None;
    self.point_dimension_count = 0;
    self.point_index_dimension_count = 0;
    self.point_num_bytes = 0;
    if let Some(vectors) = &mut self.vectors {
      vectors.schema = EMPTY_VECTOR_SCHEMA;
    }
  }

  #[inline]
  pub(crate) fn assert_same_schema(&self, fi: &FieldInfo) -> Result<()> {
    self.assert_same("index options", fi.get_index_options(), &self.index_options)?;
    self.assert_same("omit norms", &fi.omits_norms(), &self.omit_norms)?;
    self.assert_same(
      "store term vector",
      &fi.has_term_vectors(),
      &self.store_term_vector,
    )?;
    self.assert_same(
      "doc values type",
      fi.get_doc_values_type(),
      &self.doc_values_type,
    )?;
    self.assert_same(
      "doc values skip index type",
      fi.doc_values_skip_index_type(),
      &self.doc_values_skip_index,
    )?;
    let vectors = self.vectors();
    self.assert_same(
      "vector similarity function",
      fi.get_vector_similarity_function(),
      &vectors.similarity,
    )?;
    self.assert_same(
      "vector encoding",
      fi.get_vector_encoding(),
      &vectors.encoding,
    )?;
    self.assert_same(
      "vector dimension",
      &(fi.get_vector_dimension() as usize),
      &vectors.dimension,
    )?;
    self.assert_same(
      "point dimension",
      &fi.get_point_dimension_count(),
      &(self.point_dimension_count as usize),
    )?;
    self.assert_same(
      "point index dimension",
      &fi.get_point_index_dimension_count(),
      &(self.point_index_dimension_count as usize),
    )?;
    self.assert_same(
      "point num bytes",
      &fi.get_point_num_bytes(),
      &(self.point_num_bytes as usize),
    )?;
    Ok(())
  }
}

struct DocValuesLeafReaderImpl1<'a, D>
where
  D: Directory + Clone,
{
  index_chain: &'a mut IndexingChain<D>,
  base: DocValuesLeafReader,
  field_infos: &'a Arc<FieldInfos>,
  index_base: IndexReaderBase,
}
impl<'a, D> DocValuesLeafReaderImpl1<'a, D>
where
  D: Directory + Clone,
{
  fn new(index_chain: &'a mut IndexingChain<D>, field_infos: &'a Arc<FieldInfos>) -> Self {
    let base = DocValuesLeafReader;
    DocValuesLeafReaderImpl1 {
      index_chain,
      base,
      field_infos,
      index_base: IndexReaderBase::new(),
    }
  }

  fn per_field(&self, field: &str) -> Result<Option<&PerField>> {
    let Some(index) = self.index_chain.get_per_field(field) else {
      return Ok(None);
    };
    self
      .index_chain
      .per_fields
      .get(index)
      .map(Some)
      .ok_or_else(|| LuceneError::illegal_state(format!("field index {index} is out of range")))
  }
}

impl<D> Display for DocValuesLeafReaderImpl1<'_, D>
where
  D: Directory + Clone,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

impl<D> IndexReader for DocValuesLeafReaderImpl1<'_, D>
where
  D: Directory + Clone,
{
  type ContextKind = LeafReaderContextKind;

  type TermVectors = <DocValuesLeafReader as IndexReader>::TermVectors;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    self.base.term_vectors()
  }

  fn max_doc(&self) -> Result<i32> {
    self.base.max_doc()
  }

  fn num_docs(&self) -> Result<i32> {
    self.base.num_docs()
  }

  type StoredFields = <DocValuesLeafReader as IndexReader>::StoredFields;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    self.base.stored_fields()
  }

  fn do_close(&self) -> Result<()> {
    self.base.do_close()
  }

  type ReaderCacheHelper = <DocValuesLeafReader as IndexReader>::ReaderCacheHelper;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    self.base.get_reader_cache_helper()
  }

  fn doc_freq(&self, term: &Term) -> Result<i32> {
    LeafReader::doc_freq(self, term)
  }

  fn total_term_freq(&self, term: &Term) -> Result<i64> {
    LeafReader::get_total_term_freq(self, term)
  }

  fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
    LeafReader::get_sum_doc_freq(self, field)
  }

  fn get_doc_count(&self, field: &str) -> Result<i32> {
    LeafReader::get_doc_count(self, field)
  }

  fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
    LeafReader::get_sum_total_term_freq(self, field)
  }

  fn index_base(&self) -> &IndexReaderBase {
    &self.index_base
  }
}

impl<'a, D> LeafReader for DocValuesLeafReaderImpl1<'a, D>
where
  D: Directory + Clone,
{
  type CacheHelper = <DocValuesLeafReader as LeafReader>::CacheHelper;

  fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
    self.base.get_core_cache_helper()
  }

  type Terms = <DocValuesLeafReader as LeafReader>::Terms;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    self.base.terms(field)
  }

  type NumericDocValues = BufferedNumericDocValues;

  fn get_numeric_doc_values(&self, field: &str) -> Result<Option<Self::NumericDocValues>> {
    let Some(pf) = self.per_field(field)? else {
      return Ok(None);
    };
    let field_info = pf
      .schema
      .field_info()
      .ok_or_else(|| LuceneError::illegal_state("field info is missing"))?;
    if *field_info.get_doc_values_type() != DocValuesType::Numeric {
      return Ok(None);
    }
    match pf.doc_values_writer.as_ref() {
      Some(DocValuesWriterEnum::Numeric(writer)) => Ok(Option::from(writer.get_doc_values()?)),
      _ => Err(LuceneError::illegal_state(format!(
        "field=\"{}\": expected Numeric DocValuesWriter",
        pf.schema.name()
      ))),
    }
  }

  type BinaryDocValues = BufferedBinaryDocValues<DocsWithFieldSetDISI, PagedBytesDataInput>;

  fn get_binary_doc_values(&self, field: &str) -> Result<Option<Self::BinaryDocValues>> {
    let Some(pf) = self.per_field(field)? else {
      return Ok(None);
    };
    let field_info = pf
      .schema
      .field_info()
      .ok_or_else(|| LuceneError::illegal_state("field info is missing"))?;
    if *field_info.get_doc_values_type() != DocValuesType::Binary {
      return Ok(None);
    }
    match pf.doc_values_writer.as_ref() {
      Some(DocValuesWriterEnum::Binary(writer)) => Ok(Option::from(writer.get_doc_values()?)),
      _ => Err(LuceneError::illegal_state(format!(
        "field=\"{}\": expected Binary DocValuesWriter",
        pf.schema.name()
      ))),
    }
  }

  type SortedDocValues = BufferedSortedDocValues<DocsWithFieldSetDISI>;

  fn get_sorted_doc_values(&self, field: &str) -> Result<Option<Self::SortedDocValues>> {
    let Some(pf) = self.per_field(field)? else {
      return Ok(None);
    };
    let field_info = pf
      .schema
      .field_info()
      .ok_or_else(|| LuceneError::illegal_state("field info is missing"))?;
    if *field_info.get_doc_values_type() != DocValuesType::Sorted {
      return Ok(None);
    }
    match pf.doc_values_writer.as_ref() {
      Some(DocValuesWriterEnum::Sorted(writer)) => Ok(Option::from(writer.get_doc_values()?)),
      _ => Err(LuceneError::illegal_state(format!(
        "field=\"{}\": expected Sorted DocValuesWriter",
        pf.schema.name()
      ))),
    }
  }

  type SortedNumericDocValues = SortedNumericDocValuesWriterValues;

  fn get_sorted_numeric_doc_values(
    &self,
    field: &str,
  ) -> Result<Option<Self::SortedNumericDocValues>> {
    let Some(pf) = self.per_field(field)? else {
      return Ok(None);
    };
    let field_info = pf
      .schema
      .field_info()
      .ok_or_else(|| LuceneError::illegal_state("field info is missing"))?;
    if *field_info.get_doc_values_type() != DocValuesType::SortedNumeric {
      return Ok(None);
    }
    match pf.doc_values_writer.as_ref() {
      Some(DocValuesWriterEnum::SortedNumeric(writer)) => {
        Ok(Option::from(writer.get_doc_values()?))
      },
      _ => Err(LuceneError::illegal_state(format!(
        "field=\"{}\": expected SortedNumeric DocValuesWriter",
        pf.schema.name()
      ))),
    }
  }

  fn search_nearest_vectors_f32<B, K, TV>(
    &self,
    field: &str,
    target: TV,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
    TV: Into<Vec<f32>>,
  {
    let target = target.into();
    self
      .base
      .search_nearest_vectors_f32(field, target, knn_collector, accept_docs)
  }

  fn search_nearest_vectors_u8<B, K, TV>(
    &self,
    field: &str,
    target: TV,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
    TV: Into<Vec<u8>>,
  {
    let target = target.into();
    self
      .base
      .search_nearest_vectors_u8(field, target, knn_collector, accept_docs)
  }

  type SortedSetDocValues = SortedSetDocValuesWriterDocIdSetIterator;

  fn get_sorted_set_doc_values(&self, field: &str) -> Result<Option<Self::SortedSetDocValues>> {
    let Some(pf) = self.per_field(field)? else {
      return Ok(None);
    };
    let field_info = pf
      .schema
      .field_info()
      .ok_or_else(|| LuceneError::illegal_state("field info is missing"))?;
    if *field_info.get_doc_values_type() != DocValuesType::SortedSet {
      return Ok(None);
    }
    match pf.doc_values_writer.as_ref() {
      Some(DocValuesWriterEnum::SortedSet(writer)) => Ok(Option::from(writer.get_doc_values()?)),
      _ => Err(LuceneError::illegal_state(format!(
        "field=\"{}\": expected SortedSet DocValuesWriter",
        pf.schema.name()
      ))),
    }
  }

  type NormNumericDocValues = <DocValuesLeafReader as LeafReader>::NumericDocValues;

  fn get_norm_values(&self, field: &str) -> Result<Option<Self::NormNumericDocValues>> {
    self.base.get_norm_values(field)
  }

  type DocValuesSkipper = <DocValuesLeafReader as LeafReader>::DocValuesSkipper;

  fn get_doc_values_skipper(&self, field: &str) -> Result<Option<Self::DocValuesSkipper>> {
    self.base.get_doc_values_skipper(field)
  }

  type FloatVectorValues = <DocValuesLeafReader as LeafReader>::FloatVectorValues;

  fn get_float_vector_values(&self, field: &str) -> Result<Option<Self::FloatVectorValues>> {
    self.base.get_float_vector_values(field)
  }

  type ByteVectorValues = <DocValuesLeafReader as LeafReader>::ByteVectorValues;

  fn get_byte_vector_values(&self, field: &str) -> Result<Option<Self::ByteVectorValues>> {
    self.base.get_byte_vector_values(field)
  }

  fn get_field_infos(&self) -> Result<Arc<FieldInfos>> {
    Ok(Arc::clone(self.field_infos))
  }

  type Bits = <DocValuesLeafReader as LeafReader>::Bits;

  fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
    self.base.get_live_docs()
  }

  type PointValues = <DocValuesLeafReader as LeafReader>::PointValues;

  fn get_point_values(&self, _field: &str) -> Result<Option<Self::PointValues>> {
    self.base.get_point_values(_field)
  }

  fn check_integrity(&self) -> Result<()> {
    self.base.check_integrity()
  }

  fn get_metadata(&self) -> Result<&LeafMetaData> {
    self.base.get_metadata()
  }
}
struct DocValuesLeafReaderImpl2<'a, SFB>
where
  SFB: SortFiledBase,
{
  field_to_validate: &'a str,
  dv_type: &'a DocValuesType,
  base: DocValuesLeafReader,
  sort_field: &'a SFB,
  index_base: IndexReaderBase,
}
impl<'a, SFB> DocValuesLeafReaderImpl2<'a, SFB>
where
  SFB: SortFiledBase,
{
  fn new(field_to_validate: &'a str, dv_type: &'a DocValuesType, sort_field: &'a SFB) -> Self {
    let base = DocValuesLeafReader;
    DocValuesLeafReaderImpl2 {
      field_to_validate,
      dv_type,
      base,
      sort_field,
      index_base: IndexReaderBase::new(),
    }
  }
}

impl<SFB> Display for DocValuesLeafReaderImpl2<'_, SFB>
where
  SFB: SortFiledBase,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

impl<SFB> IndexReader for DocValuesLeafReaderImpl2<'_, SFB>
where
  SFB: SortFiledBase,
{
  type ContextKind = LeafReaderContextKind;

  type TermVectors = <DocValuesLeafReader as IndexReader>::TermVectors;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    self.base.term_vectors()
  }
  fn max_doc(&self) -> Result<i32> {
    self.base.max_doc()
  }

  fn num_docs(&self) -> Result<i32> {
    self.base.num_docs()
  }

  type StoredFields = <DocValuesLeafReader as IndexReader>::StoredFields;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    self.base.stored_fields()
  }
  fn do_close(&self) -> Result<()> {
    self.base.do_close()
  }

  type ReaderCacheHelper = <DocValuesLeafReader as IndexReader>::ReaderCacheHelper;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    self.base.get_reader_cache_helper()
  }

  fn doc_freq(&self, term: &Term) -> Result<i32> {
    LeafReader::doc_freq(self, term)
  }

  fn total_term_freq(&self, term: &Term) -> Result<i64> {
    LeafReader::get_total_term_freq(self, term)
  }

  fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
    LeafReader::get_sum_doc_freq(self, field)
  }

  fn get_doc_count(&self, field: &str) -> Result<i32> {
    LeafReader::get_doc_count(self, field)
  }

  fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
    LeafReader::get_sum_total_term_freq(self, field)
  }

  fn index_base(&self) -> &IndexReaderBase {
    &self.index_base
  }
}

impl<SFB> LeafReader for DocValuesLeafReaderImpl2<'_, SFB>
where
  SFB: SortFiledBase,
{
  type CacheHelper = <DocValuesLeafReader as LeafReader>::CacheHelper;

  fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
    self.base.get_core_cache_helper()
  }
  type Terms = <DocValuesLeafReader as LeafReader>::Terms;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    self.base.terms(field)
  }

  type NumericDocValues = EmptyNumeric;

  fn get_numeric_doc_values(&self, field: &str) -> Result<Option<Self::NumericDocValues>> {
    if field == self.field_to_validate && *self.dv_type != DocValuesType::Numeric {
      return Err(LuceneError::illegal_argument(format!(
        "SortField {} expected field [{}] to be NUMERIC but it is [{}]",
        self.sort_field, field, self.dv_type
      )));
    }
    Ok(Some(DocValues::empty_numeric()))
  }

  type BinaryDocValues = EmptyBinary;

  fn get_binary_doc_values(&self, field: &str) -> Result<Option<Self::BinaryDocValues>> {
    if field == self.field_to_validate && *self.dv_type != DocValuesType::Binary {
      return Err(LuceneError::illegal_argument(format!(
        "SortField {} expected field [{}] to be BINARY but it is [{}]",
        self.sort_field, field, self.dv_type
      )));
    }
    Ok(Some(DocValues::empty_binary()))
  }

  type SortedDocValues = EmptySorted;

  fn get_sorted_doc_values(&self, field: &str) -> Result<Option<Self::SortedDocValues>> {
    if field == self.field_to_validate && *self.dv_type != DocValuesType::Sorted {
      return Err(LuceneError::illegal_argument(format!(
        "SortField {} expected field [{}] to be SORTED but it is [{}]",
        self.sort_field, field, self.dv_type
      )));
    }
    Ok(Some(DocValues::empty_sorted()))
  }

  type SortedNumericDocValues = SingletonSortedNumericDocValues<EmptyNumeric>;

  fn get_sorted_numeric_doc_values(
    &self,
    field: &str,
  ) -> Result<Option<Self::SortedNumericDocValues>> {
    if field == self.field_to_validate && *self.dv_type != DocValuesType::SortedNumeric {
      return Err(LuceneError::illegal_argument(format!(
        "SortField {} expected field [{}] to be SORTED_NUMERIC but it is [{}]",
        self.sort_field, field, self.dv_type
      )));
    }
    Ok(Some(DocValues::empty_sorted_numeric()?))
  }

  type SortedSetDocValues = EmptySortedSet;

  fn get_sorted_set_doc_values(&self, field: &str) -> Result<Option<Self::SortedSetDocValues>> {
    if field == self.field_to_validate && *self.dv_type != DocValuesType::SortedSet {
      return Err(LuceneError::illegal_argument(format!(
        "SortField {} expected field [{}] to be SORTED_SET but it is [{}]",
        self.sort_field, field, self.dv_type
      )));
    }
    Ok(Some(DocValues::empty_sorted_set()?))
  }

  type NormNumericDocValues = <DocValuesLeafReader as LeafReader>::NumericDocValues;

  fn get_norm_values(&self, field: &str) -> Result<Option<Self::NormNumericDocValues>> {
    self.base.get_norm_values(field)
  }

  type DocValuesSkipper = <DocValuesLeafReader as LeafReader>::DocValuesSkipper;

  fn get_doc_values_skipper(&self, field: &str) -> Result<Option<Self::DocValuesSkipper>> {
    self.base.get_doc_values_skipper(field)
  }

  fn search_nearest_vectors_f32<B, K, TV>(
    &self,
    field: &str,
    target: TV,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
    TV: Into<Vec<f32>>,
  {
    let target = target.into();
    self
      .base
      .search_nearest_vectors_f32(field, target, knn_collector, accept_docs)
  }

  fn search_nearest_vectors_u8<B, K, TV>(
    &self,
    field: &str,
    target: TV,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
    TV: Into<Vec<u8>>,
  {
    let target = target.into();
    self
      .base
      .search_nearest_vectors_u8(field, target, knn_collector, accept_docs)
  }

  type FloatVectorValues = <DocValuesLeafReader as LeafReader>::FloatVectorValues;

  fn get_float_vector_values(&self, field: &str) -> Result<Option<Self::FloatVectorValues>> {
    self.base.get_float_vector_values(field)
  }

  type ByteVectorValues = <DocValuesLeafReader as LeafReader>::ByteVectorValues;

  fn get_byte_vector_values(&self, field: &str) -> Result<Option<Self::ByteVectorValues>> {
    self.base.get_byte_vector_values(field)
  }

  fn get_field_infos(&self) -> Result<Arc<FieldInfos>> {
    self.base.get_field_infos()
  }

  type Bits = <DocValuesLeafReader as LeafReader>::Bits;

  fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
    self.base.get_live_docs()
  }

  type PointValues = <DocValuesLeafReader as LeafReader>::PointValues;

  fn get_point_values(&self, _field: &str) -> Result<Option<Self::PointValues>> {
    self.base.get_point_values(_field)
  }
  fn check_integrity(&self) -> Result<()> {
    self.base.check_integrity()
  }

  fn get_metadata(&self) -> Result<&LeafMetaData> {
    self.base.get_metadata()
  }
}

struct DocComparatorImpl<'a, DC> {
  parents: &'a SparseFixedBitSetBitSet,
  doc_comparator: DC,
}
impl<'a, DC> DocComparatorImpl<'a, DC> {
  fn new(parents: &'a SparseFixedBitSetBitSet, doc_comparator: DC) -> Self {
    DocComparatorImpl {
      parents,
      doc_comparator,
    }
  }
}
impl<DC> DocComparator for DocComparatorImpl<'_, DC>
where
  DC: DocComparator,
{
  fn compare(&self, doc_id1: usize, doc_id2: usize) -> i32 {
    let doc_id1 = self.parents.next_set_bit(doc_id1);
    let doc_id2 = self.parents.next_set_bit(doc_id2);
    self.doc_comparator.compare(doc_id1, doc_id2)
  }
}

pub struct ReservedField<T> {
  delegate: T,
}
impl<T> ReservedField<T> {
  pub(crate) fn new(delegate: T) -> Self {
    ReservedField { delegate }
  }
}

#[cfg(test)]
impl<T> Clone for ReservedField<T>
where
  T: IndexableField + Clone,
{
  fn clone(&self) -> Self {
    Self {
      delegate: self.delegate.clone(),
    }
  }
}

impl<T> Display for ReservedField<T>
where
  T: IndexableField,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    self.delegate.fmt(f)
  }
}

impl<T> IndexableField for ReservedField<T>
where
  T: IndexableField,
{
  fn name(&self) -> &str {
    self.delegate.name()
  }

  type FieldType<'a>
    = <T as IndexableField>::FieldType<'a>
  where
    Self: 'a;

  fn field_type(&self) -> Self::FieldType<'_> {
    self.delegate.field_type()
  }
  fn token_stream<'a, A>(
    &'a mut self,
    analyzer: &'a A,
    reuse_token_stream: &'a mut Option<ReusedIndexingTokenStream>,
  ) -> Result<IndexingTokenStream<'a>>
  where
    A: Analyzer,
  {
    self.delegate.token_stream(analyzer, reuse_token_stream)
  }

  fn binary_value(&self) -> Result<Option<BinaryValueEnum<'_>>> {
    self.delegate.binary_value()
  }

  fn take_binary_value(&mut self) -> Result<Option<BytesRef<Vec<u8>>>> {
    self.delegate.take_binary_value()
  }

  fn string_value(&self) -> Result<Option<Cow<'_, str>>> {
    self.delegate.string_value()
  }

  fn take_string_value(&mut self) -> Result<Option<String>> {
    self.delegate.take_string_value()
  }

  fn get_char_sequence_value(&self) -> Result<Option<Cow<'_, str>>> {
    self.delegate.get_char_sequence_value()
  }

  fn take_reader_value(&mut self) -> Result<Option<ReaderEnum>> {
    self.delegate.take_reader_value()
  }

  fn numeric_value(&self) -> Result<Option<Number>> {
    self.delegate.numeric_value()
  }

  type StoredValue<'a>
    = T::StoredValue<'a>
  where
    Self: 'a;

  fn stored_value(&self) -> Result<Option<Self::StoredValue<'_>>> {
    self.delegate.stored_value()
  }
  fn invertable_type(&self) -> &InvertableType {
    self.delegate.invertable_type()
  }

  fn is_reserved(&self) -> bool {
    true
  }
}
