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
use crate::core::codecs::compressing::lucene90_compressing_term_vectors_format::Lucene90CompressingTermVectorsFormat;
use crate::core::codecs::compression::compression_mode::CompressionModeEnum;
use crate::core::codecs::term_vectors_format::TermVectorsFormat;
use crate::core::codecs::term_vectors_reader::TermVectorsReader;
use crate::core::codecs::term_vectors_writer::{DefaultTermVectorsWriter, TermVectorsWriter};
use crate::core::codecs::{Codec, Codecs};
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::fields::Fields;
use crate::core::index::postings_enum::{OFFSETS, PAYLOADS, PostingsEnum};
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::sorter::DocMap;
use crate::core::index::sorting_stored_fields_consumer::NoCompression;
use crate::core::index::term_vectors::TermVectors;
use crate::core::index::term_vectors_consumer::{
  TermVectorsConsumerBase, TermVectorsConsumerDefaults,
};
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::index::tracking_tmp_output_directory_wrapper::TrackingTmpOutputDirectoryWrapper;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::store::IOContext;
use crate::core::store::directory::Directory;
use crate::core::store::flush_info::FlushInfo;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::iterator::IteratorExt;
use crate::core::util::{IOUtils, ToInt};
use std::borrow::Cow;
use std::sync::Arc;

pub(crate) struct SortingTermVectorsConsumer<D>
where
  D: Directory,
{
  pub(crate) writer: Option<DefaultTermVectorsWriter<TrackingTmpOutputDirectoryWrapper<D>>>,
  pub(crate) tmp_directory: TrackingTmpOutputDirectoryWrapper<D>,
  tmp_term_vectors_format: Lucene90CompressingTermVectorsFormat,
}
impl<D> SortingTermVectorsConsumer<D>
where
  D: Directory + Clone,
{
  pub(crate) fn new(dir: D) -> Result<Self> {
    let tmp_term_vectors_format = Lucene90CompressingTermVectorsFormat::new(
      "TempTermVectors",
      "",
      CompressionModeEnum::Impl(NoCompression),
      8 * 1024,
      128,
      10,
    )?;
    let tmp_directory = TrackingTmpOutputDirectoryWrapper::new(dir);
    Ok(Self {
      writer: None,
      tmp_directory,
      tmp_term_vectors_format,
    })
  }

  fn write_term_vectors<TVW, F>(
    writer: &mut TVW,
    vectors: Option<&F>,
    field_infos: &Arc<FieldInfos>,
  ) -> Result<()>
  where
    TVW: TermVectorsWriter,
    F: Fields,
  {
    let vectors = match vectors {
      Some(v) => v,
      None => {
        writer.start_document(0)?;
        writer.finish_document()?;
        return Ok(());
      },
    };

    let mut num_fields = vectors.size()?;
    if num_fields == -1 {
      // count manually! TODO: Maybe enforce that Fields.size() returns something valid?
      let mut iter = vectors.iterator()?;
      while iter.has_next()? {
        match iter.next()? {
          Some(_) => {
            num_fields += 1;
          },
          None => break,
        }
      }
    }
    writer.start_document(num_fields)?;
    let mut last_field_name: Option<String> = None;
    let mut docs_and_positions = None;
    let mut field_count = 0;
    let mut terms_enum;
    let mut iter = vectors.iterator()?;
    while iter.has_next()? {
      match iter.next()? {
        Some(field_name) => {
          field_count += 1;
          let field_info = match field_infos.field_info_by_name(field_name)? {
            Some(fi) => fi,
            None => {
              return Err(LuceneError::illegal_state(format!(
                "Field '{field_name}' not found in FieldInfos"
              )));
            },
          };

          debug_assert!({
            let v = last_field_name.is_none()
              || field_name.cmp(last_field_name.as_ref().unwrap()).to_int() > 0;
            last_field_name = Some(field_name.clone());
            v
          });

          let terms = match vectors.terms(field_name)? {
            Some(t) => t,
            None => continue,
          };

          let has_positions = terms.has_positions();
          let has_offsets = terms.has_offsets();
          let has_payloads = terms.has_payloads();
          debug_assert!(!has_payloads || has_positions);

          let mut num_terms = terms.size()?;
          if num_terms == -1 {
            // count manually. It is stupid, but needed, as Terms.size() is not a mandatory statistics
            // function
            num_terms = 0;
            terms_enum = terms.iterator()?;
            while terms_enum.next()?.is_some() {
              num_terms += 1;
            }
          }
          writer.start_field(
            &field_info,
            num_terms as usize,
            has_positions,
            has_offsets,
            has_payloads,
          )?;
          terms_enum = terms.iterator()?;
          let mut term_count = 0;
          while terms_enum.next()?.is_some() {
            term_count += 1;

            let freq = terms_enum.total_term_freq()? as i32;
            writer.start_term(&*terms_enum.term()?, freq)?;

            if has_positions || has_offsets {
              docs_and_positions = Some(
                terms_enum.postings_with_flags(docs_and_positions, (OFFSETS | PAYLOADS) as i32)?,
              );
              match docs_and_positions {
                Some(ref mut dap) => {
                  let doc_id = dap.next_doc()?;
                  debug_assert!(doc_id != NO_MORE_DOCS);
                  debug_assert!(dap.freq()? == freq);

                  for _ in 0..freq {
                    let pos = dap.next_position()?;
                    let start_offset = dap.start_offset()?;
                    let end_offset = dap.end_offset()?;
                    let payload = dap.get_payload()?;
                    debug_assert!(!has_positions || pos >= 0);
                    writer.add_position(
                      pos,
                      start_offset,
                      end_offset,
                      payload.as_ref().map(Cow::as_ref),
                    )?;
                  }
                },
                None => {
                  debug_assert!(false, "docs_and_positions is None");
                },
              }
            }
            writer.finish_term()?;
          }
          debug_assert!(term_count == num_terms);
          writer.finish_field()?;
        },
        None => break,
      }
    }
    debug_assert!(field_count == num_fields);
    writer.finish_document()?;
    Ok(())
  }
}

