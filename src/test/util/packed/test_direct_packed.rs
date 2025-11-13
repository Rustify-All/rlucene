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

use crate::core::store::data_output::DataOutput;
use crate::core::store::directory::Directory;
use crate::core::store::{IOContext, IndexInput};
use crate::core::util::error::lucene_error::Result;
use crate::core::util::long_values::LongValues;
use crate::core::util::packed::PackedInts;
use crate::core::util::packed::direct_reader::DirectReader;
use crate::core::util::packed::direct_writer::{DirectWriter, bits_required};
use crate::test::util::lucene_test_case::lucene_test_case_util::{
    is_night_mode, new_directory, random,
};
use crate::test::util::test_util::TestUtil;

#[allow(dead_code)] // for quick search
pub struct TestDirectPacked;

#[test]
fn test_simple() -> Result<()> {
    let mut random = random();
    let dir = new_directory(&mut random)?;
    let bits_per_value = bits_required(2)?;
    {
        let mut output = dir.create_output("foo", &IOContext::default_io_context()?)?;
        let mut writer = DirectWriter::get_instance(&mut output, 5, bits_per_value)?;
        writer.add(1)?;
        writer.add(0)?;
        writer.add(2)?;
        writer.add(1)?;
        writer.add(2)?;
        writer.finish()?;
    }
    let input = dir.open_input("foo", &IOContext::default_io_context()?)?;
    let slice = input.random_access_slice(0, input.length())?;
    let reader =
        DirectReader::get_instance_with_offset(Arc::new(Mutex::new(slice)), bits_per_value, 0);
    assert_eq!(1, reader.get(0)?);
    assert_eq!(0, reader.get(1)?);
    assert_eq!(2, reader.get(2)?);
    assert_eq!(1, reader.get(3)?);
    assert_eq!(2, reader.get(4)?);
    Ok(())
}
/// test exception is delivered if you add the wrong number of values.
#[test]
fn test_not_enough_values() -> Result<()> {
    let mut random = random();
    let dir = new_directory(&mut random)?;
    let bits_per_value = bits_required(2)?;
    {
        let mut output = dir.create_output("foo", &IOContext::default_io_context()?)?;
        let mut writer = DirectWriter::get_instance(&mut output, 5, bits_per_value)?;
        writer.add(1)?;
        writer.add(0)?;
        writer.add(2)?;
        writer.add(1)?;
        let err = writer.finish().unwrap_err();
        assert!(err.to_string().starts_with("Wrong number of values added"));
    }
    Ok(())
}

#[test]
fn test_random() -> Result<()> {
    let mut random = random();
    let dir = new_directory(&mut random)?;
    for bpv in 1..=64 {
        do_test_bpv(&mut random, &dir, bpv, 0, false)?;
    }
    Ok(())
}

#[test]
fn test_random_with_offset() -> Result<()> {
    let mut random = random();
    let dir = new_directory(&mut random)?;
    let offset = TestUtil::next_int(&mut random, 1, 100);
    for bpv in 1..=64 {
        do_test_bpv(&mut random, &dir, bpv, offset as i64, false)?;
    }
    Ok(())
}

#[test]
fn test_random_merge() -> Result<()> {
    let mut random = random();
    let dir = new_directory(&mut random)?;
    for bpv in 1..=1 {
        do_test_bpv(&mut random, &dir, bpv, 0, true)?;
    }
    Ok(())
}

#[test]
fn test_random_merge_with_offset() -> Result<()> {
    let mut random = random();
    let dir = new_directory(&mut random)?;
    let offset = TestUtil::next_int(&mut random, 1, 100);
    for bpv in 1..=64 {
        do_test_bpv(&mut random, &dir, bpv, offset as i64, true)?;
    }
    Ok(())
}

fn do_test_bpv<R: Rng + ?Sized>(
    random: &mut R,
    directory: &impl Directory,
    bpv: i32,
    offset: i64,
    merge: bool,
) -> Result<()> {
    let num_iters = if is_night_mode() { 100 } else { 10 };
    for i in 0..num_iters {
        let original = random_longs(random, bpv);
        let bits_required = if bpv == 64 {
            64
        } else {
            bits_required(1i64 << (bpv - 1))?
        };
        let name = format!("bpv{}_{}", bpv, i);
        {
            let mut output = directory.create_output(&name, &IOContext::default_io_context()?)?;
            for _ in 0..offset {
                output.write_byte(random.random())?;
            }
            let mut writer =
                DirectWriter::get_instance(&mut output, original.len() as i64, bits_required)?;
            for &val in &original {
                writer.add(val)?;
            }
            writer.finish()?;
        }

        let input = directory.open_input(&name, &IOContext::default_io_context()?)?;
        let slice = Arc::new(Mutex::new(input.random_access_slice(0, input.length())?));
        let reader = if merge {
            DirectReader::get_merge_instance_with_base_offset(
                slice.clone(),
                bits_required,
                offset,
                original.len() as i64,
            )
        } else {
            DirectReader::get_instance_with_offset(slice.clone(), bits_required, offset)
        };
        for (j, &expected) in original.iter().enumerate() {
            assert_eq!(expected, reader.get(j as i64)?, "bpv={}", bpv);
        }
    }
    Ok(())
}

fn random_longs<R: Rng + ?Sized>(random: &mut R, bpv: i32) -> Vec<i64> {
    let amount = random.random_range(0..5000);
    let max = PackedInts::max_value(bpv);
    (0..amount).map(|_| random.random_range(0..=max)).collect()
}
