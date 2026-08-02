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
use crate::test_framework::core::util::lucene_test_case::{
  at_least_usize, create_temp_dir_with_prefix, new_directory_shared, new_fs_directory,
  new_io_context, new_mock_fs_directory,
};
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

use rand::Rng;
use rand::RngExt;

use crate::core::codecs::compound_directory::CompoundDirectory;
use crate::core::codecs::lucene90_compound_reader::Lucene90CompoundReader;
use crate::core::codecs::{Codec, CodecUtil, CompoundFormat, codec};
use crate::core::document::document::Document;
use crate::core::document::field::{FieldBase, Store};
use crate::core::document::stored_field::StoredField;
use crate::core::document::string_field::StringField;
use crate::core::document::text_field::TextField;
use crate::core::index::index_reader::Identity;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_infos::SegmentInfos;
use crate::core::store::IndexOutput;
use crate::core::store::directory::Directory;
use crate::core::store::flush_info::FlushInfo;
use crate::core::store::nrt_caching_directory::NRTCachingDirectory;
use crate::core::store::{DataInput, DataOutput, IOContext};
use crate::core::store::{IO_CONTEXT_DEFAULT, IndexInput};
use crate::core::util::bit_set::BitSet;
use crate::core::util::bits::Bits;
use crate::core::util::clone::TryClone as OtherClone;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::io_utils::IOUtils;
use crate::core::util::{HasIdentity, LATEST, StringHelper};
use crate::test_framework::core::index::base_index_file_format_test_case::{
  BaseIndexFileFormatTestCase, BaseIndexFileFormatTestCaseDefaults, FileTrackingDirectoryWrapper,
  ReadBytesDirectoryWrapper,
};
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::util::test_util::TestUtil;

pub struct BaseCompoundFormatTestCaseDefaults;

