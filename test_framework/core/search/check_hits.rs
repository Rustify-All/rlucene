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
use crate::core::codecs::dummy::dummy_binary_doc_values::DummyBinaryDocValues;
use crate::core::codecs::dummy::dummy_doc_values_skipper::DummyDocValuesSkipper;
use crate::core::codecs::dummy::dummy_numeric_doc_values::DummyNumericDocValues;
use crate::core::codecs::dummy::dummy_sorted_doc_values::DummySortedDocValues;
use crate::core::codecs::dummy::dummy_sorted_numeric_doc_values::DummySortedNumericDocValues;
use crate::core::codecs::dummy::dummy_sorted_set_doc_values::DummySortedSetDocValues;
use crate::core::index::dummy::dummy_byte_vector_values::DummyByteVectorValues;
use crate::core::index::dummy::dummy_cache_helper::DummyCacheHelper;
use crate::core::index::dummy::dummy_float_vector_values::DummyFloatVectorValues;
use crate::core::index::dummy::dummy_point_value_base::DummyPointValues;
use crate::core::index::dummy::dummy_terms::DummyTerms;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::index_reader::{IndexReader, IndexReaderBase, LeafReaderContextKind};
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::leaf_metadata::LeafMetaData;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::stored_field_visitor::StoredFieldVisitor;
use crate::core::index::stored_fields::{RawStoredFieldsReader, StoredFields};
use crate::core::index::term::Term;
use crate::core::index::term_vectors::EmptyTermVectors;
use crate::core::search::collector::Collector;
use crate::core::search::collector_manager::CollectorManager;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::explanation::Explanation;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::search::leaf_collector::LeafCollector;
use crate::core::search::query::{Query, QueryBase, QueryWeightSsScorer};
use crate::core::search::scorable::Scorable;
use crate::core::search::score_doc::ScoreDocLike;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::simple_collector::SimpleCollector;
use crate::core::search::top_score_doc_collector_manager::TopScoreDocCollectorManager;
use crate::core::search::weight::Weight;
use crate::core::store::dummy::dummy_index_input::DummyIndexInput;
use crate::core::util::bits::{Bits, MatchNoBits};
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::version::LATEST;
use crate::test_framework::core::search::query_utils::QueryUtils;
use crate::test_framework::core::util::lucene_test_case::rarely;
use crate::test_framework::ulp_f32;
use rand::Rng;
use rand::RngExt;
use regex::Regex;
use std::collections::BTreeSet;
use std::fmt::{Display, Formatter};
use std::sync::{Arc, LazyLock};

pub struct CheckHits;
impl CheckHits {
  /// Tests that all documents up to `maxDoc` which are *not* in the expected result set, have an
  /// explanation which indicates that the document does not match.
  pub fn check_no_match_explanations<IRC>(
    q: Query,
    default_field_name: &str,
    searcher: &IndexSearcher<IRC>,
    results: &[i32],
  ) -> Result<()>
  where
    IRC: IndexReaderContext + Sync + 'static,
  {
    let d = q.to_string(default_field_name)?;
    let ignore: BTreeSet<i32> = results.iter().copied().collect();

    let max_doc = searcher.get_index_reader().max_doc()?;
    for doc in 0..max_doc {
      if ignore.contains(&doc) {
        continue;
      }

      let exp = searcher.explain(q.clone(), doc)?;
      assert!(
        !exp.is_match(),
        "Explanation of [[{}]] for #{} doesn't indicate non-match: {}",
        d,
        doc,
        exp
      );
    }

    Ok(())
  }

  /// Tests that a query matches the expected set of documents using a collector.
  ///
  /// Note that when using the collector API, documents will be collected if they "match"
  /// regardless of what their score is.
  ///
  /// * `random` - a random instance
  /// * `query` - the query to test
  /// * `default_field_name` - used for displaying the query in assertion messages
  /// * `searcher` - the searcher to test the query against
  /// * `results` - a list of documentIds that must match the query
  ///   See also: `check_hits`
  pub fn check_hit_collector<IRC, R>(
    random: &mut R,
    query: Query,
    default_field_name: &str,
    searcher: &IndexSearcher<IRC>,
    results: &[i32],
  ) -> Result<()>
  where
    R: Rng + ?Sized,
    IRC: IndexReaderContext + Sync + 'static,
    IRC::LeafReader: Clone,
  {
    QueryUtils::check_from_searcher(random, query.clone(), searcher)?;

    let correct: BTreeSet<i32> = results.iter().copied().collect();
    let query_string = query.to_string(default_field_name)?;

    let manager = SetCollectorManager::new();
    let actual = searcher.search_with_collector_manager(query.clone(), &manager)?;
    assert_eq!(correct, actual, "Simple: {}", query_string);

    for i in -1..2 {
      let s = QueryUtils::wrap_underlying_reader(random, searcher, i)?;
      let manager = SetCollectorManager::new();
      let actual = s.search_with_collector_manager(query.clone(), &manager)?;
      assert_eq!(correct, actual, "Wrap Reader {}: {}", i, query_string);
    }

    Ok(())
  }

