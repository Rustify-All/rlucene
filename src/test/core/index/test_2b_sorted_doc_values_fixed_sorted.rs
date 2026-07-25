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
use crate::core::document::fields::Fields;
use crate::core::document::sorted_doc_values_field::SortedDocValuesField;
use crate::core::index::BytesRef;
use crate::core::index::concurrent_merge_scheduler::ConcurrentMergeScheduler;
use crate::core::index::directory_reader;
use crate::core::index::doc_values::DocValues;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::{IndexWriter, MAX_DOCS};
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::index_writer_config::OpenMode;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::sorted_doc_values::SortedDocValues;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::store::mock_directory_wrapper::Throttling;
use crate::test_framework::core::util::lucene_test_case::{
  create_temp_dir_with_prefix, new_fs_directory, new_index_writer_config_with_analyzer,
  new_log_merge_policy_with_merge_factor, random,
};
use crate::test_framework::core::util::test_util::TestUtil;
use std::sync::Arc;

#[allow(dead_code)] // for quick search
struct Test2BSortedDocValuesFixedSorted;

// Indexes IndexWriter::MAX_DOCS documents with a fixed binary field.
#[cfg(feature = "monster")]
#[test]
#[ignore = "monster"]
fn test_fixed_sorted() -> Result<()> {
  let mut random = random();
  let dir = new_fs_directory(&mut random, create_temp_dir_with_prefix("2BFixedSorted")?)?;
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
  let mut bytes = vec![0u8; 2];
  doc.add(SortedDocValuesField::new(
    "dv",
    BytesRef::from_bytes(bytes.clone()),
  ));
  for i in 0..MAX_DOCS {
    bytes[0] = (i >> 8) as u8;
    bytes[1] = i as u8;
    let Some(Fields::SortedDocValues(field)) = doc.get_field_mut("dv") else {
      unreachable!("dv must be a SortedDocValuesField");
    };
    field.set_bytes_value(BytesRef::from_bytes(bytes.clone()))?;
    writer.add_document(doc.clone())?;
    if i % 100_000 == 0 {
      println!("indexed: {i}");
    }
  }

  writer.force_merge(1)?;
  writer.close()?;

  println!("verifying...");
  let reader = directory_reader::open(dir.clone())?;
  let mut expected_value = 0i32;
  let context = (&reader).get_context()?;
  for context in context.leaves()? {
    let leaf = context.reader();
    let mut values = DocValues::get_sorted(leaf, "dv")?;
    for i in 0..leaf.max_doc()? {
      assert_eq!(i, values.next_doc()?);
      bytes[0] = (expected_value >> 8) as u8;
      bytes[1] = expected_value as u8;
      let expected = BytesRef::from_bytes(bytes.clone());
      let ord = values.ord_value()?;
      let term = values.lookup_ord(ord)?;
      assert_eq!(&expected, term.as_ref());
      expected_value += 1;
    }
  }

  reader.close()?;
  dir.as_ref().close()?;
  Ok(())
}

// TODO: variable
