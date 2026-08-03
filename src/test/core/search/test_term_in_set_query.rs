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
use crate::core::document::keyword_field::KeywordField;
use crate::core::document::sorted_set_doc_values_field::SortedSetDocValuesField;
use crate::core::document::string_field::StringField;
use crate::core::index::BytesRef;
use crate::core::index::automaton_terms_enum::AutomatonTermsEnum;
use crate::core::index::base_composite_reader::{
  BCRStoredFieldsImpl, BCRTermVectorsImpl, BaseCompositeReader, BaseCompositeReaderBase,
};
use crate::core::index::composite_reader::CompositeReader;
use crate::core::index::directory_reader::{DirectoryReader, DirectoryReaderBase};
use crate::core::index::filter_directory_reader::{FilterDirectoryReader, SubReaderWrapper};
use crate::core::index::filtered_terms_enum::FilteredTermsEnum;
use crate::core::index::index_reader::{
  CompositeReaderContextKind, IndexReader, IndexReaderBase, LeafReaderContextKind,
};
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::leaf_metadata::LeafMetaData;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::term::Term;
use crate::core::index::terms::Terms;
use crate::core::search::abstract_multi_term_query_constant_score_wrapper::BOOLEAN_REWRITE_TERM_COUNT_THRESHOLD;
use crate::core::search::boolean_clause::Occur;
use crate::core::search::boolean_query::Builder as BooleanQueryBuilder;
use crate::core::search::boost_query::BoostQuery;
use crate::core::search::constant_score_query::ConstantScoreQuery;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::search::multi_term_query::DOC_VALUES_REWRITE;
use crate::core::search::query::{IntoQuery, Query, QueryBase};
use crate::core::search::query_caching_policy::QueryCachingPolicy;
use crate::core::search::score_doc::ScoreDocLike;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::sort::Sort;
use crate::core::search::term_in_set_query::TermInSetQuery;
use crate::core::search::term_query::TermQuery;
use crate::core::search::top_docs::TopDocsLike;
use crate::core::search::usage_tracking_query_caching_policy::UsageTrackingQueryCachingPolicy;
use crate::core::util::accountable::Accountable;
use crate::core::util::automation::compiled_automaton::CompiledAutomaton;
use crate::core::util::bits::Bits;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::close::CloseableRef;
use crate::core::util::core_helper::CoreHelper;
use crate::core::util::dummy::dummy_comparator::DummyComparator;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::io_utils::IOUtils;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::search::query_utils::QueryUtils;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, new_bytes_ref_from_bytes, new_bytes_ref_from_string, new_directory_shared,
  new_searcher, new_searcher_with_reader, random,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::RngExt;
use rand::seq::SliceRandom;
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

#[allow(dead_code)] // for quick search
struct TestTermInSetQuery;

#[test]
fn test_all_docs_in_field_term() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = RandomIndexWriter::new(&mut random, dir.clone())?;
  let field = "f";

  let dense_term_string = TestUtil::random_analysis_string(&mut random, 10, true);
  let dense_term = new_bytes_ref_from_string(&mut random, &dense_term_string)?;

  let mut random_terms = HashSet::new();
  while random_terms.len() < BOOLEAN_REWRITE_TERM_COUNT_THRESHOLD {
    let term_string = TestUtil::random_analysis_string(&mut random, 10, true);
    random_terms.insert(new_bytes_ref_from_string(&mut random, &term_string)?);
  }
  assert_eq!(BOOLEAN_REWRITE_TERM_COUNT_THRESHOLD, random_terms.len());
  let other_terms: Vec<_> = random_terms.iter().cloned().collect();

  let num_docs = 10 * other_terms.len();
  for i in 0..num_docs {
    let mut doc = Document::new();
    doc.add(StringField::from_bytes_ref(
      field,
      dense_term.clone(),
      Store::No,
    )?);
    let sparse_term = other_terms[i % other_terms.len()].clone();
    doc.add(StringField::from_bytes_ref(field, sparse_term, Store::No)?);
    writer.add_document(&mut random, doc)?;
  }

  for _ in 0..100 {
    let mut doc = Document::new();
    doc.add(StringField::from_string("foo", "bar", Store::No)?);
  }

  let reader = writer.get_reader(&mut random)?;
  let searcher = new_searcher(&mut random, reader)?;

  let mut query_terms = other_terms;
  query_terms.push(dense_term);

  let query = TermInSetQuery::new(field, query_terms)?;
  let top_docs = searcher.search(query, num_docs)?;
  assert_eq!(num_docs, top_docs.total_hits().value());

  writer.close(&mut random)?;
  Ok(())
}