pub trait BaseCompoundFormatTestCase:
  BaseIndexFileFormatTestCase<Defaults = BaseCompoundFormatTestCaseDefaults>
{
  fn test_empty<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let mut si = new_segment_info(random, dir.clone(), "_123")?;
    si.set_files(HashSet::new())?;
    si.get_codec()?
      .compound_format()
      .write(dir.as_ref(), &si, &IO_CONTEXT_DEFAULT)?;
    let cfs = si
      .get_codec()?
      .compound_format()
      .get_compound_reader(dir.as_ref(), &si)?;
    assert_eq!(0, cfs.list_all()?.len());
    Ok(())
  }
  /// This test creates compound file based on a single file. Files of
  /// different sizes are tested: 0, 1, 10, 100 bytes.
  fn test_single_file<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let data = [0, 1, 10, 100];
    for (i, &size) in data.iter().enumerate() {
      let test_file = format!("_{}.test", i);
      let dir = new_directory_shared(random)?;
      let mut si = new_segment_info(random, dir.clone(), &format!("_{}", i))?;
      create_sequence_file(
        random,
        dir.as_ref(),
        &test_file,
        0,
        size,
        si.get_id(),
        "suffix",
      )?;

      si.set_files(HashSet::from([test_file.clone()]))?;
      si.get_codec()?
        .compound_format()
        .write(dir.as_ref(), &si, &IO_CONTEXT_DEFAULT)?;

      let cfs = si
        .get_codec()?
        .compound_format()
        .get_compound_reader(dir.as_ref(), &si)?;

      let mut expected = dir.open_input(&test_file, &new_io_context(random)?)?;
      let mut actual = cfs.open_input(&test_file, &new_io_context(random)?)?;

      assert_same_streams(&test_file, &mut expected, &mut actual)?;
      assert_same_seek_behavior(&test_file, &mut expected, &mut actual)?;
    }
    Ok(())
  }
  /// This test creates compound file based on two files.
  fn test_two_files<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let files = ["_123.d1", "_123.d2"];
    let dir = new_directory_shared(random)?;
    let mut si = new_segment_info(random, dir.clone(), "_123")?;

    create_sequence_file(random, dir.as_ref(), files[0], 0, 15, si.get_id(), "suffix")?;
    create_sequence_file(
      random,
      dir.as_ref(),
      files[1],
      0,
      114,
      si.get_id(),
      "suffix",
    )?;

    let files_set: HashSet<String> = files.iter().map(|&file| file.to_string()).collect();
    si.set_files(files_set)?;
    si.get_codec()?
      .compound_format()
      .write(dir.as_ref(), &si, &IO_CONTEXT_DEFAULT)?;
    let cfs = si
      .get_codec()?
      .compound_format()
      .get_compound_reader(dir.as_ref(), &si)?;

    for file in files.iter() {
      let mut expected = dir.open_input(file, &new_io_context(random)?)?;
      let mut actual = cfs.open_input(file, &new_io_context(random)?)?;
      assert_same_streams(file, &mut expected, &mut actual)?;
      assert_same_seek_behavior(file, &mut expected, &mut actual)?;
    }
    Ok(())
  }
  // test that a second call to close() behaves according to Closeable
  fn test_double_close<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let test_file = "_123.test";

    let dir = new_directory_shared(random)?;
    let mut si = new_segment_info(random, dir.clone(), "_123")?;
    let mut out = dir.create_output(test_file, &IO_CONTEXT_DEFAULT)?;
    let body_result = (|| {
      CodecUtil::write_index_header(&mut out, "Foo", 0, si.get_id(), "suffix")?;
      out.write_int(3)?;
      CodecUtil::write_footer(&mut out)
    })();
    IOUtils::use_or_suppress_result(body_result, out.close())?;

    si.set_files(HashSet::from([test_file.to_string()]))?;
    si.get_codec()?
      .compound_format()
      .write(dir.as_ref(), &si, &IO_CONTEXT_DEFAULT)?;
    let cfs = si
      .get_codec()?
      .compound_format()
      .get_compound_reader(dir.as_ref(), &si)?;
    assert_eq!(1, cfs.list_all()?.len());
    cfs.close()?;
    cfs.close()?; // second close should not throw exception
    dir.close()
  }

  // LUCENE-5724: things like NRTCachingDir rely upon IOContext being properly passed down
  fn test_pass_io_context<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let test_file = "_123.test";
    let my_context = Arc::new(IO_CONTEXT_DEFAULT.clone());

    let dir = Arc::new(IOContextAssertingDirectoryWrapper::new(
      new_directory_shared(random)?,
      my_context.clone(),
    ));
    let mut si = new_segment_info(random, dir.clone(), "_123")?;
    let mut out = dir.create_output(test_file, my_context.as_ref())?;
    let body_result = (|| {
      CodecUtil::write_index_header(&mut out, "Foo", 0, si.get_id(), "suffix")?;
      out.write_int(3)?;
      CodecUtil::write_footer(&mut out)
    })();
    IOUtils::use_or_suppress_result(body_result, out.close())?;

    si.set_files(HashSet::from([test_file.to_string()]))?;
    si.get_codec()?
      .compound_format()
      .write(dir.as_ref(), &si, my_context.as_ref())?;
    dir.close()
  }

  // LUCENE-5724: actually test we play nice with NRTCachingDir and massive file
  fn test_large_cfs<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let test_file = "_123.test";
    let context = IOContext::with_flush(FlushInfo::new(0, 512 * 1024 * 1024))?;

    let fs_dir = new_fs_directory(
      random,
      create_temp_dir_with_prefix("BaseCompoundFormatTestCase")?,
    )?;
    let dir = Arc::new(NRTCachingDirectory::new(fs_dir, 2.0, 25.0));

    let mut si = new_segment_info(random, dir.clone(), "_123")?;
    let mut out = dir.create_output(test_file, &context)?;
    let body_result = (|| {
      CodecUtil::write_index_header(&mut out, "Foo", 0, si.get_id(), "suffix")?;
      let bytes = [0u8; 512];
      for _ in 0..1024 * 1024 {
        out.write_bytes_range(&bytes, 0, bytes.len())?;
      }
      CodecUtil::write_footer(&mut out)
    })();
    IOUtils::use_or_suppress_result(body_result, out.close())?;

    si.set_files(HashSet::from([test_file.to_string()]))?;
    si.get_codec()?
      .compound_format()
      .write(dir.as_ref(), &si, &context)?;

    dir.close()
  }

  // Just tests that we can open all files returned by listAll
  fn test_list_all<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    // riw should sometimes create docvalues fields, etc
    let riw = RandomIndexWriter::new(random, dir.clone())?;

    let mut doc = Document::new();
    // these fields should sometimes get term vectors, etc
    let mut id_field = StringField::from_string("id", "", Store::No)?;
    let mut body_field = TextField::from_string("body", "", Store::No)?;
    doc.add(id_field.clone());
    doc.add(body_field.clone());

    for i in 0..100 {
      id_field.set_string_value(i.to_string())?;
      body_field.set_string_value(TestUtil::random_unicode_string(random))?;
      let mut doc = Document::new();
      doc.add(id_field.clone());
      doc.add(body_field.clone());
      riw.add_document(random, doc)?;

      if random.random_range(0..7) == 0 {
        riw.commit(random)?;
      }
    }

    riw.close(random)?;

    let infos = SegmentInfos::read_latest_commit(dir.clone())?;

    for si in infos.iter() {
      if si.info.get_use_compound_file() {
        let cfs_dir = si
          .info
          .get_codec()?
          .compound_format()
          .get_compound_reader(dir.as_ref(), &si.info)?;
        let files = cfs_dir.list_all()?;

        for file in files {
          let input = cfs_dir.open_input(&file, &IO_CONTEXT_DEFAULT)?;
          input.close()?;
        }
        cfs_dir.close()?;
      }
    }

    dir.close()
  }
  /// Test that the compound file system (CFS) reader is read-only by
  /// attempting to create an output.
  fn test_create_output_disabled<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let mut si = new_segment_info(random, dir.clone(), "_123")?;
    si.set_files(HashSet::new())?;
    si.get_codec()?
      .compound_format()
      .write(dir.as_ref(), &si, &IO_CONTEXT_DEFAULT)?;
    let cfs = si
      .get_codec()?
      .compound_format()
      .get_compound_reader(dir.as_ref(), &si)?;
    let io_context = IOContext::default_io_context()?;
    let result = cfs.create_output("bogus", &io_context);
    assert!(matches!(result, Err(LuceneError::UnsupportedOperation(_))));
    Ok(())
  }
  /// Test that the CFS reader is read-only, and that `deleteFile` is
  /// disabled.
  fn test_delete_file_disabled<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let testfile = "_123.test";
    let dir = new_directory_shared(random)?;
    let mut out = dir.create_output(testfile, &IOContext::default_io_context()?)?;
    out.write_int(3)?;
    let mut si = new_segment_info(random, dir.clone(), "_123")?;
    si.set_files(HashSet::new())?;
    si.get_codec()?
      .compound_format()
      .write(dir.as_ref(), &si, &IO_CONTEXT_DEFAULT)?;
    let cfs = si
      .get_codec()?
      .compound_format()
      .get_compound_reader(dir.as_ref(), &si)?;
    let result = cfs.delete_file(testfile);
    assert!(matches!(result, Err(LuceneError::UnsupportedOperation(_))));
    Ok(())
  }
  /// Test that the CFS reader is read-only, and that `rename` is disabled.
  fn test_rename_file_disabled<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let testfile = "_123.test";
    let dir = new_directory_shared(random)?;
    let mut out = dir.create_output(testfile, &IOContext::default_io_context()?)?;
    out.write_int(3)?;
    let mut si = new_segment_info(random, dir.clone(), "_123")?;
    si.set_files(HashSet::new())?;
    si.get_codec()?
      .compound_format()
      .write(dir.as_ref(), &si, &IO_CONTEXT_DEFAULT)?;
    let cfs = si
      .get_codec()?
      .compound_format()
      .get_compound_reader(dir.as_ref(), &si)?;
    let result = cfs.rename(testfile, "bogus");
    assert!(matches!(result, Err(LuceneError::UnsupportedOperation(_))));
    Ok(())
  }
  /// Test that the CFS reader is read-only, and that `sync` is disabled.
  fn test_sync_disabled<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let testfile = "_123.test";
    let dir = new_directory_shared(random)?;
    let mut out = dir.create_output(testfile, &IOContext::default_io_context()?)?;
    out.write_int(3)?;
    let mut si = new_segment_info(random, dir.clone(), "_123")?;
    si.set_files(HashSet::new())?;
    si.get_codec()?
      .compound_format()
      .write(dir.as_ref(), &si, &IO_CONTEXT_DEFAULT)?;
    let cfs = si
      .get_codec()?
      .compound_format()
      .get_compound_reader(dir.as_ref(), &si)?;
    let sync_files = vec![testfile.to_string()];
    let result = cfs.sync(&sync_files);
    assert!(matches!(result, Err(LuceneError::UnsupportedOperation(_))));
    Ok(())
  }

  /// Test that the CFS reader is read-only, and that obtaining locks is
  /// disabled.
  fn test_make_lock_disabled<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let testfile = "_123.test";
    let dir = new_directory_shared(random)?;
    let mut out = dir.create_output(testfile, &IOContext::default_io_context()?)?;
    out.write_int(3)?;
    let mut si = new_segment_info(random, dir.clone(), "_123")?;
    si.set_files(HashSet::new())?;
    si.get_codec()?
      .compound_format()
      .write(dir.as_ref(), &si, &IO_CONTEXT_DEFAULT)?;
    let cfs = si
      .get_codec()?
      .compound_format()
      .get_compound_reader(dir.as_ref(), &si)?;
    let result = cfs.obtain_lock("foobar");
    assert!(matches!(result, Err(LuceneError::UnsupportedOperation(_))));
    Ok(())
  }
  /// This test creates a compound file based on a large number of files of
  /// various length. The file content is generated randomly. The sizes
  /// range from 0 to 1Mb. Some of the sizes are selected to test the
  /// buffering logic in the file reading code. For this, the chunk
  /// variable is set to the length of the buffer used internally by the
  /// compound file logic.
  fn test_random_files<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let segment = "_123";
    let chunk = 1024; // internal buffer size used by the stream
    let mut si = new_segment_info(random, dir.clone(), "_123")?;
    let seg_id = si.get_id();
    create_random_file(
      random,
      dir.as_ref(),
      &format!("{}.zero", segment),
      0,
      seg_id,
    )?;
    create_random_file(random, dir.as_ref(), &format!("{}.one", segment), 1, seg_id)?;
    create_random_file(
      random,
      dir.as_ref(),
      &format!("{}.ten", segment),
      10,
      seg_id,
    )?;
    create_random_file(
      random,
      dir.as_ref(),
      &format!("{}.hundred", segment),
      100,
      seg_id,
    )?;
    create_random_file(
      random,
      dir.as_ref(),
      &format!("{}.big1", segment),
      chunk,
      seg_id,
    )?;
    create_random_file(
      random,
      dir.as_ref(),
      &format!("{}.big2", segment),
      chunk - 1,
      seg_id,
    )?;
    create_random_file(
      random,
      dir.as_ref(),
      &format!("{}.big3", segment),
      chunk + 1,
      seg_id,
    )?;
    create_random_file(
      random,
      dir.as_ref(),
      &format!("{}.big4", segment),
      3 * chunk,
      seg_id,
    )?;
    create_random_file(
      random,
      dir.as_ref(),
      &format!("{}.big5", segment),
      3 * chunk - 1,
      seg_id,
    )?;
    create_random_file(
      random,
      dir.as_ref(),
      &format!("{}.big6", segment),
      3 * chunk + 1,
      seg_id,
    )?;
    create_random_file(
      random,
      dir.as_ref(),
      &format!("{}.big7", segment),
      1000 * chunk,
      seg_id,
    )?;
    let files: Vec<String> = dir
      .list_all()?
      .into_iter()
      .filter(|file| file.starts_with(segment))
      .collect();
    si.set_files(files.iter().cloned().collect())?;
    si.get_codec()?
      .compound_format()
      .write(dir.as_ref(), &si, &IO_CONTEXT_DEFAULT)?;
    let cfs = si
      .get_codec()?
      .compound_format()
      .get_compound_reader(dir.as_ref(), &si)?;

    // Validate each file
    for file in files.iter() {
      let mut check = dir.open_input(file, &new_io_context(random)?)?;
      let mut test = cfs.open_input(file, &new_io_context(random)?)?;
      assert_same_streams(file, &mut check, &mut test)?;
      assert_same_seek_behavior(file, &mut check, &mut test)?;
    }

    Ok(())
  }

  fn test_many_sub_files<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = Arc::new(new_mock_fs_directory(
      random,
      create_temp_dir_with_prefix("CFSManySubFiles")?,
    )?);
    let file_count = at_least_usize(random, 500);
    let mut files = Vec::new();
    let mut si = new_segment_info(random, dir.clone(), "_123")?;
    for file_idx in 0..file_count {
      let file = format!("_123.{}", file_idx);
      files.push(file.clone());
      let mut out = dir.create_output(&file, &new_io_context(random)?)?;
      CodecUtil::write_index_header(&mut out, "Foo", 0, si.get_id(), "suffix")?;
      out.write_byte(file_idx as u8)?;
      CodecUtil::write_footer(&mut out)?;
    }
    let file_sets = files.iter().cloned().collect();
    si.set_files(file_sets)?;
    si.get_codec()?
      .compound_format()
      .write(dir.as_ref(), &si, &IO_CONTEXT_DEFAULT)?;
    let cfs = si
      .get_codec()?
      .compound_format()
      .get_compound_reader(dir.as_ref(), &si)?;
    let mut ins = Vec::with_capacity(file_count);
    // Open the files
    for file_idx in 0..file_count {
      let file = format!("_123.{}", file_idx);
      let mut input = cfs.open_input(&file, &new_io_context(random)?)?;
      CodecUtil::check_index_header(&mut input, "Foo", 0, 0, si.get_id(), "suffix")?;
      ins.push(input);
    }
    // assert_eq!(dir.get_file_handle_count(), 1);
    for (file_idx, input) in ins.iter_mut().enumerate() {
      assert_eq!(DataInput::read_byte(input)?, file_idx as u8);
    }
    // Ensure only one file handle is used
    // assert_eq!(dir.get_file_handle_count(), 1);
    // for input in ins.iter_mut() {
    //     input.close()?;
    // }
    Ok(())
  }
  fn test_cloned_streams_closing<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let cr = create_large_cfs(random, dir.clone())?;

    let mut expected = dir.open_input("_123.f11", &new_io_context(random)?)?;
    let mut one = cr.open_input("_123.f11", &new_io_context(random)?)?;
    let mut two = one.try_clone()?;

    assert_same_streams("basic clone one", &mut expected, &mut one)?;
    expected.seek(0)?;
    assert_same_streams("basic clone two", &mut expected, &mut two)?;
    Ok(())
  }
  /// This test opens two files from a compound stream and verifies that their
  /// file positions are independent of each other.
  fn test_random_access<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let cr = create_large_cfs(random, dir.clone())?;

    // Open two files
    let mut e1 = dir.open_input("_123.f11", &new_io_context(random)?)?;
    let mut e2 = dir.open_input("_123.f3", &new_io_context(random)?)?;

    let mut a1 = cr.open_input("_123.f11", &new_io_context(random)?)?;
    let mut a2 = dir.open_input("_123.f3", &new_io_context(random)?)?;

    // Seek the first pair
    e1.seek(100)?;
    a1.seek(100)?;
    assert_eq!(100, e1.get_file_pointer()?);
    assert_eq!(100, a1.get_file_pointer()?);
    let be1 = DataInput::read_byte(&mut e1)?;
    let ba1 = DataInput::read_byte(&mut a1)?;
    assert_eq!(be1, ba1);

    // Now seek the second pair
    e2.seek(1027)?;
    a2.seek(1027)?;
    assert_eq!(1027, e2.get_file_pointer()?);
    assert_eq!(1027, a2.get_file_pointer()?);
    let be2 = DataInput::read_byte(&mut e2)?;
    let ba2 = DataInput::read_byte(&mut a2)?;
    assert_eq!(be2, ba2);

    // Now make sure the first one didn't move
    assert_eq!(101, e1.get_file_pointer()?);
    assert_eq!(101, a1.get_file_pointer()?);
    let be1 = DataInput::read_byte(&mut e1)?;
    let ba1 = DataInput::read_byte(&mut a1)?;
    assert_eq!(be1, ba1);

    // Now move the first one again, past the buffer length
    e1.seek(1910)?;
    a1.seek(1910)?;
    assert_eq!(1910, e1.get_file_pointer()?);
    assert_eq!(1910, a1.get_file_pointer()?);
    let be1 = DataInput::read_byte(&mut e1)?;
    let ba1 = DataInput::read_byte(&mut a1)?;
    assert_eq!(be1, ba1);

    // Now make sure the second set didn't move
    assert_eq!(1028, e2.get_file_pointer()?);
    assert_eq!(1028, a2.get_file_pointer()?);
    let be2 = DataInput::read_byte(&mut e2)?;
    let ba2 = DataInput::read_byte(&mut a2)?;
    assert_eq!(be2, ba2);

    // Move the second set back, again crossing the buffer size
    e2.seek(17)?;
    a2.seek(17)?;
    assert_eq!(17, e2.get_file_pointer()?);
    assert_eq!(17, a2.get_file_pointer()?);
    let be2 = DataInput::read_byte(&mut e2)?;
    let ba2 = DataInput::read_byte(&mut a2)?;
    assert_eq!(be2, ba2);

    // Finally, make sure the first set didn't move
    assert_eq!(1911, e1.get_file_pointer()?);
    assert_eq!(1911, a1.get_file_pointer()?);
    let be1 = DataInput::read_byte(&mut e1)?;
    let ba1 = DataInput::read_byte(&mut a1)?;
    assert_eq!(be1, ba1);
    Ok(())
  }
  /// This test opens two files from a compound stream and verifies that their
  /// file positions are independent of each other.
  fn test_random_access_clones<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let cr = create_large_cfs(random, dir.clone())?;

    // Open two files
    let mut e1 = cr.open_input("_123.f11", &new_io_context(random)?)?;
    let mut e2 = cr.open_input("_123.f3", &new_io_context(random)?)?;

    let mut a1 = e1.try_clone()?;
    let mut a2 = e2.try_clone()?;

    // Seek the first pair
    e1.seek(100)?;
    a1.seek(100)?;
    assert_eq!(100, e1.get_file_pointer()?);
    assert_eq!(100, a1.get_file_pointer()?);
    assert_eq!(
      DataInput::read_byte(&mut e1)?,
      DataInput::read_byte(&mut a1)?
    );

    // Now seek the second pair
    e2.seek(1027)?;
    a2.seek(1027)?;
    assert_eq!(1027, e2.get_file_pointer()?);
    assert_eq!(1027, a2.get_file_pointer()?);
    assert_eq!(
      DataInput::read_byte(&mut e2)?,
      DataInput::read_byte(&mut a2)?
    );

    // Now make sure the first one didn't move
    assert_eq!(101, e1.get_file_pointer()?);
    assert_eq!(101, a1.get_file_pointer()?);
    assert_eq!(
      DataInput::read_byte(&mut e1)?,
      DataInput::read_byte(&mut a1)?
    );

    // Now move the first one again, past the buffer length
    e1.seek(1910)?;
    a1.seek(1910)?;
    assert_eq!(1910, e1.get_file_pointer()?);
    assert_eq!(1910, a1.get_file_pointer()?);
    assert_eq!(
      DataInput::read_byte(&mut e1)?,
      DataInput::read_byte(&mut a1)?
    );

    // Now make sure the second set didn't move
    assert_eq!(1028, e2.get_file_pointer()?);
    assert_eq!(1028, a2.get_file_pointer()?);
    assert_eq!(
      DataInput::read_byte(&mut e2)?,
      DataInput::read_byte(&mut a2)?
    );

    // Move the second set back, again crossing the buffer size
    e2.seek(17)?;
    a2.seek(17)?;
    assert_eq!(17, e2.get_file_pointer()?);
    assert_eq!(17, a2.get_file_pointer()?);
    assert_eq!(
      DataInput::read_byte(&mut e2)?,
      DataInput::read_byte(&mut a2)?
    );

    // Finally, make sure the first set didn't move
    assert_eq!(1911, e1.get_file_pointer()?);
    assert_eq!(1911, a1.get_file_pointer()?);
    assert_eq!(
      DataInput::read_byte(&mut e1)?,
      DataInput::read_byte(&mut a1)?
    );

    Ok(())
  }
  fn test_file_not_found<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let cr = create_large_cfs(random, dir.clone())?;

    let result = cr.open_input("bogus", &new_io_context(random)?);
    assert!(matches!(result, Err(LuceneError::NoSuchFile(_))));
    Ok(())
  }
  fn test_read_past_eof<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let cr = create_large_cfs(random, dir.clone())?;
    let mut is = cr.open_input("_123.f2", &new_io_context(random)?)?;
    is.seek(IndexInput::length(&is)? - 10)?;
    let mut b = vec![0u8; 100];
    DataInput::read_bytes(&mut is, b.as_mut_slice(), 0, 10)?;
    let result = DataInput::read_byte(&mut is);
    assert!(matches!(result, Err(LuceneError::Eof(_))));
    is.seek(IndexInput::length(&is)? - 10)?;
    let result = DataInput::read_bytes(&mut is, &mut b, 0, 50);
    assert!(matches!(result, Err(LuceneError::Eof(_))));
    Ok(())
  }

  fn test_merge_stability(&self) -> Result<()> {
    // Test does not work with CFS.
    Ok(())
  }

  fn test_resource_name_inside_compound_file<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let sub_file = "_123.xyz";
    let mut si = new_segment_info(random, dir.clone(), "_123")?;
    create_sequence_file(random, dir.as_ref(), sub_file, 0, 10, si.get_id(), "suffix")?;
    let mut hash_set_file = HashSet::new();
    hash_set_file.insert(sub_file.to_string());
    si.set_files(hash_set_file)?;
    si.get_codec()?
      .compound_format()
      .write(dir.as_ref(), &si, &IO_CONTEXT_DEFAULT)?;
    let cfs = si
      .get_codec()?
      .compound_format()
      .get_compound_reader(dir.as_ref(), &si)?;
    let in_stream = cfs.open_input(sub_file, &new_io_context(random)?)?;
    let desc = in_stream.to_string();
    assert!(
      desc.contains(&format!("[slice={}]", sub_file)),
      "resource description hides that it's inside a compound file: {}",
      desc
    );
    Ok(())
  }
  fn test_missing_codec_headers_are_caught<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let sub_file = "_123.xyz";

    // Missing codec header
    {
      let mut os = dir.create_output(sub_file, &new_io_context(random)?)?;
      for i in 0..1024 {
        os.write_byte(i as u8)?;
      }
    }

    let mut si = new_segment_info(random, dir.clone(), "_123")?;
    let mut hash_set_file = HashSet::new();
    hash_set_file.insert(sub_file.to_string());
    si.set_files(hash_set_file)?;

    let result = si
      .get_codec()?
      .compound_format()
      .write(dir.as_ref(), &si, &IO_CONTEXT_DEFAULT);
    assert!(matches!(result, Err(LuceneError::CorruptIndex(_))));
    match result {
      Ok(_) => unreachable!(),
      Err(e) => {
        assert!(e.to_string().contains("codec header mismatch"));
        Ok(())
      },
    }
  }
  fn test_corrupt_files_are_caught<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let sub_file = "_123.xyz";

    // wrong checksum
    let mut si = new_segment_info(random, dir.clone(), "_123")?;
    {
      let mut os = dir.create_output(sub_file, &new_io_context(random)?)?;
      CodecUtil::write_index_header(&mut os, "Foo", 0, si.get_id(), "suffix")?;
      for i in 0..1024 {
        os.write_byte(i as u8)?;
      }

      // write footer with wrong checksum
      CodecUtil::write_be_int(&mut os, CodecUtil::FOOTER_MAGIC)?;
      CodecUtil::write_be_int(&mut os, 0)?;
      let checksum = os.get_checksum()?;
      assert!(checksum <= i64::MAX as u64);
      CodecUtil::write_be_long(&mut os, checksum as i64 + 1)?;
    }

    let mut hash_set_file = HashSet::new();
    hash_set_file.insert(sub_file.to_string());
    si.set_files(hash_set_file)?;

    let result = si
      .get_codec()?
      .compound_format()
      .write(dir.as_ref(), &si, &IO_CONTEXT_DEFAULT);

    assert!(matches!(result, Err(LuceneError::CorruptIndex(_))));

    match result {
      Ok(_) => unreachable!(),
      Err(e) => {
        assert!(
          e.to_string()
            .contains("checksum failed (hardware problem?)")
        );
        Ok(())
      },
    }
  }
  fn test_check_integrity<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let read_tracking_dir = Arc::new(ReadBytesDirectoryWrapper::new(dir.clone()));
    let sub_file = "_123.xyz";
    let mut si = new_segment_info(random, read_tracking_dir.clone(), "_123")?;
    let mut os = dir.create_output(sub_file, &new_io_context(random)?)?;
    let body_result = (|| {
      CodecUtil::write_index_header(&mut os, "Foo", 0, si.get_id(), "suffix")?;
      for i in 0..1024 {
        os.write_byte(i as u8)?;
      }
      CodecUtil::write_be_int(&mut os, CodecUtil::FOOTER_MAGIC)?;
      CodecUtil::write_be_int(&mut os, 0)?;
      let checksum = os.get_checksum()?;
      assert!(checksum <= i64::MAX as u64);
      CodecUtil::write_be_long(&mut os, checksum as i64)
    })();
    IOUtils::use_or_suppress_result(body_result, os.close())?;

    si.set_files(HashSet::from([sub_file.to_string()]))?;

    let write_tracking_dir = FileTrackingDirectoryWrapper::new(dir.clone());
    si.get_codec()?
      .compound_format()
      .write(&write_tracking_dir, &si, &IO_CONTEXT_DEFAULT)?;
    let created_files = write_tracking_dir.get_files();

    let compound_dir = si
      .get_codec()?
      .compound_format()
      .get_compound_reader(read_tracking_dir.as_ref(), &si)?;
    compound_dir.check_integrity()?;
    let read_bytes = read_tracking_dir.get_read_bytes();
    assert_eq!(
      created_files,
      read_bytes.keys().cloned().collect::<HashSet<_>>()
    );
    for (file, read) in read_bytes {
      let mut unread_bytes = read.clone();
      unread_bytes.flip_range(0, unread_bytes.length());
      let next = unread_bytes.next_set_bit(0);
      assert_eq!(
        crate::core::search::doc_id_set_iterator::NO_MORE_DOCS as usize,
        next,
        "Byte at offset {next} of {file} was not read"
      );
    }
    compound_dir.close()?;
    dir.close()?;
    Ok(())
  }
}

