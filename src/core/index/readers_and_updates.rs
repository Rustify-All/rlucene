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
use crate::core::codecs::doc_values_producer::DocValuesProducer;
use crate::core::codecs::dummy::dummy_binary_doc_values::DummyBinaryDocValues;
use crate::core::codecs::dummy::dummy_doc_values_skipper::DummyDocValuesSkipper;
use crate::core::codecs::dummy::dummy_numeric_doc_values::DummyNumericDocValues;
use crate::core::codecs::dummy::dummy_sorted_doc_values::DummySortedDocValues;
use crate::core::codecs::dummy::dummy_sorted_numeric_doc_values::DummySortedNumericDocValues;
use crate::core::codecs::dummy::dummy_sorted_set_doc_values::DummySortedSetDocValues;
use crate::core::codecs::field_infos_format::FieldInfosFormat;
use crate::core::index::binary_doc_values::BinaryDocValues;
use crate::core::index::doc_values_field_updates::{
  BinaryDocValuesDVFU, DocValuesFieldIterator, DocValuesFieldIteratorEnum,
  DocValuesFieldUpdatesEnum, MergedFieldIterator, MergedIterator, NumericDocValuesDVFU,
};
use crate::core::index::doc_values_field_updates::{merged_field_iterator, merged_iterator};
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::{FieldInfos, FieldNumbersLock};
use crate::core::index::index_reader::IndexReader;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::merge_policy::{MergePolicy, MergeReader, MergeReaderSR};
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::pending_deletes::{DocBits, PendingDeletesBase, PendingDeletesEnum};
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_reader::{DefaultLeafReader, SegmentReader};
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::sorter::DocMapImpl;
use crate::core::index::{BytesRef, pending_soft_deletes};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::DocIdSetIteratorEnum2;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::store::IO_CONTEXT_DEFAULT;
use crate::core::store::IOContext;
use crate::core::store::directory::Directory;
use crate::core::store::flush_info::FlushInfo;
use crate::core::store::tracking_directory_wrapper::TrackingDirectoryWrapper;
use crate::core::util::IOUtils;
use crate::core::util::accountable::Accountable;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::function::Function;
use crate::core::util::info_stream::{InfoStream, NO_OUTPUT_INFO_STREAM};
#[cfg(test)]
use crate::test_framework::core::util::failure_context::{
  ExecutionMethod, ExecutionOwner, ExecutionScope,
};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, AtomicI64, Ordering};

type DocValuesUpdateList = smallvec::SmallVec<[Arc<DocValuesFieldUpdatesEnum>; 1]>;

/// Used by IndexWriter to hold open SegmentReaders (for
/// searching or merging), plus pending deletes and updates,
/// for a given segment
pub(crate) struct ReadersAndUpdates<D>
where
  D: Directory,
{
  // Tracks how many consumers are using this instance:
  ref_count: AtomicI32, // starts at 1
  // the major version this index was created with
  pub(crate) index_created_version_major: i32,
  // Only set if there are doc values updates against this segment, and the index is sorted:
  pub(crate) sort_map: Option<Arc<DocMapImpl>>,
  pub(crate) ram_bytes_used: AtomicI64,
  pub(crate) inner: Mutex<Inner<D>>,
  pub(crate) info_id: Arc<str>,
}

pub(crate) struct Inner<D>
where
  D: Directory,
{
  // Set once (None, and then maybe set, and never set again):
  pub(crate) reader: Option<DefaultLeafReader<D>>,
  // How many further deletions we've done against
  // liveDocs vs when we loaded it or last wrote it:
  pub(crate) pending_deletes: PendingDeletesEnum,
  // Indicates whether this segment is currently being merged. While a segment
  // is merging, all field updates are also registered in the
  // mergingDVUpdates map. Also, calls to writeFieldUpdates merge the
  // updates with mergingDVUpdates.
  // That way, when the segment is done merging, IndexWriter can apply the
  // updates on the merged segment too.
  is_merging: bool,
  // Holds resolved (to docIDs) doc values updates that have not yet been
  // written to the index
  pending_dv_updates: HashMap<Arc<str>, DocValuesUpdateList>,
  // Holds resolved (to docIDs) doc values updates that were resolved while
  // this segment was being merged; at the end of the merge we carry over
  // these updates (remapping their docIDs) to the newly merged segment
  merging_dv_updates: HashMap<Arc<str>, DocValuesUpdateList>,
}

