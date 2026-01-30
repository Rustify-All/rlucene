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
use crate::core::store::buffered_checksum_index_input::BufferedChecksumIndexInput;
use crate::core::store::data_output::DataOutput;
use crate::core::store::index_input::IndexInput;
use crate::core::store::lock::{Lock, LockEnum2};
use crate::core::store::{IOContext, IndexInputEnum2, IndexOutput, IndexOutputEnum2};
use crate::core::util::HasIdentity;
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::Result;
use num_bigint::BigInt;
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
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
/// [`FSDirectory`](crate::core::store::fs_directory::FSDirectory)
/// [`ByteBuffersDirectory`](crate::core::store::byte_buffers_directory::ByteBuffersDirectory)
/// [`FilterDirectory`](crate::core::store::filter_directory::FilterDirectory)
pub trait Directory: Display + Closeable + HasIdentity {
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
    type IndexInput: IndexInput;
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
    /// - Returns a `LockObtainFailedException` (optional specific exception) if
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
    fn copy_from(
        &self,
        from: &impl Directory,
        src: &str,
        dest: &str,
        context: &IOContext,
    ) -> Result<()> {
        let result = (|| -> Result<()> {
            let mut is = from.open_input(src, &IOContext::read_once_io_context()?)?;
            let mut os = self.create_output(dest, context)?;
            let length = IndexInput::length(&is);
            os.copy_bytes(&mut is, length)?;
            Ok(())
        })();

        if result.is_err() {
            self.delete_files_ignoring_exceptions(&[dest.to_string()]);
        }
        result
    }
    fn delete_files_ignoring_exceptions(&self, files: &[String]) {
        for name in files {
            if self.delete_file(name).is_err() {
                // ignore
            }
        }
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

            fn copy_from(
                &self,
                from: &impl Directory,
                src: &str,
                dest: &str,
                context: &IOContext,
            ) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.copy_from(from, src, dest, context), )+
                }
            }

            fn delete_files_ignoring_exceptions(&self, files: &[String]) {
                match self {
                    $( Self::$Variant(inner) => inner.delete_files_ignoring_exceptions(files), )+
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

        impl<$( $T ),+> Closeable for $name<$( $T ),+>
        where
            $( $T: Directory ),+
        {
            fn close(&mut self) -> Result<()> {
                // TODO
                Ok(())
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
    fn copy_from(
        &self,
        from: &impl Directory,
        src: &str,
        dst: &str,
        ctx: &IOContext,
    ) -> Result<()> {
        (**self).copy_from(from, src, dst, ctx)
    }
    fn delete_files_ignoring_exceptions(&self, files: &[String]) {
        (**self).delete_files_ignoring_exceptions(files)
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

impl<D: Directory> Closeable for &D {
    fn close(&mut self) -> Result<()> {
        // TODO
        Ok(())
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
    fn copy_from(
        &self,
        from: &impl Directory,
        src: &str,
        dst: &str,
        ctx: &IOContext,
    ) -> Result<()> {
        (**self).copy_from(from, src, dst, ctx)
    }
    fn delete_files_ignoring_exceptions(&self, files: &[String]) {
        (**self).delete_files_ignoring_exceptions(files)
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

impl<D: Directory> Closeable for Arc<D> {
    fn close(&mut self) -> Result<()> {
        // TODO
        Ok(())
    }
}
