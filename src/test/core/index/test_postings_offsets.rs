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
use crate::core::analysis::analyzer::{
  Analyzer, AnalyzerEnum, AnalyzerStoredValue, TokenStreamComponents,
};
use crate::core::analysis::token_attributes::offset_attribute::OffsetAttribute;
use crate::core::analysis::token_attributes::payload_attribute::PayloadAttribute;
use crate::core::analysis::token_attributes::type_attribute::TypeAttribute;
use crate::core::analysis::token_stream::TokenStream;
use crate::core::document::document::Document;
use crate::core::document::field::{Field, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::fields::FieldTokenStreamEnum;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::string_field::StringField;
use crate::core::index::BytesRef;
use crate::core::index::directory_reader;
use crate::core::index::doc_values::DocValues;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::multi_terms::{get_term_postings_enum, get_term_postings_enum_with_flag};
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::postings_enum::{ALL, FREQS, PostingsEnum};
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::analysis::canned_token_stream::CannedTokenStream;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::analysis::mock_payload_analyzer::MockPayloadAnalyzer;
use crate::test_framework::core::analysis::mock_tokenizer::{KEYWORD, MockTokenizer};
use crate::test_framework::core::analysis::token;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::util::english::English;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, new_directory_shared, new_index_writer_config_with_analyzer, new_log_merge_policy,
  random, random_from_seed,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::RngExt;
use std::collections::HashMap;

#[allow(dead_code)] // for quick search
pub struct TestPostingsOffsets;

#[test]
fn test_basic() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let w = RandomIndexWriter::with_config(&mut random, dir, iwc);
  let mut doc = Document::new();

  let mut ft = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  ft.set_index_options(IndexOptions::DocsAndFreqsAndPositionsAndOffsets)?;
  if random.random_bool(0.5) {
    ft.set_store_term_vectors(true)?;
    ft.set_store_term_vector_positions(random.random_bool(0.5))?;
    ft.set_store_term_vector_offsets(random.random_bool(0.5))?;
  }
  let tokens = vec![
    token::with_pos_inc("a", 1, 0, 6)?,
    token::with_pos_inc("b", 1, 8, 9)?,
    token::with_pos_inc("a", 1, 9, 17)?,
    token::with_pos_inc("c", 1, 19, 50)?,
  ];
  doc.add(Field::from_token_stream(
    "content",
    FieldTokenStreamEnum::custom(CannedTokenStream::new(tokens)),
    ft,
  )?);

  w.add_document(&mut random, doc)?;
  let r = w.get_reader(&mut random)?;
  w.close(&mut random)?;

  let mut dp = get_term_postings_enum(&r, "content", &BytesRef::from_string("a"))?
    .expect("postings enum for term 'a' must exist");
  assert_eq!(0, dp.next_doc()?);
  assert_eq!(2, dp.freq()?);
  assert_eq!(0, dp.next_position()?);
  assert_eq!(0, dp.start_offset()?);
  assert_eq!(6, dp.end_offset()?);
  assert_eq!(2, dp.next_position()?);
  assert_eq!(9, dp.start_offset()?);
  assert_eq!(17, dp.end_offset()?);
  assert_eq!(NO_MORE_DOCS, dp.next_doc()?);

  let mut dp = get_term_postings_enum(&r, "content", &BytesRef::from_string("b"))?
    .expect("postings enum for term 'b' must exist");
  assert_eq!(0, dp.next_doc()?);
  assert_eq!(1, dp.freq()?);
  assert_eq!(1, dp.next_position()?);
  assert_eq!(8, dp.start_offset()?);
  assert_eq!(9, dp.end_offset()?);
  assert_eq!(NO_MORE_DOCS, dp.next_doc()?);

  let mut dp = get_term_postings_enum(&r, "content", &BytesRef::from_string("c"))?
    .expect("postings enum for term 'c' must exist");
  assert_eq!(0, dp.next_doc()?);
  assert_eq!(1, dp.freq()?);
  assert_eq!(3, dp.next_position()?);
  assert_eq!(19, dp.start_offset()?);
  assert_eq!(50, dp.end_offset()?);
  assert_eq!(NO_MORE_DOCS, dp.next_doc()?);

  Ok(())
}