#[test]
fn test_duel() -> Result<()> {
  let mut random = random();
  let iters = at_least(&mut random, 2);
  let field = "f";
  for _ in 0..iters {
    let mut all_terms = Vec::new();
    let max_terms_power = TestUtil::next_int(&mut random, 1, 10);
    let num_terms = TestUtil::next_int(&mut random, 1, 1 << max_terms_power);
    for _ in 0..num_terms {
      let value = TestUtil::random_analysis_string(&mut random, 10, true);
      all_terms.push(new_bytes_ref_from_string(&mut random, &value)?);
    }
    let dir = new_directory_shared(&mut random)?;
    let writer = RandomIndexWriter::new(&mut random, dir.clone())?;
    let num_docs = at_least(&mut random, 10_000);
    for _ in 0..num_docs {
      let mut doc = Document::new();
      let term = all_terms[random.random_range(0..all_terms.len())].clone();
      doc.add(StringField::from_bytes_ref(field, term.clone(), Store::No)?);
      doc.add(SortedSetDocValuesField::indexed_field(field, term));
      writer.add_document(&mut random, doc)?;
    }
    if num_terms > 1 && random.random_bool(0.5) {
      writer.delete_documents_with_queries(
        &mut random,
        vec![TermQuery::new(Term::new(field, all_terms[0].clone())).into()],
      )?;
    }
    writer.commit(&mut random)?;
    let reader = writer.get_reader(&mut random)?;
    let searcher = new_searcher(&mut random, reader)?;
    writer.close(&mut random)?;

    if searcher.get_index_reader().num_docs()? == 0 {
      // may occasionally happen if all documents got the same term
      IOUtils::use_or_suppress_result(searcher.get_index_reader().close(), dir.close())?;
      continue;
    }

    for _ in 0..100 {
      let boost = random.random::<f32>() * 10.0;
      let max_query_terms_power = TestUtil::next_int(&mut random, 1, 8);
      let num_query_terms = TestUtil::next_int(&mut random, 1, 1 << max_query_terms_power);
      let mut query_terms = Vec::new();
      for _ in 0..num_query_terms {
        query_terms.push(all_terms[random.random_range(0..all_terms.len())].clone());
      }
      let mut bq = BooleanQueryBuilder::new();
      for term in &query_terms {
        bq.add(
          TermQuery::new(Term::new(field, term.clone())),
          Occur::Should,
        )?;
      }
      let q1: Query = ConstantScoreQuery::new(bq.build()).into();
      let q2: Query = TermInSetQuery::new(field, query_terms.clone())?.into_query();
      let q3: Query =
        TermInSetQuery::new_with_rewrite_method(DOC_VALUES_REWRITE, field, query_terms)?
          .into_query();
      assert_same_matches(
        &searcher,
        BoostQuery::new(q1.clone(), boost)?.into(),
        BoostQuery::new(q2, boost)?.into(),
        true,
      )?;
      assert_same_matches(
        &searcher,
        BoostQuery::new(q1, boost)?.into(),
        BoostQuery::new(q3, boost)?.into(),
        false,
      )?;
    }

    searcher.get_index_reader().close()?;
    dir.close()?;
  }
  Ok(())
}

#[test]
fn test_returns_null_score_supplier() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = RandomIndexWriter::new(&mut random, dir)?;
  for ch in 'a'..='z' {
    let mut doc = Document::new();
    let value = ch.to_string();
    doc.add(KeywordField::from_string("id", value.clone(), Store::Yes)?);
    doc.add(KeywordField::from_string("content", value, Store::Yes)?);
    writer.add_document(&mut random, doc)?;
  }
  let reader = writer.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;
  writer.close(&mut random)?;

  let mut terms = Vec::new();
  for ch in 'a'..='z' {
    terms.push(new_bytes_ref_from_string(&mut random, &ch.to_string())?);
  }
  let query2: Query = TermInSetQuery::new("content", terms)?.into_query();

  {
    let query1: Query = TermInSetQuery::new(
      "id",
      vec![
        new_bytes_ref_from_string(&mut random, "aaa")?,
        new_bytes_ref_from_string(&mut random, "bbb")?,
      ],
    )?
    .into_query();
    let mut query_builder = BooleanQueryBuilder::new();
    query_builder.add(query1.clone(), Occur::Filter)?;
    query_builder.add(query2.clone(), Occur::Filter)?;
    let bool_query: Query = query_builder.build().into();

    let ctx = &searcher.get_leaf_contexts()?[0];
    let rewritten_query1 = searcher.rewrite(query1)?;
    let weight1 = searcher.create_weight(rewritten_query1, ScoreMode::Complete, 1.0)?;
    let scorer_supplier1 = weight1.scorer_supplier(ctx, &searcher)?;
    assert!(scorer_supplier1.is_none());
    let rewritten_bool_query = searcher.rewrite(bool_query)?;
    let weight = searcher.create_weight(rewritten_bool_query, ScoreMode::Complete, 1.0)?;
    let scorer_supplier = weight.scorer_supplier(ctx, &searcher)?;
    assert!(scorer_supplier.is_none());
  }

  {
    let query1: Query = TermInSetQuery::new(
      "id",
      vec![
        new_bytes_ref_from_string(&mut random, "aaa")?,
        new_bytes_ref_from_string(&mut random, "bbb")?,
        new_bytes_ref_from_string(&mut random, "b")?,
      ],
    )?
    .into_query();
    let mut query_builder = BooleanQueryBuilder::new();
    query_builder.add(query1.clone(), Occur::Filter)?;
    query_builder.add(query2, Occur::Filter)?;
    let bool_query: Query = query_builder.build().into();

    let ctx = &searcher.get_leaf_contexts()?[0];
    let rewritten_query1 = searcher.rewrite(query1)?;
    let weight1 = searcher.create_weight(rewritten_query1, ScoreMode::Complete, 1.0)?;
    let scorer_supplier1 = weight1.scorer_supplier(ctx, &searcher)?;
    assert!(scorer_supplier1.is_some());
    let rewritten_bool_query = searcher.rewrite(bool_query)?;
    let weight = searcher.create_weight(rewritten_bool_query, ScoreMode::Complete, 1.0)?;
    let scorer_supplier = weight.scorer_supplier(ctx, &searcher)?;
    assert!(scorer_supplier.is_some());
  }

  Ok(())
}

