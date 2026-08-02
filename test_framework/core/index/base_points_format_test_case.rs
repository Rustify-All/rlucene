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
use crate::core::codecs::dummy::dummy_mutable_point_tree::DummyMutablePointTree;
use crate::core::document::binary_point::BinaryPoint;
use crate::core::document::document::Document;
use crate::core::document::field::Store;
use crate::core::document::field_type::FieldType;
use crate::core::document::int_point::IntPoint;
use crate::core::document::long_point::LongPoint;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::index::directory_reader;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::merge_scheduler::MergeSchedulerEnum;
use crate::core::index::multi_bits::get_live_docs;
use crate::core::index::multi_doc_values::MultiDocValues;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::point_values::{
  IntersectVisitor, MAX_DIMENSIONS, MAX_INDEX_DIMENSIONS, MAX_NUM_BYTES, PointTree, PointTreeEnum,
  PointValues, Relation,
};
use crate::core::index::serial_merge_scheduler::SerialMergeScheduler;
use crate::core::index::term::Term;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::store::directory::{DirEnum, Directory};
use crate::core::util::bit_set::BitSet;
use crate::core::util::bits::Bits;
use crate::core::util::clone::TryClone;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::io_utils::IOUtils;
use crate::core::util::numeric_utils::NumericUtils;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::base_index_file_format_test_case::{
  BaseIndexFileFormatTestCase, BaseIndexFileFormatTestCaseDefaults,
};
use crate::test_framework::core::index::mismatched_codec_reader::MismatchedCodecReader;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, create_temp_dir, get_only_leaf_reader, is_night_mode, new_directory_shared,
  new_fs_directory, new_index_writer_config, new_index_writer_config_with_analyzer,
  new_log_merge_policy, new_mock_fs_directory, new_searcher_with_reader, new_string_field, rarely,
};
use crate::test_framework::core::util::test_util::TestUtil;
use num_bigint::{BigInt, BigUint};
use rand::Rng;
use rand::RngExt;
use std::collections::HashMap;
use std::sync::Arc;

pub struct BasePointsFormatTestCaseDefaults;

impl<T> BaseIndexFileFormatTestCaseDefaults<T> for BasePointsFormatTestCaseDefaults
where
  T: BasePointsFormatTestCase,
{
  fn add_random_fields<R>(_test_case: &T, random: &mut R, document: &mut Document) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_values = random.random_range(0..3);
    for _ in 0..num_values {
      document.add(IntPoint::new("f", vec![random.random()])?);
    }
    Ok(())
  }

  fn merge_is_stable(_test_case: &T) -> bool {
    // Suppress this test from the base trait: merges for BKD trees are not stable because the tree
    // created by merge will have a different structure than the tree created by adding points
    // separately.
    false
  }
}

