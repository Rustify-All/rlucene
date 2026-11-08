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
use crate::core::store::base_directory::{BaseDirectory, BaseDirectoryBase};
use crate::core::store::directory::{Directory, get_temp_file_name};
use crate::core::store::fs_directory_base::{FSDirectoryBase, FSDirectoryBaseEnum};
use crate::core::store::lock_factory::{LockFactory, LockFactoryEnum};
#[cfg(target_pointer_width = "64")]
use crate::core::store::mmap_directory::MMapDirectory;
#[cfg(not(target_pointer_width = "64"))]
use crate::core::store::nio_fs_directory::NIOFSDirectory;
use crate::core::store::{
  IOContext, NativeFSLockFactory, OutputStreamIndexOutput, fs_lock_factory,
};
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::{HasIdentity, IOUtils};
use parking_lot::Mutex;
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::SeqCst;
use std::sync::atomic::{AtomicU32, AtomicU64};
use std::{fs, io};

/// Base trait for [`Directory`] implementations that store index files in the
/// file system. There are currently two core implementations:
///
/// - [`MMapDirectory`]: Reserved for a
///   memory-mapped directory implementation.
/// - [`NIOFSDirectory`](crate::core::store::nio_fs_directory::NIOFSDirectory): Uses
///   `std::fs::File` positional reads so multiple threads can read from the same file without a
///   shared seek position.
///
/// The default locking implementation is [`NativeFSLockFactory`],
/// but it can be replaced with a custom [`LockFactory`].
///
/// # See Also
/// [`Directory`]
pub struct FSDirectory<D, T>
where
  D: LockFactory,
  T: FSDirectoryBase,
{
  pub(crate) directory: PathBuf,
  /// Maps files that we are trying to delete (or we tried already but
  /// failed) before attempting to delete that key.
  pending_deletes: Mutex<HashSet<String>>,
  ops_since_last_delete: AtomicU32,
  /// Used to generate temp file names in
  /// [`Directory::create_temp_output`].
  next_temp_file_counter: AtomicU64,
  pub(crate) sub_fs_directory: T,
  base: BaseDirectoryBase<D>,
  id: Identity,
}

pub type FSDirectories = FSDirectory<LockFactoryEnum, FSDirectoryBaseEnum>;