impl<D> ReadersAndUpdates<D>
where
  D: Directory,
{
  pub(crate) fn new(
    index_created_version_major: i32,
    info_id: Arc<str>,
    pending_deletes: PendingDeletesEnum,
  ) -> Self {
    let inner = Mutex::new(Inner {
      reader: None,
      pending_deletes,
      is_merging: false,
      pending_dv_updates: HashMap::new(),
      merging_dv_updates: HashMap::new(),
    });
    Self {
      ref_count: AtomicI32::new(1),
      index_created_version_major,
      sort_map: None,
      ram_bytes_used: AtomicI64::new(0),
      inner,
      info_id,
    }
  }
  /// Init from a previously opened SegmentReader.
  pub(crate) fn with_reader(
    index_created_version_major: i32,
    reader: DefaultLeafReader<D>,
    info: &SegmentCommitInfo<D>,
    pending_deletes: PendingDeletesEnum,
  ) -> Result<Self> {
    debug_assert!(reader.get_original_segment_info_id() == pending_deletes.get_info_id());
    let info_id = Arc::clone(&reader.original_si_id);
    let v = Self::new(index_created_version_major, info_id, pending_deletes);
    {
      let mut inner = v.inner.lock();
      inner.pending_deletes.on_new_reader(&reader, info)?;
      inner.reader = Some(reader);
    }
    Ok(v)
  }
  pub fn inc_ref(&self) {
    let rc = self.ref_count.fetch_add(1, Ordering::SeqCst) + 1;
    debug_assert!(rc > 1);
  }

  pub fn dec_ref(&self) {
    let rc = self.ref_count.fetch_sub(1, Ordering::SeqCst) - 1;
    debug_assert!(rc >= 0);
  }

  pub fn ref_count(&self) -> i32 {
    let rc = self.ref_count.load(Ordering::SeqCst);
    debug_assert!(rc >= 0);
    rc
  }
  pub(crate) fn get_del_count(&self, info: &SegmentCommitInfo<D>) -> i32
  where
    D: Directory,
  {
    self.inner.lock().pending_deletes.get_del_count(info)
  }

  fn assert_no_dup_gen(
    &self,
    field_updates: &[Arc<DocValuesFieldUpdatesEnum>],
    update: &DocValuesFieldUpdatesEnum,
  ) -> bool {
    let dup = field_updates
      .iter()
      .any(|old_update| old_update.del_gen == update.del_gen);
    debug_assert!(!dup, "duplicate delGen={}", update.del_gen);
    true
  }
  /// Adds a new resolved (meaning it maps docIDs to new values) doc values packet.
  /// We buffer these in RAM and write to disk when too much RAM is used or when a merge needs to kick off, or a commit/refresh.
  pub fn add_dv_update<T>(&self, update: T) -> Result<()>
  where
    T: Into<Arc<DocValuesFieldUpdatesEnum>>,
  {
    let update = update.into();
    let mut inner = self.inner.lock();
    if !update.get_finished() {
      return Err(LuceneError::illegal_argument("call finish first"));
    }

    let field = &update.field;
    let update_bytes = update.ram_bytes_used()?;
    let is_merging = inner.is_merging;

    let field_updates = if let Some(updates) = inner.pending_dv_updates.get_mut(field.as_ref()) {
      updates
    } else {
      inner
        .pending_dv_updates
        .entry(Arc::clone(field))
        .or_default()
    };

    debug_assert!(self.assert_no_dup_gen(field_updates, &update));
    self
      .ram_bytes_used
      .fetch_add(update_bytes, Ordering::SeqCst);

    if is_merging {
      field_updates.push(update.clone());
      let merging_updates = if let Some(updates) = inner.merging_dv_updates.get_mut(field.as_ref())
      {
        updates
      } else {
        inner
          .merging_dv_updates
          .entry(Arc::clone(field))
          .or_default()
      };
      merging_updates.push(update);
    } else {
      field_updates.push(update);
    }
    Ok(())
  }

  pub(crate) fn get_num_dv_updates(&self) -> usize {
    let inner = self.inner.lock();
    let mut count = 0usize;
    for updates in inner.pending_dv_updates.values() {
      count += updates.len();
    }
    count
  }

  pub fn release(&self, sr: &SegmentReader<D>, inner: Option<&Inner<D>>) -> Result<()> {
    let _inner = match inner {
      Some(inner) => inner,
      None => &mut *self.inner.lock(),
    };
    debug_assert!(self.info_id.as_ref() == sr.get_original_segment_info_id());
    sr.dec_ref()?;
    Ok(())
  }

  pub fn delete(
    &self,
    doc_id: i32,
    info: &SegmentCommitInfo<D>,
    inner: Option<&mut Inner<D>>,
  ) -> Result<bool> {
    let inner = match inner {
      Some(inner) => inner,
      None => &mut *self.inner.lock(),
    };
    if inner.reader.is_none() && inner.pending_deletes.must_init_on_delete() {
      get_reader(
        IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?,
        info,
        inner,
        self.index_created_version_major,
      )?
      .dec_ref()?; // pass a reader to initialize the pending deletes
    }

    inner.pending_deletes.delete(doc_id, info)
  }
  pub fn drop_readers(&self) -> Result<()> {
    let mut inner = self.inner.lock();
    if let Some(reader) = inner.reader.take() {
      reader.dec_ref()?;
    }
    self.dec_ref();
    Ok(())
  }
  /// Returns a ref to a clone. NOTE: you should decRef() the reader when you're done (ie do not call close()).
  pub(crate) fn get_read_only_clone(
    &self,
    context: &IOContext,
    info: &SegmentCommitInfo<D>,
  ) -> Result<Option<DefaultLeafReader<D>>> {
    #[cfg(test)]
    let _execution_scope = ExecutionScope::enter(
      ExecutionOwner::ReadersAndUpdates,
      ExecutionMethod::GetReadOnlyClone,
    );
    let mut inner = self.inner.lock();
    if inner.reader.is_none() {
      get_reader(context, info, &mut inner, self.index_created_version_major)?.dec_ref()?;
      debug_assert!(inner.reader.is_some());
    }

    // force new liveDocs
    if let Some(live_docs) = inner.pending_deletes.get_live_docs() {
      let hard_live_docs = inner.pending_deletes.get_hard_live_docs();
      let sr = SegmentReader::new_from_reader(
        info,
        inner
          .reader
          .as_ref()
          .ok_or_else(|| LuceneError::illegal_state("reader was not initialized"))?,
        Some(live_docs),
        hard_live_docs,
        inner.pending_deletes.num_docs(info)?,
        true,
      )?;
      return Ok(Some(Arc::new(sr)));
    }
    {
      // liveDocs == None and reader != None. That can only be if there are no deletes
      let r = inner
        .reader
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("reader was not initialized"))?;
      debug_assert!(r.get_live_docs()?.is_none());
      r.inc_ref()?;
      Ok(inner.reader.clone())
    }
  }

  pub(crate) fn num_deletes_to_merge<P>(
    &self,
    policy: &P,
    info: &SegmentCommitInfo<D>,
  ) -> Result<i32>
  where
    P: MergePolicy<D> + ?Sized,
  {
    let del_count = {
      let mut inner = self.inner.lock();
      let is_soft = matches!(inner.pending_deletes, PendingDeletesEnum::Soft(_));
      if is_soft {
        let dv_gen = inner.pending_deletes.dv_gen()?;
        let (reader, field_infos, on_new_reader) = if dv_gen == -2 {
          let field_infos = pending_soft_deletes::read_field_infos(info)?;
          let field_info = field_infos.field_info_by_name(inner.pending_deletes.field()?)?;
          let on_new_reader = pending_soft_deletes::do_on_new_reader(field_info);
          let reader = if on_new_reader {
            Some(self.get_latest_read(info, &mut inner, self.index_created_version_major)?)
          } else {
            None
          };
          (reader, Some(field_infos), on_new_reader)
        } else {
          (None, None, false)
        };
        inner.pending_deletes.num_deletes_to_merge(
          info,
          reader.as_ref(),
          field_infos,
          on_new_reader,
        )?;
      } else {
        inner
          .pending_deletes
          .num_deletes_to_merge(info, None, None, false)?;
      };
      inner.pending_deletes.get_del_count(info)
    };

    policy.num_deletes_to_merge(info, del_count, &|| {
      let mut inner = self.inner.lock();
      self.get_latest_read(info, &mut inner, self.index_created_version_major)
    })
  }

  /// Returns a snapshot of the live docs.
  pub fn get_live_docs(&self) -> Option<DocBits> {
    let mut inner = self.inner.lock();
    inner.pending_deletes.get_live_docs()
  }

  /// Returns the live-docs bits excluding documents that are not live due to soft-deletes.
  pub fn get_hard_live_docs(&self) -> Option<DocBits> {
    let mut inner = self.inner.lock();
    inner.pending_deletes.get_hard_live_docs()
  }
  pub fn drop_changes(&self) {
    // Discard (don't save) changes when we are dropping
    // the reader; this is used only on the sub-readers
    // after a successful merge.  If deletes had
    // accumulated on those sub-readers while the merge
    // is running, by now we have carried forward those
    // deletes onto the newly merged segment, so we can
    // discard them on the sub-readers:
    let mut inner = self.inner.lock();
    inner.pending_deletes.drop_changes();
    self.drop_merging_updates(Some(&mut inner));
  }
  // Commit live docs (writes new _X_N.del files) and field updates (writes new
  // _X_N updates files) to the directory; returns true if it wrote any file
  // and false if there were no new deletes or updates to write:
  pub fn write_live_docs<D1>(&self, dir: &D1, info: &mut SegmentCommitInfo<D>) -> Result<bool>
  where
    D: Directory,
    D1: Directory,
  {
    let mut inner = self.inner.lock();
    inner.pending_deletes.write_live_docs(dir, info)
  }

  #[allow(clippy::too_many_arguments)]
  pub fn handle_dv_updates<D1, F, IS>(
    &self,
    infos: &FieldInfos,
    dir: D1,
    dv_format: &F,
    inner: &mut Inner<D>,
    reader: &SegmentReader<D>,
    field_files: &mut HashMap<i32, HashSet<String>>,
    max_del_gen: i64,
    info_stream: &IS,
    info: &mut SegmentCommitInfo<D>,
  ) -> Result<()>
  where
    F: DocValuesFormat,
    D1: Directory,
    IS: InfoStream,
  {
    let mut updates_to_apply = smallvec::SmallVec::<[&Arc<DocValuesFieldUpdatesEnum>; 4]>::new();
    for (field, updates) in inner.pending_dv_updates.iter() {
      let ty = updates[0].type_;
      debug_assert!(
        matches!(ty, DocValuesType::Numeric | DocValuesType::Binary),
        "unsupported type: {:?}",
        ty
      );

      updates_to_apply.clear();
      let mut bytes: i64 = 0;

      for update in updates {
        if update.del_gen <= max_del_gen {
          // safe to apply this one
          bytes += update.ram_bytes_used()?;
          updates_to_apply.push(update);
        }
      }

      if updates_to_apply.is_empty() {
        // nothing to apply yet
        continue;
      }

      if info_stream.is_enabled("BD") {
        info_stream.message(
          "BD",
          &format!(
            "now write {} pending numeric DV updates for field={}, seg={}, bytes={:.3} MB",
            updates_to_apply.len(),
            field,
            info,
            (bytes as f64) / 1024.0 / 1024.0
          ),
        )?;
      }

      let next_doc_values_gen = info.get_next_doc_values_gen();
      let segment_suffix =
        crate::core::index::index_file_names::base36(next_doc_values_gen, &mut [0; 14]).to_owned();
      let updates_context = IOContext::with_flush(FlushInfo::new(info.info.max_doc()?, bytes))?;

      let field_info = infos
        .field_info_by_name(field)?
        .ok_or_else(|| LuceneError::illegal_argument("fieldInfo is None"))?;
      field_info.set_doc_values_gen(next_doc_values_gen)?;

      let field_infos = Arc::new(FieldInfos::new(vec![field_info.clone()])?);

      let tracking_dir = TrackingDirectoryWrapper::new(&dir);

      let state = SegmentWriteState::with_suffix(
        Arc::clone(&NO_OUTPUT_INFO_STREAM),
        &tracking_dir,
        field_infos,
        &updates_context,
        segment_suffix,
      );

      {
        let mut fields_consumer = dv_format.fields_consumer(&state, &info.info)?;

        let write_result =
          std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
            let update_supplier = FunctionImpl::new(field_info, &updates_to_apply);

            inner.pending_deletes.on_doc_values_update(
              field_info,
              update_supplier.apply_legacy(field_info)?,
              info,
            )?;
            if ty == DocValuesType::Binary {
              let v = DocValuesProducerBinary::new(update_supplier, field, reader, field_info);
              fields_consumer.add_binary_field(&state, &info.info, field_info, &v)?
            } else {
              let v = DocValuesProducerNumeric::new(update_supplier, field, reader, field_info);
              fields_consumer.add_numeric_field(&state, &info.info, field_info, &v)?;
            }
            Ok(())
          }));
        let close_result =
          std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| fields_consumer.close()));
        IOUtils::use_or_suppress_caught_result(write_result, close_result)?;
      }

      info.advance_doc_values_gen();
      debug_assert!(!field_files.contains_key(&field_info.number));
      field_files.insert(field_info.number, state.directory.take_created_files());
    }
    Ok(())
  }

  fn write_field_infos_gen<D1, F>(
    &self,
    field_infos: &FieldInfos,
    dir: D1,
    infos_format: &F,
    info: &mut SegmentCommitInfo<D>,
  ) -> Result<HashSet<String>>
  where
    F: FieldInfosFormat,
    D1: Directory,
  {
    let next_field_infos_gen = info.get_next_field_infos_gen();
    let mut suffix_digits = [0; 14];
    let segment_suffix =
      crate::core::index::index_file_names::base36(next_field_infos_gen, &mut suffix_digits);
    // we write approximately that many bytes (based on Lucene46DVF):
    // HEADER + FOOTER: 40
    // 90 bytes per-field (over estimating long name and attributes map)
    let est_infos_size = 40 + 90 * (field_infos.size() as i64);
    // IOContext for a flush with estimated size
    let flush_info = FlushInfo::new(info.info.max_doc()?, est_infos_size);
    let infos_context = IOContext::with_flush(flush_info)?;
    // separately also track which files were created for this gen
    let tracking_dir = TrackingDirectoryWrapper::new(dir);
    infos_format.write(
      &tracking_dir,
      &info.info,
      segment_suffix,
      field_infos,
      &infos_context,
    )?;
    info.advance_field_infos_gen();
    Ok(tracking_dir.take_created_files())
  }
  pub fn write_field_updates<D1, IS>(
    &self,
    dir: D1,
    field_numbers: &FieldNumbersLock,
    max_del_gen: i64,
    info_stream: &IS,
    info: Option<&mut SegmentCommitInfo<D>>,
  ) -> Result<bool>
  where
    D1: Directory,
    IS: InfoStream,
  {
    let mut inner = self.inner.lock();
    let start_time_ns = std::time::Instant::now();

    let mut any = false;
    'outer: for updates in inner.pending_dv_updates.values() {
      for update in updates {
        if update.del_gen <= max_del_gen && update.any() {
          any = true;
          break 'outer;
        }
      }
    }
    if !any {
      // no updates
      return Ok(false);
    }

    let info = info.ok_or_else(|| LuceneError::illegal_state("SegmentCommitInfo missing"))?;
    let mut new_dv_files = HashMap::with_capacity(inner.pending_dv_updates.len());
    let mut field_infos_files: Option<HashSet<String>> = None;
    let mut field_infos = FieldInfos::default();

    // Do this so we can delete any created files on
    // error; this saves all codecs from having to do it:
    let tracking_dir = TrackingDirectoryWrapper::new(&dir);

    let mut success = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      let codec = info.info.get_codec()?.clone();

      let reader = if let Some(reader) = inner.reader.as_ref() {
        Arc::clone(reader)
      } else {
        let reader = Arc::new(SegmentReader::new(
          info,
          self.index_created_version_major,
          &IOContext::read_once_io_context()?,
        )?);
        inner.pending_deletes.on_new_reader(&reader, info)?;
        reader
      };

      let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        // clone FieldInfos so that we can update their dvGen separately from
        // the reader's infos and write them to a new fieldInfos_gen file.
        let mut max_field_number: i32 = -1;
        let reader_field_infos = reader.get_field_infos()?;
        let mut updated_infos = Vec::with_capacity(reader_field_infos.size());

        for fi in reader_field_infos.iter() {
          // cannot use builder.add(fi) because it does not preserve
          // the local field number. Field numbers can be different from
          // the global ones if the segment was created externally (and added to
          // this index with `IndexWriter::add_indexes(Directory)`.)
          updated_infos.push(Arc::new(clone_field_info(fi, fi.number)?));
          max_field_number = max_field_number.max(fi.number);
        }

        // create new fields with the right DV type for updates whose field doesn't yet exist
        for updates in inner.pending_dv_updates.values() {
          if let Some(update) = updates.first() {
            let field = &update.field;
            if let Some(fi) = reader_field_infos.field_info_by_name(field)? {
              // the field already exists in this segment
              debug_assert_eq!(*fi.get_doc_values_type(), update.type_);
            } else {
              // the field is not present in this segment so we clone the global field
              // (which is guaranteed to exist) and remaps its field number locally.
              let fi = field_numbers.lock().construct_field_info(
                field,
                update.type_,
                max_field_number + 1,
              )?;
              if let Some(fi) = fi {
                max_field_number += 1;
                updated_infos.push(Arc::new(fi));
              } else {
                debug_assert!(false);
              }
            }
          }
        }

        field_infos = FieldInfos::new(updated_infos)?;

        let dv_format = codec.doc_values_format();

        self.handle_dv_updates(
          &field_infos,
          &tracking_dir,
          &dv_format,
          &mut inner,
          &reader,
          &mut new_dv_files,
          max_del_gen,
          info_stream,
          info,
        )?;

        let files = self.write_field_infos_gen(
          &field_infos,
          &tracking_dir,
          &codec.field_infos_format(),
          info,
        )?;
        field_infos_files = Some(files);

        Ok(())
      }));

      if !matches!(
        inner.reader.as_ref(),
        Some(pooled_reader) if Arc::ptr_eq(&reader, pooled_reader)
      ) {
        reader.close()?;
      }

      unwrap_caught_result!(result)?;
      success = true;
      Ok(())
    }));

    if !success {
      info.advance_next_write_field_infos_gen();
      info.advance_next_write_doc_values_gen();
      IOUtils::delete_files_ignoring_exceptions(&dir, tracking_dir.take_created_files());
    }
    unwrap_caught_result!(result)?;
    // Prune the now-written DV updates:
    let mut bytes_freed: i64 = 0;
    for updates in inner.pending_dv_updates.values_mut() {
      let mut upto = 0;
      for read in 0..updates.len() {
        if updates[read].del_gen > max_del_gen {
          updates.swap(upto, read);
          upto += 1;
        } else {
          bytes_freed += updates[read].ram_bytes_used()?;
        }
      }
      updates.truncate(upto);
    }
    inner
      .pending_dv_updates
      .retain(|_, updates| !updates.is_empty());

    let prev = self.ram_bytes_used.fetch_sub(bytes_freed, Ordering::SeqCst);
    let bytes_now = prev - bytes_freed;
    debug_assert!(bytes_now >= 0, "ram_bytes_used should not go negative");
    // writing field updates succeeded
    debug_assert!(field_infos_files.is_some());
    let field_infos_files = field_infos_files
      .take()
      .ok_or_else(|| LuceneError::illegal_state("field infos files were not written"))?;
    info.set_owned_field_infos_files(field_infos_files);
    // update the doc-values updates files. the files map each field to its set
    // of files, hence we copy from the existing map all fields w/ updates that
    // were not updated in this session, and add new mappings for fields that
    // were updated now.
    debug_assert!(!new_dv_files.is_empty());

    let existing_dv_files = info.take_doc_values_updates_files();
    for (field_num, files_set) in existing_dv_files {
      new_dv_files.entry(field_num).or_insert(files_set);
    }
    info.set_doc_values_updates_files(std::borrow::Cow::Owned(new_dv_files));
    // if there is a reader open, reopen it to reflect the updates
    if inner.reader.is_some() {
      swap_new_reader_with_latest_live_docs(&mut inner, info)?;
    }
    if info_stream.is_enabled("BD") {
      let message = format!(
        "done write field updates for seg={}; took {:.3}s; new files: {:?}",
        info,
        start_time_ns.elapsed().as_secs_f64(),
        info.get_doc_values_updates_files(),
      );
      info_stream.message("BD", &message)?;
    }
    Ok(true)
  }

  pub(crate) fn set_is_merging(&self) {
    let mut inner = self.inner.lock();
    if !inner.is_merging {
      inner.is_merging = true;
      debug_assert!(inner.merging_dv_updates.is_empty());
    }
  }

  #[cfg(test)]
  pub(crate) fn is_merging(&self) -> bool {
    let inner = self.inner.lock();
    inner.is_merging
  }

  /// Returns a reader for merge, with the latest doc values updates and deletions.
  pub(crate) fn get_reader_for_merge<F>(
    &self,
    context: &IOContext,
    info: &SegmentCommitInfo<D>,
    reader_consumer: F,
  ) -> Result<MergeReaderSR<D>>
  where
    F: FnOnce(&MergeReaderSR<D>) -> Result<()>,
  {
    // We must carry over any still-pending DV updates because they were not
    // successfully written, e.g. because there was a hole in the delGens,
    // or they arrived after we wrote all DVs for merge but before we set
    // isMerging here:
    let mut inner = self.inner.lock();
    let Inner {
      pending_dv_updates,
      merging_dv_updates,
      ..
    } = &mut *inner;

    for (field, updates) in pending_dv_updates.iter() {
      let entry = match merging_dv_updates.get_mut(field) {
        Some(entry) => {
          entry.reserve(updates.len());
          entry
        },
        None => merging_dv_updates
          .entry(field.clone())
          .or_insert_with(|| DocValuesUpdateList::with_capacity(updates.len())),
      };
      entry.extend(updates.iter().cloned());
    }

    let mut reader = get_reader(context, info, &mut inner, self.index_created_version_major)?;
    if inner.pending_deletes.needs_refresh(reader.as_ref(), info)?
      || reader.get_segment_info().get_del_gen() != info.get_del_gen()
    {
      // beware of zombies:
      debug_assert!(inner.pending_deletes.get_live_docs().is_some());
      reader = Arc::new(create_new_reader_with_latest_live_docs(
        &mut inner,
        Some(reader.as_ref()),
        info,
      )?);
    }

    debug_assert!(
      inner
        .pending_deletes
        .verify_doc_counts(reader.as_ref(), info)?
    );

    let merge_reader = MergeReader::new(reader, inner.pending_deletes.get_hard_live_docs());
    reader_consumer(&merge_reader)?;
    Ok(merge_reader)
  }

  /// Drops all merging updates.
  /// Called from IndexWriter after this segment finished merging (whether successfully or not).
  pub fn drop_merging_updates(&self, inner: Option<&mut Inner<D>>) {
    let inner = match inner {
      Some(inner) => inner,
      None => &mut *self.inner.lock(),
    };
    inner.merging_dv_updates.clear();
    inner.is_merging = false;
  }
  pub fn get_merging_dv_updates(&self) -> HashMap<Arc<str>, DocValuesUpdateList> {
    // We must atomically (in single sync'd block) clear isMerging when we return the DV updates
    // otherwise we can lose updates:
    let mut inner = self.inner.lock();
    inner.is_merging = false;
    inner.merging_dv_updates.clone()
  }
  pub fn is_fully_deleted(&self, info: &SegmentCommitInfo<D>) -> Result<bool> {
    let mut inner = self.inner.lock();
    let is_soft = matches!(inner.pending_deletes, PendingDeletesEnum::Soft(_));
    if is_soft {
      let dv_gen = inner.pending_deletes.dv_gen()?;
      let (reader, field_infos, on_new_reader) = if dv_gen == -2 {
        let field_infos = pending_soft_deletes::read_field_infos(info)?;
        let field_info = field_infos.field_info_by_name(inner.pending_deletes.field()?)?;
        let on_new_reader = pending_soft_deletes::do_on_new_reader(field_info);
        let reader = if on_new_reader {
          Some(self.get_latest_read(info, &mut inner, self.index_created_version_major)?)
        } else {
          None
        };
        (reader, Some(field_infos), on_new_reader)
      } else {
        (None, None, false)
      };
      inner
        .pending_deletes
        .is_fully_deleted(info, reader.as_ref(), field_infos, on_new_reader)
    } else {
      inner
        .pending_deletes
        .is_fully_deleted(info, None, None, false)
    }
  }
  pub(crate) fn keep_fully_deleted_segment<P>(
    &self,
    merge_policy: &P,
    info: &SegmentCommitInfo<D>,
  ) -> Result<bool>
  where
    P: MergePolicy<D> + ?Sized,
  {
    merge_policy.keep_fully_deleted_segment(|| {
      let mut inner = self.inner.lock();
      self.get_latest_read(info, &mut inner, self.index_created_version_major)
    })
  }
  pub(crate) fn get_info_id(&self) -> &str {
    &self.info_id
  }
  pub(crate) fn get_latest_read(
    &self,
    info: &SegmentCommitInfo<D>,
    inner: &mut Inner<D>,
    index_created_version_major: i32,
  ) -> Result<DefaultLeafReader<D>>
  where
    D: Directory,
  {
    if inner.reader.is_none() {
      // get a reader and dec the ref right away we just make sure we have a reader
      get_reader(
        IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?,
        info,
        inner,
        index_created_version_major,
      )?
      .dec_ref()?;
    }
    // we should take the reader out of the struct temporarily, cause borrow check
    // it is safe take reader under lock because we put it back right away
    let needs_refresh = {
      let reader = inner
        .reader
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("reader was not initialized"))?;
      inner.pending_deletes.needs_refresh(reader, info)?
    };
    if needs_refresh {
      // we have a reader but its live-docs are out of sync. let's create a temporary one that we
      // never share
      swap_new_reader_with_latest_live_docs(inner, info)?;
    }
    // put reader back and return a clone
    let reader = inner
      .reader
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("reader was not initialized"))?;
    Ok(Arc::clone(reader))
  }
}

