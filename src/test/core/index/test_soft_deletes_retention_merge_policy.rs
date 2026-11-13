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
use crate::core::document::field::{Field, FieldDataEnum, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::fields::Fields;
use crate::core::document::int_point::IntPoint;
use crate::core::document::long_point::LongPoint;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::string_field::StringField;
use crate::core::index::base_composite_reader::{
  BCRStoredFieldsImpl, BCRTermVectorsImpl, BaseCompositeReader, BaseCompositeReaderBase,
};
use crate::core::index::composite_reader::CompositeReader;
use crate::core::index::directory_reader::{self, DirectoryReader, DirectoryReaderBase};
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::filter_directory_reader::{FilterDirectoryReader, SubReaderWrapper};
use crate::core::index::index_reader::{
  CompositeReaderContextKind, IndexReader, IndexReaderBase, LeafReaderContextKind,
};
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::{IndexReaderWarmer, IndexReaderWarmerEnum, IndexWriter};
use crate::core::index::index_writer_config::DISABLE_AUTO_FLUSH;
use crate::core::index::leaf_metadata::LeafMetaData;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::log_doc_merge_policy::LogDocMergePolicy;
use crate::core::index::log_merge_policy::LogMergePolicy;
use crate::core::index::merge_policy::MergePolicy;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::index::segment_reader::DefaultLeafReader;
use crate::core::index::soft_deletes_retention_merge_policy::SoftDeletesRetentionMergePolicy;
use crate::core::index::standard_directory_reader::StandardDirectoryReader;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term::Term;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::search::field_exists_query::FieldExistsQuery;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::search::match_all_docs_query::MatchAllDocsQuery;
use crate::core::search::match_no_docs_query::MatchNoDocsQuery;
use crate::core::search::prefix_query::PrefixQuery;
use crate::core::search::query::{IntoQuery, Query};
use crate::core::search::reference_manager::ReferenceManagerBase;
use crate::core::search::searcher_manager::SearcherManager;
use crate::core::search::term_query::TermQuery;
use crate::core::store::directory::Directory;
use crate::core::util::bits::Bits;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::dummy::dummy_comparator::DummyComparator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::io_utils::IOUtils;
use crate::test_framework::core::index::test_concurrent_merge_scheduler::CountDownLatch;
use crate::test_framework::core::util::lucene_test_case::{
  new_directory_shared, new_index_writer_config, new_log_merge_policy, new_merge_policy,
  new_merge_policy_with_mock_mp, new_searcher_with_reader, random, random_from_seed, rarely,
};
use rand::RngExt;
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering::SeqCst};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[allow(dead_code)] // for quick search
struct TestSoftDeletesRetentionMergePolicy;

#[test]
fn test_force_merge_fully_deleted() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let let_it_go = Arc::new(AtomicBool::new(false));
  let query_let_it_go = let_it_go.clone();
  let policy = SoftDeletesRetentionMergePolicy::<crate::core::store::directory::DirEnum>::new(
    "soft_delete",
    move || {
      if query_let_it_go.load(SeqCst) {
        Ok(MatchNoDocsQuery::new().into())
      } else {
        Ok(MatchAllDocsQuery::new().into())
      }
    },
    LogMergePolicy::log_doc(),
  );
  let mut index_writer_config = new_index_writer_config(&mut random)?;
  index_writer_config
    .set_merge_policy(policy.clone())
    .set_soft_deletes_field("soft_delete");
  let writer = IndexWriter::new(dir.clone(), index_writer_config)?;

  let body_result = (|| -> Result<()> {
    let mut doc = Document::new();
    doc.add(StringField::from_string("id", "1", Store::Yes)?);
    doc.add(NumericDocValuesField::new("soft_delete", 1));
    writer.add_document(doc)?;
    writer.commit()?;
    let mut doc = Document::new();
    doc.add(StringField::from_string("id", "2", Store::Yes)?);
    doc.add(NumericDocValuesField::new("soft_delete", 1));
    writer.add_document(doc)?;
    let mut reader = directory_reader::open_from_writer(&writer)?;
    {
      let context = (&reader).get_context()?;
      let leaves = context.leaves()?;
      assert_eq!(2, leaves.len());
      let segment_reader = leaves[0].reader();
      assert!(policy.keep_fully_deleted_segment(|| Ok(segment_reader.clone()))?);
      assert_eq!(
        0,
        policy.num_deletes_to_merge(segment_reader.get_segment_info(), 0, &|| Ok(
          segment_reader.clone()
        ),)?
      );
    }
    {
      let context = (&reader).get_context()?;
      let leaves = context.leaves()?;
      let segment_reader = leaves[1].reader();
      assert!(policy.keep_fully_deleted_segment(|| Ok(segment_reader.clone()))?);
      assert_eq!(
        0,
        policy.num_deletes_to_merge(segment_reader.get_segment_info(), 0, &|| Ok(
          segment_reader.clone()
        ),)?
      );
      writer.force_merge(1)?;
      reader.close()?;
    }
    reader = directory_reader::open_from_writer(&writer)?;
    {
      let context = (&reader).get_context()?;
      let leaves = context.leaves()?;
      assert_eq!(1, leaves.len());
      let segment_reader = leaves[0].reader();
      assert_eq!(2, reader.max_doc()?);
      assert!(policy.keep_fully_deleted_segment(|| Ok(segment_reader.clone()))?);
      assert_eq!(
        0,
        policy.num_deletes_to_merge(segment_reader.get_segment_info(), 0, &|| Ok(
          segment_reader.clone()
        ),)?
      );
    }
    writer.force_merge(1)?; // Make sure we don't merge this.
    assert!(directory_reader::open_if_changed(&reader)?.is_none());

    writer.force_merge_deletes()?; // Make sure we don't merge this.
    assert!(directory_reader::open_if_changed(&reader)?.is_none());
    let_it_go.store(true, SeqCst);
    writer.force_merge_deletes()?;
    let directory_reader = directory_reader::open_if_changed(&reader)?
      .expect("reader should change after retained documents are released");
    assert_eq!(0, directory_reader.num_deleted_docs()?);
    assert_eq!(0, directory_reader.max_doc()?);
    let close_result = IOUtils::use_or_suppress_result(directory_reader.close(), reader.close());
    IOUtils::use_or_suppress_result(close_result, Ok(()))
  })();

  let close_result = IOUtils::use_or_suppress_result(writer.close(), dir.close());
  IOUtils::use_or_suppress_result(body_result, close_result)
}

#[test]
fn test_keep_fully_deleted_segments() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut index_writer_config = new_index_writer_config(&mut random)?;
  index_writer_config.set_merge_policy(NoMergePolicy::default());
  let writer = IndexWriter::new(dir.clone(), index_writer_config)?;

  let body_result = (|| -> Result<()> {
    let mut doc = Document::new();
    doc.add(StringField::from_string("id", "1", Store::Yes)?);
    doc.add(NumericDocValuesField::new("soft_delete", 1));
    writer.add_document(doc)?;
    let mut reader = directory_reader::open_from_writer(&writer)?;
    let context = (&reader).get_context()?;
    assert_eq!(1, context.leaves()?.len());
    let policy = SoftDeletesRetentionMergePolicy::<crate::core::store::directory::DirEnum>::new(
      "soft_delete",
      || Ok(FieldExistsQuery::new("keep_around").into()),
      NoMergePolicy::default(),
    );
    let segment_reader = context.leaves()?[0].reader();
    assert!(!policy.keep_fully_deleted_segment(|| Ok(segment_reader.clone()))?);
    reader.close()?;

    let mut doc = Document::new();
    doc.add(StringField::from_string("id", "1", Store::Yes)?);
    doc.add(NumericDocValuesField::new("keep_around", 1));
    doc.add(NumericDocValuesField::new("soft_delete", 1));
    writer.add_document(doc)?;

    reader = directory_reader::open_from_writer(&writer)?;
    let context = (&reader).get_context()?;
    let leaves = context.leaves()?;
    assert_eq!(2, leaves.len());
    let segment_reader = leaves[0].reader();
    assert!(!policy.keep_fully_deleted_segment(|| Ok(segment_reader.clone()))?);
    let segment_reader = leaves[1].reader();
    assert!(policy.keep_fully_deleted_segment(|| Ok(segment_reader.clone()))?);
    reader.close()
  })();

  let close_result = IOUtils::use_or_suppress_result(writer.close(), dir.close());
  IOUtils::use_or_suppress_result(body_result, close_result)
}

