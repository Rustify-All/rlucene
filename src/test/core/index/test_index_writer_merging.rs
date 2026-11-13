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
use crate::core::document::field::FieldBase;
use crate::core::document::field::{Field, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::fields::Fields;
use crate::core::document::string_field::StringField;
use crate::core::index::directory_reader;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::{DISABLE_AUTO_FLUSH, OpenMode};
use crate::core::index::indexable_field::IndexableField;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::log_merge_policy::LogMergePolicy;
use crate::core::index::merge_scheduler::MergeSchedulerEnum;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::index::serial_merge_scheduler::SerialMergeScheduler;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term::Term;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::store::directory::Directory;
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind, Result};
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::test_index_writer_merging::MyMergeScheduler;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, new_directory_shared, new_index_writer_config_with_analyzer, new_log_merge_policy,
  new_log_merge_policy_with_merge_factor, new_string_field, random,
};
use rand::Rng;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;

#[allow(dead_code)] // for quick search
pub struct TestIndexWriterMerging;

#[test]
fn test_lucene() -> Result<()> {
  let mut random = random();
  let num = 100;

  let index_a = new_directory_shared(&mut random)?;
  let index_b = new_directory_shared(&mut random)?;
  let mut field_types = HashMap::new();

  fill_index(&mut random, index_a.clone(), 0, num, &mut field_types)?;
  assert!(!verify_index(index_a.clone(), 0)?, "Index a is invalid");

  fill_index(&mut random, index_b.clone(), num, num, &mut field_types)?;
  assert!(!verify_index(index_b.clone(), num)?, "Index b is invalid");

  let merged = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_merge_policy(new_log_merge_policy_with_merge_factor(&mut random, 2)?);
  let writer = IndexWriter::new(merged.clone(), conf)?;
  writer.add_indexes_from_directory(&[index_a, index_b])?;
  writer.force_merge(1)?;
  writer.close()?;

  assert!(!verify_index(merged, 0)?, "The merged index is invalid");
  Ok(())
}

fn verify_index<D>(directory: Arc<D>, start_at: i32) -> Result<bool>
where
  D: Directory + 'static,
{
  let reader = directory_reader::open(directory)?;
  let max = reader.max_doc()?;
  let mut stored_fields = reader.stored_fields()?;
  let mut fail = false;

  for i in 0..max {
    let temp = stored_fields.document(i)?;
    let field = temp.get_field("count").expect("count field should exist");
    let value = field.string_value()?.expect("count should be stored");
    if value.as_ref() != (i + start_at).to_string() {
      fail = true;
      println!(
        "Document {} is returning document {}",
        i + start_at,
        value.as_ref()
      );
    }
  }
  reader.close()?;
  Ok(fail)
}

fn fill_index<D, R>(
  random: &mut R,
  dir: Arc<D>,
  start: i32,
  num_docs: i32,
  field_types: &mut HashMap<String, FieldType>,
) -> Result<()>
where
  D: Directory + 'static,
  R: Rng + ?Sized,
{
  let analyzer = MockAnalyzer::new(random);
  let mut conf = new_index_writer_config_with_analyzer(random, analyzer)?;
  conf.set_open_mode(OpenMode::Create);
  conf.set_max_buffered_docs(2);
  conf.set_merge_policy(new_log_merge_policy_with_merge_factor(random, 2)?);

  let writer = IndexWriter::new(dir, conf)?;
  for i in start..start + num_docs {
    let mut temp = Document::new();
    temp.add(new_string_field(
      random,
      "count",
      i.to_string(),
      Store::Yes,
      field_types,
    )?);
    writer.add_document(temp)?;
  }
  writer.close()?;
  Ok(())
}

