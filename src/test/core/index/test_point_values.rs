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
use crate::core::document::binary_point::BinaryPoint;
use crate::core::document::document::Document;
use crate::core::document::double_point::DoublePoint;
use crate::core::document::field::{FieldBase, Store};
use crate::core::document::float_point::FloatPoint;
use crate::core::document::int_point::IntPoint;
use crate::core::document::long_point::LongPoint;
use crate::core::index::check_index::Level;
use crate::core::index::directory_reader;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, create_temp_dir, get_only_leaf_reader, is_night_mode, new_directory_shared,
  new_fs_directory, new_index_writer_config, new_index_writer_config_with_analyzer,
  new_string_field, random,
};

use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::indexable_field::IndexableField;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::multi_reader::MultiReader;
use crate::core::index::point_values::{
  IntersectVisitor, MAX_INDEX_DIMENSIONS, MAX_NUM_BYTES, PointValues, Relation, get_doc_count,
  get_max_packed_value, get_min_packed_value, size,
};
use crate::core::index::term::Term;
use crate::core::store::{ByteBuffersDirectory, FSDirectories};
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::util::test_util::TestUtil;
use rand::Rng;
use rand::RngExt;
use std::collections::HashMap;

use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::util::TryIntoInt;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use std::sync::Arc;
use std::vec;

