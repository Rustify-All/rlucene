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
use crate::core::index::index_commit::{IndexCommit, cmp_commit, is_same_commit};
use crate::core::index::index_deletion_policy::IndexDeletionPolicy;
use crate::core::index::index_writer::{IndexWriter, IndexWriterDir};
use crate::core::index::segment_infos::SegmentInfos;
use crate::core::index::{CODEC_FILE_PATTERN, IndexFileNames};
use crate::core::store::directory::Directory;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::file_deleter::{FileDeleter, Messenger, MsgType};
use crate::core::util::info_stream::{InfoStream, InfoStreamMT};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::SeqCst;

/// This struct keeps track of each `SegmentInfos` instance that is still "live", either because it
/// corresponds to a `segments_N` file in the `Directory` (a "commit", i.e. a committed
/// `SegmentInfos`) or because it's an in-memory `SegmentInfos` that a writer is actively
/// updating but has not yet committed. This struct uses simple reference counting to map the live
/// `SegmentInfos` instances to individual files in the `Directory`.
///
/// The same directory file may be referenced by more than one `IndexCommit`, i.e. more than one
/// `SegmentInfos`. Therefore we count how many commits reference each file. When all the commits
/// referencing a certain file have been deleted, the refcount for that file becomes zero, and the
/// file is deleted.
///
/// A separate deletion policy trait (`IndexDeletionPolicy`) is consulted on creation.
/// (`on_init`) and once per commit (`on_commit`), to decide when a commit should be removed.
///
/// It is the business of the `IndexDeletionPolicy` to choose when to delete commit points. The
/// actual mechanics of file deletion, retrying, etc., derived from the deletion of commit points is
/// the business of the `IndexFileDeleter`.
///
/// The current default deletion policy is [`KeepOnlyLastCommitDeletionPolicy`](crate::core::index::keep_only_last_commit_deletion_policy::KeepOnlyLastCommitDeletionPolicy), which removes all
/// prior commits when a new commit has completed. This matches the behavior before 2.2.
///
/// Note that you must hold the `write.lock` before instantiating This struct. It opens `segments_N`
/// file(s) directly with no retry logic.
pub struct IndexFileDeleter<D>
where
  D: Directory,
{
  /// Holds all commits (segments_N) currently in the index.
  /// This will have just 1 commit if you are using the
  /// default delete policy (KeepOnlyLastCommitDeletionPolicy).
  /// Other policies may leave commit points live for longer
  /// in which case this list would be longer than 1.
  commits: Vec<Arc<CommitPoint<D>>>,

  /// Holds files we had incref'd from the previous non-commit checkpoint.
  last_files: Vec<String>,

  /// Commits that the IndexDeletionPolicy have decided to delete.
  commits_to_delete: Arc<AtomicBool>,

  info_stream: InfoStreamMT,
  directory_orig: Arc<D>,
  directory: Arc<IndexWriterDir<D>>,

  /// Whether the starting commit was deleted.
  pub(crate) starting_commit_deleted: bool,

  verbose_ref_counts: bool,
  file_deleter: FileDeleter<D, MessengerImpl>,
}
impl<D> IndexFileDeleter<D>
where
  D: Directory,
{
  /// Initialize the deleter: find all previous commits in the Directory, incref the files they reference, call the policy to let it delete commits.
  /// This will remove any files not referenced by any of the commits.
  #[allow(clippy::too_many_arguments)]
  pub fn new<P>(
    files: impl IntoIterator<Item = String>,
    directory_orig: Arc<D>,
    directory: Arc<IndexWriterDir<D>>,
    policy: &P,
    segment_infos: &mut SegmentInfos<D>,
    info_stream: InfoStreamMT,
    initial_index_exists: bool,
    is_reader_init: bool,
  ) -> Result<Self>
  where
    P: IndexDeletionPolicy<Arc<CommitPoint<D>>>,
  {
    // init fields
    let commits = Vec::new();
    let mut last_segment_infos: Option<SegmentInfos<D>> = None;

    let current_segments_file_opt = segment_infos.get_segments_file_name();
    if info_stream.is_enabled("IFD") {
      info_stream.message(
                "IFD",
                &format!(
                    "init: current segments file is \"{current_segments_file_opt:?}\"; deletionPolicy={policy}"
                ),
            )?;
    }

    // create file_deleter
    let file_deleter = FileDeleter::new(
      directory.clone(),
      Some(MessengerImpl::new(info_stream.clone(), false)),
    );

    let mut index_file_deleter = Self {
      commits,
      last_files: Vec::new(),
      commits_to_delete: Arc::new(AtomicBool::new(false)),
      info_stream,
      directory_orig: directory_orig.clone(),
      directory,
      starting_commit_deleted: !initial_index_exists,
      verbose_ref_counts: false,
      file_deleter,
    };
    let mut current_commit_point = None;
    if current_segments_file_opt.is_some() {
      let current_gen = segment_infos.get_generation();
      for file in files {
        if file.ends_with("write.lock") {
          continue;
        }
        let is_segments = file.starts_with(IndexFileNames::SEGMENTS);
        if CODEC_FILE_PATTERN.is_match(&file)
          || is_segments
          || file.starts_with(IndexFileNames::PENDING_SEGMENTS)
        {
          // Add this file to refCounts with initial count 0:
          index_file_deleter.file_deleter.init_ref_count(&file);
          if is_segments {
            // This is a commit (segments or segments_N), and
            // it's valid (<= the max gen).  Load it, then
            // incref all files it refers to:
            if index_file_deleter.info_stream.is_enabled("IFD") {
              index_file_deleter
                .info_stream
                .message("IFD", &format!("init: load commit \"{file}\""))?;
            }
            let sis = SegmentInfos::read_commit(directory_orig.clone(), &file)?;
            let commit_point = Arc::new(CommitPoint::new(
              index_file_deleter.commits_to_delete.clone(),
              directory_orig.clone(),
              &sis,
            )?);
            if sis.get_generation() == current_gen {
              current_commit_point = Some(Arc::clone(&commit_point));
            }
            index_file_deleter.commits.push(commit_point);
            index_file_deleter.inc_ref_from_segment(&sis, true)?;

            if last_segment_infos.is_none()
              || sis.get_generation() > last_segment_infos.as_ref().unwrap().get_generation()
            {
              last_segment_infos = Some(sis);
            }
          }
        }
      }
    }
    if let Some(file) = current_segments_file_opt
      && current_commit_point.is_none()
      && initial_index_exists
    {
      // We did not in fact see the segments_N file
      // corresponding to the segmentInfos that was passed
      // in.  Yet, it must exist, because our caller holds
      // the write lock.  This can happen when the directory
      // listing was stale (eg when index accessed via NFS
      // client with stale directory listing cache).  So we
      // try now to explicitly open this commit point:
      let sis = SegmentInfos::read_commit(directory_orig.clone(), &file);
      let sis = sis.map_err(|e| {
        let mut error = LuceneError::corrupt_index(format!(
          "unable to read current segments_N file (resource={file})"
        ));
        error.add_suppressed(e);
        error
      })?;
      if index_file_deleter.info_stream.is_enabled("IFD") {
        index_file_deleter.info_stream.message(
          "IFD",
          &format!(
            "forced open of current segments file {:?}",
            segment_infos.get_segments_file_name()
          ),
        )?;
      }
      let commit_point = Arc::new(CommitPoint::new(
        index_file_deleter.commits_to_delete.clone(),
        directory_orig.clone(),
        &sis,
      )?);
      current_commit_point = Some(Arc::clone(&commit_point));
      index_file_deleter.commits.push(commit_point);
      index_file_deleter.inc_ref_from_segment(&sis, true)?;
    }

    if is_reader_init {
      // Incoming SegmentInfos may have NRT changes not yet visible in the latest commit, so we have
      // to protect its files from deletion too:
      index_file_deleter.checkpoint(segment_infos, false, policy)?;
    }

    // keep commits sorted by generation
    index_file_deleter.commits.sort_unstable();

    let pending = directory_orig.get_pending_deletions()?;
    let relevant_files = index_file_deleter.file_deleter.get_all_files();
    if !pending.is_empty() {
      let relevant_files = relevant_files.chain(pending.iter());
      inflate_gens(
        segment_infos,
        relevant_files,
        &index_file_deleter.info_stream,
      )?;
    } else {
      inflate_gens(
        segment_infos,
        relevant_files,
        &index_file_deleter.info_stream,
      )?;
    }

    // inflate gens and delete abandoned files
    let unrefed = index_file_deleter.file_deleter.get_unrefed_files()?;
    for file in &unrefed {
      if file.starts_with(IndexFileNames::SEGMENTS) {
        return Err(LuceneError::illegal_state(
          "file \"{file}\" has refCount=0, which should never happen on init",
        ));
      }
      if index_file_deleter.info_stream.is_enabled("IFD") {
        index_file_deleter.info_stream.message(
          "IFD",
          &format!("init: removing unreferenced file \"{file}\""),
        )?;
      }
    }
    index_file_deleter
      .file_deleter
      .delete_files_if_no_ref(&unrefed)?;
    // Finally, give policy a chance to remove things on
    // startup:
    policy.on_init(&index_file_deleter.commits)?;
    // Always protect the incoming segmentInfos since
    // sometime it may not be the most recent commit
    index_file_deleter.checkpoint(segment_infos, false, policy)?;

    index_file_deleter.starting_commit_deleted = match current_commit_point {
      Some(commit) => commit.is_deleted(),
      None => false,
    };

    index_file_deleter.delete_commits()?;
    Ok(index_file_deleter)
  }
  fn ensure_open(&self, index_writer: &IndexWriter<D>) -> Result<()> {
    index_writer.do_ensure_open(false)?;

    let tragic_arc = index_writer.get_tragic_exception();
    let error = tragic_arc.get();
    if let Some(e) = error {
      let mut error = LuceneError::already_closed(
        "refusing to delete any files: this IndexWriter hit an unrecoverable exception",
      );
      error.add_suppressed(e.clone());
      return Err(error);
    }

    Ok(())
  }
  pub(crate) fn is_closed(&self, index_writer: &IndexWriter<D>) -> Result<bool> {
    match self.ensure_open(index_writer) {
      Ok(_) => Ok(false),
      Err(e) => {
        if matches!(e, LuceneError::AlreadyClosed(_)) {
          Ok(true)
        } else {
          Err(e)
        }
      },
    }
  }
  /// Remove the CommitPoints in the commitsToDelete List by DecRef'ing all files from each SegmentInfos.
  fn delete_commits(&mut self) -> Result<()> {
    if !self.commits_to_delete.load(SeqCst) {
      return Ok(());
    }
    let removed = self
      .commits
      .iter()
      .filter(|commit| commit.is_deleted())
      .cloned()
      .collect::<Vec<_>>();

    // First decref all files that had been referred to by
    // the now-deleted commits:
    let mut errors = Vec::new();
    let mut first_panic = None;
    for commit in removed {
      if self.info_stream.is_enabled("IFD") {
        self.info_stream.message(
          "IFD",
          &format!(
            "deleteCommits: now decRef commit \"{}\"",
            commit.get_segments_file_name()
          ),
        )?;
      }
      match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        self.dec_ref(commit.files.iter())
      })) {
        Ok(Ok(())) => {},
        Ok(Err(error)) if first_panic.is_none() => {
          errors.push(error);
        },
        Ok(Err(_)) => {},
        Err(payload) if !errors.is_empty() => {
          errors.push(LuceneError::tragedy_from_panic(
            "panic while decrementing file references",
            payload.as_ref(),
          ));
        },
        Err(payload) if first_panic.is_none() => first_panic = Some(payload),
        Err(_) => {},
      }
    }
    self.commits_to_delete.store(false, SeqCst);

    // Now compact commits to remove deleted ones (preserving the sort):
    self.commits.retain(|commit| !commit.is_deleted());

    if let Some(payload) = first_panic {
      std::panic::resume_unwind(payload);
    }
    let mut first_error = None;
    for mut error in errors.into_iter().rev() {
      if let Some(suppressed) = first_error {
        error.add_suppressed(suppressed);
      }
      first_error = Some(error);
    }
    match first_error {
      Some(error) => Err(error),
      None => Ok(()),
    }
  }

  /// Writer calls this when it has hit an error and had to roll back, to tell us that there may now be unreferenced files in the filesystem.
  /// So we re-list the filesystem and delete such files.
  /// If segmentName is present, we will only delete files corresponding to that segment.
  pub(crate) fn refresh(&self) -> Result<()> {
    // debug_assert!(self.locked());
    let mut to_delete = HashSet::new();

    let files = self.directory.list_all()?;

    for file_name in files {
      let is_lock_file = file_name.ends_with("write.lock");
      let is_codec_match = CODEC_FILE_PATTERN.is_match(&file_name);
      let is_segments = file_name.starts_with(IndexFileNames::SEGMENTS);
      let is_pending_segments = file_name.starts_with(IndexFileNames::PENDING_SEGMENTS);

      // we only try to clear out pending_segments_N during rollback(), because we don't
      // ref-count it
      // TODO: this is sneaky, should we do this, or change TestIWExceptions? rollback
      // closes anyway, and
      // any leftover file will be deleted/retried on next IW bootup anyway...
      if !is_lock_file
        && !self.file_deleter.exists(&file_name)
        && (is_codec_match || is_segments || is_pending_segments)
      {
        if self.info_stream.is_enabled("IFD") {
          self.info_stream.message(
            "IFD",
            &format!("refresh: removing newly created unreferenced file \"{file_name}\""),
          )?;
        }
        to_delete.insert(file_name);
      }
    }

    self.file_deleter.delete_files_if_no_ref(&to_delete)
  }
  pub fn close(&mut self) -> Result<()> {
    if !self.last_files.is_empty() {
      let files = std::mem::take(&mut self.last_files);
      self.dec_ref(files.iter())?;
    }
    Ok(())
  }
  fn assert_commits_are_not_deleted(&self, commits: &[Arc<CommitPoint<D>>]) -> bool {
    for commit in commits {
      debug_assert!(
        !commit.is_deleted(),
        "Commit [{commit}] was deleted already"
      );
    }
    true
  }
  /// Revisits the `IndexDeletionPolicy` by calling its [`IndexDeletionPolicy::on_commit()`] again with the known commits.
  /// This is useful when using a deletion policy that holds onto index commits.
  /// The application may know that some commits are no longer held by the policy and call `IndexWriter::delete_unused_files()`,
  /// which will attempt to delete those unused commits again.
  pub(crate) fn revisit_policy<P>(&mut self, policy: &P) -> Result<()>
  where
    P: IndexDeletionPolicy<Arc<CommitPoint<D>>>,
  {
    {
      if self.info_stream.is_enabled("IFD") {
        self.info_stream.message("IFD", "now revisitPolicy")?;
      }
    }

    if !self.commits.is_empty() {
      debug_assert!(self.assert_commits_are_not_deleted(&self.commits));
      policy.on_commit(&self.commits)?;
      self.delete_commits()?;
    }

    Ok(())
  }
  /// For definition of “check point” see `IndexWriter` comments: “Clarification: Check Points (and commits)”.
  ///
  /// Writer calls this when it has made a “consistent change” to the index, meaning new files are
  /// written to the index and the in-memory `SegmentInfos` have been modified to point to those files.
  ///
  /// This may or may not be a commit (`segments_N` may or may not have been written).
  ///
  /// We simply incref the files referenced by the new `SegmentInfos` and decref the files we had
  /// previously seen (if any).
  ///
  /// If this is a commit, we also call the policy to give it a chance to remove other commits. If
  /// any commits are removed, we decref their files as well.
  pub fn checkpoint<P>(
    &mut self,
    segment_infos: &SegmentInfos<D>,
    is_commit: bool,
    policy: &P,
  ) -> Result<()>
  where
    P: IndexDeletionPolicy<Arc<CommitPoint<D>>>,
  {
    // In Java Lucene, this method should be called while synchronized on IndexWriter instance.
    // In Rust Lucene, IndexFileDeleter under IndexWriter's Inner Mutex, So it is similar to Java Lucene's `assert Thread.holdsLock(IndexWriter);`
    let t0 = std::time::Instant::now();

    {
      if self.info_stream.is_enabled("IFD") {
        // TODO:
      }
    }
    // Incref the files:
    self.inc_ref_from_segment(segment_infos, is_commit)?;

    if is_commit {
      // Append to our commits list:
      self.commits.push(Arc::new(CommitPoint::new(
        Arc::clone(&self.commits_to_delete),
        Arc::clone(&self.directory_orig),
        segment_infos,
      )?));

      debug_assert!(self.assert_commits_are_not_deleted(&self.commits));
      policy.on_commit(&self.commits)?;
      // Decref files for commits that were deleted by the policy:
      self.delete_commits()?;
    } else {
      // DecRef old files from the last checkpoint, if any:
      let files = std::mem::take(&mut self.last_files);
      self.dec_ref(files.iter())?;
      // Save files so we can decr on next checkpoint/commit:
      self.last_files = segment_infos.files(false)?.into_iter().collect();
    }

    {
      if self.info_stream.is_enabled("IFD") {
        let elapsed_ms = t0.elapsed().as_millis();
        self
          .info_stream
          .message("IFD", &format!("{elapsed_ms} ms to checkpoint"))?;
      }
    }

    Ok(())
  }
  pub fn inc_ref_from_segment(
    &mut self,
    segment_infos: &SegmentInfos<D>,
    is_commit: bool,
  ) -> Result<()> {
    for file_name in segment_infos.files(is_commit)? {
      self.file_deleter.inc_ref_single(&file_name)?;
    }

    Ok(())
  }

  pub fn inc_ref_files<I, S>(&mut self, files: I) -> Result<()>
  where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
  {
    self.file_deleter.inc_ref(files)
  }

  /// Decrefs all provided files, even on error; returns first error hit, if any.
  pub(crate) fn dec_ref<'a, I>(&mut self, files: I) -> Result<()>
  where
    I: IntoIterator<Item = &'a String>,
  {
    self.file_deleter.dec_ref(files)
  }
  pub(crate) fn dec_ref_from_segment(&mut self, segment_infos: &SegmentInfos<D>) -> Result<()> {
    let files = segment_infos.files(false)?;
    self.dec_ref(files.iter())
  }
  pub fn exists(&self, file_name: &str) -> bool {
    self.file_deleter.exists(file_name)
  }
  /// Deletes the specified files, but only if they are new (have not yet been incref'd)
  pub(crate) fn delete_new_files<'a>(
    &self,
    files: impl IntoIterator<Item = &'a String>,
  ) -> Result<()> {
    self.file_deleter.delete_files_if_no_ref(files)
  }
}
impl<D> Drop for IndexFileDeleter<D>
where
  D: Directory,
{
  fn drop(&mut self) {
    let v = self.close();
    match v {
      Ok(_) => {},
      Err(e) => {
        if self.info_stream.is_enabled("IFD") {
          self
            .info_stream
            .message("IFD", &format!("Error closing IndexFileDeleter: {e}"))
            .unwrap_or_default();
        }
      },
    }
  }
}
/// Holds details for each commit point. This struct is also passed to the deletion policy.
/// Note: This struct has a natural ordering that is inconsistent with equals.
pub struct CommitPoint<D> {
  pub(crate) files: Vec<String>,
  pub(crate) segments_file_name: String,
  pub(crate) deleted: AtomicBool,
  pub(crate) directory_orig: Arc<D>,
  pub(crate) generation: i64,
  pub(crate) user_data: HashMap<String, String>,
  pub(crate) segment_count: usize,
  pub(crate) commits_to_delete: Arc<AtomicBool>,
}
impl<D> CommitPoint<D>
where
  D: Directory,
{
  pub(crate) fn new(
    commits_to_delete: Arc<AtomicBool>,
    directory_orig: Arc<D>,
    segment_infos: &SegmentInfos<D>,
  ) -> Result<Self> {
    // TODO：是不是只要保存segment的ID就行,避免一些拷贝
    let user_data = segment_infos.get_user_data().clone();
    let segments_file_name = segment_infos
      .get_segments_file_name()
      .ok_or_else(|| LuceneError::illegal_state("segment_N file is none"))?;
    let generation = segment_infos.get_generation();
    let files = segment_infos.files(true)?.into_iter().collect();
    let segment_count = segment_infos.size();

    Ok(CommitPoint {
      files,
      segments_file_name,
      deleted: AtomicBool::new(false),
      directory_orig,
      generation,
      user_data,
      segment_count,
      commits_to_delete,
    })
  }
}

