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
use crate::core::store::base_directory::{BaseDirectory, BaseDirectoryBase};
use crate::core::store::byte_buffers_data_input::ByteBuffersDataInput;
use crate::core::store::directory::Directory;
use crate::core::store::lock_factory::LockFactory;
use crate::core::store::single_instance_lock_factory::SingleInstanceLockFactory;
use crate::core::store::{
  ByteBuffersDataOutput, ByteBuffersIndexInput, ByteBuffersIndexInputOwned, ByteBuffersIndexOutput,
  ByteBuffersIndexOutputOnClose, IOContext,
};
use crate::core::util::clone::TryClone;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::{HasIdentity, TryIntoInt};
use crc32fast::Hasher;
use num_bigint::BigInt;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet, hash_map::Entry};
use std::fmt::{Display, Formatter};
use std::io::{Cursor, Error, ErrorKind};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

pub type CustomByteBuffersDirectoryOutputToInput =
  Arc<dyn Fn(&str, ByteBuffersDataOutput) -> Result<ByteBuffersIndexInputOwned> + Send + Sync>;

#[derive(Clone)]
pub enum BBOutputToInput {
  ManyBuffers,
  OneBuffer,
  ByteArray,
  NRTCachingDirectory(Arc<AtomicI64>),
  Custom(CustomByteBuffersDirectoryOutputToInput),
}

impl BBOutputToInput {
  pub fn custom<F>(output_to_input: F) -> Self
  where
    F:
      Fn(&str, ByteBuffersDataOutput) -> Result<ByteBuffersIndexInputOwned> + Send + Sync + 'static,
  {
    Self::Custom(Arc::new(output_to_input))
  }

  fn to_input(
    &self,
    file_name: &str,
    output: ByteBuffersDataOutput,
  ) -> Result<ByteBuffersIndexInputOwned> {
    match self {
      Self::ManyBuffers => output_as_many_buffers(file_name, output),
      Self::OneBuffer => output_as_one_buffer(file_name, output),
      Self::ByteArray => output_as_byte_array(file_name, output),
      Self::NRTCachingDirectory(cache_size) => {
        let size: i64 = output.size().try_convert()?;
        cache_size.fetch_add(size, Ordering::SeqCst);
        output_as_many_buffers(file_name, output)
      },
      Self::Custom(output_to_input) => output_to_input(file_name, output),
    }
  }
}

pub const OUTPUT_AS_MANY_BUFFERS: BBOutputToInput = BBOutputToInput::ManyBuffers;
pub const OUTPUT_AS_ONE_BUFFER: BBOutputToInput = BBOutputToInput::OneBuffer;
pub const OUTPUT_AS_BYTE_ARRAY: BBOutputToInput = BBOutputToInput::ByteArray;

pub type CustomByteBuffersDirectoryOutputSupplier =
  Box<dyn Fn() -> ByteBuffersDataOutput + Send + Sync>;

pub enum BBOutputSupplier {
  ByteBuffersDataOutput,
  Custom(CustomByteBuffersDirectoryOutputSupplier),
}

impl BBOutputSupplier {
  pub fn custom<S>(bb_output_supplier: S) -> Self
  where
    S: Fn() -> ByteBuffersDataOutput + Send + Sync + 'static,
  {
    Self::Custom(Box::new(bb_output_supplier))
  }

  fn new_output(&self) -> ByteBuffersDataOutput {
    match self {
      Self::ByteBuffersDataOutput => ByteBuffersDataOutput::new(),
      Self::Custom(bb_output_supplier) => bb_output_supplier(),
    }
  }
}

pub const BYTE_BUFFERS_DATA_OUTPUT: BBOutputSupplier = BBOutputSupplier::ByteBuffersDataOutput;

pub(crate) type DirectoryByteBuffersIndexOutput =
  ByteBuffersIndexOutput<ByteBuffersDirectoryOutputOnClose>;

struct ByteBuffersDirectoryTempFileName {
  counter: AtomicU64,
}

impl ByteBuffersDirectoryTempFileName {
  fn new() -> Self {
    Self {
      counter: AtomicU64::new(0),
    }
  }

  fn apply(&self, suffix: &str) -> String {
    let counter = self.counter.fetch_add(1, Ordering::SeqCst);
    format!("{}_{}", suffix, BigInt::from(counter).to_str_radix(36))
  }
}

