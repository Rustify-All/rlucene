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
use crate::core::document::document::Document;
use crate::core::document::field::{Field, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::stored_field::stored_field_type;
use crate::core::index::BytesRef;
use crate::core::index::directory_reader;
use crate::core::index::fields::Fields;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::{DISABLE_AUTO_FLUSH, IndexWriterConfig};
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::log_merge_policy::LogMergePolicy;
use crate::core::index::postings_enum::{ALL, PostingsEnum};
use crate::core::index::serial_merge_scheduler::SerialMergeScheduler;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term_vectors::TermVectors;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::util::lucene_test_case::{
  new_directory_shared, new_field, new_index_writer_config, new_index_writer_config_with_analyzer,
  new_string_field, new_text_field, random,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::Rng;
use std::collections::HashMap;
use std::sync::Arc;

#[allow(dead_code)]
struct TestTermVectorsWriter;

#[test]
fn test_double_offset_counting() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut field_types = HashMap::new();
  let mut doc = Document::new();

  let mut custom_type =
    FieldType::from_ref(&*crate::core::document::string_field::TYPE_NOT_STORED)?;
  custom_type.set_store_term_vectors(true)?;
  custom_type.set_store_term_vector_positions(true)?;
  custom_type.set_store_term_vector_offsets(true)?;

  let f = new_field(&mut random, "field", "abcd", &custom_type, &mut field_types)?;
  doc.add(f.clone());
  doc.add(f.clone());

  let f2 = new_field(&mut random, "field", "", &custom_type, &mut field_types)?;
  doc.add(f2);

  doc.add(f);

  w.add_document(doc)?;
  w.close()?;

  let reader = directory_reader::open(dir.clone())?;

  let mut tv_reader = reader.term_vectors()?;
  let field0 = tv_reader.get(0)?;
  let tv = field0.as_ref().unwrap();
  let terms = tv.terms("field")?;
  let terms = terms.as_ref().unwrap();
  let mut terms_enum = terms.iterator()?;

  assert!(terms_enum.next()?.is_some());
  assert_eq!("", terms_enum.term()?.utf8_to_string()?);

  assert_eq!(1, terms_enum.total_term_freq()?);

  let mut dp_enum = terms_enum.postings_with_flags(None, ALL as i32)?;
  assert_ne!(dp_enum.next_doc()?, NO_MORE_DOCS);

  dp_enum.next_position()?;
  assert_eq!(8, dp_enum.start_offset()?);
  assert_eq!(8, dp_enum.end_offset()?);

  assert_eq!(NO_MORE_DOCS, dp_enum.next_doc()?);

  let next = terms_enum.next()?;
  assert!(next.is_some());
  assert_eq!(&BytesRef::from_string("abcd"), next.unwrap().as_ref());

  let mut dp_enum = terms_enum.postings_with_flags(Some(dp_enum), ALL as i32)?;
  assert_eq!(3, terms_enum.total_term_freq()?);

  assert_ne!(dp_enum.next_doc()?, NO_MORE_DOCS);

  dp_enum.next_position()?;
  assert_eq!(0, dp_enum.start_offset()?);
  assert_eq!(4, dp_enum.end_offset()?);

  dp_enum.next_position()?;
  assert_eq!(4, dp_enum.start_offset()?);
  assert_eq!(8, dp_enum.end_offset()?);

  dp_enum.next_position()?;
  assert_eq!(8, dp_enum.start_offset()?);
  assert_eq!(12, dp_enum.end_offset()?);

  assert_eq!(NO_MORE_DOCS, dp_enum.next_doc()?);

  assert!(terms_enum.next()?.is_none());

  Ok(())
}

#[test]
fn test_double_offset_counting2() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut field_types = HashMap::new();
  let mut doc = Document::new();

  let mut custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  custom_type.set_store_term_vectors(true)?;
  custom_type.set_store_term_vector_positions(true)?;
  custom_type.set_store_term_vector_offsets(true)?;

  let f = new_field(&mut random, "field", "abcd", &custom_type, &mut field_types)?;
  doc.add(f.clone());
  doc.add(f);

  w.add_document(doc)?;
  w.close()?;

  let reader = directory_reader::open(dir.clone())?;

  let mut tv_reader = reader.term_vectors()?;
  let field = tv_reader.get(0)?;
  let tv = field.as_ref().unwrap();
  let terms = tv.terms("field")?;
  let terms = terms.as_ref().unwrap();
  let mut terms_enum = terms.iterator()?;

  assert!(terms_enum.next()?.is_some());

  let mut dp_enum = terms_enum.postings_with_flags(None, ALL as i32)?;

  assert_eq!(2, terms_enum.total_term_freq()?);

  assert_ne!(dp_enum.next_doc()?, NO_MORE_DOCS);

  dp_enum.next_position()?;
  assert_eq!(0, dp_enum.start_offset()?);
  assert_eq!(4, dp_enum.end_offset()?);

  dp_enum.next_position()?;
  assert_eq!(5, dp_enum.start_offset()?);
  assert_eq!(9, dp_enum.end_offset()?);

  assert_eq!(NO_MORE_DOCS, dp_enum.next_doc()?);

  Ok(())
}

