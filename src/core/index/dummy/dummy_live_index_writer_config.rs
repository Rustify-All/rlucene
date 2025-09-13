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
use crate::core::analysis::dummy::dummy_analyzer::DummyAnalyzer;
use crate::core::codecs::lucene101_codec::Lucene101Codec;
use crate::core::index::dummy::dummy_flush_policy::DummyFlushPolicy;
use crate::core::index::dummy::dummy_index_commit::DummyIndexCommit;
use crate::core::index::dummy::dummy_merge_policy::DummyMergePolicy;
use crate::core::index::index_writer_config::OpenMode;
use crate::core::index::keep_only_last_commit_deletion_policy::KeepOnlyLastCommitDeletionPolicy;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::sort::Sort;
use crate::core::search::dummy::dummy_similarity::DummySimilarity;
use crate::core::util::info_stream::{InfoStreamEnum, InfoStreamMT, NoOutput};
use std::sync::Arc;

pub struct DummyLiveIndexWriterConfig {
    info_stream: InfoStreamMT,
    codec: Lucene101Codec,
    analyzer: DummyAnalyzer,
    similarity: DummySimilarity,
}
impl Default for DummyLiveIndexWriterConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl DummyLiveIndexWriterConfig {
    pub fn new() -> Self {
        DummyLiveIndexWriterConfig {
            info_stream: Arc::new(InfoStreamEnum::NoOutput(NoOutput)),
            codec: Lucene101Codec,
            analyzer: DummyAnalyzer,
            similarity: DummySimilarity,
        }
    }
}
impl LiveIndexWriterConfig for DummyLiveIndexWriterConfig {
    type Analyzer = DummyAnalyzer;

    fn get_analyzer(&self) -> &Self::Analyzer {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    type Similarity = DummySimilarity;

    fn get_similarity(&self) -> &Self::Similarity {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    type Codec = Lucene101Codec;

    fn get_codec(&self) -> &Self::Codec {
        &self.codec
    }

    fn get_index_sort(&self) -> Option<Arc<Sort>> {
        None
    }

    fn get_use_compound_file(&self) -> bool {
        false
    }

    fn get_soft_deletes_field(&self) -> Option<&str> {
        None
    }

    fn get_info_stream(&self) -> InfoStreamMT {
        self.info_stream.clone()
    }

    fn get_parent_field(&self) -> Option<&str> {
        None
    }

    type MergePolicy = DummyMergePolicy;

    fn get_merge_policy(&self) -> &Self::MergePolicy {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    type FlushPolicy = DummyFlushPolicy;

    fn get_flush_policy(&self) -> &Self::FlushPolicy {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn get_ram_buffer_size_mb(&self) -> f64 {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn get_ram_per_thread_hard_limit_mb(&self) -> i32 {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn get_max_buffered_docs(&self) -> i32 {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn get_check_pending_flush_on_update(&self) -> bool {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    type IndexDeletionPolicy = KeepOnlyLastCommitDeletionPolicy;

    fn get_index_deletion_policy(&self) -> &Self::IndexDeletionPolicy {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn get_max_full_flush_merge_wait_millis(&self) -> i64 {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn get_commit_on_close(&self) -> bool {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn get_open_mode(&self) -> &OpenMode {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    type IndexCommit = DummyIndexCommit;

    fn get_index_commit(&mut self) -> Option<Self::IndexCommit> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn get_index_created_version_major(&self) -> i32 {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn get_reader_pooling(&self) -> bool {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }
}
