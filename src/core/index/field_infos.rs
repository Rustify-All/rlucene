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
use crate::core::index::doc_values_skip_index_type::DocValuesSkipIndexType;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::util::collection_util::CollectionUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use parking_lot::Mutex;
use std::borrow::Borrow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::Relaxed;

/// Collection of FieldInfos (accessible by number or by name).
///
/// # Experimental
#[derive(Default)]
pub struct FieldInfos {
  has_freq: bool,
  has_postings: bool,
  has_prox: bool,
  has_payloads: bool,
  has_offsets: bool,
  has_term_vectors: bool,
  has_norms: bool,
  has_doc_values: bool,
  has_point_values: bool,
  has_vector_values: bool,
  soft_deletes_field: Option<Arc<FieldInfo>>,

  parent_field: Option<Arc<FieldInfo>>,
  // Dense field numbers use the already sorted values; sparse numbers retain indexed slots.
  by_number: Option<Box<[usize]>>,
  by_name: HashMap<String, Arc<FieldInfo>>,
  values: Box<[Arc<FieldInfo>]>,
  pub(crate) hook: FieldInfosHook,
}

#[derive(Default)]
pub(crate) enum FieldInfosHook {
  #[default]
  Default,
  Filter(FilterFieldInfosHook),
}

pub(crate) struct FilterFieldInfosHook {
  pub(crate) filtered_names: Arc<HashSet<String>>,
  pub(crate) filtered: Box<[Arc<FieldInfo>]>,

  // Copy of the private fields from FieldInfos
  // Renamed so as to be less confusing about which fields we're referring to
  pub(crate) filtered_has_vectors: bool,
  pub(crate) filtered_has_postings: bool,
  pub(crate) filtered_has_prox: bool,
  pub(crate) filtered_has_payloads: bool,
  pub(crate) filtered_has_offsets: bool,
  pub(crate) filtered_has_freq: bool,
  pub(crate) filtered_has_norms: bool,
  pub(crate) filtered_has_doc_values: bool,
  pub(crate) filtered_has_point_values: bool,
}

struct AvailableFieldNames<'a>(&'a HashSet<String>);

impl std::fmt::Display for AvailableFieldNames<'_> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    for (index, name) in self.0.iter().enumerate() {
      if index != 0 {
        f.write_str(", ")?;
      }
      f.write_str(name)?;
    }
    Ok(())
  }
}

struct FieldInfosDefaults;

impl FieldInfosDefaults {
  fn has_freq(in_: &FieldInfos) -> bool {
    in_.has_freq
  }

  fn has_postings(in_: &FieldInfos) -> bool {
    in_.has_postings
  }

  fn has_prox(in_: &FieldInfos) -> bool {
    in_.has_prox
  }

  fn has_payloads(in_: &FieldInfos) -> bool {
    in_.has_payloads
  }

  fn has_offsets(in_: &FieldInfos) -> bool {
    in_.has_offsets
  }

  fn has_term_vectors(in_: &FieldInfos) -> bool {
    in_.has_term_vectors
  }

  fn has_norms(in_: &FieldInfos) -> bool {
    in_.has_norms
  }

  fn has_doc_values(in_: &FieldInfos) -> bool {
    in_.has_doc_values
  }

  fn has_point_values(in_: &FieldInfos) -> bool {
    in_.has_point_values
  }

  fn size(in_: &FieldInfos) -> usize {
    in_.by_name.len()
  }

  fn values(in_: &FieldInfos) -> &[Arc<FieldInfo>] {
    &in_.values
  }

  fn field_info_by_name<'a>(
    in_: &'a FieldInfos,
    field_name: &str,
  ) -> Result<Option<&'a Arc<FieldInfo>>> {
    Ok(in_.by_name.get(field_name))
  }

  fn field_info_by_number(in_: &FieldInfos, field_number: i32) -> Result<Option<&Arc<FieldInfo>>> {
    if field_number < 0 {
      return Err(LuceneError::illegal_argument(format!(
        "Illegal field number: {field_number}"
      )));
    }
    Ok(match &in_.by_number {
      Some(by_number) => by_number
        .get(field_number as usize)
        .and_then(|&index| in_.values.get(index)),
      None => in_.values.get(field_number as usize),
    })
  }
}

impl FieldInfosHook {
  fn has_freq(&self, in_: &FieldInfos) -> bool {
    match self {
      Self::Default => FieldInfosDefaults::has_freq(in_),
      Self::Filter(hook) => hook.filtered_has_freq,
    }
  }

  fn has_postings(&self, in_: &FieldInfos) -> bool {
    match self {
      Self::Default => FieldInfosDefaults::has_postings(in_),
      Self::Filter(hook) => hook.filtered_has_postings,
    }
  }

  fn has_prox(&self, in_: &FieldInfos) -> bool {
    match self {
      Self::Default => FieldInfosDefaults::has_prox(in_),
      Self::Filter(hook) => hook.filtered_has_prox,
    }
  }

