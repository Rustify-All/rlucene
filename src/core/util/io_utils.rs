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

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use crate::core::store::directory::Directory;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};

macro_rules! record_close_result {
  ($result:expr, $errors:ident, $first_panic:ident) => {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| $result)) {
      Ok(Ok(())) => {},
      Ok(Err(e)) => {
        $errors.push(e);
      },
      Err(payload) if $errors.is_empty() && $first_panic.is_none() => {
        $first_panic = Some(payload);
      },
      Err(payload) => $errors.push(LuceneError::tragedy_from_panic(
        "panic while closing",
        payload.as_ref(),
      )),
    }
  };
}

macro_rules! finish_close_result {
  ($errors:ident, $first_panic:ident) => {{
    if let Some(payload) = $first_panic {
      std::panic::resume_unwind(payload);
    }

    let mut error = None;
    for mut current in $errors.into_iter().rev() {
      if let Some(error) = error {
        current.add_suppressed(error);
      }
      error = Some(current);
    }

    match error {
      Some(error) => Err(error),
      None => Ok(()),
    }
  }};
}

pub(crate) trait CloseableRefTuple {
  fn close_refs(self) -> Result<()>;
}

macro_rules! impl_closeable_ref_tuple {
  ($(($T:ident, $index:tt)),+) => {
    impl<'a, $($T),+> CloseableRefTuple for ($(Option<&'a $T>,)+)
    where
      $($T: CloseableRef + ?Sized + 'a),+
    {
      fn close_refs(self) -> Result<()> {
        let mut errors = Vec::new();
        let mut first_panic = None;
        $(
          if let Some(object) = self.$index {
            record_close_result!(object.close(), errors, first_panic);
          }
        )+
        finish_close_result!(errors, first_panic)
      }
    }
  };
}

impl_closeable_ref_tuple!((A, 0), (B, 1));
impl_closeable_ref_tuple!((A, 0), (B, 1), (C, 2));
impl_closeable_ref_tuple!((A, 0), (B, 1), (C, 2), (D, 3));
impl_closeable_ref_tuple!((A, 0), (B, 1), (C, 2), (D, 3), (E, 4));
impl_closeable_ref_tuple!((A, 0), (B, 1), (C, 2), (D, 3), (E, 4), (F, 5));
impl_closeable_ref_tuple!((A, 0), (B, 1), (C, 2), (D, 3), (E, 4), (F, 5), (G, 6));
impl_closeable_ref_tuple!(
  (A, 0),
  (B, 1),
  (C, 2),
  (D, 3),
  (E, 4),
  (F, 5),
  (G, 6),
  (H, 7)
);

pub struct IOUtils;
pub(crate) struct CloseWhileHandlingError {
  first_panic: Option<Box<dyn std::any::Any + Send>>,
}
impl CloseWhileHandlingError {
  pub(crate) fn new() -> Self {
    Self { first_panic: None }
  }

  pub(crate) fn close<F>(&mut self, close: F)
  where
    F: FnOnce() -> Result<()>,
  {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(close)) {
      Ok(_) => {},
      Err(payload) if self.first_panic.is_none() => {
        self.first_panic = Some(payload);
      },
      Err(_) => {},
    }
  }

  pub(crate) fn finish(self) -> Result<()> {
    if let Some(payload) = self.first_panic {
      std::panic::resume_unwind(payload);
    }
    Ok(())
  }
}

pub(crate) trait CloseWhileHandlingResource {
  fn close_while_handling_error(self, error: &mut CloseWhileHandlingError);
}

impl<T> CloseWhileHandlingResource for &mut T
where
  T: Closeable + ?Sized,
{
  fn close_while_handling_error(self, error: &mut CloseWhileHandlingError) {
    error.close(|| self.close());
  }
}

impl<T> CloseWhileHandlingResource for &T
where
  T: CloseableRef + ?Sized,
{
  fn close_while_handling_error(self, error: &mut CloseWhileHandlingError) {
    error.close(|| self.close());
  }
}

impl<T> CloseWhileHandlingResource for Option<T>
where
  T: CloseWhileHandlingResource,
{
  fn close_while_handling_error(self, error: &mut CloseWhileHandlingError) {
    if let Some(resource) = self {
      resource.close_while_handling_error(error);
    }
  }
}

