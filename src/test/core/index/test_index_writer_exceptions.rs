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
use crate::core::analysis::analyzer::{Analyzer, AnalyzerStoredValue, TokenStreamComponents};
use crate::core::analysis::reader::ReaderEnum;
use crate::core::analysis::reader::StringReader;
use crate::core::analysis::token_filter::{TokenFilter, TokenFilterBase};
use crate::core::analysis::token_stream::TokenStream;
use crate::core::document::binary_doc_values_field::BinaryDocValuesField;
use crate::core::document::document::Document;
use crate::core::document::field::{Field, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::fields::{FieldTokenStreamEnum, Fields};
use crate::core::document::int_point::IntPoint;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::sorted_doc_values_field::SortedDocValuesField;
use crate::core::document::sorted_numeric_doc_values_field::SortedNumericDocValuesField;
use crate::core::document::sorted_set_doc_values_field::SortedSetDocValuesField;
use crate::core::document::stored_field::StoredField;
use crate::core::document::string_field::{StringField, TYPE_NOT_STORED as STRING_NOT_STORED};
use crate::core::document::text_field::{
  TYPE_NOT_STORED as TEXT_NOT_STORED, TYPE_STORED as TEXT_STORED, TextField,
};
use crate::core::index::BytesRef;
use crate::core::index::concurrent_merge_scheduler::ConcurrentMergeScheduler;
use crate::core::index::directory_reader;
use crate::core::index::index_file_names::IndexFileNames;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::tests::INDEX_WRITER_ACCESS;
use crate::core::index::index_writer::{
  DefaultIndexWriter, IndexWriter, IndexWriterHooks, IndexWriterHooksEnum, WRITE_LOCK_NAME,
};
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::indexable_field::IndexableField;
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::merge_policy::{MergePolicy, MergePolicyEnum};
use crate::core::index::multi_bits;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::index::segment_infos::{
  SegmentInfos, get_last_commit_generation_from_directory,
  get_last_commit_segments_file_name_from_directory,
};
use crate::core::index::soft_deletes_retention_merge_policy::SoftDeletesRetentionMergePolicy;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term::Term;
use crate::core::index::term_vectors::TermVectors;
use crate::core::index::term_vectors_consumer::TermVectorsConsumer;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, NO_MORE_DOCS};
use crate::core::search::match_all_docs_query::MatchAllDocsQuery;
use crate::core::search::phrase_query::PhraseQuery;
use crate::core::store::data_input::DataInput;
use crate::core::store::data_output::DataOutput;
use crate::core::store::directory::Directory;
use crate::core::store::index_input::IndexInput;
use crate::core::store::io_context::IOContext;
use crate::core::util::attribute_source::{AttributeSource, Attributes};
use crate::core::util::bits::Bits;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind, Result};
use crate::core::util::info_stream::{InfoStream, InfoStreamEnum};
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::analysis::mock_tokenizer::{
  KEYWORD, MockTokenizer, SIMPLE, WHITESPACE,
};
use crate::test_framework::core::index::random_index_writer::{RandomIndexWriter, TestPoint};
use crate::test_framework::core::internal::index_writer_access::IndexWriterAccess;
use crate::test_framework::core::store::mock_directory_wrapper::{Failure, MockDirectoryWrapper};
use crate::test_framework::core::util::failure_context::{
  ExecutionMethod, ExecutionOwner, FailureContext, FailurePoint,
};
use crate::test_framework::core::util::lucene_test_case::{
  at_least, get_only_leaf_reader, is_night_mode, new_directory_shared, new_field,
  new_index_writer_config, new_index_writer_config_with_analyzer, new_mock_directory, new_searcher,
  new_string_field, new_text_field, random, random_from_seed, random_multiplier,
};
use crate::test_framework::core::util::test_util::TestUtil;
use parking_lot::Mutex;
use rand::prelude::StdRng;
use rand::{Rng, RngExt, SeedableRng};
use std::cell::Cell;
use std::collections::HashMap;
use std::io::Error;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::thread;

#[allow(dead_code)] // for quick search
struct TestIndexWriterExceptions;

static CUSTOM_1: LazyLock<FieldType> = LazyLock::new(|| {
  let mut field_type =
    FieldType::from_ref(&*TEXT_NOT_STORED).expect("copying TextField type should succeed");
  field_type
    .set_store_term_vectors(true)
    .expect("setting term vectors should succeed");
  field_type
    .set_store_term_vector_positions(true)
    .expect("setting term vector positions should succeed");
  field_type
    .set_store_term_vector_offsets(true)
    .expect("setting term vector offsets should succeed");
  field_type
});

static CUSTOM_2: LazyLock<FieldType> = LazyLock::new(|| {
  let mut field_type = FieldType::new();
  field_type
    .set_stored(true)
    .expect("setting stored should succeed");
  field_type
    .set_index_options(IndexOptions::DocsAndFreqsAndPositions)
    .expect("setting index options should succeed");
  field_type
});

static CUSTOM_3: LazyLock<FieldType> = LazyLock::new(|| {
  let mut field_type = FieldType::new();
  field_type
    .set_stored(true)
    .expect("setting stored should succeed");
  field_type
});

static CUSTOM_4: LazyLock<FieldType> = LazyLock::new(|| {
  let mut field_type =
    FieldType::from_ref(&*STRING_NOT_STORED).expect("copying StringField type should succeed");
  field_type
    .set_store_term_vectors(true)
    .expect("setting term vectors should succeed");
  field_type
    .set_store_term_vector_positions(true)
    .expect("setting term vector positions should succeed");
  field_type
    .set_store_term_vector_offsets(true)
    .expect("setting term vector offsets should succeed");
  field_type
});

static CUSTOM_5: LazyLock<FieldType> = LazyLock::new(|| {
  let mut field_type =
    FieldType::from_ref(&*TEXT_STORED).expect("copying TextField type should succeed");
  field_type
    .set_store_term_vectors(true)
    .expect("setting term vectors should succeed");
  field_type
    .set_store_term_vector_positions(true)
    .expect("setting term vector positions should succeed");
  field_type
    .set_store_term_vector_offsets(true)
    .expect("setting term vector offsets should succeed");
  field_type
});

thread_local! {
  static DO_FAIL: Cell<bool> = const { Cell::new(false) };
}

struct IndexerThread<'a, D>
where
  D: Directory + 'static,
{
  writer: &'a DefaultIndexWriter<D>,
  r: StdRng,
  field_types: &'a Mutex<HashMap<String, FieldType>>,
}

impl<'a, D> IndexerThread<'a, D>
where
  D: Directory + 'static,
{
  fn new(
    writer: &'a DefaultIndexWriter<D>,
    seed: u64,
    field_types: &'a Mutex<HashMap<String, FieldType>>,
  ) -> Self {
    Self {
      writer,
      r: StdRng::seed_from_u64(seed),
      field_types,
    }
  }

  fn create_doc(&mut self, id: &str) -> Result<Document> {
    let mut doc = Document::new();

    doc.add({
      let mut field_types = self.field_types.lock();
      new_text_field(
        &mut self.r,
        "content1",
        "aaa bbb ccc ddd",
        Store::Yes,
        &mut field_types,
      )?
    });
    doc.add({
      let mut field_types = self.field_types.lock();
      new_field(
        &mut self.r,
        "content6",
        "aaa bbb ccc ddd",
        &CUSTOM_1,
        &mut field_types,
      )?
    });
    doc.add({
      let mut field_types = self.field_types.lock();
      new_field(
        &mut self.r,
        "content2",
        "aaa bbb ccc ddd",
        &CUSTOM_2,
        &mut field_types,
      )?
    });
    doc.add({
      let mut field_types = self.field_types.lock();
      new_field(
        &mut self.r,
        "content3",
        "aaa bbb ccc ddd",
        &CUSTOM_3,
        &mut field_types,
      )?
    });

    doc.add({
      let mut field_types = self.field_types.lock();
      new_text_field(
        &mut self.r,
        "content4",
        "aaa bbb ccc ddd",
        Store::No,
        &mut field_types,
      )?
    });
    doc.add({
      let mut field_types = self.field_types.lock();
      new_string_field(
        &mut self.r,
        "content5",
        "aaa bbb ccc ddd",
        Store::No,
        &mut field_types,
      )?
    });
    doc.add(NumericDocValuesField::new("numericdv", 5));
    doc.add(BinaryDocValuesField::new(
      "binarydv",
      BytesRef::from_string("hello"),
    ));
    doc.add(SortedDocValuesField::new(
      "sorteddv",
      BytesRef::from_string("world"),
    ));
    doc.add(SortedSetDocValuesField::new(
      "sortedsetdv",
      BytesRef::from_string("hellllo"),
    ));
    doc.add(SortedSetDocValuesField::new(
      "sortedsetdv",
      BytesRef::from_string("again"),
    ));
    doc.add(SortedNumericDocValuesField::new("sortednumericdv", 10));
    doc.add(SortedNumericDocValuesField::new("sortednumericdv", 5));

    doc.add({
      let mut field_types = self.field_types.lock();
      new_field(
        &mut self.r,
        "content7",
        "aaa bbb ccc ddd",
        &CUSTOM_4,
        &mut field_types,
      )?
    });

    doc.add({
      let mut field_types = self.field_types.lock();
      new_field(&mut self.r, "id", id, &CUSTOM_2, &mut field_types)?
    });
    Ok(doc)
  }

  fn run(mut self) -> Result<()> {
    let max_iterations = 250;
    let mut iterations = 0;
    loop {
      DO_FAIL.with(|do_fail| do_fail.set(true));
      let id = self.r.random_range(0..50).to_string();
      let mut doc = self.create_doc(&id)?;
      let id_term = Term::from_text("id", &id);
      let result = if self.r.random() {
        let count = TestUtil::next_usize(&mut self.r, 1, 20);
        let mut docs = Vec::with_capacity(count);
        for _ in 0..count {
          docs.push(self.create_doc(&id)?);
        }
        self
          .writer
          .update_documents_with_term(Some(id_term.clone()), docs)
      } else {
        self
          .writer
          .update_document_with_term(Some(id_term.clone()), &mut doc)
      };
      if let Err(error) = result {
        if !matches!(error.kind(), LuceneErrorKind::IllegalState(_)) {
          DO_FAIL.with(|do_fail| do_fail.set(false));
          return Err(error);
        }
        TestUtil::check_index(&mut self.r, self.writer.get_directory())?;
      }

      DO_FAIL.with(|do_fail| do_fail.set(false));

      // After a possible error (above) I should be able
      // to add a new document without hitting an
      // error:
      self
        .writer
        .update_document_with_term(Some(id_term), &mut doc)?;

      iterations += 1;
      if iterations >= max_iterations {
        break;
      }
    }
    Ok(())
  }
}

struct TestPoint1 {
  random: Mutex<StdRng>,
}

impl TestPoint1 {
  fn new(seed: u64) -> Self {
    Self {
      random: Mutex::new(StdRng::seed_from_u64(seed)),
    }
  }
}

impl TestPoint for TestPoint1 {
  fn apply(&self, name: &str) -> Result<()> {
    if DO_FAIL.with(Cell::get)
      && name != "startDoFlush"
      && self.random.lock().random_range(0..40) == 17
    {
      return Err(LuceneError::illegal_state(format!(
        "{:?}: intentionally failing at {name}",
        thread::current().id()
      )));
    }
    Ok(())
  }
}

// LUCENE-1198
#[derive(Clone, Default)]
struct TestPoint2 {
  do_fail: Arc<AtomicBool>,
}

