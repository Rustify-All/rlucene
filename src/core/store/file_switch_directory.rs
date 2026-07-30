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
use crate::core::store::directory::{Directory, DirectoryEnum2, get_temp_file_name};
use crate::core::store::lock::LockEnum2;
use crate::core::store::{IOContext, IndexInputEnum2, IndexOutputEnum2};
use crate::core::util::HasIdentity;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::io_utils::IOUtils;
use parking_lot::Mutex;
use regex::Regex;
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::io::{Error, ErrorKind};
use std::sync::LazyLock;

static EXT_PATTERN: LazyLock<Regex> =
  LazyLock::new(|| Regex::new(r"\.([a-zA-Z]+)").expect("valid file extension pattern"));

/// Utility method to return a file's extension.
pub fn get_extension(name: &str) -> &str {
  let Some(index) = name.rfind('.') else {
    return "";
  };

  let ext = &name[index + 1..];
  if ext == "tmp"
    && let Some(captures) = EXT_PATTERN.captures(&name[..index + 1])
    && let Some(ext) = captures.get(1)
  {
    return ext.as_str();
  }
  ext
}

/// Expert: A `Directory` instance that switches files between two other
/// `Directory` instances.
///
/// Files with the specified extensions are placed in the primary directory;
/// others are placed in the secondary directory. The provided set must not
/// change once passed to this struct, and must allow multiple threads to call
/// `contains` at once.
///
/// Locks with a name having the specified extensions are delegated to the
/// primary directory; others are delegated to the secondary directory. Ideally,
/// both `Directory` instances should use the same lock factory.
///
/// @lucene.experimental
pub struct FileSwitchDirectory<P, S>
where
  P: Directory,
  S: Directory,
{
  secondary_dir: S,
  primary_dir: P,
  primary_extensions: HashSet<String>,
  do_close: Mutex<bool>,
  id: Identity,
}

impl<P, S> FileSwitchDirectory<P, S>
where
  P: Directory,
  S: Directory,
{
  pub fn new(
    primary_extensions: HashSet<String>,
    primary_dir: P,
    secondary_dir: S,
    do_close: bool,
  ) -> Result<Self> {
    if primary_extensions.contains("tmp") {
      return Err(LuceneError::illegal_argument("tmp is a reserved extension"));
    }
    Ok(Self {
      secondary_dir,
      primary_dir,
      primary_extensions,
      do_close: Mutex::new(do_close),
      id: Identity::new(),
    })
  }

  /// Return the primary directory.
  pub fn get_primary_dir(&self) -> &P {
    &self.primary_dir
  }

  /// Return the secondary directory.
  pub fn get_secondary_dir(&self) -> &S {
    &self.secondary_dir
  }

  #[cfg(test)]
  pub(crate) fn get_secondary_dir_mut(&mut self) -> &mut S {
    &mut self.secondary_dir
  }

  fn get_directory(&self, name: &str) -> DirectoryEnum2<&P, &S> {
    let ext = get_extension(name);
    if self.primary_extensions.contains(ext) {
      DirectoryEnum2::A(&self.primary_dir)
    } else {
      DirectoryEnum2::B(&self.secondary_dir)
    }
  }

  fn is_no_such_file(error: &LuceneError) -> bool {
    match error {
      LuceneError::NoSuchFile(_) => true,
      LuceneError::Io { source, .. } | LuceneError::IoWithPath { source, .. } => {
        source.kind() == ErrorKind::NotFound
      },
      _ => false,
    }
  }
}

impl<P, S> Display for FileSwitchDirectory<P, S>
where
  P: Directory,
  S: Directory,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "{}(primary={}, secondary={})",
      std::any::type_name::<Self>(),
      self.primary_dir,
      self.secondary_dir
    )
  }
}

impl<P, S> CloseableRef for FileSwitchDirectory<P, S>
where
  P: Directory,
  S: Directory,
{
  fn close(&self) -> Result<()> {
    let mut do_close = self.do_close.lock();
    if *do_close {
      IOUtils::close_refs_tuple((Some(&self.primary_dir), Some(&self.secondary_dir)))?;
      *do_close = false;
    }
    Ok(())
  }
}
impl<P, S> Drop for FileSwitchDirectory<P, S>
where
  P: Directory,
  S: Directory,
{
  fn drop(&mut self) {
    let _ = self.close();
  }
}

