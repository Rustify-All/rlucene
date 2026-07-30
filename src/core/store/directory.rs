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
use crate::core::index::index_reader::Identity;
#[cfg(test)]
use crate::core::store::ByteBuffersDirectory;
#[cfg(test)]
use crate::core::store::ReadAdvice;
use crate::core::store::buffered_checksum_index_input::BufferedChecksumIndexInput;
#[cfg(test)]
use crate::core::store::data_input::DataInput;
use crate::core::store::data_output::DataOutput;
#[cfg(test)]
use crate::core::store::file_switch_directory::FileSwitchDirectory;
use crate::core::store::index_input::IndexInput;
use crate::core::store::lock::{Lock, LockEnum, LockEnum2, LockEnum3};
#[cfg(test)]
use crate::core::store::lock_factory::LockFactoryEnum;
#[cfg(test)]
use crate::core::store::mmap_directory::MMapDirectory;
use crate::core::store::nio_fs_directory::NIOFSDirectory;
#[cfg(test)]
use crate::core::store::nrt_caching_directory::NRTCachingDirectory;
#[cfg(test)]
use crate::core::store::random_access_input::{RandomAccessInput, RandomAccessInputEnum2};
use crate::core::store::{
  FSDirectory, IOContext, IndexInputEnum, IndexInputEnum2, IndexInputEnum3, IndexOutput,
  IndexOutputEnum, IndexOutputEnum2, IndexOutputEnum3, NativeFSLockFactory,
};
use crate::core::util::HasIdentity;
#[cfg(test)]
use crate::core::util::clone::TryClone;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::io_utils::IOUtils;
#[cfg(test)]
use crate::test_framework::core::store::mock_directory_wrapper::MockDirectoryWrapper;
#[cfg(test)]
use crate::test_framework::core::store::raw_directory_wrapper::RawDirectoryWrapper;
use num_bigint::BigInt;
#[cfg(test)]
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
#[cfg(test)]
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

/// A `Directory` provides an abstraction layer for storing a list of files.
/// A directory contains only files (no subfolder hierarchy).
///
/// # Implementation Notes
/// Implementing types must comply with the following:
/// - A file in a directory can be created `create_output`, appended to, and
///   then closed. A file open for writing may not be available for read access
///   until the corresponding [`IndexOutput`] is closed.
/// - Once a file is created, it must only be opened for input `open_input` or
///   deleted `delete_file`. Calling `create_output` on an existing file must
///   return an error similar to `FileAlreadyExistsError`.
///
/// # Note
/// If your application requires external synchronization,
/// you should **not** synchronize on the `Directory` implementation instance
/// as this may cause deadlock.
/// Instead, use your own synchronization primitives.
///
/// # See Also
/// [`FSDirectory`]
/// [`ByteBuffersDirectory`](crate::core::store::byte_buffers_directory::ByteBuffersDirectory)
/// [`FilterDirectory`](crate::core::store::filter_directory::FilterDirectory)
pub trait Directory: Display + CloseableRef + HasIdentity + Send + Sync {
  /// Returns the names of all files stored in this directory. The output must
  /// be sorted in UTF-8 order (using `str::cmp` for comparison).
  ///
  /// # Errors
  /// Returns an `std::io::Error` in case of an I/O error.
  fn list_all(&self) -> Result<Vec<String>>;
  /// Removes an existing file in the directory.
  ///
  /// # Errors
  /// This method must return an error if `name` points to a non-existing
  /// file:
  /// - [`std::io::ErrorKind::NotFound`] for non-existing files.
  ///
  /// Returns an `std::io::Error` in case of other I/O errors.
  ///
  /// # Arguments
  /// * `name` - The name of an existing file to be removed.
  fn delete_file(&self, name: &str) -> Result<()>;

  /// Returns the byte length of a file in the directory.
  ///
  /// # Errors
  /// This method must return an error if `name` points to a non-existing
  /// file:
  /// - [`std::io::ErrorKind::NotFound`] for non-existing files.
  ///
  /// Returns an `std::io::Error` in case of other I/O errors.
  ///
  /// # Arguments
  /// * `name` - The name of an existing file.
  fn file_length(&self, name: &str) -> Result<usize>;
  /// Creates a new, empty file in the directory and returns an `IndexOutput`
  /// instance for appending data to this file.
  ///
  /// # Errors
  /// This method must return an error if the file already exists:
  /// - [`std::io::ErrorKind::AlreadyExists`] for existing files.
  ///
  /// Returns an `std::io::Error` in case of other I/O errors.
  ///
  /// # Arguments
  /// * `name` - The name of the file to create.
  fn create_output(&self, name: &str, context: &IOContext) -> Result<Self::IndexOutput>;

  /// Creates a new, empty, temporary file in the directory and returns an
  /// `IndexOutput` instance for appending data to this file.
  ///
  /// The temporary file name (accessible via `IndexOutput::get_name`) will
  /// start with `prefix`, end with `suffix`, and have a reserved file
  /// extension `.tmp`.
  ///
  /// # Arguments
  /// * `prefix` - The prefix for the temporary file name.
  /// * `suffix` - The suffix for the temporary file name.
  type IndexOutput: IndexOutput;
  fn create_temp_output(
    &self,
    prefix: &str,
    suffix: &str,
    context: &IOContext,
  ) -> Result<Self::IndexOutput>;
  /// Ensures that any writings to these files are moved to stable storage
  /// (made durable).
  ///
  /// Lucene uses this to properly commit changes to the index, preventing
  /// corruption in case of a machine or OS crash.
  ///
  /// # See Also
  /// [`sync_metadata`](Directory::sync_metadata)
  fn sync(&self, names: &[String]) -> Result<()>;
  /// Ensures that directory metadata, such as recent file renames, are moved
  /// to stable storage.
  ///
  /// # See Also
  /// [`sync`](Directory::sync)
  fn sync_metadata(&self) -> Result<()>;
  /// Renames `source` file to `dest` file where `dest` must not already exist
  /// in the directory.
  ///
  /// It is permitted for this operation to not be truly atomic, meaning both
  /// `source` and `dest` could temporarily be visible in the list of
  /// files. However, the implementation must ensure that the content of
  /// `dest` appears as the entire `source` atomically. Once `dest` is visible
  /// for readers, the entire content of the previous `source` must be
  /// visible.
  ///
  /// This method is used by `IndexWriter` to publish commits.
  ///
  /// # Arguments
  /// * `source` - The file to rename.
  /// * `dest` - The new name for the file.
  fn rename(&self, source: &str, dest: &str) -> Result<()>;

