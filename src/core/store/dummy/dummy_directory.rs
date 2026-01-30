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
use crate::core::index::index_reader::Identity;
use crate::core::store::IOContext;
use crate::core::store::directory::Directory;
use crate::core::store::dummy::dummy_index_input::DummyIndexInput;
use crate::core::store::dummy::dummy_index_output::DummyIndexOutput;
use crate::core::store::dummy::dummy_lock::DummyLock;
use crate::core::util::HasIdentity;
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::Result;
use std::collections::HashSet;
use std::fmt::{Display, Formatter};

pub struct DummyDirectory;
impl Display for DummyDirectory {
    fn fmt(&self, _f: &mut Formatter<'_>) -> std::fmt::Result {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }
}

impl Closeable for DummyDirectory {
    fn close(&mut self) -> Result<()> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }
}

impl HasIdentity for DummyDirectory {
    fn identity(&self) -> &Identity {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }
}

impl Directory for DummyDirectory {
    fn list_all(&self) -> Result<Vec<String>> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn delete_file(&self, _name: &str) -> Result<()> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn file_length(&self, _name: &str) -> Result<usize> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }
    fn create_output(&self, _name: &str, _context: &IOContext) -> Result<DummyIndexOutput> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    type IndexOutput = DummyIndexOutput;
    fn create_temp_output(
        &self,
        _prefix: &str,
        _suffix: &str,
        _context: &IOContext,
    ) -> Result<Self::IndexOutput> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn sync(&self, _names: &[String]) -> Result<()> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn sync_metadata(&self) -> Result<()> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn rename(&self, _source: &str, _dest: &str) -> Result<()> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    type IndexInput = DummyIndexInput;

    fn open_input(&self, _name: &str, _context: &IOContext) -> Result<Self::IndexInput> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    type Lock = DummyLock;

    fn obtain_lock(&self, _name: &str) -> Result<Self::Lock> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn get_pending_deletions(&self) -> Result<HashSet<String>> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }
}
