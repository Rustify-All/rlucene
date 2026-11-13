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
use crate::core::document::field::{FieldBase, FieldDataEnum};
use crate::core::document::field_type::FieldType;
use crate::core::document::invertable_field::InvertableType;
use crate::core::index::BinaryValueEnum;
use crate::core::index::BytesRef;
use crate::core::index::indexable_field::{
  IndexableField, IndexingTokenStream, ReusedIndexingTokenStream,
};
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::index::point_values::MAX_INDEX_DIMENSIONS;
use crate::core::search::point_in_set_query::{PointInSetBase, PointInSetQuery};
#[cfg(debug_assertions)]
use crate::core::search::point_range_query::check_args;
use crate::core::search::point_range_query::{PointRangeBase, PointRangeQuery};
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::number::Number;
use crate::core::util::numeric_utils::NumericUtils;
use std::borrow::Cow;
use std::fmt;

/// An indexed `i32` field for fast range filters. If you also need to store the value, you should
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
/// * [`new_range_query_n`](Self::new_range_query_n) for matching points/ranges in n-dimensional
///   space.
///
/// See also [`PointValues`](crate::core::index::point_values::PointValues).
pub struct IntPoint {
  name: Cow<'static, str>,
  field_type: FieldType,
  packed: Option<[u8; MAX_INDEX_DIMENSIONS * BitUtil::INT_BYTES]>,
}

impl IntPoint {
  /// Creates a new [`IntPoint`], indexing the provided N-dimensional integer point.
  /// Static names are borrowed; owned strings are moved. Convert a non-static
  /// borrowed name to an owned string before passing it to this constructor.
  ///
  /// # Arguments
  ///
  /// * `name` - Field name.
  /// * `point` - Integer point value.
  pub fn new<T, P>(name: T, point: P) -> Result<IntPoint>
  where
    T: Into<Cow<'static, str>>,
    P: AsRef<[i32]>,
  {
    let point = point.as_ref();
    if point.is_empty() {
      return Err(LuceneError::illegal_argument(
        "point must not be 0 dimensions".to_string(),
      ));
    }
    let field_type = Self::get_type(point.len())?;
    let mut bytes = [0; MAX_INDEX_DIMENSIONS * BitUtil::INT_BYTES];
    for (i, &dim) in point.iter().enumerate() {
      Self::encode_dimension(dim, &mut bytes, i * BitUtil::INT_BYTES);
    }
    Ok(Self {
      name: name.into(),
      field_type,
      packed: Some(bytes),
    })
  }

  fn get_type(num_dims: usize) -> Result<FieldType> {
    let mut field_type = FieldType::new();
    field_type.set_dimensions(num_dims, BitUtil::INT_BYTES)?;
    field_type.freeze();
    Ok(field_type)
  }

