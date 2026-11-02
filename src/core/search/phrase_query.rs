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

use crate::core::index::BytesRef;
use crate::core::index::impacts_enum::ImpactsEnumEnum2;
use crate::core::index::index_reader::Identity;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader::{LRImpactsEnum, LRPosting};
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::postings_enum::{OFFSETS, POSITIONS};
use crate::core::index::slow_impacts_enum::SlowImpactsEnum;
use crate::core::index::term::Term;
use crate::core::index::term_states::{TermStates, build, build_with_stats};
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::search::boolean_clause::Occur;
use crate::core::search::exact_phrase_matcher::ExactPhraseMatcher;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::match_no_docs_query::MatchNoDocsQuery;
use crate::core::search::phrase_matcher::PhraseMatcherEnum;
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
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::lucene_error::Result;
use smallvec::SmallVec;
use std::borrow::{Borrow, Cow};
use std::cmp::Ordering;
use std::fmt::Debug;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct PhraseQuery {
  id: Identity,
  slop: usize,
  terms: Arc<Vec<Term>>,
  positions: Arc<Box<[usize]>>,
}
impl PhraseQuery {
  /// Create a phrase query which will match documents that contain the given
  /// list of terms at consecutive positions in `field`, and at a maximum edit
  /// distance of `slop`.
  ///
  /// Accepts arrays, vectors and iterators of strings or string references.
  /// For an empty query, pass `std::iter::empty::<&str>()`.
  ///
  /// For more complicated use-cases, use [PhraseQuery::builder](Builder).
  ///
  /// # See also
  ///
  /// - [`PhraseQuery::get_slop`]
  pub fn from_terms<I, S>(slop: usize, field: &str, terms: I) -> Result<Self>
  where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
  {
    let terms = to_terms(field, terms);
    let positions = incremental_positions(terms.len());
    PhraseQuery::new(slop, terms, positions)
  }

  /// Create a phrase query which will match documents that contain the given
  /// list of terms at consecutive positions in `field`.
  pub fn from_terms_no_slop<I, S>(field: &str, terms: I) -> Result<Self>
  where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
  {
    Self::from_terms(0, field, terms)
  }

  /// Create a phrase query which will match documents that contain the given
  /// list of terms at consecutive positions in `field`, and at a maximum edit
  /// distance of `slop`.
  ///
  /// Accepts arrays, vectors, or iterators of byte buffers or `BytesRef` values.
  /// For an empty query, pass `std::iter::empty::<BytesRef<Vec<u8>>>()`.
  ///
  /// For more complicated use-cases, use [`PhraseQuery::builder`](Builder).
  ///
  /// # See also
  ///
  /// - [`PhraseQuery::get_slop`]
  pub fn from_bytes<I, B>(slop: usize, field: &str, terms: I) -> Result<Self>
  where
    I: IntoIterator<Item = B>,
    B: Into<BytesRef<Vec<u8>>>,
  {
    let terms = to_terms_from_bytes(field, terms);
    let positions = incremental_positions(terms.len());
    PhraseQuery::new(slop, terms, positions)
  }

  /// Create a phrase query which will match documents that contain the given
  /// list of terms at consecutive positions in `field`.
  pub fn from_bytes_no_slop<I, B>(field: &str, terms: I) -> Result<Self>
  where
    I: IntoIterator<Item = B>,
    B: Into<BytesRef<Vec<u8>>>,
  {
    Self::from_bytes(0, field, terms)
  }

  /// Return the slop for this [`PhraseQuery`].
  ///
  /// The slop is an edit distance between the respective positions of terms as
  /// defined in this [`PhraseQuery`] and the actual positions of these terms in
  /// a document.
  ///
  /// For instance, when searching for `"quick fox"`, it is expected that the
  /// difference between the positions of `fox` and `quick` is `1`. So
  /// `"a quick brown fox"` would be at an edit distance of `1` since the
  /// difference of the positions of `fox` and `quick` is `2`. Similarly,
  /// `"the fox is quick"` would be at an edit distance of `3` since the
  /// difference of the positions of `fox` and `quick` is `-2`.
  ///
  /// The slop defines the maximum edit distance for a document to match this
  /// phrase query.
  ///
  /// More exact matches are scored higher than sloppier matches, so search
  /// results are ordered by exactness.
  pub fn get_slop(&self) -> usize {
    self.slop
  }

  /// Returns the field this query applies to.
  ///
  /// If the query contains no terms, this returns `None`. Otherwise, it
  /// returns the field shared by all terms in this phrase query.
  pub fn get_field(&self) -> Option<&str> {
    self.terms.first().map(Term::field)
  }