/// Converts the buffered output to an input backed by the output's blocks.
fn output_as_many_buffers(
  file_name: &str,
  mut output: ByteBuffersDataOutput,
) -> Result<ByteBuffersIndexInputOwned> {
  let data_input = output.get_data_input_owner(false)?;
  let input_name = format!(
    "{} (file={}, buffers={})",
    std::any::type_name::<ByteBuffersIndexInputOwned>()
      .rsplit("::")
      .next()
      .unwrap_or("ByteBuffersIndexInput"),
    file_name,
    data_input
  );
  Ok(ByteBuffersIndexInput::new(data_input, &input_name))
}

/// Converts the buffered output to an input backed by one contiguous buffer.
fn output_as_one_buffer(
  file_name: &str,
  mut output: ByteBuffersDataOutput,
) -> Result<ByteBuffersIndexInputOwned> {
  let bytes = output.try_get_array_ownership();
  let length = bytes.len();
  let data_input = ByteBuffersDataInput::new(vec![Cursor::new(bytes)], length)?;
  let input_name = format!(
    "{} (file={}, buffers={})",
    std::any::type_name::<ByteBuffersIndexInputOwned>()
      .rsplit("::")
      .next()
      .unwrap_or("ByteBuffersIndexInput"),
    file_name,
    data_input
  );
  Ok(ByteBuffersIndexInput::new(data_input, &input_name))
}

/// Converts the buffered output to an input backed by one byte array.
fn output_as_byte_array(
  file_name: &str,
  output: ByteBuffersDataOutput,
) -> Result<ByteBuffersIndexInputOwned> {
  output_as_one_buffer(file_name, output)
}

/// A `ByteBuffer`-based [`Directory`] implementation that can be used to store
/// index files on the heap.
///
/// Important: Note that `MMapDirectory` is nearly always a better choice as it
/// uses OS caches more effectively (through memory-mapped buffers). A
/// heap-based directory like this one can have the advantage in case of
/// ephemeral, small, short-lived indexes when disk syncs provide an additional
/// overhead.
pub struct ByteBuffersDirectory<LF>
where
  LF: LockFactory,
{
  temp_file_name: ByteBuffersDirectoryTempFileName,
  files: Mutex<HashMap<String, Arc<Mutex<FileEntry>>>>,

  /// Conversion between a buffered index output and the corresponding index
  /// input for a given file.
  output_to_input: BBOutputToInput,

  /// A supplier of [`ByteBuffersDataOutput`] instances used to buffer up the
  /// content of written files.
  bb_output_supplier: BBOutputSupplier,

  base: BaseDirectoryBase<LF>,
  id: Identity,
}

impl ByteBuffersDirectory<SingleInstanceLockFactory> {
  pub fn new() -> Self {
    Self::with_lock_factory(SingleInstanceLockFactory::new())
  }
}

impl Default for ByteBuffersDirectory<SingleInstanceLockFactory> {
  fn default() -> Self {
    Self::new()
  }
}

impl<LF> ByteBuffersDirectory<LF>
where
  LF: LockFactory,
{
  pub const OUTPUT_AS_MANY_BUFFERS: BBOutputToInput = BBOutputToInput::ManyBuffers;
  pub const OUTPUT_AS_ONE_BUFFER: BBOutputToInput = BBOutputToInput::OneBuffer;
  pub const OUTPUT_AS_BYTE_ARRAY: BBOutputToInput = BBOutputToInput::ByteArray;

  pub const BYTE_BUFFERS_DATA_OUTPUT: BBOutputSupplier = BBOutputSupplier::ByteBuffersDataOutput;

  pub fn with_lock_factory(lock_factory: LF) -> Self
  where
    LF: Send + Sync + 'static,
  {
    Self::with_output_strategy(
      lock_factory,
      Self::BYTE_BUFFERS_DATA_OUTPUT,
      Self::OUTPUT_AS_MANY_BUFFERS,
    )
  }

  pub fn with_output_strategy(
    lock_factory: LF,
    bb_output_supplier: BBOutputSupplier,
    output_to_input: BBOutputToInput,
  ) -> Self {
    Self {
      temp_file_name: ByteBuffersDirectoryTempFileName::new(),
      files: Mutex::new(HashMap::new()),
      output_to_input,
      bb_output_supplier,
      base: BaseDirectoryBase::new(lock_factory),
      id: Identity::new(),
    }
  }

  pub fn file_exists(&self, name: &str) -> Result<bool> {
    self.base.ensure_open()?;
    Ok(self.files.lock().contains_key(name))
  }
}

