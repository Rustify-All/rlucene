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
use crate::core::analysis::token_attributes::packed_token_and_binary::BinaryTokenStreamAttributeImpl;
use crate::core::analysis::token_stream::TokenStream;
use crate::core::document::document::Document;
use crate::core::document::field::Field;
use crate::core::document::field_type::FieldType;
use crate::core::document::fields::FieldTokenStreamEnum;
use crate::core::document::text_field::TYPE_NOT_STORED;
use crate::core::index::BytesRef;
use crate::core::index::concurrent_merge_scheduler::ConcurrentMergeScheduler;
use crate::core::index::directory_reader;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_reader::{IndexReader, IndexReaderContextType};
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::index_writer_config::OpenMode;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::merge_policy::MergePolicyEnum;
use crate::core::index::multi_terms;
use crate::core::index::term::Term;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::search::term_query::TermQuery;
use crate::core::util::attribute_source::{AttributeSource, Attributes};
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::util::lucene_test_case::{
  create_temp_dir_with_prefix, new_fs_directory, new_index_writer_config_with_analyzer,
  new_log_merge_policy_with_merge_factor, new_searcher, random,
};
use crate::test_framework::core::util::test_util::TestUtil;
use parking_lot::Mutex;
use rand::RngExt;
use rand::prelude::{SliceRandom, StdRng};
use std::sync::Arc;
use std::time::Instant;

// NOTE: SimpleText codec will consume very large amounts of
// disk (but, should run successfully). Best to run with
// --features monster, the current codec, and plenty of RAM, for example:
//
//   cargo test --features monster test_2b_terms::test_2b_terms -- --ignored
//
#[allow(dead_code)] // for quick search
struct Test2BTerms;

const TOKEN_LEN: usize = 5;

struct MyTokenStreamState {
  saved_terms: Vec<BytesRef<Vec<u8>>>,
  next_save: i32,
  term_counter: i64,
}

struct MyTokenStream {
  attrs: Attributes,
  tokens_per_doc: i32,
  token_count: i32,
  state: Arc<Mutex<MyTokenStreamState>>,
  random: Arc<Mutex<StdRng>>,
}

impl MyTokenStream {
  fn new(
    tokens_per_doc: i32,
    state: Arc<Mutex<MyTokenStreamState>>,
    random: Arc<Mutex<StdRng>>,
  ) -> Result<Self> {
    Ok(Self {
      attrs: BinaryTokenStreamAttributeImpl::new()?.into(),
      tokens_per_doc,
      token_count: 0,
      state,
      random,
    })
  }
}

impl Closeable for MyTokenStream {}

impl TokenStream for MyTokenStream {
  fn increment_token(&mut self) -> Result<bool> {
    self.attrs.clear_attributes()?;
    if self.token_count >= self.tokens_per_doc {
      return Ok(false);
    }

    let mut state = self.state.lock();
    let mut term_bytes = vec![0u8; TOKEN_LEN];
    let mut shift = 32;
    for byte in &mut term_bytes {
      *byte = ((state.term_counter >> shift) & 0xff) as u8;
      shift -= 8;
    }
    let bytes = BytesRef::from_bytes(term_bytes);
    state.term_counter += 1;
    self.token_count += 1;
    state.next_save -= 1;
    if state.next_save == 0 {
      state.saved_terms.push(BytesRef::deep_copy_of(&bytes));
      println!("TEST: save term={bytes}");
      state.next_save = TestUtil::next_int(&mut *self.random.lock(), 500_000, 1_000_000);
    }
    drop(state);

    self.attrs.set_bytes_ref(Some(bytes))?;
    Ok(true)
  }

