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
use crate::core::document::document::Document;
use crate::core::index::index_reader::Identity;
use crate::core::store::directory::{
  DirEnum, Directory, DirectoryEnum2, DirectoryEnum3, MockDirWrapper, RawDirEnum, SharedLockFactory,
};
use crate::core::store::fs_lock_factory;
use crate::core::store::mmap_directory::MMapDirectory;
use crate::core::store::single_instance_lock_factory::SingleInstanceLockFactory;
use crate::core::store::{
  ByteArrayDataInput, ByteBuffersDirectory, DataInput, DataOutput, IOContext, IndexInputEnum2,
};
use crate::core::util::HasIdentity;
use crate::core::util::clone::TryClone;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::io_utils::IOUtils;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::store::base_directory_test_case::BaseDirectoryTestCase;
use crate::test_framework::core::store::mock_directory_wrapper::MockDirectoryWrapper;
use crate::test_framework::core::util::lucene_test_case::{
  create_temp_dir, new_fs_directory, new_mock_directory, random,
};
use rand::{Rng, RngExt};
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

#[allow(dead_code)] // for quick search
struct TestMockDirectoryWrapper;

type MemoryMockDirectory = MockDirWrapper;
type FsMockDirectory = MockDirectoryWrapper<RawDirEnum>;
type TestDirectory = DirectoryEnum2<MemoryMockDirectory, FsMockDirectory>;
type TestIndexInput = IndexInputEnum2<
  <MemoryMockDirectory as Directory>::IndexInput,
  <FsMockDirectory as Directory>::IndexInput,
>;

impl BaseDirectoryTestCase for TestMockDirectoryWrapper {
  type Directory = TestDirectory;
  type Output = TestIndexInput;

  fn get_directory<R>(&self, path: PathBuf, random: &mut R) -> Result<Self::Directory>
  where
    R: Rng + ?Sized,
  {
    if random.random_bool(0.5) {
      Ok(DirectoryEnum2::A(new_mock_directory(random)?))
    } else {
      let lock_factory: SharedLockFactory = Arc::new(fs_lock_factory::get_default().into());
      Ok(DirectoryEnum2::B(MockDirectoryWrapper::new(
        random,
        RawDirEnum::Nio(
          crate::core::store::nio_fs_directory::NIOFSDirectory::with_lock_factory(
            path,
            lock_factory,
          )?,
        ),
      )))
    }
  }

  fn configure_is_loaded_test(&self, dir: &mut Self::Directory) -> bool {
    match dir {
      DirectoryEnum2::A(dir) => {
        let mut base = dir.state.base.lock();
        let raw_dir = match &mut base.in_ {
          DirectoryEnum2::A(raw_dir) => raw_dir,
          DirectoryEnum2::B(dir) => dir.get_delegate_mut(),
        };
        match raw_dir {
          RawDirEnum::MMap(dir) => {
            dir.set_preload(MMapDirectory::ALL_FILES);
            true
          },
          RawDirEnum::FileSwitch(dir) => match dir.get_secondary_dir_mut() {
            DirectoryEnum3::B(dir) => {
              dir.set_preload(MMapDirectory::ALL_FILES);
              true
            },
            _ => false,
          },
          _ => false,
        }
      },
      DirectoryEnum2::B(dir) => {
        let mut base = dir.state.base.lock();
        match &mut base.in_ {
          RawDirEnum::MMap(dir) => {
            dir.set_preload(MMapDirectory::ALL_FILES);
            true
          },
          _ => false,
        }
      },
    }
  }
}