  /// Opens a stream for reading an existing file.
  ///
  /// # Errors
  /// This method must return an error if `name` points to a non-existing
  /// file:
  /// - [`std::io::ErrorKind::NotFound`] for non-existing files.
  ///
  /// Returns an `std::io::Error` in case of other I/O errors.
  ///
  /// # Arguments
  /// * `name` - The name of an existing file.
  type IndexInput: IndexInput<IndexInput = Self::IndexInput, RandomAccessSlice: Send + Sync>
    + Send
    + Sync;
  fn open_input(&self, name: &str, context: &IOContext) -> Result<Self::IndexInput>;

  /// Opens a checksum-computing stream for reading an existing file.
  ///
  /// # Errors
  /// This method must return an error if `name` points to a non-existing
  /// file:
  /// - [`std::io::ErrorKind::NotFound`] for non-existing files.
  ///
  /// Returns an `std::io::Error` in case of other I/O errors.
  ///
  /// # Arguments
  /// * `name` - The name of an existing file.
  fn open_checksum_input(
    &self,
    name: &str,
  ) -> Result<BufferedChecksumIndexInput<Self::IndexInput>> {
    Ok(BufferedChecksumIndexInput::new(
      self.open_input(name, &IOContext::read_once_io_context()?)?,
    ))
  }

  type Lock: Lock;
  /// Acquires and returns a `Lock` for a file with the given name.
  ///
  /// # Errors
  /// - Returns a `LockObtainFailedError` (optional specific error) if
  ///   the lock could not be obtained because it is currently held elsewhere.
  /// - Returns an `std::io::Error` if any I/O error occurs attempting to gain
  ///   the lock.
  ///
  /// # Arguments
  /// * `name` - The name of the lock file.
  fn obtain_lock(&self, name: &str) -> Result<Self::Lock>;
  /// Copies an existing `src` file from directory `from` to a non-existent
  /// file `dest` in this directory. The given `IOContext` is only used
  /// for opening the destination file.
  ///
  /// # Arguments
  /// * `src` - The source file to copy.
  /// * `from` - The directory containing the source file.
  /// * `dest` - The destination file in this directory.
  /// * `io_context` - The I/O context used for opening the destination file.
  fn copy_from<D>(&self, from: &D, src: &str, dest: &str, context: &IOContext) -> Result<()>
  where
    Self: Sized,
    D: Directory + ?Sized,
  {
    let mut success = false;
    let mut input = None;
    let mut output = None;
    let body_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      input = Some(from.open_input(src, &IOContext::read_once_io_context()?)?);
      output = Some(self.create_output(dest, context)?);
      let input = input
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("copy source input is missing"))?;
      let output = output
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("copy destination output is missing"))?;
      let length = IndexInput::length(input)?;
      output.copy_bytes(input, length)?;
      success = true;
      Ok(())
    }));
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      IOUtils::close(0..2, |operation| match operation {
        0 => match output.as_mut() {
          Some(output) => output.close(),
          None => Ok(()),
        },
        1 => match input.as_mut() {
          Some(input) => input.close(),
          None => Ok(()),
        },
        _ => unreachable!(),
      })
    }));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      IOUtils::use_or_suppress_caught_result(body_result, close_result)
    }));

    if !success {
      IOUtils::delete_files_ignoring_exceptions(self, &[dest.to_string()]);
    }
    match result {
      Ok(result) => result,
      Err(payload) => std::panic::resume_unwind(payload),
    }
  }

  fn as_erased_directory(&self) -> Option<&dyn ErasedDirectory> {
    None
  }

  /// Returns a set of files currently pending deletion in this directory.
  ///
  /// # Note
  /// This is an internal API.
  fn get_pending_deletions(&self) -> Result<HashSet<String>>;

  #[cfg(debug_assertions)]
  fn is_fs_directory(&self) -> bool {
    false
  }
  fn ensure_open(&self) -> Result<()> {
    Ok(())
  }
}

/// Creates a file name for a temporary file. The name will start with `prefix`,
/// end with `suffix`, and have a reserved file extension `.tmp`.
///
/// # See Also
/// [`create_temp_output`](Directory)
pub fn get_temp_file_name(prefix: &str, suffix: &str, counter: u64) -> String {
  //base-36
  let counter_str = BigInt::from(counter).to_str_radix(36);
  let full_suffix = format!("{suffix}_{counter_str}");
  IndexFileNames::segment_file_name(prefix, &full_suffix, "tmp")
}

pub trait ErasedDirectory:
  Directory<IndexOutput = IndexOutputEnum, IndexInput = IndexInputEnum, Lock = LockEnum> + Send + Sync
{
  fn copy_from_erased(
    &self,
    from: &dyn ErasedDirectory,
    src: &str,
    dest: &str,
    context: &IOContext,
  ) -> Result<()>;
}

impl<T> ErasedDirectory for T
where
  T: Directory<IndexOutput = IndexOutputEnum, IndexInput = IndexInputEnum, Lock = LockEnum>
    + Send
    + Sync,
{
  fn copy_from_erased(
    &self,
    from: &dyn ErasedDirectory,
    src: &str,
    dest: &str,
    context: &IOContext,
  ) -> Result<()> {
    self.copy_from(from, src, dest, context)
  }
}

pub type DynDirectory = dyn ErasedDirectory;
pub type CustomDirectory = Box<DynDirectory>;

pub enum DirectoryEnum {
  Fs(FSDirectory<NativeFSLockFactory, NIOFSDirectory>),
  Custom(CustomDirectory),
}

impl DirectoryEnum {
  pub fn custom<D>(directory: D) -> Self
  where
    D: ErasedDirectory + 'static,
  {
    Self::Custom(Box::new(directory))
  }
}

impl From<FSDirectory<NativeFSLockFactory, NIOFSDirectory>> for DirectoryEnum {
  fn from(directory: FSDirectory<NativeFSLockFactory, NIOFSDirectory>) -> Self {
    Self::Fs(directory)
  }
}

impl Display for DirectoryEnum {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Fs(inner) => inner.fmt(f),
      Self::Custom(inner) => inner.fmt(f),
    }
  }
}

