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
use std::fmt::{Display, Write};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::core::codecs::Codecs;
use crate::core::search::sort::Sort;
#[cfg(test)]
use crate::core::store::dummy::dummy_directory::DummyDirectory;
use crate::core::util::StringHelper;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::version::Version;

/// Information about a segment such as its name, directory, and files related
/// to the segment.
///
/// # Experimental
/// This API is experimental and may change in future releases.
pub struct SegmentInfo<D> {
  /// Unique segment name in the directory.
  pub(crate) name: Arc<str>,
  max_doc: i32, // number of docs in seg
  /// Where this segment resides.
  pub(crate) dir: Arc<D>,
  is_compound_file: bool,
  /// Id that uniquely identifies this segment.
  id: [u8; StringHelper::ID_LENGTH],
  #[cfg(test)]
  id_str: String,
  pub(crate) id_key: Arc<str>,
  codec: Option<Codecs>,
  diagnostics: HashMap<String, String>,
  attributes: HashMap<String, String>,
  pub(crate) index_sort: Option<Arc<Sort>>,
  /// Tracks the Lucene version this segment was created with, since 3.1.
  /// Null indicates an older than 3.0 index, and it's used to detect a
  /// too-old index. The format expected is "x.y" - "2.x" for pre-3.0
  /// indexes (or None), and specific versions afterward ("3.0.0",
  /// "3.1.0" etc.). See [`Version`](crate::core::util::version::Version) for details.
  pub(crate) version: Option<Version>,
  /// Tracks the minimum version that contributed documents to a segment.
  /// For flush segments, that is the version that wrote it.
  /// For merged segments, this is the minimum `min_version` of all the
  /// segments that have been merged into this segment.
  pub(crate) min_version: Option<Version>,
  has_blocks: bool,
  /// The SegmentCommitInfo wraps a SegmentInfo inside an Rc,
  /// but sometimes we need to modify the set_files of the SegmentInfo contained within the SegmentCommitInfo,
  set_files: Option<HashSet<String>>,
}

// padding using
#[cfg(test)]
impl Default for SegmentInfo<DummyDirectory> {
  fn default() -> Self {
    let id = [0; 16];
    let id_str = StringHelper::id_to_string(Some(&id));
    SegmentInfo {
      name: Arc::from(""),
      max_doc: -1,
      dir: Arc::new(DummyDirectory),
      is_compound_file: false,
      id: [0; 16],
      id_str: id_str.clone(),
      id_key: Arc::from(id_str),
      diagnostics: HashMap::new(),
      attributes: HashMap::new(),
      codec: None,
      index_sort: None,
      version: None,
      min_version: None,
      has_blocks: false,
      set_files: None,
    }
  }
}

/// Used by some member fields to mean not present (e.g., norms, deletions).
pub const NO: i32 = -1; // e.g. no norms; no deletes;
/// Used by some member fields to mean present (e.g., norms, deletions).
pub const YES: i32 = 1; // e.g. have norms; have deletes;
impl<D> SegmentInfo<D> {
  /// Constructs a new complete [`SegmentInfo`] instance from input.
  ///
  /// # Arguments
  ///
  /// * `dir` - Directory where this segment resides.
  /// * `version` - The Lucene version this segment was created with.
  /// * `min_version` - The minimum version that contributed documents to this
  ///   segment.
  /// * `name` - Unique segment name.
  /// * `max_doc` - Number of documents in the segment.
  /// * `is_compound_file` - Indicates if this segment uses a compound file
  ///   format.
  /// * `has_blocks` - Indicates if the segment has blocks.
  /// * `codec` - Codec that wrote this segment, or `None` while reading the
  ///   segment info file before [`SegmentInfos`](crate::core::index::segment_infos::SegmentInfos) fills it in.
  /// * `diagnostics` - Diagnostic information related to the segment.
  /// * `id` - Unique identifier for this segment.
  /// * `attributes` - Additional attributes for the segment.
  /// * `index_sort` - The sort order of the index, if any.
  #[allow(clippy::too_many_arguments)]
  pub fn new<N: Into<Arc<str>>>(
    dir: Arc<D>,
    version: Option<Version>,
    min_version: Option<Version>,
    name: N,
    max_doc: i32,
    is_compound_file: bool,
    has_blocks: bool,
    codec: Option<Codecs>,
    diagnostics: HashMap<String, String>,
    id: [u8; StringHelper::ID_LENGTH],
    attributes: HashMap<String, String>,
    index_sort: Option<Arc<Sort>>,
  ) -> Result<SegmentInfo<D>> {
    if version.is_none() {
      return Err(LuceneError::illegal_argument(
        "segment version must not be None",
      ));
    }
    if id.len() != StringHelper::ID_LENGTH {
      return Err(LuceneError::illegal_argument(format!("Invalid id: {id:?}")));
    }
    #[cfg(test)]
    let id_key = {
      // for easy debugging
      let random = StringHelper::id_to_string(Some(&StringHelper::random_id()));
      let id_str = StringHelper::id_to_string(Some(&id));
      format!("{}-{}", id_str, random)
    };

    #[cfg(not(test))]
    let id_key = StringHelper::id_to_string(Some(&StringHelper::random_id()));
    Ok(SegmentInfo {
      dir,
      version,
      min_version,
      name: name.into(),
      max_doc,
      is_compound_file,
      has_blocks,
      codec,
      diagnostics,
      id,
      #[cfg(test)]
      id_str: StringHelper::id_to_string(Some(&id)),
      id_key: id_key.into(),
      attributes,
      index_sort,
      set_files: None,
    })
  }
}
impl<D> SegmentInfo<D> {
  /// Returns this segment's name.
  pub fn name(&self) -> &str {
    &self.name
  }

