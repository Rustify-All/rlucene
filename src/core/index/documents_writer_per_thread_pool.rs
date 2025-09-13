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
use crate::core::index::approximate_priority_queue::IdentityId;
use crate::core::index::documents_writer_per_thread::{DocumentsWriterPerThread, State};
use crate::core::index::lockable_concurrent_approximate_priority_queue::{
    Lock, LockableConcurrentApproximatePriorityQueue,
};
use crate::core::store::directory::Directory;
use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::supplier::Supplier;
use parking_lot::{Condvar, Mutex};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// [`DocumentsWriterPerThreadPool`] controls [`DocumentsWriterPerThread`] instances and their thread assignments during indexing.
/// Each [`DocumentsWriterPerThread`] is obtained from the pool and exclusively used for indexing a single document or list of documents by the obtaining thread.
/// Each indexing thread must obtain such a [`DocumentsWriterPerThread`] to make progress. Depending on the [`DocumentsWriterPerThreadPool`] implementation, [`DocumentsWriterPerThread`]
/// assignments might differ from document to document.
///
/// Once a [`DocumentsWriterPerThread`] is selected for flush, it will be checked out of the thread pool and won’t be reused for indexing. See [`checkout`](DocumentsWriterPerThreadPool::checkout)
pub(crate) struct DocumentsWriterPerThreadPool<D>
where
    D: Directory,
{
    pub(crate) inner: Mutex<Inner>,
    free_list: LockableConcurrentApproximatePriorityQueue<DocumentsWriterPerThread<D>>,
    pausing: Condvar,
    closed: AtomicBool,
}
pub(crate) struct Inner {
    pub(crate) dwpts: HashMap<String, Dwpts>,
    taken_writer_permits: i32,
}
pub(crate) struct Dwpts {
    pub(crate) r#gen: i64,
    state: Arc<State>,
}
impl<D> DocumentsWriterPerThreadPool<D>
where
    D: Directory,
{
    pub fn new() -> Result<Self> {
        let inner = Mutex::new(Inner {
            dwpts: HashMap::new(),
            taken_writer_permits: 0,
        });
        Ok(Self {
            inner,
            free_list: LockableConcurrentApproximatePriorityQueue::new()?,
            pausing: Condvar::new(),
            closed: AtomicBool::new(false),
        })
    }
    /// Returns the active number of [`DocumentsWriterPerThread`] instances.
    pub(crate) fn size(&self) -> usize {
        let inner = self.inner.lock();
        inner.dwpts.len()
    }

    pub(crate) fn lock_new_writers(&self) {
        // this is similar to a semaphore - we need to acquire all permits ie. takenWriterPermits must
        // be == 0
        // any call to lockNewWriters() must be followed by unlockNewWriters() otherwise we will
        // deadlock at some
        // point
        let mut inner = self.inner.lock();
        debug_assert!(inner.taken_writer_permits >= 0);
        inner.taken_writer_permits += 1;
    }
    pub(crate) fn unlock_new_writers(&self) {
        let mut inner = self.inner.lock();
        debug_assert!(inner.taken_writer_permits > 0);
        inner.taken_writer_permits -= 1;

        if inner.taken_writer_permits == 0 {
            self.pausing.notify_all();
        }
    }

    /// Returns a new already locked [`DocumentsWriterPerThread`]
    pub(crate) fn new_writer<S>(&self, dwpt_factory: &S) -> Result<DocumentsWriterPerThread<D>>
    where
        S: Supplier<DocumentsWriterPerThread<D>>,
    {
        let mut inner = self.inner.lock();
        debug_assert!(inner.taken_writer_permits >= 0);
        while inner.taken_writer_permits > 0 {
            self.pausing.wait(&mut inner);
        }
        // we must check if we are closed since this might happen while we are waiting for the writer
        // permit
        // and if we miss that we might release a new DWPT even though the pool is closed. Yet, that
        // wouldn't be the
        // end of the world it's violating the contract that we don't release any new DWPT after this
        // pool is closed
        self.ensure_open()?;
        let dwpt = dwpt_factory.get_immutable()?;
        let delete_queue_gen = dwpt.delete_queue.generation;
        dwpt.lock();
        let dwpts = Dwpts {
            r#gen: delete_queue_gen,
            state: dwpt.state.clone(),
        };
        inner.dwpts.insert(dwpt.id().to_string(), dwpts);
        Ok(dwpt)
    }
    /// This method is used by `DocumentsWriter`/`FlushControl` to obtain a DWPT to do an indexing
    /// operation (add/updateDocument).
    pub(crate) fn get_and_lock<S>(&self, dwpt_factory: &S) -> Result<DocumentsWriterPerThread<D>>
    where
        S: Supplier<DocumentsWriterPerThread<D>>,
    {
        self.ensure_open()?;

        if let Some(dwpt) = self.free_list.lock_and_poll() {
            return Ok(dwpt);
        }
        // newWriter() adds the DWPT to the `dwpts` set as a side-effect. However it is not added to
        // `freeList` at this point, it will be added later on once DocumentsWriter has indexed a
        // document into this DWPT and then gives it back to the pool by calling
        // #marksAsFreeAndUnlock.
        self.new_writer(dwpt_factory)
    }

    fn ensure_open(&self) -> Result<()> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(LuceneError::already_closed("DWPTPool is already closed"));
        }
        Ok(())
    }

    pub(crate) fn contains(&self, state: &DocumentsWriterPerThread<D>) -> bool {
        let inner = self.inner.lock();
        inner.dwpts.contains_key(state.id())
    }
    pub(crate) fn mark_as_free_and_unlock(&self, state: DocumentsWriterPerThread<D>) -> Result<()> {
        let ram_bytes_used = state.ram_bytes_used()?;

        debug_assert!(
            !state.is_flush_pending() && !state.is_aborted() && !state.is_queue_advanced(),
            "DWPT has pending flush: {}, aborted={}, queueAdvanced={}",
            state.is_flush_pending(),
            state.is_aborted(),
            state.is_queue_advanced()
        );

        debug_assert!(
            self.contains(&state),
            "Tried to add a DWPT back to the pool but the pool doesn't know about this DWPT"
        );

        self.free_list.add_and_unlock(state, ram_bytes_used);
        Ok(())
    }
    /// Filters all `DocumentsWriterPerThread`s that the given predicate applies to and that can be checked out of the pool via [`checkout`](Self::checkout).
    /// All returned DWPTs are already locked, and [`is_registered`](Self::is_registered) will return `true` for each one.
    pub(crate) fn filter_and_lock<F1>(
        &self,
        predicate: F1,
    ) -> Result<Vec<DocumentsWriterPerThread<D>>>
    where
        F1: Fn(&str, i64) -> bool,
    {
        let mut list = Vec::new();
        let inner = self.inner.lock();
        for (id, state) in inner.dwpts.iter() {
            if predicate(id, state.r#gen) {
                state.state.lock();
                if self.is_registered_with_state(id, Some(&inner)) {
                    list.push(id.clone());
                } else {
                    state.state.unlock();
                }
            }
        }
        // locked dwpt are safely remove from `free_list`
        let mut result = Vec::new();
        for id in list {
            let dwpt = self.free_list.remove(&id);
            debug_assert!(
                dwpt.is_some() && dwpt.as_ref().unwrap().is_locked(),
                "DWPT {id} is not locked, but it was expected to be locked"
            );
            result.push(dwpt.unwrap());
        }
        Ok(result)
    }
    /// Removes the given DWPT from the pool unless it has already been removed.
    ///
    /// # Returns
    ///
    /// `true` if the DWPT was removed; `false` otherwise.
    pub(crate) fn checkout(&self, per_thread: &str) -> bool {
        let mut inner = self.inner.lock();

        if inner.dwpts.remove(per_thread).is_some() {
            self.free_list.remove(per_thread);
            true
        } else {
            debug_assert!(!self.free_list.contains(per_thread));
            false
        }
    }
    ///  Returns `true` if this DWPT is still part of the pool
    pub(crate) fn is_registered(&self, per_thread: &str) -> bool {
        let inner = self.inner.lock();
        self.is_registered_with_state(per_thread, Some(&inner))
    }
    fn is_registered_with_state(&self, per_thread: &str, state: Option<&Inner>) -> bool {
        let state = match state {
            Some(s) => s,
            None => &*self.inner.lock(),
        };
        state.dwpts.contains_key(per_thread)
    }
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}
#[cfg(test)]
mod tests {

