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
use crate::core::codecs::doc_values_consumer::DocValuesConsumer;
use crate::core::codecs::doc_values_format::DocValuesFormat;
use crate::core::codecs::doc_values_producer::DocValuesProducer;
use crate::core::codecs::dummy::dummy_binary_doc_values::DummyBinaryDocValues;
use crate::core::codecs::dummy::dummy_doc_values_skipper::DummyDocValuesSkipper;
use crate::core::codecs::dummy::dummy_sorted_doc_values::DummySortedDocValues;
use crate::core::codecs::dummy::dummy_sorted_numeric_doc_values::DummySortedNumericDocValues;
use crate::core::codecs::dummy::dummy_sorted_set_doc_values::DummySortedSetDocValues;
use crate::core::codecs::fields_consumer::FieldsConsumer;
use crate::core::codecs::norms_consumer::NormsConsumer;
use crate::core::codecs::norms_format::NormsFormat;
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::codecs::postings_format::PostingsFormat;
use crate::core::codecs::stored_fields_format::StoredFieldsFormat;
use crate::core::codecs::stored_fields_writer::StoredFieldsWriter;
use crate::core::codecs::term_vectors_format::TermVectorsFormat;
use crate::core::codecs::term_vectors_writer::TermVectorsWriter;
use crate::core::codecs::{Codec, Codecs, codec};
use crate::core::document::document::Document;
use crate::core::document::field::{Field, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::text_field::TYPE_STORED;
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::composite_reader::CompositeReader;
use crate::core::index::directory_reader;
use crate::core::index::directory_reader::DirectoryReader;
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::{FieldInfos, get_indexed_fields};
use crate::core::index::fields::Fields;
use crate::core::index::index_reader::Identity;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_writer::{IndexWriter, WRITE_LOCK_NAME};
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::merge_policy::MergePolicy;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_infos::{SegmentInfos, get_last_commit_generation_from_directory};
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::serial_merge_scheduler::SerialMergeScheduler;
use crate::core::index::term::Term;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::index::{CODEC_FILE_PATTERN, IndexFileNames};
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, NO_MORE_DOCS};
use crate::core::store::directory::{DirEnum, Directory};
use crate::core::store::flush_info::FlushInfo;
use crate::core::store::random_access_input::RandomAccessInputWrapper;
use crate::core::store::{DataInput, IOContext, IndexInput};
use crate::core::util::HasIdentity;
use crate::core::util::StringHelper;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bits::Bits;
use crate::core::util::clone::TryClone;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::info_stream::get_default_info_stream;
use crate::core::util::io_utils::IOUtils;
use crate::core::util::iterator::{VecIter, VecIteratorExt};
use crate::core::util::version::LATEST;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::directory_reader_enum::DirectoryReaderEnum;
use crate::test_framework::core::store::mock_directory_wrapper::Throttling;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, create_temp_dir_with_prefix, get_only_leaf_reader, new_directory, new_directory_shared,
  new_fs_directory, new_index_writer_config_with_analyzer, new_mock_directory, new_string_field,
  new_tiered_merge_policy,
};
use crate::test_framework::core::util::test_util::TestUtil;
use parking_lot::Mutex;
use rand::{Rng, RngExt};
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub struct DefaultCodecGuard {
  saved_codec: Codecs,
}

impl Drop for DefaultCodecGuard {
  fn drop(&mut self) {
    codec::set_default(self.saved_codec.clone());
  }
}

struct ReaderNormsProducer<'a, LR> {
  reader: &'a LR,
}

impl<LR> CloseableRef for ReaderNormsProducer<'_, LR> where LR: LeafReader {}

impl<LR> NormsProducer for ReaderNormsProducer<'_, LR>
where
  LR: LeafReader,
{
  type NumericDocValues = LR::NormNumericDocValues;

  fn get_norms(&self, field: &Arc<FieldInfo>) -> Result<Self::NumericDocValues> {
    self.reader.get_norm_values(&field.name)?.ok_or_else(|| {
      LuceneError::illegal_state(format!("field {} does not have norms", field.name))
    })
  }

  fn check_integrity(&self) -> Result<()> {
    Ok(())
  }
}

struct OneDocFields<'a, LR> {
  one_doc_reader: &'a LR,
  indexed_fields: Vec<String>,
}

impl<'a, LR> OneDocFields<'a, LR>
where
  LR: LeafReader + Clone,
{
  fn new(reader: &'a LR) -> Result<Self> {
    let mut indexed_fields = get_indexed_fields(reader.clone())?
      .into_iter()
      .collect::<Vec<_>>();
    indexed_fields.sort();
    Ok(Self {
      one_doc_reader: reader,
      indexed_fields,
    })
  }
}