pub trait BasePointsFormatTestCase:
  BaseIndexFileFormatTestCase<Defaults = BasePointsFormatTestCaseDefaults>
{
  fn test_basic<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let mut iwc = new_index_writer_config(random)?;
    iwc.set_merge_policy(new_log_merge_policy(random)?);
    let w = IndexWriter::new(dir.clone(), iwc)?;

    let mut point = vec![0u8; 4];
    for i in 0..20 {
      let mut doc = Document::new();
      NumericUtils::int_to_sortable_bytes(i, &mut point, 0);
      doc.add(BinaryPoint::new("dim", vec![point.clone()])?);
      w.add_document(doc)?;
    }

    w.force_merge(1)?;
    w.close()?;

    let sub = get_only_leaf_reader(directory_reader::open(dir)?)?;
    let values = sub
      .get_point_values("dim")?
      .expect("point values should exist");

    let mut seen = FixedBitSet::new(20);
    values.intersect(&mut BasicIntersectVisitor { seen: &mut seen })?;
    assert_eq!(20, seen.cardinality());
    Ok(())
  }

  fn test_merge<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let mut iwc = new_index_writer_config(random)?;
    iwc.set_merge_policy(new_log_merge_policy(random)?);
    let w = IndexWriter::new(dir.clone(), iwc)?;

    let mut point = vec![0u8; 4];
    for i in 0..20 {
      let mut doc = Document::new();
      NumericUtils::int_to_sortable_bytes(i, &mut point, 0);
      doc.add(BinaryPoint::new("dim", vec![point.clone()])?);
      w.add_document(doc)?;
      if i == 10 {
        w.commit()?;
      }
    }

    w.force_merge(1)?;
    w.close()?;

    let sub = get_only_leaf_reader(directory_reader::open(dir)?)?;
    let values = sub
      .get_point_values("dim")?
      .expect("point values should exist");

    let mut seen = FixedBitSet::new(20);
    values.intersect(&mut BasicIntersectVisitor { seen: &mut seen })?;
    assert_eq!(20, seen.cardinality());
    Ok(())
  }

  fn test_all_point_docs_deleted_in_segment<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let iwc = new_index_writer_config(random)?;
    let w = IndexWriter::new(dir.clone(), iwc)?;
    let mut field_types = HashMap::new();
    let mut point = vec![0u8; 4];
    for i in 0..10i32 {
      let mut doc = Document::new();
      NumericUtils::int_to_sortable_bytes(i, &mut point, 0);
      doc.add(BinaryPoint::new("dim", vec![point.clone()])?);
      doc.add(NumericDocValuesField::new("id", i as i64));
      doc.add(new_string_field(
        random,
        "x",
        "x",
        Store::No,
        &mut field_types,
      )?);
      w.add_document(doc)?;
    }

    w.add_document(Document::new())?;
    w.delete_documents_with_terms(vec![Term::from_text("x", "x")])?;
    if random.random_bool(0.5) {
      w.force_merge(1)?;
    }
    w.close()?;

    let r = directory_reader::open(dir)?;
    assert_eq!(1, r.num_docs()?);
    let live_docs = get_live_docs(&r)?;
    let r = (&r).get_context()?;
    for ctx in r.leaves()? {
      let values = ctx.reader().get_point_values("dim")?;
      let mut id_values = ctx.reader().get_numeric_doc_values("id")?;
      if id_values.is_none() {
        // this is (surprisingly) OK, because if the random IWC flushes all 10 docs before the 11th
        // doc is added, and force merge runs, it
        // will drop the 100% deleted segments, and the "id" field never exists in the final single
        // doc segment
        continue;
      }

      let mut id_values = id_values.take().unwrap();
      let mut doc_id_to_id = vec![0i32; ctx.reader().max_doc()? as usize];
      loop {
        let doc_id = id_values.next_doc()?;
        if doc_id == NO_MORE_DOCS {
          break;
        }
        doc_id_to_id[doc_id as usize] = id_values.long_value()? as i32;
      }

      if let Some(values) = values {
        let mut seen = FixedBitSet::new(ctx.reader().max_doc()? as usize);
        values.intersect(&mut AllPointDocsDeletedIntersectVisitor {
          seen: &mut seen,
          live_docs: live_docs.as_ref(),
          doc_id_to_id: &doc_id_to_id,
        })?;
        assert_eq!(0, seen.cardinality());
      }
    }

    Ok(())
  }

  fn test_with_exceptions<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_docs = at_least(random, 1000);
    let num_bytes_per_dim = TestUtil::next_int(random, 2, MAX_NUM_BYTES as i32) as usize;
    let num_dims = TestUtil::next_int(random, 1, MAX_DIMENSIONS as i32) as usize;
    let num_index_dims = TestUtil::next_int(
      random,
      1,
      std::cmp::min(num_dims as i32, MAX_INDEX_DIMENSIONS as i32),
    ) as usize;

    let mut doc_values = Vec::with_capacity(num_docs as usize);
    for _doc_id in 0..num_docs {
      let mut values = Vec::with_capacity(num_dims);
      for _dim in 0..num_dims {
        let mut value = vec![0u8; num_bytes_per_dim];
        random.fill_bytes(&mut value);
        values.push(value);
      }
      doc_values.push(values);
    }

    // Keep retrying until we 1) we allow a big enough heap, and 2) we hit a random IOExc from MDW:
    let mut done = false;
    while !done {
      let dir = new_mock_fs_directory(random, create_temp_dir()?)?;
      dir.set_random_io_exception_rate(0.05);
      dir.set_random_io_exception_rate_on_open(0.05);

      let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let dir = Arc::new(dir.clone());
        self.verify_with_expect_exceptions(
          random,
          dir,
          &doc_values,
          None,
          num_dims,
          num_index_dims,
          num_bytes_per_dim,
          true,
        )
      }));
      let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| dir.close()));

      match IOUtils::use_or_suppress_caught_result(result, close_result) {
        Ok(()) => {},
        Err(e @ (LuceneError::AlreadyClosed(_) | LuceneError::IllegalState(_))) => {
          done = self.handle_possibly_fake_exception(e)?;
        },
        Err(LuceneError::IllegalArgument(msg)) => {
          // This just means we got a too-small maxMB for the maxPointsInLeafNode; just retry w/
          // more heap
          assert!(
            msg
              .message
              .contains("either increase maxMBSortInHeap or decrease maxPointsInLeafNode"),
            "{}",
            msg.message
          );
        },
        Err(e @ (LuceneError::Io { .. } | LuceneError::IoWithPath { .. })) => {
          done = self.handle_possibly_fake_exception(e)?;
        },
        Err(e) => return Err(e),
      }
    }

    Ok(())
  }

  // TODO: merge w/ BaseIndexFileFormatTestCase.handleFakeIOException
  fn handle_possibly_fake_exception(&self, e: LuceneError) -> Result<bool> {
    let mut current = Some(&e);
    while let Some(err) = current {
      let message = err.to_string();
      if message.contains("a random IOException")
        || message.contains("background merge hit exception")
      {
        return Ok(true);
      }
      current = err.get_suppressed()?;
    }

    Err(e)
  }

  fn test_multi_valued<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_bytes_per_dim = TestUtil::next_int(random, 2, MAX_NUM_BYTES as i32) as usize;
    let num_dims = TestUtil::next_int(random, 1, MAX_DIMENSIONS as i32) as usize;
    let num_index_dims = TestUtil::next_int(
      random,
      1,
      std::cmp::min(MAX_INDEX_DIMENSIONS as i32, num_dims as i32),
    ) as usize;

    let num_docs = if is_night_mode() {
      at_least(random, 1000)
    } else {
      at_least(random, 100)
    };

    let mut doc_values: Vec<Vec<Vec<u8>>> = Vec::new();
    let mut doc_ids: Vec<i32> = Vec::new();

    for doc_id in 0..num_docs {
      let num_values_in_doc = TestUtil::next_int(random, 1, 5);
      for _ord in 0..num_values_in_doc {
        doc_ids.push(doc_id);
        let mut values = vec![Vec::new(); num_dims];
        for value in values.iter_mut().take(num_dims) {
          *value = vec![0u8; num_bytes_per_dim];
          random.fill_bytes(value);
        }
        doc_values.push(values);
      }
    }

    self.verify(
      random,
      &doc_values,
      Some(&doc_ids),
      num_dims,
      num_index_dims,
      num_bytes_per_dim,
    )
  }

  fn test_all_equal<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_bytes_per_dim = TestUtil::next_int(random, 2, MAX_NUM_BYTES as i32) as usize;
    let num_dims = TestUtil::next_int(random, 1, MAX_INDEX_DIMENSIONS as i32) as usize;

    let num_docs = at_least(random, 1000);
    let mut doc_values: Vec<Vec<Vec<u8>>> = Vec::with_capacity(num_docs as usize);

    let mut first_values: Option<Vec<Vec<u8>>> = None;
    for doc_id in 0..num_docs {
      if doc_id == 0 {
        let mut values = Vec::with_capacity(num_dims);
        for _dim in 0..num_dims {
          let mut value = vec![0u8; num_bytes_per_dim];
          random.fill_bytes(&mut value);
          values.push(value);
        }
        first_values = Some(values.clone());
        doc_values.push(values);
      } else {
        doc_values.push(first_values.as_ref().unwrap().clone());
      }
    }

    self.verify_no_index_dims(random, &doc_values, None, num_dims, num_bytes_per_dim)
  }

  fn test_one_dim_equal<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_bytes_per_dim = TestUtil::next_int(random, 2, MAX_NUM_BYTES as i32) as usize;
    let num_dims = TestUtil::next_int(random, 1, MAX_INDEX_DIMENSIONS as i32) as usize;

    let num_docs = at_least(random, 1000);
    let the_equal_dim = random.random_range(0..num_dims);
    let mut doc_values: Vec<Vec<Vec<u8>>> = Vec::with_capacity(num_docs as usize);

    for doc_id in 0..num_docs {
      let mut values = Vec::with_capacity(num_dims);
      for _dim in 0..num_dims {
        let mut value = vec![0u8; num_bytes_per_dim];
        random.fill_bytes(&mut value);
        values.push(value);
      }
      doc_values.push(values);
      if doc_id > 0 {
        doc_values[doc_id as usize][the_equal_dim] = doc_values[0][the_equal_dim].clone();
      }
    }

    self.verify_no_index_dims(random, &doc_values, None, num_dims, num_bytes_per_dim)
  }

  fn test_one_dim_two_values<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_bytes_per_dim = TestUtil::next_int(random, 2, MAX_NUM_BYTES as i32) as usize;
    let num_dims = TestUtil::next_int(random, 1, MAX_INDEX_DIMENSIONS as i32) as usize;

    let num_docs = at_least(random, 1000);
    let the_dim = random.random_range(0..num_dims);

    let mut value1 = vec![0u8; num_bytes_per_dim];
    random.fill_bytes(&mut value1);
    let mut value2 = vec![0u8; num_bytes_per_dim];
    random.fill_bytes(&mut value2);

    let mut doc_values: Vec<Vec<Vec<u8>>> = Vec::with_capacity(num_docs as usize);

    for _doc_id in 0..num_docs {
      let mut values = Vec::with_capacity(num_dims);
      for dim in 0..num_dims {
        if dim == the_dim {
          values.push(if random.random_bool(0.5) {
            value1.clone()
          } else {
            value2.clone()
          });
        } else {
          let mut value = vec![0u8; num_bytes_per_dim];
          random.fill_bytes(&mut value);
          values.push(value);
        }
      }
      doc_values.push(values);
    }

    self.verify_no_index_dims(random, &doc_values, None, num_dims, num_bytes_per_dim)
  }
  fn test_big_int_n_dims<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_docs = at_least(random, 200);
    let dir = get_directory(random, num_docs as usize)?;
    let num_bytes_per_dim = TestUtil::next_int(random, 2, MAX_NUM_BYTES as i32) as usize;
    let num_dims = TestUtil::next_int(random, 1, MAX_INDEX_DIMENSIONS as i32) as usize;

    let analyzer = MockAnalyzer::new(random);
    let mut iwc = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwc.set_merge_policy(new_log_merge_policy(random)?);
    let w = RandomIndexWriter::with_config(random, dir.clone(), iwc);

    let mut docs: Vec<Vec<num_bigint::BigInt>> = Vec::with_capacity(num_docs as usize);

    for doc_id in 0..num_docs {
      let mut values = Vec::with_capacity(num_dims);
      if cfg!(feature = "test_log_verbose") {
        println!("  docID={doc_id}");
      }
      let mut bytes = Vec::with_capacity(num_dims);
      for dim in 0..num_dims {
        let value = random_big_int(random, num_bytes_per_dim);
        let mut dim_bytes = vec![0u8; num_bytes_per_dim];
        NumericUtils::big_int_to_sortable_bytes(&value, num_bytes_per_dim, &mut dim_bytes, 0)?;
        if cfg!(feature = "test_log_verbose") {
          println!("    {dim} -> {value}");
        }
        values.push(value);
        bytes.push(dim_bytes);
      }
      docs.push(values);
      let mut doc = Document::new();
      doc.add(BinaryPoint::new("field", bytes)?);
      w.add_document(random, doc)?;
    }

    let r = (w.get_reader(random)?).get_context()?;
    w.close(random)?;

    let iters = at_least(random, 100);
    for iter in 0..iters {
      if cfg!(feature = "test_log_verbose") {
        println!("\nTEST: iter={iter}");
      }

      let mut query_min = Vec::with_capacity(num_dims);
      let mut query_max = Vec::with_capacity(num_dims);
      for dim in 0..num_dims {
        let mut min = random_big_int(random, num_bytes_per_dim);
        let mut max = random_big_int(random, num_bytes_per_dim);
        if min > max {
          std::mem::swap(&mut min, &mut max);
        }
        if cfg!(feature = "test_log_verbose") {
          println!("  {dim}\n    min={min}\n    max={max}");
        }
        query_min.push(min);
        query_max.push(max);
      }

      let mut hits = FixedBitSet::new(num_docs as usize);
      for ctx in r.leaves()? {
        let dim_values = ctx.reader().get_point_values("field")?;
        if dim_values.is_none() {
          continue;
        }

        let doc_base = ctx.doc_base as i32;
        dim_values
          .unwrap()
          .intersect(&mut BigIntNDimsIntersectVisitor {
            hits: &mut hits,
            doc_base,
            num_dims,
            num_bytes_per_dim,
            query_min: &query_min,
            query_max: &query_max,
          })?;
      }

      for doc_id in 0..num_docs {
        let doc_values = &docs[doc_id as usize];
        let mut expected = true;
        for dim in 0..num_dims {
          let x = &doc_values[dim];
          if x < &query_min[dim] || x > &query_max[dim] {
            expected = false;
            break;
          }
        }
        let actual = hits.get(doc_id as usize)?;
        assert_eq!(expected, actual, "docID={doc_id}");
      }
    }

    Ok(())
  }
  fn test_random_binary_tiny<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    self.do_test_random_binary(random, 10)
  }

  fn test_random_binary_medium<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    self.do_test_random_binary(random, 200)
  }

  fn test_random_binary_big<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    self.do_test_random_binary(random, 200000)
  }

  fn do_test_random_binary<R>(&self, random: &mut R, count: i32) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_docs = TestUtil::next_int(random, count, count * 2);
    let num_bytes_per_dim = TestUtil::next_int(random, 2, MAX_NUM_BYTES as i32) as usize;
    let num_data_dims = TestUtil::next_int(random, 1, MAX_INDEX_DIMENSIONS as i32) as usize;
    let num_index_dims = TestUtil::next_int(random, 1, num_data_dims as i32) as usize;

    let mut doc_values: Vec<Vec<Vec<u8>>> = Vec::with_capacity(num_docs as usize);

    for _doc_id in 0..num_docs {
      let mut values = Vec::with_capacity(num_data_dims);
      for _dim in 0..num_data_dims {
        let mut value = vec![0u8; num_bytes_per_dim];
        // TODO: sometimes test on a "small" volume too, so we test the high density cases, higher
        // chance of boundary, etc. cases:
        random.fill_bytes(&mut value);
        values.push(value);
      }
      doc_values.push(values);
    }

    self.verify(
      random,
      &doc_values,
      None,
      num_data_dims,
      num_index_dims,
      num_bytes_per_dim,
    )
  }

  fn verify_no_index_dims<R>(
    &self,
    random: &mut R,
    doc_values: &[Vec<Vec<u8>>],
    doc_ids: Option<&[i32]>,
    num_dims: usize,
    num_bytes_per_dim: usize,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    self.verify(
      random,
      doc_values,
      doc_ids,
      num_dims,
      num_dims,
      num_bytes_per_dim,
    )
  }
  fn verify<R>(
    &self,
    random: &mut R,
    doc_values: &[Vec<Vec<u8>>],
    doc_ids: Option<&[i32]>,
    num_data_dims: usize,
    num_index_dims: usize,
    num_bytes_per_dim: usize,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = get_directory(random, doc_values.len())?;
    loop {
      match self.verify_with_expect_exceptions(
        random,
        dir.clone(),
        doc_values,
        doc_ids,
        num_data_dims,
        num_index_dims,
        num_bytes_per_dim,
        false,
      ) {
        Ok(()) => return Ok(()),
        Err(LuceneError::IllegalArgument(msg)) => {
          let msg = msg.to_string();
          eprintln!("{msg}");
          assert!(msg.contains("either increase maxMBSortInHeap or decrease maxPointsInLeafNode"));
        },
        Err(err) => return Err(err),
      }
    }
  }
  fn flatten_binary_point(
    &self,
    value: &[Vec<u8>],
    num_data_dims: usize,
    num_bytes_per_dim: usize,
  ) -> Vec<u8> {
    let mut result = vec![0u8; value.len() * num_bytes_per_dim];
    for d in 0..num_data_dims {
      result[d * num_bytes_per_dim..(d + 1) * num_bytes_per_dim]
        .copy_from_slice(&value[d][..num_bytes_per_dim]);
    }
    result
  }
  #[allow(clippy::too_many_arguments)]
  fn verify_with_expect_exceptions<R, D>(
    &self,
    random: &mut R,
    dir: Arc<D>,
    doc_values: &[Vec<Vec<u8>>],
    ids: Option<&[i32]>,
    num_dims: usize,
    num_index_dims: usize,
    num_bytes_per_dim: usize,
    expect_exceptions: bool,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
    D: Directory + 'static,
  {
    let num_values = doc_values.len();
    if cfg!(feature = "test_log_verbose") {
      println!(
        "TEST: numValues={} numDims={} numIndexDims={} numBytesPerDim={}",
        num_values, num_dims, num_index_dims, num_bytes_per_dim
      );
    }

    let use_real_writer = num_values > 10000;
    let iwc = if use_real_writer {
      let a = MockAnalyzer::new(random);
      new_index_writer_config_with_analyzer(random, a)?
    } else {
      new_index_writer_config(random)?
    };
    if expect_exceptions && let MergeSchedulerEnum::Concurrent(cms) = iwc.get_merge_scheduler() {
      cms.set_suppress_exceptions();
    }
    let w = RandomIndexWriter::with_config(random, dir.clone(), iwc);
    let mut field_types = HashMap::new();

    let mut expected_min_values = vec![vec![0u8; num_bytes_per_dim]; num_dims];
    let mut expected_max_values = vec![vec![0u8; num_bytes_per_dim]; num_dims];
    #[allow(clippy::needless_range_loop)]
    for ord in 0..doc_values.len() {
      for dim in 0..num_dims {
        let value = &doc_values[ord][dim][..num_bytes_per_dim];
        if ord == 0 {
          expected_min_values[dim].copy_from_slice(value);
          expected_max_values[dim].copy_from_slice(value);
        } else {
          if value < expected_min_values[dim].as_slice() {
            expected_min_values[dim].copy_from_slice(value);
          }
          if value > expected_max_values[dim].as_slice() {
            expected_max_values[dim].copy_from_slice(value);
          }
        }
      }
    }
    // 20% of the time we add into a separate directory, then at some point use
    // addIndexes to bring the indexed point values to the main directory:
    let mut source_dir = None;
    let mut source_w = None;
    let mut add_indexes_at = 0;
    if random.random_range(0..5) == 1 {
      let other_dir = get_directory(random, num_values)?;
      let other_iwc = if use_real_writer {
        let analyzer = MockAnalyzer::new(random);
        new_index_writer_config_with_analyzer(random, analyzer)?
      } else {
        new_index_writer_config(random)?
      };
      if expect_exceptions
        && let MergeSchedulerEnum::Concurrent(cms) = other_iwc.get_merge_scheduler()
      {
        cms.set_suppress_exceptions();
      }
      source_w = Some(RandomIndexWriter::with_config(
        random,
        other_dir.clone(),
        other_iwc,
      ));
      source_dir = Some(other_dir);
      add_indexes_at = TestUtil::next_int(random, 1, num_values as i32 - 1) as usize;
    }

    let field_type = {
      let mut field_type = FieldType::new();
      field_type.set_dimensions_with_index(num_dims, num_index_dims, num_bytes_per_dim)?;
      field_type.freeze();
      field_type
    };

    let mut doc: Option<Document> = None;
    let mut last_id = -1;
    for ord in 0..num_values {
      let id = ids.map_or(ord as i32, |values| values[ord]);
      if id != last_id {
        if let Some(prev_doc) = doc.take() {
          if let Some(source_w) = &source_w {
            if use_real_writer {
              source_w.w.add_document(prev_doc)?;
            } else {
              source_w.add_document(random, prev_doc)?;
            }
          } else if use_real_writer {
            w.w.add_document(prev_doc)?;
          } else {
            w.add_document(random, prev_doc)?;
          }
        }
        let mut new_doc = Document::new();
        new_doc.add(NumericDocValuesField::new("id", id as i64));
        doc = Some(new_doc);
      }
      let mut val = self.flatten_binary_point(&doc_values[ord], num_dims, num_bytes_per_dim);
      if let Some(current_doc) = doc.as_mut() {
        current_doc.add(BinaryPoint::with_type(
          "field",
          val.clone(),
          field_type.clone(),
        )?);
      } else {
        unreachable!("dco should not be none");
      }

      last_id = id;

      if random.random_range(0..30) == 17 {
        if let Some(source_w) = &source_w {
          if use_real_writer {
            source_w.w.add_document(Document::new())?;
          } else {
            source_w.add_document(random, Document::new())?;
          }
        } else if use_real_writer {
          w.w.add_document(Document::new())?;
        } else {
          w.add_document(random, Document::new())?;
        }
        if cfg!(feature = "test_log_verbose") {
          println!("add empty doc");
        }
      }

      if random.random_range(0..30) == 17 {
        let mut xdoc = Document::new();
        val = self.flatten_binary_point(&doc_values[ord], num_dims, num_bytes_per_dim);
        xdoc.add(BinaryPoint::with_type("field", val, field_type.clone())?);
        xdoc.add(new_string_field(
          random,
          "nukeme",
          "yes",
          Store::No,
          &mut field_types,
        )?);
        if let Some(source_w) = &source_w {
          if use_real_writer {
            source_w.w.add_document(xdoc)?;
          } else {
            source_w.add_document(random, xdoc)?;
          }
        } else if use_real_writer {
          w.w.add_document(xdoc)?;
        } else {
          w.add_document(random, xdoc)?;
        }
        if cfg!(feature = "test_log_verbose") {
          println!("add doc doc-to-delete");
        }
        if random.random_range(0..5) == 1 {
          if let Some(source_w) = &source_w {
            if use_real_writer {
              source_w
                .w
                .delete_documents_with_terms(vec![Term::from_text("nukeme", "yes")])?;
            } else {
              source_w
                .delete_documents_with_terms(random, vec![Term::from_text("nukeme", "yes")])?;
            }
          } else if use_real_writer {
            w.w
              .delete_documents_with_terms(vec![Term::from_text("nukeme", "yes")])?;
          } else {
            w.delete_documents_with_terms(random, vec![Term::from_text("nukeme", "yes")])?;
          }
        }
      }

      if cfg!(feature = "test_log_verbose") {
        println!("  ord={} id={}", ord, id);
        for (dim, value) in doc_values[ord].iter().enumerate().take(num_dims) {
          println!("    dim={} value={:?}", dim, value);
        }
      }

      if source_w.is_some() && ord >= add_indexes_at {
        self.switch_index(
          random,
          source_w.take().expect("source writer must exist"),
          source_dir.take().expect("source directory must exist"),
          &w,
        )?;
      }
    }

    if let Some(final_doc) = doc.take() {
      w.add_document(random, final_doc)?;
    } else {
      unreachable!("dco should not be none");
    }
    w.delete_documents_with_terms(random, vec![Term::from_text("nukeme", "yes")])?;

    if random.random_bool(0.5) {
      if cfg!(feature = "test_log_verbose") {
        println!("\nTEST: now force merge");
      }
      w.force_merge(random, 1)?;
    }

    let r = w.get_reader(random)?;
    w.close(random)?;

    if cfg!(feature = "test_log_verbose") {
      println!("TEST: reader opened");
    }

    let context = (&r).get_context()?;
    let mut doc_id_to_id = vec![0i32; r.max_doc()? as usize];
    let mut id_values = MultiDocValues::get_numeric_values(&r, "id")?.unwrap();
    loop {
      let doc_id = id_values.next_doc()?;
      if doc_id == NO_MORE_DOCS {
        break;
      }
      doc_id_to_id[doc_id as usize] = id_values.long_value()? as i32;
    }
    let live_docs = get_live_docs(&r)?;
    let mut min_values = vec![0xff; num_index_dims * num_bytes_per_dim];
    let mut max_values = vec![0u8; num_index_dims * num_bytes_per_dim];

    for ctx in context.leaves()? {
      let dim_values = match ctx.reader().get_point_values("field")? {
        Some(values) => values,
        None => continue,
      };

      self.assert_size(random, &mut dim_values.get_point_tree()?)?;
      let leaf_min_values = dim_values.get_min_packed_value()?.unwrap();
      let leaf_max_values = dim_values.get_max_packed_value()?.unwrap();

      for dim in 0..num_index_dims {
        let offset = dim * num_bytes_per_dim;
        if leaf_min_values.as_ref()[offset..offset + num_bytes_per_dim]
          < min_values[offset..offset + num_bytes_per_dim]
        {
          min_values[offset..offset + num_bytes_per_dim]
            .copy_from_slice(&leaf_min_values.as_ref()[offset..offset + num_bytes_per_dim]);
        }
        if leaf_max_values.as_ref()[offset..offset + num_bytes_per_dim]
          > max_values[offset..offset + num_bytes_per_dim]
        {
          max_values[offset..offset + num_bytes_per_dim]
            .copy_from_slice(&leaf_max_values.as_ref()[offset..offset + num_bytes_per_dim]);
        }
      }
    }

    let mut scratch = vec![0u8; num_bytes_per_dim];
    for dim in 0..num_index_dims {
      let offset = dim * num_bytes_per_dim;
      scratch.copy_from_slice(&min_values[offset..offset + num_bytes_per_dim]);
      assert_eq!(expected_min_values[dim].as_slice(), scratch.as_slice());
      scratch.copy_from_slice(&max_values[offset..offset + num_bytes_per_dim]);
      assert_eq!(expected_max_values[dim].as_slice(), scratch.as_slice());
    }

    let iters = at_least(random, 100);
    for iter in 0..iters {
      if cfg!(feature = "test_log_verbose") {
        println!("\nTEST: iter={}", iter);
      }

      let mut query_min = vec![vec![0u8; num_bytes_per_dim]; num_index_dims];
      let mut query_max = vec![vec![0u8; num_bytes_per_dim]; num_index_dims];
      for dim in 0..num_index_dims {
        random.fill(query_min[dim].as_mut_slice());
        random.fill(query_max[dim].as_mut_slice());
        if query_min[dim].as_slice() > query_max[dim].as_slice() {
          let min_slice = query_min.get_mut(dim).expect("query_min dim");
          let max_slice = query_max.get_mut(dim).expect("query_max dim");
          min_slice.swap_with_slice(max_slice);
        }
      }

      if cfg!(feature = "test_log_verbose") {
        for dim in 0..num_index_dims {
          println!(
            "  dim={}\n    queryMin={:?}\n    queryMax={:?}",
            dim, query_min[dim], query_max[dim]
          );
        }
      }

      let mut hits = bit_set::BitSet::new();

      for ctx in context.leaves()? {
        let dim_values = match ctx.reader().get_point_values("field")? {
          Some(values) => values,
          None => continue,
        };

        let doc_base = ctx.doc_base;
        dim_values.intersect(&mut VerifyIntersectVisitor {
          hits: &mut hits,
          query_min: &query_min,
          query_max: &query_max,
          live_docs: live_docs.as_ref(),
          doc_id_to_id: &doc_id_to_id,
          doc_base,
          num_index_dims,
          num_bytes_per_dim,
        })?;
      }

      let mut expected = bit_set::BitSet::new();
      for ord in 0..num_values {
        let mut matches = true;
        for dim in 0..num_index_dims {
          let x = &doc_values[ord][dim][..num_bytes_per_dim];
          if x < query_min[dim].as_slice() || x > query_max[dim].as_slice() {
            matches = false;
            break;
          }
        }

        if matches {
          let id = ids.map_or(ord as i32, |values| values[ord]);
          expected.insert(id as usize);
        }
      }
      let v1 = expected.iter().max().map_or(0, |i| i + 1);
      let v2 = hits.iter().max().map_or(0, |i| i + 1);
      let limit = std::cmp::max(v1, v2);
      let mut fail_count = 0;
      let mut success_count = 0;
      for id in 0..limit {
        if expected.contains(id) != hits.contains(id) {
          println!("FAIL: id={}", id);
          fail_count += 1;
        } else {
          success_count += 1;
        }
      }

      if fail_count != 0 {
        for doc_id in 0..r.max_doc()? {
          println!("  docID={} id={}", doc_id, doc_id_to_id[doc_id as usize]);
        }
        return Err(LuceneError::illegal_state(format!(
          "{} docs failed; {} docs succeeded",
          fail_count, success_count
        )));
      }
    }

    Ok(())
  }

  fn assert_size<R, T>(&self, random: &mut R, tree: &mut T) -> Result<()>
  where
    R: Rng + ?Sized,
    T: PointTree,
  {
    let mut clone = tree.try_clone()?;
    assert_eq!(clone.size()?, tree.size()?);

    let tree: &mut T = if rarely(random) { &mut clone } else { tree };

    let mut visitor = AssertSizeIntersectVisitor::default();

    if random.random_bool(0.5) {
      tree.visit_doc_ids(&mut visitor)?;
      tree.visit_doc_values(&mut visitor)?;
    } else {
      tree.visit_doc_values(&mut visitor)?;
      tree.visit_doc_ids(&mut visitor)?;
    }

    assert_eq!(visitor.visit_doc_id_size, visitor.visit_doc_values_size);
    assert_eq!(visitor.visit_doc_id_size as usize, tree.size()?);

    if tree.move_to_child()? {
      loop {
        self.random_point_tree_navigation(random, tree)?;
        self.assert_size(random, tree)?;
        if !tree.move_to_sibling()? {
          break;
        }
      }
      tree.move_to_parent()?;
    }

    Ok(())
  }

  fn random_point_tree_navigation<R, T>(&self, random: &mut R, tree: &mut T) -> Result<()>
  where
    R: Rng + ?Sized,
    T: PointTree,
  {
    let min_packed_value = tree.get_min_packed_value()?.into_owned();
    let max_packed_value = tree.get_max_packed_value()?.into_owned();
    let size = tree.size()?;

    if random.random_bool(0.5) && tree.move_to_child()? {
      self.random_point_tree_navigation(random, tree)?;
      if random.random_bool(0.5) && tree.move_to_sibling()? {
        self.random_point_tree_navigation(random, tree)?;
      }
      tree.move_to_parent()?;
    }

    assert_eq!(
      min_packed_value.as_slice(),
      tree.get_min_packed_value()?.as_ref()
    );
    assert_eq!(
      max_packed_value.as_slice(),
      tree.get_max_packed_value()?.as_ref()
    );
    assert_eq!(size, tree.size()?);
    Ok(())
  }

  fn test_add_indexes<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir1 = new_directory_shared(random)?;
    let w = RandomIndexWriter::new(random, dir1.clone())?;
    let mut doc = Document::new();
    doc.add(IntPoint::new("int1", vec![17])?);
    w.add_document(random, doc)?;
    let mut doc = Document::new();
    doc.add(IntPoint::new("int2", vec![42])?);
    w.add_document(random, doc)?;
    w.close(random)?;

    // Different field number assigments:
    let dir2 = new_directory_shared(random)?;
    let w = RandomIndexWriter::new(random, dir2.clone())?;
    let mut doc = Document::new();
    doc.add(IntPoint::new("int2", vec![42])?);
    w.add_document(random, doc)?;
    let mut doc = Document::new();
    doc.add(IntPoint::new("int1", vec![17])?);
    w.add_document(random, doc)?;
    w.close(random)?;

    let dir = new_directory_shared(random)?;
    let w = RandomIndexWriter::new(random, dir.clone())?;
    w.add_indexes_from_dir(random, &[dir1.clone(), dir2.clone()])?;
    w.force_merge(random, 1)?;

    let s = new_searcher_with_reader(w.get_reader(random)?)?;
    assert_eq!(2, s.count(IntPoint::new_exact_query("int1", 17)?)?);
    assert_eq!(2, s.count(IntPoint::new_exact_query("int2", 42)?)?);
    w.close(random)?;
    Ok(())
  }

  fn switch_index<R, D>(
    &self,
    random: &mut R,
    w: RandomIndexWriter<DirEnum>,
    dir: Arc<DirEnum>,
    save_w: &RandomIndexWriter<D>,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
    D: Directory + 'static,
  {
    if random.random_bool(0.5) {
      // Add via readers:
      let reader = w.get_reader(random)?;
      let body_result = if random.random_bool(0.5) {
        // Add via CodecReaders:
        let context = (&reader).get_context()?;
        let readers = context
          .leaves()?
          .iter()
          .map(|context| context.reader().clone())
          .collect::<Vec<_>>();
        if cfg!(feature = "test_log_verbose") {
          println!("TEST: now use addIndexes(CodecReader[]) to switch writers");
        }
        save_w
          .add_indexes_from_codec_readers(random, readers)
          .map(|_| ())
      } else {
        if cfg!(feature = "test_log_verbose") {
          println!("TEST: now use TestUtil.addIndexesSlowly(DirectoryReader[]) to switch writers");
        }
        TestUtil::add_indexes_slowly(&save_w.w, std::slice::from_ref(&reader))
      };
      IOUtils::use_or_suppress_result(body_result, reader.close())?;
    } else {
      // Add via directory:
      if cfg!(feature = "test_log_verbose") {
        println!("TEST: now use addIndexes(Directory[]) to switch writers");
      }
      w.close(random)?;
      save_w
        .w
        .add_indexes_from_directory(std::slice::from_ref(&dir))?;
    }
    let close_result = w.close(random);
    IOUtils::use_or_suppress_result(close_result, dir.close())
  }

  fn test_merge_missing<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let mut iwc = new_index_writer_config(random)?;
    iwc.set_max_buffered_docs(2);
    let w = RandomIndexWriter::with_config(random, dir.clone(), iwc);

    for i in 0..2 {
      let mut doc = Document::new();
      doc.add(IntPoint::new("int", vec![i])?);
      w.add_document(random, doc)?;
    }
    // index has 1 segment now (with 2 docs) and that segment does have points

    let mut doc = Document::new();
    doc.add(IntPoint::new("id", vec![0])?);
    w.add_document(random, doc)?;
    // now we write another segment where the id field does have points:
    w.force_merge(random, 1)?;
    w.close(random)?;
    Ok(())
  }

  fn test_doc_count_edge_cases(&self) -> Result<()> {
    let visitor = AlwaysInsideIntersectVisitor;

    let mut values = get_point_values(i64::MAX as usize, 1, i64::MAX as usize);
    let docs = values.estimate_doc_count(&visitor)?;
    assert_eq!(1, docs);

    values = get_point_values(i64::MAX as usize, 1, 1);
    let docs = values.estimate_doc_count(&visitor)?;
    assert_eq!(1, docs);

    values = get_point_values(i64::MAX as usize, i32::MAX, i64::MAX as usize);
    let docs = values.estimate_doc_count(&visitor)?;
    assert_eq!(i32::MAX as i64, docs);

    values = get_point_values(i64::MAX as usize, i32::MAX, (i64::MAX / 2) as usize);
    let docs = values.estimate_doc_count(&visitor)?;
    assert_eq!(i32::MAX as i64, docs);

    values = get_point_values(i64::MAX as usize, i32::MAX, 1);
    let docs = values.estimate_doc_count(&visitor)?;
    assert_eq!(1, docs);

    Ok(())
  }

  fn test_random_doc_count<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    for _ in 0..100 {
      let size = TestUtil::next_long(random, 1, i64::MAX) as usize;
      let max_doc = if size > i32::MAX as usize {
        i32::MAX
      } else {
        size as i32
      };
      let doc_count = TestUtil::next_int(random, 1, max_doc);
      let estimated_point_count = TestUtil::next_long(random, 0, size as i64) as usize;
      let values = get_point_values(size, doc_count, estimated_point_count);
      let docs = values.estimate_doc_count(&AlwaysInsideIntersectVisitor)?;

      assert!(docs <= estimated_point_count as i64);
      assert!(docs <= max_doc as i64);
      assert!(docs >= estimated_point_count as i64 / (size as i64 / doc_count as i64));
    }

    Ok(())
  }
  fn test_mismatched_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    struct MismatchedPointVisitor<'a> {
      expected: &'a [u8],
      expected_doc: i32,
    }

    impl IntersectVisitor for MismatchedPointVisitor<'_> {
      fn visit(&mut self, _doc_id: i32) -> Result<()> {
        Err(LuceneError::illegal_state(
          "unexpected visit(doc_id) in test_mismatched_fields",
        ))
      }

      fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()> {
        assert_eq!(self.expected_doc, doc_id);
        self.expected_doc += 1;
        assert_eq!(self.expected, packed_value);
        Ok(())
      }

      fn compare(&self, _min_packed: &[u8], _max_packed: &[u8]) -> Result<Relation> {
        Ok(Relation::CellCrossesQuery)
      }
    }

    let dir1 = new_directory_shared(random)?;
    let w1 = IndexWriter::new(dir1.clone(), new_index_writer_config(random)?)?;
    let mut doc = Document::new();
    doc.add(LongPoint::new("f", [1_i64])?);
    doc.add(LongPoint::new("g", [42_i64, 43_i64])?);
    w1.add_document(doc.clone())?;

    let dir2 = new_directory_shared(random)?;
    let mut iwc = new_index_writer_config(random)?;
    iwc.set_merge_scheduler(MergeSchedulerEnum::from(SerialMergeScheduler::new()));
    let w2 = IndexWriter::new(dir2.clone(), iwc)?;
    w2.add_document(doc)?;
    w2.commit()?;

    let reader = directory_reader::open_from_writer(&w1)?;
    w1.close()?;
    let leaf = get_only_leaf_reader(&reader)?;
    let mismatched = MismatchedCodecReader::new(leaf, random)?;
    w2.add_indexes_from_codec_readers(vec![mismatched])?;
    reader.close()?;
    w2.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&w2)?;
    w2.close()?;
    let leaf = get_only_leaf_reader(&reader)?;
    assert_eq!(2, leaf.max_doc()?);

    let f_points = leaf.get_point_values("f")?.expect("f points should exist");
    assert_eq!(2, f_points.size()?);
    let expected_f = LongPoint::pack([1_i64])?.bytes;
    f_points.intersect(&mut MismatchedPointVisitor {
      expected: &expected_f,
      expected_doc: 0,
    })?;

    let g_points = leaf.get_point_values("g")?.expect("g points should exist");
    assert_eq!(2, g_points.size()?);
    let expected_g = LongPoint::pack([42_i64, 43_i64])?.bytes;
    g_points.intersect(&mut MismatchedPointVisitor {
      expected: &expected_g,
      expected_doc: 0,
    })?;

    reader.close()?;
    dir1.close()?;
    dir2.close()?;
    Ok(())
  }
}
fn random_big_int<R>(random: &mut R, num_bytes: usize) -> BigInt
where
  R: Rng + ?Sized,
{
  let num_bits = num_bytes * 8 - 1;
  let num_rand_bytes = num_bits.div_ceil(8);

  let mut bytes = vec![0u8; num_rand_bytes];
  random.fill_bytes(&mut bytes);

  let excess_bits = num_rand_bytes * 8 - num_bits;
  if excess_bits > 0 {
    bytes[0] &= 0xFF >> excess_bits;
  }

  let x = BigUint::from_bytes_be(&bytes);
  let x = BigInt::from(x);

  if random.random_bool(0.5) { -x } else { x }
}
fn get_directory<R>(random: &mut R, num_points: usize) -> Result<Arc<DirEnum>>
where
  R: Rng + ?Sized,
{
  if num_points > 100000 {
    new_fs_directory(random, create_temp_dir()?)
  } else {
    new_directory_shared(random)
  }
}
struct VerifyIntersectVisitor<'a, B>
where
  B: Bits,
{
  hits: &'a mut bit_set::BitSet,
  query_min: &'a [Vec<u8>],
  query_max: &'a [Vec<u8>],
  live_docs: Option<&'a B>,
  doc_id_to_id: &'a [i32],
  doc_base: usize,
  num_index_dims: usize,
  num_bytes_per_dim: usize,
}

