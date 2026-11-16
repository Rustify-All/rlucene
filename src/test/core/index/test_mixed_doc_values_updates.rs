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
use crate::core::document::field::{Field, FieldDataEnum, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::fields::Fields;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::string_field::StringField;
use crate::core::index::binary_doc_values::BinaryDocValues;
use crate::core::index::directory_reader;
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term::Term;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::index::{BytesRef, BytesRefValue};
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, NO_MORE_DOCS};
use crate::core::search::field_exists_query::FieldExistsQuery;
use crate::core::search::term_query::TermQuery;
use crate::core::search::top_docs::TopDocsLike;
use crate::core::store::IndexInput;
use crate::core::store::directory::Directory;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind, Result};
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::test_binary_doc_values_updates::{get_value, to_bytes};
use crate::test_framework::core::util::DefaultCRReader;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, is_night_mode, new_directory_shared, new_index_writer_config,
  new_index_writer_config_with_analyzer, new_log_merge_policy_with_merge_factor,
  new_searcher_with_reader, random, random_from_seed, rarely,
};
use crate::test_framework::core::util::test_util::TestUtil;
use parking_lot::Mutex;
use rand::RngExt;
#[cfg(feature = "nightly")]
use rand::prelude::IndexedRandom;
#[cfg(feature = "nightly")]
use std::collections::HashSet;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

#[allow(dead_code)] // for quick search
pub struct TestMixedDocValuesUpdates;

