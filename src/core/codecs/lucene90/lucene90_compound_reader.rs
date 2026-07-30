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
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};

use parking_lot::Mutex;

use crate::core::codecs::CodecUtil;
use crate::core::codecs::compound_directory::CompoundDirectory;
use crate::core::codecs::lucene90::lucene90_compound_format::Lucene90CompoundFormat;
use crate::core::index::IndexFileNames;
use crate::core::index::index_reader::Identity;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::store::directory::Directory;
use crate::core::store::{IO_CONTEXT_DEFAULT, IOContext, IndexInput, ReadAdvice};
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::{HasIdentity, IOUtils, StringHelper, TryIntoInt};

/// Offset/Length for a slice inside of a compound file
pub struct FileEntry {
  pub offset: usize,
  pub length: usize,
}
/// Provides access to a compound stream. This struct implements a directory but
/// is limited to read-only operations. Any directory methods that attempt to
/// modify data will return an error.
///
/// # Note
/// This API is experimental and may change in future versions.
pub struct Lucene90CompoundReader<D>
where
  D: Directory,
{
  segment_name: String,
  entries: HashMap<String, FileEntry>,
  handle: Mutex<D::IndexInput>,

  version: i32,
  dir_fmt: String,
  id: Identity,
}
impl<D> Lucene90CompoundReader<D>
where
  D: Directory,
{
  pub fn new(directory: &D, si: &SegmentInfo<D>) -> Result<Self> {
    let segment_name = si.name.clone();
    let data_file_name =
      IndexFileNames::segment_file_name(&segment_name, "", Lucene90CompoundFormat::DATA_EXTENSION);
    let entries_file_name = IndexFileNames::segment_file_name(
      &segment_name,
      "",
      Lucene90CompoundFormat::ENTRIES_EXTENSION,
    );

    let (version, entries) = Self::read_entries(si.get_id(), directory, &entries_file_name)?;

    let mut handle = directory.open_input(
      &data_file_name,
      &IO_CONTEXT_DEFAULT.with_read_advice_self(ReadAdvice::Normal)?,
    )?;

    let expected_length = entries
      .values()
      .map(|e| e.offset + e.length)
      .max()
      .unwrap_or_else(|| CodecUtil::index_header_length(Lucene90CompoundFormat::DATA_CODEC, ""))
      + CodecUtil::footer_length();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      CodecUtil::check_index_header(
        &mut handle,
        Lucene90CompoundFormat::DATA_CODEC,
        version,
        version,
        si.get_id(),
        "",
      )?;
      // NOTE: data file is too costly to verify checksum against all the
      // bytes on open, but for now we at least verify proper
      // structure of the checksum footer: which looks
      // for FOOTER_MAGIC + algorithmID. This is cheap and can detect some
      // forms of corruption such as file truncation.
      let _ = CodecUtil::retrieve_checksum(&mut handle)?;
      // We also validate length, because e.g. if you strip 16 bytes off the
      // .cfs we otherwise would not detect it:
      let length = IndexInput::length(&handle)?;
      if length != expected_length {
        return Err(LuceneError::corrupt_index(format!(
          "length should be {expected_length} bytes, but is {length} instead (resource={handle})"
        )));
      }
      Ok(())
    }));
    match result {
      Ok(Ok(())) => {},
      Ok(Err(error)) => {
        IOUtils::close_resources_while_handling_error(&handle)?;
        return Err(error);
      },
      Err(payload) => {
        IOUtils::close_resources_while_handling_error(&handle)?;
        std::panic::resume_unwind(payload);
      },
    }
    let dir_fmt = directory.to_string();
    Ok(Self {
      segment_name,
      entries,
      handle: Mutex::new(handle),
      version,
      dir_fmt,
      id: Identity::new(),
    })
  }
  /// Helper method that reads CFS entries from an input stream.
  fn read_entries(
    segment_id: &[u8; StringHelper::ID_LENGTH],
    directory: &D,
    entries_file_name: &str,
  ) -> Result<(i32, HashMap<String, FileEntry>)> {
    let mut entries_stream = directory.open_checksum_input(entries_file_name)?;
    let mut footer_attempted = false;
    let mut result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      let result = (|| {
        let version = CodecUtil::check_index_header(
          &mut entries_stream,
          Lucene90CompoundFormat::ENTRY_CODEC,
          Lucene90CompoundFormat::VERSION_START,
          Lucene90CompoundFormat::VERSION_CURRENT,
          segment_id,
          "",
        )?;
        let mapping = Self::read_mapping(&mut entries_stream)?;
        Ok((version, mapping))
      })();

      footer_attempted = true;
      match result {
        Ok((version, mapping)) => {
          CodecUtil::check_footer(&mut entries_stream)?;
          Ok((version, mapping))
        },
        Err(e) => Err(CodecUtil::check_footer_with_error(&mut entries_stream, e)),
      }
    }));

    let footer_error = if let Err(payload) = &result
      && !footer_attempted
    {
      let error =
        LuceneError::tragedy_from_panic("panic while reading compound entries", payload.as_ref());
      Some(CodecUtil::check_footer_with_error(
        &mut entries_stream,
        error,
      ))
    } else {
      None
    };
    if let Some(error @ LuceneError::CorruptIndex(_)) = footer_error {
      result = Ok(Err(error));
    }

    let close_result =
      std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| entries_stream.close()));
    IOUtils::use_or_suppress_caught_result(result, close_result)
  }
  fn read_mapping(entries_stream: &mut impl IndexInput) -> Result<HashMap<String, FileEntry>> {
    let num_entries = entries_stream.read_vint()?;
    let mut mapping = HashMap::with_capacity(num_entries as usize);
    for _ in 0..num_entries {
      let id = entries_stream.read_string()?;
      if mapping.contains_key(&id) {
        return Err(LuceneError::corrupt_index(format!(
          "Duplicate cfs entry id={id} in CFS (resource={entries_stream})"
        )));
      }
      let offset = entries_stream.read_long()?.try_convert()?;
      let length = entries_stream.read_long()?.try_convert()?;
      mapping.insert(id, FileEntry { offset, length });
    }
    Ok(mapping)
  }
}