  /// Tests that a query matches the expected set of documents using Hits.
  ///
  /// Note that when using the Hits API, documents will only be returned if they have a positive
  /// normalized score.
  ///
  /// * `random` - a random instance
  /// * `query` - the query to test
  /// * `default_field_name` - used for displaying the query in assertion messages
  /// * `searcher` - the searcher to test the query against
  /// * `results` - a list of documentIds that must match the query
  ///   See also: `check_hit_collector`
  pub fn check_hits<IRC, R>(
    random: &mut R,
    query: Query,
    default_field_name: &str,
    searcher: &IndexSearcher<IRC>,
    results: &[i32],
  ) -> Result<()>
  where
    R: Rng + ?Sized,
    IRC: IndexReaderContext + Sync + 'static,
    IRC::LeafReader: Clone,
  {
    let hits = searcher
      .search(query.clone(), (10usize).max(results.len() * 2))?
      .score_docs;

    let correct: BTreeSet<i32> = results.iter().copied().collect();

    let mut actual = BTreeSet::new();
    for hit in &hits {
      actual.insert(hit.doc());
    }

    assert_eq!(
      correct,
      actual,
      "{}",
      query.to_string(default_field_name).unwrap()
    );

    let wrap = rarely(random);
    QueryUtils::check_from_searcher_with_wrap(random, query, searcher, wrap)
  }

  pub fn check_doc_ids<S>(mes: &str, result: &[i32], hits: &[S]) -> Result<()>
  where
    S: ScoreDocLike,
  {
    assert_eq!(hits.len(), result.len(), "{} nr of hits", mes);
    for i in 0..result.len() {
      assert_eq!(result[i], hits[i].doc(), "{} doc nrs for hit {}", mes, i);
    }
    Ok(())
  }
  pub fn check_hits_query<S>(query: &Query, hits1: &[S], hits2: &[S], result: &[i32]) -> Result<()>
  where
    S: ScoreDocLike,
  {
    Self::check_doc_ids("hits1", result, hits1)?;
    Self::check_doc_ids("hits2", result, hits2)?;
    Self::check_equal(query, hits1, hits2)
  }

