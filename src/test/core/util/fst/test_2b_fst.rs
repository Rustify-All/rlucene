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
use crate::core::store::IO_CONTEXT_DEFAULT;

// Run something like this:
// cargo test --features monster test_2b_fst::test -- --ignored --nocapture

use std::sync::Arc;
use std::time::Instant;

use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};

use crate::core::index::BytesRef;
use crate::core::store::DataInput;
use crate::core::store::directory::Directory;
use crate::core::store::dummy::dummy_index_output::DummyIndexOutput;
use crate::core::store::mmap_directory::MMapDirectory;
use crate::core::util::IOUtils;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::Result;
use crate::core::util::fst_impl::byte_sequence_outputs::ByteSequenceOutputs;
use crate::core::util::fst_impl::fst::{FST, InputType, read_metadata};
use crate::core::util::fst_impl::fst_compiler::{Builder, DataOutputEnum};
use crate::core::util::fst_impl::ints_ref_fst_enum::IntsRefFSTEnum;
use crate::core::util::fst_impl::no_outputs::NoOutputs;
use crate::core::util::fst_impl::outputs::Outputs;
use crate::core::util::fst_impl::positive_int_outputs::PositiveIntOutputs;
use crate::core::util::fst_impl::util::Util;
use crate::core::util::ints_ref::IntsRef;
use crate::test_framework::core::util::lucene_test_case::{create_temp_dir_with_prefix, random};

#[allow(dead_code)] // for quick search
struct Test2BFST;

const LIMIT: i64 = 3 * 1024 * 1024 * 1024;

