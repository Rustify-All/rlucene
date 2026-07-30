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
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::fs;
use std::fs::{File, Metadata, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use fs2::FileExt;
use parking_lot::Mutex;

use crate::core::store::fs_lock_factory::FSLockFactory;
use crate::core::store::lock::Lock;
use crate::core::store::lock_factory::LockFactory;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};

/// Implements [`lock_factory`](crate::core::store::lock_factory) using native OS file
/// locks.
///
/// # Note
/// - This `lock_factory` relies on `std::fs` and native OS file locking APIs.
///   Any issues with these APIs may cause locking to fail. For example, in
///   certain NFS environments, native file locks might fail (allowing locks to
///   be acquired twice incorrectly), whereas
///   [`simple_fs_lock_factory`](crate::core::store::simple_fs_lock_factory) works
///   correctly in those environments.
/// - For NFS-based access to an index, it is recommended to try
///   [`simple_fs_lock_factory`](crate::core::store::simple_fs_lock_factory) first and
///   handle its limitation: a lock file may remain if the process exits
///   abnormally.
///
/// # Advantages
/// The primary advantage of `native_fs_lock_factory` is that locks (but not the
/// lock files themselves) will be properly released by the operating system if
/// the process exits abnormally.
///
/// # Lock File Behavior
/// Unlike [`simple_fs_lock_factory`](crate::core::store::simple_fs_lock_factory),
/// leftover lock files in the filesystem are acceptable because the OS will
/// release the locks even if the files remain. This implementation will not
/// actively remove these lock files, so they might be visible, but this does
/// not mean the index is locked.
///
/// # Implementation Change Warning
/// Special care is required when changing the locking implementation:
/// - Ensure no writer is currently writing to the index before making changes,
///   as this could corrupt the index.
/// - Apply the `lock_factory` change across all instances using the index.
/// - Clean up leftover lock files before starting the new configuration.
///
/// Different locking implementations are not compatible and cannot work
/// together.
///
/// # See Also
/// - [`lock_factory`](crate::core::store::lock_factory)
pub struct NativeFSLockFactory {
  lock_held: Arc<Mutex<HashSet<String>>>,
}

impl Default for NativeFSLockFactory {
  fn default() -> Self {
    Self::new()
  }
}

impl NativeFSLockFactory {
  /// Creates a new instance.
  pub fn new() -> Self {
    Self {
      lock_held: get_lock_held(),
    }
  }
}
impl LockFactory for NativeFSLockFactory {
  type Lock = NativeFSLock;

  fn obtain_lock(&self, dir: &Path, lock_name: &str) -> Result<Self::Lock> {
    FSLockFactory::obtain_lock(self, dir, lock_name)
  }
}

impl Display for NativeFSLockFactory {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "NativeFSLockFactory")
  }
}

impl FSLockFactory for NativeFSLockFactory {
  fn obtain_fs_lock(&self, dir: &Path, lock_name: &str) -> Result<Self::Lock> {
    fs::create_dir_all(dir)
      .map_err(|e| LuceneError::io_with_path(dir.to_string_lossy().to_string(), e))?;

    let lock_file = dir.join(lock_name);

    // we must create the file to have a truly canonical path.
    // if it's already created, we don't care. if it cant be created, it
    // will fail below.
    let file = OpenOptions::new()
      .write(true)
      .create(true)
      .truncate(false)
      .open(&lock_file)?;
    let real_path = lock_file
      .canonicalize()
      .map_err(|e| LuceneError::io_with_path(lock_file.to_string_lossy().to_string(), e))?;
    let real_path_str = real_path.to_string_lossy().to_string();
    let metadata = file.metadata()?;

    let mut lock_held = self.lock_held.lock();
    if !lock_held.insert(real_path_str.clone()) {
      return Err(LuceneError::lock_obtain_failed(format!(
        "Lock held by this virtual machine: {real_path_str}"
      )));
    }
    let result =
      std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| file.try_lock_exclusive()));
    match result {
      Ok(Ok(())) => Ok(NativeFSLock {
        file: Mutex::new(Some(file)),
        path: real_path,
        metadata,
        closed: AtomicBool::new(false),
        #[cfg(test)]
        lock_released_for_test: AtomicBool::new(false),
        close_lock: Mutex::new(()),
      }),
      Ok(Err(error)) => {
        lock_held.remove(&real_path_str);
        if error.kind() == ErrorKind::WouldBlock {
          Err(LuceneError::lock_obtain_failed(format!(
            "Lock held by another program: {real_path_str}"
          )))
        } else {
          Err(LuceneError::io_with_path(real_path_str, error))
        }
      },
      Err(payload) => {
        lock_held.remove(&real_path_str);
        std::panic::resume_unwind(payload)
      },
    }
  }
}