    use crate::core::index::approximate_priority_queue::IdentityId;
    use crate::core::index::documents_writer_delete_queue::DocumentsWriterDeleteQueue;
    use crate::core::index::documents_writer_per_thread::DocumentsWriterPerThread;
    use crate::core::index::documents_writer_per_thread_pool::DocumentsWriterPerThreadPool;
    use crate::core::index::dummy::dummy_live_index_writer_config::DummyLiveIndexWriterConfig;
    use crate::core::index::field_infos::FieldNumbers;
    use crate::core::index::field_infos::build::Builder;

    use crate::core::index::lockable_concurrent_approximate_priority_queue::Lock;

    use crate::core::store::directory::Directory;
    use crate::core::store::lock_validating_directory_wrapper::LockValidatingDirectoryWrapper;
    use crate::core::store::nio_fs_directory::NIOFSDirectory;
    use crate::core::store::{FSDirectory, NativeFSLockFactory};
    use crate::core::util::LATEST;
    use crate::core::util::error::lucene_error::{LuceneError, Result};
    use crate::core::util::info_stream::{InfoStreamEnum, NoOutput};
    use crate::core::util::supplier::Supplier;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{
        new_directory, random, random_from_seed,
    };
    use parking_lot::Mutex;
    use rand::Rng;
    use std::sync::Arc;
    use std::sync::atomic::AtomicI64;

