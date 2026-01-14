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
use std::collections::HashSet;

use num_bigint::BigInt;

use crate::core::codecs::CodecUtil;
use crate::core::codecs::live_docs_format::LiveDocsFormat;
use crate::core::index::IndexFileNames;
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::store::directory::Directory;
use crate::core::store::{IOContext, IndexInput, IndexOutput};
use crate::core::util::TryIntoInt;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::{FixedBit, FixedBitSet};

/// Lucene 9.0 live docs format
///
/// The `.liv` file is optional, and only exists when a segment contains
/// deletions.
///
/// Although per-segment, this file is maintained exterior to compound segment
/// files.
///
/// Deletions (`.liv`) -> `IndexHeader`, `Generation`, `Bits`
///
/// - `SegmentHeader` ->
///   [`CodecUtil::write_index_header`](CodecUtil::write_index_header)
/// - `Bits` -> <[`Int64`](crate::core::store::data_output::DataOutput::write_long)>
///   <sup>LongCount</sup>
///
/// [`CodecUtil::write_index_header`](CodecUtil::write_index_header)
/// [`DataOutput::write_long`](crate::core::store::data_output::DataOutput::write_long)
pub struct Lucene90LiveDocsFormat;

impl Default for Lucene90LiveDocsFormat {
    fn default() -> Self {
        Self::new()
    }
}

impl Lucene90LiveDocsFormat {
    /// Extension of live docs
    pub const EXTENSION: &'static str = "liv";

    /// Codec of live docs
    pub const CODEC_NAME: &'static str = "Lucene90LiveDocs";

    /// Supported version range
    pub const VERSION_START: i32 = 0;

    pub const VERSION_CURRENT: i32 = Lucene90LiveDocsFormat::VERSION_START;

    pub fn new() -> Lucene90LiveDocsFormat {
        Lucene90LiveDocsFormat {}
    }
    fn read_fixed_bit_set(input: &mut impl IndexInput, length: usize) -> Result<FixedBitSet> {
        let num_words = FixedBitSet::bits2words(length);
        let mut data = vec![0i64; num_words];
        input.read_longs(&mut data, 0, num_words)?;
        FixedBitSet::with_capacity(data, length)
    }
    fn write_bits(output: &mut impl IndexOutput, bits: &impl Bits) -> Result<i32> {
        let mut del_count = 0;
        let long_count = FixedBitSet::bits2words(bits.length());
        for i in 0..long_count {
            let mut current_bits = 0i64;
            let start = i << 6;
            let end = std::cmp::min(start + 63, bits.length() - 1);

            for j in start..=end {
                if bits.get(j)? {
                    current_bits |= 1i64 << (j % 64);
                } else {
                    del_count += 1;
                }
            }

            output.write_long(current_bits)?;
        }
        Ok(del_count)
    }
}

impl LiveDocsFormat for Lucene90LiveDocsFormat {
    type Bits = FixedBit;

