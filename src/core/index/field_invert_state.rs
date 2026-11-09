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
use crate::core::index::field_info::FieldInfo;
use crate::core::index::index_options::IndexOptions;
use std::sync::Arc;

#[cfg_attr(test, derive(Clone))]
enum InvertedFieldName {
  Owned(String),
  Registered(Arc<FieldInfo>),
}

/// This struct tracks the number and position / offset parameters of terms being
/// added to the index. The information collected in This struct is also used to
/// calculate the normalization factor for a field.
#[cfg_attr(test, derive(Clone))]
pub struct FieldInvertState {
  index_created_version_major: i32,
  name: InvertedFieldName,
  index_options: IndexOptions,
  pub(crate) position: i32,
  pub(crate) length: i32,
  pub(crate) num_overlap: i32,
  pub(crate) offset: i32,
  pub(crate) max_term_frequency: i32,
  pub(crate) unique_term_count: i32,
  // we must track these across field instances (multi-valued case)
  pub(crate) last_start_offset: i32,
  pub(crate) last_position: i32,
}
impl Default for FieldInvertState {
  fn default() -> Self {
    FieldInvertState {
      index_created_version_major: 6,
      name: InvertedFieldName::Owned(String::new()),
      index_options: IndexOptions::None,
      position: -1,
      length: 0,
      num_overlap: 0,
      offset: 0,
      max_term_frequency: 0,
      unique_term_count: 0,
      last_start_offset: 0,
      last_position: 0,
    }
  }
}

impl FieldInvertState {
  /// Creates {code FieldInvertState} for the specified field name.
  pub fn new<FName>(
    index_created_version_major: i32,
    name: FName,
    index_options: IndexOptions,
  ) -> Self
  where
    FName: Into<String>,
  {
    Self::with_name(
      index_created_version_major,
      InvertedFieldName::Owned(name.into()),
      index_options,
    )
  }

  pub(crate) fn from_field_info(
    index_created_version_major: i32,
    field_info: Arc<FieldInfo>,
  ) -> Self {
    let index_options = *field_info.get_index_options();
    Self::with_name(
      index_created_version_major,
      InvertedFieldName::Registered(field_info),
      index_options,
    )
  }

  fn with_name(
    index_created_version_major: i32,
    name: InvertedFieldName,
    index_options: IndexOptions,
  ) -> Self {
    FieldInvertState {
      index_created_version_major,
      name,
      index_options,
      position: 0,
      length: 0,
      num_overlap: 0,
      max_term_frequency: 0,
      unique_term_count: 0,
      offset: 0,
      last_start_offset: 0,
      last_position: 0,
    }
  }
  /// Creates {code FieldInvertState} for the specified field name and values
  /// for all fields.
  #[allow(clippy::too_many_arguments)]
  pub fn with_states<FName>(
    index_created_version_major: i32,
    name: FName,
    index_options: IndexOptions,
    position: i32,
    length: i32,
    num_overlap: i32,
    offset: i32,
    max_term_frequency: i32,
    unique_term_count: i32,
  ) -> Self
  where
    FName: Into<String>,
  {
    let name = name.into();
    let mut state = Self::new(index_created_version_major, name, index_options);
    state.position = position;
    state.length = length;
    state.num_overlap = num_overlap;
    state.offset = offset;
    state.max_term_frequency = max_term_frequency;
    state.unique_term_count = unique_term_count;
    state
  }
  /// Re-initialize the state
  pub fn reset(&mut self) {
    self.position = -1;
    self.length = 0;
    self.num_overlap = 0;
    self.offset = 0;
    self.max_term_frequency = 0;
    self.unique_term_count = 0;
    self.last_start_offset = 0;
    self.last_position = 0;
  }
  // TODO: setAttributeSource

  /// Get the last processed term position.
  pub fn position(&self) -> i32 {
    self.position
  }
  /// Get total number of terms in this field.
  pub fn get_length(&self) -> i32 {
    self.length
  }

  /// Set length value.
  pub fn set_length(&mut self, length: i32) {
    self.length = length;
  }

  /// Get the number of terms with `position_increment == 0`.
  pub fn num_overlap(&self) -> i32 {
    self.num_overlap
  }

  /// Set number of terms with `position_increment == 0`.
  pub fn set_num_overlap(&mut self, num_overlap: i32) {
    self.num_overlap = num_overlap;
  }

  /// Get end offset of the last processed term.
  pub fn get_offset(&self) -> i32 {
    self.offset
  }

  /// Get the maximum term-frequency encountered for any term in the field. A
  /// field containing "the quick brown fox jumps over the lazy dog" would
  /// have a value of 2, because "the" appears twice.
  pub fn get_max_term_frequency(&self) -> i32 {
    self.max_term_frequency
  }

  /// Return the number of unique terms encountered in this field.
  pub fn get_unique_term_count(&self) -> i32 {
    self.unique_term_count
  }

  /// Return the field's name.
  pub fn get_name(&self) -> &str {
    match &self.name {
      InvertedFieldName::Owned(name) => name,
      InvertedFieldName::Registered(field_info) => field_info.get_name(),
    }
  }

  /// Return the version that was used to create the index, or 6 if it was
  /// created before 7.0.
  pub fn get_index_created_version_major(&self) -> i32 {
    self.index_created_version_major
  }

  /// Get the index options for this field.
  pub fn get_index_options(&self) -> IndexOptions {
    self.index_options
  }
}
