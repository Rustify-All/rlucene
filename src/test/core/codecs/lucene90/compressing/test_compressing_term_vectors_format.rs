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
use crate::core::codecs::Codecs;
use crate::core::codecs::codec_formats::CodecTermVectorsReader;
use crate::core::document::document::Document;
use crate::core::document::field::Field;
use crate::core::document::field_type::FieldType;
use crate::core::index::BytesRef;
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::composite_reader::CompositeReader;
use crate::core::index::directory_reader;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::index::term_vectors::TermVectors;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::codecs::compressing::compressing_codec::CompressingCodec;
use crate::test_framework::core::index::base_index_file_format_test_case::BaseIndexFileFormatTestCase;
use crate::test_framework::core::index::base_term_vectors_format_test_case::BaseTermVectorsFormatTestCase;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::util::lucene_test_case::{
  get_only_leaf_reader, is_night_mode, new_directory_shared, new_field,
  new_index_writer_config_with_analyzer, new_log_merge_policy, random,
};
use rand::Rng;
use rand::prelude::StdRng;
use std::collections::HashMap;

pub struct TestCompressingTermVectorsFormat {
  codec: Codecs,
}

impl TestCompressingTermVectorsFormat {
  fn new<R>(random: &mut R) -> Result<Self>
  where
    R: Rng + ?Sized,
  {
    let codec = if is_night_mode() {
      CompressingCodec::random_instance(random)?
    } else {
      CompressingCodec::reasonable_instance(random)?
    };
    Ok(Self {
      codec: codec.into(),
    })
  }
}

fn run_case<F>(f: F) -> Result<()>
where
  F: FnOnce(&TestCompressingTermVectorsFormat, &mut StdRng) -> Result<()>,
{
  let mut random = random();
  let case = TestCompressingTermVectorsFormat::new(&mut random)?;
  let codec_guard = case.set_up()?;
  let result = f(&case, &mut random);
  case.tear_down(codec_guard);
  result
}
mod base_term_vectors_format_test_case_tests {
  use crate::core::util::error::lucene_error::Result;
  use crate::test::core::codecs::lucene90::compressing::test_compressing_term_vectors_format::run_case;
  use crate::test_framework::core::index::base_term_vectors_format_test_case::BaseTermVectorsFormatTestCase;

  #[test]
  fn test_rare_vectors() -> Result<()> {
    run_case(|case, random| case.test_rare_vectors(random))
  }

  #[test]
  fn test_high_freqs() -> Result<()> {
    run_case(|case, random| case.test_high_freqs(random))
  }

  #[test]
  fn test_lots_of_fields() -> Result<()> {
    run_case(|case, random| case.test_lots_of_fields(random))
  }

  #[test]
  fn test_mixed_options() -> Result<()> {
    run_case(|case, random| case.test_mixed_options(random))
  }

  #[test]
  fn test_random() -> Result<()> {
    run_case(|case, random| case.test_random(random))
  }

  #[test]
  fn test_merge() -> Result<()> {
    run_case(|case, random| case.test_merge(random))
  }

  #[test]
  fn test_merge_with_deletes() -> Result<()> {
    run_case(|case, random| case.test_merge_with_deletes(random))
  }

  #[test]
  fn test_merge_with_index_sort() -> Result<()> {
    run_case(|case, random| case.test_merge_with_index_sort(random))
  }

  #[test]
  fn test_merge_with_index_sort_and_deletes() -> Result<()> {
    run_case(|case, random| case.test_merge_with_index_sort_and_deletes(random))
  }

  #[test]
  fn test_clone() -> Result<()> {
    run_case(|case, random| case.test_clone(random))
  }
  #[test]
  fn test_postings_enum_freqs() -> Result<()> {
    run_case(|case, random| case.test_postings_enum_freqs(random))
  }
  #[test]
  fn test_postings_enum_positions() -> Result<()> {
    run_case(|case, random| case.test_postings_enum_positions(random))
  }
  #[test]
  fn test_postings_enum_offsets() -> Result<()> {
    run_case(|case, random| case.test_postings_enum_offsets(random))
  }
  #[test]
  fn test_postings_enum_offsets_without_positions() -> Result<()> {
    run_case(|case, random| case.test_postings_enum_offsets_without_positions(random))
  }
  #[test]
  fn test_postings_enum_payloads() -> Result<()> {
    run_case(|case, random| case.test_postings_enum_payloads(random))
  }
  #[test]
  fn test_postings_enum_all() -> Result<()> {
    run_case(|case, random| case.test_postings_enum_all(random))
  }
}

impl BaseIndexFileFormatTestCase for TestCompressingTermVectorsFormat {
  type Defaults = crate::test_framework::core::index::base_term_vectors_format_test_case::BaseTermVectorsFormatTestCaseDefaults;

  fn get_codec(&self) -> Result<Codecs> {
    Ok(self.codec.clone())
  }
}

impl BaseTermVectorsFormatTestCase for TestCompressingTermVectorsFormat {}

