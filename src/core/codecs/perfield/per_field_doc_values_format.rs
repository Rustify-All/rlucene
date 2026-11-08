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

use crate::core::codecs::doc_values_consumer::DocValuesConsumer;
use crate::core::codecs::doc_values_format::DocValuesFormat;
use crate::core::codecs::doc_values_producer::DocValuesProducer;
use crate::core::codecs::perfield::per_field_merge_state::PerFieldMergeState;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::index_reader::Identity;
use crate::core::index::merge_state::MergeStateAccess;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::store::directory::Directory;
use crate::core::store::{IndexInput, IndexOutput};
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::{HasIdentity, IOUtils};
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

/// Name of this [`DocValuesFormat`].
pub const PER_FIELD_NAME: &str = "PerFieldDV40";

/// [`FieldInfo`] attribute name used to store the format name for each field.
pub const PER_FIELD_FORMAT_KEY: &str = "PerFieldDocValuesFormat.format";

/// [`FieldInfo`] attribute name used to store the segment suffix name for each field.
pub const PER_FIELD_SUFFIX_KEY: &str = "PerFieldDocValuesFormat.suffix";

/// Static-dispatch access to the format selection needed by
/// [`PerFieldDocValuesFormat`].
pub trait PerFieldDocValuesFormatBase {
  type Format: DocValuesFormat;

  /// Returns the doc values format that should be used for writing new
  /// segments of `field`.
  ///
  /// The field-to-format mapping is written to the index, so this method is
  /// only invoked when writing, not when reading.
  fn get_doc_values_format_for_field(&self, field: &str) -> Result<&Self::Format>;
}

/// Enables per-field doc values support.
///
/// The selected doc values format's name is written into the index. In order
/// for a field to be read, that name must resolve to the same implementation
/// through [`DocValuesFormat::for_name`].
///
/// Files written by each doc values format have an additional suffix
/// containing the format name. For example, in a per-field configuration, a
/// file named `_1.dat` would instead look like `_1_Lucene40_0.dat`.
///
/// # Experimental
pub struct PerFieldDocValuesFormat<B> {
  base: Arc<B>,
  identity: Identity,
}

impl<B> Clone for PerFieldDocValuesFormat<B> {
  fn clone(&self) -> Self {
    Self {
      base: Arc::clone(&self.base),
      identity: self.identity.clone(),
    }
  }
}

impl<B> PerFieldDocValuesFormat<B> {
  /// Sole constructor.
  pub fn new(base: B) -> Self {
    Self {
      base: Arc::new(base),
      identity: Identity::new(),
    }
  }
}

impl<B> HasIdentity for PerFieldDocValuesFormat<B> {
  fn identity(&self) -> &Identity {
    &self.identity
  }
}

impl<B> Display for PerFieldDocValuesFormat<B> {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "DocValuesFormat(name={PER_FIELD_NAME})")
  }
}

struct ConsumerAndSuffix<DVC> {
  consumer: DVC,
  suffix: i32,
}

impl<DVC> Closeable for ConsumerAndSuffix<DVC>
where
  DVC: Closeable,
{
  fn close(&mut self) -> Result<()> {
    self.consumer.close()
  }
}

pub struct FieldsWriter<B, DVC> {
  base: Arc<B>,
  formats: HashMap<Identity, ConsumerAndSuffix<DVC>>,
  suffixes: HashMap<String, i32>,
}

impl<B, DVC> FieldsWriter<B, DVC> {
  fn new(base: Arc<B>) -> Self {
    Self {
      base,
      formats: HashMap::new(),
      suffixes: HashMap::new(),
    }
  }
}

