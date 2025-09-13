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
use crate::core::analysis::analyzer::Analyzer;
use crate::core::codecs::Codec;
use crate::core::index::flush_policy::FlushPolicy;
use crate::core::index::index_commit::IndexCommit;
use crate::core::index::index_deletion_policy::IndexDeletionPolicy;
use crate::core::index::index_writer_config::OpenMode;
use crate::core::index::merge_policy::MergePolicy;
use crate::core::index::sort::Sort;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::util::info_stream::InfoStreamMT;
use std::sync::Arc;

pub trait LiveIndexWriterConfig {
    type Analyzer: Analyzer;
    fn get_analyzer(&self) -> &Self::Analyzer;

    type Similarity: Similarity;
    fn get_similarity(&self) -> &Self::Similarity;

    type Codec: Codec;
    fn get_codec(&self) -> &Self::Codec;

    fn get_index_sort(&self) -> Option<Arc<Sort>>;

    fn get_use_compound_file(&self) -> bool;

    fn get_soft_deletes_field(&self) -> Option<&str>;

    fn get_info_stream(&self) -> InfoStreamMT;

    fn get_parent_field(&self) -> Option<&str>;

    type MergePolicy: MergePolicy;
    fn get_merge_policy(&self) -> &Self::MergePolicy;

    type FlushPolicy: FlushPolicy;
    fn get_flush_policy(&self) -> &Self::FlushPolicy;

    fn get_ram_buffer_size_mb(&self) -> f64;

    fn get_ram_per_thread_hard_limit_mb(&self) -> i32;

    fn get_max_buffered_docs(&self) -> i32;

    fn get_check_pending_flush_on_update(&self) -> bool;

    type IndexDeletionPolicy: IndexDeletionPolicy;
    fn get_index_deletion_policy(&self) -> &Self::IndexDeletionPolicy;

    fn get_max_full_flush_merge_wait_millis(&self) -> i64;

    fn get_commit_on_close(&self) -> bool;

    fn get_open_mode(&self) -> &OpenMode;

    type IndexCommit: IndexCommit;
    fn get_index_commit(&mut self) -> Option<Self::IndexCommit>;

    fn get_index_created_version_major(&self) -> i32;

    fn get_reader_pooling(&self) -> bool;
}
