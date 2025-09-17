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
use crate::core::document::field_type::FieldType;
use crate::core::document::text_field::text;
use crate::core::index::index_writer::{IndexWriter, IndexWriterBase};
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::store::directory::Directory;
use crate::core::util::error::lucene_error::Result;
use crate::test::util::lucene_test_case::lucene_test_case_util::{
    new_directory, new_field, new_index_writer_config, new_text_field, random,
};
use once_cell::sync::Lazy;
use std::clone::Clone;
use std::sync::Arc;

static STORED_TEXT_TYPE: Lazy<FieldType> =
    Lazy::new(|| FieldType::from_ref(&text::TYPE_NOT_STORED.clone()).expect("should not fail"));
pub(crate) struct TestIndexWriter;
#[test]
fn test_doc_count() -> Result<()> {
    let mut random = random();
    let dir = Arc::new(new_directory(&mut random)?);
    let writer = IndexWriter::new(dir, new_index_writer_config(&mut random))?;
    // add 100 documents
    let n = 100;
    for i in 0..n {
        add_doc_with_index(&writer, i)?;
    }
    writer.commit()?;

    let doc_stats = writer.get_doc_stats()?;
    assert_eq!(n, doc_stats.max_doc);
    assert_eq!(n, doc_stats.num_docs);

    writer.close()?;
    Ok(())
}

pub(crate) fn add_doc<D, L, B>(writer: &IndexWriter<D, L, B>) -> Result<()>
where
    D: Directory,
    L: LiveIndexWriterConfig,
    B: IndexWriterBase,
{
    let mut doc = Document::new();
    doc.add(new_text_field("content", "aaa", Store::No)?);
    let _ = writer.add_document(doc)?;
    Ok(())
}
pub(crate) fn add_doc_with_index<D, L, B>(writer: &IndexWriter<D, L, B>, index: i32) -> Result<()>
where
    D: Directory,
    L: LiveIndexWriterConfig,
    B: IndexWriterBase,
{
    let mut doc = Document::new();
    doc.add(new_field(
        "content",
        // format!("aaa {}", index).into(),
        "我",
        &STORED_TEXT_TYPE,
    )?);
    // doc.add(new_field(
    //     "id",
    //     index.to_string().into(),
    //     &STORED_TEXT_TYPE,
    // )?);

    match writer.add_document(doc) {
        Ok(_) => Ok(()),
        Err(e) => Err(e),
    }
}
