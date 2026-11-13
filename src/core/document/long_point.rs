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

pub struct LongPoint {
  name: Cow<'static, str>,
  field_type: FieldType,
  packed: Option<[u8; MAX_INDEX_DIMENSIONS * BitUtil::LONG_BYTES]>,
}

impl LongPoint {
  /// Creates a new [`LongPoint`] with the given name and `i64` values.
  /// Static names are borrowed; owned strings are moved. Convert a non-static
  /// borrowed name to an owned string before passing it to this constructor.
  pub fn new<T, P>(name: T, point: P) -> Result<LongPoint>
  where
    T: Into<Cow<'static, str>>,
    P: AsRef<[i64]>,
  {
    let point = point.as_ref();
    if point.is_empty() {
      return Err(LuceneError::illegal_argument(
        "point must not be 0 dimensions".to_string(),
      ));
    }
    let field_type = Self::get_type(point.len())?;
    let mut bytes = [0; MAX_INDEX_DIMENSIONS * BitUtil::LONG_BYTES];
    for (i, &dim) in point.iter().enumerate() {
      Self::encode_dimension(dim, &mut bytes, i * BitUtil::LONG_BYTES);
    }
    Ok(Self {
      name: name.into(),
      field_type,
      packed: Some(bytes),
    })
  }

  fn get_type(num_dims: usize) -> Result<FieldType> {
    let mut field_type = FieldType::new();
    field_type.set_dimensions(num_dims, BitUtil::LONG_BYTES)?;
    field_type.freeze();
    Ok(field_type)
  }

  /// Change the values of this field
  #[inline(always)]
  pub fn set_long_values<V>(&mut self, point: V) -> Result<()>
  where
    V: AsRef<[i64]>,
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
      .get_or_insert_with(|| [0; MAX_INDEX_DIMENSIONS * BitUtil::LONG_BYTES]);
    for (i, &dim) in point.iter().enumerate() {
      Self::encode_dimension(dim, bytes, i * BitUtil::LONG_BYTES);
    }
    Ok(())
  }

  /// Packs an `i64` slice into bytes.
  pub fn pack<P>(point: P) -> Result<BytesRef<Vec<u8>>>
  where
    P: AsRef<[i64]>,
  {
    let point = point.as_ref();
    if point.is_empty() {
      return Err(LuceneError::illegal_argument(
        "point must not be 0 dimensions".to_string(),
      ));
    }
    let mut packed = Vec::with_capacity(point.len() * BitUtil::LONG_BYTES);
    let mut encoded = [0; BitUtil::LONG_BYTES];
    for &dim in point {
      Self::encode_dimension(dim, &mut encoded, 0);
      packed.extend_from_slice(&encoded);
    }
    Ok(BytesRef::from_bytes(packed))
  }

  /// Unpacks bytes into `i64` values.
  pub fn unpack(bytes_ref: &BytesRef<Vec<u8>>, start: usize, buf: &mut [i64]) {
    for (i, val) in buf.iter_mut().enumerate() {
      *val = Self::decode_dimension(&bytes_ref.bytes, start + i * BitUtil::LONG_BYTES);
    }
  }

  /// Encode single long dimension
  pub fn encode_dimension(value: i64, dest: &mut [u8], offset: usize) {
    NumericUtils::long_to_sortable_bytes(value, dest, offset);
  }

  /// Decode single long dimension
  pub fn decode_dimension(value: &[u8], offset: usize) -> i64 {
    NumericUtils::sortable_bytes_to_long(value, offset)
  }
  pub fn new_exact_query<T>(field: T, value: i64) -> Result<PointRangeQuery>
  where
    T: Into<String>,
  {
    Self::new_range_query(field, value, value)
  }
  pub fn new_range_query<T>(field: T, lower_value: i64, upper_value: i64) -> Result<PointRangeQuery>
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
    V: Into<Vec<i64>>,
  {
    let mut sorted_values = values.into();
    sorted_values.sort_unstable();

    PointInSetQuery::new(
      field.into(),
      1,
      BitUtil::LONG_BYTES,
      LongPointSetBytesRefIterator::new(sorted_values),
      LongPointInSetQuery,
    )
  }

  ///
  /// Lower and upper bounds may independently use arrays, vectors, or slices.
  pub fn new_range_query_n<T, L, U>(
    field: T,
    lower_value: L,
    upper_value: U,
  ) -> Result<PointRangeQuery>
  where
    T: Into<String>,
    L: AsRef<[i64]>,
    U: AsRef<[i64]>,
  {
    let field = field.into();
    let len = lower_value.as_ref().len();
    let mut lower_point = LongPoint::pack(lower_value.as_ref())?;
    let mut upper_point = LongPoint::pack(upper_value.as_ref())?;

    #[cfg(debug_assertions)]
    check_args(&field, &lower_point.bytes, &upper_point.bytes)?;

    PointRangeQuery::new(
      field,
      lower_point.take_bytes(),
      upper_point.take_bytes(),
      len, // numDims
      LongPointRangeQuery,
    )
  }
}