#[test]
fn test_skipper_optimization_gap_assumption() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = RandomIndexWriter::new(&mut random, dir)?;
  for _ in 0..10_000 {
    let mut doc = Document::new();
    let term = new_bytes_ref_from_string(&mut random, "b")?;
    doc.add(SortedSetDocValuesField::new("field", term.clone()));
    doc.add(SortedSetDocValuesField::indexed_field("idx_field", term));
    writer.add_document(&mut random, doc)?;
  }

  let mut doc = Document::new();
  let term = new_bytes_ref_from_string(&mut random, "a")?;
  doc.add(SortedSetDocValuesField::new("field", term.clone()));
  doc.add(SortedSetDocValuesField::indexed_field("idx_field", term));
  writer.add_document(&mut random, doc)?;

  let mut doc = Document::new();
  let term = new_bytes_ref_from_string(&mut random, "c")?;
  doc.add(SortedSetDocValuesField::new("field", term.clone()));
  doc.add(SortedSetDocValuesField::indexed_field("idx_field", term));
  writer.add_document(&mut random, doc)?;

  writer.commit(&mut random)?;
  let reader = writer.get_reader(&mut random)?;
  let searcher = new_searcher(&mut random, reader)?;
  writer.close(&mut random)?;

  let query_terms = vec![
    new_bytes_ref_from_string(&mut random, "a")?,
    new_bytes_ref_from_string(&mut random, "c")?,
  ];
  let q1: Query =
    TermInSetQuery::new_with_rewrite_method(DOC_VALUES_REWRITE, "field", query_terms.clone())?
      .into_query();
  let q2: Query =
    TermInSetQuery::new_with_rewrite_method(DOC_VALUES_REWRITE, "idx_field", query_terms)?
      .into_query();
  assert_same_matches(&searcher, q1, q2, false)?;

  Ok(())
}

fn assert_same_matches<IRC>(
  searcher: &IndexSearcher<IRC>,
  q1: Query,
  q2: Query,
  scores: bool,
) -> Result<()>
where
  IRC: IndexReaderContext + Sync,
{
  let max_doc = searcher.get_index_reader().max_doc()? as usize;
  if scores {
    let td1 = searcher.search(q1, max_doc)?;
    let td2 = searcher.search(q2, max_doc)?;
    assert_eq!(td1.total_hits().value(), td2.total_hits().value());
    for i in 0..td1.score_docs.len() {
      assert_eq!(td1.score_docs[i].doc, td2.score_docs[i].doc);
      assert!(
        (td1.score_docs[i].score - td2.score_docs[i].score).abs() <= 10e-7,
        "score for {i} was not the same"
      );
    }
  } else {
    let td1 = searcher.search_with_sort(q1, max_doc, Sort::get_index_order()?)?;
    let td2 = searcher.search_with_sort(q2, max_doc, Sort::get_index_order()?)?;
    assert_eq!(td1.total_hits().value(), td2.total_hits().value());
    for i in 0..td1.score_docs().len() {
      assert_eq!(td1.score_docs()[i].doc(), td2.score_docs()[i].doc());
    }
  }
  Ok(())
}

