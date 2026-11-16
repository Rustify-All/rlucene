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
use crate::core::store::IO_CONTEXT_DEFAULT;
use crate::core::util::CoreHelper;
use std::borrow::Cow;

use num_bigint::BigInt;
use num_traits::{FromPrimitive, ToPrimitive};
use rand::Rng;
use rand::RngExt;
use rand::prelude::IndexedRandom;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, LazyLock};

use crate::core::codecs::Codec;
use crate::core::codecs::lucene90_doc_values_format::Lucene90DocValuesFormat;
use crate::core::codecs::lucene99::lucene99_hnsw_vectors_format::Lucene99HnswVectorsFormat;
use crate::core::codecs::lucene101::lucene101_postings_format::Lucene101PostingsFormat;
use crate::core::codecs::lucene101_codec::Lucene101Codec;
use crate::core::index::CODEC_FILE_PATTERN;
use crate::core::index::check_index::{CheckIndex, Status};
use crate::core::index::composite_reader::CompositeReader;
use crate::core::index::directory_reader::DirectoryReader;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::index_reader::{
  CompositeReaderContextKind, IndexReader, LeafReaderContextKind,
};
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::merge_policy::{MergePolicy, MergePolicyEnum, OneMerge};
use crate::core::index::merge_scheduler::MergeSchedulerEnum;
use crate::core::index::multi_terms::{TermsType, get_terms};
use crate::core::index::postings_enum::{ALL, FREQS, OFFSETS, PAYLOADS, POSITIONS};
use crate::core::index::slow_codec_reader_wrapper::SlowCodecReaderWrapper;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::index::{BytesRef, IndexFileNames};
use crate::core::search::field_value_hit_queue::TopFieldScoreDoc;
use crate::core::search::score_doc::ScoreDocLike;
use crate::core::search::top_docs::TopDocs;
use crate::core::search::total_hits::Relation;
use crate::core::store::ByteBuffersDirectory;

use crate::core::store::directory::Directory;
use crate::core::store::no_lock_factory::NoLock;
use crate::core::store::single_instance_lock_factory::SingleInstanceLockFactory;
use crate::core::util::access::SharedAccessVec;
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind, Result};
use crate::core::util::io_utils::IOUtils;
use crate::test_framework::core::codecs::asserting_codec::{
  AssertingCodec, AssertingCodecDocValuesFormat, AssertingCodecKnnVectorsFormat,
  AssertingCodecPostingsFormat,
};

pub struct TestUtil;

/// The concrete codec returned by [`TestUtil::get_default_codec`].
pub type DefaultCodec = Lucene101Codec;
pub type DefaultPostingsFormat = Lucene101PostingsFormat;
pub type DefaultDocValuesFormat = Lucene90DocValuesFormat;
pub type DefaultStoredFieldsFormat = <DefaultCodec as Codec>::StoredFieldsFormat;
pub type DefaultTermVectorsFormat = <DefaultCodec as Codec>::TermVectorsFormat;
pub type DefaultNormsFormat = <DefaultCodec as Codec>::NormsFormat;
pub type DefaultLiveDocsFormat = <DefaultCodec as Codec>::LiveDocsFormat;
pub type DefaultPointsFormat = <DefaultCodec as Codec>::PointsFormat;

/// Static dispatch for the two `IndexReader` shapes accepted by `TestUtil.checkReader`.
pub trait CheckReaderContextKind<I>
where
  I: IndexReader,
{
  fn check_reader(reader: &I, level: i32) -> Result<()>;
}

impl<I> CheckReaderContextKind<I> for LeafReaderContextKind
where
  I: LeafReader,
{
  fn check_reader(reader: &I, level: i32) -> Result<()> {
    TestUtil::check_reader_with_level(reader, level)
  }
}

impl<I> CheckReaderContextKind<I> for CompositeReaderContextKind
where
  I: CompositeReader,
  I::LeafReader: LeafReader,
{
  fn check_reader(reader: &I, level: i32) -> Result<()> {
    let context = <&I as IndexReader>::get_context(reader)?;
    for leaf in context.leaves()? {
      TestUtil::check_reader_with_level(leaf.reader(), level)?;
    }
    Ok(())
  }
}

