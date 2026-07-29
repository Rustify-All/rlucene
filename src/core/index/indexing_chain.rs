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
use crate::core::analysis::token_stream::TokenStream;
use crate::core::codecs::Codec;
use crate::core::codecs::doc_values_format::DocValuesFormat;
use crate::core::codecs::field_infos_format::FieldInfosFormat;
use crate::core::codecs::norms_format::NormsFormat;
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::codecs::points_format::PointsFormat;
use crate::core::codecs::points_writer::PointsWriter;
use crate::core::document::fields::Fields;
use crate::core::document::invertable_field::InvertableType;
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
use std::borrow::Cow;

use crate::core::analysis::reader::ReaderEnum;
use crate::core::codecs::knn_vectors_format::KnnVectorsFormat;
use crate::core::codecs::knn_vectors_writer::KnnVectorsWriter;
use crate::core::document::field::FieldDataEnum;
use crate::core::index::index_writer::{MAX_POSITION, MAX_STORED_STRING_LENGTH, MAX_TERM_LENGTH};
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
use crate::core::index::singleton_sorted_set_doc_values::SingletonSortedSetDocValues;
use crate::core::index::sorted_doc_values_writer::{
  BufferedSortedDocValues, SortedDocValuesWriter,
};
use crate::core::index::sorted_numeric_doc_values::SortedNumericDocValuesEnum2;
use crate::core::index::sorted_numeric_doc_values_writer::{
  BufferedSortedNumericDocValues, SortedNumericDocValuesWriter,
};
use crate::core::index::sorted_set_doc_values_writer::{
  BufferedSortedSetDocValues, SortedSetDocValuesEnum2, SortedSetDocValuesWriter,
};
use crate::core::index::sorter::{DocMap, DocMapImpl, Sorter};
use crate::core::index::sorting_stored_fields_consumer::SortingStoredFieldsConsumer;
use crate::core::index::sorting_term_vectors_consumer::SortingTermVectorsConsumer;
use crate::core::index::stored_fields_consumer::StoredFieldsConsumer;
use crate::core::index::term::Term;
use crate::core::index::term_vectors_consumer::{PerFieldMeta, TermVectorsConsumer};
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::index::vector_values_consumer::VectorValuesConsumer;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::sort::Sort;
use crate::core::search::sort_field::SortFiledBase;
use crate::core::store::IOContext;
use crate::core::store::directory::Directory;
use crate::core::util::accountable::Accountable;
use crate::core::util::allocator_byte::DirectTrackingAllocatorByte;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::attribute_source::{AttributeSource, EmptyAttributeSource};
use crate::core::util::bit_set::of;
use crate::core::util::bit_set::{BitSet, SparseFixedBitSetBitSet};
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bits::Bits;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
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
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use std::vec;

macro_rules! catch_aborting_exception {
  ($has_hit_aborting_exception:expr, $aborting_exception:expr, $panic_message:expr, $operation:expr) => {{
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| $operation)) {
      Ok(Ok(value)) => Ok(value),
      Ok(Err(err)) => {
        *$has_hit_aborting_exception = true;
        let _ = $aborting_exception.set(err.clone());
        Err(err)
      },
      Err(e) => {
        let tragedy = LuceneError::tragedy_from_panic($panic_message, e.as_ref());
        *$has_hit_aborting_exception = true;
        let _ = $aborting_exception.set(tragedy.clone());
        Err(tragedy)
      },
    }
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
  field_hash: Vec<i32>,
  hash_mask: usize,
  total_field_count: usize,
  next_field_gen: i64,
  // Holds fields seen in each document
  fields: Vec<usize>,
  per_fields: Vec<PerField>,
  doc_fields: Vec<usize>,
  info_stream: InfoStreamMT,
  index_created_version_major: i32,
  has_hit_aborting_exception: bool,
  context: IndexContext,
}
pub(crate) struct IndexContext {
  pub(crate) term_vectors_int_pool: IntBlockPool,
  pub(crate) freq_prox_term_int_pool: IntBlockPool,
  pub(crate) byte_pool: ByteBlockPool,
}
impl IndexContext {
  pub(crate) fn reset(&mut self) {
    self.term_vectors_int_pool.reset(false, false);
    self.freq_prox_term_int_pool.reset(false, false);
    self.byte_pool.reset(false, false);
  }
}

