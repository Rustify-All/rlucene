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
#![allow(deprecated)]
use crate::core::index::doc_values::{DocValues, EmptyNumeric, EmptySorted};
use crate::core::index::index_sorter::{
    DocComparatorEnum, DoubleSorter, FloatSorter, IndexSorter, IntSorter, LongSorter,
    NumericDocValuesProvider, SortedDocValuesProvider, StringSorter,
};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::numeric_doc_values::Either2NumericDocValues;
use crate::core::index::sort_field_provider::SortFieldProvider;
use crate::core::index::sorted_doc_values::Either2SortedDocValues;
use crate::core::search::comparators::doc_comparator::DocComparator;
use crate::core::search::comparators::double_comparator::DoubleComparator;
use crate::core::search::comparators::float_comparator::FloatComparator;
use crate::core::search::comparators::int_comparator::IntComparator;
use crate::core::search::comparators::long_comparator::LongComparator;
use crate::core::search::comparators::term_ord_val_comparator::TermOrdValComparator;
use crate::core::search::field_comparator::{
    FieldComparator, FieldComparatorEnum, RelevanceComparator, TermValComparator,
};
use crate::core::search::field_comparator_source::FieldComparatorSourceEnum;
use crate::core::search::pruning::Pruning;
use crate::core::search::sort_field_enum::SortFieldEnum;
use crate::core::search::sorted_numeric_sort_field::NumericProvider;
use crate::core::store::{DataInput, DataOutput};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::numeric_utils::NumericUtils;
use std::fmt;
use std::fmt::Display;
use std::hash::Hash;

/// Stores information about how to sort documents by terms in an individual
/// field. Fields must be indexed to sort by them.
///
/// Sorting on a numeric field that is indexed with both doc values and points
/// may use an optimization to skip non-competitive documents. This optimization
/// relies on the assumption that the same data is stored in these points and
/// doc values.
///
/// Sorting on a SORTED(_SET) field that is indexed with both doc values and
/// term index may use an optimization to skip non-competitive documents. This
/// optimization relies on the assumption that the same data is stored in these
/// term index and doc values.
#[derive(Clone)]
pub struct SortField {
    field: Option<String>,
    pub(crate) type_: SortFieldType,
    comparator_source: Option<FieldComparatorSourceEnum>,
    /// defaults to natural order
    pub(crate) reverse: bool,
    /// Used for 'sortMissingFirst/Last'
    pub(crate) missing_value: Option<MissingValueEnum>,
    /// Indicates if sort should be optimized with indexed data. Set to true by
    /// default.
    #[deprecated(since = "10.0.0")]
    pub(crate) optimize_sort_with_indexed_data: bool,
}

