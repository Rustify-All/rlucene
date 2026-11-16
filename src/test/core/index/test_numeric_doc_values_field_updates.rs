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
use crate::core::document::binary_doc_values_field::BinaryDocValuesField;
use crate::core::document::document::Document;
use crate::core::document::field::Store;
use crate::core::document::long_point::LongPoint;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::sorted_doc_values_field::SortedDocValuesField;
use crate::core::document::sorted_set_doc_values_field::SortedSetDocValuesField;
use crate::core::document::string_field::StringField;
use crate::core::index::BytesRef;
use crate::core::index::BytesRefValue;
use crate::core::index::binary_doc_values::BinaryDocValues;
use crate::core::index::directory_reader;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::index_reader::{CacheHelper, IndexReader};
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::{DEFAULT_RAM_BUFFER_SIZE_MB, DISABLE_AUTO_FLUSH};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, at_least_usize, get_only_leaf_reader, is_night_mode, new_bytes_ref_from_string,
  new_directory_shared, new_index_writer_config, new_index_writer_config_with_analyzer,
  new_log_merge_policy, new_searcher_with_reader, random, random_from_seed,
};

use crate::core::index::merge_policy::MergePolicyEnum;
use crate::core::index::multi_bits::get_live_docs;
use crate::core::index::multi_doc_values::MultiDocValues;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::sorted_doc_values::SortedDocValues;
use crate::core::index::sorted_set_doc_values::SortedSetDocValues;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term::Term;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::index_searcher::{self, IndexSearcher};
use crate::core::search::sort::Sort;
use crate::core::search::sort_field::{SortField, SortFieldType};
use crate::core::search::term_query::TermQuery;
use crate::core::search::top_docs::TopDocsLike;
use crate::core::store::directory::Directory;
use crate::core::store::nrt_caching_directory::NRTCachingDirectory;
use crate::core::util::TryIntoInt;
use crate::core::util::bits::Bits;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind, Result};
use crate::core::util::io_utils::IOUtils;
use crate::test_framework::core::analysis::mock_analyzer;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::codecs::asserting_doc_values_format::AssertingDocValuesFormat;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::util::DefaultCRReader;
use crate::test_framework::core::util::test_util::TestUtil;
use rand::RngExt;
use rand::seq::IndexedRandom;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::thread;
use std::vec;

#[allow(dead_code)] // for quick search
struct TestNumericDocValuesUpdates;
fn doc(id: i32) -> Result<Document> {
  // make sure we don't set the doc's value to 0, to not confuse with a document that's missing values
  doc_with_val(id, (id + 1) as i64)
}

fn doc_with_val(id: i32, val: i64) -> Result<Document> {
  let mut doc = Document::new();
  doc.add(StringField::from_string(
    "id",
    format!("doc-{}", id),
    Store::No,
  )?);
  doc.add(NumericDocValuesField::new("val", val));
  Ok(doc)
}
#[test]
fn test_multiple_updates_same_doc() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config.set_max_buffered_docs(3); // small number of docs
  let writer = IndexWriter::new(dir.clone(), config)?;

  writer.update_document_with_term(
    Term::from_text("id", "doc-1"),
    doc_with_val(1, 1_000_000_000)?,
  )?;
  writer.update_numeric_doc_value(Term::from_text("id", "doc-1"), "val", 1_000_001_111)?;
  writer.update_document_with_term(
    Term::from_text("id", "doc-2"),
    doc_with_val(2, 2_000_000_000)?,
  )?;
  writer.update_document_with_term(
    Term::from_text("id", "doc-2"),
    doc_with_val(2, 2_222_222_222)?,
  )?;
  writer.update_numeric_doc_value(Term::from_text("id", "doc-1"), "val", 1_111_111_111)?;

  let reader = if random.random_bool(0.5) {
    writer.commit()?;
    directory_reader::open(dir.clone())?
  } else {
    directory_reader::open_from_writer(&writer)?
  };
  let reader = reader.get_context()?;
  let searcher = IndexSearcher::new(reader)?;

  let td = searcher.search_with_sort(
    TermQuery::new(Term::from_text("id", "doc-1")),
    1,
    Sort::with_fields(vec![SortField::new(Some("val"), SortFieldType::Long)?])?,
  )?;
  assert_eq!(td.score_docs().len(), 1, "doc-1 missing?");
  assert_eq!(
    *td.base.score_docs[0].fields()?[0].as_i64().unwrap(),
    1_111_111_111,
    "doc-1 value mismatch"
  );

  let td = searcher.search_with_sort(
    TermQuery::new(Term::from_text("id", "doc-2")),
    1,
    Sort::with_fields(vec![SortField::new(Some("val"), SortFieldType::Long)?])?,
  )?;
  assert_eq!(td.score_docs().len(), 1, "doc-2 missing?");
  assert_eq!(
    *td.base.score_docs[0].fields()?[0].as_i64().unwrap(),
    2_222_222_222,
    "doc-2 value mismatch"
  );

  writer.close()?;
  Ok(())
}
#[test]
fn test_biased_mix_of_random_updates() -> Result<()> {
  // 3 types of operations: add, updated, updateDV.
  // rather then randomizing equally, we'll pick (random) cutoffs so each test run is biased,
  // in terms of some ops happen more often then others
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), config)?;

  let add_cutoff = TestUtil::next_int(&mut random, 1, 98);
  let upd_cutoff = TestUtil::next_int(&mut random, add_cutoff + 1, 99);

  let num_operations = at_least_usize(&mut random, 1000);
  let mut expected: std::collections::HashMap<i32, i64> =
    std::collections::HashMap::with_capacity(num_operations / 3);

  // start with at least one doc before any chance of updates
  let num_seed_docs = at_least(&mut random, 1);
  for i in 0..num_seed_docs {
    let val = random.random();
    expected.insert(i, val);
    writer.add_document(doc_with_val(i, val)?)?;
  }

  for _ in 0..num_operations {
    let op = TestUtil::next_int(&mut random, 1, 100);
    let val = random.random();
    if op <= add_cutoff {
      let id = expected.len() as i32;
      expected.insert(id, val);
      writer.add_document(doc_with_val(id, val)?)?;
    } else {
      let id = TestUtil::next_int(&mut random, 0, expected.len() as i32 - 1);
      expected.insert(id, val);
      if op <= upd_cutoff {
        writer.update_document_with_term(
          Term::from_text("id", format!("doc-{id}")),
          doc_with_val(id, val)?,
        )?;
      } else {
        writer.update_numeric_doc_value(Term::from_text("id", format!("doc-{id}")), "val", val)?;
      }
    }
  }

  writer.commit()?;

  let reader = directory_reader::open(dir.clone())?;

  let searcher = index_searcher::from_reader(reader)?;

  for (id, expected_val) in expected {
    let td = searcher.search_with_sort(
      TermQuery::new(Term::from_text("id", format!("doc-{}", id))),
      1,
      Sort::with_fields(vec![SortField::new(Some("val"), SortFieldType::Long)?])?,
    )?;
    assert_eq!(
      td.total_hits().value,
      1,
      "{}",
      format_args!("{} missing?", id)
    );

    assert_eq!(
      *td.base.score_docs[0].fields()?[0].as_i64().unwrap(),
      expected_val,
      "{}",
      format_args!("{} value", id)
    );
  }

  Ok(())
}

#[test]
fn test_updates_are_flushed() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::with_automaton(&mut random, mock_analyzer::WHITESPACE.clone(), false);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config.set_ram_buffer_size_mb(0.00000001);
  let writer = IndexWriter::new(dir.clone(), config)?;

  writer.add_document(doc(0)?)?; // val=1
  writer.add_document(doc(1)?)?; // val=2
  writer.add_document(doc(3)?)?; // val=4
  writer.commit()?;

  assert_eq!(1, writer.get_flush_deletes_count());

  writer.update_numeric_doc_value(Term::from_text("id", "doc-0"), "val", 5)?;
  assert_eq!(2, writer.get_flush_deletes_count());

  writer.update_numeric_doc_value(Term::from_text("id", "doc-1"), "val", 6)?;
  assert_eq!(3, writer.get_flush_deletes_count());

  writer.update_numeric_doc_value(Term::from_text("id", "doc-2"), "val", 7)?;
  assert_eq!(4, writer.get_flush_deletes_count());

  writer.get_config()?.set_ram_buffer_size_mb(1000.0);
  writer.update_numeric_doc_value(Term::from_text("id", "doc-2"), "val", 7)?;
  assert_eq!(4, writer.get_flush_deletes_count());

  writer.close()?;
  Ok(())
}

