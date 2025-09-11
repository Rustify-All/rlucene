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
// TODO: how to handle versioning here...?

use crate::core::analysis::analyzer::Analyzer;
use crate::core::analysis::reader::ReaderEnum;
use crate::core::analysis::token_stream::TokenStream;
use crate::core::document::invertable_field::InvertableType;
use crate::core::document::stored_value::StoredValue;
use crate::core::index::BytesRef;
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::number::Number;
use std::fmt::Display;
use std::rc::Rc;

/// Represents a single field for indexing. IndexWriter consumes
/// `Iterable<IndexableField>` as a document.
///
/// @lucene.experimental
pub trait IndexableField: Display {
    /// Field name
    fn name(&self) -> &str;

    /// {@link IndexableFieldType} describing the properties of this field.
    type FieldType: IndexableFieldType;
    fn field_type(&self) -> &Self::FieldType;
    /// Creates the TokenStream used for indexing this field. If appropriate,
    /// implementations should use the given Analyzer to create the
    /// TokenStreams.
    ///
    /// * `analyzer` - Analyzer that should be used to create the TokenStreams
    ///   from
    /// * `reuse` - TokenStream for a previous instance of this field **name**.
    ///   This allows custom field types (like StringField and NumericField)
    ///   that do not use the analyzer to still have good performance. Note: the
    ///   passed-in type may be inappropriate, for example if you mix up
    ///   different types of Fields for the same field name. So it's the
    ///   responsibility of the implementation to check.
    ///
    /// # Returns
    /// TokenStream value for indexing the document. Should always return a
    /// non-null value if the field is to be indexed.
    type TokenStream: TokenStream;
    fn token_stream<'a, A>(
        &'a mut self,
        _analyzer: &A,
    ) -> Result<Option<&'a mut Self::TokenStream>>
    where
        A: Analyzer;
    /// Non-null if this field has a binary value.
    fn binary_value(&self) -> Result<Option<Rc<BytesRef<Vec<u8>>>>>;

    /// Non-null if this field has a string value.
    fn string_value(&self) -> Result<Option<Rc<String>>>;
    /// Non-null if this field has a string value.
    fn get_char_sequence_value(&self) -> Result<Option<Rc<String>>> {
        self.string_value()
    }

    /// Non-null if this field has a Reader value.
    fn reader_value(&self) -> Result<Option<ReaderEnum>>;

    /// Non-null if this field has a numeric value.
    fn numeric_value(&self) -> Result<Option<Number>>;

    /// Stored value. This method is called to populate stored fields and must
    /// return a non-null value if the field stored.
    fn stored_value(&self) -> Result<Option<StoredValue>>;

    /// Describes how this field should be inverted. This must return a non-null
    /// value if the field indexes terms and postings.
    fn invertable_type(&self) -> &InvertableType;

    fn is_reserved(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    // TODO : IndexWriter not implemented
    #[allow(dead_code)] // for quick search
    struct TestIndexableField;
}
