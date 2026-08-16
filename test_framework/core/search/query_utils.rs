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
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::search::collector::Collector;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::doc_id_stream::DocIdStream;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::leaf_collector::LeafCollector;
use crate::core::search::query::{Query, QueryWeight};
use crate::core::search::scorable::Scorable;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer::Scorer;
use crate::core::search::simple_collector::SimpleCollector;
use crate::core::search::weight::Weight;
use crate::core::util::CoreHelper;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::test_framework::core::search::check_hits::CheckHits;
use crate::test_framework::core::util::lucene_test_case::new_searcher_with_reader;
use rand::{Rng, RngExt};
use std::fmt::{Display, Formatter};
use std::hash::Hash;

pub struct QueryUtils;
impl QueryUtils {
  pub fn check_from_query(q: &Query) {
    Self::check_equal(q, q);
  }
  /// Various query sanity checks on a searcher, some checks are only done for
  /// instances of `IndexSearcher`.
  ///
  /// # See also
  ///
  /// - [`Self::check`]
  /// - [`Self::check_first_skip_to`]
  /// - [`Self::check_skip_to`]
  /// - [`Self::check_explanations`]
  /// - [`Self::check_equal`]
  /// - [`CheckHits::check_matches`]
  pub fn check_from_searcher<T, IRC, R>(random: &mut R, q1: T, s: &IndexSearcher<IRC>) -> Result<()>
  where
    R: Rng + ?Sized,
    IRC: IndexReaderContext + Sync + 'static,
    IRC::LeafReader: Clone,
    T: Into<Query>,
  {
    Self::check_from_searcher_with_wrap(random, q1, s, true)
  }

  pub fn check_from_searcher_with_wrap<T, IRC, R>(
    random: &mut R,
    q: T,
    s: &IndexSearcher<IRC>,
    wrap: bool,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
    IRC: IndexReaderContext + Sync + 'static,
    IRC::LeafReader: Clone,
    T: Into<Query>,
  {
    let q = q.into();
    Self::check_from_query(&q);
    Self::check_first_skip_to(q.clone(), s)?;
    Self::check_skip_to(q.clone(), s)?;
    Self::check_bulk_scorer_skip_to(random, q.clone(), s)?;
    Self::check_count(q.clone(), s)?;
    if wrap {
      // TODO IMPORTANT
    }
    Self::check_explanations(&q, s)?;
    CheckHits::check_matches(q, s)?;
    Ok(())
  }

  pub fn wrap_underlying_reader<'a, R, IRC>(
    _random: &mut R,
    s: &'a IndexSearcher<IRC>,
    _edge: i32,
  ) -> Result<&'a IndexSearcher<IRC>>
  where
    R: Rng + ?Sized,
    IRC: IndexReaderContext,
  {
    // TODO IMPORTANT
    Ok(s)
  }

  pub fn check_equal<Q>(q1: &Q, q2: &Q)
  where
    Q: Eq + Hash + PartialEq,
  {
    assert!(q1 == q2);

    let hash1 = CoreHelper::calculate_hash(q1);
    let hash2 = CoreHelper::calculate_hash(q2);
    assert_eq!(hash1, hash2);
  }
  pub fn check_unequal<Q>(q1: &Q, q2: &Q)
  where
    Q: Eq + Hash + PartialEq + std::fmt::Debug,
  {
    assert_ne!(q1, q2);
    assert_ne!(q2, q1);
  }

  pub fn check_explanations<IRC>(query: &Query, searcher: &IndexSearcher<IRC>) -> Result<()>
  where
    IRC: IndexReaderContext + Sync + 'static,
  {
    CheckHits::check_explanations_with_deep(query, "", searcher, true)
  }