impl TestPoint for TestPoint2 {
  fn apply(&self, name: &str) -> Result<()> {
    if self.do_fail.load(Ordering::SeqCst) && name == "DocumentsWriterPerThread addDocuments start"
    {
      return Err(LuceneError::illegal_state("intentionally failing"));
    }
    Ok(())
  }
}

const CRASH_FAIL_MESSAGE: &str = "I'm experiencing problems";

struct CrashingFilter<T>
where
  T: TokenStream,
{
  base: TokenFilterBase<T>,
  field_name: String,
  count: usize,
}

impl<T> CrashingFilter<T>
where
  T: TokenStream,
{
  fn new<T2>(field_name: T2, input: T) -> Self
  where
    T2: Into<String>,
  {
    Self {
      base: TokenFilterBase::new(input),
      field_name: field_name.into(),
      count: 0,
    }
  }
}

impl<T> Closeable for CrashingFilter<T>
where
  T: TokenStream,
{
  fn close(&mut self) -> Result<()> {
    self.base.close()
  }
}

impl<T> TokenStream for CrashingFilter<T>
where
  T: TokenStream,
{
  fn increment_token(&mut self) -> Result<bool> {
    if self.field_name == "crash" && self.count >= 4 {
      return Err(LuceneError::io(Error::other(CRASH_FAIL_MESSAGE)));
    }
    self.count += 1;
    self.base.input.increment_token()
  }

  fn reset(&mut self) -> Result<()> {
    self.base.reset()?;
    self.count = 0;
    Ok(())
  }

  fn end(&mut self) -> Result<()> {
    self.base.end()
  }

  fn get_attribute_source(&self) -> &Attributes {
    self.base.input.get_attribute_source()
  }

  fn get_attribute_source_mut(&mut self) -> &mut Attributes {
    self.base.input.get_attribute_source_mut()
  }

  fn set_reader(&mut self, input: ReaderEnum) -> Result<()> {
    self.base.input.set_reader(input)
  }

  fn set_reader_test_point(&mut self) -> Result<()> {
    self.base.input.set_reader_test_point()
  }
}

impl<T> TokenFilter for CrashingFilter<T> where T: TokenStream {}

struct CrashingAnalyzer {
  stored_value: AnalyzerStoredValue,
  do_crash: Option<Arc<AtomicBool>>,
  seed: u64,
}

impl CrashingAnalyzer {
  fn new(seed: u64) -> Self {
    Self {
      stored_value: AnalyzerStoredValue::per_field(),
      do_crash: None,
      seed,
    }
  }

  fn with_flag(seed: u64, do_crash: Arc<AtomicBool>) -> Self {
    Self {
      stored_value: AnalyzerStoredValue::per_field(),
      do_crash: Some(do_crash),
      seed,
    }
  }
}

impl Analyzer for CrashingAnalyzer {
  fn create_components(&self, field_name: &str) -> Result<TokenStreamComponents> {
    let mut tokenizer = MockTokenizer::with_default_max_token_length(
      random_from_seed(self.seed),
      WHITESPACE.clone(),
      false,
    );
    tokenizer.set_enable_checks(false); // disable workflow checking as we forcefully close() in exceptional cases.
    let should_crash = self
      .do_crash
      .as_ref()
      .is_none_or(|do_crash| do_crash.load(Ordering::SeqCst));
    if should_crash {
      Ok(TokenStreamComponents::new(
        Box::new(CrashingFilter::new(field_name, tokenizer)) as Box<dyn TokenStream + Send + Sync>,
        None,
      ))
    } else {
      Ok(TokenStreamComponents::new(
        Box::new(tokenizer) as Box<dyn TokenStream + Send + Sync>,
        None,
      ))
    }
  }

  fn stored_value(&self) -> &AnalyzerStoredValue {
    &self.stored_value
  }
}

crate::impl_analyzer_close!(CrashingAnalyzer);

struct ThrowingAnalyzer {
  stored_value: AnalyzerStoredValue,
  seed: u64,
}

impl Analyzer for ThrowingAnalyzer {
  fn create_components(&self, _field_name: &str) -> Result<TokenStreamComponents> {
    let mut tokenizer = MockTokenizer::with_default_max_token_length(
      random_from_seed(self.seed),
      SIMPLE.clone(),
      true,
    );
    tokenizer.set_enable_checks(false); // disable workflow checking as we forcefully close() in exceptional cases.
    Ok(TokenStreamComponents::new(
      Box::new(ThrowingFilter {
        base: TokenFilterBase::new(tokenizer),
        count: 0,
      }) as Box<dyn TokenStream + Send + Sync>,
      None,
    ))
  }

  fn stored_value(&self) -> &AnalyzerStoredValue {
    &self.stored_value
  }
}

crate::impl_analyzer_close!(ThrowingAnalyzer);

struct ThrowingFilter<T>
where
  T: TokenStream,
{
  base: TokenFilterBase<T>,
  count: usize,
}

impl<T> Closeable for ThrowingFilter<T>
where
  T: TokenStream,
{
  fn close(&mut self) -> Result<()> {
    self.base.close()
  }
}

impl<T> TokenStream for ThrowingFilter<T>
where
  T: TokenStream,
{
  fn increment_token(&mut self) -> Result<bool> {
    if self.count == 5 {
      return Err(LuceneError::io(Error::other("token stream failure")));
    }
    self.count += 1;
    self.base.input.increment_token()
  }

  fn reset(&mut self) -> Result<()> {
    self.base.reset()?;
    self.count = 0;
    Ok(())
  }

  fn end(&mut self) -> Result<()> {
    self.base.end()
  }

  fn get_attribute_source(&self) -> &Attributes {
    self.base.input.get_attribute_source()
  }

  fn get_attribute_source_mut(&mut self) -> &mut Attributes {
    self.base.input.get_attribute_source_mut()
  }

  fn set_reader(&mut self, input: ReaderEnum) -> Result<()> {
    self.base.input.set_reader(input)
  }

  fn set_reader_test_point(&mut self) -> Result<()> {
    self.base.input.set_reader_test_point()
  }
}

impl<T> TokenFilter for ThrowingFilter<T> where T: TokenStream {}

struct NegativeGapAnalyzer {
  stored_value: AnalyzerStoredValue,
  seed: u64,
}

impl Analyzer for NegativeGapAnalyzer {
  fn create_components(&self, _field_name: &str) -> Result<TokenStreamComponents> {
    let tokenizer = MockTokenizer::with_default_max_token_length(
      random_from_seed(self.seed),
      KEYWORD.clone(),
      false,
    );
    Ok(TokenStreamComponents::new(
      Box::new(tokenizer) as Box<dyn TokenStream + Send + Sync>,
      None,
    ))
  }

  fn get_position_increment_gap(&self, _field_name: &str) -> i32 {
    -2
  }

  fn stored_value(&self) -> &AnalyzerStoredValue {
    &self.stored_value
  }
}

crate::impl_analyzer_close!(NegativeGapAnalyzer);

struct EnableTestPoints;

impl IndexWriterHooks for EnableTestPoints {
  fn is_enable_test_points(&self) -> bool {
    true
  }
}

struct EvilRollbackInfoStream {
  message_to_fail_on: &'static str,
}

impl CloseableRef for EvilRollbackInfoStream {
  fn close(&self) -> Result<()> {
    Ok(())
  }
}

impl InfoStream for EvilRollbackInfoStream {
  fn message(&self, _component: &str, message: &str) -> Result<()> {
    if message == self.message_to_fail_on {
      return Err(LuceneError::illegal_state("BOOM!"));
    }
    Ok(())
  }

  fn is_enabled(&self, _component: &str) -> bool {
    true
  }
}

struct OutOfMemoryInfoStream {
  message_to_fail_on: &'static str,
  starts_with: bool,
  thrown: AtomicBool,
}

impl CloseableRef for OutOfMemoryInfoStream {
  fn close(&self) -> Result<()> {
    Ok(())
  }
}

impl InfoStream for OutOfMemoryInfoStream {
  fn message(&self, _component: &str, message: &str) -> Result<()> {
    let matches = if self.starts_with {
      message.starts_with(self.message_to_fail_on)
    } else {
      message.contains(self.message_to_fail_on)
    };
    if matches && !self.thrown.swap(true, Ordering::SeqCst) {
      panic!("fake OOME at {message}");
    }
    Ok(())
  }

  fn is_enabled(&self, _component: &str) -> bool {
    true
  }
}

struct RollbackOnceInfoStream {
  once: AtomicBool,
}

impl CloseableRef for RollbackOnceInfoStream {
  fn close(&self) -> Result<()> {
    Ok(())
  }
}

impl InfoStream for RollbackOnceInfoStream {
  fn message(&self, component: &str, message: &str) -> Result<()> {
    if component == "TP" && message == "rollback before checkpoint" {
      if !self.once.swap(true, Ordering::SeqCst) {
        return Err(LuceneError::illegal_state("boom"));
      }
      return Err(LuceneError::illegal_state("has been rolled back twice"));
    }
    Ok(())
  }

  fn is_enabled(&self, component: &str) -> bool {
    component == "TP"
  }
}

#[cfg_attr(not(feature = "nightly"), allow(dead_code))]
struct TooManyTokensStream {
  attrs: Attributes,
  num: i64,
}

impl Closeable for TooManyTokensStream {}

impl TokenStream for TooManyTokensStream {
  fn increment_token(&mut self) -> Result<bool> {
    if self.num == i32::MAX as i64 + 1 {
      return Ok(false);
    }
    self.attrs.clear_attributes()?;
    self
      .attrs
      .set_position_increment(if self.num == 0 { 1 } else { 0 })?;
    self.attrs.append_str(Some("a"))?;
    self.num += 1;
    Ok(true)
  }

  fn reset(&mut self) -> Result<()> {
    self.num = 0;
    Ok(())
  }

  fn end(&mut self) -> Result<()> {
    self.default_end()
  }

  fn get_attribute_source(&self) -> &Attributes {
    &self.attrs
  }

  fn get_attribute_source_mut(&mut self) -> &mut Attributes {
    &mut self.attrs
  }
}

#[derive(Clone, Default)]
struct FailOnlyOnFlush {
  do_fail: Arc<AtomicBool>,
  count: Arc<Mutex<usize>>,
}

impl<D> Failure<D> for FailOnlyOnFlush
where
  D: Directory,
{
  fn set_do_fail(&mut self) {
    self.do_fail.store(true, Ordering::SeqCst);
  }

  fn clear_do_fail(&mut self) {
    self.do_fail.store(false, Ordering::SeqCst);
  }

  fn eval_with_context(
    &mut self,
    _dir: &MockDirectoryWrapper<D>,
    context: &FailureContext,
  ) -> Result<()> {
    if self.do_fail.load(Ordering::SeqCst)
      && context.contains_method(ExecutionMethod::Flush)
      && !context.contains_method(ExecutionMethod::FinishDocument)
    {
      let mut count = self.count.lock();
      let should_fail = *count >= 30;
      *count += 1;
      if should_fail {
        self.do_fail.store(false, Ordering::SeqCst);
        return Err(LuceneError::io(Error::other("now failing during flush")));
      }
    }
    Ok(())
  }
}

// Returns an I/O error from `MockDirectoryWrapper::sync`.
#[derive(Clone, Default)]
struct FailOnlyInSync {
  do_fail: Arc<AtomicBool>,
  did_fail: Arc<AtomicBool>,
}

