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
use crate::core::codecs::codec::Codecs;
use crate::core::codecs::doc_values_producer::DocValuesProducer;
use crate::core::codecs::dummy::stored_fields_writer::DummyStoredFieldsWriter;
use crate::core::codecs::fields_producer::FieldsProducer;
use crate::core::codecs::knn_vectors_reader::KnnVectorsReader;
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::codecs::points_reader::PointsReader;
use crate::core::codecs::stored_fields_reader::StoredFieldsReader;
use crate::core::codecs::term_vectors_reader::TermVectorsReader;
use crate::core::document::document_stored_field_visitor::DocumentStoredFieldVisitor;
use crate::core::index::binary_doc_values::BinaryDocValues;
use crate::core::index::byte_vector_values::ByteVectorValues;
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::doc_values_skip_index_type::DocValuesSkipIndexType;
use crate::core::index::doc_values_skipper::DocValuesSkipper;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::fields::Fields;
use crate::core::index::float_vector_values::FloatVectorValues;
use crate::core::index::impacts::Impacts;
use crate::core::index::impacts_source::ImpactsSource;
use crate::core::index::index_file_names::IndexFileNames;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_writer::{MAX_POSITION, WRITE_LOCK_NAME};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::pending_soft_deletes::count_soft_deletes;
use crate::core::index::point_values::{IntersectVisitor, PointValues, Relation};
use crate::core::index::postings_enum::{ALL, FREQS, NONE, PostingsEnum};
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_infos::{
  OLD_SEGMENTS_GEN, SegmentInfos, generation_from_segments_file_name,
  get_last_commit_segments_file_name,
};
use crate::core::index::segment_reader::SegmentReader;
use crate::core::index::sorted_doc_values::SortedDocValues;
use crate::core::index::sorted_numeric_doc_values::SortedNumericDocValues;
use crate::core::index::sorted_set_doc_values::SortedSetDocValues;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term_vectors::TermVectors;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::index::{BytesRef, BytesRefBuilder};
use crate::core::search::doc_id_set_iterator::{
  AllDISI, DocIdSetIterator, DocIdSetIteratorEnum2, NO_MORE_DOCS,
};
use crate::core::search::dummy::dummy_scorable::DummyScorable;
use crate::core::search::field_comparator::FieldComparator;
use crate::core::search::field_exists_query::get_doc_values_doc_id_set_iterator;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::search::leaf_field_comparator::LeafFieldComparator;
use crate::core::search::pruning::Pruning;
use crate::core::search::sort::Sort;
use crate::core::search::sort_field::SortFiledBase;
use crate::core::search::top_knn_collector::TopKnnCollector;
use crate::core::store::IO_CONTEXT_DEFAULT;
use crate::core::store::directory::{Directory, DirectoryEnum};
use crate::core::store::fs_directory_base::FSDirectoryBaseEnum;
use crate::core::store::lock::{Lock, LockEnum};
use crate::core::store::lock_factory::LockFactoryEnum;
use crate::core::store::mmap_directory::MMapDirectory;
use crate::core::store::nio_fs_directory::NIOFSDirectory;
use crate::core::store::{FSDirectories, NativeFSLockFactory};
use crate::core::util::array_util::{ArrayUtil, ByteArrayComparator, ByteArrayComparatorEnum};
use crate::core::util::automation::automata::Automata;
use crate::core::util::automation::automaton::Automaton;
use crate::core::util::automation::byte_run_automaton::ByteRunAutomaton;
use crate::core::util::automation::byte_runnable::ByteRunnable;
use crate::core::util::automation::compiled_automaton::CompiledAutomaton;
use crate::core::util::automation::operations::Operations;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bits::Bits;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::iterator::IteratorExt;
use crate::core::util::long_bit_set::LongBitSet;
use crate::core::util::string_helper::StringHelper;
use crate::core::util::{CoreHelper, IOUtils, TryIntoInt};
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Sink, Stdout, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Basic tool and API to check the health of an index and write a new segments file that removes
/// references to problematic segments.
///
/// As this tool checks every byte in the index, it can take quite a long time to run on a large
/// index.
///
/// This API is experimental. Make a complete backup of the index before using it to exorcise
/// corrupted documents.
pub struct CheckIndex<D: Directory, L: Lock = <D as Directory>::Lock, W: Write = std::io::Sink> {
  dir: Arc<D>,
  write_lock: L,
  info_stream: Option<W>,
  closed: bool,
  level: i32,
  fail_fast: bool,
  verbose: bool,
  thread_count: i32,
}

/// Details the health and status of the index returned by
/// [`CheckIndex::check_index`].
pub struct Status<D: Directory> {
  /// True if no problems were found with the index.
  pub clean: bool,

  /// True if the segments_N file could not be located and loaded.
  pub missing_segments: bool,

  /// Name of latest segments_N file in the index.
  pub segments_file_name: Option<String>,

  /// Number of segments in the index.
  pub num_segments: i32,

  /// Empty unless a specific list of segments was passed to check.
  pub segments_checked: Vec<String>,

  /// True if the index was created with a newer version of Lucene than the
  /// CheckIndex tool.
  pub tool_out_of_date: bool,

  /// Status of each segment in the index.
  pub segment_infos: Vec<SegmentInfoStatus>,

  /// Directory the index is in.
  pub dir: Option<Arc<D>>,

  /// SegmentInfos containing only segments that had no problems. This is used
  /// by exorciseIndex to repair the index.
  pub(crate) new_segments: Option<SegmentInfos<D>>,

  /// How many documents will be lost to bad segments.
  pub tot_lose_doc_count: i32,

  /// How many bad segments were found.
  pub num_bad_segments: i32,

  /// True if only specific segments were checked.
  pub partial: bool,

  /// The greatest segment name.
  pub max_segment_name: i64,

  /// Whether the SegmentInfos counter is greater than every segment name.
  pub valid_counter: bool,

  /// User data of the last commit in the index.
  pub user_data: Option<HashMap<String, String>>,
}

impl<D: Directory> Default for Status<D> {
  fn default() -> Self {
    Self {
      clean: false,
      missing_segments: false,
      segments_file_name: None,
      num_segments: 0,
      segments_checked: Vec::new(),
      tool_out_of_date: false,
      segment_infos: Vec::new(),
      dir: None,
      new_segments: None,
      tot_lose_doc_count: 0,
      num_bad_segments: 0,
      partial: false,
      max_segment_name: 0,
      valid_counter: false,
      user_data: None,
    }
  }
}

/// Holds the status of one segment in the index.
pub struct SegmentInfoStatus {
  /// Name of the segment.
  pub name: Option<String>,

  /// Codec used to read this segment.
  pub codec: Option<Codecs>,

  /// Document count, not taking deletions into account.
  pub max_doc: i32,

  /// True if the segment uses the compound file format.
  pub compound: bool,

  /// Number of files referenced by this segment.
  pub num_files: i32,

  /// Net size in MB of the files referenced by this segment.
  pub size_mb: f64,

  /// True if this segment has pending deletions.
  pub has_deletions: bool,

  /// Current deletions generation.
  pub deletions_gen: i64,

  /// True if a CodecReader was successfully opened on this segment.
  pub open_reader_passed: bool,

  /// Document count in this segment that would be lost.
  pub to_lose_doc_count: i32,

  /// Debugging details recorded by IndexWriter when it created the segment.
  pub diagnostics: Option<HashMap<String, String>>,

  /// Status for testing live docs.
  pub live_doc_status: Option<LiveDocStatus>,

  /// Status for testing field infos.
  pub field_info_status: Option<FieldInfoStatus>,

  /// Status for testing field norms, or None if they could not be tested.
  pub field_norm_status: Option<FieldNormStatus>,

  /// Status for testing indexed terms, or None if they could not be tested.
  pub term_index_status: Option<TermIndexStatus>,

  /// Status for testing stored fields, or None if they could not be tested.
  pub stored_field_status: Option<StoredFieldStatus>,

  /// Status for testing term vectors, or None if they could not be tested.
  pub term_vector_status: Option<TermVectorStatus>,

  /// Status for testing DocValues, or None if they could not be tested.
  pub doc_values_status: Option<DocValuesStatus>,

  /// Status for testing PointValues, or None if they could not be tested.
  pub points_status: Option<PointsStatus>,

  /// Status of index sort.
  pub index_sort_status: Option<IndexSortStatus>,

  /// Status of vectors.
  pub vector_values_status: Option<VectorValuesStatus>,

  /// Status of soft deletes.
  pub soft_deletes_status: Option<SoftDeletesStatus>,

  /// Error thrown during the segment test, or None on success.
  pub error: Option<LuceneError>,
}

impl Default for SegmentInfoStatus {
  fn default() -> Self {
    Self {
      name: None,
      codec: None,
      max_doc: 0,
      compound: false,
      num_files: 0,
      size_mb: 0.0,
      has_deletions: false,
      deletions_gen: 0,
      open_reader_passed: false,
      to_lose_doc_count: 0,
      diagnostics: None,
      live_doc_status: None,
      field_info_status: None,
      field_norm_status: None,
      term_index_status: None,
      stored_field_status: None,
      term_vector_status: None,
      doc_values_status: None,
      points_status: None,
      index_sort_status: None,
      vector_values_status: None,
      soft_deletes_status: None,
      error: None,
    }
  }
}

/// Status from testing live docs.
#[derive(Default)]
pub struct LiveDocStatus {
  /// Number of deleted documents.
  pub num_deleted: i32,

  /// Error thrown during the live docs test, or None on success.
  pub error: Option<LuceneError>,
}

/// Status from testing field infos.
#[derive(Default)]
pub struct FieldInfoStatus {
  /// Number of fields successfully tested.
  pub tot_fields: i64,

  /// Error thrown during the field infos test, or None on success.
  pub error: Option<LuceneError>,
}

/// Status from testing field norms.
#[derive(Default)]
pub struct FieldNormStatus {
  /// Number of fields successfully tested.
  pub tot_fields: i64,

  /// Error thrown during the field norms test, or None on success.
  pub error: Option<LuceneError>,
}

/// Status from testing the term index.
#[derive(Default)]
pub struct TermIndexStatus {
  /// Number of terms with at least one live document.
  pub term_count: i64,

  /// Number of terms with zero live documents.
  pub del_term_count: i64,

  /// Total frequency across all terms.
  pub tot_freq: i64,

  /// Total number of positions.
  pub tot_pos: i64,

  /// Error thrown during the term index test, or None on success.
  pub error: Option<LuceneError>,

  /// Details of block allocations in the block tree terms dictionary. This is
  /// only set when the postings format for the segment uses block tree.
  pub block_tree_stats: Option<HashMap<String, String>>,
}

/// Status from testing stored fields.
#[derive(Default)]
pub struct StoredFieldStatus {
  /// Number of documents tested.
  pub doc_count: i32,

  /// Total number of stored fields tested.
  pub tot_fields: i64,

  /// Error thrown during the stored fields test, or None on success.
  pub error: Option<LuceneError>,
}

/// Status from testing term vectors.
#[derive(Default)]
pub struct TermVectorStatus {
  /// Number of documents tested.
  pub doc_count: i32,

  /// Total number of term vectors tested.
  pub tot_vectors: i64,

  /// Error thrown during the term vector test, or None on success.
  pub error: Option<LuceneError>,
}

/// Status from testing DocValues.
#[derive(Default)]
pub struct DocValuesStatus {
  /// Total number of DocValues fields tested.
  pub total_value_fields: i64,

  /// Total number of numeric fields.
  pub total_numeric_fields: i64,

  /// Total number of binary fields.
  pub total_binary_fields: i64,

  /// Total number of sorted fields.
  pub total_sorted_fields: i64,

  /// Total number of sorted-numeric fields.
  pub total_sorted_numeric_fields: i64,

  /// Total number of sorted-set fields.
  pub total_sorted_set_fields: i64,

  /// Total number of skipping indexes tested.
  pub total_skipping_index: i64,

  /// Error thrown during the DocValues test, or None on success.
  pub error: Option<LuceneError>,
}

/// Status from testing PointValues.
#[derive(Default)]
pub struct PointsStatus {
  /// Total number of point values tested.
  pub total_value_points: i64,

  /// Total number of fields with points.
  pub total_value_fields: i32,

  /// Error thrown during the PointValues test, or None on success.
  pub error: Option<LuceneError>,
}

/// Status from testing vector values.
#[derive(Default)]
pub struct VectorValuesStatus {
  /// Total number of vector values tested.
  pub total_vector_values: i64,

  /// Total number of fields with vectors.
  pub total_knn_vector_fields: i32,

  /// Error thrown during the vector values test, or None on success.
  pub error: Option<LuceneError>,
}

/// Status from testing index sort.
#[derive(Default)]
pub struct IndexSortStatus {
  /// Error thrown during the index sort test, or None on success.
  pub error: Option<LuceneError>,
}

/// Status from testing soft deletes.
#[derive(Default)]
pub struct SoftDeletesStatus {
  /// Error thrown during the soft deletes test, or None on success.
  pub error: Option<LuceneError>,
}

impl<D, W> CheckIndex<D, <D as Directory>::Lock, W>
where
  D: Directory,
  W: Write,
{
  /// Creates a new CheckIndex on the directory.
  pub fn new(dir: Arc<D>) -> Result<Self> {
    let write_lock = dir.obtain_lock(WRITE_LOCK_NAME)?;
    Ok(Self::with_lock(dir, write_lock))
  }
}

impl<D, L, W> CheckIndex<D, L, W>
where
  D: Directory,
  L: Lock,
  W: Write,
{
  /// Expert: creates a CheckIndex with the specified lock. This should only
  /// be used by special tests that would otherwise need to close the writer
  /// before every check.
  pub fn with_lock(dir: Arc<D>, write_lock: L) -> Self {
    let thread_count = std::thread::available_parallelism().map_or(1, |parallelism| {
      i32::try_from(parallelism.get()).unwrap_or(i32::MAX)
    });
    Self {
      dir,
      write_lock,
      info_stream: None,
      closed: false,
      level: 0,
      fail_fast: false,
      verbose: false,
      thread_count,
    }
  }

  fn ensure_open(&self) -> Result<()> {
    if self.closed {
      return Err(LuceneError::already_closed("this instance is closed"));
    }
    Ok(())
  }
}

impl<D, L, W> Closeable for CheckIndex<D, L, W>
where
  D: Directory,
  L: Lock,
  W: Write,
{
  fn close(&mut self) -> Result<()> {
    self.closed = true;
    CloseableRef::close(&self.write_lock)
  }
}