impl HasIdentity for DirectoryEnum {
  fn identity(&self) -> &Identity {
    match self {
      Self::Fs(inner) => inner.identity(),
      Self::Custom(inner) => inner.identity(),
    }
  }
}

impl Directory for DirectoryEnum {
  fn list_all(&self) -> Result<Vec<String>> {
    match self {
      Self::Fs(inner) => inner.list_all(),
      Self::Custom(inner) => inner.list_all(),
    }
  }

  fn delete_file(&self, name: &str) -> Result<()> {
    match self {
      Self::Fs(inner) => inner.delete_file(name),
      Self::Custom(inner) => inner.delete_file(name),
    }
  }

  fn file_length(&self, name: &str) -> Result<usize> {
    match self {
      Self::Fs(inner) => inner.file_length(name),
      Self::Custom(inner) => inner.file_length(name),
    }
  }

  type IndexOutput = IndexOutputEnum;

  fn create_output(&self, name: &str, context: &IOContext) -> Result<Self::IndexOutput> {
    match self {
      Self::Fs(inner) => Ok(IndexOutputEnum::Fs(inner.create_output(name, context)?)),
      Self::Custom(inner) => inner.create_output(name, context),
    }
  }

  fn create_temp_output(
    &self,
    prefix: &str,
    suffix: &str,
    context: &IOContext,
  ) -> Result<Self::IndexOutput> {
    match self {
      Self::Fs(inner) => Ok(IndexOutputEnum::Fs(
        inner.create_temp_output(prefix, suffix, context)?,
      )),
      Self::Custom(inner) => inner.create_temp_output(prefix, suffix, context),
    }
  }

  fn sync(&self, names: &[String]) -> Result<()> {
    match self {
      Self::Fs(inner) => inner.sync(names),
      Self::Custom(inner) => inner.sync(names),
    }
  }

  fn sync_metadata(&self) -> Result<()> {
    match self {
      Self::Fs(inner) => inner.sync_metadata(),
      Self::Custom(inner) => inner.sync_metadata(),
    }
  }

  fn rename(&self, source: &str, dest: &str) -> Result<()> {
    match self {
      Self::Fs(inner) => inner.rename(source, dest),
      Self::Custom(inner) => inner.rename(source, dest),
    }
  }

  type IndexInput = IndexInputEnum;

  fn open_input(&self, name: &str, context: &IOContext) -> Result<Self::IndexInput> {
    match self {
      Self::Fs(inner) => Ok(IndexInputEnum::Fs(inner.open_input(name, context)?)),
      Self::Custom(inner) => inner.open_input(name, context),
    }
  }

  type Lock = LockEnum;

  fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
    match self {
      Self::Fs(inner) => Ok(LockEnum::Native(inner.obtain_lock(name)?)),
      Self::Custom(inner) => inner.obtain_lock(name),
    }
  }

  fn copy_from<D>(&self, from: &D, src: &str, dest: &str, context: &IOContext) -> Result<()>
  where
    D: Directory + ?Sized,
  {
    match self {
      Self::Fs(inner) => inner.copy_from(from, src, dest, context),
      Self::Custom(inner) => {
        if let Some(from) = from.as_erased_directory() {
          inner.copy_from_erased(from, src, dest, context)
        } else {
          Err(LuceneError::unsupported_operation(
            "custom Directory copy_from requires an erased source Directory",
          ))
        }
      },
    }
  }

  fn as_erased_directory(&self) -> Option<&dyn ErasedDirectory> {
    Some(self)
  }

  fn get_pending_deletions(&self) -> Result<HashSet<String>> {
    match self {
      Self::Fs(inner) => inner.get_pending_deletions(),
      Self::Custom(inner) => inner.get_pending_deletions(),
    }
  }

  #[cfg(debug_assertions)]
  fn is_fs_directory(&self) -> bool {
    match self {
      Self::Fs(inner) => inner.is_fs_directory(),
      Self::Custom(inner) => inner.is_fs_directory(),
    }
  }

  fn ensure_open(&self) -> Result<()> {
    match self {
      Self::Fs(inner) => inner.ensure_open(),
      Self::Custom(inner) => inner.ensure_open(),
    }
  }
}

impl CloseableRef for DirectoryEnum {
  fn close(&self) -> Result<()> {
    match self {
      Self::Fs(inner) => inner.close(),
      Self::Custom(inner) => inner.close(),
    }
  }
}