const BLOCK_STARTS: &[u32] = &[
  0x0000, 0x0080, 0x0100, 0x0180, 0x0250, 0x02B0, 0x0300, 0x0370, 0x0400, 0x0500, 0x0530, 0x0590,
  0x0600, 0x0700, 0x0750, 0x0780, 0x07C0, 0x0800, 0x0900, 0x0980, 0x0A00, 0x0A80, 0x0B00, 0x0B80,
  0x0C00, 0x0C80, 0x0D00, 0x0D80, 0x0E00, 0x0E80, 0x0F00, 0x1000, 0x10A0, 0x1100, 0x1200, 0x1380,
  0x13A0, 0x1400, 0x1680, 0x16A0, 0x1700, 0x1720, 0x1740, 0x1760, 0x1780, 0x1800, 0x18B0, 0x1900,
  0x1950, 0x1980, 0x19E0, 0x1A00, 0x1A20, 0x1B00, 0x1B80, 0x1C00, 0x1C50, 0x1CD0, 0x1D00, 0x1D80,
  0x1DC0, 0x1E00, 0x1F00, 0x2000, 0x2070, 0x20A0, 0x20D0, 0x2100, 0x2150, 0x2190, 0x2200, 0x2300,
  0x2400, 0x2440, 0x2460, 0x2500, 0x2580, 0x25A0, 0x2600, 0x2700, 0x27C0, 0x27F0, 0x2800, 0x2900,
  0x2980, 0x2A00, 0x2B00, 0x2C00, 0x2C60, 0x2C80, 0x2D00, 0x2D30, 0x2D80, 0x2DE0, 0x2E00, 0x2E80,
  0x2F00, 0x2FF0, 0x3000, 0x3040, 0x30A0, 0x3100, 0x3130, 0x3190, 0x31A0, 0x31C0, 0x31F0, 0x3200,
  0x3300, 0x3400, 0x4DC0, 0x4E00, 0xA000, 0xA490, 0xA4D0, 0xA500, 0xA640, 0xA6A0, 0xA700, 0xA720,
  0xA800, 0xA830, 0xA840, 0xA880, 0xA8E0, 0xA900, 0xA930, 0xA960, 0xA980, 0xAA00, 0xAA60, 0xAA80,
  0xABC0, 0xAC00, 0xD7B0, 0xE000, 0xF900, 0xFB00, 0xFB50, 0xFE00, 0xFE10, 0xFE20, 0xFE30, 0xFE50,
  0xFE70, 0xFF00, 0xFFF0, 0x10000, 0x10080, 0x10100, 0x10140, 0x10190, 0x101D0, 0x10280, 0x102A0,
  0x10300, 0x10330, 0x10380, 0x103A0, 0x10400, 0x10450, 0x10480, 0x10800, 0x10840, 0x10900,
  0x10920, 0x10A00, 0x10A60, 0x10B00, 0x10B40, 0x10B60, 0x10C00, 0x10E60, 0x11080, 0x12000,
  0x12400, 0x13000, 0x1D000, 0x1D100, 0x1D200, 0x1D300, 0x1D360, 0x1D400, 0x1F000, 0x1F030,
  0x1F100, 0x1F200, 0x20000, 0x2A700, 0x2F800, 0xE0000, 0xE0100, 0xF0000, 0x100000,
];
const BLOCK_ENDS: &[u32] = &[
  0x007F, 0x00FF, 0x017F, 0x024F, 0x02AF, 0x02FF, 0x036F, 0x03FF, 0x04FF, 0x052F, 0x058F, 0x05FF,
  0x06FF, 0x074F, 0x077F, 0x07BF, 0x07FF, 0x083F, 0x097F, 0x09FF, 0x0A7F, 0x0AFF, 0x0B7F, 0x0BFF,
  0x0C7F, 0x0CFF, 0x0D7F, 0x0DFF, 0x0E7F, 0x0EFF, 0x0FFF, 0x109F, 0x10FF, 0x11FF, 0x137F, 0x139F,
  0x13FF, 0x167F, 0x169F, 0x16FF, 0x171F, 0x173F, 0x175F, 0x177F, 0x17FF, 0x18AF, 0x18FF, 0x194F,
  0x197F, 0x19DF, 0x19FF, 0x1A1F, 0x1AAF, 0x1B7F, 0x1BBF, 0x1C4F, 0x1C7F, 0x1CFF, 0x1D7F, 0x1DBF,
  0x1DFF, 0x1EFF, 0x1FFF, 0x206F, 0x209F, 0x20CF, 0x20FF, 0x214F, 0x218F, 0x21FF, 0x22FF, 0x23FF,
  0x243F, 0x245F, 0x24FF, 0x257F, 0x259F, 0x25FF, 0x26FF, 0x27BF, 0x27EF, 0x27FF, 0x28FF, 0x297F,
  0x29FF, 0x2AFF, 0x2BFF, 0x2C5F, 0x2C7F, 0x2CFF, 0x2D2F, 0x2D7F, 0x2DDF, 0x2DFF, 0x2E7F, 0x2EFF,
  0x2FDF, 0x2FFF, 0x303F, 0x309F, 0x30FF, 0x312F, 0x318F, 0x319F, 0x31BF, 0x31EF, 0x31FF, 0x32FF,
  0x33FF, 0x4DBF, 0x4DFF, 0x9FFF, 0xA48F, 0xA4CF, 0xA4FF, 0xA63F, 0xA69F, 0xA6FF, 0xA71F, 0xA7FF,
  0xA82F, 0xA83F, 0xA87F, 0xA8DF, 0xA8FF, 0xA92F, 0xA95F, 0xA97F, 0xA9DF, 0xAA5F, 0xAA7F, 0xAADF,
  0xABFF, 0xD7AF, 0xD7FF, 0xF8FF, 0xFAFF, 0xFB4F, 0xFDFF, 0xFE0F, 0xFE1F, 0xFE2F, 0xFE4F, 0xFE6F,
  0xFEFF, 0xFFEF, 0xFFFF, 0x1007F, 0x100FF, 0x1013F, 0x1018F, 0x101CF, 0x101FF, 0x1029F, 0x102DF,
  0x1032F, 0x1034F, 0x1039F, 0x103DF, 0x1044F, 0x1047F, 0x104AF, 0x1083F, 0x1085F, 0x1091F,
  0x1093F, 0x10A5F, 0x10A7F, 0x10B3F, 0x10B5F, 0x10B7F, 0x10C4F, 0x10E7F, 0x110CF, 0x123FF,
  0x1247F, 0x1342F, 0x1D0FF, 0x1D1FF, 0x1D24F, 0x1D35F, 0x1D37F, 0x1D7FF, 0x1F02F, 0x1F09F,
  0x1F1FF, 0x1F2FF, 0x2A6DF, 0x2B73F, 0x2FA1F, 0xE007F, 0xE01EF, 0xFFFFF, 0x10FFFF,
];

impl TestUtil {
  /// Return a Codec that can read any of the default codecs and formats, but always writes in the
  /// specified format.
  pub fn always_postings_format<T>(format: T) -> AssertingCodec
  where
    T: Into<AssertingCodecPostingsFormat>,
  {
    AssertingCodec::with_postings_format(format)
  }

  /// Return a Codec that can read any of the default codecs and formats, but always writes in the
  /// specified format.
  pub fn always_doc_values_format<T>(format: T) -> AssertingCodec
  where
    T: Into<AssertingCodecDocValuesFormat>,
  {
    AssertingCodec::with_doc_values_format(format)
  }

  /// Return a Codec that can read any of the default codecs and formats, but always writes in the
  /// specified format.
  pub fn always_knn_vectors_format<T>(format: T) -> AssertingCodec
  where
    T: Into<AssertingCodecKnnVectorsFormat>,
  {
    AssertingCodec::with_knn_vectors_format(format)
  }

  /// Returns the actual default codec for this version of Lucene.
  pub fn get_default_codec() -> DefaultCodec {
    match DefaultCodec::new() {
      Ok(codec) => codec,
      Err(error) => unreachable!("invalid default codec settings: {error}"),
    }
  }