impl<D, L, W> CheckIndex<D, L, W>
where
  D: Directory,
  L: Lock,
  W: Write,
{
  /// Sets the level. Higher values perform additional checks and will likely
  /// drastically increase how long CheckIndex takes to run. See [`Level`].
  pub fn set_level(&mut self, v: i32) -> Result<()> {
    Level::check_if_level_in_bounds(v)?;
    self.level = v;
    Ok(())
  }

  /// Returns the level configured by [`Self::set_level`].
  pub fn get_level(&self) -> i32 {
    self.level
  }

  /// If true, returns the original error immediately when corruption is
  /// detected rather than continuing to look for corruption in other
  /// segments.
  pub fn set_fail_fast(&mut self, v: bool) {
    self.fail_fast = v;
  }

  /// Returns the value configured by [`Self::set_fail_fast`].
  pub fn get_fail_fast(&self) -> bool {
    self.fail_fast
  }

  /// Sets the thread count used to parallelize index integrity checking.
  pub fn set_thread_count(&mut self, thread_count: i32) -> Result<()> {
    if thread_count <= 0 {
      return Err(LuceneError::illegal_argument(format!(
        "setThreadCount requires a number larger than 0, but got: {thread_count}"
      )));
    }
    self.thread_count = thread_count;
    Ok(())
  }

  /// Sets the output stream where messages should go. If `out` is `None`, no
  /// messages are printed. If `verbose` is true, more details are printed.
  pub fn set_info_stream_with_verbose(&mut self, out: impl Into<Option<W>>, verbose: bool) {
    self.info_stream = out.into();
    self.verbose = verbose;
  }

  /// Sets the output stream where messages should go without verbose output.
  /// See [`Self::set_info_stream_with_verbose`].
  pub fn set_info_stream(&mut self, out: impl Into<Option<W>>) {
    self.set_info_stream_with_verbose(out, false);
  }

  fn msg_bytes<O: Write>(out: Option<&mut O>, message: &[u8]) -> Result<()> {
    if let Some(out) = out {
      writeln!(out, "{}", String::from_utf8_lossy(message))?;
    }
    Ok(())
  }

  fn msg<O: Write>(out: Option<&mut O>, message: &str) -> Result<()> {
    if let Some(out) = out {
      writeln!(out, "{message}")?;
    }
    Ok(())
  }

  /// Returns a status detailing the state of the index.
  ///
  /// As this method checks every byte in the index, it can take quite a long time to run on a large
  /// index.
  ///
  /// # Warning
  ///
  /// Only call this method when the index is not open by any writer.
  pub fn check_index(&mut self) -> Result<Status<D>> {
    self.check_index_with_segments(None)
  }

  /// Returns a status detailing the state of the index, checking only the
  /// specified segments when `only_segments` is not None.
  ///
  /// As this method checks every byte in the specified segments, it can take quite a long time to
  /// run on a large index.
  #[allow(clippy::field_reassign_with_default, clippy::unnecessary_sort_by)]
  pub fn check_index_with_segments(
    &mut self,
    only_segments: Option<&[String]>,
  ) -> Result<Status<D>> {
    self.ensure_open()?;

    let start = Instant::now();
    Self::msg(
      self.info_stream.as_mut(),
      &format!("Checking index with threadCount: {}", self.thread_count),
    )?;

    let mut result = Status::default();
    result.dir = Some(Arc::clone(&self.dir));
    let files = self.dir.list_all()?;
    let last_segments_file = get_last_commit_segments_file_name(&files)?.ok_or_else(|| {
      LuceneError::index_not_found(format!(
        "no segments* file found in {}: files: {files:?}",
        self.dir
      ))
    })?;

    let mut all_segments_files = Vec::new();
    for file_name in &files {
      if file_name.starts_with(IndexFileNames::SEGMENTS) && file_name != OLD_SEGMENTS_GEN {
        all_segments_files.push((
          generation_from_segments_file_name(file_name)?,
          file_name.clone(),
        ));
      }
    }
    all_segments_files.sort_by(|a, b| b.0.cmp(&a.0));

    let mut last_commit = None;
    for (_, file_name) in all_segments_files {
      let is_last_commit = file_name == last_segments_file;
      let read_result = panic::catch_unwind(AssertUnwindSafe(|| {
        SegmentInfos::read_commit_with_file_min_version(Arc::clone(&self.dir), &file_name, 0)
      }));
      let read_result = match read_result {
        Ok(read_result) => read_result,
        Err(payload) if self.fail_fast => panic::resume_unwind(payload),
        Err(payload) => Err(LuceneError::tragedy_from_panic(
          &format!("could not read commit point from segments file {file_name}"),
          payload.as_ref(),
        )),
      };
      match read_result {
        Ok(infos) => {
          if is_last_commit {
            last_commit = Some(infos);
          }
        },
        Err(error) => {
          if self.fail_fast {
            return Err(error);
          }
          let commit_kind = if is_last_commit {
            "latest commit point"
          } else {
            "old (not latest) commit point"
          };
          Self::msg(
            self.info_stream.as_mut(),
            &format!(
              "ERROR: could not read {commit_kind} from segments file \"{file_name}\" in directory"
            ),
          )?;
          Self::msg(self.info_stream.as_mut(), &format!("{error:?}"))?;
          result.missing_segments = true;
          return Ok(result);
        },
      }
    }

    let last_commit = match last_commit {
      Some(last_commit) => last_commit,
      None => {
        Self::msg(
          self.info_stream.as_mut(),
          "ERROR: could not read any segments file in directory",
        )?;
        result.missing_segments = true;
        return Ok(result);
      },
    };

    if self.info_stream.is_some() {
      let mut max_doc = 0;
      let mut del_count = 0;
      for info in last_commit.iter() {
        max_doc += info.info.max_doc()?;
        del_count += info.get_del_count();
      }
      Self::msg(
        self.info_stream.as_mut(),
        &format!(
          "{:.2}% total deletions; {max_doc} documents; {del_count} deletions",
          100.0 * f64::from(del_count) / f64::from(max_doc)
        ),
      )?;
    }

    // find the oldest and newest segment versions
    let mut oldest = None;
    let mut newest = None;
    let mut old_segments = None;
    for info in last_commit.iter() {
      if let Some(version) = info.info.get_version_ref() {
        if oldest.as_ref().is_none_or(|oldest| version < oldest) {
          oldest = Some(version.clone());
        }
        if newest.as_ref().is_none_or(|newest| version >= newest) {
          newest = Some(version.clone());
        }
      } else {
        old_segments = Some("pre-3.1");
      }
    }

    let num_segments: i32 = last_commit.size().try_convert()?;
    result.segments_file_name = last_commit.get_segments_file_name();
    result.num_segments = num_segments;
    result.user_data = Some(last_commit.get_user_data().clone());

    let version_string = if let Some(old_segments) = old_segments {
      if let Some(newest) = newest.as_ref() {
        format!("versions=[{old_segments} .. {newest}]")
      } else {
        format!("version={old_segments}")
      }
    } else if let Some(newest) = newest.as_ref() {
      let oldest = oldest
        .as_ref()
        .expect("newest version implies oldest version");
      if oldest == newest {
        format!("version={oldest}")
      } else {
        format!("versions=[{oldest} .. {newest}]")
      }
    } else {
      String::new()
    };

    Self::msg(
      self.info_stream.as_mut(),
      &format!(
        "Segments file={} numSegments={} {} id={}{}",
        result.segments_file_name.as_deref().unwrap_or(""),
        num_segments,
        version_string,
        StringHelper::id_to_string(last_commit.get_id()),
        if last_commit.get_user_data().is_empty() {
          String::new()
        } else {
          format!(" userData={:?}", last_commit.get_user_data())
        }
      ),
    )?;

    if let Some(only_segments) = only_segments {
      result.partial = true;
      result.segments_checked.extend_from_slice(only_segments);
      if let Some(info_stream) = self.info_stream.as_mut() {
        write!(info_stream, "\nChecking only these segments:")?;
        for segment in only_segments {
          write!(info_stream, " {segment}")?;
        }
        writeln!(info_stream, ":")?;
      }
    }

    let mut new_segments = last_commit.try_clone()?;
    new_segments.clear();
    new_segments.dropped_segment_commit_infos.clear();
    result.new_segments = Some(new_segments);
    result.max_segment_name = -1;

    // if threadCount == 1, then use the main thread to do index checking sequentially
    if self.thread_count == 1 {
      for (index, info) in last_commit.iter().iter().enumerate() {
        Self::update_max_segment_name(&mut result, info)?;
        if only_segments.is_some_and(|segments| !segments.contains(&info.info.name)) {
          continue;
        }

        Self::msg(
          self.info_stream.as_mut(),
          &format!(
            "{} of {}: name={} maxDoc={}",
            index + 1,
            num_segments,
            info.info.name,
            info.info.max_doc()?
          ),
        )?;
        let segment_info_status = Self::test_segment(
          self.level,
          self.fail_fast,
          self.verbose,
          &last_commit,
          info,
          self.info_stream.as_mut(),
        )?;
        Self::process_segment_info_status_result(&mut result, info, segment_info_status)?;
      }
    } else {
      // checks segments concurrently
      let mut segment_commit_infos = Vec::with_capacity(last_commit.size());
      for info in last_commit.iter() {
        let size_in_bytes = match info.size_in_bytes() {
          Ok(size_in_bytes) => Some(size_in_bytes),
          Err(error) => {
            Self::msg(
              self.info_stream.as_mut(),
              "ERROR: IOException occurred when comparing SegmentCommitInfo file sizes",
            )?;
            Self::msg(self.info_stream.as_mut(), &format!("{error:?}"))?;
            None
          },
        };
        segment_commit_infos.push((size_in_bytes, info));
      }

      // sort segmentCommitInfos by segment size, as smaller segment tends to finish faster, and
      // hence its output can be printed out faster
      segment_commit_infos.sort_by(|(size1, _), (size2, _)| match (size1, size2) {
        (Some(size1), Some(size2)) => size1.cmp(size2),
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
      });

      let mut jobs = Vec::new();
      // start larger segments earlier
      for index in (0..segment_commit_infos.len()).rev() {
        let info = segment_commit_infos[index].1;
        Self::update_max_segment_name(&mut result, info)?;
        if only_segments.is_some_and(|segments| !segments.contains(&info.info.name)) {
          continue;
        }
        jobs.push((index, info));
      }

      let next_job = AtomicUsize::new(0);
      let cancelled = AtomicBool::new(false);
      let worker_count = usize::min(self.thread_count.try_convert()?, jobs.len());
      let level = self.level;
      let fail_fast = self.fail_fast;
      let verbose = self.verbose;
      thread::scope(|scope| -> Result<()> {
        let (sender, receiver) = mpsc::channel();
        let mut handles = Vec::with_capacity(worker_count);
        for _ in 0..worker_count {
          let sender = sender.clone();
          let jobs = &jobs;
          let next_job = &next_job;
          let cancelled = &cancelled;
          let last_commit = &last_commit;
          handles.push(scope.spawn(move || {
            loop {
              if fail_fast && cancelled.load(AtomicOrdering::Relaxed) {
                break;
              }
              let job_index = next_job.fetch_add(1, AtomicOrdering::Relaxed);
              let Some((index, info)) = jobs.get(job_index).copied() else {
                break;
              };
              if fail_fast && cancelled.load(AtomicOrdering::Relaxed) {
                break;
              }

              let mut output = Vec::new();
              let segment_result = match panic::catch_unwind(AssertUnwindSafe(|| {
                (|| -> Result<SegmentInfoStatus> {
                  Self::msg(
                    Some(&mut output),
                    &format!(
                      "{} of {}: name={} maxDoc={}",
                      index + 1,
                      num_segments,
                      info.info.name,
                      info.info.max_doc()?
                    ),
                  )?;
                  Self::test_segment(
                    level,
                    fail_fast,
                    verbose,
                    last_commit,
                    info,
                    Some(&mut output),
                  )
                })()
              })) {
                Ok(segment_result) => segment_result,
                Err(payload) => Err(LuceneError::tragedy_from_panic(
                  &format!("Segment {} check failed", info.info.name),
                  payload.as_ref(),
                )),
              };
              if fail_fast && segment_result.is_err() {
                cancelled.store(true, AtomicOrdering::Relaxed);
              }
              if sender.send((index, output, segment_result)).is_err() {
                break;
              }
            }
          }));
        }
        drop(sender);

        let mut segment_results: Vec<Option<(Vec<u8>, Result<SegmentInfoStatus>)>> =
          (0..segment_commit_infos.len()).map(|_| None).collect();
        let mut index = 0;
        while index < segment_commit_infos.len() {
          let info = segment_commit_infos[index].1;
          if only_segments.is_some_and(|segments| !segments.contains(&info.info.name)) {
            index += 1;
            continue;
          }

          let Some((output, segment_result)) = segment_results[index].take() else {
            match receiver.recv() {
              Ok((result_index, output, segment_result)) => {
                segment_results[result_index] = Some((output, segment_result));
                continue;
              },
              Err(_error) if fail_fast => {
                index += 1;
                continue;
              },
              Err(error) => {
                return Err(LuceneError::illegal_state(format!(
                  "failed to receive segment check result: {error}"
                )));
              },
            }
          };

          // print segment results in order
          Self::msg_bytes(self.info_stream.as_mut(), &output)?;
          let segment_info_status = match segment_result {
            Ok(segment_info_status) => segment_info_status,
            Err(error) => {
              let mut check_index_error =
                LuceneError::corrupt_index(format!("Segment {} check failed.", info.info.name));
              check_index_error.add_suppressed(error);
              return Err(check_index_error);
            },
          };
          Self::process_segment_info_status_result(&mut result, info, segment_info_status)?;
          index += 1;
        }

        for handle in handles {
          if let Err(payload) = handle.join() {
            return Err(LuceneError::tragedy_from_panic(
              "CheckIndex segment worker failed",
              payload.as_ref(),
            ));
          }
        }
        Ok(())
      })?;
    }

    if result.num_bad_segments == 0 {
      result.clean = true;
    } else {
      Self::msg(
        self.info_stream.as_mut(),
        &format!(
          "WARNING: {} broken segments (containing {} documents) detected",
          result.num_bad_segments, result.tot_lose_doc_count
        ),
      )?;
    }

    result.valid_counter = result.max_segment_name < last_commit.counter;
    if !result.valid_counter {
      result.clean = false;
      if let Some(new_segments) = result.new_segments.as_mut() {
        new_segments.counter = result.max_segment_name + 1;
      }
      Self::msg(
        self.info_stream.as_mut(),
        &format!(
          "ERROR: Next segment name counter {} is not greater than max segment name {}",
          last_commit.counter, result.max_segment_name
        ),
      )?;
    }

    if result.clean {
      Self::msg(
        self.info_stream.as_mut(),
        "No problems were detected with this index.\n",
      )?;
    }
    Self::msg(
      self.info_stream.as_mut(),
      &format!(
        "Took {:.3} sec total.",
        ns_to_sec(start.elapsed().as_nanos())
      ),
    )?;

    Ok(result)
  }

  fn update_max_segment_name(result: &mut Status<D>, info: &SegmentCommitInfo<D>) -> Result<()> {
    let segment_name_suffix = info
      .info
      .name
      .get(1..)
      .filter(|suffix| !suffix.is_empty())
      .ok_or_else(|| {
        LuceneError::number_format(format!("invalid segment name {}", info.info.name))
      })?;
    let segment_name = i64::from_str_radix(segment_name_suffix, 36).map_err(|error| {
      LuceneError::number_format(format!(
        "failed to parse segment name {}: {error}",
        info.info.name
      ))
    })?;
    if segment_name > result.max_segment_name {
      result.max_segment_name = segment_name;
    }
    Ok(())
  }

  fn process_segment_info_status_result(
    result: &mut Status<D>,
    info: &SegmentCommitInfo<D>,
    segment_info_status: SegmentInfoStatus,
  ) -> Result<()> {
    let has_error = segment_info_status.error.is_some();
    let to_lose_doc_count = segment_info_status.to_lose_doc_count;
    result.segment_infos.push(segment_info_status);
    if has_error {
      result.tot_lose_doc_count += to_lose_doc_count;
      result.num_bad_segments += 1;
    } else if let Some(new_segments) = result.new_segments.as_mut() {
      new_segments.add(info.clone())?;
    }
    Ok(())
  }

  #[allow(clippy::field_reassign_with_default)]
  fn test_segment<O: Write>(
    level: i32,
    fail_fast: bool,
    verbose: bool,
    segment_infos: &SegmentInfos<D>,
    info: &SegmentCommitInfo<D>,
    mut info_stream: Option<&mut O>,
  ) -> Result<SegmentInfoStatus> {
    let mut segment_status = SegmentInfoStatus::default();
    segment_status.name = Some(info.info.name.clone());
    segment_status.max_doc = info.info.max_doc()?;
    if segment_status.max_doc <= 0 {
      return Err(LuceneError::corrupt_index(format!(
        " illegal number of documents: maxDoc={}",
        segment_status.max_doc
      )));
    }

    let mut to_lose_doc_count = segment_status.max_doc;
    let mut reader = None;
    let body_result = panic::catch_unwind(AssertUnwindSafe(|| -> Result<()> {
      Self::msg(
        info_stream.as_deref_mut(),
        &format!(
          "    version={}",
          info
            .info
            .get_version_ref()
            .map_or_else(|| "3.0".to_string(), ToString::to_string)
        ),
      )?;
      Self::msg(
        info_stream.as_deref_mut(),
        &format!(
          "    id={}",
          StringHelper::id_to_string(Some(info.info.get_id()))
        ),
      )?;
      let codec = info.info.get_codec()?.clone();
      Self::msg(info_stream.as_deref_mut(), &format!("    codec={codec}"))?;
      segment_status.codec = Some(codec);
      segment_status.compound = info.info.get_use_compound_file();
      Self::msg(
        info_stream.as_deref_mut(),
        &format!("    compound={}", segment_status.compound),
      )?;
      segment_status.num_files = info.files()?.len().try_convert()?;
      Self::msg(
        info_stream.as_deref_mut(),
        &format!("    numFiles={}", segment_status.num_files),
      )?;
      if let Some(index_sort) = info.info.get_index_sort() {
        Self::msg(
          info_stream.as_deref_mut(),
          &format!("    sort={index_sort}"),
        )?;
      }
      segment_status.size_mb = info.size_in_bytes()? as f64 / (1024.0 * 1024.0);
      Self::msg(
        info_stream.as_deref_mut(),
        &format!("    size (MB)={:.3}", segment_status.size_mb),
      )?;
      segment_status.diagnostics = Some(info.info.get_diagnostics().clone());
      if !info.info.get_diagnostics().is_empty() {
        Self::msg(
          info_stream.as_deref_mut(),
          &format!("    diagnostics = {:?}", info.info.get_diagnostics()),
        )?;
      }

      if info.has_deletions() {
        segment_status.has_deletions = true;
        segment_status.deletions_gen = info.get_del_gen();
        Self::msg(
          info_stream.as_deref_mut(),
          &format!("    has deletions [delGen={}]", info.get_del_gen()),
        )?;
      } else {
        Self::msg(info_stream.as_deref_mut(), "    no deletions")?;
      }

      let start_open_reader = Instant::now();
      if let Some(out) = info_stream.as_deref_mut() {
        write!(out, "    test: open reader.........")?;
      }
      reader = Some(SegmentReader::new(
        info,
        segment_infos.get_index_created_version_major(),
        &IO_CONTEXT_DEFAULT,
      )?);
      Self::msg(
        info_stream.as_deref_mut(),
        &format!(
          "OK [took {:.3} sec]",
          ns_to_sec(start_open_reader.elapsed().as_nanos())
        ),
      )?;
      segment_status.open_reader_passed = true;

      let start_integrity = Instant::now();
      if let Some(out) = info_stream.as_deref_mut() {
        write!(out, "    test: check integrity.....")?;
      }
      let opened_reader = reader.as_ref().expect("segment reader was just opened");
      opened_reader.check_integrity()?;
      Self::msg(
        info_stream.as_deref_mut(),
        &format!(
          "OK [took {:.3} sec]",
          ns_to_sec(start_integrity.elapsed().as_nanos())
        ),
      )?;

      if opened_reader.max_doc()? != info.info.max_doc()? {
        return Err(LuceneError::corrupt_index(format!(
          "SegmentReader.maxDoc() {} != SegmentInfo.maxDoc {}",
          opened_reader.max_doc()?,
          info.info.max_doc()?
        )));
      }

      let num_docs = opened_reader.num_docs()?;
      to_lose_doc_count = num_docs;
      if opened_reader.has_deletions()? {
        if num_docs != info.info.max_doc()? - info.get_del_count() {
          return Err(LuceneError::corrupt_index(format!(
            "delete count mismatch: info={} vs reader={num_docs}",
            info.info.max_doc()? - info.get_del_count()
          )));
        }
        if info.info.max_doc()? - num_docs > opened_reader.max_doc()? {
          return Err(LuceneError::corrupt_index(format!(
            "too many deleted docs: maxDoc()={} vs del count={}",
            opened_reader.max_doc()?,
            info.info.max_doc()? - num_docs
          )));
        }
        if info.info.max_doc()? - num_docs != info.get_del_count() {
          return Err(LuceneError::corrupt_index(format!(
            "delete count mismatch: info={} vs reader={}",
            info.get_del_count(),
            info.info.max_doc()? - num_docs
          )));
        }
      } else if info.get_del_count() != 0 {
        return Err(LuceneError::corrupt_index(format!(
          "delete count mismatch: info={} vs reader={}",
          info.get_del_count(),
          info.info.max_doc()? - num_docs
        )));
      }
      if level >= Level::MIN_LEVEL_FOR_INTEGRITY_CHECKS {
        // Test Livedocs
        segment_status.live_doc_status =
          Some(CheckIndex::<DirectoryEnum, LockEnum, Sink>::test_live_docs(
            opened_reader,
            info_stream.as_deref_mut(),
            fail_fast,
          )?);

        // Test Fieldinfos
        segment_status.field_info_status = Some(
          CheckIndex::<DirectoryEnum, LockEnum, Sink>::test_field_infos(
            opened_reader,
            info_stream.as_deref_mut(),
            fail_fast,
          )?,
        );

        // Test Field Norms
        segment_status.field_norm_status = Some(
          CheckIndex::<DirectoryEnum, LockEnum, Sink>::test_field_norms(
            opened_reader,
            info_stream.as_deref_mut(),
            fail_fast,
          )?,
        );

        // Test the Term Index
        segment_status.term_index_status =
          Some(CheckIndex::<DirectoryEnum, LockEnum, Sink>::test_postings(
            opened_reader,
            info_stream.as_deref_mut(),
            verbose,
            level,
            fail_fast,
          )?);

        // Test Stored Fields
        segment_status.stored_field_status = Some(
          CheckIndex::<DirectoryEnum, LockEnum, Sink>::test_stored_fields(
            opened_reader,
            info_stream.as_deref_mut(),
            fail_fast,
          )?,
        );

        // Test Term Vectors
        segment_status.term_vector_status = Some(
          CheckIndex::<DirectoryEnum, LockEnum, Sink>::test_term_vectors(
            opened_reader,
            info_stream.as_deref_mut(),
            verbose,
            level,
            fail_fast,
          )?,
        );

        // Test Docvalues
        segment_status.doc_values_status = Some(
          CheckIndex::<DirectoryEnum, LockEnum, Sink>::test_doc_values(
            opened_reader,
            info_stream.as_deref_mut(),
            fail_fast,
          )?,
        );

        // Test PointValues
        segment_status.points_status =
          Some(CheckIndex::<DirectoryEnum, LockEnum, Sink>::test_points(
            opened_reader,
            info_stream.as_deref_mut(),
            fail_fast,
          )?);

        // Test FloatVectorValues and ByteVectorValues
        segment_status.vector_values_status =
          Some(CheckIndex::<DirectoryEnum, LockEnum, Sink>::test_vectors(
            opened_reader,
            info_stream.as_deref_mut(),
            fail_fast,
          )?);

        // Test Index Sort
        if let Some(index_sort) = info.info.get_index_sort() {
          segment_status.index_sort_status =
            Some(CheckIndex::<DirectoryEnum, LockEnum, Sink>::test_sort(
              opened_reader,
              index_sort.as_ref(),
              info_stream.as_deref_mut(),
              fail_fast,
            )?);
        }

        // Test Soft Deletes
        let field_infos = opened_reader.get_field_infos()?;
        if let Some(soft_deletes_field) = field_infos.get_soft_deletes_field() {
          segment_status.soft_deletes_status = Some(
            CheckIndex::<DirectoryEnum, LockEnum, Sink>::check_soft_deletes(
              soft_deletes_field,
              info,
              opened_reader,
              info_stream.as_deref_mut(),
              fail_fast,
            )?,
          );
        }

        // Rethrow the first exception we encountered
        //  This will cause stats for failed segments to be incremented properly
        // We won't be able to (easily) stop check running in another thread, so we may as well
        // wait for all of them to complete before we proceed, and that we don't return the
        // check error below while the segment part check may still print out messages
        if let Some(error) = segment_status
          .live_doc_status
          .as_ref()
          .and_then(|status| status.error.as_ref())
        {
          let mut check_error = LuceneError::corrupt_index("Live docs test failed");
          check_error.add_suppressed(error.clone());
          return Err(check_error);
        } else if let Some(error) = segment_status
          .field_info_status
          .as_ref()
          .and_then(|status| status.error.as_ref())
        {
          let mut check_error = LuceneError::corrupt_index("Field Info test failed");
          check_error.add_suppressed(error.clone());
          return Err(check_error);
        } else if let Some(error) = segment_status
          .field_norm_status
          .as_ref()
          .and_then(|status| status.error.as_ref())
        {
          let mut check_error = LuceneError::corrupt_index("Field Norm test failed");
          check_error.add_suppressed(error.clone());
          return Err(check_error);
        } else if let Some(error) = segment_status
          .term_index_status
          .as_ref()
          .and_then(|status| status.error.as_ref())
        {
          let mut check_error = LuceneError::corrupt_index("Term Index test failed");
          check_error.add_suppressed(error.clone());
          return Err(check_error);
        } else if let Some(error) = segment_status
          .stored_field_status
          .as_ref()
          .and_then(|status| status.error.as_ref())
        {
          let mut check_error = LuceneError::corrupt_index("Stored Field test failed");
          check_error.add_suppressed(error.clone());
          return Err(check_error);
        } else if let Some(error) = segment_status
          .term_vector_status
          .as_ref()
          .and_then(|status| status.error.as_ref())
        {
          let mut check_error = LuceneError::corrupt_index("Term Vector test failed");
          check_error.add_suppressed(error.clone());
          return Err(check_error);
        } else if let Some(error) = segment_status
          .doc_values_status
          .as_ref()
          .and_then(|status| status.error.as_ref())
        {
          let mut check_error = LuceneError::corrupt_index("DocValues test failed");
          check_error.add_suppressed(error.clone());
          return Err(check_error);
        } else if let Some(error) = segment_status
          .points_status
          .as_ref()
          .and_then(|status| status.error.as_ref())
        {
          let mut check_error = LuceneError::corrupt_index("Points test failed");
          check_error.add_suppressed(error.clone());
          return Err(check_error);
        } else if let Some(error) = segment_status
          .vector_values_status
          .as_ref()
          .and_then(|status| status.error.as_ref())
        {
          let mut check_error = LuceneError::corrupt_index("Vectors test failed");
          check_error.add_suppressed(error.clone());
          return Err(check_error);
        } else if let Some(error) = segment_status
          .index_sort_status
          .as_ref()
          .and_then(|status| status.error.as_ref())
        {
          let mut check_error = LuceneError::corrupt_index("Index Sort test failed");
          check_error.add_suppressed(error.clone());
          return Err(check_error);
        } else if let Some(error) = segment_status
          .soft_deletes_status
          .as_ref()
          .and_then(|status| status.error.as_ref())
        {
          let mut check_error = LuceneError::corrupt_index("Soft Deletes test failed");
          check_error.add_suppressed(error.clone());
          return Err(check_error);
        }
      }
      Self::msg(info_stream.as_deref_mut(), "")?;
      Ok(())
    }));

    let mut panic_payload = None;
    let body_result = match body_result {
      Ok(body_result) => Some(body_result),
      Err(payload) if fail_fast => {
        panic_payload = Some(payload);
        None
      },
      Err(payload) => Some(Err(LuceneError::tragedy_from_panic(
        "CheckIndex segment check failed",
        payload.as_ref(),
      ))),
    };

    let result = body_result.map(|body_result| match body_result {
      Ok(()) => Ok(segment_status),
      Err(error) if fail_fast => Err(error),
      Err(error) => {
        let error_debug = format!("{error:?}");
        segment_status.error = Some(error);
        segment_status.to_lose_doc_count = to_lose_doc_count;
        let report_result = (|| -> Result<()> {
          Self::msg(info_stream.as_deref_mut(), "FAILED")?;
          Self::msg(
            info_stream.as_deref_mut(),
            "    WARNING: exorciseIndex() would remove reference to this segment; full exception:",
          )?;
          Self::msg(info_stream.as_deref_mut(), &error_debug)?;
          Self::msg(info_stream.as_deref_mut(), "")
        })();
        report_result.map(|()| segment_status)
      },
    });

    let close_result = match reader.as_ref() {
      Some(reader) => reader.close(),
      None => Ok(()),
    };
    close_result?;
    if let Some(payload) = panic_payload {
      panic::resume_unwind(payload);
    }
    result.expect("body result is present unless a panic is resumed")
  }
}

