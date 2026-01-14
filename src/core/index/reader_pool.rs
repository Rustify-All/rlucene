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
use crate::core::util::Comparator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::info_stream::InfoStreamMT;
use crate::core::util::long_supplier::LongSupplier;
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
    // false, by default until {@link #enableReaderPooling()}
    // is called for the first time,
    // at which point it's switched to true and never changes
    // back to false.  Once this is true, we hold open and
    // reuse SegmentReader instances internally for applying
    // deletes, doing merges, and reopening near real-time
    // readers.
    // in practice this should be called once the readers are likely
    // to be needed and reused ie if IndexWriter#getReader is called.
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
    pub(crate) fn new<S, LR, C, D1>(
        directory: Arc<IndexWriterDir<D>>,
        original_directory: Arc<D>,
        info_stream: InfoStreamMT,
        soft_deletes_field: Option<S>,
        completed_del_gen_supplier: F,
        _reader: Option<StandardDirectoryReader<LR, C, D1>>,
        index_created_version_major: i32,
    ) -> Self
    where
        S: Into<String>,
        LR: LeafReader + Clone,
        C: Comparator<LR>,
        D1: Directory,
    {
        Self {
            directory,
            original_directory,
            info_stream,
            soft_deletes_field: soft_deletes_field.map(Into::into),
            pool_readers: AtomicBool::new(false),
            inner: Mutex::new(Inner {
                reader_map: HashMap::new(),
                closed: AtomicBool::new(false),
            }),
            completed_del_gen_supplier,
            index_created_version_major,
        }
    }
    /// Asserts this info still exists in IW's segment infos
    pub(crate) fn assert_info_is_live(&self, _info: &SegmentCommitInfo<D>) -> bool {
        true
    }
    /// Drops reader for the given SegmentCommitInfo if it's pooled
    pub(crate) fn drop(&self, info_id: &str) -> Result<bool> {
        let mut inner = self.inner.lock();
        if let Some(rld) = inner.reader_map.remove(info_id) {
            debug_assert_eq!(info_id, rld.info_id);
            rld.drop_readers()?;
            return Ok(true);
        }
        Ok(false)
    }
    /// Returns the sum of the ram used by all the buffered readers and updates in MB
    pub(crate) fn ram_bytes_used(&self) -> i64 {
        let inner = self.inner.lock();
        let mut bytes: i64 = 0;
        for rld in inner.reader_map.values() {
            bytes += rld
                .ram_bytes_used
                .load(std::sync::atomic::Ordering::Relaxed);
        }
        bytes
    }
    /// Returns true iff any of the buffered readers and updates has at least one pending delete
    pub(crate) fn any_deletions(
        &self,
        infos: &HashMap<String, SegmentCommitInfo<D>>,
    ) -> Result<bool> {
        let inner = self.inner.lock();
        for rld in inner.reader_map.values() {
            let info = match infos.get(&rld.info_id) {
                Some(info) => info,
                None => return Err(LuceneError::illegal_state("SegmentCommitInfo missing")),
            };
            if rld.get_del_count(info) > 0 {
                return Ok(true);
            }
        }
        Ok(false)
    }
    /// Enables reader pooling for this pool. This should be called once the readers in this pool are
    /// shared with an outside resource like an NRT reader. Once reader pooling is enabled a `ReadersAndUpdates`
    /// will be kept around in the reader pool on calling [`release(ReadersAndUpdates, boolean)`](Self::release) until the
    /// segment get dropped via calls to [`drop(SegmentCommitInfo)`](Self::drop) or `dropAll()` or `close()`. Reader pooling
    /// is disabled upon construction but can't be disabled again once it's enabled.
    pub(crate) fn enable_reader_pooling(&self) {
        self.pool_readers
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) fn is_reader_pooling_enabled(&self) -> bool {
        self.pool_readers.load(std::sync::atomic::Ordering::Relaxed)
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
        info: &mut SegmentCommitInfo<D>,
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
                    info,
                )? {
                    changed = true;
                }
                if rld.get_num_dv_updates() == 0 {
                    rld.drop_readers()?;
                    inner.reader_map.remove(&rld.info_id);
                } else {
                    // We are forced to pool this segment until its deletes fully apply
                    // (no delGen gaps)
                }
            }
        }

        Ok(changed)
    }

    pub(crate) fn close(&self) -> Result<()> {
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
            self.drop_all()?;
        }
        Ok(())
    }

    /// Writes all doc values updates to disk if there are any.
    pub(crate) fn write_all_doc_values_updates(
        &self,
        infos: &mut HashMap<String, SegmentCommitInfo<D>>,
        global_field_number: &FieldNumbers,
    ) -> Result<bool> {
        let copy: Vec<Arc<ReadersAndUpdates<D>>> = {
            let inner = self.inner.lock();
            // this needs to be protected by the reader pool lock otherwise we hit
            // ConcurrentModificationException
            inner.reader_map.values().cloned().collect()
        };

        let mut any = false;
        for rld in copy {
            let info = match infos.get_mut(&rld.info_id) {
                Some(info) => info,
                None => return Err(LuceneError::illegal_state("SegmentCommitInfo missing")),
            };
            any |= rld.write_field_updates(
                &self.directory,
                global_field_number,
                self.completed_del_gen_supplier.get_as_long(),
                self.info_stream.as_ref(),
                info,
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
            let info = infos.info_mut(ids).ok_or_else(|| {
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
                    info,
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
                    ram_bytes_used: rld
                        .ram_bytes_used
                        .load(std::sync::atomic::Ordering::Relaxed),
                })
                .collect()
        };
        // Sort this outside of the lock by largest ramBytesUsed:
        holders.sort_by(|a, b| b.ram_bytes_used.cmp(&a.ram_bytes_used));

        holders.into_iter().map(|h| h.updates).collect()
    }
    /// Remove all our references to readers, and commits any pending changes.
    pub(crate) fn drop_all(&self) -> Result<()> {
        // TODO: IMPORT 这里需要实现LuceneError的嵌套返回
        let mut prior_errs = vec![];

        let mut inner = self.inner.lock();
        for (_, rld) in inner.reader_map.drain() {
            if let Err(e) = rld.drop_readers() {
                prior_errs.push(e);
            }
        }
        debug_assert!(inner.reader_map.is_empty());

        if let Some(err) = prior_errs.into_iter().next() {
            return Err(LuceneError::illegal_state(err));
        }
        Ok(())
    }
    /// Commit live docs changes for the segment readers for the provided infos.
    pub(crate) fn commit(
        &self,
        infos: &mut SegmentInfos<D>,
        global_field_number: &FieldNumbers,
    ) -> Result<bool> {
        let inner = self.inner.lock();
        let mut at_least_one_change = false;

        for info in infos.segments.values_mut() {
            if let Some(rld) = inner.reader_map.get(&info.info.get_id_str()) {
                debug_assert_eq!(rld.info_id, info.info.get_id_str());

                let mut changed = rld.write_live_docs(&self.directory, info)?;
                changed |= rld.write_field_updates(
                    &self.directory,
                    global_field_number,
                    self.completed_del_gen_supplier.get_as_long(),
                    self.info_stream.as_ref(),
                    info,
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
            Arc::ptr_eq(&info.info.dir, &self.original_directory),
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

        let rld = if let Some(rld) = inner.reader_map.get(&info.info.get_id_str()) {
            // TODO
            debug_assert!(
                rld.info_id == info.info.get_id_str(),
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
                self.new_pending_deletes(info)?,
            );
            v.sort_map = sort_map;
            let rld = Arc::new(v);
            inner
                .reader_map
                .insert(info.info.get_id_str(), Arc::clone(&rld));
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
            Some(field) => Ok(PendingDeletesEnum::B(PendingSoftDeletes::new(field, info)?)),
            None => Ok(PendingDeletesEnum::A(PendingDeletes::new(info)?)),
        }
    }

    fn new_pending_deletes_with_reader(
        &self,
        reader: &SegmentReader<D>,
        info: &SegmentCommitInfo<D>,
    ) -> Result<PendingDeletesEnum> {
        match &self.soft_deletes_field {
            Some(field) => Ok(PendingDeletesEnum::B(PendingSoftDeletes::from_reader(
                field, reader, info,
            )?)),
            None => Ok(PendingDeletesEnum::A(PendingDeletes::from_reader(
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

#[cfg(test)]
mod tests {
    use crate::core::document::document::Document;
    use crate::core::document::field::Store::Yes;
    use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
    use crate::core::document::string_field::StringField;
    use crate::core::index::directory_reader::directory_reader_util;
    use crate::core::index::doc_values_field_updates::{
        DocValuesFieldUpdates, DocValuesFieldUpdatesBase,
    };
    use crate::core::index::dummy::dummy_leaf_reader::DummyLeafReader;
    use crate::core::index::field_infos::FieldNumbersLock;
    use crate::core::index::index_reader::IndexReader;
    use crate::core::index::index_writer::IndexWriter;
    use crate::core::index::leaf_reader::LeafReader;
    use crate::core::index::numeric_doc_values::NumericDocValues;
    use crate::core::index::numeric_doc_values_field_updates::NumericDocValuesFieldUpdates;
    use crate::core::index::reader_pool::ReaderPool;
    use crate::core::index::term::Term;
    use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
    use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
    use crate::core::store::IOContext;
    use crate::core::store::directory::Directory;
    use crate::core::store::dummy::dummy_directory::DummyDirectory;
    use crate::core::store::lock_validating_directory_wrapper::LockValidatingDirectoryWrapper;
    use crate::core::util::bits::Bits;
    use crate::core::util::dummy::dummy_comparator::DummyComparator;
    use crate::core::util::error::lucene_error::Result;
    use crate::core::util::info_stream::InfoStreamEnum;
    use crate::core::util::long_supplier::LongSupplier;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{
        new_directory_shared, new_index_writer_config, random,
    };

    use rand::Rng;
    use std::sync::Arc;

    #[allow(dead_code)]
    struct TestReaderPool;

    #[derive(Default)]
    struct LongSupplierImpl;
    impl LongSupplier for LongSupplierImpl {
        fn get_as_long(&self) -> i64 {
            0
        }
    }

    #[test]
    fn test_drop() -> Result<()> {
        let mut random = random();
        let directory = new_directory_shared(&mut random)?;

        let (field_numbers, index_created_version_major) =
            build_index(directory.clone(), &mut random)?;

        let mut reader = directory_reader_util::open(directory.clone())?;
        let segment_infos = reader.segment_infos.as_mut().unwrap();
        let lock = directory.obtain_lock("writer_lock")?;
        let lock_dir = Arc::new(LockValidatingDirectoryWrapper::new(directory.clone(), lock));
        let pool = ReaderPool::new::<String, DummyLeafReader, DummyComparator, DummyDirectory>(
            lock_dir,
            directory.clone(),
            Arc::new(InfoStreamEnum::default()),
            None,
            LongSupplierImpl,
            None,
            index_created_version_major,
        );
        let idx = random.random_range(0..segment_infos.segments_idx.len());
        let commit_info = segment_infos.info_idx_mut(idx).unwrap();

        let readers_and_updates = pool.get(commit_info, true, None)?.unwrap();

        let same = pool.get(commit_info, false, None)?.unwrap();
        assert!(Arc::ptr_eq(&readers_and_updates, &same));
        assert!(pool.drop(&commit_info.info.get_id_str())?);

        if random.random_bool(0.5) {
            assert!(!pool.drop(&commit_info.info.get_id_str())?);
        }
        assert!(pool.get(commit_info, false, None)?.is_none());
        pool.release(
            &readers_and_updates,
            random.random_bool(0.5),
            commit_info,
            &field_numbers.lock(),
        )?;
        pool.close()?;
        Ok(())
    }
    #[test]
    fn test_pool_readers() -> Result<()> {
        let mut random = random();
        let directory = new_directory_shared(&mut random)?;

        let (field_numbers, index_created_version_major) =
            build_index(directory.clone(), &mut random)?;

        let mut reader = directory_reader_util::open(directory.clone())?;
        let segment_infos = reader.segment_infos.as_mut().unwrap();

        let lock = directory.obtain_lock("writer_lock")?;
        let lock_dir = Arc::new(LockValidatingDirectoryWrapper::new(directory.clone(), lock));

        let pool = ReaderPool::new::<String, DummyLeafReader, DummyComparator, DummyDirectory>(
            lock_dir,
            directory.clone(),
            Arc::new(InfoStreamEnum::default()),
            None,
            LongSupplierImpl,
            None,
            index_created_version_major,
        );

        let idx = random.random_range(0..segment_infos.segments_idx.len());
        let commit_info = segment_infos.info_idx_mut(idx).unwrap();

        assert!(!pool.is_reader_pooling_enabled());

        let rau = pool.get(commit_info, true, None)?.unwrap();
        pool.release(
            &rau,
            random.random_bool(0.5),
            commit_info,
            &field_numbers.lock(),
        )?;

        assert!(pool.get(commit_info, false, None)?.is_none());
        // now start pooling
        pool.enable_reader_pooling();
        assert!(pool.is_reader_pooling_enabled());

        let rau = pool.get(commit_info, true, None)?.unwrap();
        pool.release(
            &rau,
            random.random_bool(0.5),
            commit_info,
            &field_numbers.lock(),
        )?;

        let pooled = pool.get(commit_info, false, None)?.unwrap();
        let pooled_again = pool.get(commit_info, false, None)?.unwrap();
        assert!(Arc::ptr_eq(&pooled, &pooled_again));

        pool.drop(&commit_info.info.get_id_str())?;

        // let mut ram_bytes_used = 0_i64;
        // TODO: memory calculation not implemented
        // assert_eq!(0, pool.ram_bytes_used());

        for idx in 0..segment_infos.segments_idx.len() {
            let info = segment_infos.info_idx_mut(idx).unwrap();

            let rau = pool.get(info, true, None)?.unwrap();
            pool.release(&rau, random.random_bool(0.5), info, &field_numbers.lock())?;
            // TODO: memory calculation not implemented
            // assert_eq!(
            //     0,
            //     pool.ram_bytes_used(),
            //     " used: {} actual: {}",
            //     ram_bytes_used,
            //     pool.ram_bytes_used()
            // );

            // ram_bytes_used = pool.ram_bytes_used();

            let a = pool.get(info, false, None)?.unwrap();
            let b = pool.get(info, false, None)?.unwrap();
            assert!(Arc::ptr_eq(&a, &b));
        }
        // TODO: memory calculation not implemented
        // assert_ne!(0, pool.ram_bytes_used());

        pool.drop_all()?;

        for idx in 0..segment_infos.segments_idx.len() {
            let info = segment_infos.info_idx(idx).unwrap();
            assert!(pool.get(info, false, None)?.is_none());
        }

        // TODO: memory calculation not implemented
        // assert_eq!(0, pool.ram_bytes_used());

        pool.close()?;
        Ok(())
    }

    #[test]
    fn test_update() -> Result<()> {
        let mut random = random();
        let directory = new_directory_shared(&mut random)?;

        let (field_numbers, index_created_version_major) =
            build_index(directory.clone(), &mut random)?;

        let mut reader = directory_reader_util::open(directory.clone())?;
        let segment_infos = reader.segment_infos.as_mut().unwrap();

        let lock = directory.obtain_lock("writer_lock")?;
        let lock_dir = Arc::new(LockValidatingDirectoryWrapper::new(directory.clone(), lock));

        let pool = ReaderPool::new::<String, DummyLeafReader, DummyComparator, DummyDirectory>(
            lock_dir,
            directory.clone(),
            Arc::new(InfoStreamEnum::default()),
            None,
            LongSupplierImpl,
            None,
            index_created_version_major,
        );

        let id = random.random_range(0..10);

        if random.random_bool(0.5) {
            pool.enable_reader_pooling();
        }

        for (idx, seg_id) in segment_infos.segments_idx.clone().iter().enumerate() {
            let (read_only_clone, max_doc, readers_and_updates, mut postings) = {
                let commit_info = segment_infos.info_idx_mut(idx).unwrap();
                let readers_and_updates = pool.get(commit_info, true, None)?.unwrap();
                let read_only_clone = readers_and_updates
                    .get_read_only_clone(&IOContext::default_io_context()?, commit_info)?
                    .unwrap();

                let term = Term::from_text("id", id.to_string());
                let postings = read_only_clone.postings(&term)?;
                (
                    read_only_clone,
                    commit_info.info.max_doc()?,
                    readers_and_updates,
                    postings,
                )
            };
            let mut expect_update = false;
            let mut doc = -1_i32;

            if let Some(ref mut postings) = postings {
                if postings.next_doc()? != NO_MORE_DOCS {
                    let sub_update1 = NumericDocValuesFieldUpdates::new()?;
                    let mut number_updates = DocValuesFieldUpdates::new(
                        max_doc,
                        0,
                        "number",
                        sub_update1.sub_type(),
                        sub_update1,
                    )?;
                    doc = postings.doc_id();
                    number_updates.add_value(doc, 1000_i64)?;
                    number_updates.finish()?;

                    readers_and_updates.add_dv_update(number_updates)?;
                    expect_update = true;

                    assert_eq!(NO_MORE_DOCS, postings.next_doc()?);
                    assert!(pool.any_doc_values_changes());
                } else {
                    assert!(!pool.any_doc_values_changes());
                }
            } else {
                assert!(!pool.any_doc_values_changes());
            }
            read_only_clone.close()?;
            let written_to_disk: bool;
            if pool.is_reader_pooling_enabled() {
                if random.random_bool(0.5) {
                    written_to_disk = pool.write_all_doc_values_updates(
                        &mut segment_infos.segments,
                        &field_numbers.lock(),
                    )?;
                    assert!(!readers_and_updates.is_merging());
                } else if random.random_bool(0.5) {
                    written_to_disk = pool.commit(segment_infos, &field_numbers.lock())?;
                    assert!(!readers_and_updates.is_merging());
                } else {
                    written_to_disk = pool.write_doc_values_updates_for_merge(
                        vec![seg_id.clone()].as_ref(),
                        segment_infos,
                        &field_numbers.lock(),
                    )?;
                    assert!(readers_and_updates.is_merging());
                }
                assert!(!pool.release(
                    &readers_and_updates,
                    random.random_bool(0.5),
                    segment_infos.info_idx_mut(idx).unwrap(),
                    &field_numbers.lock(),
                )?);
            } else if random.random_bool(0.5) {
                written_to_disk = pool.release(
                    &readers_and_updates,
                    random.random_bool(0.5),
                    segment_infos.info_idx_mut(idx).unwrap(),
                    &field_numbers.lock(),
                )?;
                assert!(!readers_and_updates.is_merging());
            } else {
                written_to_disk = pool.write_doc_values_updates_for_merge(
                    vec![seg_id.clone()].as_ref(),
                    segment_infos,
                    &field_numbers.lock(),
                )?;
                assert!(readers_and_updates.is_merging());

                assert!(!pool.release(
                    &readers_and_updates,
                    random.random_bool(0.5),
                    segment_infos.info_idx_mut(idx).unwrap(),
                    &field_numbers.lock(),
                )?);
            }

            assert!(!pool.any_doc_values_changes());
            assert_eq!(expect_update, written_to_disk);

            let commit_info = segment_infos.info_idx_mut(idx).unwrap();
            if expect_update {
                let readers_and_updates = pool.get(commit_info, true, None)?.unwrap();
                let updated_reader = readers_and_updates
                    .get_read_only_clone(&IOContext::default_io_context()?, commit_info)?
                    .unwrap();

                assert_ne!(-1, doc);

                let mut number = updated_reader
                    .get_numeric_doc_values("number")?
                    .expect("numeric dv missing");

                assert_eq!(doc, number.advance(doc)?);
                assert_eq!(1000_i64, number.long_value()?);

                readers_and_updates.release(updated_reader.as_ref(), None)?;
                assert!(!pool.release(
                    &readers_and_updates,
                    random.random_bool(0.5),
                    commit_info,
                    &field_numbers.lock(),
                )?);
            }
        }

        pool.close()?;
        Ok(())
    }
    #[test]
    fn test_deletes() -> Result<()> {
        let mut random = random();
        let directory = new_directory_shared(&mut random)?;

        let (field_numbers, index_created_version_major) =
            build_index(directory.clone(), &mut random)?;

        let mut reader = directory_reader_util::open(directory.clone())?;
        let segment_infos = reader.segment_infos.as_mut().unwrap();

        let lock = directory.obtain_lock("writer_lock")?;
        let lock_dir = Arc::new(LockValidatingDirectoryWrapper::new(directory.clone(), lock));

        let pool = ReaderPool::new::<String, DummyLeafReader, DummyComparator, DummyDirectory>(
            lock_dir,
            directory.clone(),
            Arc::new(InfoStreamEnum::default()),
            None,
            LongSupplierImpl,
            None,
            index_created_version_major,
        );

        let id = random.random_range(0..10);

        if random.random_bool(0.5) {
            pool.enable_reader_pooling();
        }

        for idx in 0..segment_infos.segments_idx.len() {
            let (read_only_clone, _max_doc, readers_and_updates, mut postings) = {
                let commit_info = segment_infos.info_idx_mut(idx).unwrap();
                let readers_and_updates = pool.get(commit_info, true, None)?.unwrap();
                let read_only_clone = readers_and_updates
                    .get_read_only_clone(&IOContext::default_io_context()?, commit_info)?
                    .unwrap();

                let term = Term::from_text("id", id.to_string());
                let postings = read_only_clone.postings(&term)?;
                (
                    read_only_clone,
                    commit_info.info.max_doc()?,
                    readers_and_updates,
                    postings,
                )
            };
            let mut expect_update = false;
            let mut doc = -1_i32;
            if let Some(ref mut postings) = postings
                && postings.next_doc()? != NO_MORE_DOCS
            {
                doc = postings.doc_id();
                assert!(readers_and_updates.delete(
                    postings.doc_id(),
                    segment_infos.info_idx_mut(idx).unwrap(),
                    None
                )?);
                expect_update = true;
                assert_eq!(NO_MORE_DOCS, postings.next_doc()?);
            };
            assert!(!pool.any_doc_values_changes()); // deletes are not accounted here
            read_only_clone.close()?;
            let written_to_disk: bool;
            if pool.is_reader_pooling_enabled() {
                written_to_disk = pool.commit(segment_infos, &field_numbers.lock())?;
                let commit_info = segment_infos.info_idx_mut(idx).unwrap();
                assert!(!pool.release(
                    &readers_and_updates,
                    random.random_bool(0.5),
                    commit_info,
                    &field_numbers.lock(),
                )?);
            } else {
                let commit_info = segment_infos.info_idx_mut(idx).unwrap();
                written_to_disk = pool.release(
                    &readers_and_updates,
                    random.random_bool(0.5),
                    commit_info,
                    &field_numbers.lock(),
                )?;
            }

            assert!(!pool.any_doc_values_changes());
            assert_eq!(expect_update, written_to_disk);

            let mut commit_info = segment_infos.info_idx_mut(idx).unwrap().clone();
            if expect_update {
                let readers_and_updates = pool.get(&commit_info, true, None)?.unwrap();
                let updated_reader = readers_and_updates
                    .get_read_only_clone(&IOContext::default_io_context()?, &commit_info)?
                    .unwrap();

                assert_ne!(-1, doc);
                assert!(
                    !updated_reader
                        .get_live_docs()?
                        .as_ref()
                        .unwrap()
                        .get(doc as usize)?
                );
                readers_and_updates.release(updated_reader.as_ref(), None)?;
                assert!(!pool.release(
                    &readers_and_updates,
                    random.random_bool(0.5),
                    &mut commit_info,
                    &field_numbers.lock(),
                )?);
            }
        }
        pool.close()?;
        Ok(())
    }

    fn test_pass_reader_to_merge_policy_concurrently() -> Result<()> {
        // TODO
        Ok(())
    }
    fn test_get_reader_by_ram() -> Result<()> {
        // TODO: memory calculation not implemented
        Ok(())
    }

    fn build_index<D: Directory, R: Rng + ?Sized>(
        directory: Arc<D>,
        random: &mut R,
    ) -> Result<(FieldNumbersLock, i32)> {
        let writer = IndexWriter::new(directory, new_index_writer_config(random))?;
        for i in 0..10 {
            let mut document = Document::new();
            document.add(StringField::with_string("id", i.to_string(), Yes)?);
            document.add(NumericDocValuesField::new("number", i));

            writer.add_document(document)?;

            if random.random_bool(0.5) {
                writer.flush()?;
            }
        }
        writer.commit()?;
        let field_numbers = writer.global_field_number_map.clone();

        writer.close()?;

        Ok((field_numbers, writer.get_index_major_version_created()))
    }
}
