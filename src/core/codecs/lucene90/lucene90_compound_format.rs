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
use crate::core::codecs::CodecUtil;
use crate::core::codecs::compound_format::CompoundFormat;
use crate::core::codecs::lucene90_compound_reader::Lucene90CompoundReader;
use crate::core::index::IndexFileNames;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::store::directory::Directory;
use crate::core::store::{IOContext, IndexInput, IndexOutput};
use crate::core::util::bit_util::BitUtil;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::Result;
use crate::core::util::priority_queue::{Compare, PriorityQueue};
use crate::core::util::{IOUtils, TryIntoInt};

/// Lucene 9.0 compound file format
///
/// # Files:
///
/// - `.cfs`: An optional "virtual" file consisting of all the other index files
///   for systems that frequently run out of file handles.
/// - `.cfe`: The "virtual" compound file's entry table holding all entries in
///   the corresponding `.cfs` file.
///
/// # Description:
///
/// - Compound (.cfs) --> Header, FileData<sup>FileCount</sup>, Footer
/// - Compound Entry Table (.cfe) --> Header, FileCount, <FileName, DataOffset,
///   DataLength> <sup>FileCount</sup>
/// - Header -->
///   [`CodecUtil::write_index_header`](CodecUtil::write_index_header)
/// - FileCount -->
///   [`DataOutput::write_vint`](crate::core::store::data_output::DataOutput::write_vint)
/// - DataOffset, DataLength, Checksum -->
///   [`DataOutput::write_long`](crate::core::store::data_output::DataOutput::write_long)
/// - FileName -->
///   [`DataOutput::write_string`](crate::core::store::data_output::DataOutput::write_string)
/// - FileData --> Raw file data
/// - Footer -->
///   [`CodecUtil::write_footer`](CodecUtil::write_footer)
///
/// # Notes:
///
/// - `FileCount` indicates how many files are contained in this compound file.
///   The entry table that follows has that many entries.
/// - Each directory entry contains a long pointer to the start of this file's
///   data section, the file's length, and a String with that file's name. The
///   start of the file's data section is aligned to 8 bytes to avoid additional
///   unaligned accesses with `mmap`.
pub struct Lucene90CompoundFormat;

impl Default for Lucene90CompoundFormat {
  fn default() -> Self {
    Self::new()
  }
}
impl Lucene90CompoundFormat {
  /// Extension of compound file.
  pub const DATA_EXTENSION: &'static str = "cfs";
  /// Extension of compound file entries.
  pub const ENTRIES_EXTENSION: &'static str = "cfe";
  pub const DATA_CODEC: &'static str = "Lucene90CompoundData";
  pub const ENTRY_CODEC: &'static str = "Lucene90CompoundEntries";
  pub const VERSION_START: i32 = 0;
  pub const VERSION_CURRENT: i32 = Self::VERSION_START;
  pub fn new() -> Lucene90CompoundFormat {
    Lucene90CompoundFormat {}
  }
  pub fn write_compound_file<D>(
    &self,
    entries: &mut impl IndexOutput,
    data: &mut impl IndexOutput,
    directory: &impl Directory,
    si: &SegmentInfo<D>,
  ) -> Result<()> {
    let mut pq;
    {
      // write number of files
      let files = si.files()?;
      let num_files = files.len();
      debug_assert!(num_files <= i32::MAX as usize);
      entries.write_vint(num_files as i32)?;
      pq = PriorityQueue::new(num_files, SizedFileQueueCmp)?;
      {
        for filename in files {
          let file_length = directory.file_length(filename)?;
          pq.add(SizedFile::new(
            filename.to_string(),
            file_length.try_convert()?,
          ))?;
        }
      }
    }
    while pq.size() > 0 {
      let sized_file = pq.pop_unchecked()?;
      let file = &sized_file.name;
      let start_offset = data.align_file_pointer(BitUtil::LONG_BYTES)?;
      let mut file_input = directory.open_checksum_input(file)?;
      let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        // just copies the index header, verifying that its id matches what
        // we expect
        CodecUtil::verify_and_copy_index_header(&mut file_input, data, si.get_id())?;
        // copy all bytes except the footer
        let num_bytes_to_copy =
          file_input.length()? - CodecUtil::footer_length() - file_input.get_file_pointer()?;
        data.copy_bytes(&mut file_input, num_bytes_to_copy)?;
        // verify footer (checksum) matches for the incoming file we are
        // copying
        let checksum = CodecUtil::check_footer(&mut file_input)?;
        // this is poached from CodecUtil.writeFooter, but we need to use
        // our own checksum, not data.getChecksum(), but I think
        // adding a public method to CodecUtil to do that is somewhat
        // dangerous:
        CodecUtil::write_be_int(data, CodecUtil::FOOTER_MAGIC)?;
        CodecUtil::write_be_int(data, 0)?;
        CodecUtil::write_be_long(data, checksum)
      }));
      let close_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| file_input.close()));
      IOUtils::use_or_suppress_caught_result(result, close_result)?;
      let end_offset = data.get_file_pointer()?;
      let length = end_offset - start_offset;
      // write entry for file
      entries.write_string(IndexFileNames::strip_segment_name(file))?;
      entries.write_long(start_offset as i64)?;
      entries.write_long(length as i64)?;
    }
    Ok(())
  }
}

