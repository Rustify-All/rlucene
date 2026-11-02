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

use crate::core::index::doc_values::{DocValues, SortedSet};
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_sorter::{SortedDocValuesProvider, StringSorter};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::sort_field_provider::SortFieldProvider;
use crate::core::search::comparators::term_ord_val_comparator::{
  TermOrdValComparator, TermOrdValDocValues, TermOrdValLeafComparator,
};
use crate::core::search::field_comparator::FieldComparator;
use crate::core::search::pruning::Pruning;
use crate::core::search::sort_field::{MissingValueEnum, SortField, SortFieldType, SortFiledBase};
use crate::core::search::sort_field_enum::SortFieldEnum;
use crate::core::search::sorted_set_selector::{
  SortedDocValuesWrap, SortedSetSelector, SortedSetSelectorType,
};
use crate::core::store::{DataInput, DataOutput};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::fmt::Display;
use std::hash::{Hash, Hasher};

/// SortField for [`SortedSetDocValues`](crate::core::index::sorted_set_doc_values::SortedSetDocValues).
///
/// A SortedSetDocValues contains multiple values for a field, so sorting with this technique
/// "selects" a value as the representative sort value for the document.
///
/// By default, the minimum value in the set is selected as the sort value, but this can be
/// customized. Selectors other than the default do have some limitations to ensure that all
/// selections happen in constant-time for performance.
///
/// Like sorting by string, this also supports sorting missing values as first or last, via
/// [`SortFiledBase::set_missing_value`].
///
/// See also: [`SortedSetSelector`]
#[derive(Clone)]
pub struct SortedSetSortField {
  selector: SortedSetSelectorType,
  pub(crate) base: SortField,
}
impl SortedSetSortField {
  /// Creates a sort, possibly in reverse, by the minimum value in the set for
  /// the document.
  ///
  /// # Arguments
  ///
  /// * `field` - Name of the field to sort by.
  /// * `reverse` - `true` if natural order should be reversed.
  pub fn new<T>(field: T, reverse: bool) -> Result<Self>
  where
    T: Into<String>,
  {
    Self::with_selector(field, reverse, SortedSetSelectorType::Min)
  }

  /// Creates a sort, possibly in reverse, specifying how the sort value from
  /// the document's set is selected.
  ///
  /// # Arguments
  ///
  /// * `field` - Name of the field to sort by.
  /// * `reverse` - `true` if natural order should be reversed.
  /// * `selector` - Custom selector type for choosing the sort value from the
  ///   set.
  /// # Note
  /// selectors other than
  /// [`SortedSetSelectorType::Min`] requires
  /// optional codec support.
  pub fn with_selector<T>(field: T, reverse: bool, selector: SortedSetSelectorType) -> Result<Self>
  where
    T: Into<String>,
  {
    let sort_field = SortField::with_reverse(Some(field), SortFieldType::Custom, reverse)?;
    Ok(SortedSetSortField {
      selector,
      base: sort_field,
    })
  }

  /// Returns the selector in use for this sort.
  pub fn get_selector(&self) -> SortedSetSelectorType {
    self.selector
  }

  fn read_selector_type<DI>(data_input: &mut DI) -> Result<SortedSetSelectorType>
  where
    DI: DataInput,
  {
    let selector_type = data_input.read_int()?;

    match selector_type {
      0 => Ok(SortedSetSelectorType::Min),
      1 => Ok(SortedSetSelectorType::Max),
      2 => Ok(SortedSetSelectorType::MiddleMin),
      3 => Ok(SortedSetSelectorType::MiddleMax),
      _ => Err(LuceneError::illegal_argument(format!(
        "Cannot deserialize SortedSetSortField: unknown selector type {selector_type}"
      ))),
    }
  }
}
impl Display for SortedSetSortField {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let Some(field) = self.base.get_field() else {
      return Err(std::fmt::Error);
    };
    let reverse = if self.base.reverse { "!" } else { "" };
    let buffer = if let Some(missing_value) = &self.base.missing_value {
      format!(
        "<sortedset: \"{field}\">{reverse} missingValue={missing_value} selector={:?}",
        self.selector
      )
    } else {
      format!(
        "<sortedset: \"{field}\">{reverse} selector={:?}",
        self.selector
      )
    };
    write!(f, "{buffer}")
  }
}
impl SortFiledBase for SortedSetSortField {
  fn set_missing_value<T>(&mut self, missing_value: T) -> Result<()>
  where
    T: Into<MissingValueEnum>,
  {
    let missing_value = missing_value.into();
    match missing_value {
      MissingValueEnum::StringFirst | MissingValueEnum::StringLast => {
        self.base.missing_value = Some(missing_value);
        Ok(())
      },
      _ => Err(LuceneError::illegal_argument(
        "For SORTED_SET type, missing value must be either STRING_FIRST or STRING_LAST".to_string(),
      )),
    }
  }

  fn needs_scores(&self) -> bool {
    self.base.needs_scores()
  }

  type IndexSort = StringSorter<SProviderImpl1>;

  fn get_index_sorter(&self) -> Result<Option<Self::IndexSort>> {
    let field = self
      .base
      .get_field()
      .ok_or_else(|| LuceneError::illegal_state("sorted-set sort field has no field name"))?;
    let missing_value = self.base.missing_value.clone();
    Ok(Some(StringSorter::new(
      SetProvider::NAME,
      missing_value,
      self.base.reverse,
      SProviderImpl1::new(self.selector, field.to_string()),
    )))
  }