#[test]
fn test_disk_full() -> Result<()> {
  // test writeBytes
  let mut random = random();
  let dir = new_mock_directory(&mut random)?;
  dir.set_max_size_in_bytes(3);
  let bytes = [1, 2];
  let mut out = dir.create_output("foo", &IOContext::default_io_context()?)?;
  out.write_bytes_with_len(&bytes, bytes.len())?; // first write should succeed
  // close() to ensure the written bytes are not buffered and counted
  // against the directory size
  out.close()?;

  let mut out2 = dir.create_output("bar", &IOContext::default_io_context()?)?;
  assert!(out2.write_bytes_with_len(&bytes, bytes.len()).is_err());
  out2.close()?;
  dir.close()?;

  // test copyBytes
  let dir = new_mock_directory(&mut random)?;
  dir.set_max_size_in_bytes(3);
  let mut out = dir.create_output("foo", &IOContext::default_io_context()?)?;
  let mut input = ByteArrayDataInput::with_bytes(bytes.as_slice());
  out.copy_bytes(&mut input, bytes.len())?; // first copy should succeed
  // close() to ensure the written bytes are not buffered and counted
  // against the directory size
  out.close()?;

  let mut out3 = dir.create_output("bar", &IOContext::default_io_context()?)?;
  let mut input = ByteArrayDataInput::with_bytes(bytes.as_slice());
  assert!(out3.copy_bytes(&mut input, bytes.len()).is_err());
  out3.close()?;
  dir.close()
}

#[test]
fn test_mdw_inside_of_mdw() -> Result<()> {
  let mut random = random();
  // add MDW inside another MDW
  let inner = new_mock_directory(&mut random)?;
  let dir = Arc::new(MockDirectoryWrapper::new(&mut random, inner));
  {
    let iw = RandomIndexWriter::new(&mut random, dir.clone())?;
    for _ in 0..20 {
      iw.add_document(&mut random, Document::new())?;
    }
    iw.commit(&mut random)?;
    iw.close(&mut random)?;
  }
  dir.as_ref().close()
}

// just shields the wrapped directory from being closed
struct PreventCloseDirectoryWrapper<D> {
  in_: D,
  id: Identity,
}

impl<D> PreventCloseDirectoryWrapper<D>
where
  D: Directory,
{
  fn new(in_: D) -> Self {
    Self {
      in_,
      id: Identity::new(),
    }
  }
}

impl<D> Display for PreventCloseDirectoryWrapper<D>
where
  D: Directory,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "PreventCloseDirectoryWrapper({})", self.in_)
  }
}

impl<D> CloseableRef for PreventCloseDirectoryWrapper<D>
where
  D: Directory,
{
  fn close(&self) -> Result<()> {
    Ok(())
  }
}

impl<D> HasIdentity for PreventCloseDirectoryWrapper<D>
where
  D: Directory,
{
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl<D> Directory for PreventCloseDirectoryWrapper<D>
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

  #[cfg(debug_assertions)]
  fn is_fs_directory(&self) -> bool {
    self.in_.is_fs_directory()
  }

  fn ensure_open(&self) -> Result<()> {
    self.in_.ensure_open()
  }
}

#[test]
fn test_corrupt_on_close_is_working_fs_dir() -> Result<()> {
  let mut random = random();
  let dir = new_fs_directory(&mut random, create_temp_dir()?)?;
  test_corrupt_on_close_is_working(&mut random, DirectoryEnum2::A(dir))
}

#[test]
fn test_corrupt_on_close_is_working_on_byte_buffers_directory() -> Result<()> {
  let mut random = random();
  test_corrupt_on_close_is_working(&mut random, DirectoryEnum2::B(ByteBuffersDirectory::new()))
}

