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

use crate::core::analysis::reader::StringReader;
use crate::core::analysis::token_stream::TokenStream;
use crate::core::document::document::Document;
use crate::core::document::field::{Field, FieldBase, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::fields::{FieldTokenStreamEnum, Fields};
use crate::core::document::stored_field::StoredField;
use crate::core::document::string_field::StringField;
use crate::core::document::string_field::TYPE_STORED as STRING_TYPE_STORED;
use crate::core::document::text_field::TextField;
use crate::core::index::BytesRefValue;
use crate::test_framework::core::util::lucene_test_case::{
  new_directory_shared, new_searcher_with_reader, random,
};

use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::indexable_field::IndexableField;
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term::Term;
use crate::core::search::index_searcher::{self, IndexSearcher};
use crate::core::search::phrase_query::PhraseQuery;
use crate::core::search::score_doc::ScoreDocLike;
use crate::core::search::term_query::TermQuery;
use crate::core::search::top_docs::TopDocsLike;
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind, Result};
use crate::test_framework::core::analysis::mock_tokenizer::MockTokenizer;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;

#[allow(dead_code)] // for quick search
struct TestDocument;

/// Tests the [`Document::remove_field`] method for a brand-new [`Document`]
/// that has not been indexed yet.
///
/// # Errors
/// - Returns an error if execution fails.
#[test]
fn test_binary_field() -> Result<()> {
  let binary_val = "this text will be stored as a byte array in the index";
  let binary_val2 = "this text will be also stored as a byte array in the index";

  let mut doc = Document::new();

  let mut ft = FieldType::new();
  ft.set_stored(true)?;
  let ft_arc = ft;

  let string_fld = Field::from_string("string", binary_val, ft_arc.clone())?;
  let binary_fld = StoredField::from_binary("binary", binary_val.as_bytes().to_vec())?;
  let binary_fld2 = StoredField::from_binary("binary", binary_val2.as_bytes().to_vec())?;

  assert!(binary_fld.binary_value()?.is_some());
  assert!(string_fld.field_type().stored());
  assert_eq!(binary_fld.field_type().index_options(), &IndexOptions::None);
  doc.add(binary_fld);
  doc.add(string_fld);

  assert_eq!(doc.get_fields().len(), 2);

  match doc.get_binary_value("binary")? {
    Some(bf) => {
      let bf_value = bf.as_bytes_ref().utf8_to_string()?.to_owned();
      assert_eq!(bf_value, binary_val);
    },
    None => {
      unreachable!()
    },
  }
  match doc.get("string")? {
    Some(sf) => {
      assert_eq!(sf.as_ref(), binary_val);
    },
    None => {
      unreachable!()
    },
  }

  doc.add(binary_fld2);
  assert_eq!(doc.get_fields().len(), 3);

  let binary_tests = doc.get_binary_values("binary")?;
  assert_eq!(binary_tests.len(), 2);

  let binary_test = binary_tests[0].as_bytes_ref().utf8_to_string()?.to_owned();
  let binary_test2 = binary_tests[1].as_bytes_ref().utf8_to_string()?.to_owned();

  assert_ne!(binary_test, binary_test2);
  assert_eq!(binary_test, binary_val);
  assert_eq!(binary_test2, binary_val2);
  doc.remove_field("string");
  assert_eq!(doc.get_fields().len(), 2);
  doc.remove_fields("binary");
  assert_eq!(doc.get_fields().len(), 0);
  Ok(())
}
/// Tests the [`Document::remove_field`] method for a brand-new [`Document`]
/// that has not been indexed yet.
///
/// # Errors
/// - Returns an error if execution fails.
#[test]
fn test_remove_for_new_document() -> Result<()> {
  let mut doc = make_document_with_fields()?;
  assert_eq!(10, doc.get_fields().len());

  doc.remove_fields("keyword");
  assert_eq!(8, doc.get_fields().len());

  doc.remove_fields("doesnotexists"); // removing non-existing fields is
  doc.remove_fields("keyword"); // removing a field more than once
  assert_eq!(8, doc.get_fields().len());

  doc.remove_field("text");
  assert_eq!(7, doc.get_fields().len());

  doc.remove_field("text");
  assert_eq!(6, doc.get_fields().len());

  doc.remove_field("text");
  assert_eq!(6, doc.get_fields().len());

  doc.remove_field("doesnotexists"); // removing non-existing fields is
  assert_eq!(6, doc.get_fields().len());

  doc.remove_fields("unindexed");
  assert_eq!(4, doc.get_fields().len());

  doc.remove_fields("unstored");
  assert_eq!(2, doc.get_fields().len());

  doc.remove_fields("doesnotexists"); // removing non-existing fields is
  assert_eq!(2, doc.get_fields().len());

  doc.remove_fields("indexed_not_tokenized");
  assert_eq!(0, doc.get_fields().len());

  Ok(())
}
#[test]
fn test_constructor_exceptions() -> Result<()> {
  let mut ft = FieldType::new();
  ft.set_stored(true)?;
  Field::from_string("name", "value", ft.clone())?;

  StringField::from_string("name", "value", Store::No)?;

  let ft_invalid = FieldType::new();
  let result = Field::from_string("name", "value", ft_invalid);
  assert!(matches!(
    result.as_ref().map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));

  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = RandomIndexWriter::new(&mut random, dir.clone())?;

  Field::from_string("name", "value", ft.clone())?;

  let mut doc = Document::new();
  let mut ft2 = FieldType::new();
  ft2.set_stored(true)?;
  ft2.set_store_term_vectors(true)?;
  doc.add(Field::from_string("name", "value", ft2)?);

  let result = writer.add_document(&mut random, doc);
  assert!(matches!(
    result.as_ref().map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));

  writer.close(&mut random)?;
  Ok(())
}

