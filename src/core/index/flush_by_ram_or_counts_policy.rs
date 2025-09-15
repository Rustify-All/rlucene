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
use crate::core::index::flush_policy::FlushPolicy;
use crate::core::index::index_writer_config::DISABLE_AUTO_FLUSH;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::store::directory::Directory;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::info_stream::InfoStream;
use parking_lot::MutexGuard;
/// Default [`FlushPolicy`] implementation that flushes new segments based on RAM usage and
/// document count, depending on the `IndexWriter`'s [`IndexWriterConfig`](crate::core::index::index_writer_config::IndexWriterConfig).
/// It also applies pending deletes based on the number of buffered delete terms.
///
/// All [`IndexWriterConfig`](crate::core::index::index_writer_config::IndexWriterConfig) settings are used to mark [`DocumentsWriterPerThread`] as
/// flush-pending during indexing with respect to their live updates.
///
/// If [`IndexWriterConfig::set_ram_buffer_size_mb`](crate::core::index::index_writer_config::IndexWriterConfig::set_ram_buffer_size_mb) is enabled, the largest RAM-consuming
/// [`DocumentsWriterPerThread`] will be marked as pending **iff** the global active RAM consumption
/// is `>=` the configured max RAM buffer.
pub struct FlushByRamOrCountsPolicy;

impl FlushByRamOrCountsPolicy {
    fn flush_deletes<D, L>(
        &self,
        control: &DocumentsWriterFlushControl<D, L>,
        index_writer_config: &impl LiveIndexWriterConfig,
        delete_queue: &DocumentsWriterDeleteQueue,
    ) -> Result<()>
    where
        D: Directory,
        L: LiveIndexWriterConfig,
    {
        control.set_apply_all_deletes();

        if control.info_stream.enabled("FP") {
            control.info_stream.message(
                "FP",
                &format!(
                    "force apply deletes bytesUsed={} vs ramBufferMB={}",
                    control.get_delete_bytes_used(delete_queue)?,
                    index_writer_config.get_ram_buffer_size_mb()
                ),
            );
        }

        Ok(())
    }
    fn flush_active_bytes<D, L>(
        &self,
        control: &DocumentsWriterFlushControl<D, L>,
        per_thread: &DocumentsWriterPerThread<D>,
        delete_queue: &DocumentsWriterDeleteQueue,
        inner: &mut Inner<D>,
    ) -> Result<()>
    where
        D: Directory,
        L: LiveIndexWriterConfig,
    {
        if control.info_stream.enabled("FP") {
            control.info_stream.message(
                "FP",
                &format!(
                    "trigger flush: activeBytes={} deleteBytes={} vs ramBufferMB={}",
                    control.active_bytes(Some(inner)),
                    control.get_delete_bytes_used(delete_queue)?,
                    control.config.get_ram_buffer_size_mb()
                ),
            );
        }

        self.mark_largest_writer_pending(control, per_thread, inner)?;
        Ok(())
    }
    /// Marks the most ram consuming active [`DocumentsWriterPerThread`] flush pending
    pub(crate) fn mark_largest_writer_pending<D, L>(
        &self,
        control: &DocumentsWriterFlushControl<D, L>,
        per_thread: &DocumentsWriterPerThread<D>,
        inner: &mut Inner<D>,
    ) -> Result<()>
    where
        D: Directory,
        L: LiveIndexWriterConfig,
    {
        let largest_non_pendingwriter =
            self.find_largest_non_pending_writer_for_thread(control, per_thread);
        if let Some(largest_non_pendingwriter) = largest_non_pendingwriter {
            control.set_flush_pending(&*largest_non_pendingwriter.dwpt.lock(), Some(inner))?;
        }
        Ok(())
    }
    /// Returns `true` if this [`FlushPolicy`](crate::core::index::flush_policy::FlushPolicy) flushes on
    /// [`LiveIndexWriterConfig::get_max_buffered_docs`], otherwise `false`.
    fn flush_on_doc_count<L>(&self, index_writer_config: &L) -> bool
    where
        L: LiveIndexWriterConfig,
    {
        index_writer_config.get_max_buffered_docs() != DISABLE_AUTO_FLUSH
    }

    /// Returns `true` if this [`FlushPolicy`](crate::core::index::flush_policy::FlushPolicy) flushes on
    /// [`LiveIndexWriterConfig::get_ram_buffer_size_mb`], otherwise `false`.
    fn flush_on_ram<L>(&self, index_writer_config: &L) -> bool
    where
        L: LiveIndexWriterConfig,
    {
        index_writer_config.get_ram_buffer_size_mb() != DISABLE_AUTO_FLUSH as f64
    }
}
impl FlushPolicy for FlushByRamOrCountsPolicy {
    fn on_change<D, L>(
        &self,
        control: &DocumentsWriterFlushControl<D, L>,
        inner: &mut Inner<D>,
        per_thread: Option<&MutexGuard<'_, DocumentsWriterPerThread<D>>>,
        delete_queue: &DocumentsWriterDeleteQueue,
    ) -> Result<()>
    where
        D: Directory,
        L: LiveIndexWriterConfig,
    {
        let index_writer_config = control.config.as_ref();
        if let Some(pt) = per_thread
            && self.flush_on_doc_count(index_writer_config)
            && pt.get_num_docs_in_ram() >= index_writer_config.get_max_buffered_docs()
        {
            // Flush this state by num docs
            control.set_flush_pending(pt, Some(inner))?;
            return Ok(());
        }

        if self.flush_on_ram(index_writer_config) {
            let limit = (index_writer_config.get_ram_buffer_size_mb() * 1024.0 * 1024.0) as i64;
            let active_ram = control.active_bytes(Some(inner));
            let deletes_ram = control.get_delete_bytes_used(delete_queue)?;

            if deletes_ram >= limit
                && active_ram >= limit
                && let Some(pt) = per_thread
            {
                self.flush_deletes(control, index_writer_config, delete_queue)?;
                self.flush_active_bytes(control, pt, delete_queue, inner)?;
                return Ok(());
            }

            if deletes_ram >= limit {
                self.flush_deletes(control, index_writer_config, delete_queue)?;
            } else if active_ram + deletes_ram >= limit
                && let Some(pt) = per_thread
            {
                self.flush_active_bytes(control, pt, delete_queue, inner)?;
            }
        }
        Ok(())
    }
}
