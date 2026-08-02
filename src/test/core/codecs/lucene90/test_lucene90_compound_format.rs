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
use crate::test_framework::core::util::lucene_test_case::{new_directory_shared, random};
use rand::Rng;
use rand::RngExt;
use rand::prelude::SliceRandom;

use crate::core::codecs::{Codec, CodecUtil, Codecs, CompoundFormat, Lucene90CompoundFormat};
use crate::core::index::IndexFileNames;
use crate::core::store::directory::Directory;
use crate::core::store::{DataInput, IO_CONTEXT_DEFAULT};
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::index::base_compound_format_test_case::{
  BaseCompoundFormatTestCase, BaseCompoundFormatTestCaseDefaults, create_random_file,
  new_segment_info,
};
use crate::test_framework::core::index::base_index_file_format_test_case::BaseIndexFileFormatTestCase;
use crate::test_framework::core::util::test_util::TestUtil;
#[allow(dead_code)] // for quick search
pub struct TestLucene90CompoundFormat;

mod base_compound_format_test_case_tests {
  use crate::core::util::error::lucene_error::Result;
  use crate::test::core::codecs::lucene90::test_lucene90_compound_format::run_case;
  use crate::test_framework::core::index::base_compound_format_test_case::BaseCompoundFormatTestCase;

  #[test]
  fn test_empty() -> Result<()> {
    run_case(|case, random| case.test_empty(random))
  }

  #[test]
  fn test_single_file() -> Result<()> {
    run_case(|case, random| case.test_single_file(random))
  }

  #[test]
  fn test_two_files() -> Result<()> {
    run_case(|case, random| case.test_two_files(random))
  }

  #[test]
  fn test_double_close() -> Result<()> {
    run_case(|case, random| case.test_double_close(random))
  }

  #[test]
  fn test_pass_io_context() -> Result<()> {
    run_case(|case, random| case.test_pass_io_context(random))
  }

  #[test]
  fn test_large_cfs() -> Result<()> {
    run_case(|case, random| case.test_large_cfs(random))
  }

  #[test]
  fn test_list_all() -> Result<()> {
    run_case(|case, random| case.test_list_all(random))
  }

  #[test]
  fn test_create_output_disabled() -> Result<()> {
    run_case(|case, random| case.test_create_output_disabled(random))
  }

  #[test]
  fn test_delete_file_disabled() -> Result<()> {
    run_case(|case, random| case.test_delete_file_disabled(random))
  }

  #[test]
  fn test_rename_file_disabled() -> Result<()> {
    run_case(|case, random| case.test_rename_file_disabled(random))
  }

  #[test]
  fn test_sync_disabled() -> Result<()> {
    run_case(|case, random| case.test_sync_disabled(random))
  }

  #[test]
  fn test_make_lock_disabled() -> Result<()> {
    run_case(|case, random| case.test_make_lock_disabled(random))
  }

  #[test]
  fn test_random_files() -> Result<()> {
    run_case(|case, random| case.test_random_files(random))
  }

  #[test]
  fn test_many_sub_files() -> Result<()> {
    run_case(|case, random| case.test_many_sub_files(random))
  }

  #[test]
  fn test_cloned_streams_closing() -> Result<()> {
    run_case(|case, random| case.test_cloned_streams_closing(random))
  }

  #[test]
  fn test_random_access() -> Result<()> {
    run_case(|case, random| case.test_random_access(random))
  }

  #[test]
  fn test_random_access_clones() -> Result<()> {
    run_case(|case, random| case.test_random_access_clones(random))
  }

  #[test]
  fn test_file_not_found() -> Result<()> {
    run_case(|case, random| case.test_file_not_found(random))
  }

  #[test]
  fn test_read_past_eof() -> Result<()> {
    run_case(|case, random| case.test_read_past_eof(random))
  }

  #[test]
  fn test_merge_stability() -> Result<()> {
    run_case(|case, _random| case.test_merge_stability())
  }

  #[test]
  fn test_resource_name_inside_compound_file() -> Result<()> {
    run_case(|case, random| case.test_resource_name_inside_compound_file(random))
  }

  #[test]
  fn test_missing_codec_headers_are_caught() -> Result<()> {
    run_case(|case, random| case.test_missing_codec_headers_are_caught(random))
  }

  #[test]
  fn test_corrupt_files_are_caught() -> Result<()> {
    run_case(|case, random| case.test_corrupt_files_are_caught(random))
  }

  #[test]
  fn test_check_integrity() -> Result<()> {
    run_case(|case, random| case.test_check_integrity(random))
  }
}
#[test]
fn test_file_length_ordering() -> Result<()> {
  run_case(|case, random| case.test_file_length_ordering(random))
}
impl BaseIndexFileFormatTestCase for TestLucene90CompoundFormat {
  type Defaults = BaseCompoundFormatTestCaseDefaults;

  fn get_codec(&self) -> Result<Codecs> {
    Ok(TestUtil::get_default_codec().into())
  }
}

impl BaseCompoundFormatTestCase for TestLucene90CompoundFormat {}

fn run_case<F>(f: F) -> Result<()>
where
  F: FnOnce(&TestLucene90CompoundFormat, &mut rand::prelude::StdRng) -> Result<()>,
{
  let mut random = random();
  let case = TestLucene90CompoundFormat;
  f(&case, &mut random)
}

impl TestLucene90CompoundFormat {
  fn test_file_length_ordering<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + RngExt + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let segment = "_123";
    let chunk = 1024; // internal buffer size used by the stream
    let mut si = new_segment_info(random, dir.clone(), segment)?;

    let seg_id = si.get_id();
    let mut ordered_files = Vec::new();
    let mut random_file_size = random.random_range(0..chunk);

    for i in 0..10 {
      let filename = format!("{}.{}", segment, i);
      create_random_file(random, dir.as_ref(), &filename, random_file_size, seg_id)?;
      random_file_size += random.random_range(1..100);
      ordered_files.push(filename);
    }

    let mut shuffled_files = ordered_files.clone();
    shuffled_files.shuffle(random);
    let files = shuffled_files.into_iter().collect();
    si.set_files(files)?;

    si.get_codec()?
      .compound_format()
      .write(dir.as_ref(), &si, &IO_CONTEXT_DEFAULT)?;

    let entries_file_name =
      IndexFileNames::segment_file_name(&si.name, "", Lucene90CompoundFormat::ENTRIES_EXTENSION);
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

mod base_index_file_format_test_case_test {
  use super::run_case;
  use crate::core::util::error::lucene_error::Result;
  use crate::test_framework::core::index::base_index_file_format_test_case::BaseIndexFileFormatTestCase;

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