impl<D> Failure<D> for FailOnlyInSync
where
  D: Directory,
{
  fn eval_with_context(
    &mut self,
    _dir: &MockDirectoryWrapper<D>,
    context: &FailureContext,
  ) -> Result<()> {
    if self.do_fail.load(Ordering::SeqCst) && context.point() == FailurePoint::Sync {
      self.did_fail.store(true, Ordering::SeqCst);
      return Err(LuceneError::io(Error::other(
        "now failing on purpose during sync",
      )));
    }
    Ok(())
  }

  fn set_do_fail(&mut self) {
    self.do_fail.store(true, Ordering::SeqCst);
  }

  fn clear_do_fail(&mut self) {
    self.do_fail.store(false, Ordering::SeqCst);
  }
}

fn add_doc<D>(writer: &DefaultIndexWriter<D>) -> Result<()>
where
  D: Directory + 'static,
{
  let mut doc = Document::new();
  doc.add(TextField::from_string("content", "aaa", Store::No)?);
  writer.add_document(doc)?;
  Ok(())
}

#[derive(Clone)]
struct FailOnlyInCommit {
  fail_on_commit: Arc<AtomicBool>,
  fail_on_delete_file: Arc<AtomicBool>,
  fail_on_sync_metadata: Arc<AtomicBool>,
  dont_fail_during_global_field_map: bool,
  dont_fail_during_sync_metadata: bool,
  stage: ExecutionMethod,
  do_fail: bool,
}

// LUCENE-1347
#[derive(Clone, Default)]
struct TestPoint4 {
  do_fail: Arc<AtomicBool>,
}

impl TestPoint for TestPoint4 {
  fn apply(&self, name: &str) -> Result<()> {
    if self.do_fail.load(Ordering::SeqCst) && name == "rollback before checkpoint" {
      return Err(LuceneError::illegal_state("intentionally failing"));
    }
    Ok(())
  }
}

impl FailOnlyInCommit {
  const PREPARE_STAGE: ExecutionMethod = ExecutionMethod::PrepareCommit;
  const FINISH_STAGE: ExecutionMethod = ExecutionMethod::FinishCommit;

  fn new(
    dont_fail_during_global_field_map: bool,
    dont_fail_during_sync_metadata: bool,
    stage: ExecutionMethod,
  ) -> Self {
    Self {
      fail_on_commit: Arc::new(AtomicBool::new(false)),
      fail_on_delete_file: Arc::new(AtomicBool::new(false)),
      fail_on_sync_metadata: Arc::new(AtomicBool::new(false)),
      dont_fail_during_global_field_map,
      dont_fail_during_sync_metadata,
      stage,
      do_fail: false,
    }
  }
}

impl<D> Failure<D> for FailOnlyInCommit
where
  D: Directory,
{
  fn eval_with_context(
    &mut self,
    _dir: &MockDirectoryWrapper<D>,
    context: &FailureContext,
  ) -> Result<()> {
    let mut is_commit = context.contains(ExecutionOwner::SegmentInfos, self.stage);
    let is_delete = context.point() == FailurePoint::DeleteFile;
    let is_sync_metadata = context.point() == FailurePoint::SyncMetadata;
    let is_in_global_field_map = context.contains(
      ExecutionOwner::SegmentInfos,
      ExecutionMethod::WriteGlobalFieldMap,
    );
    if is_in_global_field_map && self.dont_fail_during_global_field_map {
      is_commit = false;
    }
    if is_sync_metadata && self.dont_fail_during_sync_metadata {
      is_commit = false;
    }
    if is_delete
      && self.fail_on_commit.load(Ordering::SeqCst)
      && !self.fail_on_sync_metadata.load(Ordering::SeqCst)
      && !self.fail_on_delete_file.swap(true, Ordering::SeqCst)
    {
      return Err(LuceneError::io(Error::other("now fail during delete")));
    }
    if is_commit {
      if !is_delete {
        self.fail_on_commit.store(true, Ordering::SeqCst);
        self
          .fail_on_sync_metadata
          .store(is_sync_metadata, Ordering::SeqCst);
        return Err(LuceneError::illegal_state("now fail first"));
      }
      self.fail_on_delete_file.store(true, Ordering::SeqCst);
      return Err(LuceneError::io(Error::other("now fail during delete")));
    }
    Ok(())
  }

  fn set_do_fail(&mut self) {
    self.do_fail = true;
  }

  fn clear_do_fail(&mut self) {
    self.do_fail = false;
  }
}

#[derive(Clone)]
struct FailOnTermVectors {
  stage: ExecutionMethod,
  do_fail: bool,
}

#[derive(Clone)]
struct TooManyFilesFailure {
  do_fail: Arc<AtomicBool>,
  random: Arc<Mutex<StdRng>>,
}

impl TooManyFilesFailure {
  fn new(seed: u64) -> Self {
    Self {
      do_fail: Arc::new(AtomicBool::new(false)),
      random: Arc::new(Mutex::new(StdRng::seed_from_u64(seed))),
    }
  }
}

impl<D> Failure<D> for TooManyFilesFailure
where
  D: Directory,
{
  fn reset(&mut self) {
    self.do_fail.store(false, Ordering::SeqCst);
  }

  fn eval(&mut self, _dir: &MockDirectoryWrapper<D>) -> Result<()> {
    if self.do_fail.load(Ordering::SeqCst) && self.random.lock().random() {
      return Err(LuceneError::io(Error::new(
        std::io::ErrorKind::NotFound,
        "some/file/name.ext (Too many open files)",
      )));
    }
    Ok(())
  }

  fn set_do_fail(&mut self) {
    self.do_fail.store(true, Ordering::SeqCst);
  }

  fn clear_do_fail(&mut self) {
    self.do_fail.store(false, Ordering::SeqCst);
  }
}

#[derive(Clone)]
struct RandomRollbackFailure {
  random: Arc<Mutex<StdRng>>,
  do_fail: bool,
}

impl RandomRollbackFailure {
  fn new(seed: u64) -> Self {
    Self {
      random: Arc::new(Mutex::new(StdRng::seed_from_u64(seed))),
      do_fail: false,
    }
  }
}

impl<D> Failure<D> for RandomRollbackFailure
where
  D: Directory,
{
  fn eval_with_context(
    &mut self,
    _dir: &MockDirectoryWrapper<D>,
    context: &FailureContext,
  ) -> Result<()> {
    if self.random.lock().random_range(0..10) == 0
      && context.contains_method(ExecutionMethod::RollbackInternal)
    {
      return Err(LuceneError::io(Error::other("a fake I/O error")));
    }
    Ok(())
  }

  fn set_do_fail(&mut self) {
    self.do_fail = true;
  }

  fn clear_do_fail(&mut self) {
    self.do_fail = false;
  }
}

#[derive(Clone)]
#[cfg_attr(not(feature = "nightly"), allow(dead_code))]
struct MergeFailure {
  random: Arc<Mutex<StdRng>>,
  did_fail: Arc<AtomicBool>,
  do_fail: bool,
}

impl<D> Failure<D> for MergeFailure
where
  D: Directory,
{
  fn eval_with_context(
    &mut self,
    _dir: &MockDirectoryWrapper<D>,
    context: &FailureContext,
  ) -> Result<()> {
    if self.random.lock().random_range(0..10) != 0 || self.did_fail.load(Ordering::SeqCst) {
      return Ok(());
    }
    if context.contains_method(ExecutionMethod::Merge) {
      self.did_fail.store(true, Ordering::SeqCst);
      return Err(LuceneError::io(Error::other("a fake I/O error")));
    }
    Ok(())
  }

  fn set_do_fail(&mut self) {
    self.do_fail = true;
  }

  fn clear_do_fail(&mut self) {
    self.do_fail = false;
  }
}

#[derive(Clone)]
struct SyncMetadataFailure {
  random: Arc<Mutex<StdRng>>,
  maybe_fail_delete: Arc<AtomicBool>,
  do_fail: bool,
}

#[derive(Clone, Default)]
struct UOEDirectoryFailure {
  do_fail: Arc<AtomicBool>,
}

impl<D> Failure<D> for UOEDirectoryFailure
where
  D: Directory,
{
  fn eval(&mut self, _dir: &MockDirectoryWrapper<D>) -> Result<()> {
    if self.do_fail.load(Ordering::SeqCst) {
      return Err(LuceneError::unsupported_operation("expected UOE"));
    }
    Ok(())
  }

  fn set_do_fail(&mut self) {
    self.do_fail.store(true, Ordering::SeqCst);
  }

  fn clear_do_fail(&mut self) {
    self.do_fail.store(false, Ordering::SeqCst);
  }
}

impl<D> Failure<D> for SyncMetadataFailure
where
  D: Directory,
{
  fn eval_with_context(
    &mut self,
    _dir: &MockDirectoryWrapper<D>,
    context: &FailureContext,
  ) -> Result<()> {
    if context.point() == FailurePoint::SyncMetadata
      && context.contains(ExecutionOwner::SegmentInfos, ExecutionMethod::FinishCommit)
    {
      return Err(LuceneError::illegal_state("boom"));
    }
    if self.maybe_fail_delete.load(Ordering::SeqCst)
      && self.random.lock().random()
      && context.contains(
        ExecutionOwner::IndexWriter,
        ExecutionMethod::RollbackInternalNoCommit,
      )
      && context.contains_method(ExecutionMethod::DeleteFiles)
    {
      return Err(LuceneError::illegal_state("bang"));
    }
    Ok(())
  }

  fn set_do_fail(&mut self) {
    self.do_fail = true;
  }

  fn clear_do_fail(&mut self) {
    self.do_fail = false;
  }
}

impl FailOnTermVectors {
  const INIT_STAGE: ExecutionMethod = ExecutionMethod::InitTermVectorsWriter;
  const AFTER_INIT_STAGE: ExecutionMethod = ExecutionMethod::FinishDocument;
  const EXC_MSG: &'static str = "FOTV";

  fn new(stage: ExecutionMethod) -> Self {
    Self {
      stage,
      do_fail: false,
    }
  }
}

impl<D> Failure<D> for FailOnTermVectors
where
  D: Directory,
{
  fn eval_with_context(
    &mut self,
    _dir: &MockDirectoryWrapper<D>,
    context: &FailureContext,
  ) -> Result<()> {
    if context.contains(ExecutionOwner::TermVectorsConsumer, self.stage) {
      return Err(LuceneError::illegal_state(Self::EXC_MSG));
    }
    Ok(())
  }

  fn set_do_fail(&mut self) {
    self.do_fail = true;
  }

  fn clear_do_fail(&mut self) {
    self.do_fail = false;
  }
}

#[derive(Clone, Default)]
struct TestPoint3 {
  do_fail: Arc<AtomicBool>,
  failed: Arc<AtomicBool>,
}

impl TestPoint for TestPoint3 {
  fn apply(&self, name: &str) -> Result<()> {
    if self.do_fail.load(Ordering::SeqCst) && name == "startMergeInit" {
      self.failed.store(true, Ordering::SeqCst);
      return Err(LuceneError::illegal_state("intentionally failing"));
    }
    Ok(())
  }
}

