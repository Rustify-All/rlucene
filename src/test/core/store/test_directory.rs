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
  create_temp_dir_with_prefix, new_io_context, random, slow_file_exists,
};
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::fs::File;

use rand::RngExt;

use crate::core::store::data_input::DataInput;
use crate::core::store::data_output::DataOutput;
use crate::core::store::directory::Directory;
use crate::core::store::fs_directory::FSDirectory;
use crate::core::store::index_input::{IndexInput, IndexInputEnum2};
use crate::core::store::io_context::IOContext;
use crate::core::store::mmap_directory::MMapDirectory;
use crate::core::store::native_fs_lock_factory::NativeFSLockFactory;
use crate::core::store::nio_fs_directory::NIOFSDirectory;
use crate::core::util::HasIdentity;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::Result;

#[allow(dead_code)] // for quick search
struct TestDirectory;

type NioDirectory = FSDirectory<NativeFSLockFactory, NIOFSDirectory>;
type MMapDirectoryType = FSDirectory<NativeFSLockFactory, MMapDirectory>;

enum TestFSDirectory {
  Nio(NioDirectory),
  MMap(MMapDirectoryType),
}

impl Display for TestFSDirectory {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Nio(directory) => directory.fmt(f),
      Self::MMap(directory) => directory.fmt(f),
    }
  }
}

impl HasIdentity for TestFSDirectory {
  fn identity(&self) -> &crate::core::index::index_reader::Identity {
    match self {
      Self::Nio(directory) => directory.identity(),
      Self::MMap(directory) => directory.identity(),
    }
  }
}

impl Directory for TestFSDirectory {
  fn list_all(&self) -> Result<Vec<String>> {
    match self {
      Self::Nio(directory) => directory.list_all(),
      Self::MMap(directory) => directory.list_all(),
    }
  }

  fn delete_file(&self, name: &str) -> Result<()> {
    match self {
      Self::Nio(directory) => directory.delete_file(name),
      Self::MMap(directory) => directory.delete_file(name),
    }
  }

  fn file_length(&self, name: &str) -> Result<usize> {
    match self {
      Self::Nio(directory) => directory.file_length(name),
      Self::MMap(directory) => directory.file_length(name),
    }
  }

  type IndexOutput = <NioDirectory as Directory>::IndexOutput;

  fn create_output(&self, name: &str, context: &IOContext) -> Result<Self::IndexOutput> {
    match self {
      Self::Nio(directory) => directory.create_output(name, context),
      Self::MMap(directory) => directory.create_output(name, context),
    }
  }

  fn create_temp_output(
    &self,
    prefix: &str,
    suffix: &str,
    context: &IOContext,
  ) -> Result<Self::IndexOutput> {
    match self {
      Self::Nio(directory) => directory.create_temp_output(prefix, suffix, context),
      Self::MMap(directory) => directory.create_temp_output(prefix, suffix, context),
    }
  }

  fn sync(&self, names: &[String]) -> Result<()> {
    match self {
      Self::Nio(directory) => directory.sync(names),
      Self::MMap(directory) => directory.sync(names),
    }
  }

  fn sync_metadata(&self) -> Result<()> {
    match self {
      Self::Nio(directory) => directory.sync_metadata(),
      Self::MMap(directory) => directory.sync_metadata(),
    }
  }

  fn rename(&self, source: &str, dest: &str) -> Result<()> {
    match self {
      Self::Nio(directory) => directory.rename(source, dest),
      Self::MMap(directory) => directory.rename(source, dest),
    }
  }

  type IndexInput = IndexInputEnum2<
    <NioDirectory as Directory>::IndexInput,
    <MMapDirectoryType as Directory>::IndexInput,
  >;

  fn open_input(&self, name: &str, context: &IOContext) -> Result<Self::IndexInput> {
    match self {
      Self::Nio(directory) => Ok(IndexInputEnum2::A(directory.open_input(name, context)?)),
      Self::MMap(directory) => Ok(IndexInputEnum2::B(directory.open_input(name, context)?)),
    }
  }

  type Lock = <NioDirectory as Directory>::Lock;

  fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
    match self {
      Self::Nio(directory) => directory.obtain_lock(name),
      Self::MMap(directory) => directory.obtain_lock(name),
    }
  }

  fn get_pending_deletions(&self) -> Result<HashSet<String>> {
    match self {
      Self::Nio(directory) => directory.get_pending_deletions(),
      Self::MMap(directory) => directory.get_pending_deletions(),
    }
  }

  #[cfg(debug_assertions)]
  fn is_fs_directory(&self) -> bool {
    true
  }

  fn ensure_open(&self) -> Result<()> {
    match self {
      Self::Nio(directory) => directory.ensure_open(),
      Self::MMap(directory) => directory.ensure_open(),
    }
  }
}

