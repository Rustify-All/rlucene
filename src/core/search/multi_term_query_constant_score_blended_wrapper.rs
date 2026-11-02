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
use crate::core::index::BytesRefValue;
use crate::core::index::index_reader::{Identity, IndexReader};
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::postings_enum::{NONE, PostingsEnum};
use crate::core::index::term::Term;
use crate::core::index::term_states::TermStates;
use crate::core::index::terms::{Terms, TermsPosting};
use crate::core::index::terms_enum::TermsEnum;
use crate::core::search::abstract_multi_term_query_constant_score_wrapper::{
  MultiTermRewritingWeight, RewritingWeight, RewritingWeightBase, TermAndState,
  WeightOrDocIdSetIterator,
};
use crate::core::search::boolean_clause::Occur;
use crate::core::search::constant_score_query::ConstantScoreQuery;
use crate::core::search::constant_score_scorer::ConstantScoreScorer;
use crate::core::search::disi_priority_queue::DisiPriorityQueue;
use crate::core::search::disi_wrapper::DisiWrapper;
use crate::core::search::disjunction_disi_approximation::DisjunctionDISIApproximation;
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, DocIdSetIteratorEnum2};
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::multi_term_query::{
  MultiTermQuery, MultiTermQuerySet, dispatch_multi_term_query,
};
use crate::core::search::query::QueryBase;
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::term_query::TermQuery;
use crate::core::util::HasIdentity;
use crate::core::util::doc_id_set_builder::{DocIdSetBuilder, OwnedDocIdSetBuilderIterator};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::priority_queue::{Compare, PriorityQueue};
use std::fmt::{Debug, Formatter};
use std::hash::Hash;
use std::sync::Arc;

/// This struct implements the logic behind [`MultiTermQuery::ConstantScoreBlendedRewrite`](crate::core::search::multi_term_query::MultiTermQuery::ConstantScoreBlendedRewrite).
///
/// It behaves similarly to a boolean-query-style rewrite for a limited number of the
/// highest-cost terms, while rewriting the remaining lower-cost terms into a filter bitset.
#[derive(Clone)]
pub struct MultiTermQueryConstantScoreBlendedWrapper {
  q: MultiTermQuerySet,
  id: Identity,
}
impl MultiTermQueryConstantScoreBlendedWrapper {
  pub fn new<T>(q: T) -> Self
  where
    T: Into<MultiTermQuerySet>,
  {
    Self {
      q: q.into(),
      id: Identity::new(),
    }
  }
}

impl Debug for MultiTermQueryConstantScoreBlendedWrapper {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self.to_string("") {
      Ok(s) => write!(f, "{}", s),
      Err(_) => Err(std::fmt::Error),
    }
  }
}

impl HasIdentity for MultiTermQueryConstantScoreBlendedWrapper {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl QueryBase for MultiTermQueryConstantScoreBlendedWrapper {
  type Weight<IRC, SIM>
    = MultiTermRewritingWeight
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static;

  fn to_string(&self, field: &str) -> Result<String> {
    self.q.to_string(field)
  }

  fn create_weight<IRC, SIM>(
    self,
    _searcher: &IndexSearcher<IRC, SIM>,
    score_mode: &ScoreMode,
    boost: f32,
  ) -> Result<Self::Weight<IRC, SIM>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    IndexSearcher<IRC, SIM>: Sync,
    Self: Sized,
  {
    let sub = BlendedRewritingWeight;
    match self.q {
      MultiTermQuerySet::Prefix(q) => Ok(MultiTermRewritingWeight::Prefix(RewritingWeight::new(
        boost,
        *score_mode,
        q,
        sub.into(),
      ))),
      MultiTermQuerySet::TermRange(q) => Ok(MultiTermRewritingWeight::TermRange(
        RewritingWeight::new(boost, *score_mode, q, sub.into()),
      )),
      MultiTermQuerySet::Automaton(q) => Ok(MultiTermRewritingWeight::Automaton(
        RewritingWeight::new(boost, *score_mode, q, sub.into()),
      )),
      MultiTermQuerySet::Fuzzy(q) => Ok(MultiTermRewritingWeight::Fuzzy(RewritingWeight::new(
        boost,
        *score_mode,
        q,
        sub.into(),
      ))),
      MultiTermQuerySet::TermInSet(q) => Ok(MultiTermRewritingWeight::TermInSet(
        RewritingWeight::new(boost, *score_mode, q, sub.into()),
      )),
      MultiTermQuerySet::Wildcard(q) => Ok(MultiTermRewritingWeight::Wildcard(
        RewritingWeight::new(boost, *score_mode, q, sub.into()),
      )),
      MultiTermQuerySet::Regexp(q) => Ok(MultiTermRewritingWeight::Regexp(RewritingWeight::new(
        boost,
        *score_mode,
        q,
        sub.into(),
      ))),
      #[cfg(test)]
      MultiTermQuerySet::BoostChecking(q) => Ok(MultiTermRewritingWeight::BoostChecking(
        RewritingWeight::new(boost, *score_mode, q, sub.into()),
      )),
      #[cfg(test)]
      MultiTermQuerySet::DumbPrefix(q) => Ok(MultiTermRewritingWeight::DumbPrefix(
        RewritingWeight::new(boost, *score_mode, q, sub.into()),
      )),
      #[cfg(test)]
      MultiTermQuerySet::DumbRegexp(q) => Ok(MultiTermRewritingWeight::DumbRegexp(
        RewritingWeight::new(boost, *score_mode, q, sub.into()),
      )),
    }
  }