impl CheckIndex<DirectoryEnum, LockEnum, Sink> {
  /// Tests index sort order.
  pub fn test_sort<R, O>(
    reader: &R,
    sort: &Sort,
    mut info_stream: Option<&mut O>,
    fail_fast: bool,
  ) -> Result<IndexSortStatus>
  where
    R: CodecReader,
    O: Write,
  {
    // This segment claims its documents are sorted according to the incoming sort ... let's make
    // sure:
    let start = Instant::now();
    let mut status = IndexSortStatus::default();

    if let Some(info_stream) = info_stream.as_deref_mut() {
      write!(info_stream, "    test: index sort..........")?;
    }

    let check_result = panic::catch_unwind(AssertUnwindSafe(|| -> Result<()> {
      let fields = sort.get_sort();
      let reverse_mul: Vec<i32> = fields
        .iter()
        .map(|field| if field.get_reverse() { -1 } else { 1 })
        .collect();
      let reader_context = LeafReaderContext::from_top_lr(reader);
      let mut comparators = Vec::with_capacity(fields.len());
      let mut leaf_comparators = Vec::with_capacity(fields.len());
      for field in fields {
        let mut comparator = field.get_comparator(1, Pruning::None)?;
        let leaf_comparator = comparator.get_leaf_comparator(&reader_context)?;
        comparators.push(comparator);
        leaf_comparators.push(leaf_comparator);
      }

      let meta_data = reader.get_metadata()?;
      let field_infos = reader.get_field_infos()?;
      if meta_data.get_has_blocks()
        && field_infos.get_parent_field().is_none()
        && meta_data.get_created_version_major() >= crate::core::util::LUCENE_10_0_0.major
      {
        return Err(LuceneError::illegal_state(format!(
          "parent field is not set but the index has document blocks and was created with version: {}",
          meta_data.get_created_version_major()
        )));
      }
      let mut iterator = if meta_data.get_has_blocks() && field_infos.get_parent_field().is_some() {
        let parent_field = field_infos
          .get_parent_field()
          .expect("the parent field is present");
        let iterator = LeafReader::get_numeric_doc_values(reader, parent_field)?
          .ok_or_else(|| LuceneError::corrupt_index("parent field has no numeric doc values"))?;
        DocIdSetIteratorEnum2::A(iterator)
      } else {
        DocIdSetIteratorEnum2::B(AllDISI::new(reader.max_doc()?))
      };

      let mut prev_doc = iterator.next_doc()?;
      let mut scorer = DummyScorable;
      loop {
        let next_doc = iterator.next_doc()?;
        if next_doc == NO_MORE_DOCS {
          break;
        }
        let mut cmp = 0;
        for i in 0..leaf_comparators.len() {
          // TODO: would be better if copy() didn't cause a term lookup in TermOrdVal & co,
          // the segments are always the same here...
          leaf_comparators[i].copy(0, prev_doc, &mut scorer, &mut comparators[i])?;
          leaf_comparators[i].set_bottom(0, &mut comparators[i])?;
          cmp = reverse_mul[i]
            * leaf_comparators[i].compare_bottom(next_doc, &mut scorer, &mut comparators[i])?;
          if cmp != 0 {
            break;
          }
        }
        if cmp > 0 {
          return Err(LuceneError::corrupt_index(format!(
            "segment has indexSort={sort} but docID={prev_doc} sorts after docID={next_doc}"
          )));
        }
        prev_doc = next_doc;
      }
      Self::msg(
        info_stream.as_deref_mut(),
        &format!("OK [took {:.3} sec]", ns_to_sec(start.elapsed().as_nanos())),
      )?;
      Ok(())
    }));

    let check_result = match check_result {
      Ok(check_result) => check_result,
      Err(payload) if fail_fast => panic::resume_unwind(payload),
      Err(payload) => Err(LuceneError::tragedy_from_panic(
        "CheckIndex index sort test failed",
        payload.as_ref(),
      )),
    };
    if let Err(error) = check_result {
      if fail_fast {
        return Err(error);
      }
      Self::msg(info_stream.as_deref_mut(), &format!("ERROR [{error}]"))?;
      if let Some(info_stream) = info_stream {
        writeln!(info_stream, "{error:?}")?;
      }
      status.error = Some(error);
    }
    Ok(status)
  }

  /// Test live docs.
  pub fn test_live_docs<R, O>(
    reader: &R,
    mut info_stream: Option<&mut O>,
    fail_fast: bool,
  ) -> Result<LiveDocStatus>
  where
    R: CodecReader,
    O: Write,
  {
    let start = Instant::now();
    let mut status = LiveDocStatus::default();

    let check_result = panic::catch_unwind(AssertUnwindSafe(|| -> Result<()> {
      if let Some(info_stream) = info_stream.as_deref_mut() {
        write!(info_stream, "    test: check live docs.....")?;
      }
      let num_docs = reader.num_docs()?;
      if reader.has_deletions()? {
        let live_docs = reader.get_live_docs()?.ok_or_else(|| {
          LuceneError::corrupt_index("segment should have deletions, but liveDocs is null")
        })?;
        let mut num_live = 0;
        for j in 0..live_docs.length() {
          if live_docs.get(j)? {
            num_live += 1;
          }
        }
        if num_live != num_docs {
          return Err(LuceneError::corrupt_index(format!(
            "liveDocs count mismatch: info={num_docs}, vs bits={num_live}"
          )));
        }

        status.num_deleted = reader.num_deleted_docs()?;
        Self::msg(
          info_stream.as_deref_mut(),
          &format!(
            "OK [{} deleted docs] [took {:.3} sec]",
            status.num_deleted,
            ns_to_sec(start.elapsed().as_nanos())
          ),
        )?;
      } else {
        if let Some(live_docs) = reader.get_live_docs()? {
          // it's ok for it to be non-null here, as long as none are set right?
          for j in 0..live_docs.length() {
            if !live_docs.get(j)? {
              return Err(LuceneError::corrupt_index(format!(
                "liveDocs mismatch: info says no deletions but doc {j} is deleted."
              )));
            }
          }
        }
        Self::msg(
          info_stream.as_deref_mut(),
          &format!("OK [took {:.3} sec]", ns_to_sec(start.elapsed().as_nanos())),
        )?;
      }
      Ok(())
    }));
    let check_result = match check_result {
      Ok(check_result) => check_result,
      Err(payload) if fail_fast => panic::resume_unwind(payload),
      Err(payload) => Err(LuceneError::tragedy_from_panic(
        "CheckIndex live docs test failed",
        payload.as_ref(),
      )),
    };

    if let Err(error) = check_result {
      if fail_fast {
        return Err(error);
      }
      Self::msg(info_stream.as_deref_mut(), &format!("ERROR [{error}]"))?;
      if let Some(info_stream) = info_stream {
        writeln!(info_stream, "{error:?}")?;
      }
      status.error = Some(error);
    }

    Ok(status)
  }

  /// Test field infos.
  pub fn test_field_infos<R, O>(
    reader: &R,
    mut info_stream: Option<&mut O>,
    fail_fast: bool,
  ) -> Result<FieldInfoStatus>
  where
    R: CodecReader,
    O: Write,
  {
    let start = Instant::now();
    let mut status = FieldInfoStatus::default();

    let check_result = panic::catch_unwind(AssertUnwindSafe(|| -> Result<()> {
      // Test Field Infos
      if let Some(info_stream) = info_stream.as_deref_mut() {
        write!(info_stream, "    test: field infos.........")?;
      }
      let field_infos = reader.get_field_infos()?;
      for field_info in field_infos.iter() {
        field_info.check_consistency()?;
      }
      Self::msg(
        info_stream.as_deref_mut(),
        &format!(
          "OK [{} fields] [took {:.3} sec]",
          field_infos.size(),
          ns_to_sec(start.elapsed().as_nanos())
        ),
      )?;
      status.tot_fields = field_infos.size().try_convert()?;
      Ok(())
    }));
    let check_result = match check_result {
      Ok(check_result) => check_result,
      Err(payload) if fail_fast => panic::resume_unwind(payload),
      Err(payload) => Err(LuceneError::tragedy_from_panic(
        "CheckIndex field infos test failed",
        payload.as_ref(),
      )),
    };

    if let Err(error) = check_result {
      if fail_fast {
        return Err(error);
      }
      Self::msg(info_stream.as_deref_mut(), &format!("ERROR [{error}]"))?;
      if let Some(info_stream) = info_stream {
        writeln!(info_stream, "{error:?}")?;
      }
      status.error = Some(error);
    }

    Ok(status)
  }

  /// Test field norms.
  pub fn test_field_norms<R, O>(
    reader: &R,
    mut info_stream: Option<&mut O>,
    fail_fast: bool,
  ) -> Result<FieldNormStatus>
  where
    R: CodecReader,
    O: Write,
  {
    let start = Instant::now();
    let mut status = FieldNormStatus::default();
    let mut norms_reader = None;
    let mut merge_norms_reader = None;

    let check_result = panic::catch_unwind(AssertUnwindSafe(|| -> Result<()> {
      // Test Field Norms
      if let Some(info_stream) = info_stream.as_deref_mut() {
        write!(info_stream, "    test: field norms.........")?;
      }
      norms_reader = reader.get_norms_reader()?;
      if let Some(reader) = norms_reader.as_ref() {
        merge_norms_reader = reader.get_merge_instance()?;
      }
      let norms_reader = merge_norms_reader.as_ref().or(norms_reader.as_ref());
      for info in reader.get_field_infos()?.iter() {
        if info.has_norms() {
          let norms_reader = norms_reader.as_ref().ok_or_else(|| {
            LuceneError::corrupt_index(format!(
              "field \"{}\" has norms but reader.getNormsReader() is null",
              info.name
            ))
          })?;
          Self::check_numeric_doc_values(
            &info.name,
            norms_reader.get_norms(info)?,
            norms_reader.get_norms(info)?,
          )?;
          status.tot_fields += 1;
        }
      }

      Self::msg(
        info_stream.as_deref_mut(),
        &format!(
          "OK [{} fields] [took {:.3} sec]",
          status.tot_fields,
          ns_to_sec(start.elapsed().as_nanos())
        ),
      )?;
      Ok(())
    }));

    let check_result = match check_result {
      Ok(check_result) => check_result,
      Err(payload) if fail_fast => panic::resume_unwind(payload),
      Err(payload) => Err(LuceneError::tragedy_from_panic(
        "CheckIndex field norms test failed",
        payload.as_ref(),
      )),
    };

    if let Err(error) = check_result {
      if fail_fast {
        return Err(error);
      }
      Self::msg(info_stream.as_deref_mut(), &format!("ERROR [{}]", error))?;
      if let Some(info_stream) = info_stream {
        writeln!(info_stream, "{error:?}")?;
      }
      status.error = Some(error);
    }

    Ok(status)
  }