#[test]
fn test_force_merge_deletes() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_max_buffered_docs(2);
  conf.set_ram_buffer_size_mb(DISABLE_AUTO_FLUSH as f64);
  let writer = IndexWriter::new(dir.clone(), conf)?;

  let mut stored_type = FieldType::new();
  stored_type.set_stored(true)?;

  let mut term_vector_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_STORED)?;
  term_vector_type.set_tokenized(false)?;
  term_vector_type.set_store_term_vectors(true)?;
  term_vector_type.set_store_term_vector_positions(true)?;
  term_vector_type.set_store_term_vector_offsets(true)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "", Store::No)?);
  doc.add(Field::new("stored", "stored", stored_type.clone()));
  doc.add(Field::new(
    "termVector",
    "termVector",
    term_vector_type.clone(),
  ));
  for i in 0..10 {
    let Some(Fields::String(id_field)) = doc.get_field_mut("id") else {
      unreachable!("id field must exist")
    };
    id_field.set_string_value(i.to_string())?;
    writer.add_document(&mut doc)?;
  }
  writer.close()?;
  drop(writer);

  let ir = directory_reader::open(dir.clone())?;
  assert_eq!(10, ir.max_doc()?);
  assert_eq!(10, ir.num_docs()?);
  ir.close()?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut dont_merge_config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  dont_merge_config.set_merge_policy(NoMergePolicy::default());
  let writer = IndexWriter::new(dir.clone(), dont_merge_config)?;
  writer.delete_documents_with_terms(vec![Term::from_text("id", "0")])?;
  writer.delete_documents_with_terms(vec![Term::from_text("id", "7")])?;
  writer.close()?;
  drop(writer);

  let ir = directory_reader::open(dir.clone())?;
  assert_eq!(8, ir.num_docs()?);
  ir.close()?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_merge_policy(new_log_merge_policy(&mut random)?);
  let writer = IndexWriter::new(dir.clone(), conf)?;
  assert_eq!(8, writer.get_doc_stats()?.num_docs);
  assert_eq!(10, writer.get_doc_stats()?.max_doc);
  writer.force_merge_deletes()?;
  assert_eq!(8, writer.get_doc_stats()?.num_docs);
  writer.close()?;

  let ir = directory_reader::open(dir)?;
  assert_eq!(8, ir.max_doc()?);
  assert_eq!(8, ir.num_docs()?);
  ir.close()?;
  Ok(())
}

#[test]
fn test_force_merge_deletes2() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_max_buffered_docs(2);
  conf.set_ram_buffer_size_mb(DISABLE_AUTO_FLUSH as f64);
  conf.set_merge_policy(new_log_merge_policy_with_merge_factor(&mut random, 50)?);
  let writer = IndexWriter::new(dir.clone(), conf)?;

  let mut stored_type = FieldType::new();
  stored_type.set_stored(true)?;

  let mut term_vector_type =
    FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  term_vector_type.set_tokenized(false)?;
  term_vector_type.set_store_term_vectors(true)?;
  term_vector_type.set_store_term_vector_positions(true)?;
  term_vector_type.set_store_term_vector_offsets(true)?;

  let mut doc = Document::new();
  doc.add(Field::new("stored", "stored", stored_type.clone()));
  doc.add(Field::new(
    "termVector",
    "termVector",
    term_vector_type.clone(),
  ));
  doc.add(StringField::from_string("id", "", Store::No)?);
  for i in 0..98 {
    let Some(Fields::String(id_field)) = doc.get_field_mut("id") else {
      unreachable!("id field must exist")
    };
    id_field.set_string_value(i.to_string())?;
    writer.add_document(&mut doc)?;
  }
  writer.close()?;
  drop(writer);

  let ir = directory_reader::open(dir.clone())?;
  assert_eq!(98, ir.max_doc()?);
  assert_eq!(98, ir.num_docs()?);
  ir.close()?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut dont_merge_config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  dont_merge_config.set_merge_policy(NoMergePolicy::default());
  let writer = IndexWriter::new(dir.clone(), dont_merge_config)?;
  for i in (0..98).step_by(2) {
    writer.delete_documents_with_terms(vec![Term::from_text("id", i.to_string())])?;
  }
  writer.close()?;
  drop(writer);

  let ir = directory_reader::open(dir.clone())?;
  assert_eq!(49, ir.num_docs()?);
  ir.close()?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_merge_policy(new_log_merge_policy_with_merge_factor(&mut random, 3)?);
  let writer = IndexWriter::new(dir.clone(), conf)?;
  assert_eq!(49, writer.get_doc_stats()?.num_docs);
  writer.force_merge_deletes()?;
  writer.close()?;

  let ir = directory_reader::open(dir)?;
  assert_eq!(49, ir.max_doc()?);
  assert_eq!(49, ir.num_docs()?);
  ir.close()?;
  Ok(())
}

