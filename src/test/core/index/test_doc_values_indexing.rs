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
use crate::core::analysis::analyzer::Analyzer;
use crate::core::analysis::reader::ReaderEnum;
use crate::core::document::binary_doc_values_field::BinaryDocValuesField;
use crate::core::document::document::Document;
use crate::core::document::field::Store::No;
use crate::core::document::field::{Field, FieldBase, FieldDataEnum, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::invertable_field::InvertableType;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::sorted_doc_values_field::SortedDocValuesField;
use crate::core::document::sorted_set_doc_values_field::SortedSetDocValuesField;
use crate::core::document::string_field::StringField;
use crate::core::document::text_field::TextField;
use crate::core::index::BytesRef;
use crate::core::index::directory_reader;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::field_infos::get_merged_field_infos;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::OpenMode::Create;
use crate::core::index::index_writer_config::{IndexWriterConfig, OpenMode};
use crate::core::index::indexable_field::{
  IndexableField, IndexingTokenStream, ReusedIndexingTokenStream,
};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::multi_doc_values::MultiDocValues;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::slow_codec_reader_wrapper::SlowCodecReaderWrapper;
use crate::core::index::sorted_doc_values::SortedDocValues;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::number::Number;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
pub use crate::test_framework::core::document::FieldImpl;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::util::lucene_test_case::{
  get_only_leaf_reader, new_bytes_ref_from_bytes, new_bytes_ref_from_string, new_directory_shared,
  new_index_writer_config, new_index_writer_config_with_analyzer, new_log_merge_policy,
  new_string_field, random,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::Rng;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

#[allow(dead_code)] // for quick search
struct TestDocValuesIndexing;

#[test]
fn test_add_indexes() -> Result<()> {
  let mut random = random();
  let mut field_types = HashMap::new();

  let d1 = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, d1.clone())?;
  let mut doc = Document::new();
  doc.add(new_string_field(
    &mut random,
    "id",
    "1",
    Store::Yes,
    &mut field_types,
  )?);
  doc.add(NumericDocValuesField::new("dv", 1));
  w.add_document(&mut random, doc)?;
  let r1 = w.get_reader(&mut random)?;
  w.close(&mut random)?;

  let d2 = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, d2.clone())?;
  let mut doc = Document::new();
  doc.add(new_string_field(
    &mut random,
    "id",
    "2",
    Store::Yes,
    &mut field_types,
  )?);
  doc.add(NumericDocValuesField::new("dv", 2));
  w.add_document(&mut random, doc)?;
  let r2 = w.get_reader(&mut random)?;
  w.close(&mut random)?;

  let d3 = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, d3.clone())?;
  w.add_indexes_from_codec_readers(
    &mut random,
    vec![
      SlowCodecReaderWrapper::wrap_leaf_reader(get_only_leaf_reader(&r1)?),
      SlowCodecReaderWrapper::wrap_leaf_reader(get_only_leaf_reader(&r2)?),
    ],
  )?;
  r1.close()?;
  d1.close()?;
  r2.close()?;
  d2.close()?;

  w.force_merge(&mut random, 1)?;
  let r3 = w.get_reader(&mut random)?;
  w.close(&mut random)?;
  let sr = get_only_leaf_reader(&r3)?;
  assert_eq!(2, sr.num_docs()?);
  let doc_values = sr.get_numeric_doc_values("dv")?;
  assert!(doc_values.is_some());
  r3.close()?;
  d3.close()?;
  Ok(())
}
#[test]
fn test_multi_valued_doc_values_field() -> Result<()> {
  let mut random = random();

  let d = new_directory_shared(&mut random)?;
  let config = new_index_writer_config(&mut random)?;
  let w = RandomIndexWriter::with_config(&mut random, d.clone(), config);

  let mut doc = Document::new();
  let f = NumericDocValuesField::new("field", 17);
  doc.add(f.clone());

  w.add_document(&mut random, doc.clone())?;

  doc.add(f.clone());
  // Index doc values are single-valued so we should not
  // be able to add same field more than once:
  let res = w.add_document(&mut random, doc);
  assert!(
    matches!(res, Err(LuceneError::IllegalArgument(_))),
    "expected IllegalArgument but got: {:?}",
    res
  );

  let r = w.get_reader(&mut random)?;
  w.close(&mut random)?;

  let leaf = get_only_leaf_reader(r)?;
  let values_opt = leaf.get_numeric_doc_values("field")?;
  assert!(values_opt.is_some());
  let mut values = values_opt.unwrap();

  assert_eq!(0, values.next_doc()?);
  assert_eq!(17, values.long_value()?);

  Ok(())
}
#[test]
fn test_different_typed_doc_values_field() -> Result<()> {
  let mut random = random();

  // directory + writer
  let d = new_directory_shared(&mut random)?;
  let config = new_index_writer_config(&mut random)?;
  let w = RandomIndexWriter::with_config(&mut random, d.clone(), config);

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("field", 17));
  w.add_document(&mut random, doc.clone())?;

  // Index doc values are single-valued so we should not
  // be able to add same field more than once:
  doc.add(BinaryDocValuesField::new(
    "field",
    new_bytes_ref_from_string(&mut random, "blah")?,
  ));

  let res = w.add_document(&mut random, doc);
  assert!(
    matches!(res, Err(LuceneError::IllegalArgument(_))),
    "expected IllegalArgument for mixed doc-values types, got: {:?}",
    res
  );

  let r = w.get_reader(&mut random)?;
  w.close(&mut random)?;

  let leaf = get_only_leaf_reader(r)?;
  let values_opt = leaf.get_numeric_doc_values("field")?;
  assert!(values_opt.is_some());

  let mut values = values_opt.unwrap();
  assert_eq!(0, values.next_doc()?);
  assert_eq!(17, values.long_value()?);

  Ok(())
}
#[test]
fn test_different_typed_doc_values_field2() -> Result<()> {
  let mut random = random();

  let d = new_directory_shared(&mut random)?;
  let config = new_index_writer_config(&mut random)?;
  let w = RandomIndexWriter::with_config(&mut random, d.clone(), config);

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("field", 17));
  w.add_document(&mut random, doc.clone())?;
  // Index doc values are single-valued so we should not
  // be able to add same field more than once:
  doc.add(SortedDocValuesField::new(
    "field",
    new_bytes_ref_from_string(&mut random, "hello")?,
  ));

  let res = w.add_document(&mut random, doc);
  assert!(
    matches!(res, Err(LuceneError::IllegalArgument(_))),
    "expected IllegalArgument but got: {:?}",
    res
  );

  let r = w.get_reader(&mut random)?;

  let leaf = get_only_leaf_reader(r)?;
  let values_opt = leaf.get_numeric_doc_values("field")?;
  assert!(values_opt.is_some());
  let mut values = values_opt.unwrap();

  assert_eq!(0, values.next_doc()?);
  assert_eq!(17, values.long_value()?);

  w.close(&mut random)?;

  Ok(())
}
#[test]
fn test_length_prefix_across_two_pages() -> Result<()> {
  let mut random = random();

  let d = new_directory_shared(&mut random)?;
  let config = IndexWriterConfig::with_analyzer(MockAnalyzer::new(&mut random))?;
  let w = IndexWriter::new(d.clone(), config)?;

  let mut doc = Document::new();

  let mut bytes = vec![0u8; 32_764];
  let mut b = BytesRef::from_bytes(bytes.clone());
  doc.add(SortedDocValuesField::new("field", b));
  w.add_document(doc.clone())?;

  bytes[0] = 1;
  b = BytesRef::from_bytes(bytes.clone());
  doc = Document::new();
  doc.add(SortedDocValuesField::new("field", b.clone()));
  w.add_document(doc)?;
  w.force_merge(1)?;
  let r = directory_reader::open_from_writer(&w)?;

  let leaf = get_only_leaf_reader(r)?;
  let mut s = leaf
    .get_sorted_doc_values("field")?
    .expect("sorted doc values must exist");

  assert_eq!(0, s.next_doc()?);
  let ord = s.ord_value()?;
  let mut bytes1 = s.lookup_ord(ord)?;

  assert_eq!(bytes.len(), bytes1.length);

  bytes[0] = 0;
  let b0 = BytesRef::from_bytes(bytes.clone());
  assert_eq!(&b0, bytes1.as_ref());

  assert_eq!(1, s.next_doc()?);
  let ord2 = s.ord_value()?;
  bytes1 = s.lookup_ord(ord2)?;
  assert_eq!(bytes.len(), bytes1.length);

  bytes[0] = 1;
  let b1 = BytesRef::from_bytes(bytes.clone());
  assert_eq!(&b1, bytes1.as_ref());

  w.close()?;

  Ok(())
}
#[test]
fn test_doc_values_unstored() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  iwc.set_merge_policy(new_log_merge_policy(&mut random)?);

  let writer = IndexWriter::new(dir.clone(), iwc)?;
  for i in 0..50 {
    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("dv", i as i64));
    doc.add(TextField::from_string("docId", i.to_string(), Store::Yes)?);
    writer.add_document(doc)?;
  }

  let reader = directory_reader::open_from_writer(&writer)?;
  let fi = get_merged_field_infos(&reader)?;
  let dv_info = fi
    .field_info_by_name("dv")
    .ok_or_else(|| LuceneError::illegal_state("missing field dv"))?;
  assert_ne!(*dv_info.get_doc_values_type(), DocValuesType::None);

  let mut dv = MultiDocValues::get_numeric_values(&reader, "dv")?.unwrap();
  let mut stored_fields = reader.stored_fields()?;

  for i in 0..50 {
    assert_eq!(i, dv.next_doc()?);
    assert_eq!(i as i64, dv.long_value()?);

    let d = stored_fields.document(i)?;
    // cannot use d.get("dv") due to another bug!
    assert!(d.get_field("dv").is_none());
    assert_eq!(&i.to_string(), d.get("docId")?.unwrap().as_ref());
  }
  writer.close()?;
  Ok(())
}
#[test]
fn test_mixed_types_same_document() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let w = IndexWriter::new(dir.clone(), config)?;

  w.add_document(Document::new())?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("foo", 0));
  doc.add(SortedDocValuesField::new(
    "foo",
    new_bytes_ref_from_string(&mut random, "hello")?,
  ));

  let res = w.add_document(doc);
  assert!(
    matches!(res, Err(LuceneError::IllegalArgument(_))),
    "expected IllegalArgument but got: {:?}",
    res
  );

  let ir = directory_reader::open_from_writer(&w)?;
  assert_eq!(1, ir.num_docs()?);

  w.close()?;

  Ok(())
}
#[test]
fn test_mixed_types_different_documents() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let w = IndexWriter::new(dir.clone(), config)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("foo", 0));
  w.add_document(doc)?;

  let mut doc2 = Document::new();
  doc2.add(SortedDocValuesField::new(
    "foo",
    new_bytes_ref_from_string(&mut random, "hello")?,
  ));

  let res = w.add_document(doc2);
  assert!(
    matches!(res, Err(LuceneError::IllegalArgument(_))),
    "expected IllegalArgument but got: {:?}",
    res
  );

  let ir = directory_reader::open_from_writer(&w)?;
  assert_eq!(1, ir.num_docs()?);

  w.close()?;

  Ok(())
}
#[test]
fn test_add_sorted_twice() -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  iwc.set_merge_policy(new_log_merge_policy(&mut random)?);
  let iwriter = IndexWriter::new(directory.clone(), iwc)?;

  let mut doc = Document::new();
  doc.add(SortedDocValuesField::new(
    "dv",
    new_bytes_ref_from_string(&mut random, "foo!")?,
  ));
  iwriter.add_document(doc.clone())?;

  doc.add(SortedDocValuesField::new(
    "dv",
    new_bytes_ref_from_string(&mut random, "bar!")?,
  ));

  let res = iwriter.add_document(doc);
  assert!(
    matches!(res, Err(LuceneError::IllegalArgument(_))),
    "expected IllegalArgument but got: {:?}",
    res
  );

  let ir = directory_reader::open_from_writer(&iwriter)?;
  assert_eq!(1, ir.num_docs()?);
  iwriter.close()?;

  Ok(())
}
#[test]
fn test_add_binary_twice() -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  iwc.set_merge_policy(new_log_merge_policy(&mut random)?);
  let iwriter = IndexWriter::new(directory.clone(), iwc)?;

  let mut doc = Document::new();
  doc.add(BinaryDocValuesField::new(
    "dv",
    new_bytes_ref_from_string(&mut random, "foo!")?,
  ));
  iwriter.add_document(doc.clone())?;

  doc.add(BinaryDocValuesField::new(
    "dv",
    new_bytes_ref_from_string(&mut random, "bar!")?,
  ));

  let res = iwriter.add_document(doc);
  assert!(
    matches!(res, Err(LuceneError::IllegalArgument(_))),
    "expected IllegalArgument but got: {:?}",
    res
  );

  let ir = directory_reader::open_from_writer(&iwriter)?;
  assert_eq!(1, ir.num_docs()?);

  iwriter.close()?;

  Ok(())
}
#[test]
fn test_add_numeric_twice() -> Result<()> {
  let mut random = random();

  let directory = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  iwc.set_merge_policy(new_log_merge_policy(&mut random)?);
  let iwriter = IndexWriter::new(directory.clone(), iwc)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("dv", 1));
  iwriter.add_document(doc.clone())?;

  doc.add(NumericDocValuesField::new("dv", 2));

  let res = iwriter.add_document(doc);
  assert!(
    matches!(res, Err(LuceneError::IllegalArgument(_))),
    "expected IllegalArgument but got: {:?}",
    res
  );

  let ir = directory_reader::open_from_writer(&iwriter)?;
  assert_eq!(1, ir.num_docs()?);

  iwriter.close()?;

  Ok(())
}
#[test]
fn test_too_large_sorted_bytes() -> Result<()> {
  let mut random = random();

  let directory = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  iwc.set_merge_policy(new_log_merge_policy(&mut random)?);
  let iwriter = IndexWriter::new(directory.clone(), iwc)?;

  let mut doc = Document::new();
  doc.add(SortedDocValuesField::new(
    "dv",
    new_bytes_ref_from_string(&mut random, "just fine")?,
  ));
  iwriter.add_document(doc.clone())?;

  // huge doc: SortedDocValues too large
  let mut huge_doc = Document::new();
  let mut bytes = vec![0u8; 100_000];
  random.fill_bytes(&mut bytes);
  let b = new_bytes_ref_from_bytes(&mut random, bytes.as_ref())?;

  huge_doc.add(SortedDocValuesField::new("dv", b));

  let res = iwriter.add_document(huge_doc);
  assert!(matches!(res, Err(LuceneError::IllegalArgument(_))));

  let ir = directory_reader::open_from_writer(&iwriter)?;
  assert_eq!(1, ir.num_docs()?);

  iwriter.close()?;

  Ok(())
}
#[test]
fn test_too_large_term_sorted_set_bytes() -> Result<()> {
  let mut random = random();

  let directory = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  iwc.set_merge_policy(new_log_merge_policy(&mut random)?);
  let iwriter = IndexWriter::new(directory.clone(), iwc)?;

  // Initial OK doc
  let mut doc = Document::new();
  doc.add(SortedSetDocValuesField::new(
    "dv",
    new_bytes_ref_from_string(&mut random, "just fine")?,
  ));
  iwriter.add_document(doc.clone())?;

  // Huge doc containing SortedSetDV with very large BytesRef
  let mut huge_doc = Document::new();
  let mut bytes = vec![0u8; 100_000];
  random.fill_bytes(&mut bytes);
  let b = BytesRef::from_bytes(bytes);

  huge_doc.add(SortedSetDocValuesField::new("dv", b));

  let res = iwriter.add_document(huge_doc);
  assert!(
    matches!(res, Err(LuceneError::IllegalArgument(_))),
    "expected IllegalArgument but got: {:?}",
    res
  );

  let ir = directory_reader::open_from_writer(&iwriter)?;
  assert_eq!(1, ir.num_docs()?);

  iwriter.close()?;

  Ok(())
}
#[test]
fn test_mixed_types_different_segments() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("foo", 0));
  w.add_document(doc)?;
  w.commit()?;

  let mut doc2 = Document::new();
  doc2.add(SortedDocValuesField::new(
    "foo",
    new_bytes_ref_from_string(&mut random, "hello")?,
  ));

  let res = w.add_document(doc2);
  assert!(
    matches!(res, Err(LuceneError::IllegalArgument(_))),
    "expected IllegalArgument but got: {:?}",
    res
  );

  w.close()?;

  Ok(())
}