  pub fn check_equal<S>(query: &Query, hits1: &[S], hits2: &[S]) -> Result<()>
  where
    S: ScoreDocLike,
  {
    const SCORE_TOLERANCE: f32 = 1.0e-6;

    if hits1.len() != hits2.len() {
      return Err(LuceneError::illegal_argument(format!(
        "Unequal lengths: hits1={}, hits2={}",
        hits1.len(),
        hits2.len()
      )));
    }

    for (i, (h1, h2)) in hits1.iter().zip(hits2.iter()).enumerate() {
      if h1.doc() != h2.doc() {
        return Err(LuceneError::illegal_argument(format!(
          "Hit {i} docnumbers don't match\nhits1={:?}\nhits2={:?}\nfor query: {:?}",
          hits1, hits2, query
        )));
      }

      if (h1.doc() != h2.doc()) || (h1.score() - h2.score()).abs() > SCORE_TOLERANCE {
        return Err(LuceneError::illegal_argument(format!(
          "Hit {i}, doc nrs {} and {}\nunequal: {}\nand: {}\nfor query: {:?}",
          h1.doc(),
          h2.doc(),
          h1.score(),
          h2.score(),
          query
        )));
      }
    }

    Ok(())
  }
  pub fn check_explanations<IRC>(
    query: &Query,
    default_field_name: &str,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<()>
  where
    IRC: IndexReaderContext + Sync + 'static,
  {
    Self::check_explanations_with_deep(query, default_field_name, searcher, false)
  }

  pub fn check_explanations_with_deep<IRC>(
    query: &Query,
    default_field_name: &str,
    searcher: &IndexSearcher<IRC>,
    deep: bool,
  ) -> Result<()>
  where
    IRC: IndexReaderContext + Sync + 'static,
  {
    let manager =
      ExplanationAsserterManager::new(query.clone(), default_field_name, searcher, deep);
    searcher.search_with_collector_manager(query.clone(), &manager)
  }

  /// Asserts that the result of calling [`Weight::matches`] for every document matching a query
  /// returns a non-`None` [`Matches`](crate::core::search::matches::Matches).
  pub fn check_matches<IRC>(query: Query, searcher: &IndexSearcher<IRC>) -> Result<()>
  where
    IRC: IndexReaderContext + Sync + 'static,
  {
    let manager = MatchesAsserterManager::new(query.clone(), searcher);
    searcher.search_with_collector_manager(query, &manager)
  }

  pub fn verify_explanation(
    q: &str,
    doc: i32,
    score: f32,
    deep: bool,
    expl: &Explanation,
  ) -> Result<()> {
    let value = expl.get_value().to_f32().ok_or_else(|| {
      LuceneError::illegal_argument(format!("cannot convert to f32: {}", expl.get_value()))
    })?;
    if value != score {
      unreachable!(
        "{}: score(doc={})={} != explanationScore={} Explanation: {}",
        q, doc, score, value, expl
      );
    }

    if !deep {
      return Ok(());
    }

    let details = expl.get_details();
    let descr = expl.get_description().to_lowercase();

    if descr.ends_with("computed from:") {
      return Ok(());
    }

    if descr.starts_with("score based on ") && descr.contains("child docs in range") {
      assert!(!details.is_empty(), "Child doc explanations are missing");
    }

    if !details.is_empty() && expl.is_match() {
      if details.len() == 1 && !COMPUTED_FROM_PATTERN.is_match(&descr) {
        let allow_compute_freq = !expl.get_description().ends_with("with freq of:")
          && (score >= 0.0 || !expl.get_description().ends_with("times others of:"));

        if allow_compute_freq {
          Self::verify_explanation(q, doc, score, deep, &details[0])?;
        }
        return Ok(());
      }

      let product_of = descr.ends_with("product of:");
      let sum_of = descr.ends_with("sum of:");
      let max_of = descr.ends_with("max of:");
      let computed_of = descr.contains("computed as") && COMPUTED_FROM_PATTERN.is_match(&descr);

      let mut max_times_others = false;
      let mut x: f32 = 0.0;

      if !(product_of || sum_of || max_of || computed_of) {
        let pat = "max plus ";
        if let Some(k1) = descr.find(pat) {
          let k1 = k1 + pat.len();
          if let Some(k2) = descr[k1..].find(' ') {
            let k2 = k1 + k2;
            let slice = descr[k1..k2].trim();
            if let Ok(val) = slice.parse::<f32>() {
              x = val;
              let remain = descr[k2..].trim();
              if remain == "times others of:" {
                max_times_others = true;
              }
            }
          }
        }
      }

      if !(product_of || sum_of || max_of || computed_of || max_times_others) {
        unreachable!(
          "{}: multi valued explanation description=\"{}\" must be 'max plus x times others', \
                 'computed as x from:' or end with 'product of', 'sum of:', 'max of:' - {}",
          q, descr, expl
        );
      }

      // sum/product/max computing
      let mut sum = 0f64;
      let mut product = 1f32;
      let mut max = f32::NEG_INFINITY;
      let mut max_error = 0f64;

      for d in details.iter() {
        let dval = d.get_value().to_f32().ok_or_else(|| {
          LuceneError::illegal_argument(format!("cannot convert to f32: {}", d.get_value()))
        })?;
        Self::verify_explanation(q, doc, dval, deep, d)?;

        product *= dval;
        sum += dval as f64;
        if dval > max {
          max = dval;
        }

        if sum_of {
          max_error += ulp_f32(dval) as f64 * 2.0;
        }
      }

      let combined: f32 = if product_of {
        product
      } else if sum_of {
        sum as f32
      } else if max_of {
        max
      } else if max_times_others {
        (max as f64 + x as f64 * (sum - max as f64)) as f32
      } else {
        // computedOf
        value
      };

      // assertEquals(combined, value, maxError)
      let diff = (combined as f64 - value as f64).abs();
      if diff > max_error {
        unreachable!(
          "{}: actual subDetails combined=={} != value={} Explanation: {}",
          q, combined, value, expl
        );
      }
    }

    Ok(())
  }
  pub fn check_top_scores<IRC, R>(
    random: &mut R,
    query: &Query,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<()>
  where
    IRC: IndexReaderContext + Sync,
    R: Rng + ?Sized,
  {
    // Check it computed the top hits correctly
    Self::do_check_top_scores(query, searcher, 1)?;
    Self::do_check_top_scores(query, searcher, 10)?;

    // Now check that the exposed max scores and block boundaries are valid
    Self::do_check_max_scores(random, query.clone(), searcher)?;

    Ok(())
  }

  fn do_check_top_scores<IRC>(
    query: &Query,
    searcher: &IndexSearcher<IRC>,
    num_hits: usize,
  ) -> Result<()>
  where
    IRC: IndexReaderContext + Sync,
  {
    let complete = TopScoreDocCollectorManager::with_after(num_hits, None, i32::MAX as usize)?;
    let top_scores = TopScoreDocCollectorManager::with_after(num_hits, None, 1)?;

    let complete_top_docs = searcher.search_with_collector_manager(query.clone(), &complete)?;
    let top_scores_top_docs = searcher.search_with_collector_manager(query.clone(), &top_scores)?;
    Self::check_equal(
      query,
      &complete_top_docs.score_docs,
      &top_scores_top_docs.score_docs,
    )?;

    Ok(())
  }

  fn do_check_max_scores<IRC, R>(
    random: &mut R,
    mut query: Query,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<()>
  where
    IRC: IndexReaderContext,
    R: Rng + ?Sized,
  {
    query = searcher.rewrite(query)?;

    let w1 = searcher.create_weight(query.clone(), ScoreMode::Complete, 1.0)?;
    let w2 = searcher.create_weight(query, ScoreMode::TopScores, 1.0)?;

    // Check boundaries and max scores when iterating all matches
    for ctx in searcher.get_leaf_contexts()? {
      let mut s1 = w1.scorer(ctx, searcher)?;
      let mut ss2 = w2.scorer_supplier(ctx, searcher)?;
      let mut s2 = if let Some(mut ss2) = ss2.take() {
        ss2.set_top_level_scoring_clause()?;
        Some(ss2.get(i64::MAX, ctx, searcher)?)
      } else {
        None
      };

      if s1.is_none() {
        if let Some(s2) = s2.as_mut() {
          assert_eq!(NO_MORE_DOCS, s2.iterator_mut().next_doc()?);
        }
        continue;
      }
      if s2.is_none() {
        let s1 = s1.as_mut().unwrap();
        assert_eq!(NO_MORE_DOCS, s1.iterator_mut().next_doc()?);
        continue;
      }

      let mut s1 = s1.unwrap();
      let mut s2 = s2.unwrap();

      let mut upto: i32 = -1;
      let mut max_score: f32 = 0.0;
      let mut min_score: f32 = 0.0;

      let mut doc2 = Self::next_doc(&mut s2)?;
      loop {
        let mut doc1 = Self::next_doc(&mut s1)?;
        while doc1 < doc2 {
          let matches1 = Self::matches(&mut s1)?;
          if matches1 {
            assert!(s1.score()? < min_score);
          }
          doc1 = Self::next_doc(&mut s1)?;
        }

        assert_eq!(doc1, doc2);
        if doc2 == NO_MORE_DOCS {
          break;
        }

        if doc2 > upto {
          upto = s2.advance_shallow(doc2)?;
          assert!(upto >= doc2);
          max_score = s2.get_max_score(upto)?;
        }

        let matches2 = Self::matches(&mut s2)?;
        if matches2 {
          let matches1 = Self::matches(&mut s1)?;
          assert!(matches1);

          let score = s2.score()?;
          assert_eq!(s1.score()?, score);
          assert!(score <= max_score);

          if score >= min_score && random.random_range(0..10) == 0 {
            min_score = score;
            s2.set_min_competitive_score(min_score)?;
          }
        }

        doc2 = Self::next_doc(&mut s2)?;
      }
    }

    // Now check advancing
    for ctx in searcher.get_leaf_contexts()? {
      let mut s1 = w1.scorer(ctx, searcher)?;
      let mut ss2 = w2.scorer_supplier(ctx, searcher)?;
      let mut s2 = if let Some(mut ss2) = ss2.take() {
        ss2.set_top_level_scoring_clause()?;
        Some(ss2.get(i64::MAX, ctx, searcher)?)
      } else {
        None
      };

      if s1.is_none() {
        if let Some(s2) = s2.as_mut() {
          assert_eq!(NO_MORE_DOCS, s2.iterator_mut().next_doc()?);
        }
        continue;
      }
      if s2.is_none() {
        let s1 = s1.as_mut().unwrap();
        assert_eq!(NO_MORE_DOCS, s1.iterator_mut().next_doc()?);
        continue;
      }

      let mut s1 = s1.unwrap();
      let mut s2 = s2.unwrap();

      let mut upto: i32 = -1;
      let mut min_score: f32 = 0.0;
      let mut max_score: f32 = 0.0;

      loop {
        let doc_id = s2.doc_id()?;
        let (advance, target) = if random.random_bool(0.5) {
          (false, doc_id.wrapping_add(1))
        } else {
          let delta = std::cmp::min(
            1 + random.random_range(0..512),
            NO_MORE_DOCS.wrapping_sub(doc_id),
          );
          (true, s2.doc_id()?.wrapping_add(delta))
        };

        if target > upto && random.random_bool(0.5) {
          let delta = std::cmp::min(
            random.random_range(0..512),
            NO_MORE_DOCS.wrapping_sub(target),
          );
          upto = target.wrapping_add(delta);
          let m = s2.advance_shallow(target)?;
          assert!(m >= target);
          max_score = s2.get_max_score(upto)?;
        }

        let doc2 = if advance {
          Self::advance(&mut s2, target)?
        } else {
          Self::next_doc(&mut s2)?
        };

        let mut doc1 = Self::advance(&mut s1, target)?;
        while doc1 < doc2 {
          let matches1 = Self::matches(&mut s1)?;
          if matches1 {
            assert!(s1.score()? < min_score);
          }
          doc1 = Self::next_doc(&mut s1)?;
        }
        assert_eq!(doc1, doc2);

        if doc2 == NO_MORE_DOCS {
          break;
        }

        let matches2 = Self::matches(&mut s2)?;
        if matches2 {
          let matches1 = Self::matches(&mut s1)?;
          assert!(matches1);

          let score = s2.score()?;
          assert_eq!(s1.score()?, score);

          if doc2 > upto {
            upto = s2.advance_shallow(doc2)?;
            assert!(upto >= doc2);
            max_score = s2.get_max_score(upto)?;
          }

          assert!(score <= max_score);

          if score >= min_score && random.random_range(0..10) == 0 {
            min_score = score;
            s2.set_min_competitive_score(min_score)?;
          }
        }
      }
    }

    Ok(())
  }

  fn advance(s: &mut QueryWeightSsScorer, target: i32) -> Result<i32> {
    if let Some(tp) = s.two_phase_iterator_mut().as_mut() {
      let mut v = tp.approximation_mut();
      v.advance(target)
    } else {
      let mut v = s.iterator_mut();
      v.advance(target)
    }
  }

  fn next_doc(s: &mut QueryWeightSsScorer) -> Result<i32> {
    if let Some(tp) = s.two_phase_iterator_mut().as_mut() {
      let mut v = tp.approximation_mut();
      v.next_doc()
    } else {
      let mut v = s.iterator_mut();
      v.next_doc()
    }
  }
  fn matches(s: &mut QueryWeightSsScorer) -> Result<bool> {
    if let Some(tp) = s.two_phase_iterator_mut().as_mut() {
      tp.matches()
    } else {
      Ok(true)
    }
  }
}

struct ExplanationAsserterManager<'a, IRC>
where
  IRC: 'static,
{
  q: Query,
  default_field_name: &'a str,
  s: &'a IndexSearcher<IRC>,
  deep: bool,
}

impl<'a, IRC> ExplanationAsserterManager<'a, IRC>
where
  IRC: IndexReaderContext + 'static,
{
  fn new(q: Query, default_field_name: &'a str, s: &'a IndexSearcher<IRC>, deep: bool) -> Self {
    Self {
      q,
      default_field_name,
      s,
      deep,
    }
  }
}

impl<'a, IRC> CollectorManager for ExplanationAsserterManager<'a, IRC>
where
  IRC: IndexReaderContext + 'static,
{
  type C = ExplanationAsserter<'a, IRC>;
  type T = ();

  fn new_collector(&self) -> Result<Self::C> {
    ExplanationAsserter::new(self.q.clone(), self.default_field_name, self.s, self.deep)
  }

  fn reduce(&self, _collectors: Vec<Self::C>) -> Result<Self::T> {
    Ok(())
  }
}

struct MatchesAsserterManager<'a, IRC>
where
  IRC: 'static,
{
  query: Query,
  searcher: &'a IndexSearcher<IRC>,
}

impl<'a, IRC> MatchesAsserterManager<'a, IRC>
where
  IRC: IndexReaderContext + 'static,
{
  fn new(query: Query, searcher: &'a IndexSearcher<IRC>) -> Self {
    Self { query, searcher }
  }
}

impl<'a, IRC> CollectorManager for MatchesAsserterManager<'a, IRC>
where
  IRC: IndexReaderContext + 'static,
{
  type C = MatchesAsserter<'a, IRC>;
  type T = ();

  fn new_collector(&self) -> Result<Self::C> {
    MatchesAsserter::new(self.query.clone(), self.searcher)
  }

  fn reduce(&self, _collectors: Vec<Self::C>) -> Result<Self::T> {
    Ok(())
  }
}

/// Asserts that the score explanation for every document matching a query corresponds with the
/// true score.
///
/// NOTE: this collector should only be used with the Query and Searcher specified when it is
/// constructed.
struct ExplanationAsserter<'a, IRC>
where
  IRC: 'static,
{
  q: Query,
  s: &'a IndexSearcher<IRC>,
  d: String,
  deep: bool,
  base: i32,
}

impl<'a, IRC> ExplanationAsserter<'a, IRC>
where
  IRC: IndexReaderContext + 'static,
{
  /// Constructs an instance which does shallow tests on the Explanation.
  fn new(
    q: Query,
    default_field_name: &str,
    s: &'a IndexSearcher<IRC>,
    deep: bool,
  ) -> Result<Self> {
    let d = q.to_string(default_field_name)?;
    Ok(Self {
      q,
      s,
      d,
      deep,
      base: 0,
    })
  }
}

impl<IRC> Collector for ExplanationAsserter<'_, IRC>
where
  IRC: IndexReaderContext + 'static,
{
  type LeafCollector<'a, IRC1>
    = &'a mut Self
  where
    Self: 'a,
    IRC1: IndexReaderContext + 'a;

  fn get_leaf_collector<'a, W, IRC1>(
    &'a mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC1>>,
    _weight: Option<&W>,
    _searcher: &IndexSearcher<IRC1>,
  ) -> Result<Self::LeafCollector<'a, IRC1>>
  where
    IRC1: IndexReaderContext,
    W: Weight<IRC1> + ?Sized,
  {
    SimpleCollector::do_set_next_reader(self, context)?;
    Ok(self)
  }

  fn score_mode(&self) -> ScoreMode {
    ScoreMode::Complete
  }
}

impl<IRC> LeafCollector for ExplanationAsserter<'_, IRC>
where
  IRC: IndexReaderContext + 'static,
{
  fn collect(&mut self, doc: i32, scorer: &mut dyn Scorable) -> Result<()> {
    let doc = doc + self.base;
    let exp = self.s.explain(self.q.clone(), doc)?;
    CheckHits::verify_explanation(&self.d, doc, scorer.score()?, self.deep, &exp)?;
    assert!(
      exp.is_match(),
      "Explanation of [[{}]] for #{} does not indicate match: {}",
      self.d,
      doc,
      exp
    );
    Ok(())
  }
}

impl<IRC> SimpleCollector for ExplanationAsserter<'_, IRC>
where
  IRC: IndexReaderContext + 'static,
{
  fn do_set_next_reader<LR>(&mut self, context: &LeafReaderContext<LR>) -> Result<()>
  where
    LR: LeafReader,
  {
    self.base = context.doc_base as i32;
    Ok(())
  }
}

impl<IRC> Display for ExplanationAsserter<'_, IRC>
where
  IRC: IndexReaderContext,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

/// Asserts that the [`Matches`](crate::core::search::matches::Matches) from a query is non-`None`
/// whenever the document it is created for is a hit.
///
/// Also checks that the previous non-matching document has a `None`
/// [`Matches`](crate::core::search::matches::Matches).
struct MatchesAsserter<'a, IRC>
where
  IRC: 'static,
{
  query: Query,
  searcher: &'a IndexSearcher<IRC>,
  query_string: String,
  context_ord: usize,
  last_checked_doc: i32,
  // With intra-segment concurrency, we may start from a doc id that isn't -1. We need to make
  // sure that we don't go outside of the bounds of the current slice, meaning -1 can't be
  // reliably used to signal that we are collecting the first doc for a given segment partition.
  collected_once: bool,
}

impl<'a, IRC> MatchesAsserter<'a, IRC>
where
  IRC: IndexReaderContext + 'static,
{
  fn new(query: Query, searcher: &'a IndexSearcher<IRC>) -> Result<Self> {
    let query_string = query.to_string("")?;
    Ok(Self {
      query,
      searcher,
      query_string,
      context_ord: 0,
      last_checked_doc: -1,
      collected_once: false,
    })
  }
}

impl<IRC> Collector for MatchesAsserter<'_, IRC>
where
  IRC: IndexReaderContext + 'static,
{
  type LeafCollector<'a, IRC1>
    = &'a mut Self
  where
    Self: 'a,
    IRC1: IndexReaderContext + 'a;

  fn get_leaf_collector<'a, W, IRC1>(
    &'a mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC1>>,
    _weight: Option<&W>,
    _searcher: &IndexSearcher<IRC1>,
  ) -> Result<Self::LeafCollector<'a, IRC1>>
  where
    IRC1: IndexReaderContext,
    W: Weight<IRC1> + ?Sized,
  {
    SimpleCollector::do_set_next_reader(self, context)?;
    Ok(self)
  }

  fn score_mode(&self) -> ScoreMode {
    ScoreMode::CompleteNoScores
  }
}

impl<IRC> LeafCollector for MatchesAsserter<'_, IRC>
where
  IRC: IndexReaderContext + 'static,
{
  fn collect(&mut self, doc: i32, _scorer: &mut dyn Scorable) -> Result<()> {
    let context = &self.searcher.get_leaf_contexts()?[self.context_ord];
    let query = self.searcher.rewrite(self.query.clone())?;
    let weight = self
      .searcher
      .create_weight(query, ScoreMode::CompleteNoScores, 1.0)?;
    let matches = weight.matches(context, doc, self.searcher)?;
    assert!(
      matches.is_some(),
      "Unexpected null Matches object in doc{} for query {}",
      doc,
      self.query_string
    );
    if self.collected_once && self.last_checked_doc != doc - 1 {
      assert!(
        weight.matches(context, doc - 1, self.searcher)?.is_none(),
        "Unexpected non-null Matches object in non-matching doc{} for query {}",
        doc,
        self.query_string
      );
    }
    self.collected_once = true;
    self.last_checked_doc = doc;
    Ok(())
  }
}

impl<IRC> SimpleCollector for MatchesAsserter<'_, IRC>
where
  IRC: IndexReaderContext + 'static,
{
  fn do_set_next_reader<LR>(&mut self, context: &LeafReaderContext<LR>) -> Result<()>
  where
    LR: LeafReader,
  {
    self.context_ord = context.ord;
    self.last_checked_doc = -1;
    Ok(())
  }
}

impl<IRC> Display for MatchesAsserter<'_, IRC>
where
  IRC: IndexReaderContext,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

struct SetCollectorManager;

impl SetCollectorManager {
  fn new() -> Self {
    Self
  }
}

impl CollectorManager for SetCollectorManager {
  type C = SetCollector;
  type T = BTreeSet<i32>;