impl<D> Display for ReadersAndUpdates<D>
where
  D: Directory,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
    let inner = self.inner.lock();
    write!(
      f,
      "ReadersAndLiveDocs(seg={}, PendingDeletesEnum={})",
      self.info_id, inner.pending_deletes
    )
  }
}
pub(crate) fn create_new_reader_with_latest_live_docs<'a, D>(
  inner: &'a mut Inner<D>, // The mutable borrow proves exclusive access to the writer state.
  reader: Option<&'a SegmentReader<D>>,
  info: &SegmentCommitInfo<D>,
) -> Result<SegmentReader<D>>
where
  D: Directory,
{
  let reader = match reader {
    Some(r) => r,
    None => inner
      .reader
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("reader was not initialized"))?
      .as_ref(),
  };

  let new_reader = SegmentReader::new_from_reader(
    info,
    reader,
    inner.pending_deletes.get_live_docs(),
    inner.pending_deletes.get_hard_live_docs(),
    inner.pending_deletes.num_docs(info)?,
    true,
  )?;

  let mut success = false;
  let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
    inner.pending_deletes.on_new_reader(&new_reader, info)?;
    reader.dec_ref()?;
    success = true;
    Ok(())
  }));

  if !success {
    new_reader.dec_ref()?;
  }
  unwrap_caught_result!(res)?;
  Ok(new_reader)
}
fn swap_new_reader_with_latest_live_docs<D>(
  inner: &mut Inner<D>,
  info: &SegmentCommitInfo<D>,
) -> Result<()>
where
  D: Directory,
{
  inner.reader = Some(Arc::new(create_new_reader_with_latest_live_docs(
    inner, None, info,
  )?));
  Ok(())
}
/// Returns a [`SegmentReader`].
pub(crate) fn get_reader<D>(
  context: &IOContext,
  info: &SegmentCommitInfo<D>,
  inner: &mut Inner<D>,
  index_created_version_major: i32,
) -> Result<DefaultLeafReader<D>>
where
  D: Directory,
{
  if inner.reader.is_none() {
    let reader = SegmentReader::new(info, index_created_version_major, context)?;
    inner.pending_deletes.on_new_reader(&reader, info)?;
    inner.reader = Some(Arc::new(reader));
  }
  // Ref for caller
  let reader = inner
    .reader
    .as_ref()
    .ok_or_else(|| LuceneError::illegal_state("reader was not initialized"))?;
  reader.inc_ref()?;
  Ok(reader.clone())
}