#[test]
fn test_hash_code_and_equals() -> Result<()> {
  let mut random = random();
  let num = at_least(&mut random, 100);
  let mut terms = Vec::new();
  let mut unique_terms = HashSet::new();
  for _ in 0..num {
    let string = TestUtil::random_realistic_unicode_string(&mut random);
    terms.push(new_bytes_ref_from_string(&mut random, &string)?);
    unique_terms.insert(new_bytes_ref_from_string(&mut random, &string)?);
    let left = TermInSetQuery::new("field", unique_terms.iter().cloned().collect())?;
    terms.shuffle(&mut random);
    let right = TermInSetQuery::new("field", terms.clone())?;
    assert_eq!(right, left);
    assert_eq!(
      CoreHelper::calculate_hash(&right),
      CoreHelper::calculate_hash(&left)
    );
    if unique_terms.len() > 1 {
      let mut as_list: Vec<_> = unique_terms.iter().cloned().collect();
      as_list.remove(0);
      let not_equal = TermInSetQuery::new("field", as_list)?;
      assert_ne!(left, not_equal);
      assert_ne!(right, not_equal);
    }
  }

  let mut tq1 = TermInSetQuery::new(
    "thing",
    vec![new_bytes_ref_from_string(&mut random, "apple")?],
  )?;
  let mut tq2 = TermInSetQuery::new(
    "thing",
    vec![new_bytes_ref_from_string(&mut random, "orange")?],
  )?;
  assert_ne!(
    CoreHelper::calculate_hash(&tq1),
    CoreHelper::calculate_hash(&tq2)
  );

  tq1 = TermInSetQuery::new(
    "thing",
    vec![new_bytes_ref_from_string(&mut random, "apple")?],
  )?;
  tq2 = TermInSetQuery::new(
    "thing2",
    vec![new_bytes_ref_from_string(&mut random, "apple")?],
  )?;
  assert_ne!(
    CoreHelper::calculate_hash(&tq1),
    CoreHelper::calculate_hash(&tq2)
  );
  Ok(())
}

#[test]
fn test_simple_equals() -> Result<()> {
  let mut random = random();
  let left = TermInSetQuery::new(
    "id",
    vec![
      new_bytes_ref_from_string(&mut random, "AaAaAa")?,
      new_bytes_ref_from_string(&mut random, "AaAaBB")?,
    ],
  )?;
  let right = TermInSetQuery::new(
    "id",
    vec![
      new_bytes_ref_from_string(&mut random, "AaAaAa")?,
      new_bytes_ref_from_string(&mut random, "BBBBBB")?,
    ],
  )?;
  assert_ne!(left, right);
  Ok(())
}

#[test]
fn test_to_string() -> Result<()> {
  let mut random = random();
  let terms_query = TermInSetQuery::new(
    "field1",
    vec![
      new_bytes_ref_from_string(&mut random, "a")?,
      new_bytes_ref_from_string(&mut random, "b")?,
      new_bytes_ref_from_string(&mut random, "c")?,
    ],
  )?;
  assert_eq!("field1:(a b c)", terms_query.to_string("")?);
  Ok(())
}

#[test]
fn test_dedup() -> Result<()> {
  let mut random = random();
  let query1 = TermInSetQuery::new("foo", vec![new_bytes_ref_from_string(&mut random, "bar")?])?;
  let query2 = TermInSetQuery::new(
    "foo",
    vec![
      new_bytes_ref_from_string(&mut random, "bar")?,
      new_bytes_ref_from_string(&mut random, "bar")?,
    ],
  )?;
  QueryUtils::check_equal(&query1, &query2);
  Ok(())
}

#[test]
fn test_order_does_not_matter() -> Result<()> {
  let mut random = random();
  let query1 = TermInSetQuery::new(
    "foo",
    vec![
      new_bytes_ref_from_string(&mut random, "bar")?,
      new_bytes_ref_from_string(&mut random, "baz")?,
    ],
  )?;
  let query2 = TermInSetQuery::new(
    "foo",
    vec![
      new_bytes_ref_from_string(&mut random, "baz")?,
      new_bytes_ref_from_string(&mut random, "bar")?,
    ],
  )?;
  QueryUtils::check_equal(&query1, &query2);
  Ok(())
}

