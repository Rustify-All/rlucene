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
use crate::core::store::byte_buffers_directory::{BBOutputToInput, BYTE_BUFFERS_DATA_OUTPUT};
use crate::core::store::directory::{Directory, DirectoryEnum2};
use crate::core::store::single_instance_lock_factory::SingleInstanceLockFactory;
use crate::core::store::{
  ByteBuffersDirectory, IOContext, IndexInputEnum2, IndexOutput, IndexOutputEnum2,
};
use crate::core::util::accountable::Accountable;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::io_utils::IOUtils;
use crate::core::util::{HasIdentity, TryIntoInt};
#[cfg(test)]
use crate::test_framework::core::store::test_nrt_caching_directory::AssertCacheWriteNRTCachingDirectory;
use parking_lot::Mutex;
use std::collections::{BTreeSet, HashSet};
use std::fmt::{Display, Formatter};
use std::io::ErrorKind;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

type CacheDirectory = ByteBuffersDirectory<SingleInstanceLockFactory>;

// TODO
//   - let implementation dictate policy...?
//   - rename to MergeCachingDir? NRTCachingDir

/// Wraps a RAM-resident directory around any provided delegate directory, to
/// be used during NRT search.
///
/// This struct is likely only useful in a near-real-time context, where the
/// indexing rate is lowish but the reopen rate is highish, resulting in many
/// tiny files being written. This directory keeps such segments (as well as
/// the segments produced by merging them, as long as they are small enough)
/// in RAM.
///
/// This is safe to use: when an application calls `IndexWriter::commit`, all
/// cached files will be flushed from the cache and synced.
///
/// # Example
///
/// ```ignore
/// let fs_dir = NIOFSDirectory::new("/path/to/index")?;
/// let cached_fs_dir = Arc::new(NRTCachingDirectory::new(fs_dir, 5.0, 60.0));
/// let config = IndexWriterConfig::new(analyzer)?;
/// let writer = IndexWriter::new(cached_fs_dir, config)?;
/// ```
///
/// This caches all newly flushed segments and all merges whose expected
/// segment size is at most `max_merge_size_mb`, unless the net cached bytes
/// exceeds `max_cached_mb`. At that point, writes are not cached until the net
/// cached bytes falls below the limit.
///
/// @lucene.experimental
pub struct NRTCachingDirectory<D>
where
  D: Directory,
{
  closed: AtomicBool,

  /// Current total size of files in the cache is maintained separately for
  /// faster access.
  cache_size: Arc<AtomicI64>,

  /// RAM-resident directory that updates `cache_size` when files are
  /// successfully closed.
  cache_directory: CacheDirectory,

  max_merge_size_bytes: i64,
  max_cached_bytes: i64,

  delegate: D,
  lock: Mutex<()>,
  hook: NRTCachingDirectoryHook,
  id: Identity,
}

impl<D> NRTCachingDirectory<D>
where
  D: Directory,
{
  /// We will cache a newly created output if 1) it is a flush or a merge and
  /// the estimated size of the merged segment is at most `max_merge_size_mb`,
  /// and 2) the total cached bytes is at most `max_cached_mb`.
  pub fn new(delegate: D, max_merge_size_mb: f64, max_cached_mb: f64) -> Self {
    Self::with_hook(
      delegate,
      max_merge_size_mb,
      max_cached_mb,
      NRTCachingDirectoryHook::Default,
    )
  }

  pub(crate) fn get_delegate_mut(&mut self) -> &mut D {
    &mut self.delegate
  }

  pub(crate) fn with_hook(
    delegate: D,
    max_merge_size_mb: f64,
    max_cached_mb: f64,
    hook: NRTCachingDirectoryHook,
  ) -> Self {
    let cache_size = Arc::new(AtomicI64::new(0));
    let cache_directory = ByteBuffersDirectory::with_output_strategy(
      SingleInstanceLockFactory::new(),
      BYTE_BUFFERS_DATA_OUTPUT,
      BBOutputToInput::NRTCachingDirectory(cache_size.clone()),
    );

    Self {
      closed: AtomicBool::new(false),
      cache_size,
      cache_directory,
      max_merge_size_bytes: (max_merge_size_mb * 1024.0 * 1024.0) as i64,
      max_cached_bytes: (max_cached_mb * 1024.0 * 1024.0) as i64,
      delegate,
      lock: Mutex::new(()),
      hook,
      id: Identity::new(),
    }
  }

  pub fn list_cached_files(&self) -> Result<Vec<String>> {
    self.cache_directory.list_all()
  }

  /// An implementation hook can customize this logic. Returns `true` if this
  /// file should be written to the RAM-based cache first.
  pub(crate) fn do_cache_write(&self, name: &str, context: &IOContext) -> bool {
    self.hook.do_cache_write(self, name, context)
  }

  /// Returns true if the file exists (can be opened), false if it cannot be
  /// opened, and returns an error if there is an unexpected failure.
  pub(crate) fn slow_file_exists<T>(directory: &T, file_name: &str) -> Result<bool>
  where
    T: Directory + ?Sized,
  {
    match directory.file_length(file_name) {
      Ok(_) => Ok(true),
      Err(LuceneError::NoSuchFile(_)) => Ok(false),
      Err(LuceneError::Io { source, .. }) | Err(LuceneError::IoWithPath { source, .. })
        if source.kind() == ErrorKind::NotFound =>
      {
        Ok(false)
      },
      Err(error) => Err(error),
    }
  }

  fn is_cached_file(&self, file_name: &str) -> Result<bool> {
    self.cache_directory.file_exists(file_name)
  }

  fn un_cache(&self, file_name: &str) -> Result<()> {
    // Must synchronize here because other methods use an
    // if (cache.fileNameExists(name)) { ... } else { ... } sequence.
    let _guard = self.lock.lock();
    if !self.cache_directory.file_exists(file_name)? {
      // Another thread beat us.
      return Ok(());
    }
    debug_assert!(
      !Self::slow_file_exists(&self.delegate, file_name)?,
      "fileName={file_name} exists both in cache and in delegate"
    );

    self.delegate.copy_from(
      &self.cache_directory,
      file_name,
      file_name,
      &IOContext::default_io_context()?,
    )?;
    let length: i64 = self.cache_directory.file_length(file_name)?.try_convert()?;
    self.cache_size.fetch_sub(length, Ordering::SeqCst);
    self.cache_directory.delete_file(file_name)
  }
}