  fn has_payloads(&self, in_: &FieldInfos) -> bool {
    match self {
      Self::Default => FieldInfosDefaults::has_payloads(in_),
      Self::Filter(hook) => hook.filtered_has_payloads,
    }
  }

  fn has_offsets(&self, in_: &FieldInfos) -> bool {
    match self {
      Self::Default => FieldInfosDefaults::has_offsets(in_),
      Self::Filter(hook) => hook.filtered_has_offsets,
    }
  }

  fn has_term_vectors(&self, in_: &FieldInfos) -> bool {
    match self {
      Self::Default => FieldInfosDefaults::has_term_vectors(in_),
      Self::Filter(hook) => hook.filtered_has_vectors,
    }
  }

  fn has_norms(&self, in_: &FieldInfos) -> bool {
    match self {
      Self::Default => FieldInfosDefaults::has_norms(in_),
      Self::Filter(hook) => hook.filtered_has_norms,
    }
  }

  fn has_doc_values(&self, in_: &FieldInfos) -> bool {
    match self {
      Self::Default => FieldInfosDefaults::has_doc_values(in_),
      Self::Filter(hook) => hook.filtered_has_doc_values,
    }
  }

  fn has_point_values(&self, in_: &FieldInfos) -> bool {
    match self {
      Self::Default => FieldInfosDefaults::has_point_values(in_),
      Self::Filter(hook) => hook.filtered_has_point_values,
    }
  }

  fn size(&self, in_: &FieldInfos) -> usize {
    match self {
      Self::Default => FieldInfosDefaults::size(in_),
      Self::Filter(hook) => hook.filtered.len(),
    }
  }

  fn values<'a>(&'a self, in_: &'a FieldInfos) -> &'a [Arc<FieldInfo>] {
    match self {
      Self::Default => FieldInfosDefaults::values(in_),
      Self::Filter(hook) => &hook.filtered,
    }
  }

  fn field_info_by_name<'a>(
    &'a self,
    in_: &'a FieldInfos,
    field_name: &str,
  ) -> Result<Option<&'a Arc<FieldInfo>>> {
    match self {
      Self::Default => FieldInfosDefaults::field_info_by_name(in_, field_name),
      Self::Filter(hook) => {
        if !hook.filtered_names.contains(field_name) {
          // Return `IllegalArgument` to match `field_info_by_number` for invalid numbers.
          let available_fields = AvailableFieldNames(&hook.filtered_names);
          return Err(LuceneError::illegal_argument(format!(
            "The field named '{field_name}' is not accessible in the current merge context, available ones are: [{available_fields}]"
          )));
        }
        FieldInfosDefaults::field_info_by_name(in_, field_name)
      },
    }
  }

  fn field_info_by_number<'a>(
    &'a self,
    in_: &'a FieldInfos,
    field_number: i32,
  ) -> Result<Option<&'a Arc<FieldInfo>>> {
    match self {
      Self::Default => FieldInfosDefaults::field_info_by_number(in_, field_number),
      Self::Filter(hook) => {
        let field_info = FieldInfosDefaults::field_info_by_number(in_, field_number)?;
        if let Some(field_info) = &field_info
          && !hook.filtered_names.contains(&field_info.name)
        {
          let available_fields = AvailableFieldNames(&hook.filtered_names);
          return Err(LuceneError::illegal_argument(format!(
            "The field named '{}' numbered '{field_number}' is not accessible in the current merge context, available ones are: [{available_fields}]",
            field_info.name
          )));
        }
        Ok(field_info)
      },
    }
  }
}

