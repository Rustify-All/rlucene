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
use crate::core::document::field::Field;
use crate::core::document::field_type::FieldType;
use crate::core::index::directory_reader;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::term::Term;
use crate::core::search::boolean_clause::Occur;
use crate::core::search::boolean_query::Builder;
use crate::core::search::boolean_scorer::SIZE;
use crate::core::search::phrase_query::PhraseQuery;
use crate::core::search::prefix_query::PrefixQuery;
use crate::core::search::query::{IntoQuery, Query};
use crate::core::search::similarities_impl::classic_similarity;
use crate::core::search::sort::Sort;
use crate::core::search::term_query::TermQuery;
use crate::core::search::top_field_collector_manager::TopFieldCollectorManager;
use crate::core::search::top_score_doc_collector_manager::TopScoreDocCollectorManager;
use crate::core::search::wildcard_query::WildcardQuery;
use crate::core::store::IOContext;
use crate::core::store::directory::{DirEnum, Directory};
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::search::boolean_query::{Callback, rand_bool_query};
use crate::test_framework::core::search::check_hits::CheckHits;
use crate::test_framework::core::search::query_utils::QueryUtils;
use crate::test_framework::core::util::DefaultIndexSearchCR;
use crate::test_framework::core::util::lucene_test_case::{
  at_least_usize, create_temp_dir, new_directory_shared, new_fs_directory,
  new_index_writer_config_with_analyzer, new_log_merge_policy, new_searcher, random,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::SeedableRng;
use rand::prelude::StdRng;
use rand::{Rng, RngExt};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::{Arc, LazyLock};

use parking_lot::RwLock;

#[allow(dead_code)] // for quick search
pub struct TestBoolean2;
const FIELD: &str = "field";
const NUM_EXTRA_DOCS: usize = 6000;
const DOC_FIELDS: [&str; 4] = [
  "w1 w2 w3 w4 w5",
  "w1 w3 w2 w3",
  "w1 xx w2 yy w3",
  "w1 w3 xx w2 yy mm",
];
pub struct TestBoolean2Context {
  pub searcher: DefaultIndexSearchCR,
  pub single_segment_searcher: DefaultIndexSearchCR,
  pub big_searcher: DefaultIndexSearchCR,
  pub mul_factor: i32,
  pub pre_filler_docs: usize,
  pub num_filler_docs: usize,
}
static CONTEXT: LazyLock<RwLock<TestBoolean2Context>> = LazyLock::new(|| {
  let mut random = random();
  RwLock::new(set_up(&mut random).expect("failed to initialize TestBoolean2"))
});
struct NoCallback;
impl Callback for NoCallback {
  fn post_create<R>(&self, _random: &mut R, _q: &mut Builder) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    Ok(())
  }
}
fn set_up<R>(random: &mut R) -> Result<TestBoolean2Context>
where
  R: Rng + ?Sized,
{
  let num_filler_docs = if random.random_bool(0.5) { 0 } else { SIZE };
  let pre_filler_docs = TestUtil::next_usize(random, 0, num_filler_docs / 2);

  if cfg!(feature = "test_log_verbose") {
    println!(
      "TEST: num_filler_docs={} pre_filler_docs={}",
      num_filler_docs, pre_filler_docs
    );
  }

  let directory = if num_filler_docs * pre_filler_docs > 100000 {
    new_fs_directory(random, create_temp_dir()?)?
  } else {
    new_directory_shared(random)?
  };

  let analyzer = MockAnalyzer::new(random);
  let mut iwc = new_index_writer_config_with_analyzer(random, analyzer)?;
  iwc.set_codec(TestUtil::get_default_codec());
  iwc.set_merge_policy(new_log_merge_policy(random)?);
  let writer = RandomIndexWriter::with_config(random, directory.clone(), iwc);
  let mut ft = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  ft.set_omit_norms(true)?;

  let mut doc = Document::new();
  for _ in 0..pre_filler_docs {
    writer.add_document(random, doc.clone())?;
  }
  for doc_field in &DOC_FIELDS {
    doc.add(Field::new(FIELD, *doc_field, ft.clone()));
    writer.add_document(random, doc.clone())?;

    doc = Document::new();
    for _ in 0..num_filler_docs {
      writer.add_document(random, doc.clone())?;
    }
  }

  writer.close(random)?;
  drop(writer);
  let little_reader = directory_reader::open(directory.clone())?;
  let mut searcher = new_searcher(random, little_reader)?;
  // this is intentionally using the baseline sim, because it compares against bigSearcher (which
  // uses a random one)
  searcher.set_similarity(classic_similarity::new());

  // make a copy of our index using a single segment
  let single_segment_directory = if num_filler_docs * pre_filler_docs > 100000 {
    new_fs_directory(random, create_temp_dir()?)?
  } else {
    new_directory_shared(random)?
  };

  // TODO: this test does not need to be doing this crazy stuff. please improve it!
  for file_name in directory.list_all()? {
    if file_name.starts_with("extra") {
      continue;
    }
    single_segment_directory.copy_from(
      directory.as_ref(),
      &file_name,
      &file_name,
      &IOContext::default_io_context()?,
    )?;
    single_segment_directory.sync(&[file_name])?;
  }

  let analyzer = MockAnalyzer::new(random);
  let mut iwc = new_index_writer_config_with_analyzer(random, analyzer)?;
  // we need docID order to be preserved:
  // randomized codecs are sometimes too costly for this test:
  iwc.set_codec(TestUtil::get_default_codec());
  iwc.set_merge_policy(new_log_merge_policy(random)?);
  {
    let w = IndexWriter::new(single_segment_directory.clone(), iwc)?;
    w.force_merge_with_wait(1, true)?;
    w.close()?;
  }

  let single_segment_reader = directory_reader::open(single_segment_directory.clone())?;
  let mut single_segment_searcher = new_searcher(random, single_segment_reader)?;
  single_segment_searcher.set_similarity(searcher.get_similarity());

  let dir2 = copy_of(random, directory.as_ref())?;

  // First multiply small test index:
  let mut mul_factor = 1;

  if cfg!(feature = "test_log_verbose") {
    println!("\nTEST: now copy index...");
  }

  loop {
    let copy = copy_of(random, dir2.as_ref())?;

    let analyzer = MockAnalyzer::new(random);
    let mut iwc = new_index_writer_config_with_analyzer(random, analyzer)?;
    // randomized codecs are sometimes too costly for this test:
    iwc.set_codec(TestUtil::get_default_codec());
    let w = RandomIndexWriter::with_config(random, dir2.clone(), iwc);

    w.add_indexes_from_dir(random, std::slice::from_ref(&copy))?;
    copy.close()?;
    let doc_count = w.get_doc_stats()?.max_doc as usize;
    w.close(random)?;
    mul_factor *= 2;
    if doc_count >= 3000 * num_filler_docs {
      break;
    }
  }

  let analyzer = MockAnalyzer::new(random);
  let mut iwc = new_index_writer_config_with_analyzer(random, analyzer)?;
  iwc.set_max_buffered_docs(TestUtil::next_int(random, 50, 1000));
  // randomized codecs are sometimes too costly for this test:
  iwc.set_codec(TestUtil::get_default_codec());
  let w = RandomIndexWriter::with_config(random, dir2.clone(), iwc);

  doc = Document::new();
  doc.add(Field::new("field2", "xxx", ft.clone()));
  for _ in 0..(NUM_EXTRA_DOCS / 2) {
    w.add_document(random, doc.clone())?;
  }

  doc = Document::new();
  doc.add(Field::new("field2", "big bad bug", ft.clone()));
  for _ in 0..(NUM_EXTRA_DOCS / 2) {
    w.add_document(random, doc.clone())?;
  }

  let reader = w.get_reader(random)?;
  let big_searcher = new_searcher(random, reader)?;
  w.close(random)?;
  Ok(TestBoolean2Context {
    searcher,
    single_segment_searcher,
    big_searcher,
    mul_factor,
    pre_filler_docs,
    num_filler_docs,
  })
}
fn copy_of<R, D>(random: &mut R, dir: &D) -> Result<Arc<DirEnum>>
where
  R: Rng + ?Sized,
  D: Directory,
{
  let copy = new_fs_directory(random, create_temp_dir()?)?;

  for name in dir.list_all()? {
    if name.starts_with("extra") {
      continue;
    }
    copy.copy_from(dir, &name, &name, &IOContext::default_io_context()?)?;
    copy.sync(&[name])?;
  }
  Ok(copy)
}
fn queries_test<R>(
  random: &mut R,
  ctx: &TestBoolean2Context,
  query: Query,
  exp_doc_nrs: &[i32],
) -> Result<()>
where
  R: Rng + ?Sized,
{
  let mut exp_doc_nrs = exp_doc_nrs.to_vec();

  if ctx.num_filler_docs > 0 {
    for doc in &mut exp_doc_nrs {
      *doc = ctx.pre_filler_docs as i32 + ((ctx.num_filler_docs as i32 + 1) * *doc);
    }
  }
  let top_docs_to_check = at_least_usize(random, 1000);

  let collector_manager = TopScoreDocCollectorManager::new(top_docs_to_check, i32::MAX as usize)?;
  let hits1 = ctx
    .searcher
    .search_with_collector_manager(query.clone(), &collector_manager)?
    .score_docs;

  let collector_manager = TopScoreDocCollectorManager::new(top_docs_to_check, i32::MAX as usize)?;
  let hits2 = ctx
    .searcher
    .search_with_collector_manager(query.clone(), &collector_manager)?
    .score_docs;

  CheckHits::check_hits_query(&query, &hits1, &hits2, &exp_doc_nrs)?;

  let collector_manager = TopScoreDocCollectorManager::new(top_docs_to_check, i32::MAX as usize)?;
  let top_docs = ctx
    .single_segment_searcher
    .search_with_collector_manager(query.clone(), &collector_manager)?;
  let hits2 = top_docs.score_docs.clone();

  CheckHits::check_hits_query(&query, &hits1, &hits2, &exp_doc_nrs)?;

  assert_eq!(
    ctx.mul_factor as usize * top_docs.total_hits.value(),
    ctx.big_searcher.count(query.clone())? as usize
  );

  let collector_manager = TopScoreDocCollectorManager::new(top_docs_to_check, i32::MAX as usize)?;
  let hits1 = ctx
    .big_searcher
    .search_with_collector_manager(query.clone(), &collector_manager)?
    .score_docs;

  let collector_manager = TopScoreDocCollectorManager::new(top_docs_to_check, i32::MAX as usize)?;
  let hits2 = ctx
    .big_searcher
    .search_with_collector_manager(query.clone(), &collector_manager)?
    .score_docs;

  CheckHits::check_equal(&query, &hits1, &hits2)?;
  Ok(())
}
#[test]
fn test_queries01() -> Result<()> {
  let mut random = random();
  let ctx = CONTEXT.read();

  let mut query = Builder::new();
  query.add(TermQuery::new(Term::from_text(FIELD, "w3")), Occur::Must)?;
  query.add(TermQuery::new(Term::from_text(FIELD, "xx")), Occur::Must)?;

  let exp_doc_nrs = [2, 3];
  queries_test(&mut random, &ctx, query.build().into(), &exp_doc_nrs)?;
  Ok(())
}
#[test]
fn test_queries02() -> Result<()> {
  let mut random = random();
  let ctx = CONTEXT.read();

  let mut query = Builder::new();
  query.add(TermQuery::new(Term::from_text(FIELD, "w3")), Occur::Must)?;
  query.add(TermQuery::new(Term::from_text(FIELD, "xx")), Occur::Should)?;

  let exp_doc_nrs = [2, 3, 1, 0];
  queries_test(&mut random, &ctx, query.build().into(), &exp_doc_nrs)?;
  Ok(())
}