#[allow(dead_code)] // for quick search
struct TestPointValues;
// Suddenly add points to an existing field:
#[test]
fn test_upgrade_field_to_points() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  {
    let w = IndexWriter::new(dir.clone(), iwc)?;

    let mut field_types = HashMap::new();
    let mut doc = Document::new();
    doc.add(new_string_field(
      &mut random,
      "dim",
      "foo",
      Store::No,
      &mut field_types,
    )?);
    w.add_document(doc)?;
    w.close()?;
  }

  let mock = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();
  let v = BinaryPoint::new("dim", vec![vec![0u8; 4]])?;
  doc.add(v);
  w.close()?;

  Ok(())
}
#[test]
fn test_illegal_dim_change_one_doc() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();
  doc.add(BinaryPoint::new("dim", vec![vec![0u8; 4]])?);
  doc.add(BinaryPoint::new("dim", vec![vec![0u8; 4], vec![0u8; 4]])?);

  let err = w.add_document(doc).unwrap_err();
  match err {
    LuceneError::IllegalArgument(msg) => {
      assert_eq!(
        msg.to_string(),
        "Inconsistency of field data structures across documents for field [dim] of doc [0]. \
 point dimension: expected '1', but it has '2'."
      );
    },
    _ => unreachable!("{:?}", err),
  }
  w.close()?;
  Ok(())
}
#[test]
fn test_illegal_dim_change_two_docs() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();
  doc.add(BinaryPoint::new("dim", vec![vec![0u8; 4]])?);
  w.add_document(doc)?;

  let mut doc2 = Document::new();
  doc2.add(BinaryPoint::new("dim", vec![vec![0u8; 4], vec![0u8; 4]])?);

  let err = w.add_document(doc2).unwrap_err();
  match err {
    LuceneError::IllegalArgument(msg) => {
      assert_eq!(
        msg.to_string(),
        "Inconsistency of field data structures across documents for field [dim] of doc [1]. \
 point dimension: expected '1', but it has '2'."
      );
    },
    _ => unreachable!("{:?}", err),
  }

  w.close()?;
  Ok(())
}
#[test]
fn test_illegal_dim_change_two_segments() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  {
    let mut doc = Document::new();
    doc.add(BinaryPoint::new("dim", vec![vec![0u8; 4]])?);
    w.add_document(doc)?;
    w.commit()?;
  }

  let mut doc2 = Document::new();
  doc2.add(BinaryPoint::new("dim", vec![vec![0u8; 4], vec![0u8; 4]])?);

  let err = w.add_document(doc2).unwrap_err();
  match err {
    LuceneError::IllegalArgument(msg) => {
      assert_eq!(
        msg.to_string(),
        "cannot change field \"dim\" from points dimensionCount=1, indexDimensionCount=1, numBytes=4 \
to inconsistent dimensionCount=2, indexDimensionCount=2, numBytes=4"
      );
    },
    _ => unreachable!("{:?}", err),
  }

  w.close()?;
  Ok(())
}
#[test]
fn test_illegal_dim_change_two_writers() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;

  {
    let mock = MockAnalyzer::new(&mut random);
    let iwc = IndexWriterConfig::with_analyzer(mock)?;
    let w = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc = Document::new();
    doc.add(BinaryPoint::new("dim", vec![vec![0u8; 4]])?);
    w.add_document(doc)?;
    w.close()?;
  }

  {
    let mock = MockAnalyzer::new(&mut random);
    let iwc = IndexWriterConfig::with_analyzer(mock)?;
    let w2 = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc2 = Document::new();
    doc2.add(BinaryPoint::new("dim", vec![vec![0u8; 4], vec![0u8; 4]])?);

    let err = w2.add_document(doc2).unwrap_err();
    match err {
      LuceneError::IllegalArgument(msg) => {
        assert_eq!(
          msg.to_string(),
          "cannot change field \"dim\" from points dimensionCount=1, indexDimensionCount=1, numBytes=4 \
to inconsistent dimensionCount=2, indexDimensionCount=2, numBytes=4"
        );
      },
      _ => unreachable!("{:?}", err),
    }

    w2.close()?;
  }
  Ok(())
}
#[test]
fn test_illegal_dim_change_via_add_indexes_directory() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(a)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;
  let mut doc = Document::new();
  doc.add(BinaryPoint::new("dim", vec![vec![0u8; 4]])?);
  w.add_document(doc)?;
  w.close()?;
  drop(w);

  let dir2 = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let iwc2 = IndexWriterConfig::with_analyzer(a)?;
  let w2 = IndexWriter::new(dir2.clone(), iwc2)?;
  let mut doc = Document::new();
  doc.add(BinaryPoint::new("dim", vec![vec![0u8; 4], vec![0u8; 4]])?);
  w2.add_document(doc)?;

  let err = w2.add_indexes_from_directory(std::slice::from_ref(&dir));
  assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
  assert_eq!(
    "cannot change field \"dim\" from points dimensionCount=2, indexDimensionCount=2, numBytes=4 to inconsistent dimensionCount=1, indexDimensionCount=1, numBytes=4",
    err.unwrap_err().to_string()
  );

  w2.close()?;
  Ok(())
}
#[test]
fn test_illegal_dim_change_via_add_indexes_codec_reader() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(a)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;
  let mut doc = Document::new();
  doc.add(BinaryPoint::new("dim", vec![vec![0u8; 4]])?);
  w.add_document(doc)?;
  w.close()?;
  drop(w);

  let dir2 = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let iwc2 = IndexWriterConfig::with_analyzer(a)?;
  let w2 = IndexWriter::new(dir2.clone(), iwc2)?;
  let mut doc = Document::new();
  doc.add(BinaryPoint::new("dim", vec![vec![0u8; 4], vec![0u8; 4]])?);
  w2.add_document(doc)?;
  let reader = directory_reader::open(dir.clone())?;
  let leaf = get_only_leaf_reader(&reader)?;
  let err = w2.add_indexes_from_codec_readers(vec![leaf]);
  assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
  assert_eq!(
    "cannot change field \"dim\" from points dimensionCount=2, indexDimensionCount=2, numBytes=4 to inconsistent dimensionCount=1, indexDimensionCount=1, numBytes=4",
    err.unwrap_err().to_string()
  );

  reader.close()?;
  w2.close()?;
  dir.close()?;
  dir2.close()?;
  Ok(())
}
#[test]
fn test_illegal_dim_change_via_add_indexes_slow_codec_reader() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(a)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;
  let mut doc = Document::new();
  doc.add(BinaryPoint::new("dim", vec![vec![0u8; 4]])?);
  w.add_document(doc)?;
  w.close()?;
  drop(w);

  let dir2 = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let iwc2 = IndexWriterConfig::with_analyzer(a)?;
  let w2 = IndexWriter::new(dir2.clone(), iwc2)?;
  let mut doc = Document::new();
  doc.add(BinaryPoint::new("dim", vec![vec![0u8; 4], vec![0u8; 4]])?);
  w2.add_document(doc)?;
  let reader = directory_reader::open(dir.clone())?;
  let err = TestUtil::add_indexes_slowly(&w2, &[&reader]);
  assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
  assert_eq!(
    "cannot change field \"dim\" from points dimensionCount=2, indexDimensionCount=2, numBytes=4 to inconsistent dimensionCount=1, indexDimensionCount=1, numBytes=4",
    err.unwrap_err().to_string()
  );

  reader.close()?;
  w2.close()?;
  dir.close()?;
  dir2.close()?;
  Ok(())
}
#[test]
fn test_illegal_num_bytes_change_one_doc() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();
  doc.add(BinaryPoint::new("dim", vec![vec![0u8; 4]])?);
  doc.add(BinaryPoint::new("dim", vec![vec![0u8; 6]])?);

  let err = w.add_document(doc).unwrap_err();
  match err {
    LuceneError::IllegalArgument(msg) => {
      assert_eq!(
        msg.to_string(),
        "Inconsistency of field data structures across documents for field [dim] of doc [0]. \
 point num bytes: expected '4', but it has '6'."
      );
    },
    _ => unreachable!("{:?}", err),
  }

  w.close()?;
  Ok(())
}

