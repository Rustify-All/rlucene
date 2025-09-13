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
use crate::core::index::index_commit::IndexCommit;
use crate::core::index::standard_directory_reader::StandardDirectoryReader;
use crate::core::store::dummy::dummy_directory::DummyDirectory;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub struct DummyIndexCommit;

impl PartialEq for DummyIndexCommit {
    fn eq(&self, _other: &Self) -> bool {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }
}

impl Eq for DummyIndexCommit {}

impl PartialOrd for DummyIndexCommit {
    fn partial_cmp(&self, _other: &Self) -> Option<Ordering> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }
}

impl Ord for DummyIndexCommit {
    fn cmp(&self, _other: &Self) -> Ordering {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }
}

impl Display for DummyIndexCommit {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }
}

impl IndexCommit for DummyIndexCommit {
    fn get_segments_file_name(&self) -> &str {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn get_file_names(&self) -> crate::core::util::error::lucene_error::Result<&[String]> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    type Directory = DummyDirectory;

    fn get_directory(&self) -> Arc<Self::Directory> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn delete(&mut self) -> crate::core::util::error::lucene_error::Result<()> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn is_deleted(&self) -> bool {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn get_segment_count(&self) -> usize {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn get_generation(&self) -> i64 {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn user_data(&self) -> &HashMap<String, String> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn get_reader(&self) -> Option<StandardDirectoryReader> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }
}
