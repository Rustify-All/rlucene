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
use crate::core::document::sorted_numeric_doc_values_range_query::{
    SNDVRQSs, SortedNumericDocValuesRangeQueryWeight,
};
use crate::core::document::sorted_numeric_doc_values_set_query::{
    SNDVSQSs, SortedNumericDocValuesSetQueryWeight,
};
use crate::core::document::sorted_set_doc_values_range_query::{
    SSDVRQSs, SortedSetDocValuesRangeQueryWeight,
};
use crate::core::index::index_reader_context::{IRCLeafReader, IRCTermState, IndexReaderContext};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::term_states::TermStates;
use crate::core::search::QueryCache;
use crate::core::search::abstract_multi_term_query_constant_score_wrapper::BOOLEAN_REWRITE_TERM_COUNT_THRESHOLD;
use crate::core::search::boolean_clause::Occur::{Filter, Must};
use crate::core::search::boolean_clause::{BooleanClause, BooleanClauseQuery, Occur};
use crate::core::search::boolean_query::BooleanQuery;
use crate::core::search::boolean_scorer_supplier::BooleanScorerSupplier;
use crate::core::search::bulk_scorer::BulkScorerEnum11;
use crate::core::search::constant_score_query::{
    BaseQueryWeight, ConstantScoreQueryWeight, ConstantScoreSs,
};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::dummy::dummy_matches::DummyMatches;
use crate::core::search::explanation::Explanation;
use crate::core::search::field_exists_query::{FieldExistsESs, FieldExistsWeight};
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::index_sort_sorted_numeric_doc_values_range_query::{
    ISSNDVRQSs, IndexSortSortedNumericDocValuesRangeQueryWeight,
};
use crate::core::search::match_all_docs_query::{MatchAllSs, MatchAllWeight};
use crate::core::search::match_no_docs_query::{MatchNoDocsSs, MatchNoDocsWeight};
use crate::core::search::matches_utils::MatchWithNoTerms;
use crate::core::search::point_range_query::{PointRangeSs, PointRangeWeight};
use crate::core::search::query::{BaseQuery, Query, QueryBase};
use crate::core::search::scorable::Scorable;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer::{Scorer, ScorerEnum11};
use crate::core::search::scorer_supplier::{ScorerSupplier, ScorerSupplierEnum11};
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::similarities_impl::similarities::SimilarityEnum;
use crate::core::search::term_query::{TermSs, TermWeight};
use crate::core::search::weight::{Weight, WeightSs};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::Arc;

pub struct BooleanWeight<W, LR>
where
    LR: LeafReader,
    W: Weight<LR>,
{
    pub(crate) similarity: Arc<SimilarityEnum>,
    pub(crate) weighted_clauses: Vec<WeightedBooleanClause<W, LR>>,
    pub(crate) query: BooleanQuery,
    pub(crate) score_mode: ScoreMode,
}