  fn new_collector(&self) -> Result<Self::C> {
    Ok(SetCollector::new(BTreeSet::new()))
  }

  fn reduce(&self, collectors: Vec<Self::C>) -> Result<Self::T> {
    let mut ids = BTreeSet::new();
    for collector in collectors {
      ids.extend(collector.bag);
    }
    Ok(ids)
  }
}

/// Just collects document ids into a set.
pub struct SetCollector {
  bag: BTreeSet<i32>,
  base: i32,
}

impl SetCollector {
  fn new(bag: BTreeSet<i32>) -> Self {
    Self { bag, base: 0 }
  }
}

impl Collector for SetCollector {
  type LeafCollector<'a, IRC>
    = &'a mut Self
  where
    Self: 'a,
    IRC: IndexReaderContext + 'a;

  fn get_leaf_collector<'a, W, IRC>(
    &'a mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _weight: Option<&W>,
    _searcher: &IndexSearcher<IRC>,
  ) -> Result<Self::LeafCollector<'a, IRC>>
  where
    IRC: IndexReaderContext,
    W: Weight<IRC> + ?Sized,
  {
    SimpleCollector::do_set_next_reader(self, context)?;
    Ok(self)
  }

  fn score_mode(&self) -> ScoreMode {
    ScoreMode::CompleteNoScores
  }
}