#[test]
fn test_ram_bytes_used() -> Result<()> {
  let mut random = random();
  let num_terms = 10_000 + random.random_range(0..1000);
  let mut terms = Vec::with_capacity(num_terms);
  for i in 0..num_terms {
    terms.push(new_bytes_ref_from_string(
      &mut random,
      &format!("term{:05}", i),
    )?);
  }
  let query = TermInSetQuery::new("f", terms)?;
  let ram_bytes_used = query.ram_bytes_used()?;

  let one_term_query =
    TermInSetQuery::new("f", vec![new_bytes_ref_from_string(&mut random, "term")?])?;

  assert!(ram_bytes_used > 0);
  // TODO: Restore Java's reflection-based size comparison after a Rust RamUsageTester equivalent
  // is available. The retained-heap invariants that Rust can currently express are checked above
  // and below.
  assert!(ram_bytes_used > one_term_query.ram_bytes_used()?);
  assert_eq!(ram_bytes_used, query.ram_bytes_used()?);
  Ok(())
}

struct TermsCountingDirectoryReaderWrapper<DR>
where
  DR: DirectoryReader,
{
  in_: DR,
  counter: Arc<AtomicI32>,
  base: BaseCompositeReaderBase<TermsCountingLeafReaderWrapper<DR::LeafReader>>,
  index_base: IndexReaderBase,
}

impl<DR> TermsCountingDirectoryReaderWrapper<DR>
where
  DR: DirectoryReader,
{
  fn new(in_: DR, counter: Arc<AtomicI32>) -> Result<Self> {
    let wrapper = TermsCountingSubReaderWrapper::new(counter.clone());
    let readers = wrapper.wrap_readers(in_.get_sequential_sub_readers().to_vec())?;
    let index_base = IndexReaderBase::new();
    let base = BaseCompositeReaderBase::new::<DummyComparator>(readers, None, &index_base)?;
    Ok(Self {
      in_,
      counter,
      base,
      index_base,
    })
  }
}

struct TermsCountingSubReaderWrapper {
  counter: Arc<AtomicI32>,
}

impl TermsCountingSubReaderWrapper {
  fn new(counter: Arc<AtomicI32>) -> Self {
    Self { counter }
  }
}

impl<LR> SubReaderWrapper<LR> for TermsCountingSubReaderWrapper
where
  LR: LeafReader,
{
  type LeafReader1 = Self::LeafReader2;

  fn wrap_readers(&self, readers: Vec<LR>) -> Result<Vec<Self::LeafReader1>> {
    self.default_wrap_readers(readers)
  }

  type LeafReader2 = TermsCountingLeafReaderWrapper<LR>;

  fn wrap(&self, reader: LR) -> Result<Self::LeafReader2> {
    TermsCountingLeafReaderWrapper::new(reader, self.counter.clone())
  }
}

struct TermsCountingLeafReaderWrapper<LR>
where
  LR: LeafReader,
{
  in_: LR,
  counter: Arc<AtomicI32>,
  index_base: IndexReaderBase,
}

impl<LR> TermsCountingLeafReaderWrapper<LR>
where
  LR: LeafReader,
{
  fn new(in_: LR, counter: Arc<AtomicI32>) -> Result<Self> {
    let index_base = IndexReaderBase::new();
    in_.register_parent_reader(&index_base)?;
    Ok(Self {
      in_,
      counter,
      index_base,
    })
  }
}

impl<LR> Clone for TermsCountingLeafReaderWrapper<LR>
where
  LR: LeafReader + Clone,
{
  fn clone(&self) -> Self {
    Self {
      in_: self.in_.clone(),
      counter: self.counter.clone(),
      index_base: self.index_base.clone(),
    }
  }
}

impl<LR> Display for TermsCountingLeafReaderWrapper<LR>
where
  LR: LeafReader,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "TermsCountingLeafReaderWrapper({})", self.in_)
  }
}

impl<LR> IndexReader for TermsCountingLeafReaderWrapper<LR>
where
  LR: LeafReader,
{
  type ContextKind = LeafReaderContextKind;
  type TermVectors = LR::TermVectors;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    self.in_.term_vectors()
  }

  fn max_doc(&self) -> Result<i32> {
    self.in_.max_doc()
  }

  fn num_docs(&self) -> Result<i32> {
    self.in_.num_docs()
  }

  type StoredFields = LR::StoredFields;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    self.in_.stored_fields()
  }

  fn do_close(&self) -> Result<()> {
    self.in_.close()
  }

  type ReaderCacheHelper = LR::ReaderCacheHelper;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    Ok(None)
  }

  fn doc_freq(&self, term: &Term) -> Result<i32> {
    IndexReader::doc_freq(&self.in_, term)
  }

  fn total_term_freq(&self, term: &Term) -> Result<i64> {
    self.in_.total_term_freq(term)
  }

  fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
    IndexReader::get_sum_doc_freq(&self.in_, field)
  }

  fn get_doc_count(&self, field: &str) -> Result<i32> {
    IndexReader::get_doc_count(&self.in_, field)
  }

  fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
    IndexReader::get_sum_total_term_freq(&self.in_, field)
  }

  fn index_base(&self) -> &IndexReaderBase {
    &self.index_base
  }
}