  /// Returns the actual default postings format for this version of Lucene.
  pub fn get_default_postings_format() -> DefaultPostingsFormat {
    match DefaultPostingsFormat::new() {
      Ok(format) => format,
      Err(error) => unreachable!("invalid default postings format settings: {error}"),
    }
  }

  /// Returns the actual default doc values format for this version of Lucene.
  pub fn get_default_doc_values_format() -> DefaultDocValuesFormat {
    match DefaultDocValuesFormat::new() {
      Ok(format) => format,
      Err(error) => unreachable!("invalid default doc values format settings: {error}"),
    }
  }

  /// Returns the actual default vector format for this version of Lucene.
  pub fn get_default_knn_vectors_format() -> Result<Lucene99HnswVectorsFormat> {
    Lucene99HnswVectorsFormat::new()
  }

  pub fn sync_concurrent_merges<D>(writer: &IndexWriter<D>) -> Result<()>
  where
    D: Directory,
  {
    Self::sync_concurrent_merges_with_scheduler(writer.get_config()?.get_merge_scheduler())
  }

  pub fn sync_concurrent_merges_with_scheduler(merge_scheduler: &MergeSchedulerEnum) -> Result<()> {
    if let MergeSchedulerEnum::Concurrent(concurrent_merge_scheduler) = merge_scheduler {
      concurrent_merge_scheduler.sync()?;
    }
    Ok(())
  }

  pub fn string_codepoint_comparator(a: &str, b: &str) -> std::cmp::Ordering {
    let mut a_chars = a.chars();
    let mut b_chars = b.chars();

    loop {
      match (a_chars.next(), b_chars.next()) {
        (Some(a_cp), Some(b_cp)) => {
          if a_cp != b_cp {
            return a_cp.cmp(&b_cp);
          }
        },
        (None, None) => return std::cmp::Ordering::Equal,
        (None, _) => return std::cmp::Ordering::Less,
        (_, None) => return std::cmp::Ordering::Greater,
      }
    }
  }
  pub fn next_usize<R>(random: &mut R, start: usize, end: usize) -> usize
  where
    R: Rng + ?Sized,
  {
    random.random_range(start..=end)
  }
  /// start and end are BOTH inclusive
  pub fn next_int<R>(random: &mut R, start: i32, end: i32) -> i32
  where
    R: Rng + ?Sized,
  {
    random.random_range(start..=end)
  }
  /// start and end are BOTH inclusive
  pub fn next_long<R>(random: &mut R, start: i64, end: i64) -> i64
  where
    R: Rng + ?Sized,
  {
    assert!(end >= start, "start={}, end={}", start, end);
    let range = BigInt::from(end) + BigInt::from(1) - BigInt::from(start);
    if range <= BigInt::from(i32::MAX) {
      start + random.random_range(0..range.to_i32().unwrap()) as i64
    } else {
      let augend = BigInt::from_f64(range.to_f64().unwrap() * random.random::<f64>()).unwrap();
      let result = BigInt::from(start) + augend;
      let result = result.to_i64().unwrap();
      assert!(result >= start);
      assert!(result <= end);
      result
    }
  }
  /// Returns a random big integer with `1 .. max_bytes` storage.
  pub fn next_big_integer<R>(random: &mut R, max_bytes: usize) -> BigInt
  where
    R: Rng + ?Sized,
  {
    let length = Self::next_usize(random, 1, max_bytes);
    let mut buffer = vec![0u8; length];
    random.fill_bytes(&mut buffer);
    BigInt::from_signed_bytes_be(&buffer)
  }
  pub fn random_simple_string_with_len<R>(random: &mut R, max_length: usize) -> String
  where
    R: Rng + ?Sized,
  {
    Self::random_simple_string_range(random, 0, max_length)
  }
  pub fn random_simple_string_range<R>(
    random: &mut R,
    min_length: usize,
    max_length: usize,
  ) -> String
  where
    R: Rng + ?Sized,
  {
    let end = random.random_range(min_length..=max_length);
    if end == 0 {
      return String::new();
    }
    (0..end)
      .map(|_| random.random_range(b'a'..=b'z') as char)
      .collect()
  }

  pub fn random_simple_string<R>(random: &mut R) -> String
  where
    R: Rng + ?Sized,
  {
    Self::random_simple_string_range(random, 0, 10)
  }

  pub fn add_indexes_slowly<D, CR>(writer: &IndexWriter<D>, readers: &[CR]) -> Result<()>
  where
    D: Directory + 'static,
    CR: DirectoryReader,
    CR::LeafReader: Clone + 'static,
    OneMerge<
      D,
      Arc<crate::core::index::slow_codec_reader_wrapper::CodecReaderImpl<CR::LeafReader>>,
    >: Send + 'static,
  {
    let mut leaves = Vec::new();
    for reader in readers {
      let reader_context = reader.get_context()?;
      for context in reader_context.leaves()? {
        leaves.push(SlowCodecReaderWrapper::wrap_leaf_reader(
          context.reader().clone(),
        ));
      }
    }
    writer.add_indexes_from_codec_readers(leaves)?;
    Ok(())
  }

  /// Just tries to configure things to keep the open file count lowish.
  pub fn reduce_open_files<D>(w: &IndexWriter<D>) -> Result<()>
  where
    D: Directory,
  {
    // Keep number of open files lowish.
    let mut merge_policy = w.get_config()?.get_merge_policy_mut();
    merge_policy.get_base_mut().set_no_cfs_ratio(1.0)?;
    match &mut *merge_policy {
      MergePolicyEnum::LogDoc(lmp) => {
        lmp.set_merge_factor(std::cmp::min(5, lmp.get_merge_factor()))?;
      },
      MergePolicyEnum::LogBytesSize(lmp) => {
        lmp.set_merge_factor(std::cmp::min(5, lmp.get_merge_factor()))?;
      },
      MergePolicyEnum::Alcoholic(lmp) => {
        lmp.set_merge_factor(std::cmp::min(5, lmp.get_merge_factor()))?;
      },
      MergePolicyEnum::Tiered(tmp) => {
        tmp.set_max_merge_at_once(std::cmp::min(5, tmp.get_max_merge_at_once()))?;
        tmp.set_segments_per_tier(CoreHelper::min_f64(tmp.get_segments_per_tier(), 5.0))?;
      },
      _ => {},
    }
    drop(merge_policy);
    let merge_scheduler = w.get_config()?.get_merge_scheduler();
    if let MergeSchedulerEnum::Concurrent(cms) = merge_scheduler {
      cms.set_max_merges_and_threads(3, 2)?;
    }
    Ok(())
  }

