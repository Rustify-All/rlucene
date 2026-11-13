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
use crate::core::index::base_composite_reader::{
  BCRStoredFieldsImpl, BCRTermVectorsImpl, BaseCompositeReader, BaseCompositeReaderBase,
};
use crate::core::index::composite_reader::CompositeReader;
use crate::core::index::directory_reader;
use crate::core::index::index_reader::{
  CacheHelper, CompositeReaderContextKind, IndexReader, IndexReaderBase, IndexReaderContextType,
};
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::indexable_field::IndexableField;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::multi_reader::MultiReader;
use crate::core::index::multi_terms;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::index::parallel_composite_reader::ParallelCompositeReader;
use crate::core::index::segment_reader::DefaultLeafReader;
use crate::core::index::standard_directory_reader::{
  CacheHelperImpl as DirectoryReaderCacheHelper, StandardDirectoryReader,
};
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term::Term;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::search::boolean_clause::Occur;
use crate::core::search::boolean_query::Builder as BooleanQueryBuilder;
use crate::core::search::index_searcher::DefaultIndexSearcher;
use crate::core::search::query::Query;
use crate::core::search::term_query::TermQuery;
use crate::core::store::directory::{DirEnum, Directory};
use crate::core::util::close::CloseableRef;
use crate::core::util::dummy::dummy_comparator::DummyComparator;
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind, Result};
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::util::lucene_test_case::{
  new_directory_shared, new_index_writer_config_with_analyzer, new_searcher_with_reader,
  new_text_field, random,
};
use rand::{Rng, RngExt};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

#[allow(dead_code)] // for quick search
struct TestParallelCompositeReader;

type DefaultCompositeLeafReader = DefaultLeafReader<DirEnum>;
type StandardTestReader = StandardDirectoryReader<DirEnum>;
type MultiTestReader = MultiReader<Arc<CompositeReaderEnum>>;

#[test]
fn test_queries() -> Result<()> {
  let mut random = random();
  let (single, dir) = single(&mut random, false)?;
  let (parallel, dir1, dir2) = parallel(&mut random, false)?;

  queries(&parallel, &single)?;

  single.get_index_reader().close()?;
  parallel.get_index_reader().close()?;
  dir.close()?;
  dir1.close()?;
  dir2.close()
}

#[test]
fn test_queries_composite_composite() -> Result<()> {
  let mut random = random();
  let (single, dir) = single(&mut random, true)?;
  let (parallel, dir1, dir2) = parallel(&mut random, true)?;

  queries(&parallel, &single)?;

  single.get_index_reader().close()?;
  parallel.get_index_reader().close()?;
  dir.close()?;
  dir1.close()?;
  dir2.close()
}

fn queries<PIRC, SIRC, SIM>(
  parallel: &DefaultIndexSearcher<PIRC, SIM>,
  single: &DefaultIndexSearcher<SIRC, SIM>,
) -> Result<()>
where
  SIM: crate::core::search::similarities_impl::similarities::Similarity,
  PIRC: IndexReaderContext,
  SIRC: IndexReaderContext,
  DefaultIndexSearcher<PIRC, SIM>: Sync,
  DefaultIndexSearcher<SIRC, SIM>: Sync,
{
  query_test(
    parallel,
    single,
    TermQuery::new(Term::from_text("f1", "v1")).into(),
  )?;
  query_test(
    parallel,
    single,
    TermQuery::new(Term::from_text("f1", "v2")).into(),
  )?;
  query_test(
    parallel,
    single,
    TermQuery::new(Term::from_text("f2", "v1")).into(),
  )?;
  query_test(
    parallel,
    single,
    TermQuery::new(Term::from_text("f2", "v2")).into(),
  )?;
  query_test(
    parallel,
    single,
    TermQuery::new(Term::from_text("f3", "v1")).into(),
  )?;
  query_test(
    parallel,
    single,
    TermQuery::new(Term::from_text("f3", "v2")).into(),
  )?;
  query_test(
    parallel,
    single,
    TermQuery::new(Term::from_text("f4", "v1")).into(),
  )?;
  query_test(
    parallel,
    single,
    TermQuery::new(Term::from_text("f4", "v2")).into(),
  )?;

  let mut bq1 = BooleanQueryBuilder::new();
  bq1.add(TermQuery::new(Term::from_text("f1", "v1")), Occur::Must)?;
  bq1.add(TermQuery::new(Term::from_text("f4", "v1")), Occur::Must)?;
  query_test(parallel, single, bq1.build().into())
}

