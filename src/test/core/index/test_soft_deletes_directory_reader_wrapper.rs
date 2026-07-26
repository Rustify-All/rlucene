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
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::string_field::StringField;
use crate::core::index::codec_reader::{CodecReader, CodecReaderEnum2};
use crate::core::index::composite_reader::CompositeReader;
use crate::core::index::directory_reader;
use crate::core::index::index_reader::{CacheHelper, CacheKey, IndexReader};
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::index::soft_deletes_directory_reader_wrapper::{
  SoftDeletesCodecReader, SoftDeletesDirectoryReaderWrapper,
};
use crate::core::index::soft_deletes_retention_merge_policy::SoftDeletesRetentionMergePolicy;
use crate::core::index::term::Term;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::search::index_searcher;
use crate::core::search::match_all_docs_query::MatchAllDocsQuery;
use crate::core::search::match_no_docs_query::MatchNoDocsQuery;
use crate::core::search::term_query::TermQuery;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::io_utils::IOUtils;
use crate::test_framework::core::util::lucene_test_case::{
  new_directory_shared, new_index_writer_config, random,
};
use rand::RngExt;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

#[allow(dead_code)] // for quick search
struct TestSoftDeletesDirectoryReaderWrapper;

#[test]
fn test_drop_fully_deleted_segments() -> Result<()> {
  let mut random = random();
  let mut index_writer_config = new_index_writer_config(&mut random)?;
  let soft_deletes_field = "soft_delete";
  index_writer_config
    .set_soft_deletes_field(soft_deletes_field)
    .set_merge_policy(SoftDeletesRetentionMergePolicy::new(
      soft_deletes_field,
      || Ok(MatchAllDocsQuery::new().into()),
      NoMergePolicy::default(),
    ));
  let dir = new_directory_shared(&mut random)?;
  let writer = IndexWriter::new(dir.clone(), index_writer_config)?;

  let body_result = (|| -> Result<()> {
    let mut doc = Document::new();
    doc.add(StringField::from_string("id", "1", Store::Yes)?);
    doc.add(StringField::from_string("version", "1", Store::Yes)?);
    writer.add_document(doc)?;
    writer.commit()?;
    let mut doc = Document::new();
    doc.add(StringField::from_string("id", "2", Store::Yes)?);
    doc.add(StringField::from_string("version", "1", Store::Yes)?);
    writer.add_document(doc)?;
    writer.commit()?;

    let reader = SoftDeletesDirectoryReaderWrapper::new(
      directory_reader::open(dir.clone())?,
      soft_deletes_field,
    )?;
    assert_eq!(2, (&reader).get_context()?.leaves()?.len());
    assert_eq!(2, reader.num_docs()?);
    assert_eq!(2, reader.max_doc()?);
    assert_eq!(0, reader.num_deleted_docs()?);
    reader.close()?;

    writer.update_doc_values(
      Term::from_text("id", "1"),
      vec![NumericDocValuesField::new(soft_deletes_field, 1).into()],
    )?;
    writer.commit()?;
    let reader = SoftDeletesDirectoryReaderWrapper::new(
      directory_reader::open_from_writer(&writer)?,
      soft_deletes_field,
    )?;
    assert_eq!(1, reader.num_docs()?);
    assert_eq!(1, reader.max_doc()?);
    assert_eq!(0, reader.num_deleted_docs()?);
    assert_eq!(1, (&reader).get_context()?.leaves()?.len());
    reader.close()?;

    let reader = SoftDeletesDirectoryReaderWrapper::new(
      directory_reader::open(dir.clone())?,
      soft_deletes_field,
    )?;
    assert_eq!(1, reader.num_docs()?);
    assert_eq!(1, reader.max_doc()?);
    assert_eq!(0, reader.num_deleted_docs()?);
    assert_eq!(1, (&reader).get_context()?.leaves()?.len());
    reader.close()?;

    let reader = directory_reader::open(dir.clone())?;
    assert_eq!(2, reader.num_docs()?);
    assert_eq!(2, reader.max_doc()?);
    assert_eq!(0, reader.num_deleted_docs()?);
    assert_eq!(2, (&reader).get_context()?.leaves()?.len());
    reader.close()
  })();

  let close_result = IOUtils::use_or_suppress_result(writer.close(), dir.close());
  IOUtils::use_or_suppress_result(body_result, close_result)
}