  fn visit<QV>(&self, visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    let field = dispatch_multi_term_query!(&self.q, |query| query.get_field());
    if visitor.accept_field(field) {
      let query = self.into();
      let mut visitor = visitor.get_sub_visitor(Occur::Filter, query);
      self.q.visit(&mut visitor)?;
    }
    Ok(())
  }
}
impl Hash for MultiTermQueryConstantScoreBlendedWrapper {
  fn hash<H>(&self, state: &mut H)
  where
    H: std::hash::Hasher,
  {
    self.q.hash(state);
  }
}
impl PartialEq for MultiTermQueryConstantScoreBlendedWrapper {
  fn eq(&self, other: &Self) -> bool {
    self.q == other.q
  }
}
impl Eq for MultiTermQueryConstantScoreBlendedWrapper {}

#[derive(Default, Clone)]
pub struct BlendedRewritingWeight;
impl RewritingWeightBase for BlendedRewritingWeight {
  type Iter<T>
    = DisjunctionDISIApproximation<
    ConstantScoreScorer<
      crate::core::search::constant_score_scorer::DisiState<
        DocIdSetIteratorEnum2<OwnedDocIdSetBuilderIterator, TermsPosting<T>>,
      >,
    >,
  >
  where
    T: Terms,
    TermsPosting<T>: 'static;