impl SortField {
    /// Creates a sort by terms in the given field with the type of term values
    /// explicitly given.
    ///
    /// # Arguments
    ///
    /// - `Field`: Name of the field to sort by. Can be `None` if `field_type`
    ///   is `SCORE` or `DOC`.
    /// - `field_type`: Type of values in the terms.
    /// - `sub_sort_field`: Provides additional (or customized) sorting
    ///   functionality. This could be a trait or type that encapsulates more
    ///   advanced logic.
    ///
    /// # Errors
    ///
    /// Returns an error if the field is `None` and the type is not `SCORE` or
    /// `DOC`.
    pub fn new<T>(field: Option<T>, field_type: SortFieldType) -> Result<Self>
    where
        T: Into<String>,
    {
        let field = field.map(|f| f.into());
        SortField::init_field_type(field, field_type)
    }
    /// Creates a sort, possibly in reverse, by terms in the given field with
    /// the type of term values explicitly given.
    ///
    /// # Arguments
    ///
    /// - `Field`: Name of the field to sort by. Can be `None` if `field_type`
    ///   is `SCORE` or `DOC`.
    /// - `field_type`: Type of values in the terms.
    /// - `reverse`: `true` if natural order should be reversed.
    /// - `Sub_sort_field`: An additional sorting criterion or a custom
    ///   implementation that provides extended sorting logic. It can be used to
    ///   define advanced or secondary sorting behavior.
    ///
    /// # Errors
    ///
    /// Returns an error if the `field` is `None` and the `field_type` is not
    /// `SCORE` or `DOC`.
    pub fn with_reverse<T>(
        field: Option<T>,
        field_type: SortFieldType,
        reverse: bool,
    ) -> Result<Self>
    where
        T: Into<String>,
    {
        let mut result = Self::new(field, field_type)?;
        result.reverse = reverse;
        Ok(result)
    }
    /// Creates a sort with a custom comparison function and an optional
    /// sub-sort field.
    ///
    /// # Arguments
    ///
    /// - `Field`: Name of the field to sort by.
    /// - `comparator`: A source that returns a comparator for sorting hits;
    ///   cannot be `None`
    /// - `sub_sort_field`: An additional sorting criterion or a custom
    ///   implementation that provides extended sorting logic. It can be used to
    ///   define advanced or secondary sorting behavior.
    /// # Errors
    ///
    /// Returns an error if the `field` is `None` and the `field_type` is not
    /// `SCORE` or `DOC`.
    pub fn with_comparator<T>(
        field: Option<T>,
        comparator: Option<FieldComparatorSourceEnum>,
    ) -> Result<Self>
    where
        T: Into<String>,
    {
        let field = field.map(|f| f.into());
        let mut result = SortField::init_field_type(field, SortFieldType::Custom)?;
        debug_assert!(comparator.is_some());
        result.comparator_source = comparator;
        Ok(result)
    }
    /// Creates a sort, possibly in reverse, with a custom comparison function
    /// and an optional sub-sort field.
    ///
    /// # Arguments
    ///
    /// - `Field`: Name of the field to sort by.
    /// - `comparator`: A source that returns a comparator for sorting hits.
    ///   cannot be `None`
    /// - `reverse`: `true` if natural order should be reversed.
    /// - `Sub_sort_field`: An additional sorting criterion or a custom
    ///   implementation that provides extended sorting logic. It can be used to
    ///   define advanced or secondary sorting behavior.
    /// # Errors
    ///
    /// Returns an error if the `field` is `None` and the `field_type` is not
    /// `SCORE` or `DOC`.
    pub fn with_comparator_reverse<T>(
        field: Option<T>,
        comparator: Option<FieldComparatorSourceEnum>,
        reverse: bool,
    ) -> Result<Self>
    where
        T: Into<String>,
    {
        let mut result = Self::with_comparator(field, comparator)?;
        result.reverse = reverse;
        Ok(result)
    }
    /// Represents sorting by document score (relevance)
    /// # Note
    /// Replace Java's `SortField.FIELD_SCORE` with this method.
    pub fn get_field_score() -> Result<Self> {
        SortField::new::<String>(None, SortFieldType::Score)
    }
    /// Represents sorting by document number (index order).
    /// # Note
    /// Replace Java's `SortField.FIELD_DOC` with this method.
    pub fn get_field_doc() -> Result<Self> {
        SortField::new::<String>(None, SortFieldType::Doc)
    }
    // Sets field & type, and ensures field is not NULL unless
    // type is SCORE or DOC
    fn init_field_type(field: Option<String>, field_type: SortFieldType) -> Result<Self> {
        if field.is_none() && field_type != SortFieldType::Score && field_type != SortFieldType::Doc
        {
            return Err(LuceneError::illegal_argument(
                "field can only be None when type is SCORE or DOC".to_string(),
            ));
        }
        Ok(Self {
            field,
            type_: field_type,
            comparator_source: None,
            reverse: false,
            missing_value: None,
            optimize_sort_with_indexed_data: true,
        })
    }
    /// Returns the value to use for documents that don't have a value.
    ///
    /// A value of `None` indicates that the default value should be used.
    pub fn get_missing_value(&self) -> Option<&MissingValueEnum> {
        self.missing_value.as_ref()
    }
    /// Returns the name of the field.
    ///
    /// This could return `None` if the sort is by `SCORE` or `DOC`.
    ///
    /// # Returns
    /// The name of the field, or `None` if the sort is by `SCORE` or `DOC`.
    pub fn get_field(&self) -> Option<&String> {
        self.field.as_ref()
    }
    /// Returns the type of contents in the field.
    ///
    /// # Returns
    /// One of the constants: `SCORE`, `DOC`, `STRING`, `INT`, or `FLOAT`.
    pub fn get_type(&self) -> &SortFieldType {
        &self.type_
    }
    /// Returns whether the sort should be reversed.
    ///
    /// # Returns
    /// `true` if natural order should be reversed.
    pub fn get_reverse(&self) -> bool {
        self.reverse
    }