enum CurrentSource {
  OnDisk,
  Update,
}
/// Merges current on-disk doc values with incoming updates, giving the updates precedence.
/// in other words the values of the update always wins over the on-disk version.
struct MergedDocValues<DI> {
  // merged docID
  doc_id_out: i32,
  // docID from our original doc values
  doc_id_on_disk: i32,
  // docID from our updates
  update_doc_id: i32,

  on_disk_doc_values: Option<DI>,
  update_doc_values: DocIdSetIteratorEnum2<
    BinaryDocValuesDVFU<MergedFieldIterator>,
    NumericDocValuesDVFU<MergedFieldIterator>,
  >,
  current_values_supplier: Option<CurrentSource>,
}
impl<DI> MergedDocValues<DI> {
  pub fn new(
    on_disk_doc_values: Option<DI>,
    update_doc_values: DocIdSetIteratorEnum2<
      BinaryDocValuesDVFU<MergedFieldIterator>,
      NumericDocValuesDVFU<MergedFieldIterator>,
    >,
  ) -> Self {
    Self {
      doc_id_out: -1,
      doc_id_on_disk: -1,
      update_doc_id: -1,
      on_disk_doc_values,
      update_doc_values,
      current_values_supplier: None,
    }
  }
}
impl<DI> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for MergedDocValues<DI>
where
  DI: DocValuesIterator,
{
}
impl<DI> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for MergedDocValues<DI> where
  DI: DocValuesIterator
{
}

