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
use crate::core::analysis::token_stream::TokenStream;
use crate::core::document::document::Document;
use crate::core::document::field::Field;
use crate::core::document::field_type::FieldType;
use crate::core::document::fields::FieldTokenStreamEnum;
use crate::core::index::field_invert_state::FieldInvertState;
use crate::core::index::fields::Fields;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::postings_enum::{FREQS, PostingsEnum};
use crate::core::index::term_vectors::TermVectors;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::index::{BytesRef, directory_reader, multi_terms};
use crate::core::search::collection_statistics::CollectionStatistics;
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, NO_MORE_DOCS};
use crate::core::search::similarities_impl::similarities::{
  BoxSimScorer, Similarity, SimilarityEnum,
};
use crate::core::search::term_statistics::TermStatistics;
use crate::core::util::attribute_source::{AttributeSource, Attributes};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::util::lucene_test_case::{
  new_directory_shared, new_index_writer_config_with_analyzer, random,
};
use parking_lot::Mutex;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

#[allow(dead_code)] // for quick search
pub struct TestCustomTermFreq;

struct CannedTermFreqs {
  terms: Vec<String>,
  term_freqs: Vec<i32>,
  attrs: Attributes,
  upto: usize,
}
impl CannedTermFreqs {
  fn new(terms: Vec<String>, term_freqs: Vec<i32>) -> Self {
    Self {
      terms,
      term_freqs,
      attrs: Attributes::default(),
      upto: 0,
    }
  }
}

impl crate::core::util::close::Closeable for CannedTermFreqs {}

impl TokenStream for CannedTermFreqs {
  fn increment_token(&mut self) -> Result<bool> {
    if self.upto == self.terms.len() {
      return Ok(false);
    }

    self.attrs.clear_attributes()?;

    self.attrs.append_str(Some(&self.terms[self.upto]))?;

    self.attrs.set_term_frequency(self.term_freqs[self.upto])?;

    self.upto += 1;

    Ok(true)
  }

  fn end(&mut self) -> Result<()> {
    self.attrs.end_attributes();
    Ok(())
  }

  fn reset(&mut self) -> Result<()> {
    self.upto = 0;
    Ok(())
  }

  fn get_attribute_source(&self) -> &Attributes {
    &self.attrs
  }

  fn get_attribute_source_mut(&mut self) -> &mut Attributes {
    &mut self.attrs
  }
}
#[test]
fn test_singleton_terms_one_doc() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();

  let mut field_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  field_type.set_index_options(IndexOptions::DocsAndFreqs)?;

  let field = Field::from_token_stream(
    "field",
    FieldTokenStreamEnum::custom(CannedTermFreqs::new(
      vec!["foo".to_string(), "bar".to_string()],
      vec![42, 128],
    )),
    field_type,
  )?;
  doc.add(field);

  w.add_document(doc)?;

  let r = directory_reader::open_from_writer(&w)?;

  let mut postings = multi_terms::get_term_postings_enum_with_flag(
    &r,
    "field",
    &BytesRef::from_string("bar"),
    FREQS as i32,
  )?
  .unwrap();

  assert_eq!(0, postings.next_doc()?);
  assert_eq!(128, postings.freq()?);
  assert_eq!(NO_MORE_DOCS, postings.next_doc()?);

  let mut postings = multi_terms::get_term_postings_enum_with_flag(
    &r,
    "field",
    &BytesRef::from_string("foo"),
    FREQS as i32,
  )?
  .unwrap();

  assert_eq!(0, postings.next_doc()?);
  assert_eq!(42, postings.freq()?);
  assert_eq!(NO_MORE_DOCS, postings.next_doc()?);

  Ok(())
}

#[test]
fn test_singleton_terms_two_docs() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();

  let mut field_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  field_type.set_index_options(IndexOptions::DocsAndFreqs)?;

  let field = Field::from_token_stream(
    "field",
    FieldTokenStreamEnum::custom(CannedTermFreqs::new(
      vec!["foo".to_string(), "bar".to_string()],
      vec![42, 128],
    )),
    field_type.clone(),
  )?;
  doc.add(field);
  w.add_document(doc)?;

  let mut doc = Document::new();
  let field = Field::from_token_stream(
    "field",
    FieldTokenStreamEnum::custom(CannedTermFreqs::new(
      vec!["foo".to_string(), "bar".to_string()],
      vec![50, 50],
    )),
    field_type,
  )?;
  doc.add(field);
  w.add_document(doc)?;

  let r = directory_reader::open_from_writer(&w)?;

  let mut postings = multi_terms::get_term_postings_enum_with_flag(
    &r,
    "field",
    &BytesRef::from_string("bar"),
    FREQS as i32,
  )?
  .unwrap();

  assert_eq!(0, postings.next_doc()?);
  assert_eq!(128, postings.freq()?);
  assert_eq!(1, postings.next_doc()?);
  assert_eq!(50, postings.freq()?);
  assert_eq!(NO_MORE_DOCS, postings.next_doc()?);

  let mut postings = multi_terms::get_term_postings_enum_with_flag(
    &r,
    "field",
    &BytesRef::from_string("foo"),
    FREQS as i32,
  )?
  .unwrap();

  assert_eq!(0, postings.next_doc()?);
  assert_eq!(42, postings.freq()?);
  assert_eq!(1, postings.next_doc()?);
  assert_eq!(50, postings.freq()?);
  assert_eq!(NO_MORE_DOCS, postings.next_doc()?);

  Ok(())
}