macro_rules! either_directory {
    ($vis:vis $name:ident, $index_output:ident, $index_input:ident, $lock:ident { $( $Variant:ident : $T:ident ),+ $(,)? }) => {
        $vis enum $name<$( $T ),+> {
            $( $Variant($T), )+
        }

        impl<$( $T ),+> Display for $name<$( $T ),+>
        where
            $( $T: Directory ),+
        {
            fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
                match self {
                    $( Self::$Variant(inner) => inner.fmt(f), )+
                }
            }
        }
        impl<$( $T ),+> HasIdentity for $name<$( $T ),+>
        where
            $( $T: Directory ),+
        {
            fn identity(&self) -> &Identity {
                match self {
                    $( Self::$Variant(inner) => inner.identity(), )+
                }
            }
        }

        impl<$( $T ),+> Directory for $name<$( $T ),+>
        where
            $( $T: Directory ),+
        {
            fn list_all(&self) -> Result<Vec<String>> {
                match self {
                    $( Self::$Variant(inner) => inner.list_all(), )+
                }
            }

            fn delete_file(&self, name: &str) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.delete_file(name), )+
                }
            }

            fn file_length(&self, name: &str) -> Result<usize> {
                match self {
                    $( Self::$Variant(inner) => inner.file_length(name), )+
                }
            }

            fn create_output(&self, name: &str, context: &IOContext) -> Result<Self::IndexOutput> {
                match self {
                    $( Self::$Variant(inner) => Ok($index_output::$Variant(
                        inner.create_output(name, context)?,
                    )), )+
                }
            }

            type IndexOutput = $index_output<$( $T::IndexOutput ),+>;

            fn create_temp_output(
                &self,
                prefix: &str,
                suffix: &str,
                context: &IOContext,
            ) -> Result<Self::IndexOutput> {
                match self {
                    $( Self::$Variant(inner) => Ok($index_output::$Variant(
                        inner.create_temp_output(prefix, suffix, context)?,
                    )), )+
                }
            }

            fn sync(&self, names: &[String]) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.sync(names), )+
                }
            }

            fn sync_metadata(&self) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.sync_metadata(), )+
                }
            }

            fn rename(&self, source: &str, dest: &str) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.rename(source, dest), )+
                }
            }

            type IndexInput = $index_input<$( $T::IndexInput ),+>;

            fn open_input(&self, name: &str, context: &IOContext) -> Result<Self::IndexInput> {
                match self {
                    $( Self::$Variant(inner) => Ok($index_input::$Variant(
                        inner.open_input(name, context)?,
                    )), )+
                }
            }

            fn open_checksum_input(
                &self,
                name: &str,
            ) -> Result<BufferedChecksumIndexInput<Self::IndexInput>> {
                let input = self.open_input(name, &IOContext::default_io_context()?)?;
                Ok(BufferedChecksumIndexInput::new(input))
            }

            type Lock = $lock<$( $T::Lock ),+>;

            fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
                match self {
                    $( Self::$Variant(inner) => Ok($lock::$Variant(inner.obtain_lock(name)?)), )+
                }
            }

            fn copy_from<D>(
                &self,
                from: &D,
                src: &str,
                dest: &str,
                context: &IOContext,
            ) -> Result<()>
            where
                D: Directory + ?Sized,
            {
                match self {
                    $( Self::$Variant(inner) => inner.copy_from(from, src, dest, context), )+
                }
            }

            fn get_pending_deletions(&self) -> Result<HashSet<String>> {
                match self {
                    $( Self::$Variant(inner) => inner.get_pending_deletions(), )+
                }
            }
            #[cfg(debug_assertions)]
            fn is_fs_directory(&self) -> bool {
                match self {
                    $( Self::$Variant(inner) => inner.is_fs_directory(), )+
                }
            }

            fn ensure_open(&self) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.ensure_open(), )+
                }
            }
        }

        impl<$( $T ),+> CloseableRef for $name<$( $T ),+>
        where
            $( $T: Directory ),+
        {
            fn close(&self) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.close(), )+
                }
            }
        }

    };
}
either_directory!(
    pub DirectoryEnum2,
    IndexOutputEnum2,
    IndexInputEnum2,
    LockEnum2 { A: A, B: B }
);
either_directory!(
    pub DirectoryEnum3,
    IndexOutputEnum3,
    IndexInputEnum3,
    LockEnum3 { A: A, B: B, C: C }
);
impl<D: Directory> Directory for &D {
  fn list_all(&self) -> Result<Vec<String>> {
    (**self).list_all()
  }
  fn delete_file(&self, name: &str) -> Result<()> {
    (**self).delete_file(name)
  }
  fn file_length(&self, name: &str) -> Result<usize> {
    (**self).file_length(name)
  }

  fn create_output(&self, name: &str, ctx: &IOContext) -> Result<Self::IndexOutput> {
    (**self).create_output(name, ctx)
  }
  type IndexOutput = D::IndexOutput;
  fn create_temp_output(&self, p: &str, s: &str, ctx: &IOContext) -> Result<Self::IndexOutput> {
    (**self).create_temp_output(p, s, ctx)
  }

  fn sync(&self, names: &[String]) -> Result<()> {
    (**self).sync(names)
  }

  fn sync_metadata(&self) -> Result<()> {
    (**self).sync_metadata()
  }
  fn rename(&self, src: &str, dst: &str) -> Result<()> {
    (**self).rename(src, dst)
  }
  type IndexInput = D::IndexInput;
  fn open_input(&self, name: &str, ctx: &IOContext) -> Result<Self::IndexInput> {
    (**self).open_input(name, ctx)
  }
  type Lock = D::Lock;
  fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
    (**self).obtain_lock(name)
  }
  fn copy_from<F>(&self, from: &F, src: &str, dst: &str, ctx: &IOContext) -> Result<()>
  where
    F: Directory + ?Sized,
  {
    (**self).copy_from(from, src, dst, ctx)
  }
  fn as_erased_directory(&self) -> Option<&dyn ErasedDirectory> {
    (**self).as_erased_directory()
  }
  fn get_pending_deletions(&self) -> Result<HashSet<String>> {
    (**self).get_pending_deletions()
  }
  #[cfg(debug_assertions)]
  fn is_fs_directory(&self) -> bool {
    (**self).is_fs_directory()
  }

  fn ensure_open(&self) -> Result<()> {
    (**self).ensure_open()
  }
}

impl<D: Directory> Directory for Arc<D> {
  fn list_all(&self) -> Result<Vec<String>> {
    (**self).list_all()
  }
  fn delete_file(&self, name: &str) -> Result<()> {
    (**self).delete_file(name)
  }
  fn file_length(&self, name: &str) -> Result<usize> {
    (**self).file_length(name)
  }

  fn create_output(&self, name: &str, ctx: &IOContext) -> Result<Self::IndexOutput> {
    (**self).create_output(name, ctx)
  }
  type IndexOutput = D::IndexOutput;
  fn create_temp_output(&self, p: &str, s: &str, ctx: &IOContext) -> Result<Self::IndexOutput> {
    (**self).create_temp_output(p, s, ctx)
  }

  fn sync(&self, names: &[String]) -> Result<()> {
    (**self).sync(names)
  }

  fn sync_metadata(&self) -> Result<()> {
    (**self).sync_metadata()
  }
  fn rename(&self, src: &str, dst: &str) -> Result<()> {
    (**self).rename(src, dst)
  }
  type IndexInput = D::IndexInput;
  fn open_input(&self, name: &str, ctx: &IOContext) -> Result<Self::IndexInput> {
    (**self).open_input(name, ctx)
  }
  type Lock = D::Lock;
  fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
    (**self).obtain_lock(name)
  }
  fn copy_from<F>(&self, from: &F, src: &str, dst: &str, ctx: &IOContext) -> Result<()>
  where
    F: Directory + ?Sized,
  {
    (**self).copy_from(from, src, dst, ctx)
  }
  fn as_erased_directory(&self) -> Option<&dyn ErasedDirectory> {
    (**self).as_erased_directory()
  }
  fn get_pending_deletions(&self) -> Result<HashSet<String>> {
    (**self).get_pending_deletions()
  }
  #[cfg(debug_assertions)]
  fn is_fs_directory(&self) -> bool {
    (**self).is_fs_directory()
  }

  fn ensure_open(&self) -> Result<()> {
    (**self).ensure_open()
  }
}

