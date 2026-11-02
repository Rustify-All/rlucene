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
use crate::core::util::fst_impl::fst::{Arc, FST, InputType};
use crate::core::util::fst_impl::fst_compiler::{Builder, DataOutputEnum};
use crate::core::util::fst_impl::no_outputs::NoOutputs;
use crate::core::util::fst_impl::outputs::Outputs;
use crate::core::util::fst_impl::util::Util;
use crate::core::util::ints_ref_builder::IntsRefBuilder;

#[allow(dead_code)] // for quick search
struct TestUtil;

#[test]
fn test_binary_search() -> Result<()> {
  let letters = ["A", "E", "J", "K", "L", "O", "T", "z"];
  let fst = build_fst(&letters, true, false)?;
  let mut arc = Arc::default();
  fst.get_first_arc(&mut arc);
  let mut reader = fst.get_bytes_reader()?;
  fst.read_first_target_arc(&arc.clone(), &mut arc, &mut reader)?;
  for (i, s) in letters.iter().enumerate() {
    let label = s.chars().next().unwrap() as i32;
    let found = Util::binary_search(&fst, &arc, label)?;
    assert_eq!(found, i as i32, "Failed to match '{}'", s);
  }
  assert_eq!(Util::binary_search(&fst, &arc, ' ' as i32)?, -1);
  assert_eq!(
    Util::binary_search(&fst, &arc, '~' as i32)?,
    -1 - letters.len() as i32
  );
  assert_eq!(Util::binary_search(&fst, &arc, 'B' as i32)?, -2);
  assert_eq!(Util::binary_search(&fst, &arc, 'C' as i32)?, -2);
  assert_eq!(Util::binary_search(&fst, &arc, 'P' as i32)?, -7);
  Ok(())
}
#[test]
fn test_continuous() -> Result<()> {
  let letters = ["A", "B", "C", "D", "E", "F", "G", "H"];

  let fst = build_fst(&letters, true, false)?;

  let mut first = Arc::default();
  fst.get_first_arc(&mut first);
  let mut arc = Arc::default();
  let mut reader = fst.get_bytes_reader()?;
  for s in &letters {
    let c = s.chars().next().unwrap() as i32;
    let result = Util::read_ceil_arc(c, &fst, &first, &mut arc, &mut reader)?;
    assert!(result.is_some());
    assert_eq!(arc.label(), c);
  }

  // in the middle
  let c = 'F' as i32;
  let result = Util::read_ceil_arc(c, &fst, &first, &mut arc, &mut reader)?;
  assert!(result.is_some());
  assert_eq!(arc.label(), c);

  // no following arcs
  let result = Util::read_ceil_arc('A' as i32, &fst, &arc.clone(), &mut arc, &mut reader)?;
  assert!(result.is_none());

  Ok(())
}
#[test]
fn test_read_ceil_arc_packed_array() -> Result<()> {
  let letters = &["A", "E", "J", "K", "L", "O", "T", "z"];
  verify_read_ceil_arc(letters, true, false)
}

#[test]
fn test_read_ceil_arc_array_with_gaps() -> Result<()> {
  let letters = &["A", "E", "J", "K", "L", "O", "T"];
  verify_read_ceil_arc(letters, true, true)
}

#[test]
fn test_read_ceil_arc_list() -> Result<()> {
  let letters = &["A", "E", "J", "K", "L", "O", "T", "z"];
  verify_read_ceil_arc(letters, false, false)
}

fn verify_read_ceil_arc(
  letters: &[&str],
  allow_array_arcs: bool,
  allow_direct_addressing: bool,
) -> Result<()> {
  let fst = build_fst(letters, allow_array_arcs, allow_direct_addressing)?;

  let mut first = Arc::default();
  fst.get_first_arc(&mut first);
  let mut arc = Arc::default();
  let mut reader = fst.get_bytes_reader()?;

  for &letter in letters {
    let c = letter.chars().next().unwrap() as i32;
    let result = Util::read_ceil_arc(c, &fst, &first, &mut arc, &mut reader)?;
    assert!(result.is_some());
    assert_eq!(arc.label(), c);
  }

  let result = Util::read_ceil_arc(' ' as i32, &fst, &first, &mut arc, &mut reader)?;
  assert!(result.is_some());
  assert_eq!(arc.label(), 'A' as i32);

  let result = Util::read_ceil_arc('~' as i32, &fst, &first, &mut arc, &mut reader)?;
  assert!(result.is_none());

  let result = Util::read_ceil_arc('F' as i32, &fst, &first, &mut arc, &mut reader)?;
  assert!(result.is_some());
  assert_eq!(arc.label(), 'J' as i32);

  let result = Util::read_ceil_arc('Z' as i32, &fst, &arc.clone(), &mut arc, &mut reader)?;
  assert!(result.is_none());

  Ok(())
}

pub fn build_fst(
  words: &[&str],
  allow_array_arcs: bool,
  allow_direct_addressing: bool,
) -> Result<FST<NoOutputs, DataOutputEnum<DummyIndexOutput>>> {
  let outputs = NoOutputs::get_singleton();

  let mut builder = Builder::new(InputType::Byte1, outputs.clone());
  builder.allow_fixed_length_arcs(allow_array_arcs);

  if !allow_direct_addressing {
    builder.with_direct_addressing_max_oversizing_factor(-1.0);
  }

  let mut compiler = builder.build()?;
  let mut v = IntsRefBuilder::new();

  for word in words {
    let bytes: BytesRef<Vec<u8>> = BytesRef::from_string(word);
    Util::to_ints_ref(&bytes, &mut v)?;
    compiler.add(v.get(), None)?;
  }

  let metadata = compiler.compile()?.unwrap();
  let fst_reader = compiler.get_fst_reader()?;

  let fst = FST::from_fst_reader(metadata, fst_reader).unwrap();
  Ok(fst)
}
