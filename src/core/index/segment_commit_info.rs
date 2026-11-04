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
use crate::core::codecs::codec::Codec;
use crate::core::codecs::live_docs_format::LiveDocsFormat;
use crate::core::index::segment_info::{SegmentInfo, named_for_this_segment};
use crate::core::store::directory::Directory;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::{StringHelper, TryIntoInt};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

/// Embeds a [read-only] [SegmentInfo] and adds per-commit fields.
///
/// @lucene.experimental
pub struct SegmentCommitInfo<D> {
  /// The SegmentInfo that we wrap.
  pub info: Arc<SegmentInfo<D>>,
  /// Id that uniquely identifies this segment commit.
  id: Option<[u8; StringHelper::ID_LENGTH]>,
  /// How many deleted docs in the segment.
  del_count: i32,
  /// How many soft-deleted docs in the segment that are not also
  /// hard-deleted.
  soft_del_count: i32,
  /// Generation number of the live docs file (-1 if there are no deletes
  /// yet).
  del_gen: i64,
  /// Normally 1 + del_gen, unless an error was hit on the last attempt
  /// to write.
  next_write_del_gen: i64,
  /// Generation number of the FieldInfos (-1 if there are no updates).
  field_infos_gen: i64,
  /// Normally 1 + field_infos_gen, unless an error was hit on the last
  /// attempt to write.
  next_write_field_infos_gen: i64,
  /// Generation number of the DocValues (-1 if there are no updates).
  doc_values_gen: i64,
  /// Normally 1 + doc_values_gen, unless an error was hit on the last
  /// attempt to write.
  next_write_doc_values_gen: i64,
  /// Track the per-field DocValues update files.
  dv_updates_files: HashMap<i32, HashSet<String>>,
  /// Track the FieldInfos update files.
  field_infos_files: HashSet<String>,
  /// Size of the segment in bytes (-1 of unknown).
  size_in_bytes: AtomicI64,
  /// Used in memory by IndexWriter to track buffered deletes. Not persisted
  /// to disk.
  buffered_deletes_gen: i64,
}
impl<D> SegmentCommitInfo<D> {
  /// Creates a new instance.
  ///
  /// # Arguments
  /// - `info`: The [`SegmentInfo`] that this wraps.
  /// - `del_count`: Number of deleted documents in this segment.
  /// - `Soft_del_count`: Number of soft-deleted documents not also
  ///   hard-deleted.
  /// - `del_gen`: Deletion generation number (used to name deletion files).
  /// - `field_infos_gen`: FieldInfos generation number (used to name
  ///   field-infos files).
  /// - `Doc_values_gen`: DocValues generation number (used to name doc-values
  ///   updates files).
  /// - `ID`: ID that uniquely identifies this segment commit.
  pub fn new<T>(
    info: T,
    del_count: i32,
    soft_del_count: i32,
    del_gen: i64,
    field_infos_gen: i64,
    doc_values_gen: i64,
    id: Option<[u8; StringHelper::ID_LENGTH]>,
  ) -> Self
  where
    T: Into<Arc<SegmentInfo<D>>>,
  {
    Self {
      info: info.into(),
      del_count,
      soft_del_count,
      del_gen,
      next_write_del_gen: if del_gen == -1 {
        1
      } else {
        del_gen.wrapping_add(1)
      },
      field_infos_gen,
      next_write_field_infos_gen: if field_infos_gen == -1 {
        1
      } else {
        field_infos_gen.wrapping_add(1)
      },
      doc_values_gen,
      next_write_doc_values_gen: if doc_values_gen == -1 {
        1
      } else {
        doc_values_gen.wrapping_add(1)
      },
      id,
      dv_updates_files: HashMap::new(),
      field_infos_files: HashSet::new(),
      size_in_bytes: AtomicI64::new(-1),
      buffered_deletes_gen: -1,
    }
  }
  /// Returns a reference to the per-field DocValues updates files.
  pub fn get_doc_values_updates_files(&self) -> &HashMap<i32, HashSet<String>> {
    &self.dv_updates_files
  }

  /// Removes and returns the per-field DocValues updates files.
  pub(crate) fn take_doc_values_updates_files(&mut self) -> HashMap<i32, HashSet<String>> {
    std::mem::take(&mut self.dv_updates_files)
  }