impl CloseableRef for TestFSDirectory {
  fn close(&self) -> Result<()> {
    match self {
      Self::Nio(directory) => directory.close(),
      Self::MMap(directory) => directory.close(),
    }
  }
}

// Test that different instances of FSDirectory can coexist on the same
// path, can read, write, and lock files.
#[test]
fn test_direct_instantiation() -> Result<()> {
  let path = create_temp_dir_with_prefix("testDirectInstantiation")?;

  let mut random = random();
  let large_buffer: Vec<u8> = (0..random.random_range(0..256 * 1024))
    .map(|i| i as u8)
    .collect();
  let mut large_read_buffer = vec![0u8; large_buffer.len()];

  let mut dirs = vec![
    TestFSDirectory::Nio(NIOFSDirectory::new(path.path().to_path_buf())?),
    TestFSDirectory::MMap(MMapDirectory::new(path.path().to_path_buf())?),
  ];

  for i in 0..dirs.len() {
    let dir = &dirs[i];
    dir.ensure_open()?;
    let fname = format!("foo.{i}");
    let lockname = format!("foo{i}.lck");
    let mut out = dir.create_output(&fname, &new_io_context(&mut random)?)?;
    out.write_byte(i as u8)?;
    out.write_bytes_with_len(&large_buffer, large_buffer.len())?;
    out.close()?;

    for d2 in &dirs {
      d2.ensure_open()?;
      assert!(d2.list_all()?.contains(&fname));
      assert_eq!(1 + large_buffer.len(), d2.file_length(&fname)?);

      let mut input = d2.open_input(&fname, &new_io_context(&mut random)?)?;
      assert_eq!(i as u8, input.read_byte()?);
      // read array with buffering enabled
      large_read_buffer.fill(0);
      input.read_bytes_with_buffer(&mut large_read_buffer, 0, large_buffer.len(), true)?;
      assert_eq!(large_buffer, large_read_buffer);
      // read again without using buffer
      input.seek(1)?;
      large_read_buffer.fill(0);
      input.read_bytes_with_buffer(&mut large_read_buffer, 0, large_buffer.len(), false)?;
      assert_eq!(large_buffer, large_read_buffer);
      input.close()?;
    }

    // delete with a different dir
    dirs[(i + 1) % dirs.len()].delete_file(&fname)?;

    for d2 in &dirs {
      assert!(!d2.list_all()?.contains(&fname));
    }

    let lock = dir.obtain_lock(&lockname)?;

    for other in &dirs {
      assert!(other.obtain_lock(&lockname).is_err());
    }

    lock.close()?;

    // now lock with different dir
    let lock = dirs[(i + 1) % dirs.len()].obtain_lock(&lockname)?;
    lock.close()?;
  }

  for dir in &mut dirs {
    dir.ensure_open()?;
    match dir {
      TestFSDirectory::Nio(dir) => dir.close()?,
      TestFSDirectory::MMap(dir) => dir.close()?,
    }
    assert!(dir.ensure_open().is_err());
  }

  Ok(())
}

// LUCENE-1468
#[test]
fn test_not_directory() -> Result<()> {
  let path = create_temp_dir_with_prefix("testnotdir")?;
  let mut random = random();
  let fs_dir = NIOFSDirectory::new(path.path().to_path_buf())?;
  let mut out = fs_dir.create_output("afile", &new_io_context(&mut random)?)?;
  out.close()?;
  assert!(slow_file_exists(&fs_dir, "afile")?);
  assert!(NIOFSDirectory::new(path.path().join("afile")).is_err());
  fs_dir.close()
}

#[test]
fn test_list_all() -> Result<()> {
  let dir = create_temp_dir_with_prefix("testdir")?;
  let file1 = dir.path().join("tempfile1");
  let file2 = dir.path().join("tempfile2");
  File::create(&file1)?;
  File::create(&file2)?;

  let fs_dir = NIOFSDirectory::new(dir.path().to_path_buf())?;
  let files: HashSet<String> = fs_dir.list_all()?.into_iter().collect();

  assert_eq!(2, files.len());
  assert!(files.contains(file1.file_name().unwrap().to_str().unwrap()));
  assert!(files.contains(file2.file_name().unwrap().to_str().unwrap()));
  fs_dir.close()?;
  Ok(())
}