#[test]
fn test_repeat_terms_one_doc() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();

  let mut field_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  field_type.set_index_options(IndexOptions::DocsAndFreqs)?;

  let field = Field::from_token_stream(
    "field",
    FieldTokenStreamEnum::custom(CannedTermFreqs::new(
      vec![
        "foo".to_string(),
        "bar".to_string(),
        "foo".to_string(),
        "bar".to_string(),
      ],
      vec![42, 128, 17, 100],
    )),
    field_type,
  )?;
  doc.add(field);
  w.add_document(doc)?;

  let r = directory_reader::open_from_writer(&w)?;

  let mut postings = multi_terms::get_term_postings_enum_with_flag(
    &r,
    "field",
    &BytesRef::from_string("bar"),
    FREQS as i32,
  )?
  .unwrap();

  assert_eq!(0, postings.next_doc()?);
  assert_eq!(228, postings.freq()?);
  assert_eq!(NO_MORE_DOCS, postings.next_doc()?);

  let mut postings = multi_terms::get_term_postings_enum_with_flag(
    &r,
    "field",
    &BytesRef::from_string("foo"),
    FREQS as i32,
  )?
  .unwrap();

  assert_eq!(0, postings.next_doc()?);
  assert_eq!(59, postings.freq()?);
  assert_eq!(NO_MORE_DOCS, postings.next_doc()?);
  w.close()?;
  Ok(())
}

#[test]
fn test_repeat_terms_two_docs() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();

  let mut field_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  field_type.set_index_options(IndexOptions::DocsAndFreqs)?;

  let field = Field::from_token_stream(
    "field",
    FieldTokenStreamEnum::custom(CannedTermFreqs::new(
      vec![
        "foo".to_string(),
        "bar".to_string(),
        "foo".to_string(),
        "bar".to_string(),
      ],
      vec![42, 128, 17, 100],
    )),
    field_type.clone(),
  )?;
  doc.add(field);
  w.add_document(doc)?;

  let mut doc = Document::new();
  field_type.set_index_options(IndexOptions::DocsAndFreqs)?;
  let field = Field::from_token_stream(
    "field",
    FieldTokenStreamEnum::custom(CannedTermFreqs::new(
      vec![
        "foo".to_string(),
        "bar".to_string(),
        "foo".to_string(),
        "bar".to_string(),
      ],
      vec![50, 60, 70, 80],
    )),
    field_type,
  )?;
  doc.add(field);
  w.add_document(doc)?;

  let r = directory_reader::open_from_writer(&w)?;

  let mut postings = multi_terms::get_term_postings_enum_with_flag(
    &r,
    "field",
    &BytesRef::from_string("bar"),
    FREQS as i32,
  )?
  .unwrap();

  assert_eq!(0, postings.next_doc()?);
  assert_eq!(228, postings.freq()?);
  assert_eq!(1, postings.next_doc()?);
  assert_eq!(140, postings.freq()?);
  assert_eq!(NO_MORE_DOCS, postings.next_doc()?);

  let mut postings = multi_terms::get_term_postings_enum_with_flag(
    &r,
    "field",
    &BytesRef::from_string("foo"),
    FREQS as i32,
  )?
  .unwrap();

  assert_eq!(0, postings.next_doc()?);
  assert_eq!(59, postings.freq()?);
  assert_eq!(1, postings.next_doc()?);
  assert_eq!(120, postings.freq()?);
  assert_eq!(NO_MORE_DOCS, postings.next_doc()?);

  Ok(())
}

