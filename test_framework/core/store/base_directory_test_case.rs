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
  at_least, at_least_usize, is_night_mode, new_directory, new_io_context, random_from_seed,
  slow_file_exists,
};
use std::collections::{HashMap, HashSet};
use std::io::{Error, ErrorKind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;
use strum::EnumCount;

use parking_lot::Mutex;
use rand::Rng;
use rand::RngExt;
use tempfile::Builder;

use crate::core::index::{IndexFileNames, directory_reader};
use crate::core::store::DataInput;
use crate::core::store::IndexInput;
use crate::core::store::IndexOutput;
use crate::core::store::check_sum_index_input::ChecksumIndexInput;
use crate::core::store::directory::Directory;
use crate::core::store::random_access_input::RandomAccessInput;
use crate::core::store::{DataOutput, ReadAdvice, write_group_vints_i64};
use crate::core::util::clone::TryClone as OtherClone;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind, Result};
use crate::core::util::group_vint_util::GroupVIntUtil;
use crate::core::util::io_utils::IOUtils;
use crate::core::util::packed::PackedInts;
use crate::test_framework::array_equals_f32;
use crate::test_framework::core::util::test_util::TestUtil;

pub const EXTRA_FILE_NAME: &str = "extra0";
pub trait BaseDirectoryTestCase {
  type Directory: Directory<IndexInput = Self::Output> + Send + Sync + 'static;
  type Output: IndexInput<IndexInput = Self::Output> + Send + Sync + 'static;
  fn get_directory<R>(&self, path: PathBuf, random: &mut R) -> Result<Self::Directory>
  where
    R: Rng + ?Sized;

  fn configure_is_loaded_test(&self, _dir: &mut Self::Directory) -> bool {
    false
  }

  fn test_copy_from<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let mut temp_dir = Builder::new().prefix("testCopy").tempdir()?;
    let source = self.get_directory(temp_dir.keep(), random)?;
    let dest = new_directory(random)?;
    Self::run_copy_from(&source, &dest, random)?;
    dest.close()?;
    source.close()?;