impl<B> IntersectVisitor for VerifyIntersectVisitor<'_, B>
where
  B: Bits,
{
  fn compare(&self, min_packed: &[u8], max_packed: &[u8]) -> Result<Relation> {
    let mut crosses = false;
    for dim in 0..self.num_index_dims {
      let offset = dim * self.num_bytes_per_dim;
      let min = &min_packed[offset..offset + self.num_bytes_per_dim];
      let max = &max_packed[offset..offset + self.num_bytes_per_dim];
      if max < self.query_min[dim].as_slice() || min > self.query_max[dim].as_slice() {
        return Ok(Relation::CellOutsideQuery);
      } else if min < self.query_min[dim].as_slice() || max > self.query_max[dim].as_slice() {
        crosses = true;
      }
    }

    if crosses {
      Ok(Relation::CellCrossesQuery)
    } else {
      Ok(Relation::CellInsideQuery)
    }
  }

  fn visit(&mut self, doc_id: i32) -> Result<()> {
    let doc_id = self.doc_base + doc_id as usize;
    if self
      .live_docs
      .is_none_or(|bits| bits.get(doc_id).expect(""))
    {
      self.hits.insert(self.doc_id_to_id[doc_id] as usize);
    }
    Ok(())
  }

  fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()> {
    let doc_id = self.doc_base + doc_id as usize;
    if self
      .live_docs
      .is_some_and(|bits| !bits.get(doc_id).expect(""))
    {
      return Ok(());
    }

    for dim in 0..self.num_index_dims {
      let offset = dim * self.num_bytes_per_dim;
      let value = &packed_value[offset..offset + self.num_bytes_per_dim];
      if value < self.query_min[dim].as_slice() || value > self.query_max[dim].as_slice() {
        return Ok(());
      }
    }

    self.hits.insert(self.doc_id_to_id[doc_id] as usize);
    Ok(())
  }
}