struct LongPointSetBytesRefIterator {
  sorted_values: Vec<i64>,
  upto: usize,
  encoded: BytesRef<[u8; BitUtil::LONG_BYTES]>,
}

impl LongPointSetBytesRefIterator {
  fn new(sorted_values: Vec<i64>) -> Self {
    Self {
      sorted_values,
      upto: 0,
      encoded: BytesRef {
        bytes: [0u8; BitUtil::LONG_BYTES],
        offset: 0,
        length: BitUtil::LONG_BYTES,
      },
    }
  }
}

impl BytesRefIterator for LongPointSetBytesRefIterator {
  type Value<'a>
    = &'a BytesRef<[u8; BitUtil::LONG_BYTES]>
  where
    Self: 'a;

  #[inline]
  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    if self.upto == self.sorted_values.len() {
      Ok(None)
    } else {
      LongPoint::encode_dimension(self.sorted_values[self.upto], &mut self.encoded.bytes, 0);
      self.upto += 1;
      Ok(Some(&self.encoded))
    }
  }
}

impl FieldBase for LongPoint {
  fn set_bytes_value<'a, B>(&mut self, _value: B) -> Result<()>
  where
    B: Into<BinaryValueEnum<'a>>,
  {
    Err(LuceneError::illegal_argument(
      "cannot change value type from long to BytesRef",
    ))
  }

  fn set_long_value(&mut self, value: i64) -> Result<()> {
    self.set_long_values([value])
  }
}

impl IndexableField for LongPoint {
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
      let length = self.field_type.point_dimension_count() * BitUtil::LONG_BYTES;
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
      Some(bytes) => Ok(Some(Number::I64(Self::decode_dimension(bytes, 0)))),
      _ => {
        debug_assert!(false, "no possible here");
        Ok(None)
      },
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

impl fmt::Display for LongPoint {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{} <{}:", std::any::type_name::<Self>(), self.name)?;
    match &self.packed {
      Some(bytes) => {
        let dim_count = self.field_type.point_dimension_count();
        for dim in 0..dim_count {
          if dim > 0 {
            write!(f, ",")?;
          }
          let value = Self::decode_dimension(bytes, dim * BitUtil::LONG_BYTES);
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
pub struct LongPointRangeQuery;

impl PointRangeBase for LongPointRangeQuery {
  fn to_string(&self, _dimension: usize, value: &[u8]) -> Result<String> {
    Ok(LongPoint::decode_dimension(value, 0).to_string())
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LongPointInSetQuery;

impl PointInSetBase for LongPointInSetQuery {
  fn to_string(&self, value: &[u8]) -> Result<String> {
    debug_assert!(value.len() == BitUtil::LONG_BYTES);
    Ok(LongPoint::decode_dimension(value, 0).to_string())
  }
}
