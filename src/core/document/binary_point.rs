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
use crate::core::analysis::analyzer::Analyzer;
use crate::core::analysis::reader::ReaderEnum;
use crate::core::document::field::{Field, FieldBase, FieldDataEnum};
use crate::core::document::field_type::FieldType;
use crate::core::document::invertable_field::InvertableType;
use crate::core::index::BinaryValueEnum;
use crate::core::index::BytesRef;
use crate::core::index::indexable_field::{
  IndexableField, IndexingTokenStream, ReusedIndexingTokenStream,
};
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::search::match_no_docs_query::MatchNoDocsQuery;
use crate::core::search::point_in_set_query::{PointInSetBase, PointInSetQuery};
#[cfg(debug_assertions)]
use crate::core::search::point_range_query::check_args;
use crate::core::search::point_range_query::{PointRangeBase, PointRangeQuery};
use crate::core::search::query::Query;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::number::Number;
use std::borrow::Cow;
use std::fmt::Formatter;
use std::fmt::{self, Write};

/// An indexed binary field for fast range filters. If you also need to store the value, you should
/// add a separate [`StoredField`](crate::core::document::stored_field::StoredField) instance.
///
/// Finding all documents within an N-dimensional shape or range at search time is efficient.
/// Multiple values for the same field in one document is allowed.
///
/// This field defines static factory methods for creating common queries:
///
/// * [`new_exact_query`](Self::new_exact_query) for matching an exact 1D point.
/// * [`new_set_query`](Self::new_set_query) for matching a set of 1D values.
/// * [`new_range_query`](Self::new_range_query) for matching a 1D range.
/// * [`new_range_query_multi_dim`](Self::new_range_query_multi_dim) for matching points/ranges in
///   n-dimensional space.
///
/// See also [`PointValues`](crate::core::index::point_values::PointValues).
pub struct BinaryPoint {
  parent_field: Field,
}

impl BinaryPoint {
  /// General purpose API: creates a new [`BinaryPoint`], indexing the provided N-dimensional binary
  /// point.
  ///
  /// # Arguments
  ///
  /// * `name` - Field name.
  /// * `point` - Binary dimensions supplied as byte arrays, vectors, or slices.
  pub fn new<T, P, B>(name: T, point: P) -> Result<BinaryPoint>
  where
    T: Into<String>,
    P: AsRef<[B]>,
    B: AsRef<[u8]>,
  {
    let point = point.as_ref();
    let packed = Self::pack(point)?;
    let field_type = Self::get_type_from_dims(point)?;
    let parent_field = Field::from_bytes_ref(name, packed, field_type)?;
    Ok(BinaryPoint { parent_field })
  }

  /// Expert API.
  pub fn with_type<T, V>(name: T, packed_point: V, field_type: FieldType) -> Result<BinaryPoint>
  where
    T: Into<String>,
    V: Into<Vec<u8>>,
  {
    let packed_point = packed_point.into();
    let expected = field_type.point_dimension_count() * field_type.point_num_bytes();
    if packed_point.len() != expected {
      return Err(LuceneError::illegal_argument(format!(
        "packed_point has length={} but field_type.point_dimension_count()={} and field_type.point_num_bytes()={}",
        packed_point.len(),
        field_type.point_dimension_count(),
        field_type.point_num_bytes()
      )));
    }

    let parent_field = Field::from_binary(name, packed_point, field_type)?;
    Ok(BinaryPoint { parent_field })
  }

  fn get_type_from_dims<B>(point: &[B]) -> Result<FieldType>
  where
    B: AsRef<[u8]>,
  {
    if point.is_empty() {
      return Err(LuceneError::illegal_argument(
        "point must not be 0 dimensions".to_string(),
      ));
    }

    let Some((first_dim, remaining_dims)) = point.split_first() else {
      return Err(LuceneError::illegal_argument(
        "point must not be 0 dimensions".to_string(),
      ));
    };
    let first_dim = first_dim.as_ref();
    if first_dim.is_empty() {
      return Err(LuceneError::illegal_argument(
        "point must not have 0-length values".to_string(),
      ));
    }
    let bytes_per_dim = first_dim.len();

    for one_dim in remaining_dims {
      let one_dim = one_dim.as_ref();
      if one_dim.is_empty() {
        return Err(LuceneError::illegal_argument(
          "point must not have 0-length values".to_string(),
        ));
      }
      if bytes_per_dim != one_dim.len() {
        return Err(LuceneError::illegal_argument(format!(
          "all dimensions must have same bytes length; got {} and {}",
          bytes_per_dim,
          one_dim.len()
        )));
      }
    }

    Self::get_type(point.len(), bytes_per_dim)
  }

  fn get_type(num_dims: usize, bytes_per_dim: usize) -> Result<FieldType> {
    let mut ty = FieldType::new();
    ty.set_dimensions(num_dims, bytes_per_dim)?;
    ty.freeze();
    Ok(ty)
  }