macro_rules! impl_close_while_handling_resource_tuple {
  ($(($T:ident, $index:tt)),+) => {
    impl<$($T),+> CloseWhileHandlingResource for ($($T,)+)
    where
      $($T: CloseWhileHandlingResource),+
    {
      fn close_while_handling_error(self, error: &mut CloseWhileHandlingError) {
        $(self.$index.close_while_handling_error(error);)+
      }
    }
  };
}

impl_close_while_handling_resource_tuple!((A, 0), (B, 1));
impl_close_while_handling_resource_tuple!((A, 0), (B, 1), (C, 2));
impl_close_while_handling_resource_tuple!((A, 0), (B, 1), (C, 2), (D, 3));
impl_close_while_handling_resource_tuple!((A, 0), (B, 1), (C, 2), (D, 3), (E, 4));
impl_close_while_handling_resource_tuple!((A, 0), (B, 1), (C, 2), (D, 3), (E, 4), (F, 5));
impl_close_while_handling_resource_tuple!((A, 0), (B, 1), (C, 2), (D, 3), (E, 4), (F, 5), (G, 6));
impl_close_while_handling_resource_tuple!(
  (A, 0),
  (B, 1),
  (C, 2),
  (D, 3),
  (E, 4),
  (F, 5),
  (G, 6),
  (H, 7)
);