/// The named Rust equivalent of the anonymous `FilterDirectory` used by
/// `testPassIOContext`.
struct IOContextAssertingDirectoryWrapper<D>
where
  D: Directory,
{
  in_: D,
  expected_context: Arc<IOContext>,
  identity: Identity,
}

impl<D> IOContextAssertingDirectoryWrapper<D>
where
  D: Directory,
{
  fn new(in_: D, expected_context: Arc<IOContext>) -> Self {
    Self {
      in_,
      expected_context,
      identity: Identity::new(),
    }
  }
}

impl<D> Directory for IOContextAssertingDirectoryWrapper<D>
where
  D: Directory,
{
  fn list_all(&self) -> Result<Vec<String>> {
    self.in_.list_all()
  }

  fn delete_file(&self, name: &str) -> Result<()> {
    self.in_.delete_file(name)
  }

  fn file_length(&self, name: &str) -> Result<usize> {
    self.in_.file_length(name)
  }

  type IndexOutput = D::IndexOutput;

  fn create_output(&self, name: &str, context: &IOContext) -> Result<Self::IndexOutput> {
    assert!(std::ptr::eq(self.expected_context.as_ref(), context));
    self.in_.create_output(name, context)
  }

  fn create_temp_output(
    &self,
    prefix: &str,
    suffix: &str,
    context: &IOContext,
  ) -> Result<Self::IndexOutput> {
    self.in_.create_temp_output(prefix, suffix, context)
  }

  fn sync(&self, names: &[String]) -> Result<()> {
    self.in_.sync(names)
  }

  fn sync_metadata(&self) -> Result<()> {
    self.in_.sync_metadata()
  }

  fn rename(&self, source: &str, dest: &str) -> Result<()> {
    self.in_.rename(source, dest)
  }

  type IndexInput = D::IndexInput;

  fn open_input(&self, name: &str, context: &IOContext) -> Result<Self::IndexInput> {
    self.in_.open_input(name, context)
  }

  type Lock = D::Lock;

  fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
    self.in_.obtain_lock(name)
  }

  fn get_pending_deletions(&self) -> Result<HashSet<String>> {
    self.in_.get_pending_deletions()
  }
}

