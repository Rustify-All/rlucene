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
use crate::core::codecs::doc_values_consumer::DocValuesConsumer;
use crate::core::codecs::doc_values_format::DocValuesFormat;
use crate::core::codecs::field_infos_format::FieldInfosFormat;
use crate::core::codecs::fields_consumer::FieldsConsumer;
use crate::core::codecs::knn_vectors_format::KnnVectorsFormat;
use crate::core::codecs::knn_vectors_writer::KnnVectorsWriter;
use crate::core::codecs::norms_consumer::NormsConsumer;
use crate::core::codecs::norms_format::NormsFormat;
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::codecs::points_format::PointsFormat;
use crate::core::codecs::points_writer::PointsWriter;
use crate::core::codecs::postings_format::PostingsFormat;
use crate::core::codecs::stored_fields_format::StoredFieldsFormat;
use crate::core::codecs::stored_fields_writer::StoredFieldsWriter;
use crate::core::codecs::term_vectors_format::TermVectorsFormat;
use crate::core::codecs::term_vectors_writer::TermVectorsWriter;
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::field_infos::Builder;
use crate::core::index::field_infos::FieldNumbersLock;
use crate::core::index::merge_state::MergeState;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::store::Context::Merge;
use crate::core::store::IOContext;
use crate::core::store::directory::Directory;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::info_stream::{InfoStream, InfoStreamMT};
use crate::core::util::{IOUtils, LATEST, StringHelper};
use std::sync::Arc;
use std::time::Instant;

/// The `SegmentMerger` struct combines two or more segments represented by
/// `IndexReader`s, into a single segment. Call the `merge` method to combine
/// the segments.
///
/// See [`SegmentMerger::merge`].
pub(crate) struct SegmentMerger<'a, D1, D2, CR>
where
  D1: Directory,
  D2: Directory,
  CR: CodecReader,
{
  directory: &'a D2,
  context: &'a IOContext,
  pub(crate) merge_state: MergeState<'a, D1, CR>,
  field_infos_builder: Builder,
  pub(crate) id: String,
}