#[test]
fn test_clear_document() -> Result<()> {
  let mut doc = make_document_with_fields()?;
  assert_eq!(doc.get_fields().len(), 10);
  doc.clear();
  assert_eq!(doc.get_fields().len(), 0);
  Ok(())
}

#[test]
#[ignore = "Java-only: Document::get_fields returns an immutable Rust slice by type"]
fn test_get_fields_immutable() -> Result<()> {
  test_not_required_in_rust_lucene!();
}

#[test]
fn test_get_values_for_new_document() -> Result<()> {
  do_assert(&make_document_with_fields()?, false)
}
#[test]
fn test_get_values_for_indexed_document() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let writer = RandomIndexWriter::new(&mut random, dir.clone())?;
  writer.add_document(&mut random, make_document_with_fields()?)?;

  let reader = writer.get_reader(&mut random)?;
  let searcher = index_searcher::from_reader(reader)?;

  // search for something that does exist
  let query = TermQuery::new(Term::from_text("keyword", "test1"));

  // ensure that queries return expected results without DateFilter first
  let top_docs = searcher.search(query, 1000)?;
  let hits = top_docs.score_docs();
  assert_eq!(hits.len(), 1);

  let doc = searcher.stored_fields()?.document(hits[0].doc())?;

  do_assert(&doc, true)?;

  writer.close(&mut random)?;

  Ok(())
}

#[test]
fn test_get_values() -> Result<()> {
  let doc = make_document_with_fields()?;

  let keyword_values = doc.get_values("keyword")?;
  assert!(
    keyword_values
      .iter()
      .map(|s| s.as_ref())
      .eq(["test1", "test2"]),
    "unexpected keyword values: {keyword_values:?}"
  );

  let text_values = doc.get_values("text")?;
  assert!(
    text_values
      .iter()
      .map(|s| s.as_ref())
      .eq(["test1", "test2"]),
    "unexpected text values: {text_values:?}"
  );

  let unindexed_values = doc.get_values("unindexed")?;
  assert!(
    unindexed_values
      .iter()
      .map(|s| s.as_ref())
      .eq(["test1", "test2"]),
    "unexpected unindexed values: {unindexed_values:?}"
  );

  let nope_values = doc.get_values("nope")?;
  assert!(nope_values.is_empty());

  Ok(())
}
#[test]
fn test_position_increment_multi_fields() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let writer = RandomIndexWriter::new(&mut random, dir.clone())?;
  writer.add_document(&mut random, make_document_with_fields()?)?;

  let reader = writer.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;

  let query = PhraseQuery::from_terms(0, "indexed_not_tokenized", ["test1", "test2"])?;

  let top_docs = searcher.search(query, 1000)?;
  let hits = top_docs.score_docs();
  assert_eq!(1, hits.len());

  let doc = searcher.stored_fields()?.document(hits[0].doc)?;
  do_assert(&doc, true)?;

  writer.close(&mut random)?;
  Ok(())
}

fn make_document_with_fields() -> Result<Document> {
  let mut doc = Document::new();
  let mut stored = FieldType::new();
  stored.set_stored(true)?;
  let mut indexed_not_tokenized = FieldType::new();
  indexed_not_tokenized.set_index_options(IndexOptions::DocsAndFreqsAndPositions)?;
  indexed_not_tokenized.set_tokenized(false)?;
  doc.add(StringField::from_string("keyword", "test1", Store::Yes)?);
  doc.add(StringField::from_string("keyword", "test2", Store::Yes)?);
  doc.add(TextField::from_string("text", "test1", Store::Yes)?);
  doc.add(TextField::from_string("text", "test2", Store::Yes)?);
  doc.add(Field::from_string("unindexed", "test1", stored.clone())?);
  doc.add(Field::from_string("unindexed", "test2", stored.clone())?);
  doc.add(TextField::from_string("unstored", "test1", Store::No)?);
  doc.add(TextField::from_string("unstored", "test2", Store::No)?);
  doc.add(Field::from_string(
    "indexed_not_tokenized",
    "test1",
    indexed_not_tokenized.clone(),
  )?);
  doc.add(Field::from_string(
    "indexed_not_tokenized",
    "test2",
    indexed_not_tokenized.clone(),
  )?);
  Ok(doc)
}

