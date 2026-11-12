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

use crate::core::document::binary_doc_values_field::BinaryDocValuesField;
use crate::core::document::document::Document;
use crate::core::document::double_doc_values_field::DoubleDocValuesField;
use crate::core::document::double_point::DoublePoint;
use crate::core::document::field::Field;
use crate::core::document::field_type::FieldType;
use crate::core::document::fields::Fields;
use crate::core::document::float_point::FloatPoint;
use crate::core::document::int_point::IntPoint;
use crate::core::document::knn_float_vector_field::KnnFloatVectorField;
use crate::core::document::long_point::LongPoint;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::sorted_set_doc_values_field::SortedSetDocValuesField;
use crate::core::index::BytesRef;
use crate::core::index::directory_reader;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::util::lucene_test_case::{
  new_directory_shared, new_index_writer_config, random,
};
use rand::{Rng, RngExt};
#[allow(dead_code)] // for quick search
pub struct TestPerFieldConsistency;
type FieldFactory = Box<dyn Fn() -> Result<Fields>>;

fn random_indexed_field<R>(random: &mut R, field_name: &str) -> FieldFactory
where
  R: Rng + ?Sized,
{
  let mut field_type = FieldType::new();
  let mut index_options = IndexOptions::values()
    .nth(random.random_range(..IndexOptions::values().count()))
    .unwrap();
  while index_options == IndexOptions::None {
    index_options = IndexOptions::values()
      .nth(random.random_range(..IndexOptions::values().count()))
      .unwrap();
  }
  field_type.set_index_options(index_options).unwrap();
  field_type.set_store_term_vectors(random.random()).unwrap();
  if field_type.store_term_vectors() {
    field_type
      .set_store_term_vector_positions(random.random())
      .unwrap();
    if field_type.store_term_vector_positions() {
      field_type
        .set_store_term_vector_payloads(random.random())
        .unwrap();
      field_type
        .set_store_term_vector_offsets(random.random())
        .unwrap();
    }
  }
  field_type.set_omit_norms(random.random()).unwrap();
  field_type.set_stored(random.random()).unwrap();
  field_type.freeze();
  let field_name = field_name.to_string();
  Box::new(move || Ok(Field::new(&field_name, "randomValue", field_type.clone()).into()))
}

fn random_point_field<R>(random: &mut R, field_name: &str) -> FieldFactory
where
  R: Rng + ?Sized,
{
  let field_name = field_name.to_string();
  match random.random_range(0..4) {
    0 => {
      let value = random.random::<i64>();
      Box::new(move || Ok(LongPoint::new(field_name.clone(), [value])?.into()))
    },
    1 => {
      let value = random.random::<i32>();
      Box::new(move || Ok(IntPoint::new(field_name.clone(), [value])?.into()))
    },
    2 => {
      let value = random.random::<f64>();
      Box::new(move || Ok(DoublePoint::new(&field_name, [value])?.into()))
    },
    _ => {
      let value = random.random::<f32>();
      Box::new(move || Ok(FloatPoint::new(&field_name, [value])?.into()))
    },
  }
}

fn random_doc_values_field<R>(random: &mut R, field_name: &str) -> FieldFactory
where
  R: Rng + ?Sized,
{
  let field_name = field_name.to_string();
  match random.random_range(0..4) {
    0 => Box::new(move || {
      Ok(
        BinaryDocValuesField::new(&field_name, BytesRef::from_bytes(b"randomValue".to_vec()))
          .into(),
      )
    }),
    1 => {
      let value = random.random::<i64>();
      Box::new(move || Ok(NumericDocValuesField::new(&field_name, value).into()))
    },
    2 => {
      let value = random.random::<f64>();
      Box::new(move || Ok(DoubleDocValuesField::new(&field_name, value).into()))
    },
    _ => Box::new(move || {
      Ok(
        SortedSetDocValuesField::new(&field_name, BytesRef::from_bytes(b"randomValue".to_vec()))
          .into(),
      )
    }),
  }
}

fn random_knn_vector_field<R>(random: &mut R, field_name: &str) -> FieldFactory
where
  R: Rng + ?Sized,
{
  let similarity_function = VectorSimilarityFunction::random(random);
  let values: Vec<f32> = (0..random.random_range(1..=10))
    .map(|_| random.random::<f32>())
    .collect();
  let field_name = field_name.to_string();
  Box::new(move || {
    Ok(
      KnnFloatVectorField::with_similarity_function(
        &field_name,
        values.clone(),
        similarity_function,
      )?
      .into(),
    )
  })
}

fn random_fields_with_the_same_name<R>(random: &mut R, field_name: &str) -> Vec<FieldFactory>
where
  R: Rng + ?Sized,
{
  vec![
    random_indexed_field(random, field_name),
    random_doc_values_field(random, field_name),
    random_point_field(random, field_name),
    random_knn_vector_field(random, field_name),
  ]
}

fn add_field(doc: &mut Document, field: &FieldFactory) -> Result<()> {
  doc.add(field()?);
  Ok(())
}