#[test]
fn test_simple() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  // make sure random config doesn't flush on us
  config.set_max_buffered_docs(10);
  config.set_ram_buffer_size_mb(DISABLE_AUTO_FLUSH as f64);
  let writer = IndexWriter::new(dir.clone(), config)?;

  writer.add_document(doc(0)?)?; // val=1
  writer.add_document(doc(1)?)?; // val=2
  if random.random_bool(0.5) {
    // randomly commit before the update is sent
    writer.commit()?;
  }

  writer.update_numeric_doc_value(Term::from_text("id", "doc-0"), "val", 2)?;

  let reader = if random.random_bool(0.5) {
    writer.close()?;
    directory_reader::open(dir.clone())?
  } else {
    let r = directory_reader::open_from_writer(&writer)?;
    writer.close()?;
    r
  };

  let reader = reader.get_context()?;
  assert_eq!(reader.leaves()?.len(), 1);
  let r = reader.leaves()?;
  let r = r[0].reader();
  let mut ndv = r.get_numeric_doc_values("val")?.unwrap();
  assert_eq!(ndv.next_doc()?, 0);
  assert_eq!(ndv.long_value()?, 2);
  assert_eq!(ndv.next_doc()?, 1);
  assert_eq!(ndv.long_value()?, 2);

  Ok(())
}
#[test]
fn test_update_few_segments() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config.set_max_buffered_docs(2); // generate few segments
  config.set_merge_policy(NoMergePolicy::default());
  let writer = IndexWriter::new(dir.clone(), config)?;

  let num_docs = 10;
  let mut expected_values = vec![0i64; num_docs];
  for (i, expected) in expected_values.iter_mut().take(num_docs).enumerate() {
    writer.add_document(doc(i as i32)?)?;
    *expected = (i + 1) as i64;
  }

  writer.commit()?;

  // update few docs
  for (i, expected) in expected_values.iter_mut().take(num_docs).enumerate() {
    if random.random_range(0.0..1.0) < 0.4 {
      let value = ((i + 1) * 2) as i64;
      writer.update_numeric_doc_value(Term::from_text("id", format!("doc-{i}")), "val", value)?;
      *expected = value;
    }
  }

  let reader = if random.random_bool(0.5) {
    writer.close()?;
    directory_reader::open(dir.clone())?
  } else {
    let r = directory_reader::open_from_writer(&writer)?;
    writer.close()?;
    r
  };
  let reader = reader.get_context()?;

  for context in reader.leaves()?.iter() {
    let r = context.reader();
    let mut ndv = r.get_numeric_doc_values("val")?.unwrap();
    for i in 0..r.max_doc()? {
      let expected = expected_values[i.try_convert()? + context.doc_base];
      assert_eq!(i, ndv.next_doc()?);
      let actual = ndv.long_value()?;
      assert_eq!(expected, actual);
    }
  }

  Ok(())
}
#[test]
fn test_reopen() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let conf = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), conf)?;

  writer.add_document(doc(0)?)?;
  writer.add_document(doc(1)?)?;

  let is_nrt = random.random_bool(0.5);
  let reader1 = if is_nrt {
    directory_reader::open_from_writer(&writer)?
  } else {
    writer.commit()?;
    directory_reader::open(dir.clone())?
  };

  if cfg!(feature = "test_log_verbose") {
    println!("TEST: isNRT={is_nrt}");
  }

  writer.update_numeric_doc_value(Term::from_text("id", "doc-0"), "val", 10)?;

  if !is_nrt {
    writer.commit()?;
  }

  if cfg!(feature = "test_log_verbose") {
    println!("TEST: openIfChanged");
  }

  let reader2 = directory_reader::open_if_changed(&reader1)?.unwrap();
  assert_ne!(
    reader1.get_reader_cache_helper()?.unwrap().get_key(),
    reader2.get_reader_cache_helper()?.unwrap().get_key()
  );

  let v = reader1.get_context()?;
  let leaves1 = v.leaves()?;
  let mut dvs1 = leaves1[0].reader().get_numeric_doc_values("val")?.unwrap();
  assert_eq!(0, dvs1.next_doc()?);
  assert_eq!(1, dvs1.long_value()?);

  let v = reader2.get_context()?;
  let leaves2 = v.leaves()?;
  let mut dvs2 = leaves2[0].reader().get_numeric_doc_values("val")?.unwrap();
  assert_eq!(0, dvs2.next_doc()?);
  assert_eq!(10, dvs2.long_value()?);

  writer.close()?;
  Ok(())
}
#[test]
fn test_updates_and_deletes() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, a)?;
  conf.set_max_buffered_docs(10);
  conf.set_merge_policy(NoMergePolicy::default());
  let writer = IndexWriter::new(dir.clone(), conf)?;

  for i in 0..6 {
    writer.add_document(doc(i)?)?;
    if i % 2 == 1 {
      writer.commit()?;
    }
  }

  writer.delete_documents_with_terms(vec![
    Term::from_text("id", "doc-1"),
    Term::from_text("id", "doc-2"),
  ])?;

  writer.update_numeric_doc_value(Term::from_text("id", "doc-3"), "val", 17)?;
  writer.update_numeric_doc_value(Term::from_text("id", "doc-5"), "val", 17)?;

  let reader = if random.random_bool(0.5) {
    writer.close()?;
    directory_reader::open(dir.clone())?
  } else {
    let reader = directory_reader::open_from_writer(&writer)?;
    writer.close()?;
    reader
  };

  let live_docs = get_live_docs(&reader)?.unwrap();
  let expected_live_docs = [true, false, false, true, true, true];
  for (i, expected) in expected_live_docs.iter().enumerate() {
    assert_eq!(*expected, live_docs.get(i).expect(""));
  }

  let expected_values = [1i64, 2, 3, 17, 5, 17];
  let mut ndv = MultiDocValues::get_numeric_values(&reader, "val")?.unwrap();
  for (i, expected) in expected_values.iter().enumerate() {
    assert_eq!(i as i32, ndv.next_doc()?);
    assert_eq!(*expected, ndv.long_value()?);
  }

  reader.close()?;
  Ok(())
}
#[test]
fn test_updates_with_deletes() -> Result<()> {
  // update and delete different documents in the same commit session
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config.set_max_buffered_docs(10);
  config.set_merge_policy(NoMergePolicy::default());
  let writer = IndexWriter::new(dir.clone(), config)?;

  writer.add_document(doc(0)?)?;
  writer.add_document(doc(1)?)?;

  if random.random_bool(0.5) {
    writer.commit()?;
  }

  writer.delete_documents_with_terms(vec![Term::from_text("id", "doc-0")])?;
  writer.update_numeric_doc_value(Term::from_text("id", "doc-1"), "val", 17)?;

  let reader = if random.random_bool(0.5) {
    writer.close()?;
    directory_reader::open(dir.clone())?
  } else {
    let r = directory_reader::open_from_writer(&writer)?;
    writer.close()?;
    r
  };

  let reader = reader.get_context()?;
  let leaf = &reader.leaves()?[0];
  let r = leaf.reader();
  let live_docs = r.get_live_docs()?.unwrap();
  assert!(!live_docs.get(0)?);
  let mut ndv = r.get_numeric_doc_values("val")?.unwrap();
  assert_eq!(ndv.advance(1)?, 1);
  assert_eq!(ndv.long_value()?, 17);

  Ok(())
}