#[test]
fn test_field_based_retention() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut index_writer_config = new_index_writer_config(&mut random)?;
  let now = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap_or_default()
    .as_millis() as i64;
  let time_24_hours_ago = now - Duration::from_secs(24 * 60 * 60).as_millis() as i64;
  let soft_deletes_field = "soft_delete";
  index_writer_config
    .set_merge_policy(SoftDeletesRetentionMergePolicy::new(
      soft_deletes_field,
      move || Ok(LongPoint::new_range_query("creation_date", time_24_hours_ago, now)?.into()),
      LogMergePolicy::log_doc(),
    ))
    .set_soft_deletes_field(soft_deletes_field);
  let writer = IndexWriter::new(dir.clone(), index_writer_config)?;

  let body_result = (|| -> Result<()> {
    let time_28_hours_ago = now - Duration::from_secs(28 * 60 * 60).as_millis() as i64;
    let mut doc = Document::new();
    doc.add(StringField::from_string("id", "1", Store::Yes)?);
    doc.add(StringField::from_string("version", "1", Store::Yes)?);
    doc.add(LongPoint::new("creation_date", [time_28_hours_ago])?);
    writer.add_document(doc)?;

    writer.flush()?;
    let time_26_hours_ago = now - Duration::from_secs(26 * 60 * 60).as_millis() as i64;
    let mut doc = Document::new();
    doc.add(StringField::from_string("id", "1", Store::Yes)?);
    doc.add(StringField::from_string("version", "2", Store::Yes)?);
    doc.add(LongPoint::new("creation_date", [time_26_hours_ago])?);
    writer.soft_update_document(
      Term::from_text("id", "1"),
      doc,
      vec![NumericDocValuesField::new("soft_delete", 1).into()],
    )?;

    if random.random_bool(0.5) {
      writer.flush()?;
    }
    let time_23_hours_ago = now - Duration::from_secs(23 * 60 * 60).as_millis() as i64;
    let mut doc = Document::new();
    doc.add(StringField::from_string("id", "1", Store::Yes)?);
    doc.add(StringField::from_string("version", "3", Store::Yes)?);
    doc.add(LongPoint::new("creation_date", [time_23_hours_ago])?);
    writer.soft_update_document(
      Term::from_text("id", "1"),
      doc,
      vec![NumericDocValuesField::new("soft_delete", 1).into()],
    )?;

    if random.random_bool(0.5) {
      writer.flush()?;
    }
    let time_12_hours_ago = now - Duration::from_secs(12 * 60 * 60).as_millis() as i64;
    let mut doc = Document::new();
    doc.add(StringField::from_string("id", "1", Store::Yes)?);
    doc.add(StringField::from_string("version", "4", Store::Yes)?);
    doc.add(LongPoint::new("creation_date", [time_12_hours_ago])?);
    writer.soft_update_document(
      Term::from_text("id", "1"),
      doc,
      vec![NumericDocValuesField::new("soft_delete", 1).into()],
    )?;

    if random.random_bool(0.5) {
      writer.flush()?;
    }
    let mut doc = Document::new();
    doc.add(StringField::from_string("id", "1", Store::Yes)?);
    doc.add(StringField::from_string("version", "5", Store::Yes)?);
    doc.add(LongPoint::new("creation_date", [now])?);
    writer.soft_update_document(
      Term::from_text("id", "1"),
      doc,
      vec![NumericDocValuesField::new("soft_delete", 1).into()],
    )?;

    if random.random_bool(0.5) {
      writer.flush()?;
    }
    writer.force_merge(1)?;
    let reader = directory_reader::open_from_writer(&writer)?;
    assert_eq!(1, reader.num_docs()?);
    assert_eq!(3, reader.max_doc()?);
    let mut versions = HashSet::new();
    let mut stored_fields = reader.stored_fields()?;
    for doc_id in 0..3 {
      versions.insert(
        stored_fields
          .document(doc_id)?
          .get("version")?
          .expect("version should be stored")
          .into_owned(),
      );
    }
    assert!(versions.contains("5"));
    assert!(versions.contains("4"));
    assert!(versions.contains("3"));
    reader.close()
  })();

  let close_result = IOUtils::use_or_suppress_result(writer.close(), dir.close());
  IOUtils::use_or_suppress_result(body_result, close_result)
}

#[test]
fn test_keep_all_docs_across_merges() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut index_writer_config = new_index_writer_config(&mut random)?;
  index_writer_config
    .set_merge_policy(SoftDeletesRetentionMergePolicy::new(
      "soft_delete",
      || Ok(MatchAllDocsQuery::new().into()),
      LogMergePolicy::log_doc(),
    ))
    .set_soft_deletes_field("soft_delete");
  let writer = IndexWriter::new(dir.clone(), index_writer_config)?;

  let body_result = (|| -> Result<()> {
    let mut doc = Document::new();
    doc.add(StringField::from_string("id", "1", Store::Yes)?);
    writer.soft_update_document(
      Term::from_text("id", "1"),
      doc,
      vec![NumericDocValuesField::new("soft_delete", 1).into()],
    )?;

    writer.commit()?;
    let mut doc = Document::new();
    doc.add(StringField::from_string("id", "1", Store::Yes)?);
    writer.soft_update_document(
      Term::from_text("id", "1"),
      doc,
      vec![NumericDocValuesField::new("soft_delete", 1).into()],
    )?;

    writer.commit()?;
    let mut doc = Document::new();
    doc.add(StringField::from_string("id", "1", Store::Yes)?);
    doc.add(NumericDocValuesField::new("soft_delete", 1)); // Already deleted.
    writer.soft_update_document(
      Term::from_text("id", "1"),
      doc,
      vec![NumericDocValuesField::new("soft_delete", 1).into()],
    )?;
    writer.commit()?;
    let mut reader = directory_reader::open_from_writer(&writer)?;
    assert_eq!(0, reader.num_docs()?);
    assert_eq!(3, reader.max_doc()?);
    assert_eq!(0, writer.get_doc_stats()?.num_docs);
    assert_eq!(3, writer.get_doc_stats()?.max_doc);
    assert_eq!(3, (&reader).get_context()?.leaves()?.len());
    reader.close()?;
    writer.force_merge(1)?;
    reader = directory_reader::open_from_writer(&writer)?;
    assert_eq!(0, reader.num_docs()?);
    assert_eq!(3, reader.max_doc()?);
    assert_eq!(0, writer.get_doc_stats()?.num_docs);
    assert_eq!(3, writer.get_doc_stats()?.max_doc);
    assert_eq!(1, (&reader).get_context()?.leaves()?.len());
    reader.close()
  })();

  let close_result = IOUtils::use_or_suppress_result(writer.close(), dir.close());
  IOUtils::use_or_suppress_result(body_result, close_result)
}