impl FieldInfos {
  /// Constructs a new FieldInfos from an array of FieldInfo objects. The
  /// array can be used directly as the backing structure.
  pub fn new(mut infos: Vec<Arc<FieldInfo>>) -> Result<Self> {
    let mut has_term_vectors = false;
    let mut has_postings = false;
    let mut has_prox = false;
    let mut has_payloads = false;
    let mut has_offsets = false;
    let mut has_freq = false;
    let mut has_norms = false;
    let mut has_doc_values = false;
    let mut has_point_values = false;
    let mut has_vector_values = false;
    let mut soft_deletes_field: Option<Arc<FieldInfo>> = None;
    let mut parent_field: Option<Arc<FieldInfo>> = None;

    let mut by_name = CollectionUtil::new_hashmap(infos.len());
    let mut max_field_number = -1;
    let mut field_number_strictly_ascending = true;

    for info in &infos {
      let field_number = info.number;
      if field_number < 0 {
        return Err(LuceneError::illegal_argument(format!(
          "illegal field number: {} for field {}",
          info.number, info.name
        )));
      }
      if field_number > max_field_number {
        max_field_number = field_number;
      } else {
        field_number_strictly_ascending = false;
      }
      if let Some(previous) = by_name.insert(info.name.clone(), info.clone()) {
        return Err(LuceneError::illegal_argument(format!(
          "duplicate field names: {} and {} have: {}",
          previous.number, info.number, info.name
        )));
      }

      has_term_vectors |= info.has_term_vectors();
      has_postings |= info.get_index_options() != &IndexOptions::None;
      has_prox |= info.get_index_options() >= &IndexOptions::DocsAndFreqsAndPositions;
      has_freq |= info.get_index_options() != &IndexOptions::Docs;
      has_offsets |= info.get_index_options() >= &IndexOptions::DocsAndFreqsAndPositionsAndOffsets;
      has_norms |= info.has_norms();
      has_doc_values |= info.get_doc_values_type() != &DocValuesType::None;
      has_payloads |= info.has_payloads();
      has_point_values |= info.get_point_dimension_count() != 0;
      has_vector_values |= info.get_vector_dimension() != 0;
      if info.is_soft_deletes_field() {
        if let Some(ref s) = soft_deletes_field {
          if s.name != info.name {
            return Err(LuceneError::illegal_argument(format!(
              "multiple soft-deletes fields [{} , {}]",
              info.name, s.name
            )));
          }
        } else {
          soft_deletes_field = Some(info.clone());
        }
      }
      if info.is_parent_field() {
        if let Some(ref p) = parent_field {
          if p.name != info.name {
            return Err(LuceneError::illegal_argument(format!(
              "multiple parent fields [{} , {}]",
              info.name, p.name
            )));
          }
        } else {
          parent_field = Some(info.clone());
        }
      }
    }

    let by_number_len = max_field_number.checked_add(1).ok_or_else(|| {
      LuceneError::illegal_argument(format!("field number is too large: {max_field_number}"))
    })? as usize;
    let by_number;
    let values: Vec<Arc<FieldInfo>>;
    if infos.len() <= 2 || (field_number_strictly_ascending && by_number_len == infos.len()) {
      if let [first, second] = infos.as_slice() {
        if first.number == second.number {
          return Err(LuceneError::illegal_argument(format!(
            "duplicate field numbers: {} and {} have: {}",
            first.name, second.name, second.number
          )));
        }
        if first.number > second.number {
          infos.swap(0, 1);
        }
      }
      // Small collections and already sorted dense field numbers need no lookup table.
      by_number = None;
      infos.shrink_to_fit();
      values = infos;
    } else {
      let mut indexed_slots: smallvec::SmallVec<[usize; 8]> =
        smallvec::smallvec![usize::MAX; by_number_len];
      for (index, field_info) in infos.iter().enumerate() {
        let slot = &mut indexed_slots[field_info.number as usize];
        if let Some(existing) = infos.get(*slot) {
          return Err(LuceneError::illegal_argument(format!(
            "duplicate field numbers: {} and {} have: {}",
            existing.name, field_info.name, field_info.number
          )));
        }
        *slot = index;
      }
      if by_number_len == infos.len() {
        // Unique field numbers now cover 0..infos.len(), so retain these owners
        // while moving each field into its numbered slot.
        for index in 0..infos.len() {
          while infos[index].number as usize != index {
            let destination = infos[index].number as usize;
            infos.swap(index, destination);
          }
        }
        values = infos;
        by_number = None;
      } else {
        if !field_number_strictly_ascending {
          infos.sort_unstable_by_key(|fi| fi.number);
        }
        // Occupied slots appear in the same field-number order as the sorted values.
        let mut indices = 0..infos.len();
        let to_index =
          |slot: Option<Arc<FieldInfo>>| slot.and_then(|_| indices.next()).unwrap_or(usize::MAX);
        infos.shrink_to_fit();
        values = infos;
        by_number = Some(if indexed_slots.spilled() {
          indexed_slots.into_vec().into_iter().map(to_index).collect()
        } else {
          let mut slots = Vec::with_capacity(by_number_len);
          slots.extend(indexed_slots.into_iter().map(to_index));
          slots
        });
      }
    }

    Ok(FieldInfos {
      has_freq,
      has_postings,
      has_prox,
      has_payloads,
      has_offsets,
      has_term_vectors,
      has_norms,
      has_doc_values,
      has_point_values,
      has_vector_values,
      soft_deletes_field,
      parent_field,
      by_number: by_number.map(Vec::into_boxed_slice),
      by_name,
      values: values.into_boxed_slice(),
      hook: FieldInfosHook::Default,
    })
  }

  /// Returns true if any fields have freqs.
  pub fn has_freq(&self) -> bool {
    self.hook.has_freq(self)
  }

  /// Returns true if any fields have postings.
  pub fn has_postings(&self) -> bool {
    self.hook.has_postings(self)
  }

  /// Returns true if any fields have positions.
  pub fn has_prox(&self) -> bool {
    self.hook.has_prox(self)
  }