#[test]
fn test_multiple_doc_values_types() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config.set_max_buffered_docs(10); // prevent merges
  let writer = IndexWriter::new(dir.clone(), config)?;

  for i in 0..4 {
    let mut doc = Document::new();
    doc.add(StringField::from_string("dvUpdateKey", "dv", Store::No)?);
    doc.add(NumericDocValuesField::new("ndv", i as i64));
    doc.add(BinaryDocValuesField::new(
      "bdv",
      new_bytes_ref_from_string(&mut random, &i.to_string())?,
    ));
    doc.add(SortedDocValuesField::new(
      "sdv",
      new_bytes_ref_from_string(&mut random, &i.to_string())?,
    ));
    doc.add(SortedSetDocValuesField::new(
      "ssdv",
      new_bytes_ref_from_string(&mut random, &i.to_string())?,
    ));
    doc.add(SortedSetDocValuesField::new(
      "ssdv",
      new_bytes_ref_from_string(&mut random, &(i * 2).to_string())?,
    ));
    writer.add_document(doc)?;
  }
  writer.commit()?;

  // update all docs' ndv field
  writer.update_numeric_doc_value(Term::from_text("dvUpdateKey", "dv"), "ndv", 17)?;
  writer.close()?;

  let reader = directory_reader::open(dir.clone())?;
  let leaf = reader.get_context()?;
  let r = leaf.leaves()?;
  let r = r[0].reader();

  let mut ndv = r.get_numeric_doc_values("ndv")?.unwrap();
  let mut bdv = r.get_binary_doc_values("bdv")?.unwrap();
  let mut sdv = r.get_sorted_doc_values("sdv")?.unwrap();
  let mut ssdv = r.get_sorted_set_doc_values("ssdv")?.unwrap();

  for i in 0..r.max_doc()? {
    // numeric
    assert_eq!(i, ndv.next_doc()?);
    assert_eq!(17, ndv.long_value()?);

    // binary
    assert_eq!(i, bdv.next_doc()?);
    let term = bdv.binary_value()?.utf8_to_string()?.to_owned();
    assert_eq!(term, i.to_string());

    // sorted
    assert_eq!(i, sdv.next_doc()?);
    let ord_value = sdv.ord_value()?;
    let term = sdv.lookup_ord(ord_value)?.utf8_to_string()?.to_owned();
    assert_eq!(term, i.to_string());

    // sorted set
    assert_eq!(i, ssdv.next_doc()?);
    let ord = ssdv.next_ord()?;
    let term = ssdv.lookup_ord(ord)?.utf8_to_string()?.to_owned();
    assert_eq!(i, term.parse::<i32>()?);

    if i == 0 {
      assert_eq!(1, ssdv.doc_value_count()?);
    } else {
      assert_eq!(2, ssdv.doc_value_count()?);
      let ord = ssdv.next_ord()?;
      let term = ssdv.lookup_ord(ord)?.utf8_to_string()?.to_owned();
      assert_eq!(i * 2, term.parse::<i32>()?);
    }
  }
  Ok(())
}
#[test]
fn test_multiple_numeric_doc_values() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config.set_max_buffered_docs(10); // prevent merges
  let writer = IndexWriter::new(dir.clone(), config)?;

  for i in 0..2 {
    let mut doc = Document::new();
    doc.add(StringField::from_string("dvUpdateKey", "dv", Store::No)?);
    doc.add(NumericDocValuesField::new("ndv1", i as i64));
    doc.add(NumericDocValuesField::new("ndv2", i as i64));
    writer.add_document(doc)?;
  }
  writer.commit()?;

  // update all docs' ndv1 field
  writer.update_numeric_doc_value(Term::from_text("dvUpdateKey", "dv"), "ndv1", 17)?;
  writer.close()?;

  let reader = directory_reader::open(dir.clone())?;
  let reader = reader.get_context()?;
  let r = reader.leaves()?;
  let r = r[0].reader();

  let mut ndv1 = r.get_numeric_doc_values("ndv1")?.unwrap();
  let mut ndv2 = r.get_numeric_doc_values("ndv2")?.unwrap();

  for i in 0..r.max_doc()? {
    assert_eq!(i, ndv1.next_doc()?);
    assert_eq!(17, ndv1.long_value()?);

    assert_eq!(i, ndv2.next_doc()?);
    assert_eq!(i as i64, ndv2.long_value()?);
  }
  Ok(())
}
#[test]
fn test_document_with_no_value() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), config)?;

  for i in 0..2 {
    let mut doc = Document::new();
    doc.add(StringField::from_string("dvUpdateKey", "dv", Store::No)?);
    if i == 0 {
      // index only one document with value
      doc.add(NumericDocValuesField::new("ndv", 5));
    }
    writer.add_document(doc)?;
  }
  writer.commit()?;

  // update all docs' ndv field
  writer.update_numeric_doc_value(Term::from_text("dvUpdateKey", "dv"), "ndv", 17)?;
  writer.close()?;

  let reader = directory_reader::open(dir.clone())?;
  let reader = reader.get_context()?;
  let r = reader.leaves()?;
  let r = r[0].reader();

  let mut ndv = r.get_numeric_doc_values("ndv")?.unwrap();
  for i in 0..r.max_doc()? {
    assert_eq!(i, ndv.next_doc()?);
    assert_eq!(
      17,
      ndv.long_value()?,
      "doc={} has wrong numeric doc value",
      i
    );
  }

  Ok(())
}
#[test]
fn test_update_non_numeric_doc_values_field() -> Result<()> {
  // we don't support adding new fields or updating existing non-numeric-dv fields through numeric updates
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), config)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("key", "doc", Store::No)?);
  doc.add(StringField::from_string("foo", "bar", Store::No)?);
  writer.add_document(doc)?;
  writer.commit()?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("key", "doc", Store::No)?);
  doc.add(StringField::from_string("foo", "bar", Store::No)?);
  writer.add_document(doc)?;

  let res = writer.update_numeric_doc_value(Term::from_text("key", "doc"), "ndv", 17);
  assert!(matches!(
    res.as_ref().map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));

  // attempt to update a non-numeric field "foo"
  let res = writer.update_numeric_doc_value(Term::from_text("key", "doc"), "foo", 17);
  assert!(matches!(
    res.as_ref().map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));

  writer.close()?;
  Ok(())
}
#[test]
fn test_different_dv_format_per_field() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, a)?;
  conf.set_codec(TestUtil::always_doc_values_format(
    TestUtil::get_default_doc_values_format(),
  ));

  let writer = IndexWriter::new(dir.clone(), conf)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("key", "doc", Store::No)?);
  doc.add(NumericDocValuesField::new("ndv", 5));
  doc.add(SortedDocValuesField::new(
    "sorted",
    BytesRef::from_string("value"),
  ));

  writer.add_document(&mut doc)?;
  writer.commit()?;
  writer.add_document(doc)?;

  writer.update_numeric_doc_value(Term::from_text("key", "doc"), "ndv", 17)?;
  writer.close()?;

  let reader = directory_reader::open(dir.clone())?;

  let mut ndv = MultiDocValues::get_numeric_values(&reader, "ndv")?.unwrap();
  let mut sdv = MultiDocValues::get_sorted_values(&reader, "sorted")?.unwrap();
  for i in 0..reader.max_doc()? {
    assert_eq!(i, ndv.next_doc()?);
    assert_eq!(17, ndv.long_value()?);
    assert_eq!(i, sdv.next_doc()?);
    let ord = sdv.ord_value()?;
    let term = sdv.lookup_ord(ord)?;
    assert_eq!(b"value", term.as_bytes());
  }

  reader.close()?;
  Ok(())
}

#[test]
fn test_update_same_doc_multiple_times() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let conf = new_index_writer_config_with_analyzer(&mut random, a)?;
  let writer = IndexWriter::new(dir.clone(), conf)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("key", "doc", Store::No)?);
  doc.add(NumericDocValuesField::new("ndv", 5));
  writer.add_document(&mut doc)?;
  writer.commit()?;
  writer.add_document(doc)?;

  writer.update_numeric_doc_value(Term::from_text("key", "doc"), "ndv", 17)?;
  writer.update_numeric_doc_value(Term::from_text("key", "doc"), "ndv", 3)?;
  writer.close()?;

  let reader = directory_reader::open(dir.clone())?;
  let mut ndv = MultiDocValues::get_numeric_values(&reader, "ndv")?.unwrap();
  for i in 0..reader.max_doc()? {
    assert_eq!(i, ndv.next_doc()?);
    assert_eq!(3, ndv.long_value()?);
  }
  reader.close()?;
  Ok(())
}

#[test]
fn test_segment_merges() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, a)?;
  let mut writer = IndexWriter::new(dir.clone(), conf)?;

  let mut docid = 0;
  let num_rounds = at_least(&mut random, 10);
  if cfg!(feature = "test_log_verbose") {
    println!("TEST: {} rounds", num_rounds);
  }

  for rnd in 0..num_rounds {
    if cfg!(feature = "test_log_verbose") {
      println!("\nTEST: round={}", rnd);
    }

    let mut doc = Document::new();
    doc.add(StringField::from_string("key", "doc", Store::No)?);
    doc.add(NumericDocValuesField::new("ndv", -1));

    let num_docs = at_least(&mut random, 30);
    if cfg!(feature = "test_log_verbose") {
      println!("TEST: {} docs", num_docs);
    }

    for _ in 0..num_docs {
      doc.remove_field("id");
      doc.add(StringField::from_string(
        "id",
        docid.to_string(),
        Store::Yes,
      )?);
      if cfg!(feature = "test_log_verbose") {
        println!("TEST: add doc id={}", docid);
      }
      writer.add_document(&mut doc)?;
      docid += 1;
    }

    let value = (rnd + 1) as i64;
    if cfg!(feature = "test_log_verbose") {
      println!("TEST: update all ndv values to {}", value);
    }
    writer.update_numeric_doc_value(Term::from_text("key", "doc"), "ndv", value)?;

    if random.random::<f64>() < 0.2 {
      let del_id = random.random_range(0..docid);
      if cfg!(feature = "test_log_verbose") {
        println!("TEST: delete random doc id={}", del_id);
      }
      writer.delete_documents_with_terms(vec![Term::from_text("id", del_id.to_string())])?;
    }

    if random.random::<f64>() < 0.4 {
      if cfg!(feature = "test_log_verbose") {
        println!("\nTEST: commit writer");
      }
      writer.commit()?;
    } else if random.random::<f64>() < 0.1 {
      if cfg!(feature = "test_log_verbose") {
        println!("\nTEST: close writer");
      }
      writer.close()?;
      drop(writer);
      let a = MockAnalyzer::new(&mut random);
      conf = new_index_writer_config_with_analyzer(&mut random, a)?;
      writer = IndexWriter::new(dir.clone(), conf)?;
    }

    let mut doc = Document::new();
    doc.add(StringField::from_string(
      "id",
      docid.to_string(),
      Store::Yes,
    )?);
    doc.add(StringField::from_string("key", "doc", Store::No)?);
    doc.add(NumericDocValuesField::new("ndv", value));
    if cfg!(feature = "test_log_verbose") {
      println!("\nTEST: add one more doc id={}", docid);
    }
    writer.add_document(doc)?;
    docid += 1;

    if cfg!(feature = "test_log_verbose") {
      println!("\nTEST: force merge");
    }
    writer.force_merge_with_wait(1, true)?;

    let reader = if random.random_bool(0.5) {
      if cfg!(feature = "test_log_verbose") {
        println!("\nTEST: commit and open non-NRT reader");
      }
      writer.commit()?;
      directory_reader::open(dir.clone())?
    } else {
      if cfg!(feature = "test_log_verbose") {
        println!("\nTEST: open NRT reader");
      }
      directory_reader::open_from_writer(&writer)?
    };

    if cfg!(feature = "test_log_verbose") {
      println!("TEST: got reader={reader}");
    }

    let reader = reader.get_context()?;
    assert_eq!(1, reader.leaves()?.len());

    let leaves = reader.leaves()?;
    let r = leaves[0].reader();
    assert!(r.get_live_docs()?.is_none());

    let mut ndv = r.get_numeric_doc_values("ndv")?.unwrap();
    if cfg!(feature = "test_log_verbose") {
      println!("TEST: maxDoc={}", r.max_doc()?);
    }

    let mut stored_fields = r.stored_fields()?;
    for i in 0..r.max_doc()? {
      let rdoc = stored_fields.document(i)?;
      assert_eq!(i, ndv.next_doc()?);
      assert_eq!(
        value,
        ndv.long_value()?,
        "docid={} has wrong ndv value; doc={}",
        i,
        rdoc
      );
    }
  }

  writer.close()?;
  Ok(())
}
#[test]
fn test_update_document_by_multiple_terms() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let conf = new_index_writer_config_with_analyzer(&mut random, a)?;
  let writer = IndexWriter::new(dir.clone(), conf)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("k1", "v1", Store::No)?);
  doc.add(StringField::from_string("k2", "v2", Store::No)?);
  doc.add(NumericDocValuesField::new("ndv", 5));
  writer.add_document(&mut doc)?;
  writer.commit()?;
  writer.add_document(doc)?;

  writer.update_numeric_doc_value(Term::from_text("k1", "v1"), "ndv", 17)?;
  writer.update_numeric_doc_value(Term::from_text("k2", "v2"), "ndv", 3)?;
  writer.close()?;

  let reader = directory_reader::open(dir.clone())?;
  let mut ndv = MultiDocValues::get_numeric_values(&reader, "ndv")?.unwrap();
  for i in 0..reader.max_doc()? {
    assert_eq!(i, ndv.next_doc()?);
    assert_eq!(3, ndv.long_value()?);
  }
  reader.close()?;
  Ok(())
}
#[derive(Debug, Clone)]
struct OneSortDoc {
  pub value: i64,
  pub sort_value: i64,
  pub id: i32,
  pub deleted: bool,
}