fn do_assert(doc: &Document, from_index: bool) -> Result<()> {
  let keyword_field_values = doc.get_fields_with_name("keyword");
  let text_field_values = doc.get_fields_with_name("text");
  let unindexed_field_values = doc.get_fields_with_name("unindexed");
  let unstored_field_values = doc.get_fields_with_name("unstored");

  assert_eq!(keyword_field_values.len(), 2);
  assert_eq!(text_field_values.len(), 2);
  assert_eq!(unindexed_field_values.len(), 2);
  // this test cannot work for documents retrieved from the index
  // since unstored fields will obviously not be returned
  if !from_index {
    assert_eq!(unstored_field_values.len(), 2);
  }

  assert_eq!(
    keyword_field_values[0].string_value()?.unwrap().as_ref(),
    "test1"
  );
  assert_eq!(
    keyword_field_values[1].string_value()?.unwrap().as_ref(),
    "test2"
  );
  assert_eq!(
    text_field_values[0].string_value()?.unwrap().as_ref(),
    "test1"
  );
  assert_eq!(
    text_field_values[1].string_value()?.unwrap().as_ref(),
    "test2"
  );
  assert_eq!(
    unindexed_field_values[0].string_value()?.unwrap().as_ref(),
    "test1"
  );
  assert_eq!(
    unindexed_field_values[1].string_value()?.unwrap().as_ref(),
    "test2"
  );
  // this test cannot work for documents retrieved from the index
  // since unstored fields will obviously not be returned
  if !from_index {
    assert_eq!(
      unstored_field_values[0].string_value()?.unwrap().as_ref(),
      "test1"
    );
    assert_eq!(
      unstored_field_values[1].string_value()?.unwrap().as_ref(),
      "test2"
    );
  }

  Ok(())
}
#[test]
fn test_field_set_value() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "id1", Store::Yes)?);
  doc.add(StringField::from_string("keyword", "test", Store::Yes)?);

  let writer = RandomIndexWriter::new(&mut random, dir.clone())?;
  writer.add_document(&mut random, &mut doc)?;

  match doc.get_field_mut("id") {
    Some(Fields::String(field)) => field.set_string_value("id2")?,
    _ => panic!("expected string field"),
  }
  writer.add_document(&mut random, &mut doc)?;

  match doc.get_field_mut("id") {
    Some(Fields::String(field)) => field.set_string_value("id3")?,
    _ => panic!("expected string field"),
  }
  writer.add_document(&mut random, &mut doc)?;

  let reader = writer.get_reader(&mut random)?;
  let searcher = index_searcher::from_reader(reader)?;

  let query = TermQuery::new(Term::from_text("keyword", "test"));
  // ensure that queries return expected results without DateFilter first
  let top_docs = searcher.search(query, 1000)?;
  let hits = top_docs.score_docs();
  assert_eq!(hits.len(), 3);

  let mut stored_fields = searcher.stored_fields()?;

  let mut result = 0;
  #[allow(clippy::needless_range_loop)]
  for i in 0..3 {
    let doc2 = stored_fields.document(hits[i].doc())?;

    let f = doc2
      .get_field("id")
      .ok_or_else(|| LuceneError::illegal_state("missing id field"))?;

    let string_value = f.string_value()?.unwrap();
    let val = string_value.as_ref();

    match val {
      "id1" => result |= 1,
      "id2" => result |= 2,
      "id3" => result |= 4,
      _ => return Err(LuceneError::illegal_state("unexpected id field")),
    }
  }

  writer.close(&mut random)?;

  assert_eq!(7, result, "did not see all IDs");
  Ok(())
}

#[test]
fn test_invalid_fields() -> Result<()> {
  let random = random();
  let mut tokenizer = MockTokenizer::new(random);
  tokenizer.set_reader(StringReader::new("").into())?;
  assert!(matches!(
    (Field::from_token_stream(
      "foo",
      FieldTokenStreamEnum::custom(tokenizer),
      STRING_TYPE_STORED.clone(),
    ))
    .as_ref()
    .map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  Ok(())
}

#[test]
fn test_numeric_field_as_string() -> Result<()> {
  // build document
  let mut doc = Document::new();
  doc.add(StoredField::from_i32("int", 5)?);
  assert_eq!("5", doc.get("int")?.unwrap().as_ref());
  assert_eq!(None, doc.get("somethingElse")?);

  doc.add(StoredField::from_i32("int", 4)?);

  let values = doc.get_values("int")?;
  assert!(values.iter().map(|v| v.as_ref()).eq(["5", "4"]));

  // index it
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let iw = RandomIndexWriter::new(&mut random, dir.clone())?;
  iw.add_document(&mut random, doc)?;

  let ir = iw.get_reader(&mut random)?;
  let sdoc = ir.stored_fields()?.document(0)?;

  assert_eq!("5", sdoc.get("int")?.unwrap().as_ref());
  assert_eq!(None, sdoc.get("somethingElse")?);

  let svalues = sdoc.get_values("int")?;
  assert!(svalues.iter().map(|v| v.as_ref()).eq(["5", "4"]));

  iw.close(&mut random)?;
  Ok(())
}
