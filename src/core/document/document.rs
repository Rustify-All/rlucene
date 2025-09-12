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
use crate::core::index::BytesRef;
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
    /// change the content of an existing index! In order to achieve this, a
    /// document has to be deleted from an index and a new changed version of
    /// that document has to be added.
    pub fn add(&mut self, field: impl Into<Fields>) {
        self.fields.push(field.into());
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
    /// A `Vec<BytesRef>` of binary field values.
    pub fn get_binary_values(&self, name: &str) -> Result<Vec<&BytesRef<Vec<u8>>>> {
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
    /// A `Option<BytesRef>` containing the binary field value, or `None`
    /// if no matching field is found.
    pub fn get_binary_value(&self, name: &str) -> Result<Option<&BytesRef<Vec<u8>>>> {
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

    /// Returns an array of `IndexableField`s with the given name. This method
    /// returns an empty array when there are no matching fields. It never
    /// returns `None`.
    ///
    /// # Parameters
    /// - `name`: the name of the field.
    ///
    /// # Returns
    /// A `Vec<Arc>` array containing the matching fields.
    pub fn get_fields_with_name(&self, name: &str) -> Vec<&Fields> {
        self.fields
            .iter()
            .filter(|field| field.name() == name)
            .collect()
    }

    /// Returns a `Vec<Arc>` containing all the fields in a document.
    ///
    /// # Note
    /// Fields that are not stored are not available in documents retrieved from
    /// the index, e.g., when using `StoredFields::document(int)`.
    ///
    /// # Returns
    /// An immutable `Vec<Arc>` containing all fields in the document.
    pub fn get_fields(&self) -> &[Fields] {
        &self.fields
    }
    /// Returns an array of values of the field specified by the `name`. This
    /// method returns an empty array when there are no matching fields. It
    /// never returns `None`. For a numeric `StoredField`, it returns the
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
    /// `StoredField`, it returns the string value of the number. To get the
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

#[cfg(test)]
mod tests {

    use crate::core::document::document::Document;
    use crate::core::document::field::{Field, Store};
    use crate::core::document::field_type::FieldType;
    use crate::core::document::stored_field::StoredField;
    use crate::core::document::string_field::StringField;
    use crate::core::document::text_field::TextField;
    use crate::core::index::index_options::IndexOptions;
    use crate::core::index::indexable_field::IndexableField;
    use crate::core::index::indexable_field_type::IndexableFieldType;
    use crate::core::util::error::lucene_error::Result;

    #[allow(dead_code)] // for quick search
    struct TestDocument;

    /// Tests the [`Document::remove_field`] method for a brand-new `Document`
    /// that has not been indexed yet.
    ///
    /// # Errors
    /// - Returns an error if an exception occurs during execution.
    #[test]
    fn test_binary_field() -> Result<()> {
        let binary_val = "this text will be stored as a byte array in the index";
        let binary_val2 = "this text will be also stored as a byte array in the index";

        let mut doc = Document::new();

        let mut ft = FieldType::new();
        ft.set_stored(true)?;
        let ft_arc = ft;

        let string_fld = Field::with_string("string", binary_val, ft_arc.clone())?;
        let binary_fld = StoredField::with_binary("binary", binary_val.as_bytes().to_vec())?;
        let binary_fld2 = StoredField::with_binary("binary", binary_val2.as_bytes().to_vec())?;

        assert!(binary_fld.binary_value()?.is_some());
        assert!(string_fld.field_type().stored());
        assert_eq!(binary_fld.field_type().index_options(), &IndexOptions::None);
        doc.add(binary_fld);
        doc.add(string_fld);

        assert_eq!(doc.get_fields().len(), 2);

        match doc.get_binary_value("binary")? {
            Some(bf) => {
                let bf_value = bf.utf8_to_string()?;
                assert_eq!(bf_value, binary_val);
            },
            None => {
                unreachable!()
            },
        }
        match doc.get("string")? {
            Some(sf) => {
                assert_eq!(sf, binary_val.to_string().into());
            },
            None => {
                unreachable!()
            },
        }

        doc.add(binary_fld2);
        assert_eq!(doc.get_fields().len(), 3);

        let binary_tests = doc.get_binary_values("binary")?;
        assert_eq!(binary_tests.len(), 2);

        let binary_test = binary_tests[0].utf8_to_string()?;
        let binary_test2 = binary_tests[1].utf8_to_string()?;

        assert_ne!(binary_test, binary_test2);
        assert_eq!(binary_test, binary_val);
        assert_eq!(binary_test2, binary_val2);
        doc.remove_field("string");
        assert_eq!(doc.get_fields().len(), 2);
        doc.remove_fields("binary");
        assert_eq!(doc.get_fields().len(), 0);
        Ok(())
    }
    /// Tests the [`Document::remove_field`] method for a brand-new `Document`
    /// that has not been indexed yet.
    ///
    /// # Errors
    /// - Returns an error if an exception occurs.
    #[test]
    fn test_remove_for_new_document() -> Result<()> {
        let mut doc = make_document_with_fields()?;
        assert_eq!(10, doc.get_fields().len());

        doc.remove_fields("keyword");
        assert_eq!(8, doc.get_fields().len());

        doc.remove_fields("doesnotexists"); // removing non-existing fields is
        doc.remove_fields("keyword"); // removing a field more than once
        assert_eq!(8, doc.get_fields().len());

        doc.remove_field("text");
        assert_eq!(7, doc.get_fields().len());

        doc.remove_field("text");
        assert_eq!(6, doc.get_fields().len());

        doc.remove_field("text");
        assert_eq!(6, doc.get_fields().len());

        doc.remove_field("doesnotexists"); // removing non-existing fields is
        assert_eq!(6, doc.get_fields().len());

        doc.remove_fields("unindexed");
        assert_eq!(4, doc.get_fields().len());

        doc.remove_fields("unstored");
        assert_eq!(2, doc.get_fields().len());

        doc.remove_fields("doesnotexists"); // removing non-existing fields is
        assert_eq!(2, doc.get_fields().len());

        doc.remove_fields("indexed_not_tokenized");
        assert_eq!(0, doc.get_fields().len());

        Ok(())
    }
    #[test]
    fn test_constructor_exceptions() -> Result<()> {
        // TODO : IndexWriter not implemented
        Ok(())
    }
    #[test]
    fn test_clear_document() -> Result<()> {
        let mut doc = make_document_with_fields()?;
        assert_eq!(doc.get_fields().len(), 10);
        doc.clear();
        assert_eq!(doc.get_fields().len(), 0);
        Ok(())
    }

    #[test]
    fn test_get_fields_immutable() -> Result<()> {
        //`fields` is an immutable slice.
        Ok(())
    }

    #[test]
    fn test_get_values_for_new_document() -> Result<()> {
        do_assert(&make_document_with_fields()?, false)
    }
    #[test]
    fn test_get_values_for_indexed_document() -> Result<()> {
        // TODO : IndexWriter not implemented
        Ok(())
    }
    #[test]
    fn test_get_values() -> Result<()> {
        let doc = make_document_with_fields()?;

        let keyword_values = doc.get_values("keyword")?;
        let keyword_str: Vec<&str> = keyword_values.iter().map(|s| s.as_str()).collect();
        assert_eq!(keyword_str, vec!["test1", "test2"]);

        let text_values = doc.get_values("text")?;
        let text_str: Vec<&str> = text_values.iter().map(|s| s.as_str()).collect();
        assert_eq!(text_str, vec!["test1", "test2"]);

        let unindexed_values = doc.get_values("unindexed")?;
        let unindexed_str: Vec<&str> = unindexed_values.iter().map(|s| s.as_str()).collect();
        assert_eq!(unindexed_str, vec!["test1", "test2"]);

        let nope_values = doc.get_values("nope")?;
        assert!(nope_values.is_empty());

        Ok(())
    }
    #[test]
    fn test_position_increment_multi_fields() -> Result<()> {
        // TODO : IndexWriter not implemented
        Ok(())
    }

    fn make_document_with_fields() -> Result<Document> {
        let mut doc = Document::new();
        let mut stored = FieldType::new();
        stored.set_stored(true)?;
        let mut indexed_not_tokenized = FieldType::new();
        indexed_not_tokenized.set_index_options(IndexOptions::DocsAndFreqsAndPositions)?;
        indexed_not_tokenized.set_tokenized(false)?;
        doc.add(StringField::with_string("keyword", "test1", Store::Yes)?);
        doc.add(StringField::with_string("keyword", "test2", Store::Yes)?);
        doc.add(TextField::with_string("text", "test1", Store::Yes)?);
        doc.add(TextField::with_string("text", "test2", Store::Yes)?);
        doc.add(Field::with_string("unindexed", "test1", stored.clone())?);
        doc.add(Field::with_string("unindexed", "test2", stored.clone())?);
        doc.add(TextField::with_string("unstored", "test1", Store::No)?);
        doc.add(TextField::with_string("unstored", "test2", Store::No)?);
        doc.add(Field::with_string(
            "indexed_not_tokenized",
            "test1",
            indexed_not_tokenized.clone(),
        )?);
        doc.add(Field::with_string(
            "indexed_not_tokenized",
            "test2",
            indexed_not_tokenized.clone(),
        )?);
        Ok(doc)
    }

    fn do_assert(doc: &Document, from_index: bool) -> Result<()> {
        let keyword_field_values = doc.get_fields_with_name("keyword");
        let text_field_values = doc.get_fields_with_name("text");
        let unindexed_field_values = doc.get_fields_with_name("unindexed");
        let unstored_field_values = doc.get_fields_with_name("unstored");

        assert_eq!(keyword_field_values.len(), 2);
        assert_eq!(text_field_values.len(), 2);
        assert_eq!(unindexed_field_values.len(), 2);
        // this test cannot work for documents retrieved from the index
        // since unstored fields will obviously not be returned
        if !from_index {
            assert_eq!(unstored_field_values.len(), 2);
        }

        assert_eq!(
            keyword_field_values[0].string_value()?.unwrap().as_ref(),
            "test1"
        );
        assert_eq!(
            keyword_field_values[1].string_value()?.unwrap().as_ref(),
            "test2"
        );
        assert_eq!(
            text_field_values[0].string_value()?.unwrap().as_ref(),
            "test1"
        );
        assert_eq!(
            text_field_values[1].string_value()?.unwrap().as_ref(),
            "test2"
        );
        assert_eq!(
            unindexed_field_values[0].string_value()?.unwrap().as_ref(),
            "test1"
        );
        assert_eq!(
            unindexed_field_values[1].string_value()?.unwrap().as_ref(),
            "test2"
        );
        // this test cannot work for documents retrieved from the index
        // since unstored fields will obviously not be returned
        if !from_index {
            assert_eq!(
                unstored_field_values[0].string_value()?.unwrap().as_ref(),
                "test1"
            );
            assert_eq!(
                unstored_field_values[1].string_value()?.unwrap().as_ref(),
                "test2"
            );
        }

        Ok(())
    }
    #[test]
    fn test_field_set_value() -> Result<()> {
        // TODO : IndexWriter not implemented
        Ok(())
    }

    #[test]
    fn test_invalid_fields() {
        // TODO : IndexWriter not implemented
    }

    #[test]
    fn test_numeric_field_as_string() -> Result<()> {
        // TODO : IndexWriter not implemented
        Ok(())
    }
}