#[test]
fn test_ref_counts1() -> Result<()> {
  let mut random = random();
  let dir1 = get_dir1(&mut random)?;
  let dir2 = get_dir2(&mut random)?;
  let ir1 = Arc::new(directory_reader::open(dir1.clone())?);
  let ir2 = Arc::new(directory_reader::open(dir2.clone())?);

  // Close subreaders. ParallelCompositeReader will not change refCounts, but
  // will close them when it is closed.
  let pr = ParallelCompositeReader::new(vec![ir1.clone(), ir2.clone()])?;
  let psub1 = &pr.get_sequential_sub_readers()[0];

  // Check refCounts.
  assert_eq!(1, ir1.get_ref_count());
  assert_eq!(1, ir2.get_ref_count());
  assert_eq!(1, psub1.get_ref_count());
  pr.close()?;
  assert_eq!(0, ir1.get_ref_count());
  assert_eq!(0, ir2.get_ref_count());
  assert_eq!(0, psub1.get_ref_count());
  dir1.close()?;
  dir2.close()
}

#[test]
fn test_ref_counts2() -> Result<()> {
  let mut random = random();
  let dir1 = get_dir1(&mut random)?;
  let dir2 = get_dir2(&mut random)?;
  let ir1 = Arc::new(directory_reader::open(dir1.clone())?);
  let ir2 = Arc::new(directory_reader::open(dir2.clone())?);

  // Don't close subreaders, so ParallelCompositeReader will increment
  // refCounts.
  let pr =
    ParallelCompositeReader::new_with_close_sub_readers(false, vec![ir1.clone(), ir2.clone()])?;
  let psub1 = &pr.get_sequential_sub_readers()[0];

  // Check refCounts.
  assert_eq!(2, ir1.get_ref_count());
  assert_eq!(2, ir2.get_ref_count());
  assert_eq!(
    1,
    psub1.get_ref_count(),
    "refCount must be 1, as the synthetic reader was created by ParallelCompositeReader"
  );
  pr.close()?;
  assert_eq!(1, ir1.get_ref_count());
  assert_eq!(1, ir2.get_ref_count());
  assert_eq!(
    0,
    psub1.get_ref_count(),
    "refcount must be 0 because parent was closed"
  );
  ir1.close()?;
  ir2.close()?;
  assert_eq!(0, ir1.get_ref_count());
  assert_eq!(0, ir2.get_ref_count());
  assert_eq!(
    0,
    psub1.get_ref_count(),
    "refcount should not change anymore"
  );
  dir1.close()?;
  dir2.close()
}

impl TestParallelCompositeReader {
  #[allow(
    clippy::explicit_counter_loop,
    reason = "Mirrors the Java test's leaf index progression"
  )]
  fn test_reader_closed_listener1<R>(
    random: &mut R,
    close_sub_readers: bool,
    wrap_multi_reader_type: i32,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir1 = get_dir1(random)?;
    let ir1 = Arc::new(CompositeReaderEnum::from_standard(directory_reader::open(
      dir1.clone(),
    )?)?);
    let ir2 = match wrap_multi_reader_type {
      0 => ir1.clone(),
      1 => {
        // Default case: MultiReader closes its sub-readers.
        Arc::new(CompositeReaderEnum::from_multi(vec![ir1.clone()], true)?)
      },
      2 => Arc::new(CompositeReaderEnum::from_multi(vec![ir1.clone()], false)?),
      _ => panic!("invalid wrapMultiReaderType"),
    };

    // With overlapping readers.
    let pr = ParallelCompositeReader::new_with_stored_fields(
      close_sub_readers,
      vec![ir2.clone()],
      vec![ir2.clone()],
    )?;

    assert_eq!(3, (&pr).get_context()?.leaves()?.len());
    assert_eq!(
      ir1
        .get_reader_cache_helper()?
        .expect("reader cache helper")
        .get_key(),
      pr.get_reader_cache_helper()?
        .expect("parallel reader cache helper")
        .get_key()
    );

    let original_context = ir1.clone().get_context()?;
    let parallel_context = (&pr).get_context()?;
    let mut i = 0;
    for context in parallel_context.leaves()? {
      let original_leaf = original_context.leaves()?[i].reader();
      i += 1;
      assert_eq!(
        original_leaf
          .get_core_cache_helper()?
          .expect("core cache helper")
          .get_key(),
        context
          .reader()
          .get_core_cache_helper()?
          .expect("parallel core cache helper")
          .get_key()
      );
      assert_eq!(
        original_leaf
          .get_reader_cache_helper()?
          .expect("reader cache helper")
          .get_key(),
        context
          .reader()
          .get_reader_cache_helper()?
          .expect("parallel reader cache helper")
          .get_key()
      );
    }
    pr.close()?;
    if !close_sub_readers {
      ir1.close()?;
    }

    // We have to close the extra MultiReader, because it will not close its own sub-readers.
    if wrap_multi_reader_type == 2 {
      ir2.close()?;
    }
    dir1.close()
  }
}

