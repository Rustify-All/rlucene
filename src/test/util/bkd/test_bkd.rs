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
use crate::core::codecs::mutable_point_tree::MutablePointTree;
use crate::core::index::BytesRef;
use crate::core::index::dummy::dummy_codec_reader::DummyCodecReader;
use crate::core::index::merge_state::{DocMap, DocMapEnum};
use crate::core::index::point_values::{
    IntersectVisitor, MAX_DIMENSIONS, MAX_INDEX_DIMENSIONS, MAX_NUM_BYTES, PointTree, PointValues,
    Relation,
};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::store::directory::Directory;
use crate::core::store::{DataInput, IOContext, IndexInput, IndexOutput};
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bkd::bkd_config::BKDConfig;
use crate::core::util::bkd::bkd_reader::BKDReader;
use crate::core::util::bkd::bkd_writer::{
    BKDWriter, DEFAULT_MAX_MB_SORT_IN_HEAP, VERSION_META_FILE,
};
use crate::core::util::clone::TryClone;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::numeric_utils::NumericUtils;
use crate::core::util::{SliceCopyOps, ToInt, TryIntoInt};
use crate::test::util::lucene_test_case::lucene_test_case_util::{
    at_least, new_directory, new_directory_shared, random, random_from_seed,
};
use crate::test::util::test_util::TestUtil;
use bit_set::BitSet;
use num_bigint::{BigInt, Sign};
use num_traits::Zero;
use parking_lot::Mutex;
use rand::prelude::StdRng;
use rand::{Rng, RngCore};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

#[allow(dead_code)] // for quick search
struct TestBKD;

fn get_point_values<I: IndexInput>(index_input: Arc<Mutex<I>>) -> Result<BKDReader<I>> {
    let meta_in = &mut *index_input.lock();
    let (mut reader, version) = BKDReader::init_with_meta(meta_in, index_input.clone())?;
    let (min_leaf_block_fp, index_start_pointer) = if version >= VERSION_META_FILE {
        (
            DataInput::read_long(meta_in)?,
            DataInput::read_long(meta_in)?.try_convert()?,
        )
    } else {
        let index_start_pointer = meta_in.get_file_pointer()?;
        let min_leaf_block_fp = meta_in.read_vlong()?;
        meta_in.seek(index_start_pointer)?;
        (min_leaf_block_fp, index_start_pointer)
    };
    reader.same_in = true;
    reader.min_leaf_block_fp = min_leaf_block_fp;
    reader.index_start_pointer = index_start_pointer;
    reader.is_tree_balanced = reader.num_leaves != 1 && reader.is_tree_balanced()?;
    Ok(reader)
}
#[test]
fn test_basic_ints_1d() -> Result<()> {
    let mut random = random();
    let config = BKDConfig::new(1, 1, 4, 2)?;
    let dir = new_directory_shared(&mut random)?;

    {
        let mut writer = BKDWriter::new(100, dir.as_ref(), "tmp", config.clone(), 1.0, 100)?;
        let mut scratch = [0u8; 4];

        for doc_id in 0..100 {
            NumericUtils::int_to_sortable_bytes(doc_id, &mut scratch, 0);
            writer.add(&scratch, doc_id)?;
        }

        let index_fp;
        {
            let mut out = dir.create_output("bkd", &IOContext::default_io_context()?)?;
            let finalizer = writer.finish(&mut out)?.unwrap();
            {
                index_fp = out.get_file_pointer();
            }
            writer.write_index(&mut out, None, &finalizer)?;
        }

        {
            let mut input = dir.open_input("bkd", &IOContext::default_io_context()?)?;
            input.seek(index_fp)?;
            let sub_point_values = get_point_values(Arc::new(Mutex::new(input)))?;

            // Simple 1D range query:
            let mut query_min = vec![vec![0u8; 4]];
            NumericUtils::int_to_sortable_bytes(42, &mut query_min[0], 0);
            let mut query_max = vec![vec![0u8; 4]];
            NumericUtils::int_to_sortable_bytes(87, &mut query_max[0], 0);

            let mut hits = BitSet::new();
            let mut visitor = IntersectVisitorImpl {
                hits: &mut hits,
                query_min: &query_min,
                query_max: &query_max,
                config: config.clone(),
                random: &mut random,
            };
            let r = sub_point_values;
            r.intersect(&mut visitor)?;

            for doc_id in 0..100 {
                let expected = (42..=87).contains(&doc_id);
                let actual = hits.contains(doc_id);
                assert_eq!(expected, actual, "docID={}", doc_id);
            }
        }
    }

    Ok(())
}

