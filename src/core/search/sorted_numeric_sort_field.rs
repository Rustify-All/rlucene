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
use crate::core::index::doc_values::{DocValues, SortedNumeric};
use crate::core::index::index_sorter::{
    DocComparatorEnum, DoubleSorter, FloatSorter, IndexSorter, IntSorter, LongSorter,
    NumericDocValuesProvider,
};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::sort_field_provider::SortFieldProvider;
use crate::core::search::field_comparator::FieldComparatorEnum;
use crate::core::search::sort_field::{MissingValueEnum, SortField, SortFieldType, SortFiledBase};
use crate::core::search::sort_field_enum::SortFieldEnum;
use crate::core::search::sorted_numeric_selector::{
    NumericDocValuesImpl, SortedNumericSelector, SortedNumericSelectorType,
};
use crate::core::store::{DataInput, DataOutput};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::numeric_utils::NumericUtils;
use std::fmt::Display;
use std::hash::{Hash, Hasher};

#[derive(Clone)]
pub struct SortedNumericSortField {
    selector: SortedNumericSelectorType,
    parent_sort: SortField,
}
impl SortedNumericSortField {
    /// Creates a sort by the minimum value in the set for the document.
    ///
    /// # Arguments
    ///
    /// * `field` - Name of the field to sort by. Must not be empty.
    /// * `sort_field_type` - Type of values.
    pub fn new<T>(field: T, sort_field_type: SortFieldType) -> Result<Self>
    where
        T: Into<String>,
    {
        Self::with_reverse(field, sort_field_type, false)
    }

    /// Creates a sort, possibly in reverse, by the minimum value in the set for
    /// the document.
    ///
    /// # Arguments
    ///
    /// * `field` - Name of the field to sort by. Must not be empty.
    /// * `sort_field_type` - Type of values.
    /// * `reverse` - `true` if natural order should be reversed.
    pub fn with_reverse<T>(field: T, sort_field_type: SortFieldType, reverse: bool) -> Result<Self>
    where
        T: Into<String>,
    {
        Self::with_selector(
            field,
            sort_field_type,
            reverse,
            SortedNumericSelectorType::Min,
        )
    }
    /// Creates a sort, possibly in reverse, specifying how the sort value from
    /// the document's set is selected.
    ///
    /// # Arguments
    ///
    /// * `field` - Name of the field to sort by.
    /// * `sort_field_type` - Type of values.
    /// * `reverse` - `true` if natural order should be reversed.
    /// * `selector` - Custom selector type for choosing the sort value from the
    ///   set.
    pub fn with_selector<T>(
        field: T,
        sort_field_type: SortFieldType,
        reverse: bool,
        selector: SortedNumericSelectorType,
    ) -> Result<Self>
    where
        T: Into<String>,
    {
        let sort_field = SortField::with_reverse(Some(field), sort_field_type, reverse)?;
        Ok(SortedNumericSortField {
            selector,
            parent_sort: sort_field,
        })
    }
    pub fn read_selector_type(
        data_input: &mut impl DataInput,
    ) -> Result<SortedNumericSelectorType> {
        let selector_type = data_input.read_int()?;

        match selector_type {
            0 => Ok(SortedNumericSelectorType::Min),
            1 => Ok(SortedNumericSelectorType::Max),
            _ => Err(LuceneError::illegal_argument(format!(
                "Cannot deserialize SortedNumericSortField - unknown selector type: {selector_type}"
            ))),
        }
    }
}

impl SortFiledBase for SortedNumericSortField {
    fn set_missing_value(&mut self, missing_value: Option<MissingValueEnum>) -> Result<()> {
        self.parent_sort.missing_value = missing_value;
        Ok(())
    }

    fn needs_scores(&self) -> bool {
        self.parent_sort.needs_scores()
    }

    type IndexSort = IndexSorterNumeric;

    fn get_index_sorter(&self) -> Result<Option<Self::IndexSort>> {
        debug_assert!(self.parent_sort.get_field().is_some());
        let get_value = NumericDocValuesProviderImpl::new(
            self.selector,
            self.parent_sort.type_,
            self.parent_sort.get_field().unwrap().to_string(),
        );
        match self.parent_sort.type_ {
            SortFieldType::Int => Ok(Some(IndexSorterNumeric::Int(IntSorter::new(
                NumericProvider::NAME.to_string(),
                self.parent_sort.missing_value.clone(),
                self.parent_sort.reverse,
                get_value,
            )?))),
            SortFieldType::Long => Ok(Some(IndexSorterNumeric::Long(LongSorter::new(
                NumericProvider::NAME.to_string(),
                self.parent_sort.missing_value.clone(),
                self.parent_sort.reverse,
                get_value,
            )?))),
            SortFieldType::Double => Ok(Some(IndexSorterNumeric::Double(DoubleSorter::new(
                NumericProvider::NAME.to_string(),
                self.parent_sort.missing_value.clone(),
                self.parent_sort.reverse,
                get_value,
            )?))),
            SortFieldType::Float => Ok(Some(IndexSorterNumeric::Float(FloatSorter::new(
                NumericProvider::NAME.to_string(),
                self.parent_sort.missing_value.clone(),
                self.parent_sort.reverse,
                get_value,
            )?))),
            _ => Ok(None),
        }
    }