#[test]
fn test_skipping() -> Result<()> {
  do_test_numbers(false)
}

#[test]
fn test_payloads() -> Result<()> {
  do_test_numbers(true)
}
fn do_test_numbers(with_payloads: bool) -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer: AnalyzerEnum = if with_payloads {
    MockPayloadAnalyzer::new().into()
  } else {
    MockAnalyzer::new(&mut random).into()
  };
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  iwc.set_merge_policy(new_log_merge_policy(&mut random)?);
  let w = RandomIndexWriter::with_config(&mut random, dir, iwc);

  let mut ft = FieldType::from_ref(&*crate::core::document::text_field::TYPE_STORED)?;
  ft.set_index_options(IndexOptions::DocsAndFreqsAndPositionsAndOffsets)?;
  if random.random_bool(0.5) {
    ft.set_store_term_vectors(true)?;
    ft.set_store_term_vector_offsets(random.random_bool(0.5))?;
    ft.set_store_term_vector_positions(random.random_bool(0.5))?;
  }

  let num_docs = at_least(&mut random, 500);
  for i in 0..num_docs {
    let mut doc = Document::new();
    doc.add(Field::from_string(
      "numbers",
      English::int_to_english(i),
      ft.clone(),
    )?);
    doc.add(Field::from_string(
      "oddeven",
      if i % 2 == 0 { "even" } else { "odd" },
      ft.clone(),
    )?);
    doc.add(StringField::from_string("id", i.to_string(), Store::No)?);
    w.add_document(&mut random, doc)?;
  }

  let reader = w.get_reader(&mut random)?;
  w.close(&mut random)?;

  let terms = [
    "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "hundred",
  ];

  for term in terms {
    let mut dp = get_term_postings_enum(&reader, "numbers", &BytesRef::from_string(term))?
      .unwrap_or_else(|| panic!("postings enum for term '{term}' must exist"));
    let mut stored_fields = reader.stored_fields()?;
    while dp.next_doc()? != NO_MORE_DOCS {
      let doc = dp.doc_id();
      let stored_numbers = stored_fields
        .document(doc)?
        .get("numbers")?
        .expect("stored numbers field must exist")
        .into_owned();
      let freq = dp.freq()?;
      for _ in 0..freq {
        dp.next_position()?;
        let start = dp.start_offset()?;
        assert!(start >= 0);
        let end = dp.end_offset()?;
        assert!(end >= 0 && end >= start);
        assert_eq!(term, &stored_numbers[start as usize..end as usize]);
        if with_payloads {
          let payload = dp.get_payload()?.expect("payload must exist");
          assert!(payload.utf8_to_string()?.starts_with("pos:"));
        }
      }
    }
  }

  let num_skipping_tests = at_least(&mut random, 50);
  for _ in 0..num_skipping_tests {
    let num = TestUtil::next_int(&mut random, 100, (num_docs - 1).min(999));
    let mut dp = get_term_postings_enum(&reader, "numbers", &BytesRef::from_string("hundred"))?
      .expect("postings enum for term 'hundred' must exist");
    let mut stored_fields = reader.stored_fields()?;
    let doc = dp.advance(num)?;
    assert_eq!(num, doc);
    let stored_numbers = stored_fields
      .document(doc)?
      .get("numbers")?
      .expect("stored numbers field must exist")
      .into_owned();
    let freq = dp.freq()?;
    for _ in 0..freq {
      dp.next_position()?;
      let start = dp.start_offset()?;
      assert!(start >= 0);
      let end = dp.end_offset()?;
      assert!(end >= 0 && end >= start);
      assert_eq!("hundred", &stored_numbers[start as usize..end as usize]);
      if with_payloads {
        let payload = dp.get_payload()?.expect("payload must exist");
        assert!(payload.utf8_to_string()?.starts_with("pos:"));
      }
    }
  }

  for i in 0..num_docs {
    let mut dp =
      get_term_postings_enum_with_flag(&reader, "id", &BytesRef::from_string(&i.to_string()), 0)?
        .unwrap_or_else(|| panic!("postings enum for id '{i}' must exist"));
    assert_eq!(i, dp.next_doc()?);
    assert_eq!(NO_MORE_DOCS, dp.next_doc()?);
  }

  Ok(())
}