#[cfg(test)]
pub(crate) type SharedLockFactory = Arc<LockFactoryEnum>;
#[cfg(test)]
pub(crate) type NioDir = FSDirectory<SharedLockFactory, NIOFSDirectory>;
#[cfg(test)]
pub(crate) type MMapDir = FSDirectory<SharedLockFactory, MMapDirectory>;
#[cfg(test)]
pub(crate) type ByteBuffersDir = ByteBuffersDirectory<SharedLockFactory>;
#[cfg(test)]
pub(crate) type CoreDirEnum = DirectoryEnum3<NioDir, MMapDir, ByteBuffersDir>;
#[cfg(test)]
pub(crate) type FileSwitchDir = FileSwitchDirectory<CoreDirEnum, CoreDirEnum>;
#[cfg(test)]
pub(crate) type MaybeNrtDirEnum = DirectoryEnum2<RawDirEnum, NRTCachingDirectory<RawDirEnum>>;
#[cfg(test)]
pub(crate) type RawDirWrapper = RawDirectoryWrapper<MaybeNrtDirEnum>;
#[cfg(test)]
type MockDirWrapperInner = MockDirectoryWrapper<MaybeNrtDirEnum>;
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct MockDirWrapper(MockDirWrapperInner);

#[cfg(test)]
impl MockDirWrapper {
  pub(crate) fn from_inner(directory: MockDirWrapperInner) -> Self {
    Self(directory)
  }
}

#[cfg(test)]
impl Deref for MockDirWrapper {
  type Target = MockDirWrapperInner;

  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

#[cfg(test)]
impl DerefMut for MockDirWrapper {
  fn deref_mut(&mut self) -> &mut Self::Target {
    &mut self.0
  }
}

#[cfg(test)]
impl Display for MockDirWrapper {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    self.0.fmt(f)
  }
}

#[cfg(test)]
impl HasIdentity for MockDirWrapper {
  fn identity(&self) -> &Identity {
    self.0.identity()
  }
}

#[cfg(test)]
impl Directory for MockDirWrapper {
  fn list_all(&self) -> Result<Vec<String>> {
    self.0.list_all()
  }

  fn delete_file(&self, name: &str) -> Result<()> {
    self.0.delete_file(name)
  }

  fn file_length(&self, name: &str) -> Result<usize> {
    self.0.file_length(name)
  }

  type IndexOutput = <MockDirWrapperInner as Directory>::IndexOutput;

  fn create_output(&self, name: &str, context: &IOContext) -> Result<Self::IndexOutput> {
    self.0.create_output(name, context)
  }

  fn create_temp_output(
    &self,
    prefix: &str,
    suffix: &str,
    context: &IOContext,
  ) -> Result<Self::IndexOutput> {
    self.0.create_temp_output(prefix, suffix, context)
  }

  fn sync(&self, names: &[String]) -> Result<()> {
    self.0.sync(names)
  }

  fn sync_metadata(&self) -> Result<()> {
    self.0.sync_metadata()
  }

  fn rename(&self, source: &str, dest: &str) -> Result<()> {
    self.0.rename(source, dest)
  }

  type IndexInput = DirIndexInput;

  fn open_input(&self, name: &str, context: &IOContext) -> Result<Self::IndexInput> {
    Ok(DirIndexInput(IndexInputEnum2::B(
      self.0.open_input(name, context)?,
    )))
  }

  fn open_checksum_input(
    &self,
    name: &str,
  ) -> Result<BufferedChecksumIndexInput<Self::IndexInput>> {
    Ok(BufferedChecksumIndexInput::new(
      self.open_input(name, &IOContext::read_once_io_context()?)?,
    ))
  }

  type Lock = <MockDirWrapperInner as Directory>::Lock;

  fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
    self.0.obtain_lock(name)
  }

  fn copy_from<D>(&self, from: &D, src: &str, dest: &str, context: &IOContext) -> Result<()>
  where
    D: Directory + ?Sized,
  {
    self.0.copy_from(from, src, dest, context)
  }

  fn get_pending_deletions(&self) -> Result<HashSet<String>> {
    self.0.get_pending_deletions()
  }

  #[cfg(debug_assertions)]
  fn is_fs_directory(&self) -> bool {
    self.0.is_fs_directory()
  }

  fn ensure_open(&self) -> Result<()> {
    self.0.ensure_open()
  }
}

#[cfg(test)]
impl CloseableRef for MockDirWrapper {
  fn close(&self) -> Result<()> {
    self.0.close()
  }
}

#[cfg(test)]
type DirIndexInputInner = IndexInputEnum2<
  <RawDirWrapper as Directory>::IndexInput,
  <MockDirWrapperInner as Directory>::IndexInput,
>;
#[cfg(test)]
type DirRandomAccessInputInner = RandomAccessInputEnum2<
  <<RawDirWrapper as Directory>::IndexInput as IndexInput>::RandomAccessSlice,
  <<MockDirWrapperInner as Directory>::IndexInput as IndexInput>::RandomAccessSlice,
>;

#[cfg(test)]
pub(crate) struct DirIndexInput(DirIndexInputInner);

#[cfg(test)]
impl Display for DirIndexInput {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    self.0.fmt(f)
  }
}

#[cfg(test)]
impl CloseableRef for DirIndexInput {
  fn close(&self) -> Result<()> {
    self.0.close()
  }
}

#[cfg(test)]
impl TryClone for DirIndexInput {
  fn try_clone(&self) -> Result<Self> {
    Ok(Self(self.0.try_clone()?))
  }
}

#[cfg(test)]
impl DataInput for DirIndexInput {
  fn read_byte(&mut self) -> Result<u8> {
    DataInput::read_byte(&mut self.0)
  }

  fn read_bytes(&mut self, b: &mut [u8], offset: usize, len: usize) -> Result<()> {
    DataInput::read_bytes(&mut self.0, b, offset, len)
  }

  fn read_bytes_with_buffer(
    &mut self,
    b: &mut [u8],
    offset: usize,
    len: usize,
    use_buffer: bool,
  ) -> Result<()> {
    self.0.read_bytes_with_buffer(b, offset, len, use_buffer)
  }