impl<LR, W> BooleanWeight<W, LR>
where
    LR: LeafReader,
    W: Weight<LR>,
{
    /// Return the number of matches of required clauses, or -1 if unknown, or numDocs if there are no
    /// required clauses.
    fn req_count(&self, context: &LeafReaderContext<LR>) -> Result<i32> {
        let num_docs = context.reader().num_docs()?;
        let mut req_count = num_docs;

        for weighted_clause in &self.weighted_clauses {
            if !weighted_clause.clause.is_required() {
                continue;
            }

            let count = weighted_clause.weight.count(context)?;

            if count == -1 || count == 0 {
                // If the count of one clause is unknown, then the count of the conjunction is unknown
                // too. If one clause doesn't match any docs then the conjunction doesn't match any docs
                // either.
                return Ok(count);
            } else if count == num_docs {
                // the query matches all docs, it can be safely ignored
            } else if req_count == num_docs {
                // all clauses seen so far match all docs, so the count of the new clause is also the
                // count of the conjunction
                req_count = count;
            } else {
                // We have two clauses whose count is in [1, numDocs), we can't figure out the number of
                // docs that match the conjunction without running the query.
                return Ok(-1);
            }
        }

        Ok(req_count)
    }

    /// Return the number of matches of optional clauses, or -1 if unknown, or 0 if there are no
    /// optional clauses.
    fn opt_count(&self, context: &LeafReaderContext<LR>, occur: Occur) -> Result<i32> {
        let num_docs = context.reader().num_docs()?;
        let mut opt_count = 0i32;
        let mut unknown_count = false;

        for weighted_clause in &self.weighted_clauses {
            if *weighted_clause.clause.occur() != occur {
                continue;
            }

            let count = weighted_clause.weight.count(context)?;

            if count == -1 {
                // If one clause has a number of matches that is unknown, let's be more aggressive to
                // check whether remain clauses could match all docs.
                unknown_count = true;
                continue;
            } else if count == num_docs {
                // If either clause matches all docs, then the disjunction matches all docs.
                return Ok(count);
            } else if count == 0 {
                // We can safely ignore this clause, it doesn't affect the count.
            } else if opt_count == 0 {
                // This is the first clause we see that has a non-zero count, it becomes the count of
                // the disjunction.
                opt_count = count;
            } else {
                // We have two clauses whose count is in [1, numDocs), we can't figure out the number of
                // docs that match the disjunction without running the query.
                unknown_count = true;
            }
        }

        // If at least one of clauses has a number of matches that is unknown and no clause matches all
        // docs, then the number of matches of the disjunction is unknown.
        Ok(if unknown_count { -1 } else { opt_count })
    }
}

impl<W, LR> SegmentCacheable<LR> for BooleanWeight<W, LR>
where
    LR: LeafReader,
    W: Weight<LR>,
{
    fn is_cacheable(&self, ctx: &LeafReaderContext<LR>) -> Result<bool> {
        // Disallow caching large boolean queries to not encourage users
        // to build large boolean queries as a workaround to the fact that
        // we disallow caching large TermInSetQueries.
        if self.query.clauses().len() > BOOLEAN_REWRITE_TERM_COUNT_THRESHOLD {
            return Ok(false);
        }

        for wc in &self.weighted_clauses {
            if !wc.weight.is_cacheable(ctx)? {
                return Ok(false);
            }
        }

        Ok(true)
    }
}