#[test]
fn test_force_merge_deletes3() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_max_buffered_docs(2);
  conf.set_ram_buffer_size_mb(DISABLE_AUTO_FLUSH as f64);
  conf.set_merge_policy(new_log_merge_policy_with_merge_factor(&mut random, 50)?);
  let writer = IndexWriter::new(dir.clone(), conf)?;

  let mut stored_type = FieldType::new();
  stored_type.set_stored(true)?;

  let mut term_vector_type =
    FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  term_vector_type.set_tokenized(false)?;
  term_vector_type.set_store_term_vectors(true)?;
  term_vector_type.set_store_term_vector_positions(true)?;
  term_vector_type.set_store_term_vector_offsets(true)?;

  let mut doc = Document::new();
  doc.add(Field::new("stored", "stored", stored_type.clone()));
  doc.add(Field::new(
    "termVector",
    "termVector",
    term_vector_type.clone(),
  ));
  doc.add(StringField::from_string("id", "", Store::No)?);
  for i in 0..98 {
    let Some(Fields::String(id_field)) = doc.get_field_mut("id") else {
      unreachable!("id field must exist")
    };
    id_field.set_string_value(i.to_string())?;
    writer.add_document(&mut doc)?;
  }
  writer.close()?;
  drop(writer);

  let ir = directory_reader::open(dir.clone())?;
  assert_eq!(98, ir.max_doc()?);
  assert_eq!(98, ir.num_docs()?);
  ir.close()?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut dont_merge_config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  dont_merge_config.set_merge_policy(NoMergePolicy::default());
  let writer = IndexWriter::new(dir.clone(), dont_merge_config)?;
  for i in (0..98).step_by(2) {
    writer.delete_documents_with_terms(vec![Term::from_text("id", i.to_string())])?;
  }
  writer.close()?;
  drop(writer);

  let ir = directory_reader::open(dir.clone())?;
  assert_eq!(49, ir.num_docs()?);
  ir.close()?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_merge_policy(new_log_merge_policy_with_merge_factor(&mut random, 3)?);
  let writer = IndexWriter::new(dir.clone(), conf)?;
  writer.force_merge_deletes_with_wait(false)?;
  writer.close()?;

  let ir = directory_reader::open(dir)?;
  assert_eq!(49, ir.max_doc()?);
  assert_eq!(49, ir.num_docs()?);
  ir.close()?;
  Ok(())
}

#[test]
fn test_set_max_merge_docs() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_merge_scheduler(MergeSchedulerEnum::IndexWriterMerging(MyMergeScheduler));
  conf.set_max_buffered_docs(2);
  let mut lmp = new_log_merge_policy(&mut random)?;
  match &mut lmp {
    crate::core::index::merge_policy::MergePolicyEnum::LogDoc(lmp) => {
      lmp.set_max_merge_docs(20);
      lmp.set_merge_factor(2)?;
    },
    crate::core::index::merge_policy::MergePolicyEnum::LogBytesSize(lmp) => {
      lmp.set_max_merge_docs(20);
      lmp.set_merge_factor(2)?;
    },
    _ => {
      return Err(LuceneError::illegal_state(
        "expected LogMergePolicy variant",
      ));
    },
  }
  conf.set_merge_policy(lmp);

  let writer = IndexWriter::new(dir, conf)?;
  let mut custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  custom_type.set_tokenized(false)?;
  custom_type.set_store_term_vectors(true)?;

  let mut doc = Document::new();
  doc.add(Field::new("tvtest", "a b c", custom_type.clone()));
  for _ in 0..177 {
    writer.add_document(&mut doc)?;
  }
  writer.close()?;
  Ok(())
}