  fn read_short(&mut self) -> Result<i16> {
    DataInput::read_short(&mut self.0)
  }

  fn read_int(&mut self) -> Result<i32> {
    DataInput::read_int(&mut self.0)
  }

  fn read_group_vint(&mut self, dst: &mut [i32], offset: usize) -> Result<()> {
    self.0.read_group_vint(dst, offset)
  }

  fn read_vint(&mut self) -> Result<i32> {
    self.0.read_vint()
  }

  fn read_zint(&mut self) -> Result<i32> {
    self.0.read_zint()
  }

  fn read_long(&mut self) -> Result<i64> {
    DataInput::read_long(&mut self.0)
  }

  fn read_longs(&mut self, dst: &mut [i64], offset: usize, len: usize) -> Result<()> {
    self.0.read_longs(dst, offset, len)
  }

  fn read_ints(&mut self, dst: &mut [i32], offset: usize, len: usize) -> Result<()> {
    self.0.read_ints(dst, offset, len)
  }

  fn read_floats(&mut self, dst: &mut [f32], offset: usize, len: usize) -> Result<()> {
    self.0.read_floats(dst, offset, len)
  }

  fn read_vlong(&mut self) -> Result<i64> {
    self.0.read_vlong()
  }

  fn read_zlong(&mut self) -> Result<i64> {
    self.0.read_zlong()
  }

  fn read_string(&mut self) -> Result<String> {
    self.0.read_string()
  }

  fn read_map_of_strings(&mut self) -> Result<HashMap<String, String>> {
    self.0.read_map_of_strings()
  }

  fn read_set_of_strings(&mut self) -> Result<HashSet<String>> {
    self.0.read_set_of_strings()
  }

  fn skip_bytes(&mut self, num_bytes: i64) -> Result<()> {
    DataInput::skip_bytes(&mut self.0, num_bytes)
  }

  fn is_index_input(&self) -> bool {
    self.0.is_index_input()
  }

  fn seek_in_data_input(&mut self, pos: usize) -> Result<()> {
    self.0.seek_in_data_input(pos)
  }

  fn get_file_pointer_in_data_input(&self) -> Result<usize> {
    self.0.get_file_pointer_in_data_input()
  }
}

#[cfg(test)]
pub(crate) struct DirRandomAccessInput(DirRandomAccessInputInner);

#[cfg(test)]
impl RandomAccessInput for DirRandomAccessInput {
  fn length(&self) -> Result<usize> {
    self.0.length()
  }

  fn read_byte(&mut self, pos: usize) -> Result<u8> {
    self.0.read_byte(pos)
  }

  fn read_bytes(&mut self, pos: usize, buf: &mut [u8], offset: usize, len: usize) -> Result<()> {
    self.0.read_bytes(pos, buf, offset, len)
  }

  fn read_short(&mut self, pos: usize) -> Result<i16> {
    self.0.read_short(pos)
  }

  fn read_int(&mut self, pos: usize) -> Result<i32> {
    self.0.read_int(pos)
  }

  fn read_long(&mut self, pos: usize) -> Result<i64> {
    self.0.read_long(pos)
  }

  fn prefetch(&mut self, pos: usize, len: usize) -> Result<()> {
    RandomAccessInput::prefetch(&mut self.0, pos, len)
  }

  fn is_loaded(&self) -> Result<Option<bool>> {
    self.0.is_loaded()
  }
}

#[cfg(test)]
impl IndexInput for DirIndexInput {
  type IndexInput = Self;

  fn get_file_pointer(&self) -> Result<usize> {
    self.0.get_file_pointer()
  }

  fn seek(&mut self, pos: usize) -> Result<()> {
    self.0.seek(pos)
  }

  fn skip_bytes(&mut self, num_bytes: i64) -> Result<()> {
    IndexInput::skip_bytes(&mut self.0, num_bytes)
  }

  fn length(&self) -> Result<usize> {
    IndexInput::length(&self.0)
  }

  fn slice(
    &self,
    slice_description: &str,
    offset: usize,
    length: usize,
  ) -> Result<Self::IndexInput> {
    Ok(Self(self.0.slice(slice_description, offset, length)?))
  }

  fn slice_with_read_advice(
    &self,
    description: &str,
    offset: usize,
    length: usize,
    read_advice: &ReadAdvice,
  ) -> Result<Self::IndexInput> {
    Ok(Self(self.0.slice_with_read_advice(
      description,
      offset,
      length,
      read_advice,
    )?))
  }

  type RandomAccessSlice = DirRandomAccessInput;

  fn random_access_slice(&self, offset: usize, length: usize) -> Result<Self::RandomAccessSlice> {
    Ok(DirRandomAccessInput(
      self.0.random_access_slice(offset, length)?,
    ))
  }

  fn prefetch(&mut self, pos: usize, len: usize) -> Result<()> {
    IndexInput::prefetch(&mut self.0, pos, len)
  }

  fn update_read_advice(&self, read_advice: ReadAdvice) -> Result<()> {
    self.0.update_read_advice(read_advice)
  }

  fn is_loaded(&self) -> Result<Option<bool>> {
    IndexInput::is_loaded(&self.0)
  }
}

#[cfg(test)]
pub(crate) enum DirEnum {
  A(Box<RawDirWrapper>),
  B(MockDirWrapper),
}

#[cfg(test)]
impl Display for DirEnum {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::A(directory) => directory.fmt(f),
      Self::B(directory) => directory.fmt(f),
    }
  }
}

#[cfg(test)]
impl HasIdentity for DirEnum {
  fn identity(&self) -> &Identity {
    match self {
      Self::A(directory) => directory.identity(),
      Self::B(directory) => directory.identity(),
    }
  }
}

#[cfg(test)]
impl Directory for DirEnum {
  fn list_all(&self) -> Result<Vec<String>> {
    match self {
      Self::A(directory) => directory.list_all(),
      Self::B(directory) => directory.list_all(),
    }
  }

  fn delete_file(&self, name: &str) -> Result<()> {
    match self {
      Self::A(directory) => directory.delete_file(name),
      Self::B(directory) => directory.delete_file(name),
    }
  }

  fn file_length(&self, name: &str) -> Result<usize> {
    match self {
      Self::A(directory) => directory.file_length(name),
      Self::B(directory) => directory.file_length(name),
    }
  }

