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
use crate::core::analysis::analyzer::{Analyzer, AnalyzerStoredValue, TokenStreamComponents};
use crate::core::analysis::reader::ReaderEnum;
use crate::core::analysis::token_filter::{TokenFilter, TokenFilterBase};
use crate::core::analysis::token_stream::TokenStream;
use crate::core::document::binary_doc_values_field::BinaryDocValuesField;
use crate::core::document::document::Document;
use crate::core::document::field::{Field, FieldDataEnum, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::fields::FieldTokenStreamEnum;
use crate::core::document::int_point::IntPoint;
use crate::core::document::invertable_field::InvertableType;
use crate::core::document::knn_float_vector_field::KnnFloatVectorField;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::sorted_doc_values_field::SortedDocValuesField;
use crate::core::document::sorted_numeric_doc_values_field::SortedNumericDocValuesField;
use crate::core::document::sorted_set_doc_values_field::SortedSetDocValuesField;
use crate::core::document::stored_field::StoredField;
use crate::core::document::string_field::StringField;
use crate::core::document::text_field::TextField;
use crate::core::index::BytesRef;
use crate::core::index::directory_reader;
use crate::core::index::fields::Fields as IndexFields;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::{DISABLE_AUTO_FLUSH, OpenMode};
use crate::core::index::indexable_field::IndexableField;
use crate::core::index::indexable_field::{IndexingTokenStream, ReusedIndexingTokenStream};
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::postings_enum::{ALL, PostingsEnum};
use crate::core::index::segment_reader::SegmentReader;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term::Term;
use crate::core::index::term_vectors::TermVectors;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, NO_MORE_DOCS};
use crate::core::store::IO_CONTEXT_DEFAULT;

use crate::core::util::LATEST;
use crate::core::util::accountable::Accountable;
use crate::core::util::attribute_source::{AttributeSource, Attributes};
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind, Result};
use crate::core::util::number::Number;
use crate::test_framework::core::analysis::mock_analyzer::{MockAnalyzer, WHITESPACE};
use crate::test_framework::core::analysis::mock_tokenizer::MockTokenizer;
pub use crate::test_framework::core::document::MockIndexableField;
use crate::test_framework::core::index::doc_helper::{
  DocHelper, FIELD_1_TEXT, FIELD_2_TEXT, FIELD_3_TEXT, KEYWORD_FIELD_KEY, KEYWORD_TEXT,
  NO_NORMS_KEY, NO_NORMS_TEXT, TEXT_FIELD_1_KEY, TEXT_FIELD_2_KEY, TEXT_FIELD_3_KEY,
};
use crate::test_framework::core::util::lucene_test_case::{
  get_only_leaf_reader, new_directory_shared, new_index_writer_config,
  new_index_writer_config_with_analyzer, random,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::prelude::StdRng;
use rand::{RngExt, SeedableRng};
use std::borrow::Cow;
use std::fmt::{Display, Formatter};

#[allow(dead_code)] // for quick
struct TestDocumentWriter;

#[test]
fn test_add_document() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut test_doc = Document::new();
  DocHelper::setup_doc(&mut test_doc);
  let info = DocHelper::write_doc(&mut random, dir, &mut test_doc)?;

  let reader = SegmentReader::new(
    &info,
    LATEST.major,
    IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?,
  )?;
  let mut stored_fields = reader.stored_fields()?;
  let doc = stored_fields.document(0)?;

  let fields = doc.get_fields_with_name(TEXT_FIELD_2_KEY);
  assert_eq!(1, fields.len());
  assert_eq!(FIELD_2_TEXT, fields[0].string_value()?.unwrap().as_ref());
  assert!(fields[0].field_type().store_term_vectors());

  let fields = doc.get_fields_with_name(TEXT_FIELD_1_KEY);
  assert_eq!(1, fields.len());
  assert_eq!(FIELD_1_TEXT, fields[0].string_value()?.unwrap().as_ref());
  assert!(!fields[0].field_type().store_term_vectors());

  let fields = doc.get_fields_with_name(KEYWORD_FIELD_KEY);
  assert_eq!(1, fields.len());
  assert_eq!(KEYWORD_TEXT, fields[0].string_value()?.unwrap().as_ref());

  let fields = doc.get_fields_with_name(NO_NORMS_KEY);
  assert_eq!(1, fields.len());
  assert_eq!(NO_NORMS_TEXT, fields[0].string_value()?.unwrap().as_ref());

  let fields = doc.get_fields_with_name(TEXT_FIELD_3_KEY);
  assert_eq!(1, fields.len());
  assert_eq!(FIELD_3_TEXT, fields[0].string_value()?.unwrap().as_ref());

  for fi in reader.get_field_infos()?.iter() {
    if *fi.get_index_options() != IndexOptions::None {
      assert_eq!(
        fi.omits_norms(),
        reader.get_norm_values(&fi.name)?.is_none()
      );
    }
  }

  Ok(())
}