  /// Change the values of this field
  pub fn set_int_values<P>(&mut self, point: P) -> Result<()>
  where
    P: AsRef<[i32]>,
  {
    let point = point.as_ref();
    let dims = self.field_type.point_dimension_count();
    if dims != point.len() {
      return Err(LuceneError::illegal_argument(format!(
        "this field (name={}) uses {} dimensions; cannot change to (incoming) {} dimensions",
        self.name,
        dims,
        point.len()
      )));
    }
    #[expect(
      clippy::unnecessary_lazy_evaluations,
      reason = "Keep packed-buffer initialization on the first setter call"
    )]
    let bytes = self
      .packed
      .get_or_insert_with(|| [0; MAX_INDEX_DIMENSIONS * BitUtil::INT_BYTES]);
    for (i, &dim) in point.iter().enumerate() {
      Self::encode_dimension(dim, bytes, i * BitUtil::INT_BYTES);
    }
    Ok(())
  }

  /// Pack an integer point into a [`BytesRef`].
  ///
  /// # Arguments
  ///
  /// * `point` - Integer point value.
  ///
  /// # Errors
  ///
  /// Returns an error if the value has zero dimensions.
  pub fn pack<P>(point: P) -> Result<BytesRef<Vec<u8>>>
  where
    P: AsRef<[i32]>,
  {
    let point = point.as_ref();
    if point.is_empty() {
      return Err(LuceneError::illegal_argument(
        "point must not be 0 dimensions".to_string(),
      ));
    }
    let mut packed = Vec::with_capacity(point.len() * BitUtil::INT_BYTES);
    let mut encoded = [0; BitUtil::INT_BYTES];
    for &dim in point {
      Self::encode_dimension(dim, &mut encoded, 0);
      packed.extend_from_slice(&encoded);
    }
    Ok(BytesRef::from_bytes(packed))
  }

  /// Encode single int dimension
  pub fn encode_dimension(value: i32, dest: &mut [u8], offset: usize) {
    NumericUtils::int_to_sortable_bytes(value, dest, offset);
  }

  /// Decode single int dimension
  pub fn decode_dimension(value: &[u8], offset: usize) -> i32 {
    NumericUtils::sortable_bytes_to_int(value, offset)
  }

  /// Create a query for matching an exact integer value.
  ///
  /// This is for simple one-dimension points. For multidimensional points, use
  /// [`new_range_query_n`](Self::new_range_query_n) instead.
  ///
  /// # Arguments
  ///
  /// * `field` - Field name.
  /// * `value` - Exact value.
  pub fn new_exact_query<T>(field: T, value: i32) -> Result<PointRangeQuery>
  where
    T: Into<String>,
  {
    Self::new_range_query(field, value, value)
  }

  /// Create a range query for integer values.
  ///
  /// This is for simple one-dimension ranges. For multidimensional ranges, use
  /// [`new_range_query_n`](Self::new_range_query_n) instead.
  ///
  /// You can have half-open ranges (which are in fact `</<=` or `>/>=` queries) by setting
  /// `lower_value = i32::MIN` or `upper_value = i32::MAX`.
  ///
  /// Ranges are inclusive. For exclusive ranges, pass `lower_value + 1` or `upper_value - 1`.
  ///
  /// # Arguments
  ///
  /// * `field` - Field name.
  /// * `lower_value` - Lower portion of the range (inclusive).
  /// * `upper_value` - Upper portion of the range (inclusive).
  pub fn new_range_query<T>(field: T, lower_value: i32, upper_value: i32) -> Result<PointRangeQuery>
  where
    T: Into<String>,
  {
    Self::new_range_query_n(field, [lower_value], [upper_value])
  }
  /// Create a query matching any of the specified 1D values. This is the points equivalent of
  /// [`TermInSetQuery`](crate::core::search::term_in_set_query::TermInSetQuery).
  ///
  /// # Arguments
  ///
  /// * `field` - Field name.
  /// * `values` - All values to match.
  ///
  /// Owned vectors are sorted in place; borrowed slices are copied before sorting.
  pub fn new_set_query<T, V>(field: T, values: V) -> Result<PointInSetQuery>
  where
    T: Into<String>,
    V: Into<Vec<i32>>,
  {
    let mut sorted_values = values.into();
    sorted_values.sort_unstable();

    PointInSetQuery::new(
      field.into(),
      1,
      BitUtil::INT_BYTES,
      IntPointSetBytesRefIterator::new(sorted_values),
      IntPointInSetQuery,
    )
  }

  /// Create a range query for n-dimensional integer values.
  ///
  /// You can have half-open ranges (which are in fact `</<=` or `>/>=` queries) by setting
  /// `lower_value[i] = i32::MIN` or `upper_value[i] = i32::MAX`.
  ///
  /// Ranges are inclusive. For exclusive ranges, pass `lower_value[i] + 1` or
  /// `upper_value[i] - 1`.
  ///
  /// # Arguments
  ///
  /// * `field` - Field name.
  /// * `lower_value` - Lower portion of the range (inclusive).
  /// * `upper_value` - Upper portion of the range (inclusive).
  ///
  /// Lower and upper bounds may independently use arrays, vectors, or slices.
  pub fn new_range_query_n<T, L, U>(
    field: T,
    lower_value: L,
    upper_value: U,
  ) -> Result<PointRangeQuery>
  where
    T: Into<String>,
    L: AsRef<[i32]>,
    U: AsRef<[i32]>,
  {
    let field = field.into();
    let len = lower_value.as_ref().len();
    let mut lower_point = IntPoint::pack(lower_value)?;
    let mut upper_point = IntPoint::pack(upper_value)?;
    #[cfg(debug_assertions)]
    check_args(&field, &lower_point.bytes, &upper_point.bytes)?;
    PointRangeQuery::new(
      field,
      lower_point.take_bytes(),
      upper_point.take_bytes(),
      len,
      IntPointRangeQuery,
    )
  }
}

struct IntPointSetBytesRefIterator {
  sorted_values: Vec<i32>,
  upto: usize,
  encoded: BytesRef<[u8; BitUtil::INT_BYTES]>,
}

impl IntPointSetBytesRefIterator {
  fn new(sorted_values: Vec<i32>) -> Self {
    Self {
      sorted_values,
      upto: 0,
      encoded: BytesRef {
        bytes: [0u8; BitUtil::INT_BYTES],
        offset: 0,
        length: BitUtil::INT_BYTES,
      },
    }
  }
}