  /// Returns the list of terms in this phrase.
  ///
  /// The returned slice preserves the order in which terms were added to the
  /// phrase query. All terms are guaranteed to belong to the same field.
  pub fn get_terms(&self) -> &[Term] {
    &self.terms
  }
  pub fn get_term_arc(&self) -> Arc<Vec<Term>> {
    self.terms.clone()
  }

  /// Returns the relative positions of terms in this phrase.
  ///
  /// The returned slice has the same length as [`get_terms`](Self::get_terms),
  /// and each position corresponds to the term at the same index.
  pub fn get_positions(&self) -> &[usize] {
    &self.positions
  }

  fn new<T>(slop: usize, terms: T, positions: Vec<usize>) -> Result<Self>
  where
    T: Borrow<Vec<Term>> + Into<Arc<Vec<Term>>>,
  {
    let terms_ref = terms.borrow();
    if terms_ref.len() != positions.len() {
      return Err(LuceneError::illegal_argument(
        "Must have as many terms as positions".to_string(),
      ));
    }
    if terms_ref.len() > 1 {
      let field = terms_ref[0].field();
      for term in &terms_ref[1..] {
        if term.field() != field {
          return Err(LuceneError::illegal_argument(
            "All terms should have the same field".to_string(),
          ));
        }
      }
    }

    for i in 1..positions.len() {
      if positions[i] < positions[i - 1] {
        return Err(LuceneError::illegal_argument(format!(
          "Positions should not go backwards, got {} before {}",
          positions[i - 1],
          positions[i]
        )));
      }
    }

    Ok(Self {
      id: Identity::new(),
      slop,
      terms: terms.into(),
      positions: Arc::new(positions.into_boxed_slice()),
    })
  }
}

impl Hash for PhraseQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.slop.hash(state);
    self.terms.hash(state);
    self.positions.hash(state);
  }
}
impl Eq for PhraseQuery {}
impl PartialEq for PhraseQuery {
  fn eq(&self, other: &Self) -> bool {
    self.slop == other.slop && self.terms == other.terms && self.positions == other.positions
  }
}

impl HasIdentity for PhraseQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl QueryBase for PhraseQuery {
  type Weight<IRC, SIM>
    = PhraseWeight<PhraseQueryWeightBase<SIM>, SIM>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static;

  fn to_string(&self, f: &str) -> Result<String> {
    let mut buffer = String::new();

    if let Some(field) = self.get_field()
      && field != f
    {
      buffer.push_str(field);
      buffer.push(':');
    }

    buffer.push('"');

    let max_position = self.positions.last().copied();

    let mut pieces: Vec<Option<Cow<'_, str>>> = match max_position {
      None => Vec::new(),
      Some(max) => vec![None; max + 1],
    };

    for (term, &pos) in self.terms.iter().zip(self.positions.iter()) {
      let text = term.text().unwrap_or(Cow::Borrowed("None"));
      match &mut pieces[pos] {
        None => {
          pieces[pos] = Some(text);
        },
        Some(existing) => {
          let existing = existing.to_mut();
          existing.push('|');
          existing.push_str(&text);
        },
      }
    }

    for (i, piece) in pieces.iter().enumerate() {
      if i > 0 {
        buffer.push(' ');
      }
      match piece {
        None => buffer.push('?'),
        Some(s) => buffer.push_str(s),
      }
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
      .get_field()
      .map(str::to_owned)
      .ok_or_else(|| LuceneError::illegal_state("field is None"))?;
    let base = PhraseWeightMeta::new(field, *score_mode, similarity);
    let sub = PhraseQueryWeightBase::new(self, boost, base);
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
    let len = self.terms.len();
    if len == 0 {
      Ok(Some(
        MatchNoDocsQuery::with_reason("empty PhraseQuery").into(),
      ))
    } else if len == 1 {
      Ok(Some(TermQuery::new(self.terms[0].clone()).into()))
    } else if let Some(&first_pos) = self.positions.first() {
      if first_pos != 0 {
        let mut new_positions = Vec::with_capacity(self.positions.len());
        for &p in self.positions.iter() {
          new_positions.push(p - first_pos);
        }
        Ok(Some(
          PhraseQuery::new(self.slop, self.terms.clone(), new_positions)?.into(),
        ))
      } else {
        Ok(None)
      }
    } else {
      Ok(None)
    }
  }

