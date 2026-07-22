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
use crate::core::index::index_reader::Identity;
use crate::core::index::segment_infos::SegmentInfos;
use crate::core::index::{CODEC_FILE_PATTERN, IndexFileNames, directory_reader};
use crate::core::store::buffered_checksum_index_input::BufferedChecksumIndexInput;
use crate::core::store::directory::Directory;
use crate::core::store::index_input::IndexInput;
use crate::core::store::{
  Context, DataInput, DataOutput, IOContext, IndexOutput, IndexOutputEnum2, ReadAdvice,
};
use crate::core::util::HasIdentity;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::io_utils::IOUtils;
use crate::test_framework::core::store::base_directory_wrapper::BaseDirectoryWrapper;
use crate::test_framework::core::store::mock_index_input_wrapper::{
  MockDirectoryIndexInput, MockIndexInputWrapper,
};
use crate::test_framework::core::store::mock_index_output_wrapper::{
  MockIndexOutputHandle, MockIndexOutputWrapper,
};
use crate::test_framework::core::util::lucene_test_case::{
  is_night_mode, new_io_context, new_io_context_with_default,
};
use crate::test_framework::core::util::test_util::TestUtil;
use crate::test_framework::core::util::throttled_index_output::ThrottledIndexOutput;
use parking_lot::Mutex;
use rand::prelude::StdRng;
use rand::{Rng, RngExt, SeedableRng};
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::io::{Error, ErrorKind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, Ordering};
use std::thread;
use std::time::Duration;

#[derive(Clone, Copy)]
pub(crate) struct ThrottledOutputTemplate {
  bytes_per_second: i32,
  delay_in_millis: i64,
}

impl ThrottledOutputTemplate {
  fn new(random_state: &mut StdRng) -> Self {
    let mbits: i32 = 40 + random_state.random_range(0..10);
    Self {
      bytes_per_second: mbits.wrapping_mul(125_000_000),
      delay_in_millis: 1 + random_state.random_range(0..5) as i64,
    }
  }

  fn new_from_delegate<O>(&self, output: O) -> ThrottledIndexOutput<O>
  where
    O: IndexOutput,
  {
    ThrottledIndexOutput::new(self.bytes_per_second, self.delay_in_millis, output)
  }
}

#[derive(Clone, Copy)]
pub enum Throttling {
  /// always emulate a slow hard disk. could be very slow!
  Always,
  /// sometimes (0.5% of the time) emulate a slow hard disk.
  Sometimes,
  /// never throttle output
  Never,
}

#[derive(Debug)]
pub(crate) struct FakeIOException;

impl Display for FakeIOException {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "a fake IOException")
  }
}

impl std::error::Error for FakeIOException {}

pub(crate) trait Failure<D>: Send
where
  D: Directory,
{
  /// Called at each potential failure point.
  fn eval(&mut self, _dir: &MockDirectoryWrapper<D>) -> Result<()> {
    Ok(())
  }

  /// reset should set the state of the failure to its default (freshly
  /// constructed) state. Reset is convenient for tests that want to create one
  /// failure object and then reuse it in multiple cases. This, combined with
  /// the fact that Failure implementations are often anonymous implementations
  /// makes reset difficult to do otherwise.
  ///
  /// A typical example of use is to create a boxed failure, reset it, and
  /// then pass it to `MockDirectoryWrapper::fail_on`.
  fn reset(&mut self) {}

  fn set_do_fail(&mut self);

  fn clear_do_fail(&mut self);
}

pub(crate) struct MockDirectoryWrapperState<D>
where
  D: Directory,
{
  pub base: Mutex<BaseDirectoryWrapper<D>>,
  pub id: Identity,
  pub max_size: AtomicI64,

  // Max actual bytes used. This is set by MockIndexOutputWrapper.
  pub max_used_size: AtomicI64,
  pub random_io_exception_rate: Mutex<f64>,
  pub random_io_exception_rate_on_open: Mutex<f64>,
  pub random_state: Mutex<StdRng>,
  pub assert_no_delete_open_file: AtomicBool,
  pub track_disk_usage: AtomicBool,
  pub use_slow_open_closers: AtomicBool,
  pub allow_random_file_not_found_exception: AtomicBool,
  pub allow_reading_files_still_open_for_write: AtomicBool,
  pub unsynced_files: Mutex<HashSet<String>>,
  pub created_files: Mutex<HashSet<String>>,
  pub open_files_for_write: Mutex<HashSet<String>>,
  pub open_locks: Mutex<HashMap<String, LuceneError>>,
  pub crashed: AtomicBool,
  pub throttled_output: Mutex<ThrottledOutputTemplate>,
  pub throttling: Mutex<Throttling>,

  // for testing
  pub always_corrupt: AtomicBool,

  pub input_clone_count: AtomicI32,

  // use this for tracking files for crash.
  // additionally: provides debugging information in case you leave one open
  pub open_file_handles: Mutex<HashMap<usize, LuceneError>>,

  pub open_output_handles: Mutex<HashMap<usize, MockIndexOutputHandle<D::IndexOutput>>>,

  pub open_files: Mutex<HashMap<String, i32>>,

  // Only tracked if noDeleteOpenFile is true: if an attempt
  // is made to delete an open file, we enroll it here.
  pub open_files_deleted: Mutex<HashSet<String>>,

  pub verbose_clone: AtomicBool,
  pub fail_on_create_output: AtomicBool,
  pub fail_on_open_input: AtomicBool,
  pub assert_no_unreferenced_files_on_close: AtomicBool,
  pub failures: Mutex<Vec<Box<dyn Failure<D>>>>,
}

