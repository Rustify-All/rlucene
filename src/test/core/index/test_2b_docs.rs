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
use crate::core::document::field::Store::No;
use crate::core::document::string_field::StringField;
use crate::core::index::BytesRef;
use crate::core::index::concurrent_merge_scheduler::ConcurrentMergeScheduler;
use crate::core::index::directory_reader;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::{IndexWriter, MAX_DOCS};
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::index_writer_config::OpenMode;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, NO_MORE_DOCS};
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::store::mock_directory_wrapper::Throttling;
use crate::test_framework::core::util::lucene_test_case::{
  create_temp_dir_with_prefix, new_fs_directory, new_index_writer_config_with_analyzer,
  new_log_merge_policy_with_merge_factor, random,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::RngExt;
use std::sync::Arc;

#[allow(dead_code)] // for quick search
struct Test2BDocs;

// Indexes IndexWriter::MAX_DOCS documents with indexed fields.
#[cfg(feature = "monster")]
#[test]
#[ignore = "monster"]
fn test_2b_docs() -> Result<()> {
  let mut random = random();
  let dir = new_fs_directory(&mut random, create_temp_dir_with_prefix("2BDocs")?)?;
  if let crate::core::store::directory::DirEnum::B(dir) = dir.as_ref() {
    dir.set_throttling(Throttling::Never);
  }

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  iwc
    .set_max_buffered_docs(-1)
    .set_ram_buffer_size_mb(256.0)
    .set_merge_scheduler(ConcurrentMergeScheduler::new())
    .set_merge_policy(new_log_merge_policy_with_merge_factor(&mut random, 10)?)
    .set_open_mode(OpenMode::Create)
    .set_codec(TestUtil::get_default_codec());
  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("f1", "a", No)?);
  for _ in 0..MAX_DOCS {
    writer.add_document(doc.clone())?;
  }

  writer.force_merge(1)?;
  writer.close()?;

  let reader = directory_reader::open(dir.clone())?;
  let term = BytesRef::from_string("a");
  let mut skips = 0i64;
  let context = (&reader).get_context()?;
  for context in context.leaves()? {
    let leaf = context.reader();
    let limit = leaf.max_doc()?;
    let terms = leaf.terms("f1")?.expect("f1 terms must exist");
    for _ in 0..10000 {
      let mut terms_enum = terms.iterator()?;
      assert!(terms_enum.seek_exact(&term)?);
      let mut docs = terms_enum.postings(None)?;

      // Skip randomly through the term.
      let mut target = -1;
      loop {
        let mut max_skip_size = limit - target + 1;
        // Do a smaller skip half of the time.
        if random.random_bool(0.5) {
          max_skip_size = 256.min(max_skip_size);
        }
        let mut new_target = target + random.random_range(0..max_skip_size) + 1;
        if new_target >= limit {
          if target + 1 >= limit {
            break;
          }
          new_target = limit - 1;
        }
        target = new_target;

        let result = docs.advance(target)?;
        if result == NO_MORE_DOCS {
          break;
        }
        assert!(result >= target);

        skips += 1;
        target = result;
      }
    }
  }

  reader.close()?;
  dir.as_ref().close()?;
  assert!(skips > 0);
  Ok(())
}
