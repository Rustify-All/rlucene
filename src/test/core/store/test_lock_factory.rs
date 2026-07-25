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
  new_index_writer_config, new_text_field, random,
};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::core::document::document::Document;
use crate::core::document::field::Store;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::{IndexWriterConfig, OpenMode};
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::store::lock::{Lock, LockEnum};
use crate::core::store::lock_factory::LockFactory;
use crate::core::store::{ByteBuffersDirectory, NoLockFactory};
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::store::mock_directory_wrapper::MockDirectoryWrapper;

#[allow(dead_code)] // for quick search
struct TestLockFactory;

// Verify: we can provide our own LockFactory implementation, the right
// methods are called at the right time, locks are created, etc.

#[test]
fn test_custom_lock_factory() -> Result<()> {
  let mut random = random();
  let lf = MockLockFactory::new();
  let dir = MockDirectoryWrapper::new(
    &mut random,
    ByteBuffersDirectory::with_lock_factory(lf.clone()),
  );

  let analyzer = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let writer = IndexWriter::new(Arc::new(dir), iwc)?;

  // add 100 documents (so that commit lock is used)
  let mut field_to_type = HashMap::new();
  for _ in 0..100 {
    add_doc(&writer, &mut random, &mut field_to_type)?;
  }

  // Both write lock and commit lock should have been created:
  assert_eq!(
    lf.locks_created().len(),
    1,
    "# of unique locks created (after instantiating IndexWriter)"
  );
  writer.close()?;
  Ok(())
}

// Verify: we can use the NoLockFactory w/ no errors raised.
// Verify: NoLockFactory allows two IndexWriters
#[test]
fn test_directory_no_locking() -> Result<()> {
  let mut random = random();
  let dir = MockDirectoryWrapper::new(
    &mut random,
    ByteBuffersDirectory::with_lock_factory(NoLockFactory),
  );
  let dir = Arc::new(dir);

  let analyzer = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let writer = IndexWriter::new(dir.clone(), iwc)?;
  writer.commit()?; // required so the second open succeed

  // Create a 2nd IndexWriter. This is normally not allowed but it should
  // run through since we're not using any locks:
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc2 = IndexWriterConfig::with_analyzer(analyzer)?;
  iwc2.set_open_mode(OpenMode::Append);
  let writer2 = IndexWriter::new(dir, iwc2);
  match writer2 {
    Ok(writer2) => {
      writer.close()?;
      writer2.close()?;
    },
    Err(e) => {
      writer.close()?;
      panic!(
        "Should not have hit an IOException with no locking: {:?}",
        e
      );
    },
  }
  Ok(())
}

/// A mock `Lock` that does nothing.
struct MockLock;

impl Display for MockLock {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "MockLock")
  }
}

impl CloseableRef for MockLock {
  fn close(&self) -> Result<()> {
    Ok(())
  }
}

impl Lock for MockLock {
  fn ensure_valid(&self) -> Result<()> {
    Ok(())
  }
}

/// A mock `LockFactory` that tracks created lock names.
#[derive(Clone)]
struct MockLockFactory {
  locks_created: Arc<Mutex<HashMap<String, ()>>>,
}

impl MockLockFactory {
  fn new() -> Self {
    Self {
      locks_created: Arc::new(Mutex::new(HashMap::new())),
    }
  }

  fn locks_created(&self) -> std::sync::MutexGuard<'_, HashMap<String, ()>> {
    self.locks_created.lock().unwrap()
  }
}

impl Display for MockLockFactory {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "MockLockFactory")
  }
}

impl LockFactory for MockLockFactory {
  type Lock = LockEnum;

  fn obtain_lock(&self, _dir: &Path, lock_name: &str) -> Result<Self::Lock> {
    self
      .locks_created
      .lock()
      .unwrap()
      .insert(lock_name.to_string(), ());
    Ok(LockEnum::custom(MockLock))
  }
}

fn add_doc(
  writer: &IndexWriter<impl crate::core::store::directory::Directory + 'static>,
  random: &mut impl rand::Rng,
  field_to_type: &mut HashMap<String, crate::core::document::field_type::FieldType>,
) -> Result<()> {
  let mut doc = Document::new();
  doc.add(new_text_field(
    random,
    "content",
    "aaa",
    Store::No,
    field_to_type,
  )?);
  writer.add_document(doc)?;
  Ok(())
}