impl<B, DVC> FieldsWriter<B, DVC>
where
  B: PerFieldDocValuesFormatBase,
  DVC: DocValuesConsumer,
{
  fn get_instance<D1, D2>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    field: &Arc<FieldInfo>,
  ) -> Result<(Identity, String, &mut DVC)>
  where
    D1: Directory<IndexOutput = DVC::IndexOutput>,
    B::Format: DocValuesFormat<DocValuesConsumer<DVC::IndexOutput> = DVC>,
  {
    self.get_instance_with_ignore_current_format(write_state, segment_info, field, false)
  }

  /// Doc values consumer for the given field.
  fn get_instance_with_ignore_current_format<D1, D2>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    field: &Arc<FieldInfo>,
    ignore_current_format: bool,
  ) -> Result<(Identity, String, &mut DVC)>
  where
    D1: Directory<IndexOutput = DVC::IndexOutput>,
    B::Format: DocValuesFormat<DocValuesConsumer<DVC::IndexOutput> = DVC>,
  {
    let base = &self.base;
    let mut loaded_format = None;
    if field.get_doc_values_gen() != -1 && !ignore_current_format {
      let attributes = field.attributes();
      // This can be a new field receiving its first update in the segment.
      if let Some(format_name) = attributes.get(PER_FIELD_FORMAT_KEY) {
        loaded_format = Some(B::Format::for_name(format_name)?);
      }
    }
    let format = match loaded_format.as_ref() {
      Some(format) => format.as_ref(),
      None => base.get_doc_values_format_for_field(&field.name)?,
    };
    let format_name = format.get_name();
    let identity = format.identity().clone();

    field.put_borrowed_attribute(PER_FIELD_FORMAT_KEY, format_name);
    let mut suffix = None;

    let segment_suffix = match self.formats.get(&identity) {
      None => {
        // First time we are seeing this format; create a new instance.

        if field.get_doc_values_gen() != -1 && !ignore_current_format {
          let attributes = field.attributes();
          let suffix_attribute = attributes.get(PER_FIELD_SUFFIX_KEY);
          // Even when dvGen is != -1, it can still be a new field that never
          // existed in the segment and therefore doesn't have the recorded
          // attributes yet.
          if let Some(suffix_attribute) = suffix_attribute {
            suffix = Some(suffix_attribute.parse::<i32>().map_err(|_| {
              LuceneError::number_format(format!(
                "invalid attribute: {PER_FIELD_SUFFIX_KEY}={suffix_attribute} for field: {}",
                field.name
              ))
            })?);
          }
        }

        if suffix.is_none() {
          // Bump the suffix.
          suffix = Some(match self.suffixes.get_mut(format_name) {
            Some(suffix) => {
              *suffix += 1;
              *suffix
            },
            None => {
              self.suffixes.insert(format_name.to_string(), 0);
              0
            },
          });
        }
        let suffix = suffix.ok_or_else(|| {
          LuceneError::illegal_state(format!("missing suffix for field: {}", field.name))
        })?;
        match self.suffixes.get_mut(format_name) {
          Some(previous_suffix) => *previous_suffix = suffix,
          None => {
            self.suffixes.insert(format_name.to_string(), suffix);
          },
        }

        let segment_suffix =
          get_full_segment_suffix(&write_state.segment_suffix, format_name, suffix);
        let state = SegmentWriteState::copy_with_suffix(write_state, segment_suffix);
        let consumer = format.fields_consumer(&state, segment_info)?;
        self
          .formats
          .insert(identity.clone(), ConsumerAndSuffix { consumer, suffix });
        state.segment_suffix
      },
      Some(consumer) => {
        // We've already seen this format, so just grab its suffix.
        if !self.suffixes.contains_key(format_name) {
          return Err(LuceneError::illegal_state(format!(
            "no suffix for format name: {format_name}"
          )));
        }
        suffix = Some(consumer.suffix);
        get_full_segment_suffix(&write_state.segment_suffix, format_name, consumer.suffix)
      },
    };

    let suffix = suffix.ok_or_else(|| {
      LuceneError::illegal_state(format!("missing suffix for field: {}", field.name))
    })?;
    field.put_borrowed_attribute(
      PER_FIELD_SUFFIX_KEY,
      crate::core::index::index_file_names::decimal_suffix(suffix, &mut [0; 14]),
    );
    let consumer = self.formats.get_mut(&identity).ok_or_else(|| {
      LuceneError::illegal_state(format!(
        "missing doc values consumer for field: {}",
        field.name
      ))
    })?;
    Ok((identity, segment_suffix, &mut consumer.consumer))
  }
}