impl LeafCollector for SetCollector {
  fn set_scorer(&mut self, _scorer: &mut dyn Scorable) -> Result<()> {
    Ok(())
  }

  fn collect(&mut self, doc: i32, _scorer: &mut dyn Scorable) -> Result<()> {
    self.bag.insert(doc + self.base);
    Ok(())
  }
}

impl SimpleCollector for SetCollector {
  fn do_set_next_reader<LR>(&mut self, context: &LeafReaderContext<LR>) -> Result<()>
  where
    LR: LeafReader,
  {
    self.base = context.doc_base as i32;
    Ok(())
  }
}

impl Display for SetCollector {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}
pub static COMPUTED_FROM_PATTERN: LazyLock<Regex> =
  LazyLock::new(|| Regex::new(r"^.*, computed as .* from:$").unwrap());

fn empty_reader(max_doc: i32) -> EmptyLeafReader {
  EmptyLeafReader::new(max_doc)
}

struct EmptyLeafReader {
  max_doc: i32,
  live_docs: MatchNoBits,
  metadata: LeafMetaData,
  index_base: IndexReaderBase,
}

impl EmptyLeafReader {
  fn new(max_doc: i32) -> Self {
    assert!(max_doc >= 0, "max_doc must be non-negative");
    Self {
      max_doc,
      live_docs: MatchNoBits::new(max_doc as usize),
      metadata: LeafMetaData::new(LATEST.major, Some(LATEST.clone()), None, false)
        .expect("empty reader metadata should be valid"),
      index_base: IndexReaderBase::new(),
    }
  }
}