#[test]
fn test_illegal_num_bytes_change_two_docs() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();
  doc.add(BinaryPoint::new("dim", vec![vec![0u8; 4]])?);
  w.add_document(doc)?;

  let mut doc2 = Document::new();
  doc2.add(BinaryPoint::new("dim", vec![vec![0u8; 6]])?);

  let err = w.add_document(doc2).unwrap_err();
  match err {
    LuceneError::IllegalArgument(msg) => {
      assert_eq!(
        msg.to_string(),
        "Inconsistency of field data structures across documents for field [dim] of doc [1]. \
 point num bytes: expected '4', but it has '6'."
      );
    },
    _ => unreachable!("{:?}", err),
  }

  w.close()?;
  Ok(())
}
#[test]
fn test_illegal_num_bytes_change_two_segments() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  {
    let mut doc = Document::new();
    doc.add(BinaryPoint::new("dim", vec![vec![0u8; 4]])?);
    w.add_document(doc)?;
    w.commit()?;
  }

  let mut doc2 = Document::new();
  doc2.add(BinaryPoint::new("dim", vec![vec![0u8; 6]])?);

  let err = w.add_document(doc2).unwrap_err();
  match err {
    LuceneError::IllegalArgument(msg) => {
      assert_eq!(
        msg.to_string(),
        "cannot change field \"dim\" from points dimensionCount=1, indexDimensionCount=1, numBytes=4 \
to inconsistent dimensionCount=1, indexDimensionCount=1, numBytes=6"
      );
    },
    _ => unreachable!("{:?}", err),
  }

  w.close()?;
  Ok(())
}