#[test]
fn test_random() -> Result<()> {
  let mut actual_tokens: HashMap<String, HashMap<i32, Vec<token::Token>>> = HashMap::new();

  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let w = RandomIndexWriter::with_config(&mut random, dir, iwc);

  let num_docs = at_least(&mut random, 20);

  let mut ft = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  ft.set_index_options(IndexOptions::DocsAndFreqsAndPositionsAndOffsets)?;
  if random.random_bool(0.5) {
    ft.set_store_term_vectors(true)?;
    ft.set_store_term_vector_offsets(random.random_bool(0.5))?;
    ft.set_store_term_vector_positions(random.random_bool(0.5))?;
  }

  for doc_count in 0..num_docs {
    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("id", doc_count as i64));
    let mut tokens = Vec::new();
    let num_tokens = at_least(&mut random, 100);
    let mut pos = -1;
    let mut offset = 0;

    for token_count in 0..num_tokens {
      let text = if random.random_bool(0.5) {
        "a"
      } else if random.random_bool(0.5) {
        "b"
      } else if random.random_bool(0.5) {
        "c"
      } else {
        "d"
      };

      let mut pos_incr = if random.random_bool(0.5) {
        1
      } else {
        random.random_range(0..5)
      };
      if token_count == 0 && pos_incr == 0 {
        pos_incr = 1;
      }
      let off_incr = if random.random_bool(0.5) {
        0
      } else {
        random.random_range(0..5)
      };
      let token_offset = random.random_range(0..5);

      let mut token = token::with_pos_inc(
        text,
        pos_incr,
        offset + off_incr,
        offset + off_incr + token_offset,
      )?;
      pos += pos_incr;
      token.sub.set_type(&pos.to_string());
      actual_tokens
        .entry(text.to_string())
        .or_default()
        .entry(doc_count)
        .or_default()
        .push(token.clone());
      tokens.push(token);
      offset += off_incr + token_offset;
    }
    doc.add(Field::from_token_stream(
      "content",
      FieldTokenStreamEnum::custom(CannedTokenStream::new(tokens)),
      ft.clone(),
    )?);
    w.add_document(&mut random, doc)?;
  }
  let r = w.get_reader(&mut random)?;
  w.close(&mut random)?;

  let terms = ["a", "b", "c", "d"];
  for ctx in (&r).get_context()?.leaves()? {
    let sub = ctx.reader();
    let mut terms_enum = sub
      .terms("content")?
      .expect("content terms must exist")
      .iterator()?;
    let mut doc_id_to_id = vec![0i32; sub.max_doc()? as usize];
    let mut values = DocValues::get_numeric(&sub, "id")?;
    for i in 0..sub.max_doc()? {
      assert_eq!(i, values.next_doc()?);
      doc_id_to_id[i as usize] = values.long_value()? as i32;
    }

    for term in terms {
      if terms_enum.seek_exact(&BytesRef::from_string(term))? {
        let mut docs = terms_enum.postings_with_flags(None, FREQS as i32)?;
        while docs.next_doc()? != NO_MORE_DOCS {
          let doc = docs.doc_id();
          let expected = actual_tokens
            .get(term)
            .and_then(|by_doc| by_doc.get(&doc_id_to_id[doc as usize]))
            .expect("expected tokens must exist");
          assert_eq!(expected.len() as i32, docs.freq()?);
        }

        let mut docs_and_positions = terms_enum.postings_with_flags(Some(docs), ALL as i32)?;
        while docs_and_positions.next_doc()? != NO_MORE_DOCS {
          let doc = docs_and_positions.doc_id();
          let expected = actual_tokens
            .get(term)
            .and_then(|by_doc| by_doc.get(&doc_id_to_id[doc as usize]))
            .expect("expected tokens must exist");
          assert_eq!(expected.len() as i32, docs_and_positions.freq()?);
          for token in expected {
            let pos: i32 = token
              .sub
              .type_()
              .parse()
              .expect("token type must be numeric");
            assert_eq!(pos, docs_and_positions.next_position()?);
          }
        }

        let mut docs_and_positions_and_offsets =
          terms_enum.postings_with_flags(Some(docs_and_positions), ALL as i32)?;
        while docs_and_positions_and_offsets.next_doc()? != NO_MORE_DOCS {
          let doc = docs_and_positions_and_offsets.doc_id();
          let expected = actual_tokens
            .get(term)
            .and_then(|by_doc| by_doc.get(&doc_id_to_id[doc as usize]))
            .expect("expected tokens must exist");
          assert_eq!(
            expected.len() as i32,
            docs_and_positions_and_offsets.freq()?
          );
          for token in expected {
            let pos: i32 = token
              .sub
              .type_()
              .parse()
              .expect("token type must be numeric");
            assert_eq!(pos, docs_and_positions_and_offsets.next_position()?);
            assert_eq!(
              token.sub.start_offset(),
              docs_and_positions_and_offsets.start_offset()?
            );
            assert_eq!(
              token.sub.end_offset(),
              docs_and_positions_and_offsets.end_offset()?
            );
          }
        }
      }
    }
  }

  Ok(())
}