#[test]
fn test_position_increment_gap() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = PositionIncrementGapAnalyzer {
    stored_value: AnalyzerStoredValue::new(),
    seed: random.random(),
  };
  let writer = IndexWriter::new(
    dir.clone(),
    new_index_writer_config_with_analyzer(&mut random, Box::new(analyzer) as Box<dyn Analyzer>)?,
  )?;

  let mut doc = Document::new();
  doc.add(TextField::from_string(
    "repeated",
    "repeated one",
    Store::Yes,
  )?);
  doc.add(TextField::from_string(
    "repeated",
    "repeated two",
    Store::Yes,
  )?);

  writer.add_document(doc)?;
  writer.commit()?;
  writer.close()?;
  let reader = directory_reader::open(dir)?;
  let leaf = get_only_leaf_reader(&reader)?;

  let mut term_positions = leaf
    .postings_with_flag(&Term::from_text("repeated", "repeated"), ALL as i32)?
    .expect("repeated postings should exist");
  assert_ne!(NO_MORE_DOCS, term_positions.next_doc()?);
  assert_eq!(2, term_positions.freq()?);
  assert_eq!(0, term_positions.next_position()?);
  assert_eq!(502, term_positions.next_position()?);

  Ok(())
}

struct PositionIncrementGapAnalyzer {
  stored_value: AnalyzerStoredValue,
  seed: u64,
}

impl Analyzer for PositionIncrementGapAnalyzer {
  fn create_components(&self, _field_name: &str) -> Result<TokenStreamComponents> {
    let tokenizer = MockTokenizer::with_default_max_token_length(
      StdRng::seed_from_u64(self.seed),
      WHITESPACE.clone(),
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

  fn get_position_increment_gap(&self, _field_name: &str) -> i32 {
    500
  }
}

crate::impl_analyzer_close!(PositionIncrementGapAnalyzer);

#[test]
fn test_token_reuse() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = TokenReuseAnalyzer {
    stored_value: AnalyzerStoredValue::new(),
    seed: random.random(),
  };
  let writer = IndexWriter::new(
    dir.clone(),
    new_index_writer_config_with_analyzer(&mut random, Box::new(analyzer) as Box<dyn Analyzer>)?,
  )?;

  let mut doc = Document::new();
  doc.add(TextField::from_string("f1", "a 5 a a", Store::Yes)?);

  writer.add_document(doc)?;
  writer.commit()?;
  writer.close()?;
  let reader = directory_reader::open(dir)?;
  let leaf = get_only_leaf_reader(&reader)?;

  let mut term_positions = leaf
    .postings_with_flag(&Term::from_text("f1", "a"), ALL as i32)?
    .expect("a postings should exist");
  assert_ne!(NO_MORE_DOCS, term_positions.next_doc()?);
  assert_eq!(3, term_positions.freq()?);
  assert_eq!(0, term_positions.next_position()?);
  assert!(term_positions.get_payload()?.is_some());
  assert_eq!(6, term_positions.next_position()?);
  assert!(term_positions.get_payload()?.is_none());
  assert_eq!(7, term_positions.next_position()?);
  assert!(term_positions.get_payload()?.is_none());

  Ok(())
}

struct TokenReuseAnalyzer {
  stored_value: AnalyzerStoredValue,
  seed: u64,
}

impl Analyzer for TokenReuseAnalyzer {
  fn create_components(&self, _field_name: &str) -> Result<TokenStreamComponents> {
    let tokenizer = MockTokenizer::with_default_max_token_length(
      StdRng::seed_from_u64(self.seed),
      WHITESPACE.clone(),
      false,
    );
    let filter = TokenReuseFilter {
      token_filter_base: TokenFilterBase::new(tokenizer),
      first: true,
      state: false,
    };
    Ok(TokenStreamComponents::new(
      Box::new(filter) as Box<dyn TokenStream + Send + Sync>,
      None,
    ))
  }

