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
use crate::core::document::field::Store;
use crate::core::document::field_type::FieldType;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::index::directory_reader;
use crate::core::index::index_reader::{IndexReader, IndexReaderContextType};
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::indexable_field::IndexableField;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::log_doc_merge_policy::LogDocMergePolicy;
use crate::core::index::log_merge_policy::LogMergePolicy;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::parallel_leaf_reader::ParallelLeafReader;
use crate::core::index::segment_reader::DefaultLeafReader;
use crate::core::index::standard_directory_reader::StandardDirectoryReader;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term::Term;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::search::boolean_clause::Occur;
use crate::core::search::boolean_query::Builder as BooleanQueryBuilder;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::index_searcher::{DefaultIndexSearcher, IndexSearcher};
use crate::core::search::query::Query;
use crate::core::search::sort::Sort;
use crate::core::search::sort_field::{SortField, SortFieldType};
use crate::core::search::term_query::TermQuery;
use crate::core::store::directory::{DirEnum, Directory};
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind, Result};
use crate::core::util::io_utils::IOUtils;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::util::lucene_test_case::{
  get_only_leaf_reader, new_directory_shared, new_index_writer_config,
  new_index_writer_config_with_analyzer, new_searcher_with_reader, new_text_field, random,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::Rng;
use rand::RngExt;
use std::collections::HashMap;
use std::sync::Arc;

#[allow(dead_code)] // for quick search
struct TestParallelLeafReader;
type SingleSearcher =
  DefaultIndexSearcher<IndexReaderContextType<StandardDirectoryReader<DirEnum>>>;
type ParallelSearcher =
  DefaultIndexSearcher<IndexReaderContextType<ParallelLeafReader<DefaultLeafReader<DirEnum>>>>;

#[test]
fn test_queries() -> Result<()> {
  let mut random = random();
  let (single, dir) = single(&mut random)?;
  let (parallel, dir1, dir2) = parallel(&mut random)?;

  query_test(
    &parallel,
    &single,
    TermQuery::new(Term::from_text("f1", "v1")).into(),
  )?;
  query_test(
    &parallel,
    &single,
    TermQuery::new(Term::from_text("f1", "v2")).into(),
  )?;
  query_test(
    &parallel,
    &single,
    TermQuery::new(Term::from_text("f2", "v1")).into(),
  )?;
  query_test(
    &parallel,
    &single,
    TermQuery::new(Term::from_text("f2", "v2")).into(),
  )?;
  query_test(
    &parallel,
    &single,
    TermQuery::new(Term::from_text("f3", "v1")).into(),
  )?;
  query_test(
    &parallel,
    &single,
    TermQuery::new(Term::from_text("f3", "v2")).into(),
  )?;
  query_test(
    &parallel,
    &single,
    TermQuery::new(Term::from_text("f4", "v1")).into(),
  )?;
  query_test(
    &parallel,
    &single,
    TermQuery::new(Term::from_text("f4", "v2")).into(),
  )?;

  let mut bq1 = BooleanQueryBuilder::new();
  bq1.add(TermQuery::new(Term::from_text("f1", "v1")), Occur::Must)?;
  bq1.add(TermQuery::new(Term::from_text("f4", "v1")), Occur::Must)?;
  query_test(&parallel, &single, bq1.build().into())?;

  single.get_index_reader().close()?;
  parallel.get_index_reader().close()?;
  dir.close()?;
  dir1.close()?;
  dir2.close()
}

#[test]
fn test_field_names() -> Result<()> {
  let mut random = random();
  let dir1 = get_dir1(&mut random)?;
  let dir2 = get_dir2(&mut random)?;
  let pr = ParallelLeafReader::new(vec![
    get_only_leaf_reader(directory_reader::open(dir1.clone())?)?,
    get_only_leaf_reader(directory_reader::open(dir2.clone())?)?,
  ])?;
  let field_infos = pr.get_field_infos()?;
  assert_eq!(4, field_infos.size());
  assert!(field_infos.field_info_by_name("f1")?.is_some());
  assert!(field_infos.field_info_by_name("f2")?.is_some());
  assert!(field_infos.field_info_by_name("f3")?.is_some());
  assert!(field_infos.field_info_by_name("f4")?.is_some());
  pr.close()?;
  dir1.close()?;
  dir2.close()
}

#[test]
fn test_ref_counts1() -> Result<()> {
  let mut random = random();
  let dir1 = get_dir1(&mut random)?;
  let dir2 = get_dir2(&mut random)?;
  let ir1 = get_only_leaf_reader(directory_reader::open(dir1.clone())?)?;
  let ir2 = get_only_leaf_reader(directory_reader::open(dir2.clone())?)?;
  // Close subreaders. ParallelLeafReader will not change refCounts, but will
  // close them when it is closed.
  let pr = ParallelLeafReader::new(vec![ir1.clone(), ir2.clone()])?;

  // Check refCounts.
  assert_eq!(1, ir1.get_ref_count());
  assert_eq!(1, ir2.get_ref_count());
  pr.close()?;
  assert_eq!(0, ir1.get_ref_count());
  assert_eq!(0, ir2.get_ref_count());
  dir1.close()?;
  dir2.close()
}

#[test]
fn test_ref_counts2() -> Result<()> {
  let mut random = random();
  let dir1 = get_dir1(&mut random)?;
  let dir2 = get_dir2(&mut random)?;
  let ir1 = get_only_leaf_reader(directory_reader::open(dir1.clone())?)?;
  let ir2 = get_only_leaf_reader(directory_reader::open(dir2.clone())?)?;
  // Don't close subreaders, so ParallelLeafReader will increment refCounts.
  let pr = ParallelLeafReader::new_with_close_sub_readers(false, vec![ir1.clone(), ir2.clone()])?;
  // Check refCounts.
  assert_eq!(2, ir1.get_ref_count());
  assert_eq!(2, ir2.get_ref_count());
  pr.close()?;
  assert_eq!(1, ir1.get_ref_count());
  assert_eq!(1, ir2.get_ref_count());
  ir1.close()?;
  ir2.close()?;
  assert_eq!(0, ir1.get_ref_count());
  assert_eq!(0, ir2.get_ref_count());
  dir1.close()?;
  dir2.close()
}

#[test]
fn test_close_inner_reader() -> Result<()> {
  let mut random = random();
  let dir1 = get_dir1(&mut random)?;
  let ir1 = get_only_leaf_reader(directory_reader::open(dir1.clone())?)?;

  // With overlapping readers.
  let pr = ParallelLeafReader::new_with_stored_fields(true, vec![ir1.clone()], vec![ir1.clone()])?;

  ir1.close()?;

  // It should already be closed because an inner reader is closed.
  let result = pr
    .stored_fields()
    .and_then(|mut stored_fields| stored_fields.document(0).map(|_| ()));
  assert!(matches!(
    result.as_ref().map_err(LuceneError::kind),
    Err(LuceneErrorKind::AlreadyClosed(_))
  ));

  // No-op.
  pr.close()?;
  dir1.close()
}

#[test]
fn test_incompatible_indexes() -> Result<()> {
  let mut random = random();
  // Two documents.
  let dir1 = get_dir1(&mut random)?;

  // One document only.
  let dir2 = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let writer = IndexWriter::new(
    dir2.clone(),
    new_index_writer_config_with_analyzer(&mut random, analyzer)?,
  )?;
  let mut d3 = Document::new();
  let mut field_types = HashMap::<String, FieldType>::new();
  d3.add(new_text_field(
    &mut random,
    "f3",
    "v1",
    Store::Yes,
    &mut field_types,
  )?);
  writer.add_document(d3)?;
  writer.close()?;

  let ir1 = get_only_leaf_reader(directory_reader::open(dir1.clone())?)?;
  let ir2 = get_only_leaf_reader(directory_reader::open(dir2.clone())?)?;

  // Indexes don't have the same number of documents.
  assert!(matches!(
    (ParallelLeafReader::new(vec![ir1.clone(), ir2.clone()]))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));

  assert!(matches!(
    (ParallelLeafReader::new_with_stored_fields(
      random.random_bool(0.5),
      vec![ir1.clone(), ir2.clone()],
      vec![ir1.clone(), ir2.clone()],
    ))
    .as_ref()
    .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));

  // Check refCounts.
  assert_eq!(1, ir1.get_ref_count());
  assert_eq!(1, ir2.get_ref_count());
  ir1.close()?;
  ir2.close()?;
  dir1.close()?;
  dir2.close()
}

#[test]
fn test_ignore_stored_fields() -> Result<()> {
  let mut random = random();
  let dir1 = get_dir1(&mut random)?;
  let dir2 = get_dir2(&mut random)?;
  let ir1 = get_only_leaf_reader(directory_reader::open(dir1.clone())?)?;
  let ir2 = get_only_leaf_reader(directory_reader::open(dir2.clone())?)?;

  // With overlapping readers.
  let pr = ParallelLeafReader::new_with_stored_fields(
    false,
    vec![ir1.clone(), ir2.clone()],
    vec![ir1.clone()],
  )?;
  assert_eq!(
    Some("v1"),
    pr.stored_fields()?
      .document(0)?
      .get_field("f1")
      .map(IndexableField::string_value)
      .transpose()?
      .flatten()
      .as_deref()
  );
  assert_eq!(
    Some("v1"),
    pr.stored_fields()?
      .document(0)?
      .get_field("f2")
      .map(IndexableField::string_value)
      .transpose()?
      .flatten()
      .as_deref()
  );
  assert!(pr.stored_fields()?.document(0)?.get_field("f3").is_none());
  assert!(pr.stored_fields()?.document(0)?.get_field("f4").is_none());
  // Check that fields are there.
  assert!(pr.terms("f1")?.is_some());
  assert!(pr.terms("f2")?.is_some());
  assert!(pr.terms("f3")?.is_some());
  assert!(pr.terms("f4")?.is_some());
  pr.close()?;

  // No stored fields at all.
  let pr = ParallelLeafReader::new_with_stored_fields(false, vec![ir2.clone()], Vec::new())?;
  assert!(pr.stored_fields()?.document(0)?.get_field("f1").is_none());
  assert!(pr.stored_fields()?.document(0)?.get_field("f2").is_none());
  assert!(pr.stored_fields()?.document(0)?.get_field("f3").is_none());
  assert!(pr.stored_fields()?.document(0)?.get_field("f4").is_none());
  // Check that fields are there.
  assert!(pr.terms("f1")?.is_none());
  assert!(pr.terms("f2")?.is_none());
  assert!(pr.terms("f3")?.is_some());
  assert!(pr.terms("f4")?.is_some());
  pr.close()?;

  // Without overlapping readers.
  let pr = ParallelLeafReader::new_with_stored_fields(true, vec![ir2.clone()], vec![ir1.clone()])?;
  assert_eq!(
    Some("v1"),
    pr.stored_fields()?
      .document(0)?
      .get_field("f1")
      .map(IndexableField::string_value)
      .transpose()?
      .flatten()
      .as_deref()
  );
  assert_eq!(
    Some("v1"),
    pr.stored_fields()?
      .document(0)?
      .get_field("f2")
      .map(IndexableField::string_value)
      .transpose()?
      .flatten()
      .as_deref()
  );
  assert!(pr.stored_fields()?.document(0)?.get_field("f3").is_none());
  assert!(pr.stored_fields()?.document(0)?.get_field("f4").is_none());
  // Check that fields are there.
  assert!(pr.terms("f1")?.is_none());
  assert!(pr.terms("f2")?.is_none());
  assert!(pr.terms("f3")?.is_some());
  assert!(pr.terms("f4")?.is_some());
  pr.close()?;

  // No main readers.
  assert!(matches!(
    (ParallelLeafReader::new_with_stored_fields(true, Vec::new(), vec![ir1]))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));

  dir1.close()?;
  dir2.close()
}

fn query_test(parallel: &ParallelSearcher, single: &SingleSearcher, query: Query) -> Result<()> {
  let parallel_hits = parallel.search(query.clone(), 1000)?.score_docs;
  let single_hits = single.search(query, 1000)?.score_docs;
  assert_eq!(parallel_hits.len(), single_hits.len());
  let mut parallel_fields = parallel.stored_fields()?;
  let mut single_fields = single.stored_fields()?;
  for i in 0..parallel_hits.len() {
    assert!((parallel_hits[i].score - single_hits[i].score).abs() <= 0.001);
    let doc_parallel = parallel_fields.document(parallel_hits[i].doc)?;
    let doc_single = single_fields.document(single_hits[i].doc)?;
    for field in ["f1", "f2", "f3", "f4"] {
      let parallel_value = doc_parallel
        .get_field(field)
        .map(IndexableField::string_value)
        .transpose()?
        .flatten();
      let single_value = doc_single
        .get_field(field)
        .map(IndexableField::string_value)
        .transpose()?
        .flatten();
      assert_eq!(parallel_value, single_value);
    }
  }
  Ok(())
}

// Fields 1-4 indexed together.
fn single<R>(random: &mut R) -> Result<(SingleSearcher, Arc<DirEnum>)>
where
  R: Rng + ?Sized,
{
  let dir = new_directory_shared(random)?;
  let analyzer = MockAnalyzer::new(random);
  let writer = IndexWriter::new(
    dir.clone(),
    new_index_writer_config_with_analyzer(random, analyzer)?,
  )?;
  let mut field_types = HashMap::<String, FieldType>::new();
  let mut d1 = Document::new();
  d1.add(new_text_field(
    random,
    "f1",
    "v1",
    Store::Yes,
    &mut field_types,
  )?);
  d1.add(new_text_field(
    random,
    "f2",
    "v1",
    Store::Yes,
    &mut field_types,
  )?);
  d1.add(new_text_field(
    random,
    "f3",
    "v1",
    Store::Yes,
    &mut field_types,
  )?);
  d1.add(new_text_field(
    random,
    "f4",
    "v1",
    Store::Yes,
    &mut field_types,
  )?);
  writer.add_document(d1)?;
  let mut d2 = Document::new();
  d2.add(new_text_field(
    random,
    "f1",
    "v2",
    Store::Yes,
    &mut field_types,
  )?);
  d2.add(new_text_field(
    random,
    "f2",
    "v2",
    Store::Yes,
    &mut field_types,
  )?);
  d2.add(new_text_field(
    random,
    "f3",
    "v2",
    Store::Yes,
    &mut field_types,
  )?);
  d2.add(new_text_field(
    random,
    "f4",
    "v2",
    Store::Yes,
    &mut field_types,
  )?);
  writer.add_document(d2)?;
  writer.close()?;

  let ir = directory_reader::open(dir.clone())?;
  Ok((new_searcher_with_reader(ir)?, dir))
}

// Fields 1 & 2 in one index, 3 & 4 in the other, with ParallelLeafReader.
fn parallel<R>(random: &mut R) -> Result<(ParallelSearcher, Arc<DirEnum>, Arc<DirEnum>)>
where
  R: Rng + ?Sized,
{
  let dir1 = get_dir1(random)?;
  let dir2 = get_dir2(random)?;
  let pr = ParallelLeafReader::new(vec![
    get_only_leaf_reader(directory_reader::open(dir1.clone())?)?,
    get_only_leaf_reader(directory_reader::open(dir2.clone())?)?,
  ])?;
  TestUtil::check_reader(&pr)?;
  Ok((new_searcher_with_reader(pr)?, dir1, dir2))
}

fn get_dir1<R>(random: &mut R) -> Result<Arc<DirEnum>>
where
  R: Rng + ?Sized,
{
  let dir1 = new_directory_shared(random)?;
  let analyzer = MockAnalyzer::new(random);
  let mut config = new_index_writer_config_with_analyzer(random, analyzer)?;
  config.set_merge_policy(LogMergePolicy::log_doc());
  let writer = IndexWriter::new(dir1.clone(), config)?;
  let mut field_types = HashMap::<String, FieldType>::new();
  let mut d1 = Document::new();
  d1.add(new_text_field(
    random,
    "f1",
    "v1",
    Store::Yes,
    &mut field_types,
  )?);
  d1.add(new_text_field(
    random,
    "f2",
    "v1",
    Store::Yes,
    &mut field_types,
  )?);
  writer.add_document(d1)?;
  let mut d2 = Document::new();
  d2.add(new_text_field(
    random,
    "f1",
    "v2",
    Store::Yes,
    &mut field_types,
  )?);
  d2.add(new_text_field(
    random,
    "f2",
    "v2",
    Store::Yes,
    &mut field_types,
  )?);
  writer.add_document(d2)?;
  writer.force_merge(1)?;
  writer.close()?;
  Ok(dir1)
}

fn get_dir2<R>(random: &mut R) -> Result<Arc<DirEnum>>
where
  R: Rng + ?Sized,
{
  let dir2 = new_directory_shared(random)?;
  let analyzer = MockAnalyzer::new(random);
  let mut config = new_index_writer_config_with_analyzer(random, analyzer)?;
  config.set_merge_policy(LogMergePolicy::log_doc());
  let writer = IndexWriter::new(dir2.clone(), config)?;
  let mut field_types = HashMap::<String, FieldType>::new();
  let mut d3 = Document::new();
  d3.add(new_text_field(
    random,
    "f3",
    "v1",
    Store::Yes,
    &mut field_types,
  )?);
  d3.add(new_text_field(
    random,
    "f4",
    "v1",
    Store::Yes,
    &mut field_types,
  )?);
  writer.add_document(d3)?;
  let mut d4 = Document::new();
  d4.add(new_text_field(
    random,
    "f3",
    "v2",
    Store::Yes,
    &mut field_types,
  )?);
  d4.add(new_text_field(
    random,
    "f4",
    "v2",
    Store::Yes,
    &mut field_types,
  )?);
  writer.add_document(d4)?;
  writer.force_merge(1)?;
  writer.close()?;
  Ok(dir2)
}

// It is not okay to have one leaf with an index sort and another with a
// different index sort.
#[test]
fn test_with_index_sort1() -> Result<()> {
  let mut random = random();
  let dir1 = new_directory_shared(&mut random)?;
  let mut iwc1 = new_index_writer_config(&mut random)?;
  iwc1.set_index_sort(Sort::with_fields(vec![SortField::new(
    Some("foo"),
    SortFieldType::Int,
  )?])?)?;
  let w1 = IndexWriter::new(dir1.clone(), iwc1)?;
  w1.add_document(Document::new())?;
  w1.commit()?;
  w1.add_document(Document::new())?;
  w1.force_merge(1)?;
  w1.close()?;
  let r1 = directory_reader::open(dir1.clone())?;

  let dir2 = new_directory_shared(&mut random)?;
  let mut iwc2 = new_index_writer_config(&mut random)?;
  iwc2.set_index_sort(Sort::with_fields(vec![SortField::new(
    Some("bar"),
    SortFieldType::Int,
  )?])?)?;
  let w2 = IndexWriter::new(dir2.clone(), iwc2)?;
  w2.add_document(Document::new())?;
  w2.commit()?;
  w2.add_document(Document::new())?;
  w2.force_merge(1)?;
  w2.close()?;
  let r2 = directory_reader::open(dir2.clone())?;

  let error =
    match ParallelLeafReader::new(vec![get_only_leaf_reader(&r1)?, get_only_leaf_reader(&r2)?]) {
      Ok(_) => {
        return Err(LuceneError::illegal_state(
          "expected incompatible index sorts",
        ));
      },
      Err(error) => error,
    };
  assert_eq!(
    "cannot combine LeafReaders that have different index sorts: saw both sort=<int: \"foo\"> and <int: \"bar\">",
    error.to_string()
  );
  let close_result = IOUtils::use_or_suppress_result(r1.close(), dir1.close());
  let close_result = IOUtils::use_or_suppress_result(close_result, r2.close());
  IOUtils::use_or_suppress_result(close_result, dir2.close())
}

// It is okay to have one leaf with an index sort and the other with no sort.
#[test]
fn test_with_index_sort2() -> Result<()> {
  let mut random = random();
  let dir1 = new_directory_shared(&mut random)?;
  let mut iwc1 = new_index_writer_config(&mut random)?;
  iwc1.set_index_sort(Sort::with_fields(vec![SortField::new(
    Some("foo"),
    SortFieldType::Int,
  )?])?)?;
  let w1 = IndexWriter::new(dir1.clone(), iwc1)?;
  w1.add_document(Document::new())?;
  w1.commit()?;
  w1.add_document(Document::new())?;
  w1.force_merge(1)?;
  w1.close()?;
  let r1 = directory_reader::open(dir1.clone())?;

  let dir2 = new_directory_shared(&mut random)?;
  let iwc2 = new_index_writer_config(&mut random)?;
  let w2 = IndexWriter::new(dir2.clone(), iwc2)?;
  w2.add_document(Document::new())?;
  w2.add_document(Document::new())?;
  w2.close()?;

  let r2 = directory_reader::open(dir2.clone())?;
  ParallelLeafReader::new_with_close_sub_readers(
    false,
    vec![get_only_leaf_reader(&r1)?, get_only_leaf_reader(&r2)?],
  )?
  .close()?;
  ParallelLeafReader::new_with_close_sub_readers(
    false,
    vec![get_only_leaf_reader(&r2)?, get_only_leaf_reader(&r1)?],
  )?
  .close()?;
  let close_result = IOUtils::use_or_suppress_result(r1.close(), dir1.close());
  let close_result = IOUtils::use_or_suppress_result(close_result, r2.close());
  IOUtils::use_or_suppress_result(close_result, dir2.close())
}

#[test]
fn test_with_doc_values_updates() -> Result<()> {
  let mut random = random();
  let dir1 = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let iwc1 = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let w1 = IndexWriter::new(dir1.clone(), iwc1)?;
  let mut d = Document::new();
  let mut field_types = HashMap::<String, FieldType>::new();
  d.add(new_text_field(
    &mut random,
    "name",
    "billy",
    Store::No,
    &mut field_types,
  )?);
  d.add(NumericDocValuesField::new("age", 21));
  w1.add_document(d)?;
  w1.commit()?;
  w1.update_numeric_doc_value(Term::from_text("name", "billy"), "age", 22)?;
  w1.close()?;

  let r1 = directory_reader::open(dir1.clone())?;
  let lr = ParallelLeafReader::new_with_close_sub_readers(false, vec![get_only_leaf_reader(&r1)?])?;

  let mut dv = lr
    .get_numeric_doc_values("age")?
    .ok_or_else(|| LuceneError::illegal_state("missing age doc values"))?;
  assert_eq!(0, dv.next_doc()?);
  assert_eq!(22, dv.long_value()?);

  assert_eq!(
    1,
    lr.get_field_infos()?
      .field_info_by_name("age")?
      .ok_or_else(|| LuceneError::illegal_state("missing age field info"))?
      .get_doc_values_gen()
  );

  let close_result = IOUtils::use_or_suppress_result(lr.close(), r1.close());
  IOUtils::use_or_suppress_result(close_result, dir1.close())
}
