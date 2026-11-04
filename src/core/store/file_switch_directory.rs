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
use crate::core::store::directory::{Directory, get_temp_file_name};
use crate::core::util::HasIdentity;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind, Result};
use crate::core::util::io_utils::IOUtils;
use parking_lot::Mutex;
use regex::Regex;
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::io::{Error, ErrorKind};
use std::sync::LazyLock;

static EXT_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
  expect_invariant!(
    Regex::new(r"\.([a-zA-Z]+)"),
    "the built-in file extension pattern is valid"
  )
});

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

/// Expert: A [`Directory`] instance that switches files between two other
/// [`Directory`] instances.
///
/// Files with the specified extensions are placed in the primary directory;
/// others are placed in the secondary directory. The provided set must not
/// change once passed to this struct, and must allow multiple threads to call
/// `contains` at once.
///
/// Locks with a name having the specified extensions are delegated to the
/// primary directory; others are delegated to the secondary directory. Ideally,
/// both [`Directory`] instances should use the same lock factory.
///
/// @lucene.experimental
pub struct FileSwitchDirectory<D>
where
  D: Directory,
{
  secondary_dir: D,
  primary_dir: D,
  primary_extensions: HashSet<String>,
  do_close: Mutex<bool>,
  id: Identity,
}

impl<D> FileSwitchDirectory<D>
where
  D: Directory,
{
  pub fn new<I, S>(
    primary_extensions: I,
    primary_dir: D,
    secondary_dir: D,
    do_close: bool,
  ) -> Result<Self>
  where
    I: IntoIterator<Item = S>,
    S: Into<String>,
  {
    let primary_extensions = primary_extensions
      .into_iter()
      .map(Into::into)
      .collect::<HashSet<String>>();
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
  pub fn get_primary_dir(&self) -> &D {
    &self.primary_dir
  }

  /// Return the secondary directory.
  pub fn get_secondary_dir(&self) -> &D {
    &self.secondary_dir
  }

  #[cfg(test)]
  pub(crate) fn get_secondary_dir_mut(&mut self) -> &mut D {
    &mut self.secondary_dir
  }

  fn get_directory(&self, name: &str) -> &D {
    if self.is_primary(name) {
      &self.primary_dir
    } else {
      &self.secondary_dir
    }
  }

  fn is_primary(&self, name: &str) -> bool {
    self.primary_extensions.contains(get_extension(name))
  }

  fn is_no_such_file(error: &LuceneError) -> bool {
    match error.kind() {
      LuceneErrorKind::NoSuchFile(_) => true,
      LuceneErrorKind::Io { source, .. } | LuceneErrorKind::IoWithPath { source, .. } => {
        source.kind() == ErrorKind::NotFound
      },
      _ => false,
    }
  }
}

impl<D> Display for FileSwitchDirectory<D>
where
  D: Directory,
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

impl<D> CloseableRef for FileSwitchDirectory<D>
where
  D: Directory,
{
  fn close(&self) -> Result<()> {
    let mut do_close = self.do_close.lock();
    if *do_close {
      IOUtils::close((Some(&self.primary_dir), Some(&self.secondary_dir)))?;
      *do_close = false;
    }
    Ok(())
  }
}
impl<D> Drop for FileSwitchDirectory<D>
where
  D: Directory,
{
  fn drop(&mut self) {
    let _ = self.close();
  }
}

impl<D> HasIdentity for FileSwitchDirectory<D>
where
  D: Directory,
{
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl<D> Directory for FileSwitchDirectory<D>
where
  D: Directory,
{
  fn list_all(&self) -> Result<Vec<String>> {
    let mut files = Vec::new();
    // LUCENE-3380: either or both of our dirs could be FSDirs,
    // but if one underlying delegate is an FSDir and mkdirs() has not
    // yet been called, because so far everything is written to the other,
    // In this case, we do not want to return a not-found I/O error.
    let mut exc = None;
    match self.primary_dir.list_all() {
      Ok(mut primary_files) => {
        // we should respect the extension here as well to ensure that we
        // don't list a file that is already deleted or rather in the one of
        // the directories pending deletions if both directories point to the
        // same filesystem path. This is quite common for instance to use
        // NIOFS as a primary and MMap as a secondary to only mmap files like
        // docvalues or term dictionaries.
        // Retain the primary result's allocation for the combined listing.
        primary_files.retain(|file| self.primary_extensions.contains(get_extension(file)));
        files = primary_files;
      },
      Err(err) if Self::is_no_such_file(&err) => {
        exc = Some(err);
      },
      Err(err) => return Err(err),
    }
    match self.secondary_dir.list_all() {
      Ok(mut secondary_files) => {
        secondary_files.retain(|file| !self.primary_extensions.contains(get_extension(file)));
        files.extend(secondary_files);
      },
      Err(err) if Self::is_no_such_file(&err) => {
        // Both directories returned a not-found I/O error.
        // Return the first error.
        if let Some(exc) = exc {
          return Err(exc);
        }
        // The secondary directory returned a not-found I/O error,
        // and the primary is empty.
        if files.is_empty() {
          return Err(err);
        }
      },
      Err(err) => return Err(err),
    }
    // The primary directory returned a not-found I/O error,
    // and the secondary is empty.
    if let Some(exc) = exc
      && files.is_empty()
    {
      return Err(exc);
    }
    files.sort_unstable();
    Ok(files)
  }

  fn delete_file(&self, name: &str) -> Result<()> {
    self.get_directory(name).delete_file(name)
  }

  fn file_length(&self, name: &str) -> Result<usize> {
    self.get_directory(name).file_length(name)
  }

  fn create_output(&self, name: &str, context: &IOContext) -> Result<Self::IndexOutput> {
    self.get_directory(name).create_output(name, context)
  }

  type IndexOutput = D::IndexOutput;

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
    let mut primary_names = smallvec::SmallVec::<[String; 2]>::new();
    let mut secondary_names = smallvec::SmallVec::<[String; 2]>::new();

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
    // won't happen with standard lucene index files since pending and commit
    // will always have the same extension ("")
    if self.is_primary(source) == self.is_primary(dest) {
      self.get_directory(source).rename(source, dest)
    } else {
      Err(LuceneError::io(Error::other(format!(
        "{source} -> {dest}: source and dest are in different directories"
      ))))
    }
  }

  type IndexInput = D::IndexInput;

  fn open_input(&self, name: &str, context: &IOContext) -> Result<Self::IndexInput> {
    self.get_directory(name).open_input(name, context)
  }

  type Lock = D::Lock;

  fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
    self.get_directory(name).obtain_lock(name)
  }

  fn get_pending_deletions(&self) -> Result<HashSet<String>> {
    let mut primary_deletions = self.primary_dir.get_pending_deletions()?;
    let secondary_deletions = self.secondary_dir.get_pending_deletions()?;
    if primary_deletions.is_empty() && secondary_deletions.is_empty() {
      Ok(HashSet::new())
    } else {
      primary_deletions.extend(secondary_deletions);
      Ok(primary_deletions)
    }
  }
}