#[test]
fn test_many_reopens_and_fields() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_merge_policy(new_log_merge_policy_with_merge_factor(&mut random, 3)?);
  let writer = IndexWriter::new(dir.clone(), conf)?;

  let is_nrt = random.random_bool(0.5);
  let mut reader = if is_nrt {
    directory_reader::open_from_writer(&writer)?
  } else {
    writer.commit()?;
    directory_reader::open(dir.clone())?
  };

  let num_fields = random.random_range(0..4) + 3;
  let num_ndv_fields = random.random_range(0..(num_fields / 2)) + 1;
  let mut field_values = vec![1_i64; num_fields];

  let num_rounds = at_least(&mut random, 15);
  let mut doc_id = 0;
  for _ in 0..num_rounds {
    let num_docs = at_least(&mut random, 5);
    for _ in 0..num_docs {
      let mut doc = Document::new();
      doc.add(StringField::from_string(
        "id",
        format!("doc-{doc_id}"),
        Store::No,
      )?);
      doc.add(StringField::from_string("key", "all", Store::No)?);
      #[allow(clippy::needless_range_loop)]
      for f in 0..field_values.len() {
        if f < num_ndv_fields {
          doc.add(NumericDocValuesField::new(format!("f{f}"), field_values[f]));
        } else {
          doc.add(BinaryDocValuesField::new(
            format!("f{f}"),
            to_bytes(&mut random, field_values[f])?,
          ));
        }
      }
      writer.add_document(doc)?;
      doc_id += 1;
    }

    let field_idx = random.random_range(0..field_values.len());
    let update_field = format!("f{field_idx}");
    field_values[field_idx] += 1;
    if field_idx < num_ndv_fields {
      writer.update_numeric_doc_value(
        Term::from_text("key", "all"),
        update_field,
        field_values[field_idx],
      )?;
    } else {
      writer.update_binary_doc_value(
        Term::from_text("key", "all"),
        update_field,
        to_bytes(&mut random, field_values[field_idx])?,
      )?;
    }

    if random.random_bool(0.2) {
      let delete_doc = random.random_range(0..doc_id);
      writer
        .delete_documents_with_terms(vec![Term::from_text("id", format!("doc-{delete_doc}"))])?;
    }

    if !is_nrt {
      writer.commit()?;
    }

    let new_reader = directory_reader::open_if_changed(&reader)?.unwrap();
    reader.close()?;
    reader = new_reader;

    assert!(reader.num_docs()? > 0);
    let context = (&reader).get_context()?;
    for context in context.leaves()? {
      let r = context.reader();
      let live_docs = r.get_live_docs()?;
      #[allow(clippy::needless_range_loop)]
      for field in 0..field_values.len() {
        let f = format!("f{field}");
        let mut bdv = r.get_binary_doc_values(&f)?;
        let mut ndv = r.get_numeric_doc_values(&f)?;
        if field < num_ndv_fields {
          assert!(ndv.is_some());
          assert!(bdv.is_none());
        } else {
          assert!(ndv.is_none());
          assert!(bdv.is_some());
        }
        let max_doc = r.max_doc()?;
        for doc in 0..max_doc {
          if live_docs
            .as_ref()
            .is_none_or(|bits| bits.get(doc as usize).expect(""))
          {
            if field < num_ndv_fields {
              let ndv = ndv.as_mut().unwrap();
              assert_eq!(doc, ndv.advance(doc)?);
              assert_eq!(field_values[field], ndv.long_value()?);
            } else {
              let bdv = bdv.as_mut().unwrap();
              assert_eq!(doc, bdv.advance(doc)?);
              assert_eq!(field_values[field], get_value(bdv)?,);
            }
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
fn test_stress_multi_threading() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let conf = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), conf)?;

  // create index
  let num_fields = TestUtil::next_int(&mut random, 2, 4);
  let num_threads = TestUtil::next_int(&mut random, 3, 6);
  let num_docs = at_least(&mut random, 2000);
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
      doc.add(NumericDocValuesField::new(
        format!("cf{j}"),
        value.wrapping_mul(2),
      ));
    }
    writer.add_document(doc)?;
  }

  let num_updates = AtomicI32::new(at_least(&mut random, 100));

  // same thread updates a field as well as reopens
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
                  NumericDocValuesField::new(cf, upd_value.wrapping_mul(2)).into(),
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
      let mut control = r.get_numeric_doc_values(&format!("cf{i}"))?.unwrap();
      let live_docs = r.get_live_docs()?;
      for j in 0..r.max_doc()? {
        if live_docs
          .as_ref()
          .is_none_or(|bits| bits.get(j as usize).expect(""))
        {
          assert_eq!(j, control.advance(j)?);
          let ctrl_value = control.long_value()?;
          assert_eq!(j, bdv.advance(j)?);
          let bdv_value = get_value(&bdv)?.wrapping_mul(2);
          assert_eq!(ctrl_value, bdv_value);
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
  let mut conf = new_index_writer_config_with_analyzer(&mut random, mock)?;
  conf.set_max_buffered_docs(4);
  let writer = IndexWriter::new(dir.clone(), conf)?;
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
    doc.add(NumericDocValuesField::new("cf", value.wrapping_mul(2)));
    writer.add_document(doc)?;
  }

  let num_gens = at_least(&mut random, 5);
  for _ in 0..num_gens {
    let doc = random.random_range(0..num_docs);
    let t = Term::from_text("id", format!("doc{doc}"));
    let value = random.random::<i64>();
    if random.random_bool(0.5) {
      do_update(t, &writer, || {
        Ok(vec![
          BinaryDocValuesField::new("f", to_bytes(&mut random, value)?).into(),
          NumericDocValuesField::new("cf", value.wrapping_mul(2)).into(),
        ])
      })?;
    } else {
      writer.update_doc_values(
        t,
        vec![
          BinaryDocValuesField::new("f", to_bytes(&mut random, value)?).into(),
          NumericDocValuesField::new("cf", value.wrapping_mul(2)).into(),
        ],
      )?;
    }

    let reader = directory_reader::open_from_writer(&writer)?;
    let reader = reader.get_context()?;
    for context in reader.leaves()? {
      let r = context.reader();
      let mut fbdv = r.get_binary_doc_values("f")?.unwrap();
      let mut cfndv = r.get_numeric_doc_values("cf")?.unwrap();
      for j in 0..r.max_doc()? {
        assert_eq!(j, cfndv.next_doc()?);
        assert_eq!(j, fbdv.next_doc()?);
        assert_eq!(cfndv.long_value()?, get_value(&fbdv)?.wrapping_mul(2));
      }
    }
  }
  writer.close()?;
  Ok(())
}

#[cfg(feature = "nightly")]
#[test]
#[ignore = "nightly"]
fn test_tons_of_updates() -> Result<()> {
  // LUCENE-5248: make sure that when there are many updates, we don't use too much RAM
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, mock)?;
  conf.set_ram_buffer_size_mb(crate::core::index::index_writer_config::DEFAULT_RAM_BUFFER_SIZE_MB);
  conf.set_max_buffered_docs(crate::core::index::index_writer_config::DISABLE_AUTO_FLUSH);
  let writer = IndexWriter::new(dir.clone(), conf)?;

  // test data: lots of documents (few 10Ks) and lots of update terms (few hundreds)
  let num_docs = at_least(&mut random, 20000);
  let num_binary_fields = at_least(&mut random, 5);
  let num_terms = TestUtil::next_usize(&mut random, 10, 100); // terms should affect many docs
  let mut update_terms = HashSet::new();
  while update_terms.len() < num_terms {
    update_terms.insert(TestUtil::random_simple_string(&mut random));
  }
  let update_terms: Vec<_> = update_terms.into_iter().collect();

  // build a large index with many BDV fields and update terms
  for _ in 0..num_docs {
    let mut doc = Document::new();
    let num_update_terms = TestUtil::next_usize(&mut random, 1, num_terms / 10);
    for _ in 0..num_update_terms {
      doc.add(StringField::from_string(
        "upd",
        update_terms.choose(&mut random).unwrap(),
        Store::No,
      )?);
    }
    for j in 0..num_binary_fields {
      let val = random.random::<i32>() as i64;
      doc.add(BinaryDocValuesField::new(
        format!("f{j}"),
        to_bytes(&mut random, val)?,
      ));
      doc.add(NumericDocValuesField::new(
        format!("cf{j}"),
        val.wrapping_mul(2),
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
        NumericDocValuesField::new(format!("cf{field}"), value.wrapping_mul(2)).into(),
      ],
    )?;
  }

  writer.close()?;

  let reader = directory_reader::open(dir.clone())?;
  let reader = reader.get_context()?;
  for context in reader.leaves()? {
    for i in 0..num_binary_fields {
      let r = context.reader();
      let mut f = r.get_binary_doc_values(&format!("f{i}"))?.unwrap();
      let mut cf = r.get_numeric_doc_values(&format!("cf{i}"))?.unwrap();
      for j in 0..r.max_doc()? {
        assert_eq!(j, cf.next_doc()?);
        assert_eq!(j, f.next_doc()?);
        assert_eq!(cf.long_value()?, get_value(&f)?.wrapping_mul(2));
      }
    }
  }
  Ok(())
}

#[test]
fn test_try_update_doc_values() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let conf = new_index_writer_config(&mut random)?;
  let writer = IndexWriter::new(dir.clone(), conf)?;
  let num_docs = 1 + random.random_range(0..128);
  for i in 0..num_docs {
    let mut doc = Document::new();
    doc.add(StringField::from_string("id", i.to_string(), Store::Yes)?);
    doc.add(NumericDocValuesField::new("numericId", i as i64));
    doc.add(BinaryDocValuesField::new(
      "binaryId",
      BytesRef::from_bytes(vec![i as u8]),
    ));
    writer.add_document(doc)?;
    if random.random_bool(0.5) {
      writer.flush()?;
    }
  }
  let doc = random.random_range(0..num_docs);
  do_update(Term::from_text("id", doc.to_string()), &writer, || {
    Ok(vec![
      NumericDocValuesField::new("numericId", (doc + 1) as i64).into(),
      BinaryDocValuesField::new("binaryId", BytesRef::from_bytes(vec![(doc + 1) as u8])).into(),
    ])
  })?;

  let reader = directory_reader::open_from_writer(&writer)?;
  let context = (&reader).get_context()?;
  let mut numeric_id_values = None;
  let mut binary_id_values = None;
  for c in context.leaves()? {
    let searcher = new_searcher_with_reader(c.reader().clone())?;
    let top_docs = searcher.search(TermQuery::new(Term::from_text("id", doc.to_string())), 10)?;
    if top_docs.total_hits.value() == 1 {
      assert!(numeric_id_values.is_none());
      assert!(binary_id_values.is_none());
      let leaf_doc = top_docs.score_docs[0].doc;
      let mut numeric = c.reader().get_numeric_doc_values("numericId")?.unwrap();
      assert_eq!(leaf_doc, numeric.advance(leaf_doc)?);
      let mut binary = c.reader().get_binary_doc_values("binaryId")?.unwrap();
      assert_eq!(leaf_doc, binary.advance(leaf_doc)?);
      numeric_id_values = Some(numeric.long_value()?);
      binary_id_values = Some(binary.binary_value()?.into_owned());
    } else {
      assert_eq!(0, top_docs.total_hits.value());
    }
  }

  assert_eq!(Some((doc + 1) as i64), numeric_id_values);
  assert_eq!(
    Some([(doc + 1) as u8].as_slice()),
    binary_id_values.as_ref().map(BytesRefValue::as_bytes)
  );
  reader.close()?;
  writer.close()?;
  Ok(())
}

