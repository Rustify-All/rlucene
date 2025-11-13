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
use parking_lot::Mutex;
use rand::Rng;
use std::sync::Arc;

use crate::core::store::directory::Directory;
use crate::core::store::dummy::dummy_index_output::DummyIndexOutput;
use crate::core::store::index_output::IndexOutput;
use crate::core::store::{IOContext, IndexInput};
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::long_values::LongValues;
use crate::core::util::packed::direct_monotonic_reader::{DirectMonotonicReader, load_meta};
use crate::core::util::packed::direct_monotonic_writer::{
    DirectMonotonicWriter, MAX_BLOCK_SHIFT, MIN_BLOCK_SHIFT,
};
use crate::test::util::lucene_test_case::lucene_test_case_util::{at_least, new_directory, random};
use crate::test::util::test_util::TestUtil;

#[allow(dead_code)] // for quick search
pub struct TestDirectMonotonic;

#[test]
fn test_validation() {
    let mut meta_out = DummyIndexOutput;
    let mut data_out = DummyIndexOutput;
    let result = DirectMonotonicWriter::get_instance(&mut meta_out, &mut data_out, -1, 10);
    assert!(
        matches!(result, Err(LuceneError::IllegalArgument(msg)) if "numValues can't be negative, got -1".eq(&msg.message))
    );

    let result = DirectMonotonicWriter::get_instance(&mut meta_out, &mut data_out, 10, 1);
    assert!(
        matches!(result, Err(LuceneError::IllegalArgument(msg)) if "blockShift must be in [2-22], got 1".eq(&msg.message))
    );

    let result = DirectMonotonicWriter::get_instance(&mut meta_out, &mut data_out, 1 << 40, 5);
    assert!(
        matches!(result, Err(LuceneError::IllegalArgument(msg)) if format!("blockShift is too low for the provided number of values: blockShift=5, numValues=1099511627776, MAX_ARRAY_LENGTH={}",ArrayUtil::MAX_ARRAY_LENGTH).eq(&msg.message))
    );
}
#[test]
pub fn test_empty() -> Result<()> {
    let mut random = random();
    let dir = new_directory(&mut random)?;
    let block_shift = TestUtil::next_int(&mut random, MIN_BLOCK_SHIFT, MAX_BLOCK_SHIFT);

    let data_length;
    {
        let mut meta_out = dir.create_output("meta", &IOContext::default_io_context()?)?;
        let mut data_out = dir.create_output("data", &IOContext::default_io_context()?)?;
        let mut writer =
            DirectMonotonicWriter::get_instance(&mut meta_out, &mut data_out, 0, block_shift)?;
        writer.finish()?;
        data_length = data_out.get_file_pointer();
    }

    {
        let mut meta_in = dir.open_input("meta", &IOContext::read_once_io_context()?)?;
        let data_in = dir.open_input("data", &IOContext::default_io_context()?)?;
        let meta = load_meta(&mut meta_in, 0, block_shift)?;
        let slice = Arc::new(Mutex::new(data_in.random_access_slice(0, data_length)?));
        DirectMonotonicReader::get_instance(&meta, slice)?;
    }
    Ok(())
}
#[test]
pub fn test_simple() -> Result<()> {
    let mut random = random();
    let dir = new_directory(&mut random)?;
    let block_shift = 2;

    let actual_values = vec![1, 2, 5, 7, 8, 100];
    let num_values = actual_values.len();

    let data_length;
    {
        let mut meta_out = dir.create_output("meta", &IOContext::default_io_context()?)?;
        let mut data_out = dir.create_output("data", &IOContext::default_io_context()?)?;
        let mut writer = DirectMonotonicWriter::get_instance(
            &mut meta_out,
            &mut data_out,
            num_values as i64,
            block_shift,
        )?;
        for &v in &actual_values {
            writer.add(v)?;
        }
        writer.finish()?;
        data_length = data_out.get_file_pointer();
    }

    {
        let mut meta_in = dir.open_input("meta", &IOContext::read_once_io_context()?)?;
        let data_in = dir.open_input("data", &IOContext::default_io_context()?)?;
        let meta = load_meta(&mut meta_in, num_values as i64, block_shift)?;
        let slice = Arc::new(Mutex::new(data_in.random_access_slice(0, data_length)?));
        let values = DirectMonotonicReader::get_instance(&meta, slice)?;
        for (i, &v) in actual_values.iter().enumerate() {
            assert_eq!(v, values.get(i as i64)?);
        }
    }
    Ok(())
}

