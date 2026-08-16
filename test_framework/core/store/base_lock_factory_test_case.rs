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
use crate::core::document::field::Store;
use crate::core::document::field_type::FieldType;
use crate::core::index::directory_reader;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::OpenMode;
use crate::core::index::term::Term;
use crate::core::search::term_query::TermQuery;
use crate::core::store::IndexInput;
use crate::core::store::directory::Directory;
use crate::core::store::lock::Lock;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, create_temp_dir, new_index_writer_config_with_analyzer, new_searcher_with_reader,
  new_text_field, random_from_seed,
};
use rand::Rng;
use rand::RngExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;

/// Base test support for each `LockFactory` implementation.
pub trait BaseLockFactoryTestCase
where
  <<Self::Directory as Directory>::IndexInput as IndexInput>::RandomAccessSlice: Send + Sync,
  <<Self::Directory as Directory>::IndexInput as IndexInput>::IndexInput: Send + Sync,
{
  type Directory: Directory + Send + Sync + 'static;

  /// Implementations return the directory to test; an FS-based directory should point to
  /// the specified path, else it can ignore it.
  fn get_directory<R>(&self, random: &mut R, path: PathBuf) -> Result<Self::Directory>
  where
    R: Rng + ?Sized;

  /// Test obtaining and releasing locks, checking validity
  fn test_basics<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_path = create_temp_dir()?;
    let dir = self.get_directory(random, temp_path.path().to_path_buf())?;

    let mut l = dir.obtain_lock("commit")?;
    // shouldn't be able to get the lock twice
    assert!(matches!(
      dir.obtain_lock("commit"),
      Err(LuceneError::LockObtainFailed(_))
    ));
    l.close()?;

    // Make sure we can obtain first one again:
    l = dir.obtain_lock("commit")?;
    l.close()?;
    Ok(())
  }

  /// Test closing locks twice
  fn test_double_close<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_path = create_temp_dir()?;
    let dir = self.get_directory(random, temp_path.path().to_path_buf())?;

    let l = dir.obtain_lock("commit")?;
    l.close()?;
    l.close()?; // close again, should be no exception

    Ok(())
  }

  /// Test ensureValid returns true after acquire
  fn test_valid_after_acquire<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_path = create_temp_dir()?;
    let dir = self.get_directory(random, temp_path.path().to_path_buf())?;
    let l = dir.obtain_lock("commit")?;
    l.ensure_valid()?; // no exception
    l.close()?;
    Ok(())
  }

  /// Test ensureValid returns error after close
  fn test_invalid_after_close<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_path = create_temp_dir()?;
    let dir = self.get_directory(random, temp_path.path().to_path_buf())?;

    let l = dir.obtain_lock("commit")?;
    l.close()?;

    assert!(matches!(
      l.ensure_valid(),
      Err(LuceneError::AlreadyClosed(_))
    ));
    Ok(())
  }

  fn test_obtain_concurrently<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_path = create_temp_dir()?;
    let directory = Arc::new(self.get_directory(random, temp_path.path().to_path_buf())?);
    let running = Arc::new(AtomicBool::new(true));
    let atomic_counter = Arc::new(AtomicI32::new(0));
    let asserting_lock = Arc::new(Mutex::new(()));
    let num_threads = 2 + random.random_range(0..10);
    let runs = at_least(random, 1000);
    let barrier = Arc::new(Barrier::new(num_threads));
    let mut threads = Vec::new();

    for _ in 0..num_threads {
      let directory = directory.clone();
      let running = running.clone();
      let atomic_counter = atomic_counter.clone();
      let asserting_lock = asserting_lock.clone();
      let barrier = barrier.clone();
      threads.push(thread::spawn(move || -> Result<()> {
        barrier.wait();
        while running.load(Ordering::SeqCst) {
          if let Ok(lock) = directory.obtain_lock("foo.lock") {
            if let Ok(asserting_guard) = asserting_lock.try_lock() {
              drop(asserting_guard);
            } else {
              panic!("lock factory allowed concurrent lock ownership");
            }
            lock.close()?;
          }
          if atomic_counter.fetch_add(1, Ordering::SeqCst) + 1 > runs {
            running.store(false, Ordering::SeqCst);
          }
        }
        Ok(())
      }));
    }

    for thread in threads {
      thread.join().expect("thread panicked")?;
    }

    let directory = directory;
    directory.close()?;
    Ok(())
  }

  // Verify: do stress test, by opening IndexReaders and
  // IndexWriters over & over in 2 threads and making sure
  // no unexpected errors are raised:
  fn test_stress_locks<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let temp_path = create_temp_dir()?;
    let dir = Arc::new(self.get_directory(random, temp_path.path().to_path_buf())?);
    let field_to_type = Arc::new(Mutex::new(HashMap::new()));

    // First create a 1 doc index:
    let analyzer = MockAnalyzer::new(random);
    let mut iwc = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwc.set_open_mode(OpenMode::Create);
    let w = IndexWriter::new(dir.clone(), iwc)?;
    {
      let mut field_to_type = field_to_type.lock().expect("field_to_type mutex poisoned");
      Self::add_doc(&w, random, &mut field_to_type)?;
    }
    w.close()?;
    drop(w);

    let num_iterations = at_least(random, 20);
    let writer = WriterThread::new(
      num_iterations,
      dir.clone(),
      field_to_type.clone(),
      random.random(),
    );
    let searcher = SearcherThread::new(num_iterations, dir.clone());
    let writer_handle = writer.start();
    let searcher_handle = searcher.start();

    let writer_hit_exception = writer_handle.join().expect("writer thread panicked")?;
    let searcher_hit_exception = searcher_handle.join().expect("searcher thread panicked")?;

    assert!(
      !writer_hit_exception,
      "IndexWriter hit unexpected exceptions"
    );
    assert!(
      !searcher_hit_exception,
      "IndexSearcher hit unexpected exceptions"
    );
    Ok(())
  }

  fn add_doc<D, R>(
    writer: &IndexWriter<D>,
    random: &mut R,
    field_to_type: &mut HashMap<String, FieldType>,
  ) -> Result<()>
  where
    D: Directory + 'static,
    R: Rng + ?Sized,
  {
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
}

