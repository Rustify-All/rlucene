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
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::atomic::{AtomicU32, AtomicU64};
use std::{fs, io};

use crate::core::index::index_reader::Identity;
use crate::core::store::base_directory::{BaseDirectory, BaseDirectoryBase};
use crate::core::store::directory::{Directory, get_temp_file_name};
use crate::core::store::fs_directory_base::FSDirectoryBase;
use crate::core::store::lock_factory::LockFactory;
use crate::core::store::{IOContext, NativeFSLockFactory, OutputStreamIndexOutput};
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::{HasIdentity, IOUtils};
use parking_lot::Mutex;

/// Base trait for `Directory` implementations that store index files in the
/// file system. There are currently two core implementations:
///
/// - [`MMapDirectory`](crate::core::store::mmap_directory::MMapDirectory): Uses
///   memory-mapped IO when reading. This is a good choice if you have plenty of
///   virtual memory relative to your index size. It works well on 64-bit
///   systems or on 32-bit systems with small enough index sizes. This
///   implementation utilizes the modern `MemorySegment` API available since
///   Rust 21, allowing safe unmapping of previously memory-mapped files after
///   closing `IndexInput`s. No need to enable the "preview feature" of your
///   Java version.
/// - [`NIOFSDirectory`](crate::core::store::nio_fs_directory::NIOFSDirectory): Uses
///   `java.nio`'s `FileChannel`'s positional IO to avoid synchronization when
///   reading from the same file. This is the preferred choice on all platforms
///   except Windows, where a bug in the Sun JRE causes performance issues.
///   Applications using thread interruption or future cancellation should use
///   `RAFDirectory` instead.
///
/// # Note
/// Accessing one of the above SubStruct directly or indirectly from a thread
/// while it's interrupted can cause the underlying channel to close
/// immediately, leading to subsequent `ClosedChannelException` errors. If your
/// application uses `Thread::interrupt()` or `Future::cancel()`, it's
/// recommended to use the legacy `RAFDirectory` from the `misc` module.
///
/// The default locking implementation is [`NativeFSLockFactory`],
/// but it can be replaced with a custom `LockFactory`.
///
/// # See Also
/// [`Directory`]
pub struct FSDirectory<D, T>
where
    D: LockFactory,
    T: FSDirectoryBase,
{
    directory: PathBuf,
    /// Maps files that we are trying to delete (or we tried already but
    /// failed) before attempting to delete that key.
    pending_deletes: Arc<Mutex<HashSet<String>>>,
    ops_since_last_delete: AtomicU32,
    /// Used to generate temp file names in
    /// [`createTempOutput`](Directory::create_temp_output).
    next_temp_file_counter: AtomicU64,
    sub_fs_directory: T,
    base: BaseDirectoryBase<D>,
    id: Identity,
}
impl<D, T> FSDirectory<D, T>
where
    D: LockFactory,
    T: FSDirectoryBase,
{
    pub fn with_lock_factory(
        directory: PathBuf,
        lock_factory: D,
        sub_fs_directory: T,
    ) -> Result<FSDirectory<D, T>> {
        if !directory.is_dir() {
            fs::create_dir(&directory)?;
        }
        let base = BaseDirectoryBase::new(lock_factory);
        Ok(FSDirectory {
            directory,
            pending_deletes: Arc::new(Mutex::new(HashSet::new())),
            ops_since_last_delete: AtomicU32::new(0),
            next_temp_file_counter: AtomicU64::new(0),
            sub_fs_directory,
            base,
            id: Identity::new(),
        })
    }

    fn list_all(dir: &Path, skip_names: Option<&HashSet<String>>) -> Result<Vec<String>> {
        let mut entries = Vec::new();

        for entry in dir.read_dir()? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();

            if let Some(skip) = &skip_names
                && skip.contains(&name)
            {
                continue;
            }

            entries.push(name);
        }

        entries.sort();
        Ok(entries)
    }
    pub fn maybe_delete_pending_files(
        directory: &Path,
        pending_deletes: &mut HashSet<String>,
        ops_since_last_delete: &AtomicU32,
    ) -> Result<()> {
        if !pending_deletes.is_empty() {
            let count = ops_since_last_delete.fetch_add(1, SeqCst) + 1;

            if count as usize >= pending_deletes.len() {
                ops_since_last_delete.fetch_sub(count, SeqCst);
                Self::delete_pending_files(directory, pending_deletes)?;
            }
        }
        Ok(())
    }

    /// Ensure that the given file is synchronized to the storage device.
    ///
    /// # Arguments
    ///
    /// * `name` - The name of the file to sync.
    ///
    /// # Errors
    ///
    /// Returns a `LuceneError` if the file cannot be found or synchronized.
    pub fn fsync(&self, name: &str) -> Result<()> {
        IOUtils::fsync(&self.directory.join(name), false)
    }

    /// Try to delete any pending files that we had previously tried to delete
    /// but failed because we are on Windows and the files were still held
    /// open.
    pub fn delete_pending_files(
        directory: &Path,
        pending_deletes: &mut HashSet<String>,
    ) -> Result<()> {
        if !pending_deletes.is_empty() {
            // TODO: we could fix IndexInputs from FSDirectory SubStruct to
            // call this when they are closed?

            // Clone the set since we mutate it in privateDeleteFile:
            let files_to_delete: Vec<String> = pending_deletes.clone().into_iter().collect();

            for name in files_to_delete {
                Self::private_delete_file(directory, &name, true, pending_deletes)?;
            }
        }
        Ok(())
    }

    fn private_delete_file(
        directory: &Path,
        name: &str,
        is_pending_delete: bool,
        pending_deletes: &mut HashSet<String>,
    ) -> Result<()> {
        let file_path = directory.join(name);
        let file_name = file_path.to_string_lossy().to_string();
        match fs::remove_file(file_path) {
            Ok(_) => {
                pending_deletes.remove(name);
                Ok(())
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                pending_deletes.remove(name);

                if is_pending_delete && cfg!(windows) {
                    // TODO: can we remove this OS-specific hacky logic?  If
                    // windows deleteFile is buggy, we
                    // should instead contain this workaround in
                    // a WindowsFSDirectory ...
                    // LUCENE-6684: we suppress this check for Windows, since a
                    // file could be in a confusing "pending
                    // delete" state, failing the first
                    // delete attempt with access denied and then apparently
                    // falsely failing here when we try ot
                    // delete it again, with NSFE/FNFE
                    Ok(())
                } else {
                    Err(LuceneError::io_with_path(file_name, e))
                }
            },
            Err(e) => {
                // On windows, a file delete can fail because there's still an
                // open file handle against it.  We record this
                // in pendingDeletes and try again later.

                // TODO: this is hacky/lenient (we don't know which IOException
                // this is), and it should only happen on
                // filesystems that can do this, so really we should
                // move this logic to WindowsDirectory or something

                // TODO: can/should we do if (Constants.WINDOWS) here, else
                // throw the exc? but what about a Linux box
                // with a CIFS mount?
                if cfg!(windows) {
                    pending_deletes.insert(name.to_string());
                    Ok(())
                } else {
                    Err(LuceneError::io_with_path(file_name, e))
                }
            },
        }
    }
    fn ensure_can_read(&self, name: &str) -> Result<()> {
        let pending_deletes = self.pending_deletes.lock();
        if pending_deletes.contains(name) {
            return Err(LuceneError::not_found(format!(
                "file \"{name}\" is pending delete and cannot be opened for read"
            )));
        }
        Ok(())
    }
}
impl<T> FSDirectory<NativeFSLockFactory, T>
where
    T: FSDirectoryBase,
{
    pub fn new(
        directory: PathBuf,
        sub_fs_directory: T,
    ) -> Result<FSDirectory<NativeFSLockFactory, T>> {
        Self::with_lock_factory(directory, NativeFSLockFactory::new(), sub_fs_directory)
    }
}

