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
use crate::core::codecs::dummy::stored_fields_writer::DummyStoredFieldsWriter;
use crate::core::document::document::Document;
use crate::core::document::document_stored_field_visitor::DocumentStoredFieldVisitor;
use crate::core::index::directory_reader;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::Builder;
use crate::core::index::field_infos::FieldNumbers;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_reader::{Identity, IndexReader};
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::OpenMode;
use crate::core::index::indexable_field::IndexableField;
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::merge_policy::MergePolicy;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::store::directory::{DirEnum, Directory};
use crate::core::store::{BufferedIndexInput, BufferedIndexInputBase, IOContext, IndexInput};
use crate::core::util::HasIdentity;
use crate::core::util::clone::TryClone;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::doc_helper::{
  DocHelper, NO_TF_KEY, TEXT_FIELD_1_KEY, TEXT_FIELD_2_KEY, TEXT_FIELD_3_KEY,
};
use crate::test_framework::core::util::lucene_test_case::{
  create_temp_dir_with_prefix, new_directory_shared, new_fs_directory,
  new_index_writer_config_with_analyzer, new_log_merge_policy, random,
};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};

#[allow(dead_code)] // for quick search
struct TestFieldsReader;

struct TestFieldsReaderContext {
  test_doc: Document,
  dir: Arc<DirEnum>,
}

static CONTEXT: LazyLock<TestFieldsReaderContext> = LazyLock::new(|| {
  before_class().expect("failed to initialize TestFieldsReader class-level test data")
});

fn before_class() -> Result<TestFieldsReaderContext> {
  let mut random = random();

  let mut test_doc = Document::new();
  let mut field_infos = Builder::new(Arc::new(Mutex::new(FieldNumbers::new::<String, String>(
    None, None,
  )?)));
  DocHelper::setup_doc(&mut test_doc);

  for field in test_doc.get_fields() {
    let ift = field.field_type();
    field_infos.add(Arc::new(FieldInfo::new(
      field.name().to_string(),
      -1,
      false,
      ift.omit_norms(),
      false,
      *ift.index_options(),
      *ift.doc_values_type(),
      *ift.doc_values_skip_index_type(),
      -1,
      HashMap::new(),
      0,
      0,
      0,
      0,
      VectorEncoding::FLOAT32(4),
      VectorSimilarityFunction::Euclidean,
      false,
      false,
    )?))?;
  }

  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let mut mp = new_log_merge_policy(&mut random)?;
  mp.get_base_mut().set_no_cfs_ratio(0.0)?;
  conf.set_merge_policy(mp);

  let writer = IndexWriter::new(dir.clone(), conf)?;
  writer.add_document(test_doc.clone())?;
  writer.close()?;

  Ok(TestFieldsReaderContext { test_doc, dir })
}
#[test]
fn test() -> Result<()> {
  let context = &*CONTEXT;
  let reader = directory_reader::open(context.dir.clone())?;
  let doc = reader.stored_fields()?.document(0)?;
  assert!(doc.get_field(TEXT_FIELD_1_KEY).is_some());

  let field = doc.get_field(TEXT_FIELD_2_KEY);
  assert!(field.is_some());
  let field = field.unwrap();
  assert!(field.field_type().store_term_vectors());
  assert!(!field.field_type().omit_norms());
  assert_eq!(
    IndexOptions::DocsAndFreqsAndPositions,
    *field.field_type().index_options()
  );

  let field = doc.get_field(TEXT_FIELD_3_KEY);
  assert!(field.is_some());
  let field = field.unwrap();
  assert!(!field.field_type().store_term_vectors());
  assert!(field.field_type().omit_norms());
  assert_eq!(
    IndexOptions::DocsAndFreqsAndPositions,
    *field.field_type().index_options()
  );

  let field = doc.get_field(NO_TF_KEY);
  assert!(field.is_some());
  let field = field.unwrap();
  assert!(!field.field_type().store_term_vectors());
  assert!(!field.field_type().omit_norms());
  assert_eq!(IndexOptions::Docs, *field.field_type().index_options());
  let mut v = HashSet::new();
  v.insert(TEXT_FIELD_3_KEY.to_string());
  let mut visitor = DocumentStoredFieldVisitor::with_fields(&v);
  reader.stored_fields()?.document_with_visitor(
    0,
    &mut visitor,
    Some(&mut DummyStoredFieldsWriter),
  )?;
  let visited_doc = visitor.get_document_ref();
  let fields = visited_doc.get_fields();

  assert_eq!(1, fields.len());
  assert_eq!(TEXT_FIELD_3_KEY, fields[0].name());

  Ok(())
}
#[test]
fn test_exceptions() -> Result<()> {
  let mut random = random();
  let context = &*CONTEXT;

  let fs_dir = new_fs_directory(
    &mut random,
    create_temp_dir_with_prefix("testfieldswriterexceptions")?,
  )?;
  let dir = Arc::new(FaultyFSDirectory::new(fs_dir));
  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  iwc.set_open_mode(OpenMode::Create);
  let writer = IndexWriter::new(dir.clone(), iwc)?;
  for _ in 0..2 {
    writer.add_document(context.test_doc.clone())?;
  }
  writer.force_merge(1)?;
  writer.close()?;

  let reader = directory_reader::open(dir.clone())?;
  dir.start_failing();

  let mut exc = false;

  let mut stored_fields = reader.stored_fields()?;
  for i in 0..2 {
    if stored_fields.document(i).is_err() {
      // expected
      exc = true;
    }
    if stored_fields.document(i).is_err() {
      // expected
      exc = true;
    }
  }
  assert!(exc);
  reader.close()?;

  Ok(())
}