impl<DI> DocIdSetIterator for MergedDocValues<DI>
where
  DI: DocValuesIterator,
{
  fn doc_id(&self) -> i32 {
    self.doc_id_out
  }

  fn next_doc(&mut self) -> Result<i32> {
    let mut has_value = false;

    while !has_value {
      if self.doc_id_on_disk == self.doc_id_out {
        match self.on_disk_doc_values.as_mut() {
          Some(dv) => {
            self.doc_id_on_disk = dv.next_doc()?;
          },
          None => {
            self.doc_id_on_disk = NO_MORE_DOCS;
          },
        }
      }

      if self.update_doc_id == self.doc_id_out {
        self.update_doc_id = self.update_doc_values.next_doc()?;
      }

      if self.doc_id_on_disk < self.update_doc_id {
        // no update to this doc - we use the on-disk values
        self.doc_id_out = self.doc_id_on_disk;
        self.current_values_supplier = Some(CurrentSource::OnDisk);
        has_value = true;
      } else {
        self.doc_id_out = self.update_doc_id;
        if self.doc_id_out != NO_MORE_DOCS {
          self.current_values_supplier = Some(CurrentSource::Update);
          has_value = match self.update_doc_values {
            DocIdSetIteratorEnum2::A(ref mut dv) => dv.iterator.has_value()?,
            DocIdSetIteratorEnum2::B(ref mut dv) => dv.iterator.has_value()?,
          };
        } else {
          has_value = true;
        }
      }
    }
    Ok(self.doc_id_out)
  }

  fn advance(&mut self, _target: i32) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn cost(&self) -> Result<i64> {
    self
      .on_disk_doc_values
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("on-disk doc values are not available"))?
      .cost()
  }
}