  /// Packs dimensions supplied as byte arrays, vectors, or slices.
  pub fn pack<B>(point: &[B]) -> Result<BytesRef<Vec<u8>>>
  where
    B: AsRef<[u8]>,
  {
    if point.is_empty() {
      return Err(LuceneError::illegal_argument(
        "point must not be 0 dimensions".to_string(),
      ));
    }

    if point.len() == 1 {
      return Ok(BytesRef::from_bytes(point[0].as_ref().to_vec()));
    }

    let Some((first_dim, remaining_dims)) = point.split_first() else {
      return Err(LuceneError::illegal_argument(
        "point must not be 0 dimensions".to_string(),
      ));
    };
    let first_dim = first_dim.as_ref();
    if first_dim.is_empty() {
      return Err(LuceneError::illegal_argument(
        "point must not have 0-length values".to_string(),
      ));
    }
    let bytes_per_dim = first_dim.len();
    for d in remaining_dims {
      let d = d.as_ref();
      if d.is_empty() {
        return Err(LuceneError::illegal_argument(
          "point must not have 0-length values".to_string(),
        ));
      }
      if bytes_per_dim != d.len() {
        return Err(LuceneError::illegal_argument(format!(
          "all dimensions must have same bytes length; got {} and {}",
          bytes_per_dim,
          d.len()
        )));
      }
    }

    let mut packed = Vec::with_capacity(bytes_per_dim * point.len());
    for dim in point {
      packed.extend_from_slice(&dim.as_ref()[..bytes_per_dim]);
    }

    Ok(BytesRef::from_bytes(packed))
  }

  /// Create a query for matching an exact binary value.
  ///
  /// This is for simple one-dimension points. For multidimensional points, use
  /// [`new_range_query_multi_dim`](Self::new_range_query_multi_dim) instead.
  ///
  /// # Arguments
  ///
  /// * `field` - Field name.
  /// * `value` - Binary value.
  pub fn new_exact_query<T, V>(field: T, value: V) -> Result<PointRangeQuery>
  where
    T: Into<String>,
    V: Into<Vec<u8>>,
  {
    let value = value.into();
    Self::new_range_query(field, value.clone(), value)
  }

  /// Create a range query for binary values.
  ///
  /// This is for simple one-dimension ranges. For multidimensional ranges, use
  /// [`new_range_query_multi_dim`](Self::new_range_query_multi_dim) instead.
  ///
  /// # Arguments
  ///
  /// * `field` - Field name.
  /// * `lower` - Lower portion of the range (inclusive).
  /// * `upper` - Upper portion of the range (inclusive).
  pub fn new_range_query<T, L, U>(field: T, lower: L, upper: U) -> Result<PointRangeQuery>
  where
    T: Into<String>,
    L: Into<Vec<u8>>,
    U: Into<Vec<u8>>,
  {
    let field = field.into();
    let lower = lower.into();
    let upper = upper.into();
    #[cfg(debug_assertions)]
    check_args(&field, lower.as_ref(), upper.as_ref())?;
    PointRangeQuery::new(field, lower, upper, 1, BinaryPointRangeQuery)
  }

  /// Create a range query for n-dimensional binary values.
  ///
  /// # Arguments
  ///
  /// * `field` - Field name.
  /// * `lower` - Lower portion of the range (inclusive).
  /// * `upper` - Upper portion of the range (inclusive).
  pub fn new_range_query_multi_dim<T, L, U, LB, UB>(
    field: T,
    lower: L,
    upper: U,
  ) -> Result<PointRangeQuery>
  where
    T: Into<String>,
    L: AsRef<[LB]>,
    U: AsRef<[UB]>,
    LB: AsRef<[u8]>,
    UB: AsRef<[u8]>,
  {
    let lower = lower.as_ref();
    let upper = upper.as_ref();
    if lower.len() != upper.len() {
      return Err(LuceneError::illegal_argument(
        "lowerValue.length != upperValue.length".to_string(),
      ));
    }

    let field = field.into();
    let mut packed_lower = Self::pack(lower)?;
    let mut packed_upper = Self::pack(upper)?;

    #[cfg(debug_assertions)]
    check_args(&field, &packed_lower.bytes, &packed_upper.bytes)?;

    PointRangeQuery::new(
      field,
      packed_lower.take_bytes(),
      packed_upper.take_bytes(),
      lower.len(),
      BinaryPointRangeQuery,
    )
  }