#[test]
fn test_random_exceptions() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mut analyzer = MockAnalyzer::new(&mut random);
  analyzer.set_enable_checks(false); // disable workflow checking as we forcefully close() in exceptional cases.
  let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  config.set_ram_buffer_size_mb(0.1);
  let merge_scheduler = ConcurrentMergeScheduler::new();
  merge_scheduler.set_suppress_exceptions();
  config.set_merge_scheduler(merge_scheduler);
  let test_point = TestPoint1::new(random.random());
  let writer = RandomIndexWriter::mock_index_writer_with_test_point(
    &mut random,
    dir.clone(),
    config,
    test_point,
  )?;
  writer.commit()?;

  let field_types = Mutex::new(HashMap::new());
  IndexerThread::new(&writer, random.random(), &field_types).run()?;

  writer.commit()?;
  if writer.close().is_err() {
    writer.rollback()?;
  }

  // Confirm that when doc hits error partway through tokenization, it's deleted:
  let reader = directory_reader::open(dir.clone())?;
  let count = reader.doc_freq(&Term::from_text("content4", "aaa"))?;
  let count2 = reader.doc_freq(&Term::from_text("content4", "ddd"))?;
  assert_eq!(count, count2);
  reader.close()?;

  dir.as_ref().close()?;
  Ok(())
}

#[test]
fn test_random_exceptions_threads() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut analyzer = MockAnalyzer::new(&mut random);
  analyzer.set_enable_checks(false); // disable workflow checking as we forcefully close() in exceptional cases.
  let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  config.set_ram_buffer_size_mb(0.2);
  let merge_scheduler = ConcurrentMergeScheduler::new();
  merge_scheduler.set_suppress_exceptions();
  config.set_merge_scheduler(merge_scheduler);
  let test_point = TestPoint1::new(random.random());
  let writer = RandomIndexWriter::mock_index_writer_with_test_point(
    &mut random,
    dir.clone(),
    config,
    test_point,
  )?;
  writer.commit()?;

  const NUM_THREADS: usize = 4;
  let field_types = Mutex::new(HashMap::new());
  let seeds: Vec<u64> = (0..NUM_THREADS).map(|_| random.random()).collect();
  thread::scope(|scope| -> Result<()> {
    let mut threads = Vec::with_capacity(NUM_THREADS);
    for seed in seeds {
      let writer = &writer;
      let field_types = &field_types;
      threads.push(scope.spawn(move || IndexerThread::new(writer, seed, field_types).run()));
    }
    for thread in threads {
      thread.join().map_err(|panic| {
        LuceneError::tragedy_from_panic("indexer thread panicked", panic.as_ref())
      })??;
    }
    Ok(())
  })?;

  writer.commit()?;
  if writer.close().is_err() {
    writer.rollback()?;
  }

  // Confirm that when doc hits error partway through tokenization, it's deleted:
  let reader = directory_reader::open(dir.clone())?;
  let count = reader.doc_freq(&Term::from_text("content4", "aaa"))?;
  let count2 = reader.doc_freq(&Term::from_text("content4", "ddd"))?;
  assert_eq!(count, count2);
  reader.close()?;

  dir.as_ref().close()?;
  Ok(())
}

#[test]
fn test_exception_documents_writer_init() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let test_point = TestPoint2::default();
  let analyzer = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let writer = RandomIndexWriter::mock_index_writer_with_test_point(
    &mut random,
    dir.clone(),
    config,
    test_point.clone(),
  )?;
  let mut doc = Document::new();
  doc.add(TextField::from_string("field", "a field", Store::Yes)?);
  writer.add_document(&mut doc)?;

  test_point.do_fail.store(true, Ordering::SeqCst);
  assert!(matches!(
    (writer.add_document(doc))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalState(_))
  ));

  writer.close()?;
  dir.as_ref().close()?;
  Ok(())
}

#[test]
fn test_exception_just_before_flush() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let do_crash = Arc::new(AtomicBool::new(false));
  let analyzer = Box::new(CrashingAnalyzer::with_flag(
    random.random(),
    do_crash.clone(),
  )) as Box<dyn Analyzer>;
  let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  config.set_max_buffered_docs(2);
  let test_point = TestPoint1::new(random.random());
  let writer = RandomIndexWriter::mock_index_writer_with_test_point(
    &mut random,
    dir.clone(),
    config,
    test_point,
  )?;
  let mut doc = Document::new();
  doc.add(TextField::from_string("field", "a field", Store::Yes)?);
  writer.add_document(&mut doc)?;

  let mut crash_doc = Document::new();
  crash_doc.add(TextField::from_string(
    "crash",
    "do it on token 4",
    Store::Yes,
  )?);
  do_crash.store(true, Ordering::SeqCst);
  let error = writer
    .add_document(crash_doc)
    .expect_err("the crashing token stream must fail");
  assert!(error.to_string().contains(CRASH_FAIL_MESSAGE));

  writer.add_document(doc)?;
  writer.close()?;
  dir.as_ref().close()?;
  Ok(())
}

#[test]
fn test_exception_on_merge_init() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  config.set_max_buffered_docs(2);
  let mut merge_policy =
    crate::test_framework::core::util::lucene_test_case::new_log_merge_policy_with_merge_factor(
      &mut random,
      2,
    )?;
  match &mut merge_policy {
    crate::core::index::merge_policy::MergePolicyEnum::LogDoc(policy) => {
      policy.set_target_search_concurrency(1)?;
    },
    crate::core::index::merge_policy::MergePolicyEnum::LogBytesSize(policy) => {
      policy.set_target_search_concurrency(1)?;
    },
    _ => unreachable!("new_log_merge_policy must return a log merge policy"),
  }
  config.set_merge_policy(merge_policy);
  let merge_scheduler = ConcurrentMergeScheduler::new();
  merge_scheduler.set_suppress_exceptions();
  config.set_merge_scheduler(merge_scheduler);
  let test_point = TestPoint3::default();
  let writer = RandomIndexWriter::mock_index_writer_with_test_point(
    &mut random,
    dir.clone(),
    config,
    test_point.clone(),
  )?;
  test_point.do_fail.store(true, Ordering::SeqCst);
  let mut doc = Document::new();
  doc.add(TextField::from_string("field", "a field", Store::Yes)?);
  for _ in 0..10 {
    if writer.add_document(&mut doc).is_err() {
      break;
    }
  }

  let sync_result = (|| -> Result<()> {
    if let crate::core::index::merge_scheduler::MergeSchedulerEnum::Concurrent(scheduler) =
      writer.get_config()?.get_merge_scheduler()
    {
      scheduler.sync()?;
    }
    Ok(())
  })();
  match sync_result {
    Ok(()) => {},
    Err(error) if error.is_illegal_state_error() => {},
    Err(error) => return Err(error),
  }
  assert!(test_point.failed.load(Ordering::SeqCst));
  writer.close()?;
  dir.as_ref().close()?;
  Ok(())
}

#[test]
fn test_exception_from_token_stream() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = Box::new(ThrowingAnalyzer {
    stored_value: AnalyzerStoredValue::new(),
    seed: random.random(),
  }) as Box<dyn Analyzer>;
  let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  config.set_max_buffered_docs(config.get_max_buffered_docs().max(3));
  config.set_merge_policy(NoMergePolicy::default());

  let writer = IndexWriter::new(dir.clone(), config)?;

  let mut broken_doc = Document::new();
  let contents = "aa bb cc dd ee ff gg hh ii jj kk";
  broken_doc.add(TextField::from_string("content", contents, Store::No)?);
  assert!(writer.add_document(broken_doc).is_err());

  // Make sure we can add another normal document
  let mut doc = Document::new();
  doc.add(TextField::from_string("content", "aa bb cc dd", Store::No)?);
  writer.add_document(doc)?;

  // Make sure we can add another normal document
  let mut doc = Document::new();
  doc.add(TextField::from_string("content", "aa bb cc dd", Store::No)?);
  writer.add_document(doc)?;

  writer.close()?;
  let reader = directory_reader::open(dir.clone())?;
  let term = Term::from_text("content", "aa");
  assert_eq!(3, reader.doc_freq(&term)?);

  // Make sure the doc that hit the error was marked
  // as deleted:
  let mut term_docs =
    TestUtil::docs_with_reader(&mut random, &reader, term.field(), term.bytes(), None, 0)?
      .expect("postings should exist");

  let live_docs = multi_bits::get_live_docs(&reader)?;
  let mut count = 0;
  while term_docs.next_doc()? != NO_MORE_DOCS {
    if live_docs
      .as_ref()
      .is_none_or(|bits| bits.get(term_docs.doc_id() as usize).unwrap_or(false))
    {
      count += 1;
    }
  }
  assert_eq!(2, count);

  assert_eq!(0, reader.doc_freq(&Term::from_text("content", "gg"))?);
  reader.close()?;
  dir.as_ref().close()?;
  Ok(())
}

#[test]
fn test_documents_writer_abort() -> Result<()> {
  let mut random = random();
  let dir = Arc::new(new_mock_directory(&mut random)?);
  let failure = FailOnlyOnFlush::default();
  failure.do_fail.store(true, Ordering::SeqCst);
  dir.fail_on(Box::new(failure));

  let analyzer = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  config.set_max_buffered_docs(2);
  let writer = IndexWriter::new(dir.clone(), config)?;
  let mut doc = Document::new();
  let contents = "aa bb cc dd ee ff gg hh ii jj kk";
  doc.add(TextField::from_string("content", contents, Store::No)?);
  let mut hit_error = false;
  writer.add_document(&mut doc)?;

  writer
    .add_document(doc)
    .expect_err("the second document must fail while flushing");

  // only one flush should fail:
  assert!(!hit_error);
  hit_error = true;
  assert!(hit_error);
  assert!(writer.is_deleter_closed()?);
  assert!(INDEX_WRITER_ACCESS.is_closed(&writer));
  assert!(!directory_reader::index_exists(dir.as_ref())?);

  dir.as_ref().close()?;
  Ok(())
}

