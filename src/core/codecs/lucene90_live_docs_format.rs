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

use crate::core::index::index_file_names::base36;

use crate::core::codecs::CodecUtil;
use crate::core::codecs::live_docs_format::LiveDocsFormat;
use crate::core::index::IndexFileNames;
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::store::directory::Directory;
use crate::core::store::{IOContext, IndexInput, IndexOutput};
use crate::core::util::TryIntoInt;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bits::Bits;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::{FixedBit, FixedBitSet};
use crate::core::util::io_utils::IOUtils;
#[cfg(test)]
use crate::test_framework::core::util::failure_context::{
  ExecutionMethod, ExecutionOwner, ExecutionScope,
};

/// Lucene 9.0 live docs format
///
/// The `.liv` file is optional, and only exists when a segment contains
/// deletions.
///
/// Although per-segment, this file is maintained exterior to compound segment
/// files.
///
/// Deletions (`.liv`) -> `IndexHeader`, `Generation`, [`Bits`]
///
/// - `SegmentHeader` ->
///   [`CodecUtil::write_index_header`](CodecUtil::write_index_header)
/// - [`Bits`] -> <[`Int64`](crate::core::store::data_output::DataOutput::write_long)>
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
  fn read_fixed_bit_set<II>(input: &mut II, length: usize) -> Result<FixedBitSet>
  where
    II: IndexInput,
  {
    let num_words = FixedBitSet::bits2words(length);
    let mut data = vec![0i64; num_words];
    input.read_longs(&mut data, 0, num_words)?;
    FixedBitSet::with_capacity(data, length)
  }
  fn write_bits<IO, T>(output: &mut IO, bits: &T) -> Result<i32>
  where
    IO: IndexOutput,
    T: Bits,
  {
    if let Some(fixed) = bits.fixed_bit_set()
      && fixed.length() <= i32::MAX as usize
    {
      let length = fixed.length();
      let (full_words, trailing_bits) = (length / 64, length % 64);
      let words = fixed.get_bits();
      let mut live_count = 0;
      for &word in &words[..full_words] {
        live_count += word.count_ones() as usize;
        output.write_long(word)?;
      }
      if trailing_bits != 0 {
        let word = words[full_words] & ((1u64 << trailing_bits) - 1) as i64;
        live_count += word.count_ones() as usize;
        output.write_long(word)?;
      }
      return Ok((length - live_count) as i32);
    }

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

  fn read_live_docs<D, D2>(
    &self,
    directory: &D2,
    info: &SegmentCommitInfo<D>,
    _context: &IOContext,
  ) -> Result<Self::Bits>
  where
    D2: Directory,
  {
    #[cfg(test)]
    let _execution_scope = ExecutionScope::enter(
      ExecutionOwner::Lucene90LiveDocsFormat,
      ExecutionMethod::ReadLiveDocs,
    );
    let generation = info.get_del_gen();
    let length = info.info.max_doc()?.try_convert()?;
    if generation == -1 {
      return Err(LuceneError::illegal_state("live docs file name is missing"));
    }
    let mut digits = [0; 14];
    let suffix = base36(generation, &mut digits);
    let file_suffix = if generation == 0 { "" } else { suffix };
    let mut input = IndexFileNames::with_segment_file_name(
      &info.info.name,
      file_suffix,
      Lucene90LiveDocsFormat::EXTENSION,
      |name| directory.open_checksum_input(name),
    )?;
    let mut bits = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      let prior_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
          CodecUtil::check_index_header(
            &mut input,
            Lucene90LiveDocsFormat::CODEC_NAME,
            Lucene90LiveDocsFormat::VERSION_START,
            Lucene90LiveDocsFormat::VERSION_CURRENT,
            info.info.get_id(),
            suffix,
          )?;

          let fbs = Self::read_fixed_bit_set(&mut input, length)?;

          if fbs.length() - fbs.cardinality() != info.get_del_count().try_convert()? {
            return Err(LuceneError::corrupt_index(format!(
              "bits.deleted={} info.delcount={}",
              fbs.length() - fbs.cardinality(),
              info.get_del_count()
            )));
          }
          bits = Some(fbs.to_read_only_bits());
          Ok(())
        }));
      let prior_result = match prior_result {
        Ok(Ok(())) => None,
        prior_result => Some(prior_result),
      };
      CodecUtil::check_footer_with_error(&mut input, prior_result)?;
      bits.take().ok_or_else(|| {
        LuceneError::illegal_state("live docs are missing after successful construction")
      })
    }));
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| input.close()));
    IOUtils::use_or_suppress_caught_result(result, close_result)
  }

  fn write_live_docs<D, T, D2>(
    &self,
    bits: &T,
    directory: &D2,
    info: &SegmentCommitInfo<D>,
    new_del_count: i32,
    context: &IOContext,
  ) -> Result<()>
  where
    T: Bits,
    D2: Directory,
  {
    #[cfg(test)]
    let _execution_scope = ExecutionScope::enter(
      ExecutionOwner::Lucene90LiveDocsFormat,
      ExecutionMethod::WriteLiveDocs,
    );
    let generation = info.get_next_del_gen();
    if generation == -1 {
      return Err(LuceneError::illegal_state("live docs file name is missing"));
    }
    let mut digits = [0; 14];
    let suffix = base36(generation, &mut digits);
    let file_suffix = if generation == 0 { "" } else { suffix };
    let del_count: i32;
    {
      let mut output = IndexFileNames::with_segment_file_name(
        &info.info.name,
        file_suffix,
        Lucene90LiveDocsFormat::EXTENSION,
        |name| directory.create_output(name, context),
      )?;
      let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<i32> {
        CodecUtil::write_index_header(
          &mut output,
          Lucene90LiveDocsFormat::CODEC_NAME,
          Lucene90LiveDocsFormat::VERSION_CURRENT,
          info.info.get_id(),
          suffix,
        )?;

        let del_count = Self::write_bits(&mut output, bits)?;

        CodecUtil::write_footer(&mut output)?;
        Ok(del_count)
      }));
      let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| output.close()));
      del_count = IOUtils::use_or_suppress_caught_result(result, close_result)?;
    }

    if del_count != info.get_del_count().wrapping_add(new_del_count) {
      return Err(LuceneError::corrupt_index(format!(
        "bits.deleted={} info.delcount={} newdelcount={}",
        del_count,
        info.get_del_count(),
        new_del_count
      )));
    }

    Ok(())
  }

  fn files<D>(&self, info: &SegmentCommitInfo<D>, files: &mut HashSet<String>) -> Result<()> {
    if info.has_deletions() {
      let file_name = IndexFileNames::file_name_from_generation(
        &info.info.name,
        Lucene90LiveDocsFormat::EXTENSION,
        info.get_del_gen(),
      );
      files.insert(
        file_name.ok_or_else(|| LuceneError::illegal_state("live docs file name is missing"))?,
      );
    }
    Ok(())
  }
}
