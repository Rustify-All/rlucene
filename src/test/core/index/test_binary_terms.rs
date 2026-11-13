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
use crate::core::document::field_type::FieldType;
use crate::core::index::BytesRef;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term::Term;
use crate::core::search::term_query::TermQuery;
use crate::core::search::top_docs::TopDocsLike;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::util::lucene_test_case::{
  new_directory_shared, new_field, new_searcher_with_reader, new_string_field_binary, random,
};
use std::collections::HashMap;

/// Tests indexing and searching byte-slice terms.
#[allow(dead_code)] // for quick search
struct TestBinaryTerms;
#[test]
fn test_binary() -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;

  let iw = RandomIndexWriter::new(&mut random, directory.clone())?;
  let mut field_types = HashMap::new();
  let mut bytes: BytesRef<Vec<u8>> = BytesRef::with_capacity(2)?;

  for i in 0..256u16 {
    bytes.bytes[0] = i as u8;
    bytes.bytes[1] = (255 - i) as u8;
    bytes.length = 2;

    let mut doc = Document::new();

    let mut custom_type = FieldType::default();
    custom_type.set_stored(true)?;
    doc.add(new_field(
      &mut random,
      "id",
      i.to_string(),
      &custom_type,
      &mut field_types,
    )?);
    doc.add(new_string_field_binary(
      &mut random,
      "bytes",
      bytes.clone(),
      No,
      &mut field_types,
    )?);

    iw.add_document(&mut random, doc)?;
  }

  let ir = iw.get_reader(&mut random)?;
  let is = new_searcher_with_reader(ir)?;

  for i in 0..256u16 {
    bytes.bytes[0] = i as u8;
    bytes.bytes[1] = (255 - i) as u8;
    bytes.length = 2;
    let term = Term::new("bytes", bytes.clone());
    let query = TermQuery::new(term);
    let docs = is.search(query, 5)?;
    assert_eq!(docs.total_hits().value(), 1);
    let stored_doc = is.stored_fields()?.document(docs.score_docs[0].doc)?;
    let v = stored_doc.get("id")?.unwrap();
    assert_eq!(v.as_ref(), i.to_string().as_str());
  }

  Ok(())
}
#[test]
fn test_to_string() -> Result<()> {
  let bytes = BytesRef::from_bytes(vec![0xffu8, 0xfeu8]);
  let term = Term::new("foo", bytes);
  assert_eq!("foo:[ff fe]", term.to_string());
  Ok(())
}