#[test]
fn test_queries03() -> Result<()> {
  let mut random = random();
  let ctx = CONTEXT.read();

  let mut query = Builder::new();
  query.add(TermQuery::new(Term::from_text(FIELD, "w3")), Occur::Should)?;
  query.add(TermQuery::new(Term::from_text(FIELD, "xx")), Occur::Should)?;

  let exp_doc_nrs = [2, 3, 1, 0];
  queries_test(&mut random, &ctx, query.build().into(), &exp_doc_nrs)?;
  Ok(())
}

#[test]
fn test_queries04() -> Result<()> {
  let mut random = random();
  let ctx = CONTEXT.read();

  let mut query = Builder::new();
  query.add(TermQuery::new(Term::from_text(FIELD, "w3")), Occur::Should)?;
  query.add(TermQuery::new(Term::from_text(FIELD, "xx")), Occur::MustNot)?;

  let exp_doc_nrs = [1, 0];
  queries_test(&mut random, &ctx, query.build().into(), &exp_doc_nrs)?;
  Ok(())
}

#[test]
fn test_queries05() -> Result<()> {
  let mut random = random();
  let ctx = CONTEXT.read();

  let mut query = Builder::new();
  query.add(TermQuery::new(Term::from_text(FIELD, "w3")), Occur::Must)?;
  query.add(TermQuery::new(Term::from_text(FIELD, "xx")), Occur::MustNot)?;

  let exp_doc_nrs = [1, 0];
  queries_test(&mut random, &ctx, query.build().into(), &exp_doc_nrs)?;
  Ok(())
}