/// This is a Directory Wrapper that adds methods intended to be used only by
/// unit tests. It also adds a number of features useful for testing:
///
/// * Instances created by `LuceneTestCase::newDirectory()` are tracked to
///   ensure they are closed by the test.
/// * When a MockDirectoryWrapper is closed, it will throw an exception if it
///   has any open files against it (with a stacktrace indicating where they
///   were opened from).
/// * When a MockDirectoryWrapper is closed, it runs CheckIndex to test if the
///   index was corrupted.
/// * MockDirectoryWrapper simulates some "features" of Windows, such as
///   refusing to write/delete to open files.
pub(crate) struct MockDirectoryWrapper<D>
where
  D: Directory,
{
  pub state: Arc<MockDirectoryWrapperState<D>>,
}

impl<D> Clone for MockDirectoryWrapper<D>
where
  D: Directory,
{
  fn clone(&self) -> Self {
    Self {
      state: Arc::clone(&self.state),
    }
  }
}

impl<D> MockDirectoryWrapper<D>
where
  D: Directory,
{
  pub fn new<R>(random: &mut R, delegate: D) -> Self
  where
    R: Rng + ?Sized,
  {
    // must make a private random since our methods are
    // called from different threads; else test failures may
    // not be reproducible from the original seed
    let mut random_state = StdRng::seed_from_u64(random.random::<u64>());
    let throttled_output = ThrottledOutputTemplate::new(&mut random_state);
    let test_nightly = is_night_mode();
    Self {
      state: Arc::new(MockDirectoryWrapperState {
        base: Mutex::new(BaseDirectoryWrapper::new(&mut random_state, delegate)),
        id: Identity::new(),
        max_size: AtomicI64::new(0),
        max_used_size: AtomicI64::new(0),
        random_io_exception_rate: Mutex::new(0.0),
        random_io_exception_rate_on_open: Mutex::new(0.0),
        random_state: Mutex::new(random_state),
        assert_no_delete_open_file: AtomicBool::new(false),
        track_disk_usage: AtomicBool::new(false),
        use_slow_open_closers: AtomicBool::new(test_nightly),
        allow_random_file_not_found_exception: AtomicBool::new(true),
        allow_reading_files_still_open_for_write: AtomicBool::new(false),
        unsynced_files: Mutex::new(HashSet::new()),
        created_files: Mutex::new(HashSet::new()),
        open_files_for_write: Mutex::new(HashSet::new()),
        open_locks: Mutex::new(HashMap::new()),
        crashed: AtomicBool::new(false),
        throttled_output: Mutex::new(throttled_output),
        throttling: Mutex::new(if test_nightly {
          Throttling::Sometimes
        } else {
          Throttling::Never
        }),
        always_corrupt: AtomicBool::new(false),
        input_clone_count: AtomicI32::new(0),
        open_file_handles: Mutex::new(HashMap::new()),
        open_output_handles: Mutex::new(HashMap::new()),
        open_files: Mutex::new(HashMap::new()),
        open_files_deleted: Mutex::new(HashSet::new()),
        verbose_clone: AtomicBool::new(false),
        fail_on_create_output: AtomicBool::new(true),
        fail_on_open_input: AtomicBool::new(true),
        assert_no_unreferenced_files_on_close: AtomicBool::new(false),
        failures: Mutex::new(Vec::new()),
      }),
    }
  }

  pub fn get_input_clone_count(&self) -> i32 {
    self.state.input_clone_count.load(Ordering::SeqCst)
  }

  pub fn is_open(&self) -> bool {
    self.state.base.lock().is_open()
  }

  /// Set whether or not checkindex should be run on close.
  pub fn set_check_index_on_close(&self, value: bool) {
    self.state.base.lock().set_check_index_on_close(value);
  }

  pub fn get_check_index_on_close(&self) -> bool {
    self.state.base.lock().get_check_index_on_close()
  }

  pub fn set_cross_check_term_vectors_on_close(&self, value: bool) {
    self
      .state
      .base
      .lock()
      .set_cross_check_term_vectors_on_close(value);
  }

  pub fn get_level_for_check_on_close(&self) -> i32 {
    self.state.base.lock().get_level_for_check_on_close()
  }

  /// If set to true, we print a fake exception with filename and stacktrace on
  /// every indexinput clone()
  pub fn set_verbose_clone(&self, v: bool) {
    self.state.verbose_clone.store(v, Ordering::SeqCst);
  }

  pub fn set_track_disk_usage(&self, v: bool) {
    self.state.track_disk_usage.store(v, Ordering::SeqCst);
  }

  /// If set to true (the default), when we throw random IOException on
  /// openInput or createOutput, we may sometimes throw FileNotFoundException or
  /// NoSuchFileException.
  pub fn set_allow_random_file_not_found_exception(&self, value: bool) {
    self
      .state
      .allow_random_file_not_found_exception
      .store(value, Ordering::SeqCst);
  }

  /// If set to true, you can open an inputstream on a file that is still open
  /// for writes.
  pub fn set_allow_reading_files_still_open_for_write(&self, value: bool) {
    self
      .state
      .allow_reading_files_still_open_for_write
      .store(value, Ordering::SeqCst);
  }

  /// Enum for controlling hard disk throttling. Set via `set_throttling`.
  ///
  /// WARNING: can make tests very slow.
  pub fn set_throttling(&self, throttling: Throttling) {
    *self.state.throttling.lock() = throttling;
  }

  /// Add a rare small sleep to catch race conditions in open/close
  ///
  /// You can enable this if you need it.
  pub fn set_use_slow_open_closers(&self, v: bool) {
    self.state.use_slow_open_closers.store(v, Ordering::SeqCst);
  }

  pub fn size_in_bytes(&self) -> Result<usize> {
    let mut size = 0;
    let base = self.state.base.lock();
    for file in base.get_delegate().list_all()? {
      // hack 2: see TODO in ExtrasFS (ideally it would always return 0 byte
      // size for extras it creates, even though the size of non-regular files
      // is not defined)
      if !file.starts_with("extra") {
        size += base.get_delegate().file_length(&file)?;
      }
    }
    Ok(size)
  }

  pub fn corrupt_unknown_files(&self) -> Result<()> {
    if cfg!(feature = "test_log_verbose") {
      eprintln!("MDW: corrupt unknown files");
    }
    let mut known_files = HashSet::new();
    for file_name in self.list_all()? {
      if file_name.starts_with(IndexFileNames::SEGMENTS) {
        if cfg!(feature = "test_log_verbose") {
          eprintln!("MDW: read {file_name} to gather files it references");
        }
        let infos = SegmentInfos::read_commit(Arc::new(self.clone()), &file_name)?;
        known_files.extend(infos.files(true)?);
      }
    }

    let mut to_corrupt = HashSet::new();
    for file_name in self.list_all()? {
      if !known_files.contains(&file_name)
        && !file_name.ends_with("write.lock")
        && (CODEC_FILE_PATTERN.is_match(&file_name)
          || file_name.starts_with(IndexFileNames::PENDING_SEGMENTS))
      {
        to_corrupt.insert(file_name);
      }
    }

    self.corrupt_files(to_corrupt)
  }

  pub fn corrupt_files(&self, files: impl IntoIterator<Item = String>) -> Result<()> {
    self._corrupt_files(files)
  }

  fn _corrupt_files(&self, files: impl IntoIterator<Item = String>) -> Result<()> {
    // TODO: we should also mess with any recent file renames, file deletions,
    // if syncMetaData was not called!!

    // Must make a copy because we change the incoming unsyncedFiles
    // when we create temp files, delete, etc., below:
    let mut files_to_corrupt: Vec<String> = files.into_iter().collect();
    // sort the files otherwise we have reproducibility issues
    // across JVMs if the incoming collection is a hashSet etc.
    files_to_corrupt.sort();
    for name in files_to_corrupt {
      let mut damage = self.state.random_state.lock().random_range(0..6);
      if self.state.always_corrupt.load(Ordering::SeqCst) && damage == 3 {
        damage = 4;
      }
      let action: String;

      match damage {
        0 => {
          action = "deleted".to_string();
          self.delete_file(&name)?;
        },

        1 => {
          action = "zeroed".to_string();
          // Zero out file entirely
          let length = self.file_length(&name)?;

          // Delete original and write zeros back:
          self.delete_file(&name)?;

          let zeroes = [0u8; 256];
          let mut upto = 0;
          let mut out = self.state.base.lock().get_delegate().create_output(
            &name,
            &new_io_context(&mut *self.state.random_state.lock())?,
          )?;
          let result = (|| -> Result<()> {
            while upto < length {
              let limit = (length - upto).min(zeroes.len());
              out.write_bytes_range(&zeroes, 0, limit)?;
              upto += limit;
            }
            Ok(())
          })();
          IOUtils::use_or_suppress_result(result, out.close())?;
        },

        2 => {
          action = "partially truncated".to_string();
          // Partially Truncate the file:

          // First, make temp file and copy only half this
          // file over:
          let temp_file_name = (|| -> Result<String> {
            let base = self.state.base.lock();
            let mut temp_out = base.get_delegate().create_temp_output(
              "name",
              "mdw_corrupt",
              &new_io_context(&mut *self.state.random_state.lock())?,
            )?;
            let mut ii = match base.get_delegate().open_input(
              &name,
              &new_io_context(&mut *self.state.random_state.lock())?,
            ) {
              Ok(ii) => ii,
              Err(error) => {
                return IOUtils::use_or_suppress_result(Err(error), temp_out.close());
              },
            };
            let temp_file_name = temp_out.get_name().to_string();
            let result = (|| -> Result<String> {
              let length = ii.length()? / 2;
              temp_out.copy_bytes(&mut ii, length)?;
              Ok(temp_file_name)
            })();
            let result = IOUtils::use_or_suppress_result(result, ii.close());
            IOUtils::use_or_suppress_result(result, temp_out.close())
          })()?;

          // Delete original and copy bytes back:
          self.delete_file(&name)?;

          (|| -> Result<()> {
            let base = self.state.base.lock();
            let mut out = base.get_delegate().create_output(
              &name,
              &new_io_context(&mut *self.state.random_state.lock())?,
            )?;
            let mut ii = match base.get_delegate().open_input(
              &temp_file_name,
              &new_io_context(&mut *self.state.random_state.lock())?,
            ) {
              Ok(ii) => ii,
              Err(error) => return IOUtils::use_or_suppress_result(Err(error), out.close()),
            };
            let result = (|| -> Result<()> {
              let length = ii.length()?;
              out.copy_bytes(&mut ii, length)
            })();
            let result = IOUtils::use_or_suppress_result(result, ii.close());
            IOUtils::use_or_suppress_result(result, out.close())
          })()?;
          self.delete_file(&temp_file_name)?;
        },

        3 => {
          // The file survived intact:
          action = "didn't change".to_string();
        },

        4 => {
          // Corrupt one bit randomly in the file:
          let mut action_text = "didn't change".to_string();
          let temp_file_name = (|| -> Result<String> {
            let base = self.state.base.lock();
            let mut temp_out = base.get_delegate().create_temp_output(
              "name",
              "mdw_corrupt",
              &new_io_context(&mut *self.state.random_state.lock())?,
            )?;
            let mut ii = match base.get_delegate().open_input(
              &name,
              &new_io_context(&mut *self.state.random_state.lock())?,
            ) {
              Ok(ii) => ii,
              Err(error) => {
                return IOUtils::use_or_suppress_result(Err(error), temp_out.close());
              },
            };
            let temp_file_name = temp_out.get_name().to_string();
            let result = (|| -> Result<String> {
              let length = ii.length()?;
              if length > 0 {
                // Copy first part unchanged:
                let byte_to_corrupt =
                  (self.state.random_state.lock().random::<f64>() * length as f64) as usize;
                if byte_to_corrupt > 0 {
                  temp_out.copy_bytes(&mut ii, byte_to_corrupt)?;
                }

                // Randomly flip one bit from this byte:
                let mut b = ii.read_byte()?;
                let bit_to_flip = self.state.random_state.lock().random_range(0..8);
                b ^= 1 << bit_to_flip;
                temp_out.write_byte(b)?;

                action_text =
                  format!("flip bit {bit_to_flip} of byte {byte_to_corrupt} out of {length} bytes");

                // Copy last part unchanged:
                let bytes_left = length - byte_to_corrupt - 1;
                if bytes_left > 0 {
                  temp_out.copy_bytes(&mut ii, bytes_left)?;
                }
              }
              Ok(temp_file_name)
            })();
            let result = IOUtils::use_or_suppress_result(result, ii.close());
            IOUtils::use_or_suppress_result(result, temp_out.close())
          })()?;

          // Delete original and copy bytes back:
          self.delete_file(&name)?;

          (|| -> Result<()> {
            let base = self.state.base.lock();
            let mut out = base.get_delegate().create_output(
              &name,
              &new_io_context(&mut *self.state.random_state.lock())?,
            )?;
            let mut ii = match base.get_delegate().open_input(
              &temp_file_name,
              &new_io_context(&mut *self.state.random_state.lock())?,
            ) {
              Ok(ii) => ii,
              Err(error) => return IOUtils::use_or_suppress_result(Err(error), out.close()),
            };
            let result = (|| -> Result<()> {
              let length = ii.length()?;
              out.copy_bytes(&mut ii, length)
            })();
            let result = IOUtils::use_or_suppress_result(result, ii.close());
            IOUtils::use_or_suppress_result(result, out.close())
          })()?;

          self.delete_file(&temp_file_name)?;
          action = action_text;
        },

        5 => {
          action = "fully truncated".to_string();
          // Totally truncate the file to zero bytes
          self.delete_file(&name)?;

          let mut out = self.state.base.lock().get_delegate().create_output(
            &name,
            &new_io_context(&mut *self.state.random_state.lock())?,
          )?;
          let result = out.get_file_pointer().map(|_| ()); // just fake access to prevent compiler warning
          IOUtils::use_or_suppress_result(result, out.close())?;
        },

        _ => {
          return Err(LuceneError::illegal_state("unexpected corruption action"));
        },
      }

      if cfg!(feature = "test_log_verbose") {
        eprintln!("MockDirectoryWrapper: {action} unsynced file: {name}");
      }
    }
    Ok(())
  }

  /// Simulates a crash of OS or machine by overwriting unsynced files.
  pub fn crash(&self) -> Result<()> {
    self.state.open_files.lock().clear();
    self.state.open_files_for_write.lock().clear();
    self.state.open_files_deleted.lock().clear();
    // First force-close all output files, so we can corrupt them on Windows
    // and in in-memory directories whose content is published on close.
    let open_output_handles: Vec<_> = self
      .state
      .open_output_handles
      .lock()
      .iter()
      .map(|(handle_id, handle)| (*handle_id, handle.clone()))
      .collect();
    for (handle_id, handle) in open_output_handles {
      let _ = MockIndexOutputWrapper::<D>::force_close(self, handle_id, &handle);
    }
    self.state.open_output_handles.lock().clear();
    self.state.open_file_handles.lock().clear();
    let unsynced_files = self.state.unsynced_files.lock().clone();
    self.corrupt_files(unsynced_files)?;
    self.state.crashed.store(true, Ordering::SeqCst);
    self.state.unsynced_files.lock().clear();
    Ok(())
  }

  pub fn clear_crash(&self) {
    self.state.crashed.store(false, Ordering::SeqCst);
    self.state.open_locks.lock().clear();
  }

  pub fn set_max_size_in_bytes(&self, max_size: i64) {
    self.state.max_size.store(max_size, Ordering::SeqCst);
  }

  pub fn get_max_size_in_bytes(&self) -> i64 {
    self.state.max_size.load(Ordering::SeqCst)
  }

  /// Returns the peek actual storage used (bytes) in this directory.
  pub fn get_max_used_size_in_bytes(&self) -> i64 {
    self.state.max_used_size.load(Ordering::SeqCst)
  }

  pub fn reset_max_used_size_in_bytes(&self) -> Result<()> {
    self
      .state
      .max_used_size
      .store(self.size_in_bytes()? as i64, Ordering::SeqCst);
    Ok(())
  }

  /// Trip a test assert if there is an attempt to delete an open file.
  pub fn set_assert_no_delete_open_file(&self, value: bool) {
    self
      .state
      .assert_no_delete_open_file
      .store(value, Ordering::SeqCst);
  }

  pub fn get_assert_no_delete_open_file(&self) -> bool {
    self.state.assert_no_delete_open_file.load(Ordering::SeqCst)
  }

  /// If 0.0, no exceptions will be thrown. Else this should be a double 0.0 -
  /// 1.0. We will randomly throw an IOException on the first write to an
  /// OutputStream based on this probability.
  pub fn set_random_io_exception_rate(&self, rate: f64) {
    *self.state.random_io_exception_rate.lock() = rate;
  }

  pub fn get_random_io_exception_rate(&self) -> f64 {
    *self.state.random_io_exception_rate.lock()
  }

  /// If 0.0, no exceptions will be thrown during openInput and createOutput.
  /// Else this should be a double 0.0 - 1.0 and we will randomly throw an
  /// IOException in openInput and createOutput with this probability.
  pub fn set_random_io_exception_rate_on_open(&self, rate: f64) {
    *self.state.random_io_exception_rate_on_open.lock() = rate;
  }

  pub fn get_random_io_exception_rate_on_open(&self) -> f64 {
    *self.state.random_io_exception_rate_on_open.lock()
  }

  pub fn maybe_throw_io_exception(&self, message: Option<&str>) -> Result<()> {
    if self.state.random_state.lock().random::<f64>() < *self.state.random_io_exception_rate.lock()
    {
      let message = format!(
        "a random IOException{}",
        message.map(|m| format!(" ({m})")).unwrap_or_default()
      );
      if cfg!(feature = "test_log_verbose") {
        eprintln!("MockDirectoryWrapper: now throw random exception");
      }
      return Err(LuceneError::io(Error::other(message)));
    }
    Ok(())
  }

  pub fn maybe_throw_io_exception_on_open(&self, name: &str) -> Result<()> {
    if self.state.random_state.lock().random::<f64>()
      < *self.state.random_io_exception_rate_on_open.lock()
    {
      if cfg!(feature = "test_log_verbose") {
        eprintln!("MockDirectoryWrapper: now throw random exception during open file={name}");
      }
      if !self
        .state
        .allow_random_file_not_found_exception
        .load(Ordering::SeqCst)
        || self.state.random_state.lock().random_bool(0.5)
      {
        Err(LuceneError::io(Error::other(format!(
          "a random IOException ({name})"
        ))))
      } else {
        Err(LuceneError::io_with_path(
          name,
          Error::new(
            ErrorKind::NotFound,
            format!("a random IOException ({name})"),
          ),
        ))
      }
    } else {
      Ok(())
    }
  }

  /// returns current open file handle count
  pub fn get_file_handle_count(&self) -> usize {
    self.state.open_file_handles.lock().len()
  }

  fn maybe_yield(&self) {
    if self.state.random_state.lock().random_bool(0.5) {
      thread::yield_now();
    }
  }

  pub fn get_open_deleted_files(&self) -> HashSet<String> {
    self.state.open_files_deleted.lock().clone()
  }

  pub fn set_fail_on_create_output(&self, v: bool) {
    self.state.fail_on_create_output.store(v, Ordering::SeqCst);
  }

  fn maybe_throttle(
    &self,
    name: &str,
    output: MockIndexOutputWrapper<D>,
  ) -> IndexOutputEnum2<MockIndexOutputWrapper<D>, ThrottledIndexOutput<MockIndexOutputWrapper<D>>>
  {
    // throttling REALLY slows down tests, so don't do it very often for
    // SOMETIMES.
    let should_throttle = match *self.state.throttling.lock() {
      Throttling::Always => true,
      Throttling::Sometimes => self.state.random_state.lock().random_range(0..200) == 0,
      Throttling::Never => false,
    };
    if should_throttle {
      if cfg!(feature = "test_log_verbose") {
        eprintln!("MockDirectoryWrapper: throttling indexOutput ({name})");
      }
      IndexOutputEnum2::B(self.state.throttled_output.lock().new_from_delegate(output))
    } else {
      IndexOutputEnum2::A(output)
    }
  }

  fn add_file_handle(&self, handle_id: usize, name: &str, handle: Handle) {
    let mut open_files = self.state.open_files.lock();
    let value = open_files.entry(name.to_string()).or_insert(0);
    *value += 1;
    drop(open_files);

    self.state.open_file_handles.lock().insert(
      handle_id,
      LuceneError::illegal_state(format!("unclosed Index{}: {name}", handle.name())),
    );
  }

  pub fn set_fail_on_open_input(&self, v: bool) {
    self.state.fail_on_open_input.store(v, Ordering::SeqCst);
  }

  /// NOTE: This is off by default; see LUCENE-5574
  pub fn set_assert_no_unrefenced_files_on_close(&self, v: bool) {
    self
      .state
      .assert_no_unreferenced_files_on_close
      .store(v, Ordering::SeqCst);
  }

  fn remove_open_file(&self, handle_id: usize, name: &str) {
    let mut open_files = self.state.open_files.lock();
    // Could be absent when crash() was called
    if let Some(value) = open_files.get_mut(name) {
      if *value == 1 {
        open_files.remove(name);
      } else {
        *value -= 1;
      }
    }
    drop(open_files);

    self.state.open_file_handles.lock().remove(&handle_id);
  }

  pub fn remove_index_output(&self, handle_id: usize, name: &str) {
    self.state.open_files_for_write.lock().remove(name);
    self.state.open_output_handles.lock().remove(&handle_id);
    self.remove_open_file(handle_id, name);
  }

  pub fn remove_index_input(&self, handle_id: usize, name: &str) {
    self.remove_open_file(handle_id, name);
  }

  /// add a Failure object to the list of objects to be evaluated at every
  /// potential failure point
  pub fn fail_on(&self, fail: Box<dyn Failure<D>>) {
    self.state.failures.lock().push(fail);
  }

  /// Iterate through the failures list, giving each object a chance to throw
  /// an IOE
  pub fn maybe_throw_deterministic_exception(&self) -> Result<()> {
    let mut failures = self.state.failures.lock();
    for failure in failures.iter_mut() {
      if let Err(error) = failure.eval(self) {
        if cfg!(feature = "test_log_verbose") {
          eprintln!("MockDirectoryWrapper: throw exc");
        }
        return Err(error);
      }
    }
    Ok(())
  }
}

