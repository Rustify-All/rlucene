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
use crate::core::document::field::Store;
use crate::core::document::keyword_field::KeywordField;
use crate::core::index::BytesRef;
use crate::core::index::directory_reader;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::index::term::Term;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::per_thread_pk_lookup::PerThreadPKLookup;
use crate::test_framework::core::util::lucene_test_case::{
  new_directory_shared, new_index_writer_config_with_analyzer, random,
};
#[allow(dead_code)] // for quick search
pub struct TestPerThreadPKLookup;

#[test]
fn test_reopen() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut config = IndexWriterConfig::with_analyzer(analyzer)?;
  config.set_merge_policy(NoMergePolicy::default());
  let writer = IndexWriter::new(dir.clone(), config)?;

  let mut doc;
  doc = Document::new();
  doc.add(KeywordField::from_string("PK", "1", Store::No)?);
  writer.add_document(doc)?;

  doc = Document::new();
  doc.add(KeywordField::from_string("PK", "2", Store::No)?);
  writer.add_document(doc)?;
  writer.flush()?;

  // Terms in PK is None.
  doc = Document::new();
  doc.add(KeywordField::from_string("PK2", "3", Store::No)?);
  writer.add_document(doc)?;

  doc = Document::new();
  doc.add(KeywordField::from_string("PK2", "4", Store::No)?);
  writer.add_document(doc)?;
  writer.flush()?;

  let reader1 = directory_reader::open_from_writer(&writer)?;
  let context1 = (&reader1).get_context()?;
  let mut pk_lookup1 = PerThreadPKLookup::new(&context1, "PK")?;

  doc = Document::new();
  doc.add(KeywordField::from_string("PK", "5", Store::No)?);
  writer.add_document(doc)?;

  doc = Document::new();
  doc.add(KeywordField::from_string("PK", "6", Store::No)?);
  writer.add_document(doc)?;
  writer.delete_documents_with_terms(vec![Term::from_text("PK", "1")])?;
  writer.flush()?;

  // Terms in PK is None.
  doc = Document::new();
  doc.add(KeywordField::from_string("PK2", "7", Store::No)?);
  writer.add_document(doc)?;

  doc = Document::new();
  doc.add(KeywordField::from_string("PK2", "8", Store::No)?);
  writer.add_document(doc)?;
  writer.flush()?;

  assert_eq!(0, pk_lookup1.lookup(&BytesRef::from_string("1"))?);
  assert_eq!(1, pk_lookup1.lookup(&BytesRef::from_string("2"))?);
  assert_eq!(-1, pk_lookup1.lookup(&BytesRef::from_string("5"))?);
  assert_eq!(-1, pk_lookup1.lookup(&BytesRef::from_string("8"))?);
  let reader2 = directory_reader::open_if_changed(&reader1)?.unwrap();
  let context2 = (&reader2).get_context()?;
  let mut pk_lookup2 = pk_lookup1.reopen(Some(&context2))?.unwrap();

  assert_eq!(-1, pk_lookup2.lookup(&BytesRef::from_string("1"))?);
  assert_eq!(1, pk_lookup2.lookup(&BytesRef::from_string("2"))?);
  assert_eq!(4, pk_lookup2.lookup(&BytesRef::from_string("5"))?);
  assert_eq!(-1, pk_lookup2.lookup(&BytesRef::from_string("8"))?);

  doc = Document::new();
  doc.add(KeywordField::from_string("PK", "9", Store::No)?);
  writer.add_document(doc)?;

  doc = Document::new();
  doc.add(KeywordField::from_string("PK", "10", Store::No)?);
  writer.add_document(doc)?;
  writer.flush()?;

  assert_eq!(-1, pk_lookup2.lookup(&BytesRef::from_string("9"))?);
  let reader3 = directory_reader::open_if_changed(&reader2)?.unwrap();
  let context3 = (&reader3).get_context()?;
  let mut pk_lookup3 = pk_lookup2.reopen(Some(&context3))?.unwrap();
  assert_eq!(8, pk_lookup3.lookup(&BytesRef::from_string("9"))?);
  let reader4 = directory_reader::open_if_changed(&reader3)?;
  assert!(reader4.is_none());
  writer.close()?;
  reader1.close()?;
  reader2.close()?;
  reader3.close()?;
  Ok(())
}

#[test]
fn test_pk_lookup_with_update() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut config = IndexWriterConfig::with_analyzer(analyzer)?;
  config.set_merge_policy(NoMergePolicy::default());
  let writer = IndexWriter::new(dir.clone(), config)?;

  let mut doc;
  doc = Document::new();
  doc.add(KeywordField::from_string("PK", "1", Store::No)?);
  doc.add(KeywordField::from_string("version", "1", Store::No)?);
  writer.add_document(doc)?;

  doc = Document::new();
  doc.add(KeywordField::from_string("PK", "1", Store::No)?);
  doc.add(KeywordField::from_string("version", "2", Store::No)?);
  writer.update_document_with_term(Term::from_text("PK", "1"), doc)?;

  doc = Document::new();
  doc.add(KeywordField::from_string("PK", "1", Store::No)?);
  doc.add(KeywordField::from_string("version", "3", Store::No)?);
  writer.update_document_with_term(Term::from_text("PK", "1"), doc)?;
  writer.flush()?;
  writer.close()?;

  let reader = directory_reader::open(dir)?;
  let context = reader.get_context()?;
  let mut pk = PerThreadPKLookup::new(&context, "PK")?;

  let doc_id = pk.lookup(&BytesRef::from_string("1"))?;
  assert_eq!(2, doc_id);

  context.reader().close()?;
  Ok(())
}
