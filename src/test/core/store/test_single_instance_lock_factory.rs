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
use crate::core::store::directory::DirEnum;
use crate::core::store::single_instance_lock_factory::SingleInstanceLockFactory;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::store::base_lock_factory_test_case::BaseLockFactoryTestCase;
use crate::test_framework::core::util::lucene_test_case::{
  new_directory_with_lock_factory, random,
};
use std::path::PathBuf;

/// Simple tests for SingleInstanceLockFactory
#[allow(dead_code)] // for quick search
struct TestSingleInstanceLockFactory;

impl BaseLockFactoryTestCase for TestSingleInstanceLockFactory {
  type Directory = DirEnum;

  fn get_directory<R>(&self, random: &mut R, _path: PathBuf) -> Result<Self::Directory>
  where
    R: rand::Rng + ?Sized,
  {
    new_directory_with_lock_factory(random, SingleInstanceLockFactory::new())
  }
}

mod single_instance_lock_factory_tests {
  use std::sync::Arc;

  use crate::core::index::index_writer::IndexWriter;
  use crate::core::index::index_writer_config::IndexWriterConfig;
  use crate::core::index::index_writer_config::OpenMode;
  use crate::core::store::ByteBuffersDirectory;
  use crate::core::store::base_directory::BaseDirectory;
  use crate::core::store::single_instance_lock_factory::SingleInstanceLockFactory;
  use crate::core::util::error::lucene_error::{LuceneError, Result};
  use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
  use crate::test_framework::core::util::lucene_test_case::{
    new_index_writer_config_with_analyzer, random,
  };

  fn assert_single_instance_lock_factory(_: &SingleInstanceLockFactory) {}

  // Verify: basic locking on single instance lock factory (can't create two IndexWriters)
  #[test]
  fn test_default_lock_factory() -> Result<()> {
    let mut random = random();
    let dir = ByteBuffersDirectory::new();

    assert_single_instance_lock_factory(&dir.get_lock_factory().lock_factory);

    let dir = Arc::new(dir);

    let analyzer = MockAnalyzer::new(&mut random);
    let config = IndexWriterConfig::with_analyzer(analyzer)?;
    let writer = IndexWriter::new(dir.clone(), config)?;

    // Create a 2nd IndexWriter. This should fail.
    let analyzer = MockAnalyzer::new(&mut random);
    let mut config = IndexWriterConfig::with_analyzer(analyzer)?;
    config.set_open_mode(OpenMode::Append);
    assert!(matches!(
      IndexWriter::new(dir, config),
      Err(LuceneError::LockObtainFailed(_))
    ));

    writer.close()?;
    Ok(())
  }
}

mod base_lock_factory_test_case_tests {
  use crate::core::util::error::lucene_error::Result;
  use crate::test::core::store::test_single_instance_lock_factory::run_case;
  use crate::test_framework::core::store::base_lock_factory_test_case::BaseLockFactoryTestCase;

  #[test]
  fn test_basics() -> Result<()> {
    run_case(|case, random| case.test_basics(random))
  }

  #[test]
  fn test_double_close() -> Result<()> {
    run_case(|case, random| case.test_double_close(random))
  }

  #[test]
  fn test_valid_after_acquire() -> Result<()> {
    run_case(|case, random| case.test_valid_after_acquire(random))
  }

  #[test]
  fn test_invalid_after_close() -> Result<()> {
    run_case(|case, random| case.test_invalid_after_close(random))
  }

  #[test]
  fn test_obtain_concurrently() -> Result<()> {
    run_case(|case, random| case.test_obtain_concurrently(random))
  }

  #[test]
  fn test_stress_locks() -> Result<()> {
    run_case(|case, random| case.test_stress_locks(random))
  }
}

fn run_case<F>(f: F) -> Result<()>
where
  F: FnOnce(&TestSingleInstanceLockFactory, &mut rand::prelude::StdRng) -> Result<()>,
{
  let mut random = random();
  let case = TestSingleInstanceLockFactory;
  f(&case, &mut random)
}