#[test]
fn test_queries06() -> Result<()> {
  let mut random = random();
  let ctx = CONTEXT.read();

  let mut query = Builder::new();
  query.add(TermQuery::new(Term::from_text(FIELD, "w3")), Occur::Must)?;
  query.add(TermQuery::new(Term::from_text(FIELD, "xx")), Occur::MustNot)?;
  query.add(TermQuery::new(Term::from_text(FIELD, "w5")), Occur::MustNot)?;

  let exp_doc_nrs = [1];
  queries_test(&mut random, &ctx, query.build().into(), &exp_doc_nrs)?;
  Ok(())
}

#[test]
fn test_queries07() -> Result<()> {
  let mut random = random();
  let ctx = CONTEXT.read();

  let mut query = Builder::new();
  query.add(TermQuery::new(Term::from_text(FIELD, "w3")), Occur::MustNot)?;
  query.add(TermQuery::new(Term::from_text(FIELD, "xx")), Occur::MustNot)?;
  query.add(TermQuery::new(Term::from_text(FIELD, "w5")), Occur::MustNot)?;

  let exp_doc_nrs: [i32; 0] = [];
  queries_test(&mut random, &ctx, query.build().into(), &exp_doc_nrs)?;
  Ok(())
}

#[test]
fn test_queries08() -> Result<()> {
  let mut random = random();
  let ctx = CONTEXT.read();

  let mut query = Builder::new();
  query.add(TermQuery::new(Term::from_text(FIELD, "w3")), Occur::Must)?;
  query.add(TermQuery::new(Term::from_text(FIELD, "xx")), Occur::Should)?;
  query.add(TermQuery::new(Term::from_text(FIELD, "w5")), Occur::MustNot)?;

  let exp_doc_nrs = [2, 3, 1];
  queries_test(&mut random, &ctx, query.build().into(), &exp_doc_nrs)?;
  Ok(())
}

