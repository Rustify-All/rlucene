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
use crate::core::util::HasIdentity;
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::Result;
use std::collections::HashSet;
use std::fmt::{Display, Formatter};

/// Directory implementation that delegates calls to another directory.
///
/// This struct can be used to add limitations on top of an existing
/// [`Directory`] implementation such as
/// [`NRTCachingDirectory`](crate::core::store::nrt_caching_directory::NRTCachingDirectory), or to add additional
/// sanity checks for tests.
///
/// However, if you plan to write your own [`Directory`] implementation,
/// you should consider extending directly [`Directory`] or
/// [`BaseDirectory`](crate::core::store::base_directory::BaseDirectory) rather than
/// trying to reuse functionality of existing [`Directory`]s by wrapping this
/// one.
pub struct FilterDirectory<D>
where
    D: Directory,
{
    pub(crate) delegate: D,
    id: Identity,
}
impl<D> FilterDirectory<D>
where
    D: Directory,
{
    pub fn new(inner: D) -> Self {
        FilterDirectory {
            delegate: inner,
            id: Identity::new(),
        }
    }
    pub fn get_inner(&mut self) -> &mut D {
        &mut self.delegate
    }
}

impl<D> Display for FilterDirectory<D>
where
    D: Directory,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "FilterDirectory({})", self.delegate)
    }
}

impl<D> Closeable for FilterDirectory<D>
where
    D: Directory,
{
    fn close(&mut self) -> Result<()> {
        // TODO
        Ok(())
    }
}

impl<D> HasIdentity for FilterDirectory<D>
where
    D: Directory,
{
    fn identity(&self) -> &Identity {
        &self.id
    }
}

impl<D> Directory for FilterDirectory<D>
where
    D: Directory,
{
    fn list_all(&self) -> Result<Vec<String>> {
        self.delegate.list_all()
    }

    fn delete_file(&self, name: &str) -> Result<()> {
        self.delegate.delete_file(name)
    }

    fn file_length(&self, name: &str) -> Result<usize> {
        self.delegate.file_length(name)
    }

    fn create_output(&self, name: &str, context: &IOContext) -> Result<Self::IndexOutput> {
        self.delegate.create_output(name, context)
    }

    type IndexOutput = D::IndexOutput;

    fn create_temp_output(
        &self,
        prefix: &str,
        suffix: &str,
        context: &IOContext,
    ) -> Result<Self::IndexOutput> {
        self.delegate.create_temp_output(prefix, suffix, context)
    }

    fn sync(&self, names: &[String]) -> Result<()> {
        self.delegate.sync(names)
    }

    fn sync_metadata(&self) -> Result<()> {
        self.delegate.sync_metadata()
    }

    fn rename(&self, source: &str, dest: &str) -> Result<()> {
        self.delegate.rename(source, dest)
    }

    type IndexInput = D::IndexInput;

    fn open_input(&self, name: &str, context: &IOContext) -> Result<Self::IndexInput> {
        self.delegate.open_input(name, context)
    }

    type Lock = D::Lock;

    fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
        self.delegate.obtain_lock(name)
    }

    fn get_pending_deletions(&self) -> Result<HashSet<String>> {
        self.delegate.get_pending_deletions()
    }
}