#[test]
fn test_illegal_num_bytes_change_two_writers() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;

  {
    let mock = MockAnalyzer::new(&mut random);
    let iwc = IndexWriterConfig::with_analyzer(mock)?;
    let w = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc = Document::new();
    doc.add(BinaryPoint::new("dim", vec![vec![0u8; 4]])?);
    w.add_document(doc)?;
    w.close()?;
  }

  {
    let mock = MockAnalyzer::new(&mut random);
    let iwc = IndexWriterConfig::with_analyzer(mock)?;
    let w2 = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc2 = Document::new();
    doc2.add(BinaryPoint::new("dim", vec![vec![0u8; 6]])?);

    let err = w2.add_document(doc2).unwrap_err();
    match err {
      LuceneError::IllegalArgument(msg) => {
        assert_eq!(
          msg.to_string(),
          "cannot change field \"dim\" from points dimensionCount=1, indexDimensionCount=1, numBytes=4 \
to inconsistent dimensionCount=1, indexDimensionCount=1, numBytes=6"
        );
      },
      _ => unreachable!("{:?}", err),
    }

    w2.close()?;
  }

  Ok(())
}
#[test]
fn test_illegal_num_bytes_change_via_add_indexes_directory() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(a)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;
  let mut doc = Document::new();
  doc.add(BinaryPoint::new("dim", vec![vec![0u8; 4]])?);
  w.add_document(doc)?;
  w.close()?;
  drop(w);

  let dir2 = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(a)?;
  let w2 = IndexWriter::new(dir2.clone(), iwc)?;
  let mut doc = Document::new();
  doc.add(BinaryPoint::new("dim", vec![vec![0u8; 6]])?);
  w2.add_document(doc)?;

  let err = w2.add_indexes_from_directory(std::slice::from_ref(&dir));
  assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
  assert_eq!(
    "cannot change field \"dim\" from points dimensionCount=1, indexDimensionCount=1, numBytes=6 to inconsistent dimensionCount=1, indexDimensionCount=1, numBytes=4",
    err.unwrap_err().to_string()
  );

  w2.close()?;
  Ok(())
}
#[test]
fn test_illegal_num_bytes_change_via_add_indexes_codec_reader() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(a)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;
  let mut doc = Document::new();
  doc.add(BinaryPoint::new("dim", vec![vec![0u8; 4]])?);
  w.add_document(doc)?;
  w.close()?;
  drop(w);

  let dir2 = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(a)?;
  let w2 = IndexWriter::new(dir2.clone(), iwc)?;
  let mut doc = Document::new();
  doc.add(BinaryPoint::new("dim", vec![vec![0u8; 6]])?);
  w2.add_document(doc)?;
  let reader = directory_reader::open(dir.clone())?;
  let leaf = get_only_leaf_reader(&reader)?;
  let err = w2.add_indexes_from_codec_readers(vec![leaf]);
  assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
  assert_eq!(
    "cannot change field \"dim\" from points dimensionCount=1, indexDimensionCount=1, numBytes=6 to inconsistent dimensionCount=1, indexDimensionCount=1, numBytes=4",
    err.unwrap_err().to_string()
  );

  reader.close()?;
  w2.close()?;
  dir.close()?;
  dir2.close()?;
  Ok(())
}
#[test]
fn test_illegal_num_bytes_change_via_add_indexes_slow_codec_reader() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(a)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;
  let mut doc = Document::new();
  doc.add(BinaryPoint::new("dim", vec![vec![0u8; 4]])?);
  w.add_document(doc)?;
  w.close()?;
  drop(w);

  let dir2 = new_directory_shared(&mut random)?;
  let a = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(a)?;
  let w2 = IndexWriter::new(dir2.clone(), iwc)?;
  let mut doc = Document::new();
  doc.add(BinaryPoint::new("dim", vec![vec![0u8; 6]])?);
  w2.add_document(doc)?;
  let reader = directory_reader::open(dir.clone())?;
  let err = TestUtil::add_indexes_slowly(&w2, &[&reader]);
  assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
  assert_eq!(
    "cannot change field \"dim\" from points dimensionCount=1, indexDimensionCount=1, numBytes=6 to inconsistent dimensionCount=1, indexDimensionCount=1, numBytes=4",
    err.unwrap_err().to_string()
  );

  reader.close()?;
  w2.close()?;
  dir.close()?;
  dir2.close()?;
  Ok(())
}
#[test]
fn test_illegal_too_many_bytes() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let err = BinaryPoint::new("dim", vec![vec![0u8; MAX_NUM_BYTES + 1]]);
  match err {
    Err(LuceneError::IllegalArgument(_)) => {},
    _ => unreachable!(""),
  }

  let mut doc2 = Document::new();
  doc2.add(IntPoint::new("dim", vec![17])?);
  w.add_document(doc2)?;

  w.close()?;
  Ok(())
}

