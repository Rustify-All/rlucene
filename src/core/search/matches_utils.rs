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
use crate::core::search::disjunction_matches_iterator::from_sub_iterators;
use crate::core::search::matches::Matches;
use crate::core::search::query::{QueryWeightMatches, QueryWeightMatchesIterator};
use crate::core::search::query_matches::FieldMatchesSupplier;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::util::error::lucene_error::Result;
use std::cell::{Cell, OnceCell};
use std::sync::LazyLock;

#[allow(dead_code)] // for quick search
pub struct MatchesUtils;

pub static MATCH_WITH_NO_TERMS: LazyLock<MatchWithNoTerms> = LazyLock::new(|| MatchWithNoTerms);
pub struct MatchWithNoTerms;
impl Matches for MatchWithNoTerms {
  type Iterator<'a> = crate::core::search::dummy::dummy_matches_iterator::DummyMatchesIterator;
  type SubMatches = Self;
  fn get_matches(&self, _field: &str) -> Result<Option<Self::Iterator<'_>>> {
    Ok(None)
  }

  fn get_sub_matches(&self) -> &[Self::SubMatches] {
    &[]
  }

  fn field(&self) -> &[String] {
    &[]
  }
}

pub struct CombinedMatch<
  'a,
  IRC: IndexReaderContext + 'static,
  SIM = crate::core::search::similarities_impl::bm25_similarity::BM25Similarity,
> where
  SIM: Similarity,
{
  sub: Vec<QueryWeightMatches<'a, IRC, SIM>>,
  fields: OnceCell<Vec<String>>,
}
impl<'a, IRC: IndexReaderContext + 'static, SIM> CombinedMatch<'a, IRC, SIM>
where
  SIM: Similarity,
{
  pub fn new(sub: Vec<QueryWeightMatches<'a, IRC, SIM>>) -> Self {
    CombinedMatch::<_, SIM> {
      sub,
      fields: OnceCell::new(),
    }
  }
}

impl<'a, IRC: IndexReaderContext + 'static, SIM> Matches for CombinedMatch<'a, IRC, SIM>
where
  SIM: Similarity,
{
  type Iterator<'b>
    = QueryWeightMatchesIterator<'b, IRCLeafReader<IRC>, SIM>
  where
    Self: 'b;
  type SubMatches = QueryWeightMatches<'a, IRC, SIM>;
  fn get_matches(
    &self,
    field: &str,
  ) -> Result<Option<QueryWeightMatchesIterator<'_, IRCLeafReader<IRC>, SIM>>> {
    let mut sub_iterators = smallvec::SmallVec::<[_; 1]>::new();
    for matches in &self.sub {
      if let Some(iterator) = matches.get_matches(field)? {
        sub_iterators.push(iterator);
      }
    }
    if sub_iterators.len() <= 1 {
      return Ok(sub_iterators.pop());
    }
    from_sub_iterators(sub_iterators.into_vec())
  }

  fn get_sub_matches(&self) -> &[QueryWeightMatches<'a, IRC, SIM>] {
    &self.sub
  }

  fn field(&self) -> &[String] {
    self.fields.get_or_init(|| {
      let mut fields = Vec::new();
      for matches in &self.sub {
        for field in matches.field() {
          if !fields.contains(field) {
            fields.push(field.clone());
          }
        }
      }
      fields
    })
  }
}

pub fn from_sub_matches<'a, IRC: IndexReaderContext + 'static, SIM>(
  mut sub_matches: Vec<QueryWeightMatches<'a, IRC, SIM>>,
) -> Option<QueryWeightMatches<'a, IRC, SIM>>
where
  SIM: Similarity,
{
  if sub_matches.is_empty() {
    return None;
  }

  let mut match_index = 0;
  let mut match_count = 0;
  for (index, matches) in sub_matches.iter().enumerate() {
    if !matches!(matches, QueryWeightMatches::MatchWithNoTerms(_)) {
      match_index = index;
      match_count += 1;
    }
  }

  if match_count == 0 {
    return Some(QueryWeightMatches::MatchWithNoTerms(MatchWithNoTerms));
  }
  if match_count == 1 {
    return Some(sub_matches.swap_remove(match_index));
  }
  Some(QueryWeightMatches::Combined(Box::new(CombinedMatch::new(
    sub_matches,
  ))))
}

pub struct FieldMatches<
  'a,
  IRC: IndexReaderContext + 'static,
  SIM = crate::core::search::similarities_impl::bm25_similarity::BM25Similarity,
> where
  SIM: Similarity,
{
  field: FieldName<'a>,
  supplier: FieldMatchesSupplier<'a, IRC, SIM>,
  cached: Cell<Option<QueryWeightMatchesIterator<'a, IRCLeafReader<IRC>, SIM>>>,
}

impl<'a, IRC: IndexReaderContext + 'static, SIM> Matches for FieldMatches<'a, IRC, SIM>
where
  SIM: Similarity,
{
  type Iterator<'b>
    = QueryWeightMatchesIterator<'b, IRCLeafReader<IRC>, SIM>
  where
    Self: 'b;
  type SubMatches = QueryWeightMatches<'a, IRC, SIM>;
  fn get_matches(
    &self,
    field: &str,
  ) -> Result<Option<QueryWeightMatchesIterator<'_, IRCLeafReader<IRC>, SIM>>> {
    if field != self.field.as_ref().as_str() {
      return Ok(None);
    }
    if let Some(iterator) = self.cached.take() {
      Ok(Some(iterator))
    } else {
      self.supplier.get()
    }
  }

  fn get_sub_matches(&self) -> &[QueryWeightMatches<'a, IRC, SIM>] {
    &[]
  }

  fn field(&self) -> &[String] {
    std::slice::from_ref(self.field.as_ref())
  }
}

pub enum FieldName<'a> {
  Borrowed(&'a String),
  Owned(String),
}

impl AsRef<String> for FieldName<'_> {
  fn as_ref(&self) -> &String {
    match self {
      Self::Borrowed(field) => field,
      Self::Owned(field) => field,
    }
  }
}

impl<'a> From<&'a String> for FieldName<'a> {
  fn from(field: &'a String) -> Self {
    Self::Borrowed(field)
  }
}

impl From<String> for FieldName<'_> {
  fn from(field: String) -> Self {
    Self::Owned(field)
  }
}

pub fn for_field<'a, IRC: IndexReaderContext + 'static, FName, SIM>(
  field: FName,
  supplier: FieldMatchesSupplier<'a, IRC, SIM>,
) -> Result<Option<QueryWeightMatches<'a, IRC, SIM>>>
where
  SIM: Similarity,
  FName: Into<FieldName<'a>>,
{
  let field = field.into();
  let first = supplier.get()?;
  let Some(first) = first else {
    return Ok(None);
  };
  Ok(Some(QueryWeightMatches::Field(Box::new(FieldMatches::<
    _,
    SIM,
  > {
    field,
    supplier,
    cached: Cell::new(Some(first)),
  }))))
}
