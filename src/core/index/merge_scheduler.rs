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
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::concurrent_merge_scheduler::ConcurrentMergeScheduler;
use crate::core::index::index_reader::Identity;
use crate::core::index::index_writer::Inner;
use crate::core::index::merge_policy::{MergeStat, OneMerge};
use crate::core::index::merge_rate_limiter::MergeRateLimiter;
use crate::core::index::merge_trigger::MergeTrigger;
use crate::core::index::no_merge_scheduler::NoMergeScheduler;
use crate::core::index::serial_merge_scheduler::SerialMergeScheduler;
use crate::core::store::IOContext;
use crate::core::store::buffered_checksum_index_input::BufferedChecksumIndexInput;
use crate::core::store::directory::Directory;
use crate::core::store::rate_limited_directory::{
  RateLimitedDirectory, RateLimitedIndexOutputEnum,
};
use crate::core::util::HasIdentity;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::info_stream::InfoStreamMT;
use crate::impl_from_for_enum;
#[cfg(test)]
use crate::test_framework::core::index::base_knn_vectors_format_test_case::TestMergeScheduler;
#[cfg(test)]
use crate::test_framework::core::index::base_merge_policy_test_case::SerialMergeSchedulerImpl;
#[cfg(test)]
use crate::test_framework::core::index::test_add_indexes::{
  CountingSerialMergeScheduler, PartialMergeScheduler,
};
#[cfg(test)]
use crate::test_framework::core::index::test_index_writer_merge_policy::LatchedSerialMergeScheduler;
#[cfg(test)]
use crate::test_framework::core::index::test_index_writer_merging::MyMergeScheduler;
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

/// Expert: [IndexWriter] uses an instance implementing this
/// trait to execute the merges selected by a [MergePolicy].
/// The default MergeScheduler is [ConcurrentMergeScheduler].
///
/// @lucene.experimental
pub trait MergeScheduler: CloseableRef {
  /// Run the merges provided by [MergeSource::get_next_merge()].
  ///
  /// * `merge_source` - the [IndexWriter] to obtain the merges from.
  /// * `trigger` - the [MergeTrigger] that caused this merge to happen
  fn merge<MS, D>(&self, merge_source: MS, trigger: MergeTrigger) -> Result<()>
  where
    MS: MergeSource<D> + Clone + 'static,
    D: Directory + 'static,
    OneMerge<D, MS::Reader>: Send + 'static;
  type Directory<D>: Directory
  where
    D: Directory;
  /// Wraps the incoming [Directory] so that we can
  /// merge-throttle it using [RateLimitedIndexOutput].
  fn wrap_for_merge<D>(&self, _in_: D) -> Result<Self::Directory<D>>
  where
    D: Directory;

  /// [IndexWriter] calls this on init.
  fn initialize<D>(&mut self, _info_stream: InfoStreamMT, _directory: &D) -> Result<()>
  where
    D: Directory,
  {
    Ok(())
  }
}

/// Provides access to new merges and executes the actual merge
pub trait MergeSource<D>: Send
where
  D: Directory,
{
  type Reader: CodecReader;

  /// The `MergeScheduler` calls this method to retrieve the next merge
  /// requested by the `MergePolicy`.
  fn get_next_merge(&self) -> Result<Option<OneMerge<D, Self::Reader>>>;

  /// Does finishing for a merge.
  fn on_merge_finished(&self, merge: &MergeStat, inner: Option<&mut Inner<D>>);

  /// Expert: returns true if there are merges waiting to be scheduled.
  fn has_pending_merges(&self, inner: Option<&mut Inner<D>>) -> Result<bool>;

  /// Merges the indicated segments, replacing them in the stack
  /// with a single segment.
  fn merge(&self, merge: OneMerge<D, Self::Reader>) -> Result<()>
  where
    D: 'static;
}

pub enum MergeSchedulerEnum {
  Serial(SerialMergeScheduler),
  No(NoMergeScheduler),
  Concurrent(ConcurrentMergeScheduler),
  #[cfg(test)]
  SerialTest(SerialMergeSchedulerImpl),
  #[cfg(test)]
  LatchedSerial(LatchedSerialMergeScheduler),
  #[cfg(test)]
  KnnMergeScheduler(TestMergeScheduler),
  #[cfg(test)]
  IndexWriterMerging(MyMergeScheduler),
  #[cfg(test)]
  PartialAddIndexes(PartialMergeScheduler),
  #[cfg(test)]
  CountingAddIndexes(CountingSerialMergeScheduler),
}
impl_from_for_enum!(
    MergeSchedulerEnum,
    SerialMergeScheduler => Serial,
    NoMergeScheduler => No,
    ConcurrentMergeScheduler => Concurrent,
);
impl Default for MergeSchedulerEnum {
  fn default() -> Self {
    Self::Concurrent(ConcurrentMergeScheduler::new())
  }
}