    pub fn get_optimize_sort_with_indexed_data(&self) -> bool {
        self.optimize_sort_with_indexed_data
    }
}
impl SortFiledBase for SortField {
    /// Set the value to use for documents that don't have a value.
    fn set_missing_value(&mut self, missing_value: Option<MissingValueEnum>) -> Result<()> {
        match self.type_ {
            SortFieldType::String | SortFieldType::StringVal => {
                if let Some(MissingValueEnum::StringFirst | MissingValueEnum::StringLast) =
                    missing_value
                {
                    self.missing_value = missing_value;
                } else {
                    return Err(LuceneError::illegal_argument(
                        "For STRING type, missing value must be either STRING_FIRST or STRING_LAST"
                            .to_string(),
                    ));
                }
            },
            SortFieldType::Int => {
                if let Some(MissingValueEnum::Int(_)) = missing_value {
                    self.missing_value = missing_value;
                } else {
                    return Err(LuceneError::illegal_argument(
                        "Missing values for Type.INT can only be of type MissingValueEnum::Int"
                            .to_string(),
                    ));
                }
            },
            SortFieldType::Long => {
                if let Some(MissingValueEnum::Long(_)) = missing_value {
                    self.missing_value = missing_value;
                } else {
                    return Err(LuceneError::illegal_argument(
                        "Missing values for Type.LONG can only be of type MissingValueEnum::Long"
                            .to_string(),
                    ));
                }
            },
            SortFieldType::Float => {
                if let Some(MissingValueEnum::Float(_)) = missing_value {
                    self.missing_value = missing_value;
                } else {
                    return Err(LuceneError::illegal_argument(
                        "Missing values for Type.FLOAT can only be of type MissingValueEnum::Float"
                            .to_string(),
                    ));
                }
            },
            SortFieldType::Double => {
                if let Some(MissingValueEnum::Double(_)) = missing_value {
                    self.missing_value = missing_value;
                } else {
                    return Err(LuceneError::illegal_argument("Missing values for Type.DOUBLE can only be of type MissingValueEnum::Double".to_string()));
                }
            },
            _ => {
                return Err(LuceneError::illegal_argument(
                    "Missing value only works for numeric or STRING types".to_string(),
                ));
            },
        }

        Ok(())
    }

    fn needs_scores(&self) -> bool {
        self.type_ == SortFieldType::Score
    }

    type IndexSort = IndexSorterEnumSorter;

    fn get_index_sorter(&self) -> Result<Option<Self::IndexSort>> {
        debug_assert!(self.field.is_some());
        let field = self.field.as_ref().unwrap();
        let get_value = NumericDocValuesProviderImpl1::new(field.to_string());
        let v1 = SortedDocValuesProviderImpl::new(field.to_string());
        match self.type_ {
            SortFieldType::String => Ok(Some(IndexSorterEnumSorter::String(StringSorter::new(
                NumericProvider::NAME.to_string(),
                self.missing_value.clone(),
                self.reverse,
                v1,
            )))),
            SortFieldType::Int => Ok(Some(IndexSorterEnumSorter::Int(IntSorter::new(
                NumericProvider::NAME.to_string(),
                self.missing_value.clone(),
                self.reverse,
                get_value,
            )?))),
            SortFieldType::Long => Ok(Some(IndexSorterEnumSorter::Long(LongSorter::new(
                NumericProvider::NAME.to_string(),
                self.missing_value.clone(),
                self.reverse,
                get_value,
            )?))),
            SortFieldType::Double => Ok(Some(IndexSorterEnumSorter::Double(DoubleSorter::new(
                NumericProvider::NAME.to_string(),
                self.missing_value.clone(),
                self.reverse,
                get_value,
            )?))),
            SortFieldType::Float => Ok(Some(IndexSorterEnumSorter::Float(FloatSorter::new(
                NumericProvider::NAME.to_string(),
                self.missing_value.clone(),
                self.reverse,
                get_value,
            )?))),
            _ => Ok(None),
        }
    }