impl<D, T> FSDirectory<D, T>
where
  D: LockFactory,
  T: FSDirectoryBase,
{
  pub fn with_lock_factory<P>(
    directory: P,
    lock_factory: D,
    sub_fs_directory: T,
  ) -> Result<FSDirectory<D, T>>
  where
    P: Into<PathBuf>,
  {
    let directory = directory.into();
    if !directory.is_dir() {
      fs::create_dir(&directory)?;
    }
    let base = BaseDirectoryBase::new(lock_factory);
    Ok(FSDirectory {
      directory,
      pending_deletes: Mutex::new(HashSet::new()),
      ops_since_last_delete: AtomicU32::new(0),
      next_temp_file_counter: AtomicU64::new(0),
      sub_fs_directory,
      base,
      id: Identity::new(),
    })
  }

  fn list_all(dir: &Path, skip_names: Option<&HashSet<String>>) -> Result<Vec<String>> {
    let mut entries = Vec::new();

    for entry in dir.read_dir()? {
      let entry = entry?;
      let name = entry
        .file_name()
        .into_string()
        .unwrap_or_else(|name| name.to_string_lossy().into_owned());

      if let Some(skip) = &skip_names
        && skip.contains(&name)
      {
        continue;
      }

      entries.push(name);
    }

    entries.sort_unstable();
    Ok(entries)
  }
  pub fn maybe_delete_pending_files(
    directory: &Path,
    pending_deletes: &mut HashSet<String>,
    ops_since_last_delete: &AtomicU32,
  ) -> Result<()> {
    if !pending_deletes.is_empty() {
      let count = ops_since_last_delete.fetch_add(1, SeqCst) + 1;

      if count as usize >= pending_deletes.len() {
        ops_since_last_delete.fetch_sub(count, SeqCst);
        Self::delete_pending_files(directory, pending_deletes)?;
      }
    }
    Ok(())
  }

  /// Ensure that the given file is synchronized to the storage device.
  ///
  /// # Arguments
  ///
  /// * `name` - The name of the file to sync.
  ///
  /// # Errors
  ///
  /// Returns a [`LuceneError`] if the file cannot be found or synchronized.
  pub fn fsync(&self, name: &str) -> Result<()> {
    IOUtils::fsync(&self.directory.join(name), false)
  }

  /// Try to delete any pending files that we had previously tried to delete
  /// but failed because we are on Windows and the files were still held
  /// open.
  pub fn delete_pending_files(
    directory: &Path,
    pending_deletes: &mut HashSet<String>,
  ) -> Result<()> {
    if !pending_deletes.is_empty() {
      // Snapshot the names since private_delete_file mutates the set.
      let files_to_delete: Vec<String> = pending_deletes.iter().cloned().collect();

      for name in files_to_delete {
        Self::private_delete_file(directory, &name, true, pending_deletes)?;
      }
    }
    Ok(())
  }

  fn private_delete_file(
    directory: &Path,
    name: &str,
    is_pending_delete: bool,
    pending_deletes: &mut HashSet<String>,
  ) -> Result<()> {
    let file_path = directory.join(name);
    match fs::remove_file(&file_path) {
      Ok(_) => {
        pending_deletes.remove(name);
        Ok(())
      },
      Err(e) if e.kind() == io::ErrorKind::NotFound => {
        pending_deletes.remove(name);

        if is_pending_delete && cfg!(windows) {
          // LUCENE-6684: we suppress this check for Windows, since a
          // file could be in a confusing "pending
          // delete" state, failing the first
          // delete attempt with access denied and then apparently
          // falsely failing here when we try ot
          // delete it again, with NSFE/FNFE
          Ok(())
        } else {
          Err(LuceneError::io_with_path(
            file_path.to_string_lossy().into_owned(),
            e,
          ))
        }
      },
      Err(_e) => {
        // On windows, a file delete can fail because there's still an
        // open file handle against it.  We record this
        // in pendingDeletes and try again later.

        pending_deletes.reserve(1);
        if !pending_deletes.contains(name) {
          pending_deletes.insert(name.to_string());
        }
        Ok(())
      },
    }
  }
  fn ensure_can_read(&self, name: &str) -> Result<()> {
    let pending_deletes = self.pending_deletes.lock();
    if pending_deletes.contains(name) {
      return Err(LuceneError::not_such_file(format!(
        "file \"{name}\" is pending delete and cannot be opened for read"
      )));
    }
    Ok(())
  }
}

impl FSDirectory<LockFactoryEnum, FSDirectoryBaseEnum> {
  /// Selects a directory implementation for internal tools and tests.
  ///
  /// Unlike Java's public factory, this is crate-private because its enum return
  /// type can carry backend dispatch into reads. Public callers should construct
  /// `MMapDirectory` or `NIOFSDirectory` directly to retain the concrete input type.
  pub(crate) fn open<P>(directory: P) -> Result<FSDirectories>
  where
    P: Into<PathBuf>,
  {
    Self::open_with_lock(directory, fs_lock_factory::get_default())
  }

  /// Just like [`FSDirectory::open`], but uses the provided lock factory.
  ///
  /// Unlike Java's public overload, this is crate-private to keep the enum-backed
  /// factory for internal tools and tests. Public callers should use a concrete
  /// backend's `with_lock_factory` constructor to retain its concrete input type.
  pub(crate) fn open_with_lock<L, P>(directory: P, lock_factory: L) -> Result<FSDirectories>
  where
    L: Into<LockFactoryEnum>,
    P: Into<PathBuf>,
  {
    #[cfg(target_pointer_width = "64")]
    let sub_fs_directory = FSDirectoryBaseEnum::MMap(MMapDirectory::default());
    #[cfg(not(target_pointer_width = "64"))]
    let sub_fs_directory = FSDirectoryBaseEnum::NIO(NIOFSDirectory);

    Self::with_lock_factory(directory, lock_factory.into(), sub_fs_directory)
  }
}