#[test]
fn test_documents_writer_exceptions() -> Result<()> {
  let mut random = random();
  for i in 0..2 {
    let dir = new_directory_shared(&mut random)?;
    let analyzer = Box::new(CrashingAnalyzer::new(random.random())) as Box<dyn Analyzer>;
    let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
    let mut merge_policy =
      crate::test_framework::core::util::lucene_test_case::new_log_merge_policy(&mut random)?;
    match &mut merge_policy {
      crate::core::index::merge_policy::MergePolicyEnum::LogDoc(policy) => {
        policy.set_merge_factor(policy.get_merge_factor().max(5))?;
      },
      crate::core::index::merge_policy::MergePolicyEnum::LogBytesSize(policy) => {
        policy.set_merge_factor(policy.get_merge_factor().max(5))?;
      },
      _ => unreachable!("new_log_merge_policy must return a log merge policy"),
    }
    config.set_merge_policy(merge_policy);
    let writer = IndexWriter::new(dir.clone(), config)?;

    let mut doc = Document::new();
    doc.add(Field::new(
      "contents",
      "here are some contents",
      CUSTOM_5.clone(),
    ));
    writer.add_document(&mut doc)?;
    writer.add_document(&mut doc)?;
    doc.add(Field::new(
      "crash",
      "this should crash after 4 terms",
      CUSTOM_5.clone(),
    ));
    doc.add(Field::new(
      "other",
      "this will not get indexed",
      CUSTOM_5.clone(),
    ));
    let error = writer
      .add_document(doc)
      .expect_err("the crashing token stream must fail");
    assert!(error.to_string().contains(CRASH_FAIL_MESSAGE));

    if i == 0 {
      let mut doc = Document::new();
      doc.add(Field::new(
        "contents",
        "here are some contents",
        CUSTOM_5.clone(),
      ));
      writer.add_document(&mut doc)?;
      writer.add_document(&mut doc)?;
    }
    writer.close()?;

    let reader = directory_reader::open(dir.clone())?;
    if i == 0 {
      let expected = 5;
      assert_eq!(
        expected,
        reader.doc_freq(&Term::from_text("contents", "here"))?
      );
      assert_eq!(expected, reader.max_doc()?);
      let mut num_del = 0;
      let live_docs = multi_bits::get_live_docs(&reader)?.expect("there must be one deleted doc");
      let mut stored_fields = reader.stored_fields()?;
      let mut term_vectors = reader.term_vectors()?;
      for j in 0..reader.max_doc()? {
        if !live_docs.get(j as usize)? {
          num_del += 1;
        } else {
          stored_fields.document(j)?;
          term_vectors.get(j)?;
        }
      }
      assert_eq!(1, num_del);
    }
    reader.close()?;

    let analyzer = Box::new(CrashingAnalyzer::new(random.random())) as Box<dyn Analyzer>;
    let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
    config.set_max_buffered_docs(10);
    let writer = IndexWriter::new(dir.clone(), config)?;
    let mut doc = Document::new();
    doc.add(Field::new(
      "contents",
      "here are some contents",
      CUSTOM_5.clone(),
    ));
    for _ in 0..17 {
      writer.add_document(&mut doc)?;
    }
    writer.force_merge(1)?;
    writer.close()?;

    let reader = directory_reader::open(dir.clone())?;
    let expected = 19 + (1 - i) * 2;
    assert_eq!(
      expected,
      reader.doc_freq(&Term::from_text("contents", "here"))?
    );
    assert_eq!(expected, reader.max_doc()?);
    assert!(multi_bits::get_live_docs(&reader)?.is_none());
    let mut stored_fields = reader.stored_fields()?;
    let mut term_vectors = reader.term_vectors()?;
    for j in 0..reader.max_doc()? {
      stored_fields.document(j)?;
      term_vectors.get(j)?;
    }
    reader.close()?;

    dir.as_ref().close()?;
  }
  Ok(())
}

#[test]
fn test_documents_writer_exception_fail_one_doc() -> Result<()> {
  let mut random = random();
  for _ in 0..10 {
    let dir = new_directory_shared(&mut random)?;
    let analyzer = Box::new(CrashingAnalyzer::new(random.random())) as Box<dyn Analyzer>;
    let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
    config.set_max_buffered_docs(-1);
    config.set_ram_buffer_size_mb(if random.random() {
      0.00001
    } else {
      i32::MAX as f64
    });
    config.set_merge_policy(
      crate::test_framework::core::index::merge_policy::KeepFullyDeletedSegmentsMergePolicy::new(
        NoMergePolicy::default(),
      ),
    );
    let writer = IndexWriter::new(dir.clone(), config)?;
    let mut doc = Document::new();
    doc.add(Field::new(
      "contents",
      "here are some contents",
      CUSTOM_5.clone(),
    ));
    writer.add_document(&mut doc)?;
    doc.add(Field::new(
      "crash",
      "this should crash after 4 terms",
      CUSTOM_5.clone(),
    ));
    doc.add(Field::new(
      "other",
      "this will not get indexed",
      CUSTOM_5.clone(),
    ));
    let error = writer
      .add_document(doc)
      .expect_err("the crashing token stream must fail");
    assert!(error.to_string().contains(CRASH_FAIL_MESSAGE));
    writer.commit()?;
    let reader = directory_reader::open(dir.clone())?;
    assert_eq!(2, reader.doc_freq(&Term::from_text("contents", "here"))?);
    assert_eq!(2, reader.max_doc()?);
    assert_eq!(1, reader.num_docs()?);
    reader.close()?;
    writer.close()?;
    dir.as_ref().close()?;
  }
  Ok(())
}

#[test]
fn test_documents_writer_exception_threads() -> Result<()> {
  let mut random = random();
  const NUM_THREAD: i32 = 3;
  let num_iter = at_least(&mut random, 10);

  for i in 0..2 {
    let dir = new_directory_shared(&mut random)?;
    let analyzer = Box::new(CrashingAnalyzer::new(random.random())) as Box<dyn Analyzer>;
    let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
    config.set_max_buffered_docs(i32::MAX);
    config.set_ram_buffer_size_mb(-1.0); // we don't want to flush automatically
    config.set_merge_policy(
      crate::test_framework::core::index::merge_policy::KeepFullyDeletedSegmentsMergePolicy::new(
        NoMergePolicy::default(),
      ),
    );
    let writer = IndexWriter::new(dir.clone(), config)?;

    thread::scope(|scope| -> Result<()> {
      let mut threads = Vec::with_capacity(NUM_THREAD as usize);
      for _ in 0..NUM_THREAD {
        let writer = &writer;
        threads.push(scope.spawn(move || -> Result<()> {
          for _ in 0..num_iter {
            let mut doc = Document::new();
            doc.add(Field::new(
              "contents",
              "here are some contents",
              CUSTOM_5.clone(),
            ));
            writer.add_document(&mut doc)?;
            writer.add_document(&mut doc)?;
            doc.add(Field::new(
              "crash",
              "this should crash after 4 terms",
              CUSTOM_5.clone(),
            ));
            doc.add(Field::new(
              "other",
              "this will not get indexed",
              CUSTOM_5.clone(),
            ));
            let error = writer
              .add_document(doc)
              .expect_err("the crashing token stream must fail");
            assert!(error.to_string().contains(CRASH_FAIL_MESSAGE));

            if i == 0 {
              let mut extra_doc = Document::new();
              extra_doc.add(Field::new(
                "contents",
                "here are some contents",
                CUSTOM_5.clone(),
              ));
              writer.add_document(&mut extra_doc)?;
              writer.add_document(extra_doc)?;
            }
          }
          Ok(())
        }));
      }
      for thread in threads {
        thread.join().map_err(|panic| {
          LuceneError::tragedy_from_panic("documents writer thread panicked", panic.as_ref())
        })??;
      }
      Ok(())
    })?;

    writer.close()?;

    let reader = directory_reader::open(dir.clone())?;
    let mut expected = (3 + (1 - i) * 2) * NUM_THREAD * num_iter;
    assert_eq!(
      expected,
      reader.doc_freq(&Term::from_text("contents", "here"))?
    );
    assert_eq!(expected, reader.max_doc()?);
    let mut num_del = 0;
    let live_docs = multi_bits::get_live_docs(&reader)?.expect("failed docs must be deleted");
    let mut stored_fields = reader.stored_fields()?;
    let mut term_vectors = reader.term_vectors()?;
    for j in 0..reader.max_doc()? {
      if !live_docs.get(j as usize)? {
        num_del += 1;
      } else {
        stored_fields.document(j)?;
        term_vectors.get(j)?;
      }
    }
    reader.close()?;
    assert_eq!(NUM_THREAD * num_iter, num_del);

    let analyzer = Box::new(CrashingAnalyzer::new(random.random())) as Box<dyn Analyzer>;
    let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
    config.set_max_buffered_docs(10);
    let writer = IndexWriter::new(dir.clone(), config)?;
    let mut doc = Document::new();
    doc.add(Field::new(
      "contents",
      "here are some contents",
      CUSTOM_5.clone(),
    ));
    for _ in 0..17 {
      writer.add_document(&mut doc)?;
    }
    writer.force_merge(1)?;
    writer.close()?;

    let reader = directory_reader::open(dir.clone())?;
    expected += 17 - NUM_THREAD * num_iter;
    assert_eq!(
      expected,
      reader.doc_freq(&Term::from_text("contents", "here"))?
    );
    assert_eq!(expected, reader.max_doc()?);
    assert!(multi_bits::get_live_docs(&reader)?.is_none());
    let mut stored_fields = reader.stored_fields()?;
    let mut term_vectors = reader.term_vectors()?;
    for j in 0..reader.max_doc()? {
      stored_fields.document(j)?;
      term_vectors.get(j)?;
    }
    reader.close()?;
    dir.as_ref().close()?;
  }
  Ok(())
}

#[test]
fn test_exception_during_sync() -> Result<()> {
  let mut random = random();
  let dir = Arc::new(new_mock_directory(&mut random)?);
  let failure = FailOnlyInSync::default();
  dir.fail_on(Box::new(failure.clone()));

  let analyzer = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  config.set_max_buffered_docs(2);
  let merge_scheduler = ConcurrentMergeScheduler::new();
  merge_scheduler.set_suppress_exceptions();
  config.set_merge_scheduler(merge_scheduler);
  config.set_merge_policy(
    crate::test_framework::core::util::lucene_test_case::new_log_merge_policy_with_merge_factor(
      &mut random,
      5,
    )?,
  );
  let writer = IndexWriter::new(dir.clone(), config)?;
  failure.do_fail.store(true, Ordering::SeqCst);

  for i in 0..23 {
    add_doc(&writer)?;
    if (i - 1) % 2 == 0 {
      let _ = writer.commit();
    }
  }
  if let crate::core::index::merge_scheduler::MergeSchedulerEnum::Concurrent(scheduler) =
    writer.get_config()?.get_merge_scheduler()
  {
    scheduler.sync()?;
  }
  assert!(failure.did_fail.load(Ordering::SeqCst));
  failure.do_fail.store(false, Ordering::SeqCst);
  writer.close()?;

  let reader = directory_reader::open(dir.clone())?;
  assert_eq!(23, reader.num_docs()?);
  reader.close()?;
  dir.as_ref().close()?;
  Ok(())
}

#[test]
fn test_exceptions_during_commit() -> Result<()> {
  let failures = [
    // LUCENE-1214
    FailOnlyInCommit::new(false, true, FailOnlyInCommit::PREPARE_STAGE), // fail during global field map is written
    FailOnlyInCommit::new(true, false, FailOnlyInCommit::PREPARE_STAGE), // fail during sync metadata
    FailOnlyInCommit::new(true, true, FailOnlyInCommit::PREPARE_STAGE), // fail after global field map is written
    FailOnlyInCommit::new(false, true, FailOnlyInCommit::FINISH_STAGE), // fail while running finishCommit
  ];

  let mut random = random();
  for failure in failures {
    let dir = Arc::new(new_mock_directory(&mut random)?);
    dir.set_fail_on_create_output(false);
    let file_count = dir.list_all()?.len();
    let analyzer = MockAnalyzer::new(&mut random);
    let config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
    let writer = IndexWriter::new(dir.clone(), config)?;
    let mut doc = Document::new();
    doc.add(TextField::from_string("field", "a field", Store::Yes)?);
    writer.add_document(doc)?;
    dir.fail_on(Box::new(failure.clone()));
    assert!(writer.close().is_err());
    assert!(
      failure.fail_on_commit.load(Ordering::SeqCst)
        && (failure.fail_on_delete_file.load(Ordering::SeqCst)
          || failure.fail_on_sync_metadata.load(Ordering::SeqCst))
    );
    writer.rollback()?;
    let files = dir.list_all()?;
    assert!(
      files.len() == file_count
        || (files.len() == file_count + 1 && files.iter().any(|file| file == WRITE_LOCK_NAME)),
      "unexpected files after failed commit: initial={file_count}, files={files:?}"
    );
    dir.as_ref().close()?;
  }
  Ok(())
}