#[test]
fn test_reuse_unchanged_leaf_reader() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut index_writer_config = new_index_writer_config(&mut random)?;
  let soft_deletes_field = "soft_delete";
  index_writer_config
    .set_soft_deletes_field(soft_deletes_field)
    .set_merge_policy(NoMergePolicy::default());
  let writer = IndexWriter::new(dir.clone(), index_writer_config)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "1", Store::Yes)?);
  doc.add(StringField::from_string("version", "1", Store::Yes)?);
  writer.add_document(doc)?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "2", Store::Yes)?);
  doc.add(StringField::from_string("version", "1", Store::Yes)?);
  writer.add_document(doc)?;
  writer.commit()?;
  let mut reader = SoftDeletesDirectoryReaderWrapper::new(
    directory_reader::open(dir.clone())?,
    soft_deletes_field,
  )?;
  assert_eq!(2, reader.num_docs()?);
  assert_eq!(2, reader.max_doc()?);
  assert_eq!(0, reader.num_deleted_docs()?);

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "1", Store::Yes)?);
  doc.add(StringField::from_string("version", "2", Store::Yes)?);
  writer.soft_update_document(
    Term::from_text("id", "1"),
    doc,
    vec![NumericDocValuesField::new("soft_delete", 1).into()],
  )?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "3", Store::Yes)?);
  doc.add(StringField::from_string("version", "1", Store::Yes)?);
  writer.add_document(doc)?;
  writer.commit()?;

  let new_reader = directory_reader::open_if_changed(&reader)?.expect("reader should change");
  assert_ne!(
    new_reader
      .get_reader_cache_helper()?
      .expect("reader cache helper should exist")
      .get_key(),
    reader
      .get_reader_cache_helper()?
      .expect("reader cache helper should exist")
      .get_key()
  );
  reader.close()?;
  reader = new_reader;
  assert_eq!(3, reader.num_docs()?);
  assert_eq!(4, reader.max_doc()?);
  assert_eq!(1, reader.num_deleted_docs()?);

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "1", Store::Yes)?);
  doc.add(StringField::from_string("version", "3", Store::Yes)?);
  writer.soft_update_document(
    Term::from_text("id", "1"),
    doc,
    vec![NumericDocValuesField::new("soft_delete", 1).into()],
  )?;
  writer.commit()?;

  let new_reader = directory_reader::open_if_changed(&reader)?.expect("reader should change");
  assert_ne!(
    new_reader
      .get_reader_cache_helper()?
      .expect("reader cache helper should exist")
      .get_key(),
    reader
      .get_reader_cache_helper()?
      .expect("reader cache helper should exist")
      .get_key()
  );
  assert_eq!(3, new_reader.get_sequential_sub_readers().len());
  assert_eq!(2, reader.get_sequential_sub_readers().len());
  assert_eq!(
    reader_cache_key(&reader.get_sequential_sub_readers()[0])?,
    reader_cache_key(&new_reader.get_sequential_sub_readers()[0])?
  );
  assert_ne!(
    reader_cache_key(&reader.get_sequential_sub_readers()[1])?,
    reader_cache_key(&new_reader.get_sequential_sub_readers()[1])?
  );
  assert!(is_wrapped(&reader.get_sequential_sub_readers()[0]));
  // The last one has no soft deletes.
  assert!(!is_wrapped(&reader.get_sequential_sub_readers()[1]));

  assert!(is_wrapped(&new_reader.get_sequential_sub_readers()[0]));
  assert!(is_wrapped(&new_reader.get_sequential_sub_readers()[1]));
  // The last one has no soft deletes.
  assert!(!is_wrapped(&new_reader.get_sequential_sub_readers()[2]));
  reader.close()?;
  reader = new_reader;
  assert_eq!(3, reader.num_docs()?);
  assert_eq!(5, reader.max_doc()?);
  assert_eq!(2, reader.num_deleted_docs()?);

  let close_result = IOUtils::use_or_suppress_result(reader.close(), writer.close());
  IOUtils::use_or_suppress_result(close_result, dir.close())
}

fn is_wrapped<CR>(reader: &SoftDeletesCodecReader<CR>) -> bool
where
  CR: CodecReader,
  CR::ReaderCacheHelper: Clone,
{
  matches!(reader, CodecReaderEnum2::B(_))
}

fn reader_cache_key<CR>(reader: &SoftDeletesCodecReader<CR>) -> Result<CacheKey>
where
  CR: CodecReader,
  CR::ReaderCacheHelper: Clone,
{
  match reader {
    CodecReaderEnum2::A(reader) => Ok(
      reader
        .get_reader_cache_helper()?
        .expect("reader cache helper should exist")
        .get_key(),
    ),
    CodecReaderEnum2::B(reader) => Ok(
      reader
        .get_reader_cache_helper()?
        .expect("reader cache helper should exist")
        .get_key(),
    ),
  }
}