impl<D, T> HasIdentity for FSDirectory<D, T>
where
    D: LockFactory,
    T: FSDirectoryBase,
{
    fn identity(&self) -> &Identity {
        &self.id
    }
}

impl<D, T> Directory for FSDirectory<D, T>
where
    D: LockFactory,
    T: FSDirectoryBase,
{
    fn list_all(&self) -> Result<Vec<String>> {
        let pending_deletes = self.pending_deletes.lock();
        Self::list_all(&self.directory, Some(&pending_deletes))
    }

    fn delete_file(&self, name: &str) -> Result<()> {
        let mut pending_deletes = self.pending_deletes.lock();
        if pending_deletes.contains(name) {
            return Err(LuceneError::not_found(format!(
                "file \"{name}\" is already pending delete"
            )));
        }

        Self::private_delete_file(&self.directory, name, false, &mut pending_deletes)?;

        Self::maybe_delete_pending_files(
            &self.directory,
            &mut pending_deletes,
            &self.ops_since_last_delete,
        )?;

        Ok(())
    }

    fn file_length(&self, name: &str) -> Result<usize> {
        if self.pending_deletes.lock().contains(name) {
            return Err(LuceneError::not_found(format!(
                "file \"{name}\" is pending delete"
            )));
        }

        let file_path = self.directory.join(name);
        let file_name = file_path.to_string_lossy().to_string();
        let metadata =
            fs::metadata(file_path).map_err(|e| LuceneError::io_with_path(file_name, e))?;
        let length = metadata.len();
        Ok(length as usize)
    }
    fn create_output(&self, name: &str, _context: &IOContext) -> Result<Self::IndexOutput> {
        let mut pending_deletes = self.pending_deletes.lock();
        Self::maybe_delete_pending_files(
            &self.directory,
            &mut pending_deletes,
            &self.ops_since_last_delete,
        )?;

        if pending_deletes.remove(name) {
            Self::private_delete_file(&self.directory, name, true, &mut pending_deletes)?;
            pending_deletes.remove(name);
        }

        let file_path = self.directory.join(name);
        let file = File::options()
            .write(true)
            .create_new(true)
            .open(&file_path)
            .map_err(|err| {
                LuceneError::io_with_path(file_path.to_string_lossy().to_string(), err)
            })?;

        OutputStreamIndexOutput::new(
            format!("FSIndexOutput(path=\"{}\")", file_path.display()).as_str(),
            name,
            file,
            CHUNK_SIZE,
        )
    }

    type IndexOutput = OutputStreamIndexOutput<File>;
    fn create_temp_output(
        &self,
        prefix: &str,
        suffix: &str,
        _context: &IOContext,
    ) -> Result<Self::IndexOutput> {
        let mut pending_deletes = self.pending_deletes.lock();
        Self::maybe_delete_pending_files(
            &self.directory,
            &mut pending_deletes,
            &self.ops_since_last_delete,
        )?;

        loop {
            let counter = self.next_temp_file_counter.fetch_add(1, SeqCst);
            let name = get_temp_file_name(prefix, suffix, counter);

            if pending_deletes.contains(&name) {
                continue;
            }

            let file_path = self.directory.join(&name);
            match File::options()
                .write(true)
                .create_new(true)
                .open(&file_path)
            {
                Ok(file) => {
                    return OutputStreamIndexOutput::new(
                        format!("FSIndexOutput(path=\"{}\")", file_path.display()).as_str(),
                        &name,
                        file,
                        CHUNK_SIZE,
                    );
                },
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    continue;
                },
                Err(e) => {
                    return Err(LuceneError::io_with_path(
                        file_path.to_string_lossy().to_string(),
                        e,
                    ));
                },
            }
        }
    }

    fn sync(&self, names: &[String]) -> Result<()> {
        for name in names {
            self.fsync(name)?;
        }
        Self::maybe_delete_pending_files(
            &self.directory,
            &mut self.pending_deletes.lock(),
            &self.ops_since_last_delete,
        )?;
        Ok(())
    }

    fn sync_metadata(&self) -> Result<()> {
        // TODO: to improve listCommits(), IndexFileDeleter could call this
        // after deleting segments_Ns
        IOUtils::fsync(&self.directory, true)?;
        Self::maybe_delete_pending_files(
            &self.directory,
            &mut self.pending_deletes.lock(),
            &self.ops_since_last_delete,
        )?;
        Ok(())
    }

    fn rename(&self, source: &str, dest: &str) -> Result<()> {
        let mut pending_deletes = self.pending_deletes.lock();
        if pending_deletes.contains(source) {
            return Err(LuceneError::not_found(format!(
                "File \"{source}\" is pending delete and cannot be moved"
            )));
        }
        Self::maybe_delete_pending_files(
            &self.directory,
            &mut pending_deletes,
            &self.ops_since_last_delete,
        )?;

        if pending_deletes.remove(dest) {
            Self::private_delete_file(&self.directory, dest, true, &mut pending_deletes)?; // try again to delete it - this is the best effort
            pending_deletes.remove(dest); // watch out if the delete fails, it's
            // back in here
        }

        let source_path = self.directory.join(source);
        let dest_path = self.directory.join(dest);

        fs::rename(source_path, dest_path).map_err(LuceneError::io)?;

        Ok(())
    }

    type IndexInput = T::Output;
    fn open_input(&self, name: &str, context: &IOContext) -> Result<Self::IndexInput> {
        self.ensure_can_read(name)?;
        self.sub_fs_directory
            .open_input(name, context, &self.directory)
    }

    type Lock = D::Lock;

    fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
        self.base.obtain_lock(&self.directory, name)
    }

    fn get_pending_deletions(&self) -> Result<HashSet<String>> {
        let mut pending_deletes = self.pending_deletes.lock();
        Self::delete_pending_files(&self.directory, &mut pending_deletes)?;
        if pending_deletes.is_empty() {
            Ok(HashSet::new())
        } else {
            Ok(pending_deletes.clone())
        }
    }

    #[cfg(debug_assertions)]
    fn is_fs_directory(&self) -> bool {
        true
    }

    fn ensure_open(&self) -> Result<()> {
        self.base.ensure_open()
    }
}

