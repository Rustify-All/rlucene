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
use crate::core::codecs::compound_directory::CompoundDirectoryEnum;
use crate::core::codecs::field_infos_format::FieldInfosFormat;
use crate::core::codecs::norms_format::NormsFormat;
use crate::core::codecs::points_format::PointsFormat;
use crate::core::codecs::postings_format::PostingsFormat;
use crate::core::codecs::stored_fields_format::StoredFieldsFormat;

use crate::core::codecs::term_vectors_format::TermVectorsFormat;

use crate::core::codecs::{
  Codec, CodecFieldsProducer, CodecKnnVectorsReader, CodecNormsProducer, CodecPointsReader,
  CodecStoredFieldsReader, CodecTermVectorsReader, CompoundFormat, DefaultCompoundReaderImpl,
};
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::store::IOContext;
use crate::core::store::directory::Directory;
use crate::core::store::index_input::IndexInput;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::io_utils::IOUtils;

use crate::core::codecs::knn_vectors_format::KnnVectorsFormat;
use crate::core::index::index_reader::{CacheHelper, CacheKey, ClosedListener, ClosedListenerList};
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

/// Holds core readers that are shared (unchanged) when SegmentReader is cloned or reopened
pub(crate) struct SegmentCoreReaders<I>
where
  I: IndexInput<IndexInput = I> + Send + Sync,
  I::RandomAccessSlice: Send + Sync,
{
  pub(crate) ref_: AtomicI32,
  pub(crate) fields: Option<Arc<CodecFieldsProducer<I>>>,
  pub(crate) norms_producer: Option<Arc<CodecNormsProducer<I>>>,
  pub(crate) fields_reader_orig: CodecStoredFieldsReader<I>,
  pub(crate) term_vectors_reader_orig: Option<CodecTermVectorsReader<I>>,
  pub(crate) points_reader: Option<Arc<CodecPointsReader<I>>>,
  pub(crate) knn_vectors_reader: Option<Arc<CodecKnnVectorsReader<I>>>,
  pub(crate) cfs_reader: Option<DefaultCompoundReaderImpl<I>>,
  pub(crate) segment: String,
  /// fieldinfos for this core: means gen=-1. this is the exact fieldinfos these codec components saw at write.
  /// in the case of DV updates, SR may hold a newer version.
  pub(crate) core_field_infos: Arc<FieldInfos>,
  pub(crate) cache_helper: SegmentCoreReadersCacheHelperImpl,
}