  fn visit<QV>(&self, visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    let Some(field) = self.get_field() else {
      return Ok(());
    };
    if !visitor.accept_field(field) {
      return Ok(());
    }
    let query = self.into();
    let mut visitor = visitor.get_sub_visitor(Occur::Must, query);
    visitor.consume_terms(query, &self.terms)
  }
}

/// A builder for phrase queries
pub struct Builder {
  slop: usize,
  terms: Vec<Term>,
  positions: Vec<usize>,
}
#[cfg(test)]
impl Clone for Builder {
  fn clone(&self) -> Self {
    Self {
      slop: self.slop,
      terms: self.terms.clone(),
      positions: self.positions.clone(),
    }
  }
}

impl Default for Builder {
  fn default() -> Self {
    Self::new()
  }
}
impl Builder {
  pub fn new() -> Self {
    Self {
      slop: 0,
      terms: Vec::new(),
      positions: Vec::new(),
    }
  }
  pub fn set_slop(&mut self, slop: usize) -> &mut Self {
    self.slop = slop;
    self
  }
  /// Adds a term to the end of the query phrase.
  ///
  /// The relative position of the term is the one immediately after the last
  /// term added.
  pub fn add_term<TermInput>(&mut self, term: TermInput) -> Result<&mut Self>
  where
    TermInput: Into<Term>,
  {
    let term = term.into();
    let position = match self.positions.last() {
      None => 0,
      Some(&last) => last + 1,
    };
    self.add(term, position)
  }
  /// Adds a term to the end of the query phrase.
  ///
  /// The relative position of the term within the phrase is specified explicitly,
  /// but must be greater than or equal to that of the previously added term.
  /// A greater position allows phrases with gaps (e.g. in connection with
  /// stopwords).
  ///
  /// If the position is equal, you most likely should be using
  /// [`MultiPhraseQuery`](crate::core::search::multi_phrase_query::MultiPhraseQuery) instead, which only requires one term at each position
  /// to match; this query requires all of them.
  pub fn add<TermInput>(&mut self, term: TermInput, position: usize) -> Result<&mut Self>
  where
    TermInput: Into<Term>,
  {
    let term = term.into();
    if let Some(&last_position) = self.positions.last()
      && position < last_position
    {
      Err(LuceneError::illegal_argument(format!(
        "Positions must be added in order, got {} after {}",
        position, last_position
      )))?;
    }

    if let Some(first_term) = self.terms.first()
      && term.field() != first_term.field()
    {
      Err(LuceneError::illegal_argument(format!(
        "All terms must be on the same field, got {} and {}",
        term.field(),
        first_term.field()
      )))?;
    }
    self.terms.push(term);
    self.positions.push(position);
    Ok(self)
  }
  /// Build a phrase query based on the terms that have been added.
  pub fn build(self) -> Result<PhraseQuery> {
    PhraseQuery::new(self.slop, self.terms, self.positions)
  }
}

fn incremental_positions(length: usize) -> Vec<usize> {
  (0..length).collect()
}

fn to_terms<I, S>(field: &str, term_strings: I) -> Vec<Term>
where
  I: IntoIterator<Item = S>,
  S: AsRef<str>,
{
  let term_strings = term_strings.into_iter();
  let mut terms = Vec::with_capacity(term_strings.size_hint().0);
  for s in term_strings {
    terms.push(Term::from_text(field, s.as_ref()));
  }
  terms
}

fn to_terms_from_bytes<I, B>(field: &str, term_bytes: I) -> Vec<Term>
where
  I: IntoIterator<Item = B>,
  B: Into<BytesRef<Vec<u8>>>,
{
  term_bytes
    .into_iter()
    .map(|bytes| Term::new(field, bytes))
    .collect()
}
/// A guess of the average number of simple operations for the initial seek and buffer refill per
/// document for the positions of a term. See also
/// [`Lucene101PostingsReader::BlockPostingsEnum::next_position`](crate::core::codecs::lucene101::lucene101_postings_reader::BlockPostingsEnum::next_position).
///
/// Aside: Instead of being constant this could depend among others on
/// [`Lucene101PostingsFormat::BLOCK_SIZE`](crate::core::codecs::lucene101::lucene101_postings_format::Lucene101PostingsFormat::BLOCK_SIZE), [`TermsEnum::doc_freq`], [`TermsEnum::total_term_freq`],
/// [`DocIdSetIterator::cost`](crate::core::search::doc_id_set_iterator::DocIdSetIterator::cost) (expected number of matching docs), [`LeafReader::max_doc`] (total
/// number of docs in the segment), and the seek time and block size of the device storing the
/// index.
pub(crate) const TERM_POSNS_SEEK_OPS_PER_DOC: i32 = 128;