impl<D, T> Closeable for FSDirectory<D, T>
where
    D: LockFactory,
    T: FSDirectoryBase,
{
    fn close(&mut self) -> Result<()> {
        // TODO
        Ok(())
    }
}

impl<D, T> Display for FSDirectory<D, T>
where
    D: LockFactory,
    T: FSDirectoryBase,
{
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        write!(
            f,
            "{}@{} lockFactory={}",
            self.sub_fs_directory,
            self.directory.display(),
            self.base.lock_factory,
        )
    }
}

impl<D, T> BaseDirectory for FSDirectory<D, T>
where
    D: LockFactory,
    T: FSDirectoryBase,
{
    type LockFactory = D;

    fn get_lock_factory(&self) -> &BaseDirectoryBase<Self::LockFactory> {
        &self.base
    }
}
impl<D, T> Drop for FSDirectory<D, T>
where
    D: LockFactory,
    T: FSDirectoryBase,
{
    fn drop(&mut self) {
        let mut pending_deletes = self.pending_deletes.lock();
        if let Err(e) = Self::maybe_delete_pending_files(
            &self.directory,
            &mut pending_deletes,
            &self.ops_since_last_delete,
        ) {
            eprintln!("Error while deleting pending files during drop, ignoring: {e:?}");
        }
    }
}
/// The maximum chunk size is 8192 bytes in the original Java implementation
/// because:
/// - On certain platforms, Java's FileChannel or native I/O layers allocate a
///   native buffer (outside the heap) for each write operation if the writing
///   size exceeds 8192 bytes.
/// - Limiting the chunk size avoids unnecessary native memory allocation and
///   improves performance.
///
/// In Rust, this restriction is not necessary when using `BufWriter`, because:
/// - `BufWriter` internally manages a buffer with a default size of 8192 bytes,
///   which optimizes the write operations by batching smaller writes into a
///   single larger writing.
/// - There is no native memory allocation overhead similar to Java's
///   FileChannel behavior.
///
/// As a result, in Rust, we can safely rely on `BufWriter` for efficient
/// buffered writes without manually enforcing a chunk size limit.
const CHUNK_SIZE: i32 = 8192;