impl<D> Display for NRTCachingDirectory<D>
where
  D: Directory,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "NRTCachingDirectory({}; maxCacheMB={:?} maxMergeSizeMB={:?})",
      self.delegate,
      self.max_cached_bytes as f64 / 1024.0 / 1024.0,
      self.max_merge_size_bytes as f64 / 1024.0 / 1024.0
    )
  }
}

impl<D> HasIdentity for NRTCachingDirectory<D>
where
  D: Directory,
{
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl<D> Directory for NRTCachingDirectory<D>
where
  D: Directory,
{
  fn list_all(&self) -> Result<Vec<String>> {
    let _guard = self.lock.lock();
    let mut files = BTreeSet::new();
    files.extend(self.cache_directory.list_all()?);
    files.extend(self.delegate.list_all()?);
    Ok(files.into_iter().collect())
  }

  fn delete_file(&self, name: &str) -> Result<()> {
    let _guard = self.lock.lock();
    if self.cache_directory.file_exists(name)? {
      let size: i64 = self.cache_directory.file_length(name)?.try_convert()?;
      self.cache_directory.delete_file(name)?;
      let new_size = self
        .cache_size
        .fetch_sub(size, Ordering::SeqCst)
        .wrapping_sub(size);
      debug_assert!(new_size >= 0);
      Ok(())
    } else {
      self.delegate.delete_file(name)
    }
  }

  fn file_length(&self, name: &str) -> Result<usize> {
    let _guard = self.lock.lock();
    if self.cache_directory.file_exists(name)? {
      self.cache_directory.file_length(name)
    } else {
      self.delegate.file_length(name)
    }
  }

  type IndexOutput = IndexOutputEnum2<<CacheDirectory as Directory>::IndexOutput, D::IndexOutput>;

  fn create_output(&self, name: &str, context: &IOContext) -> Result<Self::IndexOutput> {
    if self.do_cache_write(name, context) {
      Ok(IndexOutputEnum2::A(
        self.cache_directory.create_output(name, context)?,
      ))
    } else {
      Ok(IndexOutputEnum2::B(
        self.delegate.create_output(name, context)?,
      ))
    }
  }

  fn sync(&self, file_names: &[String]) -> Result<()> {
    for file_name in file_names {
      self.un_cache(file_name)?;
    }
    self.delegate.sync(file_names)
  }

  fn sync_metadata(&self) -> Result<()> {
    self.delegate.sync_metadata()
  }

  fn rename(&self, source: &str, dest: &str) -> Result<()> {
    self.un_cache(source)?;
    if self.cache_directory.file_exists(dest)? {
      return Err(LuceneError::illegal_argument(format!(
        "target file {dest} already exists"
      )));
    }
    self.delegate.rename(source, dest)
  }

  type IndexInput = IndexInputEnum2<<CacheDirectory as Directory>::IndexInput, D::IndexInput>;

  fn open_input(&self, name: &str, context: &IOContext) -> Result<Self::IndexInput> {
    let _guard = self.lock.lock();
    if self.cache_directory.file_exists(name)? {
      Ok(IndexInputEnum2::A(
        self.cache_directory.open_input(name, context)?,
      ))
    } else {
      Ok(IndexInputEnum2::B(self.delegate.open_input(name, context)?))
    }
  }

  /// Creates a temporary output while ensuring that the generated name does
  /// not already exist in the other directory.
  fn create_temp_output(
    &self,
    prefix: &str,
    suffix: &str,
    context: &IOContext,
  ) -> Result<Self::IndexOutput> {
    let mut to_delete = HashSet::new();

    // This is very ugly/messy/dangerous (in some disastrous case it may create
    // too many temp files), but I don't know of a cleaner way.
    let mut success = false;

    let (first, second) = if self.do_cache_write(prefix, context) {
      (
        DirectoryEnum2::A(&self.cache_directory),
        DirectoryEnum2::B(&self.delegate),
      )
    } else {
      (
        DirectoryEnum2::B(&self.delegate),
        DirectoryEnum2::A(&self.cache_directory),
      )
    };

    // If this first creation fails, the Java finally block only closes null and
    // deletes an empty set, so there is no cleanup to perform.
    let mut out = first.create_temp_output(prefix, suffix, context)?;
    let body_result = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
      loop {
        let name = out.get_name().to_string();
        to_delete.insert(name.clone());
        if Self::slow_file_exists(&second, &name)? {
          out.close()?;
          out = first.create_temp_output(prefix, suffix, context)?;
        } else {
          to_delete.remove(&name);
          success = true;
          break;
        }
      }
      Ok(())
    }));

    if success {
      IOUtils::delete_files(&first, &to_delete)?;
    } else {
      IOUtils::close_resources_while_handling_error(&mut out)?;
      IOUtils::delete_files_ignoring_exceptions(&first, &to_delete);
    }

    match body_result {
      Ok(result) => result?,
      Err(payload) => resume_unwind(payload),
    }
    Ok(out)
  }

  type Lock = D::Lock;

  fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
    self.delegate.obtain_lock(name)
  }

  fn get_pending_deletions(&self) -> Result<HashSet<String>> {
    self.delegate.get_pending_deletions()
  }

  #[cfg(debug_assertions)]
  fn is_fs_directory(&self) -> bool {
    self.delegate.is_fs_directory()
  }

  fn ensure_open(&self) -> Result<()> {
    self.delegate.ensure_open()
  }
}