impl<LF> HasIdentity for ByteBuffersDirectory<LF>
where
  LF: LockFactory,
{
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl<LF> Directory for ByteBuffersDirectory<LF>
where
  LF: LockFactory + Send + Sync + 'static,
{
  fn list_all(&self) -> Result<Vec<String>> {
    self.ensure_open()?;
    let mut files: Vec<String> = self.files.lock().keys().cloned().collect();
    files.sort();
    Ok(files)
  }

  fn delete_file(&self, name: &str) -> Result<()> {
    self.ensure_open()?;
    let entry = self.files.lock().remove(name);
    let Some(entry) = entry else {
      return Err(LuceneError::io_with_path(
        name,
        Error::new(ErrorKind::NotFound, name.to_string()),
      ));
    };
    entry.lock().deleted = true;
    Ok(())
  }

  fn file_length(&self, name: &str) -> Result<usize> {
    self.ensure_open()?;
    let file = self.files.lock().get(name).cloned();
    match file {
      Some(file) => Ok(file.lock().length()),
      None => Err(LuceneError::io_with_path(
        name,
        Error::new(ErrorKind::NotFound, name.to_string()),
      )),
    }
  }

  fn create_output(&self, name: &str, _context: &IOContext) -> Result<Self::IndexOutput> {
    self.ensure_open()?;
    let entry = Arc::new(Mutex::new(FileEntry::new(name)));
    {
      let mut files = self.files.lock();
      if files.contains_key(name) {
        return Err(LuceneError::io_with_path(
          name,
          Error::new(
            ErrorKind::AlreadyExists,
            format!("File already exists: {name}"),
          ),
        ));
      }
      files.insert(name.to_string(), entry.clone());
    }
    create_output(
      entry,
      &self.bb_output_supplier,
      self.output_to_input.clone(),
    )
  }

  type IndexOutput = DirectoryByteBuffersIndexOutput;

  fn create_temp_output(
    &self,
    prefix: &str,
    suffix: &str,
    _context: &IOContext,
  ) -> Result<Self::IndexOutput> {
    self.ensure_open()?;
    loop {
      let segment_suffix = self.temp_file_name.apply(suffix);
      let name = IndexFileNames::segment_file_name(prefix, &segment_suffix, "tmp");
      let entry = Arc::new(Mutex::new(FileEntry::new(&name)));
      {
        let mut files = self.files.lock();
        if files.contains_key(&name) {
          continue;
        }
        files.insert(name, entry.clone());
      }
      return create_output(
        entry,
        &self.bb_output_supplier,
        self.output_to_input.clone(),
      );
    }
  }

  fn sync(&self, _names: &[String]) -> Result<()> {
    self.ensure_open()
  }

  fn sync_metadata(&self) -> Result<()> {
    self.ensure_open()
  }

  fn rename(&self, source: &str, dest: &str) -> Result<()> {
    self.ensure_open()?;
    let mut files = self.files.lock();
    let file = files.get(source).cloned().ok_or_else(|| {
      LuceneError::io_with_path(source, Error::new(ErrorKind::NotFound, source.to_string()))
    })?;
    match files.entry(dest.to_string()) {
      Entry::Occupied(_) => {
        return Err(LuceneError::io_with_path(
          dest,
          Error::new(ErrorKind::AlreadyExists, dest.to_string()),
        ));
      },
      Entry::Vacant(entry) => {
        entry.insert(file);
      },
    }
    if files.remove(source).is_none() {
      return Err(LuceneError::illegal_state(format!(
        "File was unexpectedly replaced: {source}"
      )));
    }
    Ok(())
  }

  type IndexInput = ByteBuffersIndexInputOwned;

  fn open_input(&self, name: &str, _context: &IOContext) -> Result<Self::IndexInput> {
    self.ensure_open()?;
    let file = self.files.lock().get(name).cloned();
    match file {
      Some(file) => file.lock().open_input(),
      None => Err(LuceneError::io_with_path(
        name,
        Error::new(ErrorKind::NotFound, name.to_string()),
      )),
    }
  }

  type Lock = LF::Lock;

  fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
    self.base.obtain_lock(Path::new(""), name)
  }

  fn get_pending_deletions(&self) -> Result<HashSet<String>> {
    Ok(HashSet::new())
  }

  fn ensure_open(&self) -> Result<()> {
    self.base.ensure_open()
  }
}

