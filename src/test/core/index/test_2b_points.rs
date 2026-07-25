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
use crate::core::document::long_point::LongPoint;
use crate::core::index::concurrent_merge_scheduler::ConcurrentMergeScheduler;
use crate::core::index::directory_reader;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::index_writer_config::OpenMode;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::merge_policy::MergePolicyEnum;
use crate::core::index::point_values::PointValues;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::store::FSDirectories;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::util::lucene_test_case::{
  create_temp_dir_with_prefix, new_index_writer_config_with_analyzer,
  new_log_merge_policy_with_merge_factor, random,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::RngExt;
use std::sync::Arc;

#[allow(dead_code)] // for quick search
struct Test2BPoints;

#[cfg(feature = "monster")]
#[test]
#[ignore = "monster"]
fn test_1d() -> Result<()> {
  let mut random = random();
  let dir = Arc::new(FSDirectories::open(
    create_temp_dir_with_prefix("2BPoints1D")?.keep(),
  )?);

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let merge_scheduler = ConcurrentMergeScheduler::new();
  merge_scheduler.set_max_merges_and_threads(6, 3)?;
  let mut merge_policy = new_log_merge_policy_with_merge_factor(&mut random, 10)?;
  if let MergePolicyEnum::LogBytesSize(policy) = &mut merge_policy {
    // 1 petabyte:
    policy.set_max_merge_mb(1024.0 * 1024.0 * 1024.0);
  }
  iwc
    .set_codec(TestUtil::get_default_codec())
    .set_max_buffered_docs(-1)
    .set_ram_buffer_size_mb(256.0)
    .set_merge_scheduler(merge_scheduler)
    .set_merge_policy(merge_policy)
    .set_open_mode(OpenMode::Create);
  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let num_docs = (i32::MAX / 26) + 1;
  let mut counter = 0i64;
  for _ in 0..num_docs {
    let mut doc = Document::new();
    for _ in 0..26 {
      let x = ((random.random::<i32>() as i64) << 32) | counter;
      doc.add(LongPoint::new("long", [x])?);
      counter += 1;
    }
    writer.add_document(doc)?;
  }
  writer.force_merge(1)?;
  let reader = Arc::new(directory_reader::open_from_writer(&writer)?);
  let searcher = IndexSearcher::new(reader.clone().get_context()?)?;
  assert_eq!(
    num_docs,
    searcher.count(LongPoint::new_range_query("long", i64::MIN, i64::MAX)?)?
  );
  let context = (&reader).get_context()?;
  let points = context.leaves()?[0]
    .reader()
    .get_point_values("long")?
    .expect("long points must exist");
  assert!(points.size()? > i32::MAX as usize);
  reader.close()?;
  writer.close()?;
  println!("TEST: now CheckIndex");
  TestUtil::check_index(&mut random, Arc::clone(&dir))?;
  dir.as_ref().close()?;
  Ok(())
}

#[cfg(feature = "monster")]
#[test]
#[ignore = "monster"]
fn test_2d() -> Result<()> {
  let mut random = random();
  let dir = Arc::new(FSDirectories::open(
    create_temp_dir_with_prefix("2BPoints2D")?.keep(),
  )?);

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let merge_scheduler = ConcurrentMergeScheduler::new();
  merge_scheduler.set_max_merges_and_threads(6, 3)?;
  let mut merge_policy = new_log_merge_policy_with_merge_factor(&mut random, 10)?;
  if let MergePolicyEnum::LogBytesSize(policy) = &mut merge_policy {
    // 1 petabyte:
    policy.set_max_merge_mb(1024.0 * 1024.0 * 1024.0);
  }
  iwc
    .set_codec(TestUtil::get_default_codec())
    .set_max_buffered_docs(-1)
    .set_ram_buffer_size_mb(256.0)
    .set_merge_scheduler(merge_scheduler)
    .set_merge_policy(merge_policy)
    .set_open_mode(OpenMode::Create);
  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let num_docs = (i32::MAX / 26) + 1;
  let mut counter = 0i64;
  for _ in 0..num_docs {
    let mut doc = Document::new();
    for _ in 0..26 {
      let x = ((random.random::<i32>() as i64) << 32) | counter;
      let y = ((random.random::<i32>() as i64) << 32) | random.random::<i32>() as i64;
      doc.add(LongPoint::new("long", [x, y])?);
      counter += 1;
    }
    writer.add_document(doc)?;
  }
  writer.force_merge(1)?;
  let reader = Arc::new(directory_reader::open_from_writer(&writer)?);
  let searcher = IndexSearcher::new(reader.clone().get_context()?)?;
  assert_eq!(
    num_docs,
    searcher.count(LongPoint::new_range_query_n(
      "long",
      [i64::MIN, i64::MIN],
      [i64::MAX, i64::MAX],
    )?)?
  );
  let context = (&reader).get_context()?;
  let points = context.leaves()?[0]
    .reader()
    .get_point_values("long")?
    .expect("long points must exist");
  assert!(points.size()? > i32::MAX as usize);
  reader.close()?;
  writer.close()?;
  println!("TEST: now CheckIndex");
  TestUtil::check_index(&mut random, Arc::clone(&dir))?;
  dir.as_ref().close()?;
  Ok(())
}