  pub fn check_skip_to<IRC>(q: Query, s: &IndexSearcher<IRC>) -> Result<()>
  where
    IRC: IndexReaderContext + 'static,
    IRC::LeafReader: Clone,
  {
    let reader_context_array = s.get_leaf_contexts()?;

    let skip_op = 0;
    let next_op = 1;

    let orders: &[&[i32]] = &[
      &[next_op],
      &[skip_op],
      &[skip_op, next_op],
      &[next_op, skip_op],
      &[skip_op, skip_op, next_op, next_op],
      &[next_op, next_op, skip_op, skip_op],
      &[skip_op, skip_op, skip_op, next_op, next_op],
    ];

    for order in orders {
      let max_diff: f32 = 1e-5f32;

      let mut collector =
        SimpleCollectorImpl2::new(q.clone(), s, max_diff, order, skip_op, reader_context_array);

      s.search_with_collector(q.clone(), &mut collector)?;

      if let Some(last_reader_idx) = collector.last_reader_idx {
        let previous_reader = reader_context_array[last_reader_idx].reader().clone();

        let mut index_searcher = new_searcher_with_reader(previous_reader)?;
        index_searcher.set_similarity(s.get_similarity().clone());

        let rewritten = index_searcher.rewrite(q.clone())?;
        let weight = index_searcher.create_weight(rewritten, ScoreMode::Complete, 1.0)?;
        let ctx = index_searcher.get_top_reader_context();
        let scorer_opt = weight.scorer(ctx, &index_searcher)?;
        if let Some(mut scorer) = scorer_opt {
          let mut more = false;
          {
            let mut iterator = scorer.iterator_mut();

            let bits = reader_context_array[last_reader_idx]
              .reader()
              .get_live_docs()?;
            let live_docs = bits.as_ref().map(|b| b as &dyn Bits);

            let mut d = iterator.advance(collector.last_doc + 1)?;

            while d != NO_MORE_DOCS {
              if match live_docs {
                None => true,
                Some(b) => b.get(d as usize)?,
              } {
                more = true;
                break;
              }
              d = iterator.next_doc()?;
            }
          }

          debug_assert!(
            !more,
            "query's last doc was {} but advance({}) got to {}",
            collector.last_doc,
            collector.last_doc + 1,
            scorer.doc_id()?
          );
        }
      }
    }

    Ok(())
  }