#[test]
fn test_no_ords() -> Result<()> {
  run_case(|_case, random| {
    let dir = new_directory_shared(&mut *random)?;
    let iw = RandomIndexWriter::new(&mut *random, dir.clone())?;

    let mut doc = Document::new();
    let mut ft = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
    ft.set_store_term_vectors(true)?;
    doc.add(new_field(
      &mut *random,
      "foo",
      "this is a test",
      &ft,
      &mut HashMap::new(),
    )?);
    iw.add_document(&mut *random, doc)?;

    let ir = get_only_leaf_reader(&iw.get_reader(&mut *random)?)?;
    let mut term_vectors = CodecReader::term_vectors(&ir)?;
    let terms = term_vectors.get_field_terms(0, "foo")?;
    assert!(terms.is_some());

    let terms = terms.unwrap();
    let mut terms_enum = terms.iterator()?;
    assert_eq!(
      SeekStatus::Found,
      terms_enum.seek_ceil(&BytesRef::from_string("this"))?
    );

    let err = terms_enum.ord();
    assert!(matches!(err, Err(LuceneError::UnsupportedOperation(_))));

    let err = terms_enum.seek_exact_with_ord(0);
    assert!(matches!(err, Err(LuceneError::UnsupportedOperation(_))));

    ir.close()?;
    iw.close(&mut *random)?;
    dir.close()
  })
}
#[test]
fn test_chunk_cleanup() -> Result<()> {
  run_case(|_case, random| {
    let dir = new_directory_shared(&mut *random)?;
    let analyzer = MockAnalyzer::new(&mut *random);
    let mut iwc = new_index_writer_config_with_analyzer(&mut *random, analyzer)?;
    iwc.set_merge_policy(NoMergePolicy::default());

    // We have to enforce certain things like maxDocsPerChunk to cause dirty chunks to be created by
    // this test.
    iwc.set_codec(CompressingCodec::random_instance_with_parameters(
      &mut *random,
      4 * 1024,
      4,
      false,
      8,
    )?);
    let iw = IndexWriter::new(dir.clone(), iwc)?;
    let mut ir = directory_reader::open_from_writer(&iw)?;
    for _ in 0..5 {
      let mut doc = Document::new();
      let mut ft = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
      ft.set_store_term_vectors(true)?;
      doc.add(Field::new("text", "not very long at all", ft));
      iw.add_document(doc)?;
      // Force flush.
      let ir2 = directory_reader::open_if_changed(&ir)?.expect("reader should change");
      ir.close()?;
      ir = ir2;
      // Examine dirty counts.
      for leaf in ir.get_sequential_sub_readers() {
        let reader = leaf
          .get_term_vectors_reader()?
          .expect("term vectors reader should exist");
        let CodecTermVectorsReader::Lucene90(reader) = reader else {
          panic!("compressing codec should use Lucene90 term vectors");
        };
        assert!(reader.get_num_dirty_docs()? > 0);
        assert_eq!(1, reader.get_num_dirty_chunks()?);
      }
    }
    iw.get_config_mut()
      .set_merge_policy(new_log_merge_policy(&mut *random)?);
    iw.force_merge(1)?;
    // Add one more doc and merge again.
    let mut doc = Document::new();
    let mut ft = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
    ft.set_store_term_vectors(true)?;
    doc.add(Field::new("text", "not very long at all", ft));
    iw.add_document(doc)?;
    iw.force_merge(1)?;
    let ir2 = directory_reader::open_if_changed(&ir)?.expect("reader should change");
    ir.close()?;
    ir = ir2;
    let leaf = ir
      .get_sequential_sub_readers()
      .first()
      .expect("reader should have one leaf");
    assert_eq!(1, ir.get_sequential_sub_readers().len());
    let reader = leaf
      .get_term_vectors_reader()?
      .expect("term vectors reader should exist");
    let CodecTermVectorsReader::Lucene90(reader) = reader else {
      panic!("compressing codec should use Lucene90 term vectors");
    };
    // At most 2: the 5 chunks from 5 doc segment will be collapsed into a single chunk.
    assert!(reader.get_num_dirty_chunks()? <= 2);
    ir.close()?;
    iw.close()?;
    dir.close()
  })
}

mod base_index_file_format_test_case_test {
  use super::run_case;
  use crate::core::util::error::lucene_error::Result;
  use crate::test_framework::core::index::base_index_file_format_test_case::BaseIndexFileFormatTestCase;

  #[test]
  fn test_merge_stability() -> Result<()> {
    run_case(|case, random| case.test_merge_stability(random))
  }

  #[test]
  fn test_multi_close() -> Result<()> {
    run_case(|case, random| case.test_multi_close(random))
  }

  #[test]
  fn test_random_exceptions() -> Result<()> {
    run_case(|case, random| case.test_random_exceptions(random))
  }

  #[test]
  fn test_check_integrity_reads_all_bytes() -> Result<()> {
    run_case(|case, random| case.test_check_integrity_reads_all_bytes(random))
  }
}
