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
use crate::codec::memory::direct_postings_format::DirectPostingsFormat;
use crate::core::codecs::codec;
use crate::core::document::document::Document;
use crate::core::document::field::{FieldBase, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::fields::Fields;
use crate::core::index::BytesRef;
use crate::core::index::CODEC_FILE_PATTERN;
use crate::core::index::directory_reader;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_writer::{IndexWriter, MAX_TERM_LENGTH};
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::segment_infos::SegmentInfos;
use crate::core::index::standard_directory_reader::StandardDirectoryReader;
use crate::core::index::term::Term;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::search::term_query::TermQuery;
use crate::core::store::directory::Directory;
use crate::core::util::TryIntoInt;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::test_index_writer::assert_no_unreferenced_files;
use crate::test_framework::core::util::line_file_docs::LineFileDocs;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, new_directory_shared, new_index_writer_config_with_analyzer, new_searcher_with_reader,
  new_string_field_binary, random, random_from_seed,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::RngExt;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

#[allow(dead_code)] // for quick search
struct TestRollingUpdates;

#[test]
fn test_rolling_updates() -> Result<()> {
  let mut random = random();
  let seed = random.random::<u64>();
  let mut doc_random = random_from_seed(seed);
  let dir = new_directory_shared(&mut random)?;

  let mut docs = LineFileDocs::new(&mut doc_random)?;

  if random.random_bool(0.5) {
    codec::set_default(TestUtil::always_postings_format(DirectPostingsFormat::new()));
  }

  let mut analyzer = MockAnalyzer::new(&mut random);
  analyzer.set_max_token_length(TestUtil::next_int(&mut random, 1, MAX_TERM_LENGTH));

  let w = IndexWriter::new(
    dir.clone(),
    new_index_writer_config_with_analyzer(&mut random, analyzer)?,
  )?;
  let size = at_least(&mut random, 20);
  let mut id = 0;
  let mut r: Option<Arc<StandardDirectoryReader<_>>> = None;
  let num_updates = (size as f64 * (2.0 + 5.0 * random.random::<f64>())).floor() as i32;
  let mut update_count = 0;

  for doc_iter in 0..num_updates {
    let mut doc = docs.next_doc()?;
    let my_id = id.to_string();
    if id == size - 1 {
      id = 0;
    } else {
      id += 1;
    }

    let docid_field = doc
      .get_field_mut("docid")
      .expect("LineFileDocs document must have docid");
    match docid_field {
      Fields::String(field) => field.set_string_value(&my_id)?,
      Fields::Field(field) => field.set_string_value(&my_id)?,
      _ => unreachable!("docid field is not string-backed"),
    }

    let id_term = Term::from_text("docid", &my_id);

    let do_update = if let Some(reader) = &r {
      if update_count < size {
        let s = new_searcher_with_reader(reader.clone())?;
        let hits = s.search(TermQuery::new(id_term.clone()), 1)?;
        assert_eq!(1, hits.total_hits.value());
        w.try_delete_document(reader, hits.score_docs[0].doc)? == -1
      } else {
        true
      }
    } else {
      true
    };

    update_count += 1;

    if do_update {
      if random.random_bool(0.5) {
        w.update_document_with_term(id_term, doc)?;
      } else {
        // It's OK to not be atomic for this test (no separate thread reopening readers):
        w.delete_documents_with_queries(vec![TermQuery::new(id_term).into()])?;
        w.add_document(doc)?;
      }
    } else {
      w.add_document(doc)?;
    }

    if doc_iter >= size && TestUtil::next_int(&mut random, 0, 49) == 17 {
      if let Some(reader) = r.take() {
        reader.close()?;
      }

      let apply_deletions = random.random_bool(0.5);

      let reader = Arc::new(w.get_reader(apply_deletions, false)?);
      if apply_deletions {
        assert_eq!(
          size,
          reader.num_docs()?,
          "applyDeletions={} r.numDocs()={} vs SIZE={}",
          apply_deletions,
          reader.num_docs()?,
          size
        );
      }
      r = Some(reader);
      update_count = 0;
    }
  }

  if let Some(reader) = r.take() {
    reader.close()?;
  }

  w.commit()?;
  assert_eq!(size, w.get_doc_stats()?.num_docs);

  w.close()?;

  assert_no_unreferenced_files(dir.clone(), "leftover files after rolling updates")?;
  docs.close();

  // LUCENE-4455:
  let infos = SegmentInfos::read_latest_commit(dir.clone())?;
  let mut total_bytes = 0i64;
  for sipc in infos.iter() {
    total_bytes += sipc.size_in_bytes()?;
  }
  let mut total_bytes2 = 0i64;

  for file_name in dir.list_all()? {
    if CODEC_FILE_PATTERN.is_match(&file_name) {
      let file_length: i64 = dir.file_length(&file_name)?.try_convert()?;
      total_bytes2 += file_length;
    }
  }
  assert_eq!(total_bytes2, total_bytes);
  dir.as_ref().close()
}

#[test]
fn test_update_same_doc() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut docs = LineFileDocs::new(&mut random)?;
  let field_to_type: Mutex<HashMap<String, FieldType>> = Mutex::new(HashMap::new());

  for _ in 0..3 {
    let analyzer = MockAnalyzer::new(&mut random);
    let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
    config.set_max_buffered_docs(2);
    let writer = IndexWriter::new(dir.clone(), config)?;
    let num_updates = at_least(&mut random, 20);
    let num_threads = TestUtil::next_int(&mut random, 2, 6);
    let seed = random.random::<u64>();
    let mut thread_random = random_from_seed(seed);

    std::thread::scope(|scope| {
      let writer = &writer;
      let field_to_type = &field_to_type;
      let mut threads = Vec::new();
      for _ in 0..num_threads {
        let thread_seed = thread_random.random::<u64>();
        threads.push(
          scope.spawn(move || indexing_thread(thread_seed, writer, num_updates, field_to_type)),
        );
      }

      for thread in threads {
        thread.join().expect("indexing thread panicked")?;
      }
      Ok::<(), crate::core::util::error::lucene_error::LuceneError>(())
    })?;

    writer.close()?;
  }

  let open = directory_reader::open(dir.clone())?;
  assert_eq!(1, open.num_docs()?);
  open.close()?;
  docs.close();
  dir.as_ref().close()
}

