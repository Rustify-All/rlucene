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
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::sorted_doc_values_field::SortedDocValuesField;
use crate::core::document::sorted_set_doc_values_field::SortedSetDocValuesField;
use crate::core::document::string_field::StringField;
use crate::core::index::BytesRef;
use crate::core::index::BytesRefValue;
use crate::core::index::binary_doc_values::BinaryDocValues;
use crate::core::index::directory_reader;
use crate::core::index::index_reader::{CacheHelper, IndexReader};
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::IndexWriter;
#[cfg(feature = "nightly")]
use crate::core::index::index_writer_config::DEFAULT_RAM_BUFFER_SIZE_MB;
use crate::core::index::index_writer_config::DISABLE_AUTO_FLUSH;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::merge_policy::MergePolicyEnum;
use crate::core::index::multi_bits::get_live_docs;
use crate::core::index::multi_doc_values::MultiDocValues;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::sorted_doc_values::SortedDocValues;
use crate::core::index::sorted_set_doc_values::SortedSetDocValues;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term::Term;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::sort::Sort;
use crate::core::search::sort_field::{SortField, SortFieldType};
use crate::core::store::directory::Directory;
use crate::core::store::nrt_caching_directory::NRTCachingDirectory;
use crate::core::util::TryIntoInt;
use crate::core::util::bits::Bits;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind, Result};
use crate::core::util::io_utils::IOUtils;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::analysis::mock_tokenizer;
use crate::test_framework::core::codecs::asserting_doc_values_format::AssertingDocValuesFormat;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::index::test_binary_doc_values_updates::{get_value, to_bytes};
use crate::test_framework::core::util::DefaultCRReader;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, at_least_usize, is_night_mode, new_bytes_ref_from_string, new_directory_shared,
  new_index_writer_config, new_index_writer_config_with_analyzer, new_log_merge_policy, random,
  random_from_seed,
};
use crate::test_framework::core::util::test_util::TestUtil;
#[cfg(feature = "nightly")]
use rand::prelude::IndexedRandom;
use rand::{Rng, RngExt};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::thread;

#[allow(dead_code)] // for quick search
struct TestBinaryDocValuesUpdates;