    fn serialize(&self, out: &mut impl DataOutput) -> Result<()> {
        debug_assert!(self.field.is_some());
        out.write_string(self.field.as_ref().unwrap())?;
        out.write_string(&self.type_.to_string())?;
        out.write_int(if self.reverse { 1 } else { 0 })?;
        if let Some(missing_value) = &self.missing_value {
            out.write_int(1)?;
            match &self.type_ {
                SortFieldType::String => match missing_value {
                    MissingValueEnum::StringLast => out.write_int(0)?,
                    MissingValueEnum::StringFirst => out.write_int(1)?,
                    _ => {
                        return Err(LuceneError::illegal_argument(format!(
                            "Cannot serialize missing value {missing_value} for type STRING"
                        )));
                    },
                },
                SortFieldType::Int => {
                    if let MissingValueEnum::Int(value) = missing_value {
                        out.write_int(*value)?;
                    } else {
                        return Err(LuceneError::illegal_argument(format!(
                            "Invalid missing value {missing_value} for type INT"
                        )));
                    }
                },
                SortFieldType::Long => {
                    if let MissingValueEnum::Long(value) = missing_value {
                        out.write_long(*value)?;
                    } else {
                        return Err(LuceneError::illegal_argument(format!(
                            "Invalid missing value {missing_value} for type LONG"
                        )));
                    }
                },
                SortFieldType::Float => {
                    if let MissingValueEnum::Float(value) = missing_value {
                        out.write_int(NumericUtils::float_to_sortable_int(*value))?;
                    } else {
                        return Err(LuceneError::illegal_argument(format!(
                            "Invalid missing value {missing_value} for type FLOAT"
                        )));
                    }
                },
                SortFieldType::Double => {
                    if let MissingValueEnum::Double(value) = missing_value {
                        out.write_long(NumericUtils::double_to_sortable_long(*value))?;
                    } else {
                        return Err(LuceneError::illegal_argument(format!(
                            "Invalid missing value {missing_value} for type DOUBLE"
                        )));
                    }
                },
                SortFieldType::Custom
                | SortFieldType::Doc
                | SortFieldType::Rewritable
                | SortFieldType::StringVal
                | SortFieldType::Score => {
                    return Err(LuceneError::illegal_argument(format!(
                        "Cannot serialize SortField of type {:?}",
                        self.type_
                    )));
                },
            }
        } else {
            out.write_int(0)?;
        }

        Ok(())
    }

    type FieldComparator = FieldComparatorEnum;

