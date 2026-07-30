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
use std::sync::Arc;

use crate::core::codecs::CodecUtil;
use crate::core::codecs::segment_info_format::SegmentInfoFormat;
use crate::core::index::IndexFileNames;
use crate::core::index::index_sorter::IndexSorter;
use crate::core::index::segment_info::{NO, SegmentInfo, YES};
use crate::core::index::sort_field_provider::{SortFieldProvider, for_name, write};
use crate::core::search::sort::Sort;
use crate::core::search::sort_field::SortFiledBase;
use crate::core::store::directory::Directory;
use crate::core::store::{DataInput, DataOutput, IOContext};
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::{IOUtils, StringHelper, Version};

/// Lucene 9.9 Segment info format.
///
/// # Files
///
/// - `.si`: Header, SegVersion, SegSize, IsCompoundFile, Diagnostics, Files,
///   Attributes, IndexSort, Footer
///
/// # Data Types
///
/// - **Header** -->
///   [`CodecUtil::write_index_header`](CodecUtil::write_index_header)
/// - **SegSize** --> [`DataOutput::write_int`] (Int32)
/// - **SegVersion** --> [`DataOutput::write_string`] (String)
/// - **SegMinVersion** --> [`DataOutput::write_string`] (String)
/// - **Files** --> [`DataOutput::write_set_of_strings`] (Set\<String>)
/// - **Diagnostics**, **Attributes** --> [`DataOutput::write_map_of_strings`]
///   (Map<String, String>)
/// - **IsCompoundFile** --> [`DataOutput::write_byte`] (Int8)
/// - **HasBlocks** --> [`DataOutput::write_byte`] (Int8)
/// - **IndexSort** --> [`DataOutput::write_vint`] (Int32) count, followed by
///   `count` SortField
/// - **SortField** --> [`DataOutput::write_string`] (String) sort struct,
///   followed by a per-sort byte stream (see
///   [`SortFieldProvider::read_sort_field`])
/// - **Footer** --> [`CodecUtil::write_footer`](CodecUtil::write_footer)
///
/// # Field Descriptions
///
/// - **SegVersion**: The code version that created the segment.
/// - **SegMinVersion**: The minimum code version that contributed documents to
///   the segment.
/// - **SegSize**: The number of documents contained in the segment index.
/// - **IsCompoundFile**: Records whether the segment is written as a compound
///   file or not. If this is `-1`, the segment is not a compound file. If it is
///   `1`, the segment is a compound file.
/// - **HasBlocks**: Records whether the segment contains documents written as a
///   block and guarantees consecutive document IDs for all documents in the
///   block.
/// - **Diagnostics Map**: Privately written by
///   [`IndexWriter`](crate::core::index::index_writer::IndexWriter), as debugging
///   aid, for each segment it creates. It includes metadata like the current
///   Lucene version, OS,  why the segment was created (merge,
///   flush, addIndexes), etc.
/// - **Files**: A list of files referred to by this segment.
///
/// # See Also
/// - [`SegmentInfos`](crate::core::index::segment_infos::SegmentInfos)
///
/// # Lucene Experimental
/// This API is experimental and may change in future versions.
pub struct Lucene99SegmentInfoFormat;

const SI_EXTENSION: &str = "si";