impl<D> Display for IOContextAssertingDirectoryWrapper<D>
where
  D: Directory,
{
  fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
    write!(formatter, "FilterDirectory({})", self.in_)
  }
}

impl<D> CloseableRef for IOContextAssertingDirectoryWrapper<D>
where
  D: Directory,
{
  fn close(&self) -> Result<()> {
    self.in_.close()
  }
}

impl<D> HasIdentity for IOContextAssertingDirectoryWrapper<D>
where
  D: Directory,
{
  fn identity(&self) -> &Identity {
    &self.identity
  }
}

impl<T> BaseIndexFileFormatTestCaseDefaults<T> for BaseCompoundFormatTestCaseDefaults
where
  T: BaseCompoundFormatTestCase,
{
  fn add_random_fields<R>(_test_case: &T, random: &mut R, document: &mut Document) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    document.add(StoredField::from_string(
      "foobar",
      TestUtil::random_simple_string(random),
    )?);
    Ok(())
  }
}

pub(crate) fn new_segment_info<D, R>(
  random: &mut R,
  dir: Arc<D>,
  name: &str,
) -> Result<SegmentInfo<D>>
where
  D: Directory,
  R: Rng + ?Sized,
{
  let min_version = if random.random_bool(0.5) {
    None
  } else {
    Some((*LATEST).clone())
  };
  let id = StringHelper::random_id();
  let value = SegmentInfo::new(
    dir,
    Some((*LATEST).clone()),
    min_version,
    name,
    10_000,
    false,
    false,
    Some(codec::get_default()),
    HashMap::new(),
    id,
    HashMap::new(),
    None,
  )?;
  Ok(value)
}
/// Creates a file of the specified size with random data.
pub(crate) fn create_random_file<R>(
  random: &mut R,
  dir: &impl Directory,
  name: &str,
  size: i32,
  seg_id: &[u8; StringHelper::ID_LENGTH],
) -> Result<()>
where
  R: Rng + ?Sized,
{
  let mut os = dir.create_output(name, &new_io_context(random)?)?;
  CodecUtil::write_index_header(&mut os, "Foo", 0, seg_id, "suffix")?;

  for _ in 0..size {
    let b = random.random_range(0..256) as u8;
    os.write_byte(b)?;
  }
  CodecUtil::write_footer(&mut os)?;
  Ok(())
}

