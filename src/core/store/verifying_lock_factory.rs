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
use crate::core::util::IOUtils;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use parking_lot::Mutex;
use std::fmt::{Display, Formatter};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;

pub const MSG_LOCK_RELEASED: u8 = 0;
pub const MSG_LOCK_ACQUIRED: u8 = 1;

struct Protocol<R, W> {
  input: R,
  output: W,
}

/// A lock factory that verifies lock acquisition and release against an
/// external lock verification server.
pub struct VerifyingLockFactory<LF, R, W> {
  lock_factory: LF,
  protocol: Arc<Mutex<Protocol<R, W>>>,
}

impl<LF, R, W> VerifyingLockFactory<LF, R, W> {
  pub fn new(lock_factory: LF, input: R, output: W) -> Result<Self> {
    Ok(Self {
      lock_factory,
      protocol: Arc::new(Mutex::new(Protocol { input, output })),
    })
  }
}

impl<LF, R, W> Display for VerifyingLockFactory<LF, R, W>
where
  LF: Display,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "VerifyingLockFactory({})", self.lock_factory)
  }
}

impl<LF, R, W> LockFactory for VerifyingLockFactory<LF, R, W>
where
  LF: LockFactory,
  R: Read + Send,
  W: Write + Send,
{
  type Lock = CheckedLock<LF::Lock, R, W>;

  fn obtain_lock(&self, dir: &Path, lock_name: &str) -> Result<Self::Lock> {
    CheckedLock::new(
      self.lock_factory.obtain_lock(dir, lock_name)?,
      Arc::clone(&self.protocol),
    )
  }
}

pub struct CheckedLock<L, R, W> {
  lock: L,
  protocol: Arc<Mutex<Protocol<R, W>>>,
}

impl<L, R, W> CheckedLock<L, R, W>
where
  R: Read,
  W: Write,
{
  fn new(lock: L, protocol: Arc<Mutex<Protocol<R, W>>>) -> Result<Self> {
    let checked_lock = Self { lock, protocol };
    checked_lock.verify(MSG_LOCK_ACQUIRED)?;
    Ok(checked_lock)
  }

  fn verify(&self, message: u8) -> Result<()> {
    let mut protocol = self.protocol.lock();
    protocol.output.write_all(&[message])?;
    protocol.output.flush()?;
    let mut response = [0u8; 1];
    let count = protocol.input.read(&mut response)?;
    if count == 0 {
      return Err(LuceneError::illegal_state(
        "Lock server died because of locking error.",
      ));
    }
    if response[0] != message {
      return Err(LuceneError::io(std::io::Error::other(
        "Protocol violation.",
      )));
    }
    Ok(())
  }
}

impl<L, R, W> Display for CheckedLock<L, R, W>
where
  L: Display,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    self.lock.fmt(f)
  }
}

impl<L, R, W> CloseableRef for CheckedLock<L, R, W>
where
  L: Lock,
  R: Read + Send,
  W: Write + Send,
{
  fn close(&self) -> Result<()> {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      self
        .lock
        .ensure_valid()
        .and_then(|_| self.verify(MSG_LOCK_RELEASED))
    }));
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.lock.close()));
    IOUtils::use_or_suppress_caught_result(result, close_result)
  }
}

impl<L, R, W> Lock for CheckedLock<L, R, W>
where
  L: Lock,
  R: Read + Send,
  W: Write + Send,
{
  fn ensure_valid(&self) -> Result<()> {
    self.lock.ensure_valid()
  }
}