enum Handle {
  Input,
  Output,
  Slice,
}

impl Handle {
  fn name(&self) -> &'static str {
    match self {
      Handle::Input => "Input",
      Handle::Output => "Output",
      Handle::Slice => "Slice",
    }
  }
}

impl<D> Display for MockDirectoryWrapper<D>
where
  D: Directory,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    let max_size = self.state.max_size.load(Ordering::SeqCst);
    if max_size != 0 {
      write!(
        f,
        "MockDirectoryWrapper({}, current={},max={})",
        self.state.base.lock().get_delegate(),
        self.state.max_used_size.load(Ordering::SeqCst),
        max_size
      )
    } else {
      write!(
        f,
        "BaseDirectoryWrapper({})",
        self.state.base.lock().get_delegate()
      )
    }
  }
}

impl<D> CloseableRef for MockDirectoryWrapper<D>
where
  D: Directory,
{
  fn close(&self) -> Result<()> {
    let was_open = self.state.base.lock().is_open.swap(false, Ordering::SeqCst);
    if !was_open {
      return self.state.base.lock().in_.close();
    }

    let result = (|| -> Result<()> {
      // files that we tried to delete, but couldn't because readers were open.
      // all that matters is that we tried! (they will eventually go away)
      //   still open when we tried to delete
      self.maybe_yield();
      let open_files = self.state.open_files.lock();
      if !open_files.is_empty() {
        // print the first one as it's very verbose otherwise
        let cause = self.state.open_file_handles.lock().values().next().cloned();
        let mut error = LuceneError::illegal_state(format!(
          "MockDirectoryWrapper: cannot close: there are still {} open files: {:?}",
          open_files.len(),
          *open_files
        ));
        if let Some(cause) = cause {
          error.add_suppressed(cause);
        }
        return Err(error);
      }
      drop(open_files);
      let open_locks = self.state.open_locks.lock();
      if !open_locks.is_empty() {
        let cause = open_locks.values().next().cloned();
        let mut error = LuceneError::illegal_state(format!(
          "MockDirectoryWrapper: cannot close: there are still open locks: {:?}",
          *open_locks
        ));
        if let Some(cause) = cause {
          error.add_suppressed(cause);
        }
        return Err(error);
      }
      drop(open_locks);
      *self.state.random_io_exception_rate.lock() = 0.0;
      *self.state.random_io_exception_rate_on_open.lock() = 0.0;

      let (check_index_on_close, level_for_check_on_close) = {
        let base = self.state.base.lock();
        (
          base.get_check_index_on_close(),
          base.get_level_for_check_on_close(),
        )
      };

      if (check_index_on_close
        || self
          .state
          .assert_no_unreferenced_files_on_close
          .load(Ordering::SeqCst))
        && directory_reader::index_exists(&*self)?
      {
        if check_index_on_close {
          if cfg!(feature = "test_log_verbose") {
            eprintln!("\nNOTE: MockDirectoryWrapper: now crush");
          }
          self.crash()?; // corrupt any unsynced-files
          if cfg!(feature = "test_log_verbose") {
            eprintln!("\nNOTE: MockDirectoryWrapper: now run CheckIndex");
          }

          // Methods in MockDirectoryWrapper hold locks on this, which will
          // cause deadlock when TestUtil#checkIndex checks segment
          // concurrently using another thread, but making call back to
          // synchronized methods such as MockDirectoryWrapper#fileLength.
          // Hence passing concurrent = false to this method to turn off
          // concurrent checks.
          let mut check_index_random = {
            let mut random_state = self.state.random_state.lock();
            StdRng::seed_from_u64(random_state.random())
          };
          TestUtil::check_index_with_options(
            &mut check_index_random,
            Arc::new(self.clone()),
            level_for_check_on_close,
            true,
            false,
            None,
          )?;
        }

        // TODO: factor this out / share w/ TestIW.assertNoUnreferencedFiles
        if self
          .state
          .assert_no_unreferenced_files_on_close
          .load(Ordering::SeqCst)
          && cfg!(feature = "test_log_verbose")
        {
          eprintln!("MDW: now assert no unref'd files at close");
        }
      }
      Ok(())
    })();

    let close_result = self.state.base.lock().in_.close();
    IOUtils::use_or_suppress_result(result, close_result)
  }
}

