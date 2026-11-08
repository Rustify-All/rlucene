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
use std::borrow::Cow;
use std::fmt;
use std::vec::IntoIter;

use crate::core::document::fields::Fields;
use crate::core::index::BinaryValueEnum;
use crate::core::index::indexable_field::IndexableField;
use crate::core::util::error::lucene_error::Result;

/// Documents are the unit of indexing and search.
///
/// A Document is a set of fields. Each field has a name and a textual value. A
/// field may be
/// [`IndexableFieldType::stored`](crate::core::index::indexable_field_type::IndexableFieldType::stored) with the document, in which case it is returned with search hits
/// on the document. Thus each document should typically contain one or more
/// stored fields which uniquely identify it.
///
/// Note that fields which are *not*
/// [`IndexableFieldType::stored`](crate::core::index::indexable_field_type::IndexableFieldType::stored)
/// are *not* available in documents retrieved from the index, e.g. with
/// [`ScoreDoc::doc`](crate::core::search::score_doc::ScoreDoc) or
/// [`StoredFields::document(i32)`](crate::core::index::stored_fields::StoredFields::document).
pub struct Document {
  fields: Vec<Fields>,
}
impl Default for Document {
  fn default() -> Self {
    Self::new()
  }
}

impl Document {
  /// Constructs a new document with no fields.
  pub fn new() -> Self {
    Document { fields: Vec::new() }
  }
  /// Adds a field to a document. Several fields may be added with the same
  /// name. In this case, if the fields are indexed, their text is treated
  /// as though appended for the purposes of search.
  ///
  /// Note that `add` like the `removeField(s)` methods only makes sense prior
  /// to adding a document to an index. These methods cannot be used to
  /// change the content oR an existing index! In order to achieve this, a
  /// document has to be deleted from an index and a new changed version of
  /// that document has to be added.
  pub fn add<T>(&mut self, field: T)
  where
    T: Into<Fields>,
  {
    let field = field.into();
    if self.fields.capacity() == 0 {
      self.fields.reserve_exact(10);
    }
    self.fields.push(field);
  }
  /// Removes the field with the specified name from the document. If multiple
  /// fields exist with this name, this method removes the first field
  /// that has been added. If there is no field with the specified name,
  /// the document remains unchanged.
  ///
  /// Note that the `removeField(s)` methods, like the `add` method, only make
  /// sense prior to adding a document to an index. These methods cannot
  /// be used to change the content of an existing index! In order to
  /// achieve this, a document has to be deleted from an index and a new
  /// changed version of that document has to be added.
  pub fn remove_field(&mut self, name: &str) {
    if let Some(index) = self.fields.iter().position(|field| field.name() == name) {
      self.fields.remove(index);
    }
  }
  /// Removes all fields with the given name from the document. If there is no
  /// field with the specified name, the document remains unchanged.
  ///
  /// Note that the `removeField(s)` methods, like the `add` method, only make
  /// sense prior to adding a document to an index. These methods cannot
  /// be used to change the content of an existing index! In order to
  /// achieve this, a document has to be deleted from an index and a new
  /// changed version of that document has to be added.
  pub fn remove_fields(&mut self, name: &str) {
    self.fields.retain(|field| field.name() != name);
  }
  /// Returns an array of byte arrays for the fields that have the name
  /// specified as the method parameter. This method returns an empty
  /// array when there are no matching fields. It never returns `None`.
  ///
  /// # Parameters
  /// - `name`: the name of the field
  ///
  /// # Returns
  /// A vector of binary field values.
  pub fn get_binary_values(&self, name: &str) -> Result<Vec<BinaryValueEnum<'_>>> {
    let mut result = Vec::new();