fn test_corrupt_on_close_is_working<R>(
  random: &mut R,
  dir: DirectoryEnum2<Arc<DirEnum>, ByteBuffersDirectory<SingleInstanceLockFactory>>,
) -> Result<()>
where
  R: rand::Rng + ?Sized,
{
  let raw_dir = Arc::new(dir);
  let dir = Arc::new(PreventCloseDirectoryWrapper::new(raw_dir.clone()));

  {
    let wrapped = MockDirectoryWrapper::new(random, dir.clone());

    // otherwise MDW sometimes randomly leaves the file intact and we'll see
    // false test failures:
    wrapped.state.always_corrupt.store(true, Ordering::Relaxed);

    // MDW will only try to corrupt things if it sees an index:
    {
      let iw = RandomIndexWriter::new(random, dir.clone())?;
      iw.add_document(random, Document::new())?;
      iw.close(random)?;
    }

    // not sync'd!
    let mut out = wrapped.create_output("foo", &IOContext::default_io_context()?)?;
    let write_result = (|| -> Result<()> {
      for i in 0..100 {
        out.write_int(i)?;
      }
      Ok(())
    })();
    IOUtils::use_or_suppress_result(write_result, out.close())?;

    // MDW.close now corrupts our unsync'd file (foo):
    wrapped.close()?;
  }

  let mut changed = false;
  let mut input = match dir.open_input("foo", &IOContext::default_io_context()?) {
    Ok(input) => Some(input),
    Err(LuceneError::NoSuchFile(_)) => {
      // ok
      changed = true;
      None
    },
    Err(LuceneError::IoWithPath { ref source, .. }) if source.kind() == ErrorKind::NotFound => {
      // ok
      changed = true;
      None
    },
    Err(error) => return Err(error),
  };
  if let Some(input) = input.as_mut() {
    let read_result = (|| -> Result<()> {
      for i in 0..100 {
        match input.read_int() {
          Ok(value) if value != i => {
            changed = true;
            break;
          },
          Ok(_) => {},
          Err(LuceneError::Eof(_)) => {
            changed = true;
            break;
          },
          Err(error) => return Err(error),
        }
      }
      Ok(())
    })();
    IOUtils::use_or_suppress_result(read_result, input.close())?;
  }

  assert!(
    changed,
    "MockDirectoryWrapper on dir={dir} failed to corrupt an unsync'd file"
  );

  drop(dir);
  let raw_dir = match Arc::try_unwrap(raw_dir) {
    Ok(dir) => dir,
    Err(_) => {
      return Err(LuceneError::illegal_state(
        "wrapped directory still has outstanding references",
      ));
    },
  };
  raw_dir.close()
}

#[test]
fn test_abuse_closed_index_input() -> Result<()> {
  let mut random = random();
  let dir = new_mock_directory(&mut random)?;
  let mut out = dir.create_output("foo", &IOContext::default_io_context()?)?;
  out.write_byte(42)?;
  out.close()?;
  let mut input = dir.open_input("foo", &IOContext::default_io_context()?)?;
  input.close()?;
  assert!(input.read_byte().is_err());
  dir.close()
}

#[test]
fn test_abuse_clone_after_parent_closed() -> Result<()> {
  let mut random = random();
  let dir = new_mock_directory(&mut random)?;
  let mut out = dir.create_output("foo", &IOContext::default_io_context()?)?;
  out.write_byte(42)?;
  out.close()?;
  let input = dir.open_input("foo", &IOContext::default_io_context()?)?;
  let mut clone = input.try_clone()?;
  input.close()?;
  assert!(clone.read_byte().is_err());
  dir.close()
}

#[test]
fn test_abuse_clone_of_clone_after_parent_closed() -> Result<()> {
  let mut random = random();
  let dir = new_mock_directory(&mut random)?;
  let mut out = dir.create_output("foo", &IOContext::default_io_context()?)?;
  out.write_byte(42)?;
  out.close()?;
  let input = dir.open_input("foo", &IOContext::default_io_context()?)?;
  let clone1 = input.try_clone()?;
  let mut clone2 = clone1.try_clone()?;
  input.close()?;
  assert!(clone2.read_byte().is_err());
  dir.close()
}

mod base_directory_test_case_tests {
  use crate::core::util::error::lucene_error::Result;
  use crate::test::core::store::test_mock_directory_wrapper::run_case;
  use crate::test_framework::core::store::base_directory_test_case::BaseDirectoryTestCase;

  #[test]
  fn test_copy_from() -> Result<()> {
    run_case(|case, random| case.test_copy_from(random))
  }