  fn rewrite_inner<T, TE, IRC, SIM>(
    &self,
    field_doc_count: i32,
    terms: &mut T,
    terms_enum: &mut TE,
    collected_terms: &[TermAndState],
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC, SIM>,
    field: &str,
    score_mode: &ScoreMode,
    score: f32,
  ) -> Result<WeightOrDocIdSetIterator<IRC, Self::Iter<T>, SIM>>
  where
    SIM: Similarity,
    T: Terms,
    TE: TermsEnum<PostingsEnum = <T::TermsEnum as TermsEnum>::PostingsEnum>,
    IRC: IndexReaderContext + 'static,
    IndexSearcher<IRC, SIM>: Sync,
    TermsPosting<T>: 'static,
  {
    let max_doc = context.reader().max_doc()?;
    let mut other_terms = DocIdSetBuilder::from_terms(max_doc, terms)?;
    let cmp = PostingsEnumCmp::new(vec![]);
    let mut high_frequency_terms = PriorityQueue::new(collected_terms.len(), cmp)?;
    // Handle the already-collected terms:
    let mut reuse = None;
    if !collected_terms.is_empty() {
      let mut terms_enum2 = terms.iterator()?;
      for t in collected_terms.iter() {
        terms_enum2.seek_exact_with_state(&t.term, &t.state)?;
        let mut pe = terms_enum2.postings_with_flags(reuse, NONE as i32)?;
        if t.doc_freq <= POSTINGS_PRE_PROCESS_THRESHOLD {
          other_terms.add_disi(&mut pe)?;
          reuse = Some(pe);
        } else {
          high_frequency_terms.compare.postings_enum.push(Some(pe));
          let idx = high_frequency_terms.compare.postings_enum.len() - 1;
          high_frequency_terms.add(idx)?;
          reuse = None;
        }
      }
    }
    // Then collect remaining terms:
    loop {
      let mut pe = terms_enum.postings_with_flags(reuse, NONE as i32)?;
      let doc_freq = terms_enum.doc_freq()?;

      if field_doc_count == doc_freq {
        let mut term_states = TermStates::new(searcher.get_top_reader_context())?;
        term_states.register_with_stats(
          terms_enum.term_state()?,
          context.ord,
          doc_freq,
          terms_enum.total_term_freq()?,
        );

        let term = Term::new(field, terms_enum.term()?.into_owned());
        let tq = TermQuery::with_term_state(term, Some(Arc::new(term_states)));
        let q = ConstantScoreQuery::new(tq);

        let rewritten = searcher.rewrite(q)?;
        let weight = rewritten.create_weight(searcher, score_mode, score)?;
        let v = WeightOrDocIdSetIterator::from_weight(weight);
        return Ok(v);
      }

      if doc_freq <= POSTINGS_PRE_PROCESS_THRESHOLD {
        other_terms.add_disi(&mut pe)?;
        reuse = Some(pe);
      } else {
        high_frequency_terms.compare.postings_enum.push(Some(pe));
        let idx = high_frequency_terms.compare.postings_enum.len() - 1;
        let dropped = high_frequency_terms.insert_with_overflow(idx)?;

        if let Some(dropped_idx) = dropped {
          // The just-added posting is no longer referenced by the queue when rejected.
          let mut dropped_pe = if dropped_idx == idx {
            high_frequency_terms.compare.postings_enum.pop().flatten()
          } else {
            high_frequency_terms.compare.postings_enum[dropped_idx].take()
          }
          .ok_or_else(|| LuceneError::illegal_state("posting enum is None"))?;
          other_terms.add_disi(&mut dropped_pe)?;
          // Reuse the postings that drop out of the PQ. Note that `dropped` will be None here
          // if nothing is evicted, meaning we will _not_ reuse any postings (which is intentional
          // since we can't reuse postings that are in the PQ).
          reuse = Some(dropped_pe);
        } else {
          reuse = None;
        }
      }

      if terms_enum.next()?.is_none() {
        break;
      }
    }
    let size = high_frequency_terms.size() + 1;
    let mut subs = DisiPriorityQueue::new(size);

    let mut all_scorers = Vec::with_capacity(size);
    for (idx, pe) in high_frequency_terms
      .compare
      .postings_enum
      .into_iter()
      .flatten()
      .enumerate()
    {
      let scorer = wrap_with_dummy_scorer(DocIdSetIteratorEnum2::B(pe));
      all_scorers.push(DisiWrapper::new(scorer)?);
      subs.add(idx, all_scorers.as_slice());
    }
    let scorer = wrap_with_dummy_scorer(DocIdSetIteratorEnum2::A(other_terms.build_iterator()?));
    all_scorers.push(DisiWrapper::new(scorer)?);
    let len = all_scorers.len() - 1;
    subs.add(len, all_scorers.as_slice());
    let v = WeightOrDocIdSetIterator::from_iterator(DisjunctionDISIApproximation::new(
      subs,
      all_scorers,
    )?);
    Ok(v)
  }
}
/// Wrap a DISI with a "dummy" scorer so we can directly reuse [`DisiWrapper`](crate::core::search::disi_wrapper::DisiWrapper) and
/// [`DisjunctionDISIApproximation`](crate::core::search::disjunction_disi_approximation::DisjunctionDISIApproximation) without modification.
///
/// This is merely a convenient vehicle to place the DISI into the priority queue
/// consumed by [`DisjunctionDISIApproximation`](crate::core::search::disjunction_disi_approximation::DisjunctionDISIApproximation).
///
/// The actual [`Scorer`](crate::core::search::scorer::Scorer) ultimately returned by the weight provides the real constant
/// boost and reflects the effective score mode.
fn wrap_with_dummy_scorer<D>(
  disi: D,
) -> ConstantScoreScorer<crate::core::search::constant_score_scorer::DisiState<D>>
where
  D: DocIdSetIterator,
{
  ConstantScoreScorer::from_plain_disi(1.0f32, disi)
}
const POSTINGS_PRE_PROCESS_THRESHOLD: i32 = 16;
struct PostingsEnumCmp<PE> {
  // for easy taken
  postings_enum: Vec<Option<PE>>,
}
impl<PE> PostingsEnumCmp<PE> {
  fn new(postings_enum: Vec<Option<PE>>) -> Self {
    Self { postings_enum }
  }
}
impl<PE> Compare<usize> for PostingsEnumCmp<PE>
where
  PE: PostingsEnum,
{
  fn less_than(&self, a: &usize, b: &usize) -> Result<bool> {
    let l = self.postings_enum[*a]
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("posting enum is None"))?;
    let r = self.postings_enum[*b]
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("posting enum is None"))?;
    Ok(l.cost()? < r.cost()?)
  }
}

impl crate::core::util::accountable::Accountable for MultiTermQueryConstantScoreBlendedWrapper {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(crate::core::util::ram_usage_estimator::QUERY_DEFAULT_RAM_BYTES_USED)
  }
}