impl OneSortDoc {
  pub fn new(id: i32, value: i64, sort_value: i64) -> Self {
    Self {
      value,
      sort_value,
      id,
      deleted: false,
    }
  }
}

impl PartialEq for OneSortDoc {
  fn eq(&self, other: &Self) -> bool {
    self.sort_value == other.sort_value && self.id == other.id
  }
}

impl Eq for OneSortDoc {}

impl PartialOrd for OneSortDoc {
  fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
    Some(self.cmp(other))
  }
}

impl Ord for OneSortDoc {
  fn cmp(&self, other: &Self) -> std::cmp::Ordering {
    let cmp = self.sort_value.cmp(&other.sort_value);
    if cmp == std::cmp::Ordering::Equal {
      let cmp = self.id.cmp(&other.id);
      assert_ne!(cmp, std::cmp::Ordering::Equal);
      cmp
    } else {
      cmp
    }
  }
}

#[test]
fn test_sorted_index() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mut iwc = new_index_writer_config(&mut random)?;
  iwc.set_index_sort(Sort::with_fields(vec![SortField::new(
    Some("sort"),
    SortFieldType::Long,
  )?])?)?;
  let w = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

  let value_range = TestUtil::next_int(&mut random, 1, 1000);
  let sort_value_range = TestUtil::next_int(&mut random, 1, 1000);

  let refresh_chance = TestUtil::next_int(&mut random, 5, 200);
  let delete_chance = TestUtil::next_int(&mut random, 2, 100);

  let mut deleted_count = 0;
  let mut docs = Vec::new();
  let mut r;

  let num_iters = at_least(&mut random, 1000);
  for iter in 0..num_iters {
    let value = random.random_range(0..value_range) as i64;
    if docs.is_empty() || random.random_range(0..3) == 1 {
      let id = docs.len() as i32;
      let mut doc = Document::new();
      doc.add(StringField::from_string("id", id.to_string(), Store::Yes)?);
      doc.add(NumericDocValuesField::new("number", value));
      let sort_value = random.random_range(0..sort_value_range) as i64;
      doc.add(NumericDocValuesField::new("sort", sort_value));
      if cfg!(feature = "test_log_verbose") {
        println!(
          "TEST: iter={} add doc id={} sortValue={} value={}",
          iter, id, sort_value, value
        );
      }
      w.add_document(&mut random, doc)?;
      docs.push(OneSortDoc::new(id, value, sort_value));
    } else {
      let id_to_update = random.random_range(0..docs.len());
      if cfg!(feature = "test_log_verbose") {
        println!(
          "TEST: iter={} update doc id={} new value={}",
          iter, id_to_update, value
        );
      }
      w.update_numeric_doc_value(
        &mut random,
        Term::from_text("id", id_to_update.to_string()),
        "number",
        value,
      )?;
      docs[id_to_update].value = value;
    }

    if random.random_range(0..delete_chance) == 0 {
      let id_to_delete = random.random_range(0..docs.len());
      if cfg!(feature = "test_log_verbose") {
        println!("TEST: delete doc id={}", id_to_delete);
      }
      w.delete_documents_with_terms(
        &mut random,
        vec![Term::from_text("id", id_to_delete.to_string())],
      )?;
      if !docs[id_to_delete].deleted {
        docs[id_to_delete].deleted = true;
        deleted_count += 1;
      }
    }

    if random.random_range(0..refresh_chance) == 0 {
      let r2 = w.get_reader(&mut random)?;
      r = r2;

      if cfg!(feature = "test_log_verbose") {
        println!("TEST: got reader={}", r);
      }

      let mut live_count = 0;
      let reader = r.get_context()?;
      for ctx in reader.leaves()? {
        let leaf_reader = ctx.reader();
        let mut values = leaf_reader.get_numeric_doc_values("number")?.unwrap();
        let mut sort_values = leaf_reader.get_numeric_doc_values("sort")?.unwrap();
        let live_docs = leaf_reader.get_live_docs()?;
        let mut stored_fields = leaf_reader.stored_fields()?;

        let mut last_sort_value = i64::MIN;
        for i in 0..leaf_reader.max_doc()? {
          let doc = stored_fields.document(i)?;
          let sort_doc = &docs[doc.get("id")?.unwrap().parse::<usize>().unwrap()];

          assert_eq!(i, values.next_doc()?);
          assert_eq!(i, sort_values.next_doc()?);

          if live_docs
            .as_ref()
            .is_some_and(|bits| !bits.get(i as usize).expect(""))
          {
            assert!(sort_doc.deleted);
            continue;
          }
          assert!(!sort_doc.deleted);

          assert_eq!(sort_doc.value, values.long_value()?);

          let sort_value = sort_values.long_value()?;
          assert_eq!(sort_doc.sort_value, sort_value);

          assert!(sort_value >= last_sort_value);
          last_sort_value = sort_value;
          live_count += 1;
        }
      }

      assert_eq!(docs.len() as i32 - deleted_count, live_count);
    }
  }
  w.close(&mut random)?;
  Ok(())
}
#[test]
fn test_many_reopens_and_fields() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let mut lmp = new_log_merge_policy(&mut random)?;
  match lmp {
    MergePolicyEnum::LogDoc(ref mut v) => {
      v.set_merge_factor(3)?;
    },
    MergePolicyEnum::LogBytesSize(ref mut v) => {
      v.set_merge_factor(3)?;
    },
    _ => unreachable!(""),
  }
  conf.set_merge_policy(lmp);
  let writer = IndexWriter::new(dir.clone(), conf)?;

  let is_nrt = random.random_bool(0.5);
  if cfg!(feature = "test_log_verbose") {
    println!("TEST: isNRT={is_nrt}");
  }

  let mut reader = if is_nrt {
    directory_reader::open_from_writer(&writer)?
  } else {
    writer.commit()?;
    directory_reader::open(dir.clone())?
  };

  let num_fields = random.random_range(3..7);
  let mut field_values = vec![1i64; num_fields];

  let num_rounds = at_least(&mut random, 15);
  let mut doc_id = 0;
  for i in 0..num_rounds {
    let num_docs = at_least(&mut random, 5);
    if cfg!(feature = "test_log_verbose") {
      println!("TEST: round={i}, numDocs={num_docs}");
    }
    for _ in 0..num_docs {
      let mut doc = Document::new();
      doc.add(StringField::from_string(
        "id",
        format!("doc-{doc_id}"),
        Store::Yes,
      )?);
      doc.add(StringField::from_string("key", "all", Store::No)?);
      for (f, value) in field_values.iter().enumerate() {
        doc.add(NumericDocValuesField::new(format!("f{f}"), *value));
      }
      writer.add_document(doc)?;
      if cfg!(feature = "test_log_verbose") {
        println!("TEST add doc id={doc_id}");
      }
      doc_id += 1;
    }

    let field_idx = random.random_range(0..field_values.len());
    let update_field = format!("f{field_idx}");
    field_values[field_idx] += 1;
    if cfg!(feature = "test_log_verbose") {
      println!(
        "TEST: update field={} for all docs to value={}",
        update_field, field_values[field_idx]
      );
    }
    writer.update_numeric_doc_value(
      Term::from_text("key", "all"),
      update_field,
      field_values[field_idx],
    )?;

    if random.random_bool(0.2) {
      let delete_doc = random.random_range(0..num_docs);
      if cfg!(feature = "test_log_verbose") {
        println!("TEST: delete doc id={delete_doc}");
      }
      writer
        .delete_documents_with_terms(vec![Term::from_text("id", format!("doc-{delete_doc}"))])?;
    }

    if !is_nrt {
      if cfg!(feature = "test_log_verbose") {
        println!("TEST: now commit");
      }
      writer.commit()?;
    }

    let new_reader = directory_reader::open_if_changed(&reader)?.unwrap();
    reader.close()?;
    reader = new_reader;
    if cfg!(feature = "test_log_verbose") {
      println!("TEST: got reader maxDoc={} {}", reader.max_doc()?, reader);
    }
    assert!(reader.num_docs()? > 0);

    let reader_ctx = (&reader).get_context()?;
    for context in reader_ctx.leaves()? {
      let r = context.reader();
      let live_docs = r.get_live_docs()?;
      let mut stored_fields = r.stored_fields()?;
      for (field, expected_value) in field_values.iter().enumerate() {
        let f = format!("f{field}");
        let mut ndv = r.get_numeric_doc_values(&f)?.unwrap();
        let max_doc = r.max_doc()?;
        for doc in 0..max_doc {
          if live_docs
            .as_ref()
            .is_none_or(|bits| bits.get(doc as usize).expect(""))
          {
            assert_eq!(doc, ndv.advance(doc)?);
            assert_eq!(
              *expected_value,
              ndv.long_value()?,
              "invalid value for docID={} id={} field={} reader={} doc={}",
              doc,
              stored_fields.document(doc)?.get("id")?.unwrap(),
              f,
              r,
              stored_fields.document(doc)?
            );
          }
        }
      }
    }
  }

  reader.close()?;
  writer.close()?;
  Ok(())
}
#[test]
fn test_update_segment_with_no_doc_values() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config.set_merge_policy(NoMergePolicy::default());
  let writer = IndexWriter::new(dir.clone(), config)?;

  // first segment with NDV
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "doc0", Store::No)?);
  doc.add(NumericDocValuesField::new("ndv", 3));
  writer.add_document(doc)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "doc4", Store::No)?); // document without 'ndv' field
  writer.add_document(doc)?;
  writer.commit()?;

  // second segment with no NDV
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "doc1", Store::No)?);
  writer.add_document(doc)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "doc2", Store::No)?); // document that isn't updated
  writer.add_document(doc)?;
  writer.commit()?;

  // update document in the first segment - should not affect docsWithField of
  // the document without NDV field
  writer.update_numeric_doc_value(Term::from_text("id", "doc0"), "ndv", 5)?;
  // update document in the second segment - field should be added and we should
  // be able to handle the other document correctly (e.g. no NPE)
  writer.update_numeric_doc_value(Term::from_text("id", "doc1"), "ndv", 5)?;
  writer.close()?;

  let reader = directory_reader::open(dir.clone())?;
  let reader = reader.get_context()?;
  for ctx in reader.leaves()? {
    let r = ctx.reader();
    let mut ndv = r.get_numeric_doc_values("ndv")?.unwrap();
    assert_eq!(ndv.next_doc()?, 0);
    assert_eq!(ndv.long_value()?, 5);
    // docID 1 has no ndv value
    assert!(ndv.next_doc()? > 1);
  }

  Ok(())
}
#[test]
fn test_update_segment_with_no_doc_values2() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, a)?;
  conf.set_merge_policy(NoMergePolicy::default());
  let writer = IndexWriter::new(dir.clone(), conf)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "doc0", Store::No)?);
  doc.add(NumericDocValuesField::new("ndv", 3));
  writer.add_document(doc)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "doc4", Store::No)?);
  writer.add_document(doc)?;
  writer.commit()?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "doc1", Store::No)?);
  doc.add(NumericDocValuesField::new("foo", 3));
  writer.add_document(doc)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "doc2", Store::No)?);
  writer.add_document(doc)?;
  writer.commit()?;

  writer.update_numeric_doc_value(Term::from_text("id", "doc0"), "ndv", 5)?;
  writer.update_numeric_doc_value(Term::from_text("id", "doc1"), "ndv", 5)?;
  writer.close()?;
  drop(writer);

  let reader = directory_reader::open(dir.clone())?;
  let reader = reader.get_context()?;
  for context in reader.leaves()? {
    let r = context.reader();
    let mut ndv = r.get_numeric_doc_values("ndv")?.unwrap();
    assert_eq!(0, ndv.next_doc()?);
    assert_eq!(5, ndv.long_value()?);
    assert!(ndv.next_doc()? > 1);
  }

  TestUtil::check_index(&mut random, dir.clone())?;

  let a = MockAnalyzer::new(&mut random);
  let conf = new_index_writer_config_with_analyzer(&mut random, a)?;
  let writer = IndexWriter::new(dir.clone(), conf)?;
  writer.force_merge(1)?;
  writer.close()?;

  let reader = directory_reader::open(dir.clone())?;
  let ar = get_only_leaf_reader(&reader)?;
  assert_eq!(
    DocValuesType::Numeric,
    *ar
      .get_field_infos()?
      .field_info_by_name("foo")?
      .unwrap()
      .get_doc_values_type()
  );

  let searcher = new_searcher_with_reader(reader)?;
  let td = searcher.search_with_sort(
    TermQuery::new(Term::from_text("id", "doc0")),
    1,
    Sort::with_fields(vec![SortField::new(Some("ndv"), SortFieldType::Long)?])?,
  )?;
  assert_eq!(5i64, *td.score_docs()[0].fields()?[0].as_i64().unwrap());

  let td = searcher.search_with_sort(
    TermQuery::new(Term::from_text("id", "doc1")),
    1,
    Sort::with_fields(vec![
      SortField::new(Some("ndv"), SortFieldType::Long)?,
      SortField::new(Some("foo"), SortFieldType::Long)?,
    ])?,
  )?;
  assert_eq!(5i64, *td.score_docs()[0].fields()?[0].as_i64().unwrap());
  assert_eq!(3i64, *td.score_docs()[0].fields()?[1].as_i64().unwrap());

  let td = searcher.search_with_sort(
    TermQuery::new(Term::from_text("id", "doc2")),
    1,
    Sort::with_fields(vec![SortField::new(Some("ndv"), SortFieldType::Long)?])?,
  )?;
  assert_eq!(0i64, *td.score_docs()[0].fields()?[0].as_i64().unwrap());

  let td = searcher.search_with_sort(
    TermQuery::new(Term::from_text("id", "doc4")),
    1,
    Sort::with_fields(vec![SortField::new(Some("ndv"), SortFieldType::Long)?])?,
  )?;
  assert_eq!(0i64, *td.score_docs()[0].fields()?[0].as_i64().unwrap());

  Ok(())
}
#[test]
fn test_update_segment_with_posting_but_no_doc_values() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config.set_merge_policy(NoMergePolicy::default());
  let writer = IndexWriter::new(dir.clone(), config)?;

  // first segment with ndv and ndv2 fields
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "doc0", Store::No)?);
  doc.add(NumericDocValuesField::new("ndv", 5));
  doc.add(StringField::from_string("ndv2", "10", Store::No)?);
  doc.add(NumericDocValuesField::new("ndv2", 10));
  writer.add_document(doc)?;
  writer.commit()?;

  // second segment with no ndv and ndv2 fields
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "doc1", Store::No)?);
  writer.add_document(doc)?;
  writer.commit()?;

  // update docValues of "ndv" field in the second segment (allowed)
  writer.update_numeric_doc_value(Term::from_text("id", "doc1"), "ndv", 5)?;

  // update docValues of "ndv2" field in the second segment (NOT allowed)
  let result = writer.update_numeric_doc_value(Term::from_text("id", "doc1"), "ndv2", 10);
  assert!(matches!(
    result.as_ref().map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  let actual_err_msg = "Can't update [Numeric] doc values; the field [ndv2] must be doc values only field, but is also indexed with postings.";
  assert_eq!(actual_err_msg, result.unwrap_err().to_string());

  writer.close()?;

  // Verify index content
  let reader = directory_reader::open(dir.clone())?;
  let reader = reader.get_context()?;
  for ctx in reader.leaves()? {
    let r = ctx.reader();
    let mut ndv = r.get_numeric_doc_values("ndv")?.unwrap();
    for i in 0..r.max_doc()? {
      assert_eq!(i, ndv.next_doc()?);
      assert_eq!(5, ndv.long_value()?);
    }
  }

  Ok(())
}
#[test]
fn test_update_numeric_dv_field_with_same_name_as_posting_field() -> Result<()> {
  // this used to fail because FieldInfos::Builder neglected to update globalFieldMaps.docValuesTypes map
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), config)?;

  // add document with both posting field and NDV field of the same name
  let mut doc = Document::new();
  doc.add(StringField::from_string("f", "mock-value", Store::No)?);
  doc.add(NumericDocValuesField::new("f", 5));
  writer.add_document(doc)?;
  writer.commit()?;

  let result = writer.update_numeric_doc_value(Term::from_text("f", "mock-value"), "f", 17);
  assert!(matches!(
    result.as_ref().map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  let actual_err_msg = "Can't update [Numeric] doc values; the field [f] must be doc values only field, but is also indexed with postings.";
  assert_eq!(actual_err_msg, result.unwrap_err().to_string());

  writer.close()?;

  // verify NDV content unchanged
  let reader = directory_reader::open(dir.clone())?;
  let reader = reader.get_context()?;
  let mut ndv = reader.leaves()?[0]
    .reader()
    .get_numeric_doc_values("f")?
    .unwrap();
  assert_eq!(ndv.next_doc()?, 0);
  assert_eq!(ndv.long_value()?, 5);

  Ok(())
}
#[test]
fn test_stress_multi_threading() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let conf = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), conf)?;

  let num_fields = TestUtil::next_int(&mut random, 1, 4);
  let num_docs = if is_night_mode() {
    at_least(&mut random, 2000)
  } else {
    at_least(&mut random, 200)
  };

  for i in 0..num_docs {
    let mut doc = Document::new();
    doc.add(StringField::from_string(
      "id",
      format!("doc{i}"),
      Store::No,
    )?);
    let group = random.random::<f64>();
    let g = if group < 0.1 {
      "g0"
    } else if group < 0.5 {
      "g1"
    } else if group < 0.8 {
      "g2"
    } else {
      "g3"
    };
    doc.add(StringField::from_string("updKey", g, Store::No)?);
    for j in 0..num_fields {
      let value = random.random::<i32>() as i64;
      doc.add(NumericDocValuesField::new(format!("f{j}"), value));
      doc.add(NumericDocValuesField::new(format!("cf{j}"), value * 2));
    }
    writer.add_document(doc)?;
  }

  let num_threads = if is_night_mode() {
    TestUtil::next_int(&mut random, 3, 6)
  } else {
    4
  };
  let num_updates = AtomicI32::new(at_least(&mut random, 100));

  thread::scope(|scope| -> Result<()> {
    let mut handles = Vec::new();
    for i in 0..num_threads {
      let writer = &writer;
      let num_updates = &num_updates;
      let seed = random.random();
      handles.push(
        thread::Builder::new()
          .name(format!("UpdateThread-{i}"))
          .spawn_scoped(scope, move || -> Result<()> {
            let mut reader: Option<DefaultCRReader> = None;
            let mut random = random_from_seed(seed);
            while num_updates.fetch_sub(1, Ordering::SeqCst) > 0 {
              let group = random.random::<f64>();
              let t = if group < 0.1 {
                Term::from_text("updKey", "g0")
              } else if group < 0.5 {
                Term::from_text("updKey", "g1")
              } else if group < 0.8 {
                Term::from_text("updKey", "g2")
              } else {
                Term::from_text("updKey", "g3")
              };

              let field = random.random_range(0..num_fields);
              let f = format!("f{field}");
              let cf = format!("cf{field}");
              let upd_value = random.random::<i32>() as i64;
              writer.update_doc_values(
                t,
                vec![
                  NumericDocValuesField::new(f, upd_value).into(),
                  NumericDocValuesField::new(cf, upd_value * 2).into(),
                ],
              )?;

              if random.random_bool(0.2) {
                let doc = random.random_range(0..num_docs);
                writer
                  .delete_documents_with_terms(vec![Term::from_text("id", format!("doc{doc}"))])?;
              }

              if random.random_bool(0.05) {
                writer.commit()?;
              }

              if random.random_bool(0.1) {
                if let Some(old_reader) = reader.take() {
                  if let Some(new_reader) = directory_reader::open_if_changed(&old_reader)? {
                    old_reader.close()?;
                    reader = Some(new_reader);
                  } else {
                    reader = Some(old_reader);
                  }
                } else {
                  reader = Some(directory_reader::open_from_writer(writer)?);
                }
              }
            }
            if let Some(reader) = reader {
              reader.close()?;
            }
            Ok(())
          })?,
      );
    }

    for handle in handles {
      handle
        .join()
        .map_err(|_| LuceneError::illegal_state("update thread panicked"))??;
    }
    Ok(())
  })?;

  writer.close()?;

  let reader = directory_reader::open(dir.clone())?;
  let reader = reader.get_context()?;
  for context in reader.leaves()? {
    let r = context.reader();
    for i in 0..num_fields {
      let mut ndv = r.get_numeric_doc_values(&format!("f{i}"))?.unwrap();
      let mut control = r.get_numeric_doc_values(&format!("cf{i}"))?.unwrap();
      let live_docs = r.get_live_docs()?;
      for j in 0..r.max_doc()? {
        if live_docs
          .as_ref()
          .is_none_or(|bits| bits.get(j as usize).expect(""))
        {
          assert_eq!(j, ndv.advance(j)?);
          assert_eq!(j, control.advance(j)?);
          assert_eq!(control.long_value()?, ndv.long_value()? * 2);
        }
      }
    }
  }

  Ok(())
}
#[test]
fn test_update_different_docs_in_different_gens() -> Result<()> {
  // update same document multiple times across generations
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config.set_max_buffered_docs(4);
  let writer = IndexWriter::new(dir.clone(), config)?;

  let num_docs = at_least(&mut random, 10);
  for i in 0..num_docs {
    let mut doc = Document::new();
    doc.add(StringField::from_string(
      "id",
      format!("doc{}", i),
      Store::No,
    )?);
    let value = random.random();
    doc.add(NumericDocValuesField::new("f", value));
    doc.add(NumericDocValuesField::new("cf", value.wrapping_mul(2)));
    writer.add_document(doc)?;
  }

  let num_gens = at_least(&mut random, 5);
  for _ in 0..num_gens {
    let doc_id = random.random_range(0..num_docs);
    let t = Term::from_text("id", format!("doc{}", doc_id));
    let value = random.random();
    writer.update_doc_values(
      t,
      vec![
        NumericDocValuesField::new("f", value).into(),
        NumericDocValuesField::new("cf", value.wrapping_mul(2)).into(),
      ],
    )?;

    let reader = directory_reader::open_from_writer(&writer)?;
    let reader = reader.get_context()?;
    for ctx in reader.leaves()? {
      let r = ctx.reader();
      let mut fndv = r.get_numeric_doc_values("f")?.unwrap();
      let mut cfndv = r.get_numeric_doc_values("cf")?.unwrap();

      for j in 0..r.max_doc()? {
        assert_eq!(j, fndv.next_doc()?);
        assert_eq!(j, cfndv.next_doc()?);
        assert_eq!(cfndv.long_value()?, fndv.long_value()?.wrapping_mul(2));
      }
    }
  }

  writer.close()?;
  Ok(())
}