impl Lucene99SegmentInfoFormat {
  const CODEC_NAME: &'static str = "Lucene90SegmentInfo";
  const VERSION_START: i32 = 0;
  const VERSION_CURRENT: i32 = Lucene99SegmentInfoFormat::VERSION_START;
  fn parse_segment_info<D>(
    dir: Arc<D>,
    input: &mut impl DataInput,
    segment: &str,
    segment_id: &[u8; StringHelper::ID_LENGTH],
  ) -> Result<SegmentInfo<D>>
  where
    D: Directory,
  {
    let major = input.read_int()?;
    debug_assert!(major >= 0);
    let minor = input.read_int()?;
    debug_assert!(minor >= 0);
    let bug_fix = input.read_int()?;
    debug_assert!(bug_fix >= 0);
    let version = Version::from_bits(major, minor, bug_fix)?;

    let has_min_version = input.read_byte()?;
    let min_version = match has_min_version {
      0 => None,
      1 => {
        let major = input.read_int()?;
        debug_assert!(major >= 0);
        let minor = input.read_int()?;
        debug_assert!(minor >= 0);
        let bug_fix = input.read_int()?;
        debug_assert!(bug_fix >= 0);
        Some(Version::from_bits(major, minor, bug_fix)?)
      },
      _ => {
        return Err(LuceneError::corrupt_index(format!(
          "Illegal boolean value : {has_min_version} (resource={input})"
        )));
      },
    };

    let doc_count = input.read_int()?;
    if doc_count < 0 {
      return Err(LuceneError::corrupt_index(format!(
        "Invalid docCount: {doc_count} (resource={input})"
      )));
    }
    let is_compound_file = input.read_byte()? == YES as u8;
    let has_blocks = input.read_byte()? == YES as u8;
    let diagnostics = input.read_map_of_strings()?;
    let files = input.read_set_of_strings()?;
    let attributes = input.read_map_of_strings()?;
    let num_sort_fields = input.read_vint()?;
    let index_sort = match num_sort_fields.cmp(&0) {
      std::cmp::Ordering::Greater => {
        let mut sort_fields = Vec::with_capacity(num_sort_fields as usize);
        for _ in 0..num_sort_fields {
          let name = input.read_string()?;
          let sort_field = for_name(&name).read_sort_field(input)?;
          sort_fields.push(sort_field);
        }
        Some(Arc::new(Sort::with_fields(sort_fields)?))
      },
      std::cmp::Ordering::Less => {
        return Err(LuceneError::corrupt_index(format!(
          "invalid index sort field count: {num_sort_fields} (resource={input})"
        )));
      },
      std::cmp::Ordering::Equal => None,
    };

    let mut si = SegmentInfo::new(
      dir,
      Option::from(version),
      min_version,
      segment,
      doc_count,
      is_compound_file,
      has_blocks,
      None,
      diagnostics,
      *segment_id,
      attributes,
      index_sort,
    )?;
    si.set_files(files)?;
    Ok(si)
  }
  fn write_segment_info<D>(output: &mut impl DataOutput, si: &SegmentInfo<D>) -> Result<()>
  where
    D: Directory,
  {
    let version_wrap = si.get_version_ref();
    debug_assert!(version_wrap.is_some());
    let version = version_wrap.unwrap();
    if version.major < 7 {
      return Err(LuceneError::illegal_argument(format!(
        "invalid major version: should be >= 7 but got: {} segment={}",
        version.major, si
      )));
    }
    output.write_int(version.major)?;
    output.write_int(version.minor)?;
    output.write_int(version.bug_fix)?;

    // Write the min Lucene version that contributed docs to the segment,
    // since 7.0
    if let Some(min_version) = si.get_min_version_ref() {
      output.write_byte(1)?;
      output.write_int(min_version.major)?;
      output.write_int(min_version.minor)?;
      output.write_int(min_version.bug_fix)?;
    } else {
      output.write_byte(0)?;
    }

    debug_assert_eq!(version.prerelease, 0);
    output.write_int(si.max_doc()?)?;

    output.write_byte(if si.get_use_compound_file() {
      YES as u8
    } else {
      NO as u8
    })?;
    output.write_byte(if si.get_has_blocks() {
      YES as u8
    } else {
      NO as u8
    })?;
    output.write_map_of_strings(si.get_diagnostics())?;

    {
      let files = si.files()?;
      for file in files {
        if IndexFileNames::parse_segment_name(file) != si.name {
          return Err(LuceneError::illegal_argument(format!(
            "invalid files: expected segment={}, got file={}",
            si.name, file
          )));
        }
      }
      output.write_set_of_strings(files)?;
    }
    output.write_map_of_strings(si.get_attributes()?)?;

    if let Some(index_sort) = si.get_index_sort() {
      let sort_fields = index_sort.get_sort();
      let num_sort_fields = sort_fields.len();
      output.write_vint(num_sort_fields as i32)?;

      for sort_field in sort_fields {
        if let Some(sorter) = sort_field.get_index_sorter()? {
          output.write_string(sorter.get_provider_name())?;
          write(sort_field, output)?;
        } else {
          return Err(LuceneError::illegal_argument(format!(
            "cannot serialize SortField {sort_field}"
          )));
        }
      }
    } else {
      output.write_vint(0)?;
    }
    Ok(())
  }
}

impl SegmentInfoFormat for Lucene99SegmentInfoFormat {
  fn read<D>(
    &self,
    dir: Arc<D>,
    segment: &str,
    segment_id: &[u8; StringHelper::ID_LENGTH],
    _context: &IOContext,
  ) -> Result<SegmentInfo<D>>
  where
    D: Directory,
  {
    let file_name = IndexFileNames::segment_file_name(segment, "", SI_EXTENSION);
    let mut input = dir.open_checksum_input(&file_name)?;

    let mut footer_attempted = false;
    let mut result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
      || -> Result<SegmentInfo<D>> {
        let result = (|| -> Result<SegmentInfo<D>> {
          CodecUtil::check_index_header(
            &mut input,
            Lucene99SegmentInfoFormat::CODEC_NAME,
            Lucene99SegmentInfoFormat::VERSION_START,
            Lucene99SegmentInfoFormat::VERSION_CURRENT,
            segment_id,
            "",
          )?;
          Self::parse_segment_info(dir.clone(), &mut input, segment, segment_id)
        })();
        footer_attempted = true;
        match result {
          Ok(segment_info) => {
            CodecUtil::check_footer(&mut input)?;
            Ok(segment_info)
          },
          Err(error) => Err(CodecUtil::check_footer_with_error(&mut input, error)),
        }
      },
    ));
    let footer_error = if let Err(payload) = &result
      && !footer_attempted
    {
      let error =
        LuceneError::tragedy_from_panic("panic while reading segment info", payload.as_ref());
      Some(CodecUtil::check_footer_with_error(&mut input, error))
    } else {
      None
    };
    if let Some(error @ LuceneError::CorruptIndex(_)) = footer_error {
      result = Ok(Err(error));
    }
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| input.close()));
    IOUtils::use_or_suppress_caught_result(result, close_result)
  }

  fn write<D>(
    &self,
    dir: &impl Directory,
    si: &mut SegmentInfo<D>,
    io_context: &IOContext,
  ) -> Result<()>
  where
    D: Directory,
  {
    let file_name = IndexFileNames::segment_file_name(&si.name, "", SI_EXTENSION);
    let mut output = dir.create_output(&file_name, io_context)?;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      si.add_file(file_name)?;
      CodecUtil::write_index_header(
        &mut output,
        Lucene99SegmentInfoFormat::CODEC_NAME,
        Lucene99SegmentInfoFormat::VERSION_CURRENT,
        si.get_id(),
        "",
      )?;
      Self::write_segment_info(&mut output, si)?;
      CodecUtil::write_footer(&mut output)
    }));
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| output.close()));
    IOUtils::use_or_suppress_caught_result(result, close_result)
  }
}