#[test]
fn test_random_ints_n_dims() -> Result<()> {
    let mut random = random();
    let num_docs = at_least(&mut random, 1000);
    let dir = new_directory_shared(&mut random)?;
    let num_dims = TestUtil::next_usize(&mut random, 1, 5);
    let num_index_dims = TestUtil::next_usize(&mut random, 1, num_dims);
    let max_points_in_leaf_node = TestUtil::next_usize(&mut random, 50, 100);
    let max_mb: f32 = 3.0 + (3.0 * random.random::<f32>());
    let config = BKDConfig::new(num_dims, num_index_dims, 4, max_points_in_leaf_node)?;
    let mut writer = BKDWriter::new(
        num_docs,
        dir.as_ref(),
        "tmp",
        config.clone(),
        max_mb as f64,
        num_docs as i64,
    )?;
    let num_dims = num_dims as usize;
    let mut docs = vec![vec![]; num_docs as usize];
    let mut scratch = vec![0u8; 4 * num_dims];
    let mut min_value = vec![i32::MAX; num_dims];
    let mut max_value = vec![i32::MIN; num_dims];

    for doc_id in 0..num_docs {
        let mut values = vec![0; num_dims];
        if cfg!(feature = "test_log_verbose") {
            println!("doc_id={}", doc_id);
        }
        for dim in 0..num_dims {
            values[dim] = random.random();
            min_value[dim] = min_value[dim].min(values[dim]);
            max_value[dim] = max_value[dim].max(values[dim]);
            NumericUtils::int_to_sortable_bytes(
                values[dim],
                &mut scratch,
                dim * BitUtil::INT_BYTES,
            );
            if cfg!(feature = "test_log_verbose") {
                println!("    {} -> {}", doc_id, values[dim]);
            }
        }
        docs[doc_id as usize] = values;
        writer.add(&scratch, doc_id)?;
    }

    let index_fp;
    {
        let mut out = dir.create_output("bkd", &IOContext::default_io_context()?)?;
        let finalizer = writer.finish(&mut out)?.unwrap();
        {
            index_fp = out.get_file_pointer();
        }
        writer.write_index(&mut out, None, &finalizer)?;
    }

    {
        let mut input = dir.open_input("bkd", &IOContext::default_io_context()?)?;
        input.seek(index_fp)?;
        let sub_point_values = get_point_values(Arc::new(Mutex::new(input)))?;
        let r = sub_point_values;

        let min_packed_value = r.get_min_packed_value()?.unwrap();
        let max_packed_value = r.get_max_packed_value()?.unwrap();
        for dim in 0..num_index_dims as usize {
            assert_eq!(
                min_value[dim],
                NumericUtils::sortable_bytes_to_int(
                    min_packed_value.as_ref(),
                    dim * BitUtil::INT_BYTES,
                ),
                "Mismatch in min value for dim {}",
                dim
            );
            assert_eq!(
                max_value[dim],
                NumericUtils::sortable_bytes_to_int(
                    max_packed_value.as_ref(),
                    dim * BitUtil::INT_BYTES,
                ),
                "Mismatch in max value for dim {}",
                dim
            );
        }

        let iters = at_least(&mut random, 100);
        for iter in 0..iters {
            if cfg!(feature = "test_log_verbose") {
                println!("TEST: iter={}", iter);
            }
            let mut query_min = vec![0; num_dims];
            let mut query_min_bytes = vec![vec![0u8; 4]; num_dims];
            let mut query_max = vec![0; num_dims];
            let mut query_max_bytes = vec![vec![0u8; 4]; num_dims];

            for dim in 0..num_index_dims as usize {
                query_min[dim] = random.random();
                query_max[dim] = random.random();
                if query_min[dim] > query_max[dim] {
                    std::mem::swap(&mut query_min[dim], &mut query_max[dim]);
                }
                NumericUtils::int_to_sortable_bytes(query_min[dim], &mut query_min_bytes[dim], 0);
                NumericUtils::int_to_sortable_bytes(query_max[dim], &mut query_max_bytes[dim], 0);
            }

            let mut hits = BitSet::new();
            let mut visitor = IntersectVisitorImpl {
                hits: &mut hits,
                query_min: &query_min_bytes,
                query_max: &query_max_bytes,
                config: config.clone(),
                random: &mut random,
            };

            r.intersect(&mut visitor)?;

            for (doc_id, doc_values) in docs.iter().enumerate() {
                let mut expected = true;
                for dim in 0..num_index_dims as usize {
                    let x = doc_values[dim];
                    if x < query_min[dim] || x > query_max[dim] {
                        expected = false;
                        break;
                    }
                }
                let actual = hits.contains(doc_id);
                assert_eq!(expected, actual, "docID={}", doc_id);
            }
        }
    }

    Ok(())
}
// Tests on N-dimensional points where each dimension is a BigInteger
#[test]
fn test_big_int_n_dims() -> Result<()> {
    let mut random = random();
    let num_docs = at_least(&mut random, 1000);
    let dir = new_directory_shared(&mut random)?;

    let num_bytes_per_dim = TestUtil::next_usize(&mut random, 2, 30);
    let num_dims = TestUtil::next_usize(&mut random, 1, 5);
    let max_points_in_leaf_node = TestUtil::next_usize(&mut random, 50, 100);
    let max_mb: f32 = 3.0 + (3.0 * random.random::<f32>());
    let config = BKDConfig::new(
        num_dims,
        num_dims,
        num_bytes_per_dim,
        max_points_in_leaf_node,
    )?;
    let mut writer = BKDWriter::new(
        num_docs,
        dir.as_ref(),
        "tmp",
        config.clone(),
        max_mb as f64,
        num_docs as i64,
    )?;

    let num_bytes_per_dim = num_bytes_per_dim as usize;
    let num_dims = num_dims as usize;
    let mut docs = vec![vec![]; num_docs as usize];
    let mut scratch = vec![0u8; num_bytes_per_dim * num_dims];

    for doc_id in 0..num_docs {
        let mut values = vec![BigInt::zero(); num_dims];
        if cfg!(feature = "test_log_verbose") {
            println!("  doc_id={}", doc_id);
        }
        for (dim, value) in values.iter_mut().enumerate().take(num_dims) {
            *value = random_big_int(num_bytes_per_dim, &mut random);
            NumericUtils::big_int_to_sortable_bytes(
                value,
                num_bytes_per_dim,
                &mut scratch,
                dim * num_bytes_per_dim,
            )?;
            if cfg!(feature = "test_log_verbose") {
                println!("    {} -> {}", dim, value);
            }
        }

        docs[doc_id as usize] = values;
        writer.add(&scratch, doc_id)?;
    }

    let index_fp;
    {
        let mut out = dir.create_output("bkd", &IOContext::default_io_context()?)?;
        let finalizer = writer.finish(&mut out)?.unwrap();
        {
            index_fp = out.get_file_pointer();
        }
        writer.write_index(&mut out, None, &finalizer)?;
    }

    {
        let mut input = dir.open_input("bkd", &IOContext::default_io_context()?)?;
        input.seek(index_fp)?;
        let sub_point_values = get_point_values(Arc::new(Mutex::new(input)))?;
        let point_values = sub_point_values;

        let iters = at_least(&mut random, 100);
        for iter in 0..iters {
            if cfg!(feature = "test_log_verbose") {
                println!("TEST: iter={}", iter);
            }
            let mut query_min = vec![BigInt::zero(); num_dims];
            let mut query_min_bytes = vec![vec![0u8; num_bytes_per_dim]; num_dims];
            let mut query_max = vec![BigInt::zero(); num_dims];
            let mut query_max_bytes = vec![vec![0u8; num_bytes_per_dim]; num_dims];

            for dim in 0..num_dims {
                query_min[dim] = random_big_int(num_bytes_per_dim, &mut random);
                query_max[dim] = random_big_int(num_bytes_per_dim, &mut random);
                if query_min[dim] > query_max[dim] {
                    std::mem::swap(&mut query_min[dim], &mut query_max[dim]);
                }
                NumericUtils::big_int_to_sortable_bytes(
                    &query_min[dim],
                    num_bytes_per_dim,
                    &mut query_min_bytes[dim],
                    0,
                )?;
                NumericUtils::big_int_to_sortable_bytes(
                    &query_max[dim],
                    num_bytes_per_dim,
                    &mut query_max_bytes[dim],
                    0,
                )?;
            }

            let mut hits = BitSet::new();
            let mut visitor = IntersectVisitorImpl {
                hits: &mut hits,
                query_min: &query_min_bytes,
                query_max: &query_max_bytes,
                config: config.clone(),
                random: &mut random,
            };

            point_values.intersect(&mut visitor)?;

            for (doc_id, doc_values) in docs.iter().enumerate() {
                let mut expected = true;
                for dim in 0..num_dims {
                    let x = &doc_values[dim];
                    if x < &query_min[dim] || x > &query_max[dim] {
                        expected = false;
                        break;
                    }
                }
                let actual = hits.contains(doc_id);
                assert_eq!(expected, actual, "docID={}", doc_id);
            }
        }
    }
    Ok(())
}
#[test]
fn test_with_exceptions() {
    // TODO: MockDirectoryWrapper not Implemented
}

#[test]
fn test_random_binary_tiny() -> Result<()> {
    let mut random = random();
    do_test_random_binary(&mut random, 10)
}

#[test]
fn test_random_binary_medium() -> Result<()> {
    let mut random = random();
    do_test_random_binary(&mut random, 10_000)
}

#[test]
#[ignore]
fn test_random_binary_big() -> Result<()> {
    let mut random = random();
    do_test_random_binary(&mut random, 200_000)
}
#[test]
fn test_too_little_heap() -> Result<()> {
    let dir = new_directory_shared(&mut random())?;

    let err = BKDWriter::new(
        1,
        dir.as_ref(),
        "bkd",
        BKDConfig::new(1, 1, 16, 1_000_000)?,
        0.001,
        0,
    );
    assert!(err.is_err());
    if let Err(err) = err {
        let err_msg = format!("{:?}", err);
        assert!(
            err_msg.contains("either increase maxMBSortInHeap or decrease maxPointsInLeafNode")
        );
    }
    Ok(())
}
fn do_test_random_binary<R: Rng + ?Sized>(random: &mut R, count: usize) -> Result<()> {
    let num_docs = TestUtil::next_usize(random, count, count * 2);
    let num_bytes_per_dim = TestUtil::next_usize(random, 2, 30);

    let num_data_dims = TestUtil::next_usize(random, 1, MAX_DIMENSIONS);
    let num_index_dims = std::cmp::min(
        TestUtil::next_usize(random, 1, num_data_dims),
        MAX_INDEX_DIMENSIONS,
    ) as usize;

    let mut doc_values = vec![vec![vec![0u8; num_bytes_per_dim]; num_data_dims]; num_docs];

    for doc_value in doc_values.iter_mut().take(num_docs) {
        for val in doc_value.iter_mut().take(num_data_dims) {
            random.fill_bytes(val);
        }
    }

    verify(
        random,
        &doc_values,
        None,
        num_data_dims,
        num_index_dims,
        num_bytes_per_dim,
    )
}

#[test]
fn test_all_equal() -> Result<()> {
    let mut random = random();

    let num_bytes_per_dim = TestUtil::next_usize(&mut random, 2, 30);
    let num_data_dims = TestUtil::next_usize(&mut random, 1, MAX_DIMENSIONS);
    let num_index_dims = std::cmp::min(
        TestUtil::next_usize(&mut random, 1, num_data_dims),
        MAX_INDEX_DIMENSIONS,
    ) as usize;

    let num_docs = at_least(&mut random, 1000);
    let mut doc_values = vec![
        vec![vec![0u8; num_bytes_per_dim as usize]; num_data_dims as usize];
        num_docs as usize
    ];

    for doc_id in 0..num_docs as usize {
        if doc_id == 0 {
            #[allow(clippy::needless_range_loop)]
            for dim in 0..num_data_dims as usize {
                random.fill_bytes(&mut doc_values[doc_id][dim]);
            }
        } else {
            doc_values[doc_id] = doc_values[0].clone();
        }
    }

    verify(
        &mut random,
        &doc_values,
        None,
        num_data_dims,
        num_index_dims,
        num_bytes_per_dim,
    )
}

#[test]
fn test_index_dim_equal_data_dim_different() -> Result<()> {
    let mut random = random();

    let num_bytes_per_dim = TestUtil::next_usize(&mut random, 2, 30);
    let num_data_dims = TestUtil::next_usize(&mut random, 2, MAX_DIMENSIONS);
    let num_index_dims = std::cmp::min(
        TestUtil::next_usize(&mut random, 1, num_data_dims - 1),
        MAX_INDEX_DIMENSIONS,
    ) as usize;

    let num_docs = at_least(&mut random, 1000);
    let mut doc_values = vec![
        vec![vec![0u8; num_bytes_per_dim as usize]; num_data_dims as usize];
        num_docs as usize
    ];

    let mut index_dimensions = vec![vec![0u8; num_bytes_per_dim as usize]; num_data_dims as usize];
    for dim_value in index_dimensions.iter_mut().take(num_index_dims as usize) {
        random.fill_bytes(dim_value);
    }

    for doc_value in doc_values.iter_mut().take(num_docs as usize) {
        for (dim, val) in doc_value
            .iter_mut()
            .enumerate()
            .take(num_index_dims as usize)
        {
            *val = index_dimensions[dim].clone();
        }
        for val in doc_value
            .iter_mut()
            .skip(num_index_dims as usize)
            .take(num_data_dims as usize - num_index_dims as usize)
        {
            random.fill_bytes(val);
        }
    }

    verify(
        &mut random,
        &doc_values,
        None,
        num_data_dims,
        num_index_dims,
        num_bytes_per_dim,
    )
}

