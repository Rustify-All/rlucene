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
use crate::core::analysis::token_stream::{Either2TokenStream, InnerTokenStreams};
use crate::core::document::field::{Field, FieldBase, FieldDataEnum};
use crate::core::document::field_type::FieldType;
use crate::core::document::invertable_field::InvertableType;
use crate::core::index::BytesRef;
use crate::core::index::indexable_field::IndexableField;
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::number::Number;
use crate::core::util::numeric_utils::NumericUtils;
use std::borrow::Cow;
use std::fmt;

/// A field that indexes one or more `i32` (int) point values.
pub struct IntPoint {
    parent_field: Field,
}

impl IntPoint {
    /// Create a new IntPoint with the given name and int values
    pub fn new<T, P>(name: T, point: P) -> Result<IntPoint>
    where
        T: Into<String>,
        P: AsRef<[i32]>,
    {
        let point = point.as_ref();
        let packed = Self::pack(point)?;
        let len = packed.len();
        let value = BytesRef::from_slice(packed, 0, len);
        debug_assert!(len <= i32::MAX as usize);
        let field_type = Self::get_type(point.len() as i32)?;
        let parent_field = Field::with_bytes_ref(name, value, field_type)?;
        Ok(IntPoint { parent_field })
    }

    fn get_type(num_dims: i32) -> Result<FieldType> {
        let mut field_type = FieldType::new();
        field_type.set_dimensions(num_dims, BitUtil::INT_BYTES as i32)?;
        field_type.freeze();
        Ok(field_type)
    }

    /// Change the values of this field
    pub fn set_int_values(&mut self, point: &[i32]) -> Result<()> {
        if self.parent_field.field_type().point_dimension_count() as usize != point.len() {
            return Err(LuceneError::illegal_argument(format!(
                "this field (name={}) uses {} dimensions; cannot change to (incoming) {} dimensions",
                self.parent_field.name(),
                self.parent_field.field_type().point_dimension_count(),
                point.len()
            )));
        }
        let packed = Self::pack(point)?;
        let len = packed.len();
        let value = BytesRef::from_slice(packed, 0, len);
        debug_assert!(len <= i32::MAX as usize);
        self.parent_field.fields_data = FieldDataEnum::Binary(value);
        Ok(())
    }

    /// Pack an int array into bytes
    pub fn pack(point: &[i32]) -> Result<Vec<u8>> {
        if point.is_empty() {
            return Err(LuceneError::illegal_argument(
                "point must not be 0 dimensions".to_string(),
            ));
        }
        let mut packed = vec![0u8; point.len() * BitUtil::INT_BYTES];
        for (i, &dim) in point.iter().enumerate() {
            Self::encode_dimension(dim, &mut packed, i * BitUtil::INT_BYTES);
        }
        Ok(packed)
    }

    /// Encode single int dimension
    pub fn encode_dimension(value: i32, dest: &mut [u8], offset: usize) {
        NumericUtils::int_to_sortable_bytes(value, dest, offset);
    }

    /// Decode single int dimension
    pub fn decode_dimension(value: &[u8], offset: usize) -> i32 {
        NumericUtils::sortable_bytes_to_int(value, offset)
    }
}

impl FieldBase for IntPoint {
    fn set_int_value(&mut self, value: i32) -> Result<()> {
        self.set_int_values(&[value])
    }
}

impl IndexableField for IntPoint {
    fn name(&self) -> &str {
        self.parent_field.name()
    }

    type FieldType = FieldType;

    fn field_type(&self) -> &Self::FieldType {
        self.parent_field.field_type()
    }

    type TokenStream = <Field as IndexableField>::TokenStream;

    fn token_stream<'a>(
        &'a mut self,
        token_stream: Option<&'a mut InnerTokenStreams>,
    ) -> Result<Option<Either2TokenStream<&'a mut InnerTokenStreams, &'a mut Self::TokenStream>>>
    {
        self.parent_field.token_stream(token_stream)
    }

    fn binary_value(&self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
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
        if self.parent_field.field_type().point_dimension_count() != 1 {
            return Err(LuceneError::illegal_argument(format!(
                "this field (name={}) uses {} dimensions; cannot convert to a single numeric value",
                self.parent_field.name(),
                self.parent_field.field_type().point_dimension_count()
            )));
        }
        match &self.parent_field.fields_data {
            FieldDataEnum::Binary(bytes) => {
                debug_assert!(bytes.length == BitUtil::INT_BYTES);
                let value = Self::decode_dimension(&bytes.bytes, bytes.offset);
                Ok(Some(value.into()))
            },
            _ => {
                debug_assert!(false, "no possible here");
                Ok(None)
            },
        }
    }

    fn stored_value(&self) -> Option<&FieldDataEnum> {
        self.parent_field.stored_value()
    }

    fn take_stored_value(&mut self) -> Option<FieldDataEnum> {
        self.parent_field.take_stored_value()
    }

    fn invertable_type(&self) -> &InvertableType {
        self.parent_field.invertable_type()
    }

    fn init_token_stream<A>(&mut self, analyzer: &A) -> Result<()>
    where
        A: Analyzer,
    {
        self.parent_field.init_token_stream(analyzer)
    }
}

impl fmt::Display for IntPoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "IntPoint <{}:", self.parent_field.name())?;
        match &self.parent_field.fields_data {
            FieldDataEnum::Binary(bytes) => {
                let dim_count = self.parent_field.field_type().point_dimension_count();
                for dim in 0..dim_count {
                    if dim > 0 {
                        write!(f, ",")?;
                    }
                    let value = Self::decode_dimension(
                        &bytes.bytes,
                        bytes.offset + dim as usize * BitUtil::INT_BYTES,
                    );
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
