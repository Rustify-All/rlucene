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
#[cfg(feature = "nightly")]
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::stored_field::StoredField;
use crate::core::document::string_field::StringField;
use crate::core::index::directory_reader;
use crate::core::index::index_file_deleter::CommitPoint;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::indexable_field::IndexableField;
#[cfg(feature = "nightly")]
use crate::core::index::multi_doc_values::MultiDocValues;
use crate::core::index::no_deletion_policy::NoDeletionPolicy;
#[cfg(feature = "nightly")]
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::segment_infos::SegmentInfos;
#[cfg(feature = "nightly")]
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term::Term;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
#[cfg(feature = "nightly")]
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::query::Query;
use crate::core::search::term_query::TermQuery;
use crate::core::store::directory::Directory;
use crate::core::util::dummy::dummy_comparator::DummyComparator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::number::Number;
#[cfg(feature = "nightly")]
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, new_directory_shared, new_index_writer_config, new_searcher_with_reader, random,
  random_from_seed,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::RngExt;
#[cfg(feature = "nightly")]
use std::collections::HashSet;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Barrier, Mutex};
use std::thread;

#[allow(dead_code)] // for quick search
pub struct TestIndexingSequenceNumbers;

#[test]
fn test_basic() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let w = IndexWriter::new(dir, new_index_writer_config(&mut random)?)?;
  let a = w.add_document(Document::new())?;
  let b = w.add_document(Document::new())?;
  assert!(b > a);
  w.close()?;
  Ok(())
}

#[test]
fn test_after_refresh() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let w = IndexWriter::new(dir, new_index_writer_config(&mut random)?)?;
  let a = w.add_document(Document::new())?;
  directory_reader::open_from_writer(&w)?.close()?;
  let b = w.add_document(Document::new())?;
  assert!(b > a);
  w.close()?;
  Ok(())
}

#[test]
fn test_after_commit() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let w = IndexWriter::new(dir, new_index_writer_config(&mut random)?)?;
  let a = w.add_document(Document::new())?;
  w.commit()?;
  let b = w.add_document(Document::new())?;
  assert!(b > a);
  w.close()?;
  Ok(())
}

#[cfg(feature = "nightly")]
#[test]
#[ignore = "nightly"]
fn test_stress_update_same_id() -> Result<()> {
  let mut random = random();
  let iters = at_least(&mut random, 100);
  for _ in 0..iters {
    let dir = new_directory_shared(&mut random)?;
    let w = Arc::new(RandomIndexWriter::new(&mut random, dir)?);
    let num_threads = TestUtil::next_usize(&mut random, 2, 5);
    let starting_gun = Arc::new(Barrier::new(num_threads + 1));
    let seq_nos = Arc::new(Mutex::new(vec![0_i64; num_threads]));
    let id = Term::from_text("id", "id");

    thread::scope(|scope| -> Result<()> {
      let mut handles = Vec::new();
      for thread_id in 0..num_threads {
        let w = &w;
        let starting_gun = &starting_gun;
        let seed = random.random();
        let seq_nos = &seq_nos;
        let id = id.clone();
        handles.push(scope.spawn(move || -> Result<()> {
          let mut doc = Document::new();
          doc.add(StoredField::from_i32("thread", thread_id as i32)?);
          doc.add(StringField::from_string("id", "id", Store::No)?);
          let mut r = random_from_seed(seed);
          starting_gun.wait();
          for _ in 0..100 {
            let seq_no = w.update_document_with_term(&mut r, id.clone(), &mut doc)?;
            seq_nos.lock().unwrap()[thread_id] = seq_no;
          }
          Ok(())
        }));
      }

      starting_gun.wait();
      for handle in handles {
        handle.join().expect("thread panicked")?;
      }
      Ok(())
    })?;

    let seq_nos = seq_nos.lock().unwrap();
    let mut max_thread = 0;
    let mut all_seq_nos = HashSet::new();
    for i in 0..num_threads {
      all_seq_nos.insert(seq_nos[i]);
      if seq_nos[i] > seq_nos[max_thread] {
        max_thread = i;
      }
    }
    assert_eq!(num_threads, all_seq_nos.len());

    let r = w.get_reader(&mut random)?;
    let max_doc = r.max_doc()?;
    let s = new_searcher_with_reader(r)?;
    let hits = s.search(TermQuery::new(id), 1)?;
    assert_eq!(1, hits.total_hits.value(), "maxDoc: {}", max_doc);
    let doc = s.stored_fields()?.document(hits.score_docs[0].doc)?;
    assert_eq!(max_thread as i32, get_i32_field(&doc, "thread")?);
    w.close(&mut random)?;
  }
  Ok(())
}