#[test]
fn test_add_field_twice() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let iw = RandomIndexWriter::with_config(&mut random, dir, iwc);
  let mut doc = Document::new();
  let mut custom_type3 = FieldType::from_ref(&*crate::core::document::text_field::TYPE_STORED)?;
  custom_type3.set_store_term_vectors(true)?;
  custom_type3.set_store_term_vector_positions(true)?;
  custom_type3.set_store_term_vector_offsets(true)?;
  custom_type3.set_index_options(IndexOptions::DocsAndFreqsAndPositionsAndOffsets)?;
  doc.add(Field::from_string(
    "content3",
    "here is more content with aaa aaa aaa",
    custom_type3.clone(),
  )?);
  doc.add(Field::from_string(
    "content3",
    "here is more content with aaa aaa aaa",
    custom_type3,
  )?);
  iw.add_document(&mut random, doc)?;
  iw.close(&mut random)?;
  Ok(())
}

#[test]
fn test_negative_offsets() {
  assert!(token::with_pos_inc("foo", 1, -1, -1).is_err());
}

#[test]
fn test_illegal_offsets() {
  assert!(token::with_pos_inc("foo", 1, 1, 0).is_err());
}

#[test]
fn test_illegal_offsets_across_field_instances() -> Result<()> {
  assert!(
    check_tokens(
      vec![token::with_pos_inc("use", 1, 150, 160)?],
      Some(vec![token::with_pos_inc("use", 1, 50, 60)?]),
    )
    .is_err()
  );
  Ok(())
}

#[test]
fn test_backwards_offsets() -> Result<()> {
  assert!(
    check_tokens(
      vec![
        token::with_pos_inc("foo", 1, 0, 3)?,
        token::with_pos_inc("foo", 1, 4, 7)?,
        token::with_pos_inc("foo", 0, 3, 6)?,
      ],
      None,
    )
    .is_err()
  );
  Ok(())
}

