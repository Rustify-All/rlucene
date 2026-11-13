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
use std::fmt::Write as _;

use crate::core::codecs::codec::{self, Codec};
use crate::core::codecs::compound_format::CompoundFormat;
use crate::core::document::document::Document;
use crate::core::document::field::Store;
use crate::core::document::text_field::TextField;
use crate::core::index::field_infos::FieldNumbers;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::log_merge_policy::LogMergePolicy;
use crate::core::index::postings_enum::{POSITIONS, PostingsEnum};
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_merger::SegmentMerger;
use crate::core::index::segment_reader::SegmentReader;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, NO_MORE_DOCS};
use crate::core::search::task_executor::TaskExecutor;
use crate::core::store::IOContext;
use crate::core::store::directory::{DirEnum, Directory};
use crate::core::store::merge_info::MergeInfo;
use crate::core::store::tracking_directory_wrapper::TrackingDirectoryWrapper;
use crate::core::util::bits::Bits;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::info_stream::InfoStreamEnum;
use crate::core::util::{LATEST, StringHelper};
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::util::lucene_test_case::{
  create_temp_dir_with_prefix, new_fs_directory, new_io_context, new_io_context_with_default,
  random,
};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

#[allow(dead_code)] // for quick search
struct TestDoc;
#[test]
fn test_index_and_merge() -> Result<()> {
  let mut random = random();
  let files = [
    ("test.txt", "This is the first test file"),
    ("test2.txt", "This is the second test file"),
  ];

  let directory = new_fs_directory(&mut random, create_temp_dir_with_prefix("TestDoc-index")?)?;

  let si1 = index_doc(directory.clone(), files[0].0, files[0].1)?;
  let si2 = index_doc(directory.clone(), files[1].0, files[1].1)?;

  let multi_file_output = {
    let si_merge = merge(directory.clone(), &si1, &si2, "_merge", false)?;
    let mut out = print_segment(&si1)?;
    out.push_str(&print_segment(&si2)?);
    out.push_str(&print_segment(&si_merge)?);

    let si_merge2 = merge(directory.clone(), &si1, &si2, "_merge2", false)?;
    out.push_str(&print_segment(&si_merge2)?);

    let si_merge3 = merge(directory.clone(), &si_merge, &si_merge2, "_merge3", false)?;
    out.push_str(&print_segment(&si_merge3)?);

    out
  };

  let directory2 = new_fs_directory(&mut random, create_temp_dir_with_prefix("TestDoc-index2")?)?;

  let si1_2 = index_doc(directory2.clone(), files[0].0, files[0].1)?;
  let si2_2 = index_doc(directory2.clone(), files[1].0, files[1].1)?;

  let single_file_output = {
    let si_merge = merge(directory2.clone(), &si1_2, &si2_2, "_merge", true)?;
    let mut out = print_segment(&si1_2)?;
    out.push_str(&print_segment(&si2_2)?);
    out.push_str(&print_segment(&si_merge)?);

    let si_merge2 = merge(directory2.clone(), &si1_2, &si2_2, "_merge2", true)?;
    out.push_str(&print_segment(&si_merge2)?);

    let si_merge3 = merge(directory2.clone(), &si_merge, &si_merge2, "_merge3", true)?;
    out.push_str(&print_segment(&si_merge3)?);

    out
  };

  assert_eq!(multi_file_output, single_file_output);
  Ok(())
}

fn index_doc(dir: Arc<DirEnum>, file_name: &str, text: &str) -> Result<SegmentCommitInfo<DirEnum>> {
  let mut doc = Document::new();
  doc.add(TextField::from_string(
    file_name.to_owned(),
    text,
    Store::No,
  )?);

  let mut random = random();
  let analyzer = MockAnalyzer::new(&mut random);
  let mut config = IndexWriterConfig::with_analyzer(analyzer)?;
  config.set_merge_policy(LogMergePolicy::log_doc());
  let writer = IndexWriter::new(dir.clone(), config)?;
  writer.add_document(doc)?;
  writer.commit()?;
  writer.close()?;
  let inner = writer.inner.lock();
  let last = inner.segment_infos.segments.last().unwrap().clone();
  Ok(last)
}