impl<DI> DocValuesIterator for MergedDocValues<DI>
where
  DI: DocValuesIterator,
{
  fn advance_exact(&mut self, _target: i32) -> Result<bool> {
    Err(LuceneError::unsupported_operation(""))
  }
}

struct BinaryDocValuesImpl<DI> {
  merged_doc_values: MergedDocValues<DI>,
}
impl<DI> BinaryDocValuesImpl<DI> {
  fn new(merged_doc_values: MergedDocValues<DI>) -> Self {
    Self { merged_doc_values }
  }
}

impl<DI> DocValuesIterator for BinaryDocValuesImpl<DI>
where
  DI: DocValuesIterator,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    self.merged_doc_values.advance_exact(target)
  }
}

impl<DI> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for BinaryDocValuesImpl<DI>
where
  DI: DocValuesIterator,
{
}
impl<DI> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for BinaryDocValuesImpl<DI> where
  DI: DocValuesIterator
{
}

impl<DI> DocIdSetIterator for BinaryDocValuesImpl<DI>
where
  DI: DocValuesIterator,
{
  fn doc_id(&self) -> i32 {
    self.merged_doc_values.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.merged_doc_values.next_doc()
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.merged_doc_values.advance(target)
  }

  fn cost(&self) -> Result<i64> {
    self.merged_doc_values.cost()
  }
}

