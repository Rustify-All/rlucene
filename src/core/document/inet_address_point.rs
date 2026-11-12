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
use crate::core::search::point_in_set_query::{PointInSetBase, PointInSetQuery};
#[cfg(debug_assertions)]
use crate::core::search::point_range_query::check_args;
use crate::core::search::point_range_query::{PointRangeBase, PointRangeQuery};
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::number::Number;
use crate::core::util::numeric_utils::NumericUtils;
use std::borrow::Cow;
use std::fmt;
use std::fmt::Write as _;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

const IPV4_PREFIX: [u8; 12] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff];

/// An indexed 128-bit [`IpAddr`] field.
///
/// Finding all documents within a range at search time is efficient. Multiple values for the same
/// field in one document is allowed.
///
/// This field defines static factory methods for creating common queries:
///
/// - [`new_exact_query`](Self::new_exact_query) for matching an exact network address.
/// - [`new_prefix_query`](Self::new_prefix_query) for matching a network based on CIDR prefix.
/// - [`new_range_query`](Self::new_range_query) for matching arbitrary network address ranges.
///
/// This field supports both IPv4 and IPv6 addresses: IPv4 addresses are converted to
/// IPv4-Mapped IPv6 Addresses: indexing `1.2.3.4` is the same as indexing `::FFFF:1.2.3.4`.
pub struct InetAddressPoint {
  parent_field: Field,
}

impl InetAddressPoint {
  // Implementation note: we convert all addresses to IPv6: we expect prefix compression of values,
  // so its not wasteful, but allows one field to handle both IPv4 and IPv6.

  /// The number of bytes per dimension: 128 bits.
  pub const BYTES: usize = 16;

  /// The minimum value that an ip address can hold.
  pub const MIN_VALUE: IpAddr = IpAddr::V6(Ipv6Addr::UNSPECIFIED);

  /// The maximum value that an ip address can hold.
  pub const MAX_VALUE: IpAddr = IpAddr::V6(Ipv6Addr::new(
    0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff, 0xffff,
  ));

  /// Return the [`IpAddr`] that compares immediately greater than `address`.
  ///
  /// # Errors
  ///
  /// Returns an error if the provided address is the maximum ip address.
  pub fn next_up(address: IpAddr) -> Result<IpAddr> {
    if address == Self::MAX_VALUE {
      return Err(LuceneError::number_overflow(format!(
        "Overflow: there is no greater InetAddress than {}",
        host_address(address)
      )));
    }
    let mut delta = [0u8; Self::BYTES];
    delta[Self::BYTES - 1] = 1;
    let mut next_up_bytes = [0u8; Self::BYTES];
    NumericUtils::add(
      Self::BYTES,
      0,
      &encode_address(address),
      &delta,
      &mut next_up_bytes,
    )?;
    Ok(decode_address(&next_up_bytes))
  }

  /// Return the [`IpAddr`] that compares immediately less than `address`.
  ///
  /// # Errors
  ///
  /// Returns an error if the provided address is the minimum ip address.
  pub fn next_down(address: IpAddr) -> Result<IpAddr> {
    if address == Self::MIN_VALUE {
      return Err(LuceneError::number_overflow(format!(
        "Underflow: there is no smaller InetAddress than {}",
        host_address(address)
      )));
    }
    let mut delta = [0u8; Self::BYTES];
    delta[Self::BYTES - 1] = 1;
    let mut next_down_bytes = [0u8; Self::BYTES];
    NumericUtils::subtract(
      Self::BYTES,
      0,
      &encode_address(address),
      &delta,
      &mut next_down_bytes,
    )?;
    Ok(decode_address(&next_down_bytes))
  }

  /// Creates a new InetAddressPoint, indexing the provided address.
  ///
  /// # Arguments
  ///
  /// - `name` - Field name.
  /// - `point` - InetAddress value.
  ///
  /// # Errors
  ///
  /// Returns an error if the field name or value is invalid.
  pub fn new<T>(name: T, point: IpAddr) -> Result<Self>
  where
    T: Into<String>,
  {
    let field = Self {
      parent_field: Field::from_bytes_ref(
        name,
        BytesRef::from_bytes(encode_address(point).to_vec()),
        Self::get_type()?,
      )?,
    };
    Ok(field)
  }

  fn get_type() -> Result<FieldType> {
    let mut ft = FieldType::new();
    ft.set_dimensions(1, InetAddressPoint::BYTES)?;
    ft.freeze();
    Ok(ft)
  }

  /// Change the values of this field.
  #[inline(always)]
  pub fn set_inet_address_value(&mut self, value: IpAddr) -> Result<()> {
    let encoded = encode_address(value);
    match &mut self.parent_field.fields_data {
      FieldDataEnum::Binary(bytes) if bytes.bytes.len() == Self::BYTES => {
        bytes.bytes.copy_from_slice(&encoded);
        bytes.offset = 0;
        bytes.length = Self::BYTES;
      },
      _ => {
        self.parent_field.fields_data =
          FieldDataEnum::Binary(BytesRef::from_bytes(encoded.to_vec()));
      },
    }
    Ok(())
  }