impl CloseableRef for MergeSchedulerEnum {
  fn close(&self) -> Result<()> {
    match self {
      MergeSchedulerEnum::Serial(s) => s.close(),
      MergeSchedulerEnum::No(n) => n.close(),
      MergeSchedulerEnum::Concurrent(c) => c.close(),
      #[cfg(test)]
      MergeSchedulerEnum::SerialTest(s) => s.close(),
      #[cfg(test)]
      MergeSchedulerEnum::LatchedSerial(s) => s.close(),
      #[cfg(test)]
      MergeSchedulerEnum::KnnMergeScheduler(s) => s.close(),
      #[cfg(test)]
      MergeSchedulerEnum::IndexWriterMerging(s) => s.close(),
      #[cfg(test)]
      MergeSchedulerEnum::PartialAddIndexes(s) => s.close(),
      #[cfg(test)]
      MergeSchedulerEnum::CountingAddIndexes(s) => s.close(),
    }
  }
}

pub enum MergeSchedulerDirectory<D>
where
  D: Directory,
{
  Direct(D),
  RateLimited(RateLimitedDirectory<D, Arc<MergeRateLimiter>>),
}

impl<D> Display for MergeSchedulerDirectory<D>
where
  D: Directory,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Direct(directory) => directory.fmt(f),
      Self::RateLimited(directory) => directory.fmt(f),
    }
  }
}

impl<D> HasIdentity for MergeSchedulerDirectory<D>
where
  D: Directory,
{
  fn identity(&self) -> &Identity {
    match self {
      Self::Direct(directory) => directory.identity(),
      Self::RateLimited(directory) => directory.identity(),
    }
  }
}

impl<D> CloseableRef for MergeSchedulerDirectory<D>
where
  D: Directory,
{
  fn close(&self) -> Result<()> {
    match self {
      Self::Direct(directory) => directory.close(),
      Self::RateLimited(directory) => directory.close(),
    }
  }
}

impl<D> Directory for MergeSchedulerDirectory<D>
where
  D: Directory,
{
  fn list_all(&self) -> Result<Vec<String>> {
    match self {
      Self::Direct(directory) => directory.list_all(),
      Self::RateLimited(directory) => directory.list_all(),
    }
  }

  fn delete_file(&self, name: &str) -> Result<()> {
    match self {
      Self::Direct(directory) => directory.delete_file(name),
      Self::RateLimited(directory) => directory.delete_file(name),
    }
  }

  fn file_length(&self, name: &str) -> Result<usize> {
    match self {
      Self::Direct(directory) => directory.file_length(name),
      Self::RateLimited(directory) => directory.file_length(name),
    }
  }

  type IndexOutput = RateLimitedIndexOutputEnum<D::IndexOutput, Arc<MergeRateLimiter>>;

  fn create_output(&self, name: &str, context: &IOContext) -> Result<Self::IndexOutput> {
    match self {
      Self::Direct(directory) => directory
        .create_output(name, context)
        .map(RateLimitedIndexOutputEnum::B),
      Self::RateLimited(directory) => directory.create_output(name, context),
    }
  }

  fn create_temp_output(
    &self,
    prefix: &str,
    suffix: &str,
    context: &IOContext,
  ) -> Result<Self::IndexOutput> {
    match self {
      Self::Direct(directory) => directory
        .create_temp_output(prefix, suffix, context)
        .map(RateLimitedIndexOutputEnum::B),
      Self::RateLimited(directory) => directory.create_temp_output(prefix, suffix, context),
    }
  }

  fn sync(&self, names: &[String]) -> Result<()> {
    match self {
      Self::Direct(directory) => directory.sync(names),
      Self::RateLimited(directory) => directory.sync(names),
    }
  }

  fn sync_metadata(&self) -> Result<()> {
    match self {
      Self::Direct(directory) => directory.sync_metadata(),
      Self::RateLimited(directory) => directory.sync_metadata(),
    }
  }

  fn rename(&self, source: &str, dest: &str) -> Result<()> {
    match self {
      Self::Direct(directory) => directory.rename(source, dest),
      Self::RateLimited(directory) => directory.rename(source, dest),
    }
  }

  type IndexInput = D::IndexInput;

  fn open_input(&self, name: &str, context: &IOContext) -> Result<Self::IndexInput> {
    match self {
      Self::Direct(directory) => directory.open_input(name, context),
      Self::RateLimited(directory) => directory.open_input(name, context),
    }
  }

  fn open_checksum_input(
    &self,
    name: &str,
  ) -> Result<BufferedChecksumIndexInput<Self::IndexInput>> {
    let input = self.open_input(name, &IOContext::default_io_context()?)?;
    Ok(BufferedChecksumIndexInput::new(input))
  }

  type Lock = D::Lock;

  fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
    match self {
      Self::Direct(directory) => directory.obtain_lock(name),
      Self::RateLimited(directory) => directory.obtain_lock(name),
    }
  }

  fn copy_from<F>(&self, from: &F, src: &str, dest: &str, context: &IOContext) -> Result<()>
  where
    F: Directory + ?Sized,
  {
    match self {
      Self::Direct(directory) => directory.copy_from(from, src, dest, context),
      Self::RateLimited(directory) => directory.copy_from(from, src, dest, context),
    }
  }

  fn get_pending_deletions(&self) -> Result<HashSet<String>> {
    match self {
      Self::Direct(directory) => directory.get_pending_deletions(),
      Self::RateLimited(directory) => directory.get_pending_deletions(),
    }
  }

  #[cfg(debug_assertions)]
  fn is_fs_directory(&self) -> bool {
    match self {
      Self::Direct(directory) => directory.is_fs_directory(),
      Self::RateLimited(directory) => directory.is_fs_directory(),
    }
  }

  fn ensure_open(&self) -> Result<()> {
    match self {
      Self::Direct(directory) => directory.ensure_open(),
      Self::RateLimited(directory) => directory.ensure_open(),
    }
  }
}