  /// Checks that the Fields API is consistent with itself. The searcher is optional, to verify
  /// with queries, and can be None.
  #[allow(
    clippy::too_many_arguments,
    clippy::unnecessary_unwrap,
    clippy::while_let_loop
  )]
  fn check_fields<F, B, N, O>(
    fields: &F,
    live_docs: Option<&B>,
    max_doc: i32,
    field_infos: &FieldInfos,
    norms_producer: Option<&N>,
    do_print: bool,
    is_vectors: bool,
    mut info_stream: Option<&mut O>,
    verbose: bool,
    level: i32,
  ) -> Result<TermIndexStatus>
  where
    F: Fields,
    B: Bits,
    N: NormsProducer,
    O: Write,
  {
    // TODO: we should probably return our own stats thing...?!
    let start = do_print.then(Instant::now);

    let mut status = TermIndexStatus::default();
    let mut computed_field_count = 0;

    let mut postings = None;

    let mut last_field: Option<String> = None;
    let mut fields_iterator = fields.iterator()?;
    while let Some(field) = fields_iterator.next()? {
      // MultiFieldsEnum relies upon this order...
      if last_field
        .as_ref()
        .is_some_and(|last_field| field <= last_field)
      {
        return Err(LuceneError::corrupt_index(format!(
          "fields out of order: lastField={} field={field}",
          last_field.as_deref().unwrap_or("")
        )));
      }
      last_field = Some(field.clone());

      // check that the field is in fieldinfos, and is indexed.
      // TODO: add a separate test to check this for different reader impls
      let field_info = field_infos.field_info_by_name(field)?.ok_or_else(|| {
        LuceneError::corrupt_index(format!(
          "fieldsEnum inconsistent with fieldInfos, no fieldInfos for: {field}"
        ))
      })?;
      if *field_info.get_index_options() == IndexOptions::None {
        return Err(LuceneError::corrupt_index(format!(
          "fieldsEnum inconsistent with fieldInfos, isIndexed == false for: {field}"
        )));
      }

      // TODO: really the codec should not return a field
      // from FieldsEnum if it has no Terms... but we do
      // this today:
      // assert fields.terms(field) != null;
      computed_field_count += 1;

      let Some(terms) = fields.terms(field)? else {
        continue;
      };

      let terms_doc_count = terms.get_doc_count()?;
      if terms_doc_count > max_doc {
        return Err(LuceneError::corrupt_index(format!(
          "docCount > maxDoc for field: {field}, docCount={terms_doc_count}, maxDoc={max_doc}"
        )));
      }

      let has_freqs = terms.has_freqs();
      let has_positions = terms.has_positions();
      let has_payloads = terms.has_payloads();
      let has_offsets = terms.has_offsets();

      let (min_term, max_term) = if is_vectors {
        // Term vectors impls can be very slow for getMax
        (None, None)
      } else {
        let min_term = terms.get_min()?.map(Cow::into_owned);
        if let Some(min_term) = min_term.as_ref() {
          debug_assert!(min_term.is_valid()?);
        }
        let max_term = terms.get_max()?.map(Cow::into_owned);
        if let Some(max_term) = max_term.as_ref() {
          debug_assert!(max_term.is_valid()?);
        }
        if max_term.is_some() && min_term.is_none() {
          return Err(LuceneError::corrupt_index(format!(
            "field \"{field}\" has null minTerm but non-null maxTerm"
          )));
        }
        if max_term.is_none() && min_term.is_some() {
          return Err(LuceneError::corrupt_index(format!(
            "field \"{field}\" has non-null minTerm but null maxTerm"
          )));
        }
        (min_term, max_term)
      };

      // term vectors cannot omit TF:
      let expected_has_freqs =
        is_vectors || *field_info.get_index_options() >= IndexOptions::DocsAndFreqs;
      if has_freqs != expected_has_freqs {
        return Err(LuceneError::corrupt_index(format!(
          "field \"{field}\" should have hasFreqs={expected_has_freqs} but got {has_freqs}"
        )));
      }

      if !is_vectors {
        let expected_has_positions =
          *field_info.get_index_options() >= IndexOptions::DocsAndFreqsAndPositions;
        if has_positions != expected_has_positions {
          return Err(LuceneError::corrupt_index(format!(
            "field \"{field}\" should have hasPositions={expected_has_positions} but got {has_positions}"
          )));
        }

        let expected_has_payloads = field_info.has_payloads();
        if has_payloads != expected_has_payloads {
          return Err(LuceneError::corrupt_index(format!(
            "field \"{field}\" should have hasPayloads={expected_has_payloads} but got {has_payloads}"
          )));
        }

        let expected_has_offsets =
          *field_info.get_index_options() >= IndexOptions::DocsAndFreqsAndPositionsAndOffsets;
        if has_offsets != expected_has_offsets {
          return Err(LuceneError::corrupt_index(format!(
            "field \"{field}\" should have hasOffsets={expected_has_offsets} but got {has_offsets}"
          )));
        }
      }

      let mut terms_enum = terms.iterator()?;

      let mut has_ord = true;
      let term_count_start = status.del_term_count + status.term_count;

      let mut last_term: Option<BytesRefBuilder<Vec<u8>>> = None;

      let mut sum_total_term_freq = 0;
      let mut sum_doc_freq = 0;
      let mut visited_docs = FixedBitSet::new(max_doc.try_convert()?);
      loop {
        let Some(term) = terms_enum.next()? else {
          break;
        };
        let term = term.into_owned();
        // System.out.println("CI: field=" + field + " check term=" + term + " docFreq=" +
        // termsEnum.docFreq());

        debug_assert!(term.is_valid()?);

        // make sure terms arrive in order according to
        // the comp
        if let Some(last_term) = last_term.as_mut() {
          if last_term.get_bytes_ref() >= &term {
            return Err(LuceneError::corrupt_index(format!(
              "terms out of order: lastTerm={} term={term}",
              last_term.get_bytes_ref()
            )));
          }
          last_term.copy_bytes_from_ref(&term)?;
        } else {
          let mut builder = BytesRefBuilder::new();
          builder.copy_bytes_from_ref(&term)?;
          last_term = Some(builder);
        }

        if !is_vectors {
          let min_term = min_term.as_ref().ok_or_else(|| {
            LuceneError::corrupt_index(format!(
              "field=\"{field}\": invalid term: term={term}, minTerm=null"
            ))
          })?;
          if &term < min_term {
            return Err(LuceneError::corrupt_index(format!(
              "field=\"{field}\": invalid term: term={term}, minTerm={min_term}"
            )));
          }
          let max_term = max_term
            .as_ref()
            .expect("minTerm and maxTerm are both present");
          if &term > max_term {
            return Err(LuceneError::corrupt_index(format!(
              "field=\"{field}\": invalid term: term={term}, maxTerm={max_term}"
            )));
          }
        }

        let doc_freq = terms_enum.doc_freq()?;
        if doc_freq <= 0 {
          return Err(LuceneError::corrupt_index(format!(
            "docfreq: {doc_freq} is out of bounds"
          )));
        }
        sum_doc_freq += i64::from(doc_freq);

        postings = Some(terms_enum.postings_with_flags(postings.take(), ALL as i32)?);

        if !has_freqs {
          let total_term_freq = terms_enum.total_term_freq()?;
          let terms_enum_doc_freq = terms_enum.doc_freq()?;
          if total_term_freq != i64::from(terms_enum_doc_freq) {
            return Err(LuceneError::corrupt_index(format!(
              "field \"{field}\" hasFreqs is false, but TermsEnum.totalTermFreq()={total_term_freq} (should be {terms_enum_doc_freq})"
            )));
          }
        }

        if has_ord {
          let ord = match terms_enum.ord() {
            Ok(ord) => Some(ord),
            Err(LuceneError::UnsupportedOperation(_)) => {
              has_ord = false;
              None
            },
            Err(error) => return Err(error),
          };
          if let Some(ord) = ord {
            let ord_expected = status.del_term_count + status.term_count - term_count_start;
            if ord != ord_expected {
              return Err(LuceneError::corrupt_index(format!(
                "ord mismatch: TermsEnum has ord={ord} vs actual={ord_expected}"
              )));
            }
          }
        }

        let mut last_doc = -1;
        let mut doc_count = 0;
        let mut has_non_deleted_docs = false;
        let mut total_term_freq = 0;
        {
          let postings = postings.as_mut().expect("postings were just opened");
          loop {
            let doc = postings.next_doc()?;
            if doc == NO_MORE_DOCS {
              break;
            }
            let doc_index: usize = doc.try_convert()?;
            CoreHelper::check_index(doc_index, visited_docs.length())?;
            visited_docs.set(doc_index);
            let freq = postings.freq()?;
            if freq <= 0 {
              return Err(LuceneError::corrupt_index(format!(
                "term {term}: doc {doc}: freq {freq} is out of bounds"
              )));
            }
            if !has_freqs {
              // When a field didn't index freq, it must
              // consistently "lie" and pretend that freq was
              // 1:
              if postings.freq()? != 1 {
                return Err(LuceneError::corrupt_index(format!(
                  "term {term}: doc {doc}: freq {freq} != 1 when Terms.hasFreqs() is false"
                )));
              }
            }
            total_term_freq += i64::from(freq);

            if match live_docs {
              Some(live_docs) => live_docs.get(doc_index)?,
              None => true,
            } {
              has_non_deleted_docs = true;
              status.tot_freq += 1;
              if freq >= 0 {
                status.tot_pos += i64::from(freq);
              }
            }
            doc_count += 1;

            if doc <= last_doc {
              return Err(LuceneError::corrupt_index(format!(
                "term {term}: doc {doc} <= lastDoc {last_doc}"
              )));
            }
            if doc >= max_doc {
              return Err(LuceneError::corrupt_index(format!(
                "term {term}: doc {doc} >= maxDoc {max_doc}"
              )));
            }

            last_doc = doc;

            let mut last_pos = -1;
            let mut last_offset = 0;
            if has_positions {
              for _ in 0..freq {
                let pos = postings.next_position()?;

                if pos < 0 {
                  return Err(LuceneError::corrupt_index(format!(
                    "term {term}: doc {doc}: pos {pos} is out of bounds"
                  )));
                }
                if pos > MAX_POSITION {
                  return Err(LuceneError::corrupt_index(format!(
                    "term {term}: doc {doc}: pos {pos} > IndexWriter.MAX_POSITION={MAX_POSITION}"
                  )));
                }
                if pos < last_pos {
                  return Err(LuceneError::corrupt_index(format!(
                    "term {term}: doc {doc}: pos {pos} < lastPos {last_pos}"
                  )));
                }
                last_pos = pos;
                {
                  let payload = postings.get_payload()?;
                  if let Some(payload) = payload.as_ref() {
                    debug_assert!(payload.is_valid()?);
                    if payload.length < 1 {
                      return Err(LuceneError::corrupt_index(format!(
                        "term {term}: doc {doc}: pos {pos} payload length is out of bounds {}",
                        payload.length
                      )));
                    }
                  }
                }
                if has_offsets {
                  let start_offset = postings.start_offset()?;
                  let end_offset = postings.end_offset()?;
                  if start_offset < 0 {
                    return Err(LuceneError::corrupt_index(format!(
                      "term {term}: doc {doc}: pos {pos}: startOffset {start_offset} is out of bounds"
                    )));
                  }
                  if start_offset < last_offset {
                    return Err(LuceneError::corrupt_index(format!(
                      "term {term}: doc {doc}: pos {pos}: startOffset {start_offset} < lastStartOffset {last_offset}; consider using the FixBrokenOffsets tool in Lucene's backward-codecs module to correct your index"
                    )));
                  }
                  if end_offset < 0 {
                    return Err(LuceneError::corrupt_index(format!(
                      "term {term}: doc {doc}: pos {pos}: endOffset {end_offset} is out of bounds"
                    )));
                  }
                  if end_offset < start_offset {
                    return Err(LuceneError::corrupt_index(format!(
                      "term {term}: doc {doc}: pos {pos}: endOffset {end_offset} < startOffset {start_offset}"
                    )));
                  }
                  last_offset = start_offset;
                }
              }
            }
          }
        }

        if has_non_deleted_docs {
          status.term_count += 1;
        } else {
          status.del_term_count += 1;
        }

        let total_term_freq_2 = terms_enum.total_term_freq()?;

        if doc_count != doc_freq {
          return Err(LuceneError::corrupt_index(format!(
            "term {term} docFreq={doc_freq} != tot docs w/o deletions {doc_count}"
          )));
        }
        let terms_doc_count = terms.get_doc_count()?;
        if doc_freq > terms_doc_count {
          return Err(LuceneError::corrupt_index(format!(
            "term {term} docFreq={doc_freq} > docCount={terms_doc_count}"
          )));
        }
        if total_term_freq_2 <= 0 {
          return Err(LuceneError::corrupt_index(format!(
            "totalTermFreq: {total_term_freq_2} is out of bounds"
          )));
        }
        sum_total_term_freq += total_term_freq;
        if total_term_freq != total_term_freq_2 {
          return Err(LuceneError::corrupt_index(format!(
            "term {term} totalTermFreq={total_term_freq_2} != recomputed totalTermFreq={total_term_freq}"
          )));
        }
        if total_term_freq_2 < i64::from(doc_freq) {
          return Err(LuceneError::corrupt_index(format!(
            "totalTermFreq: {total_term_freq_2} is out of bounds, docFreq={doc_freq}"
          )));
        }
        if !has_freqs && total_term_freq != i64::from(doc_freq) {
          return Err(LuceneError::corrupt_index(format!(
            "term {term} totalTermFreq={total_term_freq} !=  docFreq={doc_freq}"
          )));
        }

        // Test skipping
        if has_positions {
          for idx in 0..7 {
            let skip_doc_id = (((idx + 1) * i64::from(max_doc)) / 8).try_convert()?;
            postings = Some(terms_enum.postings_with_flags(postings.take(), ALL as i32)?);
            let postings_ref = postings.as_mut().expect("postings were just opened");
            let doc_id = postings_ref.advance(skip_doc_id)?;
            if doc_id == NO_MORE_DOCS {
              break;
            }
            if doc_id < skip_doc_id {
              return Err(LuceneError::corrupt_index(format!(
                "term {term}: advance(docID={skip_doc_id}) returned docID={doc_id}"
              )));
            }
            let freq = postings_ref.freq()?;
            if freq <= 0 {
              return Err(LuceneError::corrupt_index(format!(
                "termFreq {freq} is out of bounds"
              )));
            }
            let mut last_position = -1;
            let mut last_offset = 0;
            for _ in 0..freq {
              let pos = postings_ref.next_position()?;

              if pos < 0 {
                return Err(LuceneError::corrupt_index(format!(
                  "position {pos} is out of bounds"
                )));
              }
              if pos < last_position {
                return Err(LuceneError::corrupt_index(format!(
                  "position {pos} is < lastPosition {last_position}"
                )));
              }
              last_position = pos;
              if has_offsets {
                let start_offset = postings_ref.start_offset()?;
                let end_offset = postings_ref.end_offset()?;
                // NOTE: we cannot enforce any bounds whatsoever on vectors... they were a
                // free-for-all before?
                // but for offsets in the postings lists these checks are fine: they were always
                // enforced by IndexWriter
                if !is_vectors {
                  if start_offset < 0 {
                    return Err(LuceneError::corrupt_index(format!(
                      "term {term}: doc {doc_id}: pos {pos}: startOffset {start_offset} is out of bounds"
                    )));
                  }
                  if start_offset < last_offset {
                    return Err(LuceneError::corrupt_index(format!(
                      "term {term}: doc {doc_id}: pos {pos}: startOffset {start_offset} < lastStartOffset {last_offset}"
                    )));
                  }
                  if end_offset < 0 {
                    return Err(LuceneError::corrupt_index(format!(
                      "term {term}: doc {doc_id}: pos {pos}: endOffset {end_offset} is out of bounds"
                    )));
                  }
                  if end_offset < start_offset {
                    return Err(LuceneError::corrupt_index(format!(
                      "term {term}: doc {doc_id}: pos {pos}: endOffset {end_offset} < startOffset {start_offset}"
                    )));
                  }
                }
                last_offset = start_offset;
              }
            }

            let next_doc_id = postings_ref.next_doc()?;
            if next_doc_id == NO_MORE_DOCS {
              break;
            }
            if next_doc_id <= doc_id {
              return Err(LuceneError::corrupt_index(format!(
                "term {term}: advance(docID={skip_doc_id}), then .next() returned docID={next_doc_id} vs prev docID={doc_id}"
              )));
            }

            if is_vectors {
              // Only 1 doc in the postings for term vectors, so we only test 1 advance:
              break;
            }
          }
        } else {
          for idx in 0..7 {
            let skip_doc_id = (((idx + 1) * i64::from(max_doc)) / 8).try_convert()?;
            postings = Some(terms_enum.postings_with_flags(postings.take(), NONE as i32)?);
            let postings_ref = postings.as_mut().expect("postings were just opened");
            let doc_id = postings_ref.advance(skip_doc_id)?;
            if doc_id == NO_MORE_DOCS {
              break;
            }
            if doc_id < skip_doc_id {
              return Err(LuceneError::corrupt_index(format!(
                "term {term}: advance(docID={skip_doc_id}) returned docID={doc_id}"
              )));
            }
            let next_doc_id = postings_ref.next_doc()?;
            if next_doc_id == NO_MORE_DOCS {
              break;
            }
            if next_doc_id <= doc_id {
              return Err(LuceneError::corrupt_index(format!(
                "term {term}: advance(docID={skip_doc_id}), then .next() returned docID={next_doc_id} vs prev docID={doc_id}"
              )));
            }
            if is_vectors {
              // Only 1 doc in the postings for term vectors, so we only test 1 advance:
              break;
            }
          }
        }

        // Checking score blocks is heavy, we only do it on long postings lists, on every 1024th
        // term or if slow checks are enabled.
        if level >= Level::MIN_LEVEL_FOR_SLOW_CHECKS
          || doc_freq > 1024
          || (status.term_count + status.del_term_count) % 1024 == 0
        {
          // First check max scores and block uptos
          // But only if slow checks are enabled since we visit all docs
          if level >= Level::MIN_LEVEL_FOR_SLOW_CHECKS {
            let mut max = -1;
            let mut max_freq = 0;
            let mut impacts_enum = terms_enum.impacts(FREQS as i32)?;
            postings = Some(terms_enum.postings_with_flags(postings.take(), FREQS as i32)?);
            loop {
              let doc = impacts_enum.next_doc()?;
              let postings_ref = postings.as_mut().expect("postings were just opened");
              if postings_ref.next_doc()? != doc {
                return Err(LuceneError::corrupt_index(format!(
                  "Wrong next doc: {doc}, expected {}",
                  postings_ref.doc_id()
                )));
              }
              if doc == NO_MORE_DOCS {
                break;
              }
              let postings_freq = postings_ref.freq()?;
              let impacts_freq = impacts_enum.freq()?;
              if postings_freq != impacts_freq {
                return Err(LuceneError::corrupt_index(format!(
                  "Wrong freq, expected {postings_freq}, but got {impacts_freq}"
                )));
              }
              if doc > max {
                impacts_enum.advance_shallow(doc)?;
                let impacts = impacts_enum.get_impacts()?;
                Self::check_impacts(&impacts, doc)?;
                max = impacts.get_doc_id_upto(0);
                let impacts_0 = impacts.get_impacts(0)?;
                max_freq = impacts_0
                  .last()
                  .expect("checkImpacts verified a non-empty list")
                  .freq;
              }
              if impacts_enum.freq()? > max_freq {
                return Err(LuceneError::corrupt_index(format!(
                  "freq {} is greater than the max freq according to impacts {max_freq}",
                  impacts_enum.freq()?
                )));
              }
            }
          }

          // Now check advancing
          let mut impacts_enum = terms_enum.impacts(FREQS as i32)?;
          postings = Some(terms_enum.postings_with_flags(postings.take(), FREQS as i32)?);

          let field_hash_code = field.encode_utf16().fold(0i32, |hash, value| {
            hash.wrapping_mul(31).wrapping_add(i32::from(value))
          });
          let mut max = -1;
          let mut max_freq = 0;
          loop {
            let mut doc = impacts_enum.doc_id();
            let (advance, target) = if (field_hash_code.wrapping_add(doc) & 1) == 1 {
              (false, doc.wrapping_add(1))
            } else {
              let delta = std::cmp::min(
                1 + (field_hash_code.wrapping_mul(31).wrapping_add(doc) & 0x1ff),
                NO_MORE_DOCS.wrapping_sub(doc),
              );
              (true, impacts_enum.doc_id().wrapping_add(delta))
            };

            if target > max && target % 2 == 1 {
              let delta = std::cmp::min(
                field_hash_code.wrapping_mul(31).wrapping_add(target) & 0x1ff,
                NO_MORE_DOCS.wrapping_sub(target),
              );
              max = target.wrapping_add(delta);
              impacts_enum.advance_shallow(target)?;
              let impacts = impacts_enum.get_impacts()?;
              Self::check_impacts(&impacts, doc)?;
              max_freq = i32::MAX;
              for impacts_level in 0..impacts.num_levels() {
                if impacts.get_doc_id_upto(impacts_level) >= max {
                  let per_level_impacts = impacts.get_impacts(impacts_level)?;
                  max_freq = per_level_impacts
                    .last()
                    .expect("checkImpacts verified a non-empty list")
                    .freq;
                  break;
                }
              }
            }

            doc = if advance {
              impacts_enum.advance(target)?
            } else {
              impacts_enum.next_doc()?
            };

            let postings_ref = postings.as_mut().expect("postings were just opened");
            if postings_ref.advance(target)? != doc {
              return Err(LuceneError::corrupt_index(format!(
                "Impacts do not advance to the same document as postings for target {target}, postings: {}, impacts: {doc}",
                postings_ref.doc_id()
              )));
            }
            if doc == NO_MORE_DOCS {
              break;
            }
            let postings_freq = postings_ref.freq()?;
            let impacts_freq = impacts_enum.freq()?;
            if postings_freq != impacts_freq {
              return Err(LuceneError::corrupt_index(format!(
                "Wrong freq, expected {postings_freq}, but got {impacts_freq}"
              )));
            }

            if doc >= max {
              let delta = std::cmp::min(
                field_hash_code.wrapping_mul(31).wrapping_add(target) & 0x1ff,
                NO_MORE_DOCS.wrapping_sub(doc),
              );
              max = doc.wrapping_add(delta);
              impacts_enum.advance_shallow(doc)?;
              let impacts = impacts_enum.get_impacts()?;
              Self::check_impacts(&impacts, doc)?;
              max_freq = i32::MAX;
              for impacts_level in 0..impacts.num_levels() {
                if impacts.get_doc_id_upto(impacts_level) >= max {
                  let per_level_impacts = impacts.get_impacts(impacts_level)?;
                  max_freq = per_level_impacts
                    .last()
                    .expect("checkImpacts verified a non-empty list")
                    .freq;
                  break;
                }
              }
            }

            if impacts_enum.freq()? > max_freq {
              return Err(LuceneError::corrupt_index(format!(
                "Term frequency {} is greater than the max freq according to impacts {max_freq}",
                impacts_enum.freq()?
              )));
            }
          }
        }
      }

      if min_term.is_some() && status.term_count + status.del_term_count == 0 {
        return Err(LuceneError::corrupt_index(format!(
          "field=\"{field}\": minTerm is non-null yet we saw no terms: {}",
          min_term.as_ref().expect("minTerm is present")
        )));
      }

      let field_terms = fields.terms(field)?;
      if let Some(field_terms) = field_terms {
        let field_term_count = status.del_term_count + status.term_count - term_count_start;

        let stats = field_terms.get_stats()?;
        status
          .block_tree_stats
          .get_or_insert_with(HashMap::new)
          .insert(field.clone(), stats);

        let actual_sum_doc_freq = field_terms.get_sum_doc_freq()?;
        if sum_doc_freq != actual_sum_doc_freq {
          return Err(LuceneError::corrupt_index(format!(
            "sumDocFreq for field {field}={actual_sum_doc_freq} != recomputed sumDocFreq={sum_doc_freq}"
          )));
        }

        let actual_sum_total_term_freq = field_terms.get_sum_total_term_freq()?;
        if sum_total_term_freq != actual_sum_total_term_freq {
          return Err(LuceneError::corrupt_index(format!(
            "sumTotalTermFreq for field {field}={actual_sum_total_term_freq} != recomputed sumTotalTermFreq={sum_total_term_freq}"
          )));
        }

        if !has_freqs && sum_total_term_freq != sum_doc_freq {
          return Err(LuceneError::corrupt_index(format!(
            "sumTotalTermFreq for field {field} should be {sum_doc_freq}, got sumTotalTermFreq={sum_total_term_freq}"
          )));
        }

        let doc_count = field_terms.get_doc_count()?;
        if visited_docs.cardinality() != doc_count.try_convert()? {
          return Err(LuceneError::corrupt_index(format!(
            "docCount for field {field}={doc_count} != recomputed docCount={}",
            visited_docs.cardinality()
          )));
        }

        if field_info.has_norms() && !is_vectors {
          let norms_producer = norms_producer.ok_or_else(|| {
            LuceneError::corrupt_index(format!(
              "field \"{field}\" has norms but normsProducer is null"
            ))
          })?;
          let mut norms = norms_producer.get_norms(&field_info)?;
          // count of valid norm values found for the field
          let mut actual_count = 0;
          // Cross-check terms with norms
          loop {
            let doc = norms.next_doc()?;
            if doc == NO_MORE_DOCS {
              break;
            }
            let doc_index: usize = doc.try_convert()?;
            if match live_docs {
              Some(live_docs) => !live_docs.get(doc_index)?,
              None => false,
            } {
              // Norms may only be out of sync with terms on deleted documents.
              // This happens when a document fails indexing and in that case it
              // should be immediately marked as deleted by the IndexWriter.
              continue;
            }
            let norm = norms.long_value()?;
            if norm != 0 {
              actual_count += 1;
              if !visited_docs.get(doc_index)? {
                return Err(LuceneError::corrupt_index(format!(
                  "Document {doc} doesn't have terms according to postings but has a norm value that is not zero: {}",
                  norm as u64
                )));
              }
            } else if visited_docs.get(doc_index)? {
              return Err(LuceneError::corrupt_index(format!(
                "Document {doc} has terms according to postings but its norm value is 0, which may only be used on documents that have no terms"
              )));
            }
          }
          let mut expected_count = 0;
          let mut doc = visited_docs.next_set_bit(0);
          while doc != NO_MORE_DOCS as usize {
            if match live_docs {
              Some(live_docs) => live_docs.get(doc)?,
              None => true,
            } {
              expected_count += 1;
            }
            doc = if doc + 1 >= visited_docs.length() {
              NO_MORE_DOCS as usize
            } else {
              visited_docs.next_set_bit(doc + 1)
            };
          }
          if expected_count != actual_count {
            return Err(LuceneError::corrupt_index(format!(
              "actual norm count: {actual_count} but expected: {expected_count}"
            )));
          }
        }

        // Test seek to last term:
        if let Some(last_term) = last_term.as_ref() {
          if terms_enum.seek_ceil(last_term.get_bytes_ref())? != SeekStatus::Found {
            return Err(LuceneError::corrupt_index(format!(
              "seek to last term {} failed",
              last_term.get_bytes_ref()
            )));
          }
          let current_term = terms_enum.term()?.into_owned();
          if &current_term != last_term.get_bytes_ref() {
            return Err(LuceneError::corrupt_index(format!(
              "seek to last term {} returned FOUND but seeked to the wrong term {current_term}",
              last_term.get_bytes_ref()
            )));
          }

          let expected_doc_freq = terms_enum.doc_freq()?;
          let mut docs = terms_enum.postings_with_flags(None, NONE as i32)?;
          let mut doc_freq = 0;
          while docs.next_doc()? != NO_MORE_DOCS {
            doc_freq += 1;
          }
          if doc_freq != expected_doc_freq {
            return Err(LuceneError::corrupt_index(format!(
              "docFreq for last term {}={expected_doc_freq} != recomputed docFreq={doc_freq}",
              last_term.get_bytes_ref()
            )));
          }
        }

        // check unique term count
        let mut term_count = -1;
        if field_term_count > 0 {
          term_count = field_terms.size()?;
          if term_count != -1 && term_count != field_term_count {
            return Err(LuceneError::corrupt_index(format!(
              "termCount mismatch {term_count} vs {field_term_count}"
            )));
          }
        }

        // Test seeking by ord
        if has_ord && status.term_count - term_count_start > 0 {
          let seek_count = std::cmp::min(10000, term_count);
          if seek_count > 0 {
            let seek_count_usize: usize = seek_count.try_convert()?;
            let mut seek_terms = vec![BytesRef::<Vec<u8>>::new(); seek_count_usize];

            // Seek by ord
            for i in (0..seek_count_usize).rev() {
              let ord = (i as i64) * (term_count / seek_count);
              terms_enum.seek_exact_with_ord(ord)?;
              let actual_ord = terms_enum.ord()?;
              if actual_ord != ord {
                return Err(LuceneError::corrupt_index(format!(
                  "seek to ord {ord} returned ord {actual_ord}"
                )));
              }
              seek_terms[i] = terms_enum.term()?.into_owned();
            }

            // Seek by term
            for i in (0..seek_count_usize).rev() {
              if terms_enum.seek_ceil(&seek_terms[i])? != SeekStatus::Found {
                return Err(LuceneError::corrupt_index(format!(
                  "seek to existing term {} failed",
                  seek_terms[i]
                )));
              }
              let current_term = terms_enum.term()?.into_owned();
              if current_term != seek_terms[i] {
                return Err(LuceneError::corrupt_index(format!(
                  "seek to existing term {} returned FOUND but seeked to the wrong term {current_term}",
                  seek_terms[i]
                )));
              }

              postings = Some(terms_enum.postings_with_flags(postings.take(), NONE as i32)?);
            }
          }
        }

        // Test Terms#intersect
        // An automaton that should match a good number of terms
        let any_binary_1 = Automata::make_any_binary()?;
        let char_range = Automata::make_char_range('a' as i32, 'e' as i32)?;
        let any_binary_2 = Automata::make_any_binary()?;
        let automaton =
          Operations::concatenate_with_list(&[&any_binary_1, &char_range, &any_binary_2])?;
        Self::check_terms_intersect(&terms, automaton.clone(), None)?;

        let start_term = BytesRef::<Vec<u8>>::new();
        Self::check_terms_intersect(&terms, automaton, Some(&start_term))?;

        let automaton = Automata::make_non_empty_binary()?;
        let start_term = BytesRef::from_bytes(vec![b'l']);
        Self::check_terms_intersect(&terms, automaton.clone(), Some(&start_term))?;

        // a term that likely compares greater than every other term in the dictionary
        let start_term = BytesRef::from_bytes(vec![0xff, 0xff, 0xff, 0xff]);
        Self::check_terms_intersect(&terms, automaton, Some(&start_term))?;
      } else {
        // Unusual: the FieldsEnum returned a field but
        // the Terms for that field is null; this should
        // only happen if it's a ghost field (field with
        // no terms, e.g. there used to be terms but all
        // docs got deleted and then merged away):
      }
    }

    let field_count = fields.size()?;
    if field_count != -1 {
      if field_count < 0 {
        return Err(LuceneError::corrupt_index(format!(
          "invalid fieldCount: {field_count}"
        )));
      }
      if field_count != computed_field_count {
        return Err(LuceneError::corrupt_index(format!(
          "fieldCount mismatch {field_count} vs recomputed field count {computed_field_count}"
        )));
      }
    }

    if let Some(start) = start {
      Self::msg(
        info_stream.as_deref_mut(),
        &format!(
          "OK [{} terms; {} terms/docs pairs; {} tokens] [took {:.3} sec]",
          status.term_count,
          status.tot_freq,
          status.tot_pos,
          ns_to_sec(start.elapsed().as_nanos())
        ),
      )?;
    }

    if verbose
      && status.term_count > 0
      && let (Some(block_tree_stats), Some(info_stream)) =
        (status.block_tree_stats.as_ref(), info_stream)
    {
      for (field, stats) in block_tree_stats {
        writeln!(info_stream, "      field \"{field}\":")?;
        writeln!(info_stream, "      {}", stats.replace('\n', "\n      "))?;
      }
    }

    Ok(status)
  }

  fn check_terms_intersect<T>(
    terms: &T,
    automaton: Automaton,
    start_term: Option<&BytesRef<Vec<u8>>>,
  ) -> Result<()>
  where
    T: Terms,
  {
    let mut all_terms = terms.iterator()?;
    let automaton =
      Operations::determinize(&automaton, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?.into_owned();
    let compiled_automaton = CompiledAutomaton::with_binary(automaton.clone(), false, true, true)?;
    let mut run_automaton = ByteRunAutomaton::with_bool(automaton, true)?;
    let mut filtered_terms = terms.intersect(&compiled_automaton, start_term)?;
    let mut term = if let Some(start_term) = start_term {
      match all_terms.seek_ceil(start_term)? {
        SeekStatus::Found => all_terms.next()?.map(Cow::into_owned),
        SeekStatus::NotFound => Some(all_terms.term()?.into_owned()),
        SeekStatus::End => None,
      }
    } else {
      all_terms.next()?.map(Cow::into_owned)
    };
    while let Some(current_term) = term {
      if run_automaton.run(
        &current_term.bytes,
        current_term.offset,
        current_term.length,
      )? {
        let filtered_term = filtered_terms.next()?.map(Cow::into_owned);
        if filtered_term.as_ref() != Some(&current_term) {
          return Err(LuceneError::corrupt_index(format!(
            "Expected next filtered term: {current_term}, but got {filtered_term:?}"
          )));
        }
      }
      term = all_terms.next()?.map(Cow::into_owned);
    }
    let filtered_term = filtered_terms.next()?.map(Cow::into_owned);
    if filtered_term.is_some() {
      return Err(LuceneError::corrupt_index(format!(
        "Expected exhausted TermsEnum, but got {filtered_term:?}"
      )));
    }
    Ok(())
  }

  /// For use in tests only.
  pub(crate) fn check_impacts<I>(impacts: &I, last_target: i32) -> Result<()>
  where
    I: Impacts,
  {
    let num_levels = impacts.num_levels();
    if num_levels < 1 {
      return Err(LuceneError::corrupt_index(format!(
        "The number of impact levels must be >= 1, got {num_levels}"
      )));
    }

    let doc_id_up_to_0 = impacts.get_doc_id_upto(0);
    if doc_id_up_to_0 < last_target {
      return Err(LuceneError::corrupt_index(format!(
        "getDocIdUpTo returned {doc_id_up_to_0} on level 0, which is less than the target {last_target}"
      )));
    }

    for impacts_level in 1..num_levels {
      let doc_id_up_to = impacts.get_doc_id_upto(impacts_level);
      let previous_doc_id_up_to = impacts.get_doc_id_upto(impacts_level - 1);
      if doc_id_up_to < previous_doc_id_up_to {
        return Err(LuceneError::corrupt_index(format!(
          "Decreasing return for getDocIdUpTo: level {} returned {previous_doc_id_up_to} but level {impacts_level} returned {doc_id_up_to} for target {last_target}",
          impacts_level - 1
        )));
      }
    }

    for impacts_level in 0..num_levels {
      let per_level_impacts = impacts.get_impacts(impacts_level)?;
      if per_level_impacts.is_empty() {
        return Err(LuceneError::corrupt_index(format!(
          "Got empty list of impacts on level {impacts_level}"
        )));
      }
      let first = &per_level_impacts[0];
      if first.freq < 1 {
        return Err(LuceneError::corrupt_index(format!(
          "First impact had a freq <= 0: {first}"
        )));
      }
      if first.norm == 0 {
        return Err(LuceneError::corrupt_index(format!(
          "First impact had a norm == 0: {first}"
        )));
      }
      // Impacts must be in increasing order of norm AND freq
      let previous = first;
      for impact in per_level_impacts.iter().skip(1) {
        if impact.freq <= previous.freq || (impact.norm as u64) <= previous.norm as u64 {
          return Err(LuceneError::corrupt_index(format!(
            "Impacts are not ordered or contain dups, got {previous} then {impact}"
          )));
        }
      }
      if impacts_level > 0 {
        // Make sure that impacts at level N trigger better scores than an impactsLevel N-1
        let previous_level_impacts = impacts.get_impacts(impacts_level - 1)?;
        let mut previous_iterator = previous_level_impacts.iter();
        previous_iterator
          .next()
          .expect("the previous impacts level is not empty");
        let mut iterator = per_level_impacts.iter();
        let mut impact = iterator
          .next()
          .expect("the current impacts level is not empty");
        for previous in previous_iterator {
          if previous.freq <= impact.freq && (previous.norm as u64) >= impact.norm as u64 {
            // previous triggers a lower score than the current impact, all good
            continue;
          }
          let Some(next_impact) = iterator.next() else {
            return Err(LuceneError::corrupt_index(format!(
              "Found impact {previous} on level {} but no impact on level {impacts_level} triggers a better score: {per_level_impacts:?}",
              impacts_level - 1
            )));
          };
          impact = next_impact;
        }
      }
    }
    Ok(())
  }

  /// Test the term index.
  #[allow(clippy::field_reassign_with_default)]
  pub fn test_postings<R, O>(
    reader: &R,
    mut info_stream: Option<&mut O>,
    verbose: bool,
    level: i32,
    fail_fast: bool,
  ) -> Result<TermIndexStatus>
  where
    R: CodecReader,
    O: Write,
  {
    // TODO: we should go and verify term vectors match, if the Level is high enough to
    // include slow checks
    let max_doc = reader.max_doc()?;
    let mut fields_reader = None;
    let mut merge_fields_reader = None;
    let mut norms_reader = None;
    let mut merge_norms_reader = None;

    let check_result = panic::catch_unwind(AssertUnwindSafe(|| -> Result<TermIndexStatus> {
      if let Some(info_stream) = info_stream.as_deref_mut() {
        write!(info_stream, "    test: terms, freq, prox...")?;
      }

      fields_reader = reader.get_postings_reader()?;
      let Some(fields) = fields_reader.as_ref() else {
        return Ok(TermIndexStatus::default());
      };
      merge_fields_reader = fields.get_merge_instance()?;
      let fields = merge_fields_reader
        .as_ref()
        .or(fields_reader.as_ref())
        .expect("the postings reader is present");

      let field_infos = reader.get_field_infos()?;
      norms_reader = reader.get_norms_reader()?;
      if let Some(norms_reader) = norms_reader.as_ref() {
        merge_norms_reader = norms_reader.get_merge_instance()?;
      }
      let norms_producer = merge_norms_reader.as_ref().or(norms_reader.as_ref());
      let live_docs = reader.get_live_docs()?;
      Self::check_fields(
        fields,
        live_docs.as_ref(),
        max_doc,
        field_infos.as_ref(),
        norms_producer,
        true,
        false,
        info_stream.as_deref_mut(),
        verbose,
        level,
      )
    }));

    let check_result = match check_result {
      Ok(check_result) => check_result,
      Err(payload) if fail_fast => panic::resume_unwind(payload),
      Err(payload) => Err(LuceneError::tragedy_from_panic(
        "CheckIndex postings test failed",
        payload.as_ref(),
      )),
    };

    match check_result {
      Ok(status) => Ok(status),
      Err(error) if fail_fast => Err(error),
      Err(error) => {
        Self::msg(info_stream.as_deref_mut(), &format!("ERROR: {error}"))?;
        if let Some(info_stream) = info_stream {
          writeln!(info_stream, "{error:?}")?;
        }
        let mut status = TermIndexStatus::default();
        status.error = Some(error);
        Ok(status)
      },
    }
  }

  /// Test the points index.
  pub fn test_points<R, O>(
    reader: &R,
    mut info_stream: Option<&mut O>,
    fail_fast: bool,
  ) -> Result<PointsStatus>
  where
    R: CodecReader,
    O: Write,
  {
    if let Some(info_stream) = info_stream.as_deref_mut() {
      write!(info_stream, "    test: points..............")?;
    }
    let start = Instant::now();
    let field_infos = reader.get_field_infos()?;
    let mut status = PointsStatus::default();
    let mut points_reader = None;

    let check_result = panic::catch_unwind(AssertUnwindSafe(|| -> Result<()> {
      if field_infos.has_point_values() {
        points_reader = reader.get_points_reader()?;
        let points_reader = points_reader.as_ref().ok_or_else(|| {
          LuceneError::corrupt_index(
            "there are fields with points, but reader.getPointsReader() is null",
          )
        })?;
        for field_info in field_infos.iter() {
          if field_info.get_point_dimension_count() > 0 {
            let Some(values) = points_reader.get_values(&field_info.name)? else {
              continue;
            };

            status.total_value_fields += 1;

            let size: i64 = values.size()?.try_convert()?;
            let doc_count = values.get_doc_count()?;

            let cross_cost = values.estimate_point_count(&ConstantRelationIntersectVisitor {
              relation: Relation::CellCrossesQuery,
            })?;
            if cross_cost < size / 2 {
              return Err(LuceneError::corrupt_index(
                "estimatePointCount should return >= size/2 when all cells match",
              ));
            }
            let inside_cost = values.estimate_point_count(&ConstantRelationIntersectVisitor {
              relation: Relation::CellInsideQuery,
            })?;
            if inside_cost < size {
              return Err(LuceneError::corrupt_index(
                "estimatePointCount should return >= size when all cells fully match",
              ));
            }
            let outside_cost = values.estimate_point_count(&ConstantRelationIntersectVisitor {
              relation: Relation::CellOutsideQuery,
            })?;
            if outside_cost != 0 {
              return Err(LuceneError::corrupt_index(
                "estimatePointCount should return 0 when no cells match",
              ));
            }

            let mut visitor =
              VerifyPointsVisitor::new(field_info.name.clone(), reader.max_doc()?, &values)?;
            values.intersect(&mut visitor)?;

            if visitor.get_point_count_seen() != size {
              return Err(LuceneError::corrupt_index(format!(
                "point values for field \"{}\" claims to have size={} points, but in fact has {}",
                field_info.name,
                size,
                visitor.get_point_count_seen()
              )));
            }

            if visitor.get_doc_count_seen() != i64::from(doc_count) {
              return Err(LuceneError::corrupt_index(format!(
                "point values for field \"{}\" claims to have docCount={} but in fact has {}",
                field_info.name,
                doc_count,
                visitor.get_doc_count_seen()
              )));
            }

            status.total_value_points += visitor.get_point_count_seen();
          }
        }
      }

      Self::msg(
        info_stream.as_deref_mut(),
        &format!(
          "OK [{} fields, {} points] [took {:.3} sec]",
          status.total_value_fields,
          status.total_value_points,
          ns_to_sec(start.elapsed().as_nanos())
        ),
      )?;
      Ok(())
    }));
    let check_result = match check_result {
      Ok(check_result) => check_result,
      Err(payload) if fail_fast => panic::resume_unwind(payload),
      Err(payload) => Err(LuceneError::tragedy_from_panic(
        "CheckIndex points test failed",
        payload.as_ref(),
      )),
    };

    if let Err(error) = check_result {
      if fail_fast {
        return Err(error);
      }
      Self::msg(info_stream.as_deref_mut(), &format!("ERROR: {error}"))?;
      if let Some(info_stream) = info_stream {
        writeln!(info_stream, "{error:?}")?;
      }
      status.error = Some(error);
    }

    Ok(status)
  }
}