  /// Encode InetAddress value into binary encoding.
  #[inline]
  pub fn encode(value: IpAddr) -> [u8; Self::BYTES] {
    encode_address(value)
  }

  /// Decodes InetAddress value from binary encoding.
  pub fn decode(value: &[u8]) -> IpAddr {
    decode_address(value)
  }

  /// Create a query for matching a network address.
  ///
  /// # Arguments
  ///
  /// - `field` - Field name.
  /// - `value` - Exact value.
  ///
  /// # Returns
  ///
  /// A query matching documents with this exact value.
  pub fn new_exact_query<T>(field: T, value: IpAddr) -> Result<PointRangeQuery>
  where
    T: Into<String>,
  {
    Self::new_range_query(field, value, value)
  }

  /// Create a prefix query for matching a CIDR network range.
  ///
  /// # Arguments
  ///
  /// - `field` - Field name.
  /// - `value` - Any host address.
  /// - `prefix_length` - The network prefix length for this address. This is also known as the
  ///   subnet mask in the context of IPv4 addresses.
  ///
  /// # Returns
  ///
  /// A query matching documents with addresses contained within this network.
  ///
  /// # Errors
  ///
  /// Returns an error if `prefix_length` is invalid.
  pub fn new_prefix_query<T>(
    field: T,
    value: IpAddr,
    prefix_length: usize,
  ) -> Result<PointRangeQuery>
  where
    T: Into<String>,
  {
    let ipv4_bytes;
    let ipv6_bytes;
    let address_bytes: &[u8] = match value {
      IpAddr::V4(address) => {
        ipv4_bytes = address.octets();
        &ipv4_bytes
      },
      IpAddr::V6(address) => {
        ipv6_bytes = address.octets();
        &ipv6_bytes
      },
    };
    if prefix_length > 8 * address_bytes.len() {
      return Err(LuceneError::illegal_argument(format!(
        "illegal prefixLength '{}'. Must be 0-32 for IPv4 ranges, 0-128 for IPv6 ranges",
        prefix_length
      )));
    }

    let mut lower_buffer = [0u8; Self::BYTES];
    let mut upper_buffer = [0u8; Self::BYTES];
    let lower = &mut lower_buffer[..address_bytes.len()];
    let upper = &mut upper_buffer[..address_bytes.len()];
    lower.copy_from_slice(address_bytes);
    upper.copy_from_slice(address_bytes);
    for i in prefix_length..(8 * lower.len()) {
      let mask = 1u8 << (7 - (i & 7));
      lower[i >> 3] &= !mask;
      upper[i >> 3] |= mask;
    }

    let lower_value = if lower.len() == 4 {
      let mut bytes = [0u8; 4];
      bytes.copy_from_slice(lower);
      IpAddr::V4(bytes.into())
    } else {
      let mut bytes = [0u8; Self::BYTES];
      bytes.copy_from_slice(lower);
      IpAddr::V6(bytes.into())
    };
    let upper_value = if upper.len() == 4 {
      let mut bytes = [0u8; 4];
      bytes.copy_from_slice(upper);
      IpAddr::V4(bytes.into())
    } else {
      let mut bytes = [0u8; Self::BYTES];
      bytes.copy_from_slice(upper);
      IpAddr::V6(bytes.into())
    };

    Self::new_range_query(field, lower_value, upper_value)
  }

  /// Create a range query for network addresses.
  ///
  /// You can have half-open ranges (which are in fact `<`/`<=` or `>`/`>=` queries) by using
  /// [`Self::MIN_VALUE`] for `lower_value` or [`Self::MAX_VALUE`] for `upper_value`.
  ///
  /// Ranges are inclusive. For exclusive ranges, transform the bound with [`Self::next_up`] or
  /// [`Self::next_down`].
  ///
  /// # Arguments
  ///
  /// - `field` - Field name.
  /// - `lower_value` - Lower portion of the range (inclusive).
  /// - `upper_value` - Upper portion of the range (inclusive).
  ///
  /// # Returns
  ///
  /// A query matching documents within this range.
  pub fn new_range_query<T>(
    field: T,
    lower_value: IpAddr,
    upper_value: IpAddr,
  ) -> Result<PointRangeQuery>
  where
    T: Into<String>,
  {
    let field = field.into();
    let lower_point = encode_address(lower_value);
    let upper_point = encode_address(upper_value);
    #[cfg(debug_assertions)]
    check_args(&field, &lower_point, &upper_point)?;
    PointRangeQuery::new(
      field,
      lower_point.to_vec(),
      upper_point.to_vec(),
      1,
      InetAddressPointRangeQuery,
    )
  }