impl MergeScheduler for MergeSchedulerEnum {
  fn merge<MS, D>(&self, merge_source: MS, trigger: MergeTrigger) -> Result<()>
  where
    MS: MergeSource<D> + Clone + 'static,
    D: Directory + 'static,
    OneMerge<D, MS::Reader>: Send + 'static,
  {
    match self {
      MergeSchedulerEnum::Serial(s) => s.merge(merge_source, trigger),
      MergeSchedulerEnum::No(n) => n.merge(merge_source, trigger),
      MergeSchedulerEnum::Concurrent(c) => c.merge(merge_source, trigger),
      #[cfg(test)]
      MergeSchedulerEnum::SerialTest(s) => s.merge(merge_source, trigger),
      #[cfg(test)]
      MergeSchedulerEnum::LatchedSerial(s) => s.merge(merge_source, trigger),
      #[cfg(test)]
      MergeSchedulerEnum::KnnMergeScheduler(s) => s.merge(merge_source, trigger),
      #[cfg(test)]
      MergeSchedulerEnum::IndexWriterMerging(s) => s.merge(merge_source, trigger),
      #[cfg(test)]
      MergeSchedulerEnum::PartialAddIndexes(s) => s.merge(merge_source, trigger),
      #[cfg(test)]
      MergeSchedulerEnum::CountingAddIndexes(s) => s.merge(merge_source, trigger),
    }
  }

  type Directory<D>
    = MergeSchedulerDirectory<D>
  where
    D: Directory;

  fn wrap_for_merge<D>(&self, in_: D) -> Result<Self::Directory<D>>
  where
    D: Directory,
  {
    match self {
      MergeSchedulerEnum::Serial(s) => Ok(MergeSchedulerDirectory::Direct(s.wrap_for_merge(in_)?)),
      MergeSchedulerEnum::No(n) => Ok(MergeSchedulerDirectory::Direct(n.wrap_for_merge(in_)?)),
      MergeSchedulerEnum::Concurrent(c) => {
        Ok(MergeSchedulerDirectory::RateLimited(c.wrap_for_merge(in_)?))
      },
      #[cfg(test)]
      MergeSchedulerEnum::SerialTest(s) => {
        Ok(MergeSchedulerDirectory::Direct(s.wrap_for_merge(in_)?))
      },
      #[cfg(test)]
      MergeSchedulerEnum::LatchedSerial(s) => {
        Ok(MergeSchedulerDirectory::Direct(s.wrap_for_merge(in_)?))
      },
      #[cfg(test)]
      MergeSchedulerEnum::KnnMergeScheduler(s) => {
        Ok(MergeSchedulerDirectory::Direct(s.wrap_for_merge(in_)?))
      },
      #[cfg(test)]
      MergeSchedulerEnum::IndexWriterMerging(s) => {
        Ok(MergeSchedulerDirectory::Direct(s.wrap_for_merge(in_)?))
      },
      #[cfg(test)]
      MergeSchedulerEnum::PartialAddIndexes(s) => {
        Ok(MergeSchedulerDirectory::Direct(s.wrap_for_merge(in_)?))
      },
      #[cfg(test)]
      MergeSchedulerEnum::CountingAddIndexes(s) => {
        Ok(MergeSchedulerDirectory::Direct(s.wrap_for_merge(in_)?))
      },
    }
  }

  fn initialize<D>(&mut self, info_stream: InfoStreamMT, directory: &D) -> Result<()>
  where
    D: Directory,
  {
    match self {
      MergeSchedulerEnum::Serial(s) => s.initialize(info_stream, directory),
      MergeSchedulerEnum::No(n) => n.initialize(info_stream, directory),
      MergeSchedulerEnum::Concurrent(c) => c.initialize(info_stream, directory),
      #[cfg(test)]
      MergeSchedulerEnum::SerialTest(s) => s.initialize(info_stream, directory),
      #[cfg(test)]
      MergeSchedulerEnum::LatchedSerial(s) => s.initialize(info_stream, directory),
      #[cfg(test)]
      MergeSchedulerEnum::KnnMergeScheduler(s) => s.initialize(info_stream, directory),
      #[cfg(test)]
      MergeSchedulerEnum::IndexWriterMerging(s) => s.initialize(info_stream, directory),
      #[cfg(test)]
      MergeSchedulerEnum::PartialAddIndexes(s) => s.initialize(info_stream, directory),
      #[cfg(test)]
      MergeSchedulerEnum::CountingAddIndexes(s) => s.initialize(info_stream, directory),
    }
  }
}