impl<LR> Fields for OneDocFields<'_, LR>
where
  LR: LeafReader,
{
  type FieldIter<'a>
    = VecIter<'a, String>
  where
    Self: 'a;

  fn iterator(&self) -> Result<Self::FieldIter<'_>> {
    Ok(self.indexed_fields.iter_ext())
  }

  type Terms = LR::Terms;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    self.one_doc_reader.terms(field)
  }

  fn size(&self) -> Result<i32> {
    Ok(self.indexed_fields.len() as i32)
  }
}

struct OneDocDocValuesProducer;

impl CloseableRef for OneDocDocValuesProducer {}

impl DocValuesProducer for OneDocDocValuesProducer {
  type NumericDocValues = OneDocDocValues;

  fn get_numeric(&self, _field: &Arc<FieldInfo>) -> Result<Self::NumericDocValues> {
    Ok(OneDocDocValues::new())
  }

  type BinaryDocValues = DummyBinaryDocValues;
  type SortedDocValues = DummySortedDocValues;
  type SortedNumericDocValues = DummySortedNumericDocValues;
  type SortedSetDocValues = DummySortedSetDocValues;
  type DocValuesSkipper = DummyDocValuesSkipper;

  fn check_integrity(&self) -> Result<()> {
    Ok(())
  }
}

struct OneDocDocValues {
  doc_id: i32,
}

impl OneDocDocValues {
  fn new() -> Self {
    Self { doc_id: -1 }
  }
}

impl DocIdSetIterator for OneDocDocValues {
  fn doc_id(&self) -> i32 {
    self.doc_id
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.doc_id += 1;
    if self.doc_id == 1 {
      self.doc_id = NO_MORE_DOCS;
    }
    Ok(self.doc_id)
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    if self.doc_id <= 0 && target == 0 {
      self.doc_id = 0;
    } else {
      self.doc_id = NO_MORE_DOCS;
    }
    Ok(self.doc_id)
  }

  fn cost(&self) -> Result<i64> {
    Ok(1)
  }
}

impl DocValuesIterator for OneDocDocValues {
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    self.doc_id = target;
    Ok(target == 0)
  }
}

impl NumericDocValues for OneDocDocValues {
  fn long_value(&mut self) -> Result<i64> {
    Ok(5)
  }
}

struct OneDocNormsProducer;

impl CloseableRef for OneDocNormsProducer {}

impl NormsProducer for OneDocNormsProducer {
  type NumericDocValues = OneDocNormValues;

  fn get_norms(&self, _field: &Arc<FieldInfo>) -> Result<Self::NumericDocValues> {
    Ok(OneDocNormValues::new())
  }

  fn check_integrity(&self) -> Result<()> {
    Ok(())
  }
}

struct OneDocNormValues {
  doc_id: i32,
}

impl OneDocNormValues {
  fn new() -> Self {
    Self { doc_id: -1 }
  }
}

impl DocIdSetIterator for OneDocNormValues {
  fn doc_id(&self) -> i32 {
    self.doc_id
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.doc_id += 1;
    if self.doc_id == 1 {
      self.doc_id = NO_MORE_DOCS;
    }
    Ok(self.doc_id)
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    if self.doc_id <= 0 && target == 0 {
      self.doc_id = 0;
    } else {
      self.doc_id = NO_MORE_DOCS;
    }
    Ok(self.doc_id)
  }

  fn cost(&self) -> Result<i64> {
    Ok(1)
  }
}

impl DocValuesIterator for OneDocNormValues {
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    self.doc_id = target;
    Ok(target == 0)
  }
}

impl NumericDocValues for OneDocNormValues {
  fn long_value(&mut self) -> Result<i64> {
    Ok(5)
  }
}

/// A directory that tracks created files that have not been deleted.
pub(crate) struct FileTrackingDirectoryWrapper<D> {
  in_: D,
  files: Mutex<HashSet<String>>,
  identity: Identity,
}

impl<D> FileTrackingDirectoryWrapper<D>
where
  D: Directory,
{
  /// Sole constructor.
  pub(crate) fn new(in_: D) -> Self {
    Self {
      in_,
      files: Mutex::new(HashSet::new()),
      identity: Identity::new(),
    }
  }

  /// Get the set of created files.
  pub(crate) fn get_files(&self) -> HashSet<String> {
    self.files.lock().clone()
  }
}

impl<D> Directory for FileTrackingDirectoryWrapper<D>
where
  D: Directory,
{
  type IndexOutput = D::IndexOutput;

  fn create_output(&self, name: &str, context: &IOContext) -> Result<Self::IndexOutput> {
    self.files.lock().insert(name.to_string());
    self.in_.create_output(name, context)
  }

  fn rename(&self, source: &str, destination: &str) -> Result<()> {
    let mut files = self.files.lock();
    files.remove(source);
    files.insert(destination.to_string());
    drop(files);
    self.in_.rename(source, destination)
  }

  fn delete_file(&self, name: &str) -> Result<()> {
    self.files.lock().remove(name);
    self.in_.delete_file(name)
  }

  fn list_all(&self) -> Result<Vec<String>> {
    self.in_.list_all()
  }

  fn file_length(&self, name: &str) -> Result<usize> {
    self.in_.file_length(name)
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

  type IndexInput = D::IndexInput;

  fn open_input(&self, name: &str, context: &IOContext) -> Result<Self::IndexInput> {
    self.in_.open_input(name, context)
  }

  type Lock = D::Lock;

  fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
    self.in_.obtain_lock(name)
  }

  fn get_pending_deletions(&self) -> Result<HashSet<String>> {
    self.in_.get_pending_deletions()
  }
}