fn do_test_doc_with_missing_schema_options_throws_error<D>(
  fields: &[FieldFactory],
  missing: usize,
  writer: &IndexWriter<D>,
  error_msg: &str,
) -> Result<()>
where
  D: crate::core::store::directory::Directory + 'static,
{
  let mut doc = Document::new();
  for (i, field) in fields.iter().enumerate() {
    if i != missing {
      add_field(&mut doc, field)?;
    }
  }
  let error = writer.add_document(doc).unwrap_err();
  assert!(
    error.to_string().contains(error_msg),
    "'{}' not found in '{}'",
    error_msg,
    error
  );
  Ok(())
}

fn do_test_doc_with_extra_schema_options_throws_error<D>(
  existing: &FieldFactory,
  extra: &FieldFactory,
  writer: &IndexWriter<D>,
  error_msg: &str,
) -> Result<()>
where
  D: crate::core::store::directory::Directory + 'static,
{
  let mut doc = Document::new();
  add_field(&mut doc, existing)?;
  add_field(&mut doc, extra)?;
  let error = writer.add_document(doc).unwrap_err();
  assert!(
    error.to_string().contains(error_msg),
    "'{}' not found in '{}'",
    error_msg,
    error
  );
  Ok(())
}

#[test]
fn test_doc_with_missing_schema_options_throws_error() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut config = IndexWriterConfig::new()?;
  config.set_merge_policy(NoMergePolicy::default());
  let writer = IndexWriter::new(dir, config)?;
  let fields = random_fields_with_the_same_name(&mut random, "myfield");

  let mut doc0 = Document::new();
  for field in &fields {
    add_field(&mut doc0, field)?;
  }
  writer.add_document(&mut doc0)?;

  let mut num_not_indexed_docs = 0;
  for missing_field_idx in 0..fields.len() {
    num_not_indexed_docs += 1;
    do_test_doc_with_missing_schema_options_throws_error(
      &fields,
      missing_field_idx,
      &writer,
      &format!(
        "Inconsistency of field data structures across documents for field [myfield] of doc [{}].",
        num_not_indexed_docs
      ),
    )?;
  }
  writer.flush()?;
  let reader = directory_reader::open_from_writer(&writer)?;
  let reader = (&reader).get_context()?;
  assert_eq!(1, reader.leaves()?.len());
  assert_eq!(1, reader.leaves()?[0].reader().num_docs()?);
  assert_eq!(
    num_not_indexed_docs,
    reader.leaves()?[0].reader().num_deleted_docs()?
  );

  num_not_indexed_docs = 0;
  for missing_field_idx in 0..fields.len() {
    num_not_indexed_docs += 1;
    do_test_doc_with_missing_schema_options_throws_error(
      &fields,
      missing_field_idx,
      &writer,
      "cannot change field \"myfield\" from ",
    )?;
  }
  writer.add_document(&mut doc0)?;
  writer.flush()?;
  let reader = directory_reader::open_from_writer(&writer)?;
  let reader = (&reader).get_context()?;
  assert_eq!(2, reader.leaves()?.len());
  assert_eq!(1, reader.leaves()?[1].reader().num_docs()?);
  assert_eq!(
    num_not_indexed_docs,
    reader.leaves()?[1].reader().num_deleted_docs()?
  );
  writer.close()?;

  Ok(())
}

#[test]
fn test_doc_with_extra_schema_options_throws_error() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut config = IndexWriterConfig::new()?;
  config.set_merge_policy(NoMergePolicy::default());
  let writer = IndexWriter::new(dir, config)?;
  let fields = random_fields_with_the_same_name(&mut random, "myfield");

  let existing_field_idx = random.random_range(0..fields.len());
  let mut doc0 = Document::new();
  add_field(&mut doc0, &fields[existing_field_idx])?;
  writer.add_document(&mut doc0)?;

  let mut num_not_indexed_docs = 0;
  for extra_field_index in 0..fields.len() {
    if extra_field_index == existing_field_idx {
      continue;
    }
    num_not_indexed_docs += 1;
    do_test_doc_with_extra_schema_options_throws_error(
      &fields[existing_field_idx],
      &fields[extra_field_index],
      &writer,
      &format!(
        "Inconsistency of field data structures across documents for field [myfield] of doc [{}].",
        num_not_indexed_docs
      ),
    )?;
  }
  writer.flush()?;
  let reader = directory_reader::open_from_writer(&writer)?;
  let reader = (&reader).get_context()?;
  assert_eq!(1, reader.leaves()?.len());
  assert_eq!(1, reader.leaves()?[0].reader().num_docs()?);
  assert_eq!(
    num_not_indexed_docs,
    reader.leaves()?[0].reader().num_deleted_docs()?
  );

  num_not_indexed_docs = 0;
  for extra_field_index in 0..fields.len() {
    if extra_field_index == existing_field_idx {
      continue;
    }
    num_not_indexed_docs += 1;
    do_test_doc_with_extra_schema_options_throws_error(
      &fields[existing_field_idx],
      &fields[extra_field_index],
      &writer,
      "cannot change field \"myfield\" from ",
    )?;
  }
  writer.add_document(&mut doc0)?;
  writer.flush()?;
  let reader = directory_reader::open_from_writer(&writer)?;
  let reader = (&reader).get_context()?;
  assert_eq!(2, reader.leaves()?.len());
  assert_eq!(1, reader.leaves()?[1].reader().num_docs()?);
  assert_eq!(
    num_not_indexed_docs,
    reader.leaves()?[1].reader().num_deleted_docs()?
  );
  writer.close()?;

  Ok(())
}