#[test]
fn test_mix_soft_and_hard_deletes() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut index_writer_config = new_index_writer_config(&mut random)?;
  let soft_deletes_field = "soft_delete";
  index_writer_config.set_soft_deletes_field(soft_deletes_field);
  let writer = IndexWriter::new(dir.clone(), index_writer_config)?;
  let mut unique_docs = HashSet::new();
  for _ in 0..100 {
    let doc_id = random.random_range(0..5);
    unique_docs.insert(doc_id);
    let mut doc = Document::new();
    doc.add(StringField::from_string(
      "id",
      doc_id.to_string(),
      Store::Yes,
    )?);
    if doc_id % 2 == 0 {
      writer.update_document_with_term(Term::from_text("id", doc_id.to_string()), doc)?;
    } else {
      writer.soft_update_document(
        Term::from_text("id", doc_id.to_string()),
        doc,
        vec![NumericDocValuesField::new(soft_deletes_field, 0).into()],
      )?;
    }
  }

  writer.commit()?;
  writer.close()?;
  let reader = SoftDeletesDirectoryReaderWrapper::new(
    directory_reader::open(dir.clone())?,
    soft_deletes_field,
  )?;
  assert_eq!(unique_docs.len() as i32, reader.num_docs()?);
  let searcher = index_searcher::from_reader(reader)?;
  for doc_id in unique_docs {
    assert_eq!(
      1,
      searcher.count(TermQuery::new(Term::from_text("id", doc_id.to_string())))?
    );
  }

  let close_result = searcher.get_index_reader().close();
  IOUtils::use_or_suppress_result(close_result, dir.close())
}

#[test]
fn test_reader_cache_key() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut index_writer_config = new_index_writer_config(&mut random)?;
  let soft_deletes_field = "soft_delete";
  index_writer_config
    .set_soft_deletes_field(soft_deletes_field)
    .set_merge_policy(NoMergePolicy::default());
  let writer = IndexWriter::new(dir.clone(), index_writer_config)?;

  let body_result = (|| -> Result<()> {
    let mut doc = Document::new();
    doc.add(StringField::from_string("id", "1", Store::Yes)?);
    doc.add(StringField::from_string("version", "1", Store::Yes)?);
    writer.add_document(doc)?;

    let mut doc = Document::new();
    doc.add(StringField::from_string("id", "2", Store::Yes)?);
    doc.add(StringField::from_string("version", "1", Store::Yes)?);
    writer.add_document(doc)?;
    writer.commit()?;

    let mut reader = SoftDeletesDirectoryReaderWrapper::new(
      directory_reader::open(dir.clone())?,
      soft_deletes_field,
    )?;
    let reader_context = (&reader).get_context()?;
    let leaf_cache_helper = reader_context.leaves()?[0]
      .reader()
      .get_reader_cache_helper()?
      .expect("leaf reader must expose a reader cache helper");
    let leaf_cache_key = leaf_cache_helper.get_key();
    let leaf_called = Arc::new(AtomicI32::new(0));
    let leaf_called_listener = leaf_called.clone();
    leaf_cache_helper.add_closed_listener(Box::new(move |key: &CacheKey| {
      leaf_called_listener.fetch_add(1, Ordering::SeqCst);
      if &leaf_cache_key != key {
        return Err(
          crate::core::util::error::lucene_error::LuceneError::illegal_state(
            "leaf close listener received a different cache key",
          ),
        );
      }
      Ok(())
    }))?;

    let dir_cache_helper = reader
      .get_reader_cache_helper()?
      .expect("directory reader must expose a reader cache helper");
    let old_dir_cache_key = dir_cache_helper.get_key();
    let listener_dir_cache_key = old_dir_cache_key.clone();
    let dir_called = Arc::new(AtomicI32::new(0));
    let dir_called_listener = dir_called.clone();
    dir_cache_helper.add_closed_listener(Box::new(move |key: &CacheKey| {
      dir_called_listener.fetch_add(1, Ordering::SeqCst);
      if &listener_dir_cache_key != key {
        return Err(
          crate::core::util::error::lucene_error::LuceneError::illegal_state(
            "directory close listener received a different cache key",
          ),
        );
      }
      Ok(())
    }))?;

    assert_eq!(2, reader.num_docs()?);
    assert_eq!(2, reader.max_doc()?);
    assert_eq!(0, reader.num_deleted_docs()?);

    let mut doc = Document::new();
    doc.add(StringField::from_string("id", "1", Store::Yes)?);
    doc.add(StringField::from_string("version", "2", Store::Yes)?);
    writer.soft_update_document(
      Term::from_text("id", "1"),
      doc,
      vec![NumericDocValuesField::new(soft_deletes_field, 1).into()],
    )?;

    let mut doc = Document::new();
    doc.add(StringField::from_string("id", "3", Store::Yes)?);
    doc.add(StringField::from_string("version", "1", Store::Yes)?);
    writer.add_document(doc)?;
    writer.commit()?;

    assert_eq!(0, leaf_called.load(Ordering::SeqCst));
    assert_eq!(0, dir_called.load(Ordering::SeqCst));
    let new_reader = directory_reader::open_if_changed(&reader)?
      .expect("reader must observe the committed changes");
    assert_eq!(0, leaf_called.load(Ordering::SeqCst));
    assert_eq!(0, dir_called.load(Ordering::SeqCst));
    let new_dir_cache_key = new_reader
      .get_reader_cache_helper()?
      .expect("directory reader must expose a reader cache helper")
      .get_key();
    assert_ne!(new_dir_cache_key, old_dir_cache_key);
    reader.close()?;
    reader = new_reader;
    assert_eq!(1, dir_called.load(Ordering::SeqCst));
    assert_eq!(1, leaf_called.load(Ordering::SeqCst));
    reader.close()
  })();

  let body_result = IOUtils::use_or_suppress_result(body_result, writer.close());
  IOUtils::use_or_suppress_result(body_result, dir.close())
}