#[test]
fn test_illegal_too_many_dimensions() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(mock)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut values: Vec<Vec<u8>> = Vec::with_capacity(MAX_INDEX_DIMENSIONS + 1);
  for _ in 0..(MAX_INDEX_DIMENSIONS + 1) {
    values.push(vec![0u8; 4]);
  }

  let bp = BinaryPoint::new("dim", values);
  match bp {
    Err(LuceneError::IllegalArgument(_)) => {},
    _ => unreachable!(""),
  }

  let mut doc2 = Document::new();
  doc2.add(IntPoint::new("dim", vec![17])?);
  w.add_document(doc2)?;

  w.close()?;
  Ok(())
}
#[test]
fn test_different_codecs_1() -> Result<()> {
  test_not_required_in_rust_lucene!();
}

#[test]
fn test_different_codecs_2() -> Result<()> {
  test_not_required_in_rust_lucene!();
}
#[test]
fn test_invalid_int_point_usage() -> Result<()> {
  let mut field = IntPoint::new("field", vec![17, 42])?;

  let err = field.set_int_value(14).unwrap_err();
  match err {
    LuceneError::IllegalArgument(_) => {},
    _ => unreachable!("{:?}", err),
  }

  let err = field.numeric_value().unwrap_err();
  match err {
    LuceneError::IllegalState(_) => {},
    _ => unreachable!("{:?}", err),
  }

  Ok(())
}
#[test]
fn test_invalid_long_point_usage() -> Result<()> {
  let mut field = LongPoint::new("field", vec![17, 42])?;

  let err = field.set_long_value(14).unwrap_err();
  match err {
    LuceneError::IllegalArgument(_) => {},
    _ => unreachable!("{:?}", err),
  }

  let err = field.numeric_value().unwrap_err();
  match err {
    LuceneError::IllegalState(_) => {},
    _ => unreachable!("{:?}", err),
  }

  Ok(())
}

#[test]
fn test_invalid_float_point_usage() -> Result<()> {
  let mut field = FloatPoint::new("field", vec![17.0_f32, 42.0_f32])?;

  let err = field.set_float_value(14.0).unwrap_err();
  match err {
    LuceneError::IllegalArgument(_) => {},
    _ => unreachable!("{:?}", err),
  }

  let err = field.numeric_value().unwrap_err();
  match err {
    LuceneError::IllegalState(_) => {},
    _ => unreachable!("{:?}", err),
  }

  Ok(())
}

#[test]
fn test_invalid_double_point_usage() -> Result<()> {
  let mut field = DoublePoint::new("field", vec![17.0_f64, 42.0_f64])?;

  let err = field.set_double_value(14.0).unwrap_err();
  match err {
    LuceneError::IllegalArgument(_) => {},
    _ => unreachable!("{:?}", err),
  }

  let err = field.numeric_value().unwrap_err();
  match err {
    LuceneError::IllegalState(_) => {},
    _ => unreachable!("{:?}", err),
  }

  Ok(())
}
struct IntersectVisitorImpl {
  last_doc_id: i32,
}
impl IntersectVisitorImpl {
  fn new() -> Self {
    Self { last_doc_id: -1 }
  }
}
impl IntersectVisitor for IntersectVisitorImpl {
  fn visit(&mut self, doc_id: i32) -> Result<()> {
    if doc_id < self.last_doc_id {
      return Err(LuceneError::illegal_state(format!(
        "docs out of order: docID={} but lastDocID={}",
        doc_id, self.last_doc_id
      )));
    }
    self.last_doc_id = doc_id;
    Ok(())
  }