  /// Create a query matching any of the specified 1D values. This is the points equivalent of
  /// [`TermInSetQuery`](crate::core::search::term_in_set_query::TermInSetQuery).
  ///
  /// # Arguments
  ///
  /// * `field` - Field name.
  /// * `values` - All values to match.
  pub fn new_set_query<T, V>(field: T, values: V) -> Result<PointInSetQuery>
  where
    T: Into<String>,
    V: AsRef<[IpAddr]>,
  {
    let mut sorted_values = Vec::with_capacity(values.as_ref().len());
    for value in values.as_ref() {
      sorted_values.push(Self::encode(*value));
    }
    sorted_values.sort_unstable();

    PointInSetQuery::new(
      field.into(),
      1,
      Self::BYTES,
      InetAddressPointSetBytesRefIterator::new(sorted_values),
      InetAddressPointInSetQuery,
    )
  }
}

struct InetAddressPointSetBytesRefIterator {
  sorted_values: Vec<[u8; InetAddressPoint::BYTES]>,
  upto: usize,
}

impl InetAddressPointSetBytesRefIterator {
  fn new(sorted_values: Vec<[u8; InetAddressPoint::BYTES]>) -> Self {
    Self {
      sorted_values,
      upto: 0,
    }
  }
}

impl BytesRefIterator for InetAddressPointSetBytesRefIterator {
  type Value<'a>
    = BytesRef<&'a [u8]>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    if self.upto == self.sorted_values.len() {
      Ok(None)
    } else {
      let bytes = self.sorted_values[self.upto].as_slice();
      self.upto += 1;
      Ok(Some(BytesRef {
        bytes,
        offset: 0,
        length: bytes.len(),
      }))
    }
  }
}

impl FieldBase for InetAddressPoint {
  fn set_bytes_value<'a, B>(&mut self, _value: B) -> Result<()>
  where
    B: Into<BinaryValueEnum<'a>>,
  {
    Err(LuceneError::illegal_argument(
      "cannot change value type from InetAddress to BytesRef",
    ))
  }
}

impl IndexableField for InetAddressPoint {
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
    todo!()
  }

  fn numeric_value(&self) -> Result<Option<Number>> {
    Err(LuceneError::illegal_argument(
      "cannot convert InetAddressPoint to a single numeric value",
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

impl fmt::Display for InetAddressPoint {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "InetAddressPoint <{}:", self.parent_field.name())?;
    match &self.parent_field.fields_data {
      FieldDataEnum::Binary(bytes) => {
        let address = decode_address(&bytes.bytes[bytes.offset..bytes.offset + Self::BYTES]);
        if matches!(address, IpAddr::V6(_)) {
          write!(f, "[{}]", host_address(address))?;
        } else {
          write!(f, "{}", host_address(address))?;
        }
      },
      _ => {
        write!(f, "Unsupported FieldDataEnum variant")?;
      },
    }
    write!(f, ">")
  }
}

/// Encode InetAddress value into binary encoding.
#[inline]
pub fn encode_address(value: IpAddr) -> [u8; InetAddressPoint::BYTES] {
  match value {
    IpAddr::V4(address) => {
      let mut mapped = [0u8; InetAddressPoint::BYTES];
      mapped[..IPV4_PREFIX.len()].copy_from_slice(&IPV4_PREFIX);
      mapped[IPV4_PREFIX.len()..].copy_from_slice(&address.octets());
      mapped
    },
    IpAddr::V6(address) => address.octets(),
  }
}

/// Decodes InetAddress value from binary encoding.
pub fn decode_address(value: &[u8]) -> IpAddr {
  let mut bytes = [0u8; InetAddressPoint::BYTES];
  bytes.copy_from_slice(&value[..InetAddressPoint::BYTES]);
  if bytes[..IPV4_PREFIX.len()] == IPV4_PREFIX {
    return IpAddr::V4(Ipv4Addr::new(bytes[12], bytes[13], bytes[14], bytes[15]));
  }
  IpAddr::V6(Ipv6Addr::from(bytes))
}

pub(crate) fn host_address(address: IpAddr) -> String {
  match address {
    IpAddr::V4(address) => address.to_string(),
    IpAddr::V6(address) => {
      let mut result = String::with_capacity(39);
      for (index, segment) in address.segments().iter().enumerate() {
        if index != 0 {
          result.push(':');
        }
        let _ = write!(result, "{segment:x}");
      }
      result
    },
  }
}

#[derive(Debug, Clone)]
pub struct InetAddressPointRangeQuery;

impl PointRangeBase for InetAddressPointRangeQuery {
  fn to_string(&self, _dimension: usize, value: &[u8]) -> Result<String> {
    Ok(host_address(decode_address(value)))
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InetAddressPointInSetQuery;

impl PointInSetBase for InetAddressPointInSetQuery {
  fn to_string(&self, value: &[u8]) -> Result<String> {
    Ok(host_address(InetAddressPoint::decode(value)))
  }
}