#[test]
fn test_queries09() -> Result<()> {
  let mut random = random();
  let ctx = CONTEXT.read();

  let mut query = Builder::new();
  query.add(TermQuery::new(Term::from_text(FIELD, "w3")), Occur::Must)?;
  query.add(TermQuery::new(Term::from_text(FIELD, "xx")), Occur::Must)?;
  query.add(TermQuery::new(Term::from_text(FIELD, "w2")), Occur::Must)?;
  query.add(TermQuery::new(Term::from_text(FIELD, "zz")), Occur::Should)?;

  let exp_doc_nrs = [2, 3];
  queries_test(&mut random, &ctx, query.build().into(), &exp_doc_nrs)?;
  Ok(())
}
#[test]
fn test_random_queries() -> Result<()> {
  let mut random = random();
  let mut ctx = CONTEXT.write();
  let vals: Vec<String> = ["w1", "w2", "w3", "w4", "w5", "xx", "yy", "zzz"]
    .into_iter()
    .map(str::to_string)
    .collect();
  let mut q1: Option<Query> = None;

  let result = (|| -> Result<()> {
    let num = at_least_usize(&mut random, 3);
    for _ in 0..num {
      let level = random.random_range(0..3);
      let mut nested_random = StdRng::seed_from_u64(random.random::<u64>());
      let built = rand_bool_query(
        &mut nested_random,
        random.random_bool(0.5),
        level,
        FIELD,
        &vals,
        None::<&NoCallback>,
      )?
      .build();
      let query: Query = built.into();
      q1 = Some(query.clone());

      let sort = Sort::get_index_order()?;

      QueryUtils::check_from_searcher(&mut random, query.clone(), &ctx.searcher)?;
      let baseline_similarity = ctx.searcher.get_similarity();
      let random_similarity = ctx.big_searcher.get_similarity();
      ctx.searcher.set_similarity(random_similarity);
      let random_check_result = catch_unwind(AssertUnwindSafe(|| {
        QueryUtils::check_from_searcher(&mut random, query.clone(), &ctx.searcher)
      }));
      ctx.searcher.set_similarity(baseline_similarity);
      match random_check_result {
        Ok(result) => result?,
        Err(payload) => resume_unwind(payload),
      }

      let cm = TopFieldCollectorManager::new(sort.clone(), 1000, 1)?;
      let hits1 = ctx
        .searcher
        .search_with_collector_manager(query.clone(), &cm)?;
      let cm = TopFieldCollectorManager::new(sort.clone(), 1000, 1)?;
      let top_docs = ctx
        .searcher
        .search_with_collector_manager(query.clone(), &cm)?;
      let hits2 = top_docs.base.score_docs.clone();
      CheckHits::check_equal(&query, &hits1.base.score_docs, &hits2)?;

      let mut q3 = Builder::new();
      q3.add(query.clone(), Occur::Should)?;
      q3.add(
        PrefixQuery::new(Term::from_text("field2", "b"))?,
        Occur::Should,
      )?;
      assert_eq!(
        ctx.mul_factor as usize * top_docs.base.total_hits.value() + NUM_EXTRA_DOCS / 2,
        ctx.big_searcher.count(q3.build())? as usize
      );

      let cm = TopFieldCollectorManager::new(sort.clone(), ctx.mul_factor as usize, 1)?;
      let hits1 = ctx
        .big_searcher
        .search_with_collector_manager(query.clone(), &cm)?;
      let cm = TopFieldCollectorManager::new(sort.clone(), ctx.mul_factor as usize, 1)?;
      let hits2 = ctx
        .big_searcher
        .search_with_collector_manager(query.clone(), &cm)?;
      CheckHits::check_equal(&query, &hits1.base.score_docs, &hits2.base.score_docs)?;
    }
    Ok(())
  })();

  if let Err(e) = result {
    if let Some(query) = q1 {
      println!("failed query: {:?}", query);
    }
    return Err(e);
  }

  Ok(())
}
