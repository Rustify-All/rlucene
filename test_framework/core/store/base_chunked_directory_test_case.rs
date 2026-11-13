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
use crate::test_framework::core::util::lucene_test_case::{
  at_least, create_temp_dir, create_temp_dir_with_prefix, is_night_mode,
  new_index_writer_config_with_analyzer, new_io_context, new_log_merge_policy, new_string_field,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use rand::{Rng, RngExt};

use crate::core::document::document::Document;
use crate::core::document::field::Store;
use crate::core::document::fields::Fields;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::stored_fields::StoredFields;
use crate::core::store::directory::Directory;
use crate::core::store::random_access_input::RandomAccessInput;
use crate::core::store::{DataInput, DataOutput, IndexInput, write_group_vints_i64};
use crate::core::util::clone::TryClone;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind, Result};
use crate::core::util::group_vint_util::GroupVIntUtil;
use crate::core::util::io_utils::IOUtils;
use crate::test_framework::array_equals_f32;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::store::base_directory_test_case::BaseDirectoryTestCase;
use crate::test_framework::core::util::test_util::TestUtil;

pub trait BaseChunkedDirectoryTestCase: BaseDirectoryTestCase {
  /// Creates a new directory with the specified max chunk size.
  fn get_directory_with_max_chunk_size(
    &self,
    path: PathBuf,
    max_chunk_size: usize,
  ) -> Result<Self::Directory>;