#[test]
fn test_mixed_types_after_delete_all() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("foo", 0));
  w.add_document(doc)?;

  w.delete_all()?;

  let mut doc = Document::new();
  doc.add(SortedDocValuesField::new(
    "foo",
    BytesRef::from_string("hello"),
  ));
  w.add_document(doc)?;
  w.close()?;
  Ok(())
}
#[test]
fn test_mixed_types_after_reopen_create() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let iwc1 = new_index_writer_config_with_analyzer(&mut random, mock)?;
  {
    let w = IndexWriter::new(dir.clone(), iwc1)?;
    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("foo", 0));
    w.add_document(doc)?;
    w.close()?;
  }

  let mut iwc2 = new_index_writer_config(&mut random)?;
  iwc2.set_open_mode(OpenMode::Create);
  let w2 = IndexWriter::new(dir.clone(), iwc2)?;

  let doc2 = Document::new();
  w2.add_document(doc2)?;

  w2.close()?;

  Ok(())
}

#[test]
fn test_mixed_types_after_reopen_append1() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let iwc1 = new_index_writer_config_with_analyzer(&mut random, mock)?;
  {
    let w = IndexWriter::new(dir.clone(), iwc1)?;
    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("foo", 0));
    w.add_document(doc)?;
    w.close()?;
  }

  let iwc2 = new_index_writer_config(&mut random)?;
  let w2 = IndexWriter::new(dir.clone(), iwc2)?;

  let mut doc2 = Document::new();
  doc2.add(SortedDocValuesField::new(
    "foo",
    new_bytes_ref_from_string(&mut random, "hello")?,
  ));

  let res = w2.add_document(doc2);
  assert!(
    matches!(res, Err(LuceneError::IllegalArgument(_))),
    "expected IllegalArgument but got: {:?}",
    res
  );

  w2.close()?;

  Ok(())
}
#[test]
fn test_mixed_types_after_reopen_append2() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let iwc1 = new_index_writer_config_with_analyzer(&mut random, mock)?;
  {
    let w = IndexWriter::new(dir.clone(), iwc1)?;
    let mut doc = Document::new();
    doc.add(SortedSetDocValuesField::new(
      "foo",
      new_bytes_ref_from_string(&mut random, "foo")?,
    ));
    w.add_document(doc)?;
    w.close()?;
  }

  let iwc2 = new_index_writer_config(&mut random)?;
  let w2 = IndexWriter::new(dir.clone(), iwc2)?;

  // Add a field first as StringField (no DV), then as BinaryDV → must error
  let mut doc2 = Document::new();
  doc2.add(StringField::from_string("foo", "bar", No)?);
  doc2.add(BinaryDocValuesField::new(
    "foo",
    new_bytes_ref_from_string(&mut random, "foo")?,
  ));

  let res = w2.add_document(doc2);
  // NOTE: this case follows a different code path inside
  // DefaultIndexingChain/FieldInfos, because the field (foo)
  // is first added without DocValues:
  assert!(
    matches!(res, Err(LuceneError::IllegalArgument(_))),
    "expected IllegalArgument but got: {:?}",
    res
  );

  w2.force_merge(1)?;
  w2.close()?;

  Ok(())
}
#[test]
fn test_mixed_types_after_reopen_append3() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let iwc1 = new_index_writer_config_with_analyzer(&mut random, mock)?;
  {
    let w = IndexWriter::new(dir.clone(), iwc1)?;
    let mut doc = Document::new();
    doc.add(SortedSetDocValuesField::new(
      "foo",
      new_bytes_ref_from_string(&mut random, "foo")?,
    ));
    w.add_document(doc)?;
    w.close()?;
  }

  let iwc2 = new_index_writer_config(&mut random)?;
  let w2 = IndexWriter::new(dir.clone(), iwc2)?;

  // Add a StringField first (no DV), then BinaryDV → must error
  let mut doc2 = Document::new();
  doc2.add(StringField::from_string("foo", "bar", No)?);
  doc2.add(BinaryDocValuesField::new(
    "foo",
    new_bytes_ref_from_string(&mut random, "foo")?,
  ));

  let res = w2.add_document(doc2);
  assert!(
    matches!(res, Err(LuceneError::IllegalArgument(_))),
    "expected IllegalArgument but got: {:?}",
    res
  );

  // Also add another document to ensure a segment is written
  w2.add_document(Document::new())?;
  w2.force_merge(1)?;
  w2.close()?;

  Ok(())
}
#[test]
fn test_mixed_types_different_threads() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let starting_gun = Arc::new(Barrier::new(4));
  let hit_exc = Arc::new(AtomicBool::new(false));
  let mut docs = Vec::new();
  for i in 0..3 {
    let mut doc = Document::new();
    if i == 0 {
      doc.add(SortedDocValuesField::new(
        "foo",
        new_bytes_ref_from_string(&mut random, "hello")?,
      ));
    } else if i == 1 {
      doc.add(NumericDocValuesField::new("foo", 0));
    } else {
      doc.add(BinaryDocValuesField::new(
        "foo",
        new_bytes_ref_from_string(&mut random, "bazz")?,
      ));
    }
    docs.push(doc);
  }

  let thread_results = thread::scope(|scope| {
    let mut handles = Vec::new();
    for doc in docs {
      let starting_gun = starting_gun.clone();
      let hit_exc = hit_exc.clone();
      let writer = &w;
      handles.push(scope.spawn(move || -> Result<()> {
        starting_gun.wait();
        let res = writer.add_document(doc);
        match res {
          Ok(_) => Ok(()),
          Err(LuceneError::IllegalArgument(_)) => {
            hit_exc.store(true, Ordering::SeqCst);
            Ok(())
          },
          Err(err) => Err(err),
        }
      }));
    }

    starting_gun.wait();

    let mut results = Vec::new();
    for handle in handles {
      results.push(handle.join());
    }
    results
  });

  for thread_result in thread_results {
    thread_result.map_err(|_| LuceneError::illegal_state("thread hit exception"))??;
  }

  assert!(hit_exc.load(Ordering::SeqCst));
  w.close()?;
  Ok(())
}
#[test]
fn test_mixed_types_via_add_indexes() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let conf = new_index_writer_config_with_analyzer(&mut random, a)?;
  let w = IndexWriter::new(dir.clone(), conf)?;
  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("foo", 0));
  w.add_document(doc)?;

  // Make 2nd index w/ inconsistent field
  let dir2 = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let conf = new_index_writer_config_with_analyzer(&mut random, a)?;
  let w2 = IndexWriter::new(dir2.clone(), conf)?;
  let mut doc = Document::new();
  doc.add(SortedDocValuesField::new(
    "foo",
    new_bytes_ref_from_string(&mut random, "hello")?,
  ));
  w2.add_document(doc)?;
  w2.close()?;

  let err = w.add_indexes_from_directory(std::slice::from_ref(&dir2));
  assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

  let r = directory_reader::open(dir2.clone())?;
  let err = TestUtil::add_indexes_slowly(&w, &[&r]);
  assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

  r.close()?;
  w.close()?;
  Ok(())
}
#[test]
fn test_illegal_type_change() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let conf = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), conf)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("dv", 0));
  writer.add_document(doc)?;

  let mut doc2 = Document::new();
  doc2.add(SortedDocValuesField::new(
    "dv",
    new_bytes_ref_from_string(&mut random, "foo")?,
  ));

  let res = writer.add_document(doc2);
  assert!(
    matches!(res, Err(LuceneError::IllegalArgument(_))),
    "expected IllegalArgument but got: {:?}",
    res
  );

  let ir = directory_reader::open_from_writer(&writer)?;
  assert_eq!(1, ir.num_docs()?);

  writer.close()?;

  Ok(())
}
#[test]
fn test_illegal_type_change_across_segments() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let conf1 = new_index_writer_config_with_analyzer(&mut random, mock)?;
  {
    let writer = IndexWriter::new(dir.clone(), conf1)?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("dv", 0));
    writer.add_document(doc)?;
    writer.close()?;
  }

  let conf2 = new_index_writer_config(&mut random)?;
  let writer2 = IndexWriter::new(dir.clone(), conf2)?;

  let mut doc2 = Document::new();
  doc2.add(SortedDocValuesField::new(
    "dv",
    new_bytes_ref_from_string(&mut random, "foo")?,
  ));

  let res = writer2.add_document(doc2);
  assert!(
    matches!(res, Err(LuceneError::IllegalArgument(_))),
    "expected IllegalArgument but got {:?}",
    res
  );

  writer2.close()?;

  Ok(())
}
#[test]
fn test_type_change_after_close_and_delete_all() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let conf1 = new_index_writer_config_with_analyzer(&mut random, mock)?;
  {
    let writer = IndexWriter::new(dir.clone(), conf1)?;
    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("dv", 0));
    writer.add_document(doc)?;
    writer.close()?;
  }

  let conf2 = new_index_writer_config(&mut random)?;
  let writer2 = IndexWriter::new(dir.clone(), conf2)?;
  writer2.delete_all()?;

  let mut doc2 = Document::new();
  doc2.add(SortedDocValuesField::new(
    "dv",
    new_bytes_ref_from_string(&mut random, "foo")?,
  ));
  writer2.add_document(doc2)?;

  writer2.close()?;

  Ok(())
}