/// Tests soft deletes that carry over deleted documents on merge for history retention.
#[test]
fn test_soft_delete_with_retention() -> Result<()> {
  let mut random = random();
  let seq_ids = Arc::new(AtomicI32::new(0));
  let dir = new_directory_shared(&mut random)?;
  let mut index_writer_config = new_index_writer_config(&mut random)?;
  let retention_seq_ids = seq_ids.clone();
  let merge_policy = index_writer_config.get_merge_policy().clone();
  index_writer_config
    .set_merge_policy(SoftDeletesRetentionMergePolicy::new(
      "soft_delete",
      move || {
        Ok(
          IntPoint::new_range_query("seq_id", retention_seq_ids.load(SeqCst) - 50, i32::MAX)?
            .into(),
        )
      },
      merge_policy,
    ))
    .set_soft_deletes_field("soft_delete");
  let writer = IndexWriter::new(dir.clone(), index_writer_config)?;

  let body_result = (|| -> Result<()> {
    let num_threads = 2 + random.random_range(0..3);
    let start_latch = CountDownLatch::new(1);
    let started = CountDownLatch::new(num_threads);
    let update_several_docs = random.random_bool(0.5);
    let ids = Arc::new(Mutex::new(HashSet::new()));
    let seeds: Vec<u64> = (0..num_threads).map(|_| random.random()).collect();
    thread::scope(|scope| -> Result<()> {
      let mut threads = Vec::with_capacity(num_threads);
      for seed in seeds {
        let writer = &writer;
        let seq_ids = &seq_ids;
        let ids = &ids;
        let start_latch = &start_latch;
        let started = &started;
        threads.push(scope.spawn(move || -> Result<()> {
          let mut random = random_from_seed(seed);
          started.count_down();
          start_latch.wait();
          for _ in 0..100 {
            let id = random.random_range(0..10).to_string();
            let seq_id = seq_ids.fetch_add(1, SeqCst) + 1;
            let mut doc = Document::new();
            doc.add(StringField::from_string("id", &id, Store::Yes)?);
            doc.add(IntPoint::new("seq_id", [seq_id])?);
            if update_several_docs {
              let mut doc_copy = Document::new();
              doc_copy.add(StringField::from_string("id", &id, Store::Yes)?);
              doc_copy.add(IntPoint::new("seq_id", [seq_id])?);
              writer.soft_update_documents(
                Term::from_text("id", &id),
                vec![doc, doc_copy],
                vec![NumericDocValuesField::new("soft_delete", 1).into()],
              )?;
            } else {
              writer.soft_update_document(
                Term::from_text("id", &id),
                doc,
                vec![NumericDocValuesField::new("soft_delete", 1).into()],
              )?;
            }
            if rarely(&mut random) {
              writer.flush()?;
            }
            ids.lock().expect("ids mutex poisoned").insert(id);
          }
          Ok(())
        }));
      }
      started.wait();
      start_latch.count_down();
      for thread in threads {
        thread
          .join()
          .map_err(|_| LuceneError::illegal_state("indexing thread panicked"))??;
      }
      Ok(())
    })?;

    let mut reader = Arc::new(directory_reader::open_from_writer(&writer)?);
    let ids = ids.lock().expect("ids mutex poisoned").clone();
    let searcher = new_searcher_with_reader(reader.clone())?;
    for id in &ids {
      let top_docs = searcher.search(TermQuery::new(Term::from_text("id", id)), 10)?;
      if update_several_docs {
        assert_eq!(2, top_docs.total_hits.value());
        assert_eq!(
          1,
          (top_docs.score_docs[0].doc - top_docs.score_docs[1].doc).abs()
        );
      } else {
        assert_eq!(1, top_docs.total_hits.value());
      }
    }
    writer.add_document(Document::new())?; // Add a dummy doc to trigger a segment here.
    writer.flush()?;
    writer.force_merge(1)?;
    drop(searcher);
    if let Some(new_reader) =
      directory_reader::open_if_changed_with_writer(reader.as_ref(), &writer)?
    {
      reader.close()?;
      reader = Arc::new(new_reader);
    }
    let context = reader.clone().get_context()?;
    assert_eq!(1, context.leaves()?.len());

    let leaf_reader = context.leaves()?[0].reader().clone();
    let searcher =
      new_searcher_with_reader(SoftDeleteWithRetentionFilterLeafReader::new(leaf_reader)?)?;
    let seq_id = searcher.search(
      IntPoint::new_range_query("seq_id", seq_ids.load(SeqCst) - 50, i32::MAX)?,
      10,
    )?;
    assert!(
      seq_id.total_hits.value() >= 50,
      "{} hits",
      seq_id.total_hits.value()
    );
    drop(searcher);

    let searcher = new_searcher_with_reader(reader.clone())?;
    for id in &ids {
      let count = searcher
        .search(TermQuery::new(Term::from_text("id", id)), 10)?
        .total_hits
        .value();
      if update_several_docs {
        assert_eq!(2, count);
      } else {
        assert_eq!(1, count);
      }
    }
    reader.close()
  })();

  let close_result = IOUtils::use_or_suppress_result(writer.close(), dir.close());
  IOUtils::use_or_suppress_result(body_result, close_result)
}

#[test]
fn test_force_merge_deletes() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut config = new_index_writer_config(&mut random)?;
  config.set_soft_deletes_field("soft_delete");
  config.set_merge_policy(new_merge_policy_with_mock_mp(&mut random, false)?);
  if random.random_bool(0.5) {
    let merge_policy = config.get_merge_policy().clone();
    config.set_merge_policy(SoftDeletesRetentionMergePolicy::new(
      "soft_delete",
      || Ok(MatchNoDocsQuery::new().into()),
      merge_policy,
    ));
  }
  let writer = IndexWriter::new(dir.clone(), config)?;

  let body_result = (|| -> Result<()> {
    // The first segment includes d1 and d2.
    for i in 0..2 {
      let mut d = Document::new();
      d.add(StringField::from_string("id", i.to_string(), Store::Yes)?);
      writer.add_document(d)?;
    }
    writer.flush()?;
    // The second segment includes only the tombstone.
    let mut tombstone = Document::new();
    tombstone.add(NumericDocValuesField::new("soft_delete", 1));
    writer.soft_update_document(
      Term::from_text("id", "1"),
      tombstone,
      vec![NumericDocValuesField::new("soft_delete", 1).into()],
    )?;
    writer.flush_with_apply_merge_deletes(false, true)?;
    // Now we have two segments, both having soft-deleted documents. We expect any merge policy to
    // merge these segments into one segment when calling forceMergeDeletes.
    writer.force_merge_deletes_with_wait(true)?;
    assert_eq!(1, writer.clone_segment_infos()?.size());
    assert_eq!(1, writer.get_doc_stats()?.num_docs);
    assert_eq!(1, writer.get_doc_stats()?.max_doc);
    Ok(())
  })();

  let close_result = IOUtils::use_or_suppress_result(writer.close(), dir.close());
  IOUtils::use_or_suppress_result(body_result, close_result)
}

#[test]
fn test_drop_fully_soft_deleted_segment() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let soft_delete = random.random_bool(0.5).then_some("soft_delete");
  let mut config = new_index_writer_config(&mut random)?;
  if let Some(soft_delete) = soft_delete {
    config.set_soft_deletes_field(soft_delete);
  }
  config.set_merge_policy(new_merge_policy_with_mock_mp(&mut random, true)?);
  if let Some(soft_delete) = soft_delete
    && random.random_bool(0.5)
  {
    let merge_policy = config.get_merge_policy().clone();
    config.set_merge_policy(SoftDeletesRetentionMergePolicy::new(
      soft_delete,
      || Ok(MatchNoDocsQuery::new().into()),
      merge_policy,
    ));
  }
  let writer = IndexWriter::new(dir.clone(), config)?;

  let body_result = (|| -> Result<()> {
    for i in 0..2 {
      let mut d = Document::new();
      d.add(StringField::from_string("id", i.to_string(), Store::Yes)?);
      writer.add_document(d)?;
    }
    writer.flush()?;
    assert_eq!(1, writer.clone_segment_infos()?.size());

    if let Some(soft_delete) = soft_delete {
      // The newly created segment should be dropped as it is fully deleted (that is, it only
      // contains deleted documents).
      if random.random_bool(0.5) {
        let mut tombstone = Document::new();
        tombstone.add(NumericDocValuesField::new(soft_delete, 1));
        writer.soft_update_document(
          Term::from_text("id", "1"),
          tombstone,
          vec![NumericDocValuesField::new(soft_delete, 1).into()],
        )?;
      } else {
        let mut doc = Document::new();
        doc.add(StringField::from_string("id", "1", Store::Yes)?);
        if random.random_bool(0.5) {
          writer.soft_update_document(
            Term::from_text("id", "1"),
            doc,
            vec![NumericDocValuesField::new(soft_delete, 1).into()],
          )?;
        } else {
          writer.add_document(doc)?;
        }
        writer.update_doc_values(
          Term::from_text("id", "1"),
          vec![NumericDocValuesField::new(soft_delete, 1).into()],
        )?;
      }
    } else {
      let mut d = Document::new();
      d.add(StringField::from_string("id", "1", Store::Yes)?);
      writer.add_document(d)?;
      writer.delete_documents_with_terms(vec![Term::from_text("id", "1")])?;
    }
    writer.commit()?;
    let reader = directory_reader::open_from_writer(&writer)?;
    assert_eq!(1, reader.num_docs()?);
    reader.close()?;
    assert_eq!(1, writer.clone_segment_infos()?.size());
    Ok(())
  })();

  let close_result = IOUtils::use_or_suppress_result(writer.close(), dir.close());
  IOUtils::use_or_suppress_result(body_result, close_result)
}