  fn visit_with_packed_value(&mut self, doc_id: i32, _packed_value: &[u8]) -> Result<()> {
    self.visit(doc_id)
  }

  fn compare(&self, _min_packed_value: &[u8], _max_packed_value: &[u8]) -> Result<Relation> {
    if random().random_bool(0.5) {
      Ok(Relation::CellCrossesQuery)
    } else {
      Ok(Relation::CellInsideQuery)
    }
  }
}
#[test]
fn test_tie_break_by_doc_id() -> Result<()> {
  let mut random = random();

  let dir = new_fs_directory(&mut random, create_temp_dir()?)?;

  let iwc = new_index_writer_config(&mut random)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();
  doc.add(IntPoint::new("int", vec![17])?);

  let num_docs = if is_night_mode() {
    at_least(&mut random, 300_000)
  } else {
    at_least(&mut random, 3_000)
  };

  for _ in 0..num_docs {
    w.add_document(doc.clone())?;
    if random.random_range(0..1000) == 17 {
      w.commit()?;
    }
  }

  let reader = directory_reader::open_from_writer(&w)?;
  let reader = reader.get_context()?;

  for leaf in reader.leaves()? {
    let points = leaf.reader().get_point_values("int")?;
    if let Some(points) = points {
      let mut visitor = IntersectVisitorImpl { last_doc_id: -1 };
      points.intersect(&mut visitor)?;
    }
  }

  w.close()?;
  Ok(())
}
#[test]
fn test_delete_all_point_docs() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let iwc = new_index_writer_config(&mut random)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut field_types = HashMap::new();
  let mut doc = Document::new();
  doc.add(new_string_field(
    &mut random,
    "id",
    "0",
    Store::No,
    &mut field_types,
  )?);
  doc.add(IntPoint::new("int", vec![17])?);
  w.add_document(doc)?;

  w.add_document(Document::new())?;
  w.commit()?;

  w.delete_documents_with_terms(vec![Term::from_text("id", "0")])?;

  w.force_merge(1)?;
  let reader = directory_reader::open_from_writer(&w)?;

  let ctx = reader.get_context()?;
  let leaves = ctx.leaves()?;
  let leaf = &leaves[0];

  assert!(leaf.reader().get_point_values("int")?.is_none());

  w.close()?;
  Ok(())
}

