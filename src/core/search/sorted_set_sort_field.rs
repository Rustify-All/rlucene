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
use crate::core::index::index_sorter::{SortedDocValuesProvider, StringSorter};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::sort_field_provider::SortFieldProvider;
use crate::core::search::field_comparator::FieldComparatorEnum;
use crate::core::search::sort_field::{MissingValueEnum, SortField, SortFieldType, SortFiledBase};
use crate::core::search::sort_field_enum::SortFieldEnum;
use crate::core::search::sorted_set_selector::{
    SortedDocValuesWrapEnum, SortedSetSelector, SortedSetSelectorType,
};
use crate::core::store::{DataInput, DataOutput};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::fmt::Display;
use std::hash::{Hash, Hasher};

#[derive(Clone)]
pub struct SortedSetSortField {
    selector: SortedSetSelectorType,
    parent_sort: SortField,
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
    /// [`SortedSetSelectorType#Min`](SortedSetSelectorType::Min) require
    /// optional codec support.
    pub fn with_selector<T>(
        field: T,
        reverse: bool,
        selector: SortedSetSelectorType,
    ) -> Result<Self>
    where
        T: Into<String>,
    {
        let sort_field = SortField::with_reverse(Some(field), SortFieldType::Custom, reverse)?;
        Ok(SortedSetSortField {
            selector,
            parent_sort: sort_field,
        })
    }
    fn read_selector_type(data_input: &mut impl DataInput) -> Result<SortedSetSelectorType> {
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
        let mut buffer = String::new();
        debug_assert!(self.parent_sort.get_field().is_some());
        buffer.push_str(&format!(
            "<sortedset: \"{}\">",
            self.parent_sort.get_field().unwrap()
        ));
        if self.parent_sort.reverse {
            buffer.push('!');
        }
        if let Some(missing_value) = &self.parent_sort.missing_value {
            buffer.push_str(&format!(" missingValue={missing_value}"));
        }
        buffer.push_str(&format!(" selector={:?}", self.selector));
        write!(f, "{buffer}")
    }
}
impl SortFiledBase for SortedSetSortField {
    fn set_missing_value(&mut self, missing_value: Option<MissingValueEnum>) -> Result<()> {
        match missing_value {
            Some(MissingValueEnum::StringFirst) | Some(MissingValueEnum::StringLast) => {
                self.parent_sort.missing_value = missing_value;
                Ok(())
            },
            _ => Err(LuceneError::illegal_argument(
                "For SORTED_SET type, missing value must be either STRING_FIRST or STRING_LAST"
                    .to_string(),
            )),
        }
    }

    fn needs_scores(&self) -> bool {
        self.parent_sort.needs_scores()
    }

    type IndexSort = StringSorter<SortedDocValuesProviderImpl>;

    fn get_index_sorter(&self) -> Result<Option<Self::IndexSort>> {
        debug_assert!(self.parent_sort.get_field().is_some());
        let missing_value = self.parent_sort.missing_value.clone();
        Ok(Some(StringSorter::new(
            SetProvider::NAME.to_string(),
            missing_value,
            self.parent_sort.reverse,
            SortedDocValuesProviderImpl::new(
                self.selector,
                self.parent_sort.get_field().unwrap().to_string(),
            ),
        )))
    }

    fn serialize(&self, out: &mut impl DataOutput) -> Result<()> {
        debug_assert!(self.parent_sort.get_field().is_some());
        out.write_string(self.parent_sort.get_field().unwrap())?;
        out.write_int(if self.parent_sort.reverse { 1 } else { 0 })?;
        out.write_int(self.selector as i32)?;
        match self.parent_sort.missing_value {
            Some(MissingValueEnum::StringFirst) => out.write_int(1)?,
            Some(MissingValueEnum::StringLast) => out.write_int(2)?,
            _ => out.write_int(0)?,
        }
        Ok(())
    }

    type FieldComparator = FieldComparatorEnum;
}
impl Hash for SortedSetSortField {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.selector.hash(state);
        self.parent_sort.hash(state);
    }
}

pub struct SetProvider;
impl SetProvider {
    /// The name this Provider is registered under.
    pub const NAME: &'static str = "SortedSetSortField";
}
impl SortFieldProvider for SetProvider {
    fn read_sort_field(&self, data_input: &mut impl DataInput) -> Result<SortFieldEnum> {
        let field_name = data_input.read_string()?;
        let reverse = data_input.read_int()? == 1;
        let selector = SortedSetSortField::read_selector_type(data_input)?;
        let mut sorted_set_sort_field =
            SortedSetSortField::with_selector(field_name, reverse, selector)?;

        let value = data_input.read_int()?;
        match value {
            1 => sorted_set_sort_field.set_missing_value(Some(MissingValueEnum::StringFirst))?,
            2 => sorted_set_sort_field.set_missing_value(Some(MissingValueEnum::StringLast))?,
            _ => {
                debug_assert!(value == 0);
            },
        }
        Ok(SortFieldEnum::SortedSet(sorted_set_sort_field))
    }

    fn write_sort_field(&self, sf: &SortFieldEnum, output: &mut impl DataOutput) -> Result<()> {
        sf.serialize(output)
    }
}
impl PartialEq for SortedSetSortField {
    fn eq(&self, other: &Self) -> bool {
        if self.parent_sort != other.parent_sort {
            return false;
        }
        self.selector == other.selector
    }
}
impl Eq for SortedSetSortField {}

pub struct SortedDocValuesProviderImpl {
    selector: SortedSetSelectorType,
    field: String,
}
impl SortedDocValuesProviderImpl {
    pub fn new(selector: SortedSetSelectorType, field: String) -> Self {
        SortedDocValuesProviderImpl { selector, field }
    }
}
impl SortedDocValuesProvider for SortedDocValuesProviderImpl {
    type SortedDocValues<LR>
        = SortedDocValuesWrapEnum<SortedSet<LR>>
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