fn merge(
  dir: Arc<DirEnum>,
  si1: &SegmentCommitInfo<DirEnum>,
  si2: &SegmentCommitInfo<DirEnum>,
  merged: &str,
  use_compound_file: bool,
) -> Result<SegmentCommitInfo<DirEnum>> {
  let mut random = random();
  let context = new_io_context_with_default(
    &mut random,
    &IOContext::with_merge(MergeInfo::new(-1, -1, false, -1))?,
  )?;
  let r1 = SegmentReader::new(si1, LATEST.major, &new_io_context(&mut random)?)?;
  let r2 = SegmentReader::new(si2, LATEST.major, &new_io_context(&mut random)?)?;

  let codec = codec::get_default();
  let tracking_dir = TrackingDirectoryWrapper::new(si1.info.dir.as_ref());
  let mut si = SegmentInfo::new(
    dir.clone(),
    Some((*LATEST).clone()),
    None,
    merged,
    -1,
    false,
    false,
    Some(codec.clone()),
    HashMap::new(),
    StringHelper::random_id(),
    HashMap::new(),
    None,
  )?;
  let info_stream = Arc::new(InfoStreamEnum::default());
  let field_numbers = Arc::new(Mutex::new(FieldNumbers::new::<String, String>(None, None)?));
  let readers: Vec<SegmentReader<DirEnum>> = vec![r1, r2];

  {
    let mut merger = SegmentMerger::new(
      &readers,
      &mut si,
      info_stream,
      &tracking_dir,
      field_numbers,
      &context,
      Arc::new(TaskExecutor::direct()),
    )?;
    merger.merge()?;
  }

  let created = tracking_dir.get_created_files();
  si.set_files(created)?;

  if use_compound_file {
    let files_to_delete = si.files()?.clone();
    codec.compound_format().write(dir.as_ref(), &si, &context)?;
    si.set_use_compound_file(true);
    for name in &files_to_delete {
      dir.as_ref().delete_file(name)?;
    }
  }

  Ok(SegmentCommitInfo::new(
    si,
    0,
    0,
    -1,
    -1,
    -1,
    Some(StringHelper::random_id()),
  ))
}

fn print_segment(si: &SegmentCommitInfo<DirEnum>) -> Result<String> {
  let mut random = random();
  let reader = SegmentReader::new(si, LATEST.major, &new_io_context(&mut random)?)?;
  let num_docs = reader.num_docs()?;
  let mut out = String::new();

  {
    let mut stored_fields = reader.stored_fields()?;
    for i in 0..num_docs {
      let doc = stored_fields.document(i)?;
      out.push_str(&format!("{}\n", doc));
    }
  }

  let field_infos = reader.get_field_infos()?;
  for field_info in field_infos.iter() {
    if *field_info.get_index_options() == IndexOptions::None {
      continue;
    }
    let terms = reader.terms(field_info.name.as_str())?;
    assert!(terms.is_some());
    let mut tis = terms.unwrap().iterator()?;
    while BytesRefIterator::next(&mut tis)?.is_some() {
      out.push_str(&format!("  term={}:{}\n", field_info.name, tis.term()?));
      writeln!(out, "    DF={}", tis.doc_freq()?)?;

      let mut positions = tis.postings_with_flags(None, POSITIONS as i32)?;
      let live_docs = reader.get_live_docs()?;

      while positions.next_doc()? != NO_MORE_DOCS {
        if let Some(ref ld) = live_docs {
          let doc_id = positions.doc_id();
          if !ld.get(doc_id as usize)? {
            continue;
          }
        }
        writeln!(out, " doc={}", positions.doc_id())?;
        let freq = positions.freq()?;
        writeln!(out, " TF={}", freq)?;
        out.push_str(" pos=");
        write!(out, "{}", positions.next_position()?)?;
        for _j in 1..freq {
          write!(out, ",{}", positions.next_position()?)?;
        }
        out.push('\n');
      }
    }
  }
  reader.close()?;
  Ok(out)
}
