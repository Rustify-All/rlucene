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
use crate::core::document::document::Document;
use crate::core::document::sorted_doc_values_field::SortedDocValuesField;
use crate::core::document::sorted_set_doc_values_field::SortedSetDocValuesField;
use crate::core::index::BytesRef;
use crate::core::index::directory_reader;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::multi_doc_values::MultiDocValues;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::index::sorted_doc_values::SortedDocValuesEnum2;
use crate::core::index::sorted_set_doc_values_writer::SortedSetDocValuesEnum2;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::long_values::LongValuesEnum2;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::util::lucene_test_case::{
  new_directory_shared, new_index_writer_config_with_analyzer, random,
};
use rand::RngExt;

#[allow(dead_code)] // for quick search
struct TestOrdinalMap;

#[test]
fn test_ram_bytes_used() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let mut cfg = IndexWriterConfig::with_analyzer(mock)?;
  cfg.set_merge_policy(NoMergePolicy::default());

  let iw = IndexWriter::new(dir.clone(), cfg)?;

  for value in ["a", "b", "c"] {
    let mut d = Document::new();
    d.add(SortedDocValuesField::new(
      "sdv",
      BytesRef::from_string(value),
    ));
    d.add(SortedSetDocValuesField::new(
      "ssdv",
      BytesRef::from_string(value),
    ));
    iw.add_document(d)?;
  }
  iw.commit()?;

  for value in ["b", "c", "d"] {
    let mut d = Document::new();
    d.add(SortedDocValuesField::new(
      "sdv",
      BytesRef::from_string(value),
    ));
    d.add(SortedSetDocValuesField::new(
      "ssdv",
      BytesRef::from_string(value),
    ));
    d.add(SortedSetDocValuesField::new(
      "ssdv",
      BytesRef::from_string(format!("{}{}", value, value).as_str()),
    ));
    iw.add_document(d)?;
  }
  iw.commit()?;

  let r = directory_reader::open_from_writer(&iw)?;
  let sdv = MultiDocValues::get_sorted_values(r, "sdv")?;
  assert!(sdv.is_some());
  let sdv = sdv.unwrap();

  if let SortedDocValuesEnum2::B(ref msdv) = sdv {
    let ram_bytes_used = msdv.mapping.ram_bytes_used()?;
    // TODO: RamUsageTester is not implemented, so Java's exact recursive retained-heap comparison
    // cannot be reproduced; the Accountable result is only sanity-checked for now.
    assert!(ram_bytes_used > 0);
  } else {
    unreachable!("sdv should be MultiSortedDocValues");
  }

  let r = directory_reader::open_from_writer(&iw)?;
  let ssdv = MultiDocValues::get_sorted_set_values(r, "ssdv")?;
  assert!(ssdv.is_some());
  let ssdv = ssdv.unwrap();

  if let SortedSetDocValuesEnum2::B(ref mssdv) = ssdv {
    let ram_bytes_used = mssdv.mapping.ram_bytes_used()?;
    // TODO: RamUsageTester is not implemented, so Java's exact recursive retained-heap comparison
    // cannot be reproduced; the Accountable result is only sanity-checked for now.
    assert!(ram_bytes_used > 0);
  } else {
    unreachable!("ssdv should be MultiSortedSetDocValues");
  }

  iw.close()?;

  Ok(())
}

/// Tests the case where one segment contains all of the global ords.
/// In this case, we apply a small optimization and hardcode the first segment indices and global ord deltas as all zeroes.
// TODO IMPORTANT: Restore `#[test]` once custom codec configuration can force the default
// DocValuesFormat for every field, like Java's `TestUtil.alwaysDocValuesFormat`.
#[allow(dead_code)]
fn test_one_segment_with_all_values() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let mut cfg = IndexWriterConfig::with_analyzer(mock)?;
  cfg.set_merge_policy(NoMergePolicy::default());

  let iw = IndexWriter::new(dir.clone(), cfg)?;

  let num_terms = 1000;

  for i in 0..num_terms {
    let mut d = Document::new();
    let term = i.to_string();
    d.add(SortedDocValuesField::new(
      "sdv",
      BytesRef::from_string(term.as_str()),
    ));
    iw.add_document(d)?;
  }

  iw.force_merge(1)?;

  for _ in 0..10 {
    let mut d = Document::new();
    let term = random.random_range(0..num_terms).to_string();
    d.add(SortedDocValuesField::new(
      "sdv",
      BytesRef::from_string(term.as_str()),
    ));
    iw.add_document(d)?;
  }

  iw.commit()?;

  let r = directory_reader::open_from_writer(&iw)?;

  let sdv = MultiDocValues::get_sorted_values(r, "sdv")?;
  assert!(sdv.is_some());
  let sdv = sdv.unwrap();

  // Check that the optimization kicks in.
  let map = match sdv {
    SortedDocValuesEnum2::B(ref msdv) => &msdv.mapping,
    _ => unreachable!("sdv should be MultiSortedDocValues"),
  };

  assert!(matches!(map.first_segments, LongValuesEnum2::B(_)));
  assert!(matches!(map.global_ord_deltas, LongValuesEnum2::B(_)));

  // Check the map's basic behavior.
  assert_eq!(num_terms as i64, map.get_value_count());
  for i in 0..num_terms {
    assert_eq!(0, map.get_first_segment_number(i as usize)?);
    assert_eq!(i as i64, map.get_first_segment_ord(i as usize)?);
  }

  iw.close()?;

  Ok(())
}