    for field in &self.fields {
      if field.name() == name {
        match field.binary_value() {
          Ok(Some(bytes)) => result.push(bytes),
          Ok(None) => continue,
          Err(e) => return Err(e),
        }
      }
    }
    Ok(result)
  }
  /// Returns an array of bytes for the first (or only) field that has the
  /// name specified as the method parameter. This method will return
  /// `None` if no binary fields with the specified name are available.
  /// There may be non-binary fields with the same name.
  ///
  /// # Parameters
  /// - `name`: the name of the field.
  ///
  /// # Returns
  /// The binary field value, or `None` if no matching field is found.
  pub fn get_binary_value(&self, name: &str) -> Result<Option<BinaryValueEnum<'_>>> {
    for field in &self.fields {
      if field.name() == name {
        return match field.binary_value() {
          Ok(Some(bytes)) => Ok(Some(bytes)),
          Ok(None) => Ok(None),
          Err(e) => Err(e),
        };
      }
    }
    Ok(None)
  }
  /// Returns a field with the given name if any exist in this document, or
  /// `None`. If multiple fields exist with this name, this method returns
  /// the first value added.
  ///
  /// # Parameters
  /// - `name`: the name of the field.
  ///
  /// # Returns
  /// An `Option<Arc>`,  `None` means no matching field is found.
  pub fn get_field(&self, name: &str) -> Option<&Fields> {
    self.fields.iter().find(|field| field.name() == name)
  }

  /// Returns a mutable field with the given name if any exist in this document, or
  /// `None`. If multiple fields exist with this name, this method returns
  /// the first value added.
  pub fn get_field_mut(&mut self, name: &str) -> Option<&mut Fields> {
    self.fields.iter_mut().find(|field| field.name() == name)
  }

  /// Returns an array of [`IndexableField`]s with the given name. This method
  /// returns an empty array when there are no matching fields. It never
  /// returns `None`.
  ///
  /// # Parameters
  /// - `name`: the name of the field.
  ///
  /// # Returns
  /// A `Vec<Arc>` array containing the matching fields.
  pub fn get_fields_with_name(&self, name: &str) -> Vec<&Fields> {
    self
      .fields
      .iter()
      .filter(|field| field.name() == name)
      .collect()
  }

  /// Returns a `Vec<Arc>` containing all the fields in a document.
  ///
  /// # Note
  /// Fields that are not stored are not available in documents retrieved from
  /// the index, for example when using
  /// [`StoredFields::document`](crate::core::index::stored_fields::StoredFields::document).
  ///
  /// # Returns
  /// An immutable `Vec<Arc>` containing all fields in the document.
  pub fn get_fields(&self) -> &[Fields] {
    &self.fields
  }
  /// Returns an array of values of the field specified by the `name`. This
  /// method returns an empty array when there are no matching fields. It
  /// never returns `None`. For a numeric [`StoredField`](crate::core::document::stored_field::StoredField), it returns the
  /// string representation of the number. To get the actual numeric field
  /// instances, use `getFields`.
  ///
  /// # Parameters
  /// - `name`: the name of the field.
  ///
  /// # Returns
  /// A `Vec<Arc<String>`, which is an empty vector if no matching fields are
  /// found.
  pub fn get_values(&self, name: &str) -> Result<Vec<Cow<'_, String>>> {
    let mut result = Vec::new();
    for field in &self.fields {
      if field.name() == name {
        match field.string_value() {
          Ok(Some(value)) => result.push(value),
          Ok(None) => continue,
          Err(e) => return Err(e),
        }
      }
    }
    Ok(result)
  }
  /// Returns the string value of the field with the given name if any exist
  /// in this document, or `None`. If multiple fields exist with this
  /// name, this method returns the first value added. If only binary
  /// fields with this name exist, returns `None`. For a numeric
  /// [`StoredField`](crate::core::document::stored_field::StoredField), it returns the string value of the number. To get the
  /// actual numeric field instance, use `getField`.
  ///
  /// # Parameters
  /// - `name`: the name of the field.
  ///
  /// # Returns
  /// An `Option<Arc<String>>`,  `None` means no string value is found (e.g.,
  /// for binary fields).
  pub fn get(&self, name: &str) -> Result<Option<Cow<'_, String>>> {
    for field in &self.fields {
      if field.name() == name {
        return match field.string_value() {
          Ok(Some(value)) => Ok(Some(value)),
          Ok(None) => Ok(None),
          Err(e) => Err(e),
        };
      }
    }
    Ok(None)
  }
  /// Removes all the fields from document.
  pub fn clear(&mut self) {
    self.fields.clear();
  }
}

impl fmt::Display for Document {
  /// Prints the fields of a document for human consumption.
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}<", std::any::type_name::<Self>())?;
    for (i, field) in self.fields.iter().enumerate() {
      write!(f, "{field}")?;
      if i != self.fields.len() - 1 {
        write!(f, " ")?;
      }
    }

    write!(f, ">")
  }
}
impl IntoIterator for Document {
  type Item = Fields;
  type IntoIter = IntoIter<Fields>;

  fn into_iter(self) -> Self::IntoIter {
    self.fields.into_iter()
  }
}
impl<'a> IntoIterator for &'a Document {
  type Item = &'a Fields;
  type IntoIter = std::slice::Iter<'a, Fields>;

  fn into_iter(self) -> Self::IntoIter {
    self.fields.iter()
  }
}

impl<'a> IntoIterator for &'a mut Document {
  type Item = &'a mut Fields;
  type IntoIter = std::slice::IterMut<'a, Fields>;

  fn into_iter(self) -> Self::IntoIter {
    self.fields.iter_mut()
  }
}