struct WriterThread<D> {
  num_iteration: i32,
  dir: Arc<D>,
  field_to_type: Arc<Mutex<HashMap<String, FieldType>>>,
  seed: u64,
}

impl<D> WriterThread<D>
where
  D: Directory + Send + Sync + 'static,
{
  fn new(
    num_iteration: i32,
    dir: Arc<D>,
    field_to_type: Arc<Mutex<HashMap<String, FieldType>>>,
    seed: u64,
  ) -> Self {
    Self {
      num_iteration,
      dir,
      field_to_type,
      seed,
    }
  }

  fn start(self) -> thread::JoinHandle<Result<bool>> {
    thread::spawn(move || {
      let mut random = random_from_seed(self.seed);
      let mut hit_exception = false;
      for _ in 0..self.num_iteration {
        let analyzer = MockAnalyzer::new(&mut random);
        let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
        iwc.set_open_mode(OpenMode::Append);
        let writer = match IndexWriter::new(self.dir.clone(), iwc) {
          Ok(writer) => writer,
          Err(_v) => {
            hit_exception = true;
            break;
          },
        };
        let add_result = {
          let mut field_to_type = self
            .field_to_type
            .lock()
            .expect("field_to_type mutex poisoned");
          Self::add_doc(&writer, &mut random, &mut field_to_type)
        };
        if add_result.is_err() {
          hit_exception = true;
          break;
        }
        if writer.close().is_err() {
          hit_exception = true;
          break;
        }
      }
      Ok(hit_exception)
    })
  }

  fn add_doc<R>(
    writer: &IndexWriter<D>,
    random: &mut R,
    field_to_type: &mut HashMap<String, FieldType>,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
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
}

struct SearcherThread<D> {
  num_iteration: i32,
  dir: Arc<D>,
}

impl<D> SearcherThread<D>
where
  D: Directory + 'static + std::marker::Send + Sync,
  <<D as Directory>::IndexInput as IndexInput>::RandomAccessSlice: Send + Sync,
  <D as Directory>::IndexInput: Send + Sync,
{
  fn new(num_iteration: i32, dir: Arc<D>) -> Self {
    Self { num_iteration, dir }
  }

  fn start(self) -> thread::JoinHandle<Result<bool>> {
    thread::spawn(move || {
      let mut hit_exception = false;
      let query = TermQuery::new(Term::from_text("content", "aaa"));
      for _ in 0..self.num_iteration {
        let reader = match directory_reader::open(self.dir.clone()) {
          Ok(reader) => reader,
          Err(_) => {
            hit_exception = true;
            break;
          },
        };
        let searcher = match new_searcher_with_reader(reader) {
          Ok(searcher) => searcher,
          Err(_) => {
            hit_exception = true;
            break;
          },
        };
        if searcher.search(query.clone(), 1000).is_err() {
          hit_exception = true;
          break;
        }
        if searcher.get_index_reader().close().is_err() {
          hit_exception = true;
          break;
        }
      }
      Ok(hit_exception)
    })
  }
}
