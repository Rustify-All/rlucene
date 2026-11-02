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
use crate::core::index::index_reader::Identity;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::term::Term;
use crate::core::search::boolean_clause::Occur;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::phrase_query::{Builder as PhraseQueryBuilder, PhraseQuery};
use crate::core::search::query::{Query, QueryBase};
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::util::HasIdentity;
use crate::core::util::error::lucene_error::Result;
use std::hash::{Hash, Hasher};

/// This is a [`PhraseQuery`] which is optimized for n-gram phrase query. For example, when you
/// query "ABCD" on a 2-gram field, you may want to use NGramPhraseQuery rather than
/// [`PhraseQuery`], because NGramPhraseQuery will [`Query::rewrite`] the query to
/// "AB/0 CD/2", while [`PhraseQuery`] will query "AB/0 BC/1 CD/2" (where term/position).
#[derive(Debug, Clone)]
pub struct NGramPhraseQuery {
  id: Identity,
  n: usize,
  phrase_query: PhraseQuery,
}

impl NGramPhraseQuery {
  /// Creates a query with the given n-gram size.
  pub fn new(n: usize, phrase_query: PhraseQuery) -> Self {
    Self {
      id: Identity::new(),
      n,
      phrase_query,
    }
  }

  /// Return the n in n-gram.
  pub fn get_n(&self) -> usize {
    self.n
  }

  /// Return the list of terms.
  pub fn get_terms(&self) -> &[Term] {
    self.phrase_query.get_terms()
  }

  /// Return the list of relative positions that each term should appear at.
  pub fn get_positions(&self) -> &[usize] {
    self.phrase_query.get_positions()
  }
}

impl Eq for NGramPhraseQuery {}

impl PartialEq for NGramPhraseQuery {
  fn eq(&self, other: &Self) -> bool {
    self.n == other.n && self.phrase_query == other.phrase_query
  }
}

impl Hash for NGramPhraseQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    std::any::type_name::<Self>().hash(state);
    self.phrase_query.hash(state);
    self.n.hash(state);
  }
}

impl HasIdentity for NGramPhraseQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl QueryBase for NGramPhraseQuery {
  type Weight<IRC, SIM>
    = <PhraseQuery as QueryBase>::Weight<IRC, SIM>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static;

  fn to_string(&self, field: &str) -> Result<String> {
    self.phrase_query.to_string(field)
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
    IndexSearcher<IRC, SIM>: Sync,
    Self: Sized,
  {
    self.phrase_query.create_weight(searcher, score_mode, boost)
  }

  fn rewrite<IRC, SIM>(&self, searcher: &IndexSearcher<IRC, SIM>) -> Result<Option<Query>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    IndexSearcher<IRC, SIM>: Sync,
    Self: Sized,
  {
    let terms = self.phrase_query.get_terms();
    let positions = self.phrase_query.get_positions();

    let is_optimizable = self.phrase_query.get_slop() == 0
      && self.n >= 2
      && terms.len() >= 3
      && positions
        .windows(2)
        .all(|window| window[1] == window[0] + 1);

    if !is_optimizable {
      return Ok(Some(
        self
          .phrase_query
          .rewrite(searcher)?
          .unwrap_or_else(|| self.phrase_query.clone().into()),
      ));
    }
    let n = self.n;
    let terms = self.phrase_query.get_terms();

    let terms_len = terms.len();
    let selected_count = (terms_len - 1).div_ceil(n) + 1;
    let mut builder = PhraseQueryBuilder::with_capacity(selected_count);

    let last = terms_len - 1;
    for i in (0..last).step_by(n) {
      builder.add(terms[i].clone(), i)?;
    }
    builder.add(terms[last].clone(), last)?;
    Ok(Some(builder.build()?.into()))
  }

  fn visit<QV>(&self, visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    let query = self.into();
    let mut visitor = visitor.get_sub_visitor(Occur::Must, query);
    self.phrase_query.visit(&mut visitor)
  }
}

impl crate::core::util::accountable::Accountable for NGramPhraseQuery {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(crate::core::util::ram_usage_estimator::QUERY_DEFAULT_RAM_BYTES_USED)
  }
}
