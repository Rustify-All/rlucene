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

use crate::core::analysis::reader::ReaderEnum;
use crate::core::analysis::reusable_string_reader::ReusableStringReader;
use crate::core::index::index_reader::IndexReader;
use crate::test_framework::core::util::lucene_test_case::{
  new_bytes_ref_from_bytes, new_bytes_ref_from_string, new_directory_shared,
  new_searcher_with_reader, random,
};

use crate::core::document::binary_doc_values_field::BinaryDocValuesField;
use crate::core::document::document::Document;
use crate::core::document::double_doc_values_field::DoubleDocValuesField;
use crate::core::document::double_field::DoubleField;
use crate::core::document::double_point::DoublePoint;
use crate::core::document::field::{Field, FieldBase, FieldDataEnum, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::fields::FieldTokenStreamEnum;
use crate::core::document::float_doc_values_field::FloatDocValuesField;
use crate::core::document::float_field::FloatField;
use crate::core::document::float_point::FloatPoint;
use crate::core::document::int_field::IntField;
use crate::core::document::int_point::IntPoint;
use crate::core::document::knn_byte_vector_field::KnnByteVectorField;
use crate::core::document::knn_float_vector_field::KnnFloatVectorField;
use crate::core::document::long_field::LongField;
use crate::core::document::long_point::LongPoint;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::sorted_doc_values_field::SortedDocValuesField;
use crate::core::document::stored_field::StoredField;
use crate::core::document::string_field::StringField;
use crate::core::document::text_field::TextField;
use crate::core::index::byte_vector_values::ByteVectorValues;
use crate::core::index::float_vector_values::FloatVectorValues;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::{BytesRef, BytesRefValue};

use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::indexable_field::IndexableField;
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::index::knn_vector_values::KnnVectorValues;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term::Term;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::term_query::TermQuery;
use crate::core::search::top_docs::TopDocsLike;
use crate::core::util::TryIntoInt;
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind, Result};
use crate::core::util::number::Number;
use crate::core::util::numeric_utils::NumericUtils;
use crate::test_framework::core::analysis::canned_token_stream::CannedTokenStream;
use crate::test_framework::core::analysis::token;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use std::vec;
#[allow(dead_code)] // for quick search
struct TestField;