#[test]
fn test_soft_delete_while_merge_survives() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let soft_delete = "soft_delete";
  let mut config = new_index_writer_config(&mut random)?;
  config
    .set_soft_deletes_field(soft_delete)
    .set_reader_pooling(true)
    .set_merge_policy(SoftDeletesRetentionMergePolicy::new(
      "soft_delete",
      || Ok(FieldExistsQuery::new("keep").into()),
      LogMergePolicy::log_doc(),
    ));
  let update = Arc::new(AtomicBool::new(true));
  let writer = IndexWriter::new(dir.clone(), config)?;
  let warmer_writer = Arc::downgrade(&writer);
  let warmer_update = update.clone();
  writer
    .get_config_mut()
    .set_merged_segment_warmer(Some(IndexReaderWarmerEnum::custom(
      SoftDeleteWhileMergeWarmer {
        writer: warmer_writer,
        update: warmer_update,
        soft_delete: soft_delete.to_string(),
      },
    )));

  let body_result = (|| -> Result<()> {
    let pre_existing_deletes = random.random_bool(0.5);
    for i in 0..2 {
      let mut d = Document::new();
      d.add(StringField::from_string("id", i.to_string(), Store::Yes)?);
      if pre_existing_deletes && random.random_bool(0.5) {
        writer.add_document(&mut d)?; // Randomly add a preexisting hard delete we don't retain.
        writer.delete_documents_with_terms(vec![Term::from_text("id", i.to_string())])?;
        d.add(NumericDocValuesField::new("keep", 1));
        writer.add_document(&mut d)?;
      } else {
        d.add(NumericDocValuesField::new("keep", 1));
        writer.add_document(&mut d)?;
      }
      writer.flush()?;
    }
    writer.force_merge(1)?;
    writer.commit()?;
    assert!(!update.load(SeqCst));
    let open = directory_reader::open(dir.clone())?;
    assert_eq!(0, open.num_deleted_docs()?);
    assert_eq!(3, open.max_doc()?);
    open.close()
  })();

  let close_result = IOUtils::use_or_suppress_result(writer.close(), dir.close());
  IOUtils::use_or_suppress_result(body_result, close_result)
}

/*
 * This test is trying to hard-delete a particular document while the segment is merged which is
 * already soft-deleted. This requires special logic inside IndexWriter#carryOverHardDeletes since
 * doc maps are not created for this document.
 */
#[test]
fn test_delete_doc_while_merge_that_is_soft_deleted() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let soft_delete = "soft_delete";
  let mut config = new_index_writer_config(&mut random)?;
  config
    .set_soft_deletes_field(soft_delete)
    .set_reader_pooling(true)
    .set_merge_policy(LogMergePolicy::log_doc());
  let delete = Arc::new(AtomicBool::new(true));
  let writer = IndexWriter::new(dir.clone(), config)?;

  let body_result = (|| -> Result<()> {
    let mut d = Document::new();
    d.add(StringField::from_string("id", "0", Store::Yes)?);
    writer.add_document(d)?;
    let mut d = Document::new();
    d.add(StringField::from_string("id", "1", Store::Yes)?);
    writer.add_document(d)?;
    if random.random_bool(0.5) {
      // Randomly run with a preexisting hard delete.
      let mut d = Document::new();
      d.add(StringField::from_string("id", "2", Store::Yes)?);
      writer.add_document(d)?;
      writer.delete_documents_with_terms(vec![Term::from_text("id", "2")])?;
    }

    writer.flush()?;
    let reader = Arc::new(directory_reader::open_from_writer(&writer)?);
    writer.soft_update_document(
      Term::from_text("id", "0"),
      Document::new(),
      vec![NumericDocValuesField::new(soft_delete, 1).into()],
    )?;
    writer.flush()?;
    let warmer_writer = Arc::downgrade(&writer);
    let warmer_reader = reader.clone();
    let warmer_delete = delete.clone();
    writer
      .get_config_mut()
      .set_merged_segment_warmer(Some(IndexReaderWarmerEnum::custom(
        DeleteDocWhileMergeWarmer {
          writer: warmer_writer,
          reader: warmer_reader,
          delete: warmer_delete,
        },
      )));
    writer.force_merge(1)?;
    assert_eq!(2, writer.get_doc_stats()?.num_docs);
    assert_eq!(2, writer.get_doc_stats()?.max_doc);
    assert!(!delete.load(SeqCst));
    reader.close()
  })();

  let close_result = IOUtils::use_or_suppress_result(writer.close(), dir.close());
  IOUtils::use_or_suppress_result(body_result, close_result)
}

#[test]
fn test_undelete_document() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let soft_delete = "soft_delete";
  let mut config = new_index_writer_config(&mut random)?;
  config.set_soft_deletes_field(soft_delete).set_merge_policy(
    SoftDeletesRetentionMergePolicy::new(
      "soft_delete",
      || Ok(MatchAllDocsQuery::new().into()),
      LogMergePolicy::log_doc(),
    ),
  );
  config.set_reader_pooling(true);
  config.set_merge_policy(LogMergePolicy::log_doc());
  let writer = IndexWriter::new(dir.clone(), config)?;

  let body_result = (|| -> Result<()> {
    let mut d = Document::new();
    d.add(StringField::from_string("id", "0", Store::Yes)?);
    d.add(StringField::from_string("seq_id", "0", Store::Yes)?);
    writer.add_document(d)?;
    let mut d = Document::new();
    d.add(StringField::from_string("id", "1", Store::Yes)?);
    writer.add_document(d)?;
    writer.update_doc_values(
      Term::from_text("id", "0"),
      vec![NumericDocValuesField::new("soft_delete", 1).into()],
    )?;
    let reader = directory_reader::open_from_writer(&writer)?;
    assert_eq!(2, reader.max_doc()?);
    assert_eq!(1, reader.num_docs()?);
    reader.close()?;

    let mut field_type = FieldType::new();
    field_type.set_doc_values_type(DocValuesType::Numeric)?;
    field_type.freeze();
    do_update(Term::from_text("id", "0"), &writer, || {
      Ok(vec![
        Field::new("soft_delete", FieldDataEnum::Dummy(()), field_type.clone()).into(),
      ])
    })?;
    let reader = directory_reader::open_from_writer(&writer)?;
    assert_eq!(2, reader.max_doc()?);
    assert_eq!(2, reader.num_docs()?);
    reader.close()
  })();

  let close_result = IOUtils::use_or_suppress_result(writer.close(), dir.close());
  IOUtils::use_or_suppress_result(body_result, close_result)
}