impl<D> TermVectorsConsumerBase for SortingTermVectorsConsumer<D>
where
  D: Directory + Clone,
{
  type Directory = D;

  fn flush<DM, D1>(
    &mut self,
    codec: &Codecs,
    last_doc_id: &mut i32,
    state: &SegmentWriteState<Self::Directory>,
    sort_map: Option<&DM>,
    segment_info: &SegmentInfo<D1>,
  ) -> Result<()>
  where
    DM: DocMap,
    D1: Directory,
  {
    if self.writer.is_none() {
      return Ok(());
    }

    TermVectorsConsumerDefaults::flush(
      &mut self.writer,
      last_doc_id,
      &self.tmp_directory,
      segment_info,
    )?;

    let mut reader = self.tmp_term_vectors_format.vectors_reader(
      &self.tmp_directory,
      segment_info,
      state.field_infos.clone(),
      &IOContext::default_io_context()?,
    )?;
    // Don't pull a merge instance, since merge instances optimize for
    // sequential access while term vectors will likely be accessed in random
    // order here.
    let mut writer = codec.term_vectors_format().vectors_writer(
      state.directory,
      segment_info,
      &state.context.clone(),
    )?;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      reader.check_integrity()?;
      let max_doc = segment_info.max_doc()?;
      for doc_id in 0..max_doc {
        let read_id = match sort_map {
          Some(sm) => sm.new_to_old(doc_id)?,
          None => doc_id,
        };
        let vectors = reader.get(read_id)?;
        Self::write_term_vectors(&mut writer, vectors.as_ref(), &state.field_infos)?;
      }
      writer.finish(max_doc, state.directory)?;
      Ok(())
    }));

    let finally_result: Result<()> = (|| {
      let reader_close_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| reader.close()));
      let writer_close_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| writer.close()));
      let close_result = match reader_close_result {
        Ok(reader_close_result) => match writer_close_result {
          Ok(writer_close_result) => {
            IOUtils::use_or_suppress_result(reader_close_result, writer_close_result)
          },
          Err(payload) => match reader_close_result {
            Ok(()) => std::panic::resume_unwind(payload),
            Err(error) => Err(error),
          },
        },
        Err(payload) => std::panic::resume_unwind(payload),
      };
      close_result?;

      let file_names: Vec<String> = self
        .tmp_directory
        .get_temporary_files()
        .lock()
        .file_names
        .values()
        .cloned()
        .collect();
      IOUtils::delete_files(&self.tmp_directory, &file_names)?;
      Ok(())
    })();
    finally_result?;
    match result {
      Ok(result) => result,
      Err(payload) => std::panic::resume_unwind(payload),
    }
  }

  fn init_term_vectors_writer<D1>(
    &mut self,
    _directory: &Self::Directory,
    _codec: &Codecs,
    last_doc_id: &mut i32,
    info: &SegmentInfo<D1>,
    bytes_used: i64,
  ) -> Result<()>
  where
    D1: Directory,
  {
    if self.writer.is_none() {
      let context = IOContext::with_flush(FlushInfo::new(*last_doc_id, bytes_used))?;
      self.writer = Option::from(self.tmp_term_vectors_format.vectors_writer(
        self.tmp_directory.clone(),
        info,
        &context,
      )?);
      *last_doc_id = 0;
    }
    Ok(())
  }

  fn abort(&mut self) -> Result<()> {
    let abort_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      TermVectorsConsumerDefaults::abort(&mut self.writer)
    }));
    let file_names: Vec<String> = self
      .tmp_directory
      .get_temporary_files()
      .lock()
      .file_names
      .values()
      .cloned()
      .collect();
    IOUtils::delete_files_ignoring_exceptions(&self.tmp_directory, &file_names);
    match abort_result {
      Ok(result) => result,
      Err(payload) => std::panic::resume_unwind(payload),
    }
  }
}