impl<LR> LeafReader for TermsCountingLeafReaderWrapper<LR>
where
  LR: LeafReader,
{
  type CacheHelper = LR::CacheHelper;

  fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
    Ok(None)
  }

  type Terms = TermsCountingTerms<LR::Terms>;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    Ok(
      self
        .in_
        .terms(field)?
        .map(|terms| TermsCountingTerms::new(terms, self.counter.clone())),
    )
  }

  type NumericDocValues = LR::NumericDocValues;

  fn get_numeric_doc_values(&self, field: &str) -> Result<Option<Self::NumericDocValues>> {
    self.in_.get_numeric_doc_values(field)
  }

  type BinaryDocValues = LR::BinaryDocValues;

  fn get_binary_doc_values(&self, field: &str) -> Result<Option<Self::BinaryDocValues>> {
    self.in_.get_binary_doc_values(field)
  }

  type SortedDocValues = LR::SortedDocValues;

  fn get_sorted_doc_values(&self, field: &str) -> Result<Option<Self::SortedDocValues>> {
    self.in_.get_sorted_doc_values(field)
  }

  type SortedNumericDocValues = LR::SortedNumericDocValues;

  fn get_sorted_numeric_doc_values(
    &self,
    field: &str,
  ) -> Result<Option<Self::SortedNumericDocValues>> {
    self.in_.get_sorted_numeric_doc_values(field)
  }

  type SortedSetDocValues = LR::SortedSetDocValues;

  fn get_sorted_set_doc_values(&self, field: &str) -> Result<Option<Self::SortedSetDocValues>> {
    self.in_.get_sorted_set_doc_values(field)
  }

  type NormNumericDocValues = LR::NormNumericDocValues;

  fn get_norm_values(&self, field: &str) -> Result<Option<Self::NormNumericDocValues>> {
    self.in_.get_norm_values(field)
  }

  type DocValuesSkipper = LR::DocValuesSkipper;

  fn get_doc_values_skipper(&self, field: &str) -> Result<Option<Self::DocValuesSkipper>> {
    self.in_.get_doc_values_skipper(field)
  }

  type FloatVectorValues = LR::FloatVectorValues;

  fn get_float_vector_values(&self, field: &str) -> Result<Option<Self::FloatVectorValues>> {
    self.in_.get_float_vector_values(field)
  }

  type ByteVectorValues = LR::ByteVectorValues;

  fn get_byte_vector_values(&self, field: &str) -> Result<Option<Self::ByteVectorValues>> {
    self.in_.get_byte_vector_values(field)
  }

  fn search_nearest_vectors_f32<B, K>(
    &self,
    field: &str,
    target: Vec<f32>,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
  {
    self
      .in_
      .search_nearest_vectors_f32(field, target, knn_collector, accept_docs)
  }

  fn search_nearest_vectors_u8<B, K>(
    &self,
    field: &str,
    target: Vec<u8>,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
  {
    self
      .in_
      .search_nearest_vectors_u8(field, target, knn_collector, accept_docs)
  }

  fn get_field_infos(&self) -> Result<Arc<crate::core::index::field_infos::FieldInfos>> {
    self.in_.get_field_infos()
  }

  type Bits = LR::Bits;

  fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
    self.in_.get_live_docs()
  }

  type PointValues = LR::PointValues;

  fn get_point_values(&self, field: &str) -> Result<Option<Self::PointValues>> {
    self.in_.get_point_values(field)
  }

  fn check_integrity(&self) -> Result<()> {
    self.in_.check_integrity()
  }

  fn get_metadata(&self) -> Result<&LeafMetaData> {
    self.in_.get_metadata()
  }
}

struct TermsCountingTerms<T>
where
  T: Terms,
{
  in_: T,
  counter: Arc<AtomicI32>,
}

impl<T> TermsCountingTerms<T>
where
  T: Terms,
{
  fn new(in_: T, counter: Arc<AtomicI32>) -> Self {
    Self { in_, counter }
  }
}