#[test]
fn test_no_wait_close() -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;

  let mut custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  custom_type.set_tokenized(false)?;

  let mut doc = Document::new();
  doc.add(Field::new("id", "", custom_type.clone()));
  for pass in 0..2 {
    if cfg!(feature = "test_log_verbose") {
      println!("TEST: pass={}", pass);
    }

    let analyzer = MockAnalyzer::new(&mut random);
    let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
    conf.set_open_mode(OpenMode::Create);
    conf.set_max_buffered_docs(2);
    conf.set_merge_policy(new_log_merge_policy(&mut random)?);
    conf.set_commit_on_close(false);
    if pass == 2 {
      conf.set_merge_scheduler(SerialMergeScheduler::new());
    }

    let mut writer = IndexWriter::new(directory.clone(), conf)?;
    match writer.get_config_mut().get_merge_policy_mut() {
      crate::core::index::merge_policy::MergePolicyEnum::LogDoc(mp) => mp.set_merge_factor(100)?,
      crate::core::index::merge_policy::MergePolicyEnum::LogBytesSize(mp) => {
        mp.set_merge_factor(100)?
      },
      _ => {
        return Err(LuceneError::illegal_state(
          "expected LogMergePolicy variant",
        ));
      },
    }

    for iter in 0..at_least(&mut random, 3) {
      if cfg!(feature = "test_log_verbose") {
        println!("TEST: iter={}", iter);
      }
      for j in 0..199 {
        let Some(Fields::Field(id_field)) = doc.get_field_mut("id") else {
          unreachable!("id field must exist")
        };
        id_field.set_string_value((iter * 201 + j).to_string())?;
        writer.add_document(&mut doc)?;
      }

      let mut del_id = iter * 199;
      for _ in 0..20 {
        writer.delete_documents_with_terms(vec![Term::from_text("id", del_id.to_string())])?;
        del_id += 5;
      }

      writer.commit()?;
      match writer.get_config_mut().get_merge_policy_mut() {
        crate::core::index::merge_policy::MergePolicyEnum::LogDoc(mp) => mp.set_merge_factor(2)?,
        crate::core::index::merge_policy::MergePolicyEnum::LogBytesSize(mp) => {
          mp.set_merge_factor(2)?
        },
        _ => {
          return Err(LuceneError::illegal_state(
            "expected LogMergePolicy variant",
          ));
        },
      }

      let failure = Arc::new(Mutex::new(None));
      thread::scope(|scope| {
        let writer_ref = &writer;
        let failure_ref = &failure;
        let doc_ref = &mut doc;
        let handle = scope.spawn(move || {
          let mut done = false;
          while !done {
            for _ in 0..100 {
              match (writer_ref.add_document(&mut *doc_ref)).map_err(LuceneError::into_kind) {
                Ok(_) => {},
                Err(LuceneErrorKind::AlreadyClosed(_)) | Err(LuceneErrorKind::IllegalState(_)) => {
                  done = true;
                  break;
                },
                Err(e) => {
                  *failure_ref.lock().unwrap() = Some(e);
                  done = true;
                  break;
                },
              }
            }
            thread::yield_now();
          }
        });

        writer.close()?;
        handle.join().expect("thread panicked");
        Ok::<(), LuceneError>(())
      })?;

      if let Some(e) = failure.lock().unwrap().take() {
        return Err(e.into());
      }

      let reader = directory_reader::open(directory.clone())?;
      reader.close()?;

      let analyzer = MockAnalyzer::new(&mut random);
      let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
      conf.set_open_mode(OpenMode::Append);
      conf.set_merge_policy(new_log_merge_policy(&mut random)?);
      conf.set_commit_on_close(false);
      drop(writer);
      writer = IndexWriter::new(directory.clone(), conf)?;
    }
    writer.close()?;
  }

  Ok(())
}