  /// Assert that the given [`TopDocs`] have the same top docs and consistent hit counts.
  pub(crate) fn assert_consistent(
    expected: &TopDocs<TopFieldScoreDoc>,
    actual: &TopDocs<TopFieldScoreDoc>,
  ) {
    assert_eq!(
      expected.total_hits.value() == 0,
      actual.total_hits.value() == 0,
      "wrong total hits"
    );
    if expected.total_hits.relation() == Relation::EqualTo {
      if actual.total_hits.relation() == Relation::EqualTo {
        assert_eq!(
          expected.total_hits.value(),
          actual.total_hits.value(),
          "wrong total hits"
        );
      } else {
        assert!(
          expected.total_hits.value() >= actual.total_hits.value(),
          "wrong total hits"
        );
      }
    } else if actual.total_hits.relation() == Relation::EqualTo {
      assert!(
        expected.total_hits.value() <= actual.total_hits.value(),
        "wrong total hits"
      );
    }
    assert_eq!(
      expected.score_docs.len(),
      actual.score_docs.len(),
      "wrong hit count"
    );
    for (expected, actual) in expected.score_docs.iter().zip(&actual.score_docs) {
      assert_eq!(expected.doc(), actual.doc(), "wrong hit docID");
      assert!(
        expected.score() == actual.score() || expected.score().is_nan() && actual.score().is_nan(),
        "wrong hit score: expected={} actual={}",
        expected.score(),
        actual.score()
      );
      match expected {
        TopFieldScoreDoc::Field(expected) => {
          let TopFieldScoreDoc::Field(actual) = actual else {
            panic!("actual hit is not a FieldDoc");
          };
          assert_eq!(expected.fields, actual.fields, "wrong sort field values");
        },
        _ => assert!(
          !matches!(actual, TopFieldScoreDoc::Field(_)),
          "actual hit is a FieldDoc"
        ),
      }
    }
  }

  /// This runs the CheckIndex tool on the index in. If any issues are hit, a runtime error is
  /// returned; otherwise, the index status is returned.
  pub fn check_index<R, D>(random: &mut R, dir: D) -> Result<Status<D>>
  where
    R: Rng + ?Sized,
    D: Directory,
  {
    Self::check_index_with_level(
      random,
      dir,
      crate::core::index::check_index::Level::MIN_LEVEL_FOR_SLOW_CHECKS,
    )
  }

  pub fn check_index_with_level<R, D>(random: &mut R, dir: D, level: i32) -> Result<Status<D>>
  where
    R: Rng + ?Sized,
    D: Directory,
  {
    Self::check_index_with_options(random, Arc::new(dir), level, false, true, None)
  }

  /// If `fail_fast` is true, return the first error when index corruption is hit, instead
  /// of moving on to other fields/segments to look for any other corruption.
  pub fn check_index_with_options<R, D>(
    random: &mut R,
    dir: Arc<D>,
    level: i32,
    fail_fast: bool,
    concurrent: bool,
    output: Option<&mut Vec<u8>>,
  ) -> Result<Status<D>>
  where
    R: Rng + ?Sized,
    D: Directory,
  {
    let mut default_output = if output.is_none() {
      Vec::with_capacity(1024)
    } else {
      Vec::new()
    };
    let output = output.unwrap_or(&mut default_output);

    let mut checker = CheckIndex::with_lock(dir, NoLock);
    checker.set_level(level)?;
    checker.set_fail_fast(fail_fast);
    checker.set_info_stream_with_verbose(&mut *output, false);
    if concurrent {
      checker.set_thread_count(random.random_range(2..=5))?;
    } else {
      checker.set_thread_count(1)?;
    }
    let result = catch_unwind(AssertUnwindSafe(|| checker.check_index()));
    let close_result = catch_unwind(AssertUnwindSafe(|| checker.close()));
    drop(checker);
    let result = IOUtils::use_or_suppress_caught_result(result, close_result);
    match result {
      Ok(index_status) if !index_status.clean => {
        println!("CheckIndex failed");
        println!("{}", String::from_utf8_lossy(output));
        Err(LuceneError::illegal_state("CheckIndex failed"))
      },
      Ok(index_status) => {
        if cfg!(feature = "test_log_verbose") {
          println!("{}", String::from_utf8_lossy(output));
        }
        Ok(index_status)
      },
      Err(error) => Err(error),
    }
  }

  /// This runs the CheckIndex tool on the reader. If any issues are hit, a runtime error is
  /// returned.
  pub fn check_reader<I>(reader: &I) -> Result<()>
  where
    I: IndexReader,
    I::ContextKind: CheckReaderContextKind<I>,
  {
    I::ContextKind::check_reader(
      reader,
      crate::core::index::check_index::Level::MIN_LEVEL_FOR_SLOW_CHECKS,
    )
  }

