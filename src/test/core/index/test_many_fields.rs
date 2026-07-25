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
use crate::core::document::field_type::FieldType;
use crate::core::index::directory_reader;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::term::Term;
use crate::core::search::term_query::TermQuery;
use crate::core::store::directory::DirEnum;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, create_temp_dir, new_directory_shared, new_field, new_fs_directory,
  new_index_writer_config, new_index_writer_config_with_analyzer, new_searcher_with_reader, random,
};
use rand::RngExt;
use std::collections::HashMap;

#[allow(dead_code)] // for quick search
struct TestManyFields;
#[test]
fn test_many_fields() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  iwc.set_max_buffered_docs(10);

  let writer = IndexWriter::new(dir.clone(), iwc)?;
  let mut field_types = HashMap::new();

  let mut stored_text_type =
    FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  stored_text_type.freeze();
  for j in 0..100 {
    let mut doc = Document::new();

    doc.add(new_field(
      &mut random,
      format!("a{}", j),
      format!("aaa{}", j),
      &stored_text_type,
      &mut field_types,
    )?);
    doc.add(new_field(
      &mut random,
      format!("b{}", j),
      format!("aaa{}", j),
      &stored_text_type,
      &mut field_types,
    )?);
    doc.add(new_field(
      &mut random,
      format!("c{}", j),
      format!("aaa{}", j),
      &stored_text_type,
      &mut field_types,
    )?);
    doc.add(new_field(
      &mut random,
      format!("d{}", j),
      "aaa",
      &stored_text_type,
      &mut field_types,
    )?);
    doc.add(new_field(
      &mut random,
      format!("e{}", j),
      "aaa",
      &stored_text_type,
      &mut field_types,
    )?);
    doc.add(new_field(
      &mut random,
      format!("f{}", j),
      "aaa",
      &stored_text_type,
      &mut field_types,
    )?);
    writer.add_document(doc)?;
  }

  writer.close()?;

  let reader = directory_reader::open(dir.clone())?;
  assert_eq!(100, reader.max_doc()?);
  assert_eq!(100, reader.num_docs()?);
  for j in 0..100 {
    assert_eq!(
      1,
      reader.doc_freq(&Term::from_text(format!("a{}", j), format!("aaa{}", j)))?
    );
    assert_eq!(
      1,
      reader.doc_freq(&Term::from_text(format!("b{}", j), format!("aaa{}", j)))?
    );
    assert_eq!(
      1,
      reader.doc_freq(&Term::from_text(format!("c{}", j), format!("aaa{}", j)))?
    );
    assert_eq!(
      1,
      reader.doc_freq(&Term::from_text(format!("d{}", j), "aaa"))?
    );
    assert_eq!(
      1,
      reader.doc_freq(&Term::from_text(format!("e{}", j), "aaa"))?
    );
    assert_eq!(
      1,
      reader.doc_freq(&Term::from_text(format!("f{}", j), "aaa"))?
    );
  }

  Ok(())
}
#[test]
fn test_diverse_docs() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let _iwc = new_index_writer_config_with_analyzer::<DirEnum, _, _>(&mut random, mock)?;
  let mut iwc = new_index_writer_config(&mut random)?;
  iwc.set_ram_buffer_size_mb(0.5);
  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let mut field_types = HashMap::new();
  let mut stored_text_type =
    FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  stored_text_type.freeze();

  let n = at_least(&mut random, 1);

  for _ in 0..n {
    for _j in 0..100 {
      // First, docs where every term is unique (heavy on
      // Posting instances)
      let mut doc = Document::new();
      for _k in 0..100 {
        let v = random.random::<i32>().to_string();
        doc.add(new_field(
          &mut random,
          "field",
          v,
          &stored_text_type,
          &mut field_types,
        )?);
      }
      writer.add_document(doc)?;
    }
    // Next, many single term docs where only one term
    // occurs (heavy on byte blocks)
    for _j in 0..100 {
      let mut doc = Document::new();
      doc.add(new_field(
        &mut random,
        "field",
        "aaa aaa aaa aaa aaa aaa aaa aaa aaa aaa",
        &stored_text_type,
        &mut field_types,
      )?);
      writer.add_document(doc)?;
    }
    // Next, many single term docs where only one term
    // occurs but the terms are very long (heavy on
    // char[] arrays)
    for j in 0..100 {
      let x = format!("{}.", j);
      let long_term = x.repeat(1000);
      let mut doc = Document::new();
      doc.add(new_field(
        &mut random,
        "field",
        long_term,
        &stored_text_type,
        &mut field_types,
      )?);
      writer.add_document(doc)?;
    }
  }

  writer.close()?;

  let reader = directory_reader::open(dir.clone())?;
  let searcher = new_searcher_with_reader(reader)?;
  let total_hits = searcher.count(TermQuery::new(Term::from_text("field", "aaa")))?;
  assert_eq!(n * 100, total_hits);

  Ok(())
}
#[test]
fn test_rotating_field_names() -> Result<()> {
  let mut random = random();
  let dir = new_fs_directory(&mut random, create_temp_dir()?)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(mock)?;
  iwc.set_ram_buffer_size_mb(0.2);
  iwc.set_max_buffered_docs(-1);

  let writer = IndexWriter::new(dir.clone(), iwc)?;
  let mut field_types = HashMap::new();

  let mut upto: i32 = 0;

  let mut ft = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  ft.set_omit_norms(true)?;

  let mut first_doc_count: i32 = -1;

  for iter in 0..10 {
    let start_flush_count = writer.get_flush_count();

    let mut doc_count = 0;

    while writer.get_flush_count() == start_flush_count {
      let mut doc = Document::new();
      for _ in 0..10 {
        let field_name = format!("field{}", upto);
        upto += 1;
        doc.add(new_field(
          &mut random,
          field_name,
          "content",
          &ft,
          &mut field_types,
        )?);
      }

      writer.add_document(doc)?;
      doc_count += 1;
    }

    if iter == 0 {
      first_doc_count = doc_count;
    }

    let ratio = (doc_count as f32) / (first_doc_count as f32);
    assert!(
      ratio > 0.9,
      "flushed after too few docs: first segment flushed at docCount={}, \
current segment flushed after docCount={}, iter={} (ratio={})",
      first_doc_count,
      doc_count,
      iter,
      ratio,
    );

    if upto > 5000 {
      upto = 0;
    }
  }

  writer.close()?;
  Ok(())
}