#[test]
fn test_try_update_multi_threaded() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let conf = new_index_writer_config(&mut random)?;
  let writer = IndexWriter::new(dir.clone(), conf)?;
  let num_locks = 25 + random.random_range(0..50);
  let mut values = Vec::new();

  for i in 0..num_locks {
    let mut doc = Document::new();
    let value = random.random::<i64>();
    values.push(Mutex::new(Some(value)));
    doc.add(StringField::from_string("id", i.to_string(), Store::No)?);
    doc.add(NumericDocValuesField::new("value", value));
    writer.add_document(doc)?;
  }

  let values = Arc::new(values);
  let num_threads = if is_night_mode() {
    2 + random.random_range(0..3)
  } else {
    2
  };
  let barrier = Arc::new(Barrier::new(num_threads + 1));

  thread::scope(|scope| -> Result<()> {
    let mut handles = Vec::new();
    for _ in 0..num_threads {
      let writer = &writer;
      let barrier = &barrier;
      let values = &values;
      let seed = random.random();
      handles.push(scope.spawn(move || -> Result<()> {
        let mut random = random_from_seed(seed);
        barrier.wait();
        for _ in 0..1000 {
          let doc_id = random.random_range(0..values.len());
          let mut value_guard = values[doc_id].lock();
          let value = if rarely(&mut random) {
            None
          } else {
            Some(random.random::<i64>())
          };
          if random.random_bool(0.5) {
            writer.update_doc_values(
              Term::from_text("id", doc_id.to_string()),
              vec![if let Some(value) = value {
                NumericDocValuesField::new("value", value).into()
              } else {
                let mut field_type = FieldType::new();
                field_type.set_doc_values_type(DocValuesType::Numeric)?;
                field_type.freeze();
                Field::new("value", FieldDataEnum::Dummy(()), field_type).into()
              }],
            )?;
          } else {
            do_update(Term::from_text("id", doc_id.to_string()), writer, || {
              Ok(vec![if let Some(value) = value {
                NumericDocValuesField::new("value", value).into()
              } else {
                let mut field_type = FieldType::new();
                field_type.set_doc_values_type(DocValuesType::Numeric)?;
                field_type.freeze();
                Field::new("value", FieldDataEnum::Dummy(()), field_type).into()
              }])
            })?;
          }
          *value_guard = value;

          if rarely(&mut random) {
            writer.flush()?;
          }
        }
        Ok(())
      }));
    }

    barrier.wait();
    for handle in handles {
      handle
        .join()
        .map_err(|_| LuceneError::illegal_state("update thread panicked"))??;
    }
    Ok(())
  })?;

  let reader = directory_reader::open_from_writer(&writer)?;
  let searcher = new_searcher_with_reader(reader)?;
  let context = &searcher.reader_context;
  for i in 0..values.len() {
    let value_guard = values[i].lock();
    let value = *value_guard;
    let top_docs = searcher.search(TermQuery::new(Term::from_text("id", i.to_string())), 10)?;
    assert_eq!(1, top_docs.total_hits.value());
    let mut doc_id = top_docs.score_docs[0].doc;
    let leaves = context.leaves()?;
    let sub_index = leaves
      .iter()
      .position(|leaf| {
        let max_doc = leaf.reader().max_doc().expect("max_doc should not fail") as usize;
        doc_id as usize >= leaf.doc_base && (doc_id as usize) < leaf.doc_base + max_doc
      })
      .expect("matching leaf should exist");
    let leaf_reader = leaves[sub_index].reader();
    doc_id -= leaves[sub_index].doc_base as i32;
    let mut numeric_doc_values = leaf_reader.get_numeric_doc_values("value")?.unwrap();
    if let Some(value) = value {
      assert!(numeric_doc_values.advance_exact(doc_id)?);
      assert_eq!(value, numeric_doc_values.long_value()?);
    } else {
      assert!(!numeric_doc_values.advance_exact(doc_id)?);
    }
  }
  context.reader().close()?;
  writer.close()?;
  Ok(())
}