  /// Returns the directory that contains this segment.
  pub fn dir(&self) -> &Arc<D> {
    &self.dir
  }

  /// Sets the diagnostics map by taking ownership of the given map.
  pub(crate) fn set_diagnostics(&mut self, diagnostics: HashMap<String, String>) {
    self.diagnostics = diagnostics;
  }

  /// Adds or modifies this segment's diagnostics.
  ///
  /// Entries in the given map whose keys are not present in the current
  /// diagnostics are added. Otherwise, existing entries are modified with
  /// the given map's value.
  ///
  /// # Arguments
  /// * `diagnostics` - The additional diagnostics to be added or modified.
  pub fn add_diagnostics(&mut self, diagnostics: HashMap<String, String>) {
    self.diagnostics.extend(diagnostics);
  }

  /// Can only be called once.
  pub fn set_codec<C>(&mut self, codec: C) -> Result<()>
  where
    C: Into<Codecs>,
  {
    if self.codec.is_some() {
      return Err(LuceneError::illegal_state("codec can only be set once"));
    }
    self.codec = Some(codec.into());
    Ok(())
  }

  /// Return the [`Codec`](crate::core::codecs::Codec) that wrote this segment.
  pub fn get_codec(&self) -> Result<&Codecs> {
    self
      .codec
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("segment codec is not set"))
  }

  /// Returns diagnostics saved into the segment when it was written.
  /// The map is immutable.
  pub fn get_diagnostics(&self) -> &HashMap<String, String> {
    &self.diagnostics
  }
  /// Marks whether this segment is stored as a compound file.
  ///
  /// # Arguments
  ///
  /// * `is_compound_file` - `true` if this is a compound file; otherwise,
  ///   `false`.
  pub(crate) fn set_use_compound_file(&mut self, is_compound_file: bool) {
    self.is_compound_file = is_compound_file;
  }
  /// Returns `true` if this segment is stored as a compound file; otherwise,
  /// `false`.
  pub fn get_use_compound_file(&self) -> bool {
    self.is_compound_file
  }

  /// Returns `true` if this segment contains documents written as blocks.
  ///
  /// # See Also
  /// [`LeafMetaData::get_has_blocks`](crate::core::index::leaf_metadata::LeafMetaData::get_has_blocks)
  pub fn get_has_blocks(&self) -> bool {
    self.has_blocks
  }

  /// Sets the `has_blocks` property to `true`. This setting is viral and
  /// can't be unset.
  pub(crate) fn set_has_blocks(&mut self) {
    self.has_blocks = true;
  }
  /// Returns the number of documents in this segment (deletions are not taken
  /// into account)
  pub fn max_doc(&self) -> Result<i32> {
    if self.max_doc == -1 {
      return Err(LuceneError::illegal_state("maxDoc isn't set yet"));
    }
    Ok(self.max_doc)
  }

  /// Sets the max_doc value, can only be called once
  pub(crate) fn set_max_doc(&mut self, max_doc: i32) -> Result<()> {
    if self.max_doc != -1 {
      return Err(LuceneError::illegal_state(format!(
        "maxDoc was already set: this.maxDoc={} vs maxDoc {}",
        self.max_doc, max_doc
      )));
    }
    self.max_doc = max_doc;
    Ok(())
  }

  /// Returns all files referenced by this SegmentInfo
  pub fn files(&self) -> Result<&HashSet<String>> {
    match self.set_files {
      Some(ref files) => Ok(files),
      None => Err(LuceneError::illegal_state(format!(
        "files were not set; segment={} maxDoc={}",
        self.name, self.max_doc
      ))),
    }
  }
  pub fn take_files(&mut self) -> Result<HashSet<String>> {
    match self.set_files.take() {
      Some(files) => Ok(files),
      None => Err(LuceneError::illegal_state(format!(
        "files were not set; segment={} maxDoc={}",
        self.name, self.max_doc
      ))),
    }
  }

  /// Sets the files written for this segment.
  pub fn set_files<I, S>(&mut self, files: I) -> Result<()>
  where
    I: IntoIterator<Item = S>,
    S: AsRef<str> + Into<String>,
  {
    self.set_files = Some(HashSet::new());
    self.add_files(files)
  }
  /// Replaces the files written for this segment and returns the previous set.
  pub(crate) fn replace_files<I, S>(&mut self, files: I) -> Result<HashSet<String>>
  where
    I: IntoIterator<Item = S>,
    S: AsRef<str> + Into<String>,
  {
    let previous = self.take_files()?;
    self.set_files(files)?;
    Ok(previous)
  }
  /// Converts this segment information into a formatted string with deletions
  /// count.
  ///
  /// # Arguments
  ///
  /// * `del_count` - Number of deletions in the segment.
  ///
  /// # Format
  ///
  /// `_a(3.1):c45/4:[sorter=<long: "timestamp">!]`
  ///
  /// - `_a`: The segment's name.
  /// - `(3.1)`: Lucene version used to create the segment (`?` if unknown).
  /// - `c`: Indicates the compound file format (`C` if not compound).
  /// - `45`: Number of documents in the segment.
  /// - `/4`: Number of deletions (only present if deletions exist).
  /// - `[sorter=<long: "timestamp">!]`: Indicates the segment is sorted by
  ///   the `timestamp` field in descending order (optional, omitted for
  ///   unsorted segments).
  pub fn to_string(&self, del_count: i32) -> std::result::Result<String, std::fmt::Error> {
    let mut s = String::new();
    s.push_str(&self.name);

    // 处理 version 字段
    s.push('(');
    if let Some(version) = &self.version {
      write!(s, "{version}")?;
    } else {
      s.push('?');
    }
    s.push(')');

    let cfs = if self.is_compound_file { 'c' } else { 'C' };
    s.push(':');
    s.push(cfs);

    write!(s, "{}", self.max_doc)?;

    if del_count != 0 {
      s.push('/');
      write!(s, "{del_count}")?;
    }

    if let Some(index_sort) = &self.index_sort {
      s.push_str(":[indexSort=");
      write!(s, "{index_sort}")?;
      s.push(']');
    }

    if !self.diagnostics.is_empty() {
      s.push_str(":[diagnostics=");
      write!(s, "{:?}", self.diagnostics)?;
      s.push(']');
    }

    if !self.attributes.is_empty() {
      s.push_str(":[attributes=");
      write!(s, "{:?}", self.attributes)?;
      s.push(']');
    }
    Ok(s)
  }
  /// Returns the version of the code which wrote the segment.
  pub fn get_version_ref(&self) -> Option<&Version> {
    self.version.as_ref()
  }

  /// Returns the minimum Lucene version that contributed documents to this
  /// segment, or `None` if it is unknown.
  pub fn get_min_version_ref(&self) -> Option<&Version> {
    self.min_version.as_ref()
  }
  pub fn get_min_version(&self) -> Option<Version> {
    self.min_version.as_ref().cloned()
  }

  /// Returns the id that uniquely identifies this segment.
  pub fn get_id(&self) -> &[u8; StringHelper::ID_LENGTH] {
    &self.id
  }
  pub fn get_id_key(&self) -> &str {
    &self.id_key
  }

  /// Add these files to the set of files written for this segment.
  pub fn add_files<I, S>(&mut self, files: I) -> Result<()>
  where
    I: IntoIterator<Item = S>,
    S: AsRef<str> + Into<String>,
  {
    let files: Vec<S> = files.into_iter().collect();
    self.check_file_names(&files)?;
    match self.set_files {
      Some(ref mut set_files) => {
        set_files.reserve(files.len());
        for f in files {
          set_files.insert(named_for_this_segment(&self.name, f));
        }
      },
      None => return Err(LuceneError::illegal_state("set_files was not initialized")),
    }
    Ok(())
  }

  /// Add this file to the set of files written for this segment.
  pub fn add_file<F>(&mut self, file: F) -> Result<()>
  where
    F: Into<String>,
  {
    let file = file.into();
    self.check_file_names(std::iter::once(&file))?;
    match self.set_files {
      Some(ref mut set_files) => {
        set_files.insert(named_for_this_segment(&self.name, file));
      },
      None => return Err(LuceneError::illegal_state("set_files was not initialized")),
    }
    Ok(())
  }

  fn check_file_names<I, S>(&self, files: I) -> Result<()>
  where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
  {
    for file in files {
      let file = file.as_ref();
      // Check if the file name matches the codec file pattern
      if !CODEC_FILE_PATTERN.is_match(file) {
        return Err(LuceneError::illegal_argument(format!(
          "Invalid codec filename '{}', must match: {}",
          file,
          CODEC_FILE_PATTERN.as_str()
        )));
      }

      if file.as_bytes()[file.len().saturating_sub(4)..].eq_ignore_ascii_case(b".tmp") {
        return Err(LuceneError::illegal_argument(format!(
          "Invalid codec filename '{file}', cannot end with .tmp extension"
        )));
      }
    }

    Ok(())
  }

  /// Get a codec attribute value, or None if it does not exist.
  pub fn get_attribute(&self, key: &str) -> Option<&str> {
    self.attributes.get(key).map(|v| v.as_str())
  }
  /// Puts a codec attribute value.
  ///
  /// This is a key-value mapping for the field that the codec can use to
  /// store additional metadata, and will be available to the codec when
  /// reading the segment via `get_attribute`.
  ///
  /// If a value already exists for the field, it will be replaced with the
  /// new value. This method ensures thread safety by making a
  /// copy-on-write for every attribute change.
  pub fn put_attribute<K, V>(&mut self, key: K, value: V) -> Option<String>
  where
    K: Into<String>,
    V: Into<String>,
  {
    let value = value.into();
    let key = key.into();
    // This needs to be thread-safe because multiple threads may be updating
    // (different) attributes at the same time due to concurrent
    // merging, while other threads may be formatting
    // segment info while other threads are updating attributes.
    self.attributes.insert(key, value)
  }
  /// Returns the internal codec attributes map.
  pub fn get_attributes(&self) -> Result<&HashMap<String, String>> {
    Ok(&self.attributes)
  }

  /// Returns the sort order of this segment, or None if the index has no
  /// sort.
  pub fn get_index_sort(&self) -> Option<Arc<Sort>> {
    self.index_sort.as_ref().cloned()
  }

  // padding using
  pub(crate) fn dummy(dir: Arc<D>, name: Arc<str>) -> Self {
    let id = [0u8; StringHelper::ID_LENGTH];
    let id_str = "0";
    SegmentInfo {
      name,
      max_doc: 0,
      dir,
      is_compound_file: false,
      id,
      #[cfg(test)]
      id_str: id_str.to_string(),
      id_key: Arc::from(id_str),
      diagnostics: HashMap::new(),
      attributes: HashMap::new(),
      codec: None,
      index_sort: None,
      version: None,
      min_version: None,
      has_blocks: false,
      set_files: None,
    }
  }
}
impl<D> PartialEq for SegmentInfo<D> {
  fn eq(&self, other: &Self) -> bool {
    Arc::ptr_eq(&self.dir, &other.dir) && self.name == other.name
  }
}
impl<D> Eq for SegmentInfo<D> {}

