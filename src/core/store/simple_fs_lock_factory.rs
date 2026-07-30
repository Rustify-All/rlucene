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
use std::fmt::{Display, Formatter};
use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use crate::core::store::fs_lock_factory::FSLockFactory;
use crate::core::store::lock::Lock;
use crate::core::store::lock_factory::LockFactory;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use parking_lot::Mutex;

/// Implements [`LockFactory`] using `Files::create_file`.
///
/// The main downside with using this API for locking is that the Lucene write lock may not be
/// released when the process exits abnormally.
///
/// When this happens, a `LockObtainFailedError` is hit when trying to create a writer,
/// in which case you may need to explicitly clear the lock file first by manually removing the file.
/// But, first be certain that no writer is in fact writing to the index otherwise you can easily
/// corrupt your index.
///
/// Special care needs to be taken if you change the locking implementation: First be certain that
/// no writer is in fact writing to the index otherwise you can easily corrupt your index. Be sure to
/// do the LockFactory change all Lucene instances and clean up all leftover lock files before
/// starting the new configuration for the first time. Different implementations can not work
/// together!
///
/// If you suspect that this or any other [`LockFactory`] is not working properly in your environment,
/// you can easily test it by using `VerifyingLockFactory`, `LockVerifyServer` and
/// `LockStressTest`.
///
/// This is a singleton, you have to use `INSTANCE`.
///
/// See also: [`LockFactory`].
pub struct SimpleFSLockFactory;

impl Default for SimpleFSLockFactory {
  fn default() -> Self {
    Self::new()
  }
}

impl SimpleFSLockFactory {
  pub fn new() -> Self {
    Self
  }
}

impl LockFactory for SimpleFSLockFactory {
  type Lock = SimpleFSLock;

  fn obtain_lock(&self, dir: &Path, lock_name: &str) -> Result<Self::Lock> {
    FSLockFactory::obtain_lock(self, dir, lock_name)
  }
}

impl Display for SimpleFSLockFactory {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "SimpleFSLockFactory")
  }
}

impl FSLockFactory for SimpleFSLockFactory {
  fn obtain_fs_lock(&self, dir: &Path, lock_name: &str) -> Result<Self::Lock> {
    fs::create_dir_all(dir)
      .map_err(|e| LuceneError::io_with_path(dir.to_string_lossy().to_string(), e))?;

    let lock_file = dir.join(lock_name);
    match OpenOptions::new()
      .write(true)
      .create_new(true)
      .open(&lock_file)
    {
      Ok(file) => {
        let metadata = file
          .metadata()
          .map_err(|e| LuceneError::io_with_path(lock_file.to_string_lossy().to_string(), e))?;
        let creation_time = metadata
          .created()
          .or_else(|_| metadata.modified())
          .map_err(|e| LuceneError::io_with_path(lock_file.to_string_lossy().to_string(), e))?;
        Ok(SimpleFSLock::new(lock_file, creation_time))
      },
      Err(e) if e.kind() == ErrorKind::AlreadyExists || e.kind() == ErrorKind::PermissionDenied => {
        let error = LuceneError::io_with_path(lock_file.to_string_lossy().to_string(), e);
        let mut lock_obtain_failed_error =
          LuceneError::lock_obtain_failed(format!("Lock held elsewhere: {}", lock_file.display()));
        lock_obtain_failed_error.add_suppressed(error);
        Err(lock_obtain_failed_error)
      },
      Err(e) => Err(LuceneError::io_with_path(
        lock_file.to_string_lossy().to_string(),
        e,
      )),
    }
  }
}

pub struct SimpleFSLock {
  pub(crate) path: PathBuf,
  pub(crate) creation_time: SystemTime,
  pub(crate) closed: AtomicBool,
  close_lock: Mutex<()>,
}

impl SimpleFSLock {
  pub(crate) fn new(path: PathBuf, creation_time: SystemTime) -> Self {
    Self {
      path,
      creation_time,
      closed: AtomicBool::new(false),
      close_lock: Mutex::new(()),
    }
  }
}

impl Display for SimpleFSLock {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "SimpleFSLock(path={},creationTime={:?})",
      self.path.display(),
      self.creation_time
    )
  }
}

impl CloseableRef for SimpleFSLock {
  fn close(&self) -> Result<()> {
    let _guard = self.close_lock.lock();
    if self.closed.load(Ordering::SeqCst) {
      return Ok(());
    }

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      self.ensure_valid().map_err(|e| {
        let mut error = LuceneError::lock_release_failed(
          "Lock file cannot be safely removed. Manual intervention is recommended.",
        );
        error.add_suppressed(e);
        error
      })?;

      fs::remove_file(&self.path).map_err(|e| {
        let mut error = LuceneError::lock_release_failed(
          "Unable to remove lock file. Manual intervention is recommended",
        );
        error.add_suppressed(e.into());
        error
      })?;
      Ok(())
    }));

    self.closed.store(true, Ordering::SeqCst);
    match result {
      Ok(result) => result,
      Err(payload) => std::panic::resume_unwind(payload),
    }
  }
}

impl Lock for SimpleFSLock {
  fn ensure_valid(&self) -> Result<()> {
    if self.closed.load(Ordering::SeqCst) {
      return Err(LuceneError::already_closed(format!(
        "Lock instance already released: {self}"
      )));
    }

    let metadata = fs::metadata(&self.path)
      .map_err(|e| LuceneError::io_with_path(self.path.to_string_lossy().to_string(), e))?;
    let creation_time = metadata
      .created()
      .map_err(|e| LuceneError::io_with_path(self.path.to_string_lossy().to_string(), e))?;
    if self.creation_time != creation_time {
      return Err(LuceneError::already_closed(format!(
        "Underlying file changed by an external force at {creation_time:?}, (lock={self})"
      )));
    }

    Ok(())
  }
}

impl Drop for SimpleFSLock {
  fn drop(&mut self) {
    self
      .close()
      .unwrap_or_else(|e| eprintln!("Failed to release lock on drop: {}", e));
  }
}