#[cfg(feature = "nightly")]
#[derive(Clone, Debug, Default)]
struct Operation {
  // 0 = update, 1 = delete, 2 = commit, 3 = add
  what: u8,
  id: usize,
  seq_no: i64,
}

#[cfg(feature = "nightly")]
#[test]
#[ignore = "nightly"]
fn test_stress_concurrent_commit() -> Result<()> {
  let mut random = random();
  let op_count = at_least(&mut random, 10000);
  let id_count = TestUtil::next_usize(&mut random, 10, 1000);

  let dir = new_directory_shared(&mut random)?;
  let mut iwc = new_index_writer_config(&mut random)?;
  iwc.set_index_deletion_policy(NoDeletionPolicy);
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let num_threads = TestUtil::next_usize(&mut random, 2, 10);
  let starting_gun = Arc::new(Barrier::new(num_threads + 1));
  let thread_ops = (0..num_threads)
    .map(|_| Mutex::new(Vec::new()))
    .collect::<Vec<_>>();
  let commit_lock = Arc::new(Mutex::new(()));
  let commits = Arc::new(Mutex::new(Vec::new()));

  thread::scope(|scope| -> Result<()> {
    let mut handles = Vec::new();
    for (thread_id, ops) in thread_ops.iter().enumerate() {
      let seed = random.random();
      let w = w.clone();
      let starting_gun = starting_gun.clone();
      let commit_lock = commit_lock.clone();
      let commits = commits.clone();
      handles.push(scope.spawn(move || -> Result<()> {
        let mut random = random_from_seed(seed);
        starting_gun.wait();
        for _ in 0..op_count {
          let mut op = Operation::default();
          if random.random_range(0..500) == 17 {
            op.what = 2;
            let _guard = commit_lock.lock().unwrap();
            op.seq_no = w.commit()?;
            if op.seq_no != -1 {
              commits.lock().unwrap().push(op);
            }
          } else {
            op.id = random.random_range(0..id_count);
            let id_term = Term::from_text("id", op.id.to_string());
            if random.random_range(0..10) == 1 {
              op.what = 1;
              op.seq_no = if random.random_bool(0.5) {
                w.delete_documents_with_terms(vec![id_term])?
              } else {
                w.delete_documents_with_queries(vec![Query::from(TermQuery::new(id_term))])?
              };
            } else {
              let mut doc = Document::new();
              doc.add(StoredField::from_i32("thread", thread_id as i32)?);
              doc.add(StringField::from_string(
                "id",
                op.id.to_string(),
                Store::No,
              )?);
              op.seq_no = w.update_document_with_term(id_term, doc)?;
              op.what = 0;
            }
            ops.lock().unwrap().push(op);
          }
        }
        Ok(())
      }));
    }

    starting_gun.wait();
    for handle in handles {
      handle.join().expect("thread panicked")?;
    }
    Ok(())
  })?;

  let commit_op = Operation {
    seq_no: w.commit()?,
    ..Default::default()
  };
  if commit_op.seq_no != -1 {
    commits.lock().unwrap().push(commit_op);
  }

  let index_commits = list_commits(dir.clone())?;
  let commits = std::mem::take(&mut *commits.lock().unwrap());
  assert_eq!(commits.len(), index_commits.len());

  let mut expected_thread_ids = vec![-1; id_count];
  let mut seq_nos = vec![0_i64; id_count];

  for (i, commit) in commits.iter().enumerate() {
    let commit_seq_no = commit.seq_no;
    expected_thread_ids.fill(-1);
    seq_nos.fill(0);

    for (thread_id, ops) in thread_ops.iter().enumerate() {
      let mut last_seq_no = 0;
      for op in ops.lock().unwrap().iter() {
        if op.seq_no <= commit_seq_no && op.seq_no > seq_nos[op.id] {
          seq_nos[op.id] = op.seq_no;
          if op.what == 0 {
            expected_thread_ids[op.id] = thread_id as i32;
          } else {
            expected_thread_ids[op.id] = -1;
          }
        }
        assert!(op.seq_no > last_seq_no);
        last_seq_no = op.seq_no;
      }
    }

    let r = directory_reader::open_from_commit::<_, _>(&index_commits[i])?;
    let s = new_searcher_with_reader(r)?;
    for (id, expected_thread_id) in expected_thread_ids.iter().enumerate() {
      let hits = s.search(TermQuery::new(Term::from_text("id", id.to_string())), 1)?;
      if *expected_thread_id != -1 {
        assert_eq!(1, hits.total_hits.value());
        let doc = s.stored_fields()?.document(hits.score_docs[0].doc)?;
        assert_eq!(
          *expected_thread_id,
          get_i32_field(&doc, "thread")?,
          "id={id}"
        );
      } else {
        assert_eq!(0, hits.total_hits.value());
      }
    }
  }
  w.close()?;
  Ok(())
}

