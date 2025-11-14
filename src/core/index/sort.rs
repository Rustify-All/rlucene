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
use crate::core::search::sort_field::{SortField, SortFiledBase};
use crate::core::search::sort_field_enum::SortFieldEnum;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::fmt;
use std::fmt::Display;
use std::hash::Hash;
/// Encapsulates sort criteria for returned hits.
///
/// A [`Sort`] can be created with an empty constructor, yielding an object
/// that instructs searches to return hits sorted by relevance; or it can be
/// created with one or more [`SortField`]s.
///
/// See also: [`SortField`].
#[derive(Clone)]
pub struct Sort {
    pub(crate) fields: Vec<SortFieldEnum>,
}

impl Sort {
    /// Represents sorting by index order.
    pub fn get_index_order() -> Result<Self> {
        let sort_field = SortFieldEnum::Sorter(SortField::get_field_doc()?);
        Self::with_fields(vec![sort_field])
    }
    /// Represents sorting by computed relevance. Using this sort criteria returns the same results as
    /// calling [`IndexSearcher::search(Query, i32)`](crate::core::search::index_searcher::IndexSearcher::search) without a sort criteria,
    /// only with slightly more overhead.
    pub fn get_relevance() -> Result<Self> {
        Self::new()
    }
    /// Returns true if the relevance score is needed to sort documents.
    pub fn needs_scores(&self) -> bool {
        for sort_field in &self.fields {
            if sort_field.needs_scores() {
                return true;
            }
        }
        false
    }
}

impl Sort {
    /// Sorts by computed relevance.
    ///
    /// This is the same sort criteria as calling `IndexSearcher::search`
    /// without a sort criteria, only with slightly more overhead.
    pub fn new() -> Result<Self> {
        let sort_field = SortFieldEnum::Sorter(SortField::get_field_score()?);
        Self::with_fields(vec![sort_field])
    }

    /// Sets the sort to the given criteria in succession.
    ///
    /// The first `SortField` is checked first, but if it produces a tie, then
    /// the second `SortField` is used to break the tie, and so on. Finally,
    /// if there is still a tie after all `SortField`s are checked, the
    /// internal Lucene doc ID is used to break it.
    ///
    /// # Arguments
    /// - `fields`: A vector of `SortField` to define the sorting order.
    ///
    /// # Errors
    /// Returns an error if the provided `fields` vector is empty.
    /// # Note
    /// You could use
    /// [`push`](crate::core::search::sort_field_enum::SortFieldVecExt::push_iterm)
    /// to init SortFieldEnum vector. # Example
    /// ```rust
    /// use rlucene::core::index::sort::Sort;
    /// use rlucene::core::search::sort_field::{SortField, SortFieldType};
    /// use rlucene::core::search::sort_field_enum::SortFieldVecExt;
    /// use rlucene::core::search::sorted_numeric_sort_field::SortedNumericSortField;
    /// use rlucene::core::search::sorted_set_sort_field::SortedSetSortField;
    /// let sort_field1 = SortField::new(Some("field1"), SortFieldType::Custom).unwrap();
    /// let sort_field2 = SortedSetSortField::new("field2", false).unwrap();
    /// let mut fileds = Vec::new();
    /// fileds.push_iterm(sort_field1);
    /// fileds.push_iterm(sort_field2);
    /// let sort = Sort::with_fields(fileds);
    /// assert!(sort.is_ok());
    /// ```
    pub fn with_fields<T>(fields: Vec<T>) -> Result<Self>
    where
        T: Into<SortFieldEnum>,
    {
        let fields: Vec<SortFieldEnum> = fields.into_iter().map(Into::into).collect();
        if fields.is_empty() {
            Err(LuceneError::illegal_argument(
                "There must be at least 1 sort field".to_string(),
            ))
        } else {
            Ok(Self { fields })
        }
    }

    /// Representation of the sort criteria.
    ///
    /// # Returns
    /// Array (Vec) of `SortField` objects used in this sort criteria.
    pub fn get_sort(&self) -> &[SortFieldEnum] {
        &self.fields
    }
    pub fn take_sort(&mut self) -> Vec<SortFieldEnum> {
        std::mem::take(&mut self.fields)
    }
}

impl Display for Sort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let fields_string = self
            .fields
            .iter()
            .map(|field| field.to_string())
            .collect::<Vec<_>>()
            .join(",");
        write!(f, "{fields_string}")
    }
}
impl PartialEq for Sort {
    fn eq(&self, other: &Self) -> bool {
        self.fields == other.fields
    }
}
impl Eq for Sort {}

impl Hash for Sort {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.fields.hash(state);
    }
}

#[cfg(test)]
mod tests {
    use crate::core::document::binary_doc_values_field::BinaryDocValuesField;
    use crate::core::document::document::Document;
    use crate::core::document::field::Store;
    use crate::core::document::sorted_doc_values_field::SortedDocValuesField;
    use crate::core::document::string_field::StringField;

    use crate::core::document::double_doc_values_field::DoubleDocValuesField;
    use crate::core::document::float_doc_values_field::FloatDocValuesField;
    use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
    use crate::core::index::sort::Sort;
    use crate::core::index::stored_fields::StoredFields;
    use crate::core::search::match_all_docs_query::MatchAllDocsQuery;
    use crate::core::search::sort_field::MissingValueEnum::StringFirst;
    use crate::core::search::sort_field::{SortField, SortFieldType, SortFiledBase};
    use crate::core::search::top_docs::TopDocsLike;
    use crate::core::util::error::lucene_error::Result;
    use crate::test::index::random_index_writer::RandomIndexWriter;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{
        new_bytes_ref_from_string, new_directory, new_searcher_with_reader, random,
    };
    use std::hash::DefaultHasher;
    use std::sync::Arc;
    use std::vec;