  /// Sets the DocValues updates file names, per field number. Borrowed maps
  /// are copied into renamed sets; owned maps are renamed in place.
  pub fn set_doc_values_updates_files<'a>(
    &mut self,
    dv_updates_files: Cow<'a, HashMap<i32, HashSet<String>>>,
  ) {
    self.dv_updates_files.clear();
    match dv_updates_files {
      Cow::Borrowed(files) => {
        self.dv_updates_files.reserve(files.len());
        for (key, file_set) in files {
          let mut renamed_set = HashSet::with_capacity(file_set.len());
          for file in file_set {
            renamed_set.insert(named_for_this_segment(&self.info.name, file));
          }
          self.dv_updates_files.insert(*key, renamed_set);
        }
      },
      Cow::Owned(mut files) => {
        for file_set in files.values_mut() {
          let previous_files = std::mem::replace(file_set, HashSet::with_capacity(file_set.len()));
          for file in previous_files {
            file_set.insert(named_for_this_segment(&self.info.name, file));
          }
        }
        self.dv_updates_files = files;
      },
    }
  }
  /// Returns a reference to the FieldInfos file names.
  /// Returns the [`FieldInfos`](crate::core::index::field_infos::FieldInfos) file names.
  pub fn get_field_infos_files(&self) -> &HashSet<String> {
    &self.field_infos_files
  }

  /// Sets the FieldInfos file names.
  /// Sets the [`FieldInfos`](crate::core::index::field_infos::FieldInfos) file names.
  pub fn set_field_infos_files<I, S>(&mut self, field_infos_files: I)
  where
    I: IntoIterator<Item = S>,
    S: AsRef<str> + Into<String>,
  {
    self.field_infos_files.clear();
    for file in field_infos_files {
      self
        .field_infos_files
        .insert(named_for_this_segment(&self.info.name, file));
    }
  }

  /// Called when we succeed in writing deletes.
  pub fn advance_del_gen(&mut self) {
    self.del_gen = self.next_write_del_gen;
    self.next_write_del_gen = self.del_gen.wrapping_add(1);
    self.generation_advanced();
  }

  /// Called if there was an error while writing deletes, so that we don't
  /// try to write to the same file more than once.
  pub fn advance_next_write_del_gen(&mut self) {
    self.next_write_del_gen = self.next_write_del_gen.wrapping_add(1);
  }

  /// Gets the `nextWriteDelGen`.
  pub fn get_next_write_del_gen(&self) -> i64 {
    self.next_write_del_gen
  }

  /// Sets the `nextWriteDelGen`.
  pub fn set_next_write_del_gen(&mut self, v: i64) {
    self.next_write_del_gen = v;
  }

  /// Called when we succeed in writing a new FieldInfos generation.
  pub fn advance_field_infos_gen(&mut self) {
    self.field_infos_gen = self.next_write_field_infos_gen;
    self.next_write_field_infos_gen = self.field_infos_gen.wrapping_add(1);
    self.generation_advanced();
  }

  /// Called if there was an error while writing a new generation of
  /// FieldInfos, so that we don't try to write to the same file more than
  /// once.
  pub fn advance_next_write_field_infos_gen(&mut self) {
    self.next_write_field_infos_gen = self.next_write_field_infos_gen.wrapping_add(1);
  }

  /// Gets the `nextWriteFieldInfosGen`.
  pub fn get_next_write_field_infos_gen(&self) -> i64 {
    self.next_write_field_infos_gen
  }

  /// Sets the `nextWriteFieldInfosGen`.
  pub fn set_next_write_field_infos_gen(&mut self, v: i64) {
    self.next_write_field_infos_gen = v;
  }

  /// Called when we succeed in writing a new DocValues generation.
  pub fn advance_doc_values_gen(&mut self) {
    self.doc_values_gen = self.next_write_doc_values_gen;
    self.next_write_doc_values_gen = self.doc_values_gen.wrapping_add(1);
    self.generation_advanced();
  }

  /// Called if there was an error while writing a new generation of
  /// DocValues, so that we don't try to write to the same file more than
  /// once.
  pub fn advance_next_write_doc_values_gen(&mut self) {
    self.next_write_doc_values_gen = self.next_write_doc_values_gen.wrapping_add(1);
  }

  /// Gets the `nextWriteDocValuesGen`.
  pub fn get_next_write_doc_values_gen(&self) -> i64 {
    self.next_write_doc_values_gen
  }

  /// Sets the `nextWriteDocValuesGen`.
  pub fn set_next_write_doc_values_gen(&mut self, v: i64) {
    self.next_write_doc_values_gen = v;
  }

  /// Returns the total size in bytes of all files for this segment.
  pub fn size_in_bytes(&self) -> Result<i64>
  where
    D: Directory,
  {
    let current_size = self.size_in_bytes.load(Ordering::SeqCst);
    if current_size != -1 {
      return Ok(current_size);
    }
    let mut sum = 0;
    let files = if !self.has_deletions()
      && self.field_infos_files.is_empty()
      && self.dv_updates_files.is_empty()
    {
      Cow::Borrowed(self.info.files()?)
    } else {
      Cow::Owned(self.files()?)
    };
    for file_name in files.iter() {
      sum += self.info.dir.file_length(file_name)?;
    }
    let v: i64 = sum.try_convert()?;
    self.size_in_bytes.store(v, Ordering::SeqCst);

    Ok(v)
  }
  /// Returns all files in use by this segment.
  pub fn files(&self) -> Result<HashSet<String>> {
    // Start from the wrapped info's files (deep copy):
    let update_file_count = self
      .dv_updates_files
      .values()
      .map(HashSet::len)
      .sum::<usize>();
    let info_files = self.info.files()?;
    let mut files =
      HashSet::with_capacity(info_files.len() + update_file_count + self.field_infos_files.len());
    files.extend(info_files.iter().cloned());
    if self.has_deletions() {
      // debug_assert!(self.info.codec.is_some());
      self
        .info
        .get_codec()?
        .live_docs_format()
        .files(self, &mut files)?;
    }
    for update_files in self.dv_updates_files.values() {
      files.extend(update_files.iter().cloned());
    }
    // must separately add fieldInfos files
    files.extend(self.field_infos_files.iter().cloned());
    Ok(files)
  }

  /// Returns the buffered deletes generation number.
  pub fn get_buffered_deletes_gen(&self) -> i64 {
    self.buffered_deletes_gen
  }

  /// Sets the buffered deletes generation number.
  /// Can only be set once, otherwise it will return an error.
  pub fn set_buffered_deletes_gen(&mut self, v: i64) -> Result<()> {
    if self.buffered_deletes_gen == -1 {
      self.buffered_deletes_gen = v;
      self.generation_advanced();
      Ok(())
    } else {
      Err(LuceneError::illegal_state(
        "buffered deletes gen should only be set once",
      ))
    }
  }
  /// Determines if this segment has deletions.
  pub fn has_deletions(&self) -> bool {
    self.del_gen != -1
  }
  /// Returns `true` if there are any field updates for the segment in this
  /// commit.
  pub fn has_field_updates(&self) -> bool {
    self.field_infos_gen != -1
  }

  /// Returns the next available generation number of the FieldInfos files.
  pub fn get_next_field_infos_gen(&self) -> i64 {
    self.next_write_field_infos_gen
  }

  /// Returns the generation number of the field infos file, or `-1` if there
  /// are no field updates yet.
  pub fn get_field_infos_gen(&self) -> i64 {
    self.field_infos_gen
  }

  /// Returns the next available generation number of the DocValues files.
  pub fn get_next_doc_values_gen(&self) -> i64 {
    self.next_write_doc_values_gen
  }

  /// Returns the generation number of the DocValues file or `-1` if there are
  /// no doc-values updates yet.
  pub fn get_doc_values_gen(&self) -> i64 {
    self.doc_values_gen
  }

  /// Returns the next available generation number of the live docs file.
  pub fn get_next_del_gen(&self) -> i64 {
    self.next_write_del_gen
  }

  /// Returns the generation number of the live docs file or `-1` if there are
  /// no deletes yet.
  pub fn get_del_gen(&self) -> i64 {
    self.del_gen
  }

  /// Returns the number of deleted docs in the segment.
  pub fn get_del_count(&self) -> i32 {
    self.del_count
  }

  /// Returns the number of only soft-deleted docs.
  pub fn get_soft_del_count(&self) -> i32 {
    self.soft_del_count
  }

  pub fn set_del_count(&mut self, del_count: i32) -> Result<()> {
    let max_doc = self.info.max_doc()?;
    if del_count < 0 || del_count > max_doc {
      return Err(LuceneError::illegal_argument(format!(
        "invalid delCount={del_count} (maxDoc={max_doc})"
      )));
    }

    debug_assert!(
      self.soft_del_count + del_count <= max_doc,
      "maxDoc={}, delCount={}, softDelCount={}",
      max_doc,
      del_count,
      self.soft_del_count
    );

    self.del_count = del_count;
    Ok(())
  }

  pub fn set_soft_del_count(&mut self, soft_del_count: i32) -> Result<()> {
    let max_doc = self.info.max_doc()?;
    validate_soft_del_count(self.del_count, max_doc, soft_del_count)?;
    self.soft_del_count = soft_del_count;
    Ok(())
  }
  pub fn set_soft_del_count_without_check(&mut self, soft_del_count: i32) {
    self.soft_del_count = soft_del_count;
  }

  /// Returns a description of this segment.
  pub fn to_string_with_pending_del_count(
    &self,
    pending_del_count: i32,
  ) -> std::result::Result<String, std::fmt::Error> {
    let mut s = SegmentInfo::to_string(&self.info, self.del_count + pending_del_count)?;

    if self.del_gen != -1 {
      write!(s, ":delGen={}", self.del_gen)?;
    }
    if self.field_infos_gen != -1 {
      write!(s, ":fieldInfosGen={}", self.field_infos_gen)?;
    }
    if self.doc_values_gen != -1 {
      write!(s, ":dvGen={}", self.doc_values_gen)?;
    }
    if self.soft_del_count > 0 {
      write!(s, " :softDel={}", self.soft_del_count)?;
    }
    if self.id.is_some() {
      write!(s, " :id={}", StringHelper::id_to_string(self.id.as_ref()))?;
    }
    Ok(s)
  }
  /// Returns the number of deleted documents in the segment.
  /// If `include_soft_deletes` is `true`, it includes soft-deleted documents.
  pub fn get_del_count_with_soft_deletes(&self, include_soft_deletes: bool) -> i32 {
    if include_soft_deletes {
      self.get_del_count() + self.get_soft_del_count()
    } else {
      self.get_del_count()
    }
  }

  /// Advances the generation, resetting `size_in_bytes` and generating a new
  /// `id`.
  pub fn generation_advanced(&mut self) {
    self.size_in_bytes.store(-1, Ordering::SeqCst);
    self.id = Option::from(StringHelper::random_id());
  }

  /// Returns an ID that uniquely identifies this segment commit.
  /// If no ID is assigned, returns `None`.
  pub fn get_id(&self) -> Option<&[u8; StringHelper::ID_LENGTH]> {
    if self.id.is_none() {
      None
    } else {
      self.id.as_ref()
    }
  }
}
pub fn validate_soft_del_count(del_count: i32, max_doc: i32, soft_del_count: i32) -> Result<()> {
  if soft_del_count < 0 || soft_del_count > max_doc {
    return Err(LuceneError::illegal_argument(format!(
      "invalid softDelCount={soft_del_count} (maxDoc={max_doc})"
    )));
  }

  debug_assert!(
    del_count + soft_del_count <= max_doc,
    "maxDoc={}, delCount={}, softDelCount={}",
    max_doc,
    del_count,
    soft_del_count
  );
  Ok(())
}
/// Implement `Display` for [`SegmentCommitInfo`].
impl<D> std::fmt::Display for SegmentCommitInfo<D> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", self.to_string_with_pending_del_count(0)?)
  }
}
impl<D> Clone for SegmentCommitInfo<D> {
  fn clone(&self) -> Self {
    let mut other = SegmentCommitInfo::new(
      self.info.clone(),
      self.del_count,
      self.soft_del_count,
      self.del_gen,
      self.field_infos_gen,
      self.doc_values_gen,
      self.get_id().copied(),
    );

    other.next_write_del_gen = self.next_write_del_gen;
    other.next_write_field_infos_gen = self.next_write_field_infos_gen;
    other.next_write_doc_values_gen = self.next_write_doc_values_gen;

    other.dv_updates_files.reserve(self.dv_updates_files.len());
    for (key, value) in &self.dv_updates_files {
      other.dv_updates_files.insert(*key, value.clone());
    }

    other.field_infos_files = self.field_infos_files.clone();

    other
  }
}
impl<D> PartialEq for SegmentCommitInfo<D> {
  fn eq(&self, other: &Self) -> bool {
    std::ptr::eq(self, other)
  }
}