#[cfg(feature = "nightly")]
#[test]
#[ignore = "nightly"]
fn test_stress_concurrent_doc_values_updates_commit() -> Result<()> {
  let mut random = random();
  let op_count = at_least(&mut random, 10000);
  let id_count = TestUtil::next_usize(&mut random, 10, 1000);

  let dir = new_directory_shared(&mut random)?;
  let mut iwc = new_index_writer_config(&mut random)?;
  iwc.set_index_deletion_policy(NoDeletionPolicy);
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let num_threads = TestUtil::next_usize(&mut random, 2, 10);
  let starting_gun = Arc::new(Barrier::new(num_threads + 1));
  let thread_ops = (0..num_threads)
    .map(|_| Mutex::new(Vec::new()))
    .collect::<Vec<_>>();
  let commit_lock = Arc::new(Mutex::new(()));
  let commits = Arc::new(Mutex::new(Vec::new()));

  for id in 0..id_count {
    let mut op = Operation {
      id,
      ..Default::default()
    };
    let mut doc = Document::new();
    doc.add(StoredField::from_i32("thread", 0)?);
    doc.add(NumericDocValuesField::new("thread", 0));
    doc.add(StringField::from_string("id", id.to_string(), Store::No)?);
    op.seq_no = w.add_document(doc)?;
    thread_ops[0].lock().unwrap().push(op);
  }

  thread::scope(|scope| -> Result<()> {
    let mut handles = Vec::new();
    for (thread_id, ops) in thread_ops.iter().enumerate() {
      let seed = random.random();
      let w = w.clone();
      let starting_gun = starting_gun.clone();
      let commit_lock = commit_lock.clone();
      let commits = commits.clone();
      handles.push(scope.spawn(move || -> Result<()> {
        let mut random = random_from_seed(seed);
        starting_gun.wait();
        for _ in 0..op_count {
          let mut op = Operation::default();
          if random.random_range(0..500) == 17 {
            op.what = 2;
            let _guard = commit_lock.lock().unwrap();
            op.seq_no = w.commit()?;
            if op.seq_no != -1 {
              commits.lock().unwrap().push(op);
            }
          } else {
            op.id = random.random_range(0..id_count);
            op.seq_no = w.update_numeric_doc_value(
              Term::from_text("id", op.id.to_string()),
              "thread",
              thread_id as i64,
            )?;
            op.what = 0;
            ops.lock().unwrap().push(op);
          }
        }
        Ok(())
      }));
    }

    starting_gun.wait();
    for handle in handles {
      handle.join().expect("thread panicked")?;
    }
    Ok(())
  })?;

  let commit_op = Operation {
    seq_no: w.commit()?,
    ..Default::default()
  };
  if commit_op.seq_no != -1 {
    commits.lock().unwrap().push(commit_op);
  }

  let index_commits = list_commits(dir.clone())?;
  let commits = std::mem::take(&mut *commits.lock().unwrap());
  assert_eq!(commits.len(), index_commits.len());

  let mut expected_thread_ids = vec![-1; id_count];
  let mut seq_nos = vec![0_i64; id_count];

  for (i, commit) in commits.iter().enumerate() {
    let commit_seq_no = commit.seq_no;
    expected_thread_ids.fill(-1);
    seq_nos.fill(0);

    for (thread_id, ops) in thread_ops.iter().enumerate() {
      let mut last_seq_no = 0;
      for op in ops.lock().unwrap().iter() {
        if op.seq_no <= commit_seq_no && op.seq_no > seq_nos[op.id] {
          seq_nos[op.id] = op.seq_no;
          assert_eq!(0, op.what);
          expected_thread_ids[op.id] = thread_id as i32;
        }
        assert!(op.seq_no > last_seq_no);
        last_seq_no = op.seq_no;
      }
    }

    let r = directory_reader::open_from_commit::<_, _>(&index_commits[i])?;
    let doc_values_reader = directory_reader::open_from_commit::<_, _>(&index_commits[i])?;
    let s = new_searcher_with_reader(r)?;
    let mut doc_values = MultiDocValues::get_numeric_values(doc_values_reader, "thread")?
      .ok_or_else(|| LuceneError::illegal_state("missing thread doc values"))?;
    for (id, expected_thread_id) in expected_thread_ids.iter().enumerate() {
      assert_ne!(-1, *expected_thread_id);
      let hits = s.search(TermQuery::new(Term::from_text("id", id.to_string())), 1)?;
      assert_eq!(1, hits.total_hits.value());
      let hit_doc = hits.score_docs[0].doc;
      assert_eq!(hit_doc, doc_values.advance(hit_doc)?);
      assert_eq!(
        *expected_thread_id as i64,
        doc_values.long_value()?,
        "id={id} docID={hit_doc}",
      );
    }
  }
  w.close()?;
  Ok(())
}