impl<B, DVC> DocValuesConsumer for FieldsWriter<B, DVC>
where
  B: PerFieldDocValuesFormatBase,
  B::Format: DocValuesFormat<DocValuesConsumer<DVC::IndexOutput> = DVC>,
  DVC: DocValuesConsumer,
{
  type IndexOutput = DVC::IndexOutput;

  fn add_numeric_field<D1, D2, D>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    field: &Arc<FieldInfo>,
    values_producer: &D,
  ) -> Result<()>
  where
    D1: Directory<IndexOutput = Self::IndexOutput>,
    D: DocValuesProducer,
  {
    let (_, segment_suffix, consumer) = self.get_instance(write_state, segment_info, field)?;
    let state = SegmentWriteState::copy_with_suffix(write_state, segment_suffix);
    consumer.add_numeric_field(&state, segment_info, field, values_producer)
  }

  fn add_binary_field<D1, D2, D>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    field: &Arc<FieldInfo>,
    values_producer: &D,
  ) -> Result<()>
  where
    D1: Directory<IndexOutput = Self::IndexOutput>,
    D: DocValuesProducer,
  {
    let (_, segment_suffix, consumer) = self.get_instance(write_state, segment_info, field)?;
    let state = SegmentWriteState::copy_with_suffix(write_state, segment_suffix);
    consumer.add_binary_field(&state, segment_info, field, values_producer)
  }

  fn add_sorted_field<D1, D2, D>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    field: &Arc<FieldInfo>,
    values_producer: &D,
  ) -> Result<()>
  where
    D1: Directory<IndexOutput = Self::IndexOutput>,
    D: DocValuesProducer,
  {
    let (_, segment_suffix, consumer) = self.get_instance(write_state, segment_info, field)?;
    let state = SegmentWriteState::copy_with_suffix(write_state, segment_suffix);
    consumer.add_sorted_field(&state, segment_info, field, values_producer)
  }

  fn add_sorted_numeric_field<D1, D2, D>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    field: &Arc<FieldInfo>,
    values_producer: &D,
  ) -> Result<()>
  where
    D1: Directory<IndexOutput = Self::IndexOutput>,
    D: DocValuesProducer,
  {
    let (_, segment_suffix, consumer) = self.get_instance(write_state, segment_info, field)?;
    let state = SegmentWriteState::copy_with_suffix(write_state, segment_suffix);
    consumer.add_sorted_numeric_field(&state, segment_info, field, values_producer)
  }

  fn add_sorted_set_field<D1, D2, D>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    field: &Arc<FieldInfo>,
    values_producer: &D,
  ) -> Result<()>
  where
    D1: Directory<IndexOutput = Self::IndexOutput>,
    D: DocValuesProducer,
  {
    let (_, segment_suffix, consumer) = self.get_instance(write_state, segment_info, field)?;
    let state = SegmentWriteState::copy_with_suffix(write_state, segment_suffix);
    consumer.add_sorted_set_field(&state, segment_info, field, values_producer)
  }

  fn merge<D1, D2, MS>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    merge_state: &MS,
  ) -> Result<()>
  where
    D1: Directory<IndexOutput = Self::IndexOutput>,
    MS: MergeStateAccess,
  {
    let mut consumers_to_fields: HashMap<Identity, (String, Vec<&String>)> = HashMap::new();

    // Group each consumer by the fields it handles.
    for field_info in merge_state.merge_field_infos().iter() {
      if field_info.get_doc_values_type() == &DocValuesType::None {
        continue;
      }
      // Merge should ignore current format for the fields being merged.
      let (identity, segment_suffix, _) = self.get_instance_with_ignore_current_format(
        write_state,
        segment_info,
        field_info,
        true,
      )?;
      consumers_to_fields
        .entry(identity)
        .or_insert_with(|| (segment_suffix, Vec::new()))
        .1
        .push(&field_info.name);
    }

    // Delegate the merge to the appropriate consumer.
    for (identity, (segment_suffix, fields)) in consumers_to_fields {
      let restricted = PerFieldMergeState::restrict_fields(merge_state, &fields)?;
      let state = SegmentWriteState::copy_with_suffix(write_state, segment_suffix);
      let consumer = self
        .formats
        .get_mut(&identity)
        .ok_or_else(|| LuceneError::illegal_state("missing doc values consumer for merge"))?;
      consumer.consumer.merge(&state, segment_info, &restricted)?;
    }
    Ok(())
  }
}

impl<B, DVC> Closeable for FieldsWriter<B, DVC>
where
  DVC: Closeable,
{
  fn close(&mut self) -> Result<()> {
    // Close all subs.
    IOUtils::close_with(self.formats.values_mut(), Closeable::close)
  }
}

fn get_full_segment_suffix<T: Display>(
  outer_segment_suffix: &str,
  format_name: &str,
  suffix: T,
) -> String {
  if outer_segment_suffix.is_empty() {
    format!("{format_name}_{suffix}")
  } else {
    format!("{outer_segment_suffix}_{format_name}_{suffix}")
  }
}

pub struct FieldsReader<DVP> {
  fields: HashMap<i32, Arc<DVP>>,
  formats: HashMap<String, Arc<DVP>>,
}

