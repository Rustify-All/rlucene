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
use crate::core::store::NativeFSLockFactory;
use crate::core::store::directory::DirEnum;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::store::base_lock_factory_test_case::BaseLockFactoryTestCase;
use crate::test_framework::core::util::lucene_test_case::new_fs_directory_with_lock_factory;
use std::path::PathBuf;

/** Simple tests for NativeFSLockFactory */
#[allow(dead_code)] // for quick search
struct TestNativeFSLockFactory;

impl BaseLockFactoryTestCase for TestNativeFSLockFactory {
  type Directory = DirEnum;

  fn get_directory<R>(&self, random: &mut R, path: PathBuf) -> Result<Self::Directory>
  where
    R: rand::Rng + ?Sized,
  {
    new_fs_directory_with_lock_factory(random, path, NativeFSLockFactory::new())
  }
}

mod native_fs_lock_factory_tests {
  use super::TestNativeFSLockFactory;
  use crate::core::store::directory::{DirEnum, Directory, RawDirEnum};
  use crate::core::store::lock::{Lock, LockEnum, LockEnum2, LockEnum3};
  use crate::core::util::close::{Closeable, CloseableRef};
  use crate::core::util::error::lucene_error::Result;
  use crate::test_framework::core::store::base_lock_factory_test_case::BaseLockFactoryTestCase;
  use crate::test_framework::core::util::lucene_test_case::{create_temp_dir, random};
  use std::fs::{self, File};

  fn release_raw_native_lock(lock: &<RawDirEnum as Directory>::Lock) -> Result<()> {
    match lock {
      LockEnum3::A(LockEnum::Native(lock)) | LockEnum3::B(LockEnum::Native(lock)) => {
        lock.release_lock_for_test()
      },
      _ => unreachable!("newFSDirectory must use NativeFSLockFactory"),
    }
  }

  fn release_native_lock(lock: &<DirEnum as Directory>::Lock) -> Result<()> {
    match lock {
      LockEnum2::A(lock) | LockEnum2::B(lock) => release_raw_native_lock(lock),
    }
  }

  /** Verify NativeFSLockFactory works correctly if the lock file exists */
  #[test]
  fn test_lock_file_exists() -> Result<()> {
    let case = TestNativeFSLockFactory;
    let mut random = random();
    let temp_dir = create_temp_dir()?;
    let lock_file = temp_dir.path().join("test.lock");
    File::create(lock_file)?;

    let dir = case.get_directory(&mut random, temp_dir.path().to_path_buf())?;
    let l = dir.obtain_lock("test.lock")?;
    l.close()?;
    Ok(())
  }

  /** release the lock and test ensureValid fails */
  #[test]
  fn test_invalidate_lock() -> Result<()> {
    let case = TestNativeFSLockFactory;
    let mut random = random();
    let temp_dir = create_temp_dir()?;
    let dir = case.get_directory(&mut random, temp_dir.path().to_path_buf())?;
    let lock = dir.obtain_lock("test.lock")?;
    lock.ensure_valid()?;

    release_native_lock(&lock)?;
    assert!(lock.ensure_valid().is_err());

    lock.close()?;
    let dir = dir;
    dir.close()?;
    Ok(())
  }

  /** close the channel and test ensureValid fails */
  #[test]
  fn test_invalidate_channel() -> Result<()> {
    let case = TestNativeFSLockFactory;
    let mut random = random();
    let temp_dir = create_temp_dir()?;
    let dir = case.get_directory(&mut random, temp_dir.path().to_path_buf())?;
    let lock = dir.obtain_lock("test.lock")?;
    lock.ensure_valid()?;

    lock.close()?;
    assert!(lock.ensure_valid().is_err());

    let dir = dir;
    dir.close()?;
    Ok(())
  }

  /** delete the lockfile and test ensureValid fails */
  #[test]
  fn test_delete_lock_file() -> Result<()> {
    let case = TestNativeFSLockFactory;
    let mut random = random();
    let temp_dir = create_temp_dir()?;
    let dir = case.get_directory(&mut random, temp_dir.path().to_path_buf())?;
    let lock = dir.obtain_lock("test.lock")?;
    lock.ensure_valid()?;

    dir.delete_file("test.lock")?;

    assert!(lock.ensure_valid().is_err());

    lock.close()?;
    let dir = dir;
    dir.close()?;
    Ok(())
  }
  /// This test relies on Unix directory write permissions; Windows readonly directories can still create files.
  #[cfg(unix)]
  #[test]
  fn test_bad_permissions() -> Result<()> {
    let case = TestNativeFSLockFactory;
    let mut random = random();
    // create a directory that will fail while creating test.lock
    let tmp_dir = create_temp_dir()?;
    let index_dir = tmp_dir.path().join("indexDir");
    let dir = case.get_directory(&mut random, index_dir.clone())?;
    let mut permissions = fs::metadata(&index_dir)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&index_dir, permissions)?;

    let result = dir.obtain_lock("test.lock");

    assert!(result.is_err());

    let dir = dir;
    dir.close()?;
    Ok(())
  }
}

mod base_lock_factory_test_case_tests {
  use super::TestNativeFSLockFactory;
  use crate::core::util::error::lucene_error::Result;
  use crate::test_framework::core::store::base_lock_factory_test_case::BaseLockFactoryTestCase;
  use crate::test_framework::core::util::lucene_test_case::random;

  #[test]
  fn test_basics() -> Result<()> {
    let case = TestNativeFSLockFactory;
    let mut random = random();
    case.test_basics(&mut random)
  }

  #[test]
  fn test_double_close() -> Result<()> {
    let case = TestNativeFSLockFactory;
    let mut random = random();
    case.test_double_close(&mut random)
  }

  #[test]
  fn test_valid_after_acquire() -> Result<()> {
    let case = TestNativeFSLockFactory;
    let mut random = random();
    case.test_valid_after_acquire(&mut random)
  }

  #[test]
  fn test_invalid_after_close() -> Result<()> {
    let case = TestNativeFSLockFactory;
    let mut random = random();
    case.test_invalid_after_close(&mut random)
  }

  #[test]
  fn test_obtain_concurrently() -> Result<()> {
    let case = TestNativeFSLockFactory;
    let mut random = random();
    case.test_obtain_concurrently(&mut random)
  }

  #[test]
  fn test_stress_locks() -> Result<()> {
    let case = TestNativeFSLockFactory;
    let mut random = random();
    case.test_stress_locks(&mut random)
  }
}
