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
use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

use parking_lot::{ReentrantMutex, ReentrantMutexGuard};
use smallvec::SmallVec;

use crate::core::index::index_commit::{IndexCommit, cmp_commit, is_same_commit};
use crate::core::index::index_deletion_policy::{IndexDeletionPolicy, IndexDeletionPolicyEnum};
use crate::core::index::index_file_deleter::CommitPoint;
use crate::core::store::directory::Directory;
use crate::core::util::error::lucene_error::{LuceneError, Result};

const MISUSE_MESSAGE: &str = "this instance is not being used by IndexWriter; be sure to use the instance returned from writer.get_config().get_index_deletion_policy()";

/// An [`IndexDeletionPolicy`] that wraps any other [`IndexDeletionPolicy`] and adds the ability to
/// hold and later release snapshots of an index. While a snapshot is held, the
/// [`IndexWriter`](crate::core::index::index_writer::IndexWriter) will not remove any files
/// associated with it even if the index is otherwise being actively, arbitrarily changed. Because we
/// wrap another arbitrary [`IndexDeletionPolicy`], this gives you the freedom to continue using
/// whatever [`IndexDeletionPolicy`] you would normally want to use with your index.
///
/// This struct maintains all snapshots in-memory, and so the information is not persisted and not
/// protected against system failures. If persistence is important, you can use
/// [`PersistentSnapshotDeletionPolicy`](crate::core::index::persistent_snapshot_deletion_policy::PersistentSnapshotDeletionPolicy).
///
/// # Experimental
pub struct SnapshotDeletionPolicy<D> {
  shared: Arc<SnapshotPolicyShared<D>>,
}

struct SnapshotPolicyShared<D> {
  primary: IndexDeletionPolicyEnum<D>,
  // The policy and its commit wrappers share the same reentrant monitor.
  inner: ReentrantMutex<RefCell<Inner<D>>>,
}

pub(crate) struct Inner<D> {
  /// Records how many snapshots are held against each commit generation.
  pub(crate) ref_counts: HashMap<i64, i32>,

  /// Used to map gen to IndexCommit.
  pub(crate) index_commits: HashMap<i64, Arc<CommitPoint<D>>>,

  /// Most recently committed IndexCommit.
  last_commit: Option<Arc<CommitPoint<D>>>,

  /// Used to detect misuse.
  init_called: bool,
}

impl<D> Clone for SnapshotDeletionPolicy<D> {
  fn clone(&self) -> Self {
    Self {
      shared: self.shared.clone(),
    }
  }
}