impl<W, LR> Weight<LR> for BooleanWeight<W, LR>
where
    LR: LeafReader,
    W: Weight<LR>,
{
    type Matches = DummyMatches;

    fn matches(
        &self,
        _context: &LeafReaderContext<LR>,
        _doc: i32,
    ) -> Result<Option<Self::Matches>> {
        todo!()
    }

    fn explain(&self, context: &LeafReaderContext<LR>, doc: i32) -> Result<Explanation> {
        let min_should_match = self.query.get_minimum_number_should_match();

        let mut subs: Vec<Explanation> = Vec::new();
        let mut fail = false;
        let mut match_count = 0;
        let mut should_match_count = 0;

        for wc in &self.weighted_clauses {
            let clause = &wc.clause;
            let weight = &wc.weight;

            let e = weight.explain(context, doc)?;

            if e.is_match() {
                if clause.is_scoring() {
                    subs.push(e);
                } else if clause.is_required() {
                    subs.push(Explanation::match_(
                        0.0,
                        "match on required clause, product of:",
                        vec![
                            Explanation::match_(0.0, format!("{} clause", Occur::Filter), vec![]),
                            e,
                        ],
                    ));
                } else if clause.is_prohibited() {
                    subs.push(Explanation::no_match(
                        format!(
                            "match on prohibited clause ({})",
                            clause.query.as_string("")
                        ),
                        vec![e],
                    ));
                    fail = true;
                }

                if !clause.is_prohibited() {
                    match_count += 1;
                }

                if clause.occur() == &Occur::Should {
                    should_match_count += 1;
                }
            } else if clause.is_required() {
                subs.push(Explanation::no_match(
                    format!(
                        "no match on required clause ({})",
                        clause.query.as_string("")
                    ),
                    vec![e],
                ));
                fail = true;
            }
        }

        if fail {
            Ok(Explanation::no_match(
                "Failure to meet condition(s) of required/prohibited clause(s)",
                subs,
            ))
        } else if match_count == 0 {
            Ok(Explanation::no_match("No matching clauses", subs))
        } else if should_match_count < min_should_match {
            Ok(Explanation::no_match(
                format!(
                    "Failure to match minimum number of optional clauses: {}",
                    min_should_match
                ),
                subs,
            ))
        } else {
            // Replicating the same floating-point errors as the scorer does is quite
            // complex (essentially because of how ReqOptSumScorer casts intermediate
            // contributions to the score to floats), so in order to make sure that
            // explanations have the same value as the score, we pull a scorer and
            // use it to compute the score.

            let mut scorer = self
                .scorer(context)?
                .ok_or_else(|| LuceneError::illegal_state("no scorer available for explanation"))?;

            let advanced = scorer.iterator_mut().advance(doc)?;
            debug_assert!(advanced == doc);

            Ok(Explanation::match_(scorer.score()?, "sum of:", subs))
        }
    }

    fn get_query(&self) -> Arc<Query> {
        let v: Query = self.query.clone().into();
        Arc::new(v)
    }

    type ScorerSupplier = BooleanScorerSupplier<WeightSs<W, LR>, LR>;

    fn scorer_supplier(
        &self,
        context: &LeafReaderContext<LR>,
    ) -> Result<Option<Self::ScorerSupplier>> {
        let mut min_should_match = self.query.get_minimum_number_should_match();

        let mut must = Vec::new();
        let mut should = Vec::new();
        let mut filter = Vec::new();
        let mut must_not = Vec::new();

        for wc in &self.weighted_clauses {
            let sub_supplier = wc.weight.scorer_supplier(context)?;
            match sub_supplier {
                None => {
                    if wc.clause.is_required() {
                        return Ok(None);
                    }
                },
                Some(sub_scorer) => match wc.clause.occur() {
                    Occur::Must => must.push(sub_scorer),
                    Occur::Should => should.push(sub_scorer),
                    Occur::Filter => filter.push(sub_scorer),
                    Occur::MustNot => must_not.push(sub_scorer),
                },
            }
        }
        // scorer simplifications:
        if (should.len() as i32) == min_should_match {
            must.append(&mut should);
            min_should_match = 0;
        }

        if (filter.is_empty() && must.is_empty() && should.is_empty())
            || (should.len() as i32) < min_should_match
        {
            return Ok(None);
        }

        if !self.score_mode.needs_scores()
            && min_should_match == 0
            && (must.len() + filter.len()) > 0
        {
            should.clear();
        }
        let max_doc = context.reader().max_doc()?;
        let mut scores = HashMap::new();
        scores.insert(Occur::Must, must);
        scores.insert(Occur::Should, should);
        scores.insert(Occur::Filter, filter);
        scores.insert(Occur::MustNot, must_not);
        let v = BooleanScorerSupplier::new(scores, self.score_mode, min_should_match, max_doc)?;
        Ok(Some(v))
    }

    fn count(&self, context: &LeafReaderContext<LR>) -> Result<i32> {
        let num_docs = context.reader().num_docs()?;

        if self.query.is_pure_disjunction() {
            return self.opt_count(context, Occur::Should);
        }

        let positive_count = if (!self.query.get_clauses_idx(Filter).is_empty()
            || !self.query.get_clauses_idx(Must).is_empty())
            && self.query.get_minimum_number_should_match() == 0
        {
            self.req_count(context)?
        } else {
            // The query has a non-zero min-should match. We could handle some cases, e.g.
            // minShouldMatch=N and we can find N SHOULD clauses that match all docs, but are there
            // real-world queries that would benefit from Lucene handling this case?
            -1
        };

        if positive_count == 0 {
            return Ok(0);
        }

        let prohibited_count = self.opt_count(context, Occur::MustNot)?;

        if prohibited_count == -1 {
            Ok(-1)
        } else if prohibited_count == 0 {
            Ok(positive_count)
        } else if prohibited_count == num_docs {
            Ok(0)
        } else if positive_count == num_docs {
            Ok(num_docs - prohibited_count)
        } else {
            Ok(-1)
        }
    }
}
pub(crate) struct WeightedBooleanClause<W, LR>
where
    W: Weight<LR>,
    LR: LeafReader,
{
    pub(crate) clause: BooleanClause,
    pub(crate) weight: W,
    _phantom: PhantomData<LR>,
}