fn do_update<D, F>(doc: Term, writer: &Arc<IndexWriter<D>>, mut fields: F) -> Result<()>
where
  D: Directory + 'static + std::marker::Send + Sync,
  F: FnMut() -> Result<Vec<Fields>>,
  <<D as Directory>::IndexInput as IndexInput>::RandomAccessSlice: Send + Sync,
  <D as Directory>::IndexInput: Send + Sync,
{
  let mut seq_id = -1;
  while seq_id == -1 {
    let reader = directory_reader::open_from_writer(writer)?;
    let searcher = new_searcher_with_reader(reader)?;
    let top_docs = searcher.search(TermQuery::new(doc.clone()), 10)?;
    assert_eq!(1, top_docs.total_hits.value());
    let the_doc = top_docs.score_docs()[0].doc;
    let reader = searcher.reader_context.reader();
    seq_id = writer.try_update_doc_value(reader, the_doc, fields()?)?;
    reader.close()?;
  }
  Ok(())
}

#[test]
fn test_reset_value() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let conf = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), conf)?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "0", Store::No)?);
  doc.add(NumericDocValuesField::new("val", 5));
  doc.add(BinaryDocValuesField::new(
    "val-bin",
    BytesRef::from_bytes(vec![5]),
  ));
  writer.add_document(doc)?;

  if random.random_bool(0.5) {
    writer.commit()?;
  }
  {
    let reader = directory_reader::open_from_writer(&writer)?;
    let context = (&reader).get_context()?;
    let leaves = context.leaves()?;
    assert_eq!(1, leaves.len());
    let r = leaves[0].reader();
    let mut ndv = r.get_numeric_doc_values("val")?.unwrap();
    assert_eq!(0, ndv.next_doc()?);
    assert_eq!(5, ndv.long_value()?);
    assert_eq!(NO_MORE_DOCS, ndv.next_doc()?);

    let mut bdv = r.get_binary_doc_values("val-bin")?.unwrap();
    assert_eq!(0, bdv.next_doc()?);
    assert_eq!([5u8].as_slice(), bdv.binary_value()?.as_bytes());
    assert_eq!(NO_MORE_DOCS, bdv.next_doc()?);
    reader.close()?;
  }

  let mut field_type = FieldType::new();
  field_type.set_doc_values_type(DocValuesType::Binary)?;
  field_type.freeze();
  writer.update_doc_values(
    Term::from_text("id", "0"),
    vec![Field::new("val-bin", FieldDataEnum::Dummy(()), field_type).into()],
  )?;
  {
    let reader = directory_reader::open_from_writer(&writer)?;
    let context = (&reader).get_context()?;
    let leaves = context.leaves()?;
    assert_eq!(1, leaves.len());
    let r = leaves[0].reader();
    let mut ndv = r.get_numeric_doc_values("val")?.unwrap();
    assert_eq!(0, ndv.next_doc()?);
    assert_eq!(5, ndv.long_value()?);
    assert_eq!(NO_MORE_DOCS, ndv.next_doc()?);

    let mut bdv = r.get_binary_doc_values("val-bin")?.unwrap();
    assert_eq!(NO_MORE_DOCS, bdv.next_doc()?);
    reader.close()?;
  }
  writer.close()?;
  Ok(())
}