    fn get_comparator(&self, num_hits: usize, pruning: Pruning) -> Result<Self::FieldComparator> {
        let mut field_comparator: FieldComparatorEnum = match self.type_ {
            SortFieldType::Score => RelevanceComparator::new(num_hits).into(),

            SortFieldType::Doc => DocComparator::new(num_hits, self.reverse, pruning).into(),

            SortFieldType::Int => {
                let missing = self.missing_value.as_ref().map(|v| v.as_i32());
                IntComparator::new(
                    self.field.as_ref().unwrap().clone(),
                    num_hits,
                    missing,
                    self.reverse,
                    pruning,
                )
                .into()
            },

            SortFieldType::Float => {
                let missing = self.missing_value.as_ref().map(|v| v.as_f32());
                FloatComparator::new(
                    self.field.as_ref().unwrap().clone(),
                    num_hits,
                    missing,
                    self.reverse,
                    pruning,
                )
                .into()
            },

            SortFieldType::Long => {
                let missing = self.missing_value.as_ref().map(|v| v.as_i64());
                LongComparator::new(
                    self.field.as_ref().unwrap().clone(),
                    num_hits,
                    missing,
                    self.reverse,
                    pruning,
                )
                .into()
            },

            SortFieldType::Double => {
                let missing = self.missing_value.as_ref().map(|v| v.as_f64());
                DoubleComparator::new(
                    self.field.as_ref().unwrap().clone(),
                    num_hits,
                    missing,
                    self.reverse,
                    pruning,
                )
                .into()
            },

            SortFieldType::String => TermOrdValComparator::new(
                self.field.as_ref().unwrap().clone(),
                num_hits,
                matches!(self.missing_value, Some(MissingValueEnum::StringLast)),
                self.reverse,
                pruning,
            )
            .into(),

            SortFieldType::StringVal => TermValComparator::new(
                self.field.as_ref().unwrap().clone(),
                num_hits,
                matches!(self.missing_value, Some(MissingValueEnum::StringLast)),
            )
            .into(),

            SortFieldType::Custom => {
                return Err(LuceneError::unsupported_operation("not supported yet"));
            },

            SortFieldType::Rewritable => {
                return Err(LuceneError::IllegalState(
                    "SortField needs to be rewritten through Sort.rewrite(..) and SortField.rewrite(..)".into(),
                ));
            },
        };

        if !self.optimize_sort_with_indexed_data {
            field_comparator.disable_skipping()
        }

        Ok(field_comparator)
    }
}
impl Display for SortField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut buffer = String::new();
        match self.type_ {
            SortFieldType::Score => buffer.push_str("<score>"),
            SortFieldType::Doc => buffer.push_str("<doc>"),
            SortFieldType::String => {
                buffer.push_str("<string: \"");
                if let Some(ref field) = self.field {
                    buffer.push_str(field);
                }
                buffer.push_str("\">");
            },
            SortFieldType::Int => {
                buffer.push_str("<int: \"");
                if let Some(ref field) = self.field {
                    buffer.push_str(field);
                }
                buffer.push_str("\">");
            },
            SortFieldType::Long => {
                buffer.push_str("<long: \"");
                if let Some(ref field) = self.field {
                    buffer.push_str(field);
                }
                buffer.push_str("\">");
            },
            SortFieldType::Float => {
                buffer.push_str("<float: \"");
                if let Some(ref field) = self.field {
                    buffer.push_str(field);
                }
                buffer.push_str("\">");
            },
            SortFieldType::Double => {
                buffer.push_str("<double: \"");
                if let Some(ref field) = self.field {
                    buffer.push_str(field);
                }
                buffer.push_str("\">");
            },
            SortFieldType::Custom => {
                buffer.push_str("<custom: \"");
                if let Some(ref field) = self.field {
                    buffer.push_str(field);
                }
                buffer.push_str("\": ");
                if let Some(ref comparator) = self.comparator_source {
                    buffer.push_str(&format!("{comparator}"));
                }
                buffer.push('>');
            },
            SortFieldType::StringVal => {
                buffer.push_str("<string_val: \"");
                if let Some(ref field) = self.field {
                    buffer.push_str(field);
                }
                buffer.push_str("\">");
            },
            SortFieldType::Rewritable => {
                buffer.push_str("<rewriteable: \"");
                if let Some(ref field) = self.field {
                    buffer.push_str(field);
                }
                buffer.push_str("\">");
            },
        }
        if self.reverse {
            buffer.push('!');
        }
        if let Some(ref missing_value) = self.missing_value {
            buffer.push_str(" missingValue=");
            buffer.push_str(&format!("{missing_value}"));
        }
        write!(f, "{buffer}")
    }
}
impl PartialEq for SortField {
    fn eq(&self, other: &Self) -> bool {
        self.field == other.field
            && self.type_ == other.type_
            && self.comparator_source == other.comparator_source
            && self.reverse == other.reverse
            && self.missing_value == other.missing_value
    }
}
impl Eq for SortField {}
impl Hash for SortField {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.field.hash(state);
        self.type_.hash(state);
        self.reverse.hash(state);
        self.comparator_source.hash(state);
        self.missing_value.hash(state);
    }
}
// int
pub(crate) struct NumericDocValuesProviderImpl {
    field: String,
}
impl NumericDocValuesProviderImpl {
    pub fn new(field: String) -> Self {
        NumericDocValuesProviderImpl { field }
    }
}
impl NumericDocValuesProvider for NumericDocValuesProviderImpl {
    type NumericDocValues<LR>
        = Either2NumericDocValues<LR::NumericDocValues, EmptyNumeric>
    where
        LR: LeafReader;
    fn get<LR>(&self, leaf_reader: &LR) -> Result<Self::NumericDocValues<LR>>
    where
        LR: LeafReader,
    {
        DocValues::get_numeric(leaf_reader, &self.field)
    }
}

pub(crate) enum IndexSorterEnum {
    Int(IntSorter<NumericDocValuesProviderImpl>),
    Long(LongSorter<NumericDocValuesProviderImpl>),
    Double(DoubleSorter<NumericDocValuesProviderImpl>),
    Float(FloatSorter<NumericDocValuesProviderImpl>),
}