  fn reset(&mut self) -> Result<()> {
    self.token_count = 0;
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

#[cfg(feature = "monster")]
#[test]
#[ignore = "monster"]
fn test_2b_terms() -> Result<()> {
  let mut random = random();
  println!("Starting Test2B");
  let term_count = i64::from(i32::MAX) + 100_000_000;

  let terms_per_doc = TestUtil::next_int(&mut random, 100_000, 1_000_000);

  let dir = new_fs_directory(&mut random, create_temp_dir_with_prefix("2BTerms")?)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let mut merge_policy = new_log_merge_policy_with_merge_factor(&mut random, 10)?;
  if let MergePolicyEnum::LogBytesSize(policy) = &mut merge_policy {
    // 1 petabyte:
    policy.set_max_merge_mb(1024.0 * 1024.0 * 1024.0);
  }
  iwc
    .set_max_buffered_docs(-1)
    .set_ram_buffer_size_mb(256.0)
    .set_merge_scheduler(ConcurrentMergeScheduler::new())
    .set_merge_policy(merge_policy)
    .set_open_mode(OpenMode::Create);
  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let random = Arc::new(Mutex::new(random));
  let state = Arc::new(Mutex::new(MyTokenStreamState {
    saved_terms: Vec::new(),
    next_save: TestUtil::next_int(&mut *random.lock(), 500_000, 1_000_000),
    term_counter: 0,
  }));

  let mut custom_type = FieldType::from_ref(&*TYPE_NOT_STORED)?;
  custom_type.set_index_options(IndexOptions::Docs)?;
  custom_type.set_omit_norms(true)?;
  // `Document` is consumed by Rust's `IndexWriter::add_document`, so each document gets a
  // token-stream wrapper over the same Java-equivalent stream state.
  let num_docs = (term_count / i64::from(terms_per_doc)) as i32;

  println!("TERMS_PER_DOC={terms_per_doc}");
  println!("numDocs={num_docs}");

  for i in 0..num_docs {
    let mut doc = Document::new();
    doc.add(Field::from_token_stream(
      "field",
      FieldTokenStreamEnum::custom(MyTokenStream::new(
        terms_per_doc,
        Arc::clone(&state),
        Arc::clone(&random),
      )?),
      custom_type.clone(),
    )?);
    let t0 = Instant::now();
    writer.add_document(doc)?;
    println!("{i} of {num_docs} {} ms", t0.elapsed().as_millis());
  }
  let mut saved_terms = Some(state.lock().saved_terms.clone());

  println!("TEST: full merge");
  writer.force_merge(1)?;
  println!("TEST: close writer");
  writer.close()?;

  println!("TEST: open reader");
  let reader = Arc::new(directory_reader::open(dir.clone())?);
  let mut random = random.lock();
  if saved_terms.is_none() {
    saved_terms = Some(find_terms(&mut random, Arc::clone(&reader))?);
  }
  let mut saved_terms = saved_terms.expect("saved terms must exist");
  let num_saved_terms = saved_terms.len();
  let mut big_ord_terms = saved_terms[num_saved_terms - 10..].to_vec();
  println!("TEST: test big ord terms...");
  test_saved_terms(&mut random, Arc::clone(&reader), &mut big_ord_terms)?;
  println!("TEST: test all saved terms...");
  test_saved_terms(&mut random, Arc::clone(&reader), &mut saved_terms)?;
  reader.close()?;

  println!("TEST: now CheckIndex...");
  let status = TestUtil::check_index(&mut *random, Arc::clone(&dir))?;
  let tc = status.segment_infos[0]
    .term_index_status
    .as_ref()
    .expect("term index status must exist")
    .term_count;
  assert!(tc > i64::from(i32::MAX), "count {tc} is not > {}", i32::MAX);

  dir.as_ref().close()?;
  println!("TEST: done!");
  Ok(())
}

fn find_terms<IR>(random: &mut StdRng, reader: IR) -> Result<Vec<BytesRef<Vec<u8>>>>
where
  IR: IndexReader,
{
  println!("TEST: findTerms");
  let terms = multi_terms::get_terms(reader, "field")?.expect("field terms must exist");
  let mut terms_enum = terms.iterator()?;
  let mut saved_terms = Vec::new();
  let mut next_save = TestUtil::next_int(random, 500_000, 1_000_000);
  while let Some(term) = terms_enum.next()?.map(|term| term.into_owned()) {
    next_save -= 1;
    if next_save == 0 {
      saved_terms.push(BytesRef::deep_copy_of(&term));
      println!("TEST: add {term}");
      next_save = TestUtil::next_int(random, 500_000, 1_000_000);
    }
  }
  Ok(saved_terms)
}

fn test_saved_terms<IR>(
  random: &mut StdRng,
  reader: IR,
  terms: &mut [BytesRef<Vec<u8>>],
) -> Result<()>
where
  IR: IndexReader + Clone,
  IndexReaderContextType<IR>: Sync + 'static,
{
  println!("TEST: run {} terms on reader", terms.len());
  let searcher = new_searcher(reader.clone(), false, false)?;
  terms.shuffle(random);
  let terms_data = multi_terms::get_terms(reader, "field")?.expect("field terms must exist");
  let mut terms_enum = terms_data.iterator()?;
  let mut failed = false;
  for _ in 0..10 * terms.len() {
    let term = &terms[random.random_range(0..terms.len())];
    println!("TEST: search {term}");
    let t0 = Instant::now();
    let count = searcher.count(TermQuery::new(Term::new("field", term.clone())))?;
    if count <= 0 {
      println!("  FAILED: count={count}");
      failed = true;
    }
    println!("  took {} ms", t0.elapsed().as_millis());

    let result = terms_enum.seek_ceil(term)?;
    if result != SeekStatus::Found {
      if result == SeekStatus::End {
        println!("  FAILED: got END");
      } else {
        println!("  FAILED: wrong term: got {}", terms_enum.term()?);
      }
      failed = true;
    }
  }
  assert!(!failed);
  Ok(())
}