  pub fn check_reader_with_level<I>(reader: &I, level: i32) -> Result<()>
  where
    I: LeafReader,
  {
    let mut output = Vec::with_capacity(1024);
    // A generic LeafReader may not expose codec-level integrity checks. If it reports
    // UnsupportedOperation, the slow wrapper below supplies the codec view used by CheckIndex.
    if let Err(error) = reader.check_integrity()
      && !matches!(error.kind(), LuceneErrorKind::UnsupportedOperation(_))
    {
      return Err(error);
    }
    let codec_reader = SlowCodecReaderWrapper::wrap_leaf_reader(reader);

    CheckIndex::test_live_docs(&codec_reader, Some(&mut output), true)?;
    CheckIndex::test_field_infos(&codec_reader, Some(&mut output), true)?;
    CheckIndex::test_field_norms(&codec_reader, Some(&mut output), true)?;
    CheckIndex::test_postings(&codec_reader, Some(&mut output), false, level, true)?;
    CheckIndex::test_stored_fields(&codec_reader, Some(&mut output), true)?;
    CheckIndex::test_term_vectors(&codec_reader, Some(&mut output), false, level, true)?;
    CheckIndex::test_doc_values(&codec_reader, Some(&mut output), true)?;
    CheckIndex::test_points(&codec_reader, Some(&mut output), true)?;

    // some checks really against the reader API
    Self::check_reader_sanity(reader)?;

    if cfg!(feature = "test_log_verbose") {
      println!("{}", String::from_utf8_lossy(&output));
    }

    // FieldInfos should be cached at the reader and always return the same instance
    if !Arc::ptr_eq(&reader.get_field_infos()?, &reader.get_field_infos()?) {
      return Err(LuceneError::illegal_state(format!(
        "getFieldInfos() returned different instances for reader: {reader}"
      )));
    }

    Ok(())
  }

  // used by TestUtil.checkReader to check some things really unrelated to the index,
  // just looking for bugs in indexreader implementations.
  fn check_reader_sanity<I>(reader: &I) -> Result<()>
  where
    I: LeafReader,
  {
    for info in reader.get_field_infos()?.iter() {
      // reader shouldn't return normValues if the field does not have them
      if !info.has_norms() && reader.get_norm_values(&info.name)?.is_some() {
        return Err(LuceneError::illegal_state(format!(
          "field: {} should omit norms but has them!",
          info.name
        )));
      }

      // reader shouldn't return docValues if the field does not have them
      // reader shouldn't return multiple docvalues types for the same field.
      let has_multiple_types = match info.get_doc_values_type() {
        DocValuesType::None => {
          reader.get_binary_doc_values(&info.name)?.is_some()
            || reader.get_numeric_doc_values(&info.name)?.is_some()
            || reader.get_sorted_doc_values(&info.name)?.is_some()
            || reader.get_sorted_set_doc_values(&info.name)?.is_some()
        },
        DocValuesType::Sorted => {
          reader.get_binary_doc_values(&info.name)?.is_some()
            || reader.get_numeric_doc_values(&info.name)?.is_some()
            || reader.get_sorted_numeric_doc_values(&info.name)?.is_some()
            || reader.get_sorted_set_doc_values(&info.name)?.is_some()
        },
        DocValuesType::SortedNumeric => {
          reader.get_binary_doc_values(&info.name)?.is_some()
            || reader.get_numeric_doc_values(&info.name)?.is_some()
            || reader.get_sorted_set_doc_values(&info.name)?.is_some()
            || reader.get_sorted_doc_values(&info.name)?.is_some()
        },
        DocValuesType::SortedSet => {
          reader.get_binary_doc_values(&info.name)?.is_some()
            || reader.get_numeric_doc_values(&info.name)?.is_some()
            || reader.get_sorted_numeric_doc_values(&info.name)?.is_some()
            || reader.get_sorted_doc_values(&info.name)?.is_some()
        },
        DocValuesType::Binary => {
          reader.get_numeric_doc_values(&info.name)?.is_some()
            || reader.get_sorted_doc_values(&info.name)?.is_some()
            || reader.get_sorted_numeric_doc_values(&info.name)?.is_some()
            || reader.get_sorted_set_doc_values(&info.name)?.is_some()
        },
        DocValuesType::Numeric => {
          reader.get_binary_doc_values(&info.name)?.is_some()
            || reader.get_sorted_doc_values(&info.name)?.is_some()
            || reader.get_sorted_numeric_doc_values(&info.name)?.is_some()
            || reader.get_sorted_set_doc_values(&info.name)?.is_some()
        },
      };

      if has_multiple_types {
        let message = if *info.get_doc_values_type() == DocValuesType::None {
          format!("field: {} has docvalues but should omit them!", info.name)
        } else {
          format!("{} returns multiple docvalues types!", info.name)
        };
        return Err(LuceneError::illegal_state(message));
      }
    }
    Ok(())
  }