impl<LF> CloseableRef for ByteBuffersDirectory<LF>
where
  LF: LockFactory,
{
  fn close(&self) -> Result<()> {
    self.base.close();
    self.files.lock().clear();
    Ok(())
  }
}
impl<LF> Drop for ByteBuffersDirectory<LF>
where
  LF: LockFactory,
{
  fn drop(&mut self) {
    let _ = self.close();
  }
}

impl<LF> Display for ByteBuffersDirectory<LF>
where
  LF: LockFactory,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "ByteBuffersDirectory lockFactory={}",
      self.base.lock_factory
    )
  }
}

impl<LF> BaseDirectory for ByteBuffersDirectory<LF>
where
  LF: LockFactory + Send + Sync + 'static,
{
  type LockFactory = LF;

  fn get_lock_factory(&self) -> &BaseDirectoryBase<Self::LockFactory> {
    &self.base
  }
}

struct FileEntry {
  file_name: String,
  content: Option<ByteBuffersIndexInputOwned>,
  cached_length: usize,
  deleted: bool,
}

impl FileEntry {
  fn new(name: &str) -> Self {
    Self {
      file_name: name.to_string(),
      content: None,
      cached_length: 0,
      deleted: false,
    }
  }

  fn length(&self) -> usize {
    // We return 0 length until the IndexOutput is closed and flushed.
    self.cached_length
  }

  fn open_input(&self) -> Result<ByteBuffersIndexInputOwned> {
    let Some(content) = &self.content else {
      return Err(LuceneError::io_with_path(
        self.file_name.clone(),
        Error::new(
          ErrorKind::PermissionDenied,
          format!(
            "Can't open a file still open for writing: {}",
            self.file_name
          ),
        ),
      ));
    };
    content.try_clone()
  }
}

pub struct ByteBuffersDirectoryOutputOnClose {
  entry: Arc<Mutex<FileEntry>>,
  file_name: String,
  output_to_input: BBOutputToInput,
}

impl ByteBuffersDirectoryOutputOnClose {
  fn new(
    entry: Arc<Mutex<FileEntry>>,
    file_name: String,
    output_to_input: BBOutputToInput,
  ) -> Self {
    Self {
      entry,
      file_name,
      output_to_input,
    }
  }
}

impl ByteBuffersIndexOutputOnClose for ByteBuffersDirectoryOutputOnClose {
  fn on_close(&mut self, output: ByteBuffersDataOutput) -> Result<()> {
    let mut entry = self.entry.lock();
    // Defensive check for an output that was deleted before it was closed. In
    // that case the detached entry must not publish content or notify a custom
    // output conversion strategy.
    if entry.deleted {
      return Ok(());
    }
    let cached_length = output.size();
    let content = self.output_to_input.to_input(&self.file_name, output)?;
    entry.content = Some(content);
    entry.cached_length = cached_length;
    Ok(())
  }
}

fn create_output(
  entry: Arc<Mutex<FileEntry>>,
  bb_output_supplier: &BBOutputSupplier,
  output_to_input: BBOutputToInput,
) -> Result<DirectoryByteBuffersIndexOutput> {
  let file_name = {
    let entry = entry.lock();
    if entry.content.is_some() {
      return Err(LuceneError::io_with_path(
        entry.file_name.clone(),
        Error::new(
          ErrorKind::AlreadyExists,
          format!("Can only write to a file once: {}", entry.file_name),
        ),
      ));
    }
    entry.file_name.clone()
  };
  let output_name = format!("ByteBuffersDirectory output (file={file_name})");
  let output = bb_output_supplier.new_output();
  let on_close = ByteBuffersDirectoryOutputOnClose::new(entry, file_name.clone(), output_to_input);

  Ok(ByteBuffersIndexOutput::with_checksum_and_on_close(
    output,
    &output_name,
    &file_name,
    Hasher::new(),
    on_close,
  ))
}