#[test]
fn test_one_dim_equal() -> Result<()> {
    let mut random = random();

    let num_bytes_per_dim = TestUtil::next_usize(&mut random, 2, 30);
    let num_data_dims = TestUtil::next_usize(&mut random, 1, MAX_DIMENSIONS);
    let num_index_dims = std::cmp::min(
        TestUtil::next_usize(&mut random, 1, num_data_dims),
        MAX_INDEX_DIMENSIONS,
    ) as usize;

    let num_docs = at_least(&mut random, 1000);
    let the_equal_dim = random.random_range(0..num_data_dims);
    let mut doc_values = vec![
        vec![vec![0u8; num_bytes_per_dim as usize]; num_data_dims as usize];
        num_docs as usize
    ];

    for doc_id in 0..num_docs as usize {
        #[allow(clippy::needless_range_loop)]
        for dim in 0..num_data_dims as usize {
            random.fill_bytes(&mut doc_values[doc_id][dim]);
        }
        if doc_id > 0 {
            doc_values[doc_id][the_equal_dim as usize] =
                doc_values[0][the_equal_dim as usize].clone();
        }
    }

    let max_points_in_leaf_node = TestUtil::next_usize(&mut random, 20, 50);

    verify_full(
        &mut random,
        &doc_values,
        None,
        num_data_dims,
        num_index_dims,
        num_bytes_per_dim,
        max_points_in_leaf_node,
    )
}

#[test]
fn test_one_dim_low_card() -> Result<()> {
    let mut random = random();

    let num_bytes_per_dim = TestUtil::next_usize(&mut random, 2, 30);
    let num_data_dims = TestUtil::next_usize(&mut random, 2, MAX_DIMENSIONS);
    let num_index_dims = std::cmp::min(
        TestUtil::next_usize(&mut random, 2, num_data_dims),
        MAX_INDEX_DIMENSIONS,
    ) as usize;

    let num_docs = at_least(&mut random, 10_000);
    let the_low_card_dim = random.random_range(0..num_data_dims);

    let mut value1 = vec![0u8; num_bytes_per_dim as usize];
    random.fill_bytes(&mut value1);
    let mut value2 = value1.clone();

    let last = &mut value2[num_bytes_per_dim as usize - 1];
    if *last == 0 || random.random_bool(0.5) {
        *last = last.wrapping_add(1);
    } else {
        *last = last.wrapping_sub(1);
    }

    let mut doc_values = vec![
        vec![vec![0u8; num_bytes_per_dim as usize]; num_data_dims as usize];
        num_docs as usize
    ];

    for doc_value in doc_values.iter_mut().take(num_docs as usize) {
        for (dim, val) in doc_value
            .iter_mut()
            .take(num_data_dims as usize)
            .enumerate()
        {
            if dim == the_low_card_dim as usize {
                *val = if random.random_bool(0.5) {
                    value1.clone()
                } else {
                    value2.clone()
                };
            } else {
                random.fill_bytes(val);
            }
        }
    }

    let max_points_in_leaf_node = TestUtil::next_usize(&mut random, 20, 50);
    verify_full(
        &mut random,
        &doc_values,
        None,
        num_data_dims,
        num_index_dims,
        num_bytes_per_dim,
        max_points_in_leaf_node,
    )
}

#[test]
fn test_one_dim_two_values() -> Result<()> {
    let mut random = random();

    let num_bytes_per_dim = TestUtil::next_usize(&mut random, 2, 30);
    let num_data_dims = TestUtil::next_usize(&mut random, 1, MAX_DIMENSIONS);
    let num_index_dims = std::cmp::min(
        TestUtil::next_usize(&mut random, 1, num_data_dims),
        MAX_INDEX_DIMENSIONS,
    ) as usize;

    let num_docs = at_least(&mut random, 1000);
    let the_dim = random.random_range(0..num_data_dims);

    let mut value1 = vec![0u8; num_bytes_per_dim as usize];
    random.fill_bytes(&mut value1);
    let mut value2 = vec![0u8; num_bytes_per_dim as usize];
    random.fill_bytes(&mut value2);

    let mut doc_values = vec![
        vec![vec![0u8; num_bytes_per_dim as usize]; num_data_dims as usize];
        num_docs as usize
    ];

    for doc_value in doc_values.iter_mut().take(num_docs as usize) {
        for (dim, val) in doc_value
            .iter_mut()
            .take(num_data_dims as usize)
            .enumerate()
        {
            if dim == the_dim as usize {
                *val = if random.random_bool(0.5) {
                    value1.clone()
                } else {
                    value2.clone()
                };
            } else {
                random.fill_bytes(val);
            }
        }
    }

    verify(
        &mut random,
        &doc_values,
        None,
        num_data_dims,
        num_index_dims,
        num_bytes_per_dim,
    )
}

#[test]
fn test_random_few_different_values() -> Result<()> {
    let mut random = random();

    let num_bytes_per_dim = TestUtil::next_usize(&mut random, 2, 30);
    let num_data_dims = TestUtil::next_usize(&mut random, 1, MAX_DIMENSIONS);
    let num_index_dims = std::cmp::min(
        TestUtil::next_usize(&mut random, 1, num_data_dims),
        MAX_INDEX_DIMENSIONS,
    ) as usize;

    let num_docs = at_least(&mut random, 10000);
    let cardinality = TestUtil::next_usize(&mut random, 2, 100);

    let mut values = vec![
        vec![vec![0u8; num_bytes_per_dim as usize]; num_data_dims as usize];
        cardinality as usize
    ];
    for value_set in values.iter_mut().take(cardinality as usize) {
        for value in value_set.iter_mut().take(num_data_dims as usize) {
            random.fill_bytes(value);
        }
    }

    let mut doc_values = vec![
        vec![vec![0u8; num_bytes_per_dim as usize]; num_data_dims as usize];
        num_docs as usize
    ];
    for (doc_value, _) in doc_values.iter_mut().zip(0..num_docs as usize) {
        let v = random.random_range(0..cardinality);
        *doc_value = values[v as usize].clone();
    }

    verify(
        &mut random,
        &doc_values,
        None,
        num_data_dims,
        num_index_dims,
        num_bytes_per_dim,
    )
}

pub struct DocMapMock {
    cur_doc_id_base: i32,
}
impl DocMap for DocMapMock {
    fn get(&self, doc_id: i32) -> Result<i32> {
        Ok(self.cur_doc_id_base + doc_id)
    }
}

#[test]
fn test_multi_valued() -> Result<()> {
    let mut random = random();

    let num_bytes_per_dim = TestUtil::next_usize(&mut random, 2, 30);
    let num_data_dims = TestUtil::next_usize(&mut random, 1, MAX_DIMENSIONS);
    let num_index_dims = std::cmp::min(
        TestUtil::next_usize(&mut random, 1, num_data_dims),
        MAX_INDEX_DIMENSIONS,
    ) as usize;

    let num_docs = at_least(&mut random, 1000);
    let mut doc_values: Vec<Vec<Vec<u8>>> = Vec::new();
    let mut doc_ids: Vec<i32> = Vec::new();

    for doc_id in 0..num_docs {
        let num_values_in_doc = TestUtil::next_usize(&mut random, 1, 5);
        for _ in 0..num_values_in_doc {
            doc_ids.push(doc_id);
            let mut values = vec![vec![0u8; num_bytes_per_dim as usize]; num_data_dims as usize];
            for value in values.iter_mut().take(num_data_dims as usize) {
                random.fill_bytes(value);
            }

            doc_values.push(values);
        }
    }

    let doc_values_array: Vec<Vec<Vec<u8>>> = doc_values.clone();
    let mut doc_ids_array = vec![0i32; doc_ids.len()];
    doc_ids_array.copy_from_slice(&doc_ids[..doc_ids.len()]);

    verify(
        &mut random,
        &doc_values_array,
        Some(doc_ids_array),
        num_data_dims,
        num_index_dims,
        num_bytes_per_dim,
    )
}

