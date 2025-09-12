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
use crate::core::codecs::stored_fields_writer::StoredFieldsWriter;
use crate::core::index::field_info::FieldInfo;
use crate::core::store::DataInput;
use crate::core::util::error::lucene_error::Result;
use std::sync::Arc;

/// Expert: provides a low-level means of accessing the stored field values in
/// an index.
///
/// # NOTE
/// a `StoredFieldVisitor` implementation should not try to load or visit other
/// stored documents in the same reader because the implementation of stored
/// fields for most codecs is not reentrant and you will see strange exceptions
/// as a result.
///
/// See [`DocumentStoredFieldVisitor`](crate::core::document::document_stored_field_visitor::DocumentStoredFieldVisitor), which is a `StoredFieldVisitor` that builds the [`Document`](crate::core::document::document::Document)
/// containing all stored fields.
pub trait StoredFieldVisitor {
    /// Expert: Process a binary field directly from the DataInput.
    /// Implementors of this method must read `length` bytes from the given
    /// `DataInput`. Default implementation reads into a byte array and
    /// delegates to `binary_field`.
    fn binary_field_with_input(
        &mut self,
        field_info: Arc<FieldInfo>,
        input: &mut impl DataInput,
        length: i32,
        writer: &mut impl StoredFieldsWriter,
    ) -> Result<()> {
        let mut buffer = vec![0u8; length as usize];
        input.read_bytes(&mut buffer, 0, length)?;
        self.binary_field(field_info, buffer, writer)
    }

    /// Process a binary field.
    fn binary_field(
        &mut self,
        _field_info: Arc<FieldInfo>,
        _value: Vec<u8>,
        _writer: &mut impl StoredFieldsWriter,
    ) -> Result<()> {
        Ok(())
    }

    /// Process a string field.
    fn string_field(
        &mut self,
        _field_info: Arc<FieldInfo>,
        _value: String,
        _writer: &mut impl StoredFieldsWriter,
    ) -> Result<()> {
        Ok(())
    }

    /// Process an int numeric field.
    fn int_field(
        &mut self,
        _field_info: Arc<FieldInfo>,
        _value: i32,
        _writer: &mut impl StoredFieldsWriter,
    ) -> Result<()> {
        Ok(())
    }

    /// Process a long numeric field.
    fn long_field(
        &mut self,
        _field_info: Arc<FieldInfo>,
        _value: i64,
        _writer: &mut impl StoredFieldsWriter,
    ) -> Result<()> {
        Ok(())
    }

    /// Process a float numeric field.
    fn float_field(
        &mut self,
        _field_info: Arc<FieldInfo>,
        _value: f32,
        _writer: &mut impl StoredFieldsWriter,
    ) -> Result<()> {
        Ok(())
    }

    /// Process a double numeric field.
    fn double_field(
        &mut self,
        _field_info: Arc<FieldInfo>,
        _value: f64,
        _writer: &mut impl StoredFieldsWriter,
    ) -> Result<()> {
        Ok(())
    }

    /// Hook before processing a field.
    /// Returns a [`Status`] representing whether to visit, skip, or stop.
    fn needs_field(
        &mut self,
        field_info: Arc<FieldInfo>,
        _writer: &mut impl StoredFieldsWriter,
    ) -> Result<Status>;
}

/// Enumeration of possible return values for `needs_field`.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Status {
    /// YES: the field should be visited.
    Yes,
    /// NO: don't visit this field, but continue processing fields for this
    /// document.
    No,
    /// STOP: don't visit this field and stop processing any other fields for
    /// this document.
    Stop,
}
