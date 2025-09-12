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
use crate::core::document::document::Document;
use crate::core::document::field_type::FieldType;
use crate::core::document::stored_field::StoredField;
use crate::core::document::text_field::text;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::stored_field_visitor::{Status, StoredFieldVisitor};
use crate::core::util::error::lucene_error::Result;
use std::collections::HashSet;
use std::sync::Arc;

/// A [`StoredFieldVisitor`] that creates a [`Document`] from stored fields.
///
/// This visitor supports loading all stored fields, or only specific requested
/// fields provided from a `Set`.
///
/// This is used by
/// [`StoredFields::document`](crate::core::index::stored_fields::StoredFields::document)
/// to load a document.
pub struct DocumentStoredFieldVisitor<'a> {
    doc: Document,
    fields_to_add: Option<&'a HashSet<String>>,
}
impl Default for DocumentStoredFieldVisitor<'_> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> DocumentStoredFieldVisitor<'a> {
    /// Load all fields
    pub fn new() -> Self {
        Self {
            doc: Document::default(),
            fields_to_add: None,
        }
    }

    /// Load only selected fields
    pub fn with_fields(fields: &'a HashSet<String>) -> Self {
        Self {
            doc: Document::default(),
            fields_to_add: Some(fields),
        }
    }

    pub fn get_document_ref(&self) -> &Document {
        &self.doc
    }
    /// Retrieve the visited document.
    ///
    /// Returns a [`Document`] populated with stored fields.
    /// Note that only the stored information in the field instances is valid;
    /// data such as indexing options, term vector options, etc. is not set.
    pub fn get_document_owner(&mut self) -> Document {
        std::mem::take(&mut self.doc)
    }
}
impl StoredFieldVisitor for DocumentStoredFieldVisitor<'_> {
    fn binary_field(
        &mut self,
        field_info: Arc<FieldInfo>,
        value: Vec<u8>,
        _writer: &mut impl StoredFieldsWriter,
    ) -> Result<()> {
        self.doc
            .add(StoredField::with_binary(&field_info.name, value)?);
        Ok(())
    }

    fn string_field(
        &mut self,
        field_info: Arc<FieldInfo>,
        value: String,
        _writer: &mut impl StoredFieldsWriter,
    ) -> Result<()> {
        let mut ft = FieldType::from_ref(&*text::TYPE_STORED)?;
        ft.set_store_term_vectors(field_info.has_term_vectors())?;
        ft.set_omit_norms(field_info.omits_norms())?;
        ft.set_index_options(*field_info.get_index_options())?;
        self.doc.add(StoredField::with_string_and_type(
            &field_info.name,
            value,
            ft,
        )?);
        Ok(())
    }

    fn int_field(
        &mut self,
        field_info: Arc<FieldInfo>,
        value: i32,
        _writer: &mut impl StoredFieldsWriter,
    ) -> Result<()> {
        self.doc
            .add(StoredField::with_i32(&field_info.name, value)?);
        Ok(())
    }

    fn long_field(
        &mut self,
        field_info: Arc<FieldInfo>,
        value: i64,
        _writer: &mut impl StoredFieldsWriter,
    ) -> Result<()> {
        self.doc
            .add(StoredField::with_i64(&field_info.name, value)?);
        Ok(())
    }

    fn float_field(
        &mut self,
        field_info: Arc<FieldInfo>,
        value: f32,
        _writer: &mut impl StoredFieldsWriter,
    ) -> Result<()> {
        self.doc
            .add(StoredField::with_f32(&field_info.name, value)?);
        Ok(())
    }

    fn double_field(
        &mut self,
        field_info: Arc<FieldInfo>,
        value: f64,
        _writer: &mut impl StoredFieldsWriter,
    ) -> Result<()> {
        self.doc
            .add(StoredField::with_f64(&field_info.name, value)?);
        Ok(())
    }

    fn needs_field(
        &mut self,
        field_info: Arc<FieldInfo>,
        _writer: &mut impl StoredFieldsWriter,
    ) -> Result<Status> {
        match self.fields_to_add {
            Some(fields) => {
                if fields.contains(&field_info.name) {
                    Ok(Status::Yes)
                } else {
                    Ok(Status::No)
                }
            },
            None => Ok(Status::Yes),
        }
    }
}