impl<T> FSDirectory<NativeFSLockFactory, T>
where
  T: FSDirectoryBase,
{
  pub fn new<P>(directory: P, sub_fs_directory: T) -> Result<FSDirectory<NativeFSLockFactory, T>>
  where
    P: Into<PathBuf>,
  {
    Self::with_lock_factory(directory, NativeFSLockFactory::new(), sub_fs_directory)
  }
}

impl<D, T> HasIdentity for FSDirectory<D, T>
where
  D: LockFactory,
  T: FSDirectoryBase,
{
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl<D, T> Directory for FSDirectory<D, T>
where
  D: LockFactory,
  T: FSDirectoryBase,
{
  fn list_all(&self) -> Result<Vec<String>> {
    let pending_deletes = self.pending_deletes.lock();
    Self::list_all(&self.directory, Some(&pending_deletes))
  }

  fn delete_file(&self, name: &str) -> Result<()> {
    let mut pending_deletes = self.pending_deletes.lock();
    if pending_deletes.contains(name) {
      return Err(LuceneError::not_such_file(format!(
        "file \"{name}\" is already pending delete"
      )));
    }

    Self::private_delete_file(&self.directory, name, false, &mut pending_deletes)?;

    Self::maybe_delete_pending_files(
      &self.directory,
      &mut pending_deletes,
      &self.ops_since_last_delete,
    )?;

    Ok(())
  }

  fn file_length(&self, name: &str) -> Result<usize> {
    if self.pending_deletes.lock().contains(name) {
      return Err(LuceneError::not_such_file(format!(
        "file \"{name}\" is pending delete"
      )));
    }

    let file_path = self.directory.join(name);
    let metadata = fs::metadata(&file_path)
      .map_err(|e| LuceneError::io_with_path(file_path.to_string_lossy().into_owned(), e))?;
    let length = metadata.len();
    Ok(length as usize)
  }
  fn create_output(&self, name: &str, _context: &IOContext) -> Result<Self::IndexOutput> {
    self.ensure_open()?;
    let mut pending_deletes = self.pending_deletes.lock();
    Self::maybe_delete_pending_files(
      &self.directory,
      &mut pending_deletes,
      &self.ops_since_last_delete,
    )?;

    if pending_deletes.remove(name) {
      Self::private_delete_file(&self.directory, name, true, &mut pending_deletes)?;
      pending_deletes.remove(name);
    }

    let file_path = self.directory.join(name);
    let file = File::options()
      .write(true)
      .create_new(true)
      .open(&file_path)
      .map_err(|err| LuceneError::io_with_path(file_path.to_string_lossy().into_owned(), err))?;

    OutputStreamIndexOutput::new(
      format!("FSIndexOutput(path=\"{}\")", file_path.display()),
      name,
      file,
      CHUNK_SIZE,
    )
  }

  type IndexOutput = OutputStreamIndexOutput<File>;
  fn create_temp_output(
    &self,
    prefix: &str,
    suffix: &str,
    _context: &IOContext,
  ) -> Result<Self::IndexOutput> {
    let mut pending_deletes = self.pending_deletes.lock();
    Self::maybe_delete_pending_files(
      &self.directory,
      &mut pending_deletes,
      &self.ops_since_last_delete,
    )?;

    loop {
      let counter = self.next_temp_file_counter.fetch_add(1, SeqCst);
      let name = get_temp_file_name(prefix, suffix, counter);

      if pending_deletes.contains(&name) {
        continue;
      }

      let file_path = self.directory.join(&name);
      match File::options()
        .write(true)
        .create_new(true)
        .open(&file_path)
      {
        Ok(file) => {
          return OutputStreamIndexOutput::new(
            format!("FSIndexOutput(path=\"{}\")", file_path.display()),
            name,
            file,
            CHUNK_SIZE,
          );
        },
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
          continue;
        },
        Err(e) => {
          return Err(LuceneError::io_with_path(
            file_path.to_string_lossy().into_owned(),
            e,
          ));
        },
      }
    }
  }

  fn sync(&self, names: &[String]) -> Result<()> {
    let mut file_path = PathBuf::new();
    for name in names {
      file_path.clear();
      file_path.push(&self.directory);
      file_path.push(name);
      IOUtils::fsync(&file_path, false)?;
    }
    Self::maybe_delete_pending_files(
      &self.directory,
      &mut self.pending_deletes.lock(),
      &self.ops_since_last_delete,
    )?;
    Ok(())
  }

  fn sync_metadata(&self) -> Result<()> {
    IOUtils::fsync(&self.directory, true)?;
    Self::maybe_delete_pending_files(
      &self.directory,
      &mut self.pending_deletes.lock(),
      &self.ops_since_last_delete,
    )?;
    Ok(())
  }

  fn rename(&self, source: &str, dest: &str) -> Result<()> {
    let mut pending_deletes = self.pending_deletes.lock();
    if pending_deletes.contains(source) {
      return Err(LuceneError::not_such_file(format!(
        "File \"{source}\" is pending delete and cannot be moved"
      )));
    }
    Self::maybe_delete_pending_files(
      &self.directory,
      &mut pending_deletes,
      &self.ops_since_last_delete,
    )?;

    if pending_deletes.remove(dest) {
      Self::private_delete_file(&self.directory, dest, true, &mut pending_deletes)?; // try again to delete it - this is the best effort
      pending_deletes.remove(dest); // watch out if the delete fails, it's
      // back in here
    }

    let source_path = self.directory.join(source);
    let dest_path = self.directory.join(dest);

    fs::rename(source_path, dest_path).map_err(LuceneError::io)?;

    Ok(())
  }

  type IndexInput = T::Output;
  fn open_input(&self, name: &str, context: &IOContext) -> Result<Self::IndexInput> {
    self.ensure_open()?;
    self.ensure_can_read(name)?;
    self
      .sub_fs_directory
      .open_input(name, context, &self.directory)
  }

  type Lock = D::Lock;

  fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
    self.base.obtain_lock(&self.directory, name)
  }

  fn get_pending_deletions(&self) -> Result<HashSet<String>> {
    let mut pending_deletes = self.pending_deletes.lock();
    Self::delete_pending_files(&self.directory, &mut pending_deletes)?;
    if pending_deletes.is_empty() {
      Ok(HashSet::new())
    } else {
      Ok(pending_deletes.clone())
    }
  }

  #[cfg(test)]
  fn is_fs_directory(&self) -> bool {
    true
  }

  fn ensure_open(&self) -> Result<()> {
    self.base.ensure_open()
  }
}

impl<D, T> CloseableRef for FSDirectory<D, T>
where
  D: LockFactory,
  T: FSDirectoryBase,
{
  fn close(&self) -> Result<()> {
    self.base.close();
    let mut pending_deletes = self.pending_deletes.lock();
    Self::delete_pending_files(&self.directory, &mut pending_deletes)?;
    Ok(())
  }
}
impl<D, T> Drop for FSDirectory<D, T>
where
  D: LockFactory,
  T: FSDirectoryBase,
{
  fn drop(&mut self) {
    let _ = self.close();
  }
}

impl<D, T> Display for FSDirectory<D, T>
where
  D: LockFactory,
  T: FSDirectoryBase,
{
  fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
    write!(
      f,
      "{}@{} lockFactory={}",
      self.sub_fs_directory,
      self.directory.display(),
      self.base.lock_factory,
    )
  }
}

impl<D, T> BaseDirectory for FSDirectory<D, T>
where
  D: LockFactory,
  T: FSDirectoryBase,
{
  type LockFactory = D;

  fn get_lock_factory(&self) -> &BaseDirectoryBase<Self::LockFactory> {
    &self.base
  }
}

/// File output write-buffer capacity in bytes.
const CHUNK_SIZE: usize = 8192;