impl<D> Display for FileTrackingDirectoryWrapper<D>
where
  D: Directory,
{
  fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
    Display::fmt(&self.in_, formatter)
  }
}

impl<D> CloseableRef for FileTrackingDirectoryWrapper<D>
where
  D: Directory,
{
  fn close(&self) -> Result<()> {
    self.in_.close()
  }
}

impl<D> HasIdentity for FileTrackingDirectoryWrapper<D>
where
  D: Directory,
{
  fn identity(&self) -> &Identity {
    &self.identity
  }
}

pub(crate) struct ReadBytesIndexInputWrapper<I> {
  in_: I,
  read_bytes: Arc<Mutex<FixedBitSet>>,
  file_offset: usize,
}

impl<I> ReadBytesIndexInputWrapper<I>
where
  I: IndexInput,
{
  fn new(in_: I, read_bytes: Arc<Mutex<FixedBitSet>>, file_offset: usize) -> Self {
    Self {
      in_,
      read_bytes,
      file_offset,
    }
  }
}

impl<I> TryClone for ReadBytesIndexInputWrapper<I>
where
  I: IndexInput,
{
  fn try_clone(&self) -> Result<Self> {
    Ok(Self::new(
      self.in_.try_clone()?,
      Arc::clone(&self.read_bytes),
      self.file_offset,
    ))
  }
}

impl<I> IndexInput for ReadBytesIndexInputWrapper<I>
where
  I: IndexInput<IndexInput = I>,
{
  type IndexInput = Self;

  fn slice(&self, description: &str, offset: usize, length: usize) -> Result<Self::IndexInput> {
    Ok(Self::new(
      self.in_.slice(description, offset, length)?,
      Arc::clone(&self.read_bytes),
      self.file_offset + offset,
    ))
  }

  type RandomAccessSlice = RandomAccessInputWrapper<Self>;

  fn random_access_slice(&self, offset: usize, length: usize) -> Result<Self::RandomAccessSlice> {
    Ok(RandomAccessInputWrapper::new(self.slice(
      "randomaccess",
      offset,
      length,
    )?))
  }

  fn get_file_pointer(&self) -> Result<usize> {
    self.in_.get_file_pointer()
  }

  fn seek(&mut self, position: usize) -> Result<()> {
    self.in_.seek(position)
  }

  fn length(&self) -> Result<usize> {
    self.in_.length()
  }
}

impl<I> DataInput for ReadBytesIndexInputWrapper<I>
where
  I: IndexInput,
{
  fn read_byte(&mut self) -> Result<u8> {
    let position = self.in_.get_file_pointer()?;
    self.read_bytes.lock().set(self.file_offset + position);
    self.in_.read_byte()
  }

  fn read_bytes(&mut self, bytes: &mut [u8], offset: usize, length: usize) -> Result<()> {
    let fp = self.in_.get_file_pointer()?;
    let mut read_bytes = self.read_bytes.lock();
    for i in 0..length {
      read_bytes.set(self.file_offset + fp + i);
    }
    drop(read_bytes);
    self.in_.read_bytes(bytes, offset, length)
  }

  fn read_group_vint(&mut self, destination: &mut [i32], offset: usize) -> Result<()> {
    let start = self.in_.get_file_pointer()?;
    let result = self.in_.read_group_vint(destination, offset);
    let end = self.in_.get_file_pointer()?;
    let mut read_bytes = self.read_bytes.lock();
    for i in start..end {
      read_bytes.set(self.file_offset + i);
    }
    drop(read_bytes);
    result
  }

  fn skip_bytes(&mut self, num_bytes: i64) -> Result<()> {
    IndexInput::skip_bytes(&mut self.in_, num_bytes)
  }
}

impl<I> Display for ReadBytesIndexInputWrapper<I>
where
  I: IndexInput,
{
  fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
    Display::fmt(&self.in_, formatter)
  }
}

impl<I> CloseableRef for ReadBytesIndexInputWrapper<I>
where
  I: IndexInput,
{
  fn close(&self) -> Result<()> {
    self.in_.close()
  }
}

pub(crate) struct ReadBytesDirectoryWrapper<D> {
  in_: D,
  read_bytes: Mutex<HashMap<String, Arc<Mutex<FixedBitSet>>>>,
  identity: Identity,
}