impl Display for EmptyLeafReader {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

impl IndexReader for EmptyLeafReader {
  type ContextKind = LeafReaderContextKind;

  type TermVectors = EmptyTermVectors;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    Ok(EmptyTermVectors)
  }

  fn max_doc(&self) -> Result<i32> {
    Ok(self.max_doc)
  }

  fn num_docs(&self) -> Result<i32> {
    Ok(0)
  }

  type StoredFields = EmptyStoredFields;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    Ok(EmptyStoredFields)
  }

  type ReaderCacheHelper = DummyCacheHelper;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    Ok(None)
  }

  fn doc_freq(&self, term: &Term) -> Result<i32> {
    LeafReader::doc_freq(self, term)
  }

  fn total_term_freq(&self, term: &Term) -> Result<i64> {
    LeafReader::get_total_term_freq(self, term)
  }

  fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
    LeafReader::get_sum_doc_freq(self, field)
  }

  fn get_doc_count(&self, field: &str) -> Result<i32> {
    LeafReader::get_doc_count(self, field)
  }

  fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
    LeafReader::get_sum_total_term_freq(self, field)
  }

  fn index_base(&self) -> &IndexReaderBase {
    &self.index_base
  }
}

impl LeafReader for EmptyLeafReader {
  type CacheHelper = DummyCacheHelper;

  fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
    Ok(None)
  }

  type Terms = DummyTerms;

  fn terms(&self, _field: &str) -> Result<Option<Self::Terms>> {
    Ok(None)
  }

  type NumericDocValues = DummyNumericDocValues;

  fn get_numeric_doc_values(&self, _field: &str) -> Result<Option<Self::NumericDocValues>> {
    Ok(None)
  }

  type BinaryDocValues = DummyBinaryDocValues;

  fn get_binary_doc_values(&self, _field: &str) -> Result<Option<Self::BinaryDocValues>> {
    Ok(None)
  }

  type SortedDocValues = DummySortedDocValues;

  fn get_sorted_doc_values(&self, _field: &str) -> Result<Option<Self::SortedDocValues>> {
    Ok(None)
  }

  type SortedNumericDocValues = DummySortedNumericDocValues;

  fn get_sorted_numeric_doc_values(
    &self,
    _field: &str,
  ) -> Result<Option<Self::SortedNumericDocValues>> {
    Ok(None)
  }

  type SortedSetDocValues = DummySortedSetDocValues;

  fn get_sorted_set_doc_values(&self, _field: &str) -> Result<Option<Self::SortedSetDocValues>> {
    Ok(None)
  }

  type NormNumericDocValues = DummyNumericDocValues;

  fn get_norm_values(&self, _field: &str) -> Result<Option<Self::NormNumericDocValues>> {
    Ok(None)
  }

  type DocValuesSkipper = DummyDocValuesSkipper;

  fn get_doc_values_skipper(&self, _field: &str) -> Result<Option<Self::DocValuesSkipper>> {
    Ok(None)
  }

  type FloatVectorValues = DummyFloatVectorValues;

  fn get_float_vector_values(&self, _field: &str) -> Result<Option<Self::FloatVectorValues>> {
    Ok(None)
  }

  type ByteVectorValues = DummyByteVectorValues;

  fn get_byte_vector_values(&self, _field: &str) -> Result<Option<Self::ByteVectorValues>> {
    Ok(None)
  }

  fn search_nearest_vectors_f32<B, K>(
    &self,
    _field: &str,
    _target: Vec<f32>,
    _knn_collector: &mut K,
    _accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
  {
    Ok(())
  }

  fn search_nearest_vectors_u8<B, K>(
    &self,
    _field: &str,
    _target: Vec<u8>,
    _knn_collector: &mut K,
    _accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
  {
    Ok(())
  }

  fn get_field_infos(&self) -> Result<Arc<FieldInfos>> {
    Ok(Arc::new(FieldInfos::new(vec![])?))
  }

  type Bits = MatchNoBits;

  fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
    Ok(Some(self.live_docs.clone()))
  }

  type PointValues = DummyPointValues;

  fn get_point_values(&self, _field: &str) -> Result<Option<Self::PointValues>> {
    Ok(None)
  }

  fn check_integrity(&self) -> Result<()> {
    Ok(())
  }

  fn get_metadata(&self) -> Result<&LeafMetaData> {
    Ok(&self.metadata)
  }
}

struct EmptyStoredFields;

impl StoredFields for EmptyStoredFields {
  fn document_with_visitor<S>(
    &mut self,
    _doc_id: i32,
    _visitor: &mut impl StoredFieldVisitor,
    _writer: Option<&mut S>,
  ) -> Result<()>
  where
    S: crate::core::codecs::stored_fields_writer::StoredFieldsWriter,
  {
    Ok(())
  }
}

impl RawStoredFieldsReader for EmptyStoredFields {
  type IndexInput = DummyIndexInput;
}