#[test]
fn test_force_merge_exceptions() -> Result<()> {
  let mut random = random();
  let start_dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  config.set_max_buffered_docs(2);
  config.set_merge_policy(
    crate::test_framework::core::util::lucene_test_case::new_log_merge_policy_with_merge_factor(
      &mut random,
      100,
    )?,
  );
  let writer = IndexWriter::new(start_dir.clone(), config)?;
  for _ in 0..27 {
    add_doc(&writer)?;
  }
  writer.close()?;

  let iterations = if is_night_mode() { 200 } else { 10 };
  for _ in 0..iterations {
    let copy = TestUtil::ram_copy_of(&mut random, start_dir.as_ref())?;
    let dir = Arc::new(MockDirectoryWrapper::new(&mut random, copy));
    let analyzer = MockAnalyzer::new(&mut random);
    let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
    let merge_scheduler = ConcurrentMergeScheduler::new();
    merge_scheduler.set_suppress_exceptions();
    config.set_merge_scheduler(merge_scheduler);
    let writer = IndexWriter::new(dir.clone(), config)?;
    dir.set_random_io_exception_rate(0.5);
    match writer.force_merge(1) {
      Ok(()) => {},
      Err(error) if error.is_illegal_state_error() || error.is_io_error() => {},
      Err(error) => return Err(error),
    }
    dir.set_random_io_exception_rate(0.0);
    match writer.close() {
      Ok(()) => {},
      Err(error) if error.is_illegal_state_error() => {},
      Err(error) => return Err(error),
    }
    dir.as_ref().close()?;
  }
  start_dir.as_ref().close()?;
  Ok(())
}

#[test]
fn test_out_of_memory_error_causes_close_to_fail() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  config.set_info_stream(InfoStreamEnum::Custom(Box::new(OutOfMemoryInfoStream {
    message_to_fail_on: "now flush at close",
    starts_with: true,
    thrown: AtomicBool::new(false),
  })));
  let writer = IndexWriter::new(dir.clone(), config)?;

  let close_result = catch_unwind(AssertUnwindSafe(|| writer.close()));
  assert!(
    close_result.is_err(),
    "close must propagate the fake OOME panic"
  );

  writer.close()?;
  dir.as_ref().close()?;
  Ok(())
}

#[test]
fn test_out_of_memory_error_rollback() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  config.set_info_stream(InfoStreamEnum::Custom(Box::new(OutOfMemoryInfoStream {
    message_to_fail_on: "startFullFlush",
    starts_with: false,
    thrown: AtomicBool::new(false),
  })));
  let writer = IndexWriter::new(dir.clone(), config)?;

  writer.add_document(Document::new())?;

  let commit_result = catch_unwind(AssertUnwindSafe(|| writer.commit()));
  assert!(
    commit_result.is_err(),
    "commit must propagate the fake OOME panic"
  );

  if let Err(error) = writer.close() {
    assert!(matches!(error.kind(), LuceneErrorKind::IllegalArgument(_)));
  }

  let error = writer
    .add_document(Document::new())
    .expect_err("a writer that rolled back after a tragic error must be closed");
  assert!(matches!(error.kind(), LuceneErrorKind::AlreadyClosed(_)));
  assert!(!directory_reader::index_exists(dir.as_ref())?);

  dir.as_ref().close()?;
  Ok(())
}

#[test]
fn test_rollback_exception_hang() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let test_point = TestPoint4::default();
  let analyzer = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let writer = RandomIndexWriter::mock_index_writer_with_test_point(
    &mut random,
    dir.clone(),
    config,
    test_point.clone(),
  )?;

  add_doc(&writer)?;
  test_point.do_fail.store(true, Ordering::SeqCst);
  assert!(matches!(
    (writer.rollback()).as_ref().map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalState(_))
  ));

  test_point.do_fail.store(false, Ordering::SeqCst);
  writer.rollback()?;
  dir.as_ref().close()?;
  Ok(())
}

#[test]
fn test_segments_checksum_error() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  dir.set_check_index_on_close(false);
  // we corrupt the index
  let analyzer = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let writer = IndexWriter::new(dir.clone(), config)?;

  // add 100 documents
  for _ in 0..100 {
    add_doc(&writer)?;
  }

  // close
  writer.close()?;

  let generation = get_last_commit_generation_from_directory(dir.as_ref())?;
  assert!(
    generation > 0,
    "segment generation should be > 0 but got {generation}"
  );

  let segments_file_name = get_last_commit_segments_file_name_from_directory(dir.as_ref())?
    .expect("the committed segments file should exist");
  let context = IOContext::read_once_io_context()?;
  let mut input = dir.open_input(&segments_file_name, &context)?;
  let output_name =
    IndexFileNames::file_name_from_generation(IndexFileNames::SEGMENTS, "", generation + 1)
      .expect("a positive generation has a file name");
  let mut output = dir.create_output(&output_name, &context)?;
  let length = input.length()?;
  output.copy_bytes(&mut input, length - 1)?;
  let byte = input.read_byte()?;
  output.write_byte(byte.wrapping_add(1))?;
  output.close()?;
  input.close()?;

  assert!(directory_reader::open(dir.clone()).is_err());
  dir.as_ref().close()?;
  Ok(())
}

#[test]
fn test_simulated_corrupt_index1() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  dir.set_check_index_on_close(false);
  // we are corrupting it!
  let analyzer = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let writer = IndexWriter::new(dir.clone(), config)?;

  // add 100 documents
  for _ in 0..100 {
    add_doc(&writer)?;
  }

  // close
  writer.close()?;

  let generation = get_last_commit_generation_from_directory(dir.as_ref())?;
  assert!(
    generation > 0,
    "segment generation should be > 0 but got {generation}"
  );

  let file_name_in = get_last_commit_segments_file_name_from_directory(dir.as_ref())?
    .expect("the committed segments file should exist");
  let file_name_out =
    IndexFileNames::file_name_from_generation(IndexFileNames::SEGMENTS, "", generation + 1)
      .expect("a positive generation has a file name");
  let context = IOContext::read_once_io_context()?;
  let mut input = dir.open_input(&file_name_in, &context)?;
  let mut output = dir.create_output(&file_name_out, &context)?;
  let length = input.length()?;
  for _ in 0..length - 1 {
    output.write_byte(input.read_byte()?)?;
  }
  input.close()?;
  output.close()?;
  dir.delete_file(&file_name_in)?;

  assert!(directory_reader::open(dir.clone()).is_err());
  dir.as_ref().close()?;
  Ok(())
}

#[test]
#[allow(clippy::never_loop)]
fn test_simulated_corrupt_index2() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  dir.set_check_index_on_close(false);
  // we are corrupting it!
  let analyzer = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let mut merge_policy =
    crate::test_framework::core::util::lucene_test_case::new_log_merge_policy_with_cfs(
      &mut random,
      true,
    )?;
  merge_policy.get_base_mut().set_no_cfs_ratio(1.0)?;
  merge_policy
    .get_base_mut()
    .set_max_cfs_segment_size_mb(f64::INFINITY)?;
  config.set_merge_policy(merge_policy);
  config.set_use_compound_file(true);
  let writer = IndexWriter::new(dir.clone(), config)?;

  // add 100 documents
  for _ in 0..100 {
    add_doc(&writer)?;
  }

  // close
  writer.close()?;

  let generation = get_last_commit_generation_from_directory(dir.as_ref())?;
  assert!(
    generation > 0,
    "segment generation should be > 0 but got {generation}"
  );

  let mut corrupted = false;
  let segment_infos = SegmentInfos::read_latest_commit(dir.clone())?;
  for segment in segment_infos.iter() {
    assert!(segment.info.get_use_compound_file());
    let victims = segment.info.files()?;
    let victim = victims
      .iter()
      .nth(random.random_range(0..victims.len()))
      .unwrap();
    dir.delete_file(victim)?;
    corrupted = true;
    break;
  }
  assert!(corrupted, "failed to find cfs file to remove");

  assert!(directory_reader::open(dir.clone()).is_err());
  dir.as_ref().close()?;
  Ok(())
}

#[test]
fn test_term_vector_exceptions() -> Result<()> {
  let failures = [
    FailOnTermVectors::new(FailOnTermVectors::AFTER_INIT_STAGE),
    FailOnTermVectors::new(FailOnTermVectors::INIT_STAGE),
  ];
  let mut random = random();
  let num = at_least(&mut random, 1);
  'iters: for _ in 0..num {
    for failure in &failures {
      let dir = Arc::new(new_mock_directory(&mut random)?);
      let analyzer = MockAnalyzer::new(&mut random);
      let config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
      let writer = IndexWriter::new(dir.clone(), config)?;
      dir.fail_on(Box::new(failure.clone()));
      let num_docs = 10 + random.random_range(0..30);
      let mut field_types = HashMap::new();
      for _ in 0..num_docs {
        let mut doc = Document::new();
        // random TV
        let field = new_text_field(
          &mut random,
          "field",
          "a field",
          Store::Yes,
          &mut field_types,
        )?;
        let stores_term_vectors = field.field_type().store_term_vectors();
        doc.add(field);
        match writer.add_document(doc) {
          Ok(_) => assert!(!stores_term_vectors),
          Err(error) => {
            assert!(
              error.to_string().contains(FailOnTermVectors::EXC_MSG),
              "unexpected error: {error:?}"
            );
            // This is an aborting error, so writer is closed:
            assert!(writer.is_deleter_closed()?);
            assert!(INDEX_WRITER_ACCESS.is_closed(&writer));
            dir.as_ref().close()?;
            continue 'iters;
          },
        }
        if random.random_range(0..20) == 0 {
          writer.commit()?;
          TestUtil::check_index(&mut random, dir.as_ref())?;
        }
      }
      let mut document = Document::new();
      document.add(TextField::from_string("field", "a field", Store::Yes)?);
      writer.add_document(document)?;

      for _ in 0..num_docs {
        let mut doc = Document::new();
        let field = new_text_field(
          &mut random,
          "field",
          "a field",
          Store::Yes,
          &mut field_types,
        )?;
        let stores_term_vectors = field.field_type().store_term_vectors();
        doc.add(field);
        match writer.add_document(doc) {
          Ok(_) => assert!(!stores_term_vectors),
          Err(error) => {
            assert!(
              error.to_string().contains(FailOnTermVectors::EXC_MSG),
              "unexpected error: {error:?}"
            );
          },
        }
        if random.random_range(0..20) == 0 {
          writer.commit()?;
          TestUtil::check_index(&mut random, dir.as_ref())?;
        }
      }
      let mut document = Document::new();
      document.add(TextField::from_string("field", "a field", Store::Yes)?);
      writer.add_document(document)?;
      writer.close()?;
      let reader = directory_reader::open(dir.clone())?;
      assert!(reader.num_docs()? > 0);
      SegmentInfos::read_latest_commit(dir.clone())?;
      let context = (&reader).get_context()?;
      for leaf in context.leaves()? {
        assert!(!leaf.reader().get_field_infos()?.has_term_vectors());
      }
      reader.close()?;
      dir.as_ref().close()?;
    }
  }
  Ok(())
}