  fn stored_value(&self) -> &AnalyzerStoredValue {
    &self.stored_value
  }
}

crate::impl_analyzer_close!(TokenReuseAnalyzer);

struct TokenReuseFilter {
  token_filter_base: TokenFilterBase<MockTokenizer<StdRng>>,
  first: bool,
  state: bool,
}

impl crate::core::util::close::Closeable for TokenReuseFilter {
  fn close(&mut self) -> Result<()> {
    crate::core::util::close::Closeable::close(&mut self.token_filter_base)
  }
}

impl TokenStream for TokenReuseFilter {
  fn increment_token(&mut self) -> Result<bool> {
    if self.state {
      let attrs = self.token_filter_base.input.get_attribute_source_mut();
      attrs.set_payload(None)?;
      attrs.set_position_increment(0)?;
      attrs.set_empty()?.append_str(Some("b"))?;
      self.state = false;
      return Ok(true);
    }

    if !self.token_filter_base.input.increment_token()? {
      return Ok(false);
    }

    let attrs = self.token_filter_base.input.get_attribute_source_mut();
    let position_increment = attrs
      .buffer()?
      .first()
      .and_then(|ch| ch.to_digit(10))
      .map(|value| value as i32);
    if let Some(position_increment) = position_increment {
      attrs.set_position_increment(position_increment)?;
    }
    if self.first {
      attrs.set_payload(Some(BytesRef::from_bytes(vec![100])))?;
      self.first = false;
    }

    self.state = true;
    Ok(true)
  }

  fn end(&mut self) -> Result<()> {
    self.token_filter_base.end()
  }

  fn reset(&mut self) -> Result<()> {
    self.token_filter_base.reset()?;
    self.first = true;
    self.state = false;
    Ok(())
  }

  fn get_attribute_source(&self) -> &Attributes {
    self.token_filter_base.input.get_attribute_source()
  }

  fn get_attribute_source_mut(&mut self) -> &mut Attributes {
    self.token_filter_base.input.get_attribute_source_mut()
  }

  fn set_reader(&mut self, input: ReaderEnum) -> Result<()> {
    self.token_filter_base.input.set_reader(input)
  }

  fn set_reader_test_point(&mut self) -> Result<()> {
    self.token_filter_base.input.set_reader_test_point()
  }
}

impl TokenFilter for TokenReuseFilter {}

#[test]
fn test_pre_analyzed_field() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let writer = IndexWriter::new(
    dir.clone(),
    new_index_writer_config_with_analyzer(&mut random, analyzer)?,
  )?;
  let mut doc = Document::new();

  doc.add(TextField::from_token_stream(
    "preanalyzed",
    FieldTokenStreamEnum::custom(PreAnalyzedTokenStream::new()),
  )?);

  writer.add_document(doc)?;
  writer.commit()?;
  writer.close()?;
  let reader = directory_reader::open(dir)?;
  let leaf = get_only_leaf_reader(&reader)?;

  let mut term_positions = leaf
    .postings_with_flag(&Term::from_text("preanalyzed", "term1"), ALL as i32)?
    .expect("term1 postings should exist");
  assert_ne!(NO_MORE_DOCS, term_positions.next_doc()?);
  assert_eq!(1, term_positions.freq()?);
  assert_eq!(0, term_positions.next_position()?);

  let mut term_positions = leaf
    .postings_with_flag(&Term::from_text("preanalyzed", "term2"), ALL as i32)?
    .expect("term2 postings should exist");
  assert_ne!(NO_MORE_DOCS, term_positions.next_doc()?);
  assert_eq!(2, term_positions.freq()?);
  assert_eq!(1, term_positions.next_position()?);
  assert_eq!(3, term_positions.next_position()?);

  let mut term_positions = leaf
    .postings_with_flag(&Term::from_text("preanalyzed", "term3"), ALL as i32)?
    .expect("term3 postings should exist");
  assert_ne!(NO_MORE_DOCS, term_positions.next_doc()?);
  assert_eq!(1, term_positions.freq()?);
  assert_eq!(2, term_positions.next_position()?);

  Ok(())
}

