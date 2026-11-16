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
use crate::core::document::field_type::FieldType;
use crate::core::document::text_field::TYPE_STORED;
use crate::core::index::check_index::{CheckIndex, Level};
use crate::core::index::index_writer::{IndexWriter, MAX_TERM_LENGTH};
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::term::Term;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::store::directory::Directory;
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind, Result};
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::util::line_file_docs::LineFileDocs;
use crate::test_framework::core::util::lucene_test_case::{
  new_field, new_index_writer_config_with_analyzer,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::Rng;
use std::collections::HashMap;
use std::io::Sink;
use std::sync::Arc;

/// Base trait for CheckIndex tests.
pub trait BaseTestCheckIndex {
  fn test_deleted_docs<D, R>(&self, random: &mut R, dir: &Arc<D>) -> Result<()>
  where
    D: Directory + 'static,
    R: Rng + ?Sized,
  {
    let analyzer = MockAnalyzer::new(random);
    let mut config = new_index_writer_config_with_analyzer(random, analyzer)?;
    config.set_max_buffered_docs(2);
    let writer = IndexWriter::new(Arc::clone(dir), config)?;
    let mut field_types = HashMap::new();

    for i in 0..19 {
      let mut doc = Document::new();
      let mut custom_type = FieldType::from_ref(&*TYPE_STORED)?;
      custom_type.set_store_term_vectors(true)?;
      custom_type.set_store_term_vector_positions(true)?;
      custom_type.set_store_term_vector_offsets(true)?;
      doc.add(new_field(
        random,
        "field",
        format!("aaa{i}"),
        &custom_type,
        &mut field_types,
      )?);
      writer.add_document(doc)?;
    }
    writer.force_merge(1)?;
    writer.commit()?;
    writer.delete_documents_with_terms(vec![Term::from_text("field", "aaa5")])?;
    writer.close()?;

    let mut output = Vec::with_capacity(1024);
    let mut checker = CheckIndex::new(Arc::clone(dir))?;
    checker.set_info_stream(&mut output);
    checker.set_level(Level::MIN_LEVEL_FOR_INTEGRITY_CHECKS)?;
    let index_status = checker.check_index()?;
    if !index_status.clean {
      panic!("CheckIndex failed\n{}", String::from_utf8_lossy(&output));
    }

    let segment = &index_status.segment_infos[0];
    assert!(segment.open_reader_passed);

    let diagnostics = segment.diagnostics.as_ref().expect("diagnostics");

    let field_norm_status = segment
      .field_norm_status
      .as_ref()
      .expect("field norm status");
    assert!(field_norm_status.error.is_none());
    assert_eq!(1, field_norm_status.tot_fields);

    let term_index_status = segment
      .term_index_status
      .as_ref()
      .expect("term index status");
    assert!(term_index_status.error.is_none());
    assert_eq!(18, term_index_status.term_count);
    assert_eq!(18, term_index_status.tot_freq);
    assert_eq!(18, term_index_status.tot_pos);

    let stored_field_status = segment
      .stored_field_status
      .as_ref()
      .expect("stored field status");
    assert!(stored_field_status.error.is_none());
    assert_eq!(18, stored_field_status.doc_count);
    assert_eq!(18, stored_field_status.tot_fields);

    let term_vector_status = segment
      .term_vector_status
      .as_ref()
      .expect("term vector status");
    assert!(term_vector_status.error.is_none());
    assert_eq!(18, term_vector_status.doc_count);
    assert_eq!(18, term_vector_status.tot_vectors);

    assert!(diagnostics.get("lucene.version").is_some());

    assert!(!diagnostics.is_empty());
    let only_segments = ["_0".to_string()];
    assert!(
      checker
        .check_index_with_segments(Some(&only_segments))?
        .clean
    );
    checker.close()
  }

  fn test_checksums_only<D, R>(&self, random: &mut R, dir: &Arc<D>) -> Result<()>
  where
    D: Directory + 'static,
    R: Rng + ?Sized,
  {
    let mut line_file_docs = LineFileDocs::new(random)?;
    let mut analyzer = MockAnalyzer::new(random);
    analyzer.set_max_token_length(TestUtil::next_int(random, 1, MAX_TERM_LENGTH));
    let config = new_index_writer_config_with_analyzer(random, analyzer)?;
    let writer = IndexWriter::new(Arc::clone(dir), config)?;

    for _ in 0..100 {
      writer.add_document(line_file_docs.next_doc()?)?;
    }
    writer.add_document(Document::new())?;
    writer.commit()?;
    writer.close()?;
    line_file_docs.close();

    let mut output = Vec::with_capacity(1024);
    let mut checker = CheckIndex::new(Arc::clone(dir))?;
    checker.set_info_stream(&mut output);
    let index_status = checker.check_index()?;
    assert!(index_status.clean);
    checker.close()
  }

  fn test_checksums_only_verbose<D, R>(&self, random: &mut R, dir: &Arc<D>) -> Result<()>
  where
    D: Directory + 'static,
    R: Rng + ?Sized,
  {
    let mut line_file_docs = LineFileDocs::new(random)?;
    let mut analyzer = MockAnalyzer::new(random);
    analyzer.set_max_token_length(TestUtil::next_int(random, 1, MAX_TERM_LENGTH));
    let config = new_index_writer_config_with_analyzer(random, analyzer)?;
    let writer = IndexWriter::new(Arc::clone(dir), config)?;

    for _ in 0..100 {
      writer.add_document(line_file_docs.next_doc()?)?;
    }
    writer.add_document(Document::new())?;
    writer.commit()?;
    writer.close()?;
    line_file_docs.close();

    let mut output = Vec::with_capacity(1024);
    let mut checker = CheckIndex::new(Arc::clone(dir))?;
    checker.set_info_stream(&mut output);
    let index_status = checker.check_index()?;
    assert!(index_status.clean);
    checker.close()
  }

  fn test_obtains_lock<D>(&self, dir: &Arc<D>) -> Result<()>
  where
    D: Directory + 'static,
  {
    let writer = IndexWriter::new(Arc::clone(dir), IndexWriterConfig::new()?)?;
    writer.add_document(Document::new())?;
    writer.commit()?;

    // Keep IndexWriter open... should not be able to obtain the write lock.
    let result = CheckIndex::<_, _, Sink>::new(Arc::clone(dir));
    assert!(matches!(
      result.as_ref().map_err(LuceneError::kind),
      Err(LuceneErrorKind::LockObtainFailed(_))
    ));

    writer.close()
  }
}