#[test]
fn test_add_docs_non_aborting_exception() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = RandomIndexWriter::new(&mut random, dir.clone())?;
  let num_docs1 = random.random_range(0..25);
  for _ in 0..num_docs1 {
    let mut doc = Document::new();
    doc.add(TextField::from_string(
      "content",
      "good content",
      Store::No,
    )?);
    writer.add_document(&mut random, doc)?;
  }

  let mut docs = Vec::with_capacity(7);
  for doc_count in 0..7 {
    let mut doc = Document::new();
    doc.add(StringField::from_string(
      "id",
      doc_count.to_string(),
      Store::No,
    )?);
    doc.add(TextField::from_string(
      "content",
      format!("silly content {doc_count}"),
      Store::No,
    )?);
    if doc_count == 4 {
      let mut tokenizer = MockTokenizer::with_default_max_token_length(
        random_from_seed(random.random()),
        WHITESPACE.clone(),
        false,
      );
      tokenizer.set_reader(StringReader::new("crash me on the 4th token").into())?;
      tokenizer.set_enable_checks(false); // disable workflow checking as we forcefully close() in exceptional cases.
      let field = Field::from_token_stream(
        "crash",
        FieldTokenStreamEnum::custom(CrashingFilter::new("crash", tokenizer)),
        TEXT_NOT_STORED.clone(),
      )?;
      doc.add(field);
    }
    docs.push(doc);
  }

  let error = writer
    .add_documents(&mut random, docs)
    .expect_err("the crashing token stream must fail the batch");
  assert!(error.to_string().contains(CRASH_FAIL_MESSAGE));

  let num_docs2 = random.random_range(0..25);
  for _ in 0..num_docs2 {
    let mut doc = Document::new();
    doc.add(TextField::from_string(
      "content",
      "good content",
      Store::No,
    )?);
    writer.add_document(&mut random, doc)?;
  }

  let reader = writer.get_reader(&mut random)?;
  writer.close(&mut random)?;

  let searcher = new_searcher(&mut random, reader)?;
  let query = PhraseQuery::from_terms_no_slop("content", ["silly", "good"])?;
  assert_eq!(0, searcher.count(query)?);

  let query = PhraseQuery::from_terms_no_slop("content", ["good", "content"])?;
  assert_eq!(num_docs1 + num_docs2, searcher.count(query)?);
  searcher.get_index_reader().close()?;
  dir.as_ref().close()?;
  Ok(())
}

#[test]
fn test_update_docs_non_aborting_exception() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = RandomIndexWriter::new(&mut random, dir.clone())?;
  let num_docs1 = random.random_range(0..25);
  for _ in 0..num_docs1 {
    let mut doc = Document::new();
    doc.add(TextField::from_string(
      "content",
      "good content",
      Store::No,
    )?);
    writer.add_document(&mut random, doc)?;
  }

  // Use addDocs (no error) to get docs in the index:
  let mut docs = Vec::new();
  let num_docs2 = random.random_range(0..25);
  docs.reserve(num_docs2 as usize);
  for doc_count in 0..num_docs2 {
    let mut doc = Document::new();
    doc.add(StringField::from_string("subid", "subs", Store::No)?);
    doc.add(StringField::from_string(
      "id",
      doc_count.to_string(),
      Store::No,
    )?);
    doc.add(TextField::from_string(
      "content",
      format!("silly content {doc_count}"),
      Store::No,
    )?);
    docs.push(doc);
  }
  writer.add_documents(&mut random, docs)?;

  let num_docs3 = random.random_range(0..25);
  for _ in 0..num_docs3 {
    let mut doc = Document::new();
    doc.add(TextField::from_string(
      "content",
      "good content",
      Store::No,
    )?);
    writer.add_document(&mut random, doc)?;
  }

  let mut docs = Vec::new();
  let limit = TestUtil::next_int(&mut random, 2, 25);
  let crash_at = random.random_range(0..limit);
  docs.reserve(limit as usize);
  for doc_count in 0..limit {
    let mut doc = Document::new();
    doc.add(StringField::from_string(
      "id",
      doc_count.to_string(),
      Store::No,
    )?);
    doc.add(TextField::from_string(
      "content",
      format!("silly content {doc_count}"),
      Store::No,
    )?);
    if doc_count == crash_at {
      let mut tokenizer = MockTokenizer::with_default_max_token_length(
        random_from_seed(random.random()),
        WHITESPACE.clone(),
        false,
      );
      tokenizer.set_reader(StringReader::new("crash me on the 4th token").into())?;
      tokenizer.set_enable_checks(false); // disable workflow checking as we forcefully close() in exceptional cases.
      let field = Field::from_token_stream(
        "crash",
        FieldTokenStreamEnum::custom(CrashingFilter::new("crash", tokenizer)),
        TEXT_NOT_STORED.clone(),
      )?;
      doc.add(field);
    }
    docs.push(doc);
  }

  let error = writer
    .update_documents_with_term(&mut random, Term::from_text("subid", "subs"), docs)
    .expect_err("the crashing token stream must fail the update batch");
  assert!(error.to_string().contains(CRASH_FAIL_MESSAGE));

  let num_docs4 = random.random_range(0..25);
  for _ in 0..num_docs4 {
    let mut doc = Document::new();
    doc.add(TextField::from_string(
      "content",
      "good content",
      Store::No,
    )?);
    writer.add_document(&mut random, doc)?;
  }

  let reader = writer.get_reader(&mut random)?;
  writer.close(&mut random)?;

  let searcher = new_searcher(&mut random, reader)?;
  let query = PhraseQuery::from_terms_no_slop("content", ["silly", "content"])?;
  assert_eq!(num_docs2, searcher.count(query)?);

  let query = PhraseQuery::from_terms_no_slop("content", ["good", "content"])?;
  assert_eq!(num_docs1 + num_docs3 + num_docs4, searcher.count(query)?);
  searcher.get_index_reader().close()?;
  dir.as_ref().close()?;
  Ok(())
}

#[test]
#[ignore = "Java-only: Rust stored string fields cannot contain null"]
fn test_null_stored_field() -> Result<()> {
  test_not_required_in_rust_lucene!();
}

#[test]
#[ignore = "Java-only: Rust stored string setters cannot accept null"]
fn test_null_stored_field_reuse() -> Result<()> {
  test_not_required_in_rust_lucene!();
}

#[test]
#[ignore = "Java-only: Rust stored byte fields cannot contain null arrays"]
fn test_null_stored_bytes_field() -> Result<()> {
  test_not_required_in_rust_lucene!();
}

#[test]
#[ignore = "Java-only: Rust stored byte-field setters cannot accept null arrays"]
fn test_null_stored_bytes_field_reuse() -> Result<()> {
  test_not_required_in_rust_lucene!();
}

#[test]
#[ignore = "Java-only: Rust stored BytesRef fields cannot contain null"]
fn test_null_stored_bytes_ref_field() -> Result<()> {
  test_not_required_in_rust_lucene!();
}

#[test]
#[ignore = "Java-only: Rust stored BytesRef setters cannot accept null"]
fn test_null_stored_bytes_ref_field_reuse() -> Result<()> {
  test_not_required_in_rust_lucene!();
}

#[test]
fn test_crazy_position_increment_gap() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = Box::new(NegativeGapAnalyzer {
    stored_value: AnalyzerStoredValue::new(),
    seed: random.random(),
  }) as Box<dyn Analyzer>;
  let config = IndexWriterConfig::with_analyzer(analyzer)?;
  let writer = IndexWriter::new(dir.clone(), config)?;
  // add good document
  writer.add_document(Document::new())?;
  let mut doc = Document::new();
  doc.add(TextField::from_string("foo", "bar", Store::No)?);
  doc.add(TextField::from_string("foo", "bar", Store::No)?);
  assert!(matches!(
    (writer.add_document(doc))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));

  assert!(writer.get_tragic_exception().get().is_none());
  writer.close()?;

  // make sure we see our good doc
  let reader = directory_reader::open(dir.clone())?;
  assert_eq!(1, reader.num_docs()?);
  reader.close()?;
  dir.as_ref().close()?;
  Ok(())
}

#[test]
fn test_exception_on_ctor() -> Result<()> {
  let mut random = random();
  // FilterDirectory forwarding is not available, so install the openInput failure
  // directly on MockDirectoryWrapper.
  let dir = Arc::new(new_mock_directory(&mut random)?);
  let failure = UOEDirectoryFailure::default();
  dir.fail_on(Box::new(failure.clone()));
  let writer = IndexWriter::new(dir.clone(), new_index_writer_config(&mut random)?)?;
  writer.add_document(Document::new())?;
  writer.close()?;
  failure.do_fail.store(true, Ordering::SeqCst);
  let error = match IndexWriter::new(dir.clone(), new_index_writer_config(&mut random)?) {
    Ok(_) => panic!("the constructor must propagate the injected failure"),
    Err(error) => error,
  };
  assert!(error.to_string().contains("expected UOE"));

  failure.do_fail.store(false, Ordering::SeqCst);
  dir.as_ref().close()?;
  Ok(())
}

#[test]
fn test_too_many_file_exception() -> Result<()> {
  let mut random = random();
  // Create a failure that randomly returns a "too many open files" I/O error.
  let failure = TooManyFilesFailure::new(random.random());
  let dir = Arc::new(new_mock_directory(&mut random)?);
  // The error is only returned while opening input.
  dir.set_fail_on_open_input(true);
  dir.fail_on(Box::new(failure.clone()));

  // Create an index with one document
  let analyzer = MockAnalyzer::new(&mut random);
  let config = IndexWriterConfig::with_analyzer(analyzer)?;
  let writer = IndexWriter::new(dir.clone(), config)?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("foo", "bar", Store::No)?);
  writer.add_document(doc)?;
  writer.commit()?;
  let reader = directory_reader::open(dir.clone())?;
  assert_eq!(1, reader.num_docs()?);
  reader.close()?;
  writer.close()?;

  // Open and close the index a few times
  for i in 0..10 {
    failure.do_fail.store(true, Ordering::SeqCst);
    let analyzer = MockAnalyzer::new(&mut random);
    let config = IndexWriterConfig::with_analyzer(analyzer)?;
    let writer = match IndexWriter::new(dir.clone(), config) {
      Ok(writer) => writer,
      Err(_) => continue,
    };
    failure.do_fail.store(false, Ordering::SeqCst);
    writer.close()?;
    let reader = directory_reader::open(dir.clone())?;
    assert_eq!(1, reader.num_docs()?, "lost document after iteration: {i}");
    reader.close()?;
  }

  // Check if document is still there
  failure.do_fail.store(false, Ordering::SeqCst);
  let reader = directory_reader::open(dir.clone())?;
  assert_eq!(1, reader.num_docs()?);
  reader.close()?;
  dir.as_ref().close()?;
  Ok(())
}

// kind of slow, but omits positions, so just CPU
#[cfg(feature = "nightly")]
#[test]
#[ignore = "nightly"]
fn test_too_many_tokens() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = IndexWriter::new(dir.clone(), new_index_writer_config(&mut random)?)?;
  let mut doc = Document::new();
  let mut field_type = FieldType::from_ref(&*TEXT_NOT_STORED)?;
  field_type.set_index_options(IndexOptions::DocsAndFreqs)?;
  doc.add(Field::from_token_stream(
    "foo",
    FieldTokenStreamEnum::custom(TooManyTokensStream {
      attrs: Attributes::default(),
      num: 0,
    }),
    field_type,
  )?);

  let error = writer
    .add_document(doc)
    .expect_err("more than i32::MAX tokens must fail");
  assert!(error.to_string().contains("too many tokens"));

  writer.close()?;
  dir.as_ref().close()?;
  Ok(())
}