#[test]
fn test_total_term_freq() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();

  let mut field_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  field_type.set_index_options(IndexOptions::DocsAndFreqs)?;

  let field = Field::from_token_stream(
    "field",
    FieldTokenStreamEnum::custom(CannedTermFreqs::new(
      vec![
        "foo".to_string(),
        "bar".to_string(),
        "foo".to_string(),
        "bar".to_string(),
      ],
      vec![42, 128, 17, 100],
    )),
    field_type.clone(),
  )?;
  doc.add(field);
  w.add_document(doc)?;

  let mut doc = Document::new();
  field_type.set_index_options(IndexOptions::DocsAndFreqs)?;
  let field = Field::from_token_stream(
    "field",
    FieldTokenStreamEnum::custom(CannedTermFreqs::new(
      vec![
        "foo".to_string(),
        "bar".to_string(),
        "foo".to_string(),
        "bar".to_string(),
      ],
      vec![50, 60, 70, 80],
    )),
    field_type,
  )?;
  doc.add(field);
  w.add_document(doc)?;

  let r = directory_reader::open_from_writer(&w)?;

  let mut terms_enum = multi_terms::get_terms(&r, "field")?.unwrap().iterator()?;
  assert!(terms_enum.seek_exact(&BytesRef::from_string("foo"))?);
  assert_eq!(179, terms_enum.total_term_freq()?);
  assert!(terms_enum.seek_exact(&BytesRef::from_string("bar"))?);
  assert_eq!(368, terms_enum.total_term_freq()?);

  w.close()?;

  Ok(())
}

#[test]
fn test_invalid_prox() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();

  let field_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  let field = Field::from_token_stream(
    "field",
    FieldTokenStreamEnum::custom(CannedTermFreqs::new(
      vec![
        "foo".to_string(),
        "bar".to_string(),
        "foo".to_string(),
        "bar".to_string(),
      ],
      vec![42, 128, 17, 100],
    )),
    field_type,
  )?;
  doc.add(field);

  let err = w.add_document(doc).unwrap_err();
  assert!(
    err.to_string().contains(
      "field \"field\": cannot index positions while using custom TermFrequencyAttribute"
    )
  );
  w.close()?;

  Ok(())
}

#[test]
fn test_invalid_docs_only() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();

  let mut field_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  field_type.set_index_options(IndexOptions::Docs)?;
  let field = Field::from_token_stream(
    "field",
    FieldTokenStreamEnum::custom(CannedTermFreqs::new(
      vec![
        "foo".to_string(),
        "bar".to_string(),
        "foo".to_string(),
        "bar".to_string(),
      ],
      vec![42, 128, 17, 100],
    )),
    field_type,
  )?;
  doc.add(field);

  let err = w.add_document(doc).unwrap_err();
  assert!(
    err
      .to_string()
      .contains("field \"field\": must index term freq while using custom TermFrequencyAttribute")
  );

  w.close()?;

  Ok(())
}
#[test]
fn test_overflow_int() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut field_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  field_type.set_index_options(IndexOptions::Docs)?;

  let mut doc = Document::new();
  doc.add(Field::new(
    "field",
    "this field should be indexed",
    field_type.clone(),
  ));
  w.add_document(doc)?;

  let mut doc2 = Document::new();
  let field = Field::from_token_stream(
    "field",
    FieldTokenStreamEnum::custom(CannedTermFreqs::new(
      vec!["foo".to_string(), "bar".to_string()],
      vec![3, i32::MAX],
    )),
    field_type,
  )?;
  doc2.add(field);

  let err = w.add_document(doc2);
  assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));

  let r = directory_reader::open_from_writer(&w)?;
  assert_eq!(1, r.num_docs()?);

  w.close()?;

  Ok(())
}
#[test]
fn test_invalid_term_vector_positions() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();

  let mut field_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  field_type.set_index_options(IndexOptions::DocsAndFreqs)?;
  field_type.set_store_term_vectors(true)?;
  field_type.set_store_term_vector_positions(true)?;

  let field = Field::from_token_stream(
    "field",
    FieldTokenStreamEnum::custom(CannedTermFreqs::new(
      vec![
        "foo".to_string(),
        "bar".to_string(),
        "foo".to_string(),
        "bar".to_string(),
      ],
      vec![42, 128, 17, 100],
    )),
    field_type,
  )?;
  doc.add(field);

  let err = w.add_document(doc);
  assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
  assert_eq!(
    "field \"field\": cannot index term vector positions while using custom TermFrequencyAttribute",
    err.unwrap_err().to_string()
  );

  w.close()?;

  Ok(())
}
#[test]
fn test_invalid_term_vector_offsets() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();

  let mut field_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  field_type.set_index_options(IndexOptions::DocsAndFreqs)?;
  field_type.set_store_term_vectors(true)?;
  field_type.set_store_term_vector_offsets(true)?;

  let field = Field::from_token_stream(
    "field",
    FieldTokenStreamEnum::custom(CannedTermFreqs::new(
      vec![
        "foo".to_string(),
        "bar".to_string(),
        "foo".to_string(),
        "bar".to_string(),
      ],
      vec![42, 128, 17, 100],
    )),
    field_type,
  )?;
  doc.add(field);

  let err = w.add_document(doc);
  assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
  assert_eq!(
    "field \"field\": cannot index term vector offsets while using custom TermFrequencyAttribute",
    err.unwrap_err().to_string()
  );

  w.close()?;

  Ok(())
}