#[test]
pub fn test_constant_slope() -> Result<()> {
    let mut random = random();
    let dir = new_directory(&mut random)?;
    let block_shift = TestUtil::next_int(&mut random, MIN_BLOCK_SHIFT, MAX_BLOCK_SHIFT);
    let num_values = TestUtil::next_int(&mut random, 1, 1 << 20);
    let min: i64 = random.random();
    let upper = random.random_range(0..20);
    let inc = random.random_range(0..1 << upper);

    let actual_values: Vec<i64> = (0..num_values).map(|i| min + inc * i as i64).collect();

    let data_length;
    {
        let mut meta_out = dir.create_output("meta", &IOContext::default_io_context()?)?;
        let mut data_out = dir.create_output("data", &IOContext::default_io_context()?)?;
        let mut writer = DirectMonotonicWriter::get_instance(
            &mut meta_out,
            &mut data_out,
            num_values as i64,
            block_shift,
        )?;
        for &v in &actual_values {
            writer.add(v)?;
        }
        writer.finish()?;
        data_length = data_out.get_file_pointer();
    }

    {
        let mut meta_in = dir.open_input("meta", &IOContext::read_once_io_context()?)?;
        let data_in = dir.open_input("data", &IOContext::default_io_context()?)?;
        let meta = load_meta(&mut meta_in, num_values as i64, block_shift)?;
        let slice = Arc::new(Mutex::new(data_in.random_access_slice(0, data_length)?));
        let values = DirectMonotonicReader::get_instance(&meta, slice)?;
        for (i, &v) in actual_values.iter().enumerate() {
            assert_eq!(v, values.get(i as i64)?);
        }
        assert_eq!(0, data_in.get_file_pointer());
    }

    Ok(())
}

#[test]
pub fn test_zero_values_small_blob_shift() -> Result<()> {
    let mut random = random();
    let dir = new_directory(&mut random)?;
    let num_values = TestUtil::next_int(&mut random, 8, 1 << 20);
    let block_shift = TestUtil::next_int(
        &mut random,
        MIN_BLOCK_SHIFT,
        (num_values as f64).log2() as i32 - 1,
    );

    let data_length;
    {
        let mut meta_out = dir.create_output("meta", &IOContext::default_io_context()?)?;
        let mut data_out = dir.create_output("data", &IOContext::default_io_context()?)?;
        let mut writer = DirectMonotonicWriter::get_instance(
            &mut meta_out,
            &mut data_out,
            num_values as i64,
            block_shift,
        )?;
        for _ in 0..num_values {
            writer.add(0)?;
        }
        writer.finish()?;
        data_length = data_out.get_file_pointer();
    }

    {
        let mut meta_in = dir.open_input("meta", &IOContext::read_once_io_context()?)?;
        let data_in = dir.open_input("data", &IOContext::default_io_context()?)?;
        let meta = load_meta(&mut meta_in, num_values as i64, block_shift)?;
        assert_eq!(meta_in.length(), meta_in.get_file_pointer());
        meta_in.seek(0)?;
        let slice = Arc::new(Mutex::new(data_in.random_access_slice(0, data_length)?));
        let values = DirectMonotonicReader::get_instance(&meta, slice)?;
        for _ in 0..num_values {
            assert_eq!(0, values.get(0)?);
        }
        assert_eq!(0, data_in.get_file_pointer());
    }
    Ok(())
}
#[test]
pub fn test_random() -> Result<()> {
    let mut random = random();
    do_test_random(&mut random, false)
}

#[test]
pub fn test_random_merging() -> Result<()> {
    let mut random = random();
    do_test_random(&mut random, true)
}

fn do_test_random<R: Rng + ?Sized>(random: &mut R, merging: bool) -> Result<()> {
    let iters = at_least(random, 3);
    for _ in 0..iters {
        let dir = new_directory(random)?;
        let block_shift = TestUtil::next_int(random, MIN_BLOCK_SHIFT, MAX_BLOCK_SHIFT);
        let max_num_values = 1 << 20;
        let num_values = if random.random_bool(0.5) {
            TestUtil::next_int(random, 1, max_num_values)
        } else {
            let num_blocks = TestUtil::next_int(random, 0, max_num_values >> block_shift);
            TestUtil::next_int(random, 0, num_blocks) << block_shift
        };

        let mut actual_values = Vec::with_capacity(num_values as usize);
        let mut previous: i64 = random.random();
        if num_values > 0 {
            actual_values.push(previous);
        }
        for _ in 1..num_values {
            let upper = 1 << random.random_range(1..20);
            let value = random.random_range(0..upper) as i64;
            previous += value;
            actual_values.push(previous);
        }

        let data_length;
        {
            let mut meta_out = dir.create_output("meta", &IOContext::default_io_context()?)?;
            let mut data_out = dir.create_output("data", &IOContext::default_io_context()?)?;
            let mut writer = DirectMonotonicWriter::get_instance(
                &mut meta_out,
                &mut data_out,
                num_values as i64,
                block_shift,
            )?;
            for &v in &actual_values {
                writer.add(v)?;
            }
            writer.finish()?;
            data_length = data_out.get_file_pointer();
        }

        {
            let mut meta_in = dir.open_input("meta", &IOContext::read_once_io_context()?)?;
            let data_in = dir.open_input("data", &IOContext::default_io_context()?)?;
            let meta = load_meta(&mut meta_in, num_values as i64, block_shift)?;
            let slice = Arc::new(Mutex::new(data_in.random_access_slice(0, data_length)?));
            let values = DirectMonotonicReader::get_instance_with_merging(&meta, slice, merging)?;
            for (i, &v) in actual_values.iter().enumerate() {
                assert_eq!(v, values.get(i as i64)?);
            }
        }
    }
    Ok(())
}