#[test]
fn test_lucene_1590() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut doc = Document::new();

  let mut custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  custom_type.set_omit_norms(true)?;
  let mut custom_type2 = FieldType::new();
  custom_type2.set_stored(true)?;
  doc.add(Field::new("f1", "v1", custom_type));
  doc.add(Field::new("f1", "v2", custom_type2.clone()));

  let mut custom_type3 = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  custom_type3.set_index_options(IndexOptions::Docs)?;
  let f = Field::new("f2", "v1", custom_type3);
  doc.add(f);
  doc.add(Field::new("f2", "v2", custom_type2));

  let analyzer = MockAnalyzer::new(&mut random);
  let writer = IndexWriter::new(
    dir.clone(),
    new_index_writer_config_with_analyzer(&mut random, analyzer)?,
  )?;
  writer.add_document(doc)?;
  writer.force_merge(1)?;
  writer.close()?;

  TestUtil::check_index(&mut random, dir.clone())?;

  let reader = directory_reader::open(dir)?;
  let leaf = get_only_leaf_reader(&reader)?;
  let fi = leaf.get_field_infos()?;
  let f1 = fi
    .field_info_by_name("f1")?
    .ok_or_else(|| LuceneError::illegal_state("f1 should exist"))?;
  assert!(!f1.has_norms(), "f1 should have no norms");
  assert_eq!(
    &IndexOptions::DocsAndFreqsAndPositions,
    f1.get_index_options(),
    "omitTermFreqAndPositions field bit should not be set for f1"
  );
  let f2 = fi
    .field_info_by_name("f2")?
    .ok_or_else(|| LuceneError::illegal_state("f2 should exist"))?;
  assert!(f2.has_norms(), "f2 should have norms");
  assert_eq!(
    &IndexOptions::Docs,
    f2.get_index_options(),
    "omitTermFreqAndPositions field bit should be set for f2"
  );

  Ok(())
}

fn do_test_ram_usage<F>(field_supplier: F) -> Result<()>
where
  F: Fn(&str) -> Result<crate::core::document::fields::Fields>,
{
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut iwc = new_index_writer_config(&mut random)?;
  iwc.set_max_buffered_docs(10);
  iwc.set_ram_buffer_size_mb(DISABLE_AUTO_FLUSH as f64);
  let writer = IndexWriter::new(dir, iwc)?;

  let mut doc = Document::new();
  let num_fields = 100;
  for i in 0..num_fields {
    doc.add(field_supplier(&format!("f{}", i))?);
  }
  writer.add_document(doc)?;
  assert!(writer.has_changes_in_ram()?);
  assert!(writer.doc_writer.ram_bytes_used()? < num_fields * 16384);
  writer.close()?;
  Ok(())
}

#[test]
fn test_ram_usage_stored() -> Result<()> {
  do_test_ram_usage(|field| {
    Ok(
      StoredField::from_binary(
        field.to_owned(),
        BytesRef::<Vec<u8>>::from_string("Lucene").bytes,
      )?
      .into(),
    )
  })
}

#[test]
fn test_ram_usage_indexed() -> Result<()> {
  do_test_ram_usage(|field| {
    Ok(
      StringField::from_bytes_ref(field.to_owned(), BytesRef::from_string("Lucene"), Store::No)?
        .into(),
    )
  })
}

#[test]
fn test_ram_usage_point() -> Result<()> {
  do_test_ram_usage(|field| Ok(IntPoint::new(field.to_owned(), [42])?.into()))
}

#[test]
fn test_ram_usage_numeric_doc_value() -> Result<()> {
  do_test_ram_usage(|field| Ok(NumericDocValuesField::new(field.to_owned(), 42).into()))
}

#[test]
fn test_ram_usage_sorted_doc_value() -> Result<()> {
  do_test_ram_usage(|field| {
    Ok(SortedDocValuesField::new(field.to_owned(), BytesRef::from_string("Lucene")).into())
  })
}

#[test]
fn test_ram_usage_binary_doc_value() -> Result<()> {
  do_test_ram_usage(|field| {
    Ok(BinaryDocValuesField::new(field.to_owned(), BytesRef::from_string("Lucene")).into())
  })
}

#[test]
fn test_ram_usage_sorted_numeric_doc_value() -> Result<()> {
  do_test_ram_usage(|field| Ok(SortedNumericDocValuesField::new(field.to_owned(), 42).into()))
}

#[test]
fn test_ram_usage_sorted_set_doc_value() -> Result<()> {
  do_test_ram_usage(|field| {
    Ok(SortedSetDocValuesField::new(field.to_owned(), BytesRef::from_string("Lucene")).into())
  })
}