  pub fn check_first_skip_to<IRC>(q: Query, s: &IndexSearcher<IRC>) -> Result<()>
  where
    IRC: IndexReaderContext + 'static,
    IRC::LeafReader: Clone,
  {
    let max_diff: f32 = 1e-3f32;
    let rewritten = s.rewrite(q.clone())?;
    let weight = s.create_weight(rewritten.clone(), ScoreMode::Complete, 1.0)?;
    let mut collector = SimpleCollectorImp::new(weight, max_diff, rewritten.clone(), s);
    s.search_with_collector(q, &mut collector)?;
    if let Some(last_reader_idx) = collector.last_reader_idx {
      let previous_reader = s.get_leaf_contexts()?[last_reader_idx].reader().clone();
      let mut index_searcher = new_searcher_with_reader(previous_reader)?;
      index_searcher.set_similarity(s.get_similarity().clone());
      let weight = index_searcher.create_weight(rewritten.clone(), ScoreMode::Complete, 1.0)?;
      let top = index_searcher.get_top_reader_context();
      let scorer_opt = weight.scorer(top, &index_searcher)?;
      if let Some(mut scorer) = scorer_opt {
        let mut more = false;
        let bits = s.get_leaf_contexts()?[last_reader_idx]
          .reader()
          .get_live_docs()?;
        let live_docs = bits.as_ref().map(|b| b as &dyn Bits);
        {
          let mut iterator = scorer.iterator_mut();
          let mut d = iterator.advance(collector.last_doc + 1)?;
          while d != NO_MORE_DOCS {
            if match live_docs {
              None => true,
              Some(b) => b.get(d as usize)?,
            } {
              more = true;
              break;
            }

            d = iterator.next_doc()?;
          }
        }
        debug_assert!(
          !more,
          "query's last doc was {} but advance({}) got to {}",
          collector.last_doc,
          collector.last_doc + 1,
          scorer.doc_id()?
        );
      }
    }

    Ok(())
  }
  pub fn check_bulk_scorer_skip_to<IRC, R>(
    r: &mut R,
    query: Query,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<()>
  where
    IRC: IndexReaderContext,
    R: Rng + ?Sized,
  {
    let query = searcher.rewrite(query)?;
    let weight = searcher.create_weight(query, ScoreMode::Complete, 1.0)?;

    for ctx in searcher.get_leaf_contexts()? {
      let scorer_supplier = weight.scorer_supplier(ctx, searcher)?;
      let mut scorer = if let Some(mut ss) = scorer_supplier {
        Some(ss.get(0, ctx, searcher)?)
      } else {
        None
      };

      let mut bulk_scorer = weight.bulk_scorer(ctx, searcher)?;

      if scorer.is_none() && bulk_scorer.is_none() {
        continue;
      } else if bulk_scorer.is_none() {
        let scorer = scorer.as_mut().unwrap();
        debug_assert_eq!(scorer.iterator_mut().next_doc()?, NO_MORE_DOCS);
        continue;
      }

      let scorer = scorer.as_mut().unwrap();
      let bulk_scorer = bulk_scorer.as_mut().unwrap();

      let mut up_to = 0;

      loop {
        let min = up_to + r.random_range(0..5);
        let v = r.random_bool(0.5);
        let max = min + 1 + r.random_range(if v { 0..10 } else { 0..5000 });

        if scorer.doc_id()? < min {
          scorer.iterator_mut().advance(min)?;
        }

        let mut collector = LeafCollectorImpl4::new(scorer, min, max);

        let next = bulk_scorer.score(&mut collector, None::<&dyn Bits>, min, max)?;

        debug_assert!(max <= next);
        debug_assert!(next <= scorer.doc_id()?);

        up_to = max;

        if scorer.doc_id()? == NO_MORE_DOCS {
          let mut collector = LeafCollectorImpl3;
          bulk_scorer.score(&mut collector, None::<&dyn Bits>, up_to, NO_MORE_DOCS)?;
          break;
        }
      }
    }

    Ok(())
  }

  pub fn check_count<IRC>(query: Query, searcher: &IndexSearcher<IRC>) -> Result<()>
  where
    IRC: IndexReaderContext,
  {
    let query = searcher.rewrite(query)?;
    let weight = searcher.create_weight(query, ScoreMode::CompleteNoScores, 1.0)?;

    for ctx in searcher.get_leaf_contexts()? {
      let mut scorer = match weight.bulk_scorer(ctx, searcher)? {
        Some(s) => s,
        None => continue,
      };

      let mut collector = LeafCollectorImpl2::new();
      let bits = ctx.reader().get_live_docs()?;
      let live_docs = bits.as_ref().map(|b| b as &dyn Bits);
      scorer.score(&mut collector, live_docs, 0, NO_MORE_DOCS)?;

      let expected_count = collector.expected_count;
      let doc_id_stream = collector.doc_id_stream;

      if !doc_id_stream {
        continue;
      }

      let mut scorer = match weight.bulk_scorer(ctx, searcher)? {
        Some(s) => s,
        None => {
          assert_eq!(0, expected_count);
          continue;
        },
      };

      let mut collector = LeafCollectorImpl::new();
      let bits = ctx.reader().get_live_docs()?;
      let live_docs = bits.as_ref().map(|b| b as &dyn Bits);
      scorer.score(&mut collector, live_docs, 0, NO_MORE_DOCS)?;

      assert_eq!(expected_count, collector.actual_count);
    }

    Ok(())
  }
}

struct LeafCollectorImpl {
  actual_count: i32,
}
impl LeafCollectorImpl {
  fn new() -> Self {
    Self { actual_count: 0 }
  }
}

impl Display for LeafCollectorImpl {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

impl LeafCollector for LeafCollectorImpl {
  fn collect(&mut self, _doc: i32, _scorer: &mut dyn Scorable) -> Result<()> {
    self.actual_count += 1;
    Ok(())
  }

  fn collect_stream(
    &mut self,
    stream: &mut dyn DocIdStream,
    scorer: &mut dyn Scorable,
  ) -> Result<()> {
    self.actual_count += stream.count(scorer)?;
    Ok(())
  }
}

struct LeafCollectorImpl2 {
  expected_count: i32,
  doc_id_stream: bool,
}

impl LeafCollectorImpl2 {
  fn new() -> Self {
    Self {
      expected_count: 0,
      doc_id_stream: false,
    }
  }
}

impl Display for LeafCollectorImpl2 {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

impl LeafCollector for LeafCollectorImpl2 {
  fn collect(&mut self, _doc: i32, _scorer: &mut dyn Scorable) -> Result<()> {
    self.expected_count += 1;
    Ok(())
  }