impl<W, LR> WeightedBooleanClause<W, LR>
where
    W: Weight<LR>,
    LR: LeafReader,
{
    pub(crate) fn new(clause: BooleanClause, weight: W) -> Self {
        Self {
            clause,
            weight,
            _phantom: PhantomData,
        }
    }
}
struct BooleanQueryMeta {
    minimum_number_should_match: i32,
    is_pure_disjunction: bool,
    has_no_filter: bool,
    has_no_must: bool,
    clause_size: i32,
}
impl BooleanQueryMeta {
    pub fn new(
        minimum_number_should_match: i32,
        is_pure_disjunction: bool,
        has_no_filter: bool,
        has_no_must: bool,
        clause_size: i32,
    ) -> Self {
        Self {
            minimum_number_should_match,
            is_pure_disjunction,
            has_no_filter,
            has_no_must,
            clause_size,
        }
    }
}

pub enum BaseQueryWeightEnum<LR, QC>
where
    LR: LeafReader,
    QC: QueryCache,
{
    Term(TermWeight<LR>),
    MatchAll(MatchAllWeight<LR>),
    PointRange(PointRangeWeight<LR>),
    MatchNoDocs(MatchNoDocsWeight<LR>),
    SortedNumericDocValuesSet(SortedNumericDocValuesSetQueryWeight<LR>),
    SortedNumericDocValuesRange(SortedNumericDocValuesRangeQueryWeight<LR>),
    SortedSetDocValuesRange(SortedSetDocValuesRangeQueryWeight<LR>),
    IndexSortSortedNumericDocValuesRange(IndexSortSortedNumericDocValuesRangeQueryWeight<LR>),
    FieldExists(FieldExistsWeight<LR>),
    ConstantScore(ConstantScoreQueryWeight<BaseQueryWeight<LR>, LR, QC>),
    Boost(BaseQueryWeight<LR>),
}

impl<LR, QC> SegmentCacheable<LR> for BaseQueryWeightEnum<LR, QC>
where
    LR: LeafReader,
    QC: QueryCache,
{
    fn is_cacheable(&self, ctx: &LeafReaderContext<LR>) -> Result<bool> {
        match self {
            BaseQueryWeightEnum::Term(w) => w.is_cacheable(ctx),
            BaseQueryWeightEnum::MatchAll(w) => w.is_cacheable(ctx),
            BaseQueryWeightEnum::PointRange(w) => w.is_cacheable(ctx),
            BaseQueryWeightEnum::MatchNoDocs(w) => w.is_cacheable(ctx),
            BaseQueryWeightEnum::SortedNumericDocValuesSet(w) => w.is_cacheable(ctx),
            BaseQueryWeightEnum::SortedNumericDocValuesRange(w) => w.is_cacheable(ctx),
            BaseQueryWeightEnum::SortedSetDocValuesRange(w) => w.is_cacheable(ctx),
            BaseQueryWeightEnum::IndexSortSortedNumericDocValuesRange(w) => w.is_cacheable(ctx),
            BaseQueryWeightEnum::FieldExists(w) => w.is_cacheable(ctx),
            BaseQueryWeightEnum::ConstantScore(w) => w.is_cacheable(ctx),
            BaseQueryWeightEnum::Boost(w) => w.is_cacheable(ctx),
        }
    }
}