impl<DVP> FieldsReader<DVP>
where
  DVP: DocValuesProducer,
{
  #[cfg(test)]
  pub(crate) fn map_producers<T, F>(self, mut mapper: F) -> Result<FieldsReader<T>>
  where
    T: DocValuesProducer,
    F: FnMut(DVP) -> T,
  {
    let field_formats: HashMap<i32, *const DVP> = self
      .fields
      .into_iter()
      .map(|(field_number, producer)| (field_number, Arc::as_ptr(&producer)))
      .collect();
    let mut old_to_new = HashMap::with_capacity(self.formats.len());
    let mut formats = HashMap::with_capacity(self.formats.len());
    for (segment_suffix, producer) in self.formats {
      let old_ptr = Arc::as_ptr(&producer);
      let producer = Arc::try_unwrap(producer).map_err(|_| {
        LuceneError::illegal_state(format!(
          "doc values producer for segment suffix {segment_suffix} is still shared"
        ))
      })?;
      let producer = Arc::new(mapper(producer));
      old_to_new.insert(old_ptr, Arc::clone(&producer));
      formats.insert(segment_suffix, producer);
    }
    let mut fields = HashMap::with_capacity(field_formats.len());
    for (field_number, old_ptr) in field_formats {
      let producer = old_to_new.get(&old_ptr).ok_or_else(|| {
        LuceneError::illegal_state(format!(
          "missing doc values producer for field number: {field_number}"
        ))
      })?;
      fields.insert(field_number, Arc::clone(producer));
    }
    Ok(FieldsReader { fields, formats })
  }

  // Clone for merge.
  fn from_other(other: &Self) -> Result<Self> {
    let mut fields = HashMap::with_capacity(other.fields.len());
    let mut formats = HashMap::with_capacity(other.formats.len());
    // First clone all formats.
    for (segment_suffix, format) in &other.formats {
      let values = match format.as_ref().get_merge_instance()? {
        Some(format) => Arc::new(format),
        None => Arc::clone(format),
      };
      formats.insert(segment_suffix.clone(), values);
    }

    let single = if other.formats.len() == 1 {
      other.formats.values().next().zip(formats.values().next())
    } else {
      None
    };
    let mut old_to_new = HashMap::new();
    if single.is_none() {
      old_to_new.reserve(other.formats.len());
      for (segment_suffix, format) in &other.formats {
        old_to_new.insert(Arc::as_ptr(format), &formats[segment_suffix]);
      }
    }

    for (field_number, format) in &other.fields {
      let producer = match single {
        Some((old, new)) => Arc::ptr_eq(old, format).then_some(new),
        None => old_to_new.get(&Arc::as_ptr(format)).copied(),
      };
      let producer = producer.ok_or_else(|| {
        LuceneError::illegal_state(format!(
          "missing merge instance for field number: {field_number}"
        ))
      })?;
      fields.insert(*field_number, Arc::clone(producer));
    }

    Ok(Self { fields, formats })
  }

  fn new<PF, D1, D2>(
    read_state: &SegmentReadState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self>
  where
    PF: DocValuesFormat<DocValuesProducer<D1::IndexInput> = DVP>,
    D1: Directory,
  {
    let mut fields = HashMap::new();
    let mut formats: HashMap<String, Arc<DVP>> = HashMap::new();

    let mut success = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      // Read field name -> format name.
      for field_info in read_state.field_infos.iter() {
        if field_info.get_doc_values_type() == &DocValuesType::None {
          continue;
        }
        let field_name = &field_info.name;
        let attributes = field_info.attributes();
        let Some(format_name) = attributes.get(PER_FIELD_FORMAT_KEY) else {
          // Null format name means the field is in field infos, but has no doc values.
          continue;
        };
        let suffix = attributes.get(PER_FIELD_SUFFIX_KEY).ok_or_else(|| {
          LuceneError::illegal_state(format!(
            "missing attribute: {PER_FIELD_SUFFIX_KEY} for field: {field_name}"
          ))
        })?;
        let segment_suffix =
          get_full_segment_suffix(&read_state.segment_suffix, format_name, suffix);
        let producer = match formats.entry(segment_suffix) {
          Entry::Occupied(entry) => Arc::clone(entry.get()),
          Entry::Vacant(entry) => {
            let format = PF::for_name(format_name)?;
            let state = SegmentReadState::copy_with_suffix(read_state, entry.key());
            let producer = Arc::new(format.fields_producer(&state, segment_info)?);
            Arc::clone(entry.insert(producer))
          },
        };
        fields.insert(field_info.number, producer);
      }
      success = true;
      Ok(())
    }));

    if !success {
      IOUtils::close_while_handling_exception_with(formats.values(), |format| format.close());
    }
    unwrap_caught_result!(result)?;

    Ok(Self { fields, formats })
  }
}