#[cfg(feature = "nightly")]
#[test]
#[ignore = "nightly"]
fn test_stress_concurrent_add_and_delete_and_commit() -> Result<()> {
  let mut random = random();
  let op_count = at_least(&mut random, 10000);
  let id_count = TestUtil::next_usize(&mut random, 10, 1000);

  let dir = new_directory_shared(&mut random)?;
  let mut iwc = new_index_writer_config(&mut random)?;
  iwc.set_index_deletion_policy(NoDeletionPolicy);
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let num_threads = TestUtil::next_usize(&mut random, 2, 5);
  let starting_gun = Arc::new(Barrier::new(num_threads + 1));
  let thread_ops = (0..num_threads)
    .map(|_| Mutex::new(Vec::new()))
    .collect::<Vec<_>>();
  let commit_lock = Arc::new(Mutex::new(()));
  let commits = Arc::new(Mutex::new(Vec::new()));

  thread::scope(|scope| -> Result<()> {
    let mut handles = Vec::new();
    for (thread_id, ops) in thread_ops.iter().enumerate() {
      let seed = random.random();
      let w = w.clone();
      let starting_gun = starting_gun.clone();
      let commit_lock = commit_lock.clone();
      let commits = commits.clone();
      handles.push(scope.spawn(move || -> Result<()> {
        let mut random = random_from_seed(seed);
        starting_gun.wait();
        for _ in 0..op_count {
          let mut op = Operation::default();
          if random.random_range(0..500) == 17 {
            op.what = 2;
            let _guard = commit_lock.lock().unwrap();
            op.seq_no = w.commit()?;
            if op.seq_no != -1 {
              commits.lock().unwrap().push(op);
            }
          } else {
            op.id = random.random_range(0..id_count);
            let id_term = Term::from_text("id", op.id.to_string());
            if random.random_range(0..10) == 1 {
              op.what = 1;
              op.seq_no = if random.random_bool(0.5) {
                w.delete_documents_with_terms(vec![id_term])?
              } else {
                w.delete_documents_with_queries(vec![Query::from(TermQuery::new(id_term))])?
              };
            } else {
              let thread_op = format!("{}-{}", thread_id, ops.lock().unwrap().len());
              let mut doc = Document::new();
              doc.add(StoredField::from_string("threadop", thread_op)?);
              doc.add(StringField::from_string(
                "id",
                op.id.to_string(),
                Store::No,
              )?);
              op.seq_no = if random.random_bool(0.5) {
                w.add_documents(vec![doc])?
              } else {
                w.add_document(doc)?
              };
              op.what = 3;
            }
            ops.lock().unwrap().push(op);
          }
        }
        Ok(())
      }));
    }

    starting_gun.wait();
    for handle in handles {
      handle.join().expect("thread panicked")?;
    }
    Ok(())
  })?;

  let commit_op = Operation {
    seq_no: w.commit()?,
    ..Default::default()
  };
  if commit_op.seq_no != -1 {
    commits.lock().unwrap().push(commit_op);
  }

  let index_commits = list_commits(dir.clone())?;
  let commits = std::mem::take(&mut *commits.lock().unwrap());
  assert_eq!(commits.len(), index_commits.len());

  let mut expected_counts = vec![0; id_count];
  let mut last_del_seq_nos = vec![-1_i64; id_count];

  for (i, commit) in commits.iter().enumerate() {
    let commit_seq_no = commit.seq_no;
    last_del_seq_nos.fill(-1);

    for ops in &thread_ops {
      let mut last_seq_no = 0;
      for op in ops.lock().unwrap().iter() {
        if op.what == 1 && op.seq_no <= commit_seq_no && op.seq_no > last_del_seq_nos[op.id] {
          last_del_seq_nos[op.id] = op.seq_no;
        }
        assert!(op.seq_no > last_seq_no);
        last_seq_no = op.seq_no;
      }
    }

    expected_counts.fill(0);
    for ops in &thread_ops {
      for op in ops.lock().unwrap().iter() {
        if op.what == 3 && op.seq_no <= commit_seq_no && op.seq_no > last_del_seq_nos[op.id] {
          expected_counts[op.id] += 1;
        }
      }
    }

    let r = directory_reader::open_from_commit::<_, _>(&index_commits[i])?;
    let s = new_searcher_with_reader(r)?;
    for (id, expected_count) in expected_counts.iter().enumerate() {
      let actual_count = s.count(TermQuery::new(Term::from_text("id", id.to_string())))?;
      assert_eq!(*expected_count, actual_count as i32, "commit {i} id={id}");
    }
  }
  w.close()?;
  Ok(())
}