  /// Returns true if any fields have payloads.
  pub fn has_payloads(&self) -> bool {
    self.hook.has_payloads(self)
  }

  /// Returns true if any fields have offsets.
  pub fn has_offsets(&self) -> bool {
    self.hook.has_offsets(self)
  }

  /// Returns true if any fields have term vectors.
  pub fn has_term_vectors(&self) -> bool {
    self.hook.has_term_vectors(self)
  }

  /// Returns true if any fields have norms.
  pub fn has_norms(&self) -> bool {
    self.hook.has_norms(self)
  }

  /// Returns true if any fields have DocValues.
  pub fn has_doc_values(&self) -> bool {
    self.hook.has_doc_values(self)
  }

  /// Returns true if any fields have PointValues.
  pub fn has_point_values(&self) -> bool {
    self.hook.has_point_values(self)
  }

  /// Returns true if any fields have vector values.
  pub fn has_vector_values(&self) -> bool {
    self.has_vector_values
  }

  /// Returns the soft-deletes field name if it exists; otherwise returns
  /// None.
  pub fn get_soft_deletes_field(&self) -> Option<&String> {
    self.soft_deletes_field.as_ref().map(|info| &info.name)
  }

  /// Returns the parent document field name if it exists; otherwise returns
  /// None.
  pub fn get_parent_field(&self) -> Option<&String> {
    self.parent_field.as_ref().map(|info| &info.name)
  }

  /// Returns the number of fields.
  pub fn size(&self) -> usize {
    self.hook.size(self)
  }

  /// Returns an iterator over all the FieldInfo objects present, ordered by
  /// ascending field number.
  pub fn iter(&self) -> std::slice::Iter<'_, Arc<FieldInfo>> {
    self.hook.values(self).iter()
  }

  /// Return the FieldInfo object referenced by the field name.
  ///
  /// Returns None if the given field name doesn't exist.
  pub fn field_info_by_name(&self, field_name: &str) -> Result<Option<&Arc<FieldInfo>>> {
    self.hook.field_info_by_name(self, field_name)
  }

  /// Return the FieldInfo object referenced by the field number.
  ///
  /// Returns None if the given field number doesn't exist.
  pub fn field_info_by_number(&self, field_number: i32) -> Result<Option<&Arc<FieldInfo>>> {
    self.hook.field_info_by_number(self, field_number)
  }
}
pub(crate) static EMPTY: LazyLock<Arc<FieldInfos>> = LazyLock::new(|| {
  Arc::new(expect_invariant!(
    FieldInfos::new(vec![]),
    "an empty built-in field-info collection is valid"
  ))
});

pub fn get_merged_field_infos<IR>(reader: IR) -> Result<Arc<FieldInfos>>
where
  IR: IndexReader,
{
  let crc = reader.get_context()?;
  let leaves = crc.leaves()?;

  if leaves.is_empty() {
    return Ok(EMPTY.clone());
  }

  if leaves.len() == 1 {
    return leaves[0].reader().get_field_infos();
  }

  let mut soft_deletes_field: Option<String> = None;
  for l in leaves.iter() {
    if let Some(v) = l.reader().get_field_infos()?.get_soft_deletes_field() {
      soft_deletes_field = Some(v.clone());
      break;
    }
  }

  let parent_field = get_and_validate_parent_field(leaves)?;

  let mut builder = Builder::new(Arc::new(Mutex::new(FieldNumbers::new(
    soft_deletes_field,
    parent_field,
  )?)));

  for leaf in leaves {
    for field_info in leaf.reader().get_field_infos()?.iter() {
      builder.add(field_info.as_ref())?;
    }
  }

  Ok(Arc::new(builder.finish_owned()?))
}

pub(crate) fn get_and_validate_parent_field<LR>(
  leaves: &[LeafReaderContext<LR>],
) -> Result<Option<String>>
where
  LR: LeafReader,
{
  let mut the_field: Option<String> = None;
  let mut set = false;

  for ctx in leaves {
    let field_infos = ctx.reader().get_field_infos()?;
    let field = field_infos.get_parent_field();

    if !set {
      the_field = field.cloned();
      set = true;
    } else if field != the_field.as_ref() {
      return Err(LuceneError::illegal_state(format!(
        "expected parent doc field to be \"{:?}\" across all segments \
                 but found a segment with different field \"{:?}\"",
        the_field, field
      )));
    }
  }

  Ok(the_field)
}
/// Returns a set of field names that have a terms index.
/// The order is undefined.
pub fn get_indexed_fields<IR>(reader: IR) -> Result<HashSet<String>>
where
  IR: IndexReader,
{
  let reader = reader.get_context()?;
  let leaves = reader.leaves()?;

  let mut fields = HashSet::with_capacity(leaves.len());

  for leaf in leaves {
    let field_infos = leaf.reader().get_field_infos()?;
    for fi in field_infos.iter() {
      if *fi.get_index_options() != IndexOptions::None && !fields.contains(fi.name.as_str()) {
        fields.insert(fi.name.clone());
      }
    }
  }

  Ok(fields)
}