#[test]
fn test_merge_soft_delete_and_hard_delete() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let soft_delete = "soft_delete";
  let mut config = new_index_writer_config(&mut random)?;
  config.set_soft_deletes_field(soft_delete).set_merge_policy(
    SoftDeletesRetentionMergePolicy::new(
      "soft_delete",
      || Ok(MatchAllDocsQuery::new().into()),
      LogMergePolicy::log_doc(),
    ),
  );
  config.set_reader_pooling(true);
  let writer = IndexWriter::new(dir.clone(), config)?;

  let body_result = (|| -> Result<()> {
    let mut d = Document::new();
    d.add(StringField::from_string("id", "0", Store::Yes)?);
    writer.add_document(d)?;
    let mut d = Document::new();
    d.add(StringField::from_string("id", "1", Store::Yes)?);
    d.add(NumericDocValuesField::new("soft_delete", 1));
    writer.add_document(d)?;
    let reader = directory_reader::open_from_writer(&writer)?;
    assert_eq!(2, reader.max_doc()?);
    assert_eq!(1, reader.num_docs()?);
    reader.close()?;

    loop {
      let reader = Arc::new(directory_reader::open_from_writer(&writer)?);
      let searcher = new_searcher_with_reader(IncludeSoftDeletesWrapper::new(reader.clone())?)?;
      let body_result = (|| -> Result<i64> {
        let top_docs = searcher.search(TermQuery::new(Term::from_text("id", "1")), 1)?;
        assert_eq!(1, top_docs.total_hits.value());
        writer.try_delete_document(reader.as_ref(), top_docs.score_docs[0].doc)
      })();
      drop(searcher);
      let seq_no = IOUtils::use_or_suppress_result(body_result, reader.close())?;
      if seq_no > 0 {
        break;
      }
    }
    writer.force_merge_deletes_with_wait(true)?;
    let infos = writer.clone_segment_infos()?;
    assert_eq!(1, infos.size());
    let si = infos.info(0).expect("segment should exist");
    assert_eq!(0, si.get_soft_del_count()); // Hard delete should supersede the soft delete.
    assert_eq!(0, si.get_del_count());
    assert_eq!(1, si.info.max_doc()?);
    Ok(())
  })();

  let close_result = IOUtils::use_or_suppress_result(writer.close(), dir.close());
  IOUtils::use_or_suppress_result(body_result, close_result)
}

#[test]
fn test_soft_delete_with_try_update_doc_value() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut config = new_index_writer_config(&mut random)?;
  config
    .set_soft_deletes_field("soft_delete")
    .set_merge_policy(SoftDeletesRetentionMergePolicy::new(
      "soft_delete",
      || Ok(MatchAllDocsQuery::new().into()),
      new_log_merge_policy(&mut random)?,
    ));
  let writer = IndexWriter::new(dir.clone(), config)?;
  let sm = SearcherManager::from_writer(&writer, None)?;

  let body_result = (|| -> Result<()> {
    let mut d = Document::new();
    d.add(StringField::from_string("id", "0", Store::Yes)?);
    writer.add_document(d)?;
    sm.maybe_refresh_blocking()?;
    do_update(Term::from_text("id", "0"), &writer, || {
      Ok(vec![
        NumericDocValuesField::new("soft_delete", 1).into(),
        NumericDocValuesField::new("other-field", 1).into(),
      ])
    })?;
    sm.maybe_refresh_blocking()?;
    let infos = writer.clone_segment_infos()?;
    assert_eq!(1, infos.size());
    let si = infos.info(0).expect("segment should exist");
    assert_eq!(1, si.get_soft_del_count());
    assert_eq!(1, si.info.max_doc()?);
    Ok(())
  })();

  let close_result = IOUtils::use_or_suppress_result(sm.close(), writer.close());
  let close_result = IOUtils::use_or_suppress_result(close_result, dir.close());
  IOUtils::use_or_suppress_result(body_result, close_result)
}

#[test]
fn test_mixed_soft_deletes_and_hard_deletes() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let soft_deletes_field = "soft-deletes";
  let mut config = new_index_writer_config(&mut random)?;
  config
    .set_max_buffered_docs(2 + random.random_range(0..50))
    .set_ram_buffer_size_mb(DISABLE_AUTO_FLUSH as f64)
    .set_soft_deletes_field(soft_deletes_field)
    .set_merge_policy(SoftDeletesRetentionMergePolicy::new(
      soft_deletes_field,
      || Ok(MatchAllDocsQuery::new().into()),
      new_merge_policy(&mut random)?,
    ));
  let writer = IndexWriter::new(dir.clone(), config)?;

  let body_result = (|| -> Result<()> {
    let num_docs = 10 + random.random_range(0..100);
    let mut live_docs = HashSet::new();
    for i in 0..num_docs {
      let id = i.to_string();
      let mut doc = Document::new();
      doc.add(StringField::from_string("id", &id, Store::Yes)?);
      writer.add_document(doc)?;
      live_docs.insert(id);
    }
    for i in 0..num_docs {
      if random.random_bool(0.5) {
        let id = i.to_string();
        if random.random_bool(0.5) && live_docs.contains(&id) {
          do_update(Term::from_text("id", &id), &writer, || {
            Ok(vec![
              NumericDocValuesField::new(soft_deletes_field, 1).into(),
            ])
          })?;
        } else {
          let version_id = format!("v{id}");
          let mut doc = Document::new();
          doc.add(StringField::from_string("id", &version_id, Store::Yes)?);
          writer.soft_update_document(
            Term::from_text("id", &id),
            doc,
            vec![NumericDocValuesField::new(soft_deletes_field, 1).into()],
          )?;
          live_docs.insert(version_id);
        }
      }
      if random.random_bool(0.5) && !live_docs.is_empty() {
        let del_id = live_docs
          .iter()
          .nth(random.random_range(0..live_docs.len()))
          .expect("live docs should not be empty")
          .clone();
        if random.random_bool(0.5) {
          do_delete(Term::from_text("id", &del_id), &writer)?;
        } else {
          writer.delete_documents_with_terms(vec![Term::from_text("id", &del_id)])?;
        }
        live_docs.remove(&del_id);
      }
    }
    let reader = Arc::new(directory_reader::open_from_writer(&writer)?);
    let include_soft_deletes = IncludeSoftDeletesWrapper::new(reader.clone())?;
    assert_eq!(live_docs.len() as i32, include_soft_deletes.num_docs()?);
    drop(include_soft_deletes);
    reader.close()?;
    writer.commit().map(|_| ())
  })();

  let close_result = IOUtils::use_or_suppress_result(writer.close(), dir.close());
  IOUtils::use_or_suppress_result(body_result, close_result)
}

#[test]
fn test_rewrite_retention_query() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut config = new_index_writer_config(&mut random)?;
  config
    .set_soft_deletes_field("soft_deletes")
    .set_merge_policy(SoftDeletesRetentionMergePolicy::new(
      "soft_deletes",
      || Ok(PrefixQuery::new(Term::from_text("id", "foo"))?.into_query()),
      new_merge_policy(&mut random)?,
    ));
  let writer = IndexWriter::new(dir.clone(), config)?;

  let body_result = (|| -> Result<()> {
    let mut d = Document::new();
    d.add(StringField::from_string("id", "foo-1", Store::Yes)?);
    writer.add_document(d)?;
    let mut d = Document::new();
    d.add(StringField::from_string("id", "foo-2", Store::Yes)?);
    writer.soft_update_document(
      Term::from_text("id", "foo-1"),
      d,
      vec![NumericDocValuesField::new("soft_deletes", 1).into()],
    )?;

    let mut d = Document::new();
    d.add(StringField::from_string("id", "bar-1", Store::Yes)?);
    writer.add_document(&mut d)?;
    d.add(StringField::from_string("id", "bar-2", Store::Yes)?);
    writer.soft_update_document(
      Term::from_text("id", "bar-1"),
      &mut d,
      vec![NumericDocValuesField::new("soft_deletes", 1).into()],
    )?;

    writer.force_merge(1)?;
    assert_eq!(2, writer.get_doc_stats()?.num_docs); // foo-2, bar-2
    assert_eq!(3, writer.get_doc_stats()?.max_doc); // foo-1, foo-2, bar-2
    Ok(())
  })();

  let close_result = IOUtils::use_or_suppress_result(writer.close(), dir.close());
  IOUtils::use_or_suppress_result(body_result, close_result)
}