#[test]
fn test_double_point() -> Result<()> {
  let mut field = DoublePoint::new("foo", [5.0])?;
  assert!(matches!(
    (try_set_byte_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_bytes_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_bytes_ref_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  field.set_double_value(6.0)?;
  assert!(matches!(
    (try_set_int_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_long_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_float_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_reader_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_short_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_string_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_token_stream_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  match field.numeric_value() {
    Ok(Some(Number::F64(value))) => assert_eq!(value, 6.0),
    _ => unreachable!(),
  }
  assert_eq!(
    format!("{} <foo:6>", std::any::type_name::<DoublePoint>()),
    field.to_string()
  );
  Ok(())
}
#[test]
fn test_double_point_2d() -> Result<()> {
  let mut field = DoublePoint::new("foo", [5.0, 4.0])?;
  assert!(matches!(
    (try_set_byte_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_bytes_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_bytes_ref_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_double_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  field.set_double_values([6.0, 7.0])?;
  assert!(matches!(
    (try_set_int_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_long_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_float_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_reader_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_short_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_string_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_token_stream_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));

  match field.numeric_value() {
    Err(err) => {
      assert!(err.is_illegal_state_error());
      assert!(
        err
          .to_string()
          .contains("cannot convert to a single numeric value")
      );
    },
    _ => unreachable!(),
  }

  assert_eq!(
    format!("{} <foo:6,7>", std::any::type_name::<DoublePoint>()),
    field.to_string()
  );
  Ok(())
}

#[test]
fn test_double_doc_values_field() -> Result<()> {
  let mut field = DoubleDocValuesField::new("foo", 5.0);
  assert!(matches!(
    (try_set_byte_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_bytes_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_bytes_ref_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  field.set_double_value(6.0)?;
  assert!(matches!(
    (try_set_int_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_long_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_float_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_reader_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_short_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_string_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_token_stream_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));

  match field.numeric_value() {
    Ok(Some(Number::I64(bits))) => {
      let value = f64::from_bits(bits as u64);
      assert_eq!(value, 6.0);
    },
    _ => unreachable!(),
  }

  Ok(())
}

#[test]
fn test_float_doc_values_field() -> Result<()> {
  let mut field = FloatDocValuesField::new("foo", 5.0);
  assert!(matches!(
    (try_set_byte_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_bytes_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_bytes_ref_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_double_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_int_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  field.set_float_value(6.0)?;
  assert!(matches!(
    (try_set_long_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_reader_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_short_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_string_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_token_stream_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));

  match field.numeric_value() {
    Ok(Some(Number::I64(bits))) => {
      let value = f32::from_bits(bits as u32);
      assert_eq!(value, 6.0);
    },
    _ => unreachable!(),
  }

  Ok(())
}

#[test]
fn test_float_point() -> Result<()> {
  let mut field = FloatPoint::new("foo", [5.0])?;
  assert!(matches!(
    (try_set_byte_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_bytes_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_bytes_ref_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_double_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_int_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  field.set_float_value(6.0)?;
  assert!(matches!(
    (try_set_long_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_reader_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_short_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_string_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_token_stream_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));

  match field.numeric_value() {
    Ok(Some(Number::F32(value))) => assert_eq!(value, 6.0),
    _ => unreachable!(),
  }

  assert_eq!(
    format!("{} <foo:6>", std::any::type_name::<FloatPoint>()),
    field.to_string()
  );
  Ok(())
}

#[test]
fn test_float_point_2d() -> Result<()> {
  let mut field = FloatPoint::new("foo", [5.0, 4.0])?;
  assert!(matches!(
    (try_set_byte_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_bytes_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_bytes_ref_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_double_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_int_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_float_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  field.set_float_values([6.0, 7.0])?;
  assert!(matches!(
    (try_set_long_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_reader_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_short_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_string_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_token_stream_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));

  match field.numeric_value() {
    Err(err) => {
      assert!(err.is_illegal_state_error());
      assert!(
        err
          .to_string()
          .contains("cannot convert to a single numeric value")
      );
    },
    _ => unreachable!(),
  }

  assert_eq!(
    format!("{} <foo:6,7>", std::any::type_name::<FloatPoint>()),
    field.to_string()
  );
  Ok(())
}

#[test]
fn test_int_point() -> Result<()> {
  let mut field = IntPoint::new("foo", [5])?;
  assert!(matches!(
    (try_set_byte_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_bytes_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_bytes_ref_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_double_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  field.set_int_value(6)?;
  assert!(matches!(
    (try_set_float_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_long_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_reader_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_short_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_string_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_token_stream_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));

  match field.numeric_value() {
    Ok(Some(Number::I32(value))) => assert_eq!(value, 6),
    _ => unreachable!(),
  }

  assert_eq!(
    format!("{} <foo:6>", std::any::type_name::<IntPoint>()),
    field.to_string()
  );
  Ok(())
}

#[test]
fn test_int_point_2d() -> Result<()> {
  let mut field = IntPoint::new("foo", [5, 4])?;
  assert!(matches!(
    (try_set_byte_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_bytes_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_bytes_ref_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_double_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_int_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  field.set_int_values([6, 7])?;
  assert!(matches!(
    (try_set_float_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_long_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_reader_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_short_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_string_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_token_stream_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));

  match field.numeric_value() {
    Err(err) => {
      assert!(err.is_illegal_state_error());
      assert!(
        err
          .to_string()
          .contains("cannot convert to a single numeric value")
      );
    },
    _ => unreachable!(),
  }

  assert_eq!(
    format!("{} <foo:6,7>", std::any::type_name::<IntPoint>()),
    field.to_string()
  );
  Ok(())
}

#[test]
fn test_int_field() -> Result<()> {
  let fields = [
    IntField::new("foo", 12, Store::No)?,
    IntField::new("foo", 12, Store::Yes)?,
  ];

  for mut field in fields {
    assert!(matches!(
      (try_set_byte_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_bytes_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_bytes_ref_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_double_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    field.set_int_value(6)?;
    assert!(matches!(
      (try_set_long_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_float_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_long_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_reader_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_short_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_string_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_token_stream_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));

    match field.numeric_value() {
      Ok(Some(Number::I32(value))) => assert_eq!(value, 6),
      _ => unreachable!(),
    }

    assert_eq!(
      NumericUtils::sortable_bytes_to_int(field.binary_value()?.unwrap().as_bytes_ref().bytes, 0),
      6
    );

    assert_eq!(
      format!("{} <foo:6>", std::any::type_name::<IntField>()),
      field.to_string()
    );

    if field.field_type().stored() {
      match field.stored_value()? {
        Some(FieldDataEnum::Number(v)) => assert_eq!(v.to_i32().unwrap(), 6),
        Some(_) => unreachable!(""),
        None => unreachable!(""),
      }
    } else {
      assert!(field.stored_value()?.is_none());
    }
  }

  Ok(())
}

#[test]
fn test_long_field() -> Result<()> {
  let fields = [
    LongField::new("foo", 12, Store::No)?,
    LongField::new("foo", 12, Store::Yes)?,
  ];

  for mut field in fields {
    assert!(matches!(
      (try_set_byte_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_bytes_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_bytes_ref_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_double_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_int_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    field.set_long_value(6)?;
    assert!(matches!(
      (try_set_float_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_reader_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_short_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_string_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_token_stream_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));

    match field.numeric_value() {
      Ok(Some(Number::I64(value))) => assert_eq!(value, 6),
      _ => unreachable!(),
    }

    let decoded =
      NumericUtils::sortable_bytes_to_long(field.binary_value()?.unwrap().as_bytes_ref().bytes, 0);
    assert_eq!(decoded, 6);

    assert_eq!(
      format!("{} <foo:6>", std::any::type_name::<LongField>()),
      field.to_string()
    );

    if field.field_type().stored() {
      match field.stored_value()? {
        Some(FieldDataEnum::Number(v)) => assert_eq!(v.to_i64().unwrap(), 6),
        _ => unreachable!(),
      }
    } else {
      assert!(field.stored_value()?.is_none());
    }
  }

  Ok(())
}

#[test]
fn test_float_field() -> Result<()> {
  let fields = [
    FloatField::new("foo", 12.6, Store::No)?,
    FloatField::new("foo", 12.6, Store::Yes)?,
  ];

  for mut field in fields {
    match field.numeric_value() {
      Ok(Some(Number::I64(bits))) => {
        let v = NumericUtils::sortable_int_to_float(bits as i32);
        assert!((v - 12.6).abs() < f32::EPSILON);
      },
      _ => unreachable!(),
    }
    assert!(
      (FloatPoint::decode_dimension(field.binary_value()?.unwrap().as_bytes_ref().bytes, 0) - 12.6)
        .abs()
        < f32::EPSILON
    );
    assert_eq!(
      format!("{} <foo:12.6>", std::any::type_name::<FloatField>()),
      field.to_string()
    );

    assert!(matches!(
      (try_set_byte_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_bytes_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_bytes_ref_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_double_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_int_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_long_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::IllegalArgument(_))
    ));
    field.set_float_value(-28.8)?;
    assert!(matches!(
      (try_set_reader_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_short_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_string_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_token_stream_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));

    match field.numeric_value() {
      Ok(Some(Number::I64(bits))) => {
        let v = NumericUtils::sortable_int_to_float(bits.try_convert()?);
        assert!((v + 28.8).abs() < f32::EPSILON);
      },
      _ => unreachable!(),
    }
    assert!(
      (FloatPoint::decode_dimension(field.binary_value()?.unwrap().as_bytes_ref().bytes, 0) + 28.8)
        .abs()
        < f32::EPSILON
    );
    assert_eq!(
      format!("{} <foo:-28.8>", std::any::type_name::<FloatField>()),
      field.to_string()
    );

    if field.field_type().stored() {
      match field.stored_value()? {
        Some(FieldDataEnum::Number(v)) => {
          let v = v.to_f32().ok_or_else(|| {
            LuceneError::illegal_argument(format!("cannot convert to f32: {}", v))
          })?;
          assert!((v + 28.8).abs() < f32::EPSILON);
        },
        _ => unreachable!(),
      }
    } else {
      assert!(field.stored_value()?.is_none());
    }
  }

  Ok(())
}

#[test]
fn test_double_field() -> Result<()> {
  let fields = [
    DoubleField::new("foo", 12.7, Store::No)?,
    DoubleField::new("foo", 12.7, Store::Yes)?,
  ];

  for mut field in fields {
    match field.numeric_value() {
      Ok(Some(Number::I64(bits))) => {
        let v = NumericUtils::sortable_long_to_double(bits);
        assert!((v - 12.7).abs() < f64::EPSILON);
      },
      _ => unreachable!(),
    }
    assert!(
      (DoublePoint::decode_dimension(field.binary_value()?.unwrap().as_bytes_ref().bytes, 0)
        - 12.7)
        .abs()
        < f64::EPSILON
    );
    assert_eq!(
      format!("{} <foo:12.7>", std::any::type_name::<DoubleField>()),
      field.to_string()
    );

    assert!(matches!(
      (try_set_byte_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_bytes_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_bytes_ref_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_int_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_long_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::IllegalArgument(_))
    ));
    assert!(matches!(
      (try_set_float_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    field.set_double_value(-28.8)?;
    assert!(matches!(
      (try_set_reader_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_short_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_string_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_token_stream_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));

    match field.numeric_value() {
      Ok(Some(Number::I64(bits))) => {
        let v = NumericUtils::sortable_long_to_double(bits);
        assert!((v + 28.8).abs() < f64::EPSILON);
      },
      _ => unreachable!(),
    }
    assert!(
      (DoublePoint::decode_dimension(field.binary_value()?.unwrap().as_bytes_ref().bytes, 0)
        + 28.8)
        .abs()
        < f64::EPSILON
    );
    assert_eq!(
      format!("{} <foo:-28.8>", std::any::type_name::<DoubleField>()),
      field.to_string()
    );

    if field.field_type().stored() {
      match field.stored_value()? {
        Some(FieldDataEnum::Number(v)) => {
          assert!((v.to_f64().unwrap() + 28.8).abs() < f64::EPSILON);
        },
        Some(_) => unreachable!(""),
        None => unreachable!(""),
      }
    } else {
      assert!(field.stored_value()?.is_none());
    }
  }

  Ok(())
}

#[test]
fn test_numeric_doc_values_field() -> Result<()> {
  let mut field = NumericDocValuesField::new("foo", 5);
  assert!(matches!(
    (try_set_byte_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_bytes_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_bytes_ref_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_double_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_int_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_float_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  field.set_long_value(6)?;
  assert!(matches!(
    (try_set_reader_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_short_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_string_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_token_stream_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));

  match field.numeric_value() {
    Ok(Some(Number::I64(value))) => assert_eq!(value, 6),
    _ => unreachable!(),
  }

  Ok(())
}

#[test]
fn test_long_point() -> Result<()> {
  let mut field = LongPoint::new("foo", [5])?;
  assert!(matches!(
    (try_set_byte_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_bytes_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_bytes_ref_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_double_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_int_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_float_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  field.set_long_value(6)?;
  assert!(matches!(
    (try_set_reader_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_short_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_string_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_token_stream_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));

  match field.numeric_value() {
    Ok(Some(Number::I64(value))) => assert_eq!(value, 6),
    _ => unreachable!(),
  }

  assert_eq!(
    format!("{} <foo:6>", std::any::type_name::<LongPoint>()),
    field.to_string()
  );

  Ok(())
}

#[test]
fn test_long_point_2d() -> Result<()> {
  let mut field = LongPoint::new("foo", [5, 4])?;
  assert!(matches!(
    (try_set_byte_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_bytes_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_bytes_ref_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_double_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_int_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_float_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_long_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  field.set_long_values([6, 7])?;
  assert!(matches!(
    (try_set_reader_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_short_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_string_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_token_stream_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));

  match field.numeric_value() {
    Err(err) => {
      assert!(err.is_illegal_state_error());
      assert!(
        err
          .to_string()
          .contains("cannot convert to a single numeric value")
      );
    },
    _ => unreachable!(),
  }

  assert_eq!(
    format!("{} <foo:6,7>", std::any::type_name::<LongPoint>()),
    field.to_string()
  );
  Ok(())
}

#[test]
fn test_sorted_bytes_doc_values_field() -> Result<()> {
  let mut random = random();
  let mut field = SortedDocValuesField::new("foo", new_bytes_ref_from_string(&mut random, "bar")?);
  assert!(matches!(
    (try_set_byte_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  field.set_bytes_value("fubar")?;
  field.set_bytes_value(new_bytes_ref_from_string(&mut random, "baz")?)?;
  assert!(matches!(
    (try_set_double_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_int_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_float_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_long_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_reader_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_short_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_string_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_token_stream_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  let binary_value = field.binary_value()?;
  let v = binary_value.as_ref().unwrap();
  assert_eq!(
    new_bytes_ref_from_string::<_, Vec<u8>>(&mut random, "baz")?.as_byte_slice(),
    v.as_byte_slice()
  );
  Ok(())
}
#[test]
fn test_binary_doc_values_field() -> Result<()> {
  let mut random = random();
  let mut field = BinaryDocValuesField::new("foo", new_bytes_ref_from_string(&mut random, "bar")?);
  assert!(matches!(
    (try_set_byte_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  field.set_bytes_value("fubar")?;
  field.set_bytes_value(new_bytes_ref_from_string(&mut random, "baz")?)?;
  assert!(matches!(
    (try_set_double_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_int_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_float_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_long_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_reader_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_short_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_string_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_token_stream_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));

  let binary_value = field.binary_value()?;
  let v = binary_value.as_ref().unwrap();
  assert_eq!(
    new_bytes_ref_from_string::<_, Vec<u8>>(&mut random, "baz")?.as_byte_slice(),
    v.as_byte_slice()
  );
  Ok(())
}
#[test]
fn test_string_field() -> Result<()> {
  let fields = [
    StringField::from_string("foo", "bar", Store::No)?,
    StringField::from_string("foo", "bar", Store::Yes)?,
  ];

  for mut field in fields {
    assert!(matches!(
      (try_set_byte_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_bytes_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::IllegalArgument(_))
    ));
    assert!(matches!(
      (try_set_bytes_ref_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::IllegalArgument(_))
    ));
    assert!(matches!(
      (try_set_double_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_int_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_float_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_long_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_reader_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_short_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    field.set_string_value("baz")?;
    assert!(matches!(
      (try_set_token_stream_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    let string_value = field.string_value()?;
    assert_eq!(string_value.as_ref().unwrap().as_ref(), "baz");

    if field.field_type().stored() {
      match field.stored_value()? {
        Some(FieldDataEnum::String(v)) => assert_eq!(v, "baz"),
        _ => unreachable!(),
      }
    } else {
      assert!(field.stored_value()?.is_none());
    }
  }

  Ok(())
}

#[test]
fn test_binary_string_field() -> Result<()> {
  let fields = [
    StringField::from_bytes_ref("foo", "bar", Store::No)?,
    StringField::from_bytes_ref("foo", "bar", Store::Yes)?,
  ];

  for mut field in fields {
    assert!(matches!(
      (try_set_byte_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    field.set_bytes_value("baz")?;
    assert_eq!(
      field.binary_value()?.unwrap().as_byte_slice(),
      BytesRef::<Vec<u8>>::from_string("baz").as_byte_slice()
    );
    field.set_bytes_value("baz")?;
    assert!(matches!(
      (try_set_double_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_int_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_float_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_long_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_reader_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_short_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_string_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::IllegalArgument(_))
    ));
    assert!(matches!(
      (try_set_token_stream_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));

    assert_eq!(
      field.binary_value()?.unwrap().as_byte_slice(),
      BytesRef::<Vec<u8>>::from_string("baz").as_byte_slice()
    );

    if field.field_type().stored() {
      match field.stored_value()? {
        Some(FieldDataEnum::Binary(v)) => assert_eq!(v, &BytesRef::from_string("baz")),
        _ => unreachable!(),
      }
    } else {
      assert!(field.stored_value()?.is_none());
    }
  }

  Ok(())
}

#[test]
fn test_text_field_string() -> Result<()> {
  let fields = [
    TextField::from_string("foo", "bar", Store::No)?,
    TextField::from_string("foo", "bar", Store::Yes)?,
  ];

  for mut field in fields {
    assert!(matches!(
      (try_set_byte_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_bytes_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_bytes_ref_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_double_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_int_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_float_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_long_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_reader_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::IllegalArgument(_))
    ));
    assert!(matches!(
      (try_set_short_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    field.set_string_value("baz")?;
    assert!(matches!(
      (try_set_token_stream_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));

    let string_value = field.string_value()?;
    let v = string_value.as_ref().unwrap();
    assert_eq!(v.as_ref(), "baz");
    if field.field_type().stored() {
      match field.stored_value()? {
        Some(FieldDataEnum::String(v)) => assert_eq!(v.as_str(), "baz"),
        Some(FieldDataEnum::SharedString(v)) => assert_eq!(v.as_str(), "baz"),
        _ => unreachable!(),
      }
    } else {
      assert!(field.stored_value()?.is_none());
    }
  }

  Ok(())
}

#[test]
fn test_text_field_reader() -> Result<()> {
  let mut field = TextField::from_reader("foo", "bar")?;

  assert!(matches!(
    (try_set_byte_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_bytes_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_bytes_ref_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_double_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_int_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_float_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_long_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  let mut reader = ReusableStringReader::new();
  reader.set_value("foobar");
  field.set_reader_value(ReaderEnum::ReusedString(reader))?;
  assert!(matches!(
    (try_set_short_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_string_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_token_stream_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));

  assert!(field.take_reader_value()?.is_some());
  assert!(field.stored_value()?.is_none());

  Ok(())
}

#[test]
fn test_stored_field_bytes() -> Result<()> {
  let mut random = random();
  let fields = [
    StoredField::from_binary("foo", b"bar".to_vec())?,
    StoredField::from_binary_with_range("foo", b"bar".to_vec(), 0, 3)?,
    StoredField::from_bytes_ref("foo", new_bytes_ref_from_string(&mut random, "bar")?)?,
  ];

  for mut field in fields {
    assert!(matches!(
      (try_set_byte_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    field.set_bytes_value("baz")?;
    field.set_bytes_value(new_bytes_ref_from_string(&mut random, "baz")?)?;
    assert!(matches!(
      (try_set_double_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::IllegalArgument(_))
    ));
    assert!(matches!(
      (try_set_int_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::IllegalArgument(_))
    ));
    assert!(matches!(
      (try_set_float_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::IllegalArgument(_))
    ));
    assert!(matches!(
      (try_set_long_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::IllegalArgument(_))
    ));
    assert!(matches!(
      (try_set_reader_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_short_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));
    assert!(matches!(
      (try_set_string_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::IllegalArgument(_))
    ));
    assert!(matches!(
      (try_set_token_stream_value(&mut field))
        .as_ref()
        .map_err(LuceneError::kind),
      Err(LuceneErrorKind::NotImplemented(_))
    ));

    assert_eq!(
      field.binary_value()?.unwrap().as_byte_slice(),
      new_bytes_ref_from_string::<_, Vec<u8>>(&mut random, "baz")?.as_byte_slice()
    );
  }

  Ok(())
}

#[test]
fn test_stored_field_string() -> Result<()> {
  let mut field = StoredField::from_string("foo", "bar")?;
  assert!(matches!(
    (try_set_byte_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_bytes_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_bytes_ref_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_double_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_int_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_float_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_long_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_reader_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_short_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  field.set_string_value("baz")?;
  assert!(matches!(
    (try_set_token_stream_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));

  let string_value = field.string_value()?;
  let v = string_value.as_ref().unwrap();
  assert_eq!(v.as_ref(), "baz");
  Ok(())
}

#[test]
fn test_stored_field_int() -> Result<()> {
  let mut field = StoredField::from_i32("foo", 1)?;
  assert!(matches!(
    (try_set_byte_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_bytes_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_bytes_ref_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_double_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  field.set_int_value(5)?;
  assert!(matches!(
    (try_set_float_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_long_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_reader_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_short_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_string_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_token_stream_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));

  match field.numeric_value()? {
    Some(Number::I32(v)) => assert_eq!(v, 5),
    _ => unreachable!(),
  }

  Ok(())
}

#[test]
fn test_stored_field_double() -> Result<()> {
  let mut field = StoredField::from_f64("foo", 1f64)?;
  assert!(matches!(
    (try_set_byte_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_bytes_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_bytes_ref_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  field.set_double_value(5.0)?;
  assert!(matches!(
    (try_set_int_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_float_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_long_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_reader_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_short_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_string_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_token_stream_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));

  match field.numeric_value()? {
    Some(Number::F64(v)) => assert!((v - 5.0).abs() < f64::EPSILON),
    _ => unreachable!(),
  }

  Ok(())
}

#[test]
fn test_stored_field_float() -> Result<()> {
  let mut field = StoredField::from_f32("foo", 1.0)?;
  assert!(matches!(
    (try_set_byte_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_bytes_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_bytes_ref_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_double_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_int_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  field.set_float_value(5.0)?;
  assert!(matches!(
    (try_set_long_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_reader_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_short_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_string_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_token_stream_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));

  match field.numeric_value()? {
    Some(Number::F32(v)) => assert!((v - 5.0).abs() < f32::EPSILON),
    _ => unreachable!(),
  }

  Ok(())
}
#[test]
fn test_stored_field_long() -> Result<()> {
  let mut field = StoredField::from_i64("foo", 1)?;
  assert!(matches!(
    (try_set_byte_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_bytes_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_bytes_ref_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_double_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_int_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_float_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  field.set_long_value(5)?;
  assert!(matches!(
    (try_set_reader_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_short_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));
  assert!(matches!(
    (try_set_string_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (try_set_token_stream_value(&mut field))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::NotImplemented(_))
  ));

  match field.numeric_value()? {
    Some(Number::I64(v)) => assert_eq!(v, 5),
    _ => unreachable!(),
  }

  Ok(())
}

#[test]
fn test_indexed_binary_field() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = RandomIndexWriter::new(&mut random, dir)?;

  let mut doc = Document::new();
  let br = new_bytes_ref_from_bytes(&mut random, &[0u8; 5])?;
  let field = StringField::from_bytes_ref("binary", br.clone(), Store::Yes)?;
  assert_eq!(
    field.binary_value()?.unwrap().as_byte_slice(),
    br.as_byte_slice()
  );

  doc.add(field);
  writer.add_document(&mut random, doc)?;

  let reader = writer.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;
  let query = TermQuery::new(Term::new("binary", br.clone()));
  let hits = searcher.search(query, 1)?;
  assert_eq!(hits.total_hits().value(), 1);
  let stored_doc = searcher
    .stored_fields()?
    .document(hits.score_docs()[0].doc)?;
  let stored_field = stored_doc.get_field("binary").unwrap();
  assert_eq!(
    stored_field
      .binary_value()?
      .unwrap()
      .as_bytes_ref()
      .as_byte_slice(),
    br.as_byte_slice()
  );
  writer.close(&mut random)?;
  Ok(())
}

#[test]
fn test_knn_vector_field() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = RandomIndexWriter::new(&mut random, dir)?;

  let mut doc = Document::new();
  let byte_vector = [0u8; 5];
  let byte_field = KnnByteVectorField::with_similarity_function(
    "binary",
    byte_vector,
    VectorSimilarityFunction::Euclidean,
  )?;
  assert!(byte_field.binary_value()?.is_none());
  assert_eq!(byte_field.vector_value(), byte_vector.as_slice());

  let mismatched_float_field =
    KnnFloatVectorField::with_type("bogus", vec![1.0], byte_field.field_type().clone());
  assert!(matches!(
    mismatched_float_field.as_ref().map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));

  let float_vector = [1.0f32, 2.0f32];
  let float_field = KnnFloatVectorField::new("float", float_vector)?;
  assert!(float_field.binary_value()?.is_none());

  doc.add(byte_field);
  doc.add(float_field);
  writer.add_document(&mut random, doc)?;

  let reader = writer.get_reader(&mut random)?;
  let context = (&reader).get_context()?;
  assert_eq!(1, context.leaves()?.len());
  let leaf = context.leaves()?[0].reader();

  let binary = leaf.get_byte_vector_values("binary")?.unwrap();
  assert_eq!(1, binary.size());
  let mut byte_iterator = binary.iterator()?;
  assert_ne!(NO_MORE_DOCS, byte_iterator.next_doc()?);
  assert_eq!(byte_vector.as_slice(), binary.vector_value(0)?.as_ref());
  assert_eq!(NO_MORE_DOCS, byte_iterator.next_doc()?);
  assert!(binary.vector_value(1).is_err());

  let float_values = leaf.get_float_vector_values("float")?.unwrap();
  assert_eq!(1, float_values.size());
  let mut float_iterator = float_values.iterator()?;
  assert_ne!(NO_MORE_DOCS, float_iterator.next_doc()?);
  let stored_float_vector = float_values.vector_value(0)?;
  assert_eq!(float_vector.len(), stored_float_vector.len());
  assert_eq!(float_vector[0], stored_float_vector.as_ref()[0]);
  drop(stored_float_vector);
  assert_eq!(NO_MORE_DOCS, float_iterator.next_doc()?);
  assert!(float_values.vector_value(1).is_err());

  writer.close(&mut random)?;
  Ok(())
}

fn try_set_byte_value<F>(f: &mut F) -> Result<()>
where
  F: FieldBase,
{
  f.set_byte_value(10)
}
fn try_set_bytes_value<F>(f: &mut F) -> Result<()>
where
  F: FieldBase,
{
  f.set_bytes_value(BytesRef::from_bytes(vec![5, 5]))
}

fn try_set_bytes_ref_value<F>(f: &mut F) -> Result<()>
where
  F: FieldBase,
{
  f.set_bytes_value(BytesRef::from_string("bogus"))
}

fn try_set_double_value<F>(f: &mut F) -> Result<()>
where
  F: FieldBase,
{
  f.set_double_value(f64::MAX)
}

fn try_set_int_value<F>(f: &mut F) -> Result<()>
where
  F: FieldBase,
{
  f.set_int_value(i32::MAX)
}

fn try_set_long_value<F>(f: &mut F) -> Result<()>
where
  F: FieldBase,
{
  f.set_long_value(i64::MAX)
}

fn try_set_float_value<F>(f: &mut F) -> Result<()>
where
  F: FieldBase,
{
  f.set_float_value(f32::MAX)
}

fn try_set_reader_value<F>(f: &mut F) -> Result<()>
where
  F: FieldBase,
{
  let mut reader = ReusableStringReader::new();
  reader.set_value("BOO!");
  let read = ReaderEnum::ReusedString(reader);
  f.set_reader_value(read)
}

fn try_set_short_value<F>(f: &mut F) -> Result<()>
where
  F: FieldBase,
{
  f.set_short_value(i16::MAX)
}

fn try_set_string_value<F>(f: &mut F) -> Result<()>
where
  F: FieldBase,
{
  f.set_string_value("BOO!")
}

fn try_set_token_stream_value<F>(f: &mut F) -> Result<()>
where
  F: FieldBase,
{
  let tokens = vec![token::with_range(Some("foo"), 0, 3)?];
  let token_stream = CannedTokenStream::new(tokens);
  f.set_token_stream(FieldTokenStreamEnum::custom(token_stream))
}
#[test]
fn test_disabled_field() -> Result<()> {
  let ft = FieldType::new();
  let result = Field::from_string("foo", "", ft);
  assert!(matches!(
    result.as_ref().map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  Ok(())
}
#[test]
fn test_tokenized_binary_field() -> Result<()> {
  let mut ft = FieldType::new();
  ft.set_tokenized(true)?;
  ft.set_index_options(IndexOptions::Docs)?;
  let result = Field::from_bytes_ref("foo", BytesRef::new(), ft);
  assert!(matches!(
    result.as_ref().map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  Ok(())
}
#[test]
fn test_offsets_binary_field() -> Result<()> {
  let mut ft = FieldType::new();
  ft.set_tokenized(false)?;
  ft.set_index_options(IndexOptions::DocsAndFreqsAndPositionsAndOffsets)?;
  let result = Field::from_bytes_ref("foo", BytesRef::new(), ft);
  assert!(matches!(
    result.as_ref().map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  Ok(())
}
#[test]
fn test_term_vectors_offsets_binary_field() -> Result<()> {
  let mut ft = FieldType::new();
  ft.set_tokenized(false)?;
  ft.set_store_term_vectors(true)?;
  ft.set_store_term_vector_offsets(true)?;
  ft.set_store_term_vector_offsets(true)?;
  let result = Field::from_bytes_ref("foo", BytesRef::new(), ft);
  assert!(matches!(
    result.as_ref().map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  Ok(())
}