impl Drop for NativeFSLock {
  fn drop(&mut self) {
    self
      .close()
      .unwrap_or_else(|e| eprintln!("Failed to release lock on drop: {}", e));
  }
}

static LOCK_HELD: OnceLock<Arc<Mutex<HashSet<String>>>> = OnceLock::new();

fn get_lock_held() -> Arc<Mutex<HashSet<String>>> {
  LOCK_HELD
    .get_or_init(|| Arc::new(Mutex::new(HashSet::new())))
    .clone()
}

pub struct NativeFSLock {
  file: Mutex<Option<File>>,
  pub(crate) path: PathBuf,
  pub(crate) metadata: Metadata,
  pub(crate) closed: AtomicBool,
  #[cfg(test)]
  lock_released_for_test: AtomicBool,
  close_lock: Mutex<()>,
}

impl NativeFSLock {
  #[cfg(test)]
  pub(crate) fn release_lock_for_test(&self) -> Result<()> {
    self
      .file
      .lock()
      .as_ref()
      .ok_or_else(|| LuceneError::already_closed("native file lock is closed"))?
      .unlock()?;
    self.lock_released_for_test.store(true, Ordering::SeqCst);
    Ok(())
  }

  fn format_metadata(&self) -> String {
    let size = self.metadata.len();
    let permissions = self.metadata.permissions();
    let modified_time = self.metadata.modified().ok().map_or_else(
      || "unknown".to_string(),
      |time| match time.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(duration) => {
          let datetime = DateTime::<Utc>::from(SystemTime::UNIX_EPOCH + duration);
          datetime.format("%Y-%m-%d %H:%M:%S").to_string()
        },
        Err(_) => "invalid time".to_string(),
      },
    );
    format!("size: {size} bytes, permissions: {permissions:?}, modified: {modified_time}")
  }
}

impl Display for NativeFSLock {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "NativeFSLock(path= {}, file_metadata= {})",
      self.path.display(),
      self.format_metadata()
    )
  }
}

impl CloseableRef for NativeFSLock {
  fn close(&self) -> Result<()> {
    let _guard = self.close_lock.lock();
    if !self.closed.load(Ordering::SeqCst) {
      let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        if let Some(file) = self.file.lock().take() {
          let unlock_result = file.unlock();
          drop(file);
          unlock_result?;
        }
        Ok(())
      }));
      self.closed.store(true, Ordering::SeqCst);
      let real_path_str = self.path.to_string_lossy().to_string();
      let locks = get_lock_held();
      locks.lock().remove(&real_path_str);
      match result {
        Ok(result) => result?,
        Err(payload) => std::panic::resume_unwind(payload),
      }
    }
    Ok(())
  }
}

impl Lock for NativeFSLock {
  /// Ensures the validity of the current lock.
  ///
  /// # Errors
  /// - Returns `LuceneError::illegal_state` if:
  ///   - The lock file is no longer in the global lock map.
  ///   - The file lock is no longer valid.
  ///   - The lock file size is not 0.
  ///   - The lock file has been deleted or is inaccessible.
  fn ensure_valid(&self) -> Result<()> {
    if self.closed.load(Ordering::SeqCst) {
      return Err(LuceneError::already_closed(format!(
        "Lock already released: {:?}",
        self.path
      )));
    }

    let lock_held = LOCK_HELD.get_or_init(|| Arc::new(Mutex::new(HashSet::new())));
    let lock_held = lock_held.lock();
    if !lock_held.contains(&self.path.to_string_lossy().to_string()) {
      return Err(LuceneError::already_closed(format!(
        "Lock path unexpectedly cleared from map: {:?}",
        self.path
      )));
    }

    #[cfg(test)]
    if self.lock_released_for_test.load(Ordering::SeqCst) {
      return Err(LuceneError::already_closed(format!(
        "File lock invalidated by an external force: {:?}",
        self.path
      )));
    }

    let metadata = self
      .file
      .lock()
      .as_ref()
      .ok_or_else(|| {
        LuceneError::already_closed(format!("Lock already released: {:?}", self.path))
      })?
      .metadata()
      .map_err(LuceneError::io)?;
    if metadata.len() != 0 {
      return Err(LuceneError::illegal_state(format!(
        "Unexpected lock file size: {}, (lock: {:?})",
        metadata.len(),
        self.path
      )));
    }

    if !self.path.exists() {
      return Err(LuceneError::illegal_state(format!(
        "Lock file deleted or inaccessible: {:?}",
        self.path
      )));
    }

    Ok(())
  }
}