/// Close this directory, flushing any cached files to the delegate and then
/// closing the delegate.
impl<D> CloseableRef for NRTCachingDirectory<D>
where
  D: Directory,
{
  fn close(&self) -> Result<()> {
    // Technically IndexWriter should already have synced all files, but do this
    // defensively and for applications that create outputs directly.
    IOUtils::close(0..3, |operation| match operation {
      0 => {
        if !self.closed.swap(true, Ordering::SeqCst) {
          for file_name in self.cache_directory.list_all()? {
            self.un_cache(&file_name)?;
          }
        }
        Ok(())
      },
      1 => self.cache_directory.close(),
      2 => self.delegate.close(),
      _ => unreachable!(),
    })
  }
}

impl<D> Accountable for NRTCachingDirectory<D>
where
  D: Directory,
{
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(self.cache_size.load(Ordering::SeqCst))
  }
}

pub(crate) trait NRTCachingDirectoryBase {
  fn do_cache_write<D>(
    &self,
    directory: &NRTCachingDirectory<D>,
    name: &str,
    context: &IOContext,
  ) -> bool
  where
    D: Directory;
}

pub(crate) struct NRTCachingDirectoryDefaults;

impl NRTCachingDirectoryDefaults {
  pub(crate) fn do_cache_write<D>(
    directory: &NRTCachingDirectory<D>,
    _name: &str,
    context: &IOContext,
  ) -> bool
  where
    D: Directory,
  {
    let bytes = if let Some(merge_info) = &context.merge_info {
      merge_info.get_estimated_merge_bytes()
    } else if let Some(flush_info) = &context.flush_info {
      flush_info.get_estimated_segment_size()
    } else {
      return false;
    };

    bytes <= directory.max_merge_size_bytes
      && bytes.wrapping_add(directory.cache_size.load(Ordering::SeqCst))
        <= directory.max_cached_bytes
  }
}

pub(crate) enum NRTCachingDirectoryHook {
  Default,
  #[cfg(test)]
  AssertCacheWrite(AssertCacheWriteNRTCachingDirectory),
}

impl NRTCachingDirectoryBase for NRTCachingDirectoryHook {
  fn do_cache_write<D>(
    &self,
    directory: &NRTCachingDirectory<D>,
    name: &str,
    context: &IOContext,
  ) -> bool
  where
    D: Directory,
  {
    match self {
      Self::Default => NRTCachingDirectoryDefaults::do_cache_write(directory, name, context),
      #[cfg(test)]
      Self::AssertCacheWrite(hook) => hook.do_cache_write(directory, name, context),
    }
  }
}