fn indexing_thread<D>(
  seed: u64,
  writer: &Arc<IndexWriter<D>>,
  num: i32,
  field_to_type: &Mutex<HashMap<String, FieldType>>,
) -> Result<()>
where
  D: Directory + 'static,
{
  let mut random = random_from_seed(seed);
  let mut open: Option<StandardDirectoryReader<D>> = None;

  for i in 0..num {
    let mut doc = Document::new();
    let bytes = BytesRef::from_string("test");
    let id_field = {
      let mut field_to_type = field_to_type.lock().unwrap();
      new_string_field_binary(
        &mut random,
        "id",
        bytes.clone(),
        Store::No,
        &mut field_to_type,
      )?
    };
    doc.add(id_field);
    writer.update_document_with_term(Term::new("id", bytes), doc)?;

    if TestUtil::next_int(&mut random, 0, 2) == 0 {
      if let Some(old_reader) = open.take() {
        open = match directory_reader::open_if_changed(&old_reader)? {
          Some(new_reader) => {
            old_reader.close()?;
            Some(new_reader)
          },
          None => Some(old_reader),
        };
      } else {
        open = Some(directory_reader::open_from_writer(writer)?);
      }

      let open_ref = open.as_ref().unwrap();
      assert_eq!(
        1,
        open_ref.num_docs()?,
        "iter: {} numDocs: {} del: {} max: {}",
        i,
        open_ref.num_docs()?,
        open_ref.num_deleted_docs()?,
        open_ref.max_doc()?
      );
    }
  }

  if let Some(open) = open {
    open.close()?;
  }
  Ok(())
}
