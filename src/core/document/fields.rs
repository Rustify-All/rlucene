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
use crate::core::analysis::dummy::dummy_token_stream::DummyTokenStream;
use crate::core::analysis::reader::ReaderEnum;
use crate::core::analysis::token_stream::TokenStream;
use crate::core::document::binary_doc_values_field::BinaryDocValuesField;
use crate::core::document::binary_point::BinaryPoint;
use crate::core::document::double_doc_values_field::DoubleDocValuesField;
use crate::core::document::double_field::DoubleField;
use crate::core::document::double_point::DoublePoint;
use crate::core::document::double_range::DoubleRange;
use crate::core::document::double_range_doc_values_field::DoubleRangeDocValuesField;
use crate::core::document::field::Field;
use crate::core::document::float_doc_values_field::FloatDocValuesField;
use crate::core::document::float_field::FloatField;
use crate::core::document::float_point::FloatPoint;
use crate::core::document::float_range::FloatRange;
use crate::core::document::float_range_doc_values_field::FloatRangeDocValuesField;
use crate::core::document::inet_address_point::InetAddressPoint;
use crate::core::document::inet_address_range::InetAddressRange;
use crate::core::document::int_field::IntField;
use crate::core::document::int_point::IntPoint;
use crate::core::document::int_range::IntRange;
use crate::core::document::int_range_doc_values_field::IntRangeDocValuesField;
use crate::core::document::invertable_field::InvertableType;
use crate::core::document::keyword_field::KeywordField;
use crate::core::document::knn_byte_vector_field::KnnByteVectorField;
use crate::core::document::knn_float_vector_field::KnnFloatVectorField;
use crate::core::document::lat_lon_doc_values_field::LatLonDocValuesField;
use crate::core::document::lat_lon_point::LatLonPoint;
use crate::core::document::long_field::LongField;
use crate::core::document::long_point::LongPoint;
use crate::core::document::long_range::LongRange;
use crate::core::document::long_range_doc_values_field::LongRangeDocValuesField;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::shape_field::Triangle;
use crate::core::document::sorted_doc_values_field::SortedDocValuesField;
use crate::core::document::sorted_numeric_doc_values_field::SortedNumericDocValuesField;
use crate::core::document::sorted_set_doc_values_field::SortedSetDocValuesField;
use crate::core::document::stored_field::StoredField;
use crate::core::document::string_field::StringField;
use crate::core::document::text_field::TextField;
use crate::core::document::xy_doc_values_field::XYDocValuesField;
use crate::core::document::xy_point_field::XYPointField;
use crate::core::index::BinaryValueEnum;
use crate::core::index::BytesRef;
use crate::core::index::indexable_field::{
  IndexableField, IndexingTokenStream, ReusedIndexingTokenStream, StoredValueEnum,
};
use crate::core::index::indexable_field_type::IndexableFieldTypeEnum;
use crate::core::index::indexing_chain::ReservedField;
use crate::core::util::attribute_source::Attributes;
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::number::Number;
use crate::impl_from_for_enum;
use crate::sandbox::document::big_integer_point::BigIntegerPoint;
use crate::sandbox::document::half_float_point::HalfFloatPoint;
use crate::sandbox::document::lat_lon_bounding_box::LatLonBoundingBox;
#[cfg(test)]
use crate::test_framework::core::document::FieldImpl;
#[cfg(test)]
use crate::test_framework::core::document::MockIndexableField;
#[cfg(test)]
use crate::test_framework::core::document::{CustomField, MyField};
use std::borrow::Cow;
use std::fmt::{Debug, Display, Formatter};

