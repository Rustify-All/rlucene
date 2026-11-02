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
use crate::core::search::query::QueryRef;
use std::fmt::Write as _;

use crate::core::index::bytes_ref::BytesRefValueEnum;
use crate::core::index::index_reader::Identity;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader::{LRPosting, LeafReader};
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::postings_enum::PostingsEnum;
use crate::core::index::slow_impacts_enum::SlowImpactsEnum;
use crate::core::index::term::Term;
use crate::core::index::term_states::{TermStates, build, build_with_stats};
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::search::boolean_clause::Occur;
use crate::core::search::boolean_query::Builder as BooleanQueryBuilder;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::exact_phrase_matcher::ExactPhraseMatcher;
use crate::core::search::index_searcher::{IndexSearcher, get_max_clause_count};
use crate::core::search::match_no_docs_query::MatchNoDocsQuery;
use crate::core::search::phrase_matcher::PhraseMatcherEnum;
use crate::core::search::phrase_query::{PostingsAndFreq, term_positions_cost};
use crate::core::search::phrase_weight::{
  PhraseWeight, PhraseWeightBase, PhraseWeightMeta, SimScorerImpl, SimScorerType,
};
use crate::core::search::query::{Query, QueryBase};
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::sloppy_phrase_matcher::SloppyPhraseMatcher;
use crate::core::search::term_query::TermQuery;
use crate::core::util::HasIdentity;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::priority_queue::{Compare, PriorityQueue};
use smallvec::SmallVec;
use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// A generalized version of [`PhraseQuery`](crate::core::search::phrase_query::PhraseQuery), with the possibility of adding
/// more than one term at the same position that are treated as a disjunction (OR).
#[derive(Clone)]
pub struct MultiPhraseQuery {
  id: Identity,
  slop: i32,
  data: Arc<MultiPhraseQueryData>,
}

struct MultiPhraseQueryData {
  field: Option<Arc<str>>,
  term_arrays: Vec<Vec<Term>>,
  positions: Vec<i32>,
}

impl std::fmt::Debug for MultiPhraseQuery {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("MultiPhraseQuery")
      .field("id", &self.id)
      .field("slop", &self.slop)
      .field("field", &self.data.field)
      .field("term_arrays", &self.data.term_arrays)
      .field("positions", &self.data.positions)
      .finish()
  }
}

impl PartialEq for MultiPhraseQuery {
  fn eq(&self, other: &Self) -> bool {
    self.slop == other.slop
      && term_arrays_equals(&self.data.term_arrays, &other.data.term_arrays)
      && self.data.positions == other.data.positions
  }
}

impl Eq for MultiPhraseQuery {}

impl Hash for MultiPhraseQuery {
  fn hash<H: Hasher>(&self, state: &mut H) {
    self.slop.hash(state);
    for term_array in self.data.term_arrays.iter() {
      term_array.hash(state);
    }
    self.data.positions.hash(state);
  }
}

fn term_arrays_equals(a: &[Vec<Term>], b: &[Vec<Term>]) -> bool {
  if a.len() != b.len() {
    return false;
  }
  for i in 0..a.len() {
    if a[i] != b[i] {
      return false;
    }
  }
  true
}

impl MultiPhraseQuery {
  pub fn get_slop(&self) -> i32 {
    self.slop
  }

  pub fn get_term_arrays(&self) -> &Vec<Vec<Term>> {
    &self.data.term_arrays
  }

  pub fn get_positions(&self) -> &Vec<i32> {
    &self.data.positions
  }

  pub fn builder() -> Builder {
    Builder::new()
  }
}

impl HasIdentity for MultiPhraseQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

/// A builder for multi-phrase queries.
#[derive(Debug, Clone)]
pub struct Builder {
  field: Option<Arc<str>>,
  term_arrays: Vec<Vec<Term>>,
  positions: Vec<i32>,
  slop: i32,
}

impl Default for Builder {
  fn default() -> Self {
    Self::new()
  }
}

impl Builder {
  pub fn new() -> Self {
    Builder {
      field: None,
      term_arrays: Vec::new(),
      positions: Vec::new(),
      slop: 0,
    }
  }

  pub fn from_query(query: &MultiPhraseQuery) -> Self {
    Builder {
      field: query.data.field.clone(),
      term_arrays: query.data.term_arrays.to_vec(),
      positions: query.data.positions.to_vec(),
      slop: query.slop,
    }
  }