  type IndexOutput = IndexOutputEnum2<
    <RawDirWrapper as Directory>::IndexOutput,
    <MockDirWrapper as Directory>::IndexOutput,
  >;

  fn create_output(&self, name: &str, context: &IOContext) -> Result<Self::IndexOutput> {
    match self {
      Self::A(directory) => Ok(IndexOutputEnum2::A(directory.create_output(name, context)?)),
      Self::B(directory) => Ok(IndexOutputEnum2::B(directory.create_output(name, context)?)),
    }
  }

  fn create_temp_output(
    &self,
    prefix: &str,
    suffix: &str,
    context: &IOContext,
  ) -> Result<Self::IndexOutput> {
    match self {
      Self::A(directory) => Ok(IndexOutputEnum2::A(
        directory.create_temp_output(prefix, suffix, context)?,
      )),
      Self::B(directory) => Ok(IndexOutputEnum2::B(
        directory.create_temp_output(prefix, suffix, context)?,
      )),
    }
  }

  fn sync(&self, names: &[String]) -> Result<()> {
    match self {
      Self::A(directory) => directory.sync(names),
      Self::B(directory) => directory.sync(names),
    }
  }

  fn sync_metadata(&self) -> Result<()> {
    match self {
      Self::A(directory) => directory.sync_metadata(),
      Self::B(directory) => directory.sync_metadata(),
    }
  }

  fn rename(&self, source: &str, dest: &str) -> Result<()> {
    match self {
      Self::A(directory) => directory.rename(source, dest),
      Self::B(directory) => directory.rename(source, dest),
    }
  }

  type IndexInput = DirIndexInput;

  fn open_input(&self, name: &str, context: &IOContext) -> Result<Self::IndexInput> {
    match self {
      Self::A(directory) => Ok(DirIndexInput(IndexInputEnum2::A(
        directory.open_input(name, context)?,
      ))),
      Self::B(directory) => directory.open_input(name, context),
    }
  }

  fn open_checksum_input(
    &self,
    name: &str,
  ) -> Result<BufferedChecksumIndexInput<Self::IndexInput>> {
    let input = self.open_input(name, &IOContext::default_io_context()?)?;
    Ok(BufferedChecksumIndexInput::new(input))
  }

  type Lock = LockEnum2<<RawDirWrapper as Directory>::Lock, <MockDirWrapper as Directory>::Lock>;

  fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
    match self {
      Self::A(directory) => Ok(LockEnum2::A(directory.obtain_lock(name)?)),
      Self::B(directory) => Ok(LockEnum2::B(directory.obtain_lock(name)?)),
    }
  }

  fn copy_from<D>(&self, from: &D, src: &str, dest: &str, context: &IOContext) -> Result<()>
  where
    D: Directory + ?Sized,
  {
    match self {
      Self::A(directory) => directory.copy_from(from, src, dest, context),
      Self::B(directory) => directory.copy_from(from, src, dest, context),
    }
  }

  fn get_pending_deletions(&self) -> Result<HashSet<String>> {
    match self {
      Self::A(directory) => directory.get_pending_deletions(),
      Self::B(directory) => directory.get_pending_deletions(),
    }
  }

  #[cfg(debug_assertions)]
  fn is_fs_directory(&self) -> bool {
    match self {
      Self::A(directory) => directory.is_fs_directory(),
      Self::B(directory) => directory.is_fs_directory(),
    }
  }

  fn ensure_open(&self) -> Result<()> {
    match self {
      Self::A(directory) => directory.ensure_open(),
      Self::B(directory) => directory.ensure_open(),
    }
  }
}

#[cfg(test)]
impl CloseableRef for DirEnum {
  fn close(&self) -> Result<()> {
    match self {
      Self::A(directory) => directory.close(),
      Self::B(directory) => directory.close(),
    }
  }
}

#[cfg(test)]
impl DirEnum {
  pub(crate) fn set_check_index_on_close(&self, value: bool) {
    match self {
      Self::A(directory) => directory.set_check_index_on_close(value),
      Self::B(directory) => directory.set_check_index_on_close(value),
    }
  }
}

#[cfg(test)]
pub(crate) enum RawDirEnum {
  Nio(NioDir),
  MMap(MMapDir),
  ByteBuffers(ByteBuffersDir),
  FileSwitch(FileSwitchDir),
}

#[cfg(test)]
impl From<NioDir> for RawDirEnum {
  fn from(directory: NioDir) -> Self {
    Self::Nio(directory)
  }
}

#[cfg(test)]
impl From<MMapDir> for RawDirEnum {
  fn from(directory: MMapDir) -> Self {
    Self::MMap(directory)
  }
}

#[cfg(test)]
impl From<ByteBuffersDir> for RawDirEnum {
  fn from(directory: ByteBuffersDir) -> Self {
    Self::ByteBuffers(directory)
  }
}

#[cfg(test)]
impl From<FileSwitchDir> for RawDirEnum {
  fn from(directory: FileSwitchDir) -> Self {
    Self::FileSwitch(directory)
  }
}

#[cfg(test)]
impl Display for RawDirEnum {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Nio(inner) => inner.fmt(f),
      Self::MMap(inner) => inner.fmt(f),
      Self::ByteBuffers(inner) => inner.fmt(f),
      Self::FileSwitch(inner) => inner.fmt(f),
    }
  }
}

#[cfg(test)]
impl HasIdentity for RawDirEnum {
  fn identity(&self) -> &Identity {
    match self {
      Self::Nio(inner) => inner.identity(),
      Self::MMap(inner) => inner.identity(),
      Self::ByteBuffers(inner) => inner.identity(),
      Self::FileSwitch(inner) => inner.identity(),
    }
  }
}

#[cfg(test)]
impl CloseableRef for RawDirEnum {
  fn close(&self) -> Result<()> {
    match self {
      Self::Nio(inner) => inner.close(),
      Self::MMap(inner) => inner.close(),
      Self::ByteBuffers(inner) => inner.close(),
      Self::FileSwitch(inner) => inner.close(),
    }
  }
}

#[cfg(test)]
impl Directory for RawDirEnum {
  fn list_all(&self) -> Result<Vec<String>> {
    match self {
      Self::Nio(inner) => inner.list_all(),
      Self::MMap(inner) => inner.list_all(),
      Self::ByteBuffers(inner) => inner.list_all(),
      Self::FileSwitch(inner) => inner.list_all(),
    }
  }