  #[test]
  fn test_rename() -> Result<()> {
    run_case(|case, random| case.test_rename(random))
  }

  #[test]
  fn test_delete_file() -> Result<()> {
    run_case(|case, random| case.test_delete_file(random))
  }

  #[test]
  fn test_byte() -> Result<()> {
    run_case(|case, random| case.test_byte(random))
  }

  #[test]
  fn test_short() -> Result<()> {
    run_case(|case, random| case.test_short(random))
  }

  #[test]
  fn test_int() -> Result<()> {
    run_case(|case, random| case.test_int(random))
  }

  #[test]
  fn test_long() -> Result<()> {
    run_case(|case, random| case.test_long(random))
  }

  #[test]
  fn test_aligned_little_endian_longs() -> Result<()> {
    run_case(|case, random| case.test_aligned_little_endian_longs(random))
  }

  #[test]
  fn test_unaligned_little_endian_longs() -> Result<()> {
    run_case(|case, random| case.test_unaligned_little_endian_longs(random))
  }

  #[test]
  fn test_little_endian_longs_underflow() -> Result<()> {
    run_case(|case, random| case.test_little_endian_longs_underflow(random))
  }

  #[test]
  fn test_aligned_ints() -> Result<()> {
    run_case(|case, random| case.test_aligned_ints(random))
  }

  #[test]
  fn test_unaligned_ints() -> Result<()> {
    run_case(|case, random| case.test_unaligned_ints(random))
  }

  #[test]
  fn test_ints_underflow() -> Result<()> {
    run_case(|case, random| case.test_ints_underflow(random))
  }

  #[test]
  fn test_aligned_floats() -> Result<()> {
    run_case(|case, random| case.test_aligned_floats(random))
  }

  #[test]
  fn test_unaligned_floats() -> Result<()> {
    run_case(|case, random| case.test_unaligned_floats(random))
  }

  #[test]
  fn test_floats_underflow() -> Result<()> {
    run_case(|case, random| case.test_floats_underflow(random))
  }

  #[test]
  fn test_string() -> Result<()> {
    run_case(|case, random| case.test_string(random))
  }

  #[test]
  fn test_vint() -> Result<()> {
    run_case(|case, random| case.test_vint(random))
  }

  #[test]
  fn test_vlong() -> Result<()> {
    run_case(|case, random| case.test_vlong(random))
  }

  #[test]
  fn test_zint() -> Result<()> {
    run_case(|case, random| case.test_zint(random))
  }

  #[test]
  fn test_zlong() -> Result<()> {
    run_case(|case, random| case.test_zlong(random))
  }

  #[test]
  fn test_set_of_strings() -> Result<()> {
    run_case(|case, random| case.test_set_of_strings(random))
  }

  #[test]
  fn test_map_of_strings() -> Result<()> {
    run_case(|case, random| case.test_map_of_strings(random))
  }

  #[test]
  fn test_checksum() -> Result<()> {
    run_case(|case, random| case.test_checksum(random))
  }

  // we wrap the directory in slow stuff, so only run nightly
  #[cfg(feature = "nightly")]
  #[test]
  #[ignore = "nightly"]
  fn test_thread_safety_in_list_all() -> Result<()> {
    run_case(|case, random| case.test_thread_safety_in_list_all(random))
  }

  #[test]
  fn test_file_exists_in_list_after_created() -> Result<()> {
    run_case(|case, random| case.test_file_exists_in_list_after_created(random))
  }

  #[test]
  fn test_seek_to_eof_then_back() -> Result<()> {
    run_case(|case, random| case.test_seek_to_eof_then_back(random))
  }

  #[test]
  fn test_illegal_eof() -> Result<()> {
    run_case(|case, random| case.test_illegal_eof(random))
  }

  #[test]
  fn test_seek_past_eof() -> Result<()> {
    run_case(|case, random| case.test_seek_past_eof(random))
  }