  pub fn set_slop(&mut self, s: i32) -> Result<&mut Self> {
    if s < 0 {
      return Err(LuceneError::illegal_argument(
        "slop value cannot be negative",
      ));
    }
    self.slop = s;
    Ok(self)
  }

  pub fn add_term<TermInput>(&mut self, term: TermInput) -> Result<&mut Self>
  where
    TermInput: Into<Term>,
  {
    let term = term.into();
    self.add_terms([term])
  }

  /// Adds alternatives at the next position from an array, vector, or iterator.
  /// Owned terms are moved; borrowed terms are cloned.
  /// An empty input can be written as `std::iter::empty::<Term>()`; the first
  /// group of alternatives must not be empty.
  pub fn add_terms<I, T>(&mut self, terms: I) -> Result<&mut Self>
  where
    I: IntoIterator<Item = T>,
    T: Into<Term>,
  {
    let position = if self.positions.is_empty() {
      0
    } else {
      self.positions[self.positions.len() - 1] + 1
    };
    self.add_terms_with_position(terms, position)
  }

  /// Adds alternatives at an explicit position. Owned terms are moved;
  /// borrowed terms are cloned. Accepts arrays, vectors, and iterators.
  pub fn add_terms_with_position<I, T>(&mut self, terms: I, position: i32) -> Result<&mut Self>
  where
    I: IntoIterator<Item = T>,
    T: Into<Term>,
  {
    let terms: Vec<Term> = terms.into_iter().map(Into::into).collect();
    if self.term_arrays.is_empty() {
      let first_term = terms
        .first()
        .ok_or_else(|| LuceneError::array_index_out_of_bounds("Term array must not be empty"))?;
      self.field = Some(Arc::from(first_term.field()));
    }
    let field = self
      .field
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("field is not set"))?;
    for term in &terms {
      if term.field() != field.as_ref() {
        return Err(LuceneError::illegal_argument(format!(
          "All phrase terms must be in the same field ({}): {}",
          field, term
        )));
      }
    }
    self.term_arrays.push(terms);
    self.positions.push(position);
    Ok(self)
  }

  pub fn build(self) -> MultiPhraseQuery {
    MultiPhraseQuery {
      id: Identity::new(),
      slop: self.slop,
      data: Arc::new(MultiPhraseQueryData {
        field: self.field,
        term_arrays: self.term_arrays,
        positions: self.positions,
      }),
    }
  }
}

impl QueryBase for MultiPhraseQuery {
  type Weight<IRC, SIM>
    = PhraseWeight<MultiPhraseQueryWeightBase<SIM>, SIM>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static;

  fn to_string(&self, f: &str) -> Result<String> {
    let mut buffer = String::new();
    match self.data.field.as_deref() {
      Some(field) if field == f => {},
      Some(field) => {
        buffer.push_str(field);
        buffer.push(':');
      },
      None => buffer.push_str("null:"),
    }
    buffer.push('"');
    let mut last_pos: i32 = -1;
    for i in 0..self.data.term_arrays.len() {
      let terms = &self.data.term_arrays[i];
      let position = self.data.positions[i];
      if i != 0 {
        buffer.push(' ');
        for _j in 1..(position - last_pos) {
          buffer.push_str("? ");
        }
      }
      if terms.len() > 1 {
        buffer.push('(');
        for (j, term) in terms.iter().enumerate() {
          if j > 0 {
            buffer.push(' ');
          }
          buffer.push_str(&term.text().unwrap_or(Cow::Borrowed("None")));
        }
        buffer.push(')');
      } else {
        buffer.push_str(&terms[0].text().unwrap_or(Cow::Borrowed("None")));
      }
      last_pos = position;
    }
    buffer.push('"');
    if self.slop != 0 {
      buffer.push('~');
      write!(buffer, "{}", self.slop)?;
    }
    Ok(buffer)
  }

  fn create_weight<IRC, SIM>(
    self,
    searcher: &IndexSearcher<IRC, SIM>,
    score_mode: &ScoreMode,
    boost: f32,
  ) -> Result<Self::Weight<IRC, SIM>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    Self: Sized,
    IndexSearcher<IRC, SIM>: Sync,
  {
    let similarity = searcher.get_similarity();
    let field = self
      .data
      .field
      .as_deref()
      .map(str::to_owned)
      .ok_or_else(|| LuceneError::illegal_state("MultiPhraseQuery field is not set"))?;
    let base = PhraseWeightMeta::new(field, *score_mode, similarity);
    let sub = MultiPhraseQueryWeightBase::new(self, boost, base);
    let weight = PhraseWeight::new(searcher, sub)?;
    Ok(weight)
  }