/// Number of simple operations in [`Lucene101PostingsReader::BlockPostingsEnum::next_position`](crate::core::codecs::lucene101::lucene101_postings_reader::BlockPostingsEnum::next_position)
/// when no seek or buffer refill is done.
pub(crate) const TERM_OPS_PER_POS: i32 = 7;

pub fn term_positions_cost<TE>(terms_enum: &mut TE) -> Result<f32>
where
  TE: TermsEnum,
{
  let doc_freq = terms_enum.doc_freq()?;
  debug_assert!(doc_freq > 0);

  let total_term_freq = terms_enum.total_term_freq()?;

  let exp_occurrences_in_matching_doc = total_term_freq as f32 / doc_freq as f32;

  Ok(TERM_POSNS_SEEK_OPS_PER_DOC as f32 + exp_occurrences_in_matching_doc * TERM_OPS_PER_POS as f32)
}
pub struct PhraseQueryWeightBase<
  SIM = crate::core::search::similarities_impl::bm25_similarity::BM25Similarity,
> where
  SIM: Similarity,
{
  query: PhraseQuery,
  states: Vec<TermStates>,
  boost: f32,
  base: PhraseWeightMeta<SIM>,
}
impl<SIM> PhraseQueryWeightBase<SIM>
where
  SIM: Similarity,
{
  pub(crate) fn new(query: PhraseQuery, boost: f32, base: PhraseWeightMeta<SIM>) -> Self {
    Self {
      query,
      states: Vec::new(),
      boost,
      base,
    }
  }
  #[cfg(debug_assertions)]
  fn term_not_in_reader<LR>(reader: &LR, term: &Term) -> Result<bool>
  where
    LR: LeafReader,
  {
    Ok(LeafReader::doc_freq(reader, term)? == 0)
  }
}