impl<'a, D1, D2, CR> SegmentMerger<'a, D1, D2, CR>
where
  D1: Directory,
  D2: Directory,
  CR: CodecReader,
{
  pub(crate) fn new(
    readers: &'a [CR],
    segment_info: &'a mut SegmentInfo<D1>,
    info_stream: InfoStreamMT,
    directory: &'a D2,
    field_numbers: FieldNumbersLock,
    context: &'a IOContext,
  ) -> Result<Self> {
    if *context.get_context() != Merge {
      return Err(LuceneError::illegal_argument(format!(
        "IOContext.context should be MERGE; got: {:?}",
        context.get_context()
      )));
    }

    let merge_state = MergeState::new(readers, segment_info, info_stream)?;

    let field_infos_builder = Builder::new(field_numbers);

    let mut min_version = Some(LATEST.clone());
    for reader in readers {
      let leaf_min_version = reader.get_metadata()?.get_min_version();
      match leaf_min_version {
        Some(v) => {
          if let Some(cur) = &mut min_version
            && cur.on_or_after(v)
          {
            *cur = v.clone();
          }
        },
        None => {
          min_version = None;
          break;
        },
      }
    }

    debug_assert!(
      merge_state.segment_info.min_version.is_none(),
      "The min version should be set by SegmentMerger for merged segments"
    );
    merge_state.segment_info.min_version = min_version;

    if merge_state.info_stream.is_enabled("SM")
      && let Some(sort) = merge_state.segment_info.get_index_sort()
    {
      merge_state
        .info_stream
        .message("SM", &format!("index sort during merge: {}", sort))?;
    }
    let id = StringHelper::id_to_string(Option::from(&StringHelper::random_id()));
    Ok(Self {
      directory,
      context,
      merge_state,
      field_infos_builder,
      id,
    })
  }
  fn merge_field_infos_with_state(
    &self,
    _segment_write_state: &SegmentWriteState<&D2>,
    _segment_read_state: &SegmentReadState<&D2>,
  ) -> Result<()> {
    self
      .merge_state
      .segment_info
      .get_codec()?
      .field_infos_format()
      .write(
        &self.directory,
        self.merge_state.segment_info,
        "",
        &self.merge_state.merge_field_infos,
        self.context,
      )
  }

  fn merge_doc_values(&self, segment_write_state: &SegmentWriteState<&D2>) -> Result<()> {
    let mut consumer = self
      .merge_state
      .segment_info
      .get_codec()?
      .doc_values_format()
      .fields_consumer(segment_write_state, self.merge_state.segment_info)?;

    let merge_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      consumer.merge(
        segment_write_state,
        self.merge_state.segment_info,
        &self.merge_state,
      )
    }));
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| consumer.close()));
    IOUtils::use_or_suppress_caught_result(merge_result, close_result)
  }
  fn merge_points(&self, segment_write_state: &SegmentWriteState<&D2>) -> Result<()> {
    let mut writer = self
      .merge_state
      .segment_info
      .get_codec()?
      .points_format()
      .fields_writer(segment_write_state, self.merge_state.segment_info)?;

    let merge_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      writer.merge(&self.merge_state, &self.directory)
    }));
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| writer.close()));
    IOUtils::use_or_suppress_caught_result(merge_result, close_result)
  }
  fn merge_norms(&self, segment_write_state: &SegmentWriteState<&D2>) -> Result<()> {
    let mut consumer = self
      .merge_state
      .segment_info
      .get_codec()?
      .norms_format()
      .norms_consumer(segment_write_state, self.merge_state.segment_info)?;

    let merge_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      consumer.merge(&self.merge_state)
    }));
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| consumer.close()));
    IOUtils::use_or_suppress_caught_result(merge_result, close_result)
  }
  fn merge_terms(
    &self,
    segment_write_state: &SegmentWriteState<&D2>,
    segment_read_state: &SegmentReadState<&D2>,
  ) -> Result<()> {
    let mut norms = if self.merge_state.merge_field_infos.has_norms() {
      Some(
        self
          .merge_state
          .segment_info
          .get_codec()?
          .norms_format()
          .norms_producer(segment_read_state, self.merge_state.segment_info)?,
      )
    } else {
      None
    };

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      let mut norms_merge_instance = None;
      if let Some(ref mut norms) = norms {
        // Use the merge instance in order to reuse the same IndexInput for all terms
        norms_merge_instance = norms.get_merge_instance()?;
      }

      if self.merge_state.merge_field_infos.has_postings() {
        let mut consumer = self
          .merge_state
          .segment_info
          .get_codec()?
          .postings_format()
          .fields_consumer(segment_write_state, self.merge_state.segment_info)?;

        let merge_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
          consumer.merge(
            segment_write_state,
            self.merge_state.segment_info,
            &self.merge_state,
            norms_merge_instance.as_ref(),
          )
        }));
        let close_result =
          std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| consumer.close()));
        IOUtils::use_or_suppress_caught_result(merge_result, close_result)?;
      }

      Ok(())
    }));
    let close_result =
      std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match norms.as_mut() {
        Some(norms) => norms.close(),
        None => Ok(()),
      }));
    IOUtils::use_or_suppress_caught_result(result, close_result)
  }
  fn merge_field_infos(&mut self) -> Result<()> {
    for reader_field_infos in &self.merge_state.field_infos {
      for fi in reader_field_infos.iter() {
        self.field_infos_builder.add(fi.clone())?;
      }
    }

    self.merge_state.merge_field_infos = Arc::new(self.field_infos_builder.finish()?);
    Ok(())
  }
  /// Merge stored fields from each of the segments into the new one.
  ///
  /// # Returns
  ///
  /// The number of documents in all of the readers.
  ///
  /// # Errors
  ///
  /// Returns an error if the index is corrupt or if there is a low-level I/O error.
  fn merge_fields(&mut self) -> Result<i32> {
    let mut fields_writer = self
      .merge_state
      .segment_info
      .get_codec()?
      .stored_fields_format()
      .fields_writer(self.directory, self.merge_state.segment_info, self.context)?;

    let merge_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      fields_writer.merge(&mut self.merge_state, &self.directory)
    }));
    let close_result =
      std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fields_writer.close()));
    IOUtils::use_or_suppress_caught_result(merge_result, close_result)
  }
  /// Merge the term vectors from each of the segments into the new one.
  /// # Errors
  ///
  /// Returns an error if there is a low-level I/O error.
  fn merge_term_vectors(&mut self) -> Result<i32> {
    let mut term_vectors_writer = self
      .merge_state
      .segment_info
      .get_codec()?
      .term_vectors_format()
      .vectors_writer(self.directory, self.merge_state.segment_info, self.context)?;

    let merge_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      term_vectors_writer.merge(&mut self.merge_state, &self.directory)
    }));
    let close_result =
      std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| term_vectors_writer.close()));
    let num_merged = IOUtils::use_or_suppress_caught_result(merge_result, close_result)?;

    debug_assert_eq!(num_merged, self.merge_state.segment_info.max_doc()?);

    Ok(num_merged)
  }
  fn merge_vector_values(&self, segment_write_state: &SegmentWriteState<&D2>) -> Result<()> {
    let mut writer = self
      .merge_state
      .segment_info
      .get_codec()?
      .knn_vectors_format()?
      .fields_writer(segment_write_state, self.merge_state.segment_info)?;

    let merge_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      writer
        .merge(&self.merge_state, segment_write_state)
        .map(|_| ())
    }));
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| writer.close()));
    IOUtils::use_or_suppress_caught_result(merge_result, close_result)
  }
  fn merge_with_logging<F, I>(merger: F, format_name: &str, info_stream: &I) -> Result<i32>
  where
    F: FnOnce() -> Result<i32>,
    I: InfoStream,
  {
    let mut t0 = None;

    if info_stream.is_enabled("SM") {
      t0 = Some(Instant::now());
    }

    let num_merged = merger()?;

    if let Some(t0) = t0 {
      let elapsed_ms = t0.elapsed().as_millis();
      info_stream.message(
        "SM",
        &format!(
          "{} ms to merge {} [{} docs]",
          elapsed_ms, format_name, num_merged
        ),
      )?;
    }

    Ok(num_merged)
  }
  fn merge_with_logging_with_name<F, I>(
    merger: F,
    segment_write_state: &SegmentWriteState<&D2>,
    segment_read_state: &SegmentReadState<&D2>,
    format_name: &str,
    num_merged: i32,
    info_stream: &I,
  ) -> Result<()>
  where
    F: FnOnce(&SegmentWriteState<&D2>, &SegmentReadState<&D2>) -> Result<()>,
    I: InfoStream,
  {
    let mut t0 = None;

    if info_stream.is_enabled("SM") {
      t0 = Some(Instant::now());
    }

    merger(segment_write_state, segment_read_state)?;

    if let Some(t0) = t0 {
      let elapsed_ms = t0.elapsed().as_millis();
      info_stream.message(
        "SM",
        &format!(
          "{} ms to merge {} [{} docs]",
          elapsed_ms, format_name, num_merged
        ),
      )?;
    }

    Ok(())
  }

  /// True if any merging should happen
  pub(crate) fn should_merge(&self) -> Result<bool> {
    Ok(self.merge_state.segment_info.max_doc()? > 0)
  }
  /// Merges the readers into the directory supplied when this value was created.
  ///
  /// # Returns
  ///
  /// The number of documents that were merged.
  ///
  /// # Errors
  ///
  /// Returns an error if the index is corrupt or if there is a low-level I/O error.
  pub(crate) fn merge(&mut self) -> Result<()> {
    if !self.should_merge()? {
      return Err(LuceneError::illegal_state(
        "Merge would result in 0 document segment",
      ));
    }

    self.merge_field_infos()?;
    let info_stream = self.merge_state.info_stream.clone();
    let num_merged = Self::merge_with_logging(
      || self.merge_fields(),
      "stored fields",
      info_stream.as_ref(),
    )?;

    debug_assert_eq!(
      num_merged,
      self.merge_state.segment_info.max_doc()?,
      "numMerged={} vs mergeState.segmentInfo.maxDoc()={}",
      num_merged,
      self.merge_state.segment_info.max_doc()?
    );

    let segment_write_state = SegmentWriteState::new(
      self.merge_state.info_stream.clone(),
      &self.directory,
      self.merge_state.merge_field_infos.clone(),
      self.context,
    );

    let segment_read_state = SegmentReadState::new(
      &self.directory,
      self.merge_state.merge_field_infos.clone(),
      self.context,
    );
    {
      if self.merge_state.merge_field_infos.has_norms() {
        Self::merge_with_logging_with_name(
          |sws, _srs| self.merge_norms(sws),
          &segment_write_state,
          &segment_read_state,
          "norms",
          num_merged,
          info_stream.as_ref(),
        )?;
      }

      Self::merge_with_logging_with_name(
        |sws, srs| self.merge_terms(sws, srs),
        &segment_write_state,
        &segment_read_state,
        "postings",
        num_merged,
        info_stream.as_ref(),
      )?;

      if self.merge_state.merge_field_infos.has_doc_values() {
        Self::merge_with_logging_with_name(
          |sws, _srs| self.merge_doc_values(sws),
          &segment_write_state,
          &segment_read_state,
          "doc values",
          num_merged,
          info_stream.as_ref(),
        )?;
      }

      if self.merge_state.merge_field_infos.has_point_values() {
        Self::merge_with_logging_with_name(
          |sws, _srs| self.merge_points(sws),
          &segment_write_state,
          &segment_read_state,
          "points",
          num_merged,
          info_stream.as_ref(),
        )?;
      }

      if self.merge_state.merge_field_infos.has_vector_values() {
        Self::merge_with_logging_with_name(
          |sws, _srs| self.merge_vector_values(sws),
          &segment_write_state,
          &segment_read_state,
          "numeric vectors",
          num_merged,
          info_stream.as_ref(),
        )?;
      }
    }
    Self::merge_with_logging_with_name(
      |sws, srs| self.merge_field_infos_with_state(sws, srs),
      &segment_write_state,
      &segment_read_state,
      "field infos",
      num_merged,
      info_stream.as_ref(),
    )?;
    if self.merge_state.merge_field_infos.has_term_vectors() {
      Self::merge_with_logging(
        || self.merge_term_vectors(),
        "term vectors",
        info_stream.as_ref(),
      )?;
    }

    Ok(())
  }
}