impl CheckIndex<DirectoryEnum, LockEnum, Sink> {
  /// Test vector values.
  pub fn test_vectors<R, O>(
    reader: &R,
    mut info_stream: Option<&mut O>,
    fail_fast: bool,
  ) -> Result<VectorValuesStatus>
  where
    R: CodecReader,
    O: Write,
  {
    if let Some(info_stream) = info_stream.as_deref_mut() {
      write!(info_stream, "    test: vectors.............")?;
    }
    let start = Instant::now();
    let field_infos = reader.get_field_infos()?;
    let mut status = VectorValuesStatus::default();
    let check_result = panic::catch_unwind(AssertUnwindSafe(|| -> Result<()> {
      if field_infos.has_vector_values() {
        for field_info in field_infos.iter() {
          if field_info.has_vector_values() {
            let dimension = field_info.get_vector_dimension();
            if dimension <= 0 {
              return Err(LuceneError::corrupt_index(format!(
                "Field \"{}\" has vector values but dimension is {dimension}",
                field_info.name
              )));
            }
            let float_vector_values =
              CodecReader::get_float_vector_values(reader, &field_info.name)?;
            let byte_vector_values = CodecReader::get_byte_vector_values(reader, &field_info.name)?;
            if float_vector_values.is_none() && byte_vector_values.is_none() {
              continue;
            }

            status.total_knn_vector_fields += 1;
            match field_info.get_vector_encoding() {
              VectorEncoding::BYTE(_) => Self::check_byte_vector_values(
                byte_vector_values.ok_or_else(|| {
                  LuceneError::corrupt_index(format!(
                    "Field \"{}\" has BYTE vector encoding but getByteVectorValues returned null",
                    field_info.name
                  ))
                })?,
                field_info,
                &mut status,
                reader,
              )?,
              VectorEncoding::FLOAT32(_) => Self::check_float_vector_values(
                float_vector_values.ok_or_else(|| {
                  LuceneError::corrupt_index(format!(
                    "Field \"{}\" has FLOAT32 vector encoding but getFloatVectorValues returned null",
                    field_info.name
                  ))
                })?,
                field_info,
                &mut status,
                reader,
              )?,
            }
          }
        }
      }
      Self::msg(
        info_stream.as_deref_mut(),
        &format!(
          "OK [{} fields, {} vectors] [took {:.3} sec]",
          status.total_knn_vector_fields,
          status.total_vector_values,
          ns_to_sec(start.elapsed().as_nanos())
        ),
      )?;
      Ok(())
    }));

    let check_result = match check_result {
      Ok(check_result) => check_result,
      Err(payload) if fail_fast => panic::resume_unwind(payload),
      Err(payload) => Err(LuceneError::tragedy_from_panic(
        "CheckIndex vectors test failed",
        payload.as_ref(),
      )),
    };
    if let Err(error) = check_result {
      if fail_fast {
        return Err(error);
      }
      Self::msg(info_stream.as_deref_mut(), &format!("ERROR: {error}"))?;
      if let Some(info_stream) = info_stream {
        writeln!(info_stream, "{error:?}")?;
      }
      status.error = Some(error);
    }
    Ok(status)
  }

  fn vectors_reader_supports_search<R>(codec_reader: &R, field_name: &str) -> Result<bool>
  where
    R: CodecReader,
  {
    Ok(
      codec_reader
        .get_vector_reader()?
        .is_some_and(|reader| !reader.is_flat_vectors_reader(field_name)),
    )
  }

  fn check_float_vector_values<V, R>(
    values: V,
    field_info: &Arc<crate::core::index::field_info::FieldInfo>,
    status: &mut VectorValuesStatus,
    codec_reader: &R,
  ) -> Result<()>
  where
    V: FloatVectorValues,
    R: CodecReader,
  {
    let mut count = 0;
    let every_n_doc = std::cmp::max(values.size() / 64, 1);
    while count < values.size() {
      // search the first maxNumSearches vectors to exercise the graph
      if values.ord_to_doc(count)? % every_n_doc == 0 {
        let mut collector = TopKnnCollector::new(10, usize::MAX)?;
        if Self::vectors_reader_supports_search(codec_reader, &field_info.name)? {
          let vector = values.vector_value(count)?;
          let vector = match vector.as_ref() {
            crate::core::codecs::knn_field_vectors_writer::VectorValueEnum::Float(vector) => {
              vector.clone()
            },
            crate::core::codecs::knn_field_vectors_writer::VectorValueEnum::Byte(_) => {
              return Err(LuceneError::corrupt_index(format!(
                "Field \"{}\" has FLOAT32 vector encoding but returned a byte vector",
                field_info.name
              )));
            },
          };
          CodecReader::search_nearest_vectors_f32(
            codec_reader,
            &field_info.name,
            vector,
            &mut collector,
            None::<<R as LeafReader>::Bits>,
          )?;
          let docs = collector.top_docs()?;
          if docs.score_docs.is_empty() {
            return Err(LuceneError::corrupt_index(format!(
              "Field \"{}\" failed to search k nearest neighbors",
              field_info.name
            )));
          }
        }
      }
      let value_length = values.vector_value(count)?.len();
      let vector_dimension: usize = field_info.get_vector_dimension().try_convert()?;
      if value_length != vector_dimension {
        return Err(LuceneError::corrupt_index(format!(
          "Field \"{}\" has a value whose dimension={value_length} not matching the field's dimension={}",
          field_info.name,
          field_info.get_vector_dimension()
        )));
      }
      count += 1;
    }
    if count != values.size() {
      return Err(LuceneError::corrupt_index(format!(
        "Field \"{}\" has size={} but when iterated, returns {count} docs with values",
        field_info.name,
        values.size()
      )));
    }
    let count: i64 = count.try_convert()?;
    status.total_vector_values += count;
    Ok(())
  }

  fn check_byte_vector_values<V, R>(
    values: V,
    field_info: &Arc<crate::core::index::field_info::FieldInfo>,
    status: &mut VectorValuesStatus,
    codec_reader: &R,
  ) -> Result<()>
  where
    V: ByteVectorValues,
    R: CodecReader,
  {
    let mut count = 0;
    let every_n_doc = std::cmp::max(values.size() / 64, 1);
    let supports_search = Self::vectors_reader_supports_search(codec_reader, &field_info.name)?;
    while count < values.size() {
      // search the first maxNumSearches vectors to exercise the graph
      if supports_search && values.ord_to_doc(count)? % every_n_doc == 0 {
        let mut collector = TopKnnCollector::new(10, usize::MAX)?;
        let vector = values.vector_value(count)?;
        let vector = match vector.as_ref() {
          crate::core::codecs::knn_field_vectors_writer::VectorValueEnum::Byte(vector) => {
            vector.clone()
          },
          crate::core::codecs::knn_field_vectors_writer::VectorValueEnum::Float(_) => {
            return Err(LuceneError::corrupt_index(format!(
              "Field \"{}\" has BYTE vector encoding but returned a float vector",
              field_info.name
            )));
          },
        };
        CodecReader::search_nearest_vectors_u8(
          codec_reader,
          &field_info.name,
          vector,
          &mut collector,
          None::<<R as LeafReader>::Bits>,
        )?;
        let docs = collector.top_docs()?;
        if docs.score_docs.is_empty() {
          return Err(LuceneError::corrupt_index(format!(
            "Field \"{}\" failed to search k nearest neighbors",
            field_info.name
          )));
        }
      }
      let value_length = values.vector_value(count)?.len();
      let vector_dimension: usize = field_info.get_vector_dimension().try_convert()?;
      if value_length != vector_dimension {
        return Err(LuceneError::corrupt_index(format!(
          "Field \"{}\" has a value whose dimension={value_length} not matching the field's dimension={}",
          field_info.name,
          field_info.get_vector_dimension()
        )));
      }
      count += 1;
    }
    if count != values.size() {
      return Err(LuceneError::corrupt_index(format!(
        "Field \"{}\" has size={} but when iterated, returns {count} docs with values",
        field_info.name,
        values.size()
      )));
    }
    let count: i64 = count.try_convert()?;
    status.total_vector_values += count;
    Ok(())
  }
}

/// Walks the entire N-dimensional points space, verifying that all points fall within the last
/// cell's boundaries.
///
/// This is an internal API.
pub struct VerifyPointsVisitor {
  point_count_seen: i64,
  last_doc_id: i32,
  docs_seen: FixedBitSet,
  last_min_packed_value: RefCell<Vec<u8>>,
  last_max_packed_value: RefCell<Vec<u8>>,
  last_packed_value: Vec<u8>,
  global_min_packed_value: Option<Vec<u8>>,
  global_max_packed_value: Option<Vec<u8>>,
  packed_bytes_count: usize,
  packed_index_bytes_count: usize,
  num_data_dims: usize,
  num_index_dims: usize,
  bytes_per_dim: usize,
  comparator: ByteArrayComparatorEnum,
  field_name: String,
}