impl<D> SnapshotDeletionPolicy<D>
where
  D: Directory,
{
  /// Sole constructor, taking the incoming [`IndexDeletionPolicy`] to wrap.
  pub fn new<T>(primary: T) -> Self
  where
    T: Into<IndexDeletionPolicyEnum<D>>,
  {
    SnapshotDeletionPolicy {
      shared: Arc::new(SnapshotPolicyShared {
        primary: primary.into(),
        inner: ReentrantMutex::new(RefCell::new(Inner {
          ref_counts: HashMap::new(),
          index_commits: HashMap::new(),
          last_commit: None,
          init_called: false,
        })),
      }),
    }
  }

  pub(crate) fn lock(&self) -> ReentrantMutexGuard<'_, RefCell<Inner<D>>> {
    self.shared.inner.lock()
  }

  /// Release a snapshotted commit.
  ///
  /// # Parameters
  ///
  /// * `commit` - the commit previously returned by [`Self::snapshot`].
  pub fn release(&self, commit: &Arc<CommitPoint<D>>) -> Result<()> {
    let _guard = self.lock();
    self.release_gen(commit.get_generation())
  }

  /// Release a snapshot by generation.
  pub fn release_gen(&self, generation: i64) -> Result<()> {
    let guard = self.lock();
    let mut inner = guard.borrow_mut();
    if !inner.init_called {
      return Err(LuceneError::illegal_state(MISUSE_MESSAGE));
    }
    let ref_count = inner.ref_counts.get_mut(&generation).ok_or_else(|| {
      LuceneError::illegal_argument(format!(
        "commit gen={generation} is not currently snapshotted"
      ))
    })?;
    debug_assert!(*ref_count > 0);
    *ref_count -= 1;
    if *ref_count == 0 {
      inner.ref_counts.remove(&generation);
      inner.index_commits.remove(&generation);
    }
    Ok(())
  }

  /// Increments the refCount for this [`IndexCommit`].
  pub(crate) fn inc_ref(&self, ic: &Arc<CommitPoint<D>>) {
    let guard = self.lock();
    let mut inner = guard.borrow_mut();
    let generation = ic.get_generation();
    let ref_count = inner.ref_counts.get(&generation).copied().unwrap_or(0);
    if ref_count == 0 {
      inner.index_commits.insert(generation, ic.clone());
    }
    inner.ref_counts.insert(generation, ref_count + 1);
  }

  /// Snapshots the last commit and returns it. Once a commit is 'snapshotted,' it is protected from
  /// deletion (as long as this [`IndexDeletionPolicy`] is used). The snapshot can be removed by
  /// calling [`Self::release`] followed by a call to
  /// [`IndexWriter::delete_unused_files`](crate::core::index::index_writer::IndexWriter::delete_unused_files).
  ///
  /// **NOTE:** while the snapshot is held, the files it references will not be deleted, which will
  /// consume additional disk space in your index. If you take a snapshot at a particularly bad time
  /// (say just before you call forceMerge) then in the worst case this could consume an extra 1X of
  /// your total index size, until you release the snapshot.
  ///
  /// # Errors
  ///
  /// Returns an [`IllegalState`](crate::core::util::error::lucene_error::LuceneErrorKind::IllegalState) error if this index does not have any
  /// commits yet.
  pub fn snapshot(&self) -> Result<Arc<CommitPoint<D>>> {
    let guard = self.lock();
    let inner = guard.borrow();
    if !inner.init_called {
      return Err(LuceneError::illegal_state(MISUSE_MESSAGE));
    }
    let last_commit = inner
      .last_commit
      .clone()
      .ok_or_else(|| LuceneError::illegal_state("No index commit to snapshot"))?;

    drop(inner);
    self.inc_ref(&last_commit);

    Ok(last_commit)
  }

  /// Returns all IndexCommits held by at least one snapshot.
  pub fn get_snapshots(&self) -> Vec<Arc<CommitPoint<D>>> {
    let guard = self.lock();
    guard.borrow().index_commits.values().cloned().collect()
  }

  /// Returns the total number of snapshots currently held.
  pub fn get_snapshot_count(&self) -> i32 {
    self.lock().borrow().ref_counts.values().sum()
  }

  /// Retrieve an [`IndexCommit`] from its generation; returns `None` if this commit is not
  /// currently snapshotted.
  pub fn get_index_commit(&self, generation: i64) -> Option<Arc<CommitPoint<D>>> {
    self.lock().borrow().index_commits.get(&generation).cloned()
  }

  /// Wraps each [`IndexCommit`] as a [`SnapshotCommitPoint`].
  fn wrap_commits<'a>(
    &'a self,
    commits: &'a [Arc<CommitPoint<D>>],
  ) -> SmallVec<[SnapshotCommitPoint<'a, D>; 2]> {
    let mut wrapped_commits = SmallVec::with_capacity(commits.len());
    for ic in commits {
      wrapped_commits.push(SnapshotCommitPoint::new(&self.shared.inner, ic));
    }
    wrapped_commits
  }
}
impl<D> IndexDeletionPolicy<Arc<CommitPoint<D>>> for SnapshotDeletionPolicy<D>
where
  D: Directory,
{
  fn on_init(&self, commits: &[Arc<CommitPoint<D>>]) -> Result<()> {
    let guard = self.lock();
    let wrapped_commits = self.wrap_commits(commits);
    {
      let mut inner = guard.borrow_mut();
      inner.init_called = true;
    }
    // Keep the monitor held, but release the mutable borrow before delete() reenters it.
    self.shared.primary.on_init(&wrapped_commits)?;
    let mut inner = guard.borrow_mut();
    for commit in commits {
      if inner.ref_counts.contains_key(&commit.get_generation()) {
        inner
          .index_commits
          .insert(commit.get_generation(), commit.clone());
      }
    }
    if let Some(last_commit) = commits.last() {
      inner.last_commit = Some(last_commit.clone());
    }
    Ok(())
  }

  fn on_commit(&self, commits: &[Arc<CommitPoint<D>>]) -> Result<()> {
    let guard = self.lock();
    let wrapped_commits = self.wrap_commits(commits);
    self.shared.primary.on_commit(&wrapped_commits)?;
    let mut inner = guard.borrow_mut();
    inner.last_commit = commits.last().cloned();
    Ok(())
  }
}

