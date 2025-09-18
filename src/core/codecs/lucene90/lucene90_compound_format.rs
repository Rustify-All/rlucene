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
use crate::core::codecs::compound_directory::CompoundDirectory;
use crate::core::codecs::compound_format::CompoundFormat;
use crate::core::codecs::lucene90_compound_reader::Lucene90CompoundReader;
use crate::core::index::IndexFileNames;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::store::directory::Directory;
use crate::core::store::{IOContext, IndexInput, IndexOutput};
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::priority_queue::{Compare, PriorityQueue};

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
    pub fn write_compound_file<D: Directory>(
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
            pq = PriorityQueue::new(num_files as i32, SizedFileQueueCmp)?;
            {
                for filename in files {
                    let file_length = directory.file_length(filename)?;
                    pq.add(SizedFile::new(filename.to_string(), file_length))?;
                }
            }
        }
        while pq.size() > 0 {
            let sized_file = pq.pop()?;
            debug_assert!(sized_file.is_some());
            let file = &sized_file.unwrap().name;
            let start_offset = data.align_file_pointer(BitUtil::LONG_BYTES as i32)?;
            let mut file_input = directory.open_checksum_input(file)?;
            // just copies the index header, verifying that its id matches what
            // we expect
            CodecUtil::verify_and_copy_index_header(&mut file_input, data, si.get_id())?;
            // copy all bytes except the footer
            let num_bytes_to_copy = file_input.length()
                - CodecUtil::footer_length() as i64
                - file_input.get_file_pointer();
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
            CodecUtil::write_be_long(data, checksum)?;
            let end_offset = data.get_file_pointer();
            let length = end_offset - start_offset;
            // write entry for file
            entries.write_string(IndexFileNames::strip_segment_name(file))?;
            entries.write_long(start_offset)?;
            entries.write_long(length)?;
        }
        Ok(())
    }
}

impl CompoundFormat for Lucene90CompoundFormat {
    type Directory<D>
        = CompoundDirectory<Lucene90CompoundReader<D>>
    where
        D: Directory;

    fn get_compound_reader<D>(&self, dir: &D, si: &SegmentInfo<D>) -> Result<Self::Directory<D>>
    where
        D: Directory,
    {
        Ok(CompoundDirectory::new(Lucene90CompoundReader::new(
            dir, si,
        )?))
    }