#[test]
fn test_reader_closed_listener1() -> Result<()> {
  let mut random = random();
  TestParallelCompositeReader::test_reader_closed_listener1(&mut random, false, 0)?;
  TestParallelCompositeReader::test_reader_closed_listener1(&mut random, true, 0)?;
  TestParallelCompositeReader::test_reader_closed_listener1(&mut random, false, 1)?;
  TestParallelCompositeReader::test_reader_closed_listener1(&mut random, true, 1)?;
  TestParallelCompositeReader::test_reader_closed_listener1(&mut random, false, 2)
}

#[test]
fn test_close_inner_reader() -> Result<()> {
  let mut random = random();
  let dir1 = get_dir1(&mut random)?;
  let ir1 = Arc::new(directory_reader::open(dir1.clone())?);
  assert_eq!(1, ir1.get_sequential_sub_readers()[0].get_ref_count());

  // With overlapping readers.
  let pr =
    ParallelCompositeReader::new_with_stored_fields(true, vec![ir1.clone()], vec![ir1.clone()])?;
  let psub = &pr.get_sequential_sub_readers()[0];
  assert_eq!(1, psub.get_ref_count());

  ir1.close()?;

  assert_eq!(
    1,
    psub.get_ref_count(),
    "refCount of synthetic subreader should be unchanged"
  );
  let result = psub
    .stored_fields()
    .and_then(|mut stored_fields| stored_fields.document(0).map(|_| ()));
  assert!(matches!(
    result.as_ref().map_err(LuceneError::kind),
    Err(LuceneErrorKind::AlreadyClosed(_))
  ));

  let result = pr
    .stored_fields()
    .and_then(|mut stored_fields| stored_fields.document(0).map(|_| ()));
  assert!(matches!(
    result.as_ref().map_err(LuceneError::kind),
    Err(LuceneErrorKind::AlreadyClosed(_))
  ));

  // No-op.
  pr.close()?;
  assert_eq!(0, psub.get_ref_count());
  dir1.close()
}