impl<D> PartialEq for CommitPoint<D>
where
  D: Directory,
{
  fn eq(&self, other: &Self) -> bool {
    is_same_commit(self, other)
  }
}

impl<D> Eq for CommitPoint<D> where D: Directory {}

impl<D> PartialOrd for CommitPoint<D>
where
  D: Directory,
{
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

impl<D> Ord for CommitPoint<D>
where
  D: Directory,
{
  fn cmp(&self, other: &Self) -> Ordering {
    cmp_commit(self, other)
  }
}

impl<D> Display for CommitPoint<D>
where
  D: Directory,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "{}({})",
      std::any::type_name::<Self>(),
      self.segments_file_name
    )
  }
}

impl<D> IndexCommit for CommitPoint<D>
where
  D: Directory,
{
  fn get_segments_file_name(&self) -> &str {
    &self.segments_file_name
  }

  fn get_file_names(&self) -> Result<&[String]> {
    Ok(self.files.as_slice())
  }

  type Directory = Arc<D>;

  fn get_directory(&self) -> Self::Directory {
    self.directory_orig.clone()
  }

  /// Called only be the deletion policy, to remove this commit point from the index.
  fn delete(&self) -> Result<()> {
    if !self.deleted.swap(true, SeqCst) {
      self
        .commits_to_delete
        .store(true, std::sync::atomic::Ordering::SeqCst);
    }
    Ok(())
  }

  fn is_deleted(&self) -> bool {
    self.deleted.load(SeqCst)
  }

  fn get_segment_count(&self) -> usize {
    self.segment_count
  }

  fn get_generation(&self) -> i64 {
    self.generation
  }

  fn get_user_data(&self) -> &HashMap<String, String> {
    &self.user_data
  }
}