  fn rewrite<IRC, SIM>(&self, _searcher: &IndexSearcher<IRC, SIM>) -> Result<Option<Query>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    Self: Sized,
    IndexSearcher<IRC, SIM>: Sync,
  {
    if self.data.term_arrays.is_empty() {
      Ok(Some(
        MatchNoDocsQuery::with_reason("empty MultiPhraseQuery").into(),
      ))
    } else if self.data.term_arrays.len() == 1 {
      let mut builder = BooleanQueryBuilder::new();
      if self.data.term_arrays[0].len() <= get_max_clause_count() {
        builder
          .clauses
          .reserve_exact(self.data.term_arrays[0].len());
      }
      for term in &self.data.term_arrays[0] {
        builder.add(TermQuery::new(term.clone()), Occur::Should)?;
      }
      Ok(Some(builder.build().into()))
    } else {
      Ok(None)
    }
  }

  fn visit<QV>(&self, visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    let Some(field) = self.data.field.as_deref() else {
      return Ok(());
    };
    if !visitor.accept_field(field) {
      return Ok(());
    }
    let query = self.into();
    let mut visitor = visitor.get_sub_visitor(Occur::Must, query);
    for terms in self.data.term_arrays.iter() {
      let mut sub_visitor = visitor.get_sub_visitor(Occur::Should, query);
      sub_visitor.consume_terms(query, terms)?;
    }
    Ok(())
  }
}

pub struct MultiPhraseQueryWeightBase<
  SIM = crate::core::search::similarities_impl::bm25_similarity::BM25Similarity,
> where
  SIM: Similarity,
{
  query: MultiPhraseQuery,
  term_states: HashMap<Arc<Term>, TermStates>,
  boost: f32,
  base: PhraseWeightMeta<SIM>,
}

impl<SIM> MultiPhraseQueryWeightBase<SIM>
where
  SIM: Similarity,
{
  pub(crate) fn new(query: MultiPhraseQuery, boost: f32, base: PhraseWeightMeta<SIM>) -> Self {
    Self {
      query,
      term_states: HashMap::new(),
      boost,
      base,
    }
  }
}

