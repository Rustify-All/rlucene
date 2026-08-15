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
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::codecs::compressing::compressing_codec::CompressingCodec;
use crate::test_framework::core::index::base_index_file_format_test_case::BaseIndexFileFormatTestCase;
use crate::test_framework::core::index::base_stored_fields_format_test_case::BaseStoredFieldsFormatTestCase;
use crate::test_framework::core::util::lucene_test_case::{is_night_mode, random};
use rand::Rng;
use rand::prelude::StdRng;

#[allow(dead_code)] // for quick search
pub struct TestCompressingStoredFieldsFormat {
  codec: Codecs,
}

impl TestCompressingStoredFieldsFormat {
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
  F: FnOnce(&TestCompressingStoredFieldsFormat, &mut StdRng) -> Result<()>,
{
  let mut random = random();
  let case = TestCompressingStoredFieldsFormat::new(&mut random)?;
  let codec_guard = case.set_up()?;
  let result = f(&case, &mut random);
  case.tear_down(codec_guard);
  result
}
impl BaseIndexFileFormatTestCase for TestCompressingStoredFieldsFormat {
  type Defaults = crate::test_framework::core::index::base_stored_fields_format_test_case::BaseStoredFieldsFormatTestCaseDefaults;

  fn get_codec(&self) -> Result<Codecs> {
    Ok(self.codec.clone())
  }
}

impl BaseStoredFieldsFormatTestCase for TestCompressingStoredFieldsFormat {}
mod base_stored_fields_format_test_case_test {
  use crate::core::util::error::lucene_error::Result;
  use crate::test::core::codecs::lucene90::compressing::test_compressing_stored_fields_format::run_case;
  use crate::test_framework::core::index::base_stored_fields_format_test_case::BaseStoredFieldsFormatTestCase;

  #[test]
  fn test_random_stored_fields() -> Result<()> {
    run_case(|case, random| case.test_random_stored_fields(random))
  }

  #[test]
  fn test_stored_fields_order() -> Result<()> {
    run_case(|case, random| case.test_stored_fields_order(random))
  }

  #[test]
  fn test_binary_field_offset_length() -> Result<()> {
    run_case(|case, random| case.test_binary_field_offset_length(random))
  }

  #[test]
  fn test_numeric_field() -> Result<()> {
    run_case(|case, random| case.test_numeric_field(random))
  }

  #[test]
  fn test_indexed_bit() -> Result<()> {
    run_case(|case, random| case.test_indexed_bit(random))
  }

  #[test]
  fn test_read_skip() -> Result<()> {
    run_case(|case, random| case.test_read_skip(random))
  }

  #[test]
  fn test_empty_docs() -> Result<()> {
    run_case(|case, random| case.test_empty_docs(random))
  }
  #[test]
  fn test_concurrent_reads() -> Result<()> {
    run_case(|case, random| case.test_concurrent_reads(random))
  }

  #[test]
  fn test_write_read_merge() -> Result<()> {
    run_case(|case, random| case.test_write_read_merge(random))
  }

  #[test]
  fn test_merge_filter_reader() -> Result<()> {
    run_case(|case, random| case.test_merge_filter_reader(random))
  }

  #[cfg(feature = "nightly")]
  #[test]
  #[ignore = "nightly"]
  fn test_big_documents() -> Result<()> {
    run_case(|case, random| case.test_big_documents(random))
  }

  #[test]
  fn test_bulk_merge_with_deletes() -> Result<()> {
    run_case(|case, random| case.test_bulk_merge_with_deletes(random))
  }

  #[test]
  fn test_mismatched_fields() -> Result<()> {
    run_case(|case, random| case.test_mismatched_fields(random))
  }

  #[test]
  fn test_random_stored_fields_with_index_sort() -> Result<()> {
    run_case(|case, random| case.test_random_stored_fields_with_index_sort(random))
  }

  #[test]
  fn test_line_file_docs() -> Result<()> {
    run_case(|case, random| case.test_line_file_docs(random))
  }
}

mod compression_numeric_encoding_tests {
  use crate::core::codecs::CodecStoredFieldsReader;
  use crate::core::codecs::compressing::lucene90_compressing_stored_fields_reader::{
    read_tlong, read_zdouble, read_zfloat,
  };
  use crate::core::codecs::compressing::lucene90_compressing_stored_fields_writer::{
    DAY, HOUR, SECOND, write_tlong, write_zdouble, write_zfloat,
  };
  use crate::core::document::document::Document;
  use crate::core::document::stored_field::StoredField;
  use crate::core::index::codec_reader::CodecReader;
  use crate::core::index::composite_reader::CompositeReader;
  use crate::core::index::directory_reader;
  use crate::core::index::index_reader::IndexReader;
  use crate::core::index::index_writer::IndexWriter;
  use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
  use crate::core::index::no_merge_policy::NoMergePolicy;
  use crate::core::store::{ByteArrayDataInput, ByteArrayDataOutput};
  use crate::core::util::close::CloseableRef;
  use crate::core::util::error::lucene_error::Result;
  use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
  use crate::test_framework::core::codecs::compressing::compressing_codec::CompressingCodec;
  use crate::test_framework::core::util::lucene_test_case::{
    new_directory_shared, new_index_writer_config_with_analyzer, new_log_merge_policy, random,
  };
  use rand::RngExt;