impl<D> ReadBytesDirectoryWrapper<D>
where
  D: Directory,
{
  pub(crate) fn new(in_: D) -> Self {
    Self {
      in_,
      read_bytes: Mutex::new(HashMap::new()),
      identity: Identity::new(),
    }
  }

  pub(crate) fn get_read_bytes(&self) -> HashMap<String, FixedBitSet> {
    self
      .read_bytes
      .lock()
      .iter()
      .map(|(name, bits)| (name.clone(), bits.lock().clone()))
      .collect()
  }
}

impl<D> Directory for ReadBytesDirectoryWrapper<D>
where
  D: Directory,
  D::IndexInput: IndexInput<IndexInput = D::IndexInput>,
{
  type IndexInput = ReadBytesIndexInputWrapper<D::IndexInput>;

  fn open_input(&self, name: &str, context: &IOContext) -> Result<Self::IndexInput> {
    let input = self.in_.open_input(name, context)?;
    let length = input.length()?;
    let read_bytes = {
      let mut read_bytes = self.read_bytes.lock();
      let bits = Arc::clone(
        read_bytes
          .entry(name.to_string())
          .or_insert_with(|| Arc::new(Mutex::new(FixedBitSet::new(length)))),
      );
      if bits.lock().length() != length {
        return Err(LuceneError::illegal_state(
          "file length changed while tracking read bytes",
        ));
      }
      bits
    };
    Ok(ReadBytesIndexInputWrapper::new(input, read_bytes, 0))
  }

  type IndexOutput = D::IndexOutput;

  fn create_output(&self, _name: &str, _context: &IOContext) -> Result<Self::IndexOutput> {
    Err(LuceneError::unsupported_operation(
      "ReadBytesDirectoryWrapper is read-only",
    ))
  }

  fn list_all(&self) -> Result<Vec<String>> {
    self.in_.list_all()
  }

  fn delete_file(&self, name: &str) -> Result<()> {
    self.in_.delete_file(name)
  }

  fn file_length(&self, name: &str) -> Result<usize> {
    self.in_.file_length(name)
  }

  fn create_temp_output(
    &self,
    _prefix: &str,
    _suffix: &str,
    _context: &IOContext,
  ) -> Result<Self::IndexOutput> {
    Err(LuceneError::unsupported_operation(
      "ReadBytesDirectoryWrapper is read-only",
    ))
  }

  fn sync(&self, names: &[String]) -> Result<()> {
    self.in_.sync(names)
  }

  fn sync_metadata(&self) -> Result<()> {
    self.in_.sync_metadata()
  }

  fn rename(&self, source: &str, destination: &str) -> Result<()> {
    self.in_.rename(source, destination)
  }

  type Lock = D::Lock;

  fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
    self.in_.obtain_lock(name)
  }

  fn get_pending_deletions(&self) -> Result<HashSet<String>> {
    self.in_.get_pending_deletions()
  }
}

impl<D> Display for ReadBytesDirectoryWrapper<D>
where
  D: Directory,
{
  fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
    write!(formatter, "ReadBytesDirectoryWrapper({})", self.in_)
  }
}

impl<D> CloseableRef for ReadBytesDirectoryWrapper<D>
where
  D: Directory,
{
  fn close(&self) -> Result<()> {
    self.in_.close()
  }
}

impl<D> HasIdentity for ReadBytesDirectoryWrapper<D>
where
  D: Directory,
{
  fn identity(&self) -> &Identity {
    &self.identity
  }
}

/// Static default implementations for the overridable methods on
/// [`BaseIndexFileFormatTestCase`].
pub trait BaseIndexFileFormatTestCaseDefaults<T>
where
  T: ?Sized,
{
  fn add_random_fields<R>(test_case: &T, random: &mut R, document: &mut Document) -> Result<()>
  where
    R: Rng + ?Sized;

  fn merge_is_stable(_test_case: &T) -> bool {
    true
  }
}

pub trait BaseIndexFileFormatTestCase: Sized {
  type Defaults: BaseIndexFileFormatTestCaseDefaults<Self>;

  /// Returns the codec to run tests against.
  fn get_codec(&self) -> Result<Codecs>;

  /// Returns the major version that this codec is compatible with.
  fn get_created_version_major(&self) -> i32 {
    LATEST.major
  }

  /// Set the created version of the given [`Directory`] and return it.
  fn apply_created_version_major<R, D>(&self, random: &mut R, d: D) -> Result<D>
  where
    R: Rng + ?Sized,
    D: Directory,
  {
    if get_last_commit_generation_from_directory(&d)? != -1 {
      return Err(
        crate::core::util::error::lucene_error::LuceneError::illegal_argument(
          "Cannot set the created version on a Directory that already has segments",
        ),
      );
    }
    if self.get_created_version_major() != LATEST.major || random.random_bool(0.5) {
      let mut segment_infos: SegmentInfos<D> = SegmentInfos::new(self.get_created_version_major())?;
      segment_infos.commit(&d)?;
    }
    Ok(d)
  }