    fn write<D>(&self, dir: &impl Directory, si: &SegmentInfo<D>, context: &IOContext) -> Result<()>
    where
        D: Directory,
    {
        let data_file =
            IndexFileNames::segment_file_name(&si.name, "", Lucene90CompoundFormat::DATA_EXTENSION);
        let entries_file = IndexFileNames::segment_file_name(
            &si.name,
            "",
            Lucene90CompoundFormat::ENTRIES_EXTENSION,
        );
        let mut data_output;
        let mut entries_output;
        {
            data_output = dir.create_output(&data_file, context)?;
            entries_output = dir.create_output(&entries_file, context)?;
        }

        CodecUtil::write_index_header(
            &mut data_output,
            Lucene90CompoundFormat::DATA_CODEC,
            Lucene90CompoundFormat::VERSION_CURRENT,
            si.get_id(),
            "",
        )?;
        CodecUtil::write_index_header(
            &mut entries_output,
            Lucene90CompoundFormat::ENTRY_CODEC,
            Lucene90CompoundFormat::VERSION_CURRENT,
            si.get_id(),
            "",
        )?;
        self.write_compound_file(&mut entries_output, &mut data_output, dir, si)?;
        CodecUtil::write_footer(&mut data_output)?;
        CodecUtil::write_footer(&mut entries_output)?;

        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Default)]
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rand::Rng;
    use rand::prelude::SliceRandom;

    use crate::core::codecs::{
        Codec, CodecUtil, CompoundFormat, LATEST_CODEC, Lucene90CompoundFormat,
    };
    use crate::core::index::IndexFileNames;
    use crate::core::store::directory::Directory;
    use crate::core::store::{DataInput, IO_CONTEXT_DEFAULT};
    use crate::core::util::error::lucene_error::Result;
    use crate::test::index::base_compound_format_test_case::{
        BaseCompoundFormatTestCase, create_random_file, new_segment_info,
    };
    use crate::test::util::lucene_test_case::lucene_test_case_util::new_directory;
    use crate::test::util::lucene_test_case::lucene_test_case_util::random;

    pub struct TestLucene90CompoundFormat;
    impl BaseCompoundFormatTestCase for TestLucene90CompoundFormat {}
    #[test]
    fn test_empty() -> Result<()> {
        let mut random = random();
        let case = TestLucene90CompoundFormat;
        case.test_empty(&mut random)
    }
    #[test]
    fn test_single_file() -> Result<()> {
        let mut random = random();
        let case = TestLucene90CompoundFormat;
        case.test_single_file(&mut random)
    }
    #[test]
    fn test_two_files() -> Result<()> {
        let mut random = random();
        let case = TestLucene90CompoundFormat;
        case.test_two_files(&mut random)
    }
    #[test]
    fn test_double_close() -> Result<()> {
        let case = TestLucene90CompoundFormat;
        case.test_double_close()
    }
    #[test]
    fn test_pass_io_context() -> Result<()> {
        let case = TestLucene90CompoundFormat;
        case.test_pass_io_context()
    }
    #[test]
    fn test_large_cfs() -> Result<()> {
        let case = TestLucene90CompoundFormat;
        case.test_large_cfs()
    }
    #[test]
    fn test_list_all() -> Result<()> {
        let case = TestLucene90CompoundFormat;
        case.test_list_all()
    }
    #[test]
    fn test_create_output_disabled() -> Result<()> {
        let mut random = random();
        let case = TestLucene90CompoundFormat;
        case.test_create_output_disabled(&mut random)
    }
    #[test]
    fn test_delete_file_disabled() -> Result<()> {
        let mut random = random();
        let case = TestLucene90CompoundFormat;
        case.test_delete_file_disabled(&mut random)
    }
    #[test]
    fn test_rename_file_disabled() -> Result<()> {
        let mut random = random();
        let case = TestLucene90CompoundFormat;
        case.test_rename_file_disabled(&mut random)
    }
    #[test]
    fn test_sync_disabled() -> Result<()> {
        let mut random = random();
        let case = TestLucene90CompoundFormat;
        case.test_sync_disabled(&mut random)
    }
    #[test]
    fn test_make_lock_disabled() -> Result<()> {
        let mut random = random();
        let case = TestLucene90CompoundFormat;
        case.test_make_lock_disabled(&mut random)
    }
    #[test]
    fn test_random_files() -> Result<()> {
        let mut random = random();
        let case = TestLucene90CompoundFormat;
        case.test_random_files(&mut random)
    }
    #[test]
    fn test_many_sub_files() -> Result<()> {
        let mut random = random();
        let case = TestLucene90CompoundFormat;
        case.test_many_sub_files(&mut random)
    }
    #[test]
    fn test_cloned_streams_closing() -> Result<()> {
        let mut random = random();
        let case = TestLucene90CompoundFormat;
        case.test_cloned_streams_closing(&mut random)
    }
    #[test]
    fn test_random_access() -> Result<()> {
        let mut random = random();
        let case = TestLucene90CompoundFormat;
        case.test_random_access(&mut random)
    }
    #[test]
    fn test_random_access_clones() -> Result<()> {
        let mut random = random();
        let case = TestLucene90CompoundFormat;
        case.test_random_access_clones(&mut random)
    }
    #[test]
    fn test_file_not_found() -> Result<()> {
        let mut random = random();
        let case = TestLucene90CompoundFormat;
        case.test_file_not_found(&mut random)
    }
    #[test]
    fn test_read_past_eof() -> Result<()> {
        let mut random = random();
        let case = TestLucene90CompoundFormat;
        case.test_read_past_eof(&mut random)
    }
    #[test]
    fn test_resource_name_inside_compound_file() -> Result<()> {
        let mut random = random();
        let case = TestLucene90CompoundFormat;
        case.test_resource_name_inside_compound_file(&mut random)
    }
    #[test]
    fn test_missing_codec_headers_are_caught() -> Result<()> {
        let mut random = random();
        let case = TestLucene90CompoundFormat;
        case.test_missing_codec_headers_are_caught(&mut random)
    }
    #[test]
    fn test_corrupt_files_are_caught() -> Result<()> {
        let mut random = random();
        let case = TestLucene90CompoundFormat;
        case.test_corrupt_files_are_caught(&mut random)
    }
    #[test]
    fn test_check_integrity() -> Result<()> {
        let mut random = random();
        let case = TestLucene90CompoundFormat;
        case.test_check_integrity(&mut random)
    }

    #[test]
    fn test_file_length_ordering() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        let segment = "_123";
        let chunk = 1024; // internal buffer size used by the stream
        let mut si = new_segment_info(&mut random, dir.clone(), segment)?;

        let seg_id = si.get_id();
        let mut ordered_files = Vec::new();
        let mut random_file_size = random.random_range(0..chunk);

        for i in 0..10 {
            let filename = format!("{}.{}", segment, i);
            create_random_file(
                &mut random,
                dir.as_ref(),
                &filename,
                random_file_size,
                seg_id,
            )?;
            random_file_size += random.random_range(1..100);
            ordered_files.push(filename);
        }

        let mut shuffled_files = ordered_files.clone();
        shuffled_files.shuffle(&mut random);
        let files = shuffled_files.into_iter().collect();
        si.set_files(files)?;

        LATEST_CODEC
            .compound_format()
            .write(dir.as_ref(), &si, &IO_CONTEXT_DEFAULT)?;

        // Entries file should contain files ordered by their size
        let entries_file_name = IndexFileNames::segment_file_name(
            &si.name,
            "",
            Lucene90CompoundFormat::ENTRIES_EXTENSION,
        );
        let mut entries_stream = dir.open_checksum_input(&entries_file_name)?;

        let mut prior_e = None;
        let result: Result<()> = (|| {
            CodecUtil::check_index_header(
                &mut entries_stream,
                Lucene90CompoundFormat::ENTRY_CODEC,
                Lucene90CompoundFormat::VERSION_START,
                Lucene90CompoundFormat::VERSION_CURRENT,
                si.get_id(),
                "",
            )?;

            let num_entries = entries_stream.read_vint()?;
            let mut last_offset = 0;
            let mut last_length = 0;
            for i in 0..num_entries {
                let id = entries_stream.read_string()?;
                assert_eq!(ordered_files[i as usize], format!("{}{}", segment, id));
                let offset = entries_stream.read_long()?;
                assert!(offset > last_offset);
                last_offset = offset;
                let length = entries_stream.read_long()?;
                assert!(length >= last_length);
                last_length = length;
            }
            Ok(())
        })();
        if let Err(e) = result {
            prior_e = Some(e);
        }

        if let Some(e) = prior_e {
            return Err(CodecUtil::check_footer_with_error(&mut entries_stream, e));
        } else {
            CodecUtil::check_footer(&mut entries_stream)?;
        }

        Ok(())
    }
}