  #[test]
  fn test_zfloat() -> Result<()> {
    let buffer = vec![0u8; 5]; // we never need more than 5 bytes
    let mut out = ByteArrayDataOutput::with_bytes(buffer);

    // round-trip small integer values
    for i in i16::MIN..i16::MAX {
      let f = i as f32;
      write_zfloat(&mut out, f)?;
      let mut input = ByteArrayDataInput::with_range(out.bytes.as_slice(), 0, out.get_position());
      let g = read_zfloat(&mut input)?;
      assert!(input.eof());
      assert_eq!(f.to_bits(), g.to_bits());

      // check that compression actually works
      if (-1..=123).contains(&i) {
        assert_eq!(1, out.get_position()); // single byte compression
      }
      out.reset()?;
    }

    // round-trip special values
    let special = [
      -0.0f32,
      0.0f32,
      f32::NEG_INFINITY,
      f32::INFINITY,
      f32::MIN,
      f32::MAX,
      f32::NAN,
    ];

    for &f in &special {
      write_zfloat(&mut out, f)?;
      let mut input = ByteArrayDataInput::with_range(out.bytes.as_slice(), 0, out.get_position());
      let g = read_zfloat(&mut input)?;
      assert!(input.eof());
      assert_eq!(f.to_bits(), g.to_bits());
      out.reset()?;
    }

    // round-trip random values
    let mut rng = random();
    for _ in 0..100_000 {
      let f = rng.random::<f32>() * (rng.random_range(0..100) as f32 - 50.0);
      write_zfloat(&mut out, f)?;
      let len = out.get_position();
      assert!(
        len <= if (f.to_bits() >> 31) == 1 { 5 } else { 4 },
        "length={}, f={}",
        len,
        f
      );
      let mut input = ByteArrayDataInput::with_range(out.bytes.as_slice(), 0, len);
      let g = read_zfloat(&mut input)?;
      assert!(input.eof());
      assert_eq!(f.to_bits(), g.to_bits());
      out.reset()?;
    }

    Ok(())
  }

  #[test]
  fn test_zdouble() -> Result<()> {
    let buffer = vec![0u8; 9]; // we never need more than 9 bytes
    let mut out = ByteArrayDataOutput::with_bytes(buffer);

    // round-trip small integer values
    for i in i16::MIN..i16::MAX {
      let x = i as f64;
      write_zdouble(&mut out, x)?;
      let mut input = ByteArrayDataInput::with_range(out.bytes.as_slice(), 0, out.get_position());
      let y = read_zdouble(&mut input)?;
      assert!(input.eof());
      assert_eq!(x.to_bits(), y.to_bits());

      // check that compression actually works
      if (-1..=124).contains(&i) {
        assert_eq!(1, out.get_position()); // single byte compression
      }
      out.reset()?;
    }

    // round-trip special values
    let special = [
      -0.0f64,
      0.0f64,
      f64::NEG_INFINITY,
      f64::INFINITY,
      f64::MIN,
      f64::MAX,
      f64::NAN,
    ];

    for &x in &special {
      write_zdouble(&mut out, x)?;
      let mut input = ByteArrayDataInput::with_range(out.bytes.as_slice(), 0, out.get_position());
      let y = read_zdouble(&mut input)?;
      assert!(input.eof());
      assert_eq!(x.to_bits(), y.to_bits());
      out.reset()?;
    }

    // round-trip random double values
    let mut rng = random();
    for _ in 0..100_000 {
      let x = rng.random::<f64>() * (rng.random_range(0..100) as f64 - 50.0);
      write_zdouble(&mut out, x)?;
      let len = out.get_position();
      assert!(
        len <= if x < 0.0 { 9 } else { 8 },
        "length={}, d={}",
        len,
        x
      );
      let mut input = ByteArrayDataInput::with_range(out.bytes.as_slice(), 0, len);
      let y = read_zdouble(&mut input)?;
      assert!(input.eof());
      assert_eq!(x.to_bits(), y.to_bits());
      out.reset()?;
    }

    // same with floats
    for _ in 0..100_000 {
      let x = (rng.random::<f32>() * (rng.random_range(0..100) as f32 - 50.0)) as f64;
      write_zdouble(&mut out, x)?;
      let len = out.get_position();
      assert!(len <= 5, "length={}, d={}", len, x);
      let mut input = ByteArrayDataInput::with_range(out.bytes.as_slice(), 0, len);
      let y = read_zdouble(&mut input)?;
      assert!(input.eof());
      assert_eq!(x.to_bits(), y.to_bits());
      out.reset()?;
    }

    Ok(())
  }