struct SoftDeleteWhileMergeWarmer<D>
where
  D: Directory,
  DefaultLeafReader<D>: Clone,
{
  writer: Weak<IndexWriter<D>>,
  update: Arc<AtomicBool>,
  soft_delete: String,
}

impl<D> IndexReaderWarmer<D> for SoftDeleteWhileMergeWarmer<D>
where
  D: Directory + 'static,
{
  fn warm(&self, _reader: &DefaultLeafReader<D>) -> Result<()> {
    if self
      .update
      .compare_exchange(true, false, SeqCst, SeqCst)
      .is_ok()
    {
      let writer = self
        .writer
        .upgrade()
        .ok_or_else(|| LuceneError::already_closed("IndexWriter is closed"))?;
      writer.soft_update_document(
        Term::from_text("id", "0"),
        Document::new(),
        vec![
          NumericDocValuesField::new(self.soft_delete.clone(), 1).into(),
          NumericDocValuesField::new("keep", 1).into(),
        ],
      )?;
      writer.commit()?;
    }
    Ok(())
  }
}

struct DeleteDocWhileMergeWarmer<D>
where
  D: Directory,
{
  writer: Weak<IndexWriter<D>>,
  reader: Arc<StandardDirectoryReader<D>>,
  delete: Arc<AtomicBool>,
}

impl<D> IndexReaderWarmer<D> for DeleteDocWhileMergeWarmer<D>
where
  D: Directory + 'static,
{
  fn warm(&self, _reader: &DefaultLeafReader<D>) -> Result<()> {
    if self
      .delete
      .compare_exchange(true, false, SeqCst, SeqCst)
      .is_ok()
    {
      let writer = self
        .writer
        .upgrade()
        .ok_or_else(|| LuceneError::already_closed("IndexWriter is closed"))?;
      let seq_no = writer.try_delete_document(self.reader.as_ref(), 0)?;
      assert_ne!(seq_no, -1, "seqId was -1");
    }
    Ok(())
  }
}

fn do_update<D, F>(doc: Term, writer: &Arc<IndexWriter<D>>, mut fields: F) -> Result<()>
where
  D: Directory + 'static,
  F: FnMut() -> Result<Vec<Fields>>,
{
  let mut seq_id = -1;
  while seq_id == -1 {
    let reader = Arc::new(directory_reader::open_from_writer(writer)?);
    let searcher = new_searcher_with_reader(IncludeSoftDeletesWrapper::new(reader.clone())?)?;
    let body_result = (|| -> Result<i64> {
      let top_docs = searcher.search(TermQuery::new(doc.clone()), 10)?;
      assert_eq!(1, top_docs.total_hits.value());
      let the_doc = top_docs.score_docs[0].doc;
      writer.try_update_doc_value(reader.as_ref(), the_doc, fields()?)
    })();
    drop(searcher);
    seq_id = IOUtils::use_or_suppress_result(body_result, reader.close())?;
  }
  Ok(())
}

fn do_delete<D>(doc: Term, writer: &Arc<IndexWriter<D>>) -> Result<()>
where
  D: Directory + 'static,
{
  let mut seq_id = -1;
  while seq_id == -1 {
    let reader = Arc::new(directory_reader::open_from_writer(writer)?);
    let searcher = new_searcher_with_reader(IncludeSoftDeletesWrapper::new(reader.clone())?)?;
    let body_result = (|| -> Result<i64> {
      let top_docs = searcher.search(TermQuery::new(doc.clone()), 10)?;
      assert_eq!(1, top_docs.total_hits.value());
      let the_doc = top_docs.score_docs[0].doc;
      writer.try_delete_document(reader.as_ref(), the_doc)
    })();
    drop(searcher);
    seq_id = IOUtils::use_or_suppress_result(body_result, reader.close())?;
  }
  Ok(())
}

struct SoftDeleteWithRetentionFilterLeafReader<LR>
where
  LR: LeafReader,
{
  in_: LR,
  index_base: Arc<IndexReaderBase>,
}

impl<LR> SoftDeleteWithRetentionFilterLeafReader<LR>
where
  LR: LeafReader,
{
  fn new(in_: LR) -> Result<Self> {
    let index_base = Arc::new(IndexReaderBase::new());
    in_.register_parent_reader(index_base.as_ref())?;
    Ok(Self { in_, index_base })
  }
}

impl<LR> Clone for SoftDeleteWithRetentionFilterLeafReader<LR>
where
  LR: LeafReader + Clone,
{
  fn clone(&self) -> Self {
    Self {
      in_: self.in_.clone(),
      index_base: self.index_base.clone(),
    }
  }
}

impl<LR> Display for SoftDeleteWithRetentionFilterLeafReader<LR>
where
  LR: LeafReader,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "SoftDeleteWithRetentionFilterLeafReader({})", self.in_)
  }
}

impl<LR> IndexReader for SoftDeleteWithRetentionFilterLeafReader<LR>
where
  LR: LeafReader,
{
  type ContextKind = LeafReaderContextKind;
  type TermVectors = LR::TermVectors;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    self.in_.term_vectors()
  }

  fn max_doc(&self) -> Result<i32> {
    self.in_.max_doc()
  }

  fn num_docs(&self) -> Result<i32> {
    self.max_doc()
  }

  type StoredFields = LR::StoredFields;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    self.in_.stored_fields()
  }

  fn do_close(&self) -> Result<()> {
    self.in_.close()
  }

  type ReaderCacheHelper = LR::ReaderCacheHelper;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    self.in_.get_reader_cache_helper()
  }

  fn doc_freq(&self, term: &Term) -> Result<i32> {
    IndexReader::doc_freq(&self.in_, term)
  }

  fn total_term_freq(&self, term: &Term) -> Result<i64> {
    self.in_.total_term_freq(term)
  }

  fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
    IndexReader::get_sum_doc_freq(&self.in_, field)
  }

  fn get_doc_count(&self, field: &str) -> Result<i32> {
    IndexReader::get_doc_count(&self.in_, field)
  }

  fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
    IndexReader::get_sum_total_term_freq(&self.in_, field)
  }

  fn index_base(&self) -> &IndexReaderBase {
    self.index_base.as_ref()
  }
}

