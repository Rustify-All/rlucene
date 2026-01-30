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
use crate::core::store::buffered_checksum_index_input::BufferedChecksumIndexInput;
use crate::core::store::directory::Directory;
use crate::core::util::HasIdentity;
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::collections::HashSet;
use std::fmt::{Display, Formatter};

/// A read-only [`Directory`] that provides a view over a compound file.
///
/// # See Also
/// - [`CompoundFormat`](crate::core::codecs::compound_format::CompoundFormat)
///
/// # Note
/// This API is experimental and may change in future versions.
pub struct CompoundDirectory<D>
where
    D: Directory,
{
    pub(crate) sub_compound_dir: D,
    id: Identity,
}

impl<D> CompoundDirectory<D>
where
    D: Directory,
{
    pub fn new(sub_compound_dir: D) -> Self {
        CompoundDirectory {
            sub_compound_dir,
            id: Identity::new(),
        }
    }
}

impl<D> Display for CompoundDirectory<D>
where
    D: Directory,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.sub_compound_dir.fmt(f)
    }
}

impl<D> Closeable for CompoundDirectory<D>
where
    D: Directory,
{
    fn close(&mut self) -> Result<()> {
        // TODO
        Ok(())
    }
}

impl<D> HasIdentity for CompoundDirectory<D>
where
    D: Directory,
{
    fn identity(&self) -> &Identity {
        &self.id
    }
}

impl<D> Directory for CompoundDirectory<D>
where
    D: Directory,
{
    fn list_all(&self) -> Result<Vec<String>> {
        self.sub_compound_dir.list_all()
    }

    fn delete_file(&self, _name: &str) -> Result<()> {
        Err(LuceneError::unsupported_operation(
            "delete_file".to_string(),
        ))
    }

    fn file_length(&self, name: &str) -> Result<usize> {
        self.sub_compound_dir.file_length(name)
    }

    fn create_output(&self, _name: &str, _context: &IOContext) -> Result<Self::IndexOutput> {
        Err(LuceneError::unsupported_operation(
            "create_output".to_string(),
        ))
    }

    type IndexOutput = D::IndexOutput;
    fn create_temp_output(
        &self,
        _prefix: &str,
        _suffix: &str,
        _context: &IOContext,
    ) -> Result<Self::IndexOutput> {
        Err(LuceneError::unsupported_operation(
            "create_temp_output".to_string(),
        ))
    }

    fn sync(&self, _names: &[String]) -> Result<()> {
        Err(LuceneError::unsupported_operation("sync".to_string()))
    }

    fn sync_metadata(&self) -> Result<()> {
        Ok(())
    }

    fn rename(&self, _source: &str, _dest: &str) -> Result<()> {
        Err(LuceneError::unsupported_operation("rename".to_string()))
    }

    type IndexInput = D::IndexInput;

    fn open_input(&self, name: &str, context: &IOContext) -> Result<Self::IndexInput> {
        self.sub_compound_dir.open_input(name, context)
    }

    type Lock = D::Lock;

    fn obtain_lock(&self, _name: &str) -> Result<Self::Lock> {
        Err(LuceneError::unsupported_operation(
            "obtain_lock".to_string(),
        ))
    }

    fn get_pending_deletions(&self) -> Result<HashSet<String>> {
        self.sub_compound_dir.get_pending_deletions()
    }
}

pub trait CompoundDirectoryBase {
    /// Checks the consistency of this directory.
    ///
    /// # Note
    /// This operation may be costly in terms of I/O. For example, it might
    /// compute checksum values against large data files.
    fn check_integrity(&self) -> Result<()>;
}

pub enum CompoundDirectoryEnum<'a, A, B>
where
    A: Directory,
    B: Directory<IndexInput = A::IndexInput, IndexOutput = A::IndexOutput, Lock = A::Lock>,
{
    A(&'a A),
    B(&'a B),
}

impl<A, B> Display for CompoundDirectoryEnum<'_, A, B>
where
    A: Directory,
    B: Directory<IndexInput = A::IndexInput, IndexOutput = A::IndexOutput, Lock = A::Lock>,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            CompoundDirectoryEnum::A(dir) => dir.fmt(f),
            CompoundDirectoryEnum::B(dir) => dir.fmt(f),
        }
    }
}

impl<A, B> Closeable for CompoundDirectoryEnum<'_, A, B>
where
    A: Directory,
    B: Directory<IndexInput = A::IndexInput, IndexOutput = A::IndexOutput, Lock = A::Lock>,
{
    fn close(&mut self) -> Result<()> {
        match self {
            CompoundDirectoryEnum::A(dir) => dir.close(),
            CompoundDirectoryEnum::B(dir) => dir.close(),
        }
    }
}

impl<A, B> HasIdentity for CompoundDirectoryEnum<'_, A, B>
where
    A: Directory,
    B: Directory<IndexInput = A::IndexInput, IndexOutput = A::IndexOutput, Lock = A::Lock>,
{
    fn identity(&self) -> &Identity {
        match self {
            CompoundDirectoryEnum::A(dir) => dir.identity(),
            CompoundDirectoryEnum::B(dir) => dir.identity(),
        }
    }
}