struct BasicIntersectVisitor<'a> {
  seen: &'a mut FixedBitSet,
}

impl IntersectVisitor for BasicIntersectVisitor<'_> {
  fn compare(&self, _min_packed: &[u8], _max_packed: &[u8]) -> Result<Relation> {
    Ok(Relation::CellCrossesQuery)
  }

  fn visit(&mut self, _doc_id: i32) -> Result<()> {
    Err(LuceneError::illegal_state(
      "unexpected visit(doc_id) in test_basic",
    ))
  }

  fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()> {
    self.seen.set(doc_id as usize);
    assert_eq!(doc_id, NumericUtils::sortable_bytes_to_int(packed_value, 0));
    Ok(())
  }
}

struct AllPointDocsDeletedIntersectVisitor<'a, B>
where
  B: Bits,
{
  seen: &'a mut FixedBitSet,
  live_docs: Option<&'a B>,
  doc_id_to_id: &'a [i32],
}

impl<B> IntersectVisitor for AllPointDocsDeletedIntersectVisitor<'_, B>
where
  B: Bits,
{
  fn compare(&self, _min_packed: &[u8], _max_packed: &[u8]) -> Result<Relation> {
    Ok(Relation::CellCrossesQuery)
  }

  fn visit(&mut self, _doc_id: i32) -> Result<()> {
    Err(LuceneError::illegal_state(
      "unexpected visit(doc_id) in test_all_point_docs_deleted_in_segment",
    ))
  }

  fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()> {
    if self
      .live_docs
      .is_some_and(|bits| bits.get(doc_id as usize).expect(""))
    {
      self.seen.set(doc_id as usize);
    }
    assert_eq!(
      self.doc_id_to_id[doc_id as usize],
      NumericUtils::sortable_bytes_to_int(packed_value, 0)
    );
    Ok(())
  }
}
#[derive(Default)]
struct AssertSizeIntersectVisitor {
  visit_doc_id_size: i64,
  visit_doc_values_size: i64,
}