pub(crate) struct MessengerImpl {
  info_stream: InfoStreamMT,
  verbose_ref_counts: bool,
}
impl MessengerImpl {
  pub(crate) fn new(info_stream: InfoStreamMT, verbose_ref_counts: bool) -> Self {
    MessengerImpl {
      info_stream,
      verbose_ref_counts,
    }
  }
}
impl Messenger for MessengerImpl {
  fn accept(&self, msg_type: MsgType, msg: &str) -> Result<()> {
    if msg_type == MsgType::Ref && !self.verbose_ref_counts {
      return Ok(());
    }
    if self.info_stream.is_enabled("IFD") {
      self.info_stream.message("IFD", msg)?;
    }
    Ok(())
  }
}

use crate::core::index::index_writer::WRITE_LOCK_NAME;
use crate::core::index::segment_infos::generation_from_segments_file_name;

/// Set all gens beyond what we currently see in the directory, to avoid double-write in cases
/// where the previous `IndexWriter` did not gracefully close/rollback (e.g. OS/machine crashed or
/// lost power).
pub(crate) fn inflate_gens<'a, D, I>(
  infos: &mut SegmentInfos<D>,
  files: I,
  info_stream: &InfoStreamMT,
) -> Result<()>
where
  D: Directory,
  I: IntoIterator<Item = &'a String>,
{
  let mut max_segment_gen = i64::MIN;
  let mut max_segment_name = i64::MIN;
  // Confusingly, this is the union of liveDocs, field infos, doc values
  // (and maybe others, in the future) gens.  This is somewhat messy,
  // since it means DV updates will suddenly write to the next gen after
  // live docs' gen, for example, but we don't have the APIs to ask the
  // codec which file is which:
  let mut max_per_segment_gen = HashMap::new();

  for file_name in files {
    if file_name == WRITE_LOCK_NAME {
      continue;
    } else if file_name.starts_with(IndexFileNames::SEGMENTS) {
      let v = generation_from_segments_file_name(file_name);
      match v {
        Ok(gen_) => {
          max_segment_gen = max_segment_gen.max(gen_);
        },
        Err(e) => {
          // trash file: we have to handle this since we allow anything starting with 'segments'
          // here
          if !matches!(e, LuceneError::NumberFormat(_)) {
            return Err(e);
          }
        },
      }
    } else if file_name.starts_with(IndexFileNames::PENDING_SEGMENTS) {
      let v = generation_from_segments_file_name(&file_name[8..]);
      match v {
        Ok(gen_) => {
          max_segment_gen = max_segment_gen.max(gen_);
        },
        Err(e) => {
          // trash file: we have to handle this since we allow anything starting with
          // 'pending_segments' here
          if !matches!(e, LuceneError::NumberFormat(_)) {
            return Err(e);
          }
        },
      }
    } else {
      let segment_name = IndexFileNames::parse_segment_name(file_name);
      debug_assert!(segment_name.starts_with('_'), "wtf? file={file_name}");
      if file_name.to_lowercase().ends_with(".tmp") {
        // A temp file: don't try to look at its gen
        continue;
      }
      max_segment_name = max_segment_name.max(i64::from_str_radix(&segment_name[1..], 36)?);

      let mut cur_gen = *max_per_segment_gen.get(segment_name).unwrap_or(&0i64);

      let v = IndexFileNames::parse_generation(file_name);
      match v {
        Ok(gen_) => {
          cur_gen = cur_gen.max(gen_);
        },
        Err(e) => {
          // trash file: we have to handle this since codec regex is only so good
          if !matches!(e, LuceneError::NumberFormat(_)) {
            return Err(e);
          }
        },
      }
      max_per_segment_gen.insert(segment_name.to_string(), cur_gen);
    }
  }

  // Generation is advanced before write:
  let next_gen = infos.get_generation().max(max_segment_gen);
  infos.set_next_write_generation(next_gen)?;

  let desired = 1 + max_segment_name;
  if infos.counter < desired {
    if info_stream.is_enabled("IFD") {
      info_stream.message(
        "IFD",
        &format!(
          "init: inflate infos.counter to {} vs current={}",
          desired, infos.counter
        ),
      )?;
    }
    infos.counter = desired;
  }
  for info in infos.iter_mut() {
    debug_assert!(max_per_segment_gen.contains_key(&info.info.name));
    let gen_long = *max_per_segment_gen.get(&info.info.name).unwrap();

    let next_del = info.get_next_write_del_gen();
    if next_del < gen_long + 1 {
      if info_stream.is_enabled("IFD") {
        info_stream.message(
          "IFD",
          &format!(
            "init: seg={} set nextWriteDelGen={} vs current={}",
            info.info.name,
            gen_long + 1,
            next_del
          ),
        )?;
      }
      info.set_next_write_del_gen(gen_long + 1);
    }

    let next_fi = info.get_next_write_field_infos_gen();
    if next_fi < gen_long + 1 {
      if info_stream.is_enabled("IFD") {
        info_stream.message(
          "IFD",
          &format!(
            "init: seg={} set nextWriteFieldInfosGen={} vs current={}",
            info.info.name,
            gen_long + 1,
            next_fi
          ),
        )?;
      }
      info.set_next_write_field_infos_gen(gen_long + 1);
    }

    let next_dv = info.get_next_write_doc_values_gen();
    if next_dv < gen_long + 1 {
      if info_stream.is_enabled("IFD") {
        info_stream.message(
          "IFD",
          &format!(
            "init: seg={} set nextWriteDocValuesGen={} vs current={}",
            info.info.name,
            gen_long + 1,
            next_dv
          ),
        )?;
      }
      info.set_next_write_doc_values_gen(gen_long + 1);
    }
  }
  Ok(())
}