/// Creates a file of the specified size with sequential data. The first byte is
/// written as the start byte provided. All subsequent bytes are computed as
/// start + offset where offset is the number of the byte.
fn create_sequence_file<R>(
  random: &mut R,
  dir: &impl Directory,
  name: &str,
  mut start: u8,
  size: i32,
  seg_id: &[u8; StringHelper::ID_LENGTH],
  seg_suffix: &str,
) -> Result<()>
where
  R: Rng + ?Sized,
{
  let mut os = dir.create_output(name, &new_io_context(random)?)?;
  CodecUtil::write_index_header(&mut os, "Foo", 0, seg_id, seg_suffix)?;
  for _ in 0..size {
    os.write_byte(start)?;
    start = start.wrapping_add(1);
  }
  CodecUtil::write_footer(&mut os)?;

  Ok(())
}

fn assert_same_streams(
  msg: &str,
  expected: &mut impl IndexInput,
  test: &mut impl IndexInput,
) -> Result<()> {
  assert_eq!(expected.length()?, test.length()?, "{} length", msg);
  assert_eq!(
    expected.get_file_pointer()?,
    test.get_file_pointer()?,
    "{} position",
    msg
  );

  let mut expected_buffer = vec![0u8; 512];
  let expected_len = expected.length()?;
  let mut test_buffer = vec![0u8; expected_len];

  let mut remainder = expected.length()? - expected.get_file_pointer()?;
  while remainder > 0 {
    let read_len = remainder.min(expected_buffer.len()) as usize;
    expected.read_bytes(&mut expected_buffer[..read_len], 0, read_len)?;
    test.read_bytes(&mut test_buffer[..read_len], 0, read_len)?;
    assert_equal_arrays(msg, &expected_buffer, &test_buffer, 0, read_len);
    remainder -= read_len;
  }
  Ok(())
}
fn assert_same_streams_seek_with_seek(
  msg: &str,
  expected: &mut impl IndexInput,
  actual: &mut impl IndexInput,
  seek_to: usize,
) -> Result<()> {
  if seek_to < expected.length()? {
    expected.seek(seek_to)?;
    actual.seek(seek_to)?;
    assert_same_streams(msg, expected, actual)?;
  }
  Ok(())
}