#[test]
fn test_avoid_wrapping_readers_without_soft_deletes() -> Result<()> {
  let mut random = random();
  let mut iwc = new_index_writer_config(&mut random)?;
  let soft_deletes_field = "soft_deletes";
  iwc.set_soft_deletes_field(soft_deletes_field);
  let merge_policy = iwc.get_merge_policy().clone();
  iwc.set_merge_policy(SoftDeletesRetentionMergePolicy::new(
    soft_deletes_field,
    || Ok(MatchAllDocsQuery::new().into()),
    merge_policy.clone(),
  ));
  let dir = new_directory_shared(&mut random)?;
  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let body_result = (|| -> Result<()> {
    let num_docs = 1 + random.random_range(0..10);
    for i in 0..num_docs {
      let mut doc = Document::new();
      doc.add(StringField::from_string("id", i.to_string(), Store::Yes)?);
      writer.add_document(doc)?;
    }
    let num_deletes = 1 + random.random_range(0..5);
    for _ in 0..num_deletes {
      let doc_id = random.random_range(0..num_docs).to_string();
      let mut doc = Document::new();
      doc.add(StringField::from_string("id", &doc_id, Store::Yes)?);
      writer.soft_update_document(
        Term::from_text("id", &doc_id),
        doc,
        vec![NumericDocValuesField::new(soft_deletes_field, 0).into()],
      )?;
    }
    writer.flush()?;
    let reader = directory_reader::open_from_writer(&writer)?;
    let wrapped = SoftDeletesDirectoryReaderWrapper::new(reader, soft_deletes_field)?;
    let mut expected_num_deletes = 0;
    let context = (&wrapped).get_context()?;
    for leaf in context.leaves()? {
      expected_num_deletes += leaf.reader().num_deleted_docs()?;
    }
    assert_eq!(num_docs, wrapped.num_docs()?);
    assert_eq!(expected_num_deletes, wrapped.num_deleted_docs()?);
    wrapped.close()?;

    writer
      .get_config_mut()
      .set_merge_policy(SoftDeletesRetentionMergePolicy::new(
        soft_deletes_field,
        || Ok(MatchNoDocsQuery::new().into()),
        merge_policy,
      ));
    writer.force_merge(1)?;
    let reader = directory_reader::open_from_writer(&writer)?;
    let context = (&reader).get_context()?;
    for leaf_context in context.leaves()? {
      let segment_reader = leaf_context.reader();
      assert!(segment_reader.get_live_docs()?.is_none());
      assert!(segment_reader.get_hard_live_docs()?.is_none());
    }
    let wrapped = SoftDeletesDirectoryReaderWrapper::new(reader, soft_deletes_field)?;
    assert_eq!(num_docs, wrapped.num_docs()?);
    assert_eq!(0, wrapped.num_deleted_docs()?);
    let context = (&wrapped).get_context()?;
    for leaf in context.leaves()? {
      assert!(matches!(leaf.reader(), CodecReaderEnum2::A(_)));
    }
    wrapped.close()
  })();

  let close_result = IOUtils::use_or_suppress_result(writer.close(), dir.close());
  IOUtils::use_or_suppress_result(body_result, close_result)
}