#[test]
fn test_delete_all() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let w = IndexWriter::new(dir, new_index_writer_config(&mut random)?)?;
  let a = w.add_document(Document::new())?;
  let b = w.delete_all()?;
  assert!(a < b);
  let c = w.commit()?;
  assert!(b < c);
  w.close()?;
  Ok(())
}

#[cfg(feature = "nightly")]
fn get_i32_field(doc: &Document, name: &str) -> Result<i32> {
  match doc
    .get_field(name)
    .ok_or_else(|| LuceneError::illegal_state(format!("missing field {name}")))?
    .numeric_value()?
    .ok_or_else(|| LuceneError::illegal_state(format!("missing numeric value for {name}")))?
  {
    Number::I32(value) => Ok(value),
    value => Err(LuceneError::illegal_state(format!(
      "expected i32 field {name}, got {value:?}",
    ))),
  }
}

#[cfg(feature = "nightly")]
fn list_commits<D>(dir: Arc<D>) -> Result<Vec<CommitPoint<D>>>
where
  D: Directory,
{
  let commits_to_delete = Arc::new(AtomicBool::new(false));
  let mut commits = Vec::new();
  for file in dir.list_all()? {
    if file == "segments.gen" || !file.starts_with("segments_") {
      continue;
    }
    let sis = SegmentInfos::read_commit(dir.clone(), &file)?;
    commits.push(CommitPoint::new(
      commits_to_delete.clone(),
      dir.clone(),
      &sis,
      None,
    )?);
  }
  commits.sort_unstable();
  Ok(commits)
}