struct FaultyFSDirectory<D> {
  in_: D,
  do_fail: Arc<AtomicBool>,
  id: Identity,
}

impl<D> FaultyFSDirectory<D>
where
  D: Directory,
{
  fn new(in_: D) -> Self {
    Self {
      in_,
      do_fail: Arc::new(AtomicBool::new(false)),
      id: Identity::new(),
    }
  }

  fn start_failing(&self) {
    self.do_fail.store(true, Ordering::SeqCst);
  }
}

impl<D> Display for FaultyFSDirectory<D>
where
  D: Directory,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", self.in_)
  }
}

impl<D> CloseableRef for FaultyFSDirectory<D>
where
  D: Directory,
{
  fn close(&self) -> Result<()> {
    self.in_.close()
  }
}

impl<D> HasIdentity for FaultyFSDirectory<D>
where
  D: Directory,
{
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl<D> Directory for FaultyFSDirectory<D>
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

  fn create_output(&self, name: &str, context: &IOContext) -> Result<Self::IndexOutput> {
    self.in_.create_output(name, context)
  }

  type IndexOutput = D::IndexOutput;

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

  type IndexInput = BufferedIndexInput<FaultyIndexInput<D::IndexInput>>;

  fn open_input(&self, name: &str, context: &IOContext) -> Result<Self::IndexInput> {
    let input = self.in_.open_input(name, context)?;
    let resource_desc = format!("FaultyIndexInput({input})");
    let faulty = FaultyIndexInput::new(self.do_fail.clone(), input);
    BufferedIndexInput::with_buffer_size(faulty, &resource_desc, 1024)
  }

  type Lock = D::Lock;

  fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
    self.in_.obtain_lock(name)
  }

  fn get_pending_deletions(&self) -> Result<HashSet<String>> {
    self.in_.get_pending_deletions()
  }
}

struct FaultyIndexInput<I> {
  do_fail: Arc<AtomicBool>,
  delegate: I,
  count: i32,
}
impl<I> FaultyIndexInput<I>
where
  I: IndexInput<IndexInput = I>,
{
  fn new(do_fail: Arc<AtomicBool>, delegate: I) -> Self {
    Self {
      do_fail,
      delegate,
      count: 0,
    }
  }
  fn sim_outage(&mut self) -> Result<()> {
    if self.do_fail.load(Ordering::SeqCst) {
      let count = self.count;
      self.count += 1;

      if count % 2 == 1 {
        return Err(LuceneError::io(std::io::Error::other(
          "Simulated network outage",
        )));
      }
    }
    Ok(())
  }
}

impl<I> TryClone for FaultyIndexInput<I>
where
  I: IndexInput<IndexInput = I>,
{
  fn try_clone(&self) -> Result<Self>
  where
    Self: Sized,
  {
    Ok(FaultyIndexInput::new(
      self.do_fail.clone(),
      self.delegate.try_clone()?,
    ))
  }
}

impl<I> CloseableRef for FaultyIndexInput<I>
where
  I: IndexInput<IndexInput = I>,
{
  fn close(&self) -> Result<()> {
    self.delegate.close()
  }
}

impl<I> BufferedIndexInputBase for FaultyIndexInput<I>
where
  I: IndexInput<IndexInput = I>,
{
  fn seek_internal(&mut self, _pos: usize) -> Result<()> {
    Ok(())
  }

  fn read_internal(
    &mut self,
    b: &mut Cursor<Vec<u8>>,
    len: usize,
    file_pointer: usize,
  ) -> Result<()> {
    self.sim_outage()?;
    self.delegate.seek(file_pointer)?;
    let offset = b.position() as usize;
    self.delegate.read_bytes(b.get_mut(), offset, len)?;

    b.set_position((offset + len) as u64);
    Ok(())
  }

  type Slice = BufferedIndexInput<FaultyIndexInput<I>>;

  fn slice(&self, slice_description: &str, offset: usize, length: usize) -> Result<Self::Slice> {
    let slice = self.delegate.slice(slice_description, offset, length)?;
    let fii = FaultyIndexInput::new(self.do_fail.clone(), slice);
    let d = format!("FaultyIndexInput({})", self.delegate);
    BufferedIndexInput::with_buffer_size(fii, &d, 1024)
  }

  fn length(&self) -> usize {
    self.delegate.length().expect("input length")
  }
}