impl<SIM> PhraseWeightBase<SIM> for MultiPhraseQueryWeightBase<SIM>
where
  SIM: Similarity,
{
  type SimScorer = Arc<SimScorerType<SIM>>;
  type IE<LR: LeafReader> = SlowImpactsEnum<MultiPhrasePostingsEnum<LRPosting<LR>>>;

  fn get_stats<IRC>(&mut self, searcher: &IndexSearcher<IRC, SIM>) -> Result<Self::SimScorer>
  where
    IRC: IndexReaderContext + 'static,
  {
    let mut all_term_stats: SmallVec<[_; 8]> =
      SmallVec::with_capacity(if self.base.score_mode.needs_scores() {
        self.query.data.term_arrays.len()
      } else {
        0
      });

    for terms in &*self.query.data.term_arrays {
      for term in terms {
        if !self.term_states.contains_key(term) {
          let term = Arc::new(term.clone());
          let ts = if self.base.score_mode.needs_scores() {
            build_with_stats(searcher, &term)?
          } else {
            build(searcher, term.clone(), false)?
          };
          self.term_states.insert(term, ts);
        }
        if self.base.score_mode.needs_scores() {
          let (term, ts) = self
            .term_states
            .get_key_value(term)
            .ok_or_else(|| LuceneError::illegal_state("term state should have been built"))?;
          if ts.doc_freq()? > 0 {
            let stats =
              searcher.term_statistics(term.clone(), ts.doc_freq()?, ts.total_term_freq()?)?;
            if all_term_stats.len() == all_term_stats.capacity() {
              // Match Vec's doubling when alternatives exceed the position count.
              all_term_stats.reserve_exact(all_term_stats.len());
            }
            all_term_stats.push(stats);
          }
        }
      }
    }

    if all_term_stats.is_empty() {
      Ok(Arc::new(SimScorerType::<SIM>::B(SimScorerImpl)))
    } else {
      let collection_stats = searcher
        .collection_statistics(&self.base.field)?
        .ok_or_else(|| LuceneError::illegal_state("could not get collection stats"))?;
      Ok(Arc::new(SimScorerType::<SIM>::A(
        self
          .base
          .similarity
          .scorer(self.boost, &collection_stats, &all_term_stats)?,
      )))
    }
  }

  fn get_phrase_matcher<LR, const TOP_SCORES: bool>(
    &self,
    context: &LeafReaderContext<LR>,
    scorer: Self::SimScorer,
    expose_offsets: bool,
  ) -> Result<Option<PhraseMatcherEnum<Self::IE<LR>, Self::SimScorer, TOP_SCORES>>>
  where
    LR: LeafReader,
  {
    debug_assert!(!self.query.data.term_arrays.is_empty());
    let reader = context.reader();

    let field_terms = match reader.terms(&self.base.field)? {
      Some(t) => t,
      None => return Ok(None),
    };

    if !field_terms.has_positions() {
      return Err(LuceneError::illegal_state(format!(
        "field \"{}\" was indexed without position data; cannot run MultiPhraseQuery (phrase={})",
        self.base.field,
        self.query.to_string(&self.base.field)?
      )));
    }

    let mut te = field_terms.iterator()?;
    let mut total_match_cost: f32 = 0.0;

    let mut postings_freqs: Vec<PostingsAndFreq<'_, Self::IE<LR>>> =
      Vec::with_capacity(self.query.data.term_arrays.len());

    let postings_flags = if expose_offsets {
      crate::core::index::postings_enum::ALL as i32
    } else {
      crate::core::index::postings_enum::POSITIONS as i32
    };

    for pos in 0..self.query.data.term_arrays.len() {
      let terms = &self.query.data.term_arrays[pos];
      let mut posting_enums: Vec<LRPosting<LR>> = Vec::with_capacity(terms.len());

      for term in terms {
        let ts = match self.term_states.get(term) {
          Some(ts) => ts,
          None => continue,
        };

        let mut supplier = ts.get(context)?;
        let term_state = match supplier {
          None => None,
          Some(ref mut s) => ts.resolve(s)?,
        };

        let terms_state = match term_state {
          None => continue,
          Some(s) => s,
        };

        te.seek_exact_with_state(term.bytes(), terms_state.as_ref())?;

        let pe = te.postings_with_flags(None, postings_flags)?;
        posting_enums.push(pe);
        total_match_cost += term_positions_cost(&mut te)?;
      }

      if posting_enums.is_empty() {
        return Ok(None);
      }

      let postings_enum = if posting_enums.len() == 1 {
        MultiPhrasePostingsEnum::Single(posting_enums.remove(0))
      } else if expose_offsets {
        MultiPhrasePostingsEnum::UnionFull(UnionFullPostingsEnum::new(posting_enums)?)
      } else {
        MultiPhrasePostingsEnum::Union(UnionPostingsEnum::new(posting_enums)?)
      };

      postings_freqs.push(PostingsAndFreq::new(
        SlowImpactsEnum::new(postings_enum),
        self.query.data.positions[pos],
        terms,
      ));
    }

    let v = if self.query.slop == 0 {
      postings_freqs.sort();
      PhraseMatcherEnum::Exact(ExactPhraseMatcher::new(
        postings_freqs,
        self.base.score_mode,
        scorer,
        total_match_cost,
      )?)
    } else {
      PhraseMatcherEnum::Sloppy(SloppyPhraseMatcher::new(
        postings_freqs,
        self.query.slop as usize,
        scorer,
        total_match_cost,
        expose_offsets,
      )?)
    };
    Ok(Some(v))
  }

  fn get_query(&self) -> QueryRef<'_> {
    (&self.query).into()
  }

  fn base(&self) -> &PhraseWeightMeta<SIM> {
    &self.base
  }
}

/// Takes the logical union of multiple PostingsEnum iterators.
///
/// Note: positions are merged during freq()
pub struct UnionPostingsEnum<P> {
  docs_queue: PriorityQueue<usize, DocsQueueCmp<P>>,
  cost: i64,
  pos_queue: PositionsQueue,
  pos_queue_doc: i32,
  #[cfg(debug_assertions)]
  initial_docs_queue_size: usize,
  #[cfg(debug_assertions)]
  initial_subs_len: usize,
}