/// `doc_ids` can be `None` for the single-valued case; otherwise, it maps value
/// to `doc_id`.
fn verify<R: Rng + ?Sized>(
    random: &mut R,
    doc_values: &[Vec<Vec<u8>>],
    doc_ids: Option<Vec<i32>>,
    num_data_dims: usize,
    num_index_dims: usize,
    num_bytes_per_dim: usize,
) -> Result<()> {
    let max_points_in_leaf_node = TestUtil::next_usize(random, 50, 1000);
    verify_full(
        random,
        doc_values,
        doc_ids,
        num_data_dims,
        num_index_dims,
        num_bytes_per_dim,
        max_points_in_leaf_node,
    )
}
#[allow(clippy::too_many_arguments)]
fn verify_full<R: Rng + ?Sized>(
    random: &mut R,
    doc_values: &[Vec<Vec<u8>>],
    doc_ids: Option<Vec<i32>>,
    num_data_dims: usize,
    num_index_dims: usize,
    num_bytes_per_dim: usize,
    max_points_in_leaf_node: usize,
) -> Result<()> {
    let dir = new_directory(random)?;
    let max_mb: f64 = 3.0 + (3.0 * random.random::<f64>());
    verify_with_max_mb(
        random,
        &dir,
        doc_values,
        doc_ids,
        num_data_dims,
        num_index_dims,
        num_bytes_per_dim,
        max_points_in_leaf_node,
        max_mb,
    )
}
#[allow(clippy::too_many_arguments)]
fn verify_with_max_mb<D: Directory, R: Rng + ?Sized>(
    random: &mut R,
    dir: &D,
    doc_values: &[Vec<Vec<u8>>],
    doc_ids: Option<Vec<i32>>,
    num_data_dims: usize,
    num_index_dims: usize,
    num_bytes_per_dim: usize,
    mut max_points_in_leaf_node: usize,
    mut max_mb: f64,
) -> Result<()> {
    let num_values = doc_values.len();

    if cfg!(feature = "test_log_verbose") {
        println!(
            "TEST: numValues={} numDataDims={} numIndexDims={} numBytesPerDim={} maxPointsInLeafNode={} maxMB={}",
            num_values,
            num_data_dims,
            num_index_dims,
            num_bytes_per_dim,
            max_points_in_leaf_node,
            max_mb
        );
    }

    let mut to_merge: Option<Vec<i64>> = None;
    let mut doc_maps = None;
    let mut seg = 0;

    let max_docs = if random.random_bool(0.5) {
        num_values as i64
    } else {
        let mut v = i64::MIN;
        while v < num_values as i64 {
            v = random.random::<i64>();
        }
        v
    };

    let mut writer = BKDWriter::new(
        num_values as i32,
        dir,
        &format!("_{}", seg),
        BKDConfig::new(
            num_data_dims,
            num_index_dims,
            num_bytes_per_dim,
            max_points_in_leaf_node,
        )?,
        max_mb,
        max_docs,
    )?;

    let mut out = dir.create_output("bkd", &IOContext::default_io_context()?)?;

    let mut scratch = vec![0u8; num_bytes_per_dim * num_data_dims];
    let mut last_doc_id_base = 0;
    let use_merge = num_data_dims == 1 && num_values >= 10 && random.random_bool(0.5);
    let mut values_in_this_seg = if use_merge {
        TestUtil::next_usize(random, num_values / 10, num_values)
    } else {
        0
    };

    let mut seg_count = 0;

    for ord in 0..num_values {
        let doc_id = doc_ids.as_ref().map_or(ord as i32, |ids| ids[ord]);

        if cfg!(feature = "test_log_verbose") {
            println!(
                "  ord={} docID={} lastDocIDBase={}",
                ord, doc_id, last_doc_id_base
            );
        }
        #[allow(clippy::needless_range_loop)]
        for dim in 0..num_data_dims {
            if cfg!(feature = "test_log_verbose") {
                println!(
                    "  {} -> {}",
                    dim,
                    BytesRef::from_bytes(doc_values[ord][dim].to_vec())
                );
            }
            scratch.copy_from(
                &doc_values[ord][dim][0..num_bytes_per_dim],
                dim * num_bytes_per_dim,
            );
        }

        writer.add(&scratch, doc_id - last_doc_id_base)?;

        seg_count += 1;

        if use_merge && seg_count == values_in_this_seg {
            if to_merge.is_none() {
                to_merge = Some(Vec::new());
                doc_maps = Some(Vec::new());
            }

            let cur_doc_id_base = last_doc_id_base;
            doc_maps
                .as_mut()
                .unwrap()
                .push(Rc::new(DocMapEnum::<DummyCodecReader>::Mock(DocMapMock {
                    cur_doc_id_base,
                })));

            let finalizer = writer.finish(&mut out)?.unwrap();
            to_merge
                .as_mut()
                .unwrap()
                .push(out.get_file_pointer() as i64);
            writer.write_index(&mut out, None, &finalizer)?;
            values_in_this_seg = TestUtil::next_usize(random, num_values / 10, num_values / 2);
            seg_count = 0;

            seg += 1;
            max_points_in_leaf_node = TestUtil::next_usize(random, 50, 1000);
            max_mb = 3.0 + (3.0 * random.random::<f64>());

            writer = BKDWriter::new(
                num_values as i32,
                dir,
                &format!("_{}", seg),
                BKDConfig::new(
                    num_data_dims,
                    num_index_dims,
                    num_bytes_per_dim,
                    max_points_in_leaf_node,
                )?,
                max_mb,
                doc_values.len() as i64,
            )?;
            last_doc_id_base = doc_id;
        }
    }

    let index_fp;

    let mut input;
    if let Some(to_merge) = &mut to_merge {
        if seg_count > 0 {
            let finalizer = writer.finish(&mut out)?.unwrap();
            to_merge.push(out.get_file_pointer() as i64);
            writer.write_index(&mut out, None, &finalizer)?;
            let cur_doc_id_base = last_doc_id_base;
            doc_maps
                .as_mut()
                .unwrap()
                .push(Rc::new(DocMapEnum::Mock(DocMapMock { cur_doc_id_base })));
        }
        drop(out);
        input = Arc::new(Mutex::new(
            dir.open_input("bkd", &IOContext::default_io_context()?)?,
        ));
        seg += 1;
        writer = BKDWriter::new(
            num_values as i32,
            dir,
            &format!("_{}", seg),
            BKDConfig::new(
                num_data_dims,
                num_index_dims,
                num_bytes_per_dim,
                max_points_in_leaf_node,
            )?,
            max_mb,
            doc_values.len() as i64,
        )?;

        let mut readers = Vec::new();
        for fp in to_merge {
            input.lock().seek(*fp as usize)?;
            readers.push(get_point_values(input.clone())?);
        }

        {
            let mut out = dir.create_output("bkd2", &IOContext::default_io_context()?)?;
            let finalizer = writer.merge(&mut out, doc_maps, readers)?.unwrap();
            index_fp = out.get_file_pointer();
            writer.write_index(&mut out, None, &finalizer)?;
        }
        input = Arc::new(Mutex::new(
            dir.open_input("bkd2", &IOContext::default_io_context()?)?,
        ));
    } else {
        let finalizer = writer.finish(&mut out)?.unwrap();
        index_fp = out.get_file_pointer();
        writer.write_index(&mut out, None, &finalizer)?;
        drop(out);
        input = Arc::new(Mutex::new(
            dir.open_input("bkd", &IOContext::default_io_context()?)?,
        ));
    }

    input.lock().seek(index_fp)?;
    let sub_point_values = get_point_values(input.clone())?;
    assert_size(&mut sub_point_values.get_point_tree()?, random)?;
    let point_values = sub_point_values;

    let iters = at_least(random, 100);
    for _ in 0..iters {
        let mut query_min = vec![vec![0u8; num_bytes_per_dim]; num_data_dims];
        let mut query_max = vec![vec![0u8; num_bytes_per_dim]; num_data_dims];

        for dim in 0..num_data_dims {
            random.fill_bytes(&mut query_min[dim]);
            random.fill_bytes(&mut query_max[dim]);

            if query_min[dim] > query_max[dim] {
                std::mem::swap(&mut query_min[dim], &mut query_max[dim]);
            }
        }

        let mut expected = BitSet::new();
        for (ord, value) in doc_values.iter().enumerate().take(num_values) {
            let mut matches = true;
            for (dim, (min, max)) in query_min
                .iter()
                .zip(query_max.iter())
                .enumerate()
                .take(num_index_dims)
            {
                let val = &value[dim][0..num_bytes_per_dim];
                if val.cmp(&min[0..num_bytes_per_dim]).to_int() < 0
                    || val.cmp(&max[0..num_bytes_per_dim]).to_int() > 0
                {
                    matches = false;
                    break;
                }
            }
            if matches {
                let doc_id = doc_ids.as_ref().map_or(ord as i32, |ids| ids[ord]);
                expected.insert(doc_id as usize);
            }
        }

        let config = BKDConfig::new(
            num_data_dims,
            num_index_dims,
            num_bytes_per_dim,
            max_points_in_leaf_node,
        )?;
        let mut hits = BitSet::new();
        point_values.intersect(&mut IntersectVisitorImpl {
            hits: &mut hits,
            query_min: &query_min,
            query_max: &query_max,
            config: config.clone(),
            random,
        })?;
        assert_hits(&hits, &expected);
        hits.clear();
        PointTree::visit_doc_values(
            &mut point_values.get_point_tree()?,
            &mut IntersectVisitorImpl {
                hits: &mut hits,
                query_min: &query_min,
                query_max: &query_max,
                config: config.clone(),
                random,
            },
        )?;
        assert_hits(&hits, &expected);
    }
    dir.delete_file("bkd")?;
    if to_merge.is_some() {
        dir.delete_file("bkd2")?;
    }

    Ok(())
}
fn assert_size<R: Rng + ?Sized>(tree: &mut impl PointTree, random: &mut R) -> Result<()> {
    // TODO:do we need clone?
    // let mut clone = tree.clone();
    // assert_eq!(clone.size()?, tree.size()?);

    // Rarely continue with the clone tree
    // let tree = if rarely(random) { &mut clone } else { tree };

    let mut visit_doc_id_size = vec![0; 1];
    let mut visit_doc_values_size = vec![0; 1];

    let mut visitor = IntersectVisitorMock1 {
        visit_doc_id_size: &mut visit_doc_id_size,
        visit_doc_values_size: &mut visit_doc_values_size,
    };

    if random.random_bool(0.5) {
        tree.visit_doc_ids(&mut visitor)?;
        tree.visit_doc_values(&mut visitor)?;
    } else {
        tree.visit_doc_values(&mut visitor)?;
        tree.visit_doc_ids(&mut visitor)?;
    }

    assert_eq!(visit_doc_id_size[0], visit_doc_values_size[0]);
    assert_eq!(visit_doc_id_size[0], tree.size()? as i64);

    if tree.move_to_child()? {
        loop {
            random_point_tree_navigation(tree, random)?;
            assert_size(tree, random)?;
            if !tree.move_to_sibling()? {
                break;
            }
        }
        tree.move_to_parent()?;
    }
    Ok(())
}