impl<SIM> PhraseWeightBase<SIM> for PhraseQueryWeightBase<SIM>
where
  SIM: Similarity,
{
  type IE<LR: LeafReader> = ImpactsEnumEnum2<LRImpactsEnum<LR>, SlowImpactsEnum<LRPosting<LR>>>;
  type SimScorer = Arc<SimScorerType<SIM>>;

  fn get_stats<IRC>(&mut self, searcher: &IndexSearcher<IRC, SIM>) -> Result<Self::SimScorer>
  where
    IRC: IndexReaderContext + 'static,
  {
    let positions = &self.query.positions;

    if positions.len() < 2 {
      return Err(LuceneError::illegal_state(
        "PhraseWeight does not support less than 2 terms, call rewrite first",
      ));
    } else if positions[0] != 0 {
      return Err(LuceneError::illegal_state(
        "PhraseWeight requires that the first position is 0, call rewrite first",
      ));
    }

    self.states = Vec::with_capacity(self.query.terms.len());

    let mut term_stats: SmallVec<[_; 8]> =
      SmallVec::with_capacity(if self.base.score_mode.needs_scores() {
        self.query.terms.len()
      } else {
        0
      });
    let mut term_up_to = 0usize;

    for term in &*self.query.terms {
      let ts = if self.base.score_mode.needs_scores() {
        build_with_stats(searcher, term)?
      } else {
        build(searcher, term.clone(), false)?
      };

      if self.base.score_mode.needs_scores() && ts.doc_freq()? > 0 {
        let stats =
          searcher.term_statistics(term.clone(), ts.doc_freq()?, ts.total_term_freq()?)?;
        term_stats.push(stats);
        term_up_to += 1;
      }

      self.states.push(ts);
    }

    let v = if term_up_to > 0 {
      let collection_stats = searcher
        .collection_statistics(&self.base.field)?
        .ok_or_else(|| LuceneError::illegal_state("could not get collection stats"))?;

      SimScorerType::<SIM>::A(self.base.similarity.scorer(
        self.boost,
        &collection_stats,
        term_stats[..term_up_to].as_ref(),
      )?)
    } else {
      // no terms at all, we won't use similarity
      SimScorerType::<SIM>::B(SimScorerImpl)
    };
    Ok(Arc::new(v))
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
    debug_assert!(!self.query.terms.is_empty());
    let reader = context.reader();

    let field_terms = match reader.terms(&self.base.field)? {
      Some(t) => t,
      None => return Ok(None),
    };

    if !field_terms.has_positions() {
      return Err(LuceneError::illegal_state(format!(
        "field \"{}\" was indexed without position data; cannot run PhraseQuery (phrase={})",
        self.base.field,
        self.query.to_string(&self.base.field)?
      )));
    }

    let mut te = field_terms.iterator()?;
    let mut total_match_cost: f32 = 0.0;

    let mut postings_freqs = Vec::with_capacity(self.query.terms.len());

    for i in 0..self.query.terms.len() {
      let t = &self.query.terms[i];

      let mut supplier = self.states[i].get(context)?;
      let state = match supplier {
        None => None,
        Some(ref mut s) => self.states[i].resolve(s)?,
      };

      let state = match state {
        None => {
          #[cfg(debug_assertions)]
          {
            debug_assert!(
              Self::term_not_in_reader(reader, t)?,
              "no termstate found but term exists in reader"
            );
          }
          return Ok(None);
        },
        Some(s) => s,
      };

      te.seek_exact_with_state(t.bytes(), state.as_ref())?;

      let impacts_enum = if self.base.score_mode == ScoreMode::TopScores {
        let impacts = te.impacts(if expose_offsets {
          OFFSETS as i32
        } else {
          POSITIONS as i32
        })?;
        ImpactsEnumEnum2::A(impacts)
      } else {
        let postings = te.postings_with_flags(
          None,
          if expose_offsets {
            OFFSETS as i32
          } else {
            POSITIONS as i32
          },
        )?;
        ImpactsEnumEnum2::B(SlowImpactsEnum::new(postings))
      };

      postings_freqs.push(PostingsAndFreq::new(
        impacts_enum,
        self.query.positions[i] as i32,
        std::slice::from_ref(t),
      ));

      total_match_cost += term_positions_cost(&mut te)?;
    }

    // sort by increasing docFreq order
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
        self.query.slop,
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

pub struct PostingsAndFreq<'a, IE> {
  pub(crate) postings: IE,
  pub(crate) position: i32,
  pub(crate) terms: Option<Cow<'a, [Term]>>,
  pub(crate) n_terms: usize, // for faster comparisons
}
impl<'a, IE> PostingsAndFreq<'a, IE> {
  pub fn new(postings: IE, position: i32, terms: &'a [Term]) -> Self {
    let n_terms = terms.len();

    let terms_vec = if n_terms == 0 {
      None
    } else if n_terms == 1 {
      Some(Cow::Borrowed(terms))
    } else if n_terms == 2
      && terms.iter().all(|term| {
        term
          .bytes
          .offset
          .checked_add(term.bytes.length)
          .is_some_and(|end| end <= term.bytes.bytes.len())
      })
    {
      if terms[1].cmp(&terms[0]) == Ordering::Less {
        let mut v = terms.to_vec();
        v.swap(0, 1);
        Some(Cow::Owned(v))
      } else {
        Some(Cow::Borrowed(terms))
      }
    } else if terms[0]
      .bytes
      .offset
      .checked_add(terms[0].bytes.length)
      .is_some_and(|end| end <= terms[0].bytes.bytes.len())
      && terms.windows(2).all(|pair| {
        let term = &pair[1];
        term
          .bytes
          .offset
          .checked_add(term.bytes.length)
          .is_some_and(|end| end <= term.bytes.bytes.len())
          && term.cmp(&pair[0]) != Ordering::Less
      })
    {
      Some(Cow::Borrowed(terms))
    } else {
      let mut v = terms.to_vec();
      v.sort_unstable();
      Some(Cow::Owned(v))
    };

    Self {
      postings,
      position,
      terms: terms_vec,
      n_terms,
    }
  }
}
impl<IE> Ord for PostingsAndFreq<'_, IE> {
  fn cmp(&self, other: &Self) -> Ordering {
    match self.position.cmp(&other.position) {
      Ordering::Equal => {},
      ord => return ord,
    }

    match self.n_terms.cmp(&other.n_terms) {
      Ordering::Equal => {},
      ord => return ord,
    }

    if self.n_terms == 0 {
      return Ordering::Equal;
    }

    self.terms.cmp(&other.terms)
  }
}

impl<IE> PartialOrd for PostingsAndFreq<'_, IE> {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}
impl<IE> PartialEq for PostingsAndFreq<'_, IE> {
  fn eq(&self, other: &Self) -> bool {
    if self.position != other.position {
      return false;
    }

    match (&self.terms, &other.terms) {
      (None, None) => true,
      (Some(a), Some(b)) => a == b,
      _ => false,
    }
  }
}

impl<IE> Eq for PostingsAndFreq<'_, IE> {}

impl crate::core::util::accountable::Accountable for PhraseQuery {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(crate::core::util::ram_usage_estimator::QUERY_DEFAULT_RAM_BYTES_USED)
  }
}
