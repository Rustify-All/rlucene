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
use crate::core::index::{check_index, directory_reader};
use crate::core::store::IOContext;
use crate::core::store::buffered_checksum_index_input::BufferedChecksumIndexInput;
use crate::core::store::directory::Directory;
use crate::core::util::HasIdentity;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::util::test_util::TestUtil;
use parking_lot::Mutex;
use rand::rngs::StdRng;
use rand::{Rng, RngExt, SeedableRng};
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::sync::atomic::{AtomicBool, Ordering};

/// Calls check index on close.
///
/// This mirrors Java's `BaseDirectoryWrapper extends FilterDirectory` by
/// directly implementing `Directory` and forwarding the common operations to
/// the wrapped directory.
pub struct BaseDirectoryWrapper<D> {
  pub(crate) in_: D,
  check_index_on_close: AtomicBool,
  level_for_check_on_close: i32,
  check_index_random: Mutex<StdRng>,
  pub(crate) is_open: AtomicBool,
  id: Identity,
}

impl<D> BaseDirectoryWrapper<D>
where
  D: Directory,
{
  pub fn new<R>(random: &mut R, delegate: D) -> Self
  where
    R: Rng + ?Sized,
  {
    Self {
      in_: delegate,
      check_index_on_close: AtomicBool::new(true),
      level_for_check_on_close: check_index::Level::MIN_LEVEL_FOR_SLOW_CHECKS,
      check_index_random: Mutex::new(StdRng::seed_from_u64(random.random())),
      is_open: AtomicBool::new(true),
      id: Identity::new(),
    }
  }

  pub fn get_delegate(&self) -> &D {
    &self.in_
  }

  pub fn is_open(&self) -> bool {
    self.is_open.load(Ordering::SeqCst)
  }

  /// Set whether or not checkindex should be run on close.
  pub fn set_check_index_on_close(&self, value: bool) {
    self.check_index_on_close.store(value, Ordering::Relaxed);
  }

  pub fn get_check_index_on_close(&self) -> bool {
    self.check_index_on_close.load(Ordering::Relaxed)
  }

  pub fn set_cross_check_term_vectors_on_close(&mut self, value: bool) {
    // If true, we are enabling slow checks.
    if value {
      self.level_for_check_on_close = check_index::Level::MIN_LEVEL_FOR_SLOW_CHECKS;
    } else {
      self.level_for_check_on_close = check_index::Level::MIN_LEVEL_FOR_INTEGRITY_CHECKS;
    }
  }

  pub fn get_level_for_check_on_close(&self) -> i32 {
    self.level_for_check_on_close
  }
}

impl<D> Display for BaseDirectoryWrapper<D>
where
  D: Directory,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "BaseDirectoryWrapper({})", self.in_)
  }
}

impl<D> CloseableRef for BaseDirectoryWrapper<D>
where
  D: Directory,
{
  fn close(&self) -> Result<()> {
    if self.is_open.swap(false, Ordering::SeqCst)
      && self.get_check_index_on_close()
      && directory_reader::index_exists(self)?
    {
      TestUtil::check_index_with_level(
        &mut *self.check_index_random.lock(),
        self,
        self.level_for_check_on_close,
      )?;
    }
    self.in_.close()
  }
}

impl<D> HasIdentity for BaseDirectoryWrapper<D>
where
  D: Directory,
{
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl<D> Directory for BaseDirectoryWrapper<D>
where
  D: Directory,
{
  fn list_all(&self) -> Result<Vec<String>> {
    self.in_.list_all()
  }

  fn delete_file(&self, name: &str) -> Result<()> {
    self.in_.delete_file(name)
  }

  fn file_length(&self, name: &str) -> Result<usize> {
    self.in_.file_length(name)
  }

  type IndexOutput = D::IndexOutput;

  fn create_output(&self, name: &str, context: &IOContext) -> Result<Self::IndexOutput> {
    self.in_.create_output(name, context)
  }

  fn create_temp_output(
    &self,
    prefix: &str,
    suffix: &str,
    context: &IOContext,
  ) -> Result<Self::IndexOutput> {
    self.in_.create_temp_output(prefix, suffix, context)
  }

  fn sync(&self, names: &[String]) -> Result<()> {
    self.in_.sync(names)
  }

  fn sync_metadata(&self) -> Result<()> {
    self.in_.sync_metadata()
  }

  fn rename(&self, source: &str, dest: &str) -> Result<()> {
    self.in_.rename(source, dest)
  }

  type IndexInput = D::IndexInput;

  fn open_input(&self, name: &str, context: &IOContext) -> Result<Self::IndexInput> {
    self.in_.open_input(name, context)
  }

  fn open_checksum_input(
    &self,
    name: &str,
  ) -> Result<BufferedChecksumIndexInput<Self::IndexInput>> {
    self.in_.open_checksum_input(name)
  }

  type Lock = D::Lock;

  fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
    self.in_.obtain_lock(name)
  }

  fn get_pending_deletions(&self) -> Result<HashSet<String>> {
    self.in_.get_pending_deletions()
  }

  #[cfg(debug_assertions)]
  fn is_fs_directory(&self) -> bool {
    self.in_.is_fs_directory()
  }

  fn ensure_open(&self) -> Result<()> {
    self.in_.ensure_open()
  }
}