  #[test]
  fn test_tlong() -> Result<()> {
    let buffer = vec![0u8; 10]; // we never need more than 10 bytes
    let mut out = ByteArrayDataOutput::with_bytes(buffer);

    // round-trip small integer values
    for i in i16::MIN..i16::MAX {
      for &mul in &[SECOND, HOUR, DAY] {
        let l1 = i as i64 * mul;
        write_tlong(&mut out, l1)?;
        let mut input = ByteArrayDataInput::with_range(out.bytes.as_slice(), 0, out.get_position());
        let l2 = read_tlong(&mut input)?;
        assert!(input.eof());
        assert_eq!(l1, l2);

        // check that compression actually works
        if (-16..=15).contains(&i) {
          assert_eq!(1, out.get_position()); // single byte compression
        }
        out.reset()?;
      }
    }

    // round-trip random values
    let mut rng = random();
    for _ in 0..100_000 {
      let num_bits = rng.random_range(0..=64);
      let mask = 1i64.wrapping_shl(num_bits).wrapping_sub(1);
      let mut l1 = rng.random::<i64>() & mask;
      match rng.random_range(0..4) {
        0 => l1 = l1.wrapping_mul(SECOND),
        1 => l1 = l1.wrapping_mul(HOUR),
        2 => l1 = l1.wrapping_mul(DAY),
        _ => {},
      }
      write_tlong(&mut out, l1)?;
      let mut input = ByteArrayDataInput::with_range(out.bytes.as_slice(), 0, out.get_position());
      let l2 = read_tlong(&mut input)?;
      assert!(input.eof());
      assert_eq!(l1, l2);
      out.reset()?;
    }

    Ok(())
  }
  #[test]
  fn test_chunk_cleanup() -> Result<()> {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;
    let analyzer = MockAnalyzer::new(&mut random);
    let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
    iwc.set_merge_policy(NoMergePolicy::default());

    // We have to enforce certain things like maxDocsPerChunk to cause dirty chunks to be created
    // by this test.
    iwc.set_codec(CompressingCodec::random_instance_with_parameters(
      &mut random,
      4 * 1024,
      4,
      false,
      8,
    )?);
    let iw = IndexWriter::new(dir.clone(), iwc)?;
    let mut ir = directory_reader::open_from_writer(&iw)?;
    for _ in 0..5 {
      let mut doc = Document::new();
      doc.add(StoredField::from_string("text", "not very long at all")?);
      iw.add_document(doc)?;
      // Force flush.
      let ir2 = directory_reader::open_if_changed(&ir)?.expect("reader should change");
      ir.close()?;
      ir = ir2;
      // Examine dirty counts.
      for leaf in ir.get_sequential_sub_readers() {
        let reader = leaf
          .get_fields_reader()?
          .expect("stored fields reader should exist");
        let CodecStoredFieldsReader::Lucene90(reader) = reader else {
          panic!("compressing codec should use Lucene90 stored fields");
        };
        assert!(reader.get_num_dirty_docs()? > 0);
        assert!(reader.get_num_dirty_docs()? < 100); // Can't be gte the number of docs per chunk.
        assert_eq!(1, reader.get_num_dirty_chunks()?);
      }
    }
    iw.get_config_mut()
      .set_merge_policy(new_log_merge_policy(&mut random)?);
    iw.force_merge(1)?;
    // Add a single doc and merge again.
    let mut doc = Document::new();
    doc.add(StoredField::from_string("text", "not very long at all")?);
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
      .get_fields_reader()?
      .expect("stored fields reader should exist");
    let CodecStoredFieldsReader::Lucene90(reader) = reader else {
      panic!("compressing codec should use Lucene90 stored fields");
    };
    // At most 2: the 5 chunks from 5 doc segment will be collapsed into a single chunk.
    assert!(reader.get_num_dirty_chunks()? <= 2);
    ir.close()?;
    iw.close()?;
    dir.close()
  }
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
