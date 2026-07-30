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
use crate::core::index::field_infos::FieldNumbers;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::IndexWriterDir;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::pending_deletes::{PendingDeletes, PendingDeletesEnum};
use crate::core::index::pending_soft_deletes::PendingSoftDeletes;
use crate::core::index::readers_and_updates::ReadersAndUpdates;
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_infos::SegmentInfos;
use crate::core::index::segment_reader::SegmentReader;
use crate::core::index::sorter::DocMapImpl;
use crate::core::index::standard_directory_reader::StandardDirectoryReader;
use crate::core::store::directory::Directory;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::info_stream::InfoStreamMT;
use crate::core::util::long_supplier::LongSupplier;
use crate::core::util::{HasIdentity, IOUtils};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// Holds shared SegmentReader instances.
/// IndexWriter uses SegmentReaders for
/// 1) applying deletes/DV updates,
/// 2) doing merges,
/// 3) handing out a real-time reader.
///    This pool reuses instances of the SegmentReaders in all these places
///    if it is in "near real-time mode" (getReader() has been called on this instance).
pub(crate) struct ReaderPool<D, F>
where
  D: Directory,
  F: LongSupplier,
{
  directory: Arc<IndexWriterDir<D>>,
  original_directory: Arc<D>,
  info_stream: InfoStreamMT,
  soft_deletes_field: Option<String>,
  // This is a "write once" variable (like the organic dye
  // on a DVD-R that may or may not be heated by a laser and
  // then cooled to permanently record the event): it's
  // false by default until reader pooling is enabled.
  // is called for the first time,
  // at which point it's switched to true and never changes
  // back to false.  Once this is true, we hold open and
  // reuse SegmentReader instances internally for applying
  // deletes, doing merges, and reopening near real-time
  // readers.
  // in practice this should be called once the readers are likely
  // to be needed and reused, for example when `IndexWriter::get_reader` is called.
  pool_readers: AtomicBool,
  inner: Mutex<Inner<D>>,
  completed_del_gen_supplier: F,
  index_created_version_major: i32,
}
pub(crate) struct Inner<D>
where
  D: Directory,
{
  reader_map: HashMap<String, Arc<ReadersAndUpdates<D>>>,
  closed: AtomicBool,
}
impl<D, F> ReaderPool<D, F>
where
  D: Directory,
  F: LongSupplier,
{
  #[allow(clippy::too_many_arguments)]
  pub(crate) fn new(
    directory: Arc<IndexWriterDir<D>>,
    original_directory: Arc<D>,
    segment_infos: &SegmentInfos<D>,
    info_stream: InfoStreamMT,
    soft_deletes_field: Option<String>,
    completed_del_gen_supplier: F,
    reader: Option<&StandardDirectoryReader<D>>,
    index_created_version_major: i32,
  ) -> Result<Self> {
    let mut reader_map = HashMap::new();

    if let Some(reader) = reader {
      // Pre-enroll all segment readers into the reader pool; this is necessary so
      // any in-memory NRT live docs are correctly carried over, and so NRT readers
      // pulled from this IW share the same segment reader:
      let context = reader.get_context()?;
      let leaves = context.leaves()?;
      debug_assert_eq!(segment_infos.size(), leaves.len());
      for (i, leaf) in leaves.iter().enumerate() {
        let seg_reader = leaf.reader().as_ref();
        let info = segment_infos
          .info(i)
          .ok_or_else(|| LuceneError::illegal_state("SegmentCommitInfo missing"))?;
        let new_reader = SegmentReader::new_from_reader(
          info,
          seg_reader,
          seg_reader.get_live_docs()?,
          seg_reader.get_hard_live_docs()?,
          seg_reader.num_docs()?,
          true,
        )?;
        let info_id = new_reader.get_original_segment_info_id().to_string();
        let pending_deletes =
          Self::new_pending_deletes_with_reader(&soft_deletes_field, &new_reader, info)?;
        reader_map.insert(
          info_id,
          Arc::new(ReadersAndUpdates::with_reader(
            index_created_version_major,
            Arc::new(new_reader),
            info,
            pending_deletes,
          )?),
        );
      }
    }

    Ok(Self {
      directory,
      original_directory,
      info_stream,
      soft_deletes_field,
      pool_readers: AtomicBool::new(false),
      inner: Mutex::new(Inner {
        reader_map,
        closed: AtomicBool::new(false),
      }),
      completed_del_gen_supplier,
      index_created_version_major,
    })
  }
  /// Asserts this info still exists in IW's segment infos
  pub(crate) fn assert_info_is_live(&self, _info: &SegmentCommitInfo<D>) -> bool {
    // TODO IMPORTANT
    true
  }
  /// Drops reader for the given SegmentCommitInfo if it's pooled
  pub(crate) fn drop(&self, info_id: &str, segment_infos: &mut SegmentInfos<D>) -> Result<bool> {
    let mut inner = self.inner.lock();
    if let Some(rld) = inner.reader_map.remove(info_id) {
      debug_assert_eq!(info_id, rld.info_id);
      rld.drop_readers()?;
      let remove_dropped_info = rld.ref_count() == 0;
      if remove_dropped_info {
        segment_infos.remove_dropped_segment_commit_info(info_id);
      }
      return Ok(true);
    }
    Ok(false)
  }
  /// Returns the sum of the ram used by all the buffered readers and updates in MB
  pub(crate) fn ram_bytes_used(&self) -> i64 {
    let inner = self.inner.lock();
    let mut bytes: i64 = 0;
    for rld in inner.reader_map.values() {
      bytes += rld.ram_bytes_used.load(std::sync::atomic::Ordering::SeqCst);
    }
    bytes
  }

  /// Returns true iff any of the buffered readers and updates has at least one pending delete
  pub(crate) fn any_deletions(&self, infos: &SegmentInfos<D>) -> Result<bool> {
    let inner = self.inner.lock();
    let mut found = 0;
    for info in infos
      .iter()
      .iter()
      .chain(infos.dropped_segment_commit_infos.values())
    {
      if let Some(rld) = inner.reader_map.get(info.info.get_id_key()) {
        found += 1;
        if rld.get_del_count(info) > 0 {
          return Ok(true);
        }
      }
    }
    if found != inner.reader_map.len() {
      return Err(LuceneError::illegal_state("SegmentCommitInfo missing"));
    }
    Ok(false)
  }
  /// Enables reader pooling for this pool. This should be called once the readers in this pool are
  /// shared with an outside resource like an NRT reader. Once reader pooling is enabled a `ReadersAndUpdates`
  /// will be kept around in the reader pool on calling [`release(ReadersAndUpdates, boolean)`](Self::release) until the
  /// segment get dropped via calls to [`drop(SegmentCommitInfo)`](Self::drop) or `dropAll()` or `close()`. Reader pooling
  /// is disabled upon construction but can't be disabled again once it's enabled.
  pub(crate) fn enable_reader_pooling(&self) {
    self
      .pool_readers
      .store(true, std::sync::atomic::Ordering::SeqCst);
  }

  pub(crate) fn is_reader_pooling_enabled(&self) -> bool {
    self.pool_readers.load(std::sync::atomic::Ordering::SeqCst)
  }
  /// Releases the `ReadersAndUpdates`. This should only be called if
  /// [`get(SegmentCommitInfo, bool)`](Self::get) is called with the `create` parameter set to `true`.
  ///
  /// # Returns
  ///
  /// `true` if any files were written by this release call.
  pub(crate) fn release(
    &self,
    rld: &ReadersAndUpdates<D>,
    _assert_info_live: bool,
    segment_infos: &mut SegmentInfos<D>,
    merge_info: Option<&mut SegmentCommitInfo<D>>,
    global_field_number: &FieldNumbers,
  ) -> Result<bool> {
    let mut inner = self.inner.lock();
    let mut changed = false;

    // Matches incRef in get:
    rld.dec_ref();
    if rld.ref_count() == 0 {
      // This happens if the segment was just merged away,
      // while a buffered deletes packet was still applying deletes/updates to it.
      debug_assert!(
        !inner.reader_map.contains_key(&rld.info_id),
        "seg={} has refCount 0 but still unexpectedly exists in the reader pool",
        rld.info_id
      );
      segment_infos.remove_dropped_segment_commit_info(&rld.info_id);
    } else {
      // Pool still holds a ref:
      debug_assert!(
        rld.ref_count() > 0,
        "refCount={} reader={:?}",
        rld.ref_count(),
        rld.info_id
      );

      if !self.is_reader_pooling_enabled()
        && rld.ref_count() == 1
        && inner.reader_map.contains_key(&rld.info_id)
      {
        // This is the last ref to this RLD, and we're not
        // pooling, so remove it:
        let info = match segment_infos.index_of_mut(&rld.info_id) {
          Some(info) => Some(info),
          None => merge_info,
        };
        let info = info.ok_or_else(|| LuceneError::illegal_state("info is None"))?;
        if rld.write_live_docs(&self.directory, info)? {
          // Make sure we only write del docs for a live segment:
          // TODO
          // debug_assert!(
          //     !assert_info_live || self.assert_info_is_live(rld.info()),
          //     "assertInfoIsLive failed for {:?}",
          //     info_id
          // );
          // Must checkpoint because we just created new _X_N.del and field updates files;
          // don't call IW.checkpoint because that also increments SIS.version,
          // which we do not want to do here.
          changed = true;
        }
        if rld.write_field_updates(
          &self.directory,
          global_field_number,
          self.completed_del_gen_supplier.get_as_long(),
          self.info_stream.as_ref(),
          Some(info),
        )? {
          changed = true;
        }
        if rld.get_num_dv_updates() == 0 {
          rld.drop_readers()?;
          if inner.reader_map.remove(&rld.info_id).is_some() {
            debug_assert_eq!(rld.ref_count(), 0);
            segment_infos.remove_dropped_segment_commit_info(&rld.info_id);
          }
        } else {
          // We are forced to pool this segment until its deletes fully apply
          // (no delGen gaps)
        }
      }
    }

    Ok(changed)
  }

  pub(crate) fn close(&self, segment_infos: &mut SegmentInfos<D>) -> Result<()> {
    if self
      .inner
      .lock()
      .closed
      .compare_exchange(
        false,
        true,
        std::sync::atomic::Ordering::SeqCst,
        std::sync::atomic::Ordering::SeqCst,
      )
      .is_ok()
    {
      self.drop_all(segment_infos)?;
    }
    Ok(())
  }

  /// Writes all doc values updates to disk if there are any.
  pub(crate) fn write_all_doc_values_updates(
    &self,
    infos: &mut SegmentInfos<D>,
    global_field_number: &FieldNumbers,
  ) -> Result<bool> {
    let copy: Vec<Arc<ReadersAndUpdates<D>>> = {
      let inner = self.inner.lock();
      // this needs to be protected by the reader pool lock otherwise we hit
      // concurrent-modification error
      inner.reader_map.values().cloned().collect()
    };

    let mut any = false;
    for rld in copy {
      any |= rld.write_field_updates(
        &self.directory,
        global_field_number,
        self.completed_del_gen_supplier.get_as_long(),
        self.info_stream.as_ref(),
        infos.index_of_mut(&rld.info_id),
      )?;
    }
    Ok(any)
  }
  /// Writes all doc values updates to disk if there are any.
  pub(crate) fn write_doc_values_updates_for_merge(
    &self,
    info_ids: &[String],
    infos: &mut SegmentInfos<D>,
    global_field_number: &FieldNumbers,
  ) -> Result<bool> {
    let mut any = false;
    for ids in info_ids {
      let info = infos.index_of_mut(ids).ok_or_else(|| {
        LuceneError::illegal_state(format!(
          "could not find SegmentCommitInfo with {} in SegmentInfos",
          ids
        ))
      })?;
      if let Some(rld) = self.get(info, false, None)? {
        any |= rld.write_field_updates(
          &self.directory,
          global_field_number,
          self.completed_del_gen_supplier.get_as_long(),
          self.info_stream.as_ref(),
          Some(info),
        )?;
        rld.set_is_merging();
      }
    }
    Ok(any)
  }
  /// Returns a list of all currently maintained `ReadersAndUpdates` sorted by their RAM consumption
  /// from largest to smallest. This list can also contain readers that don't consume any RAM at this
  /// point, i.e. readers without any buffered updates.
  pub(crate) fn get_readers_by_ram(&self) -> Vec<Arc<ReadersAndUpdates<D>>> {
    struct RamRecordingHolder<D>
    where
      D: Directory,
    {
      updates: Arc<ReadersAndUpdates<D>>,
      ram_bytes_used: i64,
    }

    let mut holders: Vec<RamRecordingHolder<D>> = {
      let inner = self.inner.lock();
      if inner.reader_map.is_empty() {
        return Vec::new();
      }
      // we have to record the RAM usage once and then sort
      // since the RAM usage can change concurrently and that will confuse the sort or hit an
      // assertion
      // the we can acquire here is not enough we would need to lock all ReadersAndUpdates to make
      // sure it doesn't change
      inner
        .reader_map
        .values()
        .map(|rld| RamRecordingHolder {
          updates: Arc::clone(rld),
          ram_bytes_used: rld.ram_bytes_used.load(std::sync::atomic::Ordering::SeqCst),
        })
        .collect()
    };
    // Sort this outside of the lock by largest ramBytesUsed:
    holders.sort_by_key(|holder| std::cmp::Reverse(holder.ram_bytes_used));

    holders.into_iter().map(|h| h.updates).collect()
  }
  /// Remove all our references to readers, and commits any pending changes.
  pub(crate) fn drop_all(&self, segment_infos: &mut SegmentInfos<D>) -> Result<()> {
    let mut inner = self.inner.lock();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      IOUtils::close(inner.reader_map.drain(), |(info_id, rld)| {
        rld.drop_readers()?;
        if rld.ref_count() == 0 {
          segment_infos.remove_dropped_segment_commit_info(&info_id);
        }
        Ok(())
      })
    }));
    debug_assert!(inner.reader_map.is_empty());
    match result {
      Ok(result) => result,
      Err(payload) => std::panic::resume_unwind(payload),
    }
  }
  /// Commit live docs changes for the  readers for the provided infos.
  pub(crate) fn commit(
    &self,
    infos: &mut SegmentInfos<D>,
    global_field_number: &FieldNumbers,
  ) -> Result<bool> {
    let inner = self.inner.lock();
    let mut at_least_one_change = false;

    for info in infos.segments.iter_mut() {
      if let Some(rld) = inner.reader_map.get(info.info.get_id_key()) {
        debug_assert_eq!(rld.info_id, info.info.get_id_key());

        let mut changed = rld.write_live_docs(&self.directory, info)?;
        changed |= rld.write_field_updates(
          &self.directory,
          global_field_number,
          self.completed_del_gen_supplier.get_as_long(),
          self.info_stream.as_ref(),
          Some(info),
        )?;

        if changed {
          // Make sure we only write del docs for a live segment:
          debug_assert!(self.assert_info_is_live(info));

          // Must checkpoint because we just
          // created new _X_N.del and field updates files;
          // don't call IW.checkpoint because that also
          // increments SIS.version, which we do not want to
          // do here: it was done previously (after we
          // invoked BDS.applyDeletes), whereas here all we
          // did was move the state to disk:
          at_least_one_change = true;
        }
      }
    }
    Ok(at_least_one_change)
  }
  /// Returns true iff there are any buffered doc values updates. Otherwise false .
  pub(crate) fn any_doc_values_changes(&self) -> bool {
    let inner = self.inner.lock();
    for rld in inner.reader_map.values() {
      // NOTE: we don't check for pending deletes because deletes carry over in RAM to NRT readers
      if rld.get_num_dv_updates() != 0 {
        return true;
      }
    }
    false
  }
  /// Obtains a `ReadersAndLiveDocs` instance from the reader pool.
  /// If `create` is `true`, you must later call [`release(ReadersAndUpdates, bool)`](Self::release).
  pub(crate) fn get(
    &self,
    info: &SegmentCommitInfo<D>,
    create: bool,
    sort_map: Option<Arc<DocMapImpl>>,
  ) -> Result<Option<Arc<ReadersAndUpdates<D>>>> {
    let mut inner = self.inner.lock();
    debug_assert!(
      info.info.dir.identity() == self.original_directory.identity(),
      "info.dir={} vs {}",
      info.info.dir,
      self.original_directory
    );

    if inner.closed.load(std::sync::atomic::Ordering::SeqCst) {
      debug_assert!(
        inner.reader_map.is_empty(),
        "Reader map is not empty: {:?}",
        inner.reader_map.keys().collect::<Vec<_>>()
      );
      return Err(LuceneError::already_closed("ReaderPool is already closed"));
    }

    let info_id = info.info.get_id_key();
    let rld = if let Some(rld) = inner.reader_map.get(info_id) {
      // TODO
      debug_assert!(
        rld.info_id == info_id,
        "rld.info={} info={} isLive?={} ",
        rld.info_id,
        info,
        // self.assert_info_is_live(&rld.get_info_id(None)),
        self.assert_info_is_live(info),
      );
      rld.clone()
    } else {
      if !create {
        return Ok(None);
      }
      let mut v = ReadersAndUpdates::new(
        self.index_created_version_major,
        info_id.to_string(),
        self.new_pending_deletes(info)?,
      );
      v.sort_map = sort_map;
      let rld = Arc::new(v);
      inner
        .reader_map
        .insert(info_id.to_string(), Arc::clone(&rld));
      rld
    };

    if create {
      rld.inc_ref();
    }
    #[cfg(test)]
    debug_assert!(self.no_dups(&inner));

    Ok(Some(rld))
  }
  fn new_pending_deletes(&self, info: &SegmentCommitInfo<D>) -> Result<PendingDeletesEnum> {
    match &self.soft_deletes_field {
      Some(field) => Ok(PendingDeletesEnum::Soft(PendingSoftDeletes::new(
        field, info,
      )?)),
      None => Ok(PendingDeletesEnum::PD(PendingDeletes::new(info)?)),
    }
  }

  fn new_pending_deletes_with_reader(
    soft_deletes_field: &Option<String>,
    reader: &SegmentReader<D>,
    info: &SegmentCommitInfo<D>,
  ) -> Result<PendingDeletesEnum> {
    match soft_deletes_field {
      Some(field) => Ok(PendingDeletesEnum::Soft(PendingSoftDeletes::from_reader(
        field, reader, info,
      )?)),
      None => Ok(PendingDeletesEnum::PD(PendingDeletes::from_reader(
        reader, info,
      )?)),
    }
  }
  /// Make sure that every segment appears only once in the pool.
  fn no_dups(&self, inner: &Inner<D>) -> bool {
    let mut seen = std::collections::HashSet::new();

    for rld in inner.reader_map.keys() {
      debug_assert!(!seen.contains(rld), "seen twice: {}", rld);
      seen.insert(rld);
    }
    true
  }
}

impl<D, F> Drop for ReaderPool<D, F>
where
  D: Directory,
  F: LongSupplier,
{
  fn drop(&mut self) {
    // TODO
  }
}