  pub fn random_htmlish_string<R>(random: &mut R, num_elements: usize) -> String
  where
    R: Rng + ?Sized,
  {
    use std::fmt::Write;
    let end = random.random_range(0..=num_elements);
    if end == 0 {
      return String::new();
    }

    let mut sb = String::new();

    for _ in 0..end {
      match random.random_range(0..25) {
        0 => sb.push_str("<p>"),
        1 => {
          sb.push('<');
          sb.push_str(&"    "[..random.random_range(0..=4)]);
          sb.push_str(&Self::random_simple_string(random));
          for _ in 0..random.random_range(0..10) {
            sb.push(' ');
            sb.push_str(&Self::random_simple_string(random));
            sb.push_str(&" "[..random.random_range(0..=1)]);
            sb.push('=');
            sb.push_str(&" "[..random.random_range(0..=1)]);
            sb.push_str(&"\""[..random.random_range(0..=1)]);
            sb.push_str(&Self::random_simple_string(random));
            sb.push_str(&"\""[..random.random_range(0..=1)]);
          }
          sb.push_str(&"    "[..random.random_range(0..=4)]);
          if random.random_bool(0.5) {
            sb.push('/');
          }
          if random.random_bool(0.5) {
            sb.push('>');
          }
        },
        2 => {
          sb.push_str("</");
          sb.push_str(&"    "[..random.random_range(0..=4)]);
          sb.push_str(&Self::random_simple_string(random));
          sb.push_str(&"    "[..random.random_range(0..=4)]);
          if random.random_bool(0.5) {
            sb.push('>');
          }
        },
        3 => sb.push('>'),
        4 => sb.push_str("</p>"),
        5 => sb.push_str("<!--"),
        6 => sb.push_str("<!--#"),
        7 => sb.push_str("<script><!-- f('"),
        8 => sb.push_str("</script>"),
        9 => sb.push_str("<?"),
        10 => sb.push_str("?>"),
        11 => sb.push('"'),
        12 => sb.push_str("\\\""),
        13 => sb.push('\''),
        14 => sb.push_str("\\'"),
        15 => sb.push_str("-->"),
        16 => {
          sb.push('&');
          match random.random_range(0..2) {
            0 => sb.push_str(&Self::random_simple_string(random)),
            1 => {
              let entity = HTML_CHAR_ENTITIES.choose(random).unwrap();
              sb.push_str(entity);
            },
            _ => {},
          }
          if random.random_bool(0.5) {
            sb.push(';');
          }
        },
        17 => {
          sb.push_str("&#");
          if random.random_bool(0.5) {
            write!(sb, "{}", random.random::<u32>()).unwrap();
            if random.random_bool(0.5) {
              sb.push(';');
            }
          }
        },
        18 => {
          sb.push_str("&#x");
          if random.random_bool(0.5) {
            write!(sb, "{:x}", random.random::<u32>()).unwrap();
            if random.random_bool(0.5) {
              sb.push(';');
            }
          }
        },
        19 => sb.push(';'),
        20 => {
          write!(sb, "{}", random.random::<u32>()).unwrap();
        },
        21 => sb.push('\n'),
        22 => sb.push_str(&"          "[..random.random_range(0..=10)]),
        23 => {
          sb.push('<');
          if random.random_ratio(1, 3) {
            sb.push_str(&"          "[..random.random_range(1..=10)]);
          }
          if random.random_bool(0.5) {
            sb.push('/');
            if random.random_ratio(1, 3) {
              sb.push_str(&"          "[..random.random_range(1..=10)]);
            }
          }
          let tag = match random.random_range(0..3) {
            0 => Self::randomly_recase_codepoints(random, "script"),
            1 => Self::randomly_recase_codepoints(random, "style"),
            _ => Self::randomly_recase_codepoints(random, "br"),
          };
          sb.push_str(&tag);
          if random.random_bool(0.5) {
            sb.push('>');
          }
        },
        _ => {
          sb.push_str(&Self::random_simple_string(random));
        },
      }
    }
    sb
  }
  pub fn ram_copy_of<R, D>(
    _random: &mut R,
    dir: &D,
  ) -> Result<Arc<ByteBuffersDirectory<SingleInstanceLockFactory>>>
  where
    D: Directory,
    R: Rng + ?Sized,
  {
    let ram = Arc::new(ByteBuffersDirectory::new());
    for file in dir.list_all()? {
      if file.starts_with(IndexFileNames::SEGMENTS) || CODEC_FILE_PATTERN.is_match(&file) {
        ram.copy_from(
          dir,
          &file,
          &file,
          IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?,
        )?;
      }
    }
    Ok(ram)
  }
  pub fn randomly_recase_codepoints<R>(random: &mut R, s: &str) -> String
  where
    R: Rng + ?Sized,
  {
    let mut result = String::with_capacity(s.len());

    for ch in s.chars() {
      match random.random_range(0..=2) {
        0 => result.extend(ch.to_uppercase()),
        1 => result.extend(ch.to_lowercase()),
        _ => result.push(ch), // leave intact
      }
    }
    result
  }

  pub fn random_realistic_unicode_string<R>(random: &mut R) -> String
  where
    R: Rng + ?Sized,
  {
    Self::random_realistic_unicode_string_with_len(random, 20)
  }

  pub fn random_realistic_unicode_string_with_len<R>(random: &mut R, max_length: usize) -> String
  where
    R: Rng + ?Sized,
  {
    Self::random_realistic_unicode_string_range(random, 0, max_length)
  }

  pub fn random_realistic_unicode_string_range<R>(
    rng: &mut R,
    min_length: usize,
    max_length: usize,
  ) -> String
  where
    R: Rng + ?Sized,
  {
    let end = rng.random_range(min_length..=max_length);

    // Choose a random Unicode block
    let block = rng.random_range(0..BLOCK_STARTS.len());
    let block_start = BLOCK_STARTS[block];
    let block_end = BLOCK_ENDS[block];

    // Generate random codepoints within the selected block
    let mut result = String::new();
    for _ in 0..end {
      let codepoint = rng.random_range(block_start..=block_end);
      if let Some(c) = char::from_u32(codepoint) {
        result.push(c);
      }
    }
    result
  }
  /// Returns random string, including full unicode range
  pub fn random_unicode_string<R>(random: &mut R) -> String
  where
    R: Rng + ?Sized,
  {
    Self::random_unicode_string_with_len(random, 20)
  }
  /// Returns a random string up to a certain length.
  pub fn random_unicode_string_with_len<R>(random: &mut R, max_length: usize) -> String
  where
    R: Rng + ?Sized,
  {
    let end = random.random_range(0..=max_length);
    if end == 0 {
      return "".to_string();
    }

    stack_or_heap_buffer!(buffer, u16, end, 20, 0);
    Self::random_fixed_length_unicode_string(random, buffer, 0, end);
    String::from_utf16_lossy(buffer)
  }
  pub fn random_fixed_length_unicode_string<R>(
    random: &mut R,
    chars: &mut [u16],
    offset: usize,
    length: usize,
  ) where
    R: Rng + ?Sized,
  {
    assert_eq!(offset, 0);
    let mut i = offset;
    let end = offset + length;
    while i < end {
      let t = random.random_range(0..5);
      if t == 0 && i < length - 1 {
        chars[i] = random.random_range(0xd800..=0xdbff);
        chars[i + 1] = random.random_range(0xdc00..=0xdfff);
        i += 2;
      } else if t <= 1 {
        chars[i] = random.random_range(0x00..=0x7f);
        i += 1;
      } else if t == 2 {
        chars[i] = random.random_range(0x80..=0x7ff);
        i += 1;
      } else if t == 3 {
        chars[i] = random.random_range(0x800..=0xd7ff);
        i += 1;
      } else if t == 4 {
        chars[i] = random.random_range(0xe000..=0xffff);
        i += 1;
      }
    }
  }