    let source = new_directory(random)?;
    temp_dir = Builder::new().prefix("testCopyDestination").tempdir()?;
    let dest = self.get_directory(temp_dir.keep(), random)?;
    Self::run_copy_from(&source, &dest, random)?;
    dest.close()?;
    source.close()?;
    Ok(())
  }

  fn run_copy_from<R, D, D2>(source: &D, dest: &D2, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
    D: Directory,
    D2: Directory,
  {
    let mut bytes = vec![0u8; 20000];
    let io_context = new_io_context(random)?;
    random.fill(&mut bytes[..]);
    {
      let mut output = source.create_output("foobar", &io_context)?;

      output.write_bytes_with_len(&bytes, bytes.len())?;
      output.close()?;
    }
    dest.copy_from(source, "foobar", "foobaz", &io_context)?;
    assert!(slow_file_exists(dest, "foobaz")?);
    let bytes2_len = bytes.len();
    let mut bytes2 = vec![0u8; bytes2_len];
    {
      let mut input = dest.open_input("foobaz", &io_context)?;
      DataInput::read_bytes(&mut input, &mut bytes2, 0, bytes2_len)?;
      CloseableRef::close(&input)?;
    }

    assert_eq!(bytes, bytes2);

    Ok(())
  }
  fn test_rename<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("testRename").tempdir()?;
    let dir = self.get_directory(temp_dir.keep(), random)?;
    let num_bytes = random.random_range(0..20000);
    let mut bytes = vec![0u8; num_bytes];
    let io_context = new_io_context(random)?;
    random.fill(&mut bytes[..]);
    {
      let mut output = dir.create_output("foobar", &io_context)?;
      output.write_bytes_with_len(&bytes, bytes.len())?;
      output.close()?;
    }

    dir.rename("foobar", "foobaz")?;

    let mut bytes2 = vec![0u8; num_bytes];
    {
      let mut input = dir.open_input("foobaz", &io_context)?;
      DataInput::read_bytes(&mut input, &mut bytes2, 0, num_bytes)?;
      assert_eq!(IndexInput::length(&input)?, num_bytes);
      CloseableRef::close(&input)?;
    }

    assert_eq!(bytes, bytes2);

    dir.close()
  }

  fn test_delete_file<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("testDeleteFile").tempdir()?;
    let dir = self.get_directory(temp_dir.keep(), random)?;

    let file = "foo.txt";
    let io_context = IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?;

    assert!(!dir.list_all()?.iter().any(|name| name == file));

    let mut output = dir.create_output(file, io_context)?;
    output.close()?;
    assert!(dir.list_all()?.iter().any(|name| name == file));

    dir.delete_file(file)?;
    assert!(!dir.list_all()?.iter().any(|name| name == file));

    let result = dir.delete_file(file);
    assert!(result.is_err());
    assert!(
      matches!(result.as_ref().map_err(LuceneError::kind), Err(LuceneErrorKind::IoWithPath {
            source,
            path,
            ..
        }) if source.kind() == ErrorKind::NotFound && path.contains(file))
    );
    dir.close()
  }
  fn test_byte<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("testByte").tempdir()?;
    let io_context = new_io_context(random)?;

    let dir = self.get_directory(temp_dir.keep(), random)?;

    {
      let mut output = dir.create_output("byte", &io_context)?;
      output.write_byte(128)?;
      output.close()?;
    }

    {
      let mut input = dir.open_input("byte", &io_context)?;
      assert_eq!(1, IndexInput::length(&input)?);
      assert_eq!(128u8, DataInput::read_byte(&mut input)?);
      CloseableRef::close(&input)?;
    }

    dir.close()
  }
  fn test_short<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("testShort").tempdir()?;
    let dir = self.get_directory(temp_dir.keep(), random)?;
    let io_context = new_io_context(random)?;

    {
      let mut output = dir.create_output("short", &io_context)?;
      output.write_short(-20)?;
      output.close()?;
    }

    {
      let mut input = dir.open_input("short", &io_context)?;
      assert_eq!(2, IndexInput::length(&input)?);
      assert_eq!(-20i16, DataInput::read_short(&mut input)?);
      CloseableRef::close(&input)?;
    }

    dir.close()
  }
  fn test_int<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("testInt").tempdir()?;
    let dir = self.get_directory(temp_dir.keep(), random)?;
    let io_context = new_io_context(random)?;

    {
      let mut output = dir.create_output("int", &io_context)?;
      output.write_int(-500)?;
      output.close()?;
    }

    {
      let mut input = dir.open_input("int", &io_context)?;
      assert_eq!(4, IndexInput::length(&input)?);
      assert_eq!(-500, DataInput::read_int(&mut input)?);
      CloseableRef::close(&input)?;
    }

    dir.close()
  }
  fn test_long<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("testLong").tempdir()?;
    let dir = self.get_directory(temp_dir.keep(), random)?;
    let io_context = new_io_context(random)?;

    {
      let mut output = dir.create_output("long", &io_context)?;
      output.write_long(-5000)?;
      output.close()?;
    }

    {
      let mut input = dir.open_input("long", &io_context)?;
      assert_eq!(8, IndexInput::length(&input)?);
      assert_eq!(-5000, DataInput::read_long(&mut input)?);
      CloseableRef::close(&input)?;
    }

    dir.close()
  }
  fn test_aligned_little_endian_longs<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new()
      .prefix("testAlignedLittleEndianLongs")
      .tempdir()?;
    let dir = self.get_directory(temp_dir.keep(), random)?;
    let io_context = new_io_context(random)?;

    {
      let mut out = dir.create_output("littleEndianLongs", &io_context)?;
      out.write_long(3)?;
      out.write_long(i64::MAX)?;
      out.write_long(-3)?;
      out.close()?;
    }

    {
      let mut input = dir.open_input("littleEndianLongs", &io_context)?;
      assert_eq!(24, IndexInput::length(&input)?);

      let mut l = [0; 4];
      input.read_longs(&mut l, 1, 3)?;

      assert_eq!([0, 3, i64::MAX, -3], l);
      assert_eq!(24, input.get_file_pointer()?);
      CloseableRef::close(&input)?;
    }

    dir.close()
  }
  fn test_unaligned_little_endian_longs<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new()
      .prefix("testUnalignedLittleEndianLongs")
      .tempdir()?;
    let dir = self.get_directory(temp_dir.keep(), random)?;
    let io_context = new_io_context(random)?;

    {
      let mut out = dir.create_output("littleEndianLongs", &io_context)?;
      out.write_byte(2)?;
      out.write_long(3)?;
      out.write_long(i64::MAX)?;
      out.write_long(-3)?;
      out.close()?;
    }

    {
      let mut input = dir.open_input("littleEndianLongs", &io_context)?;
      assert_eq!(25, IndexInput::length(&input)?);
      assert_eq!(2u8, DataInput::read_byte(&mut input)?);
      let mut longs = [0; 4];
      input.read_longs(&mut longs, 1, 3)?;
      assert_eq!([0, 3, i64::MAX, -3], longs);
      assert_eq!(25, input.get_file_pointer()?);
      CloseableRef::close(&input)?;
    }

    dir.close()
  }
  fn test_little_endian_longs_underflow<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new()
      .prefix("testLittleEndianLongsUnderflow")
      .tempdir()?;
    let dir = self.get_directory(temp_dir.keep(), random)?;
    let io_context = new_io_context(random)?;

    let offset = random.random_range(0..8);
    let length = TestUtil::next_usize(random, 1, 16);
    let padding = offset + length * size_of::<i64>() - random.random_range(1..=size_of::<i64>());

    {
      let mut out = dir.create_output("littleEndianLongs", &io_context)?;
      let mut bytes = [0u8; 134];
      let bytes = &mut bytes[..padding];
      random.fill(&mut bytes[..]);
      out.write_bytes_with_len(bytes, bytes.len())?;
      out.close()?;
    }

    {
      let mut input = dir.open_input("littleEndianLongs", &io_context)?;
      input.seek(offset)?;

      let result = input.read_longs(&mut [0i64; 16][..length], 0, length);
      assert!(matches!(
        result.as_ref().map_err(LuceneError::kind),
        Err(LuceneErrorKind::Eof(_))
      ));
      CloseableRef::close(&input)?;
    }

    dir.close()
  }
  fn test_aligned_ints<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("testAlignedInts").tempdir()?;
    let dir = self.get_directory(temp_dir.keep(), random)?;
    let io_context = new_io_context(random)?;

    {
      let mut out = dir.create_output("Ints", &io_context)?;
      out.write_int(3)?;
      out.write_int(i32::MAX)?;
      out.write_int(-3)?;
      out.close()?;
    }

    {
      let mut input = dir.open_input("Ints", &io_context)?;
      assert_eq!(12, IndexInput::length(&input)?);
      let mut ints = [0; 4];
      input.read_ints(&mut ints, 1, 3)?;
      assert_eq!([0, 3, i32::MAX, -3], ints);
      assert_eq!(12, input.get_file_pointer()?);
      CloseableRef::close(&input)?;
    }

    dir.close()
  }
  fn test_unaligned_ints<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("testUnalignedInts").tempdir()?;
    let dir = self.get_directory(temp_dir.keep(), random)?;
    let padding = random.random_range(1..=3);
    let io_context = new_io_context(random)?;

    {
      let mut out = dir.create_output("Ints", &io_context)?;
      for _ in 0..padding {
        out.write_byte(2)?;
      }
      out.write_int(3)?;
      out.write_int(i32::MAX)?;
      out.write_int(-3)?;
      out.close()?;
    }

    {
      let mut input = dir.open_input("Ints", &io_context)?;
      assert_eq!(12 + padding, IndexInput::length(&input)?);
      for _ in 0..padding {
        assert_eq!(2u8, DataInput::read_byte(&mut input)?);
      }
      let mut ints = [0; 4];
      input.read_ints(&mut ints, 1, 3)?;
      assert_eq!([0, 3, i32::MAX, -3], ints);
      assert_eq!(12 + padding, input.get_file_pointer()?);
      CloseableRef::close(&input)?;
    }

    dir.close()
  }
  fn test_ints_underflow<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("testIntsUnderflow").tempdir()?;
    let dir = self.get_directory(temp_dir.keep(), random)?;
    let io_context = new_io_context(random)?;

    let offset = random.random_range(0..4);
    let length = TestUtil::next_usize(random, 1, 16);

    let total_size = offset + length * size_of::<i32>();
    let truncation = random.random_range(1..=size_of::<i32>());
    let data_size = total_size - truncation;
    let mut bytes = [0u8; 66];
    let bytes = &mut bytes[..data_size];
    random.fill(&mut bytes[..]);

    {
      let mut output = dir.create_output("Ints", &io_context)?;
      output.write_bytes_with_len(bytes, bytes.len())?;
      output.close()?;
    }

    let mut input = dir.open_input("Ints", &io_context)?;
    input.seek(offset)?;

    let mut ints = [0; 16];
    let ints = &mut ints[..length];
    let result = input.read_ints(ints, 0, length);
    assert!(matches!(
      result.as_ref().map_err(LuceneError::kind),
      Err(LuceneErrorKind::Eof(_))
    ));
    CloseableRef::close(&input)?;

    dir.close()
  }

  fn test_aligned_floats<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("testAlignedFloats").tempdir()?;
    let dir = self.get_directory(temp_dir.keep(), random)?;
    let io_context = IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?;
    {
      let mut out = dir.create_output("Floats", io_context)?;
      out.write_int(3f32.to_bits() as i32)?;
      out.write_int(f32::MAX.to_bits() as i32)?;
      out.write_int((-3f32).to_bits() as i32)?;
      out.close()?;
    }

    {
      let mut input = dir.open_input("Floats", io_context)?;
      assert_eq!(12, IndexInput::length(&input)?);
      let mut floats = [0.0f32; 4];
      input.read_floats(&mut floats, 1, 3)?;
      assert!(array_equals_f32(&[0.0, 3.0, f32::MAX, -3.0], &floats, 0.0));
      assert_eq!(12, input.get_file_pointer()?);
      CloseableRef::close(&input)?;
    }

    dir.close()
  }
  fn test_unaligned_floats<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let padding = random.random_range(1..=3);
    let io_context = new_io_context(random)?;

    let temp_dir = Builder::new().prefix("testUnalignedFloats").tempdir()?;
    let dir = self.get_directory(temp_dir.keep(), random)?;

    {
      let mut output = dir.create_output("Floats", &io_context)?;
      for _ in 0..padding {
        output.write_byte(2)?;
      }
      output.write_int(3f32.to_bits() as i32)?;
      output.write_int(f32::MAX.to_bits() as i32)?;
      output.write_int((-3f32).to_bits() as i32)?;
      output.close()?;
    }

    let mut input = dir.open_input("Floats", &io_context)?;
    assert_eq!(12 + padding, IndexInput::length(&input)?);
    for _ in 0..padding {
      assert_eq!(2u8, DataInput::read_byte(&mut input)?);
    }

    let mut ff = [0f32; 4];
    input.read_floats(&mut ff, 1, 3)?;
    assert!(array_equals_f32(&[0.0, 3.0, f32::MAX, -3.0], &ff, 0.0));
    assert_eq!(12 + padding, input.get_file_pointer()?);
    CloseableRef::close(&input)?;

    dir.close()
  }
  fn test_floats_underflow<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("testFloatsUnderflow").tempdir()?;
    let dir = self.get_directory(temp_dir.keep(), random)?;
    let io_context = new_io_context(random)?;

    let offset = random.random_range(0..4);
    let length = TestUtil::next_usize(random, 1, 16);
    {
      let size = offset + length * size_of::<f32>() - random.random_range(1..=size_of::<f32>());
      let mut b = [0u8; 66];
      let b = &mut b[..size];
      random.fill(&mut b[..]);

      let mut output = dir.create_output("Floats", &io_context)?;
      output.write_bytes_with_len(b, b.len())?;
      output.close()?;
    }

    let mut input = dir.open_input("Floats", &io_context)?;
    input.seek(offset)?;
    let result = input.read_floats(&mut [0.0; 16][..length], 0, length);
    assert!(matches!(
      result.as_ref().map_err(LuceneError::kind),
      Err(LuceneErrorKind::Eof(_))
    ));
    CloseableRef::close(&input)?;

    dir.close()
  }
  fn test_string<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("testString").tempdir()?;
    let dir = self.get_directory(temp_dir.keep(), random)?;
    let io_context = new_io_context(random)?;

    {
      let mut output = dir.create_output("string", &io_context)?;
      output.write_string("hello!")?;
      output.close()?;
    }

    {
      let mut input = dir.open_input("string", &io_context)?;
      assert_eq!("hello!", input.read_string()?);
      assert_eq!(7, IndexInput::length(&input)?);
      CloseableRef::close(&input)?;
    }

    dir.close()
  }
  fn test_vint<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("testVInt").tempdir()?;
    let dir = self.get_directory(temp_dir.keep(), random)?;
    let io_context = new_io_context(random)?;

    {
      let mut output = dir.create_output("vint", &io_context)?;
      output.write_vint(500)?;
      output.close()?;
    }

    {
      let mut input = dir.open_input("vint", &io_context)?;
      assert_eq!(2, IndexInput::length(&input)?);
      assert_eq!(500, input.read_vint()?);
      CloseableRef::close(&input)?;
    }

    dir.close()
  }
  fn test_vlong<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("testVLong").tempdir()?;
    let dir = self.get_directory(temp_dir.keep(), random)?;
    let io_context = new_io_context(random)?;

    {
      let mut output = dir.create_output("vlong", &io_context)?;
      output.write_vlong(i64::MAX)?;
      output.close()?;
    }

    {
      let mut input = dir.open_input("vlong", &io_context)?;
      assert_eq!(9, IndexInput::length(&input)?);
      assert_eq!(i64::MAX, input.read_vlong()?);
      CloseableRef::close(&input)?;
    }

    dir.close()
  }
  fn test_zint<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let mut ints = Vec::new();
    let num_ints = random.random_range(0..10);

    for _ in 0..num_ints {
      let value = match random.random_range(0..3) {
        0 => random.random::<i32>(),
        1 => {
          if random.random_bool(0.5) {
            i32::MIN
          } else {
            i32::MAX
          }
        },
        2 => {
          let sign = if random.random_bool(0.5) { -1 } else { 1 };
          sign * random.random_range(0..1024)
        },
        _ => unreachable!(),
      };
      ints.push(value);
    }

    let temp_dir = Builder::new().prefix("testZInt").tempdir()?;
    let dir = self.get_directory(temp_dir.keep(), random)?;
    let io_context = new_io_context(random)?;

    {
      let mut output = dir.create_output("zint", &io_context)?;
      for &i in &ints {
        output.write_zint(i)?;
      }
      output.close()?;
    }

    {
      let mut input = dir.open_input("zint", &io_context)?;
      for &i in &ints {
        assert_eq!(i, input.read_zint()?);
      }
      assert_eq!(IndexInput::length(&input)?, input.get_file_pointer()?);
      CloseableRef::close(&input)?;
    }

    dir.close()
  }

  fn test_zlong<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let mut longs = Vec::new();
    let num_longs = random.random_range(0..10);
    let io_context = new_io_context(random)?;

    for _ in 0..num_longs {
      let value = match random.random_range(0..3) {
        0 => random.random::<i64>(), // Random 64-bit integer
        1 => {
          if random.random_bool(0.5) {
            i64::MIN // Minimum value for i64
          } else {
            i64::MAX // Maximum value for i64
          }
        },
        2 => {
          let sign = if random.random_bool(0.5) { -1 } else { 1 };
          sign * random.random_range(0..1024) as i64 // Small range value with random sign
        },
        _ => unreachable!(),
      };
      longs.push(value);
    }

    let temp_dir = Builder::new().prefix("testZLong").tempdir()?;
    let dir = self.get_directory(temp_dir.keep(), random)?;

    {
      let mut output = dir.create_output("zlong", &io_context)?;
      for &l in &longs {
        output.write_zlong(l)?;
      }
      output.close()?;
    }

    {
      let mut input = dir.open_input("zlong", &io_context)?;
      for &l in &longs {
        assert_eq!(l, input.read_zlong()?);
      }
      assert_eq!(IndexInput::length(&input)?, input.get_file_pointer()?);
      CloseableRef::close(&input)?;
    }

    dir.close()
  }
  fn test_set_of_strings<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("testSetOfStrings").tempdir()?;
    let dir = self.get_directory(temp_dir.keep(), random)?;
    let io_context = new_io_context(random)?;

    {
      let mut output = dir.create_output("stringset", &io_context)?;
      output.write_set_of_strings(
        &["test1".to_string(), "test2".to_string()]
          .into_iter()
          .collect(),
      )?;
      output.write_set_of_strings(&HashSet::new())?;
      output.write_set_of_strings(&["test3".to_string()].into_iter().collect())?;
      output.close()?;
    }

    {
      let mut input = dir.open_input("stringset", &io_context)?;

      let set1 = input.read_set_of_strings()?;
      assert!(
        set1.len() == 2 && set1.contains("test1") && set1.contains("test2"),
        "actual set: {set1:?}"
      );

      let set2 = input.read_set_of_strings()?;
      assert_eq!(set2, HashSet::new());

      let set3 = input.read_set_of_strings()?;
      assert!(
        set3.len() == 1 && set3.contains("test3"),
        "actual set: {set3:?}"
      );

      assert_eq!(IndexInput::length(&input)?, input.get_file_pointer()?);
      CloseableRef::close(&input)?;
    }

    dir.close()
  }

  fn test_map_of_strings<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let mut map = HashMap::new();
    map.insert("test1".to_string(), "value1".to_string());
    map.insert("test2".to_string(), "value2".to_string());

    let temp_dir = Builder::new().prefix("testMapOfStrings").tempdir()?;
    let dir = self.get_directory(temp_dir.keep(), random)?;
    let io_context = new_io_context(random)?;

    {
      let mut output = dir.create_output("stringmap", &io_context)?;
      output.write_map_of_strings(&map)?;
      output.write_map_of_strings(&HashMap::new())?;
      let singleton_map: HashMap<String, String> = [(String::from("key"), String::from("value"))]
        .into_iter()
        .collect();
      output.write_map_of_strings(&singleton_map)?;
      output.close()?;
    }

    {
      let mut input = dir.open_input("stringmap", &io_context)?;

      let map1 = input.read_map_of_strings()?;
      assert_eq!(map1, map);

      // Attempt to mutate the map to ensure it's immutable in context
      let mut map1_clone = map1.clone(); // Rust enforces immutability by default
      map1_clone.insert("bogus1".to_string(), "bogus2".to_string()); // This will not affect the original `map1`

      let map2 = input.read_map_of_strings()?;
      assert!(map2.is_empty());

      let mut map2_clone = map2.clone();
      map2_clone.insert("bogus1".to_string(), "bogus2".to_string()); // This will not affect the original `map2`

      let map3 = input.read_map_of_strings()?;
      assert!(
        map3.len() == 1 && map3.get("key").map(String::as_str) == Some("value"),
        "actual map: {map3:?}"
      );

      let mut map3_clone = map3.clone();
      map3_clone.insert("bogus1".to_string(), "bogus2".to_string()); // This will not affect the original `map3`

      assert_eq!(IndexInput::length(&input)?, input.get_file_pointer()?);
      CloseableRef::close(&input)?;
    }

    dir.close()
  }
  fn test_checksum<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    use crc32fast::Hasher;

    let num_bytes = random.random_range(0..20000);
    let mut bytes = vec![0u8; num_bytes];
    random.fill(&mut bytes[..]);

    let mut hasher = Hasher::new();
    hasher.update(&bytes);
    let expected_checksum = hasher.finalize();

    let temp_dir = Builder::new().prefix("testChecksum").tempdir()?;
    let dir = self.get_directory(temp_dir.keep(), random)?;
    let io_context = new_io_context(random)?;

    {
      let mut output = dir.create_output("checksum", &io_context)?;
      output.write_bytes_range(&bytes, 0, bytes.len())?;
      output.close()?;
    }

    {
      let mut input = dir.open_checksum_input("checksum")?;
      IndexInput::skip_bytes(&mut input, num_bytes as i64)?;
      let actual_checksum = input.get_checksum();
      assert_eq!(expected_checksum as i64, actual_checksum);
      CloseableRef::close(&input)?;
    }

    dir.close()
  }

  /// Make sure the directory returns `AlreadyClosed` if an output is created after closing.
  fn test_detect_close<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("testDetectClose").tempdir()?;
    let dir = self.get_directory(temp_dir.path().to_path_buf(), random)?;
    CloseableRef::close(&dir)?;

    let result = dir.create_output("test", &new_io_context(random)?);
    assert!(matches!(
      result.as_ref().map_err(LuceneError::kind),
      Err(LuceneErrorKind::AlreadyClosed(_))
    ));
    Ok(())
  }
  fn test_thread_safety_in_list_all<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("testThreadSafety").tempdir()?;
    let dir = Arc::new(Mutex::new(
      self.get_directory(temp_dir.path().to_path_buf(), random)?,
    ));

    let stop = Arc::new(AtomicBool::new(false));

    // Writer thread
    let dir_writer = Arc::clone(&dir);
    let stop_writer = Arc::clone(&stop);
    let seed: u64 = random.random();
    let writer = thread::spawn(move || -> Result<()> {
      let mut rng = random_from_seed(seed);
      let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        let file_count = rng.random_range(500..=1000);
        for i in 0..file_count {
          let file_name = format!("file-{}", i);
          let dir = dir_writer.lock();
          let mut output = dir.create_output(&file_name, &new_io_context(&mut rng)?)?;
          thread::yield_now();
          output.close()?;
          assert!(slow_file_exists(&*dir, &file_name)?);
        }
        Ok(())
      }));
      stop_writer.store(true, Ordering::SeqCst);
      match result {
        Ok(result) => result,
        Err(payload) => std::panic::resume_unwind(payload),
      }
    });

    // Reader thread
    let dir_reader = Arc::clone(&dir);
    let stop_reader = Arc::clone(&stop);
    let reader_seed: u64 = random.random();
    let reader = thread::spawn(move || -> Result<()> {
      let mut rng = random_from_seed(reader_seed);

      while !stop_reader.load(Ordering::SeqCst) {
        let files: Vec<String> = {
          let dir = dir_reader.lock();
          dir
            .list_all()?
            .into_iter()
            .filter(|name| !name.eq(EXTRA_FILE_NAME))
            .collect()
        };

        if !files.is_empty() {
          loop {
            let file = files[rng.random_range(0..files.len())].as_str();
            match (dir_reader
              .lock()
              .open_input(file, &new_io_context(&mut rng)?))
            .map_err(LuceneError::into_kind)
            {
              Ok(input) => {
                thread::sleep(Duration::from_millis(1));
                CloseableRef::close(&input)?;
              },
              Err(LuceneErrorKind::IoWithPath { source, .. })
                if source.kind() == ErrorKind::PermissionDenied =>
              {
                // ignore
              },
              Err(e) => {
                return Err(
                  LuceneErrorKind::IoWithPath {
                    path: file.to_string(),
                    source: Error::other(format!("{:?}", e)),
                    err_kind: e.to_string(),
                    suppressed: None,
                  }
                  .into(),
                );
              },
            }
            if rng.random_range(0..3) == 0 {
              break;
            }
          }
        }
      }
      Ok(())
    });

    match writer.join() {
      Ok(Ok(())) => (),
      Ok(Err(e)) => {
        eprintln!("Writer thread error: {:?}", e);
        return Err(e);
      },
      Err(_) => {
        eprintln!("Writer thread panicked!");
        unreachable!()
      },
    }

    match reader.join() {
      Ok(Ok(())) => (),
      Ok(Err(e)) => {
        eprintln!("Reader thread error: {:?}", e);
        return Err(e);
      },
      Err(_) => {
        eprintln!("Reader thread panicked!");
        unreachable!()
      },
    }

    dir.lock().close()
  }

  fn test_file_exists_in_list_after_created<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new()
      .prefix("testFileExistsInListAfterCreated")
      .tempdir()?;
    let dir = self.get_directory(temp_dir.path().to_path_buf(), random)?;

    let name = "file";
    let io_context = new_io_context(random)?;

    {
      let mut output = dir.create_output(name, &io_context)?;
      output.close()?;
    }

    assert!(
      slow_file_exists(&dir, name)?,
      "File '{}' should exist after creation.",
      name
    );

    let files: HashSet<String> = dir.list_all()?.into_iter().collect();
    assert!(
      files.contains(name),
      "File '{}' should be present in the directory listing.",
      name
    );

    dir.close()
  }

  fn test_seek_to_eof_then_back<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("testSeekToEOFThenBack").tempdir()?;
    let dir = self.get_directory(temp_dir.path().to_path_buf(), random)?;

    let buffer_length = 1024;
    let total_length = 3 * buffer_length;
    let bytes = vec![0u8; total_length];
    let io_context = new_io_context(random)?;

    {
      let mut output = dir.create_output("out", &io_context)?;
      output.write_bytes_range(&bytes, 0, total_length)?;
      output.close()?;
    }

    {
      let mut input = dir.open_input("out", &io_context)?;
      input.seek(2 * buffer_length - 1)?;
      input.seek(3 * buffer_length)?;
      input.seek(buffer_length)?;

      let mut read_bytes = vec![0u8; 2 * buffer_length];
      DataInput::read_bytes(&mut input, &mut read_bytes, 0, 2 * buffer_length)?;
      assert_eq!(&read_bytes, &bytes[buffer_length..3 * buffer_length]);
      CloseableRef::close(&input)?;
    }

    dir.close()
  }

  fn test_illegal_eof<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("testIllegalEOF").tempdir()?;
    let dir = self.get_directory(temp_dir.path().to_path_buf(), random)?;
    let io_context = new_io_context(random)?;

    let buffer = vec![0u8; 1024];
    {
      let mut output = dir.create_output("out", &io_context)?;
      output.write_bytes_range(&buffer, 0, buffer.len())?;
      output.close()?;
    }

    {
      let mut input = dir.open_input("out", &io_context)?;
      input.seek(1024)?;
      CloseableRef::close(&input)?;
    }

    dir.close()
  }
  fn test_seek_past_eof<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("testSeekPastEOF").tempdir()?;
    let dir = self.get_directory(temp_dir.path().to_path_buf(), random)?;
    let io_context = new_io_context(random)?;

    let len = random.random_range(0..2048);
    let buffer = vec![0u8; len];
    {
      let mut output = dir.create_output("out", &io_context)?;
      output.write_bytes_range(&buffer, 0, len)?;
      output.close()?;
    }

    let mut input = dir.open_input("out", &io_context)?;

    // Seeking past EOF should always return an error
    assert!(matches!(
      (input.seek(len + random.random_range(1..2048)))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::Eof(_))
    ));

    input.seek(len)?;

    assert!(matches!(
      (DataInput::read_byte(&mut input))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::Eof(_))
    ));
    assert!(matches!(
      (DataInput::read_bytes(&mut input, &mut [0u8; 1], 0, 1))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::Eof(_))
    ));
    CloseableRef::close(&input)?;

    dir.close()
  }
  fn test_slice_out_of_bounds<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("testSliceOutOfBounds").tempdir()?;
    let dir = self.get_directory(temp_dir.path().to_path_buf(), random)?;
    let io_context = new_io_context(random)?;

    let len = random.random_range(8..2048);
    let buffer = vec![0u8; len];
    {
      let mut output = dir.create_output("out", &io_context)?;
      output.write_bytes_range(&buffer, 0, len)?;
      output.close()?;
    }

    let input = dir.open_input("out", &io_context)?;

    assert!(matches!(
      (input.slice("slice1", 0, len + 1))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::IllegalArgument(_))
    ));

    let slice = input.slice("slice3", 4, len / 2)?;

    // Attempting to create a nested slice that goes out of bounds
    assert!(matches!(
      (slice.slice("slice3sub", 1, len / 2))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::IllegalArgument(_))
    ));
    CloseableRef::close(&slice)?;
    CloseableRef::close(&input)?;

    dir.close()
  }

  fn test_no_dir<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("doesnotexist").tempdir()?;
    let path = temp_dir.path().to_path_buf();
    temp_dir.close()?;
    let dir = Arc::new(self.get_directory(path, random)?);
    match (directory_reader::open(dir.clone())).map_err(LuceneError::into_kind) {
      Ok(_) => panic!("expected IndexNotFound or NoSuchFile"),
      Err(LuceneErrorKind::IndexNotFound(_)) | Err(LuceneErrorKind::NoSuchFile(_)) => {},
      Err(LuceneErrorKind::IoWithPath { source, .. }) | Err(LuceneErrorKind::Io { source, .. })
        if source.kind() == ErrorKind::NotFound => {},
      Err(err) => return Err(err.into()),
    }
    dir.close()
  }
  fn test_copy_bytes<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("testCopyBytes").tempdir()?;
    let dir = self.get_directory(temp_dir.path().to_path_buf(), random)?;
    let io_context = new_io_context(random)?;

    let bytes_len = TestUtil::next_usize(random, 1, 77777);
    let mut bytes = vec![0u8; bytes_len];

    let size = TestUtil::next_usize(random, 1, 1777777);
    let mut upto = 0;
    let mut byte_upto = 0;
    {
      let mut output = dir.create_output("test", &io_context)?;

      while upto < size {
        bytes[byte_upto] = Self::value(upto);
        byte_upto += 1;
        upto += 1;

        if byte_upto == bytes.len() {
          output.write_bytes_range(&bytes, 0, bytes.len())?;
          byte_upto = 0;
        }
      }

      output.write_bytes_range(&bytes, 0, byte_upto)?;
      assert_eq!(size, output.get_file_pointer()?);
      output.close()?;
    }
    assert_eq!(size, dir.file_length("test")?);

    {
      let mut input = dir.open_input("test", &io_context)?;
      let mut output = dir.create_output("test2", &io_context)?;

      upto = 0;
      while upto < size {
        if random.random_bool(0.5) {
          output.write_byte(DataInput::read_byte(&mut input)?)?;
          upto += 1;
        } else {
          let chunk = std::cmp::min(TestUtil::next_usize(random, 1, bytes.len()), size - upto);
          output.copy_bytes(&mut input, chunk)?;
          upto += chunk;
        }
      }
      assert_eq!(size, upto);
      output.close()?;
      CloseableRef::close(&input)?;
    }

    {
      let mut input2 = dir.open_input("test2", &io_context)?;
      upto = 0;
      while upto < size {
        if random.random_bool(0.5) {
          let v = DataInput::read_byte(&mut input2)?;
          assert_eq!(Self::value(upto), v);
          upto += 1;
        } else {
          let limit = std::cmp::min(TestUtil::next_usize(random, 1, bytes.len()), size - upto);
          DataInput::read_bytes(&mut input2, &mut bytes, 0, limit)?;
          for &byte in bytes.iter().take(limit) {
            assert_eq!(Self::value(upto), byte);
            upto += 1;
          }
        }
      }
      CloseableRef::close(&input2)?;
    }

    dir.delete_file("test")?;
    dir.delete_file("test2")?;

    dir.close()
  }

  fn value(idx: usize) -> u8 {
    ((idx % 256) * (1 + (idx / 256))) as u8
  }

  fn test_copy_bytes_with_threads<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new()
      .prefix("testCopyBytesWithThreads")
      .tempdir()?;
    let dir = Arc::new(self.get_directory(temp_dir.path().to_path_buf(), random)?);
    let io_context = IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?;
    let header_len = 100;
    let data_len = random.random_range(header_len + 1..10000);
    let mut data = vec![0u8; data_len];
    random.fill_bytes(&mut data);

    let mut output = dir.create_output("data", io_context)?;
    output.write_bytes_with_len(&data, data_len)?;
    output.close()?;

    let mut input = dir.open_input("data", io_context)?;

    let mut output_header = dir.create_output("header", io_context)?;
    // Copy our header.
    output_header.copy_bytes(&mut input, header_len)?;
    output_header.close()?;

    let threads = 10;
    {
      let barrier = Arc::new(Barrier::new(threads));
      let mut handles = Vec::with_capacity(threads);

      for i in 0..threads {
        let dir_clone = Arc::clone(&dir);
        let mut src = input.try_clone()?;
        let barrier_clone = Arc::clone(&barrier);
        let io_context = IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?;
        let handle = thread::spawn(move || -> Result<()> {
          barrier_clone.wait();
          let file_name = format!("copy{}", i);
          let mut dst = dir_clone.create_output(&file_name, io_context)?;
          let src_length = IndexInput::length(&src).expect("source length");
          dst.copy_bytes(&mut src, src_length - header_len)?;
          dst.close()?;
          Ok(())
        });
        handles.push(handle);
      }

      for handle in handles {
        handle.join().unwrap()?;
      }
    }

    let mut data_copy = Vec::new();
    for i in 0..threads {
      let io_context = IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?;
      let file_name = format!("copy{}", i);
      data_copy.clear();
      data_copy.resize(data_len, 0u8);
      let mut input_copy = dir.open_input(&file_name, io_context)?;

      let copy_source = &data[..header_len];
      data_copy[..copy_source.len()].copy_from_slice(copy_source);

      DataInput::read_bytes(
        &mut input_copy,
        &mut data_copy[header_len..],
        0,
        data_len - header_len,
      )?;

      assert_eq!(data, data_copy, "Data mismatch in copy{}", i);
      CloseableRef::close(&input_copy)?;
    }
    input.close()?;

    dir.close()
  }
  fn test_fsync_doesnt_create_new_files<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("nocreate").tempdir()?;
    let path = temp_dir.path().to_path_buf();

    let fsdir = self.get_directory(path.clone(), random)?;

    // Ensure the directory is an FSDirectory implementation.
    if !fsdir.is_fs_directory() {
      // This test only applies to FSDirectory-like implementations
      return fsdir.close();
    }
    let io_context = new_io_context(random)?;

    {
      let mut out = fsdir.create_output("afile", &io_context)?;
      out.write_string("boo")?;
      out.close()?;
    }

    // Delete the file directly via the filesystem
    std::fs::remove_file(path.join("afile"))?;

    let file_count_before = fsdir.list_all()?.len();

    let sync_files = vec!["afile".to_string()];
    let result = fsdir.sync(&sync_files);
    assert!(
      matches!(result.as_ref().map_err(LuceneError::kind), Err(LuceneErrorKind::IoWithPath { source, .. })
            if source.kind() == ErrorKind::NotFound)
    );

    // Ensure no new files were created
    let file_count_after = fsdir.list_all()?.len();
    assert_eq!(file_count_before, file_count_after);

    fsdir.close()
  }
  fn test_random_long<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("testLongs").tempdir()?;
    let dir = self.get_directory(temp_dir.path().to_path_buf(), random)?;
    let io_context = new_io_context(random)?;

    let num = TestUtil::next_usize(random, 50, 3000);
    let mut longs = vec![0i64; num];
    {
      let mut output = dir.create_output("longs", &io_context)?;
      for value in &mut longs {
        *value = TestUtil::next_long(random, i64::MIN, i64::MAX);
        output.write_long(*value)?;
      }
      output.close()?;
    }

    // Slice
    {
      let mut input = dir.open_input("longs", &io_context)?;
      let length = IndexInput::length(&input)?;
      {
        let slice = input.random_access_slice(0, length)?;
        assert_eq!(length, RandomAccessInput::length(&slice)?);
        for (i, &expected) in longs.iter().enumerate() {
          assert_eq!(expected, RandomAccessInput::read_long(&slice, i * 8)?);
        }
      }

      // Subslices
      for i in 1..longs.len() {
        let offset = i * 8;
        let subslice = input.random_access_slice(offset, length - offset)?;
        assert_eq!(length - offset, RandomAccessInput::length(&subslice)?);
        for (j, &expected) in longs.iter().skip(i).enumerate() {
          assert_eq!(expected, RandomAccessInput::read_long(&subslice, j * 8)?);
        }
      }

      // With padding
      for i in 0..7 {
        let name = format!("longs-{}", i);
        {
          let mut o = dir.create_output(&name, &io_context)?;
          let mut junk = [0; 6];
          let junk = &mut junk[..i];
          for byte in junk.iter_mut() {
            *byte = random.random();
          }
          o.write_bytes_with_len(junk, junk.len())?;
          input.seek(0)?;
          let length = IndexInput::length(&input)?;
          o.copy_bytes(&mut input, length)?;
          o.close()?;
        }

        let padded = dir.open_input(&name, &io_context)?;
        let padded_length = IndexInput::length(&padded)?;
        let whole = padded.random_access_slice(i, padded_length - i)?;
        assert_eq!(padded_length - i, RandomAccessInput::length(&whole)?);
        for (j, &expected) in longs.iter().enumerate() {
          assert_eq!(expected, RandomAccessInput::read_long(&whole, j * 8)?);
        }
        CloseableRef::close(&padded)?;
      }
      CloseableRef::close(&input)?;
    }

    dir.close()
  }
  fn test_random_int<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("testInts").tempdir()?;
    let dir = self.get_directory(temp_dir.path().to_path_buf(), random)?;
    let io_context = new_io_context(random)?;

    let num = TestUtil::next_usize(random, 50, 3000);
    let mut ints = vec![0i32; num];
    {
      let mut output = dir.create_output("ints", &io_context)?;
      for value in &mut ints {
        *value = random.random_range(i32::MIN..=i32::MAX);
        output.write_int(*value)?;
      }
      output.close()?;
    }

    // Slice
    {
      let mut input = dir.open_input("ints", &io_context)?;
      let length = IndexInput::length(&input)?;
      {
        let slice = input.random_access_slice(0, length)?;
        assert_eq!(length, RandomAccessInput::length(&slice)?);
        for (i, &expected) in ints.iter().enumerate() {
          assert_eq!(expected, RandomAccessInput::read_int(&slice, i * 4)?);
        }
      }

      // Subslices
      for i in 1..ints.len() {
        let offset = i * 4;
        let subslice = input.random_access_slice(offset, length - offset)?;
        assert_eq!(length - offset, RandomAccessInput::length(&subslice)?);
        for (j, &expected) in ints.iter().skip(i).enumerate() {
          assert_eq!(expected, RandomAccessInput::read_int(&subslice, j * 4)?);
        }
      }

      // With padding
      for i in 0..7 {
        let name = format!("ints-{}", i);
        {
          let mut o = dir.create_output(&name, &io_context)?;
          let mut junk = [0; 6];
          let junk = &mut junk[..i];
          for byte in junk.iter_mut() {
            *byte = random.random();
          }
          o.write_bytes_with_len(junk, junk.len())?;
          input.seek(0)?;
          let length = IndexInput::length(&input)?;
          o.copy_bytes(&mut input, length)?;
          o.close()?;
        }

        let padded = dir.open_input(&name, &io_context)?;
        let padded_length = IndexInput::length(&padded)?;
        let whole = padded.random_access_slice(i, padded_length - i)?;
        assert_eq!(padded_length - i, RandomAccessInput::length(&whole)?);
        for (j, &expected) in ints.iter().enumerate() {
          assert_eq!(expected, RandomAccessInput::read_int(&whole, j * 4)?);
        }
        CloseableRef::close(&padded)?;
      }
      CloseableRef::close(&input)?;
    }

    dir.close()
  }

  fn test_random_short<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("testShorts").tempdir()?;
    let dir = self.get_directory(temp_dir.path().to_path_buf(), random)?;
    let io_context = new_io_context(random)?;

    let num = TestUtil::next_usize(random, 50, 3000);
    let mut shorts = vec![0i16; num];
    {
      let mut output = dir.create_output("shorts", &io_context)?;
      for value in &mut shorts {
        *value = random.random_range(i16::MIN..=i16::MAX);
        output.write_short(*value)?;
      }
      output.close()?;
    }

    // Slice
    {
      let mut input = dir.open_input("shorts", &io_context)?;
      let length = IndexInput::length(&input)?;
      {
        let slice = input.random_access_slice(0, length)?;
        assert_eq!(length, RandomAccessInput::length(&slice)?);
        for (i, &expected) in shorts.iter().enumerate() {
          assert_eq!(expected, RandomAccessInput::read_short(&slice, i * 2)?);
        }
      }

      // Subslices
      for i in 1..shorts.len() {
        let offset = i * 2;
        let subslice = input.random_access_slice(offset, length - offset)?;
        assert_eq!(length - offset, RandomAccessInput::length(&subslice)?);
        for (j, &expected) in shorts.iter().skip(i).enumerate() {
          assert_eq!(expected, RandomAccessInput::read_short(&subslice, j * 2)?);
        }
      }

      // With padding
      for i in 0..7 {
        let name = format!("shorts-{}", i);
        {
          let mut o = dir.create_output(&name, &io_context)?;
          let mut junk = [0; 6];
          let junk = &mut junk[..i];
          for byte in junk.iter_mut() {
            *byte = random.random();
          }
          o.write_bytes_with_len(junk, junk.len())?;
          input.seek(0)?;
          let length = IndexInput::length(&input)?;
          o.copy_bytes(&mut input, length)?;
          o.close()?;
        }

        let padded = dir.open_input(&name, &io_context)?;
        let padded_length = IndexInput::length(&padded)?;
        let whole = padded.random_access_slice(i, padded_length - i)?;
        assert_eq!(padded_length - i, RandomAccessInput::length(&whole)?);
        for (j, &expected) in shorts.iter().enumerate() {
          assert_eq!(expected, RandomAccessInput::read_short(&whole, j * 2)?);
        }
        CloseableRef::close(&padded)?;
      }
      CloseableRef::close(&input)?;
    }

    dir.close()
  }
  fn test_random_byte<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("testBytes").tempdir()?;
    let dir = self.get_directory(temp_dir.path().to_path_buf(), random)?;

    let body_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      let output_context = new_io_context(random)?;
      let mut output = dir.create_output("bytes", &output_context)?;
      let num = if is_night_mode() {
        TestUtil::next_usize(random, 1000, 3000)
      } else {
        TestUtil::next_usize(random, 50, 1000)
      };
      let mut bytes = vec![0u8; num];
      random.fill_bytes(&mut bytes);
      for &byte in &bytes {
        output.write_byte(byte)?;
      }
      output.close()?;
      output.close()?;

      // Slice

      let input_context = new_io_context(random)?;
      let mut input = dir.open_input("bytes", &input_context)?;
      let length = IndexInput::length(&input)?;
      {
        let mut slice = input.random_access_slice(0, length)?;
        assert_eq!(length, RandomAccessInput::length(&slice)?);
        Self::assert_bytes(&mut slice, &bytes, 0, random)?;
      }

      // Subslices
      let length = IndexInput::length(&input)?;
      for offset in 1..bytes.len() {
        let mut subslice = input.random_access_slice(offset, length - offset)?;
        assert_eq!(length - offset, RandomAccessInput::length(&subslice)?);
        Self::assert_bytes(&mut subslice, &bytes, offset, random)?;
      }

      // With padding
      for i in 1..7 {
        let name = format!("bytes-{}", i);
        let output_context = new_io_context(random)?;
        let mut output = dir.create_output(&name, &output_context)?;
        let mut junk = [0; 6];
        let junk = &mut junk[..i];
        for byte in junk.iter_mut() {
          *byte = random.random();
        }
        output.write_bytes_with_len(junk, junk.len())?;
        let length = IndexInput::length(&input)?;
        input.seek(0)?;
        output.copy_bytes(&mut input, length)?;
        output.close()?;

        let input_context = new_io_context(random)?;
        let padded = dir.open_input(&name, &input_context)?;
        let length = IndexInput::length(&padded)?;
        let mut whole = padded.random_access_slice(i, length - i)?;
        assert_eq!(length - i, RandomAccessInput::length(&whole)?);
        Self::assert_bytes(&mut whole, &bytes, 0, random)?;
        padded.close()?;
      }

      input.close()
    }));
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dir.close()));
    IOUtils::use_or_suppress_caught_result(body_result, close_result)
  }
  fn assert_bytes<R, T>(
    slice: &mut T,
    bytes: &[u8],
    bytes_offset: usize,
    _random: &mut R,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
    T: RandomAccessInput,
  {
    let to_read = bytes.len() - bytes_offset;

    for i in 0..to_read {
      assert_eq!(bytes[bytes_offset + i], slice.read_byte(i)?);
      assert_eq!(
        &bytes[bytes_offset..bytes_offset + i],
        slice.read_bytes(0, i)?.as_ref()
      );
      assert_eq!(
        &bytes[bytes_offset + i..],
        slice.read_bytes(i, to_read - i)?.as_ref()
      );
    }

    Ok(())
  }
  fn test_slice_of_slice<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("sliceOfSlice").tempdir()?;
    let dir = self.get_directory(temp_dir.path().to_path_buf(), random)?;

    let num = if is_night_mode() {
      TestUtil::next_usize(random, 250, 2500)
    } else {
      TestUtil::next_usize(random, 50, 250)
    };

    let mut bytes = vec![0u8; num];
    random.fill_bytes(&mut bytes);
    let io_context = new_io_context(random)?;

    {
      let mut output = dir.create_output("bytes", &io_context)?;
      for &byte in &bytes {
        output.write_byte(byte)?;
      }
    }

    let mut input = dir.open_input("bytes", &io_context)?;

    // Seek to a random spot to ensure it doesn't affect slicing
    input.seek(TestUtil::next_usize(random, 0, IndexInput::length(&input)?))?;

    for i in (0..num).step_by(16) {
      let mut slice1 = input.slice("slice1", i, num - i)?;
      assert_eq!(0, slice1.get_file_pointer()?);
      let slice1_length = IndexInput::length(&slice1)?;
      assert_eq!((num - i), slice1_length);

      // seek to a random spot shouldn't impact slicing.
      slice1.seek(TestUtil::next_usize(random, 0, slice1_length))?;

      for j in (0..slice1_length).step_by(16) {
        let mut slice2 = slice1.slice("slice2", j, (num - i) - j)?;
        assert_eq!(0, slice2.get_file_pointer()?);
        let slice2_length = IndexInput::length(&slice2)?;
        assert_eq!((num - i) - j, slice2_length);

        let mut data = vec![0u8; num];
        let copy_source = &bytes[..i + j];
        data[..copy_source.len()].copy_from_slice(copy_source);

        if random.random_bool(0.5) {
          // Read the bytes for this slice-of-slice
          DataInput::read_bytes(&mut slice2, &mut data[i + j..], 0, num - i - j)?;
        } else {
          // Seek to a random spot in between, read some, seek back,
          // and read the rest
          let seek = TestUtil::next_usize(random, 0, slice2_length);
          slice2.seek(seek)?;
          DataInput::read_bytes(
            &mut slice2,
            &mut data[(i + j + seek)..],
            0,
            num - i - j - seek,
          )?;
          slice2.seek(0)?;
          DataInput::read_bytes(&mut slice2, &mut data[i + j..(i + j + seek)], 0, seek)?;
        }

        assert_eq!(bytes, data);
        CloseableRef::close(&slice2)?;
      }
      CloseableRef::close(&slice1)?;
    }

    CloseableRef::close(&input)?;
    dir.close()
  }
  /// This test verifies that writes larger than the size of the buffer output
  /// will correctly increment the file pointer.
  fn test_large_writes<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("largeWrites").tempdir()?;
    let dir = self.get_directory(temp_dir.path().to_path_buf(), random)?;
    let io_context = new_io_context(random)?;

    let mut output = dir.create_output("testBufferStart.txt", &io_context)?;

    let mut large_buf = vec![0u8; 2048];
    random.fill_bytes(&mut large_buf);

    let current_pos = output.get_file_pointer()?;
    let large_buf_len = large_buf.len();
    output.write_bytes_with_len(&large_buf, large_buf_len)?;

    assert_eq!(current_pos + large_buf.len(), output.get_file_pointer()?);
    output.close()?;
    dir.close()
  }
  /// This test verifies that the `to_string` implementation of `IndexOutput`
  /// contains the file name.
  fn test_index_output_to_string<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().tempdir()?;
    let dir = self.get_directory(temp_dir.path().to_path_buf(), random)?;
    let io_context = new_io_context(random)?;

    let mut output = dir.create_output("camelCase.txt", &io_context)?;
    let output_description = output.to_string();
    assert!(
      output_description.contains("camelCase.txt"),
      "Expected `to_string` to contain 'camelCase.txt', but got: {}",
      output_description
    );
    output.close()?;
    dir.close()
  }
  fn test_double_close_output<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().tempdir()?;
    let dir = self.get_directory(temp_dir.path().to_path_buf(), random)?;
    let body_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      let mut out = dir.create_output("foobar", &new_io_context(random)?)?;
      out.write_string("testing")?;
      Closeable::close(&mut out)?;
      Closeable::close(&mut out)?; // close again
      Ok(())
    }));
    let close_result =
      std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| CloseableRef::close(&dir)));
    IOUtils::use_or_suppress_caught_result(body_result, close_result)
  }

  fn test_double_close_input<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().tempdir()?;
    let dir = self.get_directory(temp_dir.path().to_path_buf(), random)?;
    let body_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      let mut out = dir.create_output("foobar", &new_io_context(random)?)?;
      out.write_string("testing")?;
      Closeable::close(&mut out)?;
      let mut input = dir.open_input("foobar", &new_io_context(random)?)?;
      assert_eq!("testing", input.read_string()?);
      CloseableRef::close(&input)?;
      CloseableRef::close(&input)?; // close again
      Ok(())
    }));
    let close_result =
      std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| CloseableRef::close(&dir)));
    IOUtils::use_or_suppress_caught_result(body_result, close_result)
  }
  /// This test ensures that `create_temp_output` generates unique files and
  /// writes/reads data correctly.
  fn test_create_temp_output<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().tempdir()?;
    let dir = self.get_directory(temp_dir.path().to_path_buf(), random)?;

    let mut names = Vec::new();
    let iters = at_least(random, 50);
    names.reserve(iters as usize);
    let io_context = new_io_context(random)?;

    for iter in 0..iters {
      let mut output = dir.create_temp_output("foo", "bar", &io_context)?;
      names.push(output.get_name().to_string());
      output.write_vint(iter)?;
      output.close()?;
    }

    for (iter, name) in (0..iters).zip(&names) {
      let mut input = dir.open_input(name, &io_context)?;
      assert_eq!({ iter }, input.read_vint()?);
      CloseableRef::close(&input)?;
    }

    let files: HashSet<String> = dir
      .list_all()?
      .into_iter()
      .filter(|file| file != "extra0")
      .collect();

    assert_eq!(names.into_iter().collect::<HashSet<_>>(), files);

    dir.close()
  }
  /// This test ensures that attempting to create an output for an existing
  /// file results in an error, and after deleting the file, it can be
  /// created again.
  fn test_create_output_for_existing_file<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().tempdir()?;
    let dir = self.get_directory(temp_dir.path().to_path_buf(), random)?;
    let io_context = IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?;
    let name = "file";

    {
      let mut output = dir.create_output(name, io_context)?;
      assert_eq!(output.get_name(), name);
      output.close()?;
    }

    {
      // Attempt to create the same file again, which should fail
      let result = dir.create_output(name, io_context);
      assert!(
        matches!(result.as_ref().map_err(LuceneError::kind), Err(LuceneErrorKind::IoWithPath { source, .. }) if source.kind() == ErrorKind::AlreadyExists)
      );
    }

    // Delete the file and attempt to recreate it
    dir.delete_file(name)?;
    let mut output = dir.create_output(name, io_context)?;
    output.close()?;

    dir.close()
  }

  fn test_seek_to_end_of_file<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().tempdir()?;
    let dir = self.get_directory(temp_dir.path().to_path_buf(), random)?;
    let io_context = IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?;
    {
      let mut out = dir.create_output("a", io_context)?;
      for _ in 0..1024 {
        out.write_byte(0)?;
      }
      out.close()?;
    }

    {
      let mut input = dir.open_input("a", io_context)?;
      input.seek(100)?;
      assert_eq!(100, input.get_file_pointer()?);

      input.seek(1024)?;
      assert_eq!(1024, input.get_file_pointer()?);
      CloseableRef::close(&input)?;
    }

    dir.close()
  }
  fn test_seek_beyond_end_of_file<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().tempdir()?;
    let dir = self.get_directory(temp_dir.path().to_path_buf(), random)?;
    let io_context = IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?;
    // Write a file with 1024 bytes
    {
      let mut out = dir.create_output("a", io_context)?;
      for _ in 0..1024 {
        out.write_byte(0)?;
      }
      out.close()?;
    }

    // Test seeking within and beyond the file's end
    {
      let mut input = dir.open_input("a", io_context)?;
      input.seek(100)?;
      assert_eq!(100, input.get_file_pointer()?);

      // Attempting to seek beyond the end of the file should return an
      // EOF error
      assert!(matches!(
        (input.seek(1025)).as_ref().map_err(LuceneError::kind),
        Err(LuceneErrorKind::Eof(_))
      ));
      CloseableRef::close(&input)?;
    }

    dir.close()
  }
  fn test_pending_deletions<R>(&self, _random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    test_not_required_in_rust_lucene!();
  }
  fn test_list_all_is_sorted<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("test_list_all_is_sorted").tempdir()?;
    let dir = self.get_directory(temp_dir.path().to_path_buf(), random)?;

    let count = at_least_usize(random, 20);
    let mut names = HashSet::new();

    let mut names_len = names.len();
    while names_len < count {
      let name = IndexFileNames::segment_file_name(
        &TestUtil::random_simple_string_range(random, 1, 6),
        &TestUtil::random_simple_string(random),
        "test",
      );
      let io_context = IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?;
      if random.random_range(0..5) == 1 {
        // Create a temporary output
        {
          let mut output = dir.create_temp_output(&name, "foo", io_context)?;
          let output_name = output.get_name().to_string();
          names.insert(output_name);
          output.close()?;
        }
      } else if !names.contains(name.as_str()) {
        // Create a normal output
        {
          let mut output = dir.create_output(&name, io_context)?;
          let output_name = output.get_name().to_string();
          names.insert(output_name);
          output.close()?;
        }
      }
      names_len = names.len();
    }

    let actual: Vec<String> = dir.list_all()?;
    let mut expected: Vec<&String> = actual.iter().collect();
    expected.sort();

    assert!(
      expected.len() == actual.len() && expected.iter().copied().eq(actual.iter()),
      "expected={expected:?}, actual={actual:?}"
    );

    dir.close()
  }
  fn test_data_types<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let mut values: [i64; 4] = [43, 12345, 123456, 1234567890];
    let temp_dir = Builder::new().prefix("test_data_types").tempdir()?;
    let dir = self.get_directory(temp_dir.path().to_path_buf(), random)?;
    let io_context = IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?;
    {
      let mut out = dir.create_output("test", io_context)?;
      out.write_byte(43u8)?;
      out.write_short(12345i16)?;
      out.write_int(1234567890i32)?;
      let values_len = values.len();
      write_group_vints_i64(&mut out, &mut values, values_len)?;
      out.write_long(1234567890123456789i64)?;
      out.close()?;
      out.close()?;
    }

    let mut restored = [0i64; 4];
    {
      let mut input = dir.open_input("test", io_context)?;
      assert_eq!(43, DataInput::read_byte(&mut input)? as i32);
      assert_eq!(12345, DataInput::read_short(&mut input)? as i32);
      assert_eq!(1234567890, DataInput::read_int(&mut input)?);
      let restored_len = restored.len();
      GroupVIntUtil::read_group_vints_i64(&mut input, &mut restored, restored_len)?;
      assert_eq!(values, restored);
      assert_eq!(1234567890123456789, DataInput::read_long(&mut input)?);
      CloseableRef::close(&input)?;
    }

    dir.close()
  }
  fn test_group_vint_overflow<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("testGroupVIntOverflow").tempdir()?;
    let dir = self.get_directory(temp_dir.path().to_path_buf(), random)?;

    let size = 32;
    let mut values = [0i64; 32];
    let mut restore = [0i64; 32];
    // The value is positive as `i64`, but its low 32 bits represent `i32::MIN`.
    values[0] = 1i64 << 31;

    for i in 0..size {
      if random.random_bool(0.5) {
        values[i] = values[0];
      }
    }

    // a smaller limit value covers the default implementation of
    // read_group_vints, and a bigger limit value covers the faster
    // implementation.
    let values_len = values.len();
    let limit = random.random_range(1..size);
    let io_context = IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?;
    let mut out = dir.create_output("test", io_context)?;
    write_group_vints_i64(&mut out, &mut values[..values_len], limit)?;
    out.close()?;
    {
      let mut input = dir.open_input("test", io_context)?;
      GroupVIntUtil::read_group_vints_i64(&mut input, &mut restore, limit)?;
      for i in 0..limit {
        assert_eq!(values[i], restore[i]);
      }
      CloseableRef::close(&input)?;
    }

    values[0] = (0xFFFFFFFF_u64 + 1) as i64;
    let result = write_group_vints_i64(&mut out, &mut values[..values_len], 4);
    assert!(matches!(
      result.as_ref().map_err(LuceneError::kind),
      Err(LuceneErrorKind::NumberOverflow(_))
    ));

    dir.close()
  }
  fn test_group_vint<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("testGroupVInt").tempdir()?;
    let dir = self.get_directory(temp_dir.path().to_path_buf(), random)?;

    // Test fallback to default implementation of readGroupVInt
    Self::do_test_group_vint(&dir, random, 5, 1, 6, 8)?;

    // Use more iterations to cover all bpv
    let iterations = at_least_usize(random, 100);
    Self::do_test_group_vint(&dir, random, iterations, 1, 31, 128)?;

    // BaseChunkedDirectoryTestCase::test_group_vint_multi_blocks covers multiple
    // blocks This part might be covered in another test or
    // implementation
    dir.close()
  }
  fn do_test_group_vint<R, D>(
    dir: &D,
    random: &mut R,
    iterations: usize,
    min_bpv: i32,
    max_bpv: i32,
    max_num_values: usize,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
    D: Directory,
  {
    let mut values = vec![0i64; max_num_values];
    let mut num_values_array = vec![0usize; iterations];
    let io_context = IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?;
    // Create output files
    {
      let mut group_vint_out = dir.create_output("group-varint", io_context)?;
      let mut vint_out = dir.create_output("vint", io_context)?;

      // Encode
      for num_values in num_values_array.iter_mut().take(iterations) {
        let bpv = TestUtil::next_int(random, min_bpv, max_bpv);
        *num_values = TestUtil::next_usize(random, 1, max_num_values);

        for value in values.iter_mut().take(*num_values) {
          let upper = PackedInts::max_value(bpv) as i32;
          *value = if upper == 0 {
            0
          } else {
            random.random_range(0..=upper) as i64
          };
          vint_out.write_vint(*value as i32)?;
        }

        write_group_vints_i64(&mut group_vint_out, &mut values, *num_values)?;
      }
      group_vint_out.close()?;
      vint_out.close()?;
    }

    // Decode
    {
      let mut group_vint_in = dir.open_input("group-varint", io_context)?;
      let mut vint_in = dir.open_input("vint", io_context)?;
      for &num_values in num_values_array.iter().take(iterations) {
        GroupVIntUtil::read_group_vints_i64(&mut group_vint_in, &mut values, num_values)?;

        for (j, &expected_value) in values.iter().take(num_values).enumerate() {
          let vint_value = vint_in.read_vint()?;
          assert_eq!(
            vint_value as i64, expected_value,
            "Mismatch at index {}: expected {}, got {}",
            j, expected_value, vint_value
          );
        }
      }
      CloseableRef::close(&group_vint_in)?;
      CloseableRef::close(&vint_in)?;
    }
    dir.delete_file("group-varint")?;
    dir.delete_file("vint")?;

    Ok(())
  }
  fn test_prefetch<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    self.do_test_prefetch(0, random)
  }

  fn test_prefetch_on_slice<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    self.do_test_prefetch(TestUtil::next_usize(random, 1, 1024), random)
  }

  fn test_update_read_advice<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("testUpdateReadAdvice").tempdir()?;
    let dir = self.get_directory(temp_dir.keep(), random)?;
    let total_length = TestUtil::next_usize(random, 16384, 65536);
    let mut arr = vec![0u8; total_length];
    random.fill_bytes(&mut arr[..]);
    let io_context = IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?;
    {
      let mut out = dir.create_output("temp.bin", io_context)?;
      out.write_bytes_with_len(&arr, arr.len())?;
      out.close()?;
    }

    let mut orig = Some(dir.open_input("temp.bin", io_context)?);
    let mut input = if random.random_bool(0.5) {
      orig.as_ref().expect("original input").try_clone()?
    } else {
      orig.take().expect("original input")
    };
    let read_advices: [ReadAdvice; ReadAdvice::COUNT] = [
      ReadAdvice::Normal,
      ReadAdvice::Random,
      ReadAdvice::Sequential,
      ReadAdvice::RandomPreload,
    ];

    // Read advice updated at start
    input.update_read_advice(read_advices[random.random_range(0..read_advices.len())])?;
    let input_length = IndexInput::length(&input)?;
    for _ in 0..total_length {
      let offset = TestUtil::next_usize(random, 0, input_length - 1);
      input.seek(offset)?;
      assert_eq!(arr[offset], DataInput::read_byte(&mut input)?);
    }

    // Updating readAdvice in the middle
    for _ in 0..10_000 {
      let offset = TestUtil::next_usize(random, 0, input_length - 1);
      input.seek(offset)?;
      assert_eq!(arr[offset], DataInput::read_byte(&mut input)?);
      if random.random_bool(0.5) {
        input.update_read_advice(read_advices[random.random_range(0..read_advices.len())])?;
      }
    }
    CloseableRef::close(&input)?;
    if let Some(orig) = orig {
      CloseableRef::close(&orig)?;
    }
    dir.close()
  }

  fn do_test_prefetch<R>(&self, start_offset: usize, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("test_prefetch").tempdir()?;
    let dir = self.get_directory(temp_dir.path().to_path_buf(), random)?;

    let total_length = start_offset + random.random_range(16384..=65536);
    // let mut arr = vec![0u8; total_length];
    let mut arr = vec![0u8; total_length];
    random.fill_bytes(&mut arr[..]);
    let io_context = IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?;
    {
      let mut out = dir.create_output("temp.bin", io_context)?;
      out.write_bytes_with_len(&arr, total_length)?;
      out.close()?;
    }

    let mut temp = vec![0u8; 2048];

    let mut orig = Some(dir.open_input("temp.bin", io_context)?);
    let mut input = if start_offset == 0 {
      orig.as_ref().expect("original input").try_clone()?
    } else {
      orig.as_ref().expect("original input").slice(
        "slice",
        start_offset,
        total_length - start_offset,
      )?
    };

    for _ in 0..10_000 {
      let input_length = IndexInput::length(&input)?;
      let offset = random.random_range(0..input_length);

      if random.random_bool(0.5) {
        let prefetch_length = random.random_range(1..=(input_length - offset));
        IndexInput::prefetch(&input, offset, prefetch_length)?;
      }

      input.seek(offset)?;
      assert_eq!(offset, input.get_file_pointer()?);

      match random.random_range(0..100) {
        0 => {
          let read_byte = DataInput::read_byte(&mut input)?;
          assert_eq!(arr[start_offset + offset], read_byte);
        },
        1 => {
          if (input_length - offset) >= 8 {
            let expected = i64::from_le_bytes(
              arr[start_offset + offset..start_offset + offset + 8]
                .try_into()
                .unwrap(),
            );
            let read_long = DataInput::read_long(&mut input)?;
            assert_eq!(expected, read_long);
          }
        },
        _ => {
          let read_length = random.random_range(1..=temp.len().min(input_length - offset));
          DataInput::read_bytes(&mut input, &mut temp[..read_length], 0, read_length)?;
          assert_eq!(
            &arr[start_offset + offset..start_offset + offset + read_length],
            &temp[..read_length]
          );
        },
      }
    }
    CloseableRef::close(&input)?;
    if let Some(orig) = orig.take() {
      CloseableRef::close(&orig)?;
    }
    dir.close()
  }

  fn test_is_loaded<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    self.do_test_is_loaded(0, random)
  }

  fn test_is_loaded_on_slice<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    self.do_test_is_loaded(TestUtil::next_usize(random, 1, 1024), random)
  }

  fn do_test_is_loaded<R>(&self, start_offset: usize, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_dir = Builder::new().prefix("testIsLoaded").tempdir()?;
    let mut dir = self.get_directory(temp_dir.path().to_path_buf(), random)?;
    let is_mmap_directory = self.configure_is_loaded_test(&mut dir);

    let body_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      let total_length = start_offset + TestUtil::next_usize(random, 16384, 65536);
      let mut arr = vec![0u8; total_length];
      random.fill_bytes(&mut arr);
      let io_context = IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?;

      let mut out = dir.create_output("temp.bin", io_context)?;
      let write_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        out.write_bytes_with_len(&arr, arr.len())
      }));
      let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| out.close()));
      IOUtils::use_or_suppress_caught_result(write_result, close_result)?;

      let orig = dir.open_input("temp.bin", io_context)?;
      let input_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
          let input = if start_offset == 0 {
            orig.try_clone()?
          } else {
            orig.slice("slice", start_offset, total_length - start_offset)?
          };
          let loaded = IndexInput::is_loaded(&input)?;

          if cfg!(windows) {
            // On Windows, we temporarily don't care until this is fixed: #14050
          } else if is_mmap_directory {
            // direct IO wraps MMap but does not support isLoaded
            assert!(loaded.is_some());
            assert!(loaded.unwrap());
          }
          Ok(())
        }));
      let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| orig.close()));
      IOUtils::use_or_suppress_caught_result(input_result, close_result)
    }));
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dir.close()));
    IOUtils::use_or_suppress_caught_result(body_result, close_result)
  }
}