impl<DI> BinaryDocValues for BinaryDocValuesImpl<DI>
where
  DI: BinaryDocValues,
{
  type Value<'a>
    = crate::core::index::BytesRefValueEnum2<DI::Value<'a>, BytesRef<&'a [u8]>>
  where
    Self: 'a;

  fn binary_value(&self) -> Result<Self::Value<'_>> {
    match self.merged_doc_values.current_values_supplier {
      Some(CurrentSource::OnDisk) => {
        if let Some(dv) = &self.merged_doc_values.on_disk_doc_values {
          dv.binary_value()
            .map(crate::core::index::BytesRefValueEnum2::A)
        } else {
          Err(LuceneError::illegal_state(
            "no on-disk doc values available",
          ))
        }
      },
      Some(CurrentSource::Update) => match self.merged_doc_values.update_doc_values {
        DocIdSetIteratorEnum2::A(ref dv) => dv
          .binary_value()
          .map(crate::core::index::BytesRefValueEnum2::B),
        DocIdSetIteratorEnum2::B(_) => Err(LuceneError::illegal_state(
          "update doc values should be BinaryDocValuesDVFU",
        )),
      },
      None => Err(LuceneError::illegal_state("no current values supplier set")),
    }
  }
}
struct NumericDocValuesImpl<DI> {
  merged_doc_values: MergedDocValues<DI>,
}

impl<DI> NumericDocValuesImpl<DI> {
  fn new(merged_doc_values: MergedDocValues<DI>) -> Self {
    Self { merged_doc_values }
  }
}

impl<DI> DocValuesIterator for NumericDocValuesImpl<DI>
where
  DI: DocValuesIterator,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    self.merged_doc_values.advance_exact(target)
  }
}

impl<DI> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for NumericDocValuesImpl<DI>
where
  DI: DocValuesIterator,
{
}
impl<DI> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for NumericDocValuesImpl<DI> where
  DI: DocValuesIterator
{
}

impl<DI> DocIdSetIterator for NumericDocValuesImpl<DI>
where
  DI: DocValuesIterator,
{
  fn doc_id(&self) -> i32 {
    self.merged_doc_values.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.merged_doc_values.next_doc()
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.merged_doc_values.advance(target)
  }

  fn cost(&self) -> Result<i64> {
    self.merged_doc_values.cost()
  }
}

impl<DI> NumericDocValues for NumericDocValuesImpl<DI>
where
  DI: NumericDocValues,
{
  fn long_value(&mut self) -> Result<i64> {
    match self.merged_doc_values.current_values_supplier {
      Some(CurrentSource::OnDisk) => {
        if let Some(dv) = &mut self.merged_doc_values.on_disk_doc_values {
          dv.long_value()
        } else {
          Err(LuceneError::illegal_state(
            "no on-disk doc values available",
          ))
        }
      },
      Some(CurrentSource::Update) => match self.merged_doc_values.update_doc_values {
        DocIdSetIteratorEnum2::A(_) => Err(LuceneError::illegal_state(
          "update doc values should be BinaryDocValuesDVFU",
        )),
        DocIdSetIteratorEnum2::B(ref mut dv) => dv.long_value(),
      },
      None => Err(LuceneError::illegal_state("no current values supplier set")),
    }
  }
}

struct DocValuesProducerBinary<'a, D>
where
  D: Directory,
{
  update_supplier: FunctionImpl<'a>,
  field: &'a str,
  reader: &'a SegmentReader<D>,
  field_info: &'a Arc<FieldInfo>,
}

impl<D> CloseableRef for DocValuesProducerBinary<'_, D> where D: Directory {}