  fn serialize<DO>(&self, out: &mut DO) -> Result<()>
  where
    DO: DataOutput,
  {
    let field = self
      .base
      .get_field()
      .ok_or_else(|| LuceneError::illegal_state("sorted-set sort field has no field name"))?;
    out.write_string(field)?;
    out.write_int(if self.base.reverse { 1 } else { 0 })?;
    out.write_int(self.selector as i32)?;
    match self.base.missing_value {
      Some(MissingValueEnum::StringFirst) => out.write_int(1)?,
      Some(MissingValueEnum::StringLast) => out.write_int(2)?,
      _ => out.write_int(0)?,
    }
    Ok(())
  }

  type FieldComparator = SortedDocValuesTermOrdValComparator;

  fn get_comparator(&self, num_hits: usize, pruning: Pruning) -> Result<Self::FieldComparator> {
    let final_pruning = if self.base.get_optimize_sort_with_indexed_data() {
      pruning
    } else {
      Pruning::None
    };
    let sort_missing_last = matches!(self.base.missing_value, Some(MissingValueEnum::StringLast));

    let field = self
      .base
      .get_field()
      .ok_or_else(|| LuceneError::illegal_state("field must not be None"))?
      .to_string();
    let base = TermOrdValComparator::new(
      field,
      num_hits,
      sort_missing_last,
      self.base.reverse,
      final_pruning,
    );
    Ok(SortedDocValuesTermOrdValComparator::new(
      base,
      self.selector,
    ))
  }
}
impl Hash for SortedSetSortField {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.selector.hash(state);
    self.base.hash(state);
  }
}

pub struct SetProvider;
impl SetProvider {
  /// The name this Provider is registered under.
  pub const NAME: &'static str = "SortedSetSortField";
}
impl SortFieldProvider for SetProvider {
  fn read_sort_field<DI>(&self, data_input: &mut DI) -> Result<SortFieldEnum>
  where
    DI: DataInput,
  {
    let field_name = data_input.read_string()?.into_owned();
    let reverse = data_input.read_int()? == 1;
    let selector = SortedSetSortField::read_selector_type(data_input)?;
    let mut sorted_set_sort_field =
      SortedSetSortField::with_selector(field_name, reverse, selector)?;

    let value = data_input.read_int()?;
    match value {
      1 => sorted_set_sort_field.set_missing_value(MissingValueEnum::StringFirst)?,
      2 => sorted_set_sort_field.set_missing_value(MissingValueEnum::StringLast)?,
      _ => {},
    }
    Ok(sorted_set_sort_field.into())
  }

  fn write_sort_field<DO>(&self, sf: &SortFieldEnum, output: &mut DO) -> Result<()>
  where
    DO: DataOutput,
  {
    sf.serialize(output)
  }
}
impl PartialEq for SortedSetSortField {
  fn eq(&self, other: &Self) -> bool {
    if self.base != other.base {
      return false;
    }
    self.selector == other.selector
  }
}
impl Eq for SortedSetSortField {}

pub struct SProviderImpl1 {
  selector: SortedSetSelectorType,
  field: String,
}
impl SProviderImpl1 {
  pub fn new<FName>(selector: SortedSetSelectorType, field: FName) -> Self
  where
    FName: Into<String>,
  {
    let field = field.into();
    SProviderImpl1 { selector, field }
  }
}
impl SortedDocValuesProvider for SProviderImpl1 {
  type SortedDocValues<LR>
    = SortedDocValuesWrap<SortedSet<LR>>
  where
    LR: LeafReader;

  fn get<LR>(&self, leaf_reader: &LR) -> Result<Self::SortedDocValues<LR>>
  where
    LR: LeafReader,
  {
    let v = SortedSetSelector::wrap(
      DocValues::get_sorted_set(leaf_reader, &self.field)?,
      self.selector,
    )?;
    Ok(v)
  }
}

pub struct SortedDocValuesTermOrdValComparator {
  pub(crate) base: TermOrdValComparator,
  selector: SortedSetSelectorType,
}
impl SortedDocValuesTermOrdValComparator {
  fn new(base: TermOrdValComparator, selector: SortedSetSelectorType) -> Self {
    SortedDocValuesTermOrdValComparator { base, selector }
  }
}
impl FieldComparator for SortedDocValuesTermOrdValComparator {
  type V = <TermOrdValComparator as FieldComparator>::V;

  fn compare(&self, slot1: usize, slot2: usize) -> i32 {
    self.base.compare(slot1, slot2)
  }

  fn set_top_value(&mut self, value: Self::V) -> Result<()> {
    self.base.set_top_value(value)
  }

  fn value(&self, slot: usize) -> Option<Self::V> {
    self.base.value(slot)
  }

  type LeafFieldComparator<LR>
    = TermOrdValLeafComparator<LR>
  where
    LR: LeafReader;

  fn get_leaf_comparator<LR>(
    &mut self,
    context: &LeafReaderContext<LR>,
  ) -> Result<Self::LeafFieldComparator<LR>>
  where
    LR: LeafReader,
  {
    self.base.current_reader_gen += 1;
    let c = |context: &LeafReaderContext<LR>, field: &str| -> Result<TermOrdValDocValues<LR>> {
      Ok(TermOrdValDocValues::A(SortedSetSelector::wrap(
        DocValues::get_sorted_set(context.reader(), field)?,
        self.selector,
      )?))
    };
    TermOrdValLeafComparator::new(context, c, &mut self.base)
  }

  fn compare_values(&self, first: Option<&Self::V>, second: Option<&Self::V>) -> Result<i32> {
    self.base.compare_values(first, second)
  }

  fn fallback_compare(&self, _first: &Self::V, _second: &Self::V) -> Result<i32> {
    self.base.fallback_compare(_first, _second)
  }

  fn set_single_sort(&mut self) {
    self.base.set_single_sort()
  }

  fn disable_skipping(&mut self) {
    self.base.disable_skipping()
  }
}