impl<T> Terms for TermsCountingTerms<T>
where
  T: Terms,
{
  type TermsEnum = T::TermsEnum;

  fn iterator(&self) -> Result<Self::TermsEnum> {
    self.counter.fetch_add(1, Ordering::SeqCst);
    self.in_.iterator()
  }

  type IntersectIter = FilteredTermsEnum<T::TermsEnum, AutomatonTermsEnum>;

  fn intersect(
    &self,
    compiled: &CompiledAutomaton,
    start_term: Option<&BytesRef<Vec<u8>>>,
  ) -> Result<Self::IntersectIter> {
    self.default_intersect(compiled, start_term)
  }

  fn size(&self) -> Result<i64> {
    self.in_.size()
  }

  fn get_sum_total_term_freq(&self) -> Result<i64> {
    self.in_.get_sum_total_term_freq()
  }

  fn get_sum_doc_freq(&self) -> Result<i64> {
    self.in_.get_sum_doc_freq()
  }

  fn get_doc_count(&self) -> Result<i32> {
    self.in_.get_doc_count()
  }

  fn has_freqs(&self) -> bool {
    self.in_.has_freqs()
  }

  fn has_offsets(&self) -> bool {
    self.in_.has_offsets()
  }

  fn has_positions(&self) -> bool {
    self.in_.has_positions()
  }

  fn has_payloads(&self) -> bool {
    self.in_.has_payloads()
  }

  fn get_stats(&self) -> Result<String> {
    self.in_.get_stats()
  }
}

impl<DR> BaseCompositeReader for TermsCountingDirectoryReaderWrapper<DR> where DR: DirectoryReader {}

impl<DR> CompositeReader for TermsCountingDirectoryReaderWrapper<DR>
where
  DR: DirectoryReader,
{
  type LeafReader = TermsCountingLeafReaderWrapper<DR::LeafReader>;
  type SubReader = Self::LeafReader;

  fn get_sequential_sub_readers(&self) -> &[Self::SubReader] {
    self.base.get_sequential_sub_readers()
  }

  fn visit_leaves<F>(&self, visitor: &mut F) -> Result<()>
  where
    F: FnMut(&Self::LeafReader) -> Result<()>,
  {
    for reader in self.get_sequential_sub_readers() {
      visitor(reader)?;
    }
    Ok(())
  }

  fn to_string(&self) -> String {
    format!(
      "TermsCountingDirectoryReaderWrapper({})",
      self.in_.to_string()
    )
  }
}

impl<DR> IndexReader for TermsCountingDirectoryReaderWrapper<DR>
where
  DR: DirectoryReader,
{
  type ContextKind = CompositeReaderContextKind;
  type TermVectors = BCRTermVectorsImpl<<Self as CompositeReader>::LeafReader>;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    self.base.term_vector(self)
  }

  fn max_doc(&self) -> Result<i32> {
    Ok(self.base.max_doc())
  }

  fn num_docs(&self) -> Result<i32> {
    self.base.num_docs()
  }

  type StoredFields = BCRStoredFieldsImpl<<Self as CompositeReader>::LeafReader>;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    self.base.stored_fields(self)
  }

  fn do_close(&self) -> Result<()> {
    self.in_.close()
  }

  type ReaderCacheHelper = DR::ReaderCacheHelper;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    Ok(None)
  }

  fn doc_freq(&self, term: &Term) -> Result<i32> {
    self.base.doc_freq(term, self)
  }

  fn total_term_freq(&self, term: &Term) -> Result<i64> {
    self.base.total_term_freq(term, self)
  }

  fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
    self.base.get_sum_doc_freq(field, self)
  }

  fn get_doc_count(&self, field: &str) -> Result<i32> {
    self.base.get_doc_count(field, self)
  }

  fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
    self.base.get_sum_total_term_freq(field, self)
  }

  fn index_base(&self) -> &IndexReaderBase {
    &self.index_base
  }
}

impl<DR> Display for TermsCountingDirectoryReaderWrapper<DR>
where
  DR: DirectoryReader,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", CompositeReader::to_string(self))
  }
}

impl<DR> DirectoryReader for TermsCountingDirectoryReaderWrapper<DR>
where
  DR: DirectoryReader,
{
  type DirectoryReader = TermsCountingDirectoryReaderWrapper<DR::DirectoryReader>;

  fn do_open_if_changed(&self) -> Result<Option<Self::DirectoryReader>> {
    self.wrap_directory_reader(self.in_.do_open_if_changed()?)
  }

  fn do_open_if_changed_with_commit<IC>(
    &self,
    commit: Option<&IC>,
  ) -> Result<Option<Self::DirectoryReader>>
  where
    IC: crate::core::index::index_commit::IndexCommit<Directory = Arc<Self::Directory>>,
  {
    self.wrap_directory_reader(self.in_.do_open_if_changed_with_commit(commit)?)
  }

  fn do_open_if_changed_with_deletes(
    &self,
    writer: &Arc<IndexWriter<Self::Directory>>,
    apply_deletes: bool,
  ) -> Result<Option<Self::DirectoryReader>> {
    self.wrap_directory_reader(
      self
        .in_
        .do_open_if_changed_with_deletes(writer, apply_deletes)?,
    )
  }

  fn get_version(&self) -> Result<i64> {
    self.in_.get_version()
  }

  fn is_current(&self) -> Result<bool> {
    self.in_.is_current()
  }

  type IndexCommit = DR::IndexCommit;

  fn get_index_commit(&self) -> Result<Self::IndexCommit> {
    self.in_.get_index_commit()
  }

  type Directory = DR::Directory;

  fn directory(&self) -> &DirectoryReaderBase<Self::Directory> {
    self.in_.directory()
  }
}