#[test]
pub fn test_monotonic_binary_search() -> Result<()> {
    let mut random = random();
    let dir = new_directory(&mut random)?;
    do_test_monotonic_binary_search_against_long_array(
        &mut random,
        &dir,
        &[4, 7, 8, 10, 19, 30, 55, 78, 100],
        2,
    )
}

#[test]
pub fn test_monotonic_binary_search_random() -> Result<()> {
    let mut random = random();
    let dir = new_directory(&mut random)?;
    let iters = at_least(&mut random, 100);
    for _ in 0..iters {
        let upper = 1 << random.random_range(0..14);
        let array_length = random.random_range(0..upper);
        let mut array = vec![0; array_length];
        let base: i64 = random.random();
        let bpv = TestUtil::next_int(&mut random, 4, 61);
        for value in array.iter_mut() {
            *value = base + TestUtil::next_long(&mut random, 0, (1 << bpv) - 1);
        }
        array.sort();
        let block_shift = TestUtil::next_int(&mut random, 2, 10);
        do_test_monotonic_binary_search_against_long_array(&mut random, &dir, &array, block_shift)?;
    }
    Ok(())
}

fn do_test_monotonic_binary_search_against_long_array<R: Rng + ?Sized>(
    random: &mut R,
    dir: &impl Directory,
    array: &[i64],
    block_shift: i32,
) -> Result<()> {
    {
        let mut meta_out = dir.create_output("meta", &IOContext::default_io_context()?)?;
        let mut data_out = dir.create_output("data", &IOContext::default_io_context()?)?;
        let mut writer = DirectMonotonicWriter::get_instance(
            &mut meta_out,
            &mut data_out,
            array.len() as i64,
            block_shift,
        )?;
        for &l in array {
            writer.add(l)?;
        }
        writer.finish()?;
    }

    {
        let mut meta_in = dir.open_input("meta", &IOContext::read_once_io_context()?)?;
        let data_in = dir.open_input("data", &IOContext::default_io_context()?)?;
        let meta = load_meta(&mut meta_in, array.len() as i64, block_shift)?;
        let slice = Arc::new(Mutex::new(
            data_in.random_access_slice(0, dir.file_length("data")?)?,
        ));
        let reader = DirectMonotonicReader::get_instance(&meta, slice.clone())?;

        if array.is_empty() {
            assert_eq!(-1, reader.binary_search(0, array.len() as i64, 42)?);
        } else {
            for &val in array.iter() {
                let index = reader.binary_search(0, array.len() as i64, val)?;
                let len = array.len();
                assert!(index >= 0 && (index as usize) < len);
                assert_eq!(val, array[index as usize]);
            }
            if array[0] != i64::MIN {
                assert_eq!(
                    -1,
                    reader.binary_search(0, array.len() as i64, array[0] - 1)?
                );
            }
            if array[array.len() - 1] != i64::MAX {
                assert_eq!(
                    -1 - array.len() as i64,
                    reader.binary_search(0, array.len() as i64, array[array.len() - 1] + 1)?
                );
            }
            if array.len() <= 2 {
                // no op
            } else {
                for i in 0..array.len() - 2 {
                    if array[i] + 1 < array[i + 1] {
                        let intermediate = if random.random_bool(0.5) {
                            array[i] + 1
                        } else {
                            array[i + 1] - 1
                        };
                        let index = reader.binary_search(0, array.len() as i64, intermediate)?;
                        assert!(index < 0);
                        let insertion_point: i32 = (-1 - index).try_into()?;
                        assert!(insertion_point > 0 && (insertion_point as usize) < array.len());
                        assert!(array[insertion_point as usize] > intermediate);
                        assert!(array[(insertion_point - 1) as usize] < intermediate);
                    }
                }
            }
        }
    }
    dir.delete_file("meta")?;
    dir.delete_file("data")?;
    Ok(())
}
