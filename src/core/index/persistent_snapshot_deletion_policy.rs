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
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use crate::core::codecs::codec_util::CodecUtil;
use crate::core::index::index_deletion_policy::{IndexDeletionPolicy, IndexDeletionPolicyEnum};
use crate::core::index::index_file_deleter::CommitPoint;
use crate::core::index::index_writer_config::OpenMode;
use crate::core::index::snapshot_deletion_policy::SnapshotDeletionPolicy;
use crate::core::store::directory::Directory;
use crate::core::store::{DataInput, DataOutput, IO_CONTEXT_DEFAULT};
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::io_utils::IOUtils;
#[cfg(test)]
use crate::test_framework::core::util::failure_context::{
  ExecutionMethod, ExecutionOwner, ExecutionScope,
};

/// A [`SnapshotDeletionPolicy`] which adds a persistence layer so that snapshots can be
/// maintained across the life of an application. The snapshots are persisted in a [`Directory`]
/// and are committed as soon as [`Self::snapshot`] or [`Self::release`] is called.
///
/// **NOTE:** Sharing [`PersistentSnapshotDeletionPolicy`]s that write to the same directory across
/// [`IndexWriter`](crate::core::index::index_writer::IndexWriter)s will corrupt snapshots. You
/// should make sure every [`IndexWriter`](crate::core::index::index_writer::IndexWriter) has its
/// own [`PersistentSnapshotDeletionPolicy`] and that they all write to a different [`Directory`].
/// It is OK to use the same Directory that holds the index.
///
/// This struct adds a [`Self::release_gen`] method to release commits from a previous snapshot's
/// [`IndexCommit::get_generation`](crate::core::index::index_commit::IndexCommit::get_generation).
///
/// # Experimental
pub struct PersistentSnapshotDeletionPolicy<D> {
  /// Wrapped in-memory snapshot deletion policy.
  pub base: SnapshotDeletionPolicy<D>,
  // Writes are serialized by the base policy monitor; reads of the last saved file are not.
  next_write_gen: Arc<AtomicI64>,
  dir: Arc<D>,
}

pub const SNAPSHOTS_PREFIX: &str = "snapshots_";
const VERSION_START: i32 = 0;
const VERSION_CURRENT: i32 = VERSION_START;
const CODEC_NAME: &str = "snapshots";

impl<D> Clone for PersistentSnapshotDeletionPolicy<D> {
  fn clone(&self) -> Self {
    Self {
      base: self.base.clone(),
      next_write_gen: self.next_write_gen.clone(),
      dir: self.dir.clone(),
    }
  }
}