impl<DVP> Display for FieldsReader<DVP> {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "PerFieldDocValues(formats={})", self.formats.len())
  }
}

impl<DVP> CloseableRef for FieldsReader<DVP>
where
  DVP: CloseableRef,
{
  fn close(&self) -> Result<()> {
    IOUtils::close_with(self.formats.values(), |format| format.close())
  }
}

impl<DVP> DocValuesProducer for FieldsReader<DVP>
where
  DVP: DocValuesProducer,
{
  type NumericDocValues = DVP::NumericDocValues;

  fn get_numeric(&self, field: &Arc<FieldInfo>) -> Result<Self::NumericDocValues> {
    self
      .fields
      .get(&field.number)
      .ok_or_else(|| {
        LuceneError::illegal_state(format!(
          "missing doc values producer for field: {}",
          field.name
        ))
      })?
      .get_numeric(field)
  }

  type BinaryDocValues = DVP::BinaryDocValues;

  fn get_binary(&self, field: &Arc<FieldInfo>) -> Result<Self::BinaryDocValues> {
    self
      .fields
      .get(&field.number)
      .ok_or_else(|| {
        LuceneError::illegal_state(format!(
          "missing doc values producer for field: {}",
          field.name
        ))
      })?
      .get_binary(field)
  }

  type SortedDocValues = DVP::SortedDocValues;

  fn get_sorted(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedDocValues> {
    self
      .fields
      .get(&field.number)
      .ok_or_else(|| {
        LuceneError::illegal_state(format!(
          "missing doc values producer for field: {}",
          field.name
        ))
      })?
      .get_sorted(field)
  }

  type SortedNumericDocValues = DVP::SortedNumericDocValues;

  fn get_sorted_numeric(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedNumericDocValues> {
    self
      .fields
      .get(&field.number)
      .ok_or_else(|| {
        LuceneError::illegal_state(format!(
          "missing doc values producer for field: {}",
          field.name
        ))
      })?
      .get_sorted_numeric(field)
  }

  type SortedSetDocValues = DVP::SortedSetDocValues;

  fn get_sorted_set(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedSetDocValues> {
    self
      .fields
      .get(&field.number)
      .ok_or_else(|| {
        LuceneError::illegal_state(format!(
          "missing doc values producer for field: {}",
          field.name
        ))
      })?
      .get_sorted_set(field)
  }

  type DocValuesSkipper = DVP::DocValuesSkipper;

  fn get_skipper(&self, field: &Arc<FieldInfo>) -> Result<Option<Self::DocValuesSkipper>> {
    match self.fields.get(&field.number) {
      Some(producer) => producer.get_skipper(field),
      None => Ok(None),
    }
  }

  fn check_integrity(&self) -> Result<()> {
    for format in self.formats.values() {
      format.check_integrity()?;
    }
    Ok(())
  }

  fn get_merge_instance(&self) -> Result<Option<Self>>
  where
    Self: Sized,
  {
    Ok(Some(Self::from_other(self)?))
  }
}

impl<B> DocValuesFormat for PerFieldDocValuesFormat<B>
where
  B: PerFieldDocValuesFormatBase,
{
  fn get_name(&self) -> &str {
    PER_FIELD_NAME
  }

  type DocValuesConsumer<O: IndexOutput> =
    FieldsWriter<B, <B::Format as DocValuesFormat>::DocValuesConsumer<O>>;

  fn fields_consumer<D1, D2>(
    &self,
    _state: &SegmentWriteState<D1>,
    _segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::DocValuesConsumer<D1::IndexOutput>>
  where
    D1: Directory,
  {
    Ok(FieldsWriter::new(Arc::clone(&self.base)))
  }

  type DocValuesProducer<I: IndexInput> =
    FieldsReader<<B::Format as DocValuesFormat>::DocValuesProducer<I>>;

  fn fields_producer<D1, D2>(
    &self,
    state: &SegmentReadState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::DocValuesProducer<D1::IndexInput>>
  where
    D1: Directory,
  {
    FieldsReader::new::<B::Format, D1, D2>(state, segment_info)
  }

  fn for_name(name: &str) -> Result<Arc<Self>> {
    Err(LuceneError::illegal_argument(format!(
      "Could not load doc values format named \"{name}\""
    )))
  }
}