    fn read_live_docs<D>(
        &self,
        directory: &impl Directory,
        info: &SegmentCommitInfo<D>,
        _context: &IOContext,
    ) -> Result<Self::Bits>
    where
        D: Directory,
    {
        let r#gen = info.get_del_gen();
        let name = IndexFileNames::file_name_from_generation(
            &info.info.name,
            Lucene90LiveDocsFormat::EXTENSION,
            r#gen,
        );
        let length = info.info.max_doc()?.try_convert()?;
        debug_assert!(name.is_some());
        let name_str = name.as_ref().unwrap();
        let mut input = directory.open_checksum_input(name_str)?;
        let result = (|| {
            CodecUtil::check_index_header(
                &mut input,
                Lucene90LiveDocsFormat::CODEC_NAME,
                Lucene90LiveDocsFormat::VERSION_START,
                Lucene90LiveDocsFormat::VERSION_CURRENT,
                info.info.get_id(),
                &BigInt::from(r#gen).to_str_radix(36).to_string(),
            )?;

            let fbs = Self::read_fixed_bit_set(&mut input, length)?;

            if fbs.length() - fbs.cardinality() != info.get_del_count().try_convert()? {
                return Err(LuceneError::corrupt_index(format!(
                    "bits.deleted={} info.delcount={}",
                    fbs.length() - fbs.cardinality(),
                    info.get_del_count()
                )));
            }
            Ok(fbs.to_read_only_bits())
        })();
        match result {
            Ok(_) => {
                CodecUtil::check_footer(&mut input)?;
                result
            },
            Err(e) => Err(CodecUtil::check_footer_with_error(&mut input, e)),
        }
    }

    fn write_live_docs<D>(
        &self,
        bits: &impl Bits,
        directory: &impl Directory,
        info: &SegmentCommitInfo<D>,
        new_del_count: i32,
        context: &IOContext,
    ) -> Result<()>
    where
        D: Directory,
    {
        let r#gen = info.get_next_del_gen();
        let name = IndexFileNames::file_name_from_generation(
            &info.info.name,
            Lucene90LiveDocsFormat::EXTENSION,
            r#gen,
        );
        debug_assert!(name.is_some());
        let del_count: i32;
        {
            let mut output = directory.create_output(name.as_ref().unwrap().as_str(), context)?;
            CodecUtil::write_index_header(
                &mut output,
                Lucene90LiveDocsFormat::CODEC_NAME,
                Lucene90LiveDocsFormat::VERSION_CURRENT,
                info.info.get_id(),
                &BigInt::from(r#gen).to_str_radix(36).to_string(),
            )?;

            del_count = Self::write_bits(&mut output, bits)?;

            CodecUtil::write_footer(&mut output)?;
        }

        if del_count != info.get_del_count() + new_del_count {
            return Err(LuceneError::corrupt_index(format!(
                "bits.deleted={} info.delcount={} newdelcount={}",
                del_count,
                info.get_del_count(),
                new_del_count
            )));
        }

        Ok(())
    }

    fn files<D>(&self, info: &SegmentCommitInfo<D>, files: &mut HashSet<String>) -> Result<()>
    where
        D: Directory,
    {
        if info.has_deletions() {
            let file_name = IndexFileNames::file_name_from_generation(
                &info.info.name,
                Lucene90LiveDocsFormat::EXTENSION,
                info.get_del_gen(),
            );
            debug_assert!(file_name.is_some());
            files.insert(file_name.unwrap());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::core::util::error::lucene_error::Result;
    use crate::test::index::base_live_docs_format_test_case::BaseLiveDocsFormatTestCase;
    use crate::test::util::lucene_test_case::lucene_test_case_util::is_night_mode;
    use crate::test::util::lucene_test_case::lucene_test_case_util::random;

    #[allow(dead_code)] // for quick search
    pub struct TestLucene90LiveDocsFormat;
    impl BaseLiveDocsFormatTestCase for TestLucene90LiveDocsFormat {}
    #[test]
    fn test_dense_live_docs() -> Result<()> {
        let mut random = random();
        let test = TestLucene90LiveDocsFormat;
        test.test_dense_live_docs(&mut random)
    }
    #[test]
    fn test_empty_live_docs() -> Result<()> {
        let mut random = random();
        let test = TestLucene90LiveDocsFormat;
        test.test_empty_live_docs(&mut random)
    }
    #[test]
    fn test_sparse_live_docs() -> Result<()> {
        let mut random = random();
        let test = TestLucene90LiveDocsFormat;
        test.test_sparse_live_docs(&mut random)
    }
    #[test]
    fn test_over_flow_live_docs() -> Result<()> {
        let mut random = random();
        let test = TestLucene90LiveDocsFormat;
        if is_night_mode() {
            test.test_over_flow(&mut random)
        } else {
            Ok(())
        }
    }
}