  /// Create a query matching any of the specified 1D values. This is the points equivalent of
  /// [`TermInSetQuery`](crate::core::search::term_in_set_query::TermInSetQuery).
  ///
  /// # Arguments
  ///
  /// * `field` - Field name.
  /// * `values` - All values to match, as byte arrays, vectors, or slices.
  ///
  /// For an empty set, pass `Vec::<Vec<u8>>::new()`.
  pub fn new_set_query<T, V, B>(field: T, values: V) -> Result<Query>
  where
    T: Into<String>,
    V: AsRef<[B]>,
    B: AsRef<[u8]>,
  {
    let field = field.into();
    let values = values.as_ref();

    let mut bytes_per_dim = None;
    for value in values {
      let value = value.as_ref();
      match bytes_per_dim {
        None => bytes_per_dim = Some(value.len()),
        Some(bytes_per_dim) if value.len() != bytes_per_dim => {
          return Err(LuceneError::illegal_argument(format!(
            "all byte slices must be the same length, but saw {} and {}",
            bytes_per_dim,
            value.len()
          )));
        },
        _ => {},
      }
    }

    let Some(bytes_per_dim) = bytes_per_dim else {
      return Ok(MatchNoDocsQuery::with_reason("empty BinaryPoint.newSetQuery").into());
    };

    let mut sorted_values = smallvec::SmallVec::<[&[u8]; 4]>::with_capacity(values.len());
    sorted_values.extend(values.iter().map(|value| value.as_ref()));
    sorted_values.sort_unstable();

    Ok(
      PointInSetQuery::new(
        field,
        1,
        bytes_per_dim,
        BinaryPointSetBytesRefIterator::new(sorted_values),
        BinaryPointInSetQuery,
      )?
      .into(),
    )
  }
}

struct BinaryPointSetBytesRefIterator<'a> {
  sorted_values: smallvec::SmallVec<[&'a [u8]; 4]>,
  upto: usize,
}

impl<'a> BinaryPointSetBytesRefIterator<'a> {
  fn new(sorted_values: smallvec::SmallVec<[&'a [u8]; 4]>) -> Self {
    Self {
      sorted_values,
      upto: 0,
    }
  }
}

impl BytesRefIterator for BinaryPointSetBytesRefIterator<'_> {
  type Value<'a>
    = BytesRef<&'a [u8]>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    if self.upto == self.sorted_values.len() {
      Ok(None)
    } else {
      let bytes = self.sorted_values[self.upto];
      self.upto += 1;
      Ok(Some(BytesRef {
        bytes,
        offset: 0,
        length: bytes.len(),
      }))
    }
  }
}

impl FieldBase for BinaryPoint {
  fn set_bytes_value<'a, B>(&mut self, _value: B) -> Result<()>
  where
    B: Into<BinaryValueEnum<'a>>,
  {
    Err(LuceneError::illegal_argument(
      "cannot change value type from binary point to BytesRef".to_string(),
    ))
  }

  fn set_int_value(&mut self, _value: i32) -> Result<()> {
    Err(LuceneError::illegal_argument(
      "cannot set an i32 value on BinaryPoint".to_string(),
    ))
  }
}
impl IndexableField for BinaryPoint {
  fn name(&self) -> &str {
    self.parent_field.name()
  }

  type FieldType<'a>
    = &'a FieldType
  where
    Self: 'a;

  fn field_type(&self) -> Self::FieldType<'_> {
    self.parent_field.field_type()
  }
  fn token_stream<'a, A>(
    &'a mut self,
    analyzer: &'a A,
    reuse_token_stream: &'a mut Option<ReusedIndexingTokenStream>,
  ) -> Result<IndexingTokenStream<'a>>
  where
    A: Analyzer,
  {
    self.parent_field.token_stream(analyzer, reuse_token_stream)
  }

  fn binary_value(&self) -> Result<Option<BinaryValueEnum<'_>>> {
    self.parent_field.binary_value()
  }

  fn take_binary_value(&mut self) -> Result<Option<BytesRef<Vec<u8>>>> {
    self.parent_field.take_binary_value()
  }

  fn string_value(&self) -> Result<Option<Cow<'_, String>>> {
    self.parent_field.string_value()
  }

  fn take_string_value(&mut self) -> Result<Option<String>> {
    self.parent_field.take_string_value()
  }

  fn take_reader_value(&mut self) -> Result<Option<ReaderEnum>> {
    Ok(None)
  }

  fn numeric_value(&self) -> Result<Option<Number>> {
    Err(LuceneError::illegal_argument(
      "BinaryPoint has no numericValue".to_string(),
    ))
  }

  type StoredValue<'a>
    = &'a FieldDataEnum
  where
    Self: 'a;

  fn stored_value(&self) -> Result<Option<Self::StoredValue<'_>>> {
    self.parent_field.stored_value()
  }

  fn invertable_type(&self) -> &InvertableType {
    self.parent_field.invertable_type()
  }
}

impl fmt::Display for BinaryPoint {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    self.parent_field.fmt(f)
  }
}

#[derive(Debug, Clone)]
pub struct BinaryPointRangeQuery;

impl PointRangeBase for BinaryPointRangeQuery {
  fn to_string(&self, _dimension: usize, value: &[u8]) -> Result<String> {
    let mut out = String::from("binary(");
    for (i, b) in value.iter().enumerate() {
      if i > 0 {
        out.push(' ');
      }
      write!(out, "{:x}", b)?;
    }
    out.push(')');
    Ok(out)
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BinaryPointInSetQuery;

impl PointInSetBase for BinaryPointInSetQuery {
  fn to_string(&self, value: &[u8]) -> Result<String> {
    Ok(
      BytesRef {
        bytes: value,
        offset: 0,
        length: value.len(),
      }
      .to_string(),
    )
  }
}