#[test]
fn test_type_change_after_delete_all() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let conf = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), conf)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("dv", 0));
  writer.add_document(doc)?;

  writer.delete_all()?;

  let mut doc = Document::new();
  doc.add(SortedDocValuesField::new(
    "dv",
    BytesRef::from_string("foo"),
  ));
  writer.add_document(doc)?;

  writer.close()?;

  Ok(())
}

#[test]
fn test_type_change_after_commit_and_delete_all() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let conf = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), conf)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("dv", 0));
  writer.add_document(doc)?;

  writer.commit()?;
  writer.delete_all()?;

  let mut doc = Document::new();
  doc.add(SortedDocValuesField::new(
    "dv",
    BytesRef::from_string("foo"),
  ));
  writer.add_document(doc)?;

  writer.close()?;

  Ok(())
}
#[test]
fn test_type_change_after_open_create() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let conf1 = new_index_writer_config_with_analyzer(&mut random, mock)?;
  {
    let writer = IndexWriter::new(dir.clone(), conf1)?;
    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("dv", 0));
    writer.add_document(doc)?;
    writer.close()?;
  }

  let mut conf2 = new_index_writer_config(&mut random)?;
  conf2.set_open_mode(Create);
  let writer2 = IndexWriter::new(dir.clone(), conf2)?;

  let mut doc2 = Document::new();
  doc2.add(SortedDocValuesField::new(
    "dv",
    new_bytes_ref_from_string(&mut random, "foo")?,
  ));
  writer2.add_document(doc2)?;

  writer2.close()?;

  Ok(())
}