struct IntersectVisitorMock1<'a> {
    visit_doc_id_size: &'a mut [i64],
    visit_doc_values_size: &'a mut [i64],
}
impl IntersectVisitor for IntersectVisitorMock1<'_> {
    fn visit(&mut self, _doc_id: i32) -> Result<()> {
        self.visit_doc_id_size[0] += 1;
        Ok(())
    }

    fn visit_with_packed_value(&mut self, _doc_id: i32, _packed_value: &[u8]) -> Result<()> {
        self.visit_doc_values_size[0] += 1;
        Ok(())
    }

    fn compare(&self, _min_packed_value: &[u8], _max_packed_value: &[u8]) -> Result<Relation> {
        Ok(Relation::CellCrossesQuery)
    }
}
fn random_point_tree_navigation<R: Rng + ?Sized>(
    tree: &mut impl PointTree,
    random: &mut R,
) -> Result<()> {
    let min_packed_value = tree.get_min_packed_value()?.as_ref().to_vec();
    let max_packed_value = tree.get_max_packed_value()?.as_ref().to_vec();
    let size = tree.size()?;

    if random.random_bool(0.5) && tree.move_to_child()? {
        random_point_tree_navigation(tree, random)?;
        if random.random_bool(0.5) && tree.move_to_sibling()? {
            random_point_tree_navigation(tree, random)?;
        }
        tree.move_to_parent()?;
    }

    // Ensure we always finish on the same node we started
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

fn assert_hits(hits: &BitSet, expected: &BitSet) {
    let limit = expected.len().max(hits.len());
    for doc_id in 0..limit {
        assert_eq!(
            expected.contains(doc_id),
            hits.contains(doc_id),
            "docID={}",
            doc_id
        );
    }
}

fn random_big_int<R: Rng + ?Sized>(num_bytes: usize, random: &mut R) -> BigInt {
    let num_bits = num_bytes * 8 - 1;
    let mut bytes = vec![0u8; num_bits.div_ceil(8)];

    random.fill_bytes(&mut bytes);

    if let Some(first_byte) = bytes.first_mut() {
        *first_byte &= !(1 << (num_bits % 8));
    }

    let x = BigInt::from_bytes_be(Sign::Plus, &bytes);

    if random.random_bool(0.5) { -x } else { x }
}

// TODO:
// fn get_directory(num_points: i32) {
// }
struct IntersectVisitorImpl<'a, R>
where
    R: Rng + ?Sized,
{
    hits: &'a mut BitSet,
    query_min: &'a [Vec<u8>],
    query_max: &'a [Vec<u8>],
    config: BKDConfig,
    random: &'a mut R,
}

impl<R> IntersectVisitor for IntersectVisitorImpl<'_, R>
where
    R: Rng + ?Sized,
{
    fn visit(&mut self, doc_id: i32) -> Result<()> {
        self.hits.insert(doc_id as usize);
        Ok(())
    }
    fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()> {
        let num_index_dims = self.config.num_index_dims;
        let bytes_per_dim = self.config.bytes_per_dim;

        for dim in 0..num_index_dims {
            let offset = dim * bytes_per_dim;
            if packed_value[offset..offset + bytes_per_dim]
                .cmp(&self.query_min[dim][0..bytes_per_dim])
                .to_int()
                < 0
                || packed_value[offset..offset + bytes_per_dim]
                    .cmp(&self.query_max[dim][0..bytes_per_dim])
                    .to_int()
                    > 0
            {
                return Ok(());
            }
        }
        // If all dimensions pass the range check, mark the document as a hit
        self.hits.insert(doc_id as usize);
        Ok(())
    }

    fn visit_iterator_with_packed_value(
        &mut self,
        iterator: &mut impl DocIdSetIterator,
        packed_value: &[u8],
    ) -> Result<()> {
        if self.random.random_bool(0.5) {
            // Check the default method is correct
            IntersectVisitor::default_visit_iterator_with_packed_value_(
                self,
                iterator,
                packed_value,
            )?;
        } else {
            assert_eq!(iterator.doc_id(), -1);

            let cost = iterator.cost()? as i32;
            let mut number_of_points = 0;

            while let Ok(doc_id) = iterator.next_doc() {
                if doc_id == NO_MORE_DOCS {
                    break;
                }

                assert_eq!(iterator.doc_id(), doc_id);
                self.visit_with_packed_value(doc_id, packed_value)?;
                number_of_points += 1;
            }

            assert_eq!(cost, number_of_points);
            assert_eq!(iterator.doc_id(), NO_MORE_DOCS);
            assert_eq!(iterator.next_doc()?, NO_MORE_DOCS);
            assert_eq!(iterator.doc_id(), NO_MORE_DOCS);
        }
        Ok(())
    }

    fn compare(&self, min_packed: &[u8], max_packed: &[u8]) -> Result<Relation> {
        let num_index_dims = self.config.num_index_dims;
        let bytes_per_dim = self.config.bytes_per_dim;
        let mut crosses = false;

        for dim in 0..num_index_dims {
            let offset = dim * bytes_per_dim;

            if max_packed[offset..offset + bytes_per_dim]
                .cmp(&self.query_min[dim][..bytes_per_dim])
                .to_int()
                < 0
                || min_packed[offset..offset + bytes_per_dim]
                    .cmp(&self.query_max[dim][..bytes_per_dim])
                    .to_int()
                    > 0
            {
                return Ok(Relation::CellOutsideQuery);
            } else if min_packed[offset..offset + bytes_per_dim]
                .cmp(&self.query_min[dim][..bytes_per_dim])
                .to_int()
                < 0
                || max_packed[offset..offset + bytes_per_dim]
                    .cmp(&self.query_max[dim][..bytes_per_dim])
                    .to_int()
                    > 0
            {
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
#[test]
fn test_bit_flipped_on_partition1() -> Result<()> {
    // TODO: MockDirectoryWrapper not Implemented
    Ok(())
}
#[test]
fn test_bit_flippedon_partition2() -> Result<()> {
    // TODO: MockDirectoryWrapper not Implemented
    Ok(())
}
struct IntersectVisitorMock2 {
    last_doc_id: i32,
}
impl IntersectVisitorMock2 {
    fn new() -> Self {
        Self { last_doc_id: -1 }
    }
}
impl IntersectVisitor for IntersectVisitorMock2 {
    fn visit(&mut self, doc_id: i32) -> Result<()> {
        assert!(
            doc_id > self.last_doc_id,
            "lastDocID={} docID={}",
            self.last_doc_id,
            doc_id
        );
        self.last_doc_id = doc_id;
        Ok(())
    }

    fn visit_with_packed_value(&mut self, doc_id: i32, _packed_value: &[u8]) -> Result<()> {
        self.visit(doc_id)
    }

    fn compare(&self, _min_packed_value: &[u8], _max_packed_value: &[u8]) -> Result<Relation> {
        Ok(Relation::CellCrossesQuery)
    }
}
#[test]
fn test_tie_break_order() -> Result<()> {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;
    let num_docs = 10_000;

    let config = BKDConfig::new(1, 1, 4, 2)?;
    let mut writer = BKDWriter::new(
        num_docs + 1,
        dir.as_ref(),
        "tmp",
        config,
        0.01,
        num_docs as i64,
    )?;

    let bytes = [0u8; 4];
    for doc_id in 0..num_docs {
        writer.add(&bytes, doc_id)?;
    }

    let mut out = dir.create_output("bkd", &IOContext::default_io_context()?)?;

    let finalizer = writer.finish(&mut out)?.unwrap();
    let fp = out.get_file_pointer();
    writer.write_index(&mut out, None, &finalizer)?;

    let mut input = dir.open_input("bkd", &IOContext::default_io_context()?)?;
    input.seek(fp)?;
    let sub_point_values = get_point_values(Arc::new(Mutex::new(input)))?;
    let point_values = sub_point_values;
    point_values.intersect(&mut IntersectVisitorMock2::new())?;
    Ok(())
}
struct IntersectVisitorMock3 {
    previous: Option<Vec<u8>>,
    has_changed: bool,
    num_data_dims: usize,
    num_bytes_per_dim: usize,
}
impl IntersectVisitorMock3 {
    fn new(num_data_dims: usize, num_bytes_per_dim: usize) -> Self {
        Self {
            previous: None,
            has_changed: false,
            num_data_dims,
            num_bytes_per_dim,
        }
    }
}
impl IntersectVisitor for IntersectVisitorMock3 {
    fn visit(&mut self, _doc_id: i32) -> Result<()> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn visit_with_packed_value(&mut self, _doc_id: i32, packed_value: &[u8]) -> Result<()> {
        let len = self.num_data_dims * self.num_bytes_per_dim;
        if self.previous.is_none() {
            let mut value = vec![0u8; len];
            value.copy_from(&packed_value[..len], 0);
            self.previous = Some(value);
        } else if let Some(prev) = &mut self.previous {
            let mismatch = packed_value.eq(prev.as_slice());
            if !mismatch {
                if !self.has_changed {
                    self.has_changed = true;
                    prev.copy_from(&packed_value[..len], 0);
                } else {
                    return Err(LuceneError::illegal_state(
                        "Points are not in optimal order",
                    ));
                }
            }
        }
        Ok(())
    }

    fn compare(&self, _min_packed_value: &[u8], _max_packed_value: &[u8]) -> Result<Relation> {
        Ok(Relation::CellCrossesQuery)
    }
}
#[test]
fn test_check_data_dim_optimal_order() -> Result<()> {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;
    let num_values = at_least(&mut random, 5000);
    let max_points_in_leaf_node = TestUtil::next_usize(&mut random, 50, 500);
    let num_bytes_per_dim = TestUtil::next_usize(&mut random, 1, 4);
    let max_mb = 3.0 + 3.0 * random.random::<f64>();
    let num_index_dims = TestUtil::next_usize(&mut random, 1, 8);
    let num_data_dims = TestUtil::next_usize(&mut random, num_index_dims, 8);

    let mut point_value1 = vec![0u8; (num_data_dims * num_bytes_per_dim) as usize];
    let mut point_value2 = vec![0u8; (num_data_dims * num_bytes_per_dim) as usize];
    random.fill_bytes(&mut point_value1);
    random.fill_bytes(&mut point_value2);

    // Equal index dimensions but different data dimensions
    for i in 0..num_index_dims {
        let offset = (i * num_bytes_per_dim) as usize;
        point_value2.copy_from(
            &point_value1[offset..offset + num_bytes_per_dim as usize],
            offset,
        );
    }

    let config = BKDConfig::new(
        num_data_dims,
        num_index_dims,
        num_bytes_per_dim,
        max_points_in_leaf_node,
    )?;

    let index_fp;
    {
        let mut writer = BKDWriter::new(
            2 * num_values,
            dir.as_ref(),
            "_temp",
            config.clone(),
            max_mb,
            (2 * num_values) as i64,
        )?;

        for i in 0..num_values {
            writer.add(&point_value1, i)?;
            writer.add(&point_value2, i)?;
        }

        let mut out = dir.create_output("bkd", &IOContext::default_io_context()?)?;
        let finalizer = writer.finish(&mut out)?.unwrap();
        index_fp = out.get_file_pointer();
        writer.write_index(&mut out, None, &finalizer)?;
        writer.close()?;
    }

    let mut point_in = dir.open_input("bkd", &IOContext::default_io_context()?)?;
    point_in.seek(index_fp)?;

    let sub_point_values = get_point_values(Arc::new(Mutex::new(point_in)))?;
    let point_values = sub_point_values;
    point_values.intersect(&mut IntersectVisitorMock3::new(
        num_data_dims,
        num_bytes_per_dim,
    ))?;
    Ok(())
}
struct IntersectVisitorMock4<'a> {
    count: &'a mut [i32],
    random: RefCell<StdRng>,
}

impl<'a> IntersectVisitorMock4<'a> {
    fn new(count: &'a mut [i32], random: u64) -> Self {
        let random = RefCell::new(random_from_seed(random));
        Self { count, random }
    }
}
impl IntersectVisitor for IntersectVisitorMock4<'_> {
    fn visit(&mut self, _doc_id: i32) -> Result<()> {
        self.count[0] += 1;
        Ok(())
    }

    fn visit_with_packed_value(&mut self, doc_id: i32, _packed_value: &[u8]) -> Result<()> {
        self.visit(doc_id)
    }

    fn compare(&self, _min_packed_value: &[u8], _max_packed_value: &[u8]) -> Result<Relation> {
        if self.random.borrow_mut().random_range(0..7) == 1 {
            Ok(Relation::CellCrossesQuery)
        } else {
            Ok(Relation::CellInsideQuery)
        }
    }
}
#[test]
fn test_2d_long_ords_offline() -> Result<()> {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;
    let num_docs = 100_000;

    let config = BKDConfig::new(2, 2, 4, 2)?;
    let mut writer = BKDWriter::new(
        num_docs + 1,
        dir.as_ref(),
        "tmp",
        config,
        0.01,
        num_docs as i64,
    )?;

    let mut buffer = vec![0u8; 2 * 4];
    for doc_id in 0..num_docs {
        random.fill_bytes(&mut buffer);
        writer.add(&buffer, doc_id)?;
    }

    let fp;
    {
        let mut out = dir.create_output("bkd", &IOContext::default_io_context()?)?;
        let finalizer = writer.finish(&mut out)?.unwrap();
        fp = out.get_file_pointer();
        writer.write_index(&mut out, None, &finalizer)?;
    }

    let mut input = dir.open_input("bkd", &IOContext::default_io_context()?)?;
    input.seek(fp)?;
    let sub_point_values = get_point_values(Arc::new(Mutex::new(input)))?;
    let point_values = sub_point_values;

    let mut count = [0];
    let mut visitor = IntersectVisitorMock4::new(&mut count, random.next_u64());
    point_values.intersect(&mut visitor)?;
    assert_eq!(count[0], num_docs);

    Ok(())
}
struct IntersectVisitorMock5<'a> {
    count: &'a mut [i32],
    random: RefCell<StdRng>,
    num_index_dims: usize,
    bytes_per_dim: usize,
    num_dims: usize,
}
impl<'a> IntersectVisitorMock5<'a> {
    fn new(
        count: &'a mut [i32],
        random: u64,
        num_index_dims: usize,
        bytes_per_dim: usize,
        num_dims: usize,
    ) -> Self {
        let random = RefCell::new(random_from_seed(random));
        Self {
            count,
            random,
            num_index_dims,
            bytes_per_dim,
            num_dims,
        }
    }
}
impl IntersectVisitor for IntersectVisitorMock5<'_> {
    fn visit(&mut self, _doc_id: i32) -> Result<()> {
        self.count[0] += 1;
        Ok(())
    }

    fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()> {
        assert_eq!(packed_value.len(), (self.num_dims * self.bytes_per_dim));
        self.visit(doc_id)
    }

    fn compare(&self, min_packed: &[u8], max_packed: &[u8]) -> Result<Relation> {
        assert_eq!(min_packed.len(), (self.num_index_dims * self.bytes_per_dim));
        assert_eq!(max_packed.len(), (self.num_index_dims * self.bytes_per_dim));
        if self.random.borrow_mut().random_range(0..7) == 1 {
            Ok(Relation::CellCrossesQuery)
        } else {
            Ok(Relation::CellInsideQuery)
        }
    }
}
#[test]
fn test_wasted_leading_bytes() -> Result<()> {
    let mut random = random();
    let num_dims = TestUtil::next_usize(&mut random, 1, MAX_INDEX_DIMENSIONS);
    let num_index_dims = TestUtil::next_usize(&mut random, 1, num_dims);
    let bytes_per_dim = MAX_NUM_BYTES;
    let bytes_used = TestUtil::next_usize(&mut random, 1, 3);

    let dir = new_directory_shared(&mut random)?;
    let num_docs = at_least(&mut random, 10000);
    let config = BKDConfig::new(num_dims, num_index_dims, bytes_per_dim, 32)?;

    let mut writer = BKDWriter::new(
        num_docs + 1,
        dir.as_ref(),
        "tmp",
        config.clone(),
        1.0,
        num_docs as i64,
    )?;

    let mut tmp = vec![0u8; bytes_used as usize];
    let mut buffer = vec![0u8; (num_dims * bytes_per_dim) as usize];

    for doc_id in 0..num_docs {
        for dim in 0..num_dims {
            random.fill_bytes(&mut tmp);
            let offset = (dim * bytes_per_dim + (bytes_per_dim - bytes_used)) as usize;
            buffer.copy_from(&tmp, offset);
        }
        writer.add(&buffer, doc_id)?;
    }
    let fp;
    {
        let mut out = dir.create_output("bkd", &IOContext::default_io_context()?)?;

        let finalizer = writer.finish(&mut out)?.unwrap();
        fp = out.get_file_pointer();
        writer.write_index(&mut out, None, &finalizer)?;
    }
    let mut input = dir.open_input("bkd", &IOContext::default_io_context()?)?;
    input.seek(fp)?;
    let sub_point_values = get_point_values(Arc::new(Mutex::new(input)))?;
    let point_values = sub_point_values;

    let mut count = [0];
    let mut visitor = IntersectVisitorMock5::new(
        &mut count,
        random.next_u64(),
        num_index_dims,
        bytes_per_dim,
        num_dims,
    );
    point_values.intersect(&mut visitor)?;
    assert_eq!(count[0], num_docs);

    Ok(())
}
struct IntersectVisitorMock6;
impl IntersectVisitor for IntersectVisitorMock6 {
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
struct IntersectVisitorMock7;
impl IntersectVisitor for IntersectVisitorMock7 {
    fn visit(&mut self, _doc_id: i32) -> Result<()> {
        Ok(())
    }

    fn visit_with_packed_value(&mut self, _doc_id: i32, _packed_value: &[u8]) -> Result<()> {
        Ok(())
    }

    fn compare(&self, _min_packed_value: &[u8], _max_packed_value: &[u8]) -> Result<Relation> {
        Ok(Relation::CellOutsideQuery)
    }
}
struct IntersectVisitorMock8<'a> {
    unique_point_value: &'a [u8],
    num_bytes_per_dim: usize,
}
impl IntersectVisitor for IntersectVisitorMock8<'_> {
    fn visit(&mut self, _doc_id: i32) -> Result<()> {
        Ok(())
    }

    fn visit_with_packed_value(&mut self, _doc_id: i32, _packed_value: &[u8]) -> Result<()> {
        Ok(())
    }

    fn compare(&self, min_packed_value: &[u8], max_packed_value: &[u8]) -> Result<Relation> {
        if self.unique_point_value[..self.num_bytes_per_dim]
            .cmp(&max_packed_value[..self.num_bytes_per_dim])
            .to_int()
            > 0
            || self.unique_point_value[..self.num_bytes_per_dim]
                .cmp(&min_packed_value[..self.num_bytes_per_dim])
                .to_int()
                < 0
        {
            Ok(Relation::CellOutsideQuery)
        } else {
            Ok(Relation::CellCrossesQuery)
        }
    }
}
#[test]
fn test_estimate_point_count() -> Result<()> {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;
    let num_values = at_least(&mut random, 10_000);
    let max_points_in_leaf_node = TestUtil::next_usize(&mut random, 50, 500);
    let num_bytes_per_dim = TestUtil::next_usize(&mut random, 1, 4);

    let mut point_value = vec![0u8; num_bytes_per_dim as usize];
    let mut unique_point_value = vec![0u8; num_bytes_per_dim as usize];
    random.fill_bytes(&mut unique_point_value);

    let config = BKDConfig::new(1, 1, num_bytes_per_dim, max_points_in_leaf_node)?;
    let mut writer = BKDWriter::new(
        num_values,
        dir.as_ref(),
        "_temp",
        config.clone(),
        DEFAULT_MAX_MB_SORT_IN_HEAP as f64,
        num_values as i64,
    )?;

    for i in 0..num_values {
        if i == num_values / 2 {
            writer.add(&unique_point_value, i)?;
        } else {
            loop {
                random.fill_bytes(&mut point_value);
                if point_value != unique_point_value {
                    break;
                }
            }
            writer.add(&point_value, i)?;
        }
    }

    let index_fp;
    {
        let mut out = dir.create_output("bkd", &IOContext::default_io_context()?)?;
        let finalizer = writer.finish(&mut out)?.unwrap();
        index_fp = out.get_file_pointer();
        writer.write_index(&mut out, None, &finalizer)?;
    }

    let mut input = dir.open_input("bkd", &IOContext::default_io_context()?)?;
    input.seek(index_fp)?;
    let point_values = get_point_values(Arc::new(Mutex::new(input)))?;

    // If all points match, then the point count is numValues
    assert_eq!(
        point_values.estimate_point_count(&IntersectVisitorMock6)?,
        num_values as i64
    );
    // Return 0 if no points match
    assert_eq!(
        point_values.estimate_point_count(&IntersectVisitorMock7)?,
        0
    );
    // If only one point matches, then the point count is
    // (actualMaxPointsInLeafNode + 1) / 2 in general, or maybe 2x that if
    // the point is a split value
    let point_count = point_values.estimate_point_count(&IntersectVisitorMock8 {
        unique_point_value: &unique_point_value,
        num_bytes_per_dim,
    })?;
    let last_node_point_count = num_values as usize % max_points_in_leaf_node;
    let mid = max_points_in_leaf_node.div_ceil(2) as i64;
    let mid_last = last_node_point_count.div_ceil(2) as i64;

    assert!(
        point_count == mid// common case
                // not fully populated leaf
                || point_count == mid_last
                // if the point is a split value
                || point_count == 2 * mid
                // if the point is a split value and one leaf is not fully populated
                || point_count == mid + mid_last,
        "Unexpected point count: {}",
        point_count
    );

    Ok(())
}
pub struct MutablePointTreeMock1 {
    point_value: Vec<u8>,
    num_points_added: usize,
}

