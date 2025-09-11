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
use std::fmt;
use std::rc::Rc;

use crate::core::analysis::analyzer::Analyzer;
use crate::core::analysis::reader::ReaderEnum;
use crate::core::document::field::{Field, FieldBase, FieldDataEnum};
use crate::core::document::field_type::FieldType;
use crate::core::document::invertable_field::InvertableType;
use crate::core::document::stored_value::StoredValue;
use crate::core::index::BytesRef;
use crate::core::index::indexable_field::IndexableField;
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::number::Number;
use crate::core::util::numeric_utils::NumericUtils;

pub struct DoublePoint {
    parent_field: Field,
}
impl DoublePoint {
    pub fn new(name: &str, point: &[f64]) -> Result<DoublePoint> {
        let packed = Self::pack(point)?;
        let len = packed.len();
        let value = Rc::new(BytesRef::from_slice(packed, 0, len));
        debug_assert!(len <= i32::MAX as usize);
        let field_type = Self::get_type(point.len() as i32)?;
        let parent_field = Field::with_bytes_ref(name, value, field_type)?;
        Ok(DoublePoint { parent_field })
    }

    fn get_type(num_dims: i32) -> Result<FieldType> {
        let mut field_type = FieldType::new();
        field_type.set_dimensions(num_dims, BitUtil::DOUBLE_BYTES as i32)?;
        field_type.freeze();
        Ok(field_type)
    }
    /// Change the values of this field
    pub fn set_double_values(&mut self, point: &[f64]) -> Result<()> {
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
        let value = Rc::new(BytesRef::from_slice(packed, 0, len));
        debug_assert!(len <= i32::MAX as usize);
        self.parent_field.fields_data = Option::from(FieldDataEnum::Binary(value));
        Ok(())
    }
    fn pack(point: &[f64]) -> Result<Vec<u8>> {
        if point.is_empty() {
            return Err(LuceneError::illegal_argument(
                "point must not be 0 dimensions".to_string(),
            ));
        }
        let mut packed = vec![0; point.len() * BitUtil::DOUBLE_BYTES];
        for (i, &dim) in point.iter().enumerate() {
            Self::encode_dimension(dim, &mut packed, i * BitUtil::DOUBLE_BYTES);
        }
        Ok(packed)
    }
    /// Encode a single double dimension into byte array
    pub fn encode_dimension(value: f64, dest: &mut [u8], offset: usize) {
        NumericUtils::long_to_sortable_bytes(
            NumericUtils::double_to_sortable_long(value),
            dest,
            offset,
        );
    }

    /// Decode a single double dimension from byte array
    pub fn decode_dimension(value: &[u8], offset: usize) -> f64 {
        NumericUtils::sortable_long_to_double(NumericUtils::sortable_bytes_to_long(value, offset))
    }
}
impl FieldBase for DoublePoint {
    fn set_double_value(&mut self, value: f64) -> Result<()> {
        self.set_double_values(&[value])
    }
}
impl IndexableField for DoublePoint {
    fn name(&self) -> &str {
        self.parent_field.name()
    }

    type FieldType = FieldType;

    fn field_type(&self) -> &Self::FieldType {
        self.parent_field.field_type()
    }

    type TokenStream = <Field as IndexableField>::TokenStream;

    fn token_stream<'a, A>(&'a mut self, analyzer: &A) -> Result<Option<&'a mut Self::TokenStream>>
    where
        A: Analyzer,
    {
        self.parent_field.token_stream(analyzer)
    }

    fn binary_value(&self) -> Result<Option<Rc<BytesRef<Vec<u8>>>>> {
        self.parent_field.binary_value()
    }

    fn string_value(&self) -> Result<Option<Rc<String>>> {
        self.parent_field.string_value()
    }

    fn reader_value(&self) -> Result<Option<ReaderEnum>> {
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
            Some(FieldDataEnum::Binary(bytes)) => {
                debug_assert!(bytes.length == BitUtil::DOUBLE_BYTES);
                let value = Self::decode_dimension(&bytes.bytes, bytes.offset);
                Ok(Some(Number::F64(value)))
            },
            _ => {
                debug_assert!(false, "no possible here");
                Ok(None)
            },
        }
    }

    fn stored_value(&self) -> Result<Option<StoredValue>> {
        todo!()
    }

    fn invertable_type(&self) -> &InvertableType {
        todo!()
    }
}
impl fmt::Display for DoublePoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DoublePoint <{}:", self.parent_field.name())?;

        match &self.parent_field.fields_data {
            Some(FieldDataEnum::Binary(bytes)) => {
                let dim_count = self.parent_field.field_type().point_dimension_count();
                for dim in 0..dim_count {
                    if dim > 0 {
                        write!(f, ",")?;
                    }
                    let value = Self::decode_dimension(
                        &bytes.bytes,
                        bytes.offset + dim as usize * BitUtil::DOUBLE_BYTES,
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