#[test]
fn test_exception_during_rollback() -> Result<()> {
  let mut random = random();
  // currently: fail in two different places
  let message_to_fail_on = if random.random() {
    "rollback: done finish merges"
  } else {
    "rollback before checkpoint"
  };

  // Info stream that returns an error during rollback.
  let dir = Arc::new(new_mock_directory(&mut random)?); // we want to ensure we don't leak any locks or file handles
  let mut config = IndexWriterConfig::new()?;
  config.set_info_stream(InfoStreamEnum::Custom(Box::new(EvilRollbackInfoStream {
    message_to_fail_on,
  })));
  let writer = IndexWriter::with_hooks(
    dir.clone(),
    config,
    Some(IndexWriterHooksEnum::custom(EnableTestPoints)),
  )?;

  let mut doc = Document::new();
  for _ in 0..10 {
    writer.add_document(&mut doc)?;
  }
  writer.commit()?;

  writer.add_document(doc)?;

  // pool readers
  let reader = directory_reader::open_from_writer(&writer)?;

  // sometimes sneak in a pending commit: we don't want to leak a file handle to that segments_N
  if random.random() {
    writer.prepare_commit()?;
  }

  let error = writer
    .rollback()
    .expect_err("rollback test point must return an error");
  assert_eq!("BOOM!", error.to_string());

  reader.close()?;

  // even though we hit error: we are closed, no locks or files held, index in good state
  assert!(INDEX_WRITER_ACCESS.is_closed(&writer));
  let lock = dir.obtain_lock(WRITE_LOCK_NAME)?;
  lock.close()?;

  let reader = directory_reader::open(dir.clone())?;
  assert_eq!(10, reader.max_doc()?);
  reader.close()?;

  // no leaks
  dir.as_ref().close()?;
  Ok(())
}

#[test]
fn test_random_exception_during_rollback() -> Result<()> {
  let mut random = random();
  // fail in random places on i/o
  let num_iters = random_multiplier() * 75;
  for _ in 0..num_iters {
    let dir = Arc::new(new_mock_directory(&mut random)?);
    dir.fail_on(Box::new(RandomRollbackFailure::new(random.random())));

    let config = IndexWriterConfig::new()?;
    let writer = IndexWriter::new(dir.clone(), config)?;
    let mut doc = Document::new();
    for _ in 0..10 {
      writer.add_document(&mut doc)?;
    }
    writer.commit()?;

    writer.add_document(doc)?;

    // pool readers
    let reader = directory_reader::open_from_writer(&writer)?;

    // sometimes sneak in a pending commit: we don't want to leak a file handle to that segments_N
    if random.random() {
      writer.prepare_commit()?;
    }

    let _ = writer.rollback();
    reader.close()?;

    // even though we hit error: we are closed, no locks or files held, index in good state
    assert!(INDEX_WRITER_ACCESS.is_closed(&writer));
    let lock = dir.obtain_lock(WRITE_LOCK_NAME)?;
    lock.close()?;

    let reader = directory_reader::open(dir.clone())?;
    assert_eq!(10, reader.max_doc()?);
    reader.close()?;

    // no leaks
    dir.as_ref().close()?;
  }
  Ok(())
}

#[cfg(feature = "nightly")]
#[test]
#[ignore = "nightly"]
fn test_merge_exception_is_tragic() -> Result<()> {
  let mut random = random();
  let dir = Arc::new(new_mock_directory(&mut random)?);
  let did_fail = Arc::new(AtomicBool::new(false));
  dir.fail_on(Box::new(MergeFailure {
    random: Arc::new(Mutex::new(StdRng::seed_from_u64(random.random()))),
    did_fail: did_fail.clone(),
    do_fail: false,
  }));

  let config = new_index_writer_config(&mut random)?;
  if let MergePolicyEnum::Tiered(merge_policy) = &mut *config.get_merge_policy_mut()
    && merge_policy.get_max_merged_segment_mb() < 0.2
  {
    merge_policy.set_max_merged_segment_mb(0.2)?;
  }
  let concurrent_merge_scheduler =
    if let crate::core::index::merge_scheduler::MergeSchedulerEnum::Concurrent(scheduler) =
      config.get_merge_scheduler()
    {
      scheduler.set_suppress_exceptions();
      Some(scheduler.clone())
    } else {
      None
    };
  let writer = IndexWriter::new(dir.clone(), config)?;

  loop {
    let mut doc = Document::new();
    doc.add(StringField::from_string("field", "string", Store::No)?);
    match writer.add_document(doc) {
      Ok(_) => {},
      Err(error) if error.is_illegal_state_error() || error.is_io_error() => break,
      Err(error) => return Err(error),
    }
    if random.random_range(0..10) == 7 {
      // Flush new segment:
      match directory_reader::open_from_writer(&writer) {
        Ok(reader) => reader.close()?,
        Err(error) if error.is_illegal_state_error() || error.is_io_error() => break,
        Err(error) => return Err(error),
      }
    }
  }

  assert!(writer.get_tragic_exception().get().is_some());
  assert!(!writer.is_open());
  assert!(did_fail.load(Ordering::SeqCst));

  if let Some(scheduler) = concurrent_merge_scheduler {
    // Sneaky: CMS's merge thread will be concurrently rolling back IW due
    // to the tragedy, with this main thread, so we have to wait here
    // to ensure the rollback has finished, else MDW still sees open files:
    scheduler.sync()?;
  }
  dir.as_ref().close()?;
  Ok(())
}

#[test]
fn test_only_rollback_once_on_exception() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut config = new_index_writer_config(&mut random)?;
  config.set_info_stream(InfoStreamEnum::Custom(Box::new(RollbackOnceInfoStream {
    once: AtomicBool::new(false),
  })));
  let writer = IndexWriter::with_hooks(
    dir.clone(),
    config,
    Some(IndexWriterHooksEnum::custom(EnableTestPoints)),
  )?;
  let error = writer
    .rollback()
    .expect_err("the rollback test point must fail");
  assert_eq!("boom", error.to_string());
  assert!(error.get_suppressed()?.is_none());
  dir.as_ref().close()?;
  Ok(())
}

#[test]
fn test_exception_on_sync_metadata() -> Result<()> {
  let mut random = random();
  let dir = Arc::new(new_mock_directory(&mut random)?);
  let mut config = new_index_writer_config(&mut random)?;
  config.set_commit_on_close(false);
  let writer = IndexWriter::new(dir.clone(), config)?;
  writer.commit()?;
  let maybe_fail_delete = Arc::new(AtomicBool::new(false));
  dir.fail_on(Box::new(SyncMetadataFailure {
    random: Arc::new(Mutex::new(StdRng::seed_from_u64(random.random()))),
    maybe_fail_delete: maybe_fail_delete.clone(),
    do_fail: false,
  }));
  for i in 0..5 {
    let mut doc = Document::new();
    doc.add(StringField::from_string("id", i.to_string(), Store::No)?);
    doc.add(NumericDocValuesField::new("dv", i));
    doc.add(BinaryDocValuesField::new(
      "dv2",
      BytesRef::from_string(&i.to_string()),
    ));
    doc.add(SortedDocValuesField::new(
      "dv3",
      BytesRef::from_string(&i.to_string()),
    ));
    doc.add(SortedSetDocValuesField::new(
      "dv4",
      BytesRef::from_string(&i.to_string()),
    ));
    doc.add(SortedSetDocValuesField::new(
      "dv4",
      BytesRef::from_string(&(i - 1).to_string()),
    ));
    doc.add(SortedNumericDocValuesField::new("dv5", i));
    doc.add(SortedNumericDocValuesField::new("dv5", i - 1));
    doc.add(TextField::from_string(
      "text1",
      TestUtil::random_analysis_string(&mut random, 20, true),
      Store::No,
    )?);
    // ensure we store something
    doc.add(StoredField::from_string("stored1", "foo")?);
    doc.add(StoredField::from_string("stored1", "bar")?);
    // ensure we get some payloads
    doc.add(TextField::from_string(
      "text_payloads",
      TestUtil::random_analysis_string(&mut random, 6, true),
      Store::No,
    )?);
    // ensure we get some vectors
    let mut field_type = FieldType::from_ref(&*TEXT_NOT_STORED)?;
    field_type.set_store_term_vectors(true)?;
    doc.add(Field::new(
      "text_vectors",
      TestUtil::random_analysis_string(&mut random, 6, true),
      field_type,
    ));
    doc.add(IntPoint::new("point", vec![random.random()])?);
    doc.add(IntPoint::new(
      "point2d",
      vec![random.random(), random.random()],
    )?);
    writer.add_document(Document::new())?;
  }
  let error = writer.commit().expect_err("syncMetaData must fail");
  assert_eq!("boom", error.to_string());
  maybe_fail_delete.store(true, Ordering::SeqCst);
  if let Err(error) = writer.rollback() {
    assert_eq!("bang", error.to_string());
  }
  maybe_fail_delete.store(false, Ordering::SeqCst);
  assert!(INDEX_WRITER_ACCESS.is_closed(&writer));
  assert!(directory_reader::index_exists(dir.as_ref())?);
  let reader = directory_reader::open(dir.clone())?;
  reader.close()?;
  dir.as_ref().close()?;
  Ok(())
}

#[test]
fn test_exception_just_before_flush_with_point_values() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = Box::new(ExceptionJustBeforeFlushWithPointValuesAnalyzer::new(
    random.random(),
  )) as Box<dyn Analyzer>;
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  iwc.set_commit_on_close(false).set_max_buffered_docs(3);
  let merge_policy = iwc.get_merge_policy().as_ref().clone();
  iwc.set_merge_policy(SoftDeletesRetentionMergePolicy::new(
    "soft_delete",
    || Ok(MatchAllDocsQuery::new().into()),
    merge_policy,
  ));
  let writer = RandomIndexWriter::mock_index_writer(dir.clone(), iwc, &mut random)?;
  let mut new_doc = Document::new();
  let mut field_types = HashMap::new();
  new_doc.add(new_text_field(
    &mut random,
    "crash",
    "do it on token 4",
    Store::No,
    &mut field_types,
  )?);
  new_doc.add(IntPoint::new("int", [42])?);
  let error = writer
    .add_document(new_doc)
    .expect_err("the crashing token stream must fail");
  assert!(error.to_string().contains(CRASH_FAIL_MESSAGE));
  let reader = INDEX_WRITER_ACCESS.get_reader(&writer, false, false)?;
  let only_reader = get_only_leaf_reader(reader)?;
  // We mark the failed doc as deleted.
  assert_eq!(1, only_reader.num_deleted_docs()?);
  // There are no point values, rather than an empty set of values.
  assert!(only_reader.get_point_values("field")?.is_none());
  only_reader.close()?;
  writer.close()?;
  dir.close()
}

struct ExceptionJustBeforeFlushWithPointValuesAnalyzer {
  stored_value: AnalyzerStoredValue,
  seed: u64,
}

impl ExceptionJustBeforeFlushWithPointValuesAnalyzer {
  fn new(seed: u64) -> Self {
    Self {
      stored_value: AnalyzerStoredValue::per_field(),
      seed,
    }
  }
}

impl Analyzer for ExceptionJustBeforeFlushWithPointValuesAnalyzer {
  fn create_components(&self, field_name: &str) -> Result<TokenStreamComponents> {
    let mut tokenizer = MockTokenizer::with_default_max_token_length(
      random_from_seed(self.seed),
      WHITESPACE.clone(),
      false,
    );
    tokenizer.set_enable_checks(false); // disable workflow checking as we forcefully close() in exceptional cases.
    Ok(TokenStreamComponents::new(
      Box::new(CrashingFilter::new(field_name, tokenizer)) as Box<dyn TokenStream + Send + Sync>,
      None,
    ))
  }

  fn stored_value(&self) -> &AnalyzerStoredValue {
    &self.stored_value
  }
}

crate::impl_analyzer_close!(ExceptionJustBeforeFlushWithPointValuesAnalyzer);