impl<DR> FilterDirectoryReader for TermsCountingDirectoryReaderWrapper<DR>
where
  DR: DirectoryReader,
{
  type Delegate = DR;

  fn get_delegate(&self) -> &Self::Delegate {
    &self.in_
  }

  type WrapDirectoryReader = TermsCountingDirectoryReaderWrapper<DR::DirectoryReader>;

  fn do_wrap_directory_reader(
    &self,
    in_: Option<<Self::Delegate as DirectoryReader>::DirectoryReader>,
  ) -> Result<Option<Self::WrapDirectoryReader>> {
    in_
      .map(|reader| Self::WrapDirectoryReader::new(reader, self.counter.clone()))
      .transpose()
  }
}

#[test]
fn test_pull_one_terms_enum() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = RandomIndexWriter::new(&mut random, dir.clone())?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("foo", "1", Store::No)?);
  writer.add_document(&mut random, doc)?;
  let reader = writer.get_reader(&mut random)?;
  writer.close(&mut random)?;
  let counter = Arc::new(AtomicI32::new(0));
  let wrapped = TermsCountingDirectoryReaderWrapper::new(reader, counter.clone())?;

  // enough terms to avoid the rewrite
  let num_terms = TestUtil::next_int(
    &mut random,
    BOOLEAN_REWRITE_TERM_COUNT_THRESHOLD as i32 + 1,
    100,
  );
  let mut terms = Vec::with_capacity(num_terms as usize);
  for _ in 0..num_terms {
    let term = TestUtil::random_realistic_unicode_string_range(&mut random, 10, 10);
    terms.push(new_bytes_ref_from_string(&mut random, &term)?);
  }

  let searcher = new_searcher_with_reader(wrapped)?;
  assert_eq!(
    0,
    searcher.count(TermInSetQuery::new("bar", terms.clone())?)?
  );
  assert_eq!(0, counter.load(Ordering::SeqCst)); // missing field
  searcher.count(TermInSetQuery::new("foo", terms)?)?;
  assert_eq!(1, counter.load(Ordering::SeqCst));
  searcher.get_index_reader().close()?;
  dir.close()
}

#[test]
fn test_binary_to_string() -> Result<()> {
  let mut random = random();
  let query = TermInSetQuery::new(
    "field",
    vec![new_bytes_ref_from_bytes(&mut random, &[0xff, 0xfe])?],
  )?;
  assert_eq!("field:([ff fe])", query.to_string("")?);
  Ok(())
}

#[test]
fn test_is_considered_costly_by_query_cache() -> Result<()> {
  let mut random = random();
  let query: Query = TermInSetQuery::new(
    "foo",
    vec![
      new_bytes_ref_from_string(&mut random, "bar")?,
      new_bytes_ref_from_string(&mut random, "baz")?,
    ],
  )?
  .into_query();
  let policy = UsageTrackingQueryCachingPolicy::new()?;
  assert!(!policy.should_cache(&query)?);
  policy.on_use(&query);
  policy.on_use(&query);
  assert!(policy.should_cache(&query)?);
  Ok(())
}

#[test]
fn test_visitor() -> Result<()> {
  // TODO: Restore this Java test after QueryVisitor and TermInSetQuery::visit are implemented.
  Ok(())
}

#[test]
fn test_terms_iterator() -> Result<()> {
  let mut random = random();
  let empty = TermInSetQuery::new("field", Vec::new())?;
  let mut iterator = empty.get_bytes_ref_iterator()?;
  assert!(iterator.next()?.is_none());

  let query = TermInSetQuery::new(
    "field",
    vec![
      new_bytes_ref_from_string(&mut random, "term1")?,
      new_bytes_ref_from_string(&mut random, "term2")?,
      new_bytes_ref_from_string(&mut random, "term3")?,
    ],
  )?;
  iterator = query.get_bytes_ref_iterator()?;
  assert_eq!(
    new_bytes_ref_from_string(&mut random, "term1")?,
    iterator.next()?.unwrap().into_owned()
  );
  assert_eq!(
    new_bytes_ref_from_string(&mut random, "term2")?,
    iterator.next()?.unwrap().into_owned()
  );
  assert_eq!(
    new_bytes_ref_from_string(&mut random, "term3")?,
    iterator.next()?.unwrap().into_owned()
  );
  assert!(iterator.next()?.is_none());
  Ok(())
}