pub enum Fields {
  Binary(BinaryPoint),
  BinaryDocValues(BinaryDocValuesField),
  BigIntegerPoint(BigIntegerPoint),
  DoubleDocValues(DoubleDocValuesField),
  DoubleField(DoubleField),
  DoublePoint(DoublePoint),
  DoubleRange(DoubleRange),
  DoubleRangeDocValues(DoubleRangeDocValuesField),
  Field(Field),
  #[cfg(test)]
  FieldImpl(FieldImpl),
  #[cfg(test)]
  MockIndexableField(MockIndexableField),
  #[cfg(test)]
  MyField(MyField),
  #[cfg(test)]
  CustomField(CustomField),
  FloatDocValues(FloatDocValuesField),
  FloatField(FloatField),
  FloatPoint(FloatPoint),
  FloatRange(FloatRange),
  FloatRangeDocValues(FloatRangeDocValuesField),
  HalfFloatPoint(HalfFloatPoint),
  InetAddressPoint(InetAddressPoint),
  Int(IntRange),
  IntField(IntField),
  IntPoint(IntPoint),
  IntRangeDocValues(IntRangeDocValuesField),
  InetAddressRange(InetAddressRange),
  Keyword(KeywordField),
  KnnByteVector(KnnByteVectorField),
  KnnFloatVector(KnnFloatVectorField),
  LatLonBoundingBox(LatLonBoundingBox),
  LatLonDocValues(LatLonDocValuesField),
  LatLonPoint(LatLonPoint),
  LongField(LongField),
  LongPoint(LongPoint),
  LongRange(LongRange),
  LongRangeDocValues(LongRangeDocValuesField),
  NumericDocValues(NumericDocValuesField),
  Reverse(ReservedField<NumericDocValuesField>),
  SortedDocValues(SortedDocValuesField),
  SortedNumericDocValues(SortedNumericDocValuesField),
  SortedSetDocValues(SortedSetDocValuesField),
  Stored(StoredField),
  String(StringField),
  Text(TextField),
  Triangle(Triangle),
  XYDocValues(XYDocValuesField),
  XYPoint(XYPointField),
}

macro_rules! dispatch_fields {
  ($self:expr, |$inner:ident| $body:expr) => {{
    match $self {
      Fields::Binary($inner) => $body,
      Fields::BinaryDocValues($inner) => $body,
      Fields::BigIntegerPoint($inner) => $body,
      Fields::DoubleDocValues($inner) => $body,
      Fields::DoubleField($inner) => $body,
      Fields::DoublePoint($inner) => $body,
      Fields::DoubleRange($inner) => $body,
      Fields::DoubleRangeDocValues($inner) => $body,
      Fields::Field($inner) => $body,
      #[cfg(test)]
      Fields::FieldImpl($inner) => $body,
      #[cfg(test)]
      Fields::MockIndexableField($inner) => $body,
      #[cfg(test)]
      Fields::MyField($inner) => $body,
      #[cfg(test)]
      Fields::CustomField($inner) => $body,
      Fields::FloatDocValues($inner) => $body,
      Fields::FloatField($inner) => $body,
      Fields::FloatPoint($inner) => $body,
      Fields::FloatRange($inner) => $body,
      Fields::FloatRangeDocValues($inner) => $body,
      Fields::HalfFloatPoint($inner) => $body,
      Fields::InetAddressPoint($inner) => $body,
      Fields::Int($inner) => $body,
      Fields::IntField($inner) => $body,
      Fields::IntPoint($inner) => $body,
      Fields::IntRangeDocValues($inner) => $body,
      Fields::InetAddressRange($inner) => $body,
      Fields::Keyword($inner) => $body,
      Fields::KnnByteVector($inner) => $body,
      Fields::KnnFloatVector($inner) => $body,
      Fields::LatLonBoundingBox($inner) => $body,
      Fields::LatLonDocValues($inner) => $body,
      Fields::LatLonPoint($inner) => $body,
      Fields::LongField($inner) => $body,
      Fields::LongPoint($inner) => $body,
      Fields::LongRange($inner) => $body,
      Fields::LongRangeDocValues($inner) => $body,
      Fields::NumericDocValues($inner) => $body,
      Fields::Reverse($inner) => $body,
      Fields::SortedDocValues($inner) => $body,
      Fields::SortedNumericDocValues($inner) => $body,
      Fields::SortedSetDocValues($inner) => $body,
      Fields::Stored($inner) => $body,
      Fields::String($inner) => $body,
      Fields::Text($inner) => $body,
      Fields::Triangle($inner) => $body,
      Fields::XYDocValues($inner) => $body,
      Fields::XYPoint($inner) => $body,
    }
  }};
}

pub(crate) use dispatch_fields;