#[test]
fn test_type_change_via_add_indexes() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, a)?;
  let writer = IndexWriter::new(dir.clone(), iwc)?;
  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("dv", 0));
  writer.add_document(doc)?;
  writer.close()?;
  drop(writer);

  let dir2 = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, a)?;
  let writer2 = IndexWriter::new(dir2.clone(), iwc)?;
  let mut doc = Document::new();
  doc.add(SortedDocValuesField::new(
    "dv",
    BytesRef::from_string("foo"),
  ));
  writer2.add_document(doc)?;

  let err = writer2.add_indexes_from_directory(std::slice::from_ref(&dir));
  assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

  writer2.close()?;
  Ok(())
}
#[test]
fn test_type_change_via_add_indexes_ir() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let conf = new_index_writer_config_with_analyzer(&mut random, a)?;
  let writer = IndexWriter::new(dir.clone(), conf)?;
  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("dv", 0));
  writer.add_document(doc)?;
  writer.close()?;

  let dir2 = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let conf = new_index_writer_config_with_analyzer(&mut random, a)?;
  let writer2 = IndexWriter::new(dir2.clone(), conf)?;
  let mut doc = Document::new();
  doc.add(SortedDocValuesField::new(
    "dv",
    new_bytes_ref_from_string(&mut random, "foo")?,
  ));
  writer2.add_document(doc)?;
  let reader = directory_reader::open(dir.clone())?;
  let err = TestUtil::add_indexes_slowly(&writer2, &[&reader]);
  assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

  reader.close()?;
  writer2.close()?;
  Ok(())
}
#[test]
fn test_type_change_via_add_indexes2() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, a)?;
  let writer = IndexWriter::new(dir.clone(), iwc)?;
  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("dv", 0));
  writer.add_document(doc)?;
  writer.close()?;
  drop(writer);

  let dir2 = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, a)?;
  let writer2 = IndexWriter::new(dir2.clone(), iwc)?;
  writer2.add_indexes_from_directory(std::slice::from_ref(&dir))?;

  let mut doc2 = Document::new();
  doc2.add(SortedDocValuesField::new(
    "dv",
    BytesRef::from_string("foo"),
  ));
  let err = writer2.add_document(doc2);
  assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

  writer2.close()?;
  Ok(())
}
#[test]
fn test_type_change_via_add_indexes_ir_2() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let conf = new_index_writer_config_with_analyzer(&mut random, a)?;
  let writer = IndexWriter::new(dir.clone(), conf)?;
  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("dv", 0));
  writer.add_document(doc)?;
  writer.close()?;

  let dir2 = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let conf = new_index_writer_config_with_analyzer(&mut random, a)?;
  let writer2 = IndexWriter::new(dir2.clone(), conf)?;
  let reader = directory_reader::open(dir.clone())?;
  TestUtil::add_indexes_slowly(&writer2, &[&reader])?;
  reader.close()?;

  let mut doc2 = Document::new();
  doc2.add(SortedDocValuesField::new(
    "dv",
    new_bytes_ref_from_string(&mut random, "foo")?,
  ));
  let err = writer2.add_document(doc2);
  assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

  writer2.close()?;
  Ok(())
}
#[test]
fn test_same_field_name_for_posting_and_doc_value() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let conf = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), conf)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("f", "mock-value", No)?);
  doc.add(NumericDocValuesField::new("f", 5));
  writer.add_document(doc)?;
  writer.commit()?;

  let mut doc2 = Document::new();
  doc2.add(BinaryDocValuesField::new(
    "f",
    new_bytes_ref_from_string(&mut random, "mock")?,
  ));
  let res = writer.add_document(doc2);
  assert!(matches!(res, Err(LuceneError::IllegalArgument(_))));

  writer.rollback()?;
  Ok(())
}

#[test]
fn test_exc_indexing_doc_before_doc_values() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut ft = FieldType::from_ref(&*crate::core::document::string_field::TYPE_NOT_STORED)?;
  ft.set_doc_values_type(DocValuesType::Sorted)?;
  ft.freeze();

  let bytes = BytesRef::from_string("value");
  let field = FieldImpl::new("test", bytes, ft);

  let mut doc = Document::new();
  doc.add(field);

  let res = w.add_document(doc);
  assert!(matches!(res, Err(LuceneError::UnsupportedOperation(_))));

  w.add_document(Document::new())?;
  w.close()?;
  Ok(())
}