pub struct Provider;
impl Provider {
    /// The name this Provider is registered under.
    pub const NAME: &'static str = "SortField";
}
impl SortFieldProvider for Provider {
    fn read_sort_field(&self, data_input: &mut impl DataInput) -> Result<SortFieldEnum> {
        let field_name = data_input.read_string()?;
        let field_type = SortFieldType::read_type(data_input)?;
        let reverse = data_input.read_int()? == 1;
        let mut sort_field = SortField::with_reverse(Some(field_name), field_type, reverse)?;
        if data_input.read_int()? == 1 {
            match sort_field.type_ {
                SortFieldType::String => {
                    let missing_string = data_input.read_int()?;
                    match missing_string {
                        1 => sort_field.set_missing_value(Some(MissingValueEnum::StringFirst))?,
                        _ => sort_field.set_missing_value(Some(MissingValueEnum::StringLast))?,
                    }
                },
                SortFieldType::Int => {
                    let value = data_input.read_int()?;
                    sort_field.set_missing_value(Some(MissingValueEnum::Int(value)))?;
                },
                SortFieldType::Long => {
                    let value = data_input.read_long()?;
                    sort_field.set_missing_value(Some(MissingValueEnum::Long(value)))?;
                },
                SortFieldType::Float => {
                    let value = NumericUtils::sortable_int_to_float(data_input.read_int()?);
                    sort_field.set_missing_value(Some(MissingValueEnum::Float(value)))?;
                },
                SortFieldType::Double => {
                    let value = NumericUtils::sortable_long_to_double(data_input.read_long()?);
                    sort_field.set_missing_value(Some(MissingValueEnum::Double(value)))?;
                },
                SortFieldType::Custom
                | SortFieldType::Doc
                | SortFieldType::Rewritable
                | SortFieldType::StringVal
                | SortFieldType::Score => {
                    return Err(LuceneError::illegal_argument(format!(
                        "Cannot deserialize sort of type {:?}",
                        sort_field.type_
                    )));
                },
            }
        }

        Ok(SortFieldEnum::Sorter(sort_field))
    }

    fn write_sort_field(&self, sf: &SortFieldEnum, output: &mut impl DataOutput) -> Result<()> {
        sf.serialize(output)
    }
}

/// Specifies the type of the terms to be sorted, or special types such as
/// `CUSTOM`.
#[derive(Debug, PartialEq, Eq, Clone, Copy, Hash)]
pub enum SortFieldType {
    /// Sort by document score (relevance). Sort values are `f32` and higher
    /// values are at the front.
    Score,

    /// Sort by document number (index order). Sort values are `i32` and lower
    /// values are at the front.
    Doc,

    /// Sort using term values as `String`. Sort values are `String` and lower
    /// values are at the front.
    String,

    /// Sort using term values as encoded `i32`. Sort values are `i32` and
    /// lower values are at the front. Fields must either be not indexed or
    /// indexed with `IntPoint`.
    Int,

    /// Sort using term values as encoded `f32`. Sort values are `f32` and
    /// lower values are at the front. Fields must either be not indexed or
    /// indexed with `FloatPoint`.
    Float,

    /// Sort using term values as encoded `i64`. Sort values are `i64` and
    /// lower values are at the front. Fields must either be not indexed or
    /// indexed with `LongPoint`.
    Long,

    /// Sort using term values as encoded `f64`. Sort values are `f64` and
    /// lower values are at the front. Fields must either be not indexed or
    /// indexed with `DoublePoint`.
    Double,

    /// Sort using a custom comparator. Sort values are any `Comparable` and
    /// sorting is done according to natural order.
    Custom,

    /// Sort using term values as `String`, but comparing by value (using
    /// `String::cmp`) for all comparisons. This is typically slower than
    /// `STRING`, which uses ordinals to do the sorting.
    StringVal,

    /// Force rewriting of `SortField` using `SortField::rewrite` before it can
    /// be used for sorting.
    Rewritable,
}
impl SortFieldType {
    pub fn value_of(type_str: &str) -> Result<Self> {
        match type_str {
            "Score" => Ok(SortFieldType::Score),
            "Doc" => Ok(SortFieldType::Doc),
            "String" => Ok(SortFieldType::String),
            "Int" => Ok(SortFieldType::Int),
            "Float" => Ok(SortFieldType::Float),
            "Long" => Ok(SortFieldType::Long),
            "Double" => Ok(SortFieldType::Double),
            "Custom" => Ok(SortFieldType::Custom),
            "StringVal" => Ok(SortFieldType::StringVal),
            "Rewritable" => Ok(SortFieldType::Rewritable),
            _ => Err(LuceneError::illegal_argument(format!(
                "Can't deserialize SortField - unknown type {type_str}"
            ))),
        }
    }
    pub fn read_type<DI>(input: &mut DI) -> Result<Self>
    where
        DI: DataInput,
    {
        let type_str = input.read_string()?;
        SortFieldType::value_of(&type_str)
    }
}
impl Display for SortFieldType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SortFieldType::Score => write!(f, "Score"),
            SortFieldType::Doc => write!(f, "Doc"),
            SortFieldType::String => write!(f, "String"),
            SortFieldType::Int => write!(f, "Int"),
            SortFieldType::Float => write!(f, "Float"),
            SortFieldType::Long => write!(f, "Long"),
            SortFieldType::Double => write!(f, "Double"),
            SortFieldType::Custom => write!(f, "Custom"),
            SortFieldType::StringVal => write!(f, "StringVal"),
            SortFieldType::Rewritable => write!(f, "Rewritable"),
        }
    }
}