impl<D> Display for SnapshotDeletionPolicy<D> {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

/// Wraps a provided [`IndexCommit`] and prevents it from being deleted.
pub struct SnapshotCommitPoint<'a, D> {
  /// The [`IndexCommit`] we are preventing from deletion.
  cp: &'a CommitPoint<D>,
  snapshot_policy: &'a ReentrantMutex<RefCell<Inner<D>>>,
}

impl<'a, D> SnapshotCommitPoint<'a, D> {
  /// Creates a [`SnapshotCommitPoint`] wrapping the provided [`IndexCommit`].
  fn new(policy: &'a ReentrantMutex<RefCell<Inner<D>>>, cp: &'a CommitPoint<D>) -> Self {
    SnapshotCommitPoint {
      cp,
      snapshot_policy: policy,
    }
  }
}

impl<D> Clone for SnapshotCommitPoint<'_, D> {
  fn clone(&self) -> Self {
    SnapshotCommitPoint {
      cp: self.cp,
      snapshot_policy: self.snapshot_policy,
    }
  }
}

impl<D> PartialEq for SnapshotCommitPoint<'_, D>
where
  D: Directory,
{
  fn eq(&self, other: &Self) -> bool {
    is_same_commit(self, other)
  }
}

impl<D> Eq for SnapshotCommitPoint<'_, D> where D: Directory {}

impl<D> PartialOrd for SnapshotCommitPoint<'_, D>
where
  D: Directory,
{
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

impl<D> Ord for SnapshotCommitPoint<'_, D>
where
  D: Directory,
{
  fn cmp(&self, other: &Self) -> Ordering {
    cmp_commit(self, other)
  }
}

impl<D> Display for SnapshotCommitPoint<'_, D> {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "SnapshotDeletionPolicy.SnapshotCommitPoint({})", self.cp)
  }
}

impl<D> IndexCommit for SnapshotCommitPoint<'_, D>
where
  D: Directory,
{
  fn get_segments_file_name(&self) -> &str {
    self.cp.get_segments_file_name()
  }

  fn get_file_names(&self) -> Result<&[String]> {
    self.cp.get_file_names()
  }

  type Directory = Arc<D>;

  fn get_directory(&self) -> Self::Directory {
    self.cp.get_directory()
  }

  fn delete(&self) -> Result<()> {
    let guard = self.snapshot_policy.lock();
    let inner = guard.borrow();
    // Suppress the delete request if this commit point is
    // currently snapshotted.
    if !inner.ref_counts.contains_key(&self.cp.get_generation()) {
      self.cp.delete()?;
    }
    Ok(())
  }

  fn is_deleted(&self) -> bool {
    self.cp.is_deleted()
  }

  fn get_segment_count(&self) -> usize {
    self.cp.get_segment_count()
  }

  fn get_generation(&self) -> i64 {
    self.cp.get_generation()
  }

  fn get_user_data(&self) -> &HashMap<String, String> {
    self.cp.get_user_data()
  }
}