impl<P> UnionPostingsEnum<P>
where
  P: PostingsEnum,
{
  pub fn new(subs: Vec<P>) -> Result<Self> {
    // subs should never be empty
    if subs.is_empty() {
      return Err(LuceneError::illegal_argument("subs must not be empty"));
    }
    let size = subs.len();
    let mut cost = 0;
    let cmp = DocsQueueCmp::new(subs);
    let mut docs_queue = PriorityQueue::new(size, cmp)?;
    for pe in docs_queue.compare.subs.iter() {
      cost += pe.cost()?;
    }
    for i in 0..size {
      docs_queue.add(i)?;
    }
    #[cfg(debug_assertions)]
    let initial_docs_queue_size = docs_queue.size();
    #[cfg(debug_assertions)]
    let initial_subs_len = docs_queue.compare.subs.len();
    Ok(UnionPostingsEnum {
      docs_queue,
      cost,
      pos_queue: PositionsQueue::new(),
      pos_queue_doc: -2,
      #[cfg(debug_assertions)]
      initial_docs_queue_size,
      #[cfg(debug_assertions)]
      initial_subs_len,
    })
  }

  fn top_idx(&self) -> usize {
    #[cfg(debug_assertions)]
    self.debug_assert_docs_queue_unchanged();
    *expect_invariant!(
      self.docs_queue.top(),
      "a union postings enum retains its non-empty documents queue"
    )
  }

  fn update_top_idx(&mut self) -> Result<usize> {
    let top_idx = *self.docs_queue.update_top()?;
    #[cfg(debug_assertions)]
    self.debug_assert_docs_queue_unchanged();
    Ok(top_idx)
  }

  fn subs(&self) -> &[P] {
    #[cfg(debug_assertions)]
    self.debug_assert_docs_queue_unchanged();
    &self.docs_queue.compare.subs
  }

  fn subs_mut(&mut self) -> &mut [P] {
    #[cfg(debug_assertions)]
    self.debug_assert_docs_queue_unchanged();
    &mut self.docs_queue.compare.subs
  }

  #[cfg(debug_assertions)]
  fn debug_assert_docs_queue_unchanged(&self) {
    debug_assert_eq!(self.initial_docs_queue_size, self.docs_queue.size());
    debug_assert_eq!(self.initial_subs_len, self.docs_queue.compare.subs.len());
  }
}

impl<P> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for UnionPostingsEnum<P>
where
  P: PostingsEnum,
{
}
impl<P> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for UnionPostingsEnum<P> where
  P: PostingsEnum
{
}

impl<P> DocIdSetIterator for UnionPostingsEnum<P>
where
  P: PostingsEnum,
{
  fn doc_id(&self) -> i32 {
    let index = self.top_idx();
    self.subs()[index].doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    let mut top = self.top_idx();
    let doc = self.docs_queue.compare.subs[top].doc_id();
    loop {
      self.docs_queue.compare.subs[top].next_doc()?;
      top = self.update_top_idx()?;
      if self.docs_queue.compare.subs[top].doc_id() != doc {
        return Ok(self.docs_queue.compare.subs[top].doc_id());
      }
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    let mut top = self.top_idx();
    loop {
      self.docs_queue.compare.subs[top].advance(target)?;
      top = self.update_top_idx()?;
      let doc = self.docs_queue.compare.subs[top].doc_id();
      if doc >= target {
        return Ok(doc);
      }
    }
  }

  fn cost(&self) -> Result<i64> {
    Ok(self.cost)
  }
}

impl<P> PostingsEnum for UnionPostingsEnum<P>
where
  P: PostingsEnum,
{
  fn freq(&mut self) -> Result<i32> {
    let doc = self.doc_id();
    if doc != self.pos_queue_doc {
      self.pos_queue.clear();
      for sub in &mut self.docs_queue.compare.subs {
        if sub.doc_id() == doc {
          let freq = sub.freq()?;
          for _ in 0..freq {
            let pos = sub.next_position()?;
            self.pos_queue.add(pos);
          }
        }
      }
      #[cfg(debug_assertions)]
      self.debug_assert_docs_queue_unchanged();
      self.pos_queue.sort();
      self.pos_queue_doc = doc;
    }
    Ok(self.pos_queue.size() as i32)
  }

  fn next_position(&mut self) -> Result<i32> {
    Ok(self.pos_queue.next())
  }

  fn start_offset(&self) -> Result<i32> {
    Ok(-1)
  }

  fn end_offset(&self) -> Result<i32> {
    Ok(-1)
  }

  fn get_payload(&self) -> Result<Option<BytesRefValueEnum<'_>>> {
    Ok(None)
  }
}