#[derive(Clone)]
pub enum MissingValueEnum {
    /// Pass this to `setMissingValue` to have missing string values sort
    /// first.  */
    StringFirst,
    /// Pass this to `setMissingValue` to have missing string values sort last.
    ///  */
    StringLast,
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
}
impl MissingValueEnum {
    pub fn as_i32(&self) -> i32 {
        match self {
            MissingValueEnum::Int(v) => *v,
            _ => unreachable!("should not be here"),
        }
    }

    pub fn as_i64(&self) -> i64 {
        match self {
            MissingValueEnum::Long(v) => *v,
            _ => unreachable!("should not be here"),
        }
    }

    pub fn as_f32(&self) -> f32 {
        match self {
            MissingValueEnum::Float(v) => *v,
            _ => unreachable!("should not be here"),
        }
    }

    pub fn as_f64(&self) -> f64 {
        match self {
            MissingValueEnum::Double(v) => *v,
            _ => unreachable!("should not be here"),
        }
    }
}
impl PartialEq<Self> for MissingValueEnum {
    fn eq(&self, other: &Self) -> bool {
        match self {
            MissingValueEnum::StringFirst => {
                matches!(other, MissingValueEnum::StringFirst)
            },
            MissingValueEnum::StringLast => {
                matches!(other, MissingValueEnum::StringLast)
            },
            MissingValueEnum::Int(val) => {
                if let MissingValueEnum::Int(other_val) = other {
                    *val == *other_val
                } else {
                    false
                }
            },
            MissingValueEnum::Long(val) => {
                if let MissingValueEnum::Long(other_val) = other {
                    *val == *other_val
                } else {
                    false
                }
            },
            MissingValueEnum::Float(val) => {
                if let MissingValueEnum::Float(other_val) = other {
                    // In Rust Lucene,
                    // negative Float::NAN and positive Float::NAN are
                    // considered the smallest and largest floating-point
                    // values, respectively.
                    // However, we need to stay consistent with Java Lucene,
                    // where Float::NAN, regardless of its sign,
                    // is always treated as the largest floating-point value.
                    NumericUtils::float_to_sortable_int(*val)
                        == NumericUtils::float_to_sortable_int(*other_val)
                } else {
                    false
                }
            },
            MissingValueEnum::Double(val) => {
                if let MissingValueEnum::Double(other_val) = other {
                    // In Rust Lucene,
                    // negative Double::NAN and positive Double::NAN are
                    // considered the smallest and largest floating-point
                    // values, respectively.
                    // However, we need to stay consistent with Java Lucene,
                    // where Double::NAN, regardless of its sign,
                    // is always treated as the largest floating-point value.
                    NumericUtils::double_to_sortable_long(*val)
                        == NumericUtils::double_to_sortable_long(*other_val)
                } else {
                    false
                }
            },
        }
    }
}

impl Eq for MissingValueEnum {}

impl Display for MissingValueEnum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MissingValueEnum::StringFirst => {
                write!(f, "SortField.STRING_FIRST")
            },
            MissingValueEnum::StringLast => write!(f, "SortField.STRING_LAST"),
            MissingValueEnum::Int(val) => write!(f, "SortField.INT({val})"),
            MissingValueEnum::Long(val) => write!(f, "SortField.LONG({val})"),
            MissingValueEnum::Float(val) => {
                write!(f, "SortField.FLOAT({val})")
            },
            MissingValueEnum::Double(val) => {
                write!(f, "SortField.DOUBLE({val})")
            },
        }
    }
}
impl Hash for MissingValueEnum {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            MissingValueEnum::StringFirst => "SortField.STRING_FIRST".hash(state),
            MissingValueEnum::StringLast => "SortField.STRING_LAST".hash(state),
            MissingValueEnum::Int(val) => {
                "SortField.INT".hash(state);
                val.hash(state);
            },
            MissingValueEnum::Long(val) => {
                "SortField.LONG".hash(state);
                val.hash(state);
            },
            MissingValueEnum::Float(val) => {
                "SortField.FLOAT".hash(state);
                NumericUtils::float_to_sortable_int(*val).hash(state);
            },
            MissingValueEnum::Double(val) => {
                "SortField.DOUBLE".hash(state);
                NumericUtils::double_to_sortable_long(*val).hash(state);
            },
        }
    }
}