impl PointTree for MutablePointTreeMock1 {
    fn size(&self) -> Result<usize> {
        Ok(self.num_points_added)
    }

    fn visit_doc_values<IV>(&mut self, visitor: &mut IV) -> Result<()>
    where
        IV: IntersectVisitor,
    {
        for _ in 0..self.num_points_added {
            visitor.visit_with_packed_value(0, self.point_value.as_slice())?;
        }
        Ok(())
    }
}

impl Clone for MutablePointTreeMock1 {
    fn clone(&self) -> Self {
        unreachable!()
    }
}
impl TryClone for MutablePointTreeMock1 {
    fn try_clone(&self) -> Result<Self>
    where
        Self: Sized,
    {
        Ok(self.clone())
    }
}

impl MutablePointTree for MutablePointTreeMock1 {
    fn get_value(&self, _i: usize, packed_value: &mut BytesRef<Vec<u8>>) {
        packed_value.bytes = self.point_value.clone();
    }

    fn get_byte_at(&self, i: usize, k: usize) -> u8 {
        let mut b = BytesRef::new();
        self.get_value(i, &mut b);
        b.bytes[b.offset + k]
    }

    fn get_doc_id(&self, _i: usize) -> i32 {
        0
    }

    fn swap(&mut self, _i: usize, _j: usize) {}