  #[test]
  fn test_slice_out_of_bounds() -> Result<()> {
    run_case(|case, random| case.test_slice_out_of_bounds(random))
  }

  #[test]
  fn test_no_dir() -> Result<()> {
    run_case(|case, random| case.test_no_dir(random))
  }

  #[test]
  fn test_copy_bytes() -> Result<()> {
    run_case(|case, random| case.test_copy_bytes(random))
  }

  #[test]
  fn test_copy_bytes_with_threads() -> Result<()> {
    run_case(|case, random| case.test_copy_bytes_with_threads(random))
  }

  #[test]
  fn test_fsync_doesnt_create_new_files() -> Result<()> {
    run_case(|case, random| case.test_fsync_doesnt_create_new_files(random))
  }

  #[test]
  fn test_random_long() -> Result<()> {
    run_case(|case, random| case.test_random_long(random))
  }

  #[test]
  fn test_random_int() -> Result<()> {
    run_case(|case, random| case.test_random_int(random))
  }

  #[test]
  fn test_random_short() -> Result<()> {
    run_case(|case, random| case.test_random_short(random))
  }

  #[test]
  fn test_random_byte() -> Result<()> {
    run_case(|case, random| case.test_random_byte(random))
  }

  #[test]
  fn test_slice_of_slice() -> Result<()> {
    run_case(|case, random| case.test_slice_of_slice(random))
  }

  #[test]
  fn test_large_writes() -> Result<()> {
    run_case(|case, random| case.test_large_writes(random))
  }

  #[test]
  fn test_index_output_to_string() -> Result<()> {
    run_case(|case, random| case.test_index_output_to_string(random))
  }

  #[test]
  fn test_create_temp_output() -> Result<()> {
    run_case(|case, random| case.test_create_temp_output(random))
  }

  #[test]
  fn test_create_output_for_existing_file() -> Result<()> {
    run_case(|case, random| case.test_create_output_for_existing_file(random))
  }

  #[test]
  fn test_seek_to_end_of_file() -> Result<()> {
    run_case(|case, random| case.test_seek_to_end_of_file(random))
  }

  #[test]
  fn test_seek_beyond_end_of_file() -> Result<()> {
    run_case(|case, random| case.test_seek_beyond_end_of_file(random))
  }

  #[test]
  fn test_pending_deletions() -> Result<()> {
    run_case(|case, random| case.test_pending_deletions(random))
  }

  #[test]
  fn test_list_all_is_sorted() -> Result<()> {
    run_case(|case, random| case.test_list_all_is_sorted(random))
  }

  #[test]
  fn test_data_types() -> Result<()> {
    run_case(|case, random| case.test_data_types(random))
  }

  #[test]
  fn test_group_vint_overflow() -> Result<()> {
    run_case(|case, random| case.test_group_vint_overflow(random))
  }

  #[test]
  fn test_group_vint() -> Result<()> {
    run_case(|case, random| case.test_group_vint(random))
  }

  #[test]
  fn test_prefetch() -> Result<()> {
    run_case(|case, random| case.test_prefetch(random))
  }

  #[test]
  fn test_prefetch_on_slice() -> Result<()> {
    run_case(|case, random| case.test_prefetch_on_slice(random))
  }

  #[test]
  fn test_update_read_advice() -> Result<()> {
    run_case(|case, random| case.test_update_read_advice(random))
  }

  #[test]
  fn test_is_loaded() -> Result<()> {
    run_case(|case, random| case.test_is_loaded(random))
  }

  #[test]
  fn test_is_loaded_on_slice() -> Result<()> {
    run_case(|case, random| case.test_is_loaded_on_slice(random))
  }
}

fn run_case<F>(f: F) -> Result<()>
where
  F: FnOnce(&TestMockDirectoryWrapper, &mut rand::prelude::StdRng) -> Result<()>,
{
  let mut random = random();
  let case = TestMockDirectoryWrapper;
  f(&case, &mut random)
}