  /// Returns random string, with a given UTF-8 byte length.
  pub fn random_fixed_byte_length_unicode_string<R>(random: &mut R, length: usize) -> String
  where
    R: Rng + ?Sized,
  {
    let mut bytes = length;
    let mut result = String::new();
    while bytes != 0 {
      let t = if bytes >= 4 {
        random.random_range(0..5)
      } else if bytes == 3 {
        random.random_range(0..4)
      } else if bytes == 2 {
        random.random_range(0..2)
      } else {
        0
      };

      if t == 0 {
        result.push(char::from_u32(random.random_range(0..0x80)).unwrap());
        bytes -= 1;
      } else if t == 1 {
        result.push(char::from_u32(random.random_range(0x80..=0x7ff)).unwrap());
        bytes -= 2;
      } else if t == 2 {
        result.push(char::from_u32(random.random_range(0x800..=0xd7ff)).unwrap());
        bytes -= 3;
      } else if t == 3 {
        result.push(char::from_u32(random.random_range(0xe000..=0xffff)).unwrap());
        bytes -= 3;
      } else if t == 4 {
        let high = random.random_range(0xd800..=0xdbff);
        let low = random.random_range(0xdc00..=0xdfff);
        let codepoint = 0x10000 + ((high - 0xd800) << 10) + (low - 0xdc00);
        result.push(char::from_u32(codepoint).unwrap());
        bytes -= 4;
      }
    }
    result
  }

  /// Returns a string that's "regexpish" — it contains many characters
  /// typically found in regular expressions. If you call this enough
  /// times, you might get a valid regex!
  #[allow(unused)]
  fn random_regexpish_string<R>(random: &mut R) -> String
  where
    R: Rng + ?Sized,
  {
    Self::random_regexpish_string_with_len(random, 20)
  }
  const MAX_RECURSION_BOUND: usize = 5;

  /// Returns a string that's "regexpish" — it contains many characters
  /// typically found in regular expressions.
  ///
  /// If you call this enough times, you might get a valid regex!
  ///
  /// Note:
  /// To avoid practically endless backtracking patterns, this replaces `*`
  /// and `+` operators with bounded repetitions.  
  /// See LUCENE-4111 for more information.
  ///
  /// Parameters:
  /// - `max_length`: A hint for the maximum length of the regexpish string.
  ///   The result may exceed it slightly.
  fn random_regexpish_string_with_len<R>(random: &mut R, max_len: usize) -> String
  where
    R: Rng + ?Sized,
  {
    let count = random.random_range(0..=max_len);
    let mut s = String::with_capacity(count);

    for _ in 0..count {
      if random.random_bool(0.5) {
        // a-z
        s.push((random.random_range(b'a'..=b'z')) as char);
      } else {
        // pick from ops
        s.push_str(OPS.choose(random).unwrap());
      }
    }
    s
  }

  /// Returns a random binary term.
  pub fn random_binary_term<AV, R>(rng: &mut R) -> BytesRef<AV>
  where
    AV: SharedAccessVec<u8>,
    R: Rng + ?Sized,
  {
    let len = rng.random_range(0..15);
    Self::random_binary_term_with_len(rng, len)
  }

  ///  Returns a random binary with a given length
  pub fn random_binary_term_with_len<AV, R>(random: &mut R, length: usize) -> BytesRef<AV>
  where
    AV: SharedAccessVec<u8>,
    R: Rng + ?Sized,
  {
    let mut bytes = vec![0u8; length];
    random.fill(&mut bytes[..]);
    let v = AV::from_vec(bytes);
    let mut b = BytesRef::from_bytes(v);
    b.length = length;
    b
  }