#[test]
fn test_end_offset_position_char_analyzer() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut field_types = HashMap::new();
  let mut doc = Document::new();

  let mut custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  custom_type.set_store_term_vectors(true)?;
  custom_type.set_store_term_vector_positions(true)?;
  custom_type.set_store_term_vector_offsets(true)?;

  let f = new_field(
    &mut random,
    "field",
    "abcd   ",
    &custom_type,
    &mut field_types,
  )?;
  doc.add(f.clone());
  doc.add(f);

  w.add_document(doc)?;
  w.close()?;

  let reader = directory_reader::open(dir.clone())?;

  let mut tv_reader = reader.term_vectors()?;
  let field = tv_reader.get(0)?;
  let tv = field.as_ref().unwrap();
  let terms = tv.terms("field")?;
  let terms = terms.as_ref().unwrap();
  let mut terms_enum = terms.iterator()?;

  assert!(terms_enum.next()?.is_some());

  let mut dp_enum = terms_enum.postings_with_flags(None, ALL as i32)?;

  assert_eq!(2, terms_enum.total_term_freq()?);

  assert_ne!(dp_enum.next_doc()?, NO_MORE_DOCS);

  dp_enum.next_position()?;
  assert_eq!(0, dp_enum.start_offset()?);
  assert_eq!(4, dp_enum.end_offset()?);

  dp_enum.next_position()?;
  assert_eq!(8, dp_enum.start_offset()?);
  assert_eq!(12, dp_enum.end_offset()?);

  assert_eq!(NO_MORE_DOCS, dp_enum.next_doc()?);

  Ok(())
}
#[test]
fn test_end_offset_position_with_caching_token_filter() -> Result<()> {
  // TODO: CachingTokenFilter is not implemented, so the Java token-stream reuse path cannot be
  // represented faithfully.
  Ok(())
}
#[test]
fn test_end_offset_position_stop_filter() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut field_types = HashMap::new();
  let mut doc = Document::new();

  let mut custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  custom_type.set_store_term_vectors(true)?;
  custom_type.set_store_term_vector_positions(true)?;
  custom_type.set_store_term_vector_offsets(true)?;

  let f = new_field(
    &mut random,
    "field",
    "abcd the",
    &custom_type,
    &mut field_types,
  )?;
  doc.add(f.clone());
  doc.add(f);

  w.add_document(doc)?;
  w.close()?;

  let reader = directory_reader::open(dir.clone())?;

  let mut tv_reader = reader.term_vectors()?;
  let field = tv_reader.get(0)?;
  let tv = field.as_ref().unwrap();
  let terms = tv.terms("field")?;
  let terms = terms.as_ref().unwrap();
  let mut terms_enum = terms.iterator()?;

  assert!(terms_enum.next()?.is_some());

  let mut dp_enum = terms_enum.postings_with_flags(None, ALL as i32)?;

  assert_eq!(2, terms_enum.total_term_freq()?);

  assert_ne!(dp_enum.next_doc()?, NO_MORE_DOCS);

  dp_enum.next_position()?;
  assert_eq!(0, dp_enum.start_offset()?);
  assert_eq!(4, dp_enum.end_offset()?);

  dp_enum.next_position()?;
  assert_eq!(9, dp_enum.start_offset()?);
  assert_eq!(13, dp_enum.end_offset()?);

  assert_eq!(NO_MORE_DOCS, dp_enum.next_doc()?);

  Ok(())
}
#[test]
fn test_end_offset_position_standard() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut field_types = HashMap::new();
  let mut doc = Document::new();

  let mut custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  custom_type.set_store_term_vectors(true)?;
  custom_type.set_store_term_vector_positions(true)?;
  custom_type.set_store_term_vector_offsets(true)?;

  let f = new_field(
    &mut random,
    "field",
    "abcd the  ",
    &custom_type,
    &mut field_types,
  )?;
  let f2 = new_field(
    &mut random,
    "field",
    "crunch man",
    &custom_type,
    &mut field_types,
  )?;
  doc.add(f);
  doc.add(f2);

  w.add_document(doc)?;
  w.close()?;

  let reader = directory_reader::open(dir.clone())?;

  let mut tv_reader = reader.term_vectors()?;
  let field = tv_reader.get(0)?;
  let tv = field.as_ref().unwrap();
  let terms = tv.terms("field")?;
  let terms = terms.as_ref().unwrap();

  let mut terms_enum = terms.iterator()?;

  assert!(terms_enum.next()?.is_some());
  let mut dp_enum = terms_enum.postings_with_flags(None, ALL as i32)?;

  assert_ne!(dp_enum.next_doc()?, NO_MORE_DOCS);
  dp_enum.next_position()?;
  assert_eq!(0, dp_enum.start_offset()?);
  assert_eq!(4, dp_enum.end_offset()?);

  assert!(terms_enum.next()?.is_some());
  dp_enum = terms_enum.postings_with_flags(Some(dp_enum), ALL as i32)?;
  assert_ne!(dp_enum.next_doc()?, NO_MORE_DOCS);
  dp_enum.next_position()?;
  assert_eq!(11, dp_enum.start_offset()?);
  assert_eq!(17, dp_enum.end_offset()?);

  assert!(terms_enum.next()?.is_some());
  dp_enum = terms_enum.postings_with_flags(Some(dp_enum), ALL as i32)?;
  assert_ne!(dp_enum.next_doc()?, NO_MORE_DOCS);
  dp_enum.next_position()?;
  assert_eq!(18, dp_enum.start_offset()?);
  assert_eq!(21, dp_enum.end_offset()?);

  Ok(())
}
#[test]
fn test_end_offset_position_standard_empty_field() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut field_types = HashMap::new();
  let mut doc = Document::new();

  let mut custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  custom_type.set_store_term_vectors(true)?;
  custom_type.set_store_term_vector_positions(true)?;
  custom_type.set_store_term_vector_offsets(true)?;

  let f = new_field(&mut random, "field", "", &custom_type, &mut field_types)?;
  let f2 = new_field(
    &mut random,
    "field",
    "crunch man",
    &custom_type,
    &mut field_types,
  )?;
  doc.add(f);
  doc.add(f2);

  w.add_document(doc)?;
  w.close()?;

  let reader = directory_reader::open(dir.clone())?;

  let mut tv_reader = reader.term_vectors()?;
  let field = tv_reader.get(0)?;
  let tv = field.as_ref().unwrap();
  let terms = tv.terms("field")?;
  let terms = terms.as_ref().unwrap();

  let mut terms_enum = terms.iterator()?;

  assert!(terms_enum.next()?.is_some());
  let mut dp_enum = terms_enum.postings_with_flags(None, ALL as i32)?;

  assert_eq!(1, terms_enum.total_term_freq()? as i32);

  assert_ne!(dp_enum.next_doc()?, NO_MORE_DOCS);
  dp_enum.next_position()?;
  assert_eq!(1, dp_enum.start_offset()?);
  assert_eq!(7, dp_enum.end_offset()?);

  assert!(terms_enum.next()?.is_some());
  dp_enum = terms_enum.postings_with_flags(Some(dp_enum), ALL as i32)?;

  assert_ne!(dp_enum.next_doc()?, NO_MORE_DOCS);
  dp_enum.next_position()?;
  assert_eq!(8, dp_enum.start_offset()?);
  assert_eq!(11, dp_enum.end_offset()?);

  Ok(())
}
#[test]
fn test_end_offset_position_standard_empty_field2() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut field_types = HashMap::new();
  let mut doc = Document::new();

  let mut custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  custom_type.set_store_term_vectors(true)?;
  custom_type.set_store_term_vector_positions(true)?;
  custom_type.set_store_term_vector_offsets(true)?;

  let f = new_field(&mut random, "field", "abcd", &custom_type, &mut field_types)?;
  doc.add(f);

  doc.add(new_field(
    &mut random,
    "field",
    "",
    &custom_type,
    &mut field_types,
  )?);

  let f2 = new_field(
    &mut random,
    "field",
    "crunch",
    &custom_type,
    &mut field_types,
  )?;
  doc.add(f2);

  w.add_document(doc)?;
  w.close()?;

  let reader = directory_reader::open(dir.clone())?;

  let mut tv_reader = reader.term_vectors()?;
  let field = tv_reader.get(0)?;
  let tv = field.as_ref().unwrap();
  let terms = tv.terms("field")?;
  let terms = terms.as_ref().unwrap();

  let mut terms_enum = terms.iterator()?;

  assert!(terms_enum.next()?.is_some());

  let mut dp_enum = terms_enum.postings_with_flags(None, ALL as i32)?;

  assert_eq!(1, terms_enum.total_term_freq()? as i32);

  assert_ne!(dp_enum.next_doc()?, NO_MORE_DOCS);

  dp_enum.next_position()?;
  assert_eq!(0, dp_enum.start_offset()?);
  assert_eq!(4, dp_enum.end_offset()?);

  assert!(terms_enum.next()?.is_some());

  dp_enum = terms_enum.postings_with_flags(Some(dp_enum), ALL as i32)?;

  assert_ne!(dp_enum.next_doc()?, NO_MORE_DOCS);

  dp_enum.next_position()?;
  assert_eq!(6, dp_enum.start_offset()?);
  assert_eq!(12, dp_enum.end_offset()?);

  Ok(())
}
#[test]
fn test_term_vector_corruption() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  for _iter in 0..2 {
    let a = MockAnalyzer::new(&mut random);
    let mut iwc = new_index_writer_config_with_analyzer(&mut random, a)?;
    iwc.set_max_buffered_docs(2);
    iwc.set_ram_buffer_size_mb(DISABLE_AUTO_FLUSH as f64);
    iwc.set_merge_scheduler(SerialMergeScheduler::new());
    iwc.set_merge_policy(LogMergePolicy::log_doc());
    let writer = IndexWriter::new(dir.clone(), iwc)?;

    let mut document = Document::new();
    let mut custom_type = FieldType::new();
    custom_type.set_stored(true)?;
    let stored_field = new_field(
      &mut random,
      "stored",
      "stored",
      &custom_type,
      &mut HashMap::new(),
    )?;
    document.add(stored_field.clone());
    writer.add_document(document.clone())?;
    writer.add_document(document)?;

    let mut document = Document::new();
    document.add(stored_field);
    let mut custom_type2 =
      FieldType::from_ref(&*crate::core::document::string_field::TYPE_NOT_STORED)?;
    custom_type2.set_store_term_vectors(true)?;
    custom_type2.set_store_term_vector_positions(true)?;
    custom_type2.set_store_term_vector_offsets(true)?;
    let term_vector_field = new_field(
      &mut random,
      "termVector",
      "termVector",
      &custom_type2,
      &mut HashMap::new(),
    )?;
    document.add(term_vector_field);
    writer.add_document(document)?;
    writer.force_merge(1)?;
    writer.close()?;
    drop(writer);

    let reader = directory_reader::open(dir.clone())?;
    let mut stored_fields = reader.stored_fields()?;
    let mut term_vectors = reader.term_vectors()?;
    for i in 0..reader.num_docs()? {
      stored_fields.document(i)?;
      term_vectors.get(i)?;
    }
    reader.close()?;

    let a = MockAnalyzer::new(&mut random);
    let mut iwc = new_index_writer_config_with_analyzer(&mut random, a)?;
    iwc.set_max_buffered_docs(2);
    iwc.set_ram_buffer_size_mb(DISABLE_AUTO_FLUSH as f64);
    iwc.set_merge_scheduler(SerialMergeScheduler::new());
    iwc.set_merge_policy(LogMergePolicy::log_doc());
    let writer = IndexWriter::new(dir.clone(), iwc)?;

    let index_dirs = vec![TestUtil::ram_copy_of(&mut random, dir.as_ref())?];
    writer.add_indexes_from_directory(&index_dirs)?;
    writer.force_merge(1)?;
    writer.close()?;
  }

  Ok(())
}
#[test]
fn test_term_vector_corruption2() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mut field_types = HashMap::new();

  for _ in 0..2 {
    let mock = MockAnalyzer::new(&mut random);
    let mut iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
    iwc.set_max_buffered_docs(2);
    iwc.set_ram_buffer_size_mb(DISABLE_AUTO_FLUSH as f64);
    iwc.set_merge_scheduler(SerialMergeScheduler::new());
    iwc.set_merge_policy(LogMergePolicy::log_doc());

    let writer = IndexWriter::new(dir.clone(), iwc)?;

    let mut document = Document::new();

    let mut custom_type = FieldType::new();
    custom_type.set_stored(true)?;

    let stored_field = new_field(
      &mut random,
      "stored",
      "stored",
      &custom_type,
      &mut field_types,
    )?;
    document.add(stored_field.clone());

    writer.add_document(document.clone())?;
    writer.add_document(document)?;

    let mut document = Document::new();
    document.add(stored_field);

    let mut custom_type2 =
      FieldType::from_ref(&*crate::core::document::string_field::TYPE_NOT_STORED)?;
    custom_type2.set_store_term_vectors(true)?;
    custom_type2.set_store_term_vector_positions(true)?;
    custom_type2.set_store_term_vector_offsets(true)?;

    let term_vector_field = new_field(
      &mut random,
      "termVector",
      "termVector",
      &custom_type2,
      &mut field_types,
    )?;
    document.add(term_vector_field);

    writer.add_document(document)?;
    writer.force_merge(1)?;
    writer.close()?;

    let reader = directory_reader::open(dir.clone())?;
    let mut tv_reader = reader.term_vectors()?;

    assert!(tv_reader.get(0)?.is_none());
    assert!(tv_reader.get(1)?.is_none());
    assert!(tv_reader.get(2)?.is_some());
  }

  Ok(())
}
#[test]
fn test_term_vector_corruption3() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut iwc1 = new_index_writer_config_with_analyzer(&mut random, mock)?;
  iwc1.set_max_buffered_docs(2);
  iwc1.set_ram_buffer_size_mb(DISABLE_AUTO_FLUSH as f64);
  iwc1.set_merge_scheduler(SerialMergeScheduler::new());
  iwc1.set_merge_policy(LogMergePolicy::log_doc());
  let mut document = Document::new();
  let mut field_types = HashMap::new();
  {
    let writer = IndexWriter::new(dir.clone(), iwc1)?;

    let mut custom_type = FieldType::new();
    custom_type.set_stored(true)?;
    let stored_field = new_field(
      &mut random,
      "stored",
      "stored",
      &custom_type,
      &mut field_types,
    )?;
    document.add(stored_field.clone());

    let mut custom_type2 =
      FieldType::from_ref(&*crate::core::document::string_field::TYPE_NOT_STORED)?;
    custom_type2.set_store_term_vectors(true)?;
    custom_type2.set_store_term_vector_positions(true)?;
    custom_type2.set_store_term_vector_offsets(true)?;
    let term_vector_field = new_field(
      &mut random,
      "termVector",
      "termVector",
      &custom_type2,
      &mut field_types,
    )?;
    document.add(term_vector_field.clone());

    for _ in 0..10 {
      writer.add_document(document.clone())?;
    }
    writer.close()?;
  }

  let mock = MockAnalyzer::new(&mut random);
  let mut iwc2 = new_index_writer_config_with_analyzer(&mut random, mock)?;
  iwc2.set_max_buffered_docs(2);
  iwc2.set_ram_buffer_size_mb(DISABLE_AUTO_FLUSH as f64);
  iwc2.set_merge_scheduler(SerialMergeScheduler::new());
  iwc2.set_merge_policy(LogMergePolicy::log_doc());

  let writer = IndexWriter::new(dir.clone(), iwc2)?;

  for _ in 0..6 {
    writer.add_document(document.clone())?;
  }
  writer.force_merge(1)?;
  writer.close()?;

  let reader = directory_reader::open(dir.clone())?;

  let mut stored_fields = reader.stored_fields()?;
  let mut term_vectors = reader.term_vectors()?;

  for i in 0..10 {
    term_vectors.get(i)?;
    stored_fields.document(i)?;
  }

  Ok(())
}
#[test]
fn test_no_term_vector_after_term_vector() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;

  let iwc = new_index_writer_config(&mut random)?;
  let iw = IndexWriter::new(dir.clone(), iwc)?;

  let mut field_types = HashMap::new();
  let mut custom_type2 = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  custom_type2.set_store_term_vectors(true)?;
  custom_type2.set_store_term_vector_positions(true)?;
  custom_type2.set_store_term_vector_offsets(true)?;

  let mut document = Document::new();
  document.add(new_field(
    &mut random,
    "tvtest",
    "a b c",
    &custom_type2,
    &mut field_types,
  )?);
  iw.add_document(document)?;

  let mut document = Document::new();
  document.add(new_text_field(
    &mut random,
    "tvtest",
    "x y z",
    Store::No,
    &mut field_types,
  )?);
  iw.add_document(document)?;

  iw.commit()?;

  let mut custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  custom_type.set_store_term_vectors(true)?;

  let mut document = Document::new();
  document.add(new_field(
    &mut random,
    "tvtest",
    "a b c",
    &custom_type,
    &mut field_types,
  )?);
  iw.add_document(document)?;

  iw.commit()?;

  iw.force_merge(1)?;
  iw.close()?;

  Ok(())
}
#[test]
fn test_no_term_vector_after_term_vector_merge() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let iw = IndexWriter::new(dir.clone(), iwc)?;
  let mut field_types = HashMap::new();
  let mut document = Document::new();
  let mut custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  custom_type.set_store_term_vectors(true)?;
  document.add(new_field(
    &mut random,
    "tvtest",
    "a b c",
    &custom_type,
    &mut field_types,
  )?);
  iw.add_document(document)?;
  iw.commit()?;

  let mut document = Document::new();
  document.add(new_text_field(
    &mut random,
    "tvtest",
    "x y z",
    Store::No,
    &mut field_types,
  )?);
  iw.add_document(document)?;
  // Make first segment
  iw.commit()?;

  iw.force_merge(1)?;

  let mut custom_type2 = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  custom_type2.set_store_term_vectors(true)?;

  let mut document = Document::new();
  document.add(new_field(
    &mut random,
    "tvtest",
    "a b c",
    &custom_type2,
    &mut field_types,
  )?);
  iw.add_document(document)?;
  // Make 2nd segment
  iw.commit()?;
  iw.force_merge(1)?;
  iw.close()?;
  Ok(())
}
#[test]
fn test_inconsistent_term_vector_options() -> Result<()> {
  let mut random = random();
  let mut a;
  let mut b;

  // no vectors + vectors
  a = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  b = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  b.set_store_term_vectors(true)?;
  do_test_mixup(&mut random, a, b)?;

  // vectors + vectors with pos
  a = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  a.set_store_term_vectors(true)?;
  b = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  b.set_store_term_vectors(true)?;
  b.set_store_term_vector_positions(true)?;
  do_test_mixup(&mut random, a, b)?;

  // vectors + vectors with off
  a = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  a.set_store_term_vectors(true)?;
  b = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  b.set_store_term_vectors(true)?;
  b.set_store_term_vector_offsets(true)?;
  do_test_mixup(&mut random, a, b)?;

  // vectors with pos + vectors with pos + off
  a = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  a.set_store_term_vectors(true)?;
  a.set_store_term_vector_positions(true)?;
  b = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  b.set_store_term_vectors(true)?;
  b.set_store_term_vector_positions(true)?;
  b.set_store_term_vector_offsets(true)?;
  do_test_mixup(&mut random, a, b)?;

  // vectors with pos + vectors with pos + pay
  a = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  a.set_store_term_vectors(true)?;
  a.set_store_term_vector_positions(true)?;
  b = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  b.set_store_term_vectors(true)?;
  b.set_store_term_vector_positions(true)?;
  b.set_store_term_vector_payloads(true)?;
  do_test_mixup(&mut random, a, b)?;
  Ok(())
}