impl BytesRefIterator for IntPointSetBytesRefIterator {
  type Value<'a>
    = &'a BytesRef<[u8; BitUtil::INT_BYTES]>
  where
    Self: 'a;

  #[inline]
  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    if self.upto == self.sorted_values.len() {
      Ok(None)
    } else {
      IntPoint::encode_dimension(self.sorted_values[self.upto], &mut self.encoded.bytes, 0);
      self.upto += 1;
      Ok(Some(&self.encoded))
    }
  }
}

impl FieldBase for IntPoint {
  fn set_bytes_value<'a, B>(&mut self, _value: B) -> Result<()>
  where
    B: Into<BinaryValueEnum<'a>>,
  {
    Err(LuceneError::illegal_argument(
      "cannot change value type from int to BytesRef".to_string(),
    ))
  }

  fn set_int_value(&mut self, value: i32) -> Result<()> {
    self.set_int_values([value])
  }
}

impl IndexableField for IntPoint {
  fn name(&self) -> &str {
    &self.name
  }

  type FieldType<'a>
    = &'a FieldType
  where
    Self: 'a;

  fn field_type(&self) -> Self::FieldType<'_> {
    &self.field_type
  }
  fn token_stream<'a, A>(
    &'a mut self,
    _analyzer: &'a A,
    _reuse_token_stream: &'a mut Option<ReusedIndexingTokenStream>,
  ) -> Result<IndexingTokenStream<'a>>
  where
    A: Analyzer,
  {
    Ok(None)
  }

  #[inline]
  fn binary_value(&self) -> Result<Option<BinaryValueEnum<'_>>> {
    Ok(self.packed.as_ref().map(|bytes| {
      let length = self.field_type.point_dimension_count() * BitUtil::INT_BYTES;
      BinaryValueEnum::Slice(BytesRef {
        bytes: &bytes[..length],
        offset: 0,
        length,
      })
    }))
  }

  fn take_binary_value(&mut self) -> Result<Option<BytesRef<Vec<u8>>>> {
    let value = self.binary_value()?.map(BinaryValueEnum::into_owned);
    self.packed = None;
    Ok(value)
  }

  fn string_value(&self) -> Result<Option<Cow<'_, str>>> {
    Ok(None)
  }

  fn take_string_value(&mut self) -> Result<Option<String>> {
    Ok(None)
  }

  fn take_reader_value(&mut self) -> Result<Option<ReaderEnum>> {
    todo!()
  }

  fn numeric_value(&self) -> Result<Option<Number>> {
    let dims = self.field_type.point_dimension_count();
    if dims != 1 {
      return Err(LuceneError::illegal_state(format!(
        "this field (name={}) uses {} dimensions; cannot convert to a single numeric value",
        self.name, dims
      )));
    }
    match &self.packed {
      Some(bytes) => Ok(Some(Self::decode_dimension(bytes, 0).into())),
      _ => Err(LuceneError::illegal_argument(
        "Unsupported FieldDataEnum variant",
      )),
    }
  }

  type StoredValue<'a>
    = &'a FieldDataEnum
  where
    Self: 'a;

  fn stored_value(&self) -> Result<Option<Self::StoredValue<'_>>> {
    Ok(None)
  }

  fn invertable_type(&self) -> &InvertableType {
    &InvertableType::TokenStream
  }
}

impl fmt::Display for IntPoint {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{} <{}:", std::any::type_name::<Self>(), self.name)?;
    match &self.packed {
      Some(bytes) => {
        let dim_count = self.field_type.point_dimension_count();
        for dim in 0..dim_count {
          if dim > 0 {
            write!(f, ",")?;
          }
          let value = Self::decode_dimension(bytes, dim * BitUtil::INT_BYTES);
          write!(f, "{value}")?;
        }
      },
      _ => {
        debug_assert!(false, "no possible here");
        write!(f, "Unsupported FieldDataEnum variant")?;
      },
    }
    write!(f, ">")
  }
}

#[derive(Debug, Clone)]
pub struct IntPointRangeQuery;
impl PointRangeBase for IntPointRangeQuery {
  fn to_string(&self, _dimension: usize, value: &[u8]) -> Result<String> {
    Ok(IntPoint::decode_dimension(value, 0).to_string())
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IntPointInSetQuery;

impl PointInSetBase for IntPointInSetQuery {
  fn to_string(&self, value: &[u8]) -> Result<String> {
    debug_assert!(value.len() == BitUtil::INT_BYTES);
    Ok(IntPoint::decode_dimension(value, 0).to_string())
  }
}
