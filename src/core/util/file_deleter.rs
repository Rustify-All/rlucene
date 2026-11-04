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
use crate::core::index::IndexFileNames;
use crate::core::index::index_writer::IndexWriterDir;
use crate::core::store::directory::Directory;
use crate::core::util::IOUtils;
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind, Result};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// This struct provides ability to track the reference counts of a set of index files and delete them
/// when their counts decreased to 0.
///
/// This struct is NOT thread-safe, the user should make sure the thread-safety themselves
pub struct FileDeleter<D, M>
where
  D: Directory,
{
  ref_counts: HashMap<String, RefCount>,
  directory: Arc<IndexWriterDir<D>>,
  ///  user specified message consumer, first argument will be message type second argument will be the actual message
  messenger: Option<M>,
}
impl<D, M> FileDeleter<D, M>
where
  D: Directory,
{
  pub(crate) fn new(directory: Arc<IndexWriterDir<D>>, messenger: Option<M>) -> FileDeleter<D, M> {
    FileDeleter {
      ref_counts: HashMap::new(),
      directory,
      messenger,
    }
  }
}

impl<D, M> FileDeleter<D, M>
where
  D: Directory,
  M: Messenger,
{
  pub fn inc_ref<I>(&mut self, file_names: I) -> Result<()>
  where
    I: IntoIterator,
    I::Item: AsRef<str>,
  {
    for file in file_names {
      self.inc_ref_single(file.as_ref())?;
    }
    Ok(())
  }

  pub fn inc_ref_single(&mut self, file_name: &str) -> Result<()> {
    let rc = Self::get_ref_count_internal(&mut self.ref_counts, file_name)?;
    let count = rc.count;

    if let Some(messenger) = &self.messenger {
      messenger.accept(
        MsgType::Ref,
        &format!("IncRef \"{file_name}\": pre-incr count is {count}"),
      )?;
    }
    rc.inc_ref();
    Ok(())
  }

  /// Decrease ref counts for all provided files, delete them if ref counts down to 0, even on
  /// error. Returns the first error encountered, if any.
  pub fn dec_ref<'a, I, S>(&mut self, file_names: I) -> Result<()>
  where
    I: IntoIterator<Item = &'a S>,
    S: AsRef<str> + ?Sized + 'a,
  {
    let mut to_delete = HashSet::new();
    let dec_ref_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      IOUtils::close_with(file_names, |file_name| {
        let file_name = file_name.as_ref();
        if self.dec_ref_single(file_name)? {
          to_delete.insert(file_name);
        }
        Ok(())
      })
    }));
    let delete_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      self.delete_files(to_delete.iter().copied())
    }));
    IOUtils::use_or_suppress_caught_result(dec_ref_result, delete_result)
  }
  /// Returns true if the file should be deleted
  fn dec_ref_single(&mut self, file_name: &str) -> Result<bool> {
    let rc = Self::get_ref_count_internal(&mut self.ref_counts, file_name)?;
    let count = rc.count;
    if let Some(ref mut messenger) = self.messenger {
      messenger.accept(
        MsgType::Ref,
        &format!("DecRef \"{file_name}\": pre-decr count is {count}"),
      )?;
    }
    if rc.dec_ref() == 0 {
      self.ref_counts.remove(file_name);
      Ok(true)
    } else {
      Ok(false)
    }
  }
  fn get_ref_count_internal<'a>(
    ref_counts: &'a mut HashMap<String, RefCount>,
    file_name: &str,
  ) -> Result<&'a mut RefCount> {
    if !ref_counts.contains_key(file_name) {
      ref_counts.insert(file_name.to_string(), RefCount::new(file_name));
    }
    ref_counts.get_mut(file_name).ok_or_else(|| {
      LuceneError::illegal_state(format!(
        "reference count is missing for file \"{file_name}\""
      ))
    })
  }
  /// If the file is not yet recorded, this method will create a new RefCount object with count 0
  pub fn init_ref_count(&mut self, file_name: &str) {
    if !self.ref_counts.contains_key(file_name) {
      self
        .ref_counts
        .insert(file_name.to_string(), RefCount::new(file_name));
    }
  }
  /// Get ref count for a provided file. If the file is not yet recorded, returns 0
  pub fn get_ref_count(&self, file_name: &str) -> usize {
    self
      .ref_counts
      .get(file_name)
      .map(|rc| rc.count)
      .unwrap_or(0)
  }
  /// Get all files, some of them may have ref count 0
  pub fn get_all_files(&self) -> impl Iterator<Item = &String> {
    self.ref_counts.keys()
  }

  pub fn exists(&self, file_name: &str) -> bool {
    self
      .ref_counts
      .get(file_name)
      .map(|rc| rc.count > 0)
      .unwrap_or(false)
  }
  /// Get files that are touched but not incref'ed, borrowing their names from this deleter.
  pub fn get_unrefed_files(&self) -> Result<HashSet<&str>> {
    let mut unrefed = HashSet::new();
    for (file_name, rc) in &self.ref_counts {
      if rc.count == 0 {
        if let Some(messenger) = &self.messenger {
          messenger.accept(
            MsgType::File,
            &format!("removing unreferenced file \"{file_name}\""),
          )?;
        }
        unrefed.insert(file_name.as_str());
      }
    }
    Ok(unrefed)
  }
  /// delete only files that are unref'ed
  pub fn delete_files_if_no_ref<'a, I, S>(&self, files: I) -> Result<()>
  where
    I: IntoIterator<Item = &'a S>,
    S: AsRef<str> + ?Sized + 'a,
  {
    let mut to_delete = HashSet::new();

    for file_name in files {
      let file_name = file_name.as_ref();
      // NOTE: it's very unusual yet possible for the
      // refCount to be present and 0: it can happen if you
      // open IW on a crashed index, and it removes a bunch
      // of unref'd files, and then you add new docs / do
      // merging, and it reuses that segment name.
      // TestCrash.testCrashAfterReopen can hit this:
      if !self.exists(file_name) {
        if let Some(messenger) = &self.messenger {
          messenger.accept(
            MsgType::File,
            &format!("will delete new file \"{file_name}\""),
          )?;
        }
        to_delete.insert(file_name);
      }
    }

    self.delete_files(to_delete)
  }

  pub fn force_delete(&mut self, file_name: &str) -> Result<()> {
    self.ref_counts.remove(file_name);
    self.delete_file(file_name)
  }

  pub fn delete_file_if_no_ref(&mut self, file_name: &str) -> Result<()> {
    if !self.exists(file_name) {
      if let Some(messenger) = &self.messenger {
        messenger.accept(
          MsgType::File,
          &format!("will delete new file \"{file_name}\""),
        )?;
      }
      self.delete_file(file_name)?;
    }
    Ok(())
  }

  pub fn delete_files<'a, I, S>(&self, file_names: I) -> Result<()>
  where
    I: IntoIterator<Item = &'a S>,
    S: AsRef<str> + ?Sized + 'a,
  {
    let files: smallvec::SmallVec<[&str; 4]> = file_names.into_iter().map(AsRef::as_ref).collect();

    if let Some(messenger) = &self.messenger {
      messenger.accept(
        MsgType::File,
        &format!("now delete {} files: {:?}", files.len(), files),
      )?;
    }

    // First pass: delete any segments_N files.  We do these first to be certain stale commit points
    // are removed
    // before we remove any files they reference, in case we crash right now:
    for file_name in files
      .iter()
      .filter(|f| f.starts_with(IndexFileNames::SEGMENTS))
    {
      debug_assert!(!self.exists(file_name));
      self.delete_file(file_name)?;
    }

    // Only delete other files if we were able to remove the segments_N files; this way we never
    // leave a corrupt commit in the index even in the presense of virus checkers:
    for file_name in files
      .iter()
      .filter(|f| !f.starts_with(IndexFileNames::SEGMENTS))
    {
      debug_assert!(!self.exists(file_name));
      self.delete_file(file_name)?;
    }

    Ok(())
  }

  fn delete_file(&self, file_name: &str) -> Result<()> {
    match self.directory.delete_file(file_name) {
      Ok(_) => Ok(()),
      Err(e) => {
        if cfg!(target_os = "windows") {
          if matches!(e.kind(), LuceneErrorKind::Io { source, .. }
                  if source.kind() == std::io::ErrorKind::NotFound)
          {
            Ok(())
          } else {
            Err(e)
          }
        } else {
          Err(e)
        }
      },
    }
  }
}