#[test]
fn test_reset_value_multiple_docs() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let conf = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), conf)?;
  let num_docs = 10 + random.random_range(0..50);
  let mut current_seq_id = 0;
  let mut seq_id = [-1, -1, -1, -1, -1];
  #[allow(clippy::explicit_counter_loop)]
  for i in 0..num_docs {
    let mut doc = Document::new();
    let id = random.random_range(0..5);
    seq_id[id] = current_seq_id;
    doc.add(StringField::from_string("id", id.to_string(), Store::Yes)?);
    doc.add(NumericDocValuesField::new("seqID", current_seq_id as i64));
    current_seq_id += 1;
    doc.add(NumericDocValuesField::new("is_live", 1));
    if i > 0 {
      let mut field_type = FieldType::new();
      field_type.set_doc_values_type(DocValuesType::Numeric)?;
      field_type.freeze();
      writer.update_doc_values(
        Term::from_text("id", id.to_string()),
        vec![Field::new("is_live", FieldDataEnum::Dummy(()), field_type).into()],
      )?;
    }
    writer.add_document(doc)?;
    if random.random_bool(0.5) {
      writer.flush()?;
    }
  }

  if random.random_bool(0.5) {
    writer.commit()?;
  }
  let mut num_hits = 0; // check if every doc has been selected at least once
  for i in seq_id {
    if i > -1 {
      num_hits += 1;
    }
  }
  {
    let reader = directory_reader::open_from_writer(&writer)?;
    let searcher = new_searcher_with_reader(reader)?;
    let is_live = searcher.search(FieldExistsQuery::new("is_live"), 5)?;
    assert_eq!(num_hits, is_live.total_hits.value());
    let reader = searcher.reader_context.reader();
    let mut stored_fields = reader.stored_fields()?;
    let context = &searcher.reader_context;
    for doc in is_live.score_docs {
      let id = stored_fields
        .document(doc.doc)?
        .get("id")?
        .unwrap()
        .parse::<usize>()?;
      let leaves = context.leaves()?;
      let i = leaves
        .iter()
        .position(|leaf| {
          let max_doc = leaf.reader().max_doc().expect("max_doc should not fail") as usize;
          doc.doc as usize >= leaf.doc_base && (doc.doc as usize) < leaf.doc_base + max_doc
        })
        .expect("matching leaf should exist");
      let leaf_reader_context = &leaves[i];
      let mut seq_id_values = leaf_reader_context
        .reader()
        .get_numeric_doc_values("seqID")?
        .unwrap();
      let leaf_doc = doc.doc - leaf_reader_context.doc_base as i32;
      assert!(seq_id_values.advance_exact(leaf_doc)?);
      assert_eq!(seq_id[id] as i64, seq_id_values.long_value()?);
    }
    reader.close()?;
  }
  writer.close()?;
  Ok(())
}