#[test]
fn test_stacked_tokens() -> Result<()> {
  check_tokens(
    vec![
      token::with_pos_inc("foo", 1, 0, 3)?,
      token::with_pos_inc("foo", 0, 0, 3)?,
      token::with_pos_inc("foo", 0, 0, 3)?,
    ],
    None,
  )
}
#[test]
fn test_crazy_offset_gap() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = CrazyOffsetGapAnalyzer::new(random.random());
  let iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let iw = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

  iw.add_document(&mut random, Document::new())?;

  assert!(
    (|| -> Result<()> {
      let mut ft = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
      ft.set_index_options(IndexOptions::DocsAndFreqsAndPositionsAndOffsets)?;

      let mut doc = Document::new();
      doc.add(Field::from_string("foo", "bar", ft.clone())?);
      doc.add(Field::from_string("foo", "bar", ft)?);
      iw.add_document(&mut random, doc)?;
      Ok(())
    })()
    .is_err()
  );
  iw.commit(&mut random)?;
  iw.close(&mut random)?;

  let r = directory_reader::open(dir)?;
  assert_eq!(1, r.num_docs()?);
  Ok(())
}

#[test]
fn test_legal_but_very_large_offsets() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let iw = RandomIndexWriter::with_config(&mut random, dir, iwc);
  let mut doc = Document::new();
  let mut t1 = token::with_range(Some("foo"), 0, i32::MAX - 500)?;
  if random.random_bool(0.5) {
    t1.sub
      .token
      .set_payload(Some(BytesRef::from_string("test")));
  }
  let t2 = token::with_range(Some("foo"), i32::MAX - 500, i32::MAX)?;
  let token_stream = CannedTokenStream::new(vec![t1, t2]);
  let mut ft = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  ft.set_index_options(IndexOptions::DocsAndFreqsAndPositionsAndOffsets)?;
  ft.set_store_term_vectors(true)?;
  ft.set_store_term_vector_positions(true)?;
  ft.set_store_term_vector_offsets(true)?;
  doc.add(Field::from_token_stream(
    "foo",
    FieldTokenStreamEnum::custom(token_stream),
    ft,
  )?);
  iw.add_document(&mut random, doc)?;
  iw.close(&mut random)?;
  Ok(())
}

fn check_tokens(field1: Vec<token::Token>, field2: Option<Vec<token::Token>>) -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let riw = RandomIndexWriter::with_config(&mut random, dir, iwc);

  let mut ft = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  ft.set_index_options(IndexOptions::DocsAndFreqsAndPositionsAndOffsets)?;
  ft.set_store_term_vectors(true)?;
  ft.set_store_term_vector_positions(true)?;
  ft.set_store_term_vector_offsets(true)?;

  let mut doc = Document::new();
  doc.add(Field::from_token_stream(
    "body",
    FieldTokenStreamEnum::custom(CannedTokenStream::new(field1)),
    ft.clone(),
  )?);
  if let Some(field2) = field2 {
    doc.add(Field::from_token_stream(
      "body",
      FieldTokenStreamEnum::custom(CannedTokenStream::new(field2)),
      ft,
    )?);
  }
  riw.add_document(&mut random, doc)?;
  riw.close(&mut random)?;
  Ok(())
}

struct CrazyOffsetGapAnalyzer {
  stored_value: AnalyzerStoredValue,
  seed: u64,
}

impl CrazyOffsetGapAnalyzer {
  fn new(seed: u64) -> Self {
    Self {
      stored_value: AnalyzerStoredValue::per_field(),
      seed,
    }
  }
}

impl Analyzer for CrazyOffsetGapAnalyzer {
  fn create_components(&self, _field_name: &str) -> Result<TokenStreamComponents> {
    let tokenizer = MockTokenizer::with_default_max_token_length(
      random_from_seed(self.seed),
      KEYWORD.clone(),
      false,
    );
    Ok(TokenStreamComponents::new(
      Box::new(tokenizer) as Box<dyn TokenStream + Send + Sync>,
      None,
    ))
  }

  fn stored_value(&self) -> &AnalyzerStoredValue {
    &self.stored_value
  }

  fn get_offset_gap(&self, _field_name: &str) -> i32 {
    -10
  }
}

crate::impl_analyzer_close!(CrazyOffsetGapAnalyzer);

impl From<CrazyOffsetGapAnalyzer> for AnalyzerEnum {
  fn from(analyzer: CrazyOffsetGapAnalyzer) -> Self {
    AnalyzerEnum::Custom(Box::new(analyzer))
  }
}