#[test]
fn test_term_vectors() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();

  let mut field_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  field_type.set_index_options(IndexOptions::DocsAndFreqs)?;
  field_type.set_store_term_vectors(true)?;

  let field = Field::from_token_stream(
    "field",
    FieldTokenStreamEnum::custom(CannedTermFreqs::new(
      vec![
        "foo".to_string(),
        "bar".to_string(),
        "foo".to_string(),
        "bar".to_string(),
      ],
      vec![42, 128, 17, 100],
    )),
    field_type.clone(),
  )?;
  doc.add(field);
  w.add_document(doc)?;

  let mut doc = Document::new();
  field_type.set_index_options(IndexOptions::DocsAndFreqs)?;
  let field = Field::from_token_stream(
    "field",
    FieldTokenStreamEnum::custom(CannedTermFreqs::new(
      vec![
        "foo".to_string(),
        "bar".to_string(),
        "foo".to_string(),
        "bar".to_string(),
      ],
      vec![50, 60, 70, 80],
    )),
    field_type,
  )?;
  doc.add(field);
  w.add_document(doc)?;

  let r = directory_reader::open_from_writer(&w)?;

  let mut term_vectors = r.term_vectors()?;

  let fields = term_vectors.get(0)?.unwrap();
  let mut terms_enum = fields.terms("field")?.unwrap().iterator()?;
  assert!(terms_enum.seek_exact(&BytesRef::from_string("bar"))?);
  assert_eq!(228, terms_enum.total_term_freq()?);
  let mut postings = terms_enum.postings(None)?;
  assert_eq!(0, postings.next_doc()?);
  assert_eq!(228, postings.freq()?);
  assert_eq!(NO_MORE_DOCS, postings.next_doc()?);

  assert!(terms_enum.seek_exact(&BytesRef::from_string("foo"))?);
  assert_eq!(59, terms_enum.total_term_freq()?);
  let mut postings = terms_enum.postings(None)?;
  assert_eq!(0, postings.next_doc()?);
  assert_eq!(59, postings.freq()?);
  assert_eq!(NO_MORE_DOCS, postings.next_doc()?);

  let fields = term_vectors.get(1)?.unwrap();
  let mut terms_enum = fields.terms("field")?.unwrap().iterator()?;
  assert!(terms_enum.seek_exact(&BytesRef::from_string("bar"))?);
  assert_eq!(140, terms_enum.total_term_freq()?);
  let mut postings = terms_enum.postings(None)?;
  assert_eq!(0, postings.next_doc()?);
  assert_eq!(140, postings.freq()?);
  assert_eq!(NO_MORE_DOCS, postings.next_doc()?);

  assert!(terms_enum.seek_exact(&BytesRef::from_string("foo"))?);
  assert_eq!(120, terms_enum.total_term_freq()?);
  let mut postings = terms_enum.postings(None)?;
  assert_eq!(0, postings.next_doc()?);
  assert_eq!(120, postings.freq()?);
  assert_eq!(NO_MORE_DOCS, postings.next_doc()?);

  w.close()?;

  Ok(())
}
struct NeverForgetsSimilarity {
  last_state: Arc<Mutex<(FieldInvertState, i32)>>,
}
impl NeverForgetsSimilarity {
  pub fn new(last_state: Arc<Mutex<(FieldInvertState, i32)>>) -> Self {
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
    self.last_state.lock().0.clone_from(state);
    self.last_state.lock().1 = state.num_overlap;
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
fn test_field_invert_state() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let fis = Arc::new(Mutex::new((FieldInvertState::default(), 0)));
  let similarity = NeverForgetsSimilarity::new(fis.clone());
  iwc.set_similarity(SimilarityEnum::custom(similarity));

  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();

  let mut field_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  field_type.set_index_options(IndexOptions::DocsAndFreqs)?;

  let field = Field::from_token_stream(
    "field",
    FieldTokenStreamEnum::custom(CannedTermFreqs::new(
      vec![
        "foo".to_string(),
        "bar".to_string(),
        "foo".to_string(),
        "bar".to_string(),
      ],
      vec![42, 128, 17, 100],
    )),
    field_type,
  )?;
  doc.add(field);

  w.add_document(doc)?;
  let fis = fis.lock();
  assert_eq!(228, fis.0.get_max_term_frequency());
  assert_eq!(2, fis.0.get_unique_term_count());
  assert_eq!(0, fis.1);
  assert_eq!(287, fis.0.get_length());

  w.close()?;

  Ok(())
}
