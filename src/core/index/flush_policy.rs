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
use crate::core::index::documents_writer_delete_queue::DocumentsWriterDeleteQueue;
use crate::core::index::documents_writer_flush_control::{DocumentsWriterFlushControl, Inner};
use crate::core::index::documents_writer_per_thread::DocumentsWriterPerThread;
use crate::core::index::documents_writer_per_thread_pool::DwptWrapper;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::store::directory::Directory;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::info_stream::{InfoStream, InfoStreamEnum};
use parking_lot::MutexGuard;
use std::sync::Arc;
/// [`FlushPolicy`] controls when segments are flushed from a RAM resident internal
/// data structure to the [`IndexWriter`](crate::core::index::index_writer::IndexWriter)'s [`Directory`](crate::core::store::directory::Directory).
///
/// Segments are traditionally flushed by:
///
/// - RAM consumption – configured via [`IndexWriterConfig::set_ram_buffer_size_mb`](crate::core::index::index_writer_config::IndexWriterConfig::set_ram_buffer_size_mb)
/// - Number of RAM resident documents – configured via [`IndexWriterConfig::set_max_buffered_docs`](crate::core::index::index_writer_config::IndexWriterConfig::set_max_buffered_docs)
///
/// [`IndexWriter`](crate::core::index::index_writer::IndexWriter) consults the provided [`FlushPolicy`] to control the flushing process.
/// The policy is informed for each added or updated document as well as for each delete term.
///
/// Based on the [`FlushPolicy`], the information provided via [`DocumentsWriterPerThread`] and
/// [`DocumentsWriterFlushControl`], the [`FlushPolicy`] decides if a [`DocumentsWriterPerThread`]
/// needs flushing and marks it as flush-pending via
/// [`DocumentsWriterFlushControl::set_flush_pending`], or if deletes need to be applied.
///
/// See also:
/// - [`DocumentsWriterFlushControl`]
/// - [`DocumentsWriterPerThread`]
/// - [`IndexWriterConfig::set_flush_policy`]
pub trait FlushPolicy {
    /// Called for each delete, insert or update.
    /// For pure deletes, the given [`DocumentsWriterPerThread`] may be `None`.
    ///
    /// Note: This method is called synchronized on the given [`DocumentsWriterFlushControl`]
    /// and it is guaranteed that the calling thread holds the lock on the given
    /// [`DocumentsWriterPerThread`].
    fn on_change<D, L>(
        &self,
        control: &DocumentsWriterFlushControl<D, L>,
        inner: &mut Inner<D>,
        per_thread: Option<&MutexGuard<'_, DocumentsWriterPerThread<D>>>,
        delete_queue: &DocumentsWriterDeleteQueue,
    ) -> Result<()>
    where
        D: Directory,
        L: LiveIndexWriterConfig;
    /// Returns the current most RAM consuming non-pending [`DocumentsWriterPerThread`]
    /// with at least one indexed document.
    ///
    /// This method will never return `None`.
    fn find_largest_non_pending_writer_for_thread<D, L>(
        &self,
        control: &DocumentsWriterFlushControl<D, L>,
        per_thread: &DocumentsWriterPerThread<D>,
    ) -> Option<Arc<DwptWrapper<D>>>
    where
        D: Directory,
        L: LiveIndexWriterConfig,
    {
        debug_assert!(
            per_thread.state.get_num_docs_in_ram() > 0,
            "expected per_thread to have >0 docs in RAM"
        );
        // the dwpt which needs to be flushed eventually
        let max_ram_using_writer = control.find_largest_non_pending_writer();
        debug_assert!(self.assert_message(
            "set largest ram consuming thread pending on lower watermark",
            &control.info_stream
        ));
        max_ram_using_writer
    }

    fn assert_message(&self, s: &str, info_stream: &InfoStreamEnum) -> bool {
        if info_stream.enabled("FP") {
            info_stream.message("FP", s);
        }
        true
    }
}