impl<LR> LeafReader for SoftDeleteWithRetentionFilterLeafReader<LR>
where
  LR: LeafReader,
{
  type CacheHelper = LR::CacheHelper;

  fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
    self.in_.get_core_cache_helper()
  }

  type Terms = LR::Terms;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    self.in_.terms(field)
  }

  type NumericDocValues = LR::NumericDocValues;

  fn get_numeric_doc_values(&self, field: &str) -> Result<Option<Self::NumericDocValues>> {
    self.in_.get_numeric_doc_values(field)
  }

  type BinaryDocValues = LR::BinaryDocValues;

  fn get_binary_doc_values(&self, field: &str) -> Result<Option<Self::BinaryDocValues>> {
    self.in_.get_binary_doc_values(field)
  }

  type SortedDocValues = LR::SortedDocValues;

  fn get_sorted_doc_values(&self, field: &str) -> Result<Option<Self::SortedDocValues>> {
    self.in_.get_sorted_doc_values(field)
  }

  type SortedNumericDocValues = LR::SortedNumericDocValues;

  fn get_sorted_numeric_doc_values(
    &self,
    field: &str,
  ) -> Result<Option<Self::SortedNumericDocValues>> {
    self.in_.get_sorted_numeric_doc_values(field)
  }

  type SortedSetDocValues = LR::SortedSetDocValues;

  fn get_sorted_set_doc_values(&self, field: &str) -> Result<Option<Self::SortedSetDocValues>> {
    self.in_.get_sorted_set_doc_values(field)
  }

  type NormNumericDocValues = LR::NormNumericDocValues;

  fn get_norm_values(&self, field: &str) -> Result<Option<Self::NormNumericDocValues>> {
    self.in_.get_norm_values(field)
  }

  type DocValuesSkipper = LR::DocValuesSkipper;

  fn get_doc_values_skipper(&self, field: &str) -> Result<Option<Self::DocValuesSkipper>> {
    self.in_.get_doc_values_skipper(field)
  }

  type FloatVectorValues = LR::FloatVectorValues;

  fn get_float_vector_values(&self, field: &str) -> Result<Option<Self::FloatVectorValues>> {
    self.in_.get_float_vector_values(field)
  }

  type ByteVectorValues = LR::ByteVectorValues;

  fn get_byte_vector_values(&self, field: &str) -> Result<Option<Self::ByteVectorValues>> {
    self.in_.get_byte_vector_values(field)
  }

  fn search_nearest_vectors_f32<B, K, TV>(
    &self,
    field: &str,
    target: TV,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
    TV: Into<Vec<f32>>,
  {
    let target = target.into();
    self
      .in_
      .search_nearest_vectors_f32(field, target, knn_collector, accept_docs)
  }

  fn search_nearest_vectors_u8<B, K, TV>(
    &self,
    field: &str,
    target: TV,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
    TV: Into<Vec<u8>>,
  {
    let target = target.into();
    self
      .in_
      .search_nearest_vectors_u8(field, target, knn_collector, accept_docs)
  }

  fn get_field_infos(&self) -> Result<Arc<FieldInfos>> {
    self.in_.get_field_infos()
  }

  type Bits = LR::Bits;

  fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
    Ok(None)
  }

  type PointValues = LR::PointValues;

  fn get_point_values(&self, field: &str) -> Result<Option<Self::PointValues>> {
    self.in_.get_point_values(field)
  }

  fn check_integrity(&self) -> Result<()> {
    self.in_.check_integrity()
  }

  fn get_metadata(&self) -> Result<&LeafMetaData> {
    self.in_.get_metadata()
  }
}

struct IncludeSoftDeletesFilterLeafReader<D>
where
  D: Directory,
{
  in_: DefaultLeafReader<D>,
  hard_live_docs: Option<<DefaultLeafReader<D> as LeafReader>::Bits>,
  num_docs: i32,
  index_base: Arc<IndexReaderBase>,
}

impl<D> IncludeSoftDeletesFilterLeafReader<D>
where
  D: Directory,
{
  fn new(in_: DefaultLeafReader<D>) -> Result<Self> {
    let index_base = Arc::new(IndexReaderBase::new());
    in_.register_parent_reader(index_base.as_ref())?;
    let hard_live_docs = in_.get_hard_live_docs()?;
    let num_docs = if let Some(hard_live_docs) = &hard_live_docs {
      let mut bits = 0;
      for doc_id in 0..hard_live_docs.length() {
        if hard_live_docs.get(doc_id)? {
          bits += 1;
        }
      }
      bits
    } else {
      in_.max_doc()?
    };
    Ok(Self {
      in_,
      hard_live_docs,
      num_docs,
      index_base,
    })
  }
}

impl<D> Clone for IncludeSoftDeletesFilterLeafReader<D>
where
  D: Directory,
{
  fn clone(&self) -> Self {
    Self {
      in_: self.in_.clone(),
      hard_live_docs: self.hard_live_docs.clone(),
      num_docs: self.num_docs,
      index_base: self.index_base.clone(),
    }
  }
}

impl<D> Display for IncludeSoftDeletesFilterLeafReader<D>
where
  D: Directory,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "IncludeSoftDeletesFilterLeafReader({})", self.in_)
  }
}

impl<D> IndexReader for IncludeSoftDeletesFilterLeafReader<D>
where
  D: Directory,
{
  type ContextKind = LeafReaderContextKind;
  type TermVectors = <DefaultLeafReader<D> as IndexReader>::TermVectors;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    self.in_.term_vectors()
  }

  fn max_doc(&self) -> Result<i32> {
    self.in_.max_doc()
  }

  fn num_docs(&self) -> Result<i32> {
    Ok(self.num_docs)
  }

  type StoredFields = <DefaultLeafReader<D> as IndexReader>::StoredFields;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    self.in_.stored_fields()
  }

  fn do_close(&self) -> Result<()> {
    self.in_.close()
  }

  type ReaderCacheHelper = <DefaultLeafReader<D> as IndexReader>::ReaderCacheHelper;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    Ok(None)
  }

  fn doc_freq(&self, term: &Term) -> Result<i32> {
    IndexReader::doc_freq(&self.in_, term)
  }

  fn total_term_freq(&self, term: &Term) -> Result<i64> {
    self.in_.total_term_freq(term)
  }

  fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
    IndexReader::get_sum_doc_freq(&self.in_, field)
  }

  fn get_doc_count(&self, field: &str) -> Result<i32> {
    IndexReader::get_doc_count(&self.in_, field)
  }

  fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
    IndexReader::get_sum_total_term_freq(&self.in_, field)
  }

  fn index_base(&self) -> &IndexReaderBase {
    self.index_base.as_ref()
  }
}

impl<D> LeafReader for IncludeSoftDeletesFilterLeafReader<D>
where
  D: Directory,
{
  type CacheHelper = <DefaultLeafReader<D> as LeafReader>::CacheHelper;

  fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
    Ok(None)
  }

  type Terms = <DefaultLeafReader<D> as LeafReader>::Terms;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    self.in_.terms(field)
  }

  type NumericDocValues = <DefaultLeafReader<D> as LeafReader>::NumericDocValues;

  fn get_numeric_doc_values(&self, field: &str) -> Result<Option<Self::NumericDocValues>> {
    self.in_.get_numeric_doc_values(field)
  }

  type BinaryDocValues = <DefaultLeafReader<D> as LeafReader>::BinaryDocValues;

  fn get_binary_doc_values(&self, field: &str) -> Result<Option<Self::BinaryDocValues>> {
    self.in_.get_binary_doc_values(field)
  }

  type SortedDocValues = <DefaultLeafReader<D> as LeafReader>::SortedDocValues;

  fn get_sorted_doc_values(&self, field: &str) -> Result<Option<Self::SortedDocValues>> {
    self.in_.get_sorted_doc_values(field)
  }

  type SortedNumericDocValues = <DefaultLeafReader<D> as LeafReader>::SortedNumericDocValues;

  fn get_sorted_numeric_doc_values(
    &self,
    field: &str,
  ) -> Result<Option<Self::SortedNumericDocValues>> {
    self.in_.get_sorted_numeric_doc_values(field)
  }

  type SortedSetDocValues = <DefaultLeafReader<D> as LeafReader>::SortedSetDocValues;

  fn get_sorted_set_doc_values(&self, field: &str) -> Result<Option<Self::SortedSetDocValues>> {
    self.in_.get_sorted_set_doc_values(field)
  }

  type NormNumericDocValues = <DefaultLeafReader<D> as LeafReader>::NormNumericDocValues;

  fn get_norm_values(&self, field: &str) -> Result<Option<Self::NormNumericDocValues>> {
    self.in_.get_norm_values(field)
  }

  type DocValuesSkipper = <DefaultLeafReader<D> as LeafReader>::DocValuesSkipper;

  fn get_doc_values_skipper(&self, field: &str) -> Result<Option<Self::DocValuesSkipper>> {
    self.in_.get_doc_values_skipper(field)
  }

  type FloatVectorValues = <DefaultLeafReader<D> as LeafReader>::FloatVectorValues;

  fn get_float_vector_values(&self, field: &str) -> Result<Option<Self::FloatVectorValues>> {
    self.in_.get_float_vector_values(field)
  }

  type ByteVectorValues = <DefaultLeafReader<D> as LeafReader>::ByteVectorValues;

  fn get_byte_vector_values(&self, field: &str) -> Result<Option<Self::ByteVectorValues>> {
    self.in_.get_byte_vector_values(field)
  }

  fn search_nearest_vectors_f32<B, K, TV>(
    &self,
    field: &str,
    target: TV,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
    TV: Into<Vec<f32>>,
  {
    let target = target.into();
    self
      .in_
      .search_nearest_vectors_f32(field, target, knn_collector, accept_docs)
  }

  fn search_nearest_vectors_u8<B, K, TV>(
    &self,
    field: &str,
    target: TV,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
    TV: Into<Vec<u8>>,
  {
    let target = target.into();
    self
      .in_
      .search_nearest_vectors_u8(field, target, knn_collector, accept_docs)
  }

  fn get_field_infos(&self) -> Result<Arc<FieldInfos>> {
    self.in_.get_field_infos()
  }

  type Bits = <DefaultLeafReader<D> as LeafReader>::Bits;

  fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
    Ok(self.hard_live_docs.clone())
  }

  type PointValues = <DefaultLeafReader<D> as LeafReader>::PointValues;

  fn get_point_values(&self, field: &str) -> Result<Option<Self::PointValues>> {
    self.in_.get_point_values(field)
  }

  fn check_integrity(&self) -> Result<()> {
    self.in_.check_integrity()
  }

  fn get_metadata(&self) -> Result<&LeafMetaData> {
    self.in_.get_metadata()
  }
}