impl CompoundFormat for Lucene90CompoundFormat {
  type Directory<D>
    = Lucene90CompoundReader<D::IndexInput>
  where
    D: Directory;

  fn get_compound_reader<D>(&self, dir: &D, si: &SegmentInfo<D>) -> Result<Self::Directory<D>>
  where
    D: Directory,
  {
    Lucene90CompoundReader::new(dir, si)
  }

  fn write<D>(&self, dir: &impl Directory, si: &SegmentInfo<D>, context: &IOContext) -> Result<()> {
    let data_file =
      IndexFileNames::segment_file_name(&si.name, "", Lucene90CompoundFormat::DATA_EXTENSION);
    let entries_file =
      IndexFileNames::segment_file_name(&si.name, "", Lucene90CompoundFormat::ENTRIES_EXTENSION);
    let mut data = dir.create_output(&data_file, context)?;
    let mut entries = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      dir.create_output(&entries_file, context)
    })) {
      Ok(Ok(entries_output)) => entries_output,
      entries_result => {
        let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| data.close()));
        return IOUtils::use_or_suppress_caught_result(entries_result, close_result).map(|_| ());
      },
    };

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      CodecUtil::write_index_header(
        &mut data,
        Lucene90CompoundFormat::DATA_CODEC,
        Lucene90CompoundFormat::VERSION_CURRENT,
        si.get_id(),
        "",
      )?;
      CodecUtil::write_index_header(
        &mut entries,
        Lucene90CompoundFormat::ENTRY_CODEC,
        Lucene90CompoundFormat::VERSION_CURRENT,
        si.get_id(),
        "",
      )?;
      self.write_compound_file(&mut entries, &mut data, dir, si)?;
      CodecUtil::write_footer(&mut data)?;
      CodecUtil::write_footer(&mut entries)
    }));
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      IOUtils::close([&mut entries, &mut data], Closeable::close)
    }));
    IOUtils::use_or_suppress_caught_result(result, close_result)
  }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SizedFile {
  pub name: String,
  pub length: i64,
}

impl SizedFile {
  /// Creates a new `SizedFile` instance.
  pub fn new(name: String, length: i64) -> Self {
    SizedFile { name, length }
  }
}

pub struct SizedFileQueueCmp;
impl Compare<SizedFile> for SizedFileQueueCmp {
  fn less_than(&self, sf1: &SizedFile, sf2: &SizedFile) -> Result<bool> {
    Ok(sf1.length < sf2.length)
  }
}