  fn set_up(&self) -> Result<DefaultCodecGuard> {
    let saved_codec = codec::get_default();
    codec::set_default(self.get_codec()?);
    Ok(DefaultCodecGuard { saved_codec })
  }

  fn tear_down(&self, codec_guard: DefaultCodecGuard) {
    drop(codec_guard);
  }

  /// Add random fields to the provided document.
  fn add_random_fields<R>(&self, random: &mut R, document: &mut Document) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    Self::Defaults::add_random_fields(self, random, document)
  }

  fn bytes_used_by_extension<D>(&self, directory: &D) -> Result<HashMap<String, usize>>
  where
    D: Directory,
  {
    let mut bytes_used_by_extension = HashMap::new();
    for file in directory.list_all()? {
      if CODEC_FILE_PATTERN.is_match(&file) {
        let extension = IndexFileNames::get_extension(&file).unwrap_or("");
        *bytes_used_by_extension
          .entry(extension.to_string())
          .or_insert(0) += directory.file_length(&file)?;
      }
    }
    for extension in self.excluded_extensions_from_byte_counts() {
      bytes_used_by_extension.remove(extension);
    }
    Ok(bytes_used_by_extension)
  }

  /// Return the list of extensions that should be excluded from byte counts when comparing indices
  /// that store the same content.
  fn excluded_extensions_from_byte_counts(&self) -> HashSet<&'static str> {
    HashSet::from([
      // Segment infos store pieces of information that do not solely depend on the content of the
      // index in the diagnostics (such as a timestamp), so exclude these files from byte counts.
      "si",
      // Lock files are 0 bytes (one directory in the test could be in-memory, the other on disk).
      "lock",
    ])
  }

  /// The purpose of this test is to make sure that bulk merge does not accumulate useless data over
  /// runs.
  fn test_merge_stability<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    if !self.merge_is_stable() {
      return Ok(());
    }

    let dir = new_directory_shared(random)?;
    let dir = self.apply_created_version_major(random, dir)?;

    // Do not use newMergePolicy that might return a MockMergePolicy that ignores the no-CFS ratio.
    // Do not use RandomIndexWriter, which will change things up.
    let mut merge_policy = new_tiered_merge_policy(random)?;
    MergePolicy::<DirEnum>::get_base_mut(&mut merge_policy).set_no_cfs_ratio(0.0)?;
    let mut config = IndexWriterConfig::with_analyzer(MockAnalyzer::new(random))?;
    config.set_use_compound_file(false);
    config.set_merge_policy(merge_policy);
    let writer = IndexWriter::new(dir.clone(), config)?;
    let num_docs = at_least(random, 500);
    for _ in 0..num_docs {
      let mut document = Document::new();
      self.add_random_fields(random, &mut document)?;
      writer.add_document(document)?;
    }
    writer.force_merge(1)?;
    writer.commit()?;
    writer.close()?;
    let reader = directory_reader::open(dir.clone())?;

    let dir2 = new_directory_shared(random)?;
    let dir2 = self.apply_created_version_major(random, dir2)?;
    let mut merge_policy = new_tiered_merge_policy(random)?;
    MergePolicy::<DirEnum>::get_base_mut(&mut merge_policy).set_no_cfs_ratio(0.0)?;
    let mut config = IndexWriterConfig::with_analyzer(MockAnalyzer::new(random))?;
    config.set_use_compound_file(false);
    config.set_merge_policy(merge_policy);
    let writer = IndexWriter::new(dir2.clone(), config)?;
    let leaves = reader.get_sequential_sub_readers().to_vec();
    writer.add_indexes_from_codec_readers(leaves)?;
    writer.commit()?;
    writer.close()?;

    assert_eq!(
      self.bytes_used_by_extension(dir.as_ref())?,
      self.bytes_used_by_extension(dir2.as_ref())?
    );

    reader.close()?;
    dir.close()?;
    dir2.close()?;
    Ok(())
  }

  fn merge_is_stable(&self) -> bool {
    Self::Defaults::merge_is_stable(self)
  }

  /// Calls close multiple times on closeable codec APIs.
  fn test_multi_close<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    // First make a one-doc index.
    let one_doc_index = new_directory_shared(random)?;
    let one_doc_index = self.apply_created_version_major(random, one_doc_index)?;
    let analyzer = MockAnalyzer::new(random);
    let config = new_index_writer_config_with_analyzer(random, analyzer)?;
    let writer = IndexWriter::new(one_doc_index.clone(), config)?;
    let mut one_doc = Document::new();
    let mut custom_type = FieldType::from_ref(&*TYPE_STORED)?;
    custom_type.set_store_term_vectors(true)?;
    one_doc.add(Field::new("field", "contents", custom_type));
    one_doc.add(NumericDocValuesField::new("field", 5));
    writer.add_document(one_doc)?;
    let one_doc_directory_reader = directory_reader::open_from_writer(&writer)?;
    let one_doc_reader = get_only_leaf_reader(&one_doc_directory_reader)?;
    writer.close()?;

    // Now feed the index to codec APIs manually. Use an FS directory: in-memory directories are not
    // guaranteed to fail when a codec writes to them after close, for example.
    let dir = new_fs_directory(
      random,
      create_temp_dir_with_prefix("justSoYouGetSomeChannelErrors")?,
    )?;
    let codec = self.get_codec()?;
    let mut segment_info = SegmentInfo::new(
      dir.clone(),
      Some((*LATEST).clone()),
      Some((*LATEST).clone()),
      "_0",
      1,
      false,
      false,
      Some(codec.clone()),
      HashMap::new(),
      StringHelper::random_id(),
      HashMap::new(),
      None,
    )?;
    let proto = one_doc_reader
      .get_field_infos()?
      .field_info_by_name("field")?
      .ok_or_else(|| LuceneError::illegal_state("missing FieldInfo for field"))?;
    let field = Arc::new(FieldInfo::new(
      proto.name.clone(),
      proto.number,
      proto.has_term_vectors(),
      proto.omits_norms(),
      proto.has_payloads(),
      *proto.get_index_options(),
      *proto.get_doc_values_type(),
      *proto.doc_values_skip_index_type(),
      proto.get_doc_values_gen(),
      HashMap::new(),
      proto.get_point_dimension_count(),
      proto.get_point_index_dimension_count(),
      proto.get_point_num_bytes(),
      proto.get_vector_dimension(),
      *proto.get_vector_encoding(),
      *proto.get_vector_similarity_function(),
      proto.is_soft_deletes_field(),
      proto.is_parent_field(),
    )?);
    let field_infos = Arc::new(FieldInfos::new(vec![Arc::clone(&field)])?);
    let flush_context = IOContext::with_flush(FlushInfo::new(1, 20))?;
    let read_context = IOContext::default_io_context()?;
    let write_state = SegmentWriteState::new(
      get_default_info_stream(),
      dir.as_ref(),
      Arc::clone(&field_infos),
      &flush_context,
    );
    let read_state = SegmentReadState::new(dir.as_ref(), Arc::clone(&field_infos), &read_context);

    // PostingsFormat
    let postings_format = codec.postings_format();
    let fake_norms = ReaderNormsProducer {
      reader: &one_doc_reader,
    };
    let mut consumer = postings_format.fields_consumer(&write_state, &segment_info)?;
    let mut fields = OneDocFields::new(&one_doc_reader)?;
    let body_result = (|| {
      consumer.write(&write_state, &segment_info, &mut fields, Some(&fake_norms))?;
      consumer.close()?;
      consumer.close()
    })();
    IOUtils::use_or_suppress_result(body_result, consumer.close())?;
    let producer = postings_format.fields_producer(&read_state, &segment_info)?;
    let body_result = producer.close().and_then(|_| producer.close());
    IOUtils::use_or_suppress_result(body_result, producer.close())?;

    // DocValuesFormat
    let doc_values_format = codec.doc_values_format();
    let values_producer = OneDocDocValuesProducer;
    let mut consumer = doc_values_format.fields_consumer(&write_state, &segment_info)?;
    let body_result = (|| {
      consumer.add_numeric_field(&write_state, &segment_info, &field, &values_producer)?;
      consumer.close()?;
      consumer.close()
    })();
    IOUtils::use_or_suppress_result(body_result, consumer.close())?;
    let producer = doc_values_format.fields_producer(&read_state, &segment_info)?;
    let body_result = producer.close().and_then(|_| producer.close());
    IOUtils::use_or_suppress_result(body_result, producer.close())?;

    // NormsFormat
    let norms_format = codec.norms_format();
    let mut values_producer = OneDocNormsProducer;
    let mut consumer = norms_format.norms_consumer(&write_state, &segment_info)?;
    let body_result = (|| {
      consumer.add_norms_field(&field, &mut values_producer)?;
      consumer.close()?;
      consumer.close()
    })();
    IOUtils::use_or_suppress_result(body_result, consumer.close())?;
    let producer = norms_format.norms_producer(&read_state, &segment_info)?;
    let body_result = producer.close().and_then(|_| producer.close());
    IOUtils::use_or_suppress_result(body_result, producer.close())?;

    // TermVectorsFormat
    let term_vectors_format = codec.term_vectors_format();
    let mut consumer =
      term_vectors_format.vectors_writer(dir.clone(), &segment_info, &flush_context)?;
    let body_result = (|| {
      consumer.start_document(1)?;
      consumer.start_field(field.as_ref(), 1, false, false, false)?;
      consumer.start_term(&crate::core::index::BytesRef::from_string("testing"), 2)?;
      consumer.finish_term()?;
      consumer.finish_field()?;
      consumer.finish_document()?;
      consumer.finish(1)?;
      consumer.close()?;
      consumer.close()
    })();
    IOUtils::use_or_suppress_result(body_result, consumer.close())?;
    let producer = term_vectors_format.vectors_reader(
      dir.as_ref(),
      &segment_info,
      Arc::clone(&field_infos),
      &read_context,
    )?;
    let body_result = producer.close().and_then(|_| producer.close());
    IOUtils::use_or_suppress_result(body_result, producer.close())?;

    // StoredFieldsFormat
    let stored_fields_format = codec.stored_fields_format();
    let mut consumer =
      stored_fields_format.fields_writer(dir.clone(), &mut segment_info, &flush_context)?;
    let body_result = (|| {
      consumer.start_document()?;
      consumer.write_field_str(field.as_ref(), "contents")?;
      consumer.finish_document()?;
      consumer.finish(1, dir.as_ref())?;
      consumer.close()?;
      consumer.close()
    })();
    IOUtils::use_or_suppress_result(body_result, consumer.close())?;
    let producer = stored_fields_format.fields_reader(
      dir.as_ref(),
      &segment_info,
      Arc::clone(&field_infos),
      &read_context,
    )?;
    let body_result = producer.close().and_then(|_| producer.close());
    IOUtils::use_or_suppress_result(body_result, producer.close())?;

    let close_result = one_doc_reader.close();
    let close_result = IOUtils::use_or_suppress_result(close_result, one_doc_index.close());
    IOUtils::use_or_suppress_result(close_result, dir.close())
  }

  /// Tests exception handling on write and openInput/createOutput.
  // TODO: This is really not ideal. Each Base*FormatTestCase should have unit tests doing
  // this. Until then, this shotgun approach prevents bugs by ensuring that a codec does not corrupt
  // the index or leak file handles.
  fn test_random_exceptions<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    // Disable slow things: we do not rely upon sleeps here.
    let dir = new_mock_directory(random)?;
    let dir = self.apply_created_version_major(random, dir)?;
    dir.set_throttling(Throttling::Never);
    dir.set_use_slow_open_closers(false);
    dir.set_random_io_exception_rate(0.001); // More rare.
    let dir = std::sync::Arc::new(dir);

    // Log all exceptions we hit, in case we fail (for debugging).
    let mut exception_log = Vec::new();

    let analyzer = MockAnalyzer::new(random);
    let mut config = new_index_writer_config_with_analyzer(random, analyzer)?;
    // Just for now, try to keep this test reproducible.
    config.set_merge_scheduler(SerialMergeScheduler::new());
    config.set_codec(self.get_codec()?);

    let num_docs = at_least(random, 500);
    let mut writer = IndexWriter::new(dir.clone(), config)?;
    let result = (|| {
      let mut allow_already_closed = false;
      let mut field_to_type = HashMap::new();
      for i in 0..num_docs {
        // Turn on exceptions for openInput/createOutput.
        dir.set_random_io_exception_rate_on_open(0.02);

        let mut document = Document::new();
        document.add(new_string_field(
          random,
          "id",
          i.to_string(),
          Store::No,
          &mut field_to_type,
        )?);
        self.add_random_fields(random, &mut document)?;

        let operation = writer.add_document(document).and_then(|_| {
          writer
            .delete_documents_with_terms(vec![Term::from_text("id", i.to_string())])
            .map(|_| ())
        });
        match operation {
          Ok(()) => {},
          Err(LuceneError::AlreadyClosed(_)) => {
            // The writer was closed by abort; reopen it now.
            dir.set_random_io_exception_rate_on_open(0.0);
            assert!(writer.is_deleter_closed()?);
            assert!(allow_already_closed);
            allow_already_closed = false;
            let analyzer = MockAnalyzer::new(random);
            let mut config = new_index_writer_config_with_analyzer(random, analyzer)?;
            config.set_merge_scheduler(SerialMergeScheduler::new());
            config.set_codec(self.get_codec()?);
            writer = IndexWriter::new(dir.clone(), config)?;
          },
          Err(error) if error.is_io_error() => {
            self.handle_fake_io_exception(error, &mut exception_log)?;
            allow_already_closed = true;
          },
          Err(error) => return Err(error),
        }

        if random.random_range(0..10) == 0 {
          // Trigger flush.
          let flush_result = if random.random_bool(0.5) {
            match directory_reader::open_from_writer(&writer) {
              Ok(reader) => {
                dir.set_random_io_exception_rate_on_open(0.0);
                let check_result = TestUtil::check_reader(&reader);
                IOUtils::use_or_suppress_result(check_result, reader.close())
              },
              Err(error) => Err(error),
            }
          } else {
            // Disable exceptions on openInput until the next iteration, or slowExists can trip a
            // scarier assertion.
            dir.set_random_io_exception_rate_on_open(0.0);
            writer.commit().map(|_| ())
          };

          let flush_result = flush_result.and_then(|_| {
            if directory_reader::index_exists(dir.as_ref())? {
              TestUtil::check_index(random, dir.as_ref())?;
            }
            Ok(())
          });

          match flush_result {
            Ok(()) => {},
            Err(LuceneError::AlreadyClosed(_)) => {
              // The writer was closed by abort; reopen it now.
              dir.set_random_io_exception_rate_on_open(0.0);
              assert!(writer.is_deleter_closed()?);
              assert!(allow_already_closed);
              allow_already_closed = false;
              let analyzer = MockAnalyzer::new(random);
              let mut config = new_index_writer_config_with_analyzer(random, analyzer)?;
              config.set_merge_scheduler(SerialMergeScheduler::new());
              config.set_codec(self.get_codec()?);
              writer = IndexWriter::new(dir.clone(), config)?;
            },
            Err(error) if error.is_io_error() => {
              self.handle_fake_io_exception(error, &mut exception_log)?;
              allow_already_closed = true;
            },
            Err(error) => return Err(error),
          }
        }
      }

      dir.set_random_io_exception_rate_on_open(0.0);
      match writer.close() {
        Ok(()) => {},
        Err(error) if error.is_io_error() => {
          self.handle_fake_io_exception(error, &mut exception_log)?;
          let _ = writer.rollback();
        },
        Err(error) => return Err(error),
      }
      dir.close()
    })();

    if let Err(error) = result {
      eprintln!(
        "Unexpected exception: dumping fake-exception-log:\n{}",
        exception_log.join("\n")
      );
      return Err(error);
    }

    if cfg!(feature = "test_log_verbose") {
      eprintln!(
        "TEST PASSED: dumping fake-exception-log:\n{}",
        exception_log.join("\n")
      );
    }
    Ok(())
  }

  fn handle_fake_io_exception(
    &self,
    error: LuceneError,
    exception_log: &mut Vec<String>,
  ) -> Result<()> {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(&error);
    while let Some(exception) = current {
      if exception.to_string().starts_with("a random IOException") {
        exception_log.push(format!("TEST: got expected fake exc: {error}"));
        return Ok(());
      }
      current = exception.source();
    }

    Err(error)
  }

  /// Returns `false` if only the regular fields reader should be tested, and `true` if only the
  /// merge instance should be tested.
  fn should_test_merge_instance(&self) -> bool {
    false
  }

  fn maybe_wrap_with_merging_reader<D>(&self, reader: D) -> Result<DirectoryReaderEnum<D>>
  where
    D: DirectoryReader<DirectoryReader = D>,
    D::LeafReader: CodecReader + Clone,
  {
    DirectoryReaderEnum::new(reader, self.should_test_merge_instance())
  }

  /// Best-effort verification that opening a reader and calling checkIntegrity reads all bytes of
  /// all files.
  fn test_check_integrity_reads_all_bytes<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    // Codecs has no SimpleText variant, whose files do not store checksums.
    let directory = FileTrackingDirectoryWrapper::new(new_directory(random)?);
    let directory = self.apply_created_version_major(random, directory)?;
    let directory = Arc::new(directory);

    let analyzer = MockAnalyzer::new(random);
    let config = new_index_writer_config_with_analyzer(random, analyzer)?;
    let writer = IndexWriter::new(directory.clone(), config)?;
    let num_docs = at_least(random, 100);
    for _ in 0..num_docs {
      let mut document = Document::new();
      self.add_random_fields(random, &mut document)?;
      writer.add_document(document)?;
    }
    writer.force_merge(1)?;
    writer.commit()?;
    writer.close()?;

    let read_bytes_directory = Arc::new(ReadBytesDirectoryWrapper::new(directory.clone()));
    let reader = directory_reader::open(read_bytes_directory.clone())?;
    let leaf_reader = get_only_leaf_reader(&reader)?;
    leaf_reader.check_integrity()?;

    let read_bytes = read_bytes_directory.get_read_bytes();
    let mut unread_files = directory.get_files();
    eprintln!("{:?}", directory.list_all()?);
    for name in read_bytes.keys() {
      unread_files.remove(name);
    }
    unread_files.remove(WRITE_LOCK_NAME);
    assert!(
      unread_files.is_empty(),
      "Some files have not been opened: {unread_files:?}"
    );

    let mut messages = Vec::new();
    for (name, read) in read_bytes {
      let mut unread_bytes = read.clone();
      unread_bytes.flip_range(0, unread_bytes.length());
      let unread = unread_bytes.next_set_bit(0);
      if unread != i32::MAX as usize {
        messages.push(format!(
          "Offset {unread} of file {name} ({} bytes) was not read.",
          unread_bytes.length()
        ));
      }
    }
    assert!(messages.is_empty(), "{}", messages.join("\n"));
    reader.close()?;
    directory.close()?;
    Ok(())
  }
}