  fn delete_file(&self, name: &str) -> Result<()> {
    match self {
      Self::Nio(inner) => inner.delete_file(name),
      Self::MMap(inner) => inner.delete_file(name),
      Self::ByteBuffers(inner) => inner.delete_file(name),
      Self::FileSwitch(inner) => inner.delete_file(name),
    }
  }

  fn file_length(&self, name: &str) -> Result<usize> {
    match self {
      Self::Nio(inner) => inner.file_length(name),
      Self::MMap(inner) => inner.file_length(name),
      Self::ByteBuffers(inner) => inner.file_length(name),
      Self::FileSwitch(inner) => inner.file_length(name),
    }
  }

  type IndexOutput = IndexOutputEnum2<
    <CoreDirEnum as Directory>::IndexOutput,
    <FileSwitchDir as Directory>::IndexOutput,
  >;

  fn create_output(&self, name: &str, context: &IOContext) -> Result<Self::IndexOutput> {
    match self {
      Self::Nio(inner) => Ok(IndexOutputEnum2::A(IndexOutputEnum3::A(
        inner.create_output(name, context)?,
      ))),
      Self::MMap(inner) => Ok(IndexOutputEnum2::A(IndexOutputEnum3::B(
        inner.create_output(name, context)?,
      ))),
      Self::ByteBuffers(inner) => Ok(IndexOutputEnum2::A(IndexOutputEnum3::C(
        inner.create_output(name, context)?,
      ))),
      Self::FileSwitch(inner) => Ok(IndexOutputEnum2::B(inner.create_output(name, context)?)),
    }
  }

  fn create_temp_output(
    &self,
    prefix: &str,
    suffix: &str,
    context: &IOContext,
  ) -> Result<Self::IndexOutput> {
    match self {
      Self::Nio(inner) => Ok(IndexOutputEnum2::A(IndexOutputEnum3::A(
        inner.create_temp_output(prefix, suffix, context)?,
      ))),
      Self::MMap(inner) => Ok(IndexOutputEnum2::A(IndexOutputEnum3::B(
        inner.create_temp_output(prefix, suffix, context)?,
      ))),
      Self::ByteBuffers(inner) => Ok(IndexOutputEnum2::A(IndexOutputEnum3::C(
        inner.create_temp_output(prefix, suffix, context)?,
      ))),
      Self::FileSwitch(inner) => Ok(IndexOutputEnum2::B(
        inner.create_temp_output(prefix, suffix, context)?,
      )),
    }
  }

  fn sync(&self, names: &[String]) -> Result<()> {
    match self {
      Self::Nio(inner) => inner.sync(names),
      Self::MMap(inner) => inner.sync(names),
      Self::ByteBuffers(inner) => inner.sync(names),
      Self::FileSwitch(inner) => inner.sync(names),
    }
  }

  fn sync_metadata(&self) -> Result<()> {
    match self {
      Self::Nio(inner) => inner.sync_metadata(),
      Self::MMap(inner) => inner.sync_metadata(),
      Self::ByteBuffers(inner) => inner.sync_metadata(),
      Self::FileSwitch(inner) => inner.sync_metadata(),
    }
  }

  fn rename(&self, source: &str, dest: &str) -> Result<()> {
    match self {
      Self::Nio(inner) => inner.rename(source, dest),
      Self::MMap(inner) => inner.rename(source, dest),
      Self::ByteBuffers(inner) => inner.rename(source, dest),
      Self::FileSwitch(inner) => inner.rename(source, dest),
    }
  }

  type IndexInput = IndexInputEnum2<
    <CoreDirEnum as Directory>::IndexInput,
    <FileSwitchDir as Directory>::IndexInput,
  >;

  fn open_input(&self, name: &str, context: &IOContext) -> Result<Self::IndexInput> {
    match self {
      Self::Nio(inner) => Ok(IndexInputEnum2::A(IndexInputEnum3::A(
        inner.open_input(name, context)?,
      ))),
      Self::MMap(inner) => Ok(IndexInputEnum2::A(IndexInputEnum3::B(
        inner.open_input(name, context)?,
      ))),
      Self::ByteBuffers(inner) => Ok(IndexInputEnum2::A(IndexInputEnum3::C(
        inner.open_input(name, context)?,
      ))),
      Self::FileSwitch(inner) => Ok(IndexInputEnum2::B(inner.open_input(name, context)?)),
    }
  }

  type Lock = LockEnum2<<CoreDirEnum as Directory>::Lock, <FileSwitchDir as Directory>::Lock>;

  fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
    match self {
      Self::Nio(inner) => Ok(LockEnum2::A(LockEnum3::A(inner.obtain_lock(name)?))),
      Self::MMap(inner) => Ok(LockEnum2::A(LockEnum3::B(inner.obtain_lock(name)?))),
      Self::ByteBuffers(inner) => Ok(LockEnum2::A(LockEnum3::C(inner.obtain_lock(name)?))),
      Self::FileSwitch(inner) => Ok(LockEnum2::B(inner.obtain_lock(name)?)),
    }
  }

  fn get_pending_deletions(&self) -> Result<HashSet<String>> {
    match self {
      Self::Nio(inner) => inner.get_pending_deletions(),
      Self::MMap(inner) => inner.get_pending_deletions(),
      Self::ByteBuffers(inner) => inner.get_pending_deletions(),
      Self::FileSwitch(inner) => inner.get_pending_deletions(),
    }
  }

  #[cfg(debug_assertions)]
  fn is_fs_directory(&self) -> bool {
    match self {
      Self::Nio(inner) => inner.is_fs_directory(),
      Self::MMap(inner) => inner.is_fs_directory(),
      Self::ByteBuffers(inner) => inner.is_fs_directory(),
      Self::FileSwitch(inner) => inner.is_fs_directory(),
    }
  }

  fn ensure_open(&self) -> Result<()> {
    match self {
      Self::Nio(inner) => inner.ensure_open(),
      Self::MMap(inner) => inner.ensure_open(),
      Self::ByteBuffers(inner) => inner.ensure_open(),
      Self::FileSwitch(inner) => inner.ensure_open(),
    }
  }
}