  fn collect_stream(
    &mut self,
    stream: &mut dyn DocIdStream,
    scorer: &mut dyn Scorable,
  ) -> Result<()> {
    self.doc_id_stream = true;
    self.default_collect_stream(stream, scorer)
  }
}
struct LeafCollectorImpl3;

impl Display for LeafCollectorImpl3 {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

impl LeafCollector for LeafCollectorImpl3 {
  fn collect(&mut self, _doc: i32, _scorer: &mut dyn Scorable) -> Result<()> {
    debug_assert!(false);
    Ok(())
  }
}
struct LeafCollectorImpl4<'a, S> {
  scorer: &'a mut S,
  min: i32,
  max: i32,
}
impl<'a, S> LeafCollectorImpl4<'a, S>
where
  S: Scorer,
{
  fn new(scorer: &'a mut S, min: i32, max: i32) -> Self {
    Self { scorer, min, max }
  }
}

impl<'a, S> Display for LeafCollectorImpl4<'a, S>
where
  S: Scorer,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

impl<'a, S> LeafCollector for LeafCollectorImpl4<'a, S>
where
  S: Scorer,
{
  fn collect(&mut self, doc: i32, scorer2: &mut dyn Scorable) -> Result<()> {
    debug_assert!(doc >= self.min);
    debug_assert!(doc < self.max);

    debug_assert_eq!(self.scorer.doc_id()?, doc);
    debug_assert!((self.scorer.score()? - scorer2.score()?).abs() <= 0.01);

    self.scorer.iterator_mut().next_doc()?;
    Ok(())
  }
}

struct SimpleCollectorImp<'a, IRC>
where
  IRC: 'static,
{
  weight: QueryWeight<IRC>,
  leaf_ptr: usize,
  interval_times32: i64,
  last_doc: i32,
  max_diff: f32,
  rewritten: Query,
  s: &'a IndexSearcher<IRC>,
  last_reader_idx: Option<usize>,
}
impl<'a, IRC> SimpleCollectorImp<'a, IRC>
where
  IRC: IndexReaderContext,
  IRC::LeafReader: Clone,
{
  fn new(
    weight: QueryWeight<IRC>,
    max_diff: f32,
    rewritten: Query,
    searcher: &'a IndexSearcher<IRC>,
  ) -> Self {
    Self {
      weight,
      leaf_ptr: 0,
      interval_times32: 32,
      last_doc: -1,
      max_diff,
      rewritten,
      s: searcher,
      last_reader_idx: None,
    }
  }
}
impl<IRC> Display for SimpleCollectorImp<'_, IRC>
where
  IRC: IndexReaderContext,
  IRC::LeafReader: Clone,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

impl<IRC> Collector for SimpleCollectorImp<'_, IRC>
where
  IRC: IndexReaderContext,
  IRC::LeafReader: Clone,
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
    SimpleCollectorImp::do_set_next_reader(self, context)?;
    Ok(self)
  }