impl<D> IndexingChain<D>
where
  D: Directory + Clone,
{
  fn on_aborting_exception(
    &mut self,
    aborting_exception: &OnceLock<LuceneError>,
    throwable: LuceneError,
  ) {
    self.has_hit_aborting_exception = true;
    let _ = aborting_exception.set(throwable);
  }

  pub(crate) fn new<D1>(
    index_created_version_major: i32,
    segment_info: &SegmentInfo<D1>,
    directory: D,
    index_writer_config: &impl LiveIndexWriterConfig,
  ) -> Result<Self>
  where
    D1: Directory,
    D: Clone,
  {
    let bytes_used = Arc::new(AtomicCounter::new());
    let codec = index_writer_config.get_codec().clone();
    let (stored_fields_consumer, term_vectors_writer) = if segment_info.get_index_sort().is_none() {
      (
        StoredFieldsConsumer::new(codec.clone(), directory.clone(), None),
        TermVectorsConsumer::new(codec.clone(), directory.clone(), None),
      )
    } else {
      let stored_fields_consumer_sub =
        SortingStoredFieldsConsumer::new(codec.clone(), directory.clone())?;
      let term_vector_consumer_sub =
        SortingTermVectorsConsumer::new(codec.clone(), directory.clone())?;
      (
        StoredFieldsConsumer::new(
          codec.clone(),
          directory.clone(),
          Some(stored_fields_consumer_sub),
        ),
        TermVectorsConsumer::new(
          codec.clone(),
          directory.clone(),
          Some(term_vector_consumer_sub),
        ),
      )
    };

    let terms_hash = FreqProxTermsWriter::new(bytes_used.clone(), term_vectors_writer);
    let doc_values_byte_pool = ByteBlockPool::new(DirectTrackingAllocatorByte::allocator_enum(
      bytes_used.clone(),
    ));
    let info_stream = index_writer_config.get_info_stream().clone();
    let term_vectors_int_pool =
      IntBlockPool::with_allocator(IntBlockAllocator::allocator_enum(bytes_used.clone()));
    let freq_prox_term_int_pool =
      IntBlockPool::with_allocator(IntBlockAllocator::allocator_enum(bytes_used.clone()));
    let allocator = DirectTrackingAllocatorByte::allocator_enum(bytes_used.clone());
    let byte_pool = ByteBlockPool::new(allocator);
    let context = IndexContext {
      term_vectors_int_pool,
      freq_prox_term_int_pool,
      byte_pool,
    };
    let vector_values_consumer =
      VectorValuesConsumer::new(codec.clone(), directory.clone(), info_stream.clone());
    Ok(IndexingChain {
      bytes_used,
      terms_hash,
      doc_values_byte_pool,
      stored_fields_consumer,
      vector_values_consumer,
      field_hash: vec![-1; 2],
      hash_mask: 1,
      total_field_count: 0,
      next_field_gen: 0,
      fields: vec![0; 1],
      per_fields: vec![],
      doc_fields: vec![0; 2],
      info_stream,
      index_created_version_major,
      has_hit_aborting_exception: false,
      context,
    })
  }
  fn get_doc_values_leaf_reader<'a>(
    index_chain: &'a mut IndexingChain<D>,
    field_info: &'a mut Builder,
  ) -> DocValuesLeafReaderImpl1<'a, D> {
    DocValuesLeafReaderImpl1::new(index_chain, field_info)
  }
  pub(crate) fn maybe_sort_segment<D1>(
    &mut self,
    state: &SegmentWriteState<D>,
    segment_info: &SegmentInfo<D1>,
    field_info: &mut Builder,
  ) -> Result<Option<Arc<DocMapImpl>>>
  where
    D: Directory,
    D1: Directory,
  {
    let index_created_version_major = self.index_created_version_major;
    let index_sort = segment_info.get_index_sort();
    if index_sort.is_none() {
      return Ok(None);
    }

    let doc_values_reader = Self::get_doc_values_leaf_reader(self, field_info);
    let max_doc = segment_info.max_doc()?;
    let has_blocks = segment_info.get_has_blocks();
    let parent_field = state.field_infos.get_parent_field();

    let parent_bit_set = if has_blocks {
      if let Some(parent_field) = parent_field {
        match doc_values_reader.get_numeric_doc_values(parent_field)? {
          Some(ref mut reader_values) => Some(Rc::new(of(reader_values, max_doc.try_convert()?)?)),
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
    let mut comparators = Vec::new();
    for sort_field in &index_sort.as_ref().unwrap().fields {
      let sorter = sort_field.get_index_sorter()?.ok_or_else(|| {
        LuceneError::unsupported_operation(format!(
          "Cannot sort index using sort field {sort_field}"
        ))
      })?;
      let doc_comparator = sorter.get_doc_comparator(&doc_values_reader, max_doc)?;
      let v = match &parent_bit_set {
        Some(parent_bit_set) => DocComparatorEnum2::A(DocComparatorImpl::new(
          parent_bit_set.clone(),
          doc_comparator,
        )),
        None => DocComparatorEnum2::B(doc_comparator),
      };
      comparators.push(v);
    }
    // returns None if the documents are already sorted
    match Sorter::sort(max_doc, comparators)? {
      Some(doc_map) => Ok(Some(Arc::new(doc_map))),
      None => Ok(None),
    }
  }
  pub(crate) fn flush<D1>(
    &mut self,
    state: &mut SegmentWriteState<D>,
    segment_info: &mut SegmentInfo<D1>,
    seg_updates: Option<&mut BufferedUpdates>,
    index_writer_config: &impl LiveIndexWriterConfig,
    field_info: &mut Builder,
  ) -> Result<Option<Arc<DocMapImpl>>>
  where
    D1: Directory,
  {
    // NOTE: caller (DocumentsWriterPerThread) handles
    // aborting on any error from this method
    let sort_map = self.maybe_sort_segment(state, segment_info, field_info)?;
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
      .flush(state, sort_map.as_ref(), segment_info, state.directory)?;
    if self.info_stream.is_enabled("IW") {
      self.info_stream.message(
        "IW",
        &format!("{} ms to finish stored fields", t0.elapsed().as_millis()),
      )?;
    }

    let mut fields_to_flush = HashMap::new();
    for &idx in &self.field_hash {
      let mut fp_idx = idx;
      while fp_idx >= 0 {
        let pf = &mut self.per_fields[fp_idx as usize];
        if pf.invert_state.is_some() {
          fields_to_flush.insert(
            pf.field_info.as_ref().unwrap().name.clone(),
            pf.terms_hash_per_field.take().unwrap(),
          );
        }
        fp_idx = pf.next;
      }
    }
    let io_context = IOContext::default_io_context()?;
    let read_state = SegmentReadState::with_suffix(
      state.directory,
      state.field_infos.clone(),
      &io_context,
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
    let result = (|| -> Result<()> {
      let norms_merge_instance = match norms.as_ref() {
        // Use the merge instance in order to reuse the same IndexInput for all terms
        Some(norms) => norms.get_merge_instance()?,
        None => None,
      };
      let norms_for_flush = norms_merge_instance.as_ref().or(norms.as_ref());

      // flush postings + vectors
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
    })();
    let close_result = match norms.as_mut() {
      Some(norms) => norms.close(),
      None => Ok(()),
    };
    IOUtils::use_or_suppress_result(result, close_result)?;
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
      &IOContext::default_io_context()?,
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
  pub fn write_points<DM, D1>(
    &mut self,
    state: &SegmentWriteState<D>,
    sort_map: Option<&DM>,
    index_writer_config: &impl LiveIndexWriterConfig,
    info: &SegmentInfo<D1>,
  ) -> Result<()>
  where
    DM: DocMap + Clone,
    D1: Directory,
  {
    let mut points_writer = None;
    debug_assert!(self.field_hash.len() <= i32::MAX as usize);

    let body_result = (|| -> Result<()> {
      for bucket in 0..self.field_hash.len() {
        let mut per_field_index = self.field_hash[bucket];
        while per_field_index >= 0 {
          let per_field = &mut self.per_fields[per_field_index as usize];
          if let Some(point_values_writer) = per_field.point_values_writer.as_mut() {
            let field_info = per_field.field_info.as_ref().unwrap();
            // We could have initialized pointValuesWriter, but failed to write even a single doc
            if field_info.get_point_dimension_count() > 0 {
              if points_writer.is_none() {
                // lazy init
                let fmt = index_writer_config.get_codec().points_format();
                points_writer = Some(fmt.fields_writer(state, info)?);
              }
              point_values_writer.flush(
                state.directory,
                sort_map,
                points_writer.as_mut().unwrap(),
                info,
              )?;
            }
          }

          per_field.point_values_writer = None;
          per_field_index = per_field.next;
        }
      }

      if let Some(w) = points_writer.as_mut() {
        w.finish()?;
      }
      Ok(())
    })();
    match body_result {
      Ok(()) => {
        if let Some(mut w) = points_writer {
          w.close()?;
        }
      },
      Err(err) => {
        if let Some(mut w) = points_writer {
          IOUtils::close_resources_while_handling_error(&mut w)?;
        }
        return Err(err);
      },
    }
    Ok(())
  }
  // Finishes all doc values writers.
  pub(crate) fn finish_doc_values_writer(&mut self) -> Result<()> {
    let pool = Arc::new(std::mem::take(&mut self.doc_values_byte_pool));
    let mut per_field_index;
    for i in 0..self.field_hash.len() {
      per_field_index = self.field_hash[i];
      while per_field_index >= 0 {
        let per_field = &mut self.per_fields[per_field_index as usize];
        if let Some(ref mut writer) = per_field.doc_values_writer {
          writer.finish(pool.clone())?;
        }
        per_field_index = per_field.next;
      }
    }
    Ok(())
  }

  /// Writes all buffered doc values.
  fn write_doc_values<DM, D1>(
    &mut self,
    state: &SegmentWriteState<D>,
    sort_map: Option<&DM>,
    segment_info: &SegmentInfo<D1>,
    index_writer_config: &impl LiveIndexWriterConfig,
  ) -> Result<()>
  where
    DM: DocMap,
    D1: Directory,
  {
    let mut dv_consumer = None;

    let body_result: Result<()> = (|| {
      // iterate hash buckets
      let mut per_field_index;
      debug_assert!(self.field_hash.len() <= i32::MAX as usize);
      for bucket in 0..self.field_hash.len() {
        per_field_index = self.field_hash[bucket];
        while per_field_index >= 0 {
          let per_field = &mut self.per_fields[per_field_index as usize];
          if let Some(ref mut writer) = per_field.doc_values_writer {
            let field_info = per_field.field_info.as_ref().unwrap();
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
          } else if let Some(field_info) = &per_field.field_info
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
      Ok(())
    })();

    let has_dv_consumer = dv_consumer.is_some();
    match body_result {
      Ok(()) => {
        if let Some(mut consumer) = dv_consumer {
          consumer.close()?;
        }
      },
      Err(err) => {
        if let Some(mut consumer) = dv_consumer {
          IOUtils::close_resources_while_handling_error(&mut consumer)?;
        }
        return Err(err);
      },
    }

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

  fn write_norms<DM, D1>(
    &mut self,
    state: &SegmentWriteState<D>,
    sort_map: Option<&DM>,
    segment_info: &SegmentInfo<D1>,
    index_writer_config: &impl LiveIndexWriterConfig,
  ) -> Result<()>
  where
    DM: DocMap,
    D1: Directory,
  {
    if !state.field_infos.has_norms() {
      return Ok(());
    }

    let mut norms_consumer = {
      let norm_format = index_writer_config.get_codec().norms_format();
      norm_format.norms_consumer(state, segment_info)?
    };

    let body_result = (|| -> Result<()> {
      let max_doc = segment_info.max_doc()?;
      for fi in state.field_infos.iter() {
        let per_field_index = self.get_per_field(&fi.name);
        debug_assert!(per_field_index.is_some());
        // we must check the final value of omitNorms for the fieldinfo: it could have
        // changed for this field since the first time we added it.
        if !fi.omits_norms() && *fi.get_index_options() != IndexOptions::None {
          let per_field = &mut self.per_fields[per_field_index.unwrap()];
          let norms = per_field.norms.as_mut().unwrap();
          norms.finish(max_doc);
          norms.flush(sort_map, &mut norms_consumer, segment_info)?;
        }
      }
      Ok(())
    })();
    match body_result {
      Ok(()) => norms_consumer.close()?,
      Err(err) => {
        IOUtils::close_resources_while_handling_error(&mut norms_consumer)?;
        return Err(err);
      },
    }
    Ok(())
  }

  pub(crate) fn abort(&mut self) -> Result<()> {
    self.context.reset();
    let result = (|| -> Result<()> {
      self.stored_fields_consumer.abort()?;
      self.vector_values_consumer.abort();
      Ok(())
    })();
    let close_result = self.terms_hash.abort();
    self.field_hash.fill(-1);
    IOUtils::use_or_suppress_result(result, close_result)
  }

  fn rehash(&mut self) {
    let new_hash_size = self.field_hash.len() * 2;
    debug_assert!(new_hash_size > self.field_hash.len());

    let mut new_hash_array = vec![-1; new_hash_size];
    let new_hash_mask = new_hash_size - 1;
    for &idx in &self.field_hash {
      let mut fp_idx = idx;
      while fp_idx >= 0 {
        let fp0 = &mut self.per_fields[fp_idx as usize];
        let next_fp0 = fp0.next;
        let hash_pos2 = CoreHelper::calculate_hash(&fp0.field_name) & new_hash_mask as u64;
        let idx = new_hash_array[hash_pos2 as usize];
        if idx < 0 {
          fp0.next = -1;
        } else {
          fp0.next = idx;
        };
        new_hash_array[hash_pos2 as usize] = fp_idx;
        fp_idx = next_fp0;
      }
    }
    self.field_hash = new_hash_array;
    self.hash_mask = new_hash_mask;
  }

  /// Calls `start_document` on the stored fields consumer, aborting the segment on error.
  pub(crate) fn start_stored_fields<D1>(
    &mut self,
    doc_id: i32,
    info: &mut SegmentInfo<D1>,
    aborting_exception: &OnceLock<LuceneError>,
  ) -> Result<()>
  where
    D1: Directory,
  {
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
    aborting_exception: &OnceLock<LuceneError>,
  ) -> Result<()> {
    catch_aborting_exception!(
      &mut self.has_hit_aborting_exception,
      aborting_exception,
      "panic while finishing stored fields",
      self.stored_fields_consumer.finish_document()
    )
  }
  pub(crate) fn process_document<DF, D1>(
    &mut self,
    doc_id: i32,
    document: DF,
    info: &mut SegmentInfo<D1>,
    field_infos: &mut Builder,
    index_writer_config: &impl LiveIndexWriterConfig,
    aborting_exception_consumer: &OnceLock<LuceneError>,
  ) -> Result<()>
  where
    DF: IntoIterator<Item = Fields>,
    D1: Directory,
  {
    // number of unique fields by names (collapses multiple field instances by the same name)
    let mut field_count = 0;
    // number of unique fields indexed with postings
    let mut indexed_field_count = 0;
    let field_gen = self.next_field_gen;
    self.next_field_gen += 1;
    let mut doc_field_idx = 0;
    // NOTE: we need two passes here, in case there are
    // multi-valued fields, because we must process all
    // instances of a given field at once, since the
    // analyzer is free to reuse TokenStream across fields
    // (i.e., we cannot have more than one TokenStream
    // running "at once"):
    self.terms_hash.start_document()?;
    self.start_stored_fields(doc_id, info, aborting_exception_consumer)?;

    let mut document: Vec<Fields> = document.into_iter().collect();
    // 1st pass over doc fields – verify that doc schema matches the index schema
    // build schema for each unique doc field

    let result = (|| {
      for field in &document {
        let field_type = field.field_type();
        let is_reserved = field.is_reserved();
        let pf_idx = self.get_or_add_per_field(field, is_reserved)?;
        {
          let pf = &mut self.per_fields[pf_idx];
          if pf.reserved != is_reserved {
            return Err(LuceneError::illegal_argument(format!(
              "\"{}\" is a reserved field and should not be added to any document",
              field.name()
            )));
          }

          if pf.field_gen != field_gen {
            // first time we see this field in this document
            self.fields[field_count] = pf_idx;
            field_count += 1;
            pf.field_gen = field_gen;
            pf.reset(doc_id);
          }
        }
        if doc_field_idx >= self.doc_fields.len() {
          self.oversize_doc_fields()?;
        }
        self.doc_fields[doc_field_idx] = pf_idx;
        doc_field_idx += 1;
        let pf = &mut self.per_fields[pf_idx];
        Self::update_doc_field_schema(field.name(), &mut pf.schema, &field_type)?;
      }

      // For each field, if it's the first time we see this field in this segment,
      // initialize its FieldInfo.
      // If we have already seen this field, verify that its schema
      // within the current doc matches its schema in the index.
      for i in 0..field_count {
        let idx = self.fields[i];
        let pf = &mut self.per_fields[idx];
        if let Some(field_info) = pf.field_info.as_ref() {
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

      for field in &mut document {
        let per_field_idx = self.doc_fields[doc_field_idx];
        if self.process_field(
          doc_id,
          field,
          per_field_idx,
          index_writer_config,
          aborting_exception_consumer,
        )? {
          self.fields[indexed_field_count] = self.doc_fields[doc_field_idx];
          indexed_field_count += 1;
        }
        doc_field_idx += 1;
      }
      Ok(())
    })();
    if !self.has_hit_aborting_exception {
      // Finish each indexed field name seen in the document:
      for i in 0..indexed_field_count {
        let idx = self.fields[i];
        let pf = &mut self.per_fields[idx];
        debug_assert!(pf.field_info.is_some());
        pf.finish(
          doc_id,
          &mut self.terms_hash.next_terms_hash,
          index_writer_config.get_similarity(),
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
          &mut self.context.byte_pool,
        )
      )?;
    }
    result
  }
  fn oversize_doc_fields(&mut self) -> Result<()> {
    let required = self.doc_fields.len() + 1;
    ArrayUtil::grow_with_len(&mut self.doc_fields, required)?;
    Ok(())
  }
  pub(crate) fn initialize_field_info<D1>(
    &mut self,
    per_field_index: usize,
    field_infos: &mut Builder,
    index_writer_config: &impl LiveIndexWriterConfig,
    segment_info: &SegmentInfo<D1>,
    aborting_exception: &OnceLock<LuceneError>,
  ) -> Result<()>
  where
    D1: Directory,
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
    if let Some(index_sort) = &index_writer_config.get_index_sort()
      && s.doc_values_type != DocValuesType::None
    {
      Self::validate_index_sort_dv_type(index_sort, &pf.field_name, &s.doc_values_type)?;
    }
    if s.vector_dimension != 0 {
      let max_dim = index_writer_config
        .get_codec()
        .knn_vectors_format()?
        .get_max_dimensions(&pf.field_name)?;
      Self::validate_max_vector_dimension(&pf.field_name, s.vector_dimension, max_dim)?;
    }
    let soft_deletes_field = field_infos.is_soft_deletes_field_name(&pf.field_name);
    let is_parent_field = field_infos.is_parent_field_name(&pf.field_name);
    let field_info = FieldInfo::new(
      pf.field_name.clone(),
      -1,
      s.store_term_vector,
      s.omit_norms,
      false, // storePayloads is set up during indexing, if payloads were seen
      s.index_options,
      s.doc_values_type,
      s.doc_values_skip_index,
      -1,
      std::mem::take(&mut s.attributes),
      s.point_dimension_count,
      s.point_index_dimension_count,
      s.point_num_bytes,
      s.vector_dimension as i32,
      s.vector_encoding,
      s.vector_similarity_function,
      soft_deletes_field,
      is_parent_field,
    )?;

    let fi = field_infos.add(Arc::new(field_info))?;
    pf.set_field_info(fi.clone());

    if *fi.get_index_options() != IndexOptions::None {
      pf.set_invert_state(&mut self.terms_hash, self.bytes_used.clone())?;
    }

    match fi.get_doc_values_type() {
      DocValuesType::None => {},
      DocValuesType::Numeric => {
        pf.doc_values_writer = Some(DocValuesWriterEnum::Numeric(NumericDocValuesWriter::new(
          fi.clone(),
          self.bytes_used.clone(),
        )?));
      },
      DocValuesType::Binary => {
        pf.doc_values_writer = Some(DocValuesWriterEnum::Binary(BinaryDocValuesWriter::new(
          fi.clone(),
          self.bytes_used.clone(),
        )?));
      },
      DocValuesType::Sorted => {
        pf.doc_values_writer = Some(DocValuesWriterEnum::Sorted(SortedDocValuesWriter::new(
          fi.clone(),
          self.bytes_used.clone(),
        )?));
      },
      DocValuesType::SortedNumeric => {
        pf.doc_values_writer = Some(DocValuesWriterEnum::SortedNumeric(
          SortedNumericDocValuesWriter::new(fi.clone(), self.bytes_used.clone())?,
        ));
      },
      DocValuesType::SortedSet => {
        pf.doc_values_writer = Some(DocValuesWriterEnum::SortedSet(
          SortedSetDocValuesWriter::new(fi.clone(), self.bytes_used.clone())?,
        ));
      },
    }

    if fi.get_point_dimension_count() != 0 {
      pf.point_values_writer = Some(PointValuesWriter::new(self.bytes_used.clone(), fi.clone())?);
    }

    if fi.get_vector_dimension() != 0 {
      let writer = catch_aborting_exception!(
        &mut self.has_hit_aborting_exception,
        aborting_exception,
        "panic while adding vector field",
        self
          .vector_values_consumer
          .add_field(fi.clone(), segment_info)
      )?;
      pf.knn_field_vectors_writer = Some(writer);
    }

    Ok(())
  }

  fn process_field(
    &mut self,
    doc_id: i32,
    field: &mut impl IndexableField,
    per_field_index: usize,
    index_writer_config: &impl LiveIndexWriterConfig,
    aborting_exception: &OnceLock<LuceneError>,
  ) -> Result<bool> {
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
        .stored_value()
        .ok_or_else(|| LuceneError::illegal_argument("Cannot store a null value"))?;
      if let FieldDataEnum::String(s) = &stored_value
        && s.len() > MAX_STORED_STRING_LENGTH as usize
      {
        return Err(LuceneError::illegal_argument(format!(
          "stored field \"{}\" is too large ({} characters) to store",
          field.name(),
          s.len()
        )));
      };
      catch_aborting_exception!(
        &mut self.has_hit_aborting_exception,
        aborting_exception,
        "panic while writing stored field",
        self
          .stored_fields_consumer
          .write_field(pf.field_info.as_ref().unwrap(), &stored_value)
      )?;
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
      pf.point_values_writer
        .as_mut()
        .unwrap()
        .add_packed_value(doc_id, binary_value.as_ref())?;
    }

    if field.field_type().vector_dimension() != 0 {
      let field_writer_idx = pf
        .knn_field_vectors_writer
        .ok_or_else(|| LuceneError::illegal_state("field writer idx not initialized"))?;
      self.index_vector_value(doc_id, field_writer_idx, field)?;
    }

    Ok(indexed_field)
  }
  /// Returns a previously created [`PerField`], absorbing the type information from
  /// [`FieldType`](crate::core::document::field_type::FieldType), and creates a new [`PerField`] if this field name wasn't seen yet.
  pub(crate) fn get_or_add_per_field(&mut self, field: &Fields, reserved: bool) -> Result<usize> {
    let field_name = field.name();
    let hash_pos = CoreHelper::calculate_hash(field_name) as usize & self.hash_mask;
    let mut per_field_index = self.field_hash[hash_pos];
    let mut conflict = false;
    while per_field_index >= 0 {
      conflict = true;
      debug_assert!(self.per_fields.get(per_field_index as usize).is_some());
      let pf = &mut self.per_fields[per_field_index as usize];
      if pf.field_name != field_name {
        per_field_index = pf.next;
      } else {
        break;
      }
    }
    if per_field_index < 0 {
      let schema = FieldSchema::new(field_name);
      let mut pf = PerField::new(field, self.index_created_version_major, schema, reserved);
      // filed_name's hash conflict happened, and could not find existing PerField with the same name in next chain
      if conflict {
        let old_pos = self.field_hash[hash_pos];
        pf.next = old_pos;
      }
      per_field_index = self.per_fields.len() as i32;
      pf.idx_in_doc_field = per_field_index;
      self.per_fields.push(pf);
      self.field_hash[hash_pos] = per_field_index;

      self.total_field_count += 1;

      if self.total_field_count >= (self.field_hash.len() >> 1) {
        self.rehash();
      }
      if self.total_field_count > self.fields.len() {
        ArrayUtil::grow_with_len(&mut self.fields, self.total_field_count)?;
      }
    }
    Ok(per_field_index as usize)
  }
  // update schema for field as seen in a particular document
  fn update_doc_field_schema<IFT>(
    field_name: &str,
    schema: &mut FieldSchema,
    field_type: &IFT,
  ) -> Result<()>
  where
    IFT: IndexableFieldType,
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
        schema.name,
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

    if let Some(attrs) = field_type.get_attributes()
      && !attrs.is_empty()
    {
      schema.update_attributes(attrs.clone());
    }

    Ok(())
  }

  fn verify_unindexed_field_type<IFT>(name: &str, ft: &IFT) -> Result<()>
  where
    IFT: IndexableFieldType,
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

  pub fn index_doc_value(
    doc_id: i32,
    fp: &mut PerField,
    dv_type: DocValuesType,
    field: &impl IndexableField,
    pool: &mut ByteBlockPool,
  ) -> Result<()> {
    match fp.doc_values_writer.as_mut() {
      Some(DocValuesWriterEnum::Numeric(writer)) => {
        debug_assert_eq!(dv_type, DocValuesType::Numeric);
        let num = field
          .numeric_value()?
          .ok_or_else(|| {
            LuceneError::illegal_argument(format!(
              "field=\"{}\": null value not allowed",
              fp.field_info.as_ref().unwrap().name
            ))
          })?
          .to_i64();
        match num {
          Some(num) => {
            writer.add_value(doc_id, num)?;
          },
          _ => {
            return Err(LuceneError::illegal_argument(format!(
              "field=\"{}\": numeric value out of range: {:?}",
              fp.field_info.as_ref().unwrap().name,
              num
            )));
          },
        }
      },
      Some(DocValuesWriterEnum::Binary(writer)) => {
        debug_assert_eq!(dv_type, DocValuesType::Binary);
        let bytes = field.binary_value()?.ok_or_else(|| {
          LuceneError::illegal_argument(format!(
            "field=\"{}\": null value not allowed",
            fp.field_info.as_ref().unwrap().name
          ))
        })?;
        writer.add_value(doc_id, bytes.as_ref())?;
      },
      Some(DocValuesWriterEnum::Sorted(writer)) => {
        debug_assert_eq!(dv_type, DocValuesType::Sorted);
        let bytes = field.binary_value()?.ok_or_else(|| {
          LuceneError::illegal_argument(format!(
            "field=\"{}\": null value not allowed",
            fp.field_info.as_ref().unwrap().name
          ))
        })?;
        writer.add_value(doc_id, bytes.as_ref(), pool)?;
      },
      Some(DocValuesWriterEnum::SortedNumeric(writer)) => {
        debug_assert_eq!(dv_type, DocValuesType::SortedNumeric);
        let num = field
          .numeric_value()?
          .ok_or_else(|| {
            LuceneError::illegal_argument(format!(
              "field=\"{}\": null value not allowed",
              fp.field_info.as_ref().unwrap().name
            ))
          })?
          .to_i64();

        match num {
          Some(num) => {
            writer.add_value(doc_id, num)?;
          },
          _ => {
            return Err(LuceneError::illegal_argument(format!(
              "field=\"{}\": numeric value out of range: {:?}",
              fp.field_info.as_ref().unwrap().name,
              num
            )));
          },
        }
      },
      Some(DocValuesWriterEnum::SortedSet(writer)) => {
        debug_assert_eq!(dv_type, DocValuesType::SortedSet);
        let bytes = field.binary_value()?.ok_or_else(|| {
          LuceneError::illegal_argument(format!(
            "field=\"{}\": null value not allowed",
            fp.field_info.as_ref().unwrap().name
          ))
        })?;
        writer.add_value(doc_id, bytes.as_ref(), pool)?;
      },
      None => {
        return Err(LuceneError::illegal_state(format!(
          "field=\"{}\": no DocValuesWriter for type {:?}",
          fp.field_info.as_ref().unwrap().name,
          dv_type
        )));
      },
    }
    Ok(())
  }

  fn index_vector_value(
    &mut self,
    doc_id: i32,
    field_writer_idx: usize,
    field: &impl IndexableField,
  ) -> Result<()> {
    let writer = self
      .vector_values_consumer
      .writer
      .as_mut()
      .ok_or_else(|| LuceneError::illegal_state("writer not initialized"))?;
    let vector_value = field.vector_value()?;
    writer.add_value(doc_id, vector_value, field_writer_idx)
  }

  fn get_per_field(&self, name: &str) -> Option<usize> {
    let hash_pos = CoreHelper::calculate_hash(&name.to_string()) as usize & self.hash_mask;
    let mut per_field_index = self.field_hash[hash_pos];
    while per_field_index >= 0 {
      let pf = &self.per_fields[per_field_index as usize];
      if pf.field_name == name {
        return Some(per_field_index as usize);
      }
      per_field_index = pf.next;
    }
    None
  }
  #[allow(dead_code)] // not use in rust lucene
  pub(crate) fn mark_as_reserved() {}

  pub(crate) fn get_has_doc_values(&mut self, field: &str) -> Result<Option<DocValuesWriterDISI>> {
    if let Some(idx) = self.get_per_field(field) {
      let pf = &mut self.per_fields[idx];
      if let Some(ref writer_enum) = pf.doc_values_writer {
        if *pf.field_info.as_ref().unwrap().get_doc_values_type() == DocValuesType::None {
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
}

pub(crate) struct PerField {
  pub(crate) field_name: String,
  pub(crate) index_created_version_major: i32,
  pub(crate) schema: FieldSchema,
  pub(crate) reserved: bool,
  pub(crate) field_info: Option<Arc<FieldInfo>>,
  pub(crate) invert_state: Option<FieldInvertState>,
  pub(crate) terms_hash_per_field: Option<FreqProxTermsWriterPerField>,
  pub(crate) doc_values_writer: Option<DocValuesWriterEnum>,
  pub(crate) point_values_writer: Option<PointValuesWriter>,
  pub(crate) knn_field_vectors_writer: Option<usize>,
  pub(crate) field_gen: i64,
  pub(crate) next: i32,
  pub(crate) norms: Option<NormValuesWriter>,
  // reuse
  // TODO IMPORTANT  如果同一个域既有 StringValue 跟 Binary是不是应该同时保留BinaryTokenStream, StringTokenStream是的复用最大化
  pub(crate) token_stream: Option<ReusedIndexingTokenStream>,
  pub(crate) first: bool,
  pub(crate) idx_in_doc_field: i32,
}
impl PerField {
  fn on_aborting_exception(
    has_hit_aborting_exception: &mut bool,
    aborting_exception: &OnceLock<LuceneError>,
    throwable: LuceneError,
  ) {
    *has_hit_aborting_exception = true;
    let _ = aborting_exception.set(throwable);
  }

  pub(crate) fn new(
    field: &Fields,
    index_created_version_major: i32,
    schema: FieldSchema,
    reserved: bool,
  ) -> Self {
    PerField {
      field_name: field.name().to_string(),
      index_created_version_major,
      schema,
      reserved,
      field_info: None,
      invert_state: None,
      terms_hash_per_field: None,
      doc_values_writer: None,
      point_values_writer: None,
      knn_field_vectors_writer: None,
      field_gen: -1,
      next: -1,
      norms: None,
      token_stream: None,
      first: false,
      idx_in_doc_field: -1,
    }
  }
  pub(crate) fn reset(&mut self, doc_id: i32) {
    self.first = true;
    self.schema.reset(doc_id);
  }

  pub(crate) fn set_field_info(&mut self, field_info: Arc<FieldInfo>) {
    debug_assert!(self.field_info.is_none());
    self.field_info = Some(field_info);
  }
  pub(crate) fn set_invert_state<D>(
    &mut self,
    terms_hash: &mut FreqProxTermsWriter<D>,
    bytes_used: SharedCounter,
  ) -> Result<()>
  where
    D: Directory + Clone,
  {
    let fi = self.field_info.as_ref().unwrap().clone();
    let state = FieldInvertState::new(
      self.index_created_version_major,
      fi.name.clone(),
      *fi.get_index_options(),
    );
    self.invert_state = Some(state);
    self.terms_hash_per_field =
      Some(terms_hash.add_field(self.field_info.as_ref().unwrap().clone())?);

    if !fi.omits_norms() {
      // Even if no documents actually succeed in setting a norm, we still write norms for this
      // segment
      debug_assert!(self.norms.is_none());
      self.norms = Some(NormValuesWriter::new(fi.clone(), bytes_used)?);
    }

    if fi.has_term_vectors() {
      terms_hash.next_terms_hash.set_has_vectors();
    }
    Ok(())
  }
  pub(crate) fn finish<D, S>(
    &mut self,
    doc_id: i32,
    term_vectors_consumer: &mut TermVectorsConsumer<D>,
    similarity: &S,
  ) -> Result<()>
  where
    D: Directory + Clone,
    S: Similarity,
  {
    if !self.field_info.as_ref().unwrap().omits_norms() {
      let norm_value = {
        let state = self.invert_state.as_ref().unwrap();
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
      self.norms.as_mut().unwrap().add_value(doc_id, norm_value)?;
    }
    let meta = PerFieldMeta {
      idx: self.idx_in_doc_field,
      field_name: self.field_name.clone(),
    };
    self
      .terms_hash_per_field
      .as_mut()
      .unwrap()
      .finish(term_vectors_consumer, meta)
  }
  /// Inverts one field for one document; first is true if this is the first time we are seeing
  /// this field name in this document.
  #[allow(clippy::too_many_arguments)]
  pub(crate) fn invert<A>(
    &mut self,
    doc_id: i32,
    field: &mut impl IndexableField,
    first: bool,
    analyzer: &A,
    info_stream: &InfoStreamEnum,
    context: &mut IndexContext,
    has_hit_aborting_exception: &mut bool,
    aborting_exception: &OnceLock<LuceneError>,
  ) -> Result<()>
  where
    A: Analyzer,
  {
    debug_assert!(
      *field.field_type().index_options() >= IndexOptions::Docs,
      "field must be indexed with at least Docs"
    );

    if first {
      match &mut self.invert_state {
        Some(invert_state) => {
          // First time we're seeing this field (indexed) in this document
          invert_state.reset()
        },
        None => {
          return Err(LuceneError::illegal_state("invert_state not initialized"));
        },
      }
    }

    match field.invertable_type() {
      InvertableType::BINARY => {
        self.invert_term(doc_id, field, first, context, aborting_exception)?;
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
  fn invert_token_stream<A>(
    &mut self,
    doc_id: i32,
    field: &mut impl IndexableField,
    first: bool,
    analyzer: &A,
    info_stream: &InfoStreamEnum,
    context: &mut IndexContext,
    has_hit_aborting_exception: &mut bool,
    aborting_exception: &OnceLock<LuceneError>,
  ) -> Result<()>
  where
    A: Analyzer,
  {
    let analyzed = field.field_type().tokenized();
    /*
     * To assist people in tracking down problems in analysis components, we wish to write the field name to the infostream
     * when we fail. We expect some caller to eventually deal with the real error, so we don't want any 'catch' clauses,
     * but rather a finally that takes note of the problem.
     */

    let field_name = field.name().to_string();

    {
      let terms_hash_per_field = self.terms_hash_per_field.as_mut().unwrap();
      terms_hash_per_field.start(field, first, &mut context.byte_pool)?;
      let mut stream = field
        .token_stream(analyzer, &mut self.token_stream)?
        .ok_or_else(|| LuceneError::illegal_state("Analyzer token_stream is not initialized"))?;
      let result = (|| {
        stream.reset()?;
        while stream.increment_token()? {
          // If we hit an error in stream.next below
          // (which is fairly common, e.g. if analyzer
          // chokes on a given document), then it's
          // non-aborting and (above) this one document
          // will be marked as deleted, but still
          // consume a docID
          let attribute_source = stream.get_attribute_source_mut();
          let invert_state = self.invert_state.as_mut().unwrap();
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
                "position overflowed Integer.MAX_VALUE (got posIncr={} last_position={} position={}) for field '{}'",
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
          match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            terms_hash_per_field.add_with_bytes_ref(
              // use attribute_source's bytes_ref
              None,
              doc_id,
              self.invert_state.as_mut().unwrap(),
              attribute_source,
              context,
            )
          })) {
            Ok(Ok(())) => {},
            Ok(Err(e)) => match e {
              LuceneError::MaxBytesLengthExceeded(e) => {
                let bytes_ref = attribute_source.get_bytes_ref()?.ok_or_else(|| {
                  LuceneError::illegal_state("BytesRef is None in attribute_source")
                })?;
                let mut prefix = [0u8; 30];
                let len = 30.min(bytes_ref.length);
                prefix[..len]
                  .copy_from_slice(&bytes_ref.bytes[bytes_ref.offset..bytes_ref.offset + len]);
                let mut ia = LuceneError::illegal_argument(format!(
                  "Document contains at least one immense term in field=\"{}\" (whose UTF8 encoding is longer than the max length {}), all of which were skipped. Please correct the analyzer to not produce such terms. The prefix of the first immense term is: '{:?}...', original message: {}",
                  self.field_info.as_ref().unwrap().name,
                  MAX_TERM_LENGTH,
                  prefix,
                  e
                ));
                ia.add_suppressed(e.into());
                return Err(ia);
              },
              other => {
                Self::on_aborting_exception(
                  has_hit_aborting_exception,
                  aborting_exception,
                  other.clone(),
                );
                return Err(other);
              },
            },
            Err(e) => {
              let tragedy = LuceneError::tragedy_from_panic("panic while adding term", e.as_ref());
              Self::on_aborting_exception(
                has_hit_aborting_exception,
                aborting_exception,
                tragedy.clone(),
              );
              return Err(tragedy);
            },
          }
        }
        // trigger streams to perform end-of-stream operations
        stream.end()?;
        // when we come back around to the field...
        let invert_state = self.invert_state.as_mut().unwrap();
        // TODO
        invert_state.position += stream.get_attribute_source().get_position_increment()?;
        invert_state.offset += stream.get_attribute_source().end_offset()?;
        Ok(())
      })();
      let result = IOUtils::use_or_suppress_result(result, stream.close());

      if result.is_err() && info_stream.is_enabled("DW") {
        info_stream.message(
          "DW",
          &format!("exception in invert_token_stream for {}", field_name),
        )?;
      }

      result?;
    }

    if analyzed {
      let invert_state = self.invert_state.as_mut().unwrap();
      invert_state.position +=
        analyzer.get_position_increment_gap(&self.field_info.as_ref().unwrap().name);
      invert_state.offset += analyzer.get_offset_gap(&self.field_info.as_ref().unwrap().name);
    }
    Ok(())
  }

  fn invert_term<F>(
    &mut self,
    doc_id: i32,
    field: &F,
    first: bool,
    context: &mut IndexContext,
    aborting_exception: &OnceLock<LuceneError>,
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
    let state = self.invert_state.as_mut().unwrap();
    // TODO
    // state.set_attribute_source();
    state.position += 1;
    state.length += 1;
    let terms_hash_per_field = self.terms_hash_per_field.as_mut().unwrap();
    terms_hash_per_field.start(field, first, &mut context.byte_pool)?;
    match state.length.checked_add(1) {
      Some(new_length) => {
        state.length = new_length;
      },
      None => {
        return Err(LuceneError::number_overflow("Field length overflowed"));
      },
    }
    let mut attribute_source = EmptyAttributeSource;
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      terms_hash_per_field.add_with_bytes_ref(
        Some(binary_value.as_ref()),
        doc_id,
        state,
        &mut attribute_source,
        context,
      )
    })) {
      Ok(Ok(())) => {},
      Ok(Err(e)) => match e {
        LuceneError::MaxBytesLengthExceeded(e) => {
          let mut prefix = [0u8; 30];
          prefix
            .copy_from_slice(&binary_value.bytes[binary_value.offset..binary_value.offset + 30]);
          let msg = format!(
            "Document contains at least one immense term in field=\"{}\" (whose length is longer than the max length {}), all of which were skipped. The prefix of the first immense term is: '{:?}...'",
            self.field_info.as_ref().unwrap().name,
            MAX_TERM_LENGTH,
            prefix
          );
          let mut ia = LuceneError::illegal_argument(msg);
          ia.add_suppressed(e.into());
          return Err(ia);
        },
        other => {
          let _ = aborting_exception.set(other.clone());
          return Err(other);
        },
      },
      Err(e) => {
        let tragedy = LuceneError::tragedy_from_panic("panic while adding binary term", e.as_ref());
        let _ = aborting_exception.set(tragedy.clone());
        return Err(tragedy);
      },
    }
    Ok(())
  }
}

impl PartialEq for PerField {
  fn eq(&self, other: &Self) -> bool {
    self.field_name == other.field_name
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
    self.field_name.cmp(&other.field_name)
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
/// the current document with the corresponding `FieldInfo` (constructed from the first document
/// in the segment where this field appeared). If there is any inconsistency, we return an error.
/// This ensures that a field’s data structures remain consistent across all documents.
pub(crate) struct FieldSchema {
  name: String,
  doc_id: i32,
  attributes: HashMap<String, String>,
  omit_norms: bool,
  store_term_vector: bool,
  index_options: IndexOptions,
  doc_values_type: DocValuesType,
  doc_values_skip_index: DocValuesSkipIndexType,
  point_dimension_count: usize,
  point_index_dimension_count: usize,
  point_num_bytes: usize,
  vector_dimension: usize,
  vector_encoding: VectorEncoding,
  vector_similarity_function: VectorSimilarityFunction,
}
impl FieldSchema {
  const ERR_MSG: &'static str =
    "Inconsistency of field data structures across documents for field ";
  pub(crate) fn new(name: &str) -> Self {
    FieldSchema {
      name: name.to_string(),
      doc_id: 0,
      attributes: HashMap::new(),
      omit_norms: false,
      store_term_vector: false,
      index_options: IndexOptions::None,
      doc_values_type: DocValuesType::None,
      doc_values_skip_index: DocValuesSkipIndexType::None,
      point_dimension_count: 0,
      point_index_dimension_count: 0,
      point_num_bytes: 0,
      vector_dimension: 0,
      vector_encoding: VectorEncoding::FLOAT32(4),
      vector_similarity_function: VectorSimilarityFunction::Euclidean,
    }
  }
  pub(crate) fn assert_same<T>(&self, label: &str, expected: &T, given: &T) -> Result<()>
  where
    T: PartialEq + Display,
  {
    if expected != given {
      return Err(LuceneError::illegal_argument(format!(
        "{}[{}] of doc [{}]. {}: expected '{}', but it has '{}'.",
        Self::ERR_MSG,
        self.name,
        self.doc_id,
        label,
        expected,
        given
      )));
    }
    Ok(())
  }
  pub(crate) fn update_attributes(&mut self, attrs: HashMap<String, String>) {
    self.attributes.extend(attrs);
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
      self.point_dimension_count = dimension_count;
      self.point_index_dimension_count = index_dimension_count;
      self.point_num_bytes = num_bytes;
    } else {
      self.assert_same(
        "point dimension",
        &self.point_dimension_count,
        &dimension_count,
      )?;
      self.assert_same(
        "point index dimension",
        &self.point_index_dimension_count,
        &index_dimension_count,
      )?;
      self.assert_same("point num bytes", &self.point_num_bytes, &num_bytes)?;
    }
    Ok(())
  }

  pub(crate) fn set_vectors(
    &mut self,
    encoding: VectorEncoding,
    similarity_function: VectorSimilarityFunction,
    dimension: usize,
  ) -> Result<()> {
    if self.vector_dimension == 0 {
      self.vector_encoding = encoding;
      self.vector_similarity_function = similarity_function;
      self.vector_dimension = dimension;
    } else {
      self.assert_same("vector encoding", &self.vector_encoding, &encoding)?;
      self.assert_same(
        "vector similarity function",
        &self.vector_similarity_function,
        &similarity_function,
      )?;
      self.assert_same("vector dimension", &self.vector_dimension, &dimension)?;
    }
    Ok(())
  }
  pub(crate) fn reset(&mut self, doc: i32) {
    self.doc_id = doc;
    self.omit_norms = false;
    self.store_term_vector = false;
    self.index_options = IndexOptions::None;
    self.doc_values_type = DocValuesType::None;
    self.doc_values_skip_index = DocValuesSkipIndexType::None;
    self.point_dimension_count = 0;
    self.point_index_dimension_count = 0;
    self.point_num_bytes = 0;
    self.vector_dimension = 0;
    self.vector_encoding = VectorEncoding::FLOAT32(4);
    self.vector_similarity_function = VectorSimilarityFunction::Euclidean;
  }

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
    self.assert_same(
      "vector similarity function",
      fi.get_vector_similarity_function(),
      &self.vector_similarity_function,
    )?;
    self.assert_same(
      "vector encoding",
      fi.get_vector_encoding(),
      &self.vector_encoding,
    )?;
    self.assert_same(
      "vector dimension",
      &(fi.get_vector_dimension() as usize),
      &self.vector_dimension,
    )?;
    self.assert_same(
      "point dimension",
      &fi.get_point_dimension_count(),
      &self.point_dimension_count,
    )?;
    self.assert_same(
      "point index dimension",
      &fi.get_point_index_dimension_count(),
      &self.point_index_dimension_count,
    )?;
    self.assert_same(
      "point num bytes",
      &fi.get_point_num_bytes(),
      &self.point_num_bytes,
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
  field_info: &'a mut Builder,
  index_base: IndexReaderBase,
}
impl<'a, D> DocValuesLeafReaderImpl1<'a, D>
where
  D: Directory + Clone,
{
  fn new(index_chain: &'a mut IndexingChain<D>, field_info: &'a mut Builder) -> Self {
    let base = DocValuesLeafReader;
    DocValuesLeafReaderImpl1 {
      index_chain,
      base,
      field_info,
      index_base: IndexReaderBase::new(),
    }
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
    let pf_index = self.index_chain.get_per_field(field);
    if pf_index.is_none() {
      return Ok(None);
    }
    let pf = &self.index_chain.per_fields[pf_index.unwrap()];
    if *pf.field_info.as_ref().unwrap().get_doc_values_type() == DocValuesType::Numeric {
      match pf.doc_values_writer {
        Some(DocValuesWriterEnum::Numeric(ref writer)) => {
          Ok(Option::from(writer.get_doc_values()?))
        },
        _ => Err(LuceneError::illegal_state(format!(
          "field=\"{}\": expected Numeric DocValuesWriter, found {}",
          pf.field_name,
          pf.doc_values_writer.as_ref().unwrap()
        ))),
      }
    } else {
      Ok(None)
    }
  }

  type BinaryDocValues = BufferedBinaryDocValues<DocsWithFieldSetDISI, PagedBytesDataInput>;

  fn get_binary_doc_values(&self, field: &str) -> Result<Option<Self::BinaryDocValues>> {
    let pf_index = self.index_chain.get_per_field(field);
    if pf_index.is_none() {
      return Ok(None);
    }
    let pf = &self.index_chain.per_fields[pf_index.unwrap()];
    if *pf.field_info.as_ref().unwrap().get_doc_values_type() == DocValuesType::Binary {
      match pf.doc_values_writer {
        Some(DocValuesWriterEnum::Binary(ref writer)) => Ok(Option::from(writer.get_doc_values()?)),
        _ => Err(LuceneError::illegal_state(format!(
          "field=\"{}\": expected Binary DocValuesWriter, found {}",
          pf.field_name,
          pf.doc_values_writer.as_ref().unwrap()
        ))),
      }
    } else {
      Ok(None)
    }
  }

  type SortedDocValues = BufferedSortedDocValues<DocsWithFieldSetDISI>;

  fn get_sorted_doc_values(&self, field: &str) -> Result<Option<Self::SortedDocValues>> {
    let pf_index = self.index_chain.get_per_field(field);
    if pf_index.is_none() {
      return Ok(None);
    }
    let pf = &self.index_chain.per_fields[pf_index.unwrap()];
    if *pf.field_info.as_ref().unwrap().get_doc_values_type() == DocValuesType::Sorted {
      match pf.doc_values_writer {
        Some(DocValuesWriterEnum::Sorted(ref writer)) => Ok(Option::from(writer.get_doc_values()?)),
        _ => Err(LuceneError::illegal_state(format!(
          "field=\"{}\": expected Sorted DocValuesWriter, found {}",
          pf.field_name,
          pf.doc_values_writer.as_ref().unwrap()
        ))),
      }
    } else {
      Ok(None)
    }
  }

  type SortedNumericDocValues = SortedNumericDocValuesEnum2<
    SingletonSortedNumericDocValues<BufferedNumericDocValues>,
    BufferedSortedNumericDocValues<DocsWithFieldSetDISI>,
  >;

  fn get_sorted_numeric_doc_values(
    &self,
    field: &str,
  ) -> Result<Option<Self::SortedNumericDocValues>> {
    let pf_index = self.index_chain.get_per_field(field);
    if pf_index.is_none() {
      return Ok(None);
    }
    let pf = &self.index_chain.per_fields[pf_index.unwrap()];
    if *pf.field_info.as_ref().unwrap().get_doc_values_type() == DocValuesType::SortedNumeric {
      match pf.doc_values_writer {
        Some(DocValuesWriterEnum::SortedNumeric(ref writer)) => {
          Ok(Option::from(writer.get_doc_values()?))
        },
        _ => Err(LuceneError::illegal_state(format!(
          "field=\"{}\": expected SortedNumeric DocValuesWriter, found {}",
          pf.field_name,
          pf.doc_values_writer.as_ref().unwrap()
        ))),
      }
    } else {
      Ok(None)
    }
  }

  fn search_nearest_vectors_f32<B, K>(
    &self,
    field: &str,
    target: Vec<f32>,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
  {
    self
      .base
      .search_nearest_vectors_f32(field, target, knn_collector, accept_docs)
  }

  fn search_nearest_vectors_u8<B, K>(
    &self,
    field: &str,
    target: Vec<u8>,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
  {
    self
      .base
      .search_nearest_vectors_u8(field, target, knn_collector, accept_docs)
  }

  type SortedSetDocValues = SortedSetDocValuesEnum2<
    SingletonSortedSetDocValues<BufferedSortedDocValues<DocsWithFieldSetDISI>>,
    BufferedSortedSetDocValues<DocsWithFieldSetDISI>,
  >;

  fn get_sorted_set_doc_values(&self, field: &str) -> Result<Option<Self::SortedSetDocValues>> {
    let pf_index = self.index_chain.get_per_field(field);
    if pf_index.is_none() {
      return Ok(None);
    }
    let pf = &self.index_chain.per_fields[pf_index.unwrap()];
    if *pf.field_info.as_ref().unwrap().get_doc_values_type() == DocValuesType::SortedSet {
      match pf.doc_values_writer {
        Some(DocValuesWriterEnum::SortedSet(ref writer)) => {
          Ok(Option::from(writer.get_doc_values()?))
        },
        _ => Err(LuceneError::illegal_state(format!(
          "field=\"{}\": expected SortedSet DocValuesWriter, found {}",
          pf.field_name,
          pf.doc_values_writer.as_ref().unwrap()
        ))),
      }
    } else {
      Ok(None)
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
    Ok(Arc::new(self.field_info.finish()?))
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

  fn search_nearest_vectors_f32<B, K>(
    &self,
    field: &str,
    target: Vec<f32>,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
  {
    self
      .base
      .search_nearest_vectors_f32(field, target, knn_collector, accept_docs)
  }

  fn search_nearest_vectors_u8<B, K>(
    &self,
    field: &str,
    target: Vec<u8>,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
  {
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

struct DocComparatorImpl<DC>
where
  DC: DocComparator,
{
  parents: Rc<SparseFixedBitSetBitSet>,
  doc_comparator: DC,
}
impl<DC> DocComparatorImpl<DC>
where
  DC: DocComparator,
{
  fn new(parents: Rc<SparseFixedBitSetBitSet>, doc_comparator: DC) -> Self {
    DocComparatorImpl {
      parents,
      doc_comparator,
    }
  }
}
impl<DC> DocComparator for DocComparatorImpl<DC>
where
  DC: DocComparator,
{
  fn compare(&self, doc_id1: usize, doc_id2: usize) -> i32 {
    let doc_id1 = self.parents.next_set_bit(doc_id1);
    let doc_id2 = self.parents.next_set_bit(doc_id2);
    self.doc_comparator.compare(doc_id1, doc_id2)
  }
}

pub struct ReservedField<T>
where
  T: IndexableField,
{
  delegate: T,
}
impl<T> ReservedField<T>
where
  T: IndexableField,
{
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

  fn binary_value(&self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
    self.delegate.binary_value()
  }

  fn take_binary_value(&mut self) -> Result<Option<BytesRef<Vec<u8>>>> {
    self.delegate.take_binary_value()
  }

  fn string_value(&self) -> Result<Option<Cow<'_, String>>> {
    self.delegate.string_value()
  }

  fn take_string_value(&mut self) -> Result<Option<String>> {
    self.delegate.take_string_value()
  }

  fn get_char_sequence_value(&self) -> Result<Option<Cow<'_, String>>> {
    self.delegate.get_char_sequence_value()
  }

  fn take_reader_value(&mut self) -> Result<Option<ReaderEnum>> {
    self.delegate.take_reader_value()
  }

  fn numeric_value(&self) -> Result<Option<Number>> {
    self.delegate.numeric_value()
  }

  fn stored_value(&self) -> Option<FieldDataEnum> {
    self.delegate.stored_value()
  }
  fn invertable_type(&self) -> &InvertableType {
    self.delegate.invertable_type()
  }

  fn is_reserved(&self) -> bool {
    true
  }
}
