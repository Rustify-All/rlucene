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
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::store::directory::Directory;
use crate::core::util::error::lucene_error::Result;
use parking_lot::MutexGuard;

pub struct DummyFlushPolicy;
impl FlushPolicy for DummyFlushPolicy {
    fn on_change<D, L>(
        &self,
        _control: &DocumentsWriterFlushControl<D, L>,
        _inner: &mut Inner<D>,
        _per_thread: Option<&MutexGuard<'_, DocumentsWriterPerThread<D>>>,
        _delete_queue: &DocumentsWriterDeleteQueue,
    ) -> Result<()>
    where
        D: Directory,
        L: LiveIndexWriterConfig,
    {
        Ok(())
    }
}