impl<D> Hash for SegmentInfo<D> {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    Arc::as_ptr(&self.dir).hash(state);
    self.name.hash(state);
  }
}
impl<D> Display for SegmentInfo<D> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", self.to_string(0)?)
  }
}
#[cfg(test)]
impl<D> Clone for SegmentInfo<D> {
  fn clone(&self) -> Self {
    SegmentInfo {
      name: self.name.clone(),
      max_doc: self.max_doc,
      dir: self.dir.clone(),
      is_compound_file: self.is_compound_file,
      id: self.id,
      id_str: self.id_str.clone(),
      id_key: self.id_key.clone(),
      diagnostics: self.diagnostics.clone(),
      attributes: self.attributes.clone(),
      codec: self.codec.clone(),
      index_sort: self.index_sort.clone(),
      version: self.version.clone(),
      min_version: self.min_version.clone(),
      has_blocks: self.has_blocks,
      set_files: self.set_files.clone(),
    }
  }
}

use crate::core::index::{CODEC_FILE_PATTERN, IndexFileNames};

/// Strips any segment name from the file and renames it with this segment.
/// This is because "segment names" can change, e.g., by addIndexes(Dir).
pub fn named_for_this_segment<T: AsRef<str> + Into<String>>(name: &str, file: T) -> String {
  let mut file = file.into();
  let stripped = IndexFileNames::strip_segment_name(&file);
  if file.len() == name.len() + stripped.len() && file.starts_with(name) {
    file
  } else {
    let prefix_length = file.len() - stripped.len();
    file.replace_range(..prefix_length, name);
    file
  }
}