#[test]
fn test_points_field_missing_from_one_segment() -> Result<()> {
  let mut random = random();

  let dir = Arc::new(FSDirectories::open(create_temp_dir()?.keep())?);

  let iwc = IndexWriterConfig::new()?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut field_types = HashMap::new();
  let mut doc = Document::new();
  doc.add(new_string_field(
    &mut random,
    "id",
    "0",
    Store::No,
    &mut field_types,
  )?);
  doc.add(IntPoint::new("int0", vec![0])?);
  w.add_document(doc)?;
  w.commit()?;

  let mut doc2 = Document::new();
  doc2.add(IntPoint::new("int1", vec![17])?);
  w.add_document(doc2)?;

  w.force_merge(1)?;

  w.close()?;
  dir.close()
}
#[test]
fn test_sparse_points() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let num_docs = at_least(&mut random, 1000);
  let num_fields = TestUtil::next_int(&mut random, 1, 10);

  let w = RandomIndexWriter::new(&mut random, dir.clone())?;

  let mut field_doc_counts = vec![0i32; num_fields as usize];
  let mut field_sizes = vec![0i32; num_fields as usize];

  for _ in 0..num_docs {
    let mut doc = Document::new();

    for field in 0..num_fields {
      let field_name = format!("int{}", field);

      if random.random_range(0..100) == 17 {
        let v = random.random();
        doc.add(IntPoint::new(&field_name, vec![v])?);
        field_doc_counts[field as usize] += 1;
        field_sizes[field as usize] += 1;

        if random.random_range(0..10) == 5 {
          let v2 = random.random();
          doc.add(IntPoint::new(&field_name, vec![v2])?);
          field_sizes[field as usize] += 1;
        }
      }
    }

    w.add_document(&mut random, doc)?;
  }

  let reader = w.get_reader(&mut random)?;
  let ctx = reader.get_context()?;
  let leaves = ctx.leaves()?;

  for field in 0..num_fields {
    let mut doc_count = 0i32;
    let mut size = 0i32;
    let field_name = format!("int{}", field);

    for leaf in leaves.iter() {
      if let Some(points) = leaf.reader().get_point_values(&field_name)? {
        doc_count += points.get_doc_count()?;
        size += points.size()? as i32;
      }
    }

    assert_eq!(field_doc_counts[field as usize], doc_count);
    assert_eq!(field_sizes[field as usize], size);
  }

  w.close(&mut random)?;

  Ok(())
}
#[test]
fn test_check_index_includes_points() -> Result<()> {
  let mut random = random();
  let dir = Arc::new(ByteBuffersDirectory::new());
  let w = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;
  let mut doc = Document::new();
  doc.add(IntPoint::new("int1", vec![17])?);
  w.add_document(doc)?;

  let mut doc = Document::new();
  doc.add(IntPoint::new("int1", vec![44])?);
  doc.add(IntPoint::new("int2", vec![-17])?);
  w.add_document(doc)?;
  w.close()?;

  let mut output = Vec::new();
  let status = TestUtil::check_index_with_options(
    &mut random,
    Arc::clone(&dir),
    Level::MIN_LEVEL_FOR_INTEGRITY_CHECKS,
    true,
    true,
    Some(&mut output),
  )?;
  assert_eq!(1, status.segment_infos.len());
  let points_status = status.segment_infos[0]
    .points_status
    .as_ref()
    .expect("points status");
  // total 3 point values were indexed:
  assert_eq!(3, points_status.total_value_points);
  // ... across 2 fields:
  assert_eq!(2, points_status.total_value_fields);

  // Make sure CheckIndex in fact declares that it is testing points!
  assert!(String::from_utf8(output)?.contains("test: points..."));
  dir.close()
}
#[test]
fn test_merged_stats_empty_reader() -> Result<()> {
  let reader = MultiReader::empty()?;

  assert!(get_min_packed_value(&reader, "field")?.is_none());
  assert!(get_max_packed_value(&reader, "field")?.is_none());
  assert_eq!(0, get_doc_count(&reader, "field")?);
  assert_eq!(0, size(&reader, "field")?);

  Ok(())
}