struct IncludeSoftDeletesSubReaderWrapper;

impl<D> SubReaderWrapper<DefaultLeafReader<D>> for IncludeSoftDeletesSubReaderWrapper
where
  D: Directory,
{
  type LeafReader1 = Self::LeafReader2;

  fn wrap_readers(&self, readers: &[DefaultLeafReader<D>]) -> Result<Vec<Self::LeafReader1>> {
    self.default_wrap_readers(readers.to_vec())
  }

  type LeafReader2 = IncludeSoftDeletesFilterLeafReader<D>;

  fn wrap(&self, reader: DefaultLeafReader<D>) -> Result<Self::LeafReader2> {
    IncludeSoftDeletesFilterLeafReader::new(reader)
  }
}

struct IncludeSoftDeletesWrapper<D>
where
  D: Directory,
{
  in_: Arc<StandardDirectoryReader<D>>,
  base: BaseCompositeReaderBase<IncludeSoftDeletesFilterLeafReader<D>>,
  index_base: IndexReaderBase,
}

impl<D> IncludeSoftDeletesWrapper<D>
where
  D: Directory + 'static,
{
  fn new(in_: Arc<StandardDirectoryReader<D>>) -> Result<Self> {
    let wrapper = IncludeSoftDeletesSubReaderWrapper;
    let readers = wrapper.wrap_readers(in_.get_sequential_sub_readers())?;
    let index_base = IndexReaderBase::new();
    let base = BaseCompositeReaderBase::new::<DummyComparator>(readers, None, &index_base)?;
    Ok(Self {
      in_,
      base,
      index_base,
    })
  }
}

impl<D> BaseCompositeReader for IncludeSoftDeletesWrapper<D> where D: Directory + 'static {}

impl<D> CompositeReader for IncludeSoftDeletesWrapper<D>
where
  D: Directory + 'static,
{
  type LeafReader = IncludeSoftDeletesFilterLeafReader<D>;
  type SubReader = Self::LeafReader;

  fn get_sequential_sub_readers(&self) -> &[Self::SubReader] {
    self.base.get_sequential_sub_readers()
  }

  fn visit_leaves<F>(&self, visitor: &mut F) -> Result<()>
  where
    F: FnMut(&Self::LeafReader) -> Result<()>,
  {
    for reader in self.get_sequential_sub_readers() {
      visitor(reader)?;
    }
    Ok(())
  }

  fn to_string(&self) -> String {
    format!("IncludeSoftDeletesWrapper({})", self.in_)
  }
}

impl<D> IndexReader for IncludeSoftDeletesWrapper<D>
where
  D: Directory + 'static,
{
  type ContextKind = CompositeReaderContextKind;
  type TermVectors = BCRTermVectorsImpl<<Self as CompositeReader>::LeafReader>;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    self.base.term_vector(self)
  }

  fn max_doc(&self) -> Result<i32> {
    Ok(self.base.max_doc())
  }

  fn num_docs(&self) -> Result<i32> {
    self.base.num_docs()
  }

  type StoredFields = BCRStoredFieldsImpl<<Self as CompositeReader>::LeafReader>;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    self.base.stored_fields(self)
  }

  fn do_close(&self) -> Result<()> {
    self.in_.close()
  }

  type ReaderCacheHelper = <StandardDirectoryReader<D> as IndexReader>::ReaderCacheHelper;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    Ok(None)
  }

  fn doc_freq(&self, term: &Term) -> Result<i32> {
    self.base.doc_freq(term, self)
  }

  fn total_term_freq(&self, term: &Term) -> Result<i64> {
    self.base.total_term_freq(term, self)
  }

  fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
    self.base.get_sum_doc_freq(field, self)
  }

  fn get_doc_count(&self, field: &str) -> Result<i32> {
    self.base.get_doc_count(field, self)
  }

  fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
    self.base.get_sum_total_term_freq(field, self)
  }

  fn index_base(&self) -> &IndexReaderBase {
    &self.index_base
  }
}

impl<D> Display for IncludeSoftDeletesWrapper<D>
where
  D: Directory + 'static,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", CompositeReader::to_string(self))
  }
}

impl<D> DirectoryReader for IncludeSoftDeletesWrapper<D>
where
  D: Directory + 'static,
{
  type DirectoryReader = Self;

  fn do_open_if_changed(&self) -> Result<Option<Self::DirectoryReader>> {
    self
      .in_
      .do_open_if_changed()?
      .map(|reader| Self::new(Arc::new(reader)))
      .transpose()
  }

  fn do_open_if_changed_with_commit<IC>(
    &self,
    commit: Option<&IC>,
  ) -> Result<Option<Self::DirectoryReader>>
  where
    IC: crate::core::index::index_commit::IndexCommit<Directory = Arc<Self::Directory>>,
  {
    self
      .in_
      .do_open_if_changed_with_commit(commit)?
      .map(|reader| Self::new(Arc::new(reader)))
      .transpose()
  }

  fn do_open_if_changed_with_deletes(
    &self,
    writer: &Arc<IndexWriter<Self::Directory>>,
    apply_deletes: bool,
  ) -> Result<Option<Self::DirectoryReader>> {
    self
      .in_
      .do_open_if_changed_with_deletes(writer, apply_deletes)?
      .map(|reader| Self::new(Arc::new(reader)))
      .transpose()
  }

  fn get_version(&self) -> Result<i64> {
    self.in_.get_version()
  }

  fn is_current(&self) -> Result<bool> {
    self.in_.is_current()
  }

  type IndexCommit = <StandardDirectoryReader<D> as DirectoryReader>::IndexCommit;

  fn get_index_commit(&self) -> Result<Self::IndexCommit> {
    self.in_.get_index_commit()
  }

  type Directory = D;

  fn directory(&self) -> &DirectoryReaderBase<Self::Directory> {
    self.in_.directory()
  }
}

impl<D> FilterDirectoryReader for IncludeSoftDeletesWrapper<D>
where
  D: Directory + 'static,
{
  type Delegate = Arc<StandardDirectoryReader<D>>;

  fn get_delegate(&self) -> &Self::Delegate {
    &self.in_
  }

  type WrapDirectoryReader = Self;

  fn do_wrap_directory_reader(
    &self,
    in_: Option<<Self::Delegate as DirectoryReader>::DirectoryReader>,
  ) -> Result<Option<Self::WrapDirectoryReader>> {
    in_.map(|reader| Self::new(Arc::new(reader))).transpose()
  }
}