impl IntersectVisitor for AssertSizeIntersectVisitor {
  fn visit(&mut self, _doc_id: i32) -> Result<()> {
    self.visit_doc_id_size += 1;
    Ok(())
  }

  fn visit_with_packed_value(&mut self, _doc_id: i32, _packed_value: &[u8]) -> Result<()> {
    self.visit_doc_values_size += 1;
    Ok(())
  }

  fn compare(&self, _min_packed_value: &[u8], _max_packed_value: &[u8]) -> Result<Relation> {
    Ok(Relation::CellCrossesQuery)
  }
}
struct BigIntNDimsIntersectVisitor<'a> {
  hits: &'a mut FixedBitSet,
  doc_base: i32,
  num_dims: usize,
  num_bytes_per_dim: usize,
  query_min: &'a [BigInt],
  query_max: &'a [BigInt],
}

impl IntersectVisitor for BigIntNDimsIntersectVisitor<'_> {
  fn visit(&mut self, doc_id: i32) -> Result<()> {
    self.hits.set((self.doc_base + doc_id) as usize);
    Ok(())
  }

  fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()> {
    for dim in 0..self.num_dims {
      let x = NumericUtils::sortable_bytes_to_big_int(
        packed_value,
        dim * self.num_bytes_per_dim,
        self.num_bytes_per_dim,
      )?;
      if x < self.query_min[dim] || x > self.query_max[dim] {
        return Ok(());
      }
    }

    self.hits.set((self.doc_base + doc_id) as usize);
    Ok(())
  }

  fn compare(&self, min_packed: &[u8], max_packed: &[u8]) -> Result<Relation> {
    let mut crosses = false;
    for dim in 0..self.num_dims {
      let min = NumericUtils::sortable_bytes_to_big_int(
        min_packed,
        dim * self.num_bytes_per_dim,
        self.num_bytes_per_dim,
      )?;
      let max = NumericUtils::sortable_bytes_to_big_int(
        max_packed,
        dim * self.num_bytes_per_dim,
        self.num_bytes_per_dim,
      )?;
      assert!(max >= min);

      if max < self.query_min[dim] || min > self.query_max[dim] {
        return Ok(Relation::CellOutsideQuery);
      } else if min < self.query_min[dim] || max > self.query_max[dim] {
        crosses = true;
      }
    }

    if crosses {
      Ok(Relation::CellCrossesQuery)
    } else {
      Ok(Relation::CellInsideQuery)
    }
  }
}