#[test]
fn test_incompatible_indexes1() -> Result<()> {
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

  let ir1 = Arc::new(directory_reader::open(dir1.clone())?);
  let ir2 = Arc::new(directory_reader::open(dir2.clone())?);

  assert!(matches!(
    (ParallelCompositeReader::new(vec![ir1.clone(), ir2.clone()]))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (ParallelCompositeReader::new_with_close_sub_readers(
      random.random_bool(0.5),
      vec![ir1.clone(), ir2.clone()]
    ))
    .as_ref()
    .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));

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
fn test_incompatible_indexes2() -> Result<()> {
  let mut random = random();
  let dir1 = get_dir1(&mut random)?;
  let dir2 = get_invalid_structured_dir2(&mut random)?;

  let ir1 = Arc::new(directory_reader::open(dir1.clone())?);
  let ir2 = Arc::new(directory_reader::open(dir2.clone())?);
  let readers = vec![ir1.clone(), ir2.clone()];
  assert!(matches!(
    (ParallelCompositeReader::new(readers.clone()))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  assert!(matches!(
    (ParallelCompositeReader::new_with_stored_fields(
      random.random_bool(0.5),
      readers.clone(),
      readers
    ))
    .as_ref()
    .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));

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
fn test_ignore_stored_fields() -> Result<()> {
  let mut random = random();
  let dir1 = get_dir1(&mut random)?;
  let dir2 = get_dir2(&mut random)?;
  let ir1 = Arc::new(directory_reader::open(dir1.clone())?);
  let ir2 = Arc::new(directory_reader::open(dir2.clone())?);

  // With overlapping readers.
  let pr = ParallelCompositeReader::new_with_stored_fields(
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
  assert!(multi_terms::get_terms(&pr, "f1")?.is_some());
  assert!(multi_terms::get_terms(&pr, "f2")?.is_some());
  assert!(multi_terms::get_terms(&pr, "f3")?.is_some());
  assert!(multi_terms::get_terms(&pr, "f4")?.is_some());
  pr.close()?;

  // No stored fields at all.
  let pr = ParallelCompositeReader::new_with_stored_fields(false, vec![ir2.clone()], Vec::new())?;
  assert!(pr.stored_fields()?.document(0)?.get_field("f1").is_none());
  assert!(pr.stored_fields()?.document(0)?.get_field("f2").is_none());
  assert!(pr.stored_fields()?.document(0)?.get_field("f3").is_none());
  assert!(pr.stored_fields()?.document(0)?.get_field("f4").is_none());
  // Check that fields are there.
  assert!(multi_terms::get_terms(&pr, "f1")?.is_none());
  assert!(multi_terms::get_terms(&pr, "f2")?.is_none());
  assert!(multi_terms::get_terms(&pr, "f3")?.is_some());
  assert!(multi_terms::get_terms(&pr, "f4")?.is_some());
  pr.close()?;

  // Without overlapping readers.
  let pr =
    ParallelCompositeReader::new_with_stored_fields(true, vec![ir2.clone()], vec![ir1.clone()])?;
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
  assert!(multi_terms::get_terms(&pr, "f1")?.is_none());
  assert!(multi_terms::get_terms(&pr, "f2")?.is_none());
  assert!(multi_terms::get_terms(&pr, "f3")?.is_some());
  assert!(multi_terms::get_terms(&pr, "f4")?.is_some());
  pr.close()?;

  // No main readers.
  assert!(matches!(
    (ParallelCompositeReader::new_with_stored_fields(true, Vec::new(), vec![ir1]))
      .as_ref()
      .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));

  dir1.close()?;
  dir2.close()
}

#[test]
fn test_to_string() -> Result<()> {
  let mut random = random();
  let dir1 = get_dir1(&mut random)?;
  let ir1 = directory_reader::open(dir1.clone())?;
  let pr = ParallelCompositeReader::new(vec![ir1])?;

  let string = CompositeReader::to_string(&pr);
  assert!(
    string.starts_with("ParallelCompositeReader(ParallelLeafReader("),
    "toString incorrect: {string}"
  );

  pr.close()?;
  dir1.close()
}

#[test]
fn test_to_string_composite_composite() -> Result<()> {
  let mut random = random();
  let dir1 = get_dir1(&mut random)?;
  let ir1 = directory_reader::open(dir1.clone())?;
  let pr = ParallelCompositeReader::new(vec![MultiReader::new(vec![ir1])?])?;

  let string = CompositeReader::to_string(&pr);
  assert!(
    string.starts_with("ParallelCompositeReader(ParallelLeafReader("),
    "toString incorrect (should be flattened): {string}"
  );

  pr.close()?;
  dir1.close()
}

fn query_test<PIRC, SIRC, SIM>(
  parallel: &DefaultIndexSearcher<PIRC, SIM>,
  single: &DefaultIndexSearcher<SIRC, SIM>,
  query: Query,
) -> Result<()>
where
  SIM: crate::core::search::similarities_impl::similarities::Similarity,
  PIRC: IndexReaderContext,
  SIRC: IndexReaderContext,
  DefaultIndexSearcher<PIRC, SIM>: Sync,
  DefaultIndexSearcher<SIRC, SIM>: Sync,
{
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
fn single<R>(
  random: &mut R,
  composite_composite: bool,
) -> Result<(
  DefaultIndexSearcher<IndexReaderContextType<CompositeReaderEnum>>,
  Arc<DirEnum>,
)>
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

  let mut d3 = Document::new();
  d3.add(new_text_field(
    random,
    "f1",
    "v3",
    Store::Yes,
    &mut field_types,
  )?);
  d3.add(new_text_field(
    random,
    "f2",
    "v3",
    Store::Yes,
    &mut field_types,
  )?);
  d3.add(new_text_field(
    random,
    "f3",
    "v3",
    Store::Yes,
    &mut field_types,
  )?);
  d3.add(new_text_field(
    random,
    "f4",
    "v3",
    Store::Yes,
    &mut field_types,
  )?);
  writer.add_document(d3)?;

  let mut d4 = Document::new();
  d4.add(new_text_field(
    random,
    "f1",
    "v4",
    Store::Yes,
    &mut field_types,
  )?);
  d4.add(new_text_field(
    random,
    "f2",
    "v4",
    Store::Yes,
    &mut field_types,
  )?);
  d4.add(new_text_field(
    random,
    "f3",
    "v4",
    Store::Yes,
    &mut field_types,
  )?);
  d4.add(new_text_field(
    random,
    "f4",
    "v4",
    Store::Yes,
    &mut field_types,
  )?);
  writer.add_document(d4)?;
  writer.close()?;

  let ir = if composite_composite {
    CompositeReaderEnum::from_multi(
      vec![
        Arc::new(CompositeReaderEnum::from_standard(directory_reader::open(
          dir.clone(),
        )?)?),
        Arc::new(CompositeReaderEnum::from_standard(directory_reader::open(
          dir.clone(),
        )?)?),
      ],
      true,
    )?
  } else {
    CompositeReaderEnum::from_standard(directory_reader::open(dir.clone())?)?
  };
  Ok((new_searcher_with_reader(ir)?, dir))
}

// Fields 1 & 2 in one index, 3 & 4 in the other, with
// ParallelCompositeReader.
#[allow(
  clippy::type_complexity,
  reason = "Mirrors Java's concrete ParallelCompositeReader return type"
)]
fn parallel<R>(
  random: &mut R,
  composite_composite: bool,
) -> Result<(
  DefaultIndexSearcher<IndexReaderContextType<ParallelCompositeReader<CompositeReaderEnum>>>,
  Arc<DirEnum>,
  Arc<DirEnum>,
)>
where
  R: Rng + ?Sized,
{
  let dir1 = get_dir1(random)?;
  let dir2 = get_dir2(random)?;
  let (rd1, rd2) = if composite_composite {
    let rd1 = CompositeReaderEnum::from_multi(
      vec![
        Arc::new(CompositeReaderEnum::from_standard(directory_reader::open(
          dir1.clone(),
        )?)?),
        Arc::new(CompositeReaderEnum::from_standard(directory_reader::open(
          dir1.clone(),
        )?)?),
      ],
      true,
    )?;
    let rd2 = CompositeReaderEnum::from_multi(
      vec![
        Arc::new(CompositeReaderEnum::from_standard(directory_reader::open(
          dir2.clone(),
        )?)?),
        Arc::new(CompositeReaderEnum::from_standard(directory_reader::open(
          dir2.clone(),
        )?)?),
      ],
      true,
    )?;
    let CompositeReaderEnum::Multi { reader, .. } = &rd1 else {
      panic!("expected MultiReader")
    };
    assert_eq!(2, reader.get_sequential_sub_readers().len());
    let CompositeReaderEnum::Multi { reader, .. } = &rd2 else {
      panic!("expected MultiReader")
    };
    assert_eq!(2, reader.get_sequential_sub_readers().len());
    (rd1, rd2)
  } else {
    let rd1 = CompositeReaderEnum::from_standard(directory_reader::open(dir1.clone())?)?;
    let rd2 = CompositeReaderEnum::from_standard(directory_reader::open(dir2.clone())?)?;
    let CompositeReaderEnum::Standard { reader, .. } = &rd1 else {
      panic!("expected StandardDirectoryReader")
    };
    assert_eq!(3, reader.get_sequential_sub_readers().len());
    let CompositeReaderEnum::Standard { reader, .. } = &rd2 else {
      panic!("expected StandardDirectoryReader")
    };
    assert_eq!(3, reader.get_sequential_sub_readers().len());
    (rd1, rd2)
  };
  let pr = ParallelCompositeReader::new(vec![rd1, rd2])?;
  Ok((new_searcher_with_reader(pr)?, dir1, dir2))
}

// Subreader structure: (1,2,1).
fn get_dir1<R>(random: &mut R) -> Result<Arc<DirEnum>>
where
  R: Rng + ?Sized,
{
  let dir1 = new_directory_shared(random)?;
  let analyzer = MockAnalyzer::new(random);
  let mut config = new_index_writer_config_with_analyzer(random, analyzer)?;
  config.set_merge_policy(NoMergePolicy::default());
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
  writer.commit()?;

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

  let mut d3 = Document::new();
  d3.add(new_text_field(
    random,
    "f1",
    "v3",
    Store::Yes,
    &mut field_types,
  )?);
  d3.add(new_text_field(
    random,
    "f2",
    "v3",
    Store::Yes,
    &mut field_types,
  )?);
  writer.add_document(d3)?;
  writer.commit()?;

  let mut d4 = Document::new();
  d4.add(new_text_field(
    random,
    "f1",
    "v4",
    Store::Yes,
    &mut field_types,
  )?);
  d4.add(new_text_field(
    random,
    "f2",
    "v4",
    Store::Yes,
    &mut field_types,
  )?);
  writer.add_document(d4)?;
  writer.close()?;
  Ok(dir1)
}

// Subreader structure: (1,2,1).
fn get_dir2<R>(random: &mut R) -> Result<Arc<DirEnum>>
where
  R: Rng + ?Sized,
{
  let dir2 = new_directory_shared(random)?;
  let analyzer = MockAnalyzer::new(random);
  let mut config = new_index_writer_config_with_analyzer(random, analyzer)?;
  config.set_merge_policy(NoMergePolicy::default());
  let writer = IndexWriter::new(dir2.clone(), config)?;
  let mut field_types = HashMap::<String, FieldType>::new();

  let mut d1 = Document::new();
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
  writer.commit()?;

  let mut d2 = Document::new();
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

  let mut d3 = Document::new();
  d3.add(new_text_field(
    random,
    "f3",
    "v3",
    Store::Yes,
    &mut field_types,
  )?);
  d3.add(new_text_field(
    random,
    "f4",
    "v3",
    Store::Yes,
    &mut field_types,
  )?);
  writer.add_document(d3)?;
  writer.commit()?;

  let mut d4 = Document::new();
  d4.add(new_text_field(
    random,
    "f3",
    "v4",
    Store::Yes,
    &mut field_types,
  )?);
  d4.add(new_text_field(
    random,
    "f4",
    "v4",
    Store::Yes,
    &mut field_types,
  )?);
  writer.add_document(d4)?;
  writer.close()?;
  Ok(dir2)
}

// This directory has a different subreader structure: (1,1,2).
fn get_invalid_structured_dir2<R>(random: &mut R) -> Result<Arc<DirEnum>>
where
  R: Rng + ?Sized,
{
  let dir2 = new_directory_shared(random)?;
  let analyzer = MockAnalyzer::new(random);
  let mut config = new_index_writer_config_with_analyzer(random, analyzer)?;
  config.set_merge_policy(NoMergePolicy::default());
  let writer = IndexWriter::new(dir2.clone(), config)?;
  let mut field_types = HashMap::<String, FieldType>::new();

  let mut d1 = Document::new();
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
  writer.commit()?;

  let mut d2 = Document::new();
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
  writer.commit()?;

  let mut d3 = Document::new();
  d3.add(new_text_field(
    random,
    "f3",
    "v3",
    Store::Yes,
    &mut field_types,
  )?);
  d3.add(new_text_field(
    random,
    "f4",
    "v3",
    Store::Yes,
    &mut field_types,
  )?);
  writer.add_document(d3)?;

  let mut d4 = Document::new();
  d4.add(new_text_field(
    random,
    "f3",
    "v4",
    Store::Yes,
    &mut field_types,
  )?);
  d4.add(new_text_field(
    random,
    "f4",
    "v4",
    Store::Yes,
    &mut field_types,
  )?);
  writer.add_document(d4)?;
  writer.close()?;
  Ok(dir2)
}

#[allow(
  clippy::large_enum_variant,
  reason = "Test-only enum mirrors Java's common CompositeReader variable"
)]
enum CompositeReaderEnum {
  Standard {
    reader: StandardTestReader,
    base_composite_reader_base: BaseCompositeReaderBase<DefaultCompositeLeafReader>,
    index_reader_base: IndexReaderBase,
  },
  Multi {
    reader: MultiTestReader,
    base_composite_reader_base: BaseCompositeReaderBase<DefaultCompositeLeafReader>,
    index_reader_base: IndexReaderBase,
  },
}

impl CompositeReaderEnum {
  fn from_standard(reader: StandardTestReader) -> Result<Self> {
    let index_reader_base = IndexReaderBase::new();
    let base_composite_reader_base = BaseCompositeReaderBase::new::<DummyComparator>(
      reader.get_sequential_sub_readers().to_vec(),
      None,
      &index_reader_base,
    )?;
    Ok(Self::Standard {
      reader,
      base_composite_reader_base,
      index_reader_base,
    })
  }

  fn from_multi(readers: Vec<Arc<CompositeReaderEnum>>, close_sub_readers: bool) -> Result<Self> {
    let reader = MultiReader::new_with_close_sub_readers(close_sub_readers, readers)?;
    let mut leaves = Vec::new();
    reader.visit_leaves(&mut |leaf| {
      leaves.push(leaf.clone());
      Ok(())
    })?;
    let index_reader_base = IndexReaderBase::new();
    let base_composite_reader_base =
      BaseCompositeReaderBase::new::<DummyComparator>(leaves, None, &index_reader_base)?;
    Ok(Self::Multi {
      reader,
      base_composite_reader_base,
      index_reader_base,
    })
  }

  fn base_composite_reader_base(&self) -> &BaseCompositeReaderBase<DefaultCompositeLeafReader> {
    match self {
      Self::Standard {
        base_composite_reader_base,
        ..
      }
      | Self::Multi {
        base_composite_reader_base,
        ..
      } => base_composite_reader_base,
    }
  }
}

impl IndexReader for CompositeReaderEnum {
  type ContextKind = CompositeReaderContextKind;

  type TermVectors = BCRTermVectorsImpl<DefaultCompositeLeafReader>;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    self.base_composite_reader_base().term_vector(self)
  }

  fn max_doc(&self) -> Result<i32> {
    Ok(self.base_composite_reader_base().max_doc())
  }

  fn num_docs(&self) -> Result<i32> {
    self.base_composite_reader_base().num_docs()
  }

  type StoredFields = BCRStoredFieldsImpl<DefaultCompositeLeafReader>;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    self.base_composite_reader_base().stored_fields(self)
  }

  fn do_close(&self) -> Result<()> {
    match self {
      Self::Standard { reader, .. } => reader.close(),
      Self::Multi { reader, .. } => reader.close(),
    }
  }

  type ReaderCacheHelper = DirectoryReaderCacheHelper;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    match self {
      Self::Standard { reader, .. } => reader.get_reader_cache_helper(),
      Self::Multi { reader, .. } => reader.get_reader_cache_helper(),
    }
  }

  fn doc_freq(&self, term: &Term) -> Result<i32> {
    self.base_composite_reader_base().doc_freq(term, self)
  }

  fn total_term_freq(&self, term: &Term) -> Result<i64> {
    self
      .base_composite_reader_base()
      .total_term_freq(term, self)
  }

  fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
    self
      .base_composite_reader_base()
      .get_sum_doc_freq(field, self)
  }

  fn get_doc_count(&self, field: &str) -> Result<i32> {
    self.base_composite_reader_base().get_doc_count(field, self)
  }

  fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
    self
      .base_composite_reader_base()
      .get_sum_total_term_freq(field, self)
  }

  fn index_base(&self) -> &IndexReaderBase {
    match self {
      Self::Standard {
        index_reader_base, ..
      }
      | Self::Multi {
        index_reader_base, ..
      } => index_reader_base,
    }
  }
}

impl Display for CompositeReaderEnum {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Standard { reader, .. } => write!(f, "{reader}"),
      Self::Multi { .. } => write!(f, "MultiReader"),
    }
  }
}

impl CompositeReader for CompositeReaderEnum {
  type LeafReader = DefaultCompositeLeafReader;

  type SubReader = DefaultCompositeLeafReader;

  fn get_sequential_sub_readers(&self) -> &[Self::SubReader] {
    self
      .base_composite_reader_base()
      .get_sequential_sub_readers()
  }

  fn visit_leaves<F>(&self, visitor: &mut F) -> Result<()>
  where
    F: FnMut(&Self::LeafReader) -> Result<()>,
  {
    for leaf_reader in self.get_sequential_sub_readers() {
      visitor(leaf_reader)?;
    }
    Ok(())
  }

  fn to_string(&self) -> String {
    format!("{self}")
  }
}

impl BaseCompositeReader for CompositeReaderEnum {}