    #[allow(dead_code)] // for quick search
    struct TestSort;
    fn assert_equals_sort(a: &Sort, b: &Sort) {
        assert!(a == b);
        assert!(b == a);

        use std::hash::{Hash, Hasher};
        let mut ha = DefaultHasher::new();
        let mut hb = DefaultHasher::new();
        a.hash(&mut ha);
        b.hash(&mut hb);
        assert_eq!(ha.finish(), hb.finish());
    }
    fn assert_different_sort(a: &Sort, b: &Sort) {
        assert!(a != b);
        assert!(b != a);

        use std::hash::{Hash, Hasher};
        let mut ha = DefaultHasher::new();
        let mut hb = DefaultHasher::new();
        a.hash(&mut ha);
        b.hash(&mut hb);
        assert_ne!(ha.finish(), hb.finish());
    }
    #[test]
    fn test_equals() -> Result<()> {
        let sort_field1 = SortField::new("foo".into(), SortFieldType::String)?;

        let mut sort_field2 = SortField::new("foo".into(), SortFieldType::String)?;
        assert_equals_sort(
            &Sort::with_fields(vec![sort_field1.clone()])?,
            &Sort::with_fields(vec![sort_field2])?,
        );

        sort_field2 = SortField::new("bar".into(), SortFieldType::String)?;
        assert_different_sort(
            &Sort::with_fields(vec![sort_field1.clone()])?,
            &Sort::with_fields(vec![sort_field2])?,
        );

        sort_field2 = SortField::new("foo".into(), SortFieldType::Long)?;
        assert_different_sort(
            &Sort::with_fields(vec![sort_field1.clone()])?,
            &Sort::with_fields(vec![sort_field2])?,
        );

        sort_field2 = SortField::new("foo".into(), SortFieldType::String)?;
        sort_field2.set_missing_value(StringFirst)?;
        assert_different_sort(
            &Sort::with_fields(vec![sort_field1.clone()])?,
            &Sort::with_fields(vec![sort_field2])?,
        );

        sort_field2 = SortField::with_reverse("foo".into(), SortFieldType::String, false)?;
        assert_equals_sort(
            &Sort::with_fields(vec![sort_field1.clone()])?,
            &Sort::with_fields(vec![sort_field2])?,
        );

        sort_field2 = SortField::with_reverse("foo".into(), SortFieldType::String, true)?;
        assert_different_sort(
            &Sort::with_fields(vec![sort_field1])?,
            &Sort::with_fields(vec![sort_field2])?,
        );

        Ok(())
    }
    /// Tests sorting on type string
    #[test]
    fn test_string() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        let writer = RandomIndexWriter::new(&mut random, dir);

        let mut doc = Document::new();
        doc.add(SortedDocValuesField::new(
            "value",
            new_bytes_ref_from_string(&mut random, "foo")?,
        ));
        doc.add(StringField::with_string("value", "foo", Store::Yes)?);
        writer.add_document(doc)?;

        let mut doc = Document::new();
        doc.add(SortedDocValuesField::new(
            "value",
            new_bytes_ref_from_string(&mut random, "bar")?,
        ));
        doc.add(StringField::with_string("value", "bar", Store::Yes)?);
        writer.add_document(doc)?;

        let ir = writer.get_reader()?;
        writer.close()?;

        let mut searcher = new_searcher_with_reader(Arc::new(ir))?;
        let sort = Sort::with_fields(vec![SortField::new(Some("value"), SortFieldType::String)?])?;

        let td = searcher.search_with_sort(MatchAllDocsQuery::new(), 10, sort)?;
        assert_eq!(2, td.total_hits().value);

        // 'bar' comes before 'foo'
        let v0 = searcher
            .stored_fields()?
            .document(td.score_docs()[0].doc())?;
        assert_eq!("bar", v0.get("value")?.unwrap().as_ref());

        let v1 = searcher
            .stored_fields()?
            .document(td.score_docs()[1].doc())?;
        assert_eq!("foo", v1.get("value")?.unwrap().as_ref());