fn get_point_values(size: usize, doc_count: i32, estimated_point_count: usize) -> TestPointValues {
  TestPointValues {
    size,
    doc_count,
    estimated_point_count,
  }
}

#[derive(Clone)]
struct TestPointValues {
  size: usize,
  doc_count: i32,
  estimated_point_count: usize,
}

impl PointValues for TestPointValues {
  fn get_min_packed_value(&self) -> Result<Option<std::borrow::Cow<'_, [u8]>>> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_max_packed_value(&self) -> Result<Option<std::borrow::Cow<'_, [u8]>>> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_num_dimensions(&self) -> Result<usize> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_num_index_dimensions(&self) -> Result<usize> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_bytes_per_dimension(&self) -> Result<usize> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn size(&self) -> Result<usize> {
    Ok(self.size)
  }

  fn get_doc_count(&self) -> Result<i32> {
    Ok(self.doc_count)
  }

  type PointTree = TestPointTree;
  type MutablePointTree = DummyMutablePointTree;

  fn get_point_tree(&self) -> Result<PointTreeEnum<Self::MutablePointTree, Self::PointTree>> {
    Ok(PointTreeEnum::Other(TestPointTree {
      estimated_point_count: self.estimated_point_count,
    }))
  }
}

#[derive(Clone)]
#[allow(dead_code)] // for quick search
struct TestPointTree {
  estimated_point_count: usize,
}