impl VerifyPointsVisitor {
  /// Sole constructor
  pub fn new<P>(field_name: String, max_doc: i32, values: &P) -> Result<Self>
  where
    P: PointValues,
  {
    let num_data_dims = values.get_num_dimensions()?;
    let num_index_dims = values.get_num_index_dimensions()?;
    let bytes_per_dim = values.get_bytes_per_dimension()?;
    let comparator = ArrayUtil::get_unsigned_comparator(bytes_per_dim);
    let packed_bytes_count = num_data_dims * bytes_per_dim;
    let packed_index_bytes_count = num_index_dims * bytes_per_dim;
    let global_min_packed_value = values
      .get_min_packed_value()?
      .map(|packed_value| packed_value.into_owned());
    let global_max_packed_value = values
      .get_max_packed_value()?
      .map(|packed_value| packed_value.into_owned());
    let docs_seen = FixedBitSet::new(max_doc.try_convert()?);
    let last_min_packed_value = RefCell::new(vec![0; packed_index_bytes_count]);
    let last_max_packed_value = RefCell::new(vec![0; packed_index_bytes_count]);
    let last_packed_value = vec![0; packed_bytes_count];

    let doc_count = values.get_doc_count()?;
    let size = values.size()?;
    let size_as_i64: i64 = size.try_convert()?;
    if i64::from(doc_count) > size_as_i64 {
      return Err(LuceneError::corrupt_index(format!(
        "point values for field \"{field_name}\" claims to have size={size} points and inconsistent docCount={doc_count}"
      )));
    }

    if doc_count > max_doc {
      return Err(LuceneError::corrupt_index(format!(
        "point values for field \"{field_name}\" claims to have docCount={doc_count} but that's greater than maxDoc={max_doc}"
      )));
    }

    if global_min_packed_value.is_none() {
      if size != 0 {
        return Err(LuceneError::corrupt_index(format!(
          "getMinPackedValue is null points for field \"{field_name}\" yet size={size}"
        )));
      }
    } else if global_min_packed_value.as_ref().map(Vec::len) != Some(packed_index_bytes_count) {
      return Err(LuceneError::corrupt_index(format!(
        "getMinPackedValue for field \"{field_name}\" return length={} array, but should be {packed_bytes_count}",
        global_min_packed_value.as_ref().map_or(0, Vec::len)
      )));
    }
    if global_max_packed_value.is_none() {
      if size != 0 {
        return Err(LuceneError::corrupt_index(format!(
          "getMaxPackedValue is null points for field \"{field_name}\" yet size={size}"
        )));
      }
    } else if global_max_packed_value.as_ref().map(Vec::len) != Some(packed_index_bytes_count) {
      return Err(LuceneError::corrupt_index(format!(
        "getMaxPackedValue for field \"{field_name}\" return length={} array, but should be {packed_bytes_count}",
        global_max_packed_value.as_ref().map_or(0, Vec::len)
      )));
    }

    Ok(Self {
      point_count_seen: 0,
      last_doc_id: -1,
      docs_seen,
      last_min_packed_value,
      last_max_packed_value,
      last_packed_value,
      global_min_packed_value,
      global_max_packed_value,
      packed_bytes_count,
      packed_index_bytes_count,
      num_data_dims,
      num_index_dims,
      bytes_per_dim,
      comparator,
      field_name,
    })
  }

  /// Returns total number of points in this BKD tree
  pub fn get_point_count_seen(&self) -> i64 {
    self.point_count_seen
  }

  /// Returns total number of unique docIDs in this BKD tree
  pub fn get_doc_count_seen(&self) -> i64 {
    self.docs_seen.cardinality() as i64
  }
}

impl IntersectVisitor for VerifyPointsVisitor {
  fn visit(&mut self, doc_id: i32) -> Result<()> {
    Err(LuceneError::corrupt_index(format!(
      "codec called IntersectVisitor.visit without a packed value for docID={doc_id}"
    )))
  }

  fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()> {
    self.check_packed_value("packed value", packed_value, doc_id)?;
    self.point_count_seen += 1;
    let doc_index: usize = doc_id.try_convert()?;
    CoreHelper::check_index(doc_index, self.docs_seen.length())?;
    self.docs_seen.set(doc_index);

    {
      let last_min_packed_value = self.last_min_packed_value.borrow();
      let last_max_packed_value = self.last_max_packed_value.borrow();
      for dim in 0..self.num_index_dims {
        let offset = self.bytes_per_dim * dim;

        // Compare to last cell:
        if self.comparator.compare(
          packed_value,
          offset,
          last_min_packed_value.as_slice(),
          offset,
        ) < 0
        {
          // This doc's point, in this dimension, is lower than the minimum value of the last cell
          // checked:
          return Err(LuceneError::corrupt_index(format!(
            "packed points value {packed_value:?} for field=\"{}\", docID={doc_id} is out-of-bounds of the last cell min={:?} max={:?} dim={dim}",
            self.field_name,
            last_min_packed_value.as_slice(),
            last_max_packed_value.as_slice()
          )));
        }

        if self.comparator.compare(
          packed_value,
          offset,
          last_max_packed_value.as_slice(),
          offset,
        ) > 0
        {
          // This doc's point, in this dimension, is greater than the maximum value of the last cell
          // checked:
          return Err(LuceneError::corrupt_index(format!(
            "packed points value {packed_value:?} for field=\"{}\", docID={doc_id} is out-of-bounds of the last cell min={:?} max={:?} dim={dim}",
            self.field_name,
            last_min_packed_value.as_slice(),
            last_max_packed_value.as_slice()
          )));
        }
      }
    }

    // In the 1D data case, PointValues must make a single in-order sweep through all values, and
    // tie-break by
    // increasing docID:
    // for data dimension > 1, leaves are sorted by the dimension with the lowest cardinality to
    // improve block compression
    if self.num_data_dims == 1 {
      let cmp = self
        .comparator
        .compare(&self.last_packed_value, 0, packed_value, 0);
      if cmp > 0 {
        return Err(LuceneError::corrupt_index(format!(
          "packed points value {packed_value:?} for field=\"{}\", for docID={doc_id} is out-of-order vs the previous document's value {:?}",
          self.field_name, self.last_packed_value
        )));
      } else if cmp == 0 && doc_id < self.last_doc_id {
        return Err(LuceneError::corrupt_index(format!(
          "packed points value is the same, but docID={doc_id} is out of order vs previous docID={}, field=\"{}\"",
          self.last_doc_id, self.field_name
        )));
      }
      self.last_packed_value[..self.bytes_per_dim]
        .copy_from_slice(&packed_value[..self.bytes_per_dim]);
      self.last_doc_id = doc_id;
    }
    Ok(())
  }

  fn compare(&self, min_packed_value: &[u8], max_packed_value: &[u8]) -> Result<Relation> {
    self.check_packed_value("min packed value", min_packed_value, -1)?;
    self
      .last_min_packed_value
      .borrow_mut()
      .copy_from_slice(min_packed_value);
    self.check_packed_value("max packed value", max_packed_value, -1)?;
    self
      .last_max_packed_value
      .borrow_mut()
      .copy_from_slice(max_packed_value);

    let global_min_packed_value = self.global_min_packed_value.as_deref().ok_or_else(|| {
      LuceneError::corrupt_index(format!(
        "getMinPackedValue is null points for field \"{}\"",
        self.field_name
      ))
    })?;
    let global_max_packed_value = self.global_max_packed_value.as_deref().ok_or_else(|| {
      LuceneError::corrupt_index(format!(
        "getMaxPackedValue is null points for field \"{}\"",
        self.field_name
      ))
    })?;

    for dim in 0..self.num_index_dims {
      let offset = self.bytes_per_dim * dim;

      if self
        .comparator
        .compare(min_packed_value, offset, max_packed_value, offset)
        > 0
      {
        return Err(LuceneError::corrupt_index(format!(
          "packed points cell minPackedValue {min_packed_value:?} is out-of-bounds of the cell's maxPackedValue {max_packed_value:?} dim={dim} field=\"{}\"",
          self.field_name
        )));
      }

      // Make sure this cell is not outside the global min/max:
      if self
        .comparator
        .compare(min_packed_value, offset, global_min_packed_value, offset)
        < 0
      {
        return Err(LuceneError::corrupt_index(format!(
          "packed points cell minPackedValue {min_packed_value:?} is out-of-bounds of the global minimum {global_min_packed_value:?} dim={dim} field=\"{}\"",
          self.field_name
        )));
      }

      if self
        .comparator
        .compare(max_packed_value, offset, global_min_packed_value, offset)
        < 0
      {
        return Err(LuceneError::corrupt_index(format!(
          "packed points cell maxPackedValue {max_packed_value:?} is out-of-bounds of the global minimum {global_min_packed_value:?} dim={dim} field=\"{}\"",
          self.field_name
        )));
      }

      if self
        .comparator
        .compare(min_packed_value, offset, global_max_packed_value, offset)
        > 0
      {
        return Err(LuceneError::corrupt_index(format!(
          "packed points cell minPackedValue {min_packed_value:?} is out-of-bounds of the global maximum {global_max_packed_value:?} dim={dim} field=\"{}\"",
          self.field_name
        )));
      }
      if self
        .comparator
        .compare(max_packed_value, offset, global_max_packed_value, offset)
        > 0
      {
        return Err(LuceneError::corrupt_index(format!(
          "packed points cell maxPackedValue {max_packed_value:?} is out-of-bounds of the global maximum {global_max_packed_value:?} dim={dim} field=\"{}\"",
          self.field_name
        )));
      }
    }

    // We always pretend the query shape is so complex that it crosses every cell, so
    // that packedValue is passed for every document
    Ok(Relation::CellCrossesQuery)
  }
}

impl VerifyPointsVisitor {
  fn check_packed_value(&self, desc: &str, packed_value: &[u8], doc_id: i32) -> Result<()> {
    let expected_length = if doc_id < 0 {
      self.packed_index_bytes_count
    } else {
      self.packed_bytes_count
    };
    if packed_value.len() != expected_length {
      return Err(LuceneError::corrupt_index(format!(
        "{desc} has incorrect length={} vs expected={} for docID={doc_id} field=\"{}\"",
        packed_value.len(),
        expected_length,
        self.field_name
      )));
    }
    Ok(())
  }
}

struct ConstantRelationIntersectVisitor {
  relation: Relation,
}

impl IntersectVisitor for ConstantRelationIntersectVisitor {
  fn visit(&mut self, _doc_id: i32) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn visit_with_packed_value(&mut self, _doc_id: i32, _packed_value: &[u8]) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn compare(&self, _min_packed_value: &[u8], _max_packed_value: &[u8]) -> Result<Relation> {
    Ok(self.relation)
  }
}

impl CheckIndex<DirectoryEnum, LockEnum, Sink> {
  /// Test stored fields.
  pub fn test_stored_fields<R, O>(
    reader: &R,
    mut info_stream: Option<&mut O>,
    fail_fast: bool,
  ) -> Result<StoredFieldStatus>
  where
    R: CodecReader,
    O: Write,
  {
    let start = Instant::now();
    let mut status = StoredFieldStatus::default();
    let mut stored_fields_reader = None;
    let mut merge_stored_fields_reader = None;

    let check_result = panic::catch_unwind(AssertUnwindSafe(|| -> Result<()> {
      if let Some(info_stream) = info_stream.as_deref_mut() {
        write!(info_stream, "    test: stored fields.......")?;
      }

      // Scan stored fields for all documents
      let live_docs = reader.get_live_docs()?;
      stored_fields_reader = reader.get_fields_reader()?;
      let fields_reader = stored_fields_reader
        .as_ref()
        .ok_or_else(|| LuceneError::corrupt_index("reader.getFieldsReader() is null"))?;
      merge_stored_fields_reader = fields_reader.get_merge_instance()?;
      let stored_fields = merge_stored_fields_reader
        .as_mut()
        .or(stored_fields_reader.as_mut())
        .expect("the stored fields reader is present");
      for j in 0..reader.max_doc()? {
        // Intentionally pull even deleted documents to
        // make sure they too are not corrupt:
        let mut visitor = DocumentStoredFieldVisitor::new();
        if (j & 0x03) == 0 {
          stored_fields.prefetch(j)?;
        }
        let mut dummy_writer = DummyStoredFieldsWriter;
        stored_fields.document_with_visitor(j, &mut visitor, Some(&mut dummy_writer))?;
        let doc = visitor.get_document_ref();
        let doc_index: usize = j.try_convert()?;
        if match live_docs.as_ref() {
          Some(live_docs) => live_docs.get(doc_index)?,
          None => true,
        } {
          status.doc_count += 1;
          let field_count: i64 = doc.get_fields().len().try_convert()?;
          status.tot_fields += field_count;
        }
      }

      // Validate docCount
      if status.doc_count != reader.num_docs()? {
        return Err(LuceneError::corrupt_index(format!(
          "docCount={} but saw {} undeleted docs",
          status.doc_count, status.doc_count
        )));
      }

      Self::msg(
        info_stream.as_deref_mut(),
        &format!(
          "OK [{} total field count; avg {:.1} fields per doc] [took {:.3} sec]",
          status.tot_fields,
          status.tot_fields as f32 / status.doc_count as f32,
          ns_to_sec(start.elapsed().as_nanos())
        ),
      )?;
      Ok(())
    }));

    let check_result = match check_result {
      Ok(check_result) => check_result,
      Err(payload) if fail_fast => panic::resume_unwind(payload),
      Err(payload) => Err(LuceneError::tragedy_from_panic(
        "CheckIndex stored fields test failed",
        payload.as_ref(),
      )),
    };

    if let Err(error) = check_result {
      if fail_fast {
        return Err(error);
      }
      Self::msg(info_stream.as_deref_mut(), &format!("ERROR [{error}]"))?;
      if let Some(info_stream) = info_stream {
        writeln!(info_stream, "{error:?}")?;
      }
      status.error = Some(error);
    }

    Ok(status)
  }

  /// Test docvalues.
  pub fn test_doc_values<R, O>(
    reader: &R,
    mut info_stream: Option<&mut O>,
    fail_fast: bool,
  ) -> Result<DocValuesStatus>
  where
    R: CodecReader,
    O: Write,
  {
    let start = Instant::now();
    let mut status = DocValuesStatus::default();
    let mut doc_values_reader = None;
    let mut merge_doc_values_reader = None;

    let check_result = panic::catch_unwind(AssertUnwindSafe(|| -> Result<()> {
      if let Some(info_stream) = info_stream.as_deref_mut() {
        write!(info_stream, "    test: docvalues...........")?;
      }
      doc_values_reader = reader.get_doc_values_reader()?;
      if let Some(doc_values_reader) = doc_values_reader.as_ref() {
        merge_doc_values_reader = doc_values_reader.get_merge_instance()?;
      }
      let doc_values_reader = merge_doc_values_reader
        .as_ref()
        .or(doc_values_reader.as_ref());
      for field_info in reader.get_field_infos()?.iter() {
        if *field_info.get_doc_values_type() != DocValuesType::None {
          status.total_value_fields += 1;
          let doc_values_reader = doc_values_reader.ok_or_else(|| {
            LuceneError::corrupt_index(format!(
              "field \"{}\" has doc values but reader.getDocValuesReader() is null",
              field_info.name
            ))
          })?;
          Self::check_doc_values(field_info, doc_values_reader, &mut status)?;
        }
      }

      Self::msg(
        info_stream.as_deref_mut(),
        &format!(
          "OK [{} docvalues fields; {} BINARY; {} NUMERIC; {} SORTED; {} SORTED_NUMERIC; {} SORTED_SET; {} SKIPPING INDEX] [took {:.3} sec]",
          status.total_value_fields,
          status.total_binary_fields,
          status.total_numeric_fields,
          status.total_sorted_fields,
          status.total_sorted_numeric_fields,
          status.total_sorted_set_fields,
          status.total_skipping_index,
          ns_to_sec(start.elapsed().as_nanos())
        ),
      )?;
      Ok(())
    }));

    let check_result = match check_result {
      Ok(check_result) => check_result,
      Err(payload) if fail_fast => panic::resume_unwind(payload),
      Err(payload) => Err(LuceneError::tragedy_from_panic(
        "CheckIndex docvalues test failed",
        payload.as_ref(),
      )),
    };

    if let Err(error) = check_result {
      if fail_fast {
        return Err(error);
      }
      Self::msg(info_stream.as_deref_mut(), &format!("ERROR [{error}]"))?;
      if let Some(info_stream) = info_stream {
        writeln!(info_stream, "{error:?}")?;
      }
      status.error = Some(error);
    }
    Ok(status)
  }

  fn check_doc_value_skipper<S>(
    field_info: &Arc<crate::core::index::field_info::FieldInfo>,
    mut skipper: S,
  ) -> Result<()>
  where
    S: DocValuesSkipper,
  {
    let field_name = &field_info.name;
    if skipper.max_doc_id_with_level(0) != -1 {
      return Err(LuceneError::corrupt_index(format!(
        "binary dv iterator for field: {field_name} should start at docID=-1, but got {}",
        skipper.max_doc_id_with_level(0)
      )));
    }
    if skipper.doc_count() > 0 && skipper.min_value() > skipper.max_value() {
      return Err(LuceneError::corrupt_index(format!(
        "skipper dv iterator for field: {field_name} reports wrong global value range, got  {} > {}",
        skipper.min_value(),
        skipper.max_value()
      )));
    }
    let mut doc_count = 0;
    loop {
      let doc = skipper.max_doc_id_with_level(0) + 1;
      skipper.advance(doc)?;
      if skipper.max_doc_id_with_level(0) == NO_MORE_DOCS {
        break;
      }
      if skipper.min_doc_id_with_level(0) < doc {
        return Err(LuceneError::corrupt_index(format!(
          "skipper dv iterator for field: {field_name} reports wrong minDocID, got {} < {doc}",
          skipper.min_doc_id_with_level(0)
        )));
      }
      for level in 0..skipper.num_levels() {
        if skipper.min_doc_id_with_level(level) > skipper.max_doc_id_with_level(level) {
          return Err(LuceneError::corrupt_index(format!(
            "skipper dv iterator for field: {field_name} reports wrong doc range, got {} > {}",
            skipper.min_doc_id_with_level(level),
            skipper.max_doc_id_with_level(level)
          )));
        }
        if skipper.min_value() > skipper.min_value_with_level(level) {
          return Err(LuceneError::corrupt_index(format!(
            "skipper dv iterator for field: {field_name} : global minValue  {} , got  {}",
            skipper.min_value(),
            skipper.min_value_with_level(level)
          )));
        }
        if skipper.max_value() < skipper.max_value_with_level(level) {
          return Err(LuceneError::corrupt_index(format!(
            "skipper dv iterator for field: {field_name} : global maxValue  {} , got  {}",
            skipper.max_value(),
            skipper.max_value_with_level(level)
          )));
        }
        if skipper.min_value_with_level(level) > skipper.max_value_with_level(level) {
          return Err(LuceneError::corrupt_index(format!(
            "skipper dv iterator for field: {field_name} reports wrong value range, got  {} > {}",
            skipper.min_value_with_level(level),
            skipper.max_value_with_level(level)
          )));
        }
      }
      doc_count += skipper.doc_count_with_level(0);
    }
    if skipper.doc_count() != doc_count {
      return Err(LuceneError::corrupt_index(format!(
        "skipper dv iterator for field: {field_name} inconsistent docCount, got {} != {doc_count}",
        skipper.doc_count()
      )));
    }
    Ok(())
  }

  fn check_dv_iterator<I, F>(
    field_info: &Arc<crate::core::index::field_info::FieldInfo>,
    mut producer: F,
  ) -> Result<()>
  where
    I: DocValuesIterator,
    F: FnMut(&Arc<crate::core::index::field_info::FieldInfo>) -> Result<I>,
  {
    let field = &field_info.name;

    // Check advance
    let mut iterator_1 = producer(field_info)?;
    let mut iterator_2 = producer(field_info)?;
    let mut i = 0;
    loop {
      let doc = iterator_1.next_doc()?;
      if i % 10 == 1 {
        let mut doc_2 = iterator_2.advance(doc - 1)?;
        if doc_2 < doc - 1 {
          return Err(LuceneError::corrupt_index(format!(
            "dv iterator field={field}: doc={} went backwords (got: {doc_2})",
            doc - 1
          )));
        }
        if doc_2 == doc - 1 {
          doc_2 = iterator_2.next_doc()?;
        }
        if doc_2 != doc {
          return Err(LuceneError::corrupt_index(format!(
            "dv iterator field={field}: doc={doc} was not found through advance() (got: {doc_2})"
          )));
        }
        if iterator_2.doc_id() != doc {
          return Err(LuceneError::corrupt_index(format!(
            "dv iterator field={field}: doc={doc} reports wrong doc ID (got: {})",
            iterator_2.doc_id()
          )));
        }
      }
      i += 1;
      if doc == NO_MORE_DOCS {
        break;
      }
    }

    // Check advanceExact
    let mut iterator_1 = producer(field_info)?;
    let mut iterator_2 = producer(field_info)?;
    let mut i = 0;
    let mut last_doc = -1;
    loop {
      let doc = iterator_1.next_doc()?;
      if doc == NO_MORE_DOCS {
        break;
      }
      if i % 13 == 1 {
        let found = iterator_2.advance_exact(doc - 1)?;
        if (doc - 1 == last_doc) != found {
          return Err(LuceneError::corrupt_index(format!(
            "dv iterator field={field}: doc={} disagrees about whether document exists (got: {found})",
            doc - 1
          )));
        }
        if iterator_2.doc_id() != doc - 1 {
          return Err(LuceneError::corrupt_index(format!(
            "dv iterator field={field}: doc={} reports wrong doc ID (got: {})",
            doc - 1,
            iterator_2.doc_id()
          )));
        }
        let found_2 = iterator_2.advance_exact(doc - 1)?;
        if found != found_2 {
          return Err(LuceneError::corrupt_index(format!(
            "dv iterator field={field}: doc={} has unstable advanceExact",
            doc - 1
          )));
        }
        if (i + 1) % 2 == 0 {
          let doc_2 = iterator_2.next_doc()?;
          if doc != doc_2 {
            return Err(LuceneError::corrupt_index(format!(
              "dv iterator field={field}: doc={doc} was not found through advance() (got: {doc_2})"
            )));
          }
          if iterator_2.doc_id() != doc {
            return Err(LuceneError::corrupt_index(format!(
              "dv iterator field={field}: doc={doc} reports wrong doc ID (got: {})",
              iterator_2.doc_id()
            )));
          }
        }
      }
      i += 1;
      last_doc = doc;
    }
    Ok(())
  }