    fn serialize(&self, out: &mut impl DataOutput) -> Result<()> {
        debug_assert!(self.parent_sort.get_field().is_some());
        out.write_string(self.parent_sort.get_field().unwrap())?;
        out.write_string(&self.parent_sort.type_.to_string())?;
        out.write_int(if self.parent_sort.reverse { 1 } else { 0 })?;
        out.write_int(self.selector as i32)?;
        if let Some(missing_value) = &self.parent_sort.missing_value {
            out.write_int(1)?;
            match self.parent_sort.type_ {
                SortFieldType::Int => {
                    if let MissingValueEnum::Int(value) = missing_value {
                        out.write_int(*value)?;
                    } else {
                        return Err(LuceneError::illegal_state(
                            "Missing value type mismatch for INT.".to_string(),
                        ));
                    }
                },
                SortFieldType::Long => {
                    if let MissingValueEnum::Long(value) = missing_value {
                        out.write_long(*value)?;
                    } else {
                        return Err(LuceneError::illegal_state(
                            "Missing value type mismatch for LONG.".to_string(),
                        ));
                    }
                },
                SortFieldType::Float => {
                    if let MissingValueEnum::Float(value) = missing_value {
                        out.write_int(NumericUtils::float_to_sortable_int(*value))?;
                    } else {
                        return Err(LuceneError::illegal_state(
                            "Missing value type mismatch for FLOAT.".to_string(),
                        ));
                    }
                },
                SortFieldType::Double => {
                    if let MissingValueEnum::Double(value) = missing_value {
                        out.write_long(NumericUtils::double_to_sortable_long(*value))?;
                    } else {
                        return Err(LuceneError::illegal_state(
                            "Missing value type mismatch for DOUBLE.".to_string(),
                        ));
                    }
                },
                SortFieldType::Custom
                | SortFieldType::Doc
                | SortFieldType::Rewritable
                | SortFieldType::StringVal
                | SortFieldType::Score
                | SortFieldType::String => {
                    return Err(LuceneError::illegal_state(format!(
                        "Cannot serialize field of type {:?}.",
                        self.parent_sort.type_
                    )));
                },
            }
        } else {
            out.write_int(0)?;
        }

        Ok(())
    }

    type FieldComparator = FieldComparatorEnum;
}
impl Display for SortedNumericSortField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut buffer = String::new();
        debug_assert!(self.parent_sort.get_field().is_some());
        buffer.push_str(&format!(
            "<sortednumeric: \"{}\">",
            self.parent_sort.get_field().unwrap()
        ));
        if self.parent_sort.reverse {
            buffer.push('!');
        }
        if let Some(missing_value) = &self.parent_sort.missing_value {
            buffer.push_str(&format!(" missingValue={missing_value}"));
        }
        buffer.push_str(&format!(" selector={:?}", self.selector));
        buffer.push_str(&format!(" type={:?}", self.parent_sort.type_));
        write!(f, "{buffer}")
    }
}
impl Hash for SortedNumericSortField {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.parent_sort.type_.hash(state);
        self.selector.hash(state);
        self.parent_sort.hash(state);
    }
}