impl_from_for_enum!(
    Fields,
    BinaryPoint => Binary,
    BinaryDocValuesField => BinaryDocValues,
    BigIntegerPoint => BigIntegerPoint,
    DoubleDocValuesField => DoubleDocValues,
    DoubleField => DoubleField,
    DoublePoint => DoublePoint,
    DoubleRange => DoubleRange,
    DoubleRangeDocValuesField => DoubleRangeDocValues,
    Field => Field,
    FloatDocValuesField => FloatDocValues,
    FloatField => FloatField,
    FloatPoint => FloatPoint,
    FloatRange => FloatRange,
    FloatRangeDocValuesField => FloatRangeDocValues,
    HalfFloatPoint => HalfFloatPoint,
    InetAddressPoint => InetAddressPoint,
    IntRange => Int,
    IntField => IntField,
    IntPoint => IntPoint,
    IntRangeDocValuesField => IntRangeDocValues,
    InetAddressRange => InetAddressRange,
    KeywordField => Keyword,
    KnnByteVectorField => KnnByteVector,
    KnnFloatVectorField => KnnFloatVector,
    LatLonBoundingBox => LatLonBoundingBox,
    LatLonDocValuesField => LatLonDocValues,
    LatLonPoint => LatLonPoint,
    LongField => LongField,
    LongPoint => LongPoint,
    LongRange => LongRange,
    LongRangeDocValuesField => LongRangeDocValues,
    NumericDocValuesField => NumericDocValues,
    ReservedField<NumericDocValuesField> => Reverse,
    SortedDocValuesField => SortedDocValues,
    SortedNumericDocValuesField => SortedNumericDocValues,
    SortedSetDocValuesField => SortedSetDocValues,
    StoredField => Stored,
    StringField => String,
    TextField => Text,
    Triangle=> Triangle,
    XYDocValuesField => XYDocValues,
    XYPointField=> XYPoint,
);
#[cfg(test)]
impl_from_for_enum!(Fields, FieldImpl => FieldImpl);
#[cfg(test)]
impl_from_for_enum!(Fields, MockIndexableField => MockIndexableField);
#[cfg(test)]
impl_from_for_enum!(Fields, MyField => MyField, CustomField => CustomField);

impl Display for Fields {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    dispatch_fields!(self, |field| field.fmt(f))
  }
}

impl IndexableField for Fields {
  fn name(&self) -> &str {
    dispatch_fields!(self, |field| field.name())
  }

  type FieldType<'a>
    = IndexableFieldTypeEnum<'a>
  where
    Self: 'a;

  fn field_type(&self) -> Self::FieldType<'_> {
    dispatch_fields!(self, |field| field.field_type().into())
  }
  fn token_stream<'a, A>(
    &'a mut self,
    analyzer: &'a A,
    reuse_token_stream: &'a mut Option<ReusedIndexingTokenStream>,
  ) -> Result<IndexingTokenStream<'a>>
  where
    A: Analyzer,
  {
    dispatch_fields!(self, |field| field
      .token_stream(analyzer, reuse_token_stream))
  }

  fn binary_value(&self) -> Result<Option<BinaryValueEnum<'_>>> {
    dispatch_fields!(self, |field| field.binary_value())
  }

  fn take_binary_value(&mut self) -> Result<Option<BytesRef<Vec<u8>>>> {
    dispatch_fields!(self, |field| field.take_binary_value())
  }

  fn string_value(&self) -> Result<Option<Cow<'_, str>>> {
    dispatch_fields!(self, |field| field.string_value())
  }

  fn take_string_value(&mut self) -> Result<Option<String>> {
    dispatch_fields!(self, |field| field.take_string_value())
  }

  fn get_char_sequence_value(&self) -> Result<Option<Cow<'_, str>>> {
    dispatch_fields!(self, |field| field.get_char_sequence_value())
  }

  fn take_reader_value(&mut self) -> Result<Option<ReaderEnum>> {
    dispatch_fields!(self, |field| field.take_reader_value())
  }

  fn numeric_value(&self) -> Result<Option<Number>> {
    dispatch_fields!(self, |field| field.numeric_value())
  }

  type StoredValue<'a>
    = StoredValueEnum<'a>
  where
    Self: 'a;

  fn stored_value(&self) -> Result<Option<Self::StoredValue<'_>>> {
    dispatch_fields!(self, |field| {
      Ok(field.stored_value()?.map(StoredValueEnum::from))
    })
  }

  fn invertable_type(&self) -> &InvertableType {
    dispatch_fields!(self, |field| field.invertable_type())
  }

  fn is_reserved(&self) -> bool {
    dispatch_fields!(self, |field| field.is_reserved())
  }
  fn float_vector_value(&self) -> Result<&[f32]> {
    dispatch_fields!(self, |field| field.float_vector_value())
  }
  fn byte_vector_value(&self) -> Result<&[u8]> {
    dispatch_fields!(self, |field| field.byte_vector_value())
  }
}