  fn check_binary_doc_values<B>(
    field_name: &str,
    mut binary_doc_values: B,
    mut binary_doc_values_2: B,
  ) -> Result<()>
  where
    B: BinaryDocValues,
  {
    if binary_doc_values.doc_id() != -1 {
      return Err(LuceneError::corrupt_index(format!(
        "binary dv iterator for field: {field_name} should start at docID=-1, but got {}",
        binary_doc_values.doc_id()
      )));
    }
    // TODO: we could add stats to DVs, e.g. total doc count w/ a value for this field
    loop {
      let doc = binary_doc_values.next_doc()?;
      if doc == NO_MORE_DOCS {
        break;
      }
      let value = binary_doc_values.binary_value()?.into_owned();
      value.is_valid()?;

      if !binary_doc_values_2.advance_exact(doc)? {
        return Err(LuceneError::corrupt_index(format!(
          "advanceExact did not find matching doc ID: {doc}"
        )));
      }
      let value_2 = binary_doc_values_2.binary_value()?;
      if value != *value_2 {
        return Err(LuceneError::corrupt_index(format!(
          "nextDoc and advanceExact report different values: {value} != {value_2}"
        )));
      }
    }
    Ok(())
  }

  fn check_sorted_doc_values<S>(
    field_name: &str,
    mut doc_values: S,
    mut doc_values_2: S,
  ) -> Result<()>
  where
    S: SortedDocValues,
  {
    if doc_values.doc_id() != -1 {
      return Err(LuceneError::corrupt_index(format!(
        "sorted dv iterator for field: {field_name} should start at docID=-1, but got {}",
        doc_values.doc_id()
      )));
    }
    let max_ord = doc_values.get_value_count()? - 1;
    let mut seen_ords = FixedBitSet::new(doc_values.get_value_count()?.try_convert()?);
    let mut max_ord_2 = -1;
    loop {
      let doc = doc_values.next_doc()?;
      if doc == NO_MORE_DOCS {
        break;
      }
      let ord = doc_values.ord_value()?;
      if ord == -1 {
        return Err(LuceneError::corrupt_index(format!(
          "dv for field: {field_name} has -1 ord"
        )));
      } else if ord < -1 || ord > max_ord {
        return Err(LuceneError::corrupt_index(format!(
          "ord out of bounds: {ord}"
        )));
      } else {
        max_ord_2 = std::cmp::max(max_ord_2, ord);
        let ord_index: usize = ord.try_convert()?;
        CoreHelper::check_index(ord_index, seen_ords.length())?;
        seen_ords.set(ord_index);
      }
      if !doc_values_2.advance_exact(doc)? {
        return Err(LuceneError::corrupt_index(format!(
          "advanceExact did not find matching doc ID: {doc}"
        )));
      }
      let ord_2 = doc_values_2.ord_value()?;
      if ord != ord_2 {
        return Err(LuceneError::corrupt_index(format!(
          "nextDoc and advanceExact report different ords: {ord} != {ord_2}"
        )));
      }
    }
    if max_ord != max_ord_2 {
      return Err(LuceneError::corrupt_index(format!(
        "dv for field: {field_name} reports wrong maxOrd={max_ord} but this is not the case: {max_ord_2}"
      )));
    }
    let value_count = doc_values.get_value_count()?;
    if seen_ords.cardinality() != value_count.try_convert()? {
      return Err(LuceneError::corrupt_index(format!(
        "dv for field: {field_name} has holes in its ords, valueCount={value_count} but only used: {}",
        seen_ords.cardinality()
      )));
    }
    let mut last_value: Option<BytesRef<Vec<u8>>> = None;
    for i in 0..=max_ord {
      let term = doc_values.lookup_ord(i)?.into_owned();
      term.is_valid()?;
      if last_value
        .as_ref()
        .is_some_and(|last_value| term <= *last_value)
      {
        return Err(LuceneError::corrupt_index(format!(
          "dv for field: {field_name} has ords out of order: {} >= {term}",
          last_value.as_ref().expect("last value is present")
        )));
      }
      last_value = Some(term);
    }
    Ok(())
  }

  fn check_sorted_set_doc_values<S>(
    field_name: &str,
    mut doc_values: S,
    mut doc_values_2: S,
  ) -> Result<()>
  where
    S: SortedSetDocValues,
  {
    let max_ord = doc_values.get_value_count()? - 1;
    let mut seen_ords = LongBitSet::new(doc_values.get_value_count()?.try_convert()?)?;
    let mut max_ord_2 = -1;
    loop {
      let doc_id = doc_values.next_doc()?;
      if doc_id == NO_MORE_DOCS {
        break;
      }
      let count = doc_values.doc_value_count()?;
      if count == 0 {
        return Err(LuceneError::corrupt_index(format!(
          "sortedset dv for field: {field_name} returned docValueCount=0 for docID={doc_id}"
        )));
      }
      if !doc_values_2.advance_exact(doc_id)? {
        return Err(LuceneError::corrupt_index(format!(
          "advanceExact did not find matching doc ID: {doc_id}"
        )));
      }
      let count_2 = doc_values_2.doc_value_count()?;
      if count != count_2 {
        return Err(LuceneError::corrupt_index(format!(
          "advanceExact reports different value count: {count} != {count_2}"
        )));
      }
      let mut last_ord = -1;
      let mut ord_count = 0;
      for _ in 0..count {
        let current_count = doc_values.doc_value_count()?;
        if count != current_count {
          return Err(LuceneError::corrupt_index(format!(
            "value count changed from {count} to {current_count} during iterating over all values"
          )));
        }
        let ord = doc_values.next_ord()?;
        let ord_2 = doc_values_2.next_ord()?;
        if ord != ord_2 {
          return Err(LuceneError::corrupt_index(format!(
            "nextDoc and advanceExact report different ords: {ord} != {ord_2}"
          )));
        }
        if ord <= last_ord {
          return Err(LuceneError::corrupt_index(format!(
            "ords out of order: {ord} <= {last_ord} for doc: {doc_id}"
          )));
        }
        if ord < 0 || ord > max_ord {
          return Err(LuceneError::corrupt_index(format!(
            "ord out of bounds: {ord}"
          )));
        }
        last_ord = ord;
        max_ord_2 = std::cmp::max(max_ord_2, ord);
        let ord_index: usize = ord.try_convert()?;
        CoreHelper::check_index(ord_index, seen_ords.length())?;
        seen_ords.set(ord_index);
        ord_count += 1;
      }
      let current_count = doc_values.doc_value_count()?;
      let current_count_2 = doc_values_2.doc_value_count()?;
      if current_count != current_count_2 {
        return Err(LuceneError::corrupt_index(format!(
          "dv and dv2 report different values count after iterating over all values: {current_count} != {current_count_2}"
        )));
      }
      if ord_count == 0 {
        return Err(LuceneError::corrupt_index(format!(
          "dv for field: {field_name} returned docID={doc_id} yet has no ordinals"
        )));
      }
    }
    if max_ord != max_ord_2 {
      return Err(LuceneError::corrupt_index(format!(
        "dv for field: {field_name} reports wrong maxOrd={max_ord} but this is not the case: {max_ord_2}"
      )));
    }
    let value_count = doc_values.get_value_count()?;
    if seen_ords.cardinality() != value_count.try_convert()? {
      return Err(LuceneError::corrupt_index(format!(
        "dv for field: {field_name} has holes in its ords, valueCount={value_count} but only used: {}",
        seen_ords.cardinality()
      )));
    }

    let mut last_value: Option<BytesRef<Vec<u8>>> = None;
    for i in 0..=max_ord {
      let term = doc_values.lookup_ord(i)?.into_owned();
      debug_assert!(term.is_valid()?);
      if last_value
        .as_ref()
        .is_some_and(|last_value| term <= *last_value)
      {
        return Err(LuceneError::corrupt_index(format!(
          "dv for field: {field_name} has ords out of order: {} >= {term}",
          last_value.as_ref().expect("last value is present")
        )));
      }
      last_value = Some(term);
    }
    Ok(())
  }

  fn check_sorted_numeric_doc_values<S>(
    field_name: &str,
    mut numeric_doc_values: S,
    mut numeric_doc_values_2: S,
  ) -> Result<()>
  where
    S: SortedNumericDocValues,
  {
    if numeric_doc_values.doc_id() != -1 {
      return Err(LuceneError::corrupt_index(format!(
        "dv iterator for field: {field_name} should start at docID=-1, but got {}",
        numeric_doc_values.doc_id()
      )));
    }
    loop {
      let doc_id = numeric_doc_values.next_doc()?;
      if doc_id == NO_MORE_DOCS {
        break;
      }
      let count = numeric_doc_values.doc_value_count()?;
      if count == 0 {
        return Err(LuceneError::corrupt_index(format!(
          "sorted numeric dv for field: {field_name} returned docValueCount=0 for docID={doc_id}"
        )));
      }
      if !numeric_doc_values_2.advance_exact(doc_id)? {
        return Err(LuceneError::corrupt_index(format!(
          "advanceExact did not find matching doc ID: {doc_id}"
        )));
      }
      let count_2 = numeric_doc_values_2.doc_value_count()?;
      if count != count_2 {
        return Err(LuceneError::corrupt_index(format!(
          "advanceExact reports different value count: {count} != {count_2}"
        )));
      }
      let mut previous = i64::MIN;
      for _ in 0..count {
        let value = numeric_doc_values.next_value()?;
        if value < previous {
          return Err(LuceneError::corrupt_index(format!(
            "values out of order: {value} < {previous} for doc: {doc_id}"
          )));
        }
        previous = value;

        let value_2 = numeric_doc_values_2.next_value()?;
        if value != value_2 {
          return Err(LuceneError::corrupt_index(format!(
            "advanceExact reports different value: {value} != {value_2}"
          )));
        }
      }
    }
    Ok(())
  }

  fn check_numeric_doc_values<N>(
    field_name: &str,
    mut numeric_doc_values: N,
    mut numeric_doc_values_2: N,
  ) -> Result<()>
  where
    N: NumericDocValues,
  {
    if numeric_doc_values.doc_id() != -1 {
      return Err(LuceneError::corrupt_index(format!(
        "dv iterator for field: {field_name} should start at docID=-1, but got {}",
        numeric_doc_values.doc_id()
      )));
    }
    // TODO: we could add stats to DVs, e.g. total doc count w/ a value for this field
    loop {
      let doc = numeric_doc_values.next_doc()?;
      if doc == NO_MORE_DOCS {
        break;
      }
      let value = numeric_doc_values.long_value()?;

      if !numeric_doc_values_2.advance_exact(doc)? {
        return Err(LuceneError::corrupt_index(format!(
          "advanceExact did not find matching doc ID: {doc}"
        )));
      }
      let value_2 = numeric_doc_values_2.long_value()?;
      if value != value_2 {
        return Err(LuceneError::corrupt_index(format!(
          "advanceExact reports different value: {value} != {value_2}"
        )));
      }
    }
    Ok(())
  }

  fn check_doc_values<D>(
    field_info: &Arc<crate::core::index::field_info::FieldInfo>,
    doc_values_reader: &D,
    status: &mut DocValuesStatus,
  ) -> Result<()>
  where
    D: DocValuesProducer,
  {
    if *field_info.doc_values_skip_index_type() != DocValuesSkipIndexType::None {
      status.total_skipping_index += 1;
      let skipper = doc_values_reader.get_skipper(field_info)?.ok_or_else(|| {
        LuceneError::corrupt_index(format!(
          "field \"{}\" has a doc values skip index but getSkipper returned null",
          field_info.name
        ))
      })?;
      Self::check_doc_value_skipper(field_info, skipper)?;
    }
    match field_info.get_doc_values_type() {
      DocValuesType::Sorted => {
        status.total_sorted_fields += 1;
        Self::check_dv_iterator(field_info, |field_info| {
          doc_values_reader.get_sorted(field_info)
        })?;
        Self::check_sorted_doc_values(
          &field_info.name,
          doc_values_reader.get_sorted(field_info)?,
          doc_values_reader.get_sorted(field_info)?,
        )
      },
      DocValuesType::SortedNumeric => {
        status.total_sorted_numeric_fields += 1;
        Self::check_dv_iterator(field_info, |field_info| {
          doc_values_reader.get_sorted_numeric(field_info)
        })?;
        Self::check_sorted_numeric_doc_values(
          &field_info.name,
          doc_values_reader.get_sorted_numeric(field_info)?,
          doc_values_reader.get_sorted_numeric(field_info)?,
        )
      },
      DocValuesType::SortedSet => {
        status.total_sorted_set_fields += 1;
        Self::check_dv_iterator(field_info, |field_info| {
          doc_values_reader.get_sorted_set(field_info)
        })?;
        Self::check_sorted_set_doc_values(
          &field_info.name,
          doc_values_reader.get_sorted_set(field_info)?,
          doc_values_reader.get_sorted_set(field_info)?,
        )
      },
      DocValuesType::Binary => {
        status.total_binary_fields += 1;
        Self::check_dv_iterator(field_info, |field_info| {
          doc_values_reader.get_binary(field_info)
        })?;
        Self::check_binary_doc_values(
          &field_info.name,
          doc_values_reader.get_binary(field_info)?,
          doc_values_reader.get_binary(field_info)?,
        )
      },
      DocValuesType::Numeric => {
        status.total_numeric_fields += 1;
        Self::check_dv_iterator(field_info, |field_info| {
          doc_values_reader.get_numeric(field_info)
        })?;
        Self::check_numeric_doc_values(
          &field_info.name,
          doc_values_reader.get_numeric(field_info)?,
          doc_values_reader.get_numeric(field_info)?,
        )
      },
      DocValuesType::None => Err(LuceneError::unreachable("")),
    }
  }

  /// Test term vectors.
  pub fn test_term_vectors<R, O>(
    reader: &R,
    mut info_stream: Option<&mut O>,
    verbose: bool,
    level: i32,
    fail_fast: bool,
  ) -> Result<TermVectorStatus>
  where
    R: CodecReader,
    O: Write,
  {
    let start = Instant::now();
    let mut status = TermVectorStatus::default();
    let field_infos = reader.get_field_infos()?;
    let mut postings_fields_reader = None;
    let mut merge_postings_fields_reader = None;
    let mut vectors_reader = None;
    let mut merge_vectors_reader = None;

    let check_result = panic::catch_unwind(AssertUnwindSafe(|| -> Result<()> {
      if let Some(info_stream) = info_stream.as_deref_mut() {
        write!(info_stream, "    test: term vectors........")?;
      }

      let mut postings = None;

      // Only used if the Level is high enough to include slow checks:
      let mut postings_docs = None;

      let live_docs = reader.get_live_docs()?;

      // TODO: testTermsIndex
      if level >= Level::MIN_LEVEL_FOR_SLOW_CHECKS {
        postings_fields_reader = reader.get_postings_reader()?;
        if let Some(postings_fields_reader) = postings_fields_reader.as_ref() {
          merge_postings_fields_reader = postings_fields_reader.get_merge_instance()?;
        }
      }
      let postings_fields = merge_postings_fields_reader
        .as_ref()
        .or(postings_fields_reader.as_ref());

      vectors_reader = reader.get_term_vectors_reader()?;
      if let Some(vectors_reader) = vectors_reader.as_ref() {
        merge_vectors_reader = vectors_reader.get_merge_instance()?;
      }
      if let Some(vectors_reader) = merge_vectors_reader.as_mut().or(vectors_reader.as_mut()) {
        for j in 0..reader.max_doc()? {
          if (j & 0x03) == 0 {
            vectors_reader.prefetch(j)?;
          }
          // Intentionally pull/visit (but don't count in
          // stats) deleted documents to make sure they too
          // are not corrupt:
          let Some(tfv) = vectors_reader.get(j)? else {
            continue;
          };

          // TODO: can we make a IS(FIR) that searches just
          // this term vector... to pass for searcher?

          // First run with no deletions:
          Self::check_fields::<_, <R as LeafReader>::Bits, <R as CodecReader>::NormsProducer, O>(
            &tfv,
            None,
            1,
            field_infos.as_ref(),
            None,
            false,
            true,
            info_stream.as_deref_mut(),
            verbose,
            level,
          )?;

          // Only agg stats if the doc is live:
          let do_stats = match live_docs.as_ref() {
            Some(live_docs) => live_docs.get(j.try_convert()?)?,
            None => true,
          };
          if do_stats {
            status.doc_count += 1;
          }

          let mut fields_iterator = tfv.iterator()?;
          while let Some(field) = fields_iterator.next()? {
            if do_stats {
              status.tot_vectors += 1;
            }

            // Make sure FieldInfo thinks this field is vector'd:
            let field_info = field_infos.field_info_by_name(field)?.ok_or_else(|| {
              LuceneError::corrupt_index(format!(
                "docID={j} has term vectors for field={field} but field is missing from FieldInfos"
              ))
            })?;
            if !field_info.has_term_vectors() {
              return Err(LuceneError::corrupt_index(format!(
                "docID={j} has term vectors for field={field} but FieldInfo has storeTermVector=false"
              )));
            }

            if level >= Level::MIN_LEVEL_FOR_SLOW_CHECKS {
              let terms = tfv.terms(field)?.ok_or_else(|| {
                LuceneError::corrupt_index(format!(
                  "term vector field={field} has no terms; doc={j}"
                ))
              })?;
              let mut terms_enum = terms.iterator()?;
              let postings_has_freq = *field_info.get_index_options() >= IndexOptions::DocsAndFreqs;
              let postings_has_payload = field_info.has_payloads();
              let vectors_has_payload = terms.has_payloads();

              let postings_fields = postings_fields.ok_or_else(|| {
                LuceneError::corrupt_index(format!(
                  "vector field={field} does not exist in postings; doc={j}"
                ))
              })?;
              let postings_terms = postings_fields.terms(field)?.ok_or_else(|| {
                LuceneError::corrupt_index(format!(
                  "vector field={field} does not exist in postings; doc={j}"
                ))
              })?;
              let mut postings_terms_enum = postings_terms.iterator()?;

              let has_prox = terms.has_offsets() || terms.has_positions();
              let mut seek_exact_counter = 0;
              while let Some(term) = terms_enum.next()? {
                let term = term.into_owned();

                // This is the term vectors:
                postings = Some(terms_enum.postings_with_flags(postings.take(), ALL as i32)?);

                let term_exists = if (seek_exact_counter & 0x01) == 0 {
                  seek_exact_counter += 1;
                  postings_terms_enum.seek_exact(&term)?
                } else {
                  seek_exact_counter += 1;
                  postings_terms_enum.prepare_seek_exact(&term)?.is_some()
                    && postings_terms_enum.get_prepare_seek_exact_status(&term)?
                };
                if !term_exists {
                  return Err(LuceneError::corrupt_index(format!(
                    "vector term={term} field={field} does not exist in postings; doc={j}"
                  )));
                }

                // This is the inverted index ("real" postings):
                postings_docs =
                  Some(postings_terms_enum.postings_with_flags(postings_docs.take(), ALL as i32)?);

                let advance_doc = postings_docs
                  .as_mut()
                  .expect("postings docs were just initialized")
                  .advance(j)?;
                if advance_doc != j {
                  return Err(LuceneError::corrupt_index(format!(
                    "vector term={term} field={field}: doc={j} was not found in postings (got: {advance_doc})"
                  )));
                }

                let doc = postings
                  .as_mut()
                  .expect("term-vector postings were just initialized")
                  .next_doc()?;
                if doc != 0 {
                  return Err(LuceneError::corrupt_index(format!(
                    "vector for doc {j} didn't return docID=0: got docID={doc}"
                  )));
                }

                if postings_has_freq {
                  let tf = postings
                    .as_mut()
                    .expect("term-vector postings are present")
                    .freq()?;
                  let postings_freq = postings_docs
                    .as_mut()
                    .expect("postings docs are present")
                    .freq()?;
                  if postings_has_freq && postings_freq != tf {
                    return Err(LuceneError::corrupt_index(format!(
                      "vector term={term} field={field} doc={j}: freq={tf} differs from postings freq={postings_freq}"
                    )));
                  }

                  // Term vectors has prox?
                  if has_prox {
                    for _ in 0..tf {
                      let pos = postings
                        .as_mut()
                        .expect("term-vector postings are present")
                        .next_position()?;
                      if postings_terms.has_positions() {
                        let postings_pos = postings_docs
                          .as_mut()
                          .expect("postings docs are present")
                          .next_position()?;
                        if terms.has_positions() && pos != postings_pos {
                          return Err(LuceneError::corrupt_index(format!(
                            "vector term={term} field={field} doc={j}: pos={pos} differs from postings pos={postings_pos}"
                          )));
                        }
                      }

                      // Call the methods to at least make
                      // sure they don't throw exc:
                      let start_offset = postings
                        .as_ref()
                        .expect("term-vector postings are present")
                        .start_offset()?;
                      let end_offset = postings
                        .as_ref()
                        .expect("term-vector postings are present")
                        .end_offset()?;
                      // TODO: these are too anal...?
                      /*
                      if (endOffset < startOffset) {
                      throw new RuntimeException("vector startOffset=" + startOffset + " is > endOffset=" + endOffset);
                      }
                      if (startOffset < lastStartOffset) {
                      throw new RuntimeException("vector startOffset=" + startOffset + " is < prior startOffset=" + lastStartOffset);
                      }
                      lastStartOffset = startOffset;
                       */

                      if start_offset != -1 && end_offset != -1 && postings_terms.has_offsets() {
                        let postings_start_offset = postings_docs
                          .as_ref()
                          .expect("postings docs are present")
                          .start_offset()?;
                        let postings_end_offset = postings_docs
                          .as_ref()
                          .expect("postings docs are present")
                          .end_offset()?;
                        if start_offset != postings_start_offset {
                          return Err(LuceneError::corrupt_index(format!(
                            "vector term={term} field={field} doc={j}: startOffset={start_offset} differs from postings startOffset={postings_start_offset}"
                          )));
                        }
                        if end_offset != postings_end_offset {
                          return Err(LuceneError::corrupt_index(format!(
                            "vector term={term} field={field} doc={j}: endOffset={end_offset} differs from postings endOffset={postings_end_offset}"
                          )));
                        }
                      }

                      let payload = postings
                        .as_ref()
                        .expect("term-vector postings are present")
                        .get_payload()?
                        .map(Cow::into_owned);

                      if payload.is_some() {
                        debug_assert!(vectors_has_payload);
                      }

                      if postings_has_payload && vectors_has_payload {
                        let postings_payload = postings_docs
                          .as_ref()
                          .expect("postings docs are present")
                          .get_payload()?
                          .map(Cow::into_owned);
                        match (payload, postings_payload) {
                          (None, Some(postings_payload)) => {
                            return Err(LuceneError::corrupt_index(format!(
                              "vector term={term} field={field} doc={j} has no payload but postings does: {postings_payload}"
                            )));
                          },
                          (Some(payload), None) => {
                            return Err(LuceneError::corrupt_index(format!(
                              "vector term={term} field={field} doc={j} has payload={payload} but postings does not."
                            )));
                          },
                          (Some(payload), Some(postings_payload))
                            if payload != postings_payload =>
                          {
                            return Err(LuceneError::corrupt_index(format!(
                              "vector term={term} field={field} doc={j} has payload={payload} but differs from postings payload={postings_payload}"
                            )));
                          },
                          _ => {},
                        }
                      }
                    }
                  }
                }
              }
            }
          }
        }
      }
      let vector_avg = if status.doc_count == 0 {
        0.0
      } else {
        status.tot_vectors as f32 / status.doc_count as f32
      };
      Self::msg(
        info_stream.as_deref_mut(),
        &format!(
          "OK [{} total term vector count; avg {:.1} term/freq vector fields per doc] [took {:.3} sec]",
          status.tot_vectors,
          vector_avg,
          ns_to_sec(start.elapsed().as_nanos())
        ),
      )?;
      Ok(())
    }));

    let check_result = match check_result {
      Ok(check_result) => check_result,
      Err(payload) if fail_fast => panic::resume_unwind(payload),
      Err(payload) => Err(LuceneError::tragedy_from_panic(
        "CheckIndex term vectors test failed",
        payload.as_ref(),
      )),
    };

    if let Err(error) = check_result {
      if fail_fast {
        return Err(error);
      }
      Self::msg(info_stream.as_deref_mut(), &format!("ERROR [{error}]"))?;
      if let Some(info_stream) = info_stream {
        writeln!(info_stream, "{error:?}")?;
      }
      status.error = Some(error);
    }

    Ok(status)
  }
}