pub struct NumericProvider;
impl NumericProvider {
    /// The name this Provider is registered under.
    pub const NAME: &'static str = "SortedNumericSortField";
}
impl SortFieldProvider for NumericProvider {
    fn read_sort_field(&self, data_input: &mut impl DataInput) -> Result<SortFieldEnum> {
        let field_name = data_input.read_string()?;
        let field_type = SortFieldType::read_type(data_input)?;
        let reverse = data_input.read_int()? == 1;
        let selector = SortedNumericSortField::read_selector_type(data_input)?;
        let mut sorted_numeric_sort_field =
            SortedNumericSortField::with_selector(field_name, field_type, reverse, selector)?;
        let value = data_input.read_int()?;
        if value == 1 {
            match field_type {
                SortFieldType::Int => {
                    let missing_value = data_input.read_int()?;
                    sorted_numeric_sort_field
                        .set_missing_value(Some(MissingValueEnum::Int(missing_value)))?;
                },
                SortFieldType::Long => {
                    let missing_value = data_input.read_long()?;
                    sorted_numeric_sort_field
                        .set_missing_value(Some(MissingValueEnum::Long(missing_value)))?;
                },
                SortFieldType::Float => {
                    let missing_value = NumericUtils::sortable_int_to_float(data_input.read_int()?);
                    sorted_numeric_sort_field
                        .set_missing_value(Some(MissingValueEnum::Float(missing_value)))?;
                },
                SortFieldType::Double => {
                    let missing_value =
                        NumericUtils::sortable_long_to_double(data_input.read_long()?);
                    sorted_numeric_sort_field
                        .set_missing_value(Some(MissingValueEnum::Double(missing_value)))?;
                },
                SortFieldType::Custom
                | SortFieldType::Doc
                | SortFieldType::Rewritable
                | SortFieldType::StringVal
                | SortFieldType::Score
                | SortFieldType::String => {
                    return Err(LuceneError::illegal_state(format!(
                        "Cannot deserialize sort of type {field_type:?}"
                    )));
                },
            }
        } else {
            debug_assert!(value == 0);
        }
        Ok(SortFieldEnum::SortedNumeric(sorted_numeric_sort_field))
    }

    fn write_sort_field(&self, sf: &SortFieldEnum, output: &mut impl DataOutput) -> Result<()> {
        sf.serialize(output)
    }
}
impl PartialEq for SortedNumericSortField {
    fn eq(&self, other: &Self) -> bool {
        if self.parent_sort != other.parent_sort {
            return false;
        }
        self.selector == other.selector && self.parent_sort.type_ == other.parent_sort.type_
    }
}
impl Eq for SortedNumericSortField {}

pub struct NumericDocValuesProviderImpl {
    selector: SortedNumericSelectorType,
    sort_field_type: SortFieldType,
    field: String,
}
impl NumericDocValuesProviderImpl {
    pub fn new(
        selector: SortedNumericSelectorType,
        sort_field_type: SortFieldType,
        field: String,
    ) -> Self {
        Self {
            selector,
            sort_field_type,
            field,
        }
    }
}
impl NumericDocValuesProvider for NumericDocValuesProviderImpl {
    type NumericDocValues<LR>
        = NumericDocValuesImpl<SortedNumeric<LR>>
    where
        LR: LeafReader;

    fn get<LR>(&self, leaf_reader: &LR) -> Result<Self::NumericDocValues<LR>>
    where
        LR: LeafReader,
    {
        SortedNumericSelector::wrap(
            DocValues::get_sorted_numeric(leaf_reader, &self.field)?,
            self.selector,
            self.sort_field_type,
        )
    }
}

pub enum IndexSorterNumeric {
    Int(IntSorter<NumericDocValuesProviderImpl>),
    Long(LongSorter<NumericDocValuesProviderImpl>),
    Double(DoubleSorter<NumericDocValuesProviderImpl>),
    Float(FloatSorter<NumericDocValuesProviderImpl>),
}
impl IndexSorter for IndexSorterNumeric {
    fn get_provider_name(&self) -> &str {
        match self {
            IndexSorterNumeric::Int(i) => i.get_provider_name(),
            IndexSorterNumeric::Long(l) => l.get_provider_name(),
            IndexSorterNumeric::Double(d) => d.get_provider_name(),
            IndexSorterNumeric::Float(f) => f.get_provider_name(),
        }
    }

    type DocComparator = DocComparatorEnum;

    fn get_doc_comparator<LR>(&self, leaf_reader: &LR, max_doc: i32) -> Result<Self::DocComparator>
    where
        LR: LeafReader,
    {
        match self {
            IndexSorterNumeric::Int(i) => Ok(DocComparatorEnum::Int(
                i.get_doc_comparator(leaf_reader, max_doc)?,
            )),
            IndexSorterNumeric::Long(l) => Ok(DocComparatorEnum::Long(
                l.get_doc_comparator(leaf_reader, max_doc)?,
            )),
            IndexSorterNumeric::Double(d) => Ok(DocComparatorEnum::Double(
                d.get_doc_comparator(leaf_reader, max_doc)?,
            )),
            IndexSorterNumeric::Float(f) => Ok(DocComparatorEnum::Float(
                f.get_doc_comparator(leaf_reader, max_doc)?,
            )),
        }
    }
}
