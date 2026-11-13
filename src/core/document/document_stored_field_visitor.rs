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
use crate::core::index::field_info::FieldInfo;
use crate::core::index::stored_field_visitor::{Status, StoredFieldVisitor};
use crate::core::store::DataInput;
use crate::core::util::error::lucene_error::Result;
use std::collections::HashSet;

/// A [`StoredFieldVisitor`] that creates a [`Document`] from stored fields.
///
/// This visitor supports loading all stored fields, or only specific requested
/// fields provided from a set.
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
  /// Consume this visitor and retrieve the visited document.
  ///
  /// Returns a [`Document`] populated with stored fields.
  /// Note that only the stored information in the field instances is valid;
  /// data such as indexing options, term vector options, etc. is not set.
  pub fn get_document_owner(self) -> Document {
    self.doc
  }
}
impl StoredFieldVisitor for DocumentStoredFieldVisitor<'_> {
  fn binary_field_with_input<S, DI>(
    &mut self,
    field_info: &FieldInfo,
    input: &mut DI,
    length: usize,
    writer: Option<&mut S>,
  ) -> Result<()>
  where
    S: StoredFieldsWriter,
    DI: DataInput,
  {
    // This visitor retains the bytes, so a fallback Vec can be transferred directly.
    let value = input.get_bytes(length)?;
    self.binary_field(field_info, value, writer)
  }

  fn binary_field<S>(
    &mut self,
    field_info: &FieldInfo,
    value: std::borrow::Cow<'_, [u8]>,
    _writer: Option<&mut S>,
  ) -> Result<()>
  where
    S: StoredFieldsWriter,
  {
    self.doc.add(StoredField::from_binary(
      field_info.name.clone(),
      value.into_owned(),
    )?);
    Ok(())
  }

  fn string_field<S>(
    &mut self,
    field_info: &FieldInfo,
    value: std::borrow::Cow<'_, str>,
    _writer: Option<&mut S>,
  ) -> Result<()>
  where
    S: StoredFieldsWriter,
  {
    let mut ft = FieldType::from_ref(&*crate::core::document::text_field::TYPE_STORED)?;
    ft.set_store_term_vectors(field_info.has_term_vectors())?;
    ft.set_omit_norms(field_info.omits_norms())?;
    ft.set_index_options(*field_info.get_index_options())?;
    self.doc.add(StoredField::from_string_and_type(
      field_info.name.clone(),
      value.into_owned(),
      ft,
    )?);
    Ok(())
  }

  fn int_field<S>(
    &mut self,
    field_info: &FieldInfo,
    value: i32,
    _writer: Option<&mut S>,
  ) -> Result<()>
  where
    S: StoredFieldsWriter,
  {
    self
      .doc
      .add(StoredField::from_i32(field_info.name.clone(), value)?);
    Ok(())
  }

  fn long_field<S>(
    &mut self,
    field_info: &FieldInfo,
    value: i64,
    _writer: Option<&mut S>,
  ) -> Result<()>
  where
    S: StoredFieldsWriter,
  {
    self
      .doc
      .add(StoredField::from_i64(field_info.name.clone(), value)?);
    Ok(())
  }

  fn float_field<S>(
    &mut self,
    field_info: &FieldInfo,
    value: f32,
    _writer: Option<&mut S>,
  ) -> Result<()>
  where
    S: StoredFieldsWriter,
  {
    self
      .doc
      .add(StoredField::from_f32(field_info.name.clone(), value)?);
    Ok(())
  }

  fn double_field<S>(
    &mut self,
    field_info: &FieldInfo,
    value: f64,
    _writer: Option<&mut S>,
  ) -> Result<()>
  where
    S: StoredFieldsWriter,
  {
    self
      .doc
      .add(StoredField::from_f64(field_info.name.clone(), value)?);
    Ok(())
  }

  fn needs_field<S>(&mut self, field_info: &FieldInfo, _writer: Option<&mut S>) -> Result<Status>
  where
    S: StoredFieldsWriter,
  {
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
