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
use crate::core::analysis::analyzer::Analyzer;
use crate::core::document::document::Document;
use crate::core::document::field::Store;
use crate::core::document::string_field::StringField;
use crate::core::document::text_field::TextField;
use crate::core::index::term::Term;
use crate::core::search::boolean_clause::Occur;
use crate::core::search::boolean_query::Builder as BooleanQueryBuilder;
use crate::core::search::query::{IntoQuery, Query};
use crate::core::search::term_query::TermQuery;
use crate::core::store::directory::DirEnum;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::search::check_hits::CheckHits;
use crate::test_framework::core::util::lucene_test_case::{
  new_directory_shared, new_index_writer_config_with_analyzer, new_log_merge_policy,
  new_searcher_with_reader,
};
use crate::test_framework::core::util::{DefaultCRReaderShared, DefaultIndexSearchCRShared};
use rand::{Rng, RngExt};
use std::sync::Arc;

/// Tests primitive queries (ie: that rewrite to themselves) to insure they match the expected set of
/// docs, and that the score of each match is equal to the value of the scores explanation.
///
/// The assumption is that if all of the "primitive" queries work well, then anything that
/// rewrites to a primitive will work well also.
pub trait BaseExplanationTestCase {
  fn initialize(&mut self) -> Result<()> {
    Ok(())
  }
  /// check the expDocNrs match and have scores that match the explanations. Query may be randomly
  /// wrapped in a BooleanQuery with a term that matches no documents.
  fn q_test<R, Q>(
    &self,
    random: &mut R,
    searcher: &DefaultIndexSearchCRShared,
    q: Q,
    exp_doc_nrs: &[i32],
  ) -> Result<()>
  where
    R: Rng + ?Sized,
    Q: IntoQuery,
  {
    self.default_q_test(random, searcher, q, exp_doc_nrs)
  }
  fn default_q_test<R, Q, SIM>(
    &self,
    random: &mut R,
    searcher: &DefaultIndexSearchCRShared<SIM>,
    q: Q,
    exp_doc_nrs: &[i32],
  ) -> Result<()>
  where
    SIM: crate::core::search::similarities_impl::similarities::Similarity,
    R: Rng + ?Sized,
    Q: IntoQuery,
  {
    let mut q = q.into_query();
    if random.random_bool(0.5) {
      let mut bq = BooleanQueryBuilder::new();
      bq.add(q, Occur::Should)?;
      bq.add(
        TermQuery::new(Term::from_text("NEVER", "MATCH")),
        Occur::Should,
      )?;
      q = bq.build().into();
    }
    CheckHits::check_hit_collector(random, q, FIELD, searcher, exp_doc_nrs)
  }

  /// Tests a query using qtest after wrapping it with both optB and reqB
  ///
  /// See also: q_test, req_b, opt_b.
  fn bq_test<R, Q>(
    &self,
    random: &mut R,
    searcher: &DefaultIndexSearchCRShared,
    q: Q,
    exp_doc_nrs: &[i32],
  ) -> Result<()>
  where
    R: Rng + ?Sized,
    Q: IntoQuery + Clone,
  {
    self.q_test(random, searcher, self.req_b(q.clone())?, exp_doc_nrs)?;
    self.q_test(random, searcher, self.opt_b(q)?, exp_doc_nrs)
  }

  /// Convenience implementation of TermsQuery.
  fn match_these_items(&self, terms: &[i32]) -> Result<Query> {
    let mut query = BooleanQueryBuilder::new();
    for term in terms {
      query.add(
        TermQuery::new(Term::new(KEY, term.to_string())),
        Occur::Should,
      )?;
    }
    Ok(query.build().into())
  }

  /// helper for generating MultiPhraseQueries
  fn ta(&self, s: &[&str]) -> Vec<Term> {
    s.iter().map(|term| Term::from_text(FIELD, *term)).collect()
  }

  /// MACRO: Wraps a Query in a BooleanQuery so that it is optional, along with a second prohibited
  /// clause which will never match anything
  fn opt_b<Q>(&self, q: Q) -> Result<Query>
  where
    Q: IntoQuery,
  {
    let mut bq = BooleanQueryBuilder::new();
    bq.add(q, Occur::Should)?;
    bq.add(
      TermQuery::new(Term::from_text("NEVER", "MATCH")),
      Occur::MustNot,
    )?;
    Ok(bq.build().into())
  }

  /// MACRO: Wraps a Query in a BooleanQuery so that it is required, along with a second optional
  /// clause which will match everything
  fn req_b<Q>(&self, q: Q) -> Result<Query>
  where
    Q: IntoQuery,
  {
    let mut bq = BooleanQueryBuilder::new();
    bq.add(q, Occur::Must)?;
    bq.add(TermQuery::new(Term::from_text(FIELD, "w1")), Occur::Should)?;
    Ok(bq.build().into())
  }
}

pub const KEY: &str = "KEY";
// boost on this field is the same as the iterator for the doc
pub const FIELD: &str = "field";
// same contents, but no field boost
pub const ALTFIELD: &str = "alt";

pub(crate) const DOC_FIELDS: [&str; 4] = [
  "w1 w2 w3 w4 w5",
  "w1 w3 w2 w3 zz",
  "w1 xx w2 yy w3",
  "w1 w3 xx w2 yy w3 zz",
];

pub struct BaseExplanationTestContext {
  pub searcher: DefaultIndexSearchCRShared,
  pub reader: DefaultCRReaderShared,
  pub directory: Arc<DirEnum>,
  pub analyzer: Arc<MockAnalyzer>,
}

pub fn before_class_test_explanations<R>(random: &mut R) -> Result<BaseExplanationTestContext>
where
  R: Rng + ?Sized,
{
  let directory = new_directory_shared(random)?;
  let analyzer = Arc::new(MockAnalyzer::new(random));
  let shared_analyzer: Box<dyn Analyzer> = Box::new(analyzer.clone());
  let mut config = new_index_writer_config_with_analyzer(random, shared_analyzer)?;
  config.set_merge_policy(new_log_merge_policy(random)?);
  let writer = RandomIndexWriter::with_config(random, directory.clone(), config);

  for i in 0..DOC_FIELDS.len() {
    writer.add_document(random, create_doc(i)?)?;
  }
  let reader = Arc::new(writer.get_reader(random)?);
  writer.close(random)?;
  let searcher = new_searcher_with_reader(reader.clone())?;
  Ok(BaseExplanationTestContext {
    searcher,
    reader,
    directory,
    analyzer,
  })
}
pub(crate) fn create_doc(index: usize) -> Result<Document> {
  let mut doc = Document::new();
  doc.add(StringField::from_string(KEY, index.to_string(), Store::No)?);
  doc.add(TextField::from_string(FIELD, DOC_FIELDS[index], Store::No)?);
  doc.add(TextField::from_string(
    ALTFIELD,
    DOC_FIELDS[index],
    Store::No,
  )?);
  Ok(doc)
}