struct DocsQueueCmp<P> {
  subs: Box<[P]>,
}
impl<P> DocsQueueCmp<P> {
  pub fn new(subs: Vec<P>) -> Self {
    Self {
      subs: subs.into_boxed_slice(),
    }
  }
}
impl<P> Compare<usize> for DocsQueueCmp<P>
where
  P: PostingsEnum,
{
  fn less_than(&self, a: &usize, b: &usize) -> Result<bool> {
    let a_doc = self.subs[*a].doc_id();
    let b_doc = self.subs[*b].doc_id();
    Ok(a_doc < b_doc)
  }
}

/// Queue of terms for a single document.
/// It's a sorted array of all the positions from all the postings.
struct PositionsQueue {
  index: usize,
  size: usize,
  array: Vec<i32>,
}

impl PositionsQueue {
  fn new() -> Self {
    Self {
      index: 0,
      size: 0,
      array: vec![0; 16],
    }
  }

  fn add(&mut self, i: i32) {
    if self.size == self.array.len() {
      self.grow_array();
    }

    self.array[self.size] = i;
    self.size += 1;
  }

  fn next(&mut self) -> i32 {
    let val = self.array[self.index];
    self.index += 1;
    val
  }

  fn sort(&mut self) {
    self.array[self.index..self.size].sort_unstable();
  }

  fn clear(&mut self) {
    self.index = 0;
    self.size = 0;
  }

  fn size(&self) -> usize {
    self.size
  }

  fn grow_array(&mut self) {
    self.array.resize(self.array.len() * 2, 0);
  }
}
struct PostingsAndPosition {
  pe: usize,
  pos: i32,
  upto: i32,
}

struct PosQueueCmp;

impl Compare<PostingsAndPosition> for PosQueueCmp {
  fn less_than(&self, a: &PostingsAndPosition, b: &PostingsAndPosition) -> Result<bool> {
    Ok(a.pos < b.pos)
  }
}

/// Slower version of UnionPostingsEnum that delegates offsets and positions.
pub struct UnionFullPostingsEnum<P> {
  base: UnionPostingsEnum<P>,
  freq: i32,
  started: bool,
  pos_queue: PriorityQueue<PostingsAndPosition, PosQueueCmp>,
}

impl<P> UnionFullPostingsEnum<P>
where
  P: PostingsEnum,
{
  pub fn new(subs: Vec<P>) -> Result<Self> {
    let size = subs.len();

    let base = UnionPostingsEnum::new(subs)?;

    Ok(Self {
      base,
      freq: -1,
      started: false,
      pos_queue: PriorityQueue::new(size, PosQueueCmp)?,
    })
  }
}

impl<P> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for UnionFullPostingsEnum<P>
where
  P: PostingsEnum,
{
}
impl<P> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for UnionFullPostingsEnum<P> where
  P: PostingsEnum
{
}

impl<P> DocIdSetIterator for UnionFullPostingsEnum<P>
where
  P: PostingsEnum,
{
  fn doc_id(&self) -> i32 {
    self.base.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.base.next_doc()
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.base.advance(target)
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    self.base.advance(target)
  }

  fn cost(&self) -> Result<i64> {
    self.base.cost()
  }
}