fn assert_same_seek_behavior(
  msg: &str,
  expected: &mut impl IndexInput,
  actual: &mut impl IndexInput,
) -> Result<()> {
  // Seek to 0
  let point = 0;
  assert_same_streams_seek_with_seek(msg, expected, actual, point)?;

  let length = expected.length()?;

  // Seek to middle
  let point = length / 2;
  assert_same_streams_seek_with_seek(msg, expected, actual, point)?;

  // Seek to end - 2
  let point = length - 2;
  assert_same_streams_seek_with_seek(msg, expected, actual, point)?;

  // Seek to end - 1
  let point = length - 1;
  assert_same_streams_seek_with_seek(msg, expected, actual, point)?;

  // Seek to the end
  let point = length;
  assert_same_streams_seek_with_seek(msg, expected, actual, point)?;

  // Seek past the end
  let point = length + 1;
  assert_same_streams_seek_with_seek(msg, expected, actual, point)?;

  Ok(())
}

fn assert_equal_arrays(msg: &str, expected: &[u8], test: &[u8], start: usize, len: usize) {
  assert!(!expected.is_empty(), "{} null expected", msg);
  assert!(!test.is_empty(), "{} null test", msg);

  for i in start..len {
    assert_eq!(expected[i], test[i], "{} {}", msg, i);
  }
}
/// Creates a large compound file with 20 sequential files, each of which is
/// 1000 bytes.
fn create_large_cfs<D, R>(random: &mut R, dir: Arc<D>) -> Result<Lucene90CompoundReader<D>>
where
  R: Rng + ?Sized,
  D: Directory,
{
  let mut files = HashSet::new();
  let mut si = new_segment_info(random, dir.clone(), "_123")?;

  // Create 20 sequential files
  for i in 0..20 {
    let file_name = format!("_123.f{}", i);
    create_sequence_file(
      random,
      dir.as_ref(),
      &file_name,
      0,
      2000,
      si.get_id(),
      "suffix",
    )?;
    files.insert(file_name);
  }
  si.set_files(files)?;
  si.get_codec()?
    .compound_format()
    .write(dir.as_ref(), &si, &IO_CONTEXT_DEFAULT)?;
  let cfs = si
    .get_codec()?
    .compound_format()
    .get_compound_reader(dir.as_ref(), &si)?;
  Ok(cfs)
}