#[test]
fn test_update_not_existing_field_dv() -> Result<()> {
  let mut random = random();
  let mock = MockAnalyzer::new(&mut random);
  let conf = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let dir = new_directory_shared(&mut random)?;
  let writer = IndexWriter::new(dir.clone(), conf)?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "1", Store::Yes)?);
  doc.add(NumericDocValuesField::new("test", 1));
  writer.add_document(doc)?;
  if random.random_bool(0.5) {
    writer.commit()?;
  }
  writer.update_doc_values(
    Term::from_text("id", "1"),
    vec![NumericDocValuesField::new("not_existing", 1).into()],
  )?;

  let mut doc1 = Document::new();
  doc1.add(StringField::from_string("id", "2", Store::Yes)?);
  doc1.add(BinaryDocValuesField::new("not_existing", BytesRef::new()));
  let result = writer.add_document(doc1);
  match (result).map_err(LuceneError::into_kind) {
    Err(LuceneErrorKind::IllegalArgument(msg)) => {
      assert!(msg.message.contains("cannot change field \"not_existing\""));
      assert!(msg.message.contains("inconsistent doc values type=Binary"));
    },
    other => panic!("expected IllegalArgument error, got {other:?}"),
  }

  let result = writer.update_doc_values(
    Term::from_text("id", "1"),
    vec![BinaryDocValuesField::new("not_existing", BytesRef::new()).into()],
  );
  match (result).map_err(LuceneError::into_kind) {
    Err(LuceneErrorKind::IllegalArgument(msg)) => {
      assert!(msg.message.contains("Can't update [Binary] doc values"));
      assert!(msg.message.contains("not_existing"));
      assert!(msg.message.contains("Numeric"));
    },
    other => panic!("expected IllegalArgument error, got {other:?}"),
  }
  writer.close()?;
  Ok(())
}

#[test]
fn test_update_field_with_no_previous_doc_values_throws_error() -> Result<()> {
  let mut random = random();
  let mock = MockAnalyzer::new(&mut random);
  let conf = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let dir = new_directory_shared(&mut random)?;
  let writer = IndexWriter::new(dir.clone(), conf)?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "1", Store::Yes)?);
  writer.add_document(doc)?;
  if random.random_bool(0.5) {
    let reader = directory_reader::open_from_writer(&writer)?;
    let context = (&reader).get_context()?;
    let id = context.leaves()?[0].reader().get_numeric_doc_values("id")?;
    assert!(id.is_none());
    reader.close()?;
  } else if random.random_bool(0.5) {
    writer.commit()?;
  }
  let error = writer.update_doc_values(
    Term::from_text("id", "1"),
    vec![NumericDocValuesField::new("id", 1).into()],
  );
  match (error).map_err(LuceneError::into_kind) {
    Err(LuceneErrorKind::IllegalArgument(msg)) => {
      assert!(msg.message.contains("Can't update [Numeric] doc values"));
      assert!(msg.message.contains("field [id]"));
      assert!(msg.message.contains("None"));
    },
    other => panic!("expected IllegalArgument error, got {other:?}"),
  }
  writer.close()?;
  Ok(())
}
