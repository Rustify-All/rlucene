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
use crate::core::document::field::FieldBase;
use crate::core::document::fields::Fields;
use crate::core::index::BytesRef;
use crate::core::index::binary_doc_values::BinaryDocValues;
use crate::core::index::concurrent_merge_scheduler::ConcurrentMergeScheduler;
use crate::core::index::directory_reader;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::{IndexWriter, MAX_DOCS};
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::index_writer_config::OpenMode;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::store::byte_array_data_input::ByteArrayDataInput;
use crate::core::store::byte_array_data_output::ByteArrayDataOutput;
use crate::core::store::data_input::DataInput;
use crate::core::store::data_output::DataOutput;
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
struct Test2BBinaryDocValues;

// Indexes IndexWriter::MAX_DOCS documents with a fixed binary field.
#[cfg(feature = "monster")]
#[test]
#[ignore = "monster"]
fn test_fixed_binary() -> Result<()> {
  let mut random = random();
  let dir = new_fs_directory(&mut random, create_temp_dir_with_prefix("2BFixedBinary")?)?;
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
  let mut bytes = vec![0u8; 4];
  doc.add(BinaryDocValuesField::new(
    "dv",
    BytesRef::from_bytes(bytes.clone()),
  ));

  for i in 0..MAX_DOCS {
    bytes[0] = (i >> 24) as u8;
    bytes[1] = (i >> 16) as u8;
    bytes[2] = (i >> 8) as u8;
    bytes[3] = i as u8;
    let Some(Fields::BinaryDocValues(field)) = doc.get_field_mut("dv") else {
      unreachable!("dv must be a BinaryDocValuesField");
    };
    field.set_bytes_value(BytesRef::from_bytes(bytes.clone()))?;
    // Java can reuse the same Document by reference. Rust's IndexWriter consumes it, so clone the
    // updated document while retaining the reusable instance for the next iteration.
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
    let mut values = leaf
      .get_binary_doc_values("dv")?
      .expect("dv must have binary DocValues");
    for i in 0..leaf.max_doc()? {
      bytes[0] = (expected_value >> 24) as u8;
      bytes[1] = (expected_value >> 16) as u8;
      bytes[2] = (expected_value >> 8) as u8;
      bytes[3] = expected_value as u8;
      assert_eq!(i, values.next_doc()?);
      let term = values.binary_value()?;
      let expected = BytesRef::from_bytes(bytes.clone());
      assert_eq!(&expected, term.as_ref());
      expected_value += 1;
    }
  }

  reader.close()?;
  dir.as_ref().close()?;
  Ok(())
}

// Indexes IndexWriter::MAX_DOCS documents with a variable binary field.
#[cfg(feature = "monster")]
#[test]
#[ignore = "monster"]
fn test_variable_binary() -> Result<()> {
  let mut random = random();
  let dir = new_fs_directory(
    &mut random,
    create_temp_dir_with_prefix("2BVariableBinary")?,
  )?;
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
  doc.add(BinaryDocValuesField::new(
    "dv",
    BytesRef::from_bytes(vec![0u8; 4]),
  ));
  let mut encoder = ByteArrayDataOutput::with_bytes(vec![0u8; 4]);

  for i in 0..MAX_DOCS {
    encoder.reset()?;
    encoder.write_vint(i % 65535)?; // 1, 2, or 3 bytes
    let length = encoder.get_position();
    let Some(Fields::BinaryDocValues(field)) = doc.get_field_mut("dv") else {
      unreachable!("dv must be a BinaryDocValuesField");
    };
    field.set_bytes_value(BytesRef::from_slice(encoder.bytes.clone(), 0, length))?;
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
    let mut values = leaf
      .get_binary_doc_values("dv")?
      .expect("dv must have binary DocValues");
    for i in 0..leaf.max_doc()? {
      assert_eq!(i, values.next_doc()?);
      let term = values.binary_value()?;
      let mut input =
        ByteArrayDataInput::with_range(term.bytes.as_slice(), term.offset, term.length);
      assert_eq!(expected_value % 65535, input.read_vint()?);
      assert!(input.eof());
      expected_value += 1;
    }
  }

  reader.close()?;
  dir.as_ref().close()?;
  Ok(())
}