  fn score_mode(&self) -> ScoreMode {
    ScoreMode::Complete
  }
}

impl<IRC> LeafCollector for SimpleCollectorImp<'_, IRC>
where
  IRC: IndexReaderContext,
  IRC::LeafReader: Clone,
{
  fn collect(&mut self, doc: i32, scorable: &mut dyn Scorable) -> Result<()> {
    let score = scorable.score()?;
    let mut i = self.last_doc + 1;
    while i <= doc {
      let ctx = &self.s.get_leaf_contexts()?[self.leaf_ptr];
      let mut supplier = self
        .weight
        .scorer_supplier(ctx, self.s)?
        .ok_or_else(|| LuceneError::illegal_state("scorer supplier is None"))?;

      let mut scorer = supplier.get(1, ctx, self.s)?;

      debug_assert!(
        scorer.iterator_mut().advance(i)? != NO_MORE_DOCS,
        "query collected {} but advance({}) says no more docs!",
        doc,
        i
      );

      debug_assert_eq!(
        doc,
        scorer.doc_id()?,
        "query collected {} but advance({}) got to {}",
        doc,
        i,
        scorer.doc_id()?
      );

      let advance_score = scorer.score()?;

      debug_assert!(
        (advance_score - scorer.score()?).abs() <= self.max_diff,
        "unstable advance({}) score!",
        i
      );

      debug_assert!(
        (score - advance_score).abs() <= self.max_diff,
        "query assigned doc {} a score of <{}> but advance({}) has <{}>!",
        doc,
        score,
        i,
        advance_score
      );

      let step = self.interval_times32 / 1024;
      self.interval_times32 += 1;
      i += step as i32;
    }

    self.last_doc = doc;
    Ok(())
  }
}

impl<IRC> SimpleCollector for SimpleCollectorImp<'_, IRC>
where
  IRC: IndexReaderContext,
  IRC::LeafReader: Clone,
{
  fn do_set_next_reader<LR>(&mut self, _context: &LeafReaderContext<LR>) -> Result<()>
  where
    LR: LeafReader,
  {
    if let Some(previous_reader_idx) = self.last_reader_idx {
      let lr = self.s.get_leaf_contexts()?[previous_reader_idx]
        .reader()
        .clone();
      let mut index_searcher = new_searcher_with_reader(lr)?;
      index_searcher.set_similarity(self.s.get_similarity().clone());

      let weight =
        index_searcher.create_weight(self.rewritten.clone(), ScoreMode::Complete, 1.0)?;

      let top = index_searcher.get_top_reader_context();
      let scorer_opt = weight.scorer(top, &index_searcher)?;

      if let Some(mut scorer) = scorer_opt {
        let mut more = false;
        let context_ord = _context.ord;
        let bits = self.s.get_leaf_contexts()?[context_ord]
          .reader()
          .get_live_docs()?;
        let live_docs = bits.as_ref().map(|b| b as &dyn Bits);

        {
          let mut iterator = scorer.iterator_mut();
          let mut d = iterator.advance(self.last_doc + 1)?;

          while d != NO_MORE_DOCS {
            if match live_docs {
              None => true,
              Some(b) => b.get(d as usize)?,
            } {
              more = true;
              break;
            }
            d = iterator.next_doc()?;
          }
        }

        debug_assert!(
          !more,
          "query's last doc was {} but advance({}) got to {}",
          self.last_doc,
          self.last_doc + 1,
          scorer.doc_id()?
        );
      }

      self.leaf_ptr += 1;
    }

    self.last_reader_idx = Some(_context.ord);
    self.last_doc = -1;

    Ok(())
  }
}
struct SimpleCollectorImpl2<'a, IRC>
where
  IRC: IndexReaderContext + 'static,
  IRC::LeafReader: Clone,
{
  scorer: Option<Box<dyn Scorer>>,
  leaf_ptr: usize,

  q: Query,
  s: &'a IndexSearcher<IRC>,
  max_diff: f32,

  order: &'a [i32],
  opidx: usize,
  skip_op: i32,

  reader_context_array: &'a [LeafReaderContext<IRCLeafReader<IRC>>],

  last_reader_idx: Option<usize>,
  last_doc: i32,
}

impl<'a, IRC> SimpleCollectorImpl2<'a, IRC>
where
  IRC: IndexReaderContext,
  IRC::LeafReader: Clone,
{
  fn new(
    q: Query,
    s: &'a IndexSearcher<IRC>,
    max_diff: f32,
    order: &'a [i32],
    skip_op: i32,
    reader_context_array: &'a [LeafReaderContext<IRCLeafReader<IRC>>],
  ) -> Self {
    Self {
      scorer: None,
      leaf_ptr: 0,
      q,
      s,
      max_diff,
      order,
      opidx: 0,
      skip_op,
      reader_context_array,
      last_reader_idx: None,
      last_doc: -1,
    }
  }
}

impl<IRC> Display for SimpleCollectorImpl2<'_, IRC>
where
  IRC: IndexReaderContext,
  IRC::LeafReader: Clone,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

impl<IRC> Collector for SimpleCollectorImpl2<'_, IRC>
where
  IRC: IndexReaderContext,
  IRC::LeafReader: Clone,
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
    self.do_set_next_reader(context)?;
    Ok(self)
  }

  fn score_mode(&self) -> ScoreMode {
    ScoreMode::Complete
  }
}