pub type CustomTokenStream = Box<dyn TokenStream + Send + Sync>;
pub enum FieldTokenStreamEnum {
  Dummy(DummyTokenStream),
  Custom(CustomTokenStream),
}
impl FieldTokenStreamEnum {
  pub fn custom<S>(sim: S) -> Self
  where
    S: TokenStream + Send + Sync + 'static,
  {
    FieldTokenStreamEnum::Custom(Box::new(sim))
  }
}

impl Closeable for FieldTokenStreamEnum {
  fn close(&mut self) -> Result<()> {
    match self {
      FieldTokenStreamEnum::Dummy(dummy) => dummy.close(),
      FieldTokenStreamEnum::Custom(custom) => custom.close(),
    }
  }
}

impl TokenStream for FieldTokenStreamEnum {
  fn increment_token(&mut self) -> Result<bool> {
    match self {
      FieldTokenStreamEnum::Dummy(dummy) => dummy.increment_token(),
      FieldTokenStreamEnum::Custom(custom) => custom.increment_token(),
    }
  }

  fn end(&mut self) -> Result<()> {
    match self {
      FieldTokenStreamEnum::Dummy(dummy) => dummy.end(),
      FieldTokenStreamEnum::Custom(custom) => custom.end(),
    }
  }

  fn default_end(&mut self) -> Result<()> {
    match self {
      FieldTokenStreamEnum::Dummy(dummy) => dummy.default_end(),
      FieldTokenStreamEnum::Custom(custom) => custom.default_end(),
    }
  }

  fn reset(&mut self) -> Result<()> {
    match self {
      FieldTokenStreamEnum::Dummy(dummy) => dummy.reset(),
      FieldTokenStreamEnum::Custom(custom) => custom.reset(),
    }
  }

  fn default_reset(&mut self) -> Result<()> {
    match self {
      FieldTokenStreamEnum::Dummy(dummy) => dummy.default_reset(),
      FieldTokenStreamEnum::Custom(custom) => custom.default_reset(),
    }
  }

  fn get_attribute_source(&self) -> &Attributes {
    match self {
      FieldTokenStreamEnum::Dummy(dummy) => dummy.get_attribute_source(),
      FieldTokenStreamEnum::Custom(custom) => custom.get_attribute_source(),
    }
  }

  fn get_attribute_source_mut(&mut self) -> &mut Attributes {
    match self {
      FieldTokenStreamEnum::Dummy(dummy) => dummy.get_attribute_source_mut(),
      FieldTokenStreamEnum::Custom(custom) => custom.get_attribute_source_mut(),
    }
  }

  fn set_reader(&mut self, _input: ReaderEnum) -> Result<()> {
    match self {
      FieldTokenStreamEnum::Dummy(dummy) => dummy.set_reader(_input),
      FieldTokenStreamEnum::Custom(custom) => custom.set_reader(_input),
    }
  }

  fn set_reader_test_point(&mut self) -> Result<()> {
    match self {
      FieldTokenStreamEnum::Dummy(dummy) => dummy.set_reader_test_point(),
      FieldTokenStreamEnum::Custom(custom) => custom.set_reader_test_point(),
    }
  }
}
impl Debug for FieldTokenStreamEnum {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self {
      FieldTokenStreamEnum::Dummy(dummy) => dummy.fmt(f),
      FieldTokenStreamEnum::Custom(_) => write!(f, "CustomTokenStream"),
    }
  }
}