#[test]
fn test_ram_usage_vector() -> Result<()> {
  do_test_ram_usage(|field| {
    Ok(
      KnnFloatVectorField::with_similarity_function(
        field,
        vec![1.0, 2.0, 3.0, 4.0],
        VectorSimilarityFunction::Euclidean,
      )?
      .into(),
    )
  })
}

#[test]
fn test_index_binary_value_without_token_stream() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mut illegal_field_types = Vec::new();
  {
    let mut illegal_ft = FieldType::new();
    illegal_ft.set_tokenized(true)?;
    illegal_ft.set_index_options(IndexOptions::Docs)?;
    illegal_ft.freeze();
    illegal_field_types.push(illegal_ft);
  }
  {
    let mut illegal_ft = FieldType::new();
    illegal_ft.set_tokenized(false)?;
    illegal_ft.set_index_options(IndexOptions::DocsAndFreqsAndPositions)?;
    illegal_ft.freeze();
    illegal_field_types.push(illegal_ft);
  }
  {
    let mut illegal_ft = FieldType::new();
    illegal_ft.set_tokenized(false)?;
    illegal_ft.set_index_options(IndexOptions::Docs)?;
    illegal_ft.set_store_term_vectors(true)?;
    illegal_ft.set_store_term_vector_positions(true)?;
    illegal_ft.freeze();
    illegal_field_types.push(illegal_ft);
  }
  {
    let mut illegal_ft = FieldType::new();
    illegal_ft.set_tokenized(false)?;
    illegal_ft.set_index_options(IndexOptions::Docs)?;
    illegal_ft.set_store_term_vectors(true)?;
    illegal_ft.set_store_term_vector_offsets(true)?;
    illegal_ft.freeze();
    illegal_field_types.push(illegal_ft);
  }

  for ft in illegal_field_types {
    let mut iwc = new_index_writer_config(&mut random)?;
    iwc.set_open_mode(OpenMode::Create);
    let writer = IndexWriter::new(dir.clone(), iwc)?;
    let field = MockIndexableField::new("field", Some(BytesRef::from_string("a")), ft);
    let mut doc = Document::new();
    doc.add(field);
    let res = writer.add_document(doc);
    assert!(
      matches!(
        res.as_ref().map_err(LuceneError::kind),
        Err(LuceneErrorKind::IllegalArgument(_))
      ),
      "expected IllegalArgument but got: {:?}",
      res
    );
    writer.close()?;
  }

  {
    let mut iwc = new_index_writer_config(&mut random)?;
    iwc.set_open_mode(OpenMode::Create);
    let writer = IndexWriter::new(dir.clone(), iwc)?;
    let field = MockIndexableField::new(
      "field",
      None,
      crate::core::document::string_field::TYPE_NOT_STORED.clone(),
    );
    let mut doc = Document::new();
    doc.add(field);
    let res = writer.add_document(doc);
    assert!(
      matches!(
        res.as_ref().map_err(LuceneError::kind),
        Err(LuceneErrorKind::IllegalArgument(_))
      ),
      "expected IllegalArgument but got: {:?}",
      res
    );
    writer.close()?;
  }

  let mut legal_field_types = Vec::with_capacity(6);
  {
    let mut ft = FieldType::new();
    ft.set_tokenized(false)?;
    ft.set_index_options(IndexOptions::Docs)?;
    ft.set_omit_norms(false)?;
    ft.freeze();
    legal_field_types.push(ft);
  }
  {
    let mut ft = FieldType::new();
    ft.set_tokenized(false)?;
    ft.set_index_options(IndexOptions::DocsAndFreqs)?;
    ft.set_omit_norms(false)?;
    ft.freeze();
    legal_field_types.push(ft);
  }
  {
    let mut ft = FieldType::new();
    ft.set_tokenized(false)?;
    ft.set_index_options(IndexOptions::Docs)?;
    ft.set_omit_norms(true)?;
    ft.freeze();
    legal_field_types.push(ft);
  }
  {
    let mut ft = FieldType::new();
    ft.set_tokenized(false)?;
    ft.set_index_options(IndexOptions::DocsAndFreqs)?;
    ft.set_omit_norms(true)?;
    ft.freeze();
    legal_field_types.push(ft);
  }
  {
    let mut ft = FieldType::new();
    ft.set_tokenized(false)?;
    ft.set_index_options(IndexOptions::Docs)?;
    ft.set_store_term_vectors(true)?;
    ft.freeze();
    legal_field_types.push(ft);
  }
  {
    let mut ft = FieldType::new();
    ft.set_tokenized(false)?;
    ft.set_index_options(IndexOptions::DocsAndFreqs)?;
    ft.set_store_term_vectors(true)?;
    ft.freeze();
    legal_field_types.push(ft);
  }

  for ft in legal_field_types {
    {
      let mut iwc = new_index_writer_config(&mut random)?;
      iwc.set_open_mode(OpenMode::Create);
      let writer = IndexWriter::new(dir.clone(), iwc)?;
      let mut doc = Document::new();
      doc.add(MockIndexableField::new(
        "field",
        Some(BytesRef::from_string("a")),
        ft.clone(),
      ));
      doc.add(MockIndexableField::new(
        "field",
        Some(BytesRef::from_string("a")),
        ft.clone(),
      ));
      writer.add_document(doc)?;
      writer.close()?;
    }

    let reader = directory_reader::open(dir.clone())?;
    let leaf_reader = get_only_leaf_reader(&reader)?;

    {
      let terms = leaf_reader
        .terms("field")?
        .ok_or_else(|| LuceneError::illegal_state("field terms should exist"))?;
      assert_eq!(1, terms.get_sum_doc_freq()?);
      if *ft.index_options() >= IndexOptions::DocsAndFreqs {
        assert_eq!(2, terms.get_sum_total_term_freq()?);
      } else {
        assert_eq!(1, terms.get_sum_total_term_freq()?);
      }
      let mut terms_enum = terms.iterator()?;
      assert!(terms_enum.seek_exact(&BytesRef::<Vec<u8>>::from_string("a"))?);
      let mut pe = terms_enum.postings_with_flags(None, ALL as i32)?;
      assert_eq!(0, pe.next_doc()?);
      if *ft.index_options() >= IndexOptions::DocsAndFreqs {
        assert_eq!(2, pe.freq()?);
      } else {
        assert_eq!(1, pe.freq()?);
      }
      assert_eq!(-1, pe.next_position()?);
      assert_eq!(NO_MORE_DOCS, pe.next_doc()?);
    }

    let mut term_vectors = leaf_reader.term_vectors()?;
    if ft.store_term_vectors() {
      let tv_fields = term_vectors
        .get(0)?
        .ok_or_else(|| LuceneError::illegal_state("term vectors should exist"))?;
      let tv_terms = tv_fields
        .terms("field")?
        .ok_or_else(|| LuceneError::illegal_state("field term vectors should exist"))?;
      assert_eq!(1, tv_terms.get_sum_doc_freq()?);
      assert_eq!(2, tv_terms.get_sum_total_term_freq()?);
      let mut tv_terms_enum = tv_terms.iterator()?;
      assert!(tv_terms_enum.seek_exact(&BytesRef::<Vec<u8>>::from_string("a"))?);
      let mut pe = tv_terms_enum.postings_with_flags(None, ALL as i32)?;
      assert_eq!(0, pe.next_doc()?);
      assert_eq!(2, pe.freq()?);
      assert_eq!(-1, pe.next_position()?);
      assert_eq!(NO_MORE_DOCS, pe.next_doc()?);
    } else {
      assert!(term_vectors.get(0)?.is_none());
    }
  }

  Ok(())
}

struct PreAnalyzedTokenStream {
  attrs: Attributes,
  tokens: [&'static str; 4],
  index: usize,
}

impl PreAnalyzedTokenStream {
  fn new() -> Self {
    Self {
      attrs: Attributes::default(),
      tokens: ["term1", "term2", "term3", "term2"],
      index: 0,
    }
  }
}

impl crate::core::util::close::Closeable for PreAnalyzedTokenStream {}

impl TokenStream for PreAnalyzedTokenStream {
  fn increment_token(&mut self) -> Result<bool> {
    if self.index == self.tokens.len() {
      return Ok(false);
    }

    self.attrs.clear_attributes()?;
    self.attrs.append_str(Some(self.tokens[self.index]))?;
    self.index += 1;
    Ok(true)
  }

  fn end(&mut self) -> Result<()> {
    self.default_end()
  }

  fn get_attribute_source(&self) -> &Attributes {
    &self.attrs
  }

  fn get_attribute_source_mut(&mut self) -> &mut Attributes {
    &mut self.attrs
  }
}