  pub fn random_analysis_string<R>(random: &mut R, max_length: usize, simple: bool) -> String
  where
    R: Rng + ?Sized,
  {
    if max_length == 0 {
      return String::new();
    }

    // sometimes just a purely random string
    if random.random_range(0..31) == 0 {
      let word_len = random.random_range(0..max_length);
      return Self::random_substring(random, word_len, simple);
    }

    // otherwise, try to make it more realistic with 'words' since most tests use MockTokenizer
    // first decide how big the string will really be: 0..n
    let max_length = random.random_range(0..max_length);
    let avg_word_length = TestUtil::next_int(random, 3, 8);
    let mut sb = String::new();
    while sb.len() < max_length {
      if !sb.is_empty() {
        sb.push(' ');
      }
      let mut word_length = -1;
      while word_length < 0 {
        let u1 = loop {
          let v = random.random::<f64>();
          if v > 0.0 {
            break v;
          }
        };
        let u2 = random.random::<f64>();
        let gaussian = (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos();
        word_length = (gaussian * 3.0 + avg_word_length as f64) as i32;
      }
      let word_length = std::cmp::min(word_length as usize, max_length - sb.len());
      sb.push_str(&Self::random_substring(random, word_length, simple));
    }
    sb
  }

  pub fn random_substring<R>(random: &mut R, word_len: usize, simple: bool) -> String
  where
    R: Rng + ?Sized,
  {
    if word_len == 0 {
      return String::new();
    }

    let evilness = TestUtil::next_int(random, 0, 20);

    let mut sb = String::new();
    while sb.chars().count() < word_len {
      if simple {
        if random.random_bool(0.5) {
          sb.push_str(&Self::random_simple_string_with_len(random, word_len));
        } else {
          sb.push_str(&Self::random_htmlish_string(random, word_len));
        }
      } else {
        match evilness {
          0..=9 => sb.push_str(&Self::random_simple_string_with_len(random, word_len)),
          10..=14 => {
            assert!(sb.is_empty());
            sb.push_str(&Self::random_realistic_unicode_string_range(
              random, word_len, word_len,
            ));
          },
          16 => sb.push_str(&Self::random_htmlish_string(random, word_len)),
          17 => sb.push_str(&Self::random_regexpish_string_with_len(random, word_len)),
          _ => sb.push_str(&Self::random_unicode_string_with_len(random, word_len)),
        }
      }
    }

    // truncate to exact length (UTF-16 safe, remove trailing high surrogate if
    // needed)
    let end = sb
      .char_indices()
      .nth(word_len)
      .map_or(sb.len(), |(offset, _)| offset);
    sb.truncate(end);
    let mut s = sb;
    if s
      .encode_utf16()
      .last()
      .is_some_and(|x| (0xD800..=0xDBFF).contains(&x))
    {
      s = s.chars().take(word_len - 1).collect();
    }

    if random.random_range(0..17) == 0 {
      Self::randomly_recase_codepoints(random, &s)
    } else {
      s
    }
  }
  pub fn docs_with_reader<R, IR>(
    random: &mut R,
    r: IR,
    field: &str,
    term: &BytesRef<Vec<u8>>,
    reuse: Option<<<TermsType<IR> as Terms>::TermsEnum as TermsEnum>::PostingsEnum>,
    flags: i32,
  ) -> Result<Option<<<TermsType<IR> as Terms>::TermsEnum as TermsEnum>::PostingsEnum>>
  where
    R: Rng + ?Sized,
    IR: IndexReader,
  {
    match get_terms(r, field)? {
      Some(terms) => {
        let mut terms_enum = terms.iterator()?;
        if !terms_enum.seek_exact(term)? {
          return Ok(None);
        }
        let v = Self::docs(random, &mut terms_enum, reuse, flags)?;
        Ok(Some(v))
      },
      None => Ok(None),
    }
  }

  pub fn docs<TE, R>(
    random: &mut R,
    terms_enum: &mut TE,
    reuse: Option<TE::PostingsEnum>,
    mut flags: i32,
  ) -> Result<TE::PostingsEnum>
  where
    TE: TermsEnum,
    R: Rng + ?Sized,
  {
    if random.random_bool(0.5) {
      if random.random_bool(0.5) {
        let pos_flags = match random.random_range(0..4) {
          0 => POSITIONS,
          1 => OFFSETS,
          2 => PAYLOADS,
          _ => ALL,
        };
        return terms_enum.postings_with_flags(None, pos_flags as i32);
      }

      flags |= FREQS as i32;
    }

    terms_enum.postings_with_flags(reuse, flags)
  }
}
static OPS: LazyLock<[Cow<'static, str>; 10]> = LazyLock::new(|| {
  [
    Cow::Borrowed("."),
    Cow::Borrowed("?"),
    Cow::Owned(format!("{{0,{}}}", TestUtil::MAX_RECURSION_BOUND)), // replaces '*'
    Cow::Owned(format!("{{1,{}}}", TestUtil::MAX_RECURSION_BOUND)), // replaces '+'
    Cow::Borrowed("("),
    Cow::Borrowed(")"),
    Cow::Borrowed("-"),
    Cow::Borrowed("["),
    Cow::Borrowed("]"),
    Cow::Borrowed("|"),
  ]
});
static HTML_CHAR_ENTITIES: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
  vec![
    "AElig", "Aacute", "Acirc", "Agrave", "Alpha", "AMP", "Aring", "Atilde", "Auml", "Beta",
    "COPY", "Ccedil", "Chi", "Dagger", "Delta", "ETH", "Eacute", "Ecirc", "Egrave", "Epsilon",
    "Eta", "Euml", "Gamma", "GT", "Iacute", "Icirc", "Igrave", "Iota", "Iuml", "Kappa", "Lambda",
    "LT", "Mu", "Ntilde", "Nu", "OElig", "Oacute", "Ocirc", "Ograve", "Omega", "Omicron", "Oslash",
    "Otilde", "Ouml", "Phi", "Pi", "Prime", "Psi", "QUOT", "REG", "Rho", "Scaron", "Sigma",
    "THORN", "Tau", "Theta", "Uacute", "Ucirc", "Ugrave", "Upsilon", "Uuml", "Xi", "Yacute",
    "Yuml", "Zeta", "aacute", "acirc", "acute", "aelig", "agrave", "alefsym", "alpha", "amp",
    "and", "ang", "apos", "aring", "asymp", "atilde", "auml", "bdquo", "beta", "brvbar", "bull",
    "cap", "ccedil", "cedil", "cent", "chi", "circ", "clubs", "cong", "copy", "crarr", "cup",
    "curren", "dArr", "dagger", "darr", "deg", "delta", "diams", "divide", "eacute", "ecirc",
    "egrave", "empty", "emsp", "ensp", "epsilon", "equiv", "eta", "eth", "euml", "euro", "exist",
    "fnof", "forall", "frac12", "frac14", "frac34", "frasl", "gamma", "ge", "gt", "hArr", "harr",
    "hearts", "hellip", "iacute", "icirc", "iexcl", "igrave", "image", "infin", "int", "iota",
    "iquest", "isin", "iuml", "kappa", "lArr", "lambda", "lang", "laquo", "larr", "lceil", "ldquo",
    "le", "lfloor", "lowast", "loz", "lrm", "lsaquo", "lsquo", "lt", "macr", "mdash", "micro",
    "middot", "minus", "mu", "nabla", "nbsp", "ndash", "ne", "ni", "not", "notin", "nsub",
    "ntilde", "nu", "oacute", "ocirc", "oelig", "ograve", "oline", "omega", "omicron", "oplus",
    "or", "ordf", "ordm", "oslash", "otilde", "otimes", "ouml", "para", "part", "permil", "perp",
    "phi", "pi", "piv", "plusmn", "pound", "prime", "prod", "prop", "psi", "quot", "rArr", "radic",
    "rang", "raquo", "rarr", "rceil", "rdquo", "real", "reg", "rfloor", "rho", "rlm", "rsaquo",
    "rsquo", "sbquo", "scaron", "sdot", "sect", "shy", "sigma", "sigmaf", "sim", "spades", "sub",
    "sube", "sum", "sup", "sup1", "sup2", "sup3", "supe", "szlig", "tau", "there4", "theta",
    "thetasym", "thinsp", "thorn", "tilde", "times", "trade", "uArr", "uacute", "uarr", "ucirc",
    "ugrave", "uml", "upsih", "upsilon", "uuml", "weierp", "xi", "yacute", "yen", "yuml", "zeta",
    "zwj", "zwnj",
  ]
});