impl<P> PostingsEnum for UnionFullPostingsEnum<P>
where
  P: PostingsEnum,
{
  fn freq(&mut self) -> Result<i32> {
    let doc = self.doc_id();
    if doc == self.base.pos_queue_doc {
      return Ok(self.freq);
    }

    self.freq = 0;
    self.started = false;
    self.pos_queue.clear();
    let postings = self.base.subs_mut();
    for (index, pe) in postings.iter_mut().enumerate() {
      if pe.doc_id() == doc {
        let pos = pe.next_position()?;
        let upto = pe.freq()?;
        self.pos_queue.add(PostingsAndPosition {
          pe: index,
          pos,
          upto,
        })?;
        self.freq += upto;
      }
    }
    Ok(self.freq)
  }

  fn next_position(&mut self) -> Result<i32> {
    if !self.started {
      self.started = true;
      let top = self
        .pos_queue
        .top()
        .ok_or_else(|| LuceneError::illegal_state("pos_queue is empty"))?;
      return Ok(top.pos);
    }

    let top = self
      .pos_queue
      .top_mut()
      .ok_or_else(|| LuceneError::illegal_state("pos_queue is empty"))?;

    if top.upto == 1 {
      self.pos_queue.pop()?;
      let top = self
        .pos_queue
        .top()
        .ok_or_else(|| LuceneError::illegal_state("pos_queue is empty"))?;
      return Ok(top.pos);
    }

    top.pos = self.base.subs_mut()[top.pe].next_position()?;
    top.upto -= 1;
    self.pos_queue.update_top()?;

    let top = self
      .pos_queue
      .top()
      .ok_or_else(|| LuceneError::illegal_state("pos_queue is empty"))?;
    Ok(top.pos)
  }

  fn start_offset(&self) -> Result<i32> {
    let top = self
      .pos_queue
      .top()
      .ok_or_else(|| LuceneError::illegal_state("pos_queue is empty"))?;
    self.base.subs()[top.pe].start_offset()
  }

  fn end_offset(&self) -> Result<i32> {
    let top = self
      .pos_queue
      .top()
      .ok_or_else(|| LuceneError::illegal_state("pos_queue is empty"))?;
    self.base.subs()[top.pe].end_offset()
  }
  fn get_payload(&self) -> Result<Option<BytesRefValueEnum<'_>>> {
    let top = self
      .pos_queue
      .top()
      .ok_or_else(|| LuceneError::illegal_state("pos_queue is empty"))?;
    self.base.subs()[top.pe].get_payload()
  }
}
#[allow(clippy::large_enum_variant)] // Keep postings iteration allocation-free.
pub enum MultiPhrasePostingsEnum<P> {
  Single(P),
  UnionFull(UnionFullPostingsEnum<P>),
  Union(UnionPostingsEnum<P>),
}

impl<P> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for MultiPhrasePostingsEnum<P>
where
  P: PostingsEnum,
{
}
impl<P> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for MultiPhrasePostingsEnum<P>
where
  P: PostingsEnum,
{
}

impl<P> DocIdSetIterator for MultiPhrasePostingsEnum<P>
where
  P: PostingsEnum,
{
  fn doc_id(&self) -> i32 {
    match self {
      Self::Single(postings) => postings.doc_id(),
      Self::UnionFull(postings) => postings.doc_id(),
      Self::Union(postings) => postings.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::Single(postings) => postings.next_doc(),
      Self::UnionFull(postings) => postings.next_doc(),
      Self::Union(postings) => postings.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Single(postings) => postings.advance(target),
      Self::UnionFull(postings) => postings.advance(target),
      Self::Union(postings) => postings.advance(target),
    }
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Single(postings) => postings.slow_advance(target),
      Self::UnionFull(postings) => postings.slow_advance(target),
      Self::Union(postings) => postings.slow_advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::Single(postings) => postings.cost(),
      Self::UnionFull(postings) => postings.cost(),
      Self::Union(postings) => postings.cost(),
    }
  }
}

impl<P> PostingsEnum for MultiPhrasePostingsEnum<P>
where
  P: PostingsEnum,
{
  fn freq(&mut self) -> Result<i32> {
    match self {
      Self::Single(postings) => postings.freq(),
      Self::UnionFull(postings) => postings.freq(),
      Self::Union(postings) => postings.freq(),
    }
  }

  fn next_position(&mut self) -> Result<i32> {
    match self {
      Self::Single(postings) => postings.next_position(),
      Self::UnionFull(postings) => postings.next_position(),
      Self::Union(postings) => postings.next_position(),
    }
  }

  fn start_offset(&self) -> Result<i32> {
    match self {
      Self::Single(postings) => postings.start_offset(),
      Self::UnionFull(postings) => postings.start_offset(),
      Self::Union(postings) => postings.start_offset(),
    }
  }

  fn end_offset(&self) -> Result<i32> {
    match self {
      Self::Single(postings) => postings.end_offset(),
      Self::UnionFull(postings) => postings.end_offset(),
      Self::Union(postings) => postings.end_offset(),
    }
  }

  fn get_payload(&self) -> Result<Option<BytesRefValueEnum<'_>>> {
    match self {
      Self::Single(postings) => postings.get_payload(),
      Self::UnionFull(postings) => postings.get_payload(),
      Self::Union(postings) => postings.get_payload(),
    }
  }
}

impl crate::core::util::accountable::Accountable for MultiPhraseQuery {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(crate::core::util::ram_usage_estimator::QUERY_DEFAULT_RAM_BYTES_USED)
  }
}