impl<P, S> HasIdentity for FileSwitchDirectory<P, S>
where
  P: Directory,
  S: Directory,
{
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl<P, S> Directory for FileSwitchDirectory<P, S>
where
  P: Directory,
  S: Directory,
{
  fn list_all(&self) -> Result<Vec<String>> {
    let mut files = Vec::new();
    // LUCENE-3380: either or both of our dirs could be FSDirs,
    // but if one underlying delegate is an FSDir and mkdirs() has not
    // yet been called, because so far everything is written to the other,
    // in this case, we don't want to throw a NoSuchFileException
    let mut exc = None;
    match self.primary_dir.list_all() {
      Ok(primary_files) => {
        for file in primary_files {
          let ext = get_extension(&file);
          // we should respect the extension here as well to ensure that we
          // don't list a file that is already deleted or rather in the one of
          // the directories pending deletions if both directories point to the
          // same filesystem path. This is quite common for instance to use
          // NIOFS as a primary and MMap as a secondary to only mmap files like
          // docvalues or term dictionaries.
          if self.primary_extensions.contains(ext) {
            files.push(file);
          }
        }
      },
      Err(err) if Self::is_no_such_file(&err) => {
        exc = Some(err);
      },
      Err(err) => return Err(err),
    }
    match self.secondary_dir.list_all() {
      Ok(secondary_files) => {
        for file in secondary_files {
          let ext = get_extension(&file);
          if !self.primary_extensions.contains(ext) {
            files.push(file);
          }
        }
      },
      Err(err) if Self::is_no_such_file(&err) => {
        // we got NoSuchFileException from both dirs
        // rethrow the first.
        if let Some(exc) = exc {
          return Err(exc);
        }
        // we got NoSuchFileException from the secondary,
        // and the primary is empty.
        if files.is_empty() {
          return Err(err);
        }
      },
      Err(err) => return Err(err),
    }
    // we got NoSuchFileException from the primary,
    // and the secondary is empty.
    if let Some(exc) = exc
      && files.is_empty()
    {
      return Err(exc);
    }
    files.sort();
    Ok(files)
  }

  fn delete_file(&self, name: &str) -> Result<()> {
    match self.get_directory(name) {
      DirectoryEnum2::A(primary_dir) => primary_dir.delete_file(name),
      DirectoryEnum2::B(secondary_dir) => secondary_dir.delete_file(name),
    }
  }

  fn file_length(&self, name: &str) -> Result<usize> {
    self.get_directory(name).file_length(name)
  }

  fn create_output(&self, name: &str, context: &IOContext) -> Result<Self::IndexOutput> {
    self.get_directory(name).create_output(name, context)
  }

  type IndexOutput = IndexOutputEnum2<P::IndexOutput, S::IndexOutput>;

  fn create_temp_output(
    &self,
    prefix: &str,
    suffix: &str,
    context: &IOContext,
  ) -> Result<Self::IndexOutput> {
    // this is best effort - it's ok to create a tmp file with any prefix and
    // suffix. Yet if this file is then in-turn used to rename they must match
    // to the same directory hence we use the full file-name to find the right
    // directory. Here we can't make a decision but we need to ensure that all
    // other operations map to the right directory.
    let tmp_file_name = get_temp_file_name(prefix, suffix, 0);
    self
      .get_directory(&tmp_file_name)
      .create_temp_output(prefix, suffix, context)
  }

  fn sync(&self, names: &[String]) -> Result<()> {
    let mut primary_names = Vec::new();
    let mut secondary_names = Vec::new();

    for name in names {
      if self.primary_extensions.contains(get_extension(name)) {
        primary_names.push(name.clone());
      } else {
        secondary_names.push(name.clone());
      }
    }

    self.primary_dir.sync(&primary_names)?;
    self.secondary_dir.sync(&secondary_names)
  }

  fn sync_metadata(&self) -> Result<()> {
    self.primary_dir.sync_metadata()?;
    self.secondary_dir.sync_metadata()
  }

  fn rename(&self, source: &str, dest: &str) -> Result<()> {
    let source_dir = self.get_directory(source);
    // won't happen with standard lucene index files since pending and commit
    // will always have the same extension ("")
    match (&source_dir, self.get_directory(dest)) {
      (DirectoryEnum2::A(_), DirectoryEnum2::A(_))
      | (DirectoryEnum2::B(_), DirectoryEnum2::B(_)) => source_dir.rename(source, dest),
      _ => Err(LuceneError::io(Error::other(format!(
        "{source} -> {dest}: source and dest are in different directories"
      )))),
    }
  }

  type IndexInput = IndexInputEnum2<P::IndexInput, S::IndexInput>;

  fn open_input(&self, name: &str, context: &IOContext) -> Result<Self::IndexInput> {
    self.get_directory(name).open_input(name, context)
  }

  type Lock = LockEnum2<P::Lock, S::Lock>;

  fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
    self.get_directory(name).obtain_lock(name)
  }

  fn get_pending_deletions(&self) -> Result<HashSet<String>> {
    let primary_deletions = self.primary_dir.get_pending_deletions()?;
    let secondary_deletions = self.secondary_dir.get_pending_deletions()?;
    if primary_deletions.is_empty() && secondary_deletions.is_empty() {
      Ok(HashSet::new())
    } else {
      let mut combined = HashSet::new();
      combined.extend(primary_deletions);
      combined.extend(secondary_deletions);
      Ok(combined)
    }
  }
}