impl<A, B> Directory for CompoundDirectoryEnum<'_, A, B>
where
    A: Directory,
    B: Directory<IndexInput = A::IndexInput, IndexOutput = A::IndexOutput, Lock = A::Lock>,
{
    fn list_all(&self) -> Result<Vec<String>> {
        match self {
            CompoundDirectoryEnum::A(dir) => dir.list_all(),
            CompoundDirectoryEnum::B(dir) => dir.list_all(),
        }
    }

    fn delete_file(&self, name: &str) -> Result<()> {
        match self {
            CompoundDirectoryEnum::A(dir) => dir.delete_file(name),
            CompoundDirectoryEnum::B(dir) => dir.delete_file(name),
        }
    }

    fn file_length(&self, name: &str) -> Result<usize> {
        match self {
            CompoundDirectoryEnum::A(dir) => dir.file_length(name),
            CompoundDirectoryEnum::B(dir) => dir.file_length(name),
        }
    }

    fn create_output(&self, name: &str, context: &IOContext) -> Result<Self::IndexOutput> {
        match self {
            CompoundDirectoryEnum::A(dir) => dir.create_output(name, context),
            CompoundDirectoryEnum::B(dir) => dir.create_output(name, context),
        }
    }

    type IndexOutput = A::IndexOutput;

    fn create_temp_output(
        &self,
        prefix: &str,
        suffix: &str,
        context: &IOContext,
    ) -> Result<Self::IndexOutput> {
        match self {
            CompoundDirectoryEnum::A(dir) => dir.create_temp_output(prefix, suffix, context),
            CompoundDirectoryEnum::B(dir) => dir.create_temp_output(prefix, suffix, context),
        }
    }

    fn sync(&self, names: &[String]) -> Result<()> {
        match self {
            CompoundDirectoryEnum::A(dir) => dir.sync(names),
            CompoundDirectoryEnum::B(dir) => dir.sync(names),
        }
    }

    fn sync_metadata(&self) -> Result<()> {
        match self {
            CompoundDirectoryEnum::A(dir) => dir.sync_metadata(),
            CompoundDirectoryEnum::B(dir) => dir.sync_metadata(),
        }
    }

    fn rename(&self, source: &str, dest: &str) -> Result<()> {
        match self {
            CompoundDirectoryEnum::A(dir) => dir.rename(source, dest),
            CompoundDirectoryEnum::B(dir) => dir.rename(source, dest),
        }
    }

    type IndexInput = A::IndexInput;

    fn open_input(&self, name: &str, context: &IOContext) -> Result<Self::IndexInput> {
        match self {
            CompoundDirectoryEnum::A(dir) => dir.open_input(name, context),
            CompoundDirectoryEnum::B(dir) => dir.open_input(name, context),
        }
    }

    fn open_checksum_input(
        &self,
        name: &str,
    ) -> Result<BufferedChecksumIndexInput<Self::IndexInput>> {
        match self {
            CompoundDirectoryEnum::A(dir) => dir.open_checksum_input(name),
            CompoundDirectoryEnum::B(dir) => dir.open_checksum_input(name),
        }
    }

    type Lock = A::Lock;

    fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
        match self {
            CompoundDirectoryEnum::A(dir) => dir.obtain_lock(name),
            CompoundDirectoryEnum::B(dir) => dir.obtain_lock(name),
        }
    }

    fn copy_from(
        &self,
        from: &impl Directory,
        src: &str,
        dest: &str,
        context: &IOContext,
    ) -> Result<()> {
        match self {
            CompoundDirectoryEnum::A(dir) => dir.copy_from(from, src, dest, context),
            CompoundDirectoryEnum::B(dir) => dir.copy_from(from, src, dest, context),
        }
    }

    fn delete_files_ignoring_exceptions(&self, files: &[String]) {
        match self {
            CompoundDirectoryEnum::A(dir) => dir.delete_files_ignoring_exceptions(files),
            CompoundDirectoryEnum::B(dir) => dir.delete_files_ignoring_exceptions(files),
        }
    }

    fn get_pending_deletions(&self) -> Result<HashSet<String>> {
        match self {
            CompoundDirectoryEnum::A(dir) => dir.get_pending_deletions(),
            CompoundDirectoryEnum::B(dir) => dir.get_pending_deletions(),
        }
    }

    fn is_fs_directory(&self) -> bool {
        match self {
            CompoundDirectoryEnum::A(dir) => dir.is_fs_directory(),
            CompoundDirectoryEnum::B(dir) => dir.is_fs_directory(),
        }
    }

    fn ensure_open(&self) -> Result<()> {
        match self {
            CompoundDirectoryEnum::A(dir) => dir.ensure_open(),
            CompoundDirectoryEnum::B(dir) => dir.ensure_open(),
        }
    }
}