fn doc<R>(random: &mut R, id: i32) -> Result<Document>
where
  R: Rng + ?Sized,
{
  let mut doc = Document::new();

  let id_field = StringField::from_string("id", format!("doc-{}", id), Store::No)?;
  doc.add(id_field);

  let val_bytes = to_bytes(random, (id + 1) as i64)?;
  let val_field = BinaryDocValuesField::new("val", val_bytes);
  doc.add(val_field);
  Ok(doc)
}
#[test]
fn test_updates_are_flushed() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::with_automaton(&mut random, mock_tokenizer::WHITESPACE.clone(), false);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config.set_ram_buffer_size_mb(0.00000001);
  let writer = IndexWriter::new(dir.clone(), config)?;
  writer.add_document(doc(&mut random, 0)?)?; // val=1
  writer.add_document(doc(&mut random, 1)?)?; // val=2
  writer.add_document(doc(&mut random, 3)?)?; // val=4
  writer.commit()?;

  assert_eq!(1, writer.get_flush_deletes_count());

  writer.update_binary_doc_value(
    Term::from_text("id", "doc-0"),
    "val",
    to_bytes(&mut random, 5)?,
  )?;
  assert_eq!(2, writer.get_flush_deletes_count());

  writer.update_binary_doc_value(
    Term::from_text("id", "doc-1"),
    "val",
    to_bytes(&mut random, 6)?,
  )?;
  assert_eq!(3, writer.get_flush_deletes_count());

  writer.update_binary_doc_value(
    Term::from_text("id", "doc-2"),
    "val",
    to_bytes(&mut random, 7)?,
  )?;
  assert_eq!(4, writer.get_flush_deletes_count());

  writer.get_config()?.set_ram_buffer_size_mb(1000.0);
  writer.update_binary_doc_value(
    Term::from_text("id", "doc-2"),
    "val",
    to_bytes(&mut random, 7)?,
  )?;
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

  writer.add_document(doc(&mut random, 0)?)?; // val=1
  writer.add_document(doc(&mut random, 1)?)?; // val=2

  if random.random_bool(0.5) {
    // randomly commit before the update is sent
    writer.commit()?;
  }

  writer.update_binary_doc_value(
    Term::from_text("id", "doc-0"),
    "val",
    to_bytes(&mut random, 2)?,
  )?; // doc=0, exp=2

  // Open reader: either NRT or non-NRT
  let reader = if random.random_bool(0.5) {
    writer.close()?;
    directory_reader::open(dir.clone())?
  } else {
    let r = directory_reader::open_from_writer(&writer)?;
    writer.close()?;
    r
  };

  let reader = reader.get_context()?;
  assert_eq!(1, reader.leaves()?.len());
  let leaf = &reader.leaves()?[0];
  let r = leaf.reader();

  let mut bdv = r.get_binary_doc_values("val")?.unwrap();
  assert_eq!(0, bdv.next_doc()?);
  assert_eq!(2, get_value(&bdv)?);

  assert_eq!(1, bdv.next_doc()?);
  assert_eq!(2, get_value(&bdv)?);

  Ok(())
}
#[test]
fn test_update_few_segments() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mut config = new_index_writer_config(&mut random)?;
  config.set_max_buffered_docs(2); // generate few segments
  config.set_merge_policy(NoMergePolicy::default());
  let writer = IndexWriter::new(dir.clone(), config)?;

  let num_docs = 10;
  let mut expected_values = vec![0i64; num_docs];
  for (i, expected) in expected_values.iter_mut().take(num_docs).enumerate() {
    writer.add_document(doc(&mut random, i as i32)?)?;
    *expected = (i + 1) as i64;
  }
  writer.commit()?;

  // update few docs
  for (i, expected) in expected_values.iter_mut().take(num_docs).enumerate() {
    if random.random_range(0.0..1.0) < 0.4 {
      let value = ((i + 1) * 2) as i64;
      writer.update_binary_doc_value(
        Term::from_text("id", format!("doc-{i}")),
        "val",
        to_bytes(&mut random, value)?,
      )?;
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
    let bdv = r.get_binary_doc_values("val")?;
    assert!(bdv.is_some(), "BinaryDocValues should not be None");
    let mut bdv = bdv.unwrap();

    let max_doc = r.max_doc()?;
    for i in 0..max_doc {
      assert_eq!(i, bdv.next_doc()?);
      let expected = expected_values[i.try_convert()? + context.doc_base];
      let actual = get_value(&bdv)?;
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

  writer.add_document(doc(&mut random, 0)?)?;
  writer.add_document(doc(&mut random, 1)?)?;

  let is_nrt = random.random_bool(0.5);
  let reader1 = if is_nrt {
    directory_reader::open_from_writer(&writer)?
  } else {
    writer.commit()?;
    directory_reader::open(dir.clone())?
  };

  writer.update_binary_doc_value(
    Term::from_text("id", "doc-0"),
    "val",
    to_bytes(&mut random, 10)?,
  )?;

  if !is_nrt {
    writer.commit()?;
  }

  println!("TEST: now reopen");

  let reader2 = directory_reader::open_if_changed(&reader1)?.unwrap();
  assert_ne!(
    reader1.get_reader_cache_helper()?.unwrap().get_key(),
    reader2.get_reader_cache_helper()?.unwrap().get_key()
  );

  let v = reader1.get_context()?;
  let leaves1 = v.leaves()?;
  let mut bdv1 = leaves1[0].reader().get_binary_doc_values("val")?.unwrap();

  let v = reader2.get_context()?;
  let leaves2 = v.leaves()?;
  let mut bdv2 = leaves2[0].reader().get_binary_doc_values("val")?.unwrap();

  assert_eq!(0, bdv1.next_doc()?);
  assert_eq!(1, get_value(&bdv1)?);

  assert_eq!(0, bdv2.next_doc()?);
  assert_eq!(10, get_value(&bdv2)?);

  writer.close()?;
  Ok(())
}
#[test]
fn test_updates_and_deletes() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_max_buffered_docs(10);
  conf.set_merge_policy(NoMergePolicy::default());

  let writer = IndexWriter::new(dir.clone(), conf)?;

  for i in 0..6 {
    writer.add_document(doc(&mut random, i)?)?;
    if i % 2 == 1 {
      writer.commit()?; // create 2-docs segments
    }
  }

  // delete doc-1 and doc-2
  writer.delete_documents_with_terms(vec![
    Term::from_text("id", "doc-1"),
    Term::from_text("id", "doc-2"),
  ])?;

  // update docs 3 and 5
  writer.update_binary_doc_value(
    Term::from_text("id", "doc-3"),
    "val",
    to_bytes(&mut random, 17)?,
  )?;
  writer.update_binary_doc_value(
    Term::from_text("id", "doc-5"),
    "val",
    to_bytes(&mut random, 17)?,
  )?;

  let reader = if random.random_bool(0.5) {
    writer.close()?;
    directory_reader::open(dir.clone())?
  } else {
    let r = directory_reader::open_from_writer(&writer)?;
    writer.close()?;
    r
  };

  let live_docs = get_live_docs(&reader)?.unwrap();
  let expected_live_docs = [true, false, false, true, true, true];

  for (i, expected) in expected_live_docs.iter().enumerate() {
    assert_eq!(*expected, live_docs.get(i)?);
  }

  let expected_values = [1, 2, 3, 17, 5, 17];
  let mut bdv = MultiDocValues::get_binary_values(&reader, "val")?.unwrap();

  for (i, expected) in expected_values.iter().enumerate() {
    assert_eq!(i as i32, bdv.next_doc()?);
    assert_eq!(*expected, get_value(&bdv)?);
  }

  reader.close()?;

  Ok(())
}
#[test]
fn test_updates_with_deletes() -> Result<()> {
  let mut random = random();
  // update and delete different documents in the same commit session
  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config.set_max_buffered_docs(10); // control segment flushing
  config.set_merge_policy(NoMergePolicy::default());
  let writer = IndexWriter::new(dir.clone(), config)?;

  writer.add_document(doc(&mut random, 0)?)?;
  writer.add_document(doc(&mut random, 1)?)?;

  if random.random_bool(0.5) {
    writer.commit()?;
  }

  // update and delete different documents in the same commit session
  writer.delete_documents_with_terms(vec![Term::from_text("id", "doc-0")])?;
  writer.update_binary_doc_value(
    Term::from_text("id", "doc-1"),
    "val",
    to_bytes(&mut random, 17_i64)?,
  )?;

  // open reader
  let reader = if random.random_bool(0.5) {
    writer.close()?;
    directory_reader::open(dir.clone())?
  } else {
    let r = directory_reader::open_from_writer(&writer)?;
    writer.close()?;
    r
  };
  let reader = reader.get_context()?;
  let leaves = reader.leaves()?;
  let r = leaves[0].reader();
  let live_docs = r.get_live_docs()?.unwrap();
  assert!(!live_docs.get(0)?);

  let mut bdv = r.get_binary_doc_values("val")?.unwrap();
  assert_eq!(1, bdv.advance(1)?);
  assert_eq!(17_i64, get_value(&bdv)?);

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

  // update all docs' bdv field
  writer.update_binary_doc_value(
    Term::from_text("dvUpdateKey", "dv"),
    "bdv",
    to_bytes(&mut random, 17_i64)?,
  )?;
  writer.close()?;

  let reader = directory_reader::open(dir.clone())?;
  let reader = reader.get_context()?;
  let leaves = reader.leaves()?;
  let r = leaves[0].reader();

  let mut ndv = r.get_numeric_doc_values("ndv")?.unwrap();
  let mut bdv = r.get_binary_doc_values("bdv")?.unwrap();
  let mut sdv = r.get_sorted_doc_values("sdv")?.unwrap();
  let mut ssdv = r.get_sorted_set_doc_values("ssdv")?.unwrap();

  let max_doc = r.max_doc()?;
  for i in 0..max_doc {
    // NumericDocValues
    assert_eq!(i, ndv.next_doc()?);
    assert_eq!(i as i64, ndv.long_value()?);

    // BinaryDocValues
    assert_eq!(i, bdv.next_doc()?);
    assert_eq!(17_i64, get_value(&bdv)?);

    // SortedDocValues
    assert_eq!(i, sdv.next_doc()?);
    let v = sdv.ord_value()?;
    let term = sdv.lookup_ord(v)?;
    let v: BytesRef<Vec<u8>> = new_bytes_ref_from_string(&mut random, &i.to_string())?;
    assert_eq!(&v, term);

    // SortedSetDocValues
    assert_eq!(i, ssdv.next_doc()?);
    let ord = ssdv.next_ord()?;
    let term = ssdv.lookup_ord(ord)?;
    let parsed = term.utf8_to_string()?.parse::<i32>()?;
    assert_eq!(i, parsed);
    // For the i=0 case, we added the same value twice, which was dedup'd by IndexWriter so it has
    // only one value:
    if i == 0 {
      assert_eq!(1, ssdv.doc_value_count()?);
    } else {
      assert_eq!(2, ssdv.doc_value_count()?);
      let ord = ssdv.next_ord()?;
      let term = ssdv.lookup_ord(ord)?;
      let parsed = term.utf8_to_string()?.parse::<i32>()?;
      assert_eq!(i * 2, parsed);
    }
  }

  Ok(())
}
#[test]
fn test_multiple_binary_doc_values() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config.set_max_buffered_docs(10); // prevent merges
  let writer = IndexWriter::new(dir.clone(), config)?;

  for i in 0..2 {
    let mut doc = Document::new();
    doc.add(StringField::from_string("dvUpdateKey", "dv", Store::No)?);
    doc.add(BinaryDocValuesField::new(
      "bdv1",
      to_bytes(&mut random, i as i64)?,
    ));
    doc.add(BinaryDocValuesField::new(
      "bdv2",
      to_bytes(&mut random, i as i64)?,
    ));
    writer.add_document(doc)?;
  }
  writer.commit()?;

  // update all docs' bdv1 field
  writer.update_binary_doc_value(
    Term::from_text("dvUpdateKey", "dv"),
    "bdv1",
    to_bytes(&mut random, 17_i64)?,
  )?;
  writer.close()?;

  // open reader
  let reader = directory_reader::open(dir.clone())?;
  let reader = reader.get_context()?;
  let leaves = reader.leaves()?;
  let r = leaves[0].reader();

  let mut bdv1 = r.get_binary_doc_values("bdv1")?.unwrap();
  let mut bdv2 = r.get_binary_doc_values("bdv2")?.unwrap();

  let max_doc = r.max_doc()?;
  for i in 0..max_doc {
    assert_eq!(i, bdv1.next_doc()?);
    assert_eq!(17_i64, get_value(&bdv1)?);

    assert_eq!(i, bdv2.next_doc()?);
    assert_eq!(i as i64, get_value(&bdv2)?);
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

  // add 2 docs, only first one has BinaryDocValues
  for i in 0..2 {
    let mut doc = Document::new();
    doc.add(StringField::from_string("dvUpdateKey", "dv", Store::No)?);
    if i == 0 {
      // index only one document with value
      doc.add(BinaryDocValuesField::new(
        "bdv",
        to_bytes(&mut random, 5_i64)?,
      ));
    }
    writer.add_document(doc)?;
  }
  writer.commit()?;

  // update all docs' bdv field
  writer.update_binary_doc_value(
    Term::from_text("dvUpdateKey", "dv"),
    "bdv",
    to_bytes(&mut random, 17_i64)?,
  )?;
  writer.close()?;

  // open reader
  let reader = directory_reader::open(dir.clone())?;
  let reader = reader.get_context()?;
  let leaves = reader.leaves()?;
  assert_eq!(1, leaves.len());
  let r = leaves[0].reader();

  let mut bdv = r.get_binary_doc_values("bdv")?.unwrap();
  let max_doc = r.max_doc()?;
  for i in 0..max_doc {
    assert_eq!(i, bdv.next_doc()?);
    assert_eq!(17_i64, get_value(&bdv)?);
  }

  Ok(())
}
#[test]
fn test_update_non_binary_doc_values_field() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), config)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("key", "doc", Store::No)?);
  doc.add(StringField::from_string("foo", "bar", Store::No)?);
  writer.add_document(doc)?; // flushed document
  writer.commit()?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("key", "doc", Store::No)?);
  doc.add(StringField::from_string("foo", "bar", Store::No)?);
  writer.add_document(doc)?; // in-memory document

  let result = writer.update_binary_doc_value(
    Term::from_text("key", "doc"),
    "bdv",
    to_bytes(&mut random, 17_i64)?,
  );
  assert!(matches!(
    result.as_ref().map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));

  let result = writer.update_binary_doc_value(
    Term::from_text("key", "doc"),
    "foo",
    to_bytes(&mut random, 17_i64)?,
  );
  assert!(matches!(
    result.as_ref().map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));

  writer.close()?;
  Ok(())
}
#[test]
fn test_different_dv_format_per_field() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_codec(TestUtil::always_doc_values_format(
    TestUtil::get_default_doc_values_format(),
  ));

  let writer = IndexWriter::new(dir.clone(), conf)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("key", "doc", Store::No)?);
  doc.add(BinaryDocValuesField::new("bdv", to_bytes(&mut random, 5)?));
  doc.add(SortedDocValuesField::new(
    "sorted",
    BytesRef::from_string("value"),
  ));

  writer.add_document(&mut doc)?; // flushed document
  writer.commit()?;
  writer.add_document(doc)?; // in-memory document

  writer.update_binary_doc_value(
    Term::from_text("key", "doc"),
    "bdv",
    to_bytes(&mut random, 17)?,
  )?;

  writer.close()?;

  let reader = directory_reader::open(dir.clone())?;

  let mut bdv = MultiDocValues::get_binary_values(&reader, "bdv")?.unwrap();
  let mut sdv = MultiDocValues::get_sorted_values(&reader, "sorted")?.unwrap();

  for i in 0..reader.max_doc()? {
    assert_eq!(i, bdv.next_doc()?);
    assert_eq!(17, get_value(&bdv)?);

    assert_eq!(i, sdv.next_doc()?);
    let ord_value = sdv.ord_value()?;
    let term = sdv.lookup_ord(ord_value)?;
    assert_eq!(b"value", term.as_bytes());
  }

  reader.close()?;

  Ok(())
}
#[test]
fn test_update_same_doc_multiple_times() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;

  let writer = IndexWriter::new(dir.clone(), conf)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("key", "doc", Store::No)?);
  doc.add(BinaryDocValuesField::new("bdv", to_bytes(&mut random, 5)?));

  writer.add_document(&mut doc)?; // flushed document
  writer.commit()?;
  writer.add_document(doc)?; // in-memory document

  writer.update_binary_doc_value(
    Term::from_text("key", "doc"),
    "bdv",
    to_bytes(&mut random, 17)?,
  )?; // update existing field

  writer.update_binary_doc_value(
    Term::from_text("key", "doc"),
    "bdv",
    to_bytes(&mut random, 3)?,
  )?; // update existing field 2nd time in this commit

  writer.close()?;

  let reader = directory_reader::open(dir.clone())?;

  let mut bdv = MultiDocValues::get_binary_values(&reader, "bdv")?.unwrap();

  for i in 0..reader.max_doc()? {
    assert_eq!(i, bdv.next_doc()?);
    assert_eq!(3, get_value(&bdv)?);
  }

  reader.close()?;

  Ok(())
}
#[test]
fn test_segment_merges() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let mut writer = IndexWriter::new(dir.clone(), conf)?;

  let mut docid = 0;
  let num_rounds = at_least(&mut random, 10);

  for rnd in 0..num_rounds {
    let mut doc = Document::new();
    doc.add(StringField::from_string("key", "doc", Store::No)?);
    doc.add(BinaryDocValuesField::new("bdv", to_bytes(&mut random, -1)?));

    let num_docs = at_least(&mut random, 30);
    for _ in 0..num_docs {
      doc.remove_field("id");
      doc.add(StringField::from_string(
        "id",
        docid.to_string(),
        Store::No,
      )?);
      writer.add_document(&mut doc)?;
      docid += 1;
    }

    let value = rnd as i64 + 1;

    writer.update_binary_doc_value(
      Term::from_text("key", "doc"),
      "bdv",
      to_bytes(&mut random, value)?,
    )?;

    if random.random::<f64>() < 0.2 {
      writer.delete_documents_with_terms(vec![Term::from_text(
        "id",
        random.random_range(0..docid).to_string(),
      )])?;
    }

    if random.random::<f64>() < 0.4 {
      writer.commit()?;
    } else if random.random::<f64>() < 0.1 {
      writer.close()?;
      let analyzer = MockAnalyzer::new(&mut random);
      conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
      drop(writer);
      writer = IndexWriter::new(dir.clone(), conf)?;
    }

    let mut doc = Document::new();
    doc.add(StringField::from_string(
      "id",
      docid.to_string(),
      Store::No,
    )?);
    doc.add(StringField::from_string("key", "doc", Store::No)?);
    doc.add(BinaryDocValuesField::new(
      "bdv",
      to_bytes(&mut random, value)?,
    ));
    writer.add_document(doc)?;
    docid += 1;

    writer.force_merge_with_wait(1, true)?;

    let reader = if random.random_bool(0.5) {
      writer.commit()?;
      directory_reader::open(dir.clone())?
    } else {
      directory_reader::open_from_writer(&writer)?
    };
    let reader = reader.get_context()?;
    assert_eq!(1, reader.leaves()?.len());

    let leaf = &reader.leaves()?[0];
    let r = leaf.reader();

    assert!(
      r.get_live_docs()?.is_none(),
      "index should have no deletes after forceMerge"
    );

    let mut bdv = r.get_binary_doc_values("bdv")?.unwrap();

    for i in 0..r.max_doc()? {
      assert_eq!(i, bdv.next_doc()?);
      assert_eq!(value, get_value(&bdv)?);
    }
  }
  writer.close()?;

  Ok(())
}
#[test]
fn test_update_document_by_multiple_terms() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let writer = IndexWriter::new(dir.clone(), conf)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("k1", "v1", Store::No)?);
  doc.add(StringField::from_string("k2", "v2", Store::No)?);
  doc.add(BinaryDocValuesField::new("bdv", to_bytes(&mut random, 5)?));

  writer.add_document(&mut doc)?; // flushed document
  writer.commit()?;
  writer.add_document(doc)?; // in-memory document

  writer.update_binary_doc_value(
    Term::from_text("k1", "v1"),
    "bdv",
    to_bytes(&mut random, 17)?,
  )?;

  writer.update_binary_doc_value(
    Term::from_text("k2", "v2"),
    "bdv",
    to_bytes(&mut random, 3)?,
  )?;

  writer.close()?;

  let reader = directory_reader::open(dir.clone())?;

  let mut bdv = MultiDocValues::get_binary_values(&reader, "bdv")?.unwrap();

  for i in 0..reader.max_doc()? {
    assert_eq!(i, bdv.next_doc()?);
    assert_eq!(3, get_value(&bdv)?);
  }

  reader.close()?;

  Ok(())
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

  let value_range = TestUtil::next_usize(&mut random, 1, 1000);
  let sort_value_range = TestUtil::next_usize(&mut random, 1, 1000);

  let refresh_chance = TestUtil::next_usize(&mut random, 5, 200);
  let delete_chance = TestUtil::next_usize(&mut random, 2, 100);

  let mut deleted_count = 0i32;

  let mut docs = Vec::new();
  let mut r;

  let num_iters = if is_night_mode() {
    at_least(&mut random, 1000)
  } else {
    at_least(&mut random, 100)
  };

  for _ in 0..num_iters {
    let value = random.random_range(0..value_range) as i64;
    let value = to_bytes(&mut random, value)?;

    if docs.is_empty() || random.random_range(0..3) == 1 {
      let id = docs.len() as i32;

      let mut doc = Document::new();
      doc.add(StringField::from_string("id", id.to_string(), Store::Yes)?);
      doc.add(BinaryDocValuesField::new("number", value.clone()));
      let sort_value = random.random_range(0..sort_value_range) as i64;
      doc.add(NumericDocValuesField::new("sort", sort_value));

      w.add_document(&mut random, doc)?;
      docs.push(OneSortDoc::new(id, value, sort_value));
    } else {
      let id_to_update = random.random_range(0..docs.len());

      w.update_binary_doc_value(
        &mut random,
        Term::from_text("id", id_to_update.to_string()),
        "number",
        value.clone(),
      )?;

      docs[id_to_update].value = value;
    }

    if random.random_range(0..delete_chance) == 0 {
      let id_to_delete = random.random_range(0..docs.len());

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

      let mut live_count = 0i32;
      let reader = r.get_context()?;
      for ctx in reader.leaves()? {
        let leaf_reader = ctx.reader();
        let mut values = leaf_reader.get_binary_doc_values("number")?.unwrap();
        let mut sort_values = leaf_reader.get_numeric_doc_values("sort")?.unwrap();
        let live_docs = leaf_reader.get_live_docs()?;
        let mut stored_fields = leaf_reader.stored_fields()?;

        let mut last_sort_value = i64::MIN;

        for i in 0..leaf_reader.max_doc()? {
          let doc = stored_fields.document(i)?;
          let id_str = doc.get("id")?.unwrap();
          let sort_doc = &docs[id_str.parse::<usize>()?];

          assert_eq!(i, values.next_doc()?);
          assert_eq!(i, sort_values.next_doc()?);

          if let Some(live_docs) = live_docs.as_ref()
            && !live_docs.get(i as usize)?
          {
            assert!(sort_doc.deleted);
            continue;
          }

          assert!(!sort_doc.deleted);
          assert_eq!(&sort_doc.value, &values.binary_value()?.into_owned());

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

      doc_id += 1;
    }

    let field_idx = random.random_range(0..field_values.len());
    let update_field = format!("f{field_idx}");

    if cfg!(feature = "test_log_verbose") {
      println!(
        "TEST: update field={} for all docs to value={}",
        update_field,
        field_values[field_idx] + 1
      );
    }

    field_values[field_idx] += 1;
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
            assert_eq!(doc, ndv.advance(doc)?, "advanced to wrong doc in seg={r}");
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

  writer.close()?;
  reader.close()?;
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

  // First segment with BDV
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "doc0", Store::No)?);
  doc.add(BinaryDocValuesField::new(
    "bdv",
    to_bytes(&mut random, 3i64)?,
  ));
  writer.add_document(doc)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "doc4", Store::No)?);
  writer.add_document(doc)?;
  writer.commit()?;

  // Second segment with no BDV
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "doc1", Store::No)?);
  writer.add_document(doc)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "doc2", Store::No)?);
  writer.add_document(doc)?;
  writer.commit()?;

  // update document in the first segment - should not affect docsWithField of
  // the document without BDV field
  writer.update_binary_doc_value(
    Term::from_text("id", "doc0"),
    "bdv",
    to_bytes(&mut random, 5i64)?,
  )?;
  // update document in the second segment - field should be added and we should
  // be able to handle the other document correctly (e.g. no NPE)
  writer.update_binary_doc_value(
    Term::from_text("id", "doc1"),
    "bdv",
    to_bytes(&mut random, 5i64)?,
  )?;
  writer.close()?;

  // Validation phase
  let reader = directory_reader::open(dir)?;
  let reader = reader.get_context()?;
  for ctx in reader.leaves()? {
    let r = ctx.reader();
    let mut bdv = r.get_binary_doc_values("bdv")?.unwrap();
    assert_eq!(bdv.next_doc()?, 0);
    assert_eq!(get_value(&bdv)?, 5);
    assert_eq!(bdv.next_doc()?, NO_MORE_DOCS);
  }

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

  // First segment with BDV
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "doc0", Store::No)?);
  doc.add(StringField::from_string("bdv", "mock-value", Store::No)?);
  doc.add(BinaryDocValuesField::new(
    "bdv",
    to_bytes(&mut random, 5i64)?,
  ));
  writer.add_document(doc)?;
  writer.commit()?;

  // Second segment with no BDV
  let mut doc2 = Document::new();
  doc2.add(StringField::from_string("id", "doc1", Store::No)?);
  doc2.add(StringField::from_string("bdv", "mock-value", Store::No)?);
  let result = writer.add_document(doc2);
  assert!(matches!(
    result.as_ref().map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  let expected_err_msg =
    "cannot change field \"bdv\" from doc values type=Binary to inconsistent doc values type=None";
  let actual_err_msg = result.unwrap_err().to_string();
  assert_eq!(actual_err_msg, expected_err_msg);

  let mut doc2 = Document::new();
  doc2.add(StringField::from_string("id", "doc1", Store::No)?);
  doc2.add(StringField::from_string("bdv", "mock-value", Store::No)?);
  doc2.add(BinaryDocValuesField::new(
    "bdv",
    to_bytes(&mut random, 10i64)?,
  ));
  writer.add_document(doc2)?;

  // update doc values of bdv field in the second segment
  let err = writer
    .update_binary_doc_value(
      Term::from_text("id", "doc1"),
      "bdv",
      to_bytes(&mut random, 5i64)?,
    )
    .unwrap_err();
  let expected_err_msg = "Can't update [Binary] doc values; the field [bdv] must be doc values only field, but is also indexed with postings.";
  assert_eq!(err.to_string(), expected_err_msg);

  writer.commit()?;
  writer.close()?;

  let reader = directory_reader::open(dir)?;
  let reader = reader.get_context()?;
  let leaves = reader.leaves()?;
  let r1 = leaves[0].reader();
  let mut bdv1 = r1.get_binary_doc_values("bdv")?.unwrap();
  assert_eq!(bdv1.next_doc()?, 0);
  assert_eq!(get_value(&bdv1)?, 5);

  let r2 = leaves[1].reader();
  let mut bdv2 = r2.get_binary_doc_values("bdv")?.unwrap();
  assert_eq!(bdv2.next_doc()?, 1);
  assert_eq!(get_value(&bdv2)?, 10);

  Ok(())
}
#[test]
fn test_update_binary_dv_field_with_same_name_as_posting_field() -> Result<()> {
  // this used to fail because FieldInfos.Builder neglected to update globalFieldMaps.docValuesTypes map
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), config)?;

  // add document with both posting field and BDV field of the same name
  let mut doc = Document::new();
  doc.add(StringField::from_string("f", "mock-value", Store::No)?);
  doc.add(BinaryDocValuesField::new(
    "f",
    to_bytes(&mut random, 5_i64)?,
  ));
  writer.add_document(doc)?;
  writer.commit()?;

  let result = writer.update_binary_doc_value(
    Term::from_text("f", "mock-value"),
    "f",
    to_bytes(&mut random, 17_i64)?,
  );
  assert!(matches!(
    result.as_ref().map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  let actual_err_msg = "Can't update [Binary] doc values; the field [f] must be doc values only field, but is also indexed with postings.";
  assert_eq!(actual_err_msg, result.unwrap_err().to_string());

  writer.close()?;

  // verify BDV content unchanged
  let reader = directory_reader::open(dir.clone())?;
  let reader = reader.get_context()?;
  let mut bdv = reader.leaves()?[0]
    .reader()
    .get_binary_doc_values("f")?
    .unwrap();
  assert_eq!(bdv.next_doc()?, 0);
  assert_eq!(get_value(&bdv)?, 5);
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
      doc.add(BinaryDocValuesField::new(
        format!("f{j}"),
        to_bytes(&mut random, value)?,
      ));
      doc.add(BinaryDocValuesField::new(
        format!("cf{j}"),
        to_bytes(&mut random, value.wrapping_mul(2))?,
      ));
    }

    writer.add_document(doc)?;
  }

  let num_threads = if is_night_mode() {
    TestUtil::next_int(&mut random, 3, 6)
  } else {
    2
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
            let mut random = random_from_seed(seed);
            let mut reader: Option<DefaultCRReader> = None;

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
                  BinaryDocValuesField::new(f, to_bytes(&mut random, upd_value)?).into(),
                  BinaryDocValuesField::new(cf, to_bytes(&mut random, upd_value.wrapping_mul(2))?)
                    .into(),
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
      let mut bdv = r.get_binary_doc_values(&format!("f{i}"))?.unwrap();
      let mut control = r.get_binary_doc_values(&format!("cf{i}"))?.unwrap();
      let live_docs = r.get_live_docs()?;

      for j in 0..r.max_doc()? {
        if live_docs
          .as_ref()
          .is_none_or(|bits| bits.get(j as usize).expect(""))
        {
          assert_eq!(j, bdv.advance(j)?);
          assert_eq!(j, control.advance(j)?);

          let value = get_value(&bdv)?;
          let control_value = get_value(&control)?;
          assert_eq!(control_value, value.wrapping_mul(2));
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

  let mut config = new_index_writer_config(&mut random)?;
  config.set_max_buffered_docs(4);
  let writer = IndexWriter::new(dir.clone(), config)?;

  let num_docs = at_least(&mut random, 10);
  for i in 0..num_docs {
    let mut doc = Document::new();
    doc.add(StringField::from_string(
      "id",
      format!("doc{i}"),
      Store::No,
    )?);
    let value = random.random::<i32>() as i64;
    doc.add(BinaryDocValuesField::new(
      "f",
      to_bytes(&mut random, value)?,
    ));
    doc.add(BinaryDocValuesField::new(
      "cf",
      to_bytes(&mut random, value.wrapping_mul(2))?,
    ));
    writer.add_document(doc)?;
  }

  let num_gens = at_least(&mut random, 5);
  for _ in 0..num_gens {
    let doc_idx = random.random_range(0..num_docs);
    let t = Term::from_text("id", format!("doc{doc_idx}"));
    let value = random.random::<i64>();
    writer.update_doc_values(
      t,
      vec![
        BinaryDocValuesField::new("f", to_bytes(&mut random, value)?).into(),
        BinaryDocValuesField::new("cf", to_bytes(&mut random, value.wrapping_mul(2))?).into(),
      ],
    )?;

    let reader = directory_reader::open_from_writer(&writer)?;
    let reader = reader.get_context()?;

    for ctx in reader.leaves()? {
      let r = ctx.reader();
      let mut fbdv = r.get_binary_doc_values("f")?.unwrap();
      let mut cfbdv = r.get_binary_doc_values("cf")?.unwrap();
      let max_doc = r.max_doc()?;

      for j in 0..max_doc {
        assert_eq!(j, fbdv.next_doc()?);
        assert_eq!(j, cfbdv.next_doc()?);
        let f = get_value(&fbdv)?;
        let cf = get_value(&cfbdv)?;
        assert_eq!(cf, f.wrapping_mul(2));
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
  doc.add(BinaryDocValuesField::new("f1", to_bytes(&mut random, 5)?));
  doc.add(BinaryDocValuesField::new("f2", to_bytes(&mut random, 13)?));
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
  doc.add(BinaryDocValuesField::new("f1", to_bytes(&mut random, 17)?));
  doc.add(BinaryDocValuesField::new("f2", to_bytes(&mut random, 2)?));
  writer.add_document(doc)?;
  writer.update_binary_doc_value(
    Term::from_text("id", "d0"),
    "f1",
    to_bytes(&mut random, 12)?,
  )?;
  writer.close()?;

  let reader = directory_reader::open(dir.clone())?;
  let mut f1 = MultiDocValues::get_binary_values(&reader, "f1")?.unwrap();
  let mut f2 = MultiDocValues::get_binary_values(&reader, "f2")?.unwrap();
  assert_eq!(0, f1.next_doc()?);
  assert_eq!(0, f2.next_doc()?);
  assert_eq!(12, get_value(&f1)?);
  assert_eq!(13, get_value(&f2)?);
  assert_eq!(1, f1.next_doc()?);
  assert_eq!(1, f2.next_doc()?);
  assert_eq!(17, get_value(&f1)?);
  assert_eq!(2, get_value(&f2)?);
  reader.close()?;
  dir.close()
}

#[test]
fn test_add_indexes() -> Result<()> {
  let mut random = random();
  let dir1 = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let writer = IndexWriter::new(dir1.clone(), config)?;

  let num_docs = at_least_usize(&mut random, 50);
  let num_terms = TestUtil::next_usize(&mut random, 1, num_docs / 5);
  let mut random_terms = HashSet::new();
  while random_terms.len() < num_terms {
    random_terms.insert(TestUtil::random_simple_string(&mut random));
  }
  let random_terms: Vec<String> = random_terms.into_iter().collect();

  // Create first index.
  for _ in 0..num_docs {
    let term = &random_terms[random.random_range(0..random_terms.len())];
    let mut doc = Document::new();
    doc.add(StringField::from_string("id", term, Store::No)?);
    doc.add(BinaryDocValuesField::new(
      "bdv",
      to_bytes(&mut random, 4_i64)?,
    ));
    doc.add(BinaryDocValuesField::new(
      "control",
      to_bytes(&mut random, 8_i64)?,
    ));
    writer.add_document(doc)?;
  }

  if random.random_bool(0.5) {
    writer.commit()?;
  }

  // Update some docs to a random value.
  let value = random.random::<i32>() as i64;
  let term = &random_terms[random.random_range(0..random_terms.len())];
  writer.update_doc_values(
    Term::from_text("id", term),
    vec![
      BinaryDocValuesField::new("bdv", to_bytes(&mut random, value)?).into(),
      BinaryDocValuesField::new("control", to_bytes(&mut random, value.wrapping_mul(2))?).into(),
    ],
  )?;
  writer.close()?;

  let dir2 = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let writer = IndexWriter::new(dir2.clone(), config)?;
  if random.random_bool(0.5) {
    writer.add_indexes_from_directory(std::slice::from_ref(&dir1))?;
  } else {
    let reader = directory_reader::open(dir1.clone())?;
    let add_result = TestUtil::add_indexes_slowly(&writer, &[&reader]);
    let add_result =
      crate::core::util::io_utils::IOUtils::use_or_suppress_result(add_result, reader.close());
    add_result?;
  }
  writer.close()?;

  let reader = directory_reader::open(dir2.clone())?;
  let context = (&reader).get_context()?;
  for context in context.leaves()? {
    let leaf = context.reader();
    let mut bdv = leaf.get_binary_doc_values("bdv")?.expect("bdv must exist");
    let mut control = leaf
      .get_binary_doc_values("control")?
      .expect("control must exist");
    for i in 0..leaf.max_doc()? {
      assert_eq!(i, bdv.next_doc()?);
      assert_eq!(i, control.next_doc()?);
      assert_eq!(get_value(&bdv)?.wrapping_mul(2), get_value(&control)?);
    }
  }
  reader.close()?;

  let close_result = dir1.as_ref().close();
  crate::core::util::io_utils::IOUtils::use_or_suppress_result(close_result, dir2.as_ref().close())
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
  doc.add(BinaryDocValuesField::new(
    "f1",
    to_bytes(&mut random, 1_i64)?,
  ));
  doc.add(BinaryDocValuesField::new(
    "f2",
    to_bytes(&mut random, 1_i64)?,
  ));
  writer.add_document(doc)?;

  // update each field twice to make sure all unneeded files are deleted
  for f in ["f1", "f2"] {
    writer.update_binary_doc_value(
      Term::from_text("id", "d0"),
      f,
      to_bytes(&mut random, 2_i64)?,
    )?;
    writer.commit()?;
    let num_files = dir.list_all()?.len();

    // update again, number of files shouldn't change (old field's gen is
    // removed)
    writer.update_binary_doc_value(
      Term::from_text("id", "d0"),
      f,
      to_bytes(&mut random, 3_i64)?,
    )?;
    writer.commit()?;

    // assert: file count should not grow
    assert_eq!(
      num_files,
      dir.list_all()?.len(),
      "Old updates files for field {f} were not deleted"
    );
  }

  writer.close()?;
  Ok(())
}

#[cfg(feature = "nightly")]
#[test]
#[ignore = "nightly"]
fn test_tons_of_updates() -> Result<()> {
  // LUCENE-5248: ensure we don't consume too much RAM when many updates occur
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config.set_ram_buffer_size_mb(DEFAULT_RAM_BUFFER_SIZE_MB);
  config.set_max_buffered_docs(DISABLE_AUTO_FLUSH);
  let writer = IndexWriter::new(dir.clone(), config)?;
  // test data: lots of documents (few 10Ks) and lots of update terms (few hundreds)
  let num_docs = at_least(&mut random, 20000);
  let num_binary_fields = at_least(&mut random, 5);
  let num_terms = TestUtil::next_usize(&mut random, 10, 100); // terms should affect many docs
  let mut update_terms = HashSet::new();
  while update_terms.len() < num_terms {
    update_terms.insert(TestUtil::random_simple_string(&mut random));
  }
  let update_terms: Vec<_> = update_terms.into_iter().collect();
  for _ in 0..num_docs {
    let mut doc = Document::new();

    let num_update_terms = TestUtil::next_usize(&mut random, 1, num_terms / 10);
    for _ in 0..num_update_terms {
      let term_value = update_terms.choose(&mut random).unwrap();
      doc.add(StringField::from_string("upd", term_value, Store::No)?);
    }

    for j in 0..num_binary_fields {
      let val = random.random::<i32>() as i64;
      doc.add(BinaryDocValuesField::new(
        format!("f{j}"),
        to_bytes(&mut random, val)?,
      ));
      doc.add(BinaryDocValuesField::new(
        format!("cf{j}"),
        to_bytes(&mut random, val.wrapping_mul(2))?,
      ));
    }

    writer.add_document(doc)?;
  }

  writer.commit()?; // commit so there's something to apply to

  // set to flush every 2048 bytes (approximately every 12 updates), so we get
  // many flushes during binary updates
  writer
    .get_config()?
    .set_ram_buffer_size_mb(2048.0 / 1024.0 / 1024.0);

  let num_updates = at_least(&mut random, 100);
  for _ in 0..num_updates {
    let field = random.random_range(0..num_binary_fields);
    let update_term = Term::from_text("upd", update_terms.choose(&mut random).unwrap());
    let value = random.random::<i32>() as i64;
    writer.update_doc_values(
      update_term,
      vec![
        BinaryDocValuesField::new(format!("f{field}"), to_bytes(&mut random, value)?).into(),
        BinaryDocValuesField::new(
          format!("cf{field}"),
          to_bytes(&mut random, value.wrapping_mul(2))?,
        )
        .into(),
      ],
    )?;
  }

  writer.close()?;

  let reader = directory_reader::open(dir.clone())?;
  let reader = reader.get_context()?;
  for context in reader.leaves()? {
    let r = context.reader();
    let max_doc = r.max_doc()?;

    for i in 0..num_binary_fields {
      let mut f = r.get_binary_doc_values(&format!("f{i}"))?.unwrap();
      let mut cf = r.get_binary_doc_values(&format!("cf{i}"))?.unwrap();

      for j in 0..max_doc {
        assert_eq!(j, f.next_doc()?);
        assert_eq!(j, cf.next_doc()?);

        let v_f = get_value(&f)?;
        let v_cf = get_value(&cf)?;
        assert_eq!(
          v_cf,
          v_f.wrapping_mul(2),
          "field=f{i}, doc={j}, cf={v_cf}, f={v_f}"
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

  let mut doc = Document::new();
  doc.add(StringField::from_string("upd", "t1", Store::No)?);
  doc.add(StringField::from_string("upd", "t2", Store::No)?);
  doc.add(BinaryDocValuesField::new(
    "f1",
    to_bytes(&mut random, 1_i64)?,
  ));
  doc.add(BinaryDocValuesField::new(
    "f2",
    to_bytes(&mut random, 1_i64)?,
  ));
  writer.add_document(doc)?;

  // update operations — MUST respect order
  writer.update_binary_doc_value(
    Term::from_text("upd", "t1"),
    "f1",
    to_bytes(&mut random, 2_i64)?,
  )?; // update f1 to 2
  writer.update_binary_doc_value(
    Term::from_text("upd", "t1"),
    "f2",
    to_bytes(&mut random, 2_i64)?,
  )?; // update f2 to 2
  writer.update_binary_doc_value(
    Term::from_text("upd", "t2"),
    "f1",
    to_bytes(&mut random, 3_i64)?,
  )?; // update f1 to 3
  writer.update_binary_doc_value(
    Term::from_text("upd", "t2"),
    "f2",
    to_bytes(&mut random, 3_i64)?,
  )?; // update f2 to 3
  // last update only affects f1
  writer.update_binary_doc_value(
    Term::from_text("upd", "t1"),
    "f1",
    to_bytes(&mut random, 4_i64)?,
  )?; // update f1 to 4 (but not f2)

  writer.close()?;

  let reader = directory_reader::open(dir.clone())?;
  let reader = reader.get_context()?;

  let leaf = &reader.leaves()?[0];
  let r = leaf.reader();

  let mut bdv = r.get_binary_doc_values("f1")?.unwrap();
  assert_eq!(0, bdv.next_doc()?);
  assert_eq!(4_i64, get_value(&bdv)?);

  let mut bdv = r.get_binary_doc_values("f2")?.unwrap();
  assert_eq!(0, bdv.next_doc()?);
  assert_eq!(3_i64, get_value(&bdv)?);

  Ok(())
}

#[test]
fn test_update_all_deleted_segment() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), config)?;

  // create base document
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "doc", Store::No)?);
  doc.add(BinaryDocValuesField::new(
    "f1",
    to_bytes(&mut random, 1_i64)?,
  ));

  // add two docs, then commit
  writer.add_document(doc)?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "doc", Store::No)?);
  doc.add(BinaryDocValuesField::new(
    "f1",
    to_bytes(&mut random, 1_i64)?,
  ));
  writer.add_document(doc)?;
  writer.commit()?;

  writer.delete_documents_with_terms(vec![Term::from_text("id", "doc")])?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "doc", Store::No)?);
  doc.add(BinaryDocValuesField::new(
    "f1",
    to_bytes(&mut random, 1_i64)?,
  ));
  writer.add_document(doc)?;

  writer.update_binary_doc_value(
    Term::from_text("id", "doc"),
    "f1",
    to_bytes(&mut random, 2_i64)?,
  )?;

  writer.close()?;

  // open reader and verify
  let reader = directory_reader::open(dir.clone())?;
  let reader = reader.get_context()?;

  let leaf = &reader.leaves()?[0];
  let r = leaf.reader();
  let mut bdv = r.get_binary_doc_values("f1")?.unwrap();

  assert_eq!(0, bdv.next_doc()?);
  assert_eq!(2_i64, get_value(&bdv)?);

  Ok(())
}