    fn save(&mut self, _i: usize, _j: usize) {}

    fn restore(&mut self, _i: usize, _j: usize) {}
}
#[test]
fn test_total_point_count_validation() -> Result<()> {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;
    let num_values = 10;
    let num_points_added = 50;
    let num_bytes_per_dim = TestUtil::next_usize(&mut random, 1, 4);
    let mut point_value = vec![0u8; num_bytes_per_dim as usize];
    random.fill_bytes(&mut point_value);

    let mut reader = MutablePointTreeMock1 {
        point_value,
        num_points_added,
    };

    let config = BKDConfig::new(
        1,
        1,
        num_bytes_per_dim,
        BKDConfig::DEFAULT_MAX_POINTS_IN_LEAF_NODE,
    )?;

    let mut writer = BKDWriter::new(
        num_values,
        dir.as_ref(),
        "_temp",
        config,
        DEFAULT_MAX_MB_SORT_IN_HEAP as f64,
        num_values as i64,
    )?;
    let mut out = dir.create_output("bkd", &IOContext::default_io_context()?)?;

    let result = writer.write_field(&mut out, &mut reader, "test_field_name");

    assert!(matches!(result, Err(LuceneError::IllegalState(_))));
    Ok(())
}
pub struct MutablePointTreeMock2 {
    tmp_values: Vec<Vec<u8>>,
    tmp_docs: Vec<i32>,
    num_bytes_per_dim: usize,
    point_values: Vec<Vec<u8>>,
    doc_id: Vec<i32>,
}

impl PointTree for MutablePointTreeMock2 {
    fn size(&self) -> Result<usize> {
        Ok(11)
    }

    fn visit_doc_values<IV>(&mut self, visitor: &mut IV) -> Result<()>
    where
        IV: IntersectVisitor,
    {
        for i in 0..self.size()? {
            visitor.visit_with_packed_value(self.doc_id[i], &self.point_values[i])?
        }
        Ok(())
    }
}

impl Clone for MutablePointTreeMock2 {
    fn clone(&self) -> Self {
        unreachable!()
    }
}
impl TryClone for MutablePointTreeMock2 {
    fn try_clone(&self) -> Result<Self>
    where
        Self: Sized,
    {
        Ok(self.clone())
    }
}

impl MutablePointTree for MutablePointTreeMock2 {
    fn get_value(&self, i: usize, packed_value: &mut BytesRef<Vec<u8>>) {
        packed_value.bytes = self.point_values[i].clone();
        packed_value.offset = 0;
        packed_value.length = self.num_bytes_per_dim;
    }