    #[allow(dead_code)] // for quick search
    struct TestDocumentsWriterPerThreadPool;

    struct DwptSupplier {
        seed: u64,
    }
    impl DwptSupplier {
        pub fn new(seed: u64) -> Self {
            Self { seed }
        }
    }
    impl Supplier<DocumentsWriterPerThread<FSDirectory<NativeFSLockFactory, NIOFSDirectory>>>
        for DwptSupplier
    {
        fn get_immutable(
            &self,
        ) -> Result<DocumentsWriterPerThread<FSDirectory<NativeFSLockFactory, NIOFSDirectory>>>
        {
            let mut random = random_from_seed(self.seed);
            let directory_orig = Arc::new(new_directory(&mut random)?);
            let lock = directory_orig.obtain_lock("test")?;
            let directory = Arc::new(LockValidatingDirectoryWrapper::new(
                directory_orig.clone(),
                lock,
            ));
            // TODO: LuceneTestCase::newIndexWriterConfig 为实现
            let dummy_config = DummyLiveIndexWriterConfig::new();
            DocumentsWriterPerThread::new(
                LATEST.major,
                "",
                directory_orig,
                directory,
                &dummy_config,
                Arc::new(DocumentsWriterDeleteQueue::new(Arc::new(
                    InfoStreamEnum::NoOutput(NoOutput),
                ))),
                Builder::new(Arc::new(Mutex::new(FieldNumbers::new(
                    Some("padding1"),
                    Some("padding2"),
                )?))),
                Arc::new(AtomicI64::new(0)),
                false,
            )
        }
    }

    #[test]
    fn test_lock_release_and_close() -> Result<()> {
        let mut random = random();
        let pool = DocumentsWriterPerThreadPool::new()?;
        let supplier = DwptSupplier::new(random.random());
        let first = pool.get_and_lock(&supplier)?;
        assert_eq!(pool.size(), 1);

        let second = pool.get_and_lock(&supplier)?;
        assert_eq!(pool.size(), 2);

        let first_id = first.id().to_string();
        pool.mark_as_free_and_unlock(first)?;
        assert_eq!(pool.size(), 2);

        let third = pool.get_and_lock(&supplier)?;
        assert_eq!(first_id, third.id().to_string());
        assert_eq!(pool.size(), 2);

        pool.checkout(third.id());
        assert_eq!(pool.size(), 1);

        pool.close();
        assert_eq!(pool.size(), 1);

        pool.mark_as_free_and_unlock(second)?;
        assert_eq!(pool.size(), 1);

        let v = pool.filter_and_lock(|_, _| true)?;
        for dwpt in v {
            pool.checkout(dwpt.id());
            assert!(dwpt.state.is_locked());
            dwpt.unlock();
        }
        assert_eq!(pool.size(), 0);
        Ok(())
    }
    #[test]
    fn test_close_while_new_writers_locked() -> Result<()> {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        use std::thread;
        use std::time::Duration;

        let mut random = random();
        let supplier = DwptSupplier::new(random.random());
        let pool = Arc::new(DocumentsWriterPerThreadPool::new()?);

        let first = pool.get_and_lock(&supplier)?;
        pool.lock_new_writers();

        let ready = Arc::new(AtomicBool::new(false));
        let ready_clone = ready.clone();
        let pool_clone = pool.clone();

        let handle = thread::spawn(move || {
            ready_clone.store(true, Ordering::SeqCst);
            let result = pool_clone.get_and_lock(&supplier);
            assert!(matches!(result, Err(LuceneError::AlreadyClosed(_))));
        });

        while !ready.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(10));
        }

        thread::sleep(Duration::from_millis(1000));

        first.unlock();
        pool.close();
        pool.unlock_new_writers();

        handle.join().unwrap();
        let ids = {
            let inner = pool.inner.lock();
            inner.dwpts.keys().cloned().collect::<Vec<_>>()
        };
        for dwpt in ids {
            pool.checkout(&dwpt);
            // dwpt.unlock()?;
        }

        assert_eq!(pool.size(), 0);
        Ok(())
    }
}