        Ok(())
    }
    /// Tests reverse sorting on type string
    #[test]
    fn test_string_reverse() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        let writer = RandomIndexWriter::new(&mut random, dir);

        // doc 1: bar
        let mut doc = Document::new();
        doc.add(SortedDocValuesField::new(
            "value",
            new_bytes_ref_from_string(&mut random, "bar")?,
        ));
        doc.add(StringField::with_string("value", "bar", Store::Yes)?);
        writer.add_document(doc)?;

        let mut doc = Document::new();
        doc.add(SortedDocValuesField::new(
            "value",
            new_bytes_ref_from_string(&mut random, "foo")?,
        ));
        doc.add(StringField::with_string("value", "foo", Store::Yes)?);
        writer.add_document(doc)?;

        let ir = writer.get_reader()?;
        writer.close()?;

        let mut searcher = new_searcher_with_reader(Arc::new(ir))?;
        let sort = Sort::with_fields(vec![SortField::with_reverse(
            Some("value"),
            SortFieldType::String,
            true,
        )?])?;

        let td = searcher.search_with_sort(MatchAllDocsQuery::new(), 10, sort)?;
        assert_eq!(2, td.total_hits().value);

        // reverse order: foo first, bar second
        let v0 = searcher
            .stored_fields()?
            .document(td.score_docs()[0].doc())?;
        assert_eq!("foo", v0.get("value")?.unwrap().as_ref());

        let v1 = searcher
            .stored_fields()?
            .document(td.score_docs()[1].doc())?;
        assert_eq!("bar", v1.get("value")?.unwrap().as_ref());

        Ok(())
    }
    /// Tests sorting on type string_val
    #[test]
    fn test_string_val() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        let writer = RandomIndexWriter::new(&mut random, dir);
        let mut doc = Document::new();
        doc.add(BinaryDocValuesField::new(
            "value",
            new_bytes_ref_from_string(&mut random, "foo")?,
        ));
        doc.add(StringField::with_string("value", "foo", Store::Yes)?);
        writer.add_document(doc)?;

        let mut doc = Document::new();
        doc.add(BinaryDocValuesField::new(
            "value",
            new_bytes_ref_from_string(&mut random, "bar")?,
        ));
        doc.add(StringField::with_string("value", "bar", Store::Yes)?);
        writer.add_document(doc)?;

        let ir = writer.get_reader()?;
        writer.close()?;

        let mut searcher = new_searcher_with_reader(Arc::new(ir))?;
        let sort = Sort::with_fields(vec![SortField::new(
            Some("value"),
            SortFieldType::StringVal,
        )?])?;

        let td = searcher.search_with_sort(MatchAllDocsQuery::new(), 10, sort)?;
        assert_eq!(2, td.total_hits().value);

        let v0 = searcher
            .stored_fields()?
            .document(td.score_docs()[0].doc())?;
        assert_eq!("bar", v0.get("value")?.unwrap().as_ref());

        let v1 = searcher
            .stored_fields()?
            .document(td.score_docs()[1].doc())?;
        assert_eq!("foo", v1.get("value")?.unwrap().as_ref());

        Ok(())
    }
    /// Tests reverse sorting on type string_val
    #[test]
    fn test_string_val_reverse() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        let writer = RandomIndexWriter::new(&mut random, dir);
        let mut doc = Document::new();
        doc.add(BinaryDocValuesField::new(
            "value",
            new_bytes_ref_from_string(&mut random, "bar")?,
        ));
        doc.add(StringField::with_string("value", "bar", Store::Yes)?);
        writer.add_document(doc)?;

        let mut doc = Document::new();
        doc.add(BinaryDocValuesField::new(
            "value",
            new_bytes_ref_from_string(&mut random, "foo")?,
        ));
        doc.add(StringField::with_string("value", "foo", Store::Yes)?);
        writer.add_document(doc)?;

        let ir = writer.get_reader()?;
        writer.close()?;

        let mut searcher = new_searcher_with_reader(Arc::new(ir))?;
        let sort = Sort::with_fields(vec![SortField::with_reverse(
            Some("value"),
            SortFieldType::StringVal,
            true,
        )?])?;

        let td = searcher.search_with_sort(MatchAllDocsQuery::new(), 10, sort)?;
        assert_eq!(2, td.total_hits().value);

        // reverse: foo first, bar second
        let v0 = searcher
            .stored_fields()?
            .document(td.score_docs()[0].doc())?;
        assert_eq!("foo", v0.get("value")?.unwrap().as_ref());

        let v1 = searcher
            .stored_fields()?
            .document(td.score_docs()[1].doc())?;
        assert_eq!("bar", v1.get("value")?.unwrap().as_ref());

        Ok(())
    }
    /// Tests sorting on type int
    #[test]
    fn test_int() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        let writer = RandomIndexWriter::new(&mut random, dir);

        let mut doc = Document::new();
        doc.add(NumericDocValuesField::new("value", 300000));
        doc.add(StringField::with_string("value", "300000", Store::Yes)?);
        writer.add_document(doc)?;

        let mut doc = Document::new();
        doc.add(NumericDocValuesField::new("value", -1));
        doc.add(StringField::with_string("value", "-1", Store::Yes)?);
        writer.add_document(doc)?;

        let mut doc = Document::new();
        doc.add(NumericDocValuesField::new("value", 4));
        doc.add(StringField::with_string("value", "4", Store::Yes)?);
        writer.add_document(doc)?;

        let ir = writer.get_reader()?;
        writer.close()?;

        let mut searcher = new_searcher_with_reader(Arc::new(ir))?;
        let sort = Sort::with_fields(vec![SortField::new(Some("value"), SortFieldType::Int)?])?;

        let td = searcher.search_with_sort(MatchAllDocsQuery::new(), 10, sort)?;
        assert_eq!(3, td.total_hits().value);

        let v0 = searcher
            .stored_fields()?
            .document(td.score_docs()[0].doc())?;
        assert_eq!("-1", v0.get("value")?.unwrap().as_ref());

        let v1 = searcher
            .stored_fields()?
            .document(td.score_docs()[1].doc())?;
        assert_eq!("4", v1.get("value")?.unwrap().as_ref());

        let v2 = searcher
            .stored_fields()?
            .document(td.score_docs()[2].doc())?;
        assert_eq!("300000", v2.get("value")?.unwrap().as_ref());

        Ok(())
    }
    /// Tests sorting on type int in reverse
    #[test]
    fn test_int_reverse() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        let writer = RandomIndexWriter::new(&mut random, dir);

        let mut doc = Document::new();
        doc.add(NumericDocValuesField::new("value", 300000));
        doc.add(StringField::with_string("value", "300000", Store::Yes)?);
        writer.add_document(doc)?;

        let mut doc = Document::new();
        doc.add(NumericDocValuesField::new("value", -1));
        doc.add(StringField::with_string("value", "-1", Store::Yes)?);
        writer.add_document(doc)?;

        let mut doc = Document::new();
        doc.add(NumericDocValuesField::new("value", 4));
        doc.add(StringField::with_string("value", "4", Store::Yes)?);
        writer.add_document(doc)?;

        let ir = writer.get_reader()?;
        writer.close()?;

        let mut searcher = new_searcher_with_reader(Arc::new(ir))?;
        let sort = Sort::with_fields(vec![SortField::with_reverse(
            Some("value"),
            SortFieldType::Int,
            true,
        )?])?;

        let td = searcher.search_with_sort(MatchAllDocsQuery::new(), 10, sort)?;
        assert_eq!(3, td.total_hits().value);

        let v0 = searcher
            .stored_fields()?
            .document(td.score_docs()[0].doc())?;
        assert_eq!("300000", v0.get("value")?.unwrap().as_ref());

        let v1 = searcher
            .stored_fields()?
            .document(td.score_docs()[1].doc())?;
        assert_eq!("4", v1.get("value")?.unwrap().as_ref());

        let v2 = searcher
            .stored_fields()?
            .document(td.score_docs()[2].doc())?;
        assert_eq!("-1", v2.get("value")?.unwrap().as_ref());

        Ok(())
    }
    /// Tests sorting on type int with a missing value
    #[test]
    fn test_int_missing() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        let writer = RandomIndexWriter::new(&mut random, dir);

        let doc = Document::new();
        writer.add_document(doc)?;

        let mut doc = Document::new();
        doc.add(NumericDocValuesField::new("value", -1));
        doc.add(StringField::with_string("value", "-1", Store::Yes)?);
        writer.add_document(doc)?;

        let mut doc = Document::new();
        doc.add(NumericDocValuesField::new("value", 4));
        doc.add(StringField::with_string("value", "4", Store::Yes)?);
        writer.add_document(doc)?;

        let ir = writer.get_reader()?;
        writer.close()?;

        let mut searcher = new_searcher_with_reader(Arc::new(ir))?;
        let sort = Sort::with_fields(vec![SortField::new(Some("value"), SortFieldType::Int)?])?;

        let td = searcher.search_with_sort(MatchAllDocsQuery::new(), 10, sort)?;
        assert_eq!(3, td.total_hits().value);

        let v0 = searcher
            .stored_fields()?
            .document(td.score_docs()[0].doc())?;
        assert_eq!("-1", v0.get("value")?.unwrap().as_ref());

        let v1 = searcher
            .stored_fields()?
            .document(td.score_docs()[1].doc())?;
        assert!(v1.get("value")?.is_none());

        let v2 = searcher
            .stored_fields()?
            .document(td.score_docs()[2].doc())?;
        assert_eq!("4", v2.get("value")?.unwrap().as_ref());

        Ok(())
    }
    /// Tests sorting on type int, specifying the missing value should be treated as Integer.MAX_VALUE
    #[test]
    fn test_int_missing_last() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        let writer = RandomIndexWriter::new(&mut random, dir);

        let doc = Document::new();
        writer.add_document(doc)?;

        let mut doc = Document::new();
        doc.add(NumericDocValuesField::new("value", -1));
        doc.add(StringField::with_string("value", "-1", Store::Yes)?);
        writer.add_document(doc)?;

        let mut doc = Document::new();
        doc.add(NumericDocValuesField::new("value", 4));
        doc.add(StringField::with_string("value", "4", Store::Yes)?);
        writer.add_document(doc)?;

        let ir = writer.get_reader()?;
        writer.close()?;

        let mut searcher = new_searcher_with_reader(Arc::new(ir))?;

        let mut sort_field = SortField::new("value".into(), SortFieldType::Int)?;
        sort_field.set_missing_value(i32::MAX)?;
        let sort = Sort::with_fields(vec![sort_field])?;

        let td = searcher.search_with_sort(MatchAllDocsQuery::new(), 10, sort)?;
        assert_eq!(3, td.total_hits().value);

        let v0 = searcher
            .stored_fields()?
            .document(td.score_docs()[0].doc())?;
        assert_eq!("-1", v0.get("value")?.unwrap().as_ref());

        let v1 = searcher
            .stored_fields()?
            .document(td.score_docs()[1].doc())?;
        assert_eq!("4", v1.get("value")?.unwrap().as_ref());

        let v2 = searcher
            .stored_fields()?
            .document(td.score_docs()[2].doc())?;
        assert!(v2.get("value")?.is_none());

        Ok(())
    }

    /// Tests sorting on type long
    #[test]
    fn test_long() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        let writer = RandomIndexWriter::new(&mut random, dir);

        let mut doc = Document::new();
        doc.add(NumericDocValuesField::new("value", 3_000_000_000i64));
        doc.add(StringField::with_string("value", "3000000000", Store::Yes)?);
        writer.add_document(doc)?;

        let mut doc = Document::new();
        doc.add(NumericDocValuesField::new("value", -1i64));
        doc.add(StringField::with_string("value", "-1", Store::Yes)?);
        writer.add_document(doc)?;

        let mut doc = Document::new();
        doc.add(NumericDocValuesField::new("value", 4i64));
        doc.add(StringField::with_string("value", "4", Store::Yes)?);
        writer.add_document(doc)?;

        let ir = writer.get_reader()?;
        writer.close()?;

        let mut searcher = new_searcher_with_reader(Arc::new(ir))?;
        let sort = Sort::with_fields(vec![SortField::new(Some("value"), SortFieldType::Long)?])?;

        let td = searcher.search_with_sort(MatchAllDocsQuery::new(), 10, sort)?;
        assert_eq!(3, td.total_hits().value);

        // numeric: -1, 4, 3000000000
        let v0 = searcher
            .stored_fields()?
            .document(td.score_docs()[0].doc())?;
        assert_eq!("-1", v0.get("value")?.unwrap().as_ref());

        let v1 = searcher
            .stored_fields()?
            .document(td.score_docs()[1].doc())?;
        assert_eq!("4", v1.get("value")?.unwrap().as_ref());

        let v2 = searcher
            .stored_fields()?
            .document(td.score_docs()[2].doc())?;
        assert_eq!("3000000000", v2.get("value")?.unwrap().as_ref());

        Ok(())
    }
    /// Tests sorting on type long in reverse
    #[test]
    fn test_long_reverse() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        let writer = RandomIndexWriter::new(&mut random, dir);

        // doc 1: 3000000000
        let mut doc = Document::new();
        doc.add(NumericDocValuesField::new("value", 3_000_000_000i64));
        doc.add(StringField::with_string("value", "3000000000", Store::Yes)?);
        writer.add_document(doc)?;

        // doc 2: -1
        let mut doc = Document::new();
        doc.add(NumericDocValuesField::new("value", -1i64));
        doc.add(StringField::with_string("value", "-1", Store::Yes)?);
        writer.add_document(doc)?;

        // doc 3: 4
        let mut doc = Document::new();
        doc.add(NumericDocValuesField::new("value", 4i64));
        doc.add(StringField::with_string("value", "4", Store::Yes)?);
        writer.add_document(doc)?;

        let ir = writer.get_reader()?;
        writer.close()?;

        let mut searcher = new_searcher_with_reader(Arc::new(ir))?;
        let sort = Sort::with_fields(vec![SortField::with_reverse(
            Some("value"),
            SortFieldType::Long,
            true,
        )?])?;

        let td = searcher.search_with_sort(MatchAllDocsQuery::new(), 10, sort)?;
        assert_eq!(3, td.total_hits().value);

        // reverse numeric order: 3000000000, 4, -1
        let v0 = searcher
            .stored_fields()?
            .document(td.score_docs()[0].doc())?;
        assert_eq!("3000000000", v0.get("value")?.unwrap().as_ref());

        let v1 = searcher
            .stored_fields()?
            .document(td.score_docs()[1].doc())?;
        assert_eq!("4", v1.get("value")?.unwrap().as_ref());

        let v2 = searcher
            .stored_fields()?
            .document(td.score_docs()[2].doc())?;
        assert_eq!("-1", v2.get("value")?.unwrap().as_ref());

        Ok(())
    }
    /// Tests sorting on type long with a missing value
    #[test]
    fn test_long_missing() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        let writer = RandomIndexWriter::new(&mut random, dir);

        let doc = Document::new();
        writer.add_document(doc)?;

        let mut doc = Document::new();
        doc.add(NumericDocValuesField::new("value", -1i64));
        doc.add(StringField::with_string("value", "-1", Store::Yes)?);
        writer.add_document(doc)?;

        let mut doc = Document::new();
        doc.add(NumericDocValuesField::new("value", 4i64));
        doc.add(StringField::with_string("value", "4", Store::Yes)?);
        writer.add_document(doc)?;

        let ir = writer.get_reader()?;
        writer.close()?;

        let mut searcher = new_searcher_with_reader(Arc::new(ir))?;
        let sort = Sort::with_fields(vec![SortField::new(Some("value"), SortFieldType::Long)?])?;

        let td = searcher.search_with_sort(MatchAllDocsQuery::new(), 10, sort)?;
        assert_eq!(3, td.total_hits().value);

        // null treated as 0 → -1, null, 4
        let v0 = searcher
            .stored_fields()?
            .document(td.score_docs()[0].doc())?;
        assert_eq!("-1", v0.get("value")?.unwrap().as_ref());

        let v1 = searcher
            .stored_fields()?
            .document(td.score_docs()[1].doc())?;
        assert!(v1.get("value")?.is_none());

        let v2 = searcher
            .stored_fields()?
            .document(td.score_docs()[2].doc())?;
        assert_eq!("4", v2.get("value")?.unwrap().as_ref());

        Ok(())
    }
    /// Tests sorting on type long, specifying the missing value should be treated as Long.MAX_VALUE
    #[test]
    fn test_long_missing_last() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        let writer = RandomIndexWriter::new(&mut random, dir);

        let doc = Document::new();
        writer.add_document(doc)?;

        let mut doc = Document::new();
        doc.add(NumericDocValuesField::new("value", -1i64));
        doc.add(StringField::with_string("value", "-1", Store::Yes)?);
        writer.add_document(doc)?;

        let mut doc = Document::new();
        doc.add(NumericDocValuesField::new("value", 4i64));
        doc.add(StringField::with_string("value", "4", Store::Yes)?);
        writer.add_document(doc)?;

        let ir = writer.get_reader()?;
        writer.close()?;

        let mut searcher = new_searcher_with_reader(Arc::new(ir))?;

        let mut sort_field = SortField::new("value".into(), SortFieldType::Long)?;
        sort_field.set_missing_value(i64::MAX)?;
        let sort = Sort::with_fields(vec![sort_field])?;

        let td = searcher.search_with_sort(MatchAllDocsQuery::new(), 10, sort)?;
        assert_eq!(3, td.total_hits().value);

        let v0 = searcher
            .stored_fields()?
            .document(td.score_docs()[0].doc())?;
        assert_eq!("-1", v0.get("value")?.unwrap().as_ref());

        let v1 = searcher
            .stored_fields()?
            .document(td.score_docs()[1].doc())?;
        assert_eq!("4", v1.get("value")?.unwrap().as_ref());

        let v2 = searcher
            .stored_fields()?
            .document(td.score_docs()[2].doc())?;
        assert!(v2.get("value")?.is_none());

        Ok(())
    }
    /// Tests sorting on type float
    #[test]
    fn test_float() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        let writer = RandomIndexWriter::new(&mut random, dir);

        // 30.1
        let mut doc = Document::new();
        doc.add(FloatDocValuesField::new("value", 30.1f32));
        doc.add(StringField::with_string("value", "30.1", Store::Yes)?);
        writer.add_document(doc)?;

        // -1.3
        let mut doc = Document::new();
        doc.add(FloatDocValuesField::new("value", -1.3f32));
        doc.add(StringField::with_string("value", "-1.3", Store::Yes)?);
        writer.add_document(doc)?;

        // 4.2
        let mut doc = Document::new();
        doc.add(FloatDocValuesField::new("value", 4.2f32));
        doc.add(StringField::with_string("value", "4.2", Store::Yes)?);
        writer.add_document(doc)?;

        let ir = writer.get_reader()?;
        writer.close()?;

        let mut searcher = new_searcher_with_reader(Arc::new(ir))?;
        let sort = Sort::with_fields(vec![SortField::new(Some("value"), SortFieldType::Float)?])?;

        let td = searcher.search_with_sort(MatchAllDocsQuery::new(), 10, sort)?;
        assert_eq!(3, td.total_hits().value);

        // numeric: -1.3, 4.2, 30.1
        let v0 = searcher
            .stored_fields()?
            .document(td.score_docs()[0].doc())?;
        assert_eq!("-1.3", v0.get("value")?.unwrap().as_ref());

        let v1 = searcher
            .stored_fields()?
            .document(td.score_docs()[1].doc())?;
        assert_eq!("4.2", v1.get("value")?.unwrap().as_ref());

        let v2 = searcher
            .stored_fields()?
            .document(td.score_docs()[2].doc())?;
        assert_eq!("30.1", v2.get("value")?.unwrap().as_ref());

        Ok(())
    }
    /// Tests sorting on type float in reverse
    #[test]
    fn test_float_reverse() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        let writer = RandomIndexWriter::new(&mut random, dir);

        // 30.1
        let mut doc = Document::new();
        doc.add(FloatDocValuesField::new("value", 30.1f32));
        doc.add(StringField::with_string("value", "30.1", Store::Yes)?);
        writer.add_document(doc)?;

        // -1.3
        let mut doc = Document::new();
        doc.add(FloatDocValuesField::new("value", -1.3f32));
        doc.add(StringField::with_string("value", "-1.3", Store::Yes)?);
        writer.add_document(doc)?;

        // 4.2
        let mut doc = Document::new();
        doc.add(FloatDocValuesField::new("value", 4.2f32));
        doc.add(StringField::with_string("value", "4.2", Store::Yes)?);
        writer.add_document(doc)?;

        let ir = writer.get_reader()?;
        writer.close()?;

        let mut searcher = new_searcher_with_reader(Arc::new(ir))?;
        let sort = Sort::with_fields(vec![SortField::with_reverse(
            Some("value"),
            SortFieldType::Float,
            true,
        )?])?;

        let td = searcher.search_with_sort(MatchAllDocsQuery::new(), 10, sort)?;
        assert_eq!(3, td.total_hits().value);

        // reverse: 30.1, 4.2, -1.3
        let v0 = searcher
            .stored_fields()?
            .document(td.score_docs()[0].doc())?;
        assert_eq!("30.1", v0.get("value")?.unwrap().as_ref());

        let v1 = searcher
            .stored_fields()?
            .document(td.score_docs()[1].doc())?;
        assert_eq!("4.2", v1.get("value")?.unwrap().as_ref());

        let v2 = searcher
            .stored_fields()?
            .document(td.score_docs()[2].doc())?;
        assert_eq!("-1.3", v2.get("value")?.unwrap().as_ref());

        Ok(())
    }
    /// Tests sorting on type float with a missing value
    #[test]
    fn test_float_missing() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        let writer = RandomIndexWriter::new(&mut random, dir);

        // missing
        writer.add_document(Document::new())?;

        // -1.3
        let mut doc = Document::new();
        doc.add(FloatDocValuesField::new("value", -1.3f32));
        doc.add(StringField::with_string("value", "-1.3", Store::Yes)?);
        writer.add_document(doc)?;

        // 4.2
        let mut doc = Document::new();
        doc.add(FloatDocValuesField::new("value", 4.2f32));
        doc.add(StringField::with_string("value", "4.2", Store::Yes)?);
        writer.add_document(doc)?;

        let ir = writer.get_reader()?;
        writer.close()?;

        let mut searcher = new_searcher_with_reader(Arc::new(ir))?;
        let sort = Sort::with_fields(vec![SortField::new(Some("value"), SortFieldType::Float)?])?;

        let td = searcher.search_with_sort(MatchAllDocsQuery::new(), 10, sort)?;
        assert_eq!(3, td.total_hits().value);

        // null treated as 0 → -1.3, null, 4.2
        let v0 = searcher
            .stored_fields()?
            .document(td.score_docs()[0].doc())?;
        assert_eq!("-1.3", v0.get("value")?.unwrap().as_ref());

        let v1 = searcher
            .stored_fields()?
            .document(td.score_docs()[1].doc())?;
        assert!(v1.get("value")?.is_none());

        let v2 = searcher
            .stored_fields()?
            .document(td.score_docs()[2].doc())?;
        assert_eq!("4.2", v2.get("value")?.unwrap().as_ref());

        Ok(())
    }
    /// Tests sorting on type float, specifying the missing value should be treated as Float.MAX_VALUE
    #[test]
    fn test_float_missing_last() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        let writer = RandomIndexWriter::new(&mut random, dir);

        // missing
        writer.add_document(Document::new())?;

        // -1.3
        let mut doc = Document::new();
        doc.add(FloatDocValuesField::new("value", -1.3f32));
        doc.add(StringField::with_string("value", "-1.3", Store::Yes)?);
        writer.add_document(doc)?;

        // 4.2
        let mut doc = Document::new();
        doc.add(FloatDocValuesField::new("value", 4.2f32));
        doc.add(StringField::with_string("value", "4.2", Store::Yes)?);
        writer.add_document(doc)?;

        let ir = writer.get_reader()?;
        writer.close()?;

        let mut searcher = new_searcher_with_reader(Arc::new(ir))?;

        let mut sort_field = SortField::new("value".into(), SortFieldType::Float)?;
        sort_field.set_missing_value(f32::MAX)?;
        let sort = Sort::with_fields(vec![sort_field])?;

        let td = searcher.search_with_sort(MatchAllDocsQuery::new(), 10, sort)?;
        assert_eq!(3, td.total_hits().value);

        // null → Float.MAX_VALUE
        let v0 = searcher
            .stored_fields()?
            .document(td.score_docs()[0].doc())?;
        assert_eq!("-1.3", v0.get("value")?.unwrap().as_ref());

        let v1 = searcher
            .stored_fields()?
            .document(td.score_docs()[1].doc())?;
        assert_eq!("4.2", v1.get("value")?.unwrap().as_ref());

        let v2 = searcher
            .stored_fields()?
            .document(td.score_docs()[2].doc())?;
        assert!(v2.get("value")?.is_none());

        Ok(())
    }
    /// Tests sorting on type double
    #[test]
    fn test_double() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        let writer = RandomIndexWriter::new(&mut random, dir);

        // 30.1
        let mut doc = Document::new();
        doc.add(DoubleDocValuesField::new("value", 30.1f64));
        doc.add(StringField::with_string("value", "30.1", Store::Yes)?);
        writer.add_document(doc)?;

        // -1.3
        let mut doc = Document::new();
        doc.add(DoubleDocValuesField::new("value", -1.3f64));
        doc.add(StringField::with_string("value", "-1.3", Store::Yes)?);
        writer.add_document(doc)?;

        // 4.2333333333333
        let mut doc = Document::new();
        doc.add(DoubleDocValuesField::new("value", 4.2333333333333f64));
        doc.add(StringField::with_string(
            "value",
            "4.2333333333333",
            Store::Yes,
        )?);
        writer.add_document(doc)?;

        // 4.2333333333332
        let mut doc = Document::new();
        doc.add(DoubleDocValuesField::new("value", 4.2333333333332f64));
        doc.add(StringField::with_string(
            "value",
            "4.2333333333332",
            Store::Yes,
        )?);
        writer.add_document(doc)?;

        let ir = writer.get_reader()?;
        writer.close()?;

        let mut searcher = new_searcher_with_reader(Arc::new(ir))?;
        let sort = Sort::with_fields(vec![SortField::new(Some("value"), SortFieldType::Double)?])?;

        let td = searcher.search_with_sort(MatchAllDocsQuery::new(), 10, sort)?;
        assert_eq!(4, td.total_hits().value);

        // numeric order
        let v0 = searcher
            .stored_fields()?
            .document(td.score_docs()[0].doc())?;
        assert_eq!("-1.3", v0.get("value")?.unwrap().as_ref());

        let v1 = searcher
            .stored_fields()?
            .document(td.score_docs()[1].doc())?;
        assert_eq!("4.2333333333332", v1.get("value")?.unwrap().as_ref());

        let v2 = searcher
            .stored_fields()?
            .document(td.score_docs()[2].doc())?;
        assert_eq!("4.2333333333333", v2.get("value")?.unwrap().as_ref());

        let v3 = searcher
            .stored_fields()?
            .document(td.score_docs()[3].doc())?;
        assert_eq!("30.1", v3.get("value")?.unwrap().as_ref());

        Ok(())
    }
    /// Tests sorting on type double with +/- zero
    #[test]
    fn test_double_signed_zero() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        let writer = RandomIndexWriter::new(&mut random, dir);

        // +0
        let mut doc = Document::new();
        doc.add(DoubleDocValuesField::new("value", 0.0f64));
        doc.add(StringField::with_string("value", "+0", Store::Yes)?);
        writer.add_document(doc)?;

        // -0
        let mut doc = Document::new();
        doc.add(DoubleDocValuesField::new("value", -0.0f64));
        doc.add(StringField::with_string("value", "-0", Store::Yes)?);
        writer.add_document(doc)?;

        let ir = writer.get_reader()?;
        writer.close()?;

        let mut searcher = new_searcher_with_reader(Arc::new(ir))?;
        let sort = Sort::with_fields(vec![SortField::new(Some("value"), SortFieldType::Double)?])?;

        let td = searcher.search_with_sort(MatchAllDocsQuery::new(), 10, sort)?;
        assert_eq!(2, td.total_hits().value);

        // -0 < +0
        let v0 = searcher
            .stored_fields()?
            .document(td.score_docs()[0].doc())?;
        assert_eq!("-0", v0.get("value")?.unwrap().as_ref());

        let v1 = searcher
            .stored_fields()?
            .document(td.score_docs()[1].doc())?;
        assert_eq!("+0", v1.get("value")?.unwrap().as_ref());

        Ok(())
    }
    /// Tests sorting on type double in reverse
    #[test]
    fn test_double_reverse() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        let writer = RandomIndexWriter::new(&mut random, dir);

        // 30.1
        let mut doc = Document::new();
        doc.add(DoubleDocValuesField::new("value", 30.1f64));
        doc.add(StringField::with_string("value", "30.1", Store::Yes)?);
        writer.add_document(doc)?;

        // -1.3
        let mut doc = Document::new();
        doc.add(DoubleDocValuesField::new("value", -1.3f64));
        doc.add(StringField::with_string("value", "-1.3", Store::Yes)?);
        writer.add_document(doc)?;

        // 4.2333333333333
        let mut doc = Document::new();
        doc.add(DoubleDocValuesField::new("value", 4.2333333333333f64));
        doc.add(StringField::with_string(
            "value",
            "4.2333333333333",
            Store::Yes,
        )?);
        writer.add_document(doc)?;

        // 4.2333333333332
        let mut doc = Document::new();
        doc.add(DoubleDocValuesField::new("value", 4.2333333333332f64));
        doc.add(StringField::with_string(
            "value",
            "4.2333333333332",
            Store::Yes,
        )?);
        writer.add_document(doc)?;

        let ir = writer.get_reader()?;
        writer.close()?;

        let mut searcher = new_searcher_with_reader(Arc::new(ir))?;
        let sort = Sort::with_fields(vec![SortField::with_reverse(
            Some("value"),
            SortFieldType::Double,
            true,
        )?])?;

        let td = searcher.search_with_sort(MatchAllDocsQuery::new(), 10, sort)?;
        assert_eq!(4, td.total_hits().value);

        // reverse numeric order
        let v0 = searcher
            .stored_fields()?
            .document(td.score_docs()[0].doc())?;
        assert_eq!("30.1", v0.get("value")?.unwrap().as_ref());

        let v1 = searcher
            .stored_fields()?
            .document(td.score_docs()[1].doc())?;
        assert_eq!("4.2333333333333", v1.get("value")?.unwrap().as_ref());

        let v2 = searcher
            .stored_fields()?
            .document(td.score_docs()[2].doc())?;
        assert_eq!("4.2333333333332", v2.get("value")?.unwrap().as_ref());

        let v3 = searcher
            .stored_fields()?
            .document(td.score_docs()[3].doc())?;
        assert_eq!("-1.3", v3.get("value")?.unwrap().as_ref());

        Ok(())
    }
    /// Tests sorting on type double with a missing value
    #[test]
    fn test_double_missing() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        let writer = RandomIndexWriter::new(&mut random, dir);

        // missing
        writer.add_document(Document::new())?;

        // -1.3
        let mut doc = Document::new();
        doc.add(DoubleDocValuesField::new("value", -1.3f64));
        doc.add(StringField::with_string("value", "-1.3", Store::Yes)?);
        writer.add_document(doc)?;

        // 4.2333333333333
        let mut doc = Document::new();
        doc.add(DoubleDocValuesField::new("value", 4.2333333333333f64));
        doc.add(StringField::with_string(
            "value",
            "4.2333333333333",
            Store::Yes,
        )?);
        writer.add_document(doc)?;

        // 4.2333333333332
        let mut doc = Document::new();
        doc.add(DoubleDocValuesField::new("value", 4.2333333333332f64));
        doc.add(StringField::with_string(
            "value",
            "4.2333333333332",
            Store::Yes,
        )?);
        writer.add_document(doc)?;

        let ir = writer.get_reader()?;
        writer.close()?;

        let mut searcher = new_searcher_with_reader(Arc::new(ir))?;
        let sort = Sort::with_fields(vec![SortField::new(Some("value"), SortFieldType::Double)?])?;

        let td = searcher.search_with_sort(MatchAllDocsQuery::new(), 10, sort)?;
        assert_eq!(4, td.total_hits().value);

        // null treated as 0
        let v0 = searcher
            .stored_fields()?
            .document(td.score_docs()[0].doc())?;
        assert_eq!("-1.3", v0.get("value")?.unwrap().as_ref());

        let v1 = searcher
            .stored_fields()?
            .document(td.score_docs()[1].doc())?;
        assert!(v1.get("value")?.is_none());

        let v2 = searcher
            .stored_fields()?
            .document(td.score_docs()[2].doc())?;
        assert_eq!("4.2333333333332", v2.get("value")?.unwrap().as_ref());

        let v3 = searcher
            .stored_fields()?
            .document(td.score_docs()[3].doc())?;
        assert_eq!("4.2333333333333", v3.get("value")?.unwrap().as_ref());

        Ok(())
    }

    /// Tests sorting on type double, specifying the missing value should be treated as Double.MAX_VALUE
    #[test]
    fn test_double_missing_last() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        let writer = RandomIndexWriter::new(&mut random, dir);

        // missing
        writer.add_document(Document::new())?;

        // -1.3
        let mut doc = Document::new();
        doc.add(DoubleDocValuesField::new("value", -1.3f64));
        doc.add(StringField::with_string("value", "-1.3", Store::Yes)?);
        writer.add_document(doc)?;

        // 4.2333333333333
        let mut doc = Document::new();
        doc.add(DoubleDocValuesField::new("value", 4.2333333333333f64));
        doc.add(StringField::with_string(
            "value",
            "4.2333333333333",
            Store::Yes,
        )?);
        writer.add_document(doc)?;

        // 4.2333333333332
        let mut doc = Document::new();
        doc.add(DoubleDocValuesField::new("value", 4.2333333333332f64));
        doc.add(StringField::with_string(
            "value",
            "4.2333333333332",
            Store::Yes,
        )?);
        writer.add_document(doc)?;

        let ir = writer.get_reader()?;
        writer.close()?;

        let mut searcher = new_searcher_with_reader(Arc::new(ir))?;

        let mut sort_field = SortField::new("value".into(), SortFieldType::Double)?;
        sort_field.set_missing_value(f64::MAX)?;
        let sort = Sort::with_fields(vec![sort_field])?;

        let td = searcher.search_with_sort(MatchAllDocsQuery::new(), 10, sort)?;
        assert_eq!(4, td.total_hits().value);

        let v0 = searcher
            .stored_fields()?
            .document(td.score_docs()[0].doc())?;
        assert_eq!("-1.3", v0.get("value")?.unwrap().as_ref());

        let v1 = searcher
            .stored_fields()?
            .document(td.score_docs()[1].doc())?;
        assert_eq!("4.2333333333332", v1.get("value")?.unwrap().as_ref());

        let v2 = searcher
            .stored_fields()?
            .document(td.score_docs()[2].doc())?;
        assert_eq!("4.2333333333333", v2.get("value")?.unwrap().as_ref());

        let v3 = searcher
            .stored_fields()?
            .document(td.score_docs()[3].doc())?;
        assert!(v3.get("value")?.is_none());

        Ok(())
    }
    /// Tests sorting on multiple sort fields
    #[test]
    fn test_multi_sort() -> Result<()> {
        let mut random = random();
        let dir = Arc::new(new_directory(&mut random)?);
        let writer = RandomIndexWriter::new(&mut random, dir);

        // doc1: foo, 0
        let mut doc = Document::new();
        doc.add(SortedDocValuesField::new(
            "value1",
            new_bytes_ref_from_string(&mut random, "foo")?,
        ));
        doc.add(NumericDocValuesField::new("value2", 0));
        doc.add(StringField::with_string("value1", "foo", Store::Yes)?);
        doc.add(StringField::with_string("value2", "0", Store::Yes)?);
        writer.add_document(doc)?;

        // doc2: bar, 1
        let mut doc = Document::new();
        doc.add(SortedDocValuesField::new(
            "value1",
            new_bytes_ref_from_string(&mut random, "bar")?,
        ));
        doc.add(NumericDocValuesField::new("value2", 1));
        doc.add(StringField::with_string("value1", "bar", Store::Yes)?);
        doc.add(StringField::with_string("value2", "1", Store::Yes)?);
        writer.add_document(doc)?;

        // doc3: bar, 0
        let mut doc = Document::new();
        doc.add(SortedDocValuesField::new(
            "value1",
            new_bytes_ref_from_string(&mut random, "bar")?,
        ));
        doc.add(NumericDocValuesField::new("value2", 0));
        doc.add(StringField::with_string("value1", "bar", Store::Yes)?);
        doc.add(StringField::with_string("value2", "0", Store::Yes)?);
        writer.add_document(doc)?;

        // doc4: foo, 1
        let mut doc = Document::new();
        doc.add(SortedDocValuesField::new(
            "value1",
            new_bytes_ref_from_string(&mut random, "foo")?,
        ));
        doc.add(NumericDocValuesField::new("value2", 1));
        doc.add(StringField::with_string("value1", "foo", Store::Yes)?);
        doc.add(StringField::with_string("value2", "1", Store::Yes)?);
        writer.add_document(doc)?;

        let ir = writer.get_reader()?;
        writer.close()?;

        let mut searcher = new_searcher_with_reader(Arc::new(ir))?;
        let sort = Arc::new(Sort::with_fields(vec![
            SortField::new(Some("value1"), SortFieldType::String)?,
            SortField::new(Some("value2"), SortFieldType::Long)?,
        ])?);

        let td = searcher.search_with_sort(MatchAllDocsQuery::new(), 10, sort.clone())?;
        assert_eq!(4, td.total_hits().value);

        // bar < foo
        let v0 = searcher
            .stored_fields()?
            .document(td.score_docs()[0].doc())?;
        let v1 = searcher
            .stored_fields()?
            .document(td.score_docs()[1].doc())?;
        let v2 = searcher
            .stored_fields()?
            .document(td.score_docs()[2].doc())?;
        let v3 = searcher
            .stored_fields()?
            .document(td.score_docs()[3].doc())?;

        assert_eq!("bar", v0.get("value1")?.unwrap().as_ref());
        assert_eq!("bar", v1.get("value1")?.unwrap().as_ref());
        assert_eq!("foo", v2.get("value1")?.unwrap().as_ref());
        assert_eq!("foo", v3.get("value1")?.unwrap().as_ref());

        assert_eq!("0", v0.get("value2")?.unwrap().as_ref());
        assert_eq!("1", v1.get("value2")?.unwrap().as_ref());
        assert_eq!("0", v2.get("value2")?.unwrap().as_ref());
        assert_eq!("1", v3.get("value2")?.unwrap().as_ref());

        // overflow = top 1
        let td2 = searcher.search_with_sort(MatchAllDocsQuery::new(), 1, sort)?;
        assert_eq!(4, td2.total_hits().value);

        let v = searcher
            .stored_fields()?
            .document(td2.score_docs()[0].doc())?;
        assert_eq!("bar", v.get("value1")?.unwrap().as_ref());
        assert_eq!("0", v.get("value2")?.unwrap().as_ref());

        Ok(())
    }
    #[test]
    fn test_rewrite() -> Result<()> {
        // TODO rewrite未实现
        Ok(())
    }
    #[test]
    fn test_string_ghost() -> Result<()> {
        do_test_string_ghost(true)?;
        do_test_string_ghost(false)?;
        Ok(())
    }

    fn do_test_string_ghost(_indexed: bool) -> Result<()> {
        // TODO merge 未实现
        Ok(())
    }

    #[test]
    fn test_int_ghost() -> Result<()> {
        do_test_string_ghost(true)?;
        do_test_string_ghost(false)?;
        Ok(())
    }

    fn do_test_int_ghost(_indexed: bool) -> Result<()> {
        // TODO merge 未实现
        Ok(())
    }
    #[test]
    fn test_long_ghost() -> Result<()> {
        do_test_string_ghost(true)?;
        do_test_string_ghost(false)?;
        Ok(())
    }

    fn do_test_long_ghost(_indexed: bool) -> Result<()> {
        // TODO merge 未实现
        Ok(())
    }
    #[test]
    fn test_double_ghost() -> Result<()> {
        do_test_string_ghost(true)?;
        do_test_string_ghost(false)?;
        Ok(())
    }

    fn do_test_double_ghost(_indexed: bool) -> Result<()> {
        // TODO merge 未实现
        Ok(())
    }
    #[test]
    fn test_float_ghost() -> Result<()> {
        do_test_string_ghost(true)?;
        do_test_string_ghost(false)?;
        Ok(())
    }

    fn do_test_float_ghost(_indexed: bool) -> Result<()> {
        // TODO merge 未实现
        Ok(())
    }
}