impl<'a, D> DocValuesProducerBinary<'a, D>
where
  D: Directory,
{
  pub fn new(
    update_supplier: FunctionImpl<'a>,
    field: &'a str,
    reader: &'a SegmentReader<D>,
    field_info: &'a Arc<FieldInfo>,
  ) -> Self {
    Self {
      update_supplier,
      field,
      reader,
      field_info,
    }
  }
}

impl<'a, D> DocValuesProducer for DocValuesProducerBinary<'a, D>
where
  D: Directory,
{
  type NumericDocValues = DummyNumericDocValues;
  type BinaryDocValues = BinaryDocValuesImpl<<SegmentReader<D> as LeafReader>::BinaryDocValues>;

  fn get_binary(&self, _field: &Arc<FieldInfo>) -> Result<Self::BinaryDocValues> {
    let iterator = match self.update_supplier.apply(self.field_info)? {
      Some(it) => it,
      None => {
        return Err(LuceneError::illegal_argument(
          "iterator should never None here",
        ));
      },
    };
    let merged_doc_values = MergedDocValues::new(
      self.reader.get_binary_doc_values(self.field)?,
      DocIdSetIteratorEnum2::A(BinaryDocValuesDVFU::new(iterator)),
    );
    Ok(BinaryDocValuesImpl::new(merged_doc_values))
  }

  type SortedDocValues = DummySortedDocValues;
  type SortedNumericDocValues = DummySortedNumericDocValues;
  type SortedSetDocValues = DummySortedSetDocValues;
  type DocValuesSkipper = DummyDocValuesSkipper;
}
struct DocValuesProducerNumeric<'a, D>
where
  D: Directory,
{
  update_supplier: FunctionImpl<'a>,
  field: &'a str,
  reader: &'a SegmentReader<D>,
  field_info: &'a Arc<FieldInfo>,
}

impl<D> CloseableRef for DocValuesProducerNumeric<'_, D> where D: Directory {}

impl<'a, D> DocValuesProducerNumeric<'a, D>
where
  D: Directory,
{
  pub fn new(
    update_supplier: FunctionImpl<'a>,
    field: &'a str,
    reader: &'a SegmentReader<D>,
    field_info: &'a Arc<FieldInfo>,
  ) -> Self {
    Self {
      update_supplier,
      field,
      reader,
      field_info,
    }
  }
}
impl<'a, D> DocValuesProducer for DocValuesProducerNumeric<'a, D>
where
  D: Directory,
{
  type NumericDocValues = NumericDocValuesImpl<<SegmentReader<D> as LeafReader>::NumericDocValues>;
  fn get_numeric(&self, _field: &Arc<FieldInfo>) -> Result<Self::NumericDocValues> {
    let iterator = match self.update_supplier.apply(self.field_info)? {
      Some(it) => it,
      None => {
        return Err(LuceneError::illegal_argument(
          "iterator should never None here",
        ));
      },
    };

    let merged_doc_values = MergedDocValues::new(
      self.reader.get_numeric_doc_values(self.field)?,
      DocIdSetIteratorEnum2::B(NumericDocValuesDVFU::new(iterator)),
    );
    // Merge sort of the original doc values with updated doc values:
    Ok(NumericDocValuesImpl::new(merged_doc_values))
  }
  type BinaryDocValues = DummyBinaryDocValues;
  type SortedDocValues = DummySortedDocValues;
  type SortedNumericDocValues = DummySortedNumericDocValues;
  type SortedSetDocValues = DummySortedSetDocValues;

  type DocValuesSkipper = DummyDocValuesSkipper;
}

struct FunctionImpl<'a> {
  field_info: &'a Arc<FieldInfo>,
  updates_to_apply: &'a [&'a Arc<DocValuesFieldUpdatesEnum>],
}
impl<'a> FunctionImpl<'a> {
  fn new(
    field_info: &'a Arc<FieldInfo>,
    updates_to_apply: &'a [&'a Arc<DocValuesFieldUpdatesEnum>],
  ) -> Self {
    Self {
      field_info,
      updates_to_apply,
    }
  }
}
impl FunctionImpl<'_> {
  // Preserve the original carrier for pending (including soft) delete callbacks.
  fn apply_legacy(
    &self,
    info: &Arc<FieldInfo>,
  ) -> Result<Option<MergedIterator<DocValuesFieldIteratorEnum>>> {
    merged_iterator(self.iterators(info)?)
  }

  fn iterators(&self, info: &Arc<FieldInfo>) -> Result<Vec<DocValuesFieldIteratorEnum>> {
    if !Arc::ptr_eq(info, self.field_info) {
      return Err(LuceneError::illegal_argument(format!(
        "expected field info for field: {} but got: {}",
        self.field_info.name, info.name
      )));
    }

    let mut subs = Vec::with_capacity(self.updates_to_apply.len());
    for v in self.updates_to_apply {
      subs.push(v.iterator()?)
    }
    Ok(subs)
  }
}
impl Function<Arc<FieldInfo>, Option<MergedFieldIterator>> for FunctionImpl<'_> {
  fn apply(&self, info: &Arc<FieldInfo>) -> Result<Option<MergedFieldIterator>> {
    merged_field_iterator(self.iterators(info)?)
  }
}

fn clone_field_info(fi: &FieldInfo, field_number: i32) -> Result<FieldInfo> {
  FieldInfo::new(
    fi.name.to_string(),
    field_number,
    fi.has_term_vectors(),
    fi.omits_norms(),
    fi.has_payloads(),
    *fi.get_index_options(),
    *fi.get_doc_values_type(),
    *fi.doc_values_skip_index_type(),
    fi.get_doc_values_gen(),
    fi.attributes(),
    fi.get_point_dimension_count(),
    fi.get_point_index_dimension_count(),
    fi.get_point_num_bytes(),
    fi.get_vector_dimension(),
    *fi.get_vector_encoding(),
    *fi.get_vector_similarity_function(),
    fi.is_soft_deletes_field(),
    fi.is_parent_field(),
  )
}