#[test]
fn test_change_codec() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_merge_policy(NoMergePolicy::default()); // disable merges to simplify test assertions.
  conf.set_codec(TestUtil::always_doc_values_format(
    TestUtil::get_default_doc_values_format(),
  ));
  let writer = IndexWriter::new(dir.clone(), conf)?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "d0", Store::No)?);
  doc.add(NumericDocValuesField::new("f1", 5));
  doc.add(NumericDocValuesField::new("f2", 13));
  writer.add_document(doc)?;
  writer.close()?;

  // change format
  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_merge_policy(NoMergePolicy::default()); // disable merges to simplify test assertions.
  conf.set_codec(TestUtil::always_doc_values_format(
    AssertingDocValuesFormat::new(),
  ));
  let writer = IndexWriter::new(dir.clone(), conf)?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "d1", Store::No)?);
  doc.add(NumericDocValuesField::new("f1", 17));
  doc.add(NumericDocValuesField::new("f2", 2));
  writer.add_document(doc)?;
  writer.update_numeric_doc_value(Term::from_text("id", "d0"), "f1", 12)?;
  writer.close()?;

  let reader = directory_reader::open(dir.clone())?;
  let mut f1 = MultiDocValues::get_numeric_values(&reader, "f1")?.unwrap();
  let mut f2 = MultiDocValues::get_numeric_values(&reader, "f2")?.unwrap();
  assert_eq!(0, f1.next_doc()?);
  assert_eq!(12, f1.long_value()?);
  assert_eq!(0, f2.next_doc()?);
  assert_eq!(13, f2.long_value()?);
  assert_eq!(1, f1.next_doc()?);
  assert_eq!(17, f1.long_value()?);
  assert_eq!(1, f2.next_doc()?);
  assert_eq!(2, f2.long_value()?);
  reader.close()?;
  dir.close()
}
#[test]
fn test_add_indexes() -> Result<()> {
  let mut random = random();

  let dir1 = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let conf = new_index_writer_config_with_analyzer(&mut random, a)?;
  let writer = IndexWriter::new(dir1.clone(), conf)?;

  let num_docs = at_least_usize(&mut random, 50);
  let num_terms = TestUtil::next_usize(&mut random, 1, num_docs / 5);
  let mut random_terms = HashSet::new();
  while random_terms.len() < num_terms {
    random_terms.insert(TestUtil::random_simple_string(&mut random));
  }
  let random_terms: Vec<String> = random_terms.into_iter().collect();

  for _ in 0..num_docs {
    let mut doc = Document::new();
    let idx = random.random_range(0..random_terms.len());
    doc.add(StringField::from_string(
      "id",
      random_terms[idx].clone(),
      Store::No,
    )?);
    doc.add(NumericDocValuesField::new("ndv", 4));
    doc.add(NumericDocValuesField::new("control", 8));
    writer.add_document(doc)?;
  }

  if random.random_bool(0.5) {
    writer.commit()?;
  }

  let value = random.random::<i32>() as i64;
  let idx = random.random_range(0..random_terms.len());
  let term = Term::from_text("id", random_terms[idx].clone());
  writer.update_doc_values(
    term,
    vec![
      NumericDocValuesField::new("ndv", value).into(),
      NumericDocValuesField::new("control", value * 2).into(),
    ],
  )?;
  writer.close()?;
  drop(writer);

  let dir2 = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let conf = new_index_writer_config_with_analyzer(&mut random, a)?;
  let writer = IndexWriter::new(dir2.clone(), conf)?;
  if random.random_bool(0.5) {
    writer.add_indexes_from_directory(std::slice::from_ref(&dir1))?;
  } else {
    let reader = directory_reader::open(dir1.clone())?;
    TestUtil::add_indexes_slowly(&writer, &[&reader])?;
    reader.close()?;
  }
  writer.close()?;
  drop(writer);

  let reader = (directory_reader::open(dir2.clone())?).get_context()?;
  for context in reader.leaves()? {
    let r = context.reader();
    let mut ndv = r.get_numeric_doc_values("ndv")?.unwrap();
    let mut control = r.get_numeric_doc_values("control")?.unwrap();
    for i in 0..r.max_doc()? {
      assert_eq!(i, ndv.next_doc()?);
      assert_eq!(i, control.next_doc()?);
      assert_eq!(ndv.long_value()? * 2, control.long_value()?);
    }
  }
  Ok(())
}
#[test]
fn test_add_new_field_after_add_indexes() -> Result<()> {
  let mut random = random();

  let dir1 = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, a)?;
  conf.set_merge_policy(NoMergePolicy::default());
  let num_docs = at_least(&mut random, 50);
  {
    let writer = IndexWriter::new(dir1.clone(), conf)?;
    for i in 0..num_docs {
      let mut doc = Document::new();
      doc.add(StringField::from_string("id", i.to_string(), Store::No)?);
      doc.add(NumericDocValuesField::new("a1", 0));
      doc.add(NumericDocValuesField::new("a2", 1));
      writer.add_document(doc)?;
    }
    writer.close()?;
  }

  let dir2 = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, a)?;
  conf.set_merge_policy(NoMergePolicy::default());
  {
    let writer = IndexWriter::new(dir2.clone(), conf)?;
    for i in 0..num_docs {
      let mut doc = Document::new();
      doc.add(StringField::from_string("id", i.to_string(), Store::No)?);
      doc.add(NumericDocValuesField::new("i1", 0));
      doc.add(NumericDocValuesField::new("i2", 1));
      doc.add(NumericDocValuesField::new("i3", 2));
      writer.add_document(doc)?;
    }
    writer.close()?;
  }

  let main_dir = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, a)?;
  conf.set_merge_policy(NoMergePolicy::default());
  {
    let writer = IndexWriter::new(main_dir.clone(), conf)?;
    writer.add_indexes_from_directory(&[dir1.clone(), dir2.clone()])?;

    let mut original_field_infos = Vec::new();
    {
      let reader = (directory_reader::open_from_writer(&writer)?).get_context()?;
      for leaf in reader.leaves()? {
        original_field_infos.push(leaf.reader().get_field_infos()?);
      }
    }
    assert!(!original_field_infos.is_empty());

    let value = random.random::<i32>() as i64;
    for i in 0..num_docs {
      let term = Term::new(
        "id",
        new_bytes_ref_from_string(&mut random, &i.to_string())?,
      );
      writer.update_doc_values(term, vec![NumericDocValuesField::new("ndv", value).into()])?;
    }

    {
      let reader = (directory_reader::open_from_writer(&writer)?).get_context()?;
      let leaves = reader.leaves()?;
      for (i, leaf) in leaves.iter().enumerate() {
        let leaf_reader = leaf.reader();
        let orig_field_infos = &original_field_infos[i];
        let new_field_infos = leaf_reader.get_field_infos()?;
        ensure_consistent_field_infos(orig_field_infos, &new_field_infos)?;
        assert_eq!(
          DocValuesType::Numeric,
          *new_field_infos
            .field_info_by_name("ndv")?
            .unwrap()
            .get_doc_values_type()
        );
        let mut ndv = leaf_reader.get_numeric_doc_values("ndv")?.unwrap();
        for doc_id in 0..leaf_reader.max_doc()? {
          assert_eq!(doc_id, ndv.next_doc()?);
          assert_eq!(ndv.long_value()?, value);
        }
      }
    }

    writer.close()?;
  }

  Ok(())
}
#[test]
fn test_updates_after_add_indexes() -> Result<()> {
  let mut random = random();

  let dir1 = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, a)?;
  conf.set_merge_policy(NoMergePolicy::default());
  let num_docs = at_least(&mut random, 50);
  {
    let writer = IndexWriter::new(dir1.clone(), conf)?;
    for i in 0..num_docs {
      let mut doc = Document::new();
      doc.add(StringField::from_string("id", i.to_string(), Store::No)?);
      doc.add(NumericDocValuesField::new("ndv", 4));
      doc.add(NumericDocValuesField::new("control", 8));
      doc.add(LongPoint::new("i1", [4])?);
      writer.add_document(doc)?;
    }
    writer.close()?;
  }

  let dir2 = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, a)?;
  conf.set_merge_policy(NoMergePolicy::default());
  {
    let writer = IndexWriter::new(dir2.clone(), conf)?;
    for i in num_docs..num_docs * 2 {
      let mut doc = Document::new();
      doc.add(StringField::from_string("id", i.to_string(), Store::No)?);
      doc.add(NumericDocValuesField::new("ndv", 2));
      doc.add(NumericDocValuesField::new("control", 4));
      doc.add(LongPoint::new("i2", [16])?);
      doc.add(LongPoint::new("i2", [24])?);
      writer.add_document(doc)?;
    }
    writer.close()?;
  }

  let main_dir = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, a)?;
  conf.set_merge_policy(NoMergePolicy::default());
  {
    let writer = IndexWriter::new(main_dir.clone(), conf)?;
    writer.add_indexes_from_directory(&[dir1.clone(), dir2.clone()])?;

    let mut original_field_infos = Vec::new();
    {
      let reader = (directory_reader::open_from_writer(&writer)?).get_context()?;
      for leaf in reader.leaves()? {
        original_field_infos.push(leaf.reader().get_field_infos()?);
      }
    }
    assert!(!original_field_infos.is_empty());

    let value = random.random::<i32>() as i64;
    let id = random.random_range(0..num_docs) * 2;
    let term = Term::new(
      "id",
      new_bytes_ref_from_string(&mut random, &id.to_string())?,
    );
    writer.update_doc_values(
      term,
      vec![
        NumericDocValuesField::new("ndv", value).into(),
        NumericDocValuesField::new("control", value * 2).into(),
      ],
    )?;

    {
      let reader_cr = directory_reader::open_from_writer(&writer)?;
      let reader = (&reader_cr).get_context()?;
      let leaves = reader.leaves()?;
      for (i, leaf) in leaves.iter().enumerate() {
        let leaf_reader = leaf.reader();
        let orig_field_infos = &original_field_infos[i];
        let new_field_infos = leaf_reader.get_field_infos()?;
        ensure_consistent_field_infos(orig_field_infos, &new_field_infos)?;
        assert!(new_field_infos.field_info_by_name("ndv")?.is_some());
        assert_eq!(
          DocValuesType::Numeric,
          *new_field_infos
            .field_info_by_name("ndv")?
            .unwrap()
            .get_doc_values_type()
        );
        assert_eq!(
          DocValuesType::Numeric,
          *new_field_infos
            .field_info_by_name("control")?
            .unwrap()
            .get_doc_values_type()
        );

        let mut ndv = leaf_reader.get_numeric_doc_values("ndv")?.unwrap();
        let mut control = leaf_reader.get_numeric_doc_values("control")?.unwrap();
        for doc_id in 0..leaf_reader.max_doc()? {
          assert_eq!(doc_id, ndv.next_doc()?);
          assert_eq!(doc_id, control.next_doc()?);
          assert_eq!(ndv.long_value()? * 2, control.long_value()?);
        }
      }

      let searcher = new_searcher_with_reader(reader_cr)?;
      assert_eq!(
        num_docs,
        searcher.count(LongPoint::new_exact_query("i1", 4)?)?
      );
      assert_eq!(
        num_docs,
        searcher.count(LongPoint::new_exact_query("i2", 16)?)?
      );
      assert_eq!(
        num_docs,
        searcher.count(LongPoint::new_exact_query("i2", 24)?)?
      );
      searcher.reader_context.reader().close()?;
    }

    writer.close()?;
  }

  Ok(())
}
fn ensure_consistent_field_infos(old: &FieldInfos, after: &FieldInfos) -> Result<()> {
  for fi in old.iter() {
    let by_number = after.field_info_by_number(fi.number)?;
    assert!(by_number.is_some());

    let by_name = after.field_info_by_name(&fi.name)?;
    assert!(by_name.is_some());

    let after_fi = by_name.unwrap();
    assert_eq!(fi.number, after_fi.number,);
    assert!(fi.get_doc_values_gen() <= after_fi.get_doc_values_gen(),);
  }
  Ok(())
}
#[test]
fn test_delete_unused_updates_files() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), config)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "d0", Store::No)?);
  doc.add(NumericDocValuesField::new("f1", 1));
  doc.add(NumericDocValuesField::new("f2", 1));
  writer.add_document(doc)?;

  // update each field twice to make sure all unneeded files are deleted
  for f in ["f1", "f2"] {
    writer.update_numeric_doc_value(Term::from_text("id", "d0"), f, 2)?;
    writer.commit()?;
    let num_files = dir.list_all()?.len();

    // update again, number of files shouldn't change (old field's gen is
    // removed)
    writer.update_numeric_doc_value(Term::from_text("id", "d0"), f, 3)?;
    writer.commit()?;

    assert_eq!(num_files, dir.list_all()?.len(),);
  }

  writer.close()?;
  Ok(())
}
#[test]
fn test_tons_of_updates() -> Result<()> {
  // LUCENE-5248: make sure that when there are many updates, we don't use too much RAM
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mut config = new_index_writer_config(&mut random)?;
  config.set_ram_buffer_size_mb(DEFAULT_RAM_BUFFER_SIZE_MB);
  config.set_max_buffered_docs(DISABLE_AUTO_FLUSH);
  let writer = IndexWriter::new(dir.clone(), config)?;

  // test data: lots of documents (few hundred to few 10Ks) and lots of update terms
  let num_docs = if is_night_mode() {
    at_least(&mut random, 20_000)
  } else {
    at_least(&mut random, 200)
  };
  let num_numeric_fields = at_least(&mut random, 5);
  let num_terms = random.random_range(10..=100); // terms should affect many docs

  let mut update_terms = HashSet::new();
  while update_terms.len() < num_terms {
    update_terms.insert(TestUtil::random_simple_string(&mut random));
  }
  let update_terms: Vec<_> = update_terms.into_iter().collect();

  // build a large index with many NDV fields and update terms
  for _ in 0..num_docs {
    let mut doc = Document::new();
    let num_update_terms = random.random_range(1..=(num_terms / 10).max(1));
    for _ in 0..num_update_terms {
      let term_val = update_terms.choose(&mut random).unwrap();
      doc.add(StringField::from_string("upd", term_val, Store::No)?);
    }
    for j in 0..num_numeric_fields {
      let val = random.random::<i32>() as i64;
      doc.add(NumericDocValuesField::new(format!("f{}", j), val));
      doc.add(NumericDocValuesField::new(format!("cf{}", j), val * 2));
    }
    writer.add_document(doc)?;
  }

  writer.commit()?; // commit so there's something to apply to

  // set to flush every 2048 bytes (approximately every 12 updates), so we get
  // many flushes during numeric updates
  writer
    .get_config()?
    .set_ram_buffer_size_mb(2048.0 / 1024.0 / 1024.0);
  let num_updates = at_least(&mut random, 100);

  for _ in 0..num_updates {
    let field = random.random_range(0..num_numeric_fields);
    let term_val = update_terms.choose(&mut random).unwrap();
    let update_term = Term::from_text("upd", term_val);
    let value = random.random::<i32>() as i64;
    writer.update_doc_values(
      update_term,
      vec![
        NumericDocValuesField::new(format!("f{}", field), value).into(),
        NumericDocValuesField::new(format!("cf{}", field), value * 2).into(),
      ],
    )?;
  }

  writer.close()?;

  // validate
  let reader = directory_reader::open(dir.clone())?;
  let reader = reader.get_context()?;
  for ctx in reader.leaves()? {
    let r = ctx.reader();
    for i in 0..num_numeric_fields {
      let mut f = r.get_numeric_doc_values(&format!("f{}", i))?.unwrap();
      let mut cf = r.get_numeric_doc_values(&format!("cf{}", i))?.unwrap();
      for j in 0..r.max_doc()? {
        assert_eq!(j, f.next_doc()?);
        assert_eq!(j, cf.next_doc()?);
        assert_eq!(
          cf.long_value()?,
          f.long_value()? * 2,
          "reader={}, field=f{}, doc={}",
          r,
          i,
          j
        );
      }
    }
  }

  Ok(())
}
#[test]
fn test_updates_order() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), config)?;

  // add initial document
  let mut doc = Document::new();
  doc.add(StringField::from_string("upd", "t1", Store::No)?);
  doc.add(StringField::from_string("upd", "t2", Store::No)?);
  doc.add(NumericDocValuesField::new("f1", 1));
  doc.add(NumericDocValuesField::new("f2", 1));
  writer.add_document(doc)?;

  // apply updates in specific order
  writer.update_numeric_doc_value(Term::from_text("upd", "t1"), "f1", 2)?;
  writer.update_numeric_doc_value(Term::from_text("upd", "t1"), "f2", 2)?;
  writer.update_numeric_doc_value(Term::from_text("upd", "t2"), "f1", 3)?;
  writer.update_numeric_doc_value(Term::from_text("upd", "t2"), "f2", 3)?;
  writer.update_numeric_doc_value(Term::from_text("upd", "t1"), "f1", 4)?;
  writer.close()?;

  // verify the latest values
  let reader = directory_reader::open(dir.clone())?;
  let reader = reader.get_context()?;
  let r = reader.leaves()?;
  let r = r[0].reader();

  let mut f1 = r.get_numeric_doc_values("f1")?.unwrap();
  assert_eq!(f1.next_doc()?, 0);
  assert_eq!(f1.long_value()?, 4);

  let mut f2 = r.get_numeric_doc_values("f2")?.unwrap();
  assert_eq!(f2.next_doc()?, 0);
  assert_eq!(f2.long_value()?, 3);

  Ok(())
}
#[test]
fn test_update_all_deleted_segment() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), config)?;

  // add and commit documents
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "doc", Store::No)?);
  doc.add(NumericDocValuesField::new("f1", 1));
  writer.add_document(doc)?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "doc", Store::No)?);
  doc.add(NumericDocValuesField::new("f1", 1));
  writer.add_document(doc)?;
  writer.commit()?;

  writer.delete_documents_with_terms(vec![Term::from_text("id", "doc")])?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "doc", Store::No)?);
  doc.add(NumericDocValuesField::new("f1", 1));
  writer.add_document(doc)?;
  writer.update_numeric_doc_value(Term::from_text("id", "doc"), "f1", 2)?;
  writer.close()?;

  // verify only one segment remains and update was applied
  let reader = directory_reader::open(dir.clone())?;
  let reader = reader.get_context()?;
  assert_eq!(reader.leaves()?.len(), 1);

  let r = reader.leaves()?;
  let r = r[0].reader();
  let mut dvs = r.get_numeric_doc_values("f1")?.unwrap();
  assert_eq!(dvs.next_doc()?, 0);
  assert_eq!(dvs.long_value()?, 2);

  Ok(())
}
#[test]
fn test_update_two_nonexisting_terms() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), config)?;

  // add a document
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "doc", Store::No)?);
  doc.add(NumericDocValuesField::new("f1", 1));
  writer.add_document(doc)?;

  // update with multiple nonexisting terms in the same field
  writer.update_numeric_doc_value(Term::from_text("c", "foo"), "f1", 2)?;
  writer.update_numeric_doc_value(Term::from_text("c", "bar"), "f1", 2)?;
  writer.close()?;

  // verify the value remains unchanged
  let reader = directory_reader::open(dir.clone())?;
  let reader = reader.get_context()?;
  assert_eq!(reader.leaves()?.len(), 1);

  let r = reader.leaves()?;
  let r = r[0].reader();
  let mut dvs = r.get_numeric_doc_values("f1")?.unwrap();
  assert_eq!(dvs.next_doc()?, 0);
  assert_eq!(dvs.long_value()?, 1);

  Ok(())
}
#[test]
fn test_io_context() -> Result<()> {
  // LUCENE-5591: make sure we pass an IOContext with an approximate
  // segmentSize in FlushInfo
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  // we want a single large enough segment so that a doc-values update writes a large file
  config.set_merge_policy(NoMergePolicy::default());
  config.set_max_buffered_docs(i32::MAX); // manually flush
  config.set_ram_buffer_size_mb(DISABLE_AUTO_FLUSH as f64);
  let writer = IndexWriter::new(dir.clone(), config)?;
  for i in 0..100 {
    writer.add_document(doc(i)?)?;
  }
  writer.commit()?;
  writer.close()?;

  let caching_dir = Arc::new(NRTCachingDirectory::new(
    dir,
    100.0,
    1.0 / (1024.0 * 1024.0),
  ));
  let analyzer = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  // we want a single large enough segment so that a doc-values update writes a large file
  config.set_merge_policy(NoMergePolicy::default());
  config.set_max_buffered_docs(i32::MAX); // manually flush
  config.set_ram_buffer_size_mb(DISABLE_AUTO_FLUSH as f64);
  let writer = IndexWriter::new(caching_dir.clone(), config)?;
  writer.update_numeric_doc_value(Term::from_text("id", "doc-0"), "val", 100)?;
  let reader = directory_reader::open_from_writer(&writer)?; // flush
  assert!(caching_dir.list_cached_files()?.is_empty());

  let mut close_error = None;
  for close_result in [reader.close(), writer.close(), caching_dir.close()] {
    if let Err(error) = close_result {
      close_error = Some(IOUtils::use_or_suppress(close_error, error));
    }
  }
  match close_error {
    Some(error) => Err(error),
    None => Ok(()),
  }
}