impl TryClone for TestPointTree {
  fn try_clone(&self) -> Result<Self>
  where
    Self: Sized,
  {
    Err(LuceneError::unsupported_operation(""))
  }
}

impl PointTree for TestPointTree {
  fn move_to_child(&mut self) -> Result<bool> {
    Ok(false)
  }

  fn move_to_sibling(&mut self) -> Result<bool> {
    Ok(false)
  }

  fn move_to_parent(&mut self) -> Result<bool> {
    Ok(false)
  }

  fn get_min_packed_value(&self) -> Result<std::borrow::Cow<'_, [u8]>> {
    Ok(std::borrow::Cow::Borrowed(&[]))
  }

  fn get_max_packed_value(&self) -> Result<std::borrow::Cow<'_, [u8]>> {
    Ok(std::borrow::Cow::Borrowed(&[]))
  }

  fn size(&self) -> Result<usize> {
    Ok(self.estimated_point_count)
  }

  fn visit_doc_ids<IV>(&mut self, _visitor: &mut IV) -> Result<()>
  where
    IV: IntersectVisitor,
  {
    Err(LuceneError::unsupported_operation(""))
  }

  fn visit_doc_values<IV>(&mut self, _visitor: &mut IV) -> Result<()>
  where
    IV: IntersectVisitor,
  {
    Err(LuceneError::unsupported_operation(""))
  }
}

#[derive(Clone, Copy)]
struct AlwaysInsideIntersectVisitor;

impl IntersectVisitor for AlwaysInsideIntersectVisitor {
  fn visit(&mut self, _doc_id: i32) -> Result<()> {
    Ok(())
  }

  fn visit_with_packed_value(&mut self, _doc_id: i32, _packed_value: &[u8]) -> Result<()> {
    Ok(())
  }

  fn compare(&self, _min_packed_value: &[u8], _max_packed_value: &[u8]) -> Result<Relation> {
    Ok(Relation::CellInsideQuery)
  }
}