impl<LR, QC> Weight<LR> for BaseQueryWeightEnum<LR, QC>
where
    LR: LeafReader,
    QC: QueryCache,
{
    type Matches = DummyMatches;

    fn matches(
        &self,
        _context: &LeafReaderContext<LR>,
        _doc: i32,
    ) -> Result<Option<Self::Matches>> {
        todo!()
    }

    fn default_matches(
        &self,
        context: &LeafReaderContext<LR>,
        doc: i32,
    ) -> Result<Option<MatchWithNoTerms>> {
        match self {
            BaseQueryWeightEnum::Term(w) => w.default_matches(context, doc),
            BaseQueryWeightEnum::MatchAll(w) => w.default_matches(context, doc),
            BaseQueryWeightEnum::PointRange(w) => w.default_matches(context, doc),
            BaseQueryWeightEnum::MatchNoDocs(w) => w.default_matches(context, doc),
            BaseQueryWeightEnum::SortedNumericDocValuesSet(w) => w.default_matches(context, doc),
            BaseQueryWeightEnum::SortedNumericDocValuesRange(w) => w.default_matches(context, doc),
            BaseQueryWeightEnum::SortedSetDocValuesRange(w) => w.default_matches(context, doc),
            BaseQueryWeightEnum::IndexSortSortedNumericDocValuesRange(w) => {
                w.default_matches(context, doc)
            },
            BaseQueryWeightEnum::FieldExists(w) => w.default_matches(context, doc),
            BaseQueryWeightEnum::ConstantScore(w) => w.default_matches(context, doc),
            BaseQueryWeightEnum::Boost(w) => w.default_matches(context, doc),
        }
    }

    fn explain(&self, context: &LeafReaderContext<LR>, doc: i32) -> Result<Explanation> {
        match self {
            BaseQueryWeightEnum::Term(w) => w.explain(context, doc),
            BaseQueryWeightEnum::MatchAll(w) => w.explain(context, doc),
            BaseQueryWeightEnum::PointRange(w) => w.explain(context, doc),
            BaseQueryWeightEnum::MatchNoDocs(w) => w.explain(context, doc),
            BaseQueryWeightEnum::SortedNumericDocValuesSet(w) => w.explain(context, doc),
            BaseQueryWeightEnum::SortedNumericDocValuesRange(w) => w.explain(context, doc),
            BaseQueryWeightEnum::SortedSetDocValuesRange(w) => w.explain(context, doc),
            BaseQueryWeightEnum::IndexSortSortedNumericDocValuesRange(w) => w.explain(context, doc),
            BaseQueryWeightEnum::FieldExists(w) => w.explain(context, doc),
            BaseQueryWeightEnum::ConstantScore(w) => w.explain(context, doc),
            BaseQueryWeightEnum::Boost(w) => w.explain(context, doc),
        }
    }

    fn get_query(&self) -> Arc<Query> {
        match self {
            BaseQueryWeightEnum::Term(w) => w.get_query(),
            BaseQueryWeightEnum::MatchAll(w) => w.get_query(),
            BaseQueryWeightEnum::PointRange(w) => w.get_query(),
            BaseQueryWeightEnum::MatchNoDocs(w) => w.get_query(),
            BaseQueryWeightEnum::SortedNumericDocValuesSet(w) => w.get_query(),
            BaseQueryWeightEnum::SortedNumericDocValuesRange(w) => w.get_query(),
            BaseQueryWeightEnum::SortedSetDocValuesRange(w) => w.get_query(),
            BaseQueryWeightEnum::IndexSortSortedNumericDocValuesRange(w) => w.get_query(),
            BaseQueryWeightEnum::FieldExists(w) => w.get_query(),
            BaseQueryWeightEnum::ConstantScore(w) => w.get_query(),
            BaseQueryWeightEnum::Boost(w) => w.get_query(),
        }
    }

    fn scorer(
        &self,
        context: &LeafReaderContext<LR>,
    ) -> Result<Option<<Self::ScorerSupplier as ScorerSupplier<LR>>::Scorer>> {
        match self {
            BaseQueryWeightEnum::Term(w) => Ok(w.scorer(context)?.map(ScorerEnum11::A)),
            BaseQueryWeightEnum::MatchAll(w) => Ok(w.scorer(context)?.map(ScorerEnum11::B)),
            BaseQueryWeightEnum::PointRange(w) => Ok(w.scorer(context)?.map(ScorerEnum11::C)),
            BaseQueryWeightEnum::MatchNoDocs(w) => Ok(w.scorer(context)?.map(ScorerEnum11::D)),
            BaseQueryWeightEnum::SortedNumericDocValuesSet(w) => {
                Ok(w.scorer(context)?.map(ScorerEnum11::E))
            },
            BaseQueryWeightEnum::SortedNumericDocValuesRange(w) => {
                Ok(w.scorer(context)?.map(ScorerEnum11::F))
            },
            BaseQueryWeightEnum::SortedSetDocValuesRange(w) => {
                Ok(w.scorer(context)?.map(ScorerEnum11::G))
            },
            BaseQueryWeightEnum::IndexSortSortedNumericDocValuesRange(w) => {
                Ok(w.scorer(context)?.map(ScorerEnum11::H))
            },
            BaseQueryWeightEnum::FieldExists(w) => Ok(w.scorer(context)?.map(ScorerEnum11::I)),
            BaseQueryWeightEnum::ConstantScore(w) => Ok(w.scorer(context)?.map(ScorerEnum11::J)),
            BaseQueryWeightEnum::Boost(w) => Ok(w.scorer(context)?.map(ScorerEnum11::K)),
        }
    }

    type ScorerSupplier = ScorerSupplierEnum11<
        TermSs<LR>,
        MatchAllSs,
        PointRangeSs<LR>,
        MatchNoDocsSs,
        SNDVSQSs<LR>,
        SNDVRQSs<LR>,
        SSDVRQSs<LR>,
        ISSNDVRQSs<LR>,
        FieldExistsESs<LR>,
        ConstantScoreSs<BaseQueryWeight<LR>, LR, QC>,
        <BaseQueryWeight<LR> as Weight<LR>>::ScorerSupplier,
    >;

    fn scorer_supplier(
        &self,
        context: &LeafReaderContext<LR>,
    ) -> Result<Option<Self::ScorerSupplier>> {
        match self {
            BaseQueryWeightEnum::Term(w) => {
                Ok(w.scorer_supplier(context)?.map(ScorerSupplierEnum11::A))
            },
            BaseQueryWeightEnum::MatchAll(w) => {
                Ok(w.scorer_supplier(context)?.map(ScorerSupplierEnum11::B))
            },
            BaseQueryWeightEnum::PointRange(w) => {
                Ok(w.scorer_supplier(context)?.map(ScorerSupplierEnum11::C))
            },
            BaseQueryWeightEnum::MatchNoDocs(w) => {
                Ok(w.scorer_supplier(context)?.map(ScorerSupplierEnum11::D))
            },
            BaseQueryWeightEnum::SortedNumericDocValuesSet(w) => {
                Ok(w.scorer_supplier(context)?.map(ScorerSupplierEnum11::E))
            },
            BaseQueryWeightEnum::SortedNumericDocValuesRange(w) => {
                Ok(w.scorer_supplier(context)?.map(ScorerSupplierEnum11::F))
            },
            BaseQueryWeightEnum::SortedSetDocValuesRange(w) => {
                Ok(w.scorer_supplier(context)?.map(ScorerSupplierEnum11::G))
            },
            BaseQueryWeightEnum::IndexSortSortedNumericDocValuesRange(w) => {
                Ok(w.scorer_supplier(context)?.map(ScorerSupplierEnum11::H))
            },
            BaseQueryWeightEnum::FieldExists(w) => {
                Ok(w.scorer_supplier(context)?.map(ScorerSupplierEnum11::I))
            },
            BaseQueryWeightEnum::ConstantScore(w) => {
                Ok(w.scorer_supplier(context)?.map(ScorerSupplierEnum11::J))
            },
            BaseQueryWeightEnum::Boost(w) => {
                Ok(w.scorer_supplier(context)?.map(ScorerSupplierEnum11::K))
            },
        }
    }

    fn bulk_scorer(
        &self,
        context: &LeafReaderContext<LR>,
    ) -> Result<Option<<Self::ScorerSupplier as ScorerSupplier<LR>>::BulkScorer>> {
        match self {
            BaseQueryWeightEnum::Term(w) => Ok(w.bulk_scorer(context)?.map(BulkScorerEnum11::A)),
            BaseQueryWeightEnum::MatchAll(w) => {
                Ok(w.bulk_scorer(context)?.map(BulkScorerEnum11::B))
            },
            BaseQueryWeightEnum::PointRange(w) => {
                Ok(w.bulk_scorer(context)?.map(BulkScorerEnum11::C))
            },
            BaseQueryWeightEnum::MatchNoDocs(w) => {
                Ok(w.bulk_scorer(context)?.map(BulkScorerEnum11::D))
            },
            BaseQueryWeightEnum::SortedNumericDocValuesSet(w) => {
                Ok(w.bulk_scorer(context)?.map(BulkScorerEnum11::E))
            },
            BaseQueryWeightEnum::SortedNumericDocValuesRange(w) => {
                Ok(w.bulk_scorer(context)?.map(BulkScorerEnum11::F))
            },
            BaseQueryWeightEnum::SortedSetDocValuesRange(w) => {
                Ok(w.bulk_scorer(context)?.map(BulkScorerEnum11::G))
            },
            BaseQueryWeightEnum::IndexSortSortedNumericDocValuesRange(w) => {
                Ok(w.bulk_scorer(context)?.map(BulkScorerEnum11::H))
            },
            BaseQueryWeightEnum::FieldExists(w) => {
                Ok(w.bulk_scorer(context)?.map(BulkScorerEnum11::I))
            },
            BaseQueryWeightEnum::ConstantScore(w) => {
                Ok(w.bulk_scorer(context)?.map(BulkScorerEnum11::J))
            },
            BaseQueryWeightEnum::Boost(w) => Ok(w.bulk_scorer(context)?.map(BulkScorerEnum11::K)),
        }
    }

    fn count(&self, context: &LeafReaderContext<LR>) -> Result<i32> {
        match self {
            BaseQueryWeightEnum::Term(w) => w.count(context),
            BaseQueryWeightEnum::MatchAll(w) => w.count(context),
            BaseQueryWeightEnum::PointRange(w) => w.count(context),
            BaseQueryWeightEnum::MatchNoDocs(w) => w.count(context),
            BaseQueryWeightEnum::SortedNumericDocValuesSet(w) => w.count(context),
            BaseQueryWeightEnum::SortedNumericDocValuesRange(w) => w.count(context),
            BaseQueryWeightEnum::SortedSetDocValuesRange(w) => w.count(context),
            BaseQueryWeightEnum::IndexSortSortedNumericDocValuesRange(w) => w.count(context),
            BaseQueryWeightEnum::FieldExists(w) => w.count(context),
            BaseQueryWeightEnum::ConstantScore(w) => w.count(context),
            BaseQueryWeightEnum::Boost(w) => w.count(context),
        }
    }

    fn default_count(&self, context: &LeafReaderContext<LR>) -> Result<i32> {
        match self {
            BaseQueryWeightEnum::Term(w) => w.default_count(context),
            BaseQueryWeightEnum::MatchAll(w) => w.default_count(context),
            BaseQueryWeightEnum::PointRange(w) => w.default_count(context),
            BaseQueryWeightEnum::MatchNoDocs(w) => w.default_count(context),
            BaseQueryWeightEnum::SortedNumericDocValuesSet(w) => w.default_count(context),
            BaseQueryWeightEnum::SortedNumericDocValuesRange(w) => w.default_count(context),
            BaseQueryWeightEnum::SortedSetDocValuesRange(w) => w.default_count(context),
            BaseQueryWeightEnum::IndexSortSortedNumericDocValuesRange(w) => {
                w.default_count(context)
            },
            BaseQueryWeightEnum::FieldExists(w) => w.default_count(context),
            BaseQueryWeightEnum::ConstantScore(w) => w.default_count(context),
            BaseQueryWeightEnum::Boost(w) => w.default_count(context),
        }
    }

    fn is_weight_cacheable(&self) -> bool {
        match self {
            BaseQueryWeightEnum::Term(w) => w.is_weight_cacheable(),
            BaseQueryWeightEnum::MatchAll(w) => w.is_weight_cacheable(),
            BaseQueryWeightEnum::PointRange(w) => w.is_weight_cacheable(),
            BaseQueryWeightEnum::MatchNoDocs(w) => w.is_weight_cacheable(),
            BaseQueryWeightEnum::SortedNumericDocValuesSet(w) => w.is_weight_cacheable(),
            BaseQueryWeightEnum::SortedNumericDocValuesRange(w) => w.is_weight_cacheable(),
            BaseQueryWeightEnum::SortedSetDocValuesRange(w) => w.is_weight_cacheable(),
            BaseQueryWeightEnum::IndexSortSortedNumericDocValuesRange(w) => w.is_weight_cacheable(),
            BaseQueryWeightEnum::FieldExists(w) => w.is_weight_cacheable(),
            BaseQueryWeightEnum::ConstantScore(w) => w.is_weight_cacheable(),
            BaseQueryWeightEnum::Boost(w) => w.is_weight_cacheable(),
        }
    }
}
impl BooleanClauseQuery {
    pub(crate) fn create_weight_no_boolean<IRC, QC>(
        self,
        searcher: &IndexSearcher<IRC, QC>,
        score_mode: &ScoreMode,
        boost: f32,
        per_reader_term_state: Option<TermStates<IRCTermState<IRC>>>,
    ) -> Result<BaseQueryWeightEnum<IRCLeafReader<IRC>, QC>>
    where
        IRC: IndexReaderContext,
        QC: QueryCache,
    {
        match self {
            BooleanClauseQuery::Base(query) => match query {
                BaseQuery::Term(t) => Ok(BaseQueryWeightEnum::Term(t.create_weight(
                    searcher,
                    score_mode,
                    boost,
                    per_reader_term_state,
                )?)),
                BaseQuery::MatchAll(m) => Ok(BaseQueryWeightEnum::MatchAll(m.create_weight(
                    searcher,
                    score_mode,
                    boost,
                    per_reader_term_state,
                )?)),
                BaseQuery::PointRange(p) => Ok(BaseQueryWeightEnum::PointRange(p.create_weight(
                    searcher,
                    score_mode,
                    boost,
                    per_reader_term_state,
                )?)),
                BaseQuery::MatchNoDoc(p) => Ok(BaseQueryWeightEnum::MatchNoDocs(p.create_weight(
                    searcher,
                    score_mode,
                    boost,
                    per_reader_term_state,
                )?)),
                BaseQuery::SortedNumericDocValuesSet(p) => {
                    Ok(BaseQueryWeightEnum::SortedNumericDocValuesSet(
                        p.create_weight(searcher, score_mode, boost, per_reader_term_state)?,
                    ))
                },
                BaseQuery::SortedNumericDocValuesRange(p) => {
                    Ok(BaseQueryWeightEnum::SortedNumericDocValuesRange(
                        p.create_weight(searcher, score_mode, boost, per_reader_term_state)?,
                    ))
                },
                BaseQuery::SortedSetDocValuesRange(p) => {
                    Ok(BaseQueryWeightEnum::SortedSetDocValuesRange(
                        p.create_weight(searcher, score_mode, boost, per_reader_term_state)?,
                    ))
                },
                BaseQuery::IndexSortSortedNumericDocValuesRange(p) => {
                    Ok(BaseQueryWeightEnum::IndexSortSortedNumericDocValuesRange(
                        p.create_weight(searcher, score_mode, boost, per_reader_term_state)?,
                    ))
                },
                BaseQuery::FieldExists(p) => Ok(BaseQueryWeightEnum::FieldExists(p.create_weight(
                    searcher,
                    score_mode,
                    boost,
                    per_reader_term_state,
                )?)),
                BaseQuery::Dummy(_) => Err(LuceneError::unsupported_operation(
                    "DummyQuery does not support weight creation".to_string(),
                )),
                BaseQuery::Boost(p) => Ok(BaseQueryWeightEnum::Boost(p.create_weight(
                    searcher,
                    score_mode,
                    boost,
                    per_reader_term_state,
                )?)),
            },
            BooleanClauseQuery::ConstantScore(c) => Ok(BaseQueryWeightEnum::ConstantScore(
                c.create_weight(searcher, score_mode, boost, per_reader_term_state)?,
            )),
        }
    }
}
