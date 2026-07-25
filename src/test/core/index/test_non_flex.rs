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
use crate::core::index::BytesRef;
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::directory_reader;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::multi_terms::get_terms;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::util::lucene_test_case::{
  get_only_leaf_reader, new_directory_shared, new_index_writer_config_with_analyzer,
  new_log_merge_policy, new_text_field, random,
};
use std::collections::HashMap;

#[allow(dead_code)] // for quick search
pub struct TestFlex;
#[test]
fn test_non_flex() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  const DOC_COUNT: i32 = 177;
  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(mock)?;
  iwc.set_max_buffered_docs(7);
  iwc.set_merge_policy(new_log_merge_policy(&mut random)?);

  let writer = IndexWriter::new(dir.clone(), iwc)?;
  let mut field_to_type = HashMap::new();
  for iter in 0..2 {
    if iter == 0 {
      let mut doc = Document::new();
      doc.add(new_text_field(
        &mut random,
        "field1",
        "this is field1",
        Store::No,
        &mut field_to_type,
      )?);
      doc.add(new_text_field(
        &mut random,
        "field2",
        "this is field2",
        Store::No,
        &mut field_to_type,
      )?);
      doc.add(new_text_field(
        &mut random,
        "field3",
        "aaa",
        Store::No,
        &mut field_to_type,
      )?);
      doc.add(new_text_field(
        &mut random,
        "field4",
        "bbb",
        Store::No,
        &mut field_to_type,
      )?);

      for _ in 0..DOC_COUNT {
        writer.add_document(doc.clone())?;
      }
    } else {
      writer.force_merge(1)?;
    }

    let reader = directory_reader::open_from_writer(&writer)?;
    let terms = get_terms(&reader, "field3")?
      .ok_or_else(|| LuceneError::illegal_state("terms for field3 is None"))?;
    let mut terms_enum = terms.iterator()?;

    assert_eq!(
      SeekStatus::End,
      terms_enum.seek_ceil(&BytesRef::from_string("abc"))?
    );
  }
  writer.close()?;
  Ok(())
}
#[test]
fn test_term_ord() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;

  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let mut field_to_type = HashMap::new();
  let mut doc = Document::new();
  doc.add(new_text_field(
    &mut random,
    "f",
    "a b c",
    Store::No,
    &mut field_to_type,
  )?);
  writer.add_document(doc)?;
  writer.force_merge(1)?;

  let reader = directory_reader::open_from_writer(&writer)?;
  let leaf = get_only_leaf_reader(&reader)?;
  let terms = leaf
    .terms("f")?
    .ok_or_else(|| LuceneError::illegal_state("terms for field f is None"))?;
  let mut terms_enum = terms.iterator()?;

  assert!(terms_enum.next()?.is_some());

  match terms_enum.ord() {
    Ok(ord) => assert_eq!(0, ord),
    Err(LuceneError::UnsupportedOperation(_)) => {},
    Err(err) => return Err(err),
  }

  writer.close()?;
  Ok(())
}