impl<I> SegmentCoreReaders<I>
where
  I: IndexInput<IndexInput = I> + Send + Sync,
  I::RandomAccessSlice: Send + Sync,
{
  #[allow(clippy::too_many_arguments)]
  pub(crate) fn new<D>(dir: &D, si: &SegmentCommitInfo<D>, context: &IOContext) -> Result<Self>
  where
    D: Directory<IndexInput = I>,
  {
    let codec = si.info.get_codec()?;
    let use_compound_file = si.info.get_use_compound_file();

    let mut cfs_reader = None;
    let mut fields = None;
    let mut norms_producer = None;
    let mut fields_reader_orig = None;
    let mut term_vectors_reader_orig = None;
    let mut points_reader = None;
    let mut knn_vectors_reader = None;

    let mut success = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<_> {
      cfs_reader = if use_compound_file {
        Some(codec.compound_format().get_compound_reader(dir, &si.info)?)
      } else {
        None
      };

      let cfs_dir = match cfs_reader.as_ref() {
        Some(reader) => CompoundDirectoryEnum::A(reader),
        None => CompoundDirectoryEnum::B(dir),
      };

      let segment = si.info.name.to_string();
      let core_field_infos = Arc::new(
        codec
          .field_infos_format()
          .read(&cfs_dir, &si.info, "", context)?,
      );

      fields_reader_orig = Some(codec.stored_fields_format().fields_reader(
        &cfs_dir,
        &si.info,
        core_field_infos.clone(),
        context,
      )?);

      term_vectors_reader_orig = if core_field_infos.has_term_vectors() {
        Some(codec.term_vectors_format().vectors_reader(
          &cfs_dir,
          &si.info,
          core_field_infos.clone(),
          context,
        )?)
      } else {
        None
      };

      let read_state = SegmentReadState::new(&cfs_dir, core_field_infos.clone(), context);

      fields = if core_field_infos.has_postings() {
        Some(Arc::new(
          codec
            .postings_format()
            .fields_producer(&read_state, &si.info)?,
        ))
      } else {
        None
      };

      norms_producer = if core_field_infos.has_norms() {
        Some(Arc::new(
          codec.norms_format().norms_producer(&read_state, &si.info)?,
        ))
      } else {
        None
      };
      points_reader = if core_field_infos.has_point_values() {
        Some(Arc::new(
          codec.points_format().fields_reader(&read_state, &si.info)?,
        ))
      } else {
        None
      };
      knn_vectors_reader = if core_field_infos.has_vector_values() {
        Some(Arc::new(
          codec
            .knn_vectors_format()?
            .fields_reader(&read_state, &si.info)?,
        ))
      } else {
        None
      };

      success = true;
      Ok((segment, core_field_infos))
    }));
    let result = match result {
      Ok(Err(error)) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let is_unexpected_file_read_error = match &error {
          LuceneError::Eof(_) | LuceneError::NoSuchFile(_) => true,
          LuceneError::Io { source, .. } | LuceneError::IoWithPath { source, .. } => matches!(
            source.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::UnexpectedEof
          ),
          _ => false,
        };
        if is_unexpected_file_read_error {
          let mut corrupt = LuceneError::corrupt_index(format!(
            "Problem reading index from {dir} (resource={dir})"
          ));
          corrupt.add_suppressed(error);
          Err(corrupt)
        } else {
          Err(error)
        }
      })),
      result => result,
    };

    if !success {
      IOUtils::close_refs_tuple((
        fields.as_ref(),
        term_vectors_reader_orig.as_ref(),
        fields_reader_orig.as_ref(),
        cfs_reader.as_ref(),
        norms_producer.as_ref(),
        points_reader.as_ref(),
        knn_vectors_reader.as_ref(),
      ))?;
    }
    let (segment, core_field_infos) = unwrap_caught_result!(result)?;
    let fields_reader_orig = match fields_reader_orig.take() {
      Some(fields_reader_orig) => fields_reader_orig,
      None => {
        IOUtils::close_refs_tuple((
          fields.as_ref(),
          term_vectors_reader_orig.as_ref(),
          cfs_reader.as_ref(),
          norms_producer.as_ref(),
          points_reader.as_ref(),
          knn_vectors_reader.as_ref(),
        ))?;
        return Err(LuceneError::illegal_state(
          "stored fields reader is missing after successful construction",
        ));
      },
    };
    Ok(SegmentCoreReaders {
      ref_: AtomicI32::new(1),
      fields,
      norms_producer,
      fields_reader_orig,
      term_vectors_reader_orig,
      points_reader,
      knn_vectors_reader,
      cfs_reader,
      segment,
      core_field_infos,
      cache_helper: SegmentCoreReadersCacheHelperImpl::new(),
    })
  }

  pub(crate) fn get_ref_count(&self) -> i32 {
    self.ref_.load(Ordering::SeqCst)
  }

  pub(crate) fn inc_ref(&self) -> Result<()> {
    loop {
      let count = self.ref_.load(Ordering::SeqCst);

      if count == 0 {
        return Err(LuceneError::already_closed(
          "SegmentCoreReaders is already closed".to_string(),
        ));
      }
      if count == i32::MAX {
        return Err(LuceneError::illegal_state("ref_count overflow".to_string()));
      }

      match self
        .ref_
        .compare_exchange_weak(count, count + 1, Ordering::SeqCst, Ordering::SeqCst)
      {
        Ok(_) => return Ok(()),
        Err(_) => continue,
      }
    }
  }
  pub(crate) fn dec_ref(&self) -> Result<()> {
    let count = self.ref_.fetch_sub(1, Ordering::SeqCst) - 1;
    if count == 0 {
      IOUtils::close(0..2, |operation| match operation {
        0 => IOUtils::close_refs_tuple((
          self.fields.as_ref(),
          self.term_vectors_reader_orig.as_ref(),
          Some(&self.fields_reader_orig),
          self.cfs_reader.as_ref(),
          self.norms_producer.as_ref(),
          self.points_reader.as_ref(),
          self.knn_vectors_reader.as_ref(),
        )),
        1 => self.cache_helper.notify_core_closed_listeners(),
        _ => unreachable!(),
      })?;
    }
    Ok(())
  }
  pub(crate) fn get_cache_helper_ref(&self) -> &SegmentCoreReadersCacheHelperImpl {
    &self.cache_helper
  }
  pub(crate) fn get_cache_helper(&self) -> SegmentCoreReadersCacheHelperImpl {
    self.cache_helper.clone()
  }
}
#[derive(Clone)]
pub struct SegmentCoreReadersCacheHelperImpl {
  cache_key: CacheKey,
  core_closed_listeners: ClosedListenerList,
}
impl Default for SegmentCoreReadersCacheHelperImpl {
  fn default() -> Self {
    Self::new()
  }
}

impl SegmentCoreReadersCacheHelperImpl {
  pub fn new() -> Self {
    Self {
      cache_key: CacheKey::new(),
      core_closed_listeners: Arc::new(Mutex::new(Some(Vec::new()))),
    }
  }

  fn notify_core_closed_listeners(&self) -> Result<()> {
    let mut core_closed_listeners = self.core_closed_listeners.lock();
    let listeners = core_closed_listeners.take().unwrap_or_default();
    IOUtils::apply_to_all(&listeners, |listener| listener.on_close(&self.cache_key))
  }
}
impl CacheHelper for SegmentCoreReadersCacheHelperImpl {
  fn get_key(&self) -> CacheKey {
    self.cache_key.clone()
  }

  fn add_closed_listener(&self, listener: Arc<dyn ClosedListener>) -> Result<()> {
    let mut core_closed_listeners = self.core_closed_listeners.lock();
    let Some(core_closed_listeners) = core_closed_listeners.as_mut() else {
      return Err(LuceneError::already_closed(
        "SegmentCoreReaders is already closed".to_string(),
      ));
    };
    if !core_closed_listeners
      .iter()
      .any(|existing| Arc::ptr_eq(existing, &listener))
    {
      core_closed_listeners.push(listener);
    }
    Ok(())
  }
}