    fn get_byte_at(&self, i: usize, k: usize) -> u8 {
        self.point_values[i][k]
    }

    fn get_doc_id(&self, i: usize) -> i32 {
        self.doc_id[i]
    }

    fn swap(&mut self, i: usize, j: usize) {
        self.point_values.swap(i, j);
        self.doc_id.swap(i, j)
    }

    fn save(&mut self, i: usize, j: usize) {
        self.tmp_values[j] = self.point_values[i].clone();
        self.tmp_docs[j] = self.doc_id[i];
    }

    fn restore(&mut self, i: usize, j: usize) {
        for k in i..j {
            self.point_values[k] = self.tmp_values[k].clone();
            self.doc_id[k] = self.tmp_docs[k];
        }
    }
}
#[test]
fn test_too_many_points() -> Result<()> {
    let mut random = random();
    let dir = new_directory(&mut random)?;

    let num_values = 10;
    let num_bytes_per_dim = TestUtil::next_usize(&mut random, 1, 4);
    let mut point_value = vec![0u8; num_bytes_per_dim];

    let mut w = BKDWriter::new(
        num_values as i32,
        &dir,
        "_temp",
        BKDConfig::new(1, 1, num_bytes_per_dim, 2)?,
        DEFAULT_MAX_MB_SORT_IN_HEAP as f64,
        num_values,
    )?;

    for i in 0..num_values {
        random.fill_bytes(&mut point_value);
        w.add(&point_value, i as i32)?;
    }

    random.fill_bytes(&mut point_value);
    let err = w
        .add(&point_value, num_values as i32)
        .expect_err("expected IllegalStateException");
    assert_eq!(
        err.to_string(),
        format!(
            "totalPointCount={} was passed when we were created, but we just hit {} values",
            num_values,
            num_values + 1
        )
    );
    Ok(())
}

#[test]
fn test_too_many_points_1d() -> Result<()> {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;

    let num_values = 10;
    let num_bytes_per_dim = TestUtil::next_usize(&mut random, 1, 4);
    let mut point_values = vec![vec![0u8; num_bytes_per_dim as usize]; 11];
    let mut doc_ids = vec![0i32; 11];

    for i in 0..=num_values as usize {
        random.fill_bytes(&mut point_values[i]);
        doc_ids[i] = i as i32;
    }

    let mut reader = MutablePointTreeMock2 {
        tmp_values: vec![vec![]; num_values as usize],
        tmp_docs: vec![],
        num_bytes_per_dim,
        point_values,
        doc_id: doc_ids,
    };

    let config = BKDConfig::new(1, 1, num_bytes_per_dim, 2)?;
    let mut writer = BKDWriter::new(
        num_values + 1,
        dir.as_ref(),
        "_temp",
        config,
        DEFAULT_MAX_MB_SORT_IN_HEAP as f64,
        num_values as i64,
    )?;

    let mut out = dir.create_output("bkd", &IOContext::default_io_context()?)?;

    let result = writer.write_field(&mut out, &mut reader, "");
    assert!(
        matches!(result, Err(LuceneError::IllegalState(msg)) if msg.message.eq("totalPointCount=10 was passed when we were created, but we just hit 11 values"))
    );

    Ok(())
}

use std::fmt::Debug;

// -------------------- Scorer / ScorerSupplier --------------------

trait Scorer: Debug {
    fn score(&self, doc: i32) -> f32;
}

trait ScorerSupplier: Debug {
    type Scorer: Scorer;
    fn get(&self) -> Self::Scorer;
}

// -------------------- Weight --------------------

trait Weight: Debug {
    type A: ScorerSupplier;
    fn scorer_supplier(&self) -> Option<Self::A>;
}

// -------------------- Concrete scorers --------------------

#[derive(Debug, Clone)]
struct TermScorer {
    boost: f32,
}
impl Scorer for TermScorer {
    fn score(&self, _doc: i32) -> f32 {
        1.0 * self.boost
    }
}

#[derive(Debug, Clone)]
struct FieldExistsScorer;
impl Scorer for FieldExistsScorer {
    fn score(&self, _doc: i32) -> f32 {
        1.0
    }
}

// 统一输出的 scorer（对应 QueryScorerSupplier 的输出类型）
#[derive(Debug, Clone)]
enum QueryScorer {
    Term(TermScorer),
    FieldExists(FieldExistsScorer),
    ConstantScore(Box<QueryScorer>),
}

impl Scorer for QueryScorer {
    fn score(&self, doc: i32) -> f32 {
        match self {
            QueryScorer::Term(s) => s.score(doc),
            QueryScorer::FieldExists(s) => s.score(doc),
            QueryScorer::ConstantScore(inner) => {
                // 示例：ConstantScore 把分数压成 1.0
                let _ = inner.score(doc);
                1.0
            },
        }
    }
}

// -------------------- Concrete scorer suppliers --------------------

#[derive(Debug, Clone)]
struct TermSS {
    boost: f32,
}
impl ScorerSupplier for TermSS {
    type Scorer = TermScorer;
    fn get(&self) -> Self::Scorer {
        TermScorer { boost: self.boost }
    }
}

#[derive(Debug, Clone)]
struct FieldExistsSS;
impl ScorerSupplier for FieldExistsSS {
    type Scorer = FieldExistsScorer;
    fn get(&self) -> Self::Scorer {
        FieldExistsScorer
    }
}

// -------------------- QueryScorerSupplier as enum (3 variants) --------------------

#[derive(Debug, Clone)]
enum QueryScorerSupplier {
    Term(TermSS),
    FieldExists(FieldExistsSS),
    ConstantScore(Box<QueryScorerSupplier>),
}

impl QueryScorerSupplier {
    fn term(boost: f32) -> Self {
        Self::Term(TermSS { boost })
    }
    fn field_exists() -> Self {
        Self::FieldExists(FieldExistsSS)
    }
    fn constant_score(inner: QueryScorerSupplier) -> Self {
        Self::ConstantScore(Box::new(inner))
    }
}

impl ScorerSupplier for QueryScorerSupplier {
    type Scorer = QueryScorer;

    fn get(&self) -> Self::Scorer {
        match self {
            QueryScorerSupplier::Term(ss) => QueryScorer::Term(ss.get()),
            QueryScorerSupplier::FieldExists(ss) => QueryScorer::FieldExists(ss.get()),
            QueryScorerSupplier::ConstantScore(inner) => {
                QueryScorer::ConstantScore(Box::new(inner.get()))
            },
        }
    }
}

// -------------------- Weights --------------------

#[derive(Debug, Clone)]
struct TermWeight {
    boost: f32,
}
impl Weight for TermWeight {
    type A = QueryScorerSupplier;

    fn scorer_supplier(&self) -> Option<Self::A> {
        Some(QueryScorerSupplier::term(self.boost))
    }
}

#[derive(Debug, Clone)]
struct FieldExistsWeight;
impl Weight for FieldExistsWeight {
    type A = QueryScorerSupplier;

    fn scorer_supplier(&self) -> Option<Self::A> {
        Some(QueryScorerSupplier::field_exists())
    }
}

#[derive(Debug, Clone)]
struct ConstantScoreWeight<W: Weight<A = QueryScorerSupplier>> {
    inner: W,
}
impl<W> Weight for ConstantScoreWeight<W>
where
    W: Weight<A = QueryScorerSupplier>,
{
    type A = QueryScorerSupplier;

    fn scorer_supplier(&self) -> Option<Self::A> {
        self.inner
            .scorer_supplier()
            .map(QueryScorerSupplier::constant_score)
    }
}

// 递归 QueryWeight：ConstantScore 里面包 Box<QueryWeight>
#[derive(Debug, Clone)]
enum QueryWeight {
    Term(TermWeight),
    FieldExists(FieldExistsWeight),
    ConstantScore(ConstantScoreWeight<Box<QueryWeight>>),
}

impl Weight for QueryWeight {
    type A = QueryScorerSupplier;

    fn scorer_supplier(&self) -> Option<Self::A> {
        match self {
            QueryWeight::Term(w) => w.scorer_supplier(),
            QueryWeight::FieldExists(w) => w.scorer_supplier(),
            QueryWeight::ConstantScore(w) => w.scorer_supplier(),
        }
    }
}

// Box<QueryWeight> 转发 Weight（方便 ConstantScoreWeight<Box<QueryWeight>>）
impl Weight for Box<QueryWeight> {
    type A = QueryScorerSupplier;
    fn scorer_supplier(&self) -> Option<Self::A> {
        (**self).scorer_supplier()
    }
}

// -------------------- demo --------------------
#[test]
fn main() {
    // ConstantScore(ConstantScore(Term))
    let w = QueryWeight::ConstantScore(ConstantScoreWeight {
        inner: Box::new(QueryWeight::ConstantScore(ConstantScoreWeight {
            inner: Box::new(QueryWeight::Term(TermWeight { boost: 2.0 })),
        })),
    });

    let ss = w.scorer_supplier().unwrap();
    let scorer = ss.get();

    println!("score(doc=7) = {}", scorer.score(7));
}