impl<D> PersistentSnapshotDeletionPolicy<D>
where
  D: Directory,
{
  /// [`PersistentSnapshotDeletionPolicy`] wraps another [`IndexDeletionPolicy`] to enable flexible
  /// snapshotting, passing [`OpenMode::CreateOrAppend`] by default.
  ///
  /// # Parameters
  ///
  /// * `primary` - the [`IndexDeletionPolicy`] that is used on non-snapshotted commits.
  ///   Snapshotted commits, by definition, are not deleted until explicitly released via
  ///   [`Self::release`].
  /// * `dir` - the [`Directory`] which will be used to persist the snapshots information.
  pub fn new<T>(primary: T, dir: Arc<D>) -> Result<Self>
  where
    T: Into<IndexDeletionPolicyEnum<D>>,
  {
    Self::with_open_mode(primary, dir, OpenMode::CreateOrAppend)
  }

  /// [`PersistentSnapshotDeletionPolicy`] wraps another [`IndexDeletionPolicy`] to enable flexible
  /// snapshotting.
  ///
  /// # Parameters
  ///
  /// * `primary` - the [`IndexDeletionPolicy`] that is used on non-snapshotted commits.
  ///   Snapshotted commits, by definition, are not deleted until explicitly released via
  ///   [`Self::release`].
  /// * `dir` - the [`Directory`] which will be used to persist the snapshots information.
  /// * `mode` - specifies whether a new index should be created, deleting all existing snapshots
  ///   information immediately, or open an existing index, initializing the struct with the
  ///   snapshots information.
  pub fn with_open_mode<T>(primary: T, dir: Arc<D>, mode: OpenMode) -> Result<Self>
  where
    T: Into<IndexDeletionPolicyEnum<D>>,
  {
    let policy = PersistentSnapshotDeletionPolicy {
      base: SnapshotDeletionPolicy::new(primary),
      next_write_gen: Arc::new(AtomicI64::new(0)),
      dir,
    };

    {
      let _guard = policy.base.lock();
      if mode == OpenMode::Create {
        policy.clear_prior_snapshots()?;
      }

      policy.load_prior_snapshots()?;

      if mode == OpenMode::Append && policy.next_write_gen.load(Ordering::Relaxed) == 0 {
        return Err(LuceneError::illegal_state(
          "no snapshots stored in this directory",
        ));
      }
    }

    Ok(policy)
  }

  /// Snapshots the last commit. Once this method returns, the snapshot information is persisted in
  /// the directory.
  pub fn snapshot(&self) -> Result<Arc<CommitPoint<D>>> {
    let _guard = self.base.lock();
    let index_commit = self.base.snapshot()?;
    let mut success = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      self.persist()?;
      success = true;
      Ok(())
    }));
    if !success {
      let _ = self.base.release(&index_commit);
    }
    unwrap_caught_result!(result)?;
    Ok(index_commit)
  }

  /// Deletes a snapshotted commit. Once this method returns, the snapshot information is persisted
  /// in the directory.
  pub fn release(&self, commit: &Arc<CommitPoint<D>>) -> Result<()> {
    let _guard = self.base.lock();
    self.base.release(commit)?;
    let mut success = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      self.persist()?;
      success = true;
      Ok(())
    }));
    if !success {
      self.base.inc_ref(commit);
    }
    unwrap_caught_result!(result)
  }

  /// Deletes a snapshotted commit by generation. Once this method returns, the snapshot information
  /// is persisted in the directory.
  ///
  /// # See Also
  ///
  /// [`IndexCommit::get_generation`](crate::core::index::index_commit::IndexCommit::get_generation)
  pub fn release_gen(&self, generation: i64) -> Result<()> {
    let _guard = self.base.lock();
    self.base.release_gen(generation)?;
    self.persist()
  }

  fn persist(&self) -> Result<()> {
    let guard = self.base.lock();
    #[cfg(test)]
    let _execution_scope = ExecutionScope::enter(
      ExecutionOwner::PersistentSnapshotDeletionPolicy,
      ExecutionMethod::Persist,
    );
    let next_write_gen = self.next_write_gen.load(Ordering::Relaxed);
    let file_name = format!("{SNAPSHOTS_PREFIX}{next_write_gen}");
    let mut success = false;
    let mut out = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      out = Some(self.dir.create_output(
        &file_name,
        IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?,
      )?);
      let write_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
          let out = out
            .as_mut()
            .ok_or_else(|| LuceneError::illegal_state("snapshot output is missing"))?;
          CodecUtil::write_header(out, CODEC_NAME, VERSION_CURRENT)?;
          let ref_counts = guard.borrow();
          out.write_vint(ref_counts.ref_counts.len() as i32)?;
          for (generation, ref_count) in &ref_counts.ref_counts {
            out.write_vlong(*generation)?;
            out.write_vint(*ref_count)?;
          }
          success = true;
          Ok(())
        }));
      let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        out
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("snapshot output is missing"))?
          .close()
      }));
      IOUtils::use_or_suppress_caught_result(write_result, close_result)
    }));
    if !success {
      IOUtils::delete_files_ignoring_exceptions(self.dir.as_ref(), std::iter::once(&file_name));
    }
    unwrap_caught_result!(result)?;

    self.dir.sync(std::slice::from_ref(&file_name))?;

    if next_write_gen > 0 {
      let last_save_file = format!("{SNAPSHOTS_PREFIX}{}", next_write_gen - 1);
      // The error is acceptable: the snapshot likely did not exist.
      IOUtils::delete_files_ignoring_exceptions(
        self.dir.as_ref(),
        std::iter::once(&last_save_file),
      );
    }

    self.next_write_gen.fetch_add(1, Ordering::Relaxed);
    Ok(())
  }

  fn clear_prior_snapshots(&self) -> Result<()> {
    let _guard = self.base.lock();
    for file in self.dir.list_all()? {
      if file.starts_with(SNAPSHOTS_PREFIX) {
        self.dir.delete_file(&file)?;
      }
    }
    Ok(())
  }

  /// Returns the file name the snapshots are currently saved to, or `None` if no snapshots have
  /// been saved.
  pub fn get_last_save_file(&self) -> Option<String> {
    let next_write_gen = self.next_write_gen.load(Ordering::Relaxed);
    if next_write_gen == 0 {
      None
    } else {
      Some(format!("{SNAPSHOTS_PREFIX}{}", next_write_gen - 1))
    }
  }

  /// Returns all IndexCommits held by at least one snapshot.
  pub fn get_snapshots(&self) -> Vec<Arc<CommitPoint<D>>> {
    self.base.get_snapshots()
  }

  /// Returns the total number of snapshots currently held.
  pub fn get_snapshot_count(&self) -> i32 {
    self.base.get_snapshot_count()
  }

  /// Retrieve an [`IndexCommit`](crate::core::index::index_commit::IndexCommit) from its generation;
  /// returns `None` if this commit is not currently snapshotted.
  pub fn get_index_commit(&self, generation: i64) -> Option<Arc<CommitPoint<D>>> {
    self.base.get_index_commit(generation)
  }

  /// Reads the snapshots information from the given [`Directory`].
  fn load_prior_snapshots(&self) -> Result<()> {
    let guard = self.base.lock();
    let mut gen_loaded = -1;
    let mut io_error = None;
    let files = self.dir.list_all()?;
    let mut snapshot_files = smallvec::SmallVec::<[String; 2]>::new();
    for file in files {
      if let Some(gen_part) = file.strip_prefix(SNAPSHOTS_PREFIX) {
        let gen_: i64 = gen_part.parse()?;
        if gen_loaded == -1 || gen_ > gen_loaded {
          snapshot_files.push(file);
          let file = &snapshot_files[snapshot_files.len() - 1];
          let mut ref_counts = HashMap::new();
          let mut input = self
            .dir
            .open_input(file, IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?)?;
          let read_result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
              let result = (|| -> Result<()> {
                CodecUtil::check_header(&mut input, CODEC_NAME, VERSION_START, VERSION_START)?;
                let count = input.read_vint()?;
                for _ in 0..count {
                  let commit_generation = input.read_vlong()?;
                  let ref_count = input.read_vint()?;
                  ref_counts.insert(commit_generation, ref_count);
                }
                Ok(())
              })();
              match result {
                Err(error) if error.is_io_error() => {
                  if io_error.is_none() {
                    io_error = Some(error);
                  }
                },
                Err(error) => return Err(error),
                Ok(()) => {},
              }
              Ok(())
            }));
          let close_result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| input.close()));
          IOUtils::finally_caught_result(read_result, close_result)?;

          gen_loaded = gen_;
          let mut inner = guard.borrow_mut();
          inner.ref_counts = ref_counts;
          inner.index_commits.clear();
        }
      }
    }

    if gen_loaded == -1 {
      // Nothing was loaded...
      if let Some(error) = io_error {
        // ... not for lack of trying:
        return Err(error);
      }
    } else {
      if snapshot_files.len() > 1 {
        // Remove any broken / old snapshot files:
        let current_file_name = format!("{SNAPSHOTS_PREFIX}{gen_loaded}");
        for file in snapshot_files {
          if current_file_name != file {
            IOUtils::delete_files_ignoring_exceptions(self.dir.as_ref(), std::iter::once(&file));
          }
        }
      }
      self.next_write_gen.store(1 + gen_loaded, Ordering::Relaxed);
    }

    Ok(())
  }
}

impl<D> IndexDeletionPolicy<Arc<CommitPoint<D>>> for PersistentSnapshotDeletionPolicy<D>
where
  D: Directory,
{
  fn on_init(&self, commits: &[Arc<CommitPoint<D>>]) -> Result<()> {
    self.base.on_init(commits)
  }

  fn on_commit(&self, commits: &[Arc<CommitPoint<D>>]) -> Result<()> {
    self.base.on_commit(commits)
  }
}

impl<D> Display for PersistentSnapshotDeletionPolicy<D> {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}