// Java documents more than 35 GB of memory and about 4.5 hours for this test.
#[cfg(feature = "monster")]
#[test]
#[ignore = "monster"]
fn test() -> Result<()> {
  let mut input = IntsRef::from_slice(vec![0; 7], 0, 7);
  let seed = random().random::<u64>();

  let temp_dir = create_temp_dir_with_prefix("2BFST")?;
  let dir = MMapDirectory::new(temp_dir.path().to_path_buf())?;

  for _iter in 0..1 {
    // Build FST with NoOutputs and stop when nodeCount > 2.2B.
    {
      println!("\nTEST: ~2.2B nodes; output=NO_OUTPUTS");
      let outputs = NoOutputs::get_singleton().clone();

      let builder: Builder<_, DummyIndexOutput> = Builder::new(InputType::Byte1, outputs.clone());
      let mut fst_compiler = builder.build()?;

      let mut count = 0;
      let mut r = StdRng::seed_from_u64(seed);
      let mut input2 = IntsRef::from_slice(vec![0; 200], 0, 200);
      let mut start_time = Instant::now();
      loop {
        // println!("add: {input} -> {output}");
        for value in &mut input2.ints[10..] {
          *value = r.random_range(0..256);
        }
        fst_compiler.add(&input2, None)?;
        count += 1;
        if count % 100_000 == 0 {
          println!(
            "{}: {} RAM bytes used; {} FST bytes; {} nodes; took {} seconds",
            count,
            fst_compiler.fst_ram_bytes_used()?,
            fst_compiler.fst_size_in_bytes(),
            fst_compiler.get_node_count(),
            start_time.elapsed().as_secs()
          );
        }
        if fst_compiler.get_node_count() > i32::MAX as i64 + 100 * 1024 * 1024 {
          break;
        }
        next_input(&mut r, &mut input2.ints);
      }

      let metadata = fst_compiler.compile()?.unwrap();
      let fst_reader = fst_compiler.get_fst_reader()?;
      let fst = FST::from_fst_reader(metadata, fst_reader).unwrap();

      println!(
        "\nTEST: now verify [fst size={}; nodeCount={}; arcCount={}]",
        fst.num_bytes(),
        fst_compiler.get_node_count(),
        fst_compiler.get_arc_count()
      );
      input2.ints.fill(0);
      r = StdRng::seed_from_u64(seed);
      start_time = Instant::now();
      for i in 0..count {
        if i % 1_000_000 == 0 {
          println!("{}...: took {} seconds", i, start_time.elapsed().as_secs());
        }
        for value in &mut input2.ints[10..] {
          *value = r.random_range(0..256);
        }
        assert_eq!(Some(None), Util::get_from_ints(&fst, &input2)?);
        next_input(&mut r, &mut input2.ints);
      }

      println!("\nTEST: enum all input/outputs");
      let mut fst_enum = IntsRefFSTEnum::new(fst)?;
      input2.ints.fill(0);
      r = StdRng::seed_from_u64(seed);
      let mut upto = 0;
      while let Some(pair) = fst_enum.next_value()? {
        for value in &mut input2.ints[10..] {
          *value = r.random_range(0..256);
        }
        assert_eq!(input2, pair.input);
        assert!(pair.output.is_none());
        upto += 1;
        next_input(&mut r, &mut input2.ints);
      }
      assert_eq!(count, upto);
      let fst = fst_enum.base.fst;

      println!("\nTEST: save/load FST and re-verify");
      let mut out = dir.create_output("fst", IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?)?;
      let save_result = fst.save_with_same_data_out(&mut out);
      IOUtils::use_or_suppress_result(save_result, out.close())?;
      let mut index_input =
        dir.open_input("fst", IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?)?;
      let load_result = (|| {
        let metadata = read_metadata(&mut index_input, outputs.clone())?;
        FST::from_on_heap_store(metadata, &mut index_input)
      })();
      let fst = IOUtils::use_or_suppress_result(load_result, index_input.close())?;

      println!(
        "\nTEST: now verify [fst size={}; nodeCount={}; arcCount={}]",
        fst.num_bytes(),
        fst_compiler.get_node_count(),
        fst_compiler.get_arc_count()
      );
      input2.ints.fill(0);
      r = StdRng::seed_from_u64(seed);
      start_time = Instant::now();
      for i in 0..count {
        if i % 1_000_000 == 0 {
          println!("{}...: took {} seconds", i, start_time.elapsed().as_secs());
        }
        for value in &mut input2.ints[10..] {
          *value = r.random_range(0..256);
        }
        assert_eq!(Some(None), Util::get_from_ints(&fst, &input2)?);
        next_input(&mut r, &mut input2.ints);
      }

      println!("\nTEST: enum all input/outputs");
      let mut fst_enum = IntsRefFSTEnum::new(fst)?;
      input2.ints.fill(0);
      r = StdRng::seed_from_u64(seed);
      let mut upto = 0;
      while let Some(pair) = fst_enum.next_value()? {
        for value in &mut input2.ints[10..] {
          *value = r.random_range(0..256);
        }
        assert_eq!(input2, pair.input);
        assert!(pair.output.is_none());
        upto += 1;
        next_input(&mut r, &mut input2.ints);
      }
      assert_eq!(count, upto);
      dir.delete_file("fst")?;
    }

    // Build FST with ByteSequenceOutputs and stop when FST size = 3 GB.
    {
      println!("\nTEST: 3 GB size; outputs=bytes");
      let outputs = ByteSequenceOutputs::get_singleton().clone();
      let builder: Builder<_, DummyIndexOutput> = Builder::new(InputType::Byte1, outputs.clone());
      let mut fst_compiler = builder.build()?;

      let mut output_bytes = vec![0; 20];
      input.ints.fill(0);
      let mut count = 0;
      let mut r = StdRng::seed_from_u64(seed);
      loop {
        r.fill(&mut output_bytes[..]);
        // println!("add: {input} -> {output}");
        let output = BytesRef::from_bytes(Arc::new(output_bytes.clone()));
        fst_compiler.add(&input, Some(output))?;
        count += 1;
        if count % 10_000 == 0 {
          let size = fst_compiler.fst_size_in_bytes();
          if count % 1_000_000 == 0 {
            println!("{count}...: {size} bytes");
          }
          if size > LIMIT {
            break;
          }
        }
        next_input(&mut r, &mut input.ints);
      }

      let metadata = fst_compiler.compile()?.unwrap();
      let fst_reader = fst_compiler.get_fst_reader()?;
      let fst = FST::from_fst_reader(metadata, fst_reader).unwrap();

      println!(
        "\nTEST: now verify [fst size={}; nodeCount={}; arcCount={}]",
        fst.num_bytes(),
        fst_compiler.get_node_count(),
        fst_compiler.get_arc_count()
      );
      r = StdRng::seed_from_u64(seed);
      input.ints.fill(0);
      let start_time = Instant::now();
      for i in 0..count {
        if i % 1_000_000 == 0 {
          println!("{}...: took {} seconds", i, start_time.elapsed().as_secs());
        }
        r.fill(&mut output_bytes[..]);
        let actual_output = Util::get_from_ints(&fst, &input)?;
        assert_eq!(
          Some(output_bytes.as_slice()),
          actual_output
            .as_ref()
            .and_then(Option::as_ref)
            .map(|output| &output.bytes[output.offset..output.offset + output.length]),
          "actual output: {actual_output:?}"
        );
        next_input(&mut r, &mut input.ints);
      }

      println!("\nTEST: enum all input/outputs");
      let mut fst_enum = IntsRefFSTEnum::new(fst)?;
      input.ints.fill(0);
      r = StdRng::seed_from_u64(seed);
      let mut upto = 0;
      while let Some(pair) = fst_enum.next_value()? {
        assert_eq!(input, pair.input);
        r.fill(&mut output_bytes[..]);
        assert_eq!(
          output_bytes.as_slice(),
          pair.output.as_ref().unwrap().as_byte_slice(),
          "actual output: {:?}",
          pair.output
        );
        upto += 1;
        next_input(&mut r, &mut input.ints);
      }
      assert_eq!(count, upto);
      let fst = fst_enum.base.fst;

      println!("\nTEST: save/load FST and re-verify");
      let mut out = dir.create_output("fst", IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?)?;
      let save_result = fst.save_with_same_data_out(&mut out);
      IOUtils::use_or_suppress_result(save_result, out.close())?;
      let mut index_input =
        dir.open_input("fst", IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?)?;
      let load_result = (|| {
        let metadata = read_metadata(&mut index_input, outputs.clone())?;
        FST::from_on_heap_store(metadata, &mut index_input)
      })();
      let fst = IOUtils::use_or_suppress_result(load_result, index_input.close())?;

      println!(
        "\nTEST: now verify [fst size={}; nodeCount={}; arcCount={}]",
        fst.num_bytes(),
        fst_compiler.get_node_count(),
        fst_compiler.get_arc_count()
      );
      r = StdRng::seed_from_u64(seed);
      input.ints.fill(0);
      let start_time = Instant::now();
      for i in 0..count {
        if i % 1_000_000 == 0 {
          println!("{}...: took {} seconds", i, start_time.elapsed().as_secs());
        }
        r.fill(&mut output_bytes[..]);
        let actual_output = Util::get_from_ints(&fst, &input)?;
        assert_eq!(
          Some(output_bytes.as_slice()),
          actual_output
            .as_ref()
            .and_then(Option::as_ref)
            .map(|output| &output.bytes[output.offset..output.offset + output.length]),
          "actual output: {actual_output:?}"
        );
        next_input(&mut r, &mut input.ints);
      }

      println!("\nTEST: enum all input/outputs");
      let mut fst_enum = IntsRefFSTEnum::new(fst)?;
      input.ints.fill(0);
      r = StdRng::seed_from_u64(seed);
      let mut upto = 0;
      while let Some(pair) = fst_enum.next_value()? {
        assert_eq!(input, pair.input);
        r.fill(&mut output_bytes[..]);
        assert_eq!(
          output_bytes.as_slice(),
          pair.output.as_ref().unwrap().as_byte_slice(),
          "actual output: {:?}",
          pair.output
        );
        upto += 1;
        next_input(&mut r, &mut input.ints);
      }
      assert_eq!(count, upto);
      dir.delete_file("fst")?;
    }

    // Build FST with PositiveIntOutputs and stop when FST size = 3 GB.
    {
      println!("\nTEST: 3 GB size; outputs=long");
      let outputs = PositiveIntOutputs::get_singleton().clone();
      let builder: Builder<_, DummyIndexOutput> = Builder::new(InputType::Byte1, outputs.clone());
      let mut fst_compiler = builder.build()?;

      let mut output = 1_i64;
      input.ints.fill(0);
      let mut count = 0;
      let mut r = StdRng::seed_from_u64(seed);
      loop {
        // println!("add: {input} -> {output}");
        fst_compiler.add(&input, Some(output))?;
        output += 1 + r.random_range(0..10);
        count += 1;
        if count % 10_000 == 0 {
          let size = fst_compiler.fst_size_in_bytes();
          if count % 1_000_000 == 0 {
            println!("{count}...: {size} bytes");
          }
          if size > LIMIT {
            break;
          }
        }
        next_input(&mut r, &mut input.ints);
      }

      let metadata = fst_compiler.compile()?.unwrap();
      let fst_reader = fst_compiler.get_fst_reader()?;
      let fst = FST::from_fst_reader(metadata, fst_reader).unwrap();

      println!(
        "\nTEST: now verify [fst size={}; nodeCount={}; arcCount={}]",
        fst.num_bytes(),
        fst_compiler.get_node_count(),
        fst_compiler.get_arc_count()
      );
      input.ints.fill(0);
      output = 1;
      r = StdRng::seed_from_u64(seed);
      let start_time = Instant::now();
      for i in 0..count {
        if i % 1_000_000 == 0 {
          println!("{}...: took {} seconds", i, start_time.elapsed().as_secs());
        }
        assert_eq!(Some(Some(output)), Util::get_from_ints(&fst, &input)?);
        output += 1 + r.random_range(0..10);
        next_input(&mut r, &mut input.ints);
      }

      println!("\nTEST: enum all input/outputs");
      let mut fst_enum = IntsRefFSTEnum::new(fst)?;
      input.ints.fill(0);
      r = StdRng::seed_from_u64(seed);
      let mut upto = 0;
      output = 1;
      while let Some(pair) = fst_enum.next_value()? {
        assert_eq!(input, pair.input);
        assert_eq!(Some(output), pair.output);
        output += 1 + r.random_range(0..10);
        upto += 1;
        next_input(&mut r, &mut input.ints);
      }
      assert_eq!(count, upto);
      let fst = fst_enum.base.fst;

      println!("\nTEST: save/load FST and re-verify");
      let mut out = dir.create_output("fst", IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?)?;
      let save_result = fst.save_with_same_data_out(&mut out);
      IOUtils::use_or_suppress_result(save_result, out.close())?;
      let mut index_input =
        dir.open_input("fst", IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?)?;
      let load_result = (|| {
        let metadata = read_metadata(&mut index_input, outputs.clone())?;
        FST::from_on_heap_store(metadata, &mut index_input)
      })();
      let fst = IOUtils::use_or_suppress_result(load_result, index_input.close())?;

      println!(
        "\nTEST: now verify [fst size={}; nodeCount={}; arcCount={}]",
        fst.num_bytes(),
        fst_compiler.get_node_count(),
        fst_compiler.get_arc_count()
      );
      input.ints.fill(0);
      output = 1;
      r = StdRng::seed_from_u64(seed);
      let start_time = Instant::now();
      for i in 0..count {
        if i % 1_000_000 == 0 {
          println!("{}...: took {} seconds", i, start_time.elapsed().as_secs());
        }
        assert_eq!(Some(Some(output)), Util::get_from_ints(&fst, &input)?);
        output += 1 + r.random_range(0..10);
        next_input(&mut r, &mut input.ints);
      }

      println!("\nTEST: enum all input/outputs");
      let mut fst_enum = IntsRefFSTEnum::new(fst)?;
      input.ints.fill(0);
      r = StdRng::seed_from_u64(seed);
      let mut upto = 0;
      output = 1;
      while let Some(pair) = fst_enum.next_value()? {
        assert_eq!(input, pair.input);
        assert_eq!(Some(output), pair.output);
        output += 1 + r.random_range(0..10);
        upto += 1;
        next_input(&mut r, &mut input.ints);
      }
      assert_eq!(count, upto);
      dir.delete_file("fst")?;
    }
  }
  dir.close()
}

fn next_input(r: &mut StdRng, ints: &mut [i32]) {
  for down_to in (0..=6).rev() {
    // Must add random amounts (and not just 1) because
    // otherwise FST outsmarts us and remains tiny.
    ints[down_to] += 1 + r.random_range(0..10);
    if ints[down_to] < 256 {
      break;
    } else {
      ints[down_to] = 0;
    }
  }
}
