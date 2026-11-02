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
use crate::core::index::BytesRef;
use crate::core::store::dummy::dummy_index_output::DummyIndexOutput;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::fst_impl::bytes_ref_fst_enum::BytesRefFSTEnum;
use crate::core::util::fst_impl::fst::{FST, InputType};
use crate::core::util::fst_impl::fst_compiler::{
  Builder, DIRECT_ADDRESSING_MAX_OVERSIZING_FACTOR, DataOutputEnum,
};
use crate::core::util::fst_impl::no_outputs::NoOutputs;
use crate::core::util::fst_impl::outputs::Outputs;
use crate::core::util::fst_impl::util::Util;
use crate::core::util::ints_ref_builder::IntsRefBuilder;
#[cfg(feature = "nightly")]
use rand::Rng;
#[allow(dead_code)] // for quick search
struct TestFSTDirectAddressing;

#[test]
fn test_dense_with_gap() -> Result<()> {
  let words = ["ah", "bi", "cj", "dk", "fl", "gm"];
  let entries: [BytesRef<Vec<u8>>; 6] = words.map(BytesRef::from_string);

  let fst = build_fst(&entries)?;
  let mut fst_enum = BytesRefFSTEnum::new(fst)?;
  for entry in &entries {
    assert!(
      fst_enum.seek_exact(entry)?.is_some(),
      "{} not found",
      entry.utf8_to_string()?
    );
  }
  Ok(())
}

#[test]
fn test_de_dup_tails() -> Result<()> {
  let mut entries = Vec::with_capacity(250_000);
  let mut i = 0;
  while i < 1000000 {
    let mut b = vec![0u8; 3];
    let mut val = i;
    let mut j = b.len();
    while j > 0 {
      j -= 1;
      b[j] = (val & 0xff) as u8;
      val >>= 8;
    }
    entries.push(BytesRef::from_bytes(b));
    i += 4;
  }
  let fst = build_fst(&entries)?;
  let size = fst.num_bytes() as f64;
  assert!(size <= 1648.0 * 1.01, "FST size = {} B", size);
  Ok(())
}

#[cfg(feature = "nightly")]
#[test]
#[ignore = "nightly"]
fn test_worst_case_for_direct_addressing() -> Result<()> {
  const MEMORY_INCREASE_LIMIT_PERCENT: f64 = 1.0;
  const NUM_WORDS: usize = 1000000;

  // Generate words with specially crafted bytes.
  let mut word_set = std::collections::HashSet::new();
  let mut rng = rand::rng();
  for _ in 0..NUM_WORDS {
    let mut b = vec![0u8; 5];
    rng.fill_bytes(&mut b);
    for byte in &mut b {
      *byte &= 0xfc; // Make this byte a multiple of 4.
    }
    word_set.insert(BytesRef::from_bytes(b));
  }
  let mut word_list: Vec<BytesRef<Vec<u8>>> = word_set.into_iter().collect();
  word_list.sort();

  // Disable direct addressing and measure the FST size.
  let fst_compiler = create_fst_compiler(-1.0)?;
  let fst = build_fst_with_compiler(&word_list, fst_compiler)?;
  let ram_bytes_used_no_direct_addressing = fst.num_bytes() as f64;

  // Enable direct addressing and measure the FST size.
  let fst_compiler = create_fst_compiler(DIRECT_ADDRESSING_MAX_OVERSIZING_FACTOR)?;
  let fst = build_fst_with_compiler(&word_list, fst_compiler)?;
  let ram_bytes_used = fst.num_bytes() as f64;

  // Compute the size increase in percents.
  let direct_addressing_memory_increase_percent =
    (ram_bytes_used / ram_bytes_used_no_direct_addressing - 1.0) * 100.0;

  // Verify the FST size does not exceed the limit.
  assert!(
    direct_addressing_memory_increase_percent < MEMORY_INCREASE_LIMIT_PERCENT,
    "FST size exceeds limit, size = {}, increase = {} %, limit = {} %",
    ram_bytes_used,
    direct_addressing_memory_increase_percent,
    MEMORY_INCREASE_LIMIT_PERCENT
  );
  Ok(())
}

fn build_fst(
  entries: &[BytesRef<Vec<u8>>],
) -> Result<FST<NoOutputs, DataOutputEnum<DummyIndexOutput>>> {
  build_fst_with_compiler(
    entries,
    create_fst_compiler(DIRECT_ADDRESSING_MAX_OVERSIZING_FACTOR)?,
  )
}

fn build_fst_with_compiler(
  entries: &[BytesRef<Vec<u8>>],
  mut fst_compiler: crate::core::util::fst_impl::fst_compiler::FSTCompiler<
    NoOutputs,
    DummyIndexOutput,
  >,
) -> Result<FST<NoOutputs, DataOutputEnum<DummyIndexOutput>>> {
  let mut scratch = IntsRefBuilder::new();
  for entry in entries {
    Util::to_ints_ref(entry, &mut scratch)?;
    fst_compiler.add(scratch.get(), None)?;
  }
  let metadata = fst_compiler.compile()?.unwrap();
  let fst_reader = fst_compiler.get_fst_reader()?;
  Ok(FST::from_fst_reader(metadata, fst_reader).unwrap())
}

fn create_fst_compiler(
  direct_addressing_max_oversizing_factor: f32,
) -> Result<crate::core::util::fst_impl::fst_compiler::FSTCompiler<NoOutputs, DummyIndexOutput>> {
  use crate::core::util::fst_impl::no_outputs::NoOutputs;
  let mut builder = Builder::new(InputType::Byte1, NoOutputs::get_singleton().clone());
  builder.with_direct_addressing_max_oversizing_factor(direct_addressing_max_oversizing_factor);
  builder.build()
}

// TODO IMPORTANT main 未实现
