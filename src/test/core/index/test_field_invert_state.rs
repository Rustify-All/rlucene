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
use crate::core::analysis::token_attributes::position_increment_attribute::PositionIncrementAttribute;
use crate::core::document::document::Document;
use crate::core::document::field::Field;
use crate::core::document::fields::FieldTokenStreamEnum;
use crate::core::index::field_invert_state::FieldInvertState;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::search::collection_statistics::CollectionStatistics;
use crate::core::search::similarities_impl::similarities::{
  BoxSimScorer, Similarity, SimilarityEnum,
};
use crate::core::search::term_statistics::TermStatistics;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::test_framework::core::analysis::canned_token_stream::CannedTokenStream;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::analysis::token;
use crate::test_framework::core::analysis::token::Token;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, new_directory_shared, new_index_writer_config_with_analyzer, random,
};
use crate::test_framework::core::util::test_util::TestUtil;
use parking_lot::Mutex;
use rand::RngExt;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
#[allow(dead_code)] // for quick search
pub struct TestFieldInvertState;

struct NeverForgetsSimilarity {
  last_state: Arc<Mutex<FieldInvertState>>,
}

impl NeverForgetsSimilarity {
  fn new(last_state: Arc<Mutex<FieldInvertState>>) -> Self {
    Self { last_state }
  }
}

impl Display for NeverForgetsSimilarity {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

impl Similarity for NeverForgetsSimilarity {
  fn compute_norm(&self, state: &FieldInvertState) -> Result<i64> {
    self.last_state.lock().clone_from(state);
    Ok(1)
  }

  type SimScorer = BoxSimScorer;

  fn scorer(
    &self,
    _boost: f32,
    _collection_stats: &CollectionStatistics,
    _term_stats: &[TermStatistics],
  ) -> Result<Self::SimScorer> {
    Err(LuceneError::unsupported_operation(""))
  }
}

#[test]
fn test_basic() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let fis = Arc::new(Mutex::new(FieldInvertState::default()));
  iwc.set_similarity(SimilarityEnum::custom(NeverForgetsSimilarity::new(
    fis.clone(),
  )));

  let w = IndexWriter::new(dir, iwc)?;
  let mut doc = Document::new();
  let field = Field::from_token_stream(
    "field",
    FieldTokenStreamEnum::custom(CannedTokenStream::new(vec![
      token::with_range(Some("a"), 0, 1)?,
      token::with_range(Some("b"), 2, 3)?,
      token::with_range(Some("c"), 4, 5)?,
    ])),
    crate::core::document::text_field::TYPE_NOT_STORED.clone(),
  )?;
  doc.add(field);

  w.add_document(doc)?;
  let fis = fis.lock();
  assert_eq!(1, fis.get_max_term_frequency());
  assert_eq!(3, fis.get_unique_term_count());
  assert_eq!(0, fis.num_overlap());
  assert_eq!(3, fis.get_length());

  w.close()?;

  Ok(())
}

#[test]
fn test_random() -> Result<()> {
  let mut random = random();
  let num_unique_tokens = TestUtil::next_int(&mut random, 1, 25);
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let fis = Arc::new(Mutex::new(FieldInvertState::default()));
  iwc.set_similarity(SimilarityEnum::custom(NeverForgetsSimilarity::new(
    fis.clone(),
  )));

  let w = IndexWriter::new(dir, iwc)?;
  let mut doc = Document::new();

  let num_tokens = at_least(&mut random, 10000) as usize;
  let mut tokens: Vec<Token> = Vec::with_capacity(num_tokens);
  let mut counts: HashMap<char, i32> = HashMap::new();
  let mut num_stacked = 0;
  let mut max_term_freq = 0;
  let mut pos = -1;

  for i in 0..num_tokens {
    let token_char = char::from(b'a' + random.random_range(0..num_unique_tokens) as u8);
    let new_count = counts.get(&token_char).copied().unwrap_or(0) + 1;
    counts.insert(token_char, new_count);
    max_term_freq = max_term_freq.max(new_count);

    let mut token = token::with_range(
      Some(&token_char.to_string()),
      2 * i as i32,
      2 * i as i32 + 1,
    )?;
    if i > 0 && random.random_range(0..7) == 3 {
      token.sub.set_position_increment(0)?;
      num_stacked += 1;
    } else {
      pos += 1;
    }
    tokens.push(token);
  }

  let field = Field::from_token_stream(
    "field",
    FieldTokenStreamEnum::custom(CannedTokenStream::new(tokens)),
    crate::core::document::text_field::TYPE_NOT_STORED.clone(),
  )?;
  doc.add(field);

  w.add_document(doc)?;
  let fis = fis.lock();
  assert_eq!(max_term_freq, fis.get_max_term_frequency());
  assert_eq!(counts.len() as i32, fis.get_unique_term_count());
  assert_eq!(num_stacked, fis.num_overlap());
  assert_eq!(num_tokens as i32, fis.get_length());
  assert_eq!(pos, fis.position());

  w.close()?;

  Ok(())
}