  fn test_group_vint_multi_blocks<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let max_chunk_size = random.random_range(64..512);
    let temp_dir = create_temp_dir()?;
    let dir =
      self.get_directory_with_max_chunk_size(temp_dir.path().to_path_buf(), max_chunk_size)?;
    let result = Self::do_test_group_vint(&dir, random, 10, 1, 31, 1024);
    IOUtils::use_or_suppress_result(result, dir.close())
  }

  fn test_clone_close<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = create_temp_dir_with_prefix("testCloneClose")?;
    let dir = self.get_directory(temp_dir.path().to_path_buf(), random)?;
    let io_context = new_io_context(random)?;
    let mut values = [0_i64, 7, 11, 9];
    {
      let mut io = dir.create_output("bytes", &io_context)?;
      io.write_vint(5)?;
      let values_len = values.len();
      write_group_vints_i64(&mut io, &mut values, values_len)?;
      io.close()?;
    }

    let mut one = dir.open_input("bytes", IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?)?;
    let mut two = one.try_clone()?;
    let mut three = two.try_clone()?;
    CloseableRef::close(&two)?;

    assert_eq!(5, one.read_vint()?);
    assert!(matches!(
      (two.read_vint()).as_ref().map_err(LuceneError::kind),
      Err(LuceneErrorKind::AlreadyClosed(_))
    ));
    let values_len = values.len();
    assert!(matches!(
      (GroupVIntUtil::read_group_vints_i64(&mut two, &mut values, values_len))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::AlreadyClosed(_))
    ));
    assert_eq!(5, three.read_vint()?);
    CloseableRef::close(&one)?;
    CloseableRef::close(&three)?;
    dir.close()?;
    Ok(())
  }

  fn test_clone_slice_close<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = create_temp_dir_with_prefix("testCloneSliceClose")?;
    let dir = self.get_directory(temp_dir.path().to_path_buf(), random)?;
    let io_context = new_io_context(random)?;
    let mut values = [0_i64, 7, 11, 9];
    {
      let mut io = dir.create_output("bytes", &io_context)?;
      io.write_int(1)?;
      io.write_int(2)?;
      let values_len = values.len();
      write_group_vints_i64(&mut io, &mut values, values_len)?;
      io.close()?;
    }

    let slicer = dir.open_input("bytes", &new_io_context(random)?)?;
    let mut one = slicer.slice("first int", 0, 4 + 5)?;
    let mut two = slicer.slice("second int", 4, 4)?;
    CloseableRef::close(&one)?;

    assert!(matches!(
      (DataInput::read_int(&mut one))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::AlreadyClosed(_))
    ));
    let values_len = values.len();
    assert!(matches!(
      (GroupVIntUtil::read_group_vints_i64(&mut one, &mut values, values_len))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::AlreadyClosed(_))
    ));
    assert_eq!(2, DataInput::read_int(&mut two)?);

    let mut another = slicer.slice("first int", 0, 4)?;
    assert_eq!(1, DataInput::read_int(&mut another)?);
    CloseableRef::close(&another)?;
    CloseableRef::close(&two)?;
    CloseableRef::close(&slicer)?;
    dir.close()?;
    Ok(())
  }

  fn test_seek_zero<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let upto = if is_night_mode() { 31 } else { 3 };
    for i in 0..upto {
      let temp_dir = create_temp_dir_with_prefix("testSeekZero")?;
      let dir = self.get_directory_with_max_chunk_size(temp_dir.path().to_path_buf(), 1 << i)?;
      let io_context = new_io_context(random)?;
      let mut io = dir.create_output("zeroBytes", &io_context)?;
      io.close()?;
      let mut ii = dir.open_input("zeroBytes", &new_io_context(random)?)?;
      ii.seek(0)?;
      CloseableRef::close(&ii)?;
      dir.close()?;
    }
    Ok(())
  }

  fn test_seek_slice_zero<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let upto = if is_night_mode() { 31 } else { 3 };
    for i in 0..upto {
      let temp_dir = create_temp_dir_with_prefix("testSeekSliceZero")?;
      let dir = self.get_directory_with_max_chunk_size(temp_dir.path().to_path_buf(), 1 << i)?;
      let io_context = new_io_context(random)?;
      let mut io = dir.create_output("zeroBytes", &io_context)?;
      io.close()?;
      let slicer = dir.open_input("zeroBytes", &new_io_context(random)?)?;
      let mut ii = slicer.slice("zero-length slice", 0, 0)?;
      ii.seek(0)?;
      CloseableRef::close(&ii)?;
      CloseableRef::close(&slicer)?;
      dir.close()?;
    }
    Ok(())
  }

  fn test_seek_end<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    for i in 0..17 {
      let temp_dir = create_temp_dir_with_prefix("testSeekEnd")?;
      let dir = self.get_directory_with_max_chunk_size(temp_dir.path().to_path_buf(), 1 << i)?;
      let io_context = new_io_context(random)?;
      let mut bytes = vec![0_u8; 1 << i];
      random.fill(&mut bytes[..]);
      {
        let mut io = dir.create_output("bytes", &io_context)?;
        io.write_bytes_with_len(&bytes, bytes.len())?;
        io.close()?;
      }

      let mut ii = dir.open_input("bytes", &new_io_context(random)?)?;
      let mut actual = vec![0_u8; 1 << i];
      let actual_len = actual.len();
      DataInput::read_bytes(&mut ii, &mut actual, 0, actual_len)?;
      assert_eq!(bytes, actual);
      ii.seek(1 << i)?;
      CloseableRef::close(&ii)?;
      dir.close()?;
    }
    Ok(())
  }

  fn test_seek_slice_end<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    for i in 0..17 {
      let temp_dir = create_temp_dir_with_prefix("testSeekSliceEnd")?;
      let dir = self.get_directory_with_max_chunk_size(temp_dir.path().to_path_buf(), 1 << i)?;
      let io_context = new_io_context(random)?;
      let mut bytes = vec![0_u8; 1 << i];
      random.fill(&mut bytes[..]);
      {
        let mut io = dir.create_output("bytes", &io_context)?;
        io.write_bytes_with_len(&bytes, bytes.len())?;
        io.close()?;
      }

      let slicer = dir.open_input("bytes", &new_io_context(random)?)?;
      let mut ii = slicer.slice("full slice", 0, bytes.len())?;
      let mut actual = vec![0_u8; 1 << i];
      let actual_len = actual.len();
      DataInput::read_bytes(&mut ii, &mut actual, 0, actual_len)?;
      assert_eq!(bytes, actual);
      ii.seek(1 << i)?;
      CloseableRef::close(&ii)?;
      CloseableRef::close(&slicer)?;
      dir.close()?;
    }
    Ok(())
  }

  fn test_seeking<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_iters = if is_night_mode() { 10 } else { 1 };
    for i in 0..num_iters {
      let temp_dir = create_temp_dir_with_prefix("testSeeking")?;
      let dir = self.get_directory_with_max_chunk_size(temp_dir.path().to_path_buf(), 1 << i)?;
      let io_context = new_io_context(random)?;
      let mut bytes = vec![0_u8; 1 << (i + 1)];
      random.fill(&mut bytes[..]);
      {
        let mut io = dir.create_output("bytes", &io_context)?;
        io.write_bytes_with_len(&bytes, bytes.len())?;
        io.close()?;
      }

      let mut ii = dir.open_input("bytes", &new_io_context(random)?)?;
      let mut actual = vec![0_u8; 1 << (i + 1)];
      let actual_len = actual.len();
      DataInput::read_bytes(&mut ii, &mut actual, 0, actual_len)?;
      assert_eq!(bytes, actual);
      let mut slice = Vec::new();
      for slice_start in 0..bytes.len() {
        for slice_length in 0..(bytes.len() - slice_start) {
          slice.clear();
          slice.resize(slice_length, 0_u8);
          ii.seek(slice_start)?;
          DataInput::read_bytes(&mut ii, &mut slice, 0, slice_length)?;
          assert_eq!(&bytes[slice_start..slice_start + slice_length], &slice[..]);
        }
      }
      CloseableRef::close(&ii)?;
      dir.close()?;
    }
    Ok(())
  }

  // note instead of seeking to offset and reading length, this opens slices at the
  // the various offset+length and just does readBytes.
  fn test_sliced_seeking<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_iters = if is_night_mode() { 10 } else { 1 };
    for i in 0..num_iters {
      let temp_dir = create_temp_dir_with_prefix("testSlicedSeeking")?;
      let dir = self.get_directory_with_max_chunk_size(temp_dir.path().to_path_buf(), 1 << i)?;
      let io_context = new_io_context(random)?;
      let mut bytes = vec![0_u8; 1 << (i + 1)];
      random.fill(&mut bytes[..]);
      {
        let mut io = dir.create_output("bytes", &io_context)?;
        io.write_bytes_with_len(&bytes, bytes.len())?;
        io.close()?;
      }

      let mut ii = dir.open_input("bytes", &new_io_context(random)?)?;
      let mut actual = vec![0_u8; 1 << (i + 1)];
      let actual_len = actual.len();
      DataInput::read_bytes(&mut ii, &mut actual, 0, actual_len)?;
      CloseableRef::close(&ii)?;
      assert_eq!(bytes, actual);

      let slicer = dir.open_input("bytes", &new_io_context(random)?)?;
      for slice_start in 0..bytes.len() {
        for slice_length in 0..(bytes.len() - slice_start) {
          Self::assert_slice(&bytes, &slicer, 0, slice_start, slice_length, random)?;
        }
      }
      CloseableRef::close(&slicer)?;
      dir.close()?;
    }
    Ok(())
  }

  fn test_slice_of_slice<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let upto = if is_night_mode() { 10 } else { 8 };
    for i in 0..upto {
      let temp_dir = create_temp_dir_with_prefix("testSliceOfSlice")?;
      let dir = self.get_directory_with_max_chunk_size(temp_dir.path().to_path_buf(), 1 << i)?;
      let io_context = new_io_context(random)?;
      let mut bytes = vec![0_u8; 1 << (i + 1)];
      random.fill(&mut bytes[..]);
      {
        let mut io = dir.create_output("bytes", &io_context)?;
        io.write_bytes_with_len(&bytes, bytes.len())?;
        io.close()?;
      }

      let mut ii = dir.open_input("bytes", &new_io_context(random)?)?;
      let mut actual = vec![0_u8; 1 << (i + 1)];
      let actual_len = actual.len();
      DataInput::read_bytes(&mut ii, &mut actual, 0, actual_len)?;
      CloseableRef::close(&ii)?;
      assert_eq!(bytes, actual);

      let outer_slicer = dir.open_input("bytes", &new_io_context(random)?)?;
      let outer_slice_start = random.random_range(0..(bytes.len() / 2));
      let outer_slice_length = random.random_range(0..(bytes.len() - outer_slice_start));
      let inner_slicer =
        outer_slicer.slice("parentBytesSlice", outer_slice_start, outer_slice_length)?;
      for slice_start in 0..outer_slice_length {
        for slice_length in 0..(outer_slice_length - slice_start) {
          Self::assert_slice(
            &bytes,
            &inner_slicer,
            outer_slice_start,
            slice_start,
            slice_length,
            random,
          )?;
        }
      }
      CloseableRef::close(&inner_slicer)?;
      CloseableRef::close(&outer_slicer)?;
      dir.close()?;
    }
    Ok(())
  }

  fn assert_slice<R, I>(
    bytes: &[u8],
    slicer: &I,
    outer_slice_start: usize,
    slice_start: usize,
    slice_length: usize,
    _random: &mut R,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
    I: IndexInput,
  {
    let mut slice = vec![0_u8; slice_length];
    let mut input = slicer.slice("bytesSlice", slice_start, slice_length)?;
    DataInput::read_bytes(&mut input, &mut slice, 0, slice_length)?;
    CloseableRef::close(&input)?;
    assert_eq!(
      &bytes[outer_slice_start + slice_start..outer_slice_start + slice_start + slice_length],
      &slice[..]
    );
    Ok(())
  }

  fn test_random_chunk_sizes<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num = if is_night_mode() {
      at_least(random, 10)
    } else {
      3
    };
    for _ in 0..num {
      let chunk_size = TestUtil::next_usize(random, 20, 100);
      self.assert_chunking(random, chunk_size)?;
    }
    Ok(())
  }

  fn assert_chunking<R>(&self, random: &mut R, chunk_size: usize) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = create_temp_dir_with_prefix(format!("mmap{chunk_size}"))?;
    let chunked_dir =
      self.get_directory_with_max_chunk_size(temp_dir.path().to_path_buf(), chunk_size)?;
    let dir = Arc::new(chunked_dir);
    let analyzer = MockAnalyzer::new(random);
    let mut iwc = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwc.set_merge_policy(new_log_merge_policy(random)?);
    let writer = RandomIndexWriter::with_config(random, dir.clone(), iwc);

    let mut field_to_type = HashMap::new();

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "docid",
      "0",
      Store::Yes,
      &mut field_to_type,
    )?);
    doc.add(new_string_field(
      random,
      "junk",
      "",
      Store::Yes,
      &mut field_to_type,
    )?);
    let num_docs = 100;
    for i in 0..num_docs {
      let Some(Fields::Field(docid)) = doc.get_field_mut("docid") else {
        unreachable!("docid field must exist")
      };
      docid.set_string_value(i.to_string())?;

      let Some(Fields::Field(junk)) = doc.get_field_mut("junk") else {
        unreachable!("junk field must exist")
      };
      junk.set_string_value(TestUtil::random_unicode_string(random))?;
      writer.add_document(random, &mut doc)?;
    }
    let reader = writer.get_reader(random)?;
    writer.close(random)?;

    let mut stored_fields = reader.stored_fields()?;
    let num_asserts = at_least(random, 100);
    for _ in 0..num_asserts {
      let doc_id = random.random_range(0..num_docs);
      let stored_doc = stored_fields.document(doc_id)?;
      let actual = stored_doc.get("docid")?;
      let expected = doc_id.to_string();
      assert_eq!(Some(expected.as_str()), actual.as_deref());
    }
    reader.close()?;
    dir.close()?;
    Ok(())
  }

  fn test_bytes_cross_boundary<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num = if is_night_mode() {
      TestUtil::next_usize(random, 100, 1000)
    } else {
      TestUtil::next_usize(random, 50, 100)
    };
    let mut bytes = vec![0_u8; num];
    random.fill(&mut bytes[..]);
    let temp_dir = create_temp_dir_with_prefix("testBytesCrossBoundary")?;
    let dir = self.get_directory_with_max_chunk_size(temp_dir.path().to_path_buf(), 16)?;
    let io_context = new_io_context(random)?;
    {
      let mut out = dir.create_output("bytesCrossBoundary", &io_context)?;
      out.write_bytes_with_len(&bytes, bytes.len())?;
      out.close()?;
    }

    let mut input = dir.open_input("bytesCrossBoundary", &new_io_context(random)?)?;
    let input_len = IndexInput::length(&input)?;
    let mut slice = input.random_access_slice(0, input_len)?;
    assert_eq!(input_len, RandomAccessInput::length(&slice)?);
    Self::assert_bytes(&mut slice, &bytes, 0, random)?;

    // subslices
    for offset in 1..bytes.len() {
      let mut subslice = input.random_access_slice(offset, input_len - offset)?;
      assert_eq!(input_len - offset, RandomAccessInput::length(&subslice)?);
      Self::assert_bytes(&mut subslice, &bytes, offset, random)?;
    }

    // with padding
    for i in 1..7 {
      let name = format!("bytes-{i}");
      {
        let mut out = dir.create_output(&name, &new_io_context(random)?)?;
        let mut junk = vec![0_u8; i];
        random.fill(&mut junk[..]);
        out.write_bytes_with_len(&junk, junk.len())?;
        input.seek(0)?;
        out.copy_bytes(&mut input, input_len)?;
        Closeable::close(&mut out)?;
      }
      let padded = dir.open_input(&name, &new_io_context(random)?)?;
      let padded_len = IndexInput::length(&padded)?;
      let mut whole = padded.random_access_slice(i, padded_len - i)?;
      assert_eq!(padded_len - i, RandomAccessInput::length(&whole)?);
      Self::assert_bytes(&mut whole, &bytes, 0, random)?;
      CloseableRef::close(&padded)?;
    }

    CloseableRef::close(&input)?;
    dir.close()?;
    Ok(())
  }

  fn test_little_endian_longs_cross_boundary<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = create_temp_dir_with_prefix("testLittleEndianLongsCrossBoundary")?;
    let dir = self.get_directory_with_max_chunk_size(temp_dir.path().to_path_buf(), 16)?;
    {
      let mut out = dir.create_output("littleEndianLongs", &new_io_context(random)?)?;
      out.write_byte(2)?;
      out.write_long(3)?;
      out.write_long(i64::MAX)?;
      out.write_long(-3)?;
      out.close()?;
    }

    let mut input = dir.open_input("littleEndianLongs", &new_io_context(random)?)?;
    assert_eq!(25, IndexInput::length(&input)?);
    assert_eq!(2_u8, DataInput::read_byte(&mut input)?);
    let mut l = [0_i64; 4];
    input.read_longs(&mut l, 1, 3)?;
    assert_eq!([0, 3, i64::MAX, -3], l);
    assert_eq!(25, input.get_file_pointer()?);
    CloseableRef::close(&input)?;
    dir.close()?;
    Ok(())
  }

  fn test_little_endian_floats_cross_boundary<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = create_temp_dir_with_prefix("testFloatsCrossBoundary")?;
    let dir = self.get_directory_with_max_chunk_size(temp_dir.path().to_path_buf(), 8)?;
    {
      let mut out = dir.create_output("Floats", &new_io_context(random)?)?;
      out.write_byte(2)?;
      out.write_int(3.0f32.to_bits() as i32)?;
      out.write_int(f32::MAX.to_bits() as i32)?;
      out.write_int((-3.0f32).to_bits() as i32)?;
      out.close()?;
    }

    let mut input = dir.open_input("Floats", &new_io_context(random)?)?;
    assert_eq!(13, IndexInput::length(&input)?);
    assert_eq!(2_u8, DataInput::read_byte(&mut input)?);
    let mut ff = [0.0_f32; 4];
    input.read_floats(&mut ff, 1, 3)?;
    assert!(array_equals_f32(&[0.0, 3.0, f32::MAX, -3.0], &ff, 0.0));
    assert_eq!(13, input.get_file_pointer()?);
    CloseableRef::close(&input)?;
    dir.close()?;
    Ok(())
  }
}