pub struct SortedDocValuesProviderImpl {
    field: String,
}
impl SortedDocValuesProviderImpl {
    pub fn new(field: String) -> Self {
        SortedDocValuesProviderImpl { field }
    }
}
impl SortedDocValuesProvider for SortedDocValuesProviderImpl {
    type SortedDocValues<LR>
        = Either2SortedDocValues<LR::SortedDocValues, EmptySorted>
    where
        LR: LeafReader;

    fn get<LR>(&self, leaf_reader: &LR) -> Result<Self::SortedDocValues<LR>>
    where
        LR: LeafReader,
    {
        DocValues::get_sorted(leaf_reader, self.field.as_str())
    }
}
pub struct NumericDocValuesProviderImpl1 {
    field: String,
}
impl NumericDocValuesProviderImpl1 {
    pub fn new(field: String) -> Self {
        NumericDocValuesProviderImpl1 { field }
    }
}
impl NumericDocValuesProvider for NumericDocValuesProviderImpl1 {
    type NumericDocValues<LR>
        = Either2NumericDocValues<LR::NumericDocValues, EmptyNumeric>
    where
        LR: LeafReader;

    fn get<LR>(&self, leaf_reader: &LR) -> Result<Self::NumericDocValues<LR>>
    where
        LR: LeafReader,
    {
        DocValues::get_numeric(leaf_reader, &self.field)
    }
}
pub enum IndexSorterEnumSorter {
    String(StringSorter<SortedDocValuesProviderImpl>),
    Int(IntSorter<NumericDocValuesProviderImpl1>),
    Long(LongSorter<NumericDocValuesProviderImpl1>),
    Double(DoubleSorter<NumericDocValuesProviderImpl1>),
    Float(FloatSorter<NumericDocValuesProviderImpl1>),
}
impl IndexSorter for IndexSorterEnumSorter {
    fn get_provider_name(&self) -> &str {
        match self {
            IndexSorterEnumSorter::String(_) => "SortedDocValuesProviderImpl",
            IndexSorterEnumSorter::Int(_) => "NumericDocValuesProviderImpl1",
            IndexSorterEnumSorter::Long(_) => "NumericDocValuesProviderImpl1",
            IndexSorterEnumSorter::Double(_) => "NumericDocValuesProviderImpl1",
            IndexSorterEnumSorter::Float(_) => "NumericDocValuesProviderImpl1",
        }
    }

    type DocComparator = DocComparatorEnum;

    fn get_doc_comparator<LR>(&self, leaf_reader: &LR, max_doc: i32) -> Result<Self::DocComparator>
    where
        LR: LeafReader,
    {
        match self {
            IndexSorterEnumSorter::Int(i) => Ok(DocComparatorEnum::Int(
                i.get_doc_comparator(leaf_reader, max_doc)?,
            )),
            IndexSorterEnumSorter::Long(l) => Ok(DocComparatorEnum::Long(
                l.get_doc_comparator(leaf_reader, max_doc)?,
            )),
            IndexSorterEnumSorter::Double(d) => Ok(DocComparatorEnum::Double(
                d.get_doc_comparator(leaf_reader, max_doc)?,
            )),
            IndexSorterEnumSorter::Float(f) => Ok(DocComparatorEnum::Float(
                f.get_doc_comparator(leaf_reader, max_doc)?,
            )),
            IndexSorterEnumSorter::String(s) => Ok(DocComparatorEnum::String(
                s.get_doc_comparator(leaf_reader, max_doc)?,
            )),
        }
    }
}

pub trait SortFiledBase: Display {
    /// Set the value to use for documents that don't have a value.
    fn set_missing_value(&mut self, missing_value: Option<MissingValueEnum>) -> Result<()>;
    /// Whether the relevance score is needed to sort documents.
    fn needs_scores(&self) -> bool;
    type IndexSort: IndexSorter;
    /// Returns an [`IndexSorter`] used for sorting index segments by this `SortField`.
    ///
    /// If this `SortField` cannot be used for index sorting (for example, if it uses scores or other
    /// query-dependent values), returns `None`.
    ///
    /// SortFields that implement this method should also implement a companion [`SortFieldProvider`] to
    /// serialize and deserialize the sort in index segment headers.
    fn get_index_sorter(&self) -> Result<Option<Self::IndexSort>>;
    fn serialize(&self, out: &mut impl DataOutput) -> Result<()>;
    type FieldComparator: FieldComparator;
    fn get_comparator(&self, num_hits: usize, pruning: Pruning) -> Result<Self::FieldComparator> {
        todo!()
    }
}