fn do_test_mixup<R>(random: &mut R, ft1: FieldType, ft2: FieldType) -> Result<()>
where
  R: Rng + ?Sized,
{
  let dir = new_directory_shared(random)?;
  let iw = RandomIndexWriter::new(random, dir.clone())?;

  let mut field_types = HashMap::new();
  for i in 0..3 {
    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      i.to_string(),
      Store::No,
      &mut field_types,
    )?);
    iw.add_document(random, doc)?;
  }

  let mut doc = Document::new();
  doc.add(Field::new("field", "value1", ft1.clone()));
  doc.add(Field::new("field", "value1", ft2.clone()));

  // ensure broken doc hits error
  let err = iw.add_document(random, doc).unwrap_err();
  match err {
    LuceneError::IllegalArgument(msg) => {
      assert!(
        msg.to_string().starts_with(
          "all instances of a given field name must have the same term vectors settings"
        ) || msg
          .to_string()
          .starts_with("Inconsistency of field data structures across documents for field [field]")
      );
    },
    _ => unreachable!("unexpected error type: {:?}", err),
  }
  let ir = iw.get_reader(random)?;
  assert_eq!(3, ir.num_docs()?);
  iw.close(random)?;
  Ok(())
}
#[test]
fn test_no_abort_on_bad_tv_settings() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  // Don't use RandomIndexWriter because we want to be sure both docs go to 1 seg:
  let mock = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(mock)?;
  let iw = IndexWriter::new(dir.clone(), iwc)?;
  let mut doc = Document::new();
  iw.add_document(doc.clone())?;

  let mut ft = FieldType::from_ref(&*stored_field_type::TYPE)?;
  ft.set_store_term_vectors(true)?;
  ft.freeze();

  doc.add(Field::from_string("field", "value", ft)?);

  let err = iw.add_document(doc.clone()).unwrap_err();
  match err {
    LuceneError::IllegalArgument(_) => {},
    _ => unreachable!("unexpected error: {:?}", err),
  }

  let reader = directory_reader::open_from_writer(&iw)?;
  // Make sure the exc didn't lose our first document:
  assert_eq!(1, reader.num_docs()?);

  iw.close()?;
  Ok(())
}