/// Tracks the reference count for a single index file:
pub struct RefCount {
  // fileName used only for better assert error messages
  #[cfg(debug_assertions)]
  file_name: String,
  #[cfg(debug_assertions)]
  init_done: bool,
  count: usize,
}
impl RefCount {
  pub fn new<T>(file_name: T) -> Self
  where
    T: Into<String>,
  {
    let _file_name = file_name.into();
    Self {
      #[cfg(debug_assertions)]
      file_name: _file_name,
      #[cfg(debug_assertions)]
      init_done: false,
      count: 0,
    }
  }

  pub fn inc_ref(&mut self) -> usize {
    #[cfg(debug_assertions)]
    if !self.init_done {
      self.init_done = true;
    } else {
      debug_assert!(
        self.count > 0,
        "{}: RefCount is 0 pre-increment for file `{}`",
        std::thread::current()
          .name()
          .unwrap_or("Thread name is None"),
        self.file_name
      );
    }
    self.count = self.count.wrapping_add(1);
    self.count
  }

  pub fn dec_ref(&mut self) -> usize {
    #[cfg(debug_assertions)]
    debug_assert!(
      self.count > 0,
      "{}: RefCount is 0 pre-increment for file `{}`",
      std::thread::current()
        .name()
        .unwrap_or("Thread name is None"),
      self.file_name
    );
    self.count = self.count.wrapping_sub(1);
    self.count
  }
}

pub trait Messenger {
  fn accept(&self, msg_type: MsgType, message: &str) -> Result<()>;
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgType {
  Ref,
  File,
}