#[test]
fn test_update_two_nonexisting_terms() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), config)?;

  // create initial document
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "doc", Store::No)?);
  doc.add(BinaryDocValuesField::new(
    "f1",
    to_bytes(&mut random, 1_i64)?,
  ));
  writer.add_document(doc)?;

  // update with multiple non-existing terms in same field
  writer.update_binary_doc_value(
    Term::from_text("c", "foo"),
    "f1",
    to_bytes(&mut random, 2_i64)?,
  )?;
  writer.update_binary_doc_value(
    Term::from_text("c", "bar"),
    "f1",
    to_bytes(&mut random, 2_i64)?,
  )?;
  writer.close()?;

  // open reader and verify value not changed
  let reader = directory_reader::open(dir.clone())?;
  let reader = reader.get_context()?;

  let leaf = &reader.leaves()?[0];
  let r = leaf.reader();
  let mut bdv = r.get_binary_doc_values("f1")?.unwrap();

  assert_eq!(0, bdv.next_doc()?);
  assert_eq!(1_i64, get_value(&bdv)?);

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
    writer.add_document(doc(&mut random, i)?)?;
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
  writer.update_binary_doc_value(
    Term::from_text("id", "doc-0"),
    "val",
    to_bytes(&mut random, 100)?,
  )?;
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

#[derive(Clone, Debug)]
struct OneSortDoc {
  pub value: BytesRef<Vec<u8>>,
  pub sort_value: i64,
  pub id: i32,
  pub deleted: bool,
}

impl OneSortDoc {
  fn new(id: i32, value: BytesRef<Vec<u8>>, sort_value: i64) -> Self {
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