impl<IRC> LeafCollector for SimpleCollectorImpl2<'_, IRC>
where
  IRC: IndexReaderContext,
  IRC::LeafReader: Clone,
{
  fn collect(&mut self, doc: i32, sc: &mut dyn Scorable) -> Result<()> {
    let score = sc.score()?;

    self.last_doc = doc;

    if self.scorer.is_none() {
      let rewritten = self.s.rewrite(self.q.clone())?;
      let w = self.s.create_weight(rewritten, ScoreMode::Complete, 1.0)?;
      let ctx = &self.reader_context_array[self.leaf_ptr];

      let scorer = w.scorer(ctx, self.s)?.unwrap();
      self.scorer = Some(scorer);
    }

    let op = self.order[self.opidx % self.order.len()];
    self.opidx += 1;

    let more = if op == self.skip_op {
      let doc_id = self.scorer.as_mut().unwrap().doc_id()?;
      self
        .scorer
        .as_mut()
        .unwrap()
        .iterator_mut()
        .advance(doc_id + 1)?
        != NO_MORE_DOCS
    } else {
      self.scorer.as_mut().unwrap().iterator_mut().next_doc()? != NO_MORE_DOCS
    };
    let scorer = self.scorer.as_mut().unwrap();
    let scorer_doc = scorer.doc_id()?;
    let scorer_score = scorer.score()?;
    let scorer_score2 = scorer.score()?;

    let score_diff = (score - scorer_score).abs();
    let scorer_diff = (scorer_score2 - scorer_score).abs();

    debug_assert!(more);
    debug_assert_eq!(scorer_doc, doc);
    debug_assert!(score_diff <= self.max_diff);
    debug_assert!(scorer_diff <= self.max_diff);

    Ok(())
  }
}

impl<IRC> SimpleCollector for SimpleCollectorImpl2<'_, IRC>
where
  IRC: IndexReaderContext,
  IRC::LeafReader: Clone,
{
  fn do_set_next_reader<LR>(&mut self, context: &LeafReaderContext<LR>) -> Result<()>
  where
    LR: LeafReader,
  {
    if let Some(previous_reader_idx) = self.last_reader_idx {
      let lr = self.s.get_leaf_contexts()?[previous_reader_idx]
        .reader()
        .clone();

      let mut index_searcher = new_searcher_with_reader(lr)?;
      index_searcher.set_similarity(self.s.get_similarity().clone());

      let rewritten = index_searcher.rewrite(self.q.clone())?;
      let weight = index_searcher.create_weight(rewritten, ScoreMode::Complete, 1.0)?;

      let ctx = index_searcher.get_top_reader_context();
      let scorer_opt = weight.scorer(ctx, &index_searcher)?;

      if let Some(mut scorer) = scorer_opt {
        let mut more = false;
        {
          let mut iterator = scorer.iterator_mut();

          let context_ord = context.ord;
          let bits = self.s.get_leaf_contexts()?[context_ord]
            .reader()
            .get_live_docs()?;
          let live_docs = bits.as_ref().map(|b| b as &dyn Bits);

          let mut d = iterator.advance(self.last_doc + 1)?;

          while d != NO_MORE_DOCS {
            if match live_docs {
              None => true,
              Some(b) => b.get(d as usize)?,
            } {
              more = true;
              break;
            }
            d = iterator.next_doc()?;
          }
        }

        debug_assert!(
          !more,
          "query's last doc was {} but advance({}) got to {}",
          self.last_doc,
          self.last_doc + 1,
          scorer.doc_id()?
        );
      }

      self.leaf_ptr += 1;
    }

    self.last_reader_idx = Some(context.ord);

    debug_assert!(
      self.reader_context_array[self.leaf_ptr].base().identity == context.base().identity
    );
    self.scorer = None;
    self.last_doc = -1;

    Ok(())
  }
}