#[test]
fn test_merged_stats_one_segment_without_points() -> Result<()> {
  let dir = Arc::new(ByteBuffersDirectory::new());
  let mut iwc = IndexWriterConfig::new()?;
  iwc.set_merge_policy(NoMergePolicy::default());

  let w = IndexWriter::new(dir.clone(), iwc)?;

  w.add_document(Document::new())?;

  {
    directory_reader::open_from_writer(&w)?;
  }

  let mut doc = Document::new();
  doc.add(IntPoint::new("field", vec![i32::MIN])?);
  w.add_document(doc)?;

  let reader = directory_reader::open_from_writer(&w)?;

  assert_eq!(get_min_packed_value(&reader, "field")?, Some(vec![0u8; 4]));

  assert_eq!(get_max_packed_value(&reader, "field")?, Some(vec![0u8; 4]));

  assert_eq!(get_doc_count(&reader, "field")?, 1);

  assert_eq!(size(&reader, "field")?, 1);

  assert_eq!(get_min_packed_value(&reader, "field2")?, None);
  assert_eq!(get_max_packed_value(&reader, "field2")?, None);
  assert_eq!(get_doc_count(&reader, "field2")?, 0);
  assert_eq!(size(&reader, "field2")?, 0);

  Ok(())
}
#[test]
fn test_merged_stats_all_points_deleted() -> Result<()> {
  let mut random = random();

  let dir = Arc::new(ByteBuffersDirectory::new());

  let iwc = IndexWriterConfig::new()?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  w.add_document(Document::new())?;

  {
    let mut field_types = HashMap::new();
    let mut doc = Document::new();
    doc.add(IntPoint::new("field", vec![i32::MIN])?);
    doc.add(new_string_field(
      &mut random,
      "delete",
      "yes",
      Store::No,
      &mut field_types,
    )?);
    w.add_document(doc)?;
  }

  w.force_merge(1)?;

  w.delete_documents_with_terms(vec![Term::from_text("delete", "yes")])?;

  w.add_document(Document::new())?;

  w.force_merge(1)?;

  let reader = directory_reader::open_from_writer(&w)?;

  assert_eq!(get_min_packed_value(&reader, "field")?, None);
  assert_eq!(get_max_packed_value(&reader, "field")?, None);
  assert_eq!(get_doc_count(&reader, "field")?, 0);
  assert_eq!(size(&reader, "field")?, 0);

  Ok(())
}
#[test]
fn test_merged_stats() -> Result<()> {
  let mut random = random();
  let iters = at_least(&mut random, 3);
  for _ in 0..iters {
    do_test_merged_stats(&mut random)?;
  }
  Ok(())
}

fn random_binary_value<R>(random: &mut R, num_dims: usize, num_bytes_per_dim: usize) -> Vec<Vec<u8>>
where
  R: Rng + ?Sized,
{
  let mut values = Vec::with_capacity(num_dims);
  for _ in 0..num_dims {
    let mut bytes = vec![0u8; num_bytes_per_dim];
    random.fill(bytes.as_mut_slice());
    values.push(bytes);
  }
  values
}
fn do_test_merged_stats<R>(random: &mut R) -> Result<()>
where
  R: Rng + ?Sized,
{
  let num_dims = TestUtil::next_int(random, 1, 8);
  let num_bytes_per_dim = TestUtil::next_int(random, 1, 16);

  let dir = Arc::new(ByteBuffersDirectory::new());

  let iwc = IndexWriterConfig::new()?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let num_docs = TestUtil::next_int(random, 10, 20);

  for _ in 0..num_docs {
    let mut doc = Document::new();
    let num_points = random.random_range(0..3);

    for _ in 0..num_points {
      let v = random_binary_value(random, num_dims as usize, num_bytes_per_dim as usize);
      doc.add(BinaryPoint::new("field", v)?);
    }

    w.add_document(doc)?;

    if random.random_bool(0.5) {
      directory_reader::open_from_writer(&w)?;
    }
  }

  let reader1 = directory_reader::open_from_writer(&w)?;
  w.force_merge(1)?;

  let reader2 = directory_reader::open_from_writer(&w)?;
  let leaf = get_only_leaf_reader(&reader2)?;
  let expected_opt = leaf.get_point_values("field")?;
  match expected_opt {
    Some(expected) => {
      assert_eq!(
        Some(expected.get_min_packed_value()?.unwrap().into_owned()),
        get_min_packed_value(&reader1, "field")?
      );

      assert_eq!(
        Some(expected.get_max_packed_value()?.unwrap().into_owned()),
        get_max_packed_value(&reader1, "field")?
      );

      assert_eq!(expected.get_doc_count()?, get_doc_count(&reader1, "field")?);

      assert_eq!(expected.size()?, size(&reader1, "field")?.try_convert()?);
    },
    None => {
      assert_eq!(get_min_packed_value(&reader1, "field")?, None);
      assert_eq!(get_max_packed_value(&reader1, "field")?, None);
      assert_eq!(get_doc_count(&reader1, "field")?, 0);
      assert_eq!(size(&reader1, "field")?, 0);
    },
  }

  Ok(())
}
