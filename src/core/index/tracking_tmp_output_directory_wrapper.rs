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
use crate::core::store::directory::Directory;
use crate::core::store::filter_directory::FilterDirectory;
use crate::core::store::{IOContext, IndexOutput};
use crate::core::util::HasIdentity;
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::Result;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};

pub(crate) struct TrackingTmpOutputDirectoryWrapper<D>
where
    D: Directory,
{
    pub(crate) inner: RefCell<Inner>,
    base: FilterDirectory<D>,
    id: Identity,
}
pub(crate) struct Inner {
    pub(crate) file_names: HashMap<String, String>,
}
impl<D> TrackingTmpOutputDirectoryWrapper<D>
where
    D: Directory,
{
    pub(crate) fn new(input: D) -> Self {
        let inner = RefCell::new(Inner {
            file_names: HashMap::new(),
        });
        TrackingTmpOutputDirectoryWrapper {
            inner,
            base: FilterDirectory::new(input),
            id: Identity::new(),
        }
    }
    pub(crate) fn get_temporary_files(&self) -> &RefCell<Inner> {
        &self.inner
    }
}

impl<D> Display for TrackingTmpOutputDirectoryWrapper<D>
where
    D: Directory,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}({})", std::any::type_name::<Self>(), self.base)
    }
}

impl<D> Closeable for TrackingTmpOutputDirectoryWrapper<D>
where
    D: Directory,
{
    fn close(&mut self) -> Result<()> {
        // TODO
        Ok(())
    }
}

impl<D> HasIdentity for TrackingTmpOutputDirectoryWrapper<D>
where
    D: Directory,
{
    fn identity(&self) -> &Identity {
        &self.id
    }
}

impl<D> Directory for TrackingTmpOutputDirectoryWrapper<D>
where
    D: Directory,
{
    fn list_all(&self) -> Result<Vec<String>> {
        self.base.list_all()
    }

    fn delete_file(&self, name: &str) -> Result<()> {
        self.base.delete_file(name)
    }

    fn file_length(&self, name: &str) -> Result<usize> {
        self.base.file_length(name)
    }

    fn create_output(&self, name: &str, context: &IOContext) -> Result<Self::IndexOutput> {
        let output = self.base.create_temp_output(name, "", context)?;
        self.inner
            .borrow_mut()
            .file_names
            .insert(name.to_string(), output.get_name().to_string());
        Ok(output)
    }

    type IndexOutput = D::IndexOutput;

    fn create_temp_output(
        &self,
        prefix: &str,
        suffix: &str,
        context: &IOContext,
    ) -> Result<Self::IndexOutput> {
        self.base.create_temp_output(prefix, suffix, context)
    }

    fn sync(&self, names: &[String]) -> Result<()> {
        self.base.sync(names)
    }

    fn sync_metadata(&self) -> Result<()> {
        self.base.sync_metadata()
    }

    fn rename(&self, source: &str, dest: &str) -> Result<()> {
        self.base.rename(source, dest)
    }

    type IndexInput = D::IndexInput;

    fn open_input(&self, name: &str, context: &IOContext) -> Result<Self::IndexInput> {
        let inner = self.inner.borrow();
        let tmp_name = inner
            .file_names
            .get(name)
            .map(|s| s.as_str())
            .unwrap_or(name);
        self.base.open_input(tmp_name, context)
    }

    type Lock = D::Lock;

    fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
        self.base.obtain_lock(name)
    }

    fn copy_from(
        &self,
        from: &impl Directory,
        src: &str,
        dest: &str,
        context: &IOContext,
    ) -> Result<()> {
        self.base.copy_from(from, src, dest, context)
    }

    fn delete_files_ignoring_exceptions(&self, files: &[String]) {
        self.base.delete_files_ignoring_exceptions(files)
    }

    fn get_pending_deletions(&self) -> Result<HashSet<String>> {
        self.base.get_pending_deletions()
    }
}
