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
use crate::core::analysis::standard::standard_analyzer::StandardAnalyzer;
use crate::core::document::document::Document;
use crate::core::document::field::Store;
use crate::core::index::directory_reader;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term::Term;
use crate::core::search::phrase_query::PhraseQuery;
use crate::core::search::term_query::TermQuery;
use crate::core::search::top_docs::TopDocsLike;
use crate::core::store::FSDirectories;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::io_utils::IOUtils;
use crate::test_framework::core::util::lucene_test_case::{
  create_temp_dir_with_prefix, new_index_writer_config_with_analyzer, new_searcher_with_reader,
  new_text_field, random,
};
use std::collections::HashMap;
use std::sync::Arc;

#[allow(dead_code)] // for quick search
pub struct TestDemo;

#[test]
fn test_demo() -> Result<()> {
  let mut random = random();
  let long_term = "longtermlongtermlongtermlongtermlongtermlongtermlongtermlong\
                   termlongtermlongtermlongtermlongtermlongtermlongtermlongterm\
                   longtermlongtermlongterm";
  let text = format!("This is the text to be indexed. {}", long_term);

  let dir = Arc::new(FSDirectories::open(
    create_temp_dir_with_prefix("tempIndex")?.keep(),
  )?);
  let body_result = (|| -> Result<()> {
    let analyzer = StandardAnalyzer::new();
    let writer = IndexWriter::new(dir.clone(), IndexWriterConfig::with_analyzer(analyzer)?)?;

    let write_result = (|| -> Result<()> {
      let mut field_to_type = HashMap::new();
      let mut doc = Document::new();
      doc.add(new_text_field(
        &mut random,
        "fieldname",
        &text,
        Store::Yes,
        &mut field_to_type,
      )?);
      writer.add_document(doc)?;
      Ok(())
    })();
    IOUtils::use_or_suppress_result(write_result, writer.close())?;

    let reader = directory_reader::open(dir.clone())?;
    let searcher = new_searcher_with_reader(reader)?;
    let search_result = (|| -> Result<()> {
      assert_eq!(
        1,
        searcher.count(TermQuery::new(Term::from_text("fieldname", long_term)))?
      );

      let query = TermQuery::new(Term::from_text("fieldname", "text"));
      let hits = searcher.search(query, 1)?;
      assert_eq!(1, hits.total_hits().value());

      let mut stored_fields = searcher.stored_fields()?;
      for hit in hits.score_docs() {
        let hit_doc = stored_fields.document(hit.doc)?;
        assert_eq!(text.as_str(), hit_doc.get("fieldname")?.unwrap().as_str());
      }

      let phrase_query = PhraseQuery::from_terms_no_slop("fieldname", &["to", "be"])?;
      assert_eq!(1, searcher.count(phrase_query)?);
      Ok(())
    })();
    IOUtils::use_or_suppress_result(search_result, searcher.get_index_reader().close())
  })();
  IOUtils::use_or_suppress_result(body_result, dir.close())
}