impl<D, L, W> CheckIndex<D, L, W>
where
  D: Directory,
  L: Lock,
  W: Write,
{
  /// Repairs the index using a previously returned [`Status`]. This does not remove unreferenced
  /// files; opening an [`IndexWriter`](crate::core::index::index_writer::IndexWriter) afterwards
  /// will delete them.
  ///
  /// **WARNING**: this writes a new segments file, removing all documents in broken segments.
  pub fn exorcise_index(&mut self, result: &mut Status<D>) -> Result<()> {
    self.ensure_open()?;
    if result.partial {
      return Err(LuceneError::illegal_argument(
        "can only exorcise an index that was fully checked (this status checked a subset of segments)",
      ));
    }
    let dir = result
      .dir
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("check index status has no directory"))?
      .clone();
    let new_segments = result
      .new_segments
      .as_mut()
      .ok_or_else(|| LuceneError::illegal_state("check index status has no new segments"))?;
    new_segments.changed();
    new_segments.commit(dir.as_ref())
  }

  /// Returns whether Rust debug assertions are enabled for this build.
  pub fn asserts_on() -> bool {
    cfg!(debug_assertions)
  }
}

impl CheckIndex<DirectoryEnum, LockEnum, Sink> {
  /// Command-line interface to check and exorcise corrupt segments from an index.
  ///
  /// Run it with an index path and optional `-exorcise`, `-verbose`, and one or more
  /// `-segment X` arguments. `-exorcise` writes a new `segments_N` file that removes
  /// problematic segments and therefore loses all documents in those segments. `-segment X`
  /// restricts checking to the named segments and cannot be combined with `-exorcise`.
  ///
  /// Make a complete backup before using `-exorcise`, and do not run it while the index is being
  /// written. Without `-exorcise`, this reports version information, exceptions, and the action
  /// that an exorcise would take. The process exits with code 1 if the index cannot be opened or
  /// has corruption, and 0 otherwise.
  pub fn main(args: &[String]) -> Result<()> {
    let exit_code = Self::do_main(args)?;
    std::process::exit(exit_code)
  }
}

/// Run-time configuration options for CheckIndex commands.
pub struct Options<W: Write = Sink> {
  do_exorcise: bool,
  verbose: bool,
  level: i32,
  thread_count: i32,
  only_segments: Option<Vec<String>>,
  index_path: Option<String>,
  dir_impl: Option<String>,
  out: Option<W>,
}

impl<W: Write> Default for Options<W> {
  fn default() -> Self {
    Self {
      do_exorcise: false,
      verbose: false,
      level: Level::DEFAULT_VALUE,
      thread_count: 0,
      only_segments: Some(Vec::new()),
      index_path: None,
      dir_impl: None,
      out: None,
    }
  }
}

impl<W: Write> Options<W> {
  /// Sole constructor.
  pub fn new() -> Self {
    Self::default()
  }

  /// Gets the name of the FSDirectory implementation to use.
  pub fn get_dir_impl(&self) -> Option<&str> {
    self.dir_impl.as_deref()
  }

  /// Gets the directory containing the index.
  pub fn get_index_path(&self) -> Option<&str> {
    self.index_path.as_deref()
  }

  /// Sets the writer to use for reporting results.
  pub fn set_out(&mut self, out: W) {
    self.out = Some(out);
  }
}

impl CheckIndex<DirectoryEnum, LockEnum, Sink> {
  // Actual main: returns exit code instead of terminating the process, for easy testing.
  fn do_main(args: &[String]) -> Result<i32> {
    let options = match Self::parse_options(args) {
      Ok(options) => options,
      Err(error) => {
        writeln!(std::io::stdout(), "{error}")?;
        return Ok(1);
      },
    };

    if !Self::asserts_on() {
      writeln!(
        std::io::stdout(),
        "\nNOTE: testing will be more thorough if you run with Rust debug assertions enabled"
      )?;
    }

    let Options {
      do_exorcise,
      verbose,
      level,
      thread_count,
      only_segments,
      index_path,
      dir_impl,
      out: _,
    } = options;
    let index_path = index_path.expect("parseOptions requires an index path");
    writeln!(std::io::stdout(), "\nOpening index @ {index_path}\n")?;

    let path = PathBuf::from(&index_path);
    let directory_result = match dir_impl.as_deref() {
      None => FSDirectories::open(path),
      Some("NIOFSDirectory") => FSDirectories::with_lock_factory(
        path,
        LockFactoryEnum::Native(NativeFSLockFactory::new()),
        FSDirectoryBaseEnum::NIO(NIOFSDirectory),
      ),
      Some("MMapDirectory") => FSDirectories::with_lock_factory(
        path,
        LockFactoryEnum::Native(NativeFSLockFactory::new()),
        FSDirectoryBaseEnum::MMap(MMapDirectory::default()),
      ),
      Some(dir_impl) => Err(LuceneError::illegal_argument(format!(
        "unknown FSDirectory implementation: {dir_impl}"
      ))),
    };
    let directory = match directory_result {
      Ok(directory) => Arc::new(directory),
      Err(error) => {
        writeln!(
          std::io::stdout(),
          "ERROR: could not open directory \"{index_path}\"; exiting"
        )?;
        writeln!(std::io::stdout(), "{error:?}")?;
        return Ok(1);
      },
    };

    let mut checker: CheckIndex<_, _, Stdout> = match CheckIndex::new(Arc::clone(&directory)) {
      Ok(checker) => checker,
      Err(error) => return IOUtils::use_or_suppress_result(Err(error), directory.close()),
    };
    let mut options = Options {
      do_exorcise,
      verbose,
      level,
      thread_count,
      only_segments,
      index_path: Some(index_path),
      dir_impl,
      out: Some(std::io::stdout()),
    };
    let result = checker.do_check(&mut options);
    let result = IOUtils::use_or_suppress_result(result, checker.close());
    IOUtils::use_or_suppress_result(result, directory.close())
  }
}

/// Static information about CheckIndex's `-level` parameter.
pub struct Level {
  _private: (),
}

impl Level {
  /// Minimum valid level.
  pub const MIN_VALUE: i32 = 1;

  /// Maximum valid level.
  pub const MAX_VALUE: i32 = 3;

  /// The default level if none is specified.
  pub const DEFAULT_VALUE: i32 = Self::MIN_VALUE;

  /// Minimum level required to run checksum checks.
  pub const MIN_LEVEL_FOR_CHECKSUM_CHECKS: i32 = 1;

  /// Minimum level required to run integrity checks.
  pub const MIN_LEVEL_FOR_INTEGRITY_CHECKS: i32 = 2;

  /// Minimum level required to run slow checks.
  pub const MIN_LEVEL_FOR_SLOW_CHECKS: i32 = 3;

  /// Checks if given level value is within the allowed bounds else it returns an error.
  pub fn check_if_level_in_bounds(level_val: i32) -> Result<()> {
    if !(Self::MIN_VALUE..=Self::MAX_VALUE).contains(&level_val) {
      return Err(LuceneError::illegal_argument(format!(
        "ERROR: given value: '{}' for -level option is out of bounds. Please use a value from '{}'->'{}'",
        level_val,
        Self::MIN_VALUE,
        Self::MAX_VALUE
      )));
    }

    Ok(())
  }
}

impl CheckIndex<DirectoryEnum, LockEnum, Sink> {
  /// Parses command line arguments into an [`Options`] value.
  pub fn parse_options(args: &[String]) -> Result<Options> {
    let mut options = Options::new();

    let mut i = 0;
    while i < args.len() {
      let arg = &args[i];
      if arg == "-level" {
        if i == args.len() - 1 {
          return Err(LuceneError::illegal_argument(
            "ERROR: missing value for -level option",
          ));
        }
        i += 1;
        let level = args[i]
          .parse::<i32>()
          .map_err(|error| LuceneError::illegal_argument(format!("{}: {error}", args[i])))?;
        Level::check_if_level_in_bounds(level)?;
        options.level = level;
      } else if arg == "-fast" {
        // Deprecated. Remove in Lucene 11.
        eprintln!(
          "-fast is deprecated, use '-level 1' for explicitly verifying file checksums only. This is also now the default behaviour!"
        );
      } else if arg == "-slow" {
        // Deprecated. Remove in Lucene 11.
        eprintln!("-slow is deprecated, use '-level 3' instead for slow checks");
        options.level = Level::MIN_LEVEL_FOR_SLOW_CHECKS;
      } else if arg == "-exorcise" {
        options.do_exorcise = true;
      } else if arg == "-crossCheckTermVectors" {
        // Deprecated. Remove in Lucene 11.
        eprintln!("-crossCheckTermVectors is deprecated, use '-level 3' instead");
        options.level = Level::MAX_VALUE;
      } else if arg == "-verbose" {
        options.verbose = true;
      } else if arg == "-segment" {
        if i == args.len() - 1 {
          return Err(LuceneError::illegal_argument(
            "ERROR: missing name for -segment option",
          ));
        }
        i += 1;
        options
          .only_segments
          .as_mut()
          .expect("onlySegments is a list while parsing")
          .push(args[i].clone());
      } else if arg == "-dir-impl" {
        if i == args.len() - 1 {
          return Err(LuceneError::illegal_argument(
            "ERROR: missing value for -dir-impl option",
          ));
        }
        i += 1;
        options.dir_impl = Some(args[i].clone());
      } else if arg == "-threadCount" {
        if i == args.len() - 1 {
          return Err(LuceneError::illegal_argument(
            "-threadCount requires a following number",
          ));
        }
        i += 1;
        options.thread_count = args[i]
          .parse::<i32>()
          .map_err(|error| LuceneError::illegal_argument(format!("{}: {error}", args[i])))?;
        if options.thread_count <= 0 {
          return Err(LuceneError::illegal_argument(format!(
            "-threadCount requires a number larger than 0, but got: {}",
            options.thread_count
          )));
        }
      } else {
        if options.index_path.is_some() {
          return Err(LuceneError::illegal_argument(format!(
            "ERROR: unexpected extra argument '{}'",
            args[i]
          )));
        }
        options.index_path = Some(args[i].clone());
      }
      i += 1;
    }

    if options.index_path.is_none() {
      return Err(LuceneError::illegal_argument(
        "\nERROR: index path not specified\nUsage: java org.apache.lucene.index.CheckIndex pathToIndex [-exorcise] [-level X] [-segment X] [-segment Y] [-threadCount X] [-dir-impl X]\n\n  -exorcise: actually write a new segments_N file, removing any problematic segments\n  -level X: sets the detail level of the check. The higher the value, the more checks are done.\n         1 - (Default) Checksum checks only.\n         2 - All level 1 checks + logical integrity checks.\n         3 - All level 2 checks + slow checks.\n  -codec X: when exorcising, codec to write the new segments_N file with\n  -verbose: print additional details\n  -segment X: only check the specified segments.  This can be specified multiple\n              times, to check more than one segment, e.g. '-segment _2 -segment _a'.\n              You can't use this with the -exorcise option\n  -threadCount X: number of threads used to check index concurrently.\n                  When not specified, this will default to the number of CPU cores.\n                  When '-threadCount 1' is used, index checking will be performed sequentially.\n  -dir-impl X: use a specific FSDirectory implementation.\nCheckIndex only verifies file checksums as default.\nUse -level with value of '2' or higher if you also want to check segment file contents.\n\n**WARNING**: -exorcise *LOSES DATA*. This should only be used on an emergency basis as it will cause\ndocuments (perhaps many) to be permanently removed from the index.  Always make\na backup copy of your index before running this!  Do not run this tool on an index\nthat is actively being written to.  You have been warned!\n\nRun without -exorcise, this tool will open the index, report version information\nand report any exceptions it hits and what action it would take if -exorcise were\nspecified.  With -exorcise, this tool will remove any segments that have issues and\nwrite a new segments_N file.  This means all documents contained in the affected\nsegments will be removed.\n\nThis tool exits with exit code 1 if the index cannot be opened or has any\ncorruption, else 0.\n",
      ));
    }

    if options.only_segments.as_ref().is_some_and(Vec::is_empty) {
      options.only_segments = None;
    } else if options.do_exorcise {
      return Err(LuceneError::illegal_argument(
        "ERROR: cannot specify both -exorcise and -segment",
      ));
    }

    Ok(options)
  }
}

impl<D, L, W> CheckIndex<D, L, W>
where
  D: Directory,
  L: Lock,
  W: Write,
{
  /// Actually performs the index check, returning 0 if the index is clean and 1 otherwise.
  pub fn do_check(&mut self, options: &mut Options<W>) -> Result<i32> {
    self.set_level(options.level)?;
    self.set_info_stream_with_verbose(options.out.take(), options.verbose);
    // User-provided thread count overrides the default.
    if options.thread_count > 0 {
      self.set_thread_count(options.thread_count)?;
    }

    let mut result = self.check_index_with_segments(options.only_segments.as_deref())?;

    if result.missing_segments {
      return Ok(1);
    }

    if !result.clean {
      if !options.do_exorcise {
        let output = self.info_stream.as_mut().ok_or_else(|| {
          LuceneError::illegal_state("CheckIndex Options.out must be set before doCheck")
        })?;
        writeln!(
          output,
          "WARNING: would write new segments file, and {} documents would be lost, if -exorcise were specified\n",
          result.tot_lose_doc_count
        )?;
      } else {
        {
          let output = self.info_stream.as_mut().ok_or_else(|| {
            LuceneError::illegal_state("CheckIndex Options.out must be set before doCheck")
          })?;
          writeln!(
            output,
            "WARNING: {} documents will be lost\n",
            result.tot_lose_doc_count
          )?;
          writeln!(
            output,
            "NOTE: will write new segments file in 5 seconds; this will remove {} docs from the index. YOU WILL LOSE DATA. THIS IS YOUR LAST CHANCE TO CTRL+C!",
            result.tot_lose_doc_count
          )?;
          for second in 0..5 {
            thread::sleep(Duration::from_secs(1));
            writeln!(output, "  {}...", 5 - second)?;
          }
          writeln!(output, "Writing...")?;
        }
        self.exorcise_index(&mut result)?;
        let segments_file_name = result
          .new_segments
          .as_ref()
          .and_then(SegmentInfos::get_segments_file_name)
          .ok_or_else(|| {
            LuceneError::illegal_state("new segments file name is unavailable after exorciseIndex")
          })?;
        let output = self.info_stream.as_mut().ok_or_else(|| {
          LuceneError::illegal_state("CheckIndex Options.out must be set before doCheck")
        })?;
        writeln!(output, "OK")?;
        writeln!(output, "Wrote new segments file \"{segments_file_name}\"")?;
      }
    }
    let output = self.info_stream.as_mut().ok_or_else(|| {
      LuceneError::illegal_state("CheckIndex Options.out must be set before doCheck")
    })?;
    writeln!(output)?;

    Ok(if result.clean { 0 } else { 1 })
  }
}

impl CheckIndex<DirectoryEnum, LockEnum, Sink> {
  fn check_soft_deletes<D, O>(
    soft_deletes_field: &str,
    info: &SegmentCommitInfo<D>,
    reader: &SegmentReader<D>,
    mut info_stream: Option<&mut O>,
    fail_fast: bool,
  ) -> Result<SoftDeletesStatus>
  where
    D: Directory,
    O: Write,
  {
    let mut status = SoftDeletesStatus::default();
    if let Some(info_stream) = info_stream.as_deref_mut() {
      write!(info_stream, "    test: check soft deletes.....")?;
    }
    let check_result = panic::catch_unwind(AssertUnwindSafe(|| -> Result<()> {
      let mut soft_deleted_docs = get_doc_values_doc_id_set_iterator(soft_deletes_field, reader)?;
      let live_docs = reader.get_live_docs()?;
      let soft_deletes = count_soft_deletes(soft_deleted_docs.as_mut(), live_docs.as_ref())?;
      if soft_deletes != info.get_soft_del_count() {
        return Err(LuceneError::corrupt_index(format!(
          "actual soft deletes: {soft_deletes} but expected: {}",
          info.get_soft_del_count()
        )));
      }
      Ok(())
    }));
    let check_result = match check_result {
      Ok(check_result) => check_result,
      Err(payload) if fail_fast => panic::resume_unwind(payload),
      Err(payload) => Err(LuceneError::tragedy_from_panic(
        "CheckIndex soft deletes test failed",
        payload.as_ref(),
      )),
    };
    if let Err(error) = check_result {
      if fail_fast {
        return Err(error);
      }
      Self::msg(info_stream.as_deref_mut(), &format!("ERROR [{error}]"))?;
      if let Some(info_stream) = info_stream {
        writeln!(info_stream, "{error:?}")?;
      }
      status.error = Some(error);
    }
    Ok(status)
  }
}

fn ns_to_sec(ns: u128) -> f64 {
  ns as f64 / Duration::from_secs(1).as_nanos() as f64
}