impl<D> HasIdentity for MockDirectoryWrapper<D>
where
  D: Directory,
{
  fn identity(&self) -> &Identity {
    &self.state.id
  }
}

impl<D> Directory for MockDirectoryWrapper<D>
where
  D: Directory,
{
  fn sync(&self, names: &[String]) -> Result<()> {
    self.maybe_yield();
    self.maybe_throw_deterministic_exception()?;
    if self.state.crashed.load(Ordering::SeqCst) {
      return Err(LuceneError::io(Error::other("cannot sync after crash")));
    }
    // always pass thru fsync, directories rely on this.
    // 90% of time, we use DisableFsyncFS which omits the real calls.
    for name in names {
      // randomly fail with IOE on any file
      self.maybe_throw_io_exception(Some(name))?;
      self
        .state
        .base
        .lock()
        .get_delegate()
        .sync(std::slice::from_ref(name))?;
      self.state.unsynced_files.lock().remove(name);
    }
    Ok(())
  }

  fn rename(&self, source: &str, dest: &str) -> Result<()> {
    self.maybe_yield();
    self.maybe_throw_deterministic_exception()?;

    if self.state.crashed.load(Ordering::SeqCst) {
      return Err(LuceneError::io(Error::other("cannot rename after crash")));
    }

    if self.state.open_files.lock().contains_key(source)
      && self.state.assert_no_delete_open_file.load(Ordering::SeqCst)
    {
      return Err(LuceneError::illegal_state(format!(
        "MockDirectoryWrapper: source file \"{source}\" is still open: cannot rename"
      )));
    }

    if self.state.open_files.lock().contains_key(dest)
      && self.state.assert_no_delete_open_file.load(Ordering::SeqCst)
    {
      return Err(LuceneError::illegal_state(format!(
        "MockDirectoryWrapper: dest file \"{dest}\" is still open: cannot rename"
      )));
    }

    let mut success = false;
    let result = self.state.base.lock().get_delegate().rename(source, dest);
    if result.is_ok() {
      success = true;
    }
    if success {
      // we don't do this stuff with lucene's commit, but it's just for
      // completeness
      let mut unsynced_files = self.state.unsynced_files.lock();
      if unsynced_files.remove(source) {
        unsynced_files.insert(dest.to_string());
      }
      drop(unsynced_files);
      self.state.open_files_deleted.lock().remove(source);
      self.state.created_files.lock().remove(source);
      self.state.created_files.lock().insert(dest.to_string());
    }
    result
  }

  fn sync_metadata(&self) -> Result<()> {
    self.maybe_yield();
    self.maybe_throw_deterministic_exception()?;
    if self.state.crashed.load(Ordering::SeqCst) {
      return Err(LuceneError::io(Error::other(
        "cannot sync metadata after crash",
      )));
    }
    self.state.base.lock().get_delegate().sync_metadata()
  }

  fn delete_file(&self, name: &str) -> Result<()> {
    self.maybe_yield();

    self.maybe_throw_deterministic_exception()?;

    if self.state.crashed.load(Ordering::SeqCst) {
      return Err(LuceneError::io_with_path(
        name,
        Error::other("cannot delete after crash"),
      ));
    }

    if self.state.open_files.lock().contains_key(name) {
      self
        .state
        .open_files_deleted
        .lock()
        .insert(name.to_string());
      if self.state.assert_no_delete_open_file.load(Ordering::SeqCst) {
        return Err(LuceneError::io_with_path(
          name,
          Error::other(format!(
            "MockDirectoryWrapper: file \"{name}\" is still open: cannot delete"
          )),
        ));
      }
    } else {
      self.state.open_files_deleted.lock().remove(name);
    }

    self.state.unsynced_files.lock().remove(name);
    self.state.base.lock().get_delegate().delete_file(name)?;
    self.state.created_files.lock().remove(name);
    Ok(())
  }

  type IndexOutput =
    IndexOutputEnum2<MockIndexOutputWrapper<D>, ThrottledIndexOutput<MockIndexOutputWrapper<D>>>;

  fn create_output(&self, name: &str, context: &IOContext) -> Result<Self::IndexOutput> {
    self.maybe_throw_deterministic_exception()?;
    self.maybe_throw_io_exception_on_open(name)?;
    self.maybe_yield();
    if self.state.fail_on_create_output.load(Ordering::SeqCst) {
      self.maybe_throw_deterministic_exception()?;
    }
    if self.state.crashed.load(Ordering::SeqCst) {
      return Err(LuceneError::io_with_path(
        name,
        Error::other("cannot createOutput after crash"),
      ));
    }

    if self.state.created_files.lock().contains(name) {
      return Err(LuceneError::io_with_path(
        name,
        Error::new(
          ErrorKind::AlreadyExists,
          format!("File \"{name}\" was already written to."),
        ),
      ));
    }

    if self.state.assert_no_delete_open_file.load(Ordering::SeqCst)
      && self.state.open_files.lock().contains_key(name)
    {
      return Err(LuceneError::illegal_state(format!(
        "MockDirectoryWrapper: file \"{name}\" is still open: cannot overwrite"
      )));
    }

    self.state.unsynced_files.lock().insert(name.to_string());
    self.state.created_files.lock().insert(name.to_string());

    let randomized_context =
      new_io_context_with_default(&mut *self.state.random_state.lock(), context)?;
    let delegate_output = self
      .state
      .base
      .lock()
      .get_delegate()
      .create_output(name, &randomized_context)?;
    let io = MockIndexOutputWrapper::new(self.clone(), delegate_output, name);
    let handle_id = io.handle_id;
    self
      .state
      .open_output_handles
      .lock()
      .insert(handle_id, io.output_handle());
    self.add_file_handle(handle_id, name, Handle::Output);
    self
      .state
      .open_files_for_write
      .lock()
      .insert(name.to_string());
    Ok(self.maybe_throttle(name, io))
  }

  fn create_temp_output(
    &self,
    prefix: &str,
    suffix: &str,
    context: &IOContext,
  ) -> Result<Self::IndexOutput> {
    self.maybe_throw_deterministic_exception()?;
    self.maybe_throw_io_exception_on_open(&format!("temp: prefix={prefix} suffix={suffix}"))?;
    self.maybe_yield();
    if self.state.fail_on_create_output.load(Ordering::SeqCst) {
      self.maybe_throw_deterministic_exception()?;
    }
    if self.state.crashed.load(Ordering::SeqCst) {
      return Err(LuceneError::io(Error::other(
        "cannot createTempOutput after crash",
      )));
    }

    let randomized_context =
      new_io_context_with_default(&mut *self.state.random_state.lock(), context)?;
    let delegate_output = self.state.base.lock().get_delegate().create_temp_output(
      prefix,
      suffix,
      &randomized_context,
    )?;
    let name = delegate_output.get_name().to_string();
    if !name.to_lowercase().ends_with(".tmp") {
      return Err(LuceneError::illegal_state(format!(
        "wrapped directory failed to use .tmp extension: got: {name}"
      )));
    }

    self.state.unsynced_files.lock().insert(name.clone());
    self.state.created_files.lock().insert(name.clone());
    let io = MockIndexOutputWrapper::new(self.clone(), delegate_output, &name);
    let handle_id = io.handle_id;
    self
      .state
      .open_output_handles
      .lock()
      .insert(handle_id, io.output_handle());
    self.add_file_handle(handle_id, &name, Handle::Output);
    self.state.open_files_for_write.lock().insert(name.clone());

    Ok(self.maybe_throttle(&name, io))
  }

  type IndexInput = MockDirectoryIndexInput<D, D::IndexInput>;

  fn open_input(&self, name: &str, context: &IOContext) -> Result<Self::IndexInput> {
    self.maybe_throw_deterministic_exception()?;
    self.maybe_throw_io_exception_on_open(name)?;
    self.maybe_yield();
    if self.state.fail_on_open_input.load(Ordering::SeqCst) {
      self.maybe_throw_deterministic_exception()?;
    }
    if !self
      .state
      .base
      .lock()
      .get_delegate()
      .list_all()?
      .iter()
      .any(|file| file == name)
    {
      return Err(LuceneError::io_with_path(
        name,
        Error::new(ErrorKind::NotFound, format!("{name} in dir={}", self)),
      ));
    }

    // cannot open a file for input if it's still open for output.
    if !self
      .state
      .allow_reading_files_still_open_for_write
      .load(Ordering::SeqCst)
      && self.state.open_files_for_write.lock().contains(name)
    {
      return Err(LuceneError::io_with_path(
        name,
        Error::new(
          ErrorKind::PermissionDenied,
          format!("MockDirectoryWrapper: file \"{name}\" is still open for writing"),
        ),
      ));
    }

    // record the read advice before randomizing the context
    let read_advice = *context.get_read_advice();
    let randomized_context =
      new_io_context_with_default(&mut *self.state.random_state.lock(), context)?;
    let confined = randomized_context.context == Context::Default
      && *randomized_context.get_read_advice() == ReadAdvice::Sequential;
    if name.starts_with(IndexFileNames::SEGMENTS) && !confined {
      return Err(LuceneError::illegal_state(format!(
        "MockDirectoryWrapper: opening segments file [{name}] with a non-READONCE context[{randomized_context:?}]"
      )));
    }
    let delegate_input = self
      .state
      .base
      .lock()
      .get_delegate()
      .open_input(name, &randomized_context)?;

    let ii = {
      let random_int = self.state.random_state.lock().random_range(0..500);
      if self.state.use_slow_open_closers.load(Ordering::SeqCst) && random_int == 0 {
        if cfg!(feature = "test_log_verbose") {
          eprintln!("MockDirectoryWrapper: using SlowClosingMockIndexInputWrapper for file {name}");
        }
        MockDirectoryIndexInput::SlowClosing(MockIndexInputWrapper::new(
          self.clone(),
          name,
          delegate_input,
          None,
          read_advice,
          confined,
        ))
      } else if self.state.use_slow_open_closers.load(Ordering::SeqCst) && random_int == 1 {
        if cfg!(feature = "test_log_verbose") {
          eprintln!("MockDirectoryWrapper: using SlowOpeningMockIndexInputWrapper for file {name}");
        }
        thread::sleep(Duration::from_millis(50));
        MockDirectoryIndexInput::SlowOpening(MockIndexInputWrapper::new(
          self.clone(),
          name,
          delegate_input,
          None,
          read_advice,
          confined,
        ))
      } else {
        MockDirectoryIndexInput::Mock(MockIndexInputWrapper::new(
          self.clone(),
          name,
          delegate_input,
          None,
          read_advice,
          confined,
        ))
      }
    };
    let handle_id = match &ii {
      MockDirectoryIndexInput::Mock(inner)
      | MockDirectoryIndexInput::SlowClosing(inner)
      | MockDirectoryIndexInput::SlowOpening(inner) => inner.handle_id,
    };
    self.add_file_handle(handle_id, name, Handle::Input);
    Ok(ii)
  }

  fn open_checksum_input(
    &self,
    name: &str,
  ) -> Result<BufferedChecksumIndexInput<Self::IndexInput>> {
    Ok(BufferedChecksumIndexInput::new(
      self.open_input(name, &IOContext::read_once_io_context()?)?,
    ))
  }

  fn list_all(&self) -> Result<Vec<String>> {
    self.maybe_yield();
    self.state.base.lock().get_delegate().list_all()
  }

  fn file_length(&self, name: &str) -> Result<usize> {
    self.maybe_yield();
    self.state.base.lock().get_delegate().file_length(name)
  }

  type Lock = D::Lock;

  fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
    self.maybe_yield();
    self.state.base.lock().obtain_lock(name)
    // TODO: consider mocking locks, but not all the time, can hide bugs
  }

  fn get_pending_deletions(&self) -> Result<HashSet<String>> {
    self
      .state
      .base
      .lock()
      .get_delegate()
      .get_pending_deletions()
  }

  fn ensure_open(&self) -> Result<()> {
    self.state.base.lock().ensure_open()
  }
}
