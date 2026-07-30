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
use crate::core::store::lock::Lock;
use crate::core::store::lock_factory::LockFactory;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use parking_lot::Mutex;
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
/// Implements [`LockFactory`] for a single in-process instance, meaning all locking will take
/// place through this one instance. Only use this [`LockFactory`] when you are certain all
/// IndexWriters for a given index are running against a single shared in-process Directory instance.
///
///
/// See also: [`LockFactory`]
pub struct SingleInstanceLockFactory {
  inner: Arc<Mutex<Inner>>,
}
pub struct Inner {
  locks: HashSet<String>,
}
impl Default for SingleInstanceLockFactory {
  fn default() -> Self {
    Self::new()
  }
}

impl SingleInstanceLockFactory {
  pub fn new() -> SingleInstanceLockFactory {
    SingleInstanceLockFactory {
      inner: Arc::new(Mutex::new(Inner {
        locks: HashSet::new(),
      })),
    }
  }
}

impl Display for SingleInstanceLockFactory {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "SingleInstanceLockFactory")
  }
}

impl LockFactory for SingleInstanceLockFactory {
  type Lock = SingleInstanceLock;

  fn obtain_lock(&self, dir: &Path, lock_name: &str) -> Result<Self::Lock> {
    let mut inner = self.inner.lock();

    if inner.locks.insert(lock_name.to_string()) {
      return Ok(SingleInstanceLock::new(lock_name, self.inner.clone()));
    }
    Err(LuceneError::lock_obtain_failed(format!(
      "lock instance already obtained: (dir={:?}, lockName={})",
      dir, lock_name
    )))
  }
}

pub struct SingleInstanceLock {
  lock_name: String,
  closed: AtomicBool,
  inner: Arc<Mutex<Inner>>,
  close_lock: Mutex<()>,
}
impl SingleInstanceLock {
  pub fn new(lock_name: &str, inner: Arc<Mutex<Inner>>) -> SingleInstanceLock {
    SingleInstanceLock {
      lock_name: lock_name.to_string(),
      closed: AtomicBool::new(false),
      inner,
      close_lock: Mutex::new(()),
    }
  }
}

impl Display for SingleInstanceLock {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    let addr = format!("{:p}", self);
    f.write_fmt(format_args!("{}: {}", addr, self.lock_name))
  }
}

impl CloseableRef for SingleInstanceLock {
  fn close(&self) -> Result<()> {
    let _guard = self.close_lock.lock();
    if self.closed.load(Ordering::SeqCst) {
      return Ok(());
    }

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      let mut inner = self.inner.lock();
      if !inner.locks.remove(&self.lock_name) {
        return Err(LuceneError::already_closed(format!(
          "Lock was already released: {}",
          self
        )));
      }
      Ok(())
    }));
    self.closed.store(true, Ordering::SeqCst);
    match result {
      Ok(result) => result,
      Err(payload) => std::panic::resume_unwind(payload),
    }
  }
}

impl Lock for SingleInstanceLock {
  fn ensure_valid(&self) -> Result<()> {
    if self.closed.load(Ordering::SeqCst) {
      return Err(LuceneError::already_closed(format!(
        "Lock instance already released: {}",
        self
      )));
    }

    let inner = self.inner.lock();
    if !inner.locks.contains(&self.lock_name) {
      return Err(LuceneError::already_closed(format!(
        "Lock instance was invalidated from map: {}",
        self
      )));
    }

    Ok(())
  }
}