impl<'a> IntoIterator for &'a FieldInfos {
  type Item = &'a Arc<FieldInfo>;
  type IntoIter = std::slice::Iter<'a, Arc<FieldInfo>>;

  fn into_iter(self) -> Self::IntoIter {
    self.iter()
  }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FieldDimensions {
  pub dimension_count: usize,
  pub index_dimension_count: usize,
  pub dimension_num_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FieldVectorProperties {
  pub num_dimensions: i32,
  pub vector_encoding: VectorEncoding,
  pub similarity_function: VectorSimilarityFunction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IndexOptionsProperties {
  pub store_term_vectors: bool,
  pub omit_norms: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FieldProperties {
  pub number: i32,
  pub index_options: IndexOptions,
  pub index_options_properties: Option<IndexOptionsProperties>,
  pub doc_values_type: DocValuesType,
  pub doc_values_skip_index: DocValuesSkipIndexType,
  pub field_dimensions: FieldDimensions,
  pub field_vector_properties: FieldVectorProperties,
}

pub(crate) type FieldNumbersLock = Arc<Mutex<FieldNumbers>>;
pub(crate) struct FieldNumbers {
  number_to_name: HashSet<i32>,
  field_properties: HashMap<String, FieldProperties>,
  lowest_unassigned_field_number: i32,
  soft_deletes_field_name: Option<String>,
  // The parent document field from IWC to mark parent document when indexing
  parent_field_name: Option<String>,
  // The soft-deletes field from IWC to enforce a single soft-deletes field
}

impl FieldNumbers {
  pub(crate) fn new<S, P>(
    soft_deletes_field_name: Option<S>,
    parent_field_name: Option<P>,
  ) -> Result<Self>
  where
    S: Into<String>,
    P: Into<String>,
  {
    let soft_deletes_field_name = soft_deletes_field_name.map(Into::into);
    let parent_field_name = parent_field_name.map(Into::into);
    if let (Some(soft), Some(parent)) = (&soft_deletes_field_name, &parent_field_name)
      && soft == parent
    {
      return Err(LuceneError::illegal_argument(format!(
        "parent document and soft-deletes field can't be the same field \"{parent}\""
      )));
    }

    Ok(FieldNumbers {
      number_to_name: HashSet::new(),
      field_properties: HashMap::new(),
      lowest_unassigned_field_number: -1,
      soft_deletes_field_name,
      parent_field_name,
    })
  }
  pub(crate) fn verify_field_info(&self, fi: &FieldInfo) -> Result<()> {
    let field_name = fi.get_name();
    self.verify_soft_deleted_field_name(field_name, fi.is_soft_deletes_field())?;
    self.verify_parent_field_name(field_name, fi.is_parent_field())?;
    if self.field_properties.contains_key(field_name) {
      self.verify_same_schema(fi)?;
    }
    Ok(())
  }

  /// Returns the global field number for the given field name. If the name
  /// does not exist yet it tries to add it with the given preferred field
  /// number assigned if possible otherwise the first unassigned field
  /// number is used as the field number.
  pub(crate) fn add_or_get(&mut self, fi: &FieldInfo) -> Result<i32> {
    let field_name = fi.get_name();
    self.verify_soft_deleted_field_name(field_name, fi.is_soft_deletes_field())?;
    self.verify_parent_field_name(field_name, fi.is_parent_field())?;
    let number = match self.field_properties.get(field_name) {
      Some(field_properties) => {
        self.verify_same_schema(fi)?;
        field_properties.number
      },
      None => {
        // first time we see this field in this index
        let field_number = if fi.number != -1 && !self.number_to_name.contains(&fi.number) {
          // cool - we can use this number globally
          fi.number
        } else {
          // find a new FieldNumber
          loop {
            self.lowest_unassigned_field_number += 1;
            if !self
              .number_to_name
              .contains(&self.lowest_unassigned_field_number)
            {
              break;
            }
            // might not be up to date - lets do the work once needed
          }
          self.lowest_unassigned_field_number
        };
        debug_assert!(field_number >= 0);
        self.number_to_name.insert(field_number);
        let index_options_props = if fi.get_index_options() != &IndexOptions::None {
          Some(IndexOptionsProperties {
            store_term_vectors: fi.has_term_vectors(),
            omit_norms: fi.omits_norms(),
          })
        } else {
          None
        };
        let field_properties = FieldProperties {
          number: field_number,
          index_options: *fi.get_index_options(),
          index_options_properties: index_options_props,
          doc_values_type: *fi.get_doc_values_type(),
          doc_values_skip_index: *fi.doc_values_skip_index_type(),
          field_dimensions: FieldDimensions {
            dimension_count: fi.get_point_dimension_count(),
            index_dimension_count: fi.get_point_index_dimension_count(),
            dimension_num_bytes: fi.get_point_num_bytes(),
          },
          field_vector_properties: FieldVectorProperties {
            num_dimensions: fi.get_vector_dimension(),
            vector_encoding: *fi.get_vector_encoding(),
            similarity_function: *fi.get_vector_similarity_function(),
          },
        };
        let number = field_properties.number;
        self
          .field_properties
          .insert(field_name.to_string(), field_properties);
        number
      },
    };
    Ok(number)
  }

  fn verify_soft_deleted_field_name(
    &self,
    field_name: &str,
    is_soft_deletes_field: bool,
  ) -> Result<()> {
    if is_soft_deletes_field {
      match self.soft_deletes_field_name.as_ref() {
        None => {
          return Err(LuceneError::illegal_argument(format!(
            "this index has [{field_name}] as soft-deletes already but soft-deletes field is not configured in IWC"
          )));
        },
        Some(existing) if existing != field_name => {
          return Err(LuceneError::illegal_argument(format!(
            "cannot configure [{}] as soft-deletes; this index uses [{}] as soft-deletes already",
            existing, field_name
          )));
        },
        _ => {},
      }
    } else if let Some(ref soft_name) = self.soft_deletes_field_name
      && soft_name == field_name
    {
      return Err(LuceneError::illegal_argument(format!(
        "cannot configure [{soft_name}] as soft-deletes; this index uses [{field_name}] as non-soft-deletes already"
      )));
    }
    Ok(())
  }

  fn verify_parent_field_name(&self, field_name: &str, is_parent_field: bool) -> Result<()> {
    if is_parent_field {
      match self.parent_field_name.as_ref() {
        None => {
          return Err(LuceneError::illegal_argument(format!(
            "can't add field [{field_name}] as parent document field; this IndexWriter has no parent document field configured"
          )));
        },
        Some(existing) if existing != field_name => {
          return Err(LuceneError::illegal_argument(format!(
            "can't add field [{}] as parent document field; this IndexWriter is configured with [{}] as parent document field",
            field_name, existing
          )));
        },
        _ => {},
      }
    } else if let Some(ref parent) = self.parent_field_name {
      // this would be the case if the current index has a parent field
      // that is not a parent field in the incoming index
      // (think addIndices)
      if parent == field_name {
        return Err(LuceneError::illegal_argument(format!(
          "can't add [{field_name}] as non parent document field; this IndexWriter is configured with [{parent}] as parent document field"
        )));
      }
    }
    Ok(())
  }

  fn verify_same_schema(&self, fi: &FieldInfo) -> Result<()> {
    let field_name = fi.get_name();
    let field_properties = self.field_properties.get(field_name).ok_or_else(|| {
      LuceneError::illegal_state(format!("field properties are missing for [{field_name}]"))
    })?;
    FieldInfo::verify_same_index_options(
      field_name,
      &field_properties.index_options,
      fi.get_index_options(),
    )?;
    if field_properties.index_options != IndexOptions::None {
      let index_options_properties = field_properties
        .index_options_properties
        .as_ref()
        .ok_or_else(|| {
          LuceneError::illegal_state(format!(
            "index option properties are missing for indexed field [{field_name}]"
          ))
        })?;
      let current_term_vector = index_options_properties.store_term_vectors;
      FieldInfo::verify_same_store_term_vectors(
        field_name,
        current_term_vector,
        fi.has_term_vectors(),
      )?;
      let current_omit_norms = index_options_properties.omit_norms;
      FieldInfo::verify_same_omit_norms(field_name, current_omit_norms, fi.omits_norms())?;
    }
    FieldInfo::verify_same_doc_values_type(
      field_name,
      &field_properties.doc_values_type,
      fi.get_doc_values_type(),
    )?;
    FieldInfo::verify_same_doc_values_skip_index(
      field_name,
      &field_properties.doc_values_skip_index,
      fi.doc_values_skip_index_type(),
    )?;
    let dims = &field_properties.field_dimensions;
    FieldInfo::verify_same_points_options(
      field_name,
      dims.dimension_count,
      dims.index_dimension_count,
      dims.dimension_num_bytes,
      fi.get_point_dimension_count(),
      fi.get_point_index_dimension_count(),
      fi.get_point_num_bytes(),
    )?;
    let vec_props = &field_properties.field_vector_properties;
    FieldInfo::verify_same_vector_options(
      field_name,
      vec_props.num_dimensions,
      &vec_props.vector_encoding,
      &vec_props.similarity_function,
      fi.get_vector_dimension(),
      fi.get_vector_encoding(),
      fi.get_vector_similarity_function(),
    )?;
    Ok(())
  }
  /// This function is called from [`IndexWriter`](crate::core::index::index_writer::IndexWriter) to verify if doc values of
  /// the field can be updated. If a field with this name already exists,
  /// it verifies that it is a doc-values-only field. If the field does
  /// not exist and `field_must_exist` is `false`, a new field is created in
  /// the global field numbers.
  ///
  /// # Parameters
  /// - `field_name`: Name of the field.
  /// - `dv_type`: Expected doc values type.
  /// - `field_must_exist`: Whether the field must already exist.
  ///
  /// # Errors
  /// - Returns an error if the field must exist but does not.
  /// - Returns an error if the field exists but is not a doc-values-only
  ///   field with the provided doc values type.
  pub fn verify_or_create_dv_only_field(
    &mut self,
    field_name: &str,
    dv_type: &DocValuesType,
    field_must_exist: bool,
  ) -> Result<()> {
    if !self.field_properties.contains_key(field_name) {
      if field_must_exist {
        return Err(LuceneError::illegal_argument(format!(
          "Can't update [{dv_type:?}] doc values; the field [{field_name}] doesn't exist."
        )));
      } else {
        // create dv only field
        let fi = FieldInfo::new(
          field_name.to_string(),
          -1,
          false,
          false,
          false,
          IndexOptions::None,
          *dv_type,
          DocValuesSkipIndexType::None,
          -1,
          HashMap::new(),
          0,
          0,
          0,
          0,
          VectorEncoding::FLOAT32(4),
          VectorSimilarityFunction::Euclidean,
          self
            .soft_deletes_field_name
            .as_ref()
            .is_some_and(|s| s == field_name),
          self
            .parent_field_name
            .as_ref()
            .is_some_and(|s| s == field_name),
        )?;
        self.add_or_get(&fi)?;
      }
    } else {
      // verify that field is doc values only field with the give doc
      // values type
      let field_props = self.field_properties.get(field_name).ok_or_else(|| {
        LuceneError::illegal_state(format!("field properties are missing for [{field_name}]"))
      })?;
      if *dv_type != field_props.doc_values_type {
        return Err(LuceneError::illegal_argument(format!(
          "Can't update [{:?}] doc values; the field [{}] has inconsistent doc values' type of [{:?}].",
          dv_type, field_name, field_props.doc_values_type
        )));
      }
      if field_props.doc_values_skip_index != DocValuesSkipIndexType::None {
        return Err(LuceneError::illegal_argument(format!(
          "Can't update [{dv_type:?}] doc values; the field [{field_name}] must be doc values only field, but it has doc values skip index"
        )));
      }
      if field_props.field_dimensions.dimension_count != 0 {
        return Err(LuceneError::illegal_argument(format!(
          "Can't update [{dv_type:?}] doc values; the field [{field_name}] must be doc values only field, but is also indexed with points."
        )));
      }
      if field_props.index_options != IndexOptions::None {
        return Err(LuceneError::illegal_argument(format!(
          "Can't update [{dv_type:?}] doc values; the field [{field_name}] must be doc values only field, but is also indexed with postings."
        )));
      }
      if field_props.field_vector_properties.num_dimensions != 0 {
        return Err(LuceneError::illegal_argument(format!(
          "Can't update [{dv_type:?}] doc values; the field [{field_name}] must be doc values only field, but is also indexed with vectors."
        )));
      }
    }
    Ok(())
  }

  /// Constructs a new [`FieldInfo`](crate::core::index::field_info::FieldInfo) based on the options in global field
  /// numbers. This method needs no lock because all options it uses are immutable.
  ///
  /// # Parameters
  /// - `field_name`: Name of the field.
  /// - `dv_type`: Doc values type.
  /// - `new_field_number`: A new field number.
  ///
  /// # Returns
  /// - `None` if `field_name` does not exist in the map or is not of the same
  ///   `dv_type`.
  /// - Otherwise, returns a new [`FieldInfo`](crate::core::index::field_info::FieldInfo) based on the options in global
  ///   field numbers.
  pub fn construct_field_info(
    &self,
    field_name: &str,
    dv_type: DocValuesType,
    new_field_number: i32,
  ) -> Result<Option<FieldInfo>> {
    let field_props = self.field_properties.get(field_name);
    if let Some(fp) = field_props {
      if dv_type != fp.doc_values_type {
        return Ok(None);
      }
      let is_soft_deletes_field = self
        .soft_deletes_field_name
        .as_ref()
        .is_some_and(|s| s == field_name);
      let is_parent_field = self
        .parent_field_name
        .as_ref()
        .is_some_and(|s| s == field_name);
      Ok(Some(FieldInfo::new(
        field_name.to_string(),
        new_field_number,
        false,
        false,
        false,
        IndexOptions::None,
        dv_type,
        DocValuesSkipIndexType::None,
        -1,
        HashMap::new(),
        0,
        0,
        0,
        0,
        VectorEncoding::FLOAT32(4),
        VectorSimilarityFunction::Euclidean,
        is_soft_deletes_field,
        is_parent_field,
      )?))
    } else {
      Ok(None)
    }
  }

  pub fn get_field_names(&self) -> impl ExactSizeIterator<Item = &str> {
    self.field_properties.keys().map(String::as_str)
  }

  pub fn clear(&mut self) {
    self.number_to_name.clear();
    self.field_properties.clear();
    self.lowest_unassigned_field_number = -1;
  }
}
pub struct Builder {
  by_name: HashMap<String, Arc<FieldInfo>>,
  global_field_numbers: FieldNumbersLock,
  finished: AtomicBool,
}
impl Builder {
  pub(crate) fn new(global_field_numbers: FieldNumbersLock) -> Self {
    Self {
      by_name: HashMap::new(),
      global_field_numbers,
      finished: AtomicBool::new(false),
    }
  }

  pub fn is_soft_deletes_field_name(&self, field_name: &str) -> bool {
    match self
      .global_field_numbers
      .lock()
      .soft_deletes_field_name
      .as_ref()
    {
      Some(name) => *field_name == *name,
      None => false,
    }
  }

  pub fn is_parent_field_name(&self, field_name: &str) -> bool {
    match self.global_field_numbers.lock().parent_field_name {
      Some(ref name) => *field_name == *name,
      _ => false,
    }
  }

  pub fn add<F>(&mut self, fi: F) -> Result<Arc<FieldInfo>>
  where
    F: Borrow<FieldInfo>,
  {
    self.add_with_dv_gen(fi, -1)
  }

  pub fn add_with_dv_gen<F>(&mut self, fi: F, dv_gen: i64) -> Result<Arc<FieldInfo>>
  where
    F: Borrow<FieldInfo>,
  {
    let fi = fi.borrow();
    if let Some(cur_fi) = self.add_existing(fi)? {
      return Ok(cur_fi);
    }

    self.assert_not_finished()?;

    let field_number = self.global_field_numbers.lock().add_or_get(fi)?;
    let attributes = fi.attributes();
    let fi_new = Arc::new(FieldInfo::new(
      fi.name.clone(),
      field_number,
      fi.has_term_vectors(),
      fi.omits_norms(),
      fi.has_payloads(),
      // copy
      *fi.get_index_options(),
      *fi.get_doc_values_type(),
      *fi.doc_values_skip_index_type(),
      dv_gen,
      attributes,
      fi.get_point_dimension_count(),
      fi.get_point_index_dimension_count(),
      fi.get_point_num_bytes(),
      fi.get_vector_dimension(),
      *fi.get_vector_encoding(),
      *fi.get_vector_similarity_function(),
      fi.is_soft_deletes_field(),
      fi.is_parent_field(),
    )?);
    self.by_name.insert(fi_new.name.clone(), fi_new.clone());
    Ok(fi_new)
  }

  pub(crate) fn add_owned(&mut self, mut fi: FieldInfo) -> Result<Arc<FieldInfo>> {
    if let Some(cur_fi) = self.add_existing(&fi)? {
      return Ok(cur_fi);
    }
    self.assert_not_finished()?;
    let field_number = self.global_field_numbers.lock().add_or_get(&fi)?;
    fi.number = field_number;
    fi.set_doc_values_gen(-1)?;
    let fi = Arc::new(fi);
    self.by_name.insert(fi.name.clone(), fi.clone());
    Ok(fi)
  }

  fn add_existing(&self, fi: &FieldInfo) -> Result<Option<Arc<FieldInfo>>> {
    if let Some(cur_fi) = self.by_name.get(&fi.name) {
      cur_fi.verify_same_schema(fi)?;

      let attributes = fi.attributes();
      let unchanged = attributes.is_empty() || {
        let current = cur_fi.attributes();
        Arc::ptr_eq(&attributes, &current)
          || attributes.iter().all(|(k, v)| current.get(k) == Some(v))
      };
      if !unchanged {
        for (k, v) in attributes.iter() {
          cur_fi.put_borrowed_attribute(k, v);
        }
      }
      if fi.has_payloads() {
        cur_fi.set_store_payloads()?;
      }
      return Ok(Some(cur_fi.clone()));
    }
    Ok(None)
  }
  pub fn field_info(&self, field_name: &str) -> Option<Arc<FieldInfo>> {
    self.by_name.get(field_name).cloned()
  }
  fn assert_not_finished(&self) -> Result<()> {
    if self.finished.load(Relaxed) {
      return Err(LuceneError::illegal_state(
        "FieldInfos.Builder was already finished; cannot add new fields",
      ));
    }
    Ok(())
  }
  pub fn finish(&self) -> Result<FieldInfos> {
    self.finished.store(true, Relaxed);
    FieldInfos::new(self.by_name.values().cloned().collect())
  }

  pub(crate) fn finish_owned(self) -> Result<FieldInfos> {
    FieldInfos::new(self.by_name.into_values().collect())
  }
}
