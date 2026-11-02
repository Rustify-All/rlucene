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
use crate::core::index::index_sorter::ComparableProvidersVisitor;
use crate::core::search::similarities_impl::similarities::Similarity;
use std::fmt::Write as _;

use crate::core::index::doc_values::SortedDocValuesWithEmpty;
use crate::core::index::doc_values::{DocValues, NumericDocValuesWithEmpty};
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_sorter::{
  CPEnumType1, ComparableProviderEnum5, DocComparatorImpl, DoubleSorter, FloatSorter, IndexSorter,
  IntSorter, LongSorter, NumericDocValuesProvider, SortedDocValuesProvider, StringSorter,
};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::ordinal_map::OrdinalMap;
use crate::core::index::sort_field_provider::SortFieldProvider;
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
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::pruning::Pruning;
use crate::core::search::sort_field_enum::SortFieldEnum;
use crate::core::store::{DataInput, DataOutput};
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind, Result};
use crate::core::util::numeric_utils::NumericUtils;
use crate::impl_from_for_enum;
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
  /// - `field`: Name of the field to sort by. Can be `None` if `field_type`
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
  /// - `field`: Name of the field to sort by. Can be `None` if `field_type`
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
  pub fn with_reverse<T>(field: Option<T>, field_type: SortFieldType, reverse: bool) -> Result<Self>
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
  /// - `field`: Name of the field to sort by.
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
  /// - `field`: Name of the field to sort by.
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
  pub fn get_field_score() -> Result<Self> {
    SortField::new::<String>(None, SortFieldType::Score)
  }
  /// Represents sorting by document number (index order).
  pub fn get_field_doc() -> Result<Self> {
    SortField::new::<String>(None, SortFieldType::Doc)
  }
  // Sets field & type, and ensures field is `Some` unless
  // type is SCORE or DOC
  fn init_field_type(field: Option<String>, field_type: SortFieldType) -> Result<Self> {
    if field.is_none() && field_type != SortFieldType::Score && field_type != SortFieldType::Doc {
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
  pub fn get_field(&self) -> Option<&str> {
    self.field.as_deref()
  }
  /// Returns the type of contents in the field.
  ///
  /// # Returns
  /// One of the constants: `SCORE`, `DOC`, `STRING`, `INT`, or `FLOAT`.
  pub fn get_type(&self) -> SortFieldType {
    self.type_
  }
  /// Returns whether the sort should be reversed.
  ///
  /// # Returns
  /// `true` if natural order should be reversed.
  pub fn get_reverse(&self) -> bool {
    self.reverse
  }
  /// Enables/disables numeric sort optimization to use the indexed data.
  ///
  /// Enabled by default. By default, sorting on a numeric field activates point sort optimization
  /// that can efficiently skip over non-competitive hits. Sort optimization has a number of
  /// requirements, one of which is that [`SortFieldType`] matches the Point type with which the field
  /// was indexed (e.g. sort on IntPoint field should use [`SortFieldType::Int`]). Another requirement
  /// is that the same data is indexed with points and doc values for the field.
  ///
  /// By default, sorting on a SORTED(_SET) field activates sort optimization that can efficiently
  /// skip over non-competitive hits. Sort optimization requires that the same data is indexed with
  /// term index and doc values for the field.
  ///
  /// # Parameters
  /// - `optimizeSortWithIndexedData`: providing `false` disables the optimization, in cases where
  ///   these requirements can't be met.
  ///
  /// # Deprecated
  /// Should only be used for compatibility with 8.x indices that got created with inconsistent data
  /// across fields, or the wrong sort configuration in the index sort.
  pub fn set_optimize_sort_with_indexed_data(&mut self, optimize_sort_with_indexed_data: bool) {
    self.optimize_sort_with_indexed_data = optimize_sort_with_indexed_data
  }
  ///  Returns whether sort optimization should be optimized with points index
  pub fn get_optimize_sort_with_indexed_data(&self) -> bool {
    self.optimize_sort_with_indexed_data
  }
  /// Enables/disables numeric sort optimization to use the Points index.
  ///
  /// Enabled by default. By default, sorting on a numeric field activates point sort optimization
  /// that can efficiently skip over non-competitive hits. Sort optimization has a number of
  /// requirements, one of which is that [`SortFieldType`] matches the Point type with which the field
  /// was indexed (e.g. sort on IntPoint field should use [`SortFieldType::Int`]). Another requirement
  /// is that the same data is indexed with points and doc values for the field.
  ///
  /// # Parameters
  /// - `optimize_sort_with_points`: providing `false` disables the optimization, in cases where these
  ///   requirements can't be met.
  ///
  /// # Deprecated
  /// Should only be used for compatibility with 8.x indices that got created with inconsistent data
  /// across fields, or the wrong sort configuration in the index sort.
  /// This is a duplicate method for [`SortField::set_optimize_sort_with_indexed_data`].
  pub fn set_optimize_sort_with_points(&mut self, optimize_sort_with_points: bool) {
    self.set_optimize_sort_with_indexed_data(optimize_sort_with_points);
  }

  /// Returns whether sort optimization should be optimized with points index.
  ///
  /// # Returns
  /// Whether sort optimization should be optimized with points index.
  ///
  /// # Deprecated
  /// This is a duplicate method for [`SortField::get_optimize_sort_with_indexed_data`].
  pub fn get_optimize_sort_with_points(&self) -> bool {
    self.get_optimize_sort_with_indexed_data()
  }
}
impl SortFiledBase for SortField {
  /// Set the value to use for documents that don't have a value.
  fn set_missing_value<T>(&mut self, missing_value: T) -> Result<()>
  where
    T: Into<MissingValueEnum>,
  {
    let missing_value = missing_value.into();
    match self.type_ {
      SortFieldType::String | SortFieldType::StringVal => match missing_value {
        MissingValueEnum::StringFirst | MissingValueEnum::StringLast => {
          self.missing_value = Some(missing_value);
          Ok(())
        },
        _ => Err(LuceneError::illegal_argument(
          "For STRING type, missing value must be either STRING_FIRST or STRING_LAST".to_string(),
        )),
      },
      SortFieldType::Int => match missing_value {
        MissingValueEnum::Int(_) => {
          self.missing_value = Some(missing_value);
          Ok(())
        },
        _ => Err(LuceneError::illegal_argument(
          "Missing values for Type.INT can only be of type MissingValueEnum::Int".to_string(),
        )),
      },
      SortFieldType::Long => match missing_value {
        MissingValueEnum::Long(_) => {
          self.missing_value = Some(missing_value);
          Ok(())
        },
        _ => Err(LuceneError::illegal_argument(
          "Missing values for Type.LONG can only be of type MissingValueEnum::Long".to_string(),
        )),
      },
      SortFieldType::Float => match missing_value {
        MissingValueEnum::Float(_) => {
          self.missing_value = Some(missing_value);
          Ok(())
        },
        _ => Err(LuceneError::illegal_argument(
          "Missing values for Type.FLOAT can only be of type MissingValueEnum::Float".to_string(),
        )),
      },
      SortFieldType::Double => match missing_value {
        MissingValueEnum::Double(_) => {
          self.missing_value = Some(missing_value);
          Ok(())
        },
        _ => Err(LuceneError::illegal_argument(
          "Missing values for Type.DOUBLE can only be of type MissingValueEnum::Double".to_string(),
        )),
      },
      _ => Err(LuceneError::illegal_argument(
        "Missing value only works for numeric or STRING types".to_string(),
      )),
    }
  }

  fn needs_scores(&self) -> bool {
    self.type_ == SortFieldType::Score
  }

  type IndexSort = IndexSorterEnumSorter;

  fn get_index_sorter(&self) -> Result<Option<Self::IndexSort>> {
    let make_get_value = || -> Result<NPImpl1> {
      let field = self
        .field
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("field is None"))?;
      Ok(NPImpl1::new(field.to_string()))
    };
    match self.type_ {
      SortFieldType::String => {
        let field = self
          .field
          .as_ref()
          .ok_or_else(|| LuceneError::illegal_state("doc values reader is None"))?;
        let v = SProviderImpl2::new(field.to_string());
        Ok(Some(IndexSorterEnumSorter::String(StringSorter::new(
          Provider::NAME,
          self.missing_value.clone(),
          self.reverse,
          v,
        ))))
      },
      SortFieldType::Int => Ok(Some(IndexSorterEnumSorter::Int(IntSorter::new(
        Provider::NAME,
        self.missing_value.clone(),
        self.reverse,
        make_get_value()?,
      )?))),
      SortFieldType::Long => Ok(Some(IndexSorterEnumSorter::Long(LongSorter::new(
        Provider::NAME,
        self.missing_value.clone(),
        self.reverse,
        make_get_value()?,
      )?))),
      SortFieldType::Double => Ok(Some(IndexSorterEnumSorter::Double(DoubleSorter::new(
        Provider::NAME,
        self.missing_value.clone(),
        self.reverse,
        make_get_value()?,
      )?))),
      SortFieldType::Float => Ok(Some(IndexSorterEnumSorter::Float(FloatSorter::new(
        Provider::NAME,
        self.missing_value.clone(),
        self.reverse,
        make_get_value()?,
      )?))),
      _ => Ok(None),
    }
  }

  fn serialize<DO>(&self, out: &mut DO) -> Result<()>
  where
    DO: DataOutput,
  {
    let field = self
      .field
      .as_deref()
      .ok_or_else(|| LuceneError::illegal_state("field is required when serializing SortField"))?;
    out.write_string(field)?;
    out.write_string(self.type_.as_ref())?;
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
    let field_name = || {
      self
        .field
        .clone()
        .ok_or_else(|| LuceneError::illegal_state("field is required for this sort type"))
    };
    let mut field_comparator: FieldComparatorEnum = match self.type_ {
      SortFieldType::Score => RelevanceComparator::new(num_hits).into(),

      SortFieldType::Doc => DocComparator::new(num_hits, self.reverse, pruning).into(),

      SortFieldType::Int => {
        let missing = match self.missing_value.as_ref() {
          Some(value) => Some(value.as_i32()?),
          None => None,
        };
        IntComparator::new(field_name()?, num_hits, missing, self.reverse, pruning).into()
      },

      SortFieldType::Float => {
        let missing = match self.missing_value.as_ref() {
          Some(value) => Some(value.as_f32()?),
          None => None,
        };
        FloatComparator::new(field_name()?, num_hits, missing, self.reverse, pruning).into()
      },

      SortFieldType::Long => {
        let missing = match self.missing_value.as_ref() {
          Some(value) => Some(value.as_i64()?),
          None => None,
        };
        LongComparator::new(field_name()?, num_hits, missing, self.reverse, pruning).into()
      },

      SortFieldType::Double => {
        let missing = match self.missing_value.as_ref() {
          Some(value) => Some(value.as_f64()?),
          None => None,
        };
        DoubleComparator::new(field_name()?, num_hits, missing, self.reverse, pruning).into()
      },

      SortFieldType::String => TermOrdValComparator::new(
        field_name()?,
        num_hits,
        matches!(self.missing_value, Some(MissingValueEnum::StringLast)),
        self.reverse,
        pruning,
      )
      .into(),

      SortFieldType::StringVal => TermValComparator::new(
        field_name()?,
        num_hits,
        matches!(self.missing_value, Some(MissingValueEnum::StringLast)),
      )
      .into(),

      SortFieldType::Custom => {
        return Err(LuceneError::unsupported_operation("not supported yet"));
      },

      SortFieldType::Rewritable => {
        return Err(
          LuceneErrorKind::IllegalState(
            "SortField needs to be rewritten through Sort.rewrite(..) and SortField.rewrite(..)"
              .into(),
          )
          .into(),
        );
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
    let field = self.field.as_deref().unwrap_or_default();
    let mut buffer = match self.type_ {
      SortFieldType::Score => {
        let mut buffer = String::new();
        buffer.push_str("<score>");
        buffer
      },
      SortFieldType::Doc => {
        let mut buffer = String::new();
        buffer.push_str("<doc>");
        buffer
      },
      SortFieldType::String => format!("<string: \"{field}\">"),
      SortFieldType::Int => format!("<int: \"{field}\">"),
      SortFieldType::Long => format!("<long: \"{field}\">"),
      SortFieldType::Float => format!("<float: \"{field}\">"),
      SortFieldType::Double => format!("<double: \"{field}\">"),
      SortFieldType::Custom => {
        let mut buffer = String::from("<custom: \"");
        buffer.push_str(field);
        buffer.push_str("\": ");
        if let Some(ref comparator) = self.comparator_source {
          buffer.push_str(&format!("{comparator}"));
        }
        buffer.push('>');
        buffer
      },
      SortFieldType::StringVal => format!("<string_val: \"{field}\">"),
      SortFieldType::Rewritable => format!("<rewriteable: \"{field}\">"),
    };
    if self.reverse {
      buffer.push('!');
    }
    if let Some(ref missing_value) = self.missing_value {
      buffer.push_str(" missingValue=");
      write!(buffer, "{missing_value}")?;
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
  fn hash<H>(&self, state: &mut H)
  where
    H: std::hash::Hasher,
  {
    self.field.hash(state);
    self.type_.hash(state);
    self.reverse.hash(state);
    self.comparator_source.hash(state);
    self.missing_value.hash(state);
  }
}
pub struct Provider;
impl Provider {
  /// The name this Provider is registered under.
  pub const NAME: &'static str = "SortField";
}
impl SortFieldProvider for Provider {
  fn read_sort_field<DI>(&self, data_input: &mut DI) -> Result<SortFieldEnum>
  where
    DI: DataInput,
  {
    let field_name = data_input.read_string()?.into_owned();
    let field_type = SortFieldType::read_type(data_input)?;
    let reverse = data_input.read_int()? == 1;
    let mut sort_field = SortField::with_reverse(Some(field_name), field_type, reverse)?;
    if data_input.read_int()? == 1 {
      match sort_field.type_ {
        SortFieldType::String => {
          let missing_string = data_input.read_int()?;
          match missing_string {
            1 => sort_field.set_missing_value(MissingValueEnum::StringFirst)?,
            _ => sort_field.set_missing_value(MissingValueEnum::StringLast)?,
          }
        },
        SortFieldType::Int => {
          let value = data_input.read_int()?;
          sort_field.set_missing_value(value)?;
        },
        SortFieldType::Long => {
          let value = data_input.read_long()?;
          sort_field.set_missing_value(value)?;
        },
        SortFieldType::Float => {
          let value = NumericUtils::sortable_int_to_float(data_input.read_int()?);
          sort_field.set_missing_value(value)?;
        },
        SortFieldType::Double => {
          let value = NumericUtils::sortable_long_to_double(data_input.read_long()?);
          sort_field.set_missing_value(value)?;
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

    Ok(sort_field.into())
  }

  fn write_sort_field<DO>(&self, sf: &SortFieldEnum, output: &mut DO) -> Result<()>
  where
    DO: DataOutput,
  {
    sf.serialize(output)
  }
}

/// Specifies the type of the terms to be sorted, or special types such as
/// [`Self::Custom`].
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
  /// indexed with [`IntPoint`](crate::core::document::int_point::IntPoint).
  Int,

  /// Sort using term values as encoded `f32`. Sort values are `f32` and
  /// lower values are at the front. Fields must either be not indexed or
  /// indexed with [`FloatPoint`](crate::core::document::float_point::FloatPoint).
  Float,

  /// Sort using term values as encoded `i64`. Sort values are `i64` and
  /// lower values are at the front. Fields must either be not indexed or
  /// indexed with [`LongPoint`](crate::core::document::long_point::LongPoint).
  Long,

  /// Sort using term values as encoded `f64`. Sort values are `f64` and
  /// lower values are at the front. Fields must either be not indexed or
  /// indexed with [`DoublePoint`](crate::core::document::double_point::DoublePoint).
  Double,

  /// Sort using a custom comparator. Sort values are ordered by the comparator's Rust
  /// [`Ord`] semantics.
  Custom,

  /// Sort using term values as `String`, but comparing by value (using [`str::cmp`]) for all
  /// comparisons. This is typically slower than [`Self::String`], which uses ordinals.
  StringVal,

  /// Force rewriting of [`SortField`] using [`SortField::rewrite`] before it can
  /// be used for sorting.
  Rewritable,
}
impl SortFieldType {
  /// Resolves the enum name persisted in segment sort metadata.
  pub fn value_of(type_str: &str) -> Result<Self> {
    match type_str {
      "SCORE" => Ok(SortFieldType::Score),
      "DOC" => Ok(SortFieldType::Doc),
      "STRING" => Ok(SortFieldType::String),
      "INT" => Ok(SortFieldType::Int),
      "FLOAT" => Ok(SortFieldType::Float),
      "LONG" => Ok(SortFieldType::Long),
      "DOUBLE" => Ok(SortFieldType::Double),
      "CUSTOM" => Ok(SortFieldType::Custom),
      "STRING_VAL" => Ok(SortFieldType::StringVal),
      "REWRITEABLE" => Ok(SortFieldType::Rewritable),
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
impl AsRef<str> for SortFieldType {
  fn as_ref(&self) -> &str {
    match self {
      SortFieldType::Score => "SCORE",
      SortFieldType::Doc => "DOC",
      SortFieldType::String => "STRING",
      SortFieldType::Int => "INT",
      SortFieldType::Float => "FLOAT",
      SortFieldType::Long => "LONG",
      SortFieldType::Double => "DOUBLE",
      SortFieldType::Custom => "CUSTOM",
      SortFieldType::StringVal => "STRING_VAL",
      SortFieldType::Rewritable => "REWRITEABLE",
    }
  }
}

impl Display for SortFieldType {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_ref())
  }
}

#[derive(Clone)]
pub enum MissingValueEnum {
  /// Pass this to `setMissingValue` to have missing string values sort first.
  StringFirst,
  /// Pass this to `setMissingValue` to have missing string values sort last.
  StringLast,
  Int(i32),
  Long(i64),
  Float(f32),
  Double(f64),
}
impl_from_for_enum!(
    MissingValueEnum,
    i32 => Int,
    i64 => Long,
    f32 => Float,
    f64 => Double,
);

impl MissingValueEnum {
  pub fn as_i32(&self) -> Result<i32> {
    match self {
      MissingValueEnum::Int(v) => Ok(*v),
      _ => Err(LuceneError::illegal_state(format!(
        "missing value {self} is not an i32"
      ))),
    }
  }

  pub fn as_i64(&self) -> Result<i64> {
    match self {
      MissingValueEnum::Long(v) => Ok(*v),
      _ => Err(LuceneError::illegal_state(format!(
        "missing value {self} is not an i64"
      ))),
    }
  }

  pub fn as_f32(&self) -> Result<f32> {
    match self {
      MissingValueEnum::Float(v) => Ok(*v),
      _ => Err(LuceneError::illegal_state(format!(
        "missing value {self} is not an f32"
      ))),
    }
  }

  pub fn as_f64(&self) -> Result<f64> {
    match self {
      MissingValueEnum::Double(v) => Ok(*v),
      _ => Err(LuceneError::illegal_state(format!(
        "missing value {self} is not an f64"
      ))),
    }
  }
}
impl PartialEq for MissingValueEnum {
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
          // NaN sorts after all other values, regardless of its sign.
          NumericUtils::float_to_sortable_int(*val)
            == NumericUtils::float_to_sortable_int(*other_val)
        } else {
          false
        }
      },
      MissingValueEnum::Double(val) => {
        if let MissingValueEnum::Double(other_val) = other {
          // NaN sorts after all other values, regardless of its sign.
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
  fn hash<H>(&self, state: &mut H)
  where
    H: std::hash::Hasher,
  {
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

pub struct SProviderImpl2 {
  field: String,
}
impl SProviderImpl2 {
  pub fn new<FName>(field: FName) -> Self
  where
    FName: Into<String>,
  {
    let field = field.into();
    SProviderImpl2 { field }
  }
}
impl SortedDocValuesProvider for SProviderImpl2 {
  type SortedDocValues<LR>
    = SortedDocValuesWithEmpty<LR::SortedDocValues>
  where
    LR: LeafReader;

  fn get<LR>(&self, leaf_reader: &LR) -> Result<Self::SortedDocValues<LR>>
  where
    LR: LeafReader,
  {
    DocValues::get_sorted(leaf_reader, self.field.as_str())
  }
}
pub struct NPImpl1 {
  field: String,
}
impl NPImpl1 {
  pub fn new<FName>(field: FName) -> Self
  where
    FName: Into<String>,
  {
    let field = field.into();
    NPImpl1 { field }
  }
}
pub type NDVType<LR> = NumericDocValuesWithEmpty<<LR as LeafReader>::NumericDocValues>;
impl NumericDocValuesProvider for NPImpl1 {
  type NumericDocValues<LR>
    = NDVType<LR>
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
  String(StringSorter<SProviderImpl2>),
  Int(IntSorter<NPImpl1>),
  Long(LongSorter<NPImpl1>),
  Double(DoubleSorter<NPImpl1>),
  Float(FloatSorter<NPImpl1>),
}
impl IndexSorter for IndexSorterEnumSorter {
  fn get_provider_name(&self) -> &str {
    Provider::NAME
  }

  type ComparableProvider<LR>
    = CPEnumType1<NPImpl1, LR, SProviderImpl2>
  where
    LR: LeafReader;

  fn visit_comparable_providers<LR, V>(&self, readers: &[LR], visitor: V) -> Result<V::Output>
  where
    LR: LeafReader,
    V: ComparableProvidersVisitor,
  {
    match self {
      Self::Int(sorter) => sorter.visit_comparable_providers(readers, visitor),
      Self::Long(sorter) => sorter.visit_comparable_providers(readers, visitor),
      Self::Double(sorter) => sorter.visit_comparable_providers(readers, visitor),
      Self::Float(sorter) => sorter.visit_comparable_providers(readers, visitor),
      Self::String(sorter) => sorter.visit_comparable_providers(readers, visitor),
    }
  }

  fn get_comparable_providers<LR>(
    &self,
    readers: &[LR],
  ) -> Result<Vec<Self::ComparableProvider<LR>>>
  where
    LR: LeafReader,
  {
    Ok(match self {
      Self::Int(sorter) => sorter
        .get_comparable_providers(readers)?
        .into_iter()
        .map(ComparableProviderEnum5::Int)
        .collect(),
      Self::Long(sorter) => sorter
        .get_comparable_providers(readers)?
        .into_iter()
        .map(ComparableProviderEnum5::Long)
        .collect(),
      Self::Double(sorter) => sorter
        .get_comparable_providers(readers)?
        .into_iter()
        .map(ComparableProviderEnum5::Double)
        .collect(),
      Self::Float(sorter) => sorter
        .get_comparable_providers(readers)?
        .into_iter()
        .map(ComparableProviderEnum5::Float)
        .collect(),
      Self::String(sorter) => sorter
        .get_comparable_providers(readers)?
        .into_iter()
        .map(ComparableProviderEnum5::String)
        .collect(),
    })
  }

  fn get_comparable_providers_per_reader<LR>(
    &self,
    reader: &LR,
    reader_index: usize,
    formated_missing_value: &MissingValueEnum,
    ordinal_map: Option<&OrdinalMap>,
  ) -> Result<Self::ComparableProvider<LR>>
  where
    LR: LeafReader,
  {
    match self {
      IndexSorterEnumSorter::Int(v) => Ok(ComparableProviderEnum5::Int(
        v.get_comparable_providers_per_reader(reader, reader_index, formated_missing_value, None)?,
      )),
      IndexSorterEnumSorter::Long(v) => Ok(ComparableProviderEnum5::Long(
        v.get_comparable_providers_per_reader(reader, reader_index, formated_missing_value, None)?,
      )),
      IndexSorterEnumSorter::Double(v) => Ok(ComparableProviderEnum5::Double(
        v.get_comparable_providers_per_reader(reader, reader_index, formated_missing_value, None)?,
      )),
      IndexSorterEnumSorter::Float(v) => Ok(ComparableProviderEnum5::Float(
        v.get_comparable_providers_per_reader(reader, reader_index, formated_missing_value, None)?,
      )),
      IndexSorterEnumSorter::String(v) => Ok(ComparableProviderEnum5::String(
        v.get_comparable_providers_per_reader(
          reader,
          reader_index,
          formated_missing_value,
          ordinal_map,
        )?,
      )),
    }
  }

  fn get_ordinal_map<LR>(&self, _readers: &[LR]) -> Result<Option<OrdinalMap>>
  where
    LR: LeafReader,
  {
    match self {
      IndexSorterEnumSorter::String(s) => s.get_ordinal_map(_readers),
      _ => Ok(None),
    }
  }

  fn get_missing_value(&self) -> MissingValueEnum {
    match self {
      IndexSorterEnumSorter::Int(i) => i.get_missing_value(),
      IndexSorterEnumSorter::Long(l) => l.get_missing_value(),
      IndexSorterEnumSorter::Double(d) => d.get_missing_value(),
      IndexSorterEnumSorter::Float(f) => f.get_missing_value(),
      IndexSorterEnumSorter::String(s) => s.get_missing_value(),
    }
  }

  type DocComparator = DocComparatorImpl;

  fn get_doc_comparator<LR>(&self, leaf_reader: &LR, max_doc: i32) -> Result<Self::DocComparator>
  where
    LR: LeafReader,
  {
    match self {
      IndexSorterEnumSorter::Int(i) => Ok(DocComparatorImpl::Int(
        i.get_doc_comparator(leaf_reader, max_doc)?,
      )),
      IndexSorterEnumSorter::Long(l) => Ok(DocComparatorImpl::Long(
        l.get_doc_comparator(leaf_reader, max_doc)?,
      )),
      IndexSorterEnumSorter::Double(d) => Ok(DocComparatorImpl::Double(
        d.get_doc_comparator(leaf_reader, max_doc)?,
      )),
      IndexSorterEnumSorter::Float(f) => Ok(DocComparatorImpl::Float(
        f.get_doc_comparator(leaf_reader, max_doc)?,
      )),
      IndexSorterEnumSorter::String(s) => Ok(DocComparatorImpl::String(
        s.get_doc_comparator(leaf_reader, max_doc)?,
      )),
    }
  }
}

pub trait SortFiledBase: Display {
  /// Set the value to use for documents that don't have a value.
  fn set_missing_value<T>(&mut self, missing_value: T) -> Result<()>
  where
    T: Into<MissingValueEnum>;
  /// Rewrites this [`SortField`], returning a new [`SortField`] if a change is made.
  ///
  /// Implementations should provide this method to define their rewriting behavior when this
  /// [`SortField`] is of type [`SortFieldType::Rewritable`].
  ///
  /// # Arguments
  ///
  /// * `searcher` – The [`IndexSearcher`] used during rewriting.
  ///
  /// # Returns
  ///
  /// * `Some(SortField)` – if a rewritten sort field was created.
  /// * `None` – if no rewriting is needed and the current sort field should be kept as is.
  fn rewrite<IRC, SIM>(&self, _reader: &IndexSearcher<IRC, SIM>) -> Result<Option<SortFieldEnum>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext,
    IndexSearcher<IRC, SIM>: Sync,
  {
    Ok(None)
  }

  /// Whether the relevance score is needed to sort documents.
  fn needs_scores(&self) -> bool;
  type IndexSort: IndexSorter;
  /// Returns an [`IndexSorter`] used for sorting index segments by this [`SortField`].
  ///
  /// If this [`SortField`] cannot be used for index sorting (for example, if it uses scores or other
  /// query-dependent values), returns `None`.
  ///
  /// SortFields that implement this method should also implement a companion [`SortFieldProvider`] to
  /// serialize and deserialize the sort in index segment headers.
  fn get_index_sorter(&self) -> Result<Option<Self::IndexSort>>;
  fn serialize<DO>(&self, out: &mut DO) -> Result<()>
  where
    DO: DataOutput;
  type FieldComparator: FieldComparator;
  fn get_comparator(&self, num_hits: usize, pruning: Pruning) -> Result<Self::FieldComparator>;
}