impl<D> HasIdentity for Lucene90CompoundReader<D>
where
  D: Directory,
{
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl<D> Directory for Lucene90CompoundReader<D>
where
  D: Directory,
{
  /// Returns an array of strings, one for each file in the directory.
  fn list_all(&self) -> Result<Vec<String>> {
    let mut res: Vec<String> = self.entries.keys().cloned().collect();
    for entry in &mut res {
      *entry = format!("{}{}", self.segment_name, entry);
    }
    Ok(res)
  }

  fn delete_file(&self, _name: &str) -> Result<()> {
    Err(LuceneError::unsupported_operation("delete_file"))
  }

  /// Returns the length of a file in the directory.
  fn file_length(&self, name: &str) -> Result<usize> {
    let stripped_name = IndexFileNames::strip_segment_name(name);
    let entry = self
      .entries
      .get(stripped_name)
      .ok_or_else(|| LuceneError::not_such_file(format!("{name} not found")))?;
    Ok(entry.length)
  }

  fn create_output(&self, _name: &str, _context: &IOContext) -> Result<Self::IndexOutput> {
    Err(LuceneError::unsupported_operation("create_output"))
  }

  type IndexOutput = D::IndexOutput;
  fn create_temp_output(
    &self,
    _prefix: &str,
    _suffix: &str,
    _context: &IOContext,
  ) -> Result<Self::IndexOutput> {
    Err(LuceneError::unsupported_operation("create_temp_output"))
  }

  fn sync(&self, _names: &[String]) -> Result<()> {
    Err(LuceneError::unsupported_operation("sync"))
  }

  fn sync_metadata(&self) -> Result<()> {
    Ok(())
  }

  fn rename(&self, _source: &str, _dest: &str) -> Result<()> {
    Err(LuceneError::unsupported_operation("rename"))
  }

  type IndexInput = D::IndexInput;

  fn open_input(&self, name: &str, context: &IOContext) -> Result<Self::IndexInput> {
    let id = IndexFileNames::strip_segment_name(name);
    let entry = match self.entries.get(id) {
      Some(entry) => entry,
      None => {
        let dat_file_name = IndexFileNames::segment_file_name(
          &self.segment_name,
          "",
          Lucene90CompoundFormat::DATA_EXTENSION,
        );
        return Err(LuceneError::not_such_file(format!(
          "No sub-file with id {} found in compound file \"{}\" (fileName={} files: {:?})",
          id,
          dat_file_name,
          name,
          self.entries.keys()
        )));
      },
    };
    let input = self.handle.lock().slice_with_read_advice(
      name,
      entry.offset,
      entry.length,
      context.get_read_advice(),
    )?;
    Ok(input)
  }

  type Lock = D::Lock;

  fn obtain_lock(&self, _name: &str) -> Result<Self::Lock> {
    Err(LuceneError::unsupported_operation("obtain_lock"))
  }

  fn get_pending_deletions(&self) -> Result<HashSet<String>> {
    Ok(HashSet::new())
  }
}
impl<D> Display for Lucene90CompoundReader<D>
where
  D: Directory,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "CompoundFileDirectory(segment=\"{}\" in dir={})",
      self.segment_name, self.dir_fmt
    )
  }
}

impl<D> CloseableRef for Lucene90CompoundReader<D>
where
  D: Directory,
{
  fn close(&self) -> Result<()> {
    self.handle.lock().close()
  }
}

impl<D> CompoundDirectory for Lucene90CompoundReader<D>
where
  D: Directory,
{
  fn check_integrity(&self) -> Result<()> {
    CodecUtil::checksum_entire_file(&*self.handle.lock())?;
    Ok(())
  }
}