impl IOUtils {
  /// Deletes all given filesystem paths, suppressing all returned errors.
  ///
  /// Note: The `files` collection should not be empty.
  pub fn delete_paths_ignoring_exceptions<I, P>(files: I)
  where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
  {
    for file in files {
      let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        fs::remove_file(file.as_ref())
      }));
    }
  }

  /// Deletes all given filesystem paths if they exist.
  ///
  /// If more than one path cannot be deleted, the first error is returned and
  /// the following errors are added to it as suppressed errors.
  pub fn delete_files_if_exist<I, P>(files: I) -> Result<()>
  where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
  {
    Self::close(files, |file| {
      let path = file.as_ref();
      match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(LuceneError::io_with_path(
          path.to_string_lossy().to_string(),
          error,
        )),
      }
    })
  }

  /// Deletes all given files, suppressing all returned errors.
  ///
  /// Note: The `files` collection should not be empty or contain `None`.
  pub fn delete_files_ignoring_exceptions<'a, T, D>(dir: &D, files: T)
  where
    T: IntoIterator<Item = &'a String>,
    D: Directory + ?Sized,
  {
    for name in files {
      let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dir.delete_file(name)));
    }
  }
  pub fn delete_files<'a, T, D>(dir: &D, names: T) -> Result<()>
  where
    T: IntoIterator<Item = &'a String>,
    D: Directory + ?Sized,
  {
    for name in names {
      dir.delete_file(name)?;
    }
    Ok(())
  }

  /// Closes the given object.
  pub fn close_one<T>(object: &mut T) -> Result<()>
  where
    T: Closeable,
  {
    Self::close(std::iter::once(object), Closeable::close)
  }

  /// Closes all given objects.
  ///
  /// After everything is closed, the method either returns the first failure it
  /// hit while closing, or completes normally if there were no failures.
  pub fn close<I, F>(objects: I, mut close: F) -> Result<()>
  where
    I: IntoIterator,
    F: FnMut(I::Item) -> Result<()>,
  {
    let mut errors = Vec::new();
    let mut first_panic = None;
    for object in objects {
      record_close_result!(close(object), errors, first_panic);
    }
    finish_close_result!(errors, first_panic)
  }

  /// Closes the given object by shared reference.
  pub fn close_one_ref<T>(object: &T) -> Result<()>
  where
    T: CloseableRef,
  {
    Self::close_refs(std::iter::once(object))
  }

  /// Closes all given objects by shared reference.
  ///
  /// After everything is closed, the method either returns the first error it hit
  /// while closing, or completes normally if there were no errors.
  pub fn close_refs<I>(objects: I) -> Result<()>
  where
    I: IntoIterator,
    I::Item: CloseableRef,
  {
    Self::close(objects, |object| object.close())
  }

  /// Closes a tuple of different concrete types by shared reference.
  ///
  /// `None` elements are ignored. After everything is closed, the method either
  /// returns the first failure it hit while closing, or completes normally if
  /// there were no failures.
  pub(crate) fn close_refs_tuple<T>(objects: T) -> Result<()>
  where
    T: CloseableRefTuple,
  {
    objects.close_refs()
  }

  /// Closes all given objects, suppressing all returned errors.
  ///
  /// Even if a panic is raised, all given closeables are closed before the
  /// first panic is resumed.
  pub fn close_while_handling_error<I, F>(objects: I, mut close: F) -> Result<()>
  where
    I: IntoIterator,
    F: FnMut(I::Item) -> Result<()>,
  {
    let mut error = CloseWhileHandlingError::new();
    for object in objects {
      error.close(|| close(object));
    }
    error.finish()
  }

  /// Closes one or more resources of different concrete types, suppressing all
  /// returned errors.
  ///
  /// `None` resources are ignored. Even if a panic is raised, all given
  /// resources are closed before the first panic is resumed.
  pub(crate) fn close_resources_while_handling_error<T>(resources: T) -> Result<()>
  where
    T: CloseWhileHandlingResource,
  {
    let mut error = CloseWhileHandlingError::new();
    resources.close_while_handling_error(&mut error);
    error.finish()
  }

  /// Ensure that any writes to the given file are written to the storage
  /// device.
  ///
  /// # Arguments
  ///
  /// * `file_to_sync` - The path to the file or directory to sync.
  /// * `is_dir` - If `true`, the given path is a directory. On platforms
  ///   where directory syncing is unsupported (like Windows), this will be
  ///   ignored for directories.
  pub fn fsync(file_to_sync: &PathBuf, is_dir: bool) -> Result<()> {
    if is_dir {
      if cfg!(windows) {
        if !file_to_sync.exists() {
          return Err(LuceneError::not_such_file(format!(
            "Directory not found: {}",
            file_to_sync.display()
          )));
        }
        return Ok(());
      }

      let dir_file = File::options()
        .read(true)
        .open(file_to_sync)
        .map_err(|e| match e.kind() {
          io::ErrorKind::NotFound => {
            LuceneError::not_such_file(format!("Directory not found: {}", file_to_sync.display()))
          },
          _ => LuceneError::io_with_path(file_to_sync.to_string_lossy().to_string(), e),
        })?;

      if let Err(_e) = dir_file.sync_all() {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        debug_assert!(
          false,
          "On Linux and macOS, syncing a directory should not throw an error. Got: {_e}"
        );
        return Ok(());
      }
    } else {
      let file = File::options()
        .write(true)
        .open(file_to_sync)
        .map_err(|e| LuceneError::io_with_path(file_to_sync.to_string_lossy().to_string(), e))?;

      file
        .sync_all()
        .map_err(|e| LuceneError::io_with_path(file_to_sync.to_string_lossy().to_string(), e))?;
    }

    Ok(())
  }

  /// Returns the second error if the first is [`None`], otherwise adds the second
  /// as suppressed to the first and returns it.
  pub fn use_or_suppress(first: Option<LuceneError>, second: LuceneError) -> LuceneError {
    match first {
      None => second,
      Some(mut first) => {
        first.add_suppressed(second);
        first
      },
    }
  }

  /// Combines a body result with a following close result using Java
  /// try-with-resources suppression semantics.
  #[inline]
  pub fn use_or_suppress_result<T>(result: Result<T>, close_result: Result<()>) -> Result<T> {
    match (result, close_result) {
      (Ok(value), Ok(())) => Ok(value),
      (Ok(_), Err(err)) => Err(err),
      (Err(err), Ok(())) => Err(err),
      (Err(err), Err(close_err)) => Err(Self::use_or_suppress(Some(err), close_err)),
    }
  }

  /// Applies the consumer to all elements in the collection even if an error or panic occurs.
  /// The first failure is propagated and subsequent failures are suppressed.
  pub fn apply_to_all<T, F>(collection: &[T], consumer: F) -> Result<()>
  where
    F: FnMut(&T) -> Result<()>,
  {
    Self::close(collection, consumer)
  }
}
