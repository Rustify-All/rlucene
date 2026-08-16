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
use crate::core::util::close::CloseableRef;
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
pub trait CompoundDirectory: Directory {
  /// Checks the consistency of this directory.
  ///
  /// # Note
  /// This operation may be costly in terms of I/O. For example, it might
  /// compute checksum values against large data files.
  fn check_integrity(&self) -> Result<()>;
}

pub enum CompoundDirectoryEnum<'a, A, B> {
  A(&'a A),
  B(&'a B),
}

impl<A, B> Display for CompoundDirectoryEnum<'_, A, B>
where
  A: Display,
  B: Display,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self {
      CompoundDirectoryEnum::A(dir) => dir.fmt(f),
      CompoundDirectoryEnum::B(dir) => dir.fmt(f),
    }
  }
}

impl<A, B> CloseableRef for CompoundDirectoryEnum<'_, A, B>
where
  A: CloseableRef,
  B: CloseableRef,
{
  fn close(&self) -> Result<()> {
    match self {
      CompoundDirectoryEnum::A(dir) => dir.close(),
      CompoundDirectoryEnum::B(dir) => dir.close(),
    }
  }
}

impl<A, B> HasIdentity for CompoundDirectoryEnum<'_, A, B>
where
  A: HasIdentity,
  B: HasIdentity,
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
  A: Directory<IndexInput = B::IndexInput>,
  B: Directory,
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
      CompoundDirectoryEnum::A(_) => Err(LuceneError::unsupported_operation("create_output")),
      CompoundDirectoryEnum::B(dir) => dir.create_output(name, context),
    }
  }

  type IndexOutput = B::IndexOutput;

  fn create_temp_output(
    &self,
    prefix: &str,
    suffix: &str,
    context: &IOContext,
  ) -> Result<Self::IndexOutput> {
    match self {
      CompoundDirectoryEnum::A(_) => {
        Err(LuceneError::unsupported_operation("create_temp_output"))
      },
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

  type IndexInput = B::IndexInput;

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

  type Lock = B::Lock;

  fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
    match self {
      CompoundDirectoryEnum::A(_) => Err(LuceneError::unsupported_operation("obtain_lock")),
      CompoundDirectoryEnum::B(dir) => dir.obtain_lock(name),
    }
  }

  fn copy_from<T>(&self, from: &T, src: &str, dest: &str, context: &IOContext) -> Result<()>
  where
    T: Directory + ?Sized,
  {
    match self {
      CompoundDirectoryEnum::A(dir) => dir.copy_from(from, src, dest, context),
      CompoundDirectoryEnum::B(dir) => dir.copy_from(from, src, dest, context),
    }
  }

  fn get_pending_deletions(&self) -> Result<HashSet<String>> {
    match self {
      CompoundDirectoryEnum::A(dir) => dir.get_pending_deletions(),
      CompoundDirectoryEnum::B(dir) => dir.get_pending_deletions(),
    }
  }

  #[cfg(debug_assertions)]
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
