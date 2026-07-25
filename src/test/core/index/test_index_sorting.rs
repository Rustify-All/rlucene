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
use crate::core::analysis::token_stream::TokenStream;
use crate::core::document::binary_doc_values_field::BinaryDocValuesField;
use crate::core::document::binary_point::BinaryPoint;
use crate::core::document::document::Document;
use crate::core::document::double_doc_values_field::DoubleDocValuesField;
use crate::core::document::field::{Field, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::fields::{FieldTokenStreamEnum, Fields};
use crate::core::document::float_doc_values_field::FloatDocValuesField;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::sorted_doc_values_field::SortedDocValuesField;
use crate::core::document::sorted_numeric_doc_values_field::SortedNumericDocValuesField;
use crate::core::document::sorted_set_doc_values_field::SortedSetDocValuesField;
use crate::core::document::stored_field::StoredField;
use crate::core::document::string_field::StringField;
use crate::core::document::text_field::{TYPE_NOT_STORED, TextField};
use crate::core::index::BytesRef;
use crate::core::index::binary_doc_values::BinaryDocValues;
use crate::core::index::directory_reader;
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::field_invert_state::FieldInvertState;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_options::IndexOptions::DocsAndFreqs;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::{IndexWriter, SOURCE, SOURCE_FLUSH, SOURCE_MERGE};
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::indexable_field::IndexableField;
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::merge_policy::MergePolicyEnum;
use crate::core::index::multi_doc_values::MultiDocValues;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::postings_enum::{ALL, PostingsEnum};
use crate::core::index::sorted_doc_values::SortedDocValues;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term::Term;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::search::collection_statistics::CollectionStatistics;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::index_searcher::get_default_similarity;
use crate::core::search::match_all_docs_query::MatchAllDocsQuery;
use crate::core::search::similarities_impl::similarities::{
  BoxSimScorer, Similarity, SimilarityEnum,
};
use crate::core::search::sort::Sort;
use crate::core::search::sort_field::MissingValueEnum::{StringFirst, StringLast};
use crate::core::search::sort_field::{SortField, SortFieldType, SortFiledBase};
use crate::core::search::sort_field_enum::SortFieldEnum;
use crate::core::search::sorted_numeric_sort_field::SortedNumericSortField;
use crate::core::search::sorted_set_sort_field::SortedSetSortField;
use crate::core::search::term_query::TermQuery;
use crate::core::search::term_statistics::TermStatistics;
use crate::core::search::top_docs::TopDocsLike;
use crate::core::search::top_field_collector_manager::TopFieldCollectorManager;
use crate::core::store::directory::DirEnum;
use crate::core::util::attribute_source::{AttributeSource, Attributes};
use crate::core::util::bit_set::BitSet;
use crate::core::util::bits::Bits;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::numeric_utils::NumericUtils;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::analysis::mock_tokenizer::MockTokenizer;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, at_least_usize, create_temp_dir, get_only_leaf_reader, new_bytes_ref_from_string,
  new_directory_shared, new_fs_directory, new_index_writer_config,
  new_index_writer_config_with_analyzer, new_log_merge_policy, new_searcher_with_reader,
  new_text_field, random, random_from_seed, rarely,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::prelude::{SliceRandom, StdRng};
use rand::{Rng, RngExt, SeedableRng};
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;

#[allow(dead_code)] // for quick search
pub struct TestIndexSorting;

#[test]
fn test_numeric_already_sorted() -> Result<()> {
  // TODO: AssertingNeedsIndexSortCodec requires a custom PointsFormat merge hook, while Rust
  // Codecs is still the concrete Lucene101Codec.
  Ok(())
}

#[test]
fn test_string_already_sorted() -> Result<()> {
  // TODO: AssertingNeedsIndexSortCodec requires a custom PointsFormat merge hook, while Rust
  // Codecs is still the concrete Lucene101Codec.
  Ok(())
}

#[test]
fn test_multi_valued_numeric_already_sorted() -> Result<()> {
  // TODO: AssertingNeedsIndexSortCodec requires a custom PointsFormat merge hook, while Rust
  // Codecs is still the concrete Lucene101Codec.
  Ok(())
}

#[test]
fn test_multi_valued_string_already_sorted() -> Result<()> {
  // TODO: AssertingNeedsIndexSortCodec requires a custom PointsFormat merge hook, while Rust
  // Codecs is still the concrete Lucene101Codec.
  Ok(())
}

#[test]
fn test_basic_string() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let index_sort = Sort::with_fields(vec![SortField::new(Some("foo"), SortFieldType::String)?])?;
  iwc.set_index_sort(index_sort)?;

  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();
  doc.add(SortedDocValuesField::new(
    "foo",
    BytesRef::from_string("zzz"),
  ));
  writer.add_document(doc)?;
  writer.commit()?;

  let mut doc = Document::new();
  doc.add(SortedDocValuesField::new(
    "foo",
    BytesRef::from_string("aaa"),
  ));
  writer.add_document(doc)?;
  writer.commit()?;

  let mut doc = Document::new();
  doc.add(SortedDocValuesField::new(
    "foo",
    BytesRef::from_string("mmm"),
  ));
  writer.add_document(doc)?;
  writer.force_merge(1)?;

  let reader = directory_reader::open_from_writer(&writer)?;
  let leaf = get_only_leaf_reader(&reader)?;
  assert_eq!(3, leaf.max_doc()?);

  let mut values = leaf.get_sorted_doc_values("foo")?.unwrap();

  assert_eq!(0, values.next_doc()?);
  let ord_value = values.ord_value()?;
  assert_eq!("aaa", values.lookup_ord(ord_value)?.utf8_to_string()?);

  assert_eq!(1, values.next_doc()?);
  let ord_value = values.ord_value()?;
  assert_eq!("mmm", values.lookup_ord(ord_value)?.utf8_to_string()?);

  assert_eq!(2, values.next_doc()?);
  let ord_value = values.ord_value()?;
  assert_eq!("zzz", values.lookup_ord(ord_value)?.utf8_to_string()?);
  writer.close()?;
  Ok(())
}

#[test]
fn test_basic_multi_valued_string() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let index_sort = Sort::with_fields(vec![SortedSetSortField::new("foo", false)?])?;
  iwc.set_index_sort(index_sort)?;

  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("id", 3));
  doc.add(SortedSetDocValuesField::new(
    "foo",
    BytesRef::from_string("zzz"),
  ));
  writer.add_document(doc)?;
  writer.commit()?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("id", 1));
  doc.add(SortedSetDocValuesField::new(
    "foo",
    BytesRef::from_string("aaa"),
  ));
  doc.add(SortedSetDocValuesField::new(
    "foo",
    BytesRef::from_string("zzz"),
  ));
  doc.add(SortedSetDocValuesField::new(
    "foo",
    BytesRef::from_string("bcg"),
  ));
  writer.add_document(doc)?;
  writer.commit()?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("id", 2));
  doc.add(SortedSetDocValuesField::new(
    "foo",
    BytesRef::from_string("mmm"),
  ));
  doc.add(SortedSetDocValuesField::new(
    "foo",
    BytesRef::from_string("pppp"),
  ));
  writer.add_document(doc)?;
  writer.force_merge(1)?;

  let reader = directory_reader::open_from_writer(&writer)?;
  let leaf = get_only_leaf_reader(&reader)?;
  assert_eq!(3, leaf.max_doc()?);

  let mut values = leaf.get_numeric_doc_values("id")?.unwrap();

  assert_eq!(0, values.next_doc()?);
  assert_eq!(1_i64, values.long_value()?);

  assert_eq!(1, values.next_doc()?);
  assert_eq!(2_i64, values.long_value()?);

  assert_eq!(2, values.next_doc()?);
  assert_eq!(3_i64, values.long_value()?);

  writer.close()?;
  Ok(())
}

#[test]
fn test_missing_string_first() -> Result<()> {
  for reverse in [true, false] {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;

    let analyzer = MockAnalyzer::new(&mut random);
    let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;

    let mut sort_field = SortField::with_reverse(Some("foo"), SortFieldType::String, reverse)?;
    sort_field.set_missing_value(StringFirst)?;
    let index_sort = Sort::with_fields(vec![sort_field])?;
    iwc.set_index_sort(index_sort)?;

    let writer = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc = Document::new();
    doc.add(SortedDocValuesField::new(
      "foo",
      BytesRef::from_string("zzz"),
    ));
    writer.add_document(doc)?;
    writer.commit()?;

    writer.add_document(Document::new())?;
    writer.commit()?;

    let mut doc = Document::new();
    doc.add(SortedDocValuesField::new(
      "foo",
      BytesRef::from_string("mmm"),
    ));
    writer.add_document(doc)?;
    writer.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&writer)?;
    let leaf = get_only_leaf_reader(&reader)?;
    assert_eq!(3, leaf.max_doc()?);

    let mut values = leaf.get_sorted_doc_values("foo")?.unwrap();

    if reverse {
      assert_eq!(0, values.next_doc()?);
      let ord_value = values.ord_value()?;
      assert_eq!("zzz", values.lookup_ord(ord_value)?.utf8_to_string()?);

      assert_eq!(1, values.next_doc()?);
      let ord_value = values.ord_value()?;
      assert_eq!("mmm", values.lookup_ord(ord_value)?.utf8_to_string()?);
    } else {
      assert_eq!(1, values.next_doc()?);
      let ord_value = values.ord_value()?;
      assert_eq!("mmm", values.lookup_ord(ord_value)?.utf8_to_string()?);

      assert_eq!(2, values.next_doc()?);
      let ord_value = values.ord_value()?;
      assert_eq!("zzz", values.lookup_ord(ord_value)?.utf8_to_string()?);
    }

    writer.close()?;
  }

  Ok(())
}
#[test]
fn test_missing_multi_valued_string_first() -> Result<()> {
  for reverse in [true, false] {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;

    let analyzer = MockAnalyzer::new(&mut random);
    let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;

    let mut sort_field = SortedSetSortField::new("foo", reverse)?;
    sort_field.set_missing_value(StringFirst)?;
    let index_sort = Sort::with_fields(vec![sort_field])?;
    iwc.set_index_sort(index_sort)?;

    let writer = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("id", 3));
    doc.add(SortedSetDocValuesField::new(
      "foo",
      BytesRef::from_string("zzz"),
    ));
    doc.add(SortedSetDocValuesField::new(
      "foo",
      BytesRef::from_string("zzza"),
    ));
    doc.add(SortedSetDocValuesField::new(
      "foo",
      BytesRef::from_string("zzzd"),
    ));
    writer.add_document(doc)?;
    writer.commit()?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("id", 1));
    writer.add_document(doc)?;
    writer.commit()?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("id", 2));
    doc.add(SortedSetDocValuesField::new(
      "foo",
      BytesRef::from_string("mmm"),
    ));
    doc.add(SortedSetDocValuesField::new(
      "foo",
      BytesRef::from_string("nnnn"),
    ));
    writer.add_document(doc)?;
    writer.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&writer)?;
    let leaf = get_only_leaf_reader(&reader)?;
    assert_eq!(3, leaf.max_doc()?);

    let mut values = leaf.get_numeric_doc_values("id")?.unwrap();

    if reverse {
      assert_eq!(0, values.next_doc()?);
      assert_eq!(3_i64, values.long_value()?);

      assert_eq!(1, values.next_doc()?);
      assert_eq!(2_i64, values.long_value()?);

      assert_eq!(2, values.next_doc()?);
      assert_eq!(1_i64, values.long_value()?);
    } else {
      assert_eq!(0, values.next_doc()?);
      assert_eq!(1_i64, values.long_value()?);

      assert_eq!(1, values.next_doc()?);
      assert_eq!(2_i64, values.long_value()?);

      assert_eq!(2, values.next_doc()?);
      assert_eq!(3_i64, values.long_value()?);
    }

    writer.close()?;
  }

  Ok(())
}

#[test]
fn test_missing_string_last() -> Result<()> {
  for reverse in [true, false] {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;

    let analyzer = MockAnalyzer::new(&mut random);
    let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;

    let mut sort_field = SortField::with_reverse(Some("foo"), SortFieldType::String, reverse)?;
    sort_field.set_missing_value(StringLast)?;
    let index_sort = Sort::with_fields(vec![sort_field])?;
    iwc.set_index_sort(index_sort)?;

    let writer = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc = Document::new();
    doc.add(SortedDocValuesField::new(
      "foo",
      BytesRef::from_string("zzz"),
    ));
    writer.add_document(doc)?;
    writer.commit()?;

    writer.add_document(Document::new())?;
    writer.commit()?;

    let mut doc = Document::new();
    doc.add(SortedDocValuesField::new(
      "foo",
      BytesRef::from_string("mmm"),
    ));
    writer.add_document(doc)?;
    writer.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&writer)?;
    let leaf = get_only_leaf_reader(&reader)?;
    assert_eq!(3, leaf.max_doc()?);

    let mut values = leaf.get_sorted_doc_values("foo")?.unwrap();

    if reverse {
      assert_eq!(1, values.next_doc()?);
      let ord_value = values.ord_value()?;
      assert_eq!("zzz", values.lookup_ord(ord_value)?.utf8_to_string()?);

      assert_eq!(2, values.next_doc()?);
      let ord_value = values.ord_value()?;
      assert_eq!("mmm", values.lookup_ord(ord_value)?.utf8_to_string()?);
    } else {
      assert_eq!(0, values.next_doc()?);
      let ord_value = values.ord_value()?;
      assert_eq!("mmm", values.lookup_ord(ord_value)?.utf8_to_string()?);

      assert_eq!(1, values.next_doc()?);
      let ord_value = values.ord_value()?;
      assert_eq!("zzz", values.lookup_ord(ord_value)?.utf8_to_string()?);
    }

    assert_eq!(NO_MORE_DOCS, values.next_doc()?);
    writer.close()?;
  }

  Ok(())
}

#[test]
fn test_missing_multi_valued_string_last() -> Result<()> {
  for reverse in [true, false] {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;

    let analyzer = MockAnalyzer::new(&mut random);
    let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;

    let mut sort_field = SortedSetSortField::new("foo", reverse)?;
    sort_field.set_missing_value(StringLast)?;
    let index_sort = Sort::with_fields(vec![sort_field])?;
    iwc.set_index_sort(index_sort)?;

    let writer = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("id", 2));
    doc.add(SortedSetDocValuesField::new(
      "foo",
      BytesRef::from_string("zzz"),
    ));
    doc.add(SortedSetDocValuesField::new(
      "foo",
      BytesRef::from_string("zzzd"),
    ));
    writer.add_document(doc)?;
    writer.commit()?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("id", 3));
    writer.add_document(doc)?;
    writer.commit()?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("id", 1));
    doc.add(SortedSetDocValuesField::new(
      "foo",
      BytesRef::from_string("mmm"),
    ));
    doc.add(SortedSetDocValuesField::new(
      "foo",
      BytesRef::from_string("ppp"),
    ));
    writer.add_document(doc)?;
    writer.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&writer)?;
    let leaf = get_only_leaf_reader(&reader)?;
    assert_eq!(3, leaf.max_doc()?);

    let mut values = leaf.get_numeric_doc_values("id")?.unwrap();

    if reverse {
      assert_eq!(0, values.next_doc()?);
      assert_eq!(3_i64, values.long_value()?);

      assert_eq!(1, values.next_doc()?);
      assert_eq!(2_i64, values.long_value()?);

      assert_eq!(2, values.next_doc()?);
      assert_eq!(1_i64, values.long_value()?);
    } else {
      assert_eq!(0, values.next_doc()?);
      assert_eq!(1_i64, values.long_value()?);

      assert_eq!(1, values.next_doc()?);
      assert_eq!(2_i64, values.long_value()?);

      assert_eq!(2, values.next_doc()?);
      assert_eq!(3_i64, values.long_value()?);
    }

    writer.close()?;
  }

  Ok(())
}

#[test]
fn test_basic_long() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let index_sort = Sort::with_fields(vec![SortField::new(Some("foo"), SortFieldType::Long)?])?;
  iwc.set_index_sort(index_sort)?;

  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("foo", 18));
  writer.add_document(doc)?;
  writer.commit()?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("foo", -1));
  writer.add_document(doc)?;
  writer.commit()?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("foo", 7));
  writer.add_document(doc)?;
  writer.force_merge(1)?;

  let reader = directory_reader::open_from_writer(&writer)?;
  let leaf = get_only_leaf_reader(&reader)?;
  assert_eq!(3, leaf.max_doc()?);

  let mut values = leaf.get_numeric_doc_values("foo")?.unwrap();

  assert_eq!(0, values.next_doc()?);
  assert_eq!(-1_i64, values.long_value()?);

  assert_eq!(1, values.next_doc()?);
  assert_eq!(7_i64, values.long_value()?);

  assert_eq!(2, values.next_doc()?);
  assert_eq!(18_i64, values.long_value()?);

  writer.close()?;
  Ok(())
}

#[test]
fn test_basic_multi_valued_long() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let index_sort = Sort::with_fields(vec![SortedNumericSortField::new(
    "foo",
    SortFieldType::Long,
  )?])?;
  iwc.set_index_sort(index_sort)?;

  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("id", 3));
  doc.add(SortedNumericDocValuesField::new("foo", 18));
  doc.add(SortedNumericDocValuesField::new("foo", 35));
  writer.add_document(doc)?;
  writer.commit()?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("id", 1));
  doc.add(SortedNumericDocValuesField::new("foo", -1));
  writer.add_document(doc)?;
  writer.commit()?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("id", 2));
  doc.add(SortedNumericDocValuesField::new("foo", 7));
  doc.add(SortedNumericDocValuesField::new("foo", 22));
  writer.add_document(doc)?;
  writer.force_merge(1)?;

  let reader = directory_reader::open_from_writer(&writer)?;
  let leaf = get_only_leaf_reader(&reader)?;
  assert_eq!(3, leaf.max_doc()?);

  let mut values = leaf.get_numeric_doc_values("id")?.unwrap();

  assert_eq!(0, values.next_doc()?);
  assert_eq!(1_i64, values.long_value()?);

  assert_eq!(1, values.next_doc()?);
  assert_eq!(2_i64, values.long_value()?);

  assert_eq!(2, values.next_doc()?);
  assert_eq!(3_i64, values.long_value()?);

  writer.close()?;
  Ok(())
}

#[test]
fn test_missing_long_first() -> Result<()> {
  for reverse in [true, false] {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;

    let analyzer = MockAnalyzer::new(&mut random);
    let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;

    let mut sort_field = SortField::with_reverse(Some("foo"), SortFieldType::Long, reverse)?;
    sort_field.set_missing_value(i64::MIN)?;
    let index_sort = Sort::with_fields(vec![sort_field])?;
    iwc.set_index_sort(index_sort)?;

    let writer = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("foo", 18));
    writer.add_document(doc)?;
    writer.commit()?;

    writer.add_document(Document::new())?;
    writer.commit()?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("foo", 7));
    writer.add_document(doc)?;
    writer.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&writer)?;
    let leaf = get_only_leaf_reader(&reader)?;
    assert_eq!(3, leaf.max_doc()?);

    let mut values = leaf.get_numeric_doc_values("foo")?.unwrap();

    if reverse {
      assert_eq!(0, values.next_doc()?);
      assert_eq!(18_i64, values.long_value()?);

      assert_eq!(1, values.next_doc()?);
      assert_eq!(7_i64, values.long_value()?);
    } else {
      assert_eq!(1, values.next_doc()?);
      assert_eq!(7_i64, values.long_value()?);

      assert_eq!(2, values.next_doc()?);
      assert_eq!(18_i64, values.long_value()?);
    }

    writer.close()?;
  }

  Ok(())
}

#[test]
fn test_missing_multi_valued_long_first() -> Result<()> {
  for reverse in [true, false] {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;

    let analyzer = MockAnalyzer::new(&mut random);
    let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;

    let mut sort_field = SortedNumericSortField::with_reverse("foo", SortFieldType::Long, reverse)?;
    sort_field.set_missing_value(i64::MIN)?;
    let index_sort = Sort::with_fields(vec![sort_field])?;
    iwc.set_index_sort(index_sort)?;

    let writer = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("id", 3));
    doc.add(SortedNumericDocValuesField::new("foo", 18));
    doc.add(SortedNumericDocValuesField::new("foo", 27));
    writer.add_document(doc)?;
    writer.commit()?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("id", 1));
    writer.add_document(doc)?;
    writer.commit()?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("id", 2));
    doc.add(SortedNumericDocValuesField::new("foo", 7));
    doc.add(SortedNumericDocValuesField::new("foo", 24));
    writer.add_document(doc)?;
    writer.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&writer)?;
    let leaf = get_only_leaf_reader(&reader)?;
    assert_eq!(3, leaf.max_doc()?);

    let mut values = leaf.get_numeric_doc_values("id")?.unwrap();

    if reverse {
      assert_eq!(0, values.next_doc()?);
      assert_eq!(3_i64, values.long_value()?);

      assert_eq!(1, values.next_doc()?);
      assert_eq!(2_i64, values.long_value()?);

      assert_eq!(2, values.next_doc()?);
      assert_eq!(1_i64, values.long_value()?);
    } else {
      assert_eq!(0, values.next_doc()?);
      assert_eq!(1_i64, values.long_value()?);

      assert_eq!(1, values.next_doc()?);
      assert_eq!(2_i64, values.long_value()?);

      assert_eq!(2, values.next_doc()?);
      assert_eq!(3_i64, values.long_value()?);
    }

    writer.close()?;
  }

  Ok(())
}
#[test]
fn test_missing_long_last() -> Result<()> {
  for reverse in [true, false] {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;

    let analyzer = MockAnalyzer::new(&mut random);
    let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;

    let mut sort_field = SortField::with_reverse(Some("foo"), SortFieldType::Long, reverse)?;
    sort_field.set_missing_value(i64::MAX)?;
    let index_sort = Sort::with_fields(vec![sort_field])?;
    iwc.set_index_sort(index_sort)?;

    let writer = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("foo", 18));
    writer.add_document(doc)?;
    writer.commit()?;

    writer.add_document(Document::new())?;
    writer.commit()?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("foo", 7));
    writer.add_document(doc)?;
    writer.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&writer)?;
    let leaf = get_only_leaf_reader(&reader)?;
    assert_eq!(3, leaf.max_doc()?);

    let mut values = leaf.get_numeric_doc_values("foo")?.unwrap();

    if reverse {
      assert_eq!(1, values.next_doc()?);
      assert_eq!(18_i64, values.long_value()?);

      assert_eq!(2, values.next_doc()?);
      assert_eq!(7_i64, values.long_value()?);
    } else {
      assert_eq!(0, values.next_doc()?);
      assert_eq!(7_i64, values.long_value()?);

      assert_eq!(1, values.next_doc()?);
      assert_eq!(18_i64, values.long_value()?);
    }

    assert_eq!(NO_MORE_DOCS, values.next_doc()?);
    writer.close()?;
  }

  Ok(())
}

#[test]
fn test_missing_multi_valued_long_last() -> Result<()> {
  for reverse in [true, false] {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;

    let analyzer = MockAnalyzer::new(&mut random);
    let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;

    let mut sort_field = SortedNumericSortField::with_reverse("foo", SortFieldType::Long, reverse)?;
    sort_field.set_missing_value(i64::MAX)?;
    let index_sort = Sort::with_fields(vec![sort_field])?;
    iwc.set_index_sort(index_sort)?;

    let writer = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("id", 2));
    doc.add(SortedNumericDocValuesField::new("foo", 18));
    doc.add(SortedNumericDocValuesField::new("foo", 65));
    writer.add_document(doc)?;
    writer.commit()?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("id", 3));
    writer.add_document(doc)?;
    writer.commit()?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("id", 1));
    doc.add(SortedNumericDocValuesField::new("foo", 7));
    doc.add(SortedNumericDocValuesField::new("foo", 34));
    doc.add(SortedNumericDocValuesField::new("foo", 74));
    writer.add_document(doc)?;
    writer.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&writer)?;
    let leaf = get_only_leaf_reader(&reader)?;
    assert_eq!(3, leaf.max_doc()?);

    let mut values = leaf.get_numeric_doc_values("id")?.unwrap();

    if reverse {
      assert_eq!(0, values.next_doc()?);
      assert_eq!(3_i64, values.long_value()?);

      assert_eq!(1, values.next_doc()?);
      assert_eq!(2_i64, values.long_value()?);

      assert_eq!(2, values.next_doc()?);
      assert_eq!(1_i64, values.long_value()?);
    } else {
      assert_eq!(0, values.next_doc()?);
      assert_eq!(1_i64, values.long_value()?);

      assert_eq!(1, values.next_doc()?);
      assert_eq!(2_i64, values.long_value()?);

      assert_eq!(2, values.next_doc()?);
      assert_eq!(3_i64, values.long_value()?);
    }

    writer.close()?;
  }

  Ok(())
}

#[test]
fn test_basic_int() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let index_sort = Sort::with_fields(vec![SortField::new(Some("foo"), SortFieldType::Int)?])?;
  iwc.set_index_sort(index_sort)?;

  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("foo", 18));
  writer.add_document(doc)?;
  writer.commit()?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("foo", -1));
  writer.add_document(doc)?;
  writer.commit()?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("foo", 7));
  writer.add_document(doc)?;
  writer.force_merge(1)?;

  let reader = directory_reader::open_from_writer(&writer)?;
  let leaf = get_only_leaf_reader(&reader)?;
  assert_eq!(3, leaf.max_doc()?);

  let mut values = leaf.get_numeric_doc_values("foo")?.unwrap();

  assert_eq!(0, values.next_doc()?);
  assert_eq!(-1_i64, values.long_value()?);

  assert_eq!(1, values.next_doc()?);
  assert_eq!(7_i64, values.long_value()?);

  assert_eq!(2, values.next_doc()?);
  assert_eq!(18_i64, values.long_value()?);

  writer.close()?;
  Ok(())
}

#[test]
fn test_basic_multi_valued_int() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let index_sort = Sort::with_fields(vec![SortedNumericSortField::new(
    "foo",
    SortFieldType::Int,
  )?])?;
  iwc.set_index_sort(index_sort)?;

  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("id", 3));
  doc.add(SortedNumericDocValuesField::new("foo", 18));
  doc.add(SortedNumericDocValuesField::new("foo", 34));
  writer.add_document(doc)?;
  writer.commit()?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("id", 1));
  doc.add(SortedNumericDocValuesField::new("foo", -1));
  doc.add(SortedNumericDocValuesField::new("foo", 34));
  writer.add_document(doc)?;
  writer.commit()?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("id", 2));
  doc.add(SortedNumericDocValuesField::new("foo", 7));
  doc.add(SortedNumericDocValuesField::new("foo", 22));
  doc.add(SortedNumericDocValuesField::new("foo", 27));
  writer.add_document(doc)?;
  writer.force_merge(1)?;

  let reader = directory_reader::open_from_writer(&writer)?;
  let leaf = get_only_leaf_reader(&reader)?;
  assert_eq!(3, leaf.max_doc()?);

  let mut values = leaf.get_numeric_doc_values("id")?.unwrap();

  assert_eq!(0, values.next_doc()?);
  assert_eq!(1_i64, values.long_value()?);

  assert_eq!(1, values.next_doc()?);
  assert_eq!(2_i64, values.long_value()?);

  assert_eq!(2, values.next_doc()?);
  assert_eq!(3_i64, values.long_value()?);

  writer.close()?;
  Ok(())
}

#[test]
fn test_missing_int_first() -> Result<()> {
  for reverse in [true, false] {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;

    let analyzer = MockAnalyzer::new(&mut random);
    let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;

    let mut sort_field = SortField::with_reverse(Some("foo"), SortFieldType::Int, reverse)?;
    sort_field.set_missing_value(i32::MIN)?;
    let index_sort = Sort::with_fields(vec![sort_field])?;
    iwc.set_index_sort(index_sort)?;

    let writer = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("foo", 18));
    writer.add_document(doc)?;
    writer.commit()?;

    writer.add_document(Document::new())?;
    writer.commit()?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("foo", 7));
    writer.add_document(doc)?;
    writer.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&writer)?;
    let leaf = get_only_leaf_reader(&reader)?;
    assert_eq!(3, leaf.max_doc()?);

    let mut values = leaf.get_numeric_doc_values("foo")?.unwrap();

    if reverse {
      assert_eq!(0, values.next_doc()?);
      assert_eq!(18_i64, values.long_value()?);

      assert_eq!(1, values.next_doc()?);
      assert_eq!(7_i64, values.long_value()?);
    } else {
      assert_eq!(1, values.next_doc()?);
      assert_eq!(7_i64, values.long_value()?);

      assert_eq!(2, values.next_doc()?);
      assert_eq!(18_i64, values.long_value()?);
    }

    writer.close()?;
  }

  Ok(())
}

#[test]
fn test_missing_multi_valued_int_first() -> Result<()> {
  for reverse in [true, false] {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;

    let analyzer = MockAnalyzer::new(&mut random);
    let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;

    let mut sort_field = SortedNumericSortField::with_reverse("foo", SortFieldType::Int, reverse)?;
    sort_field.set_missing_value(i32::MIN)?;
    let index_sort = Sort::with_fields(vec![sort_field])?;
    iwc.set_index_sort(index_sort)?;

    let writer = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("id", 3));
    doc.add(SortedNumericDocValuesField::new("foo", 18));
    doc.add(SortedNumericDocValuesField::new("foo", 187667));
    writer.add_document(doc)?;
    writer.commit()?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("id", 1));
    writer.add_document(doc)?;
    writer.commit()?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("id", 2));
    doc.add(SortedNumericDocValuesField::new("foo", 7));
    doc.add(SortedNumericDocValuesField::new("foo", 34));
    writer.add_document(doc)?;
    writer.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&writer)?;
    let leaf = get_only_leaf_reader(&reader)?;
    assert_eq!(3, leaf.max_doc()?);

    let mut values = leaf.get_numeric_doc_values("id")?.unwrap();

    if reverse {
      assert_eq!(0, values.next_doc()?);
      assert_eq!(3_i64, values.long_value()?);

      assert_eq!(1, values.next_doc()?);
      assert_eq!(2_i64, values.long_value()?);

      assert_eq!(2, values.next_doc()?);
      assert_eq!(1_i64, values.long_value()?);
    } else {
      assert_eq!(0, values.next_doc()?);
      assert_eq!(1_i64, values.long_value()?);

      assert_eq!(1, values.next_doc()?);
      assert_eq!(2_i64, values.long_value()?);

      assert_eq!(2, values.next_doc()?);
      assert_eq!(3_i64, values.long_value()?);
    }

    writer.close()?;
  }

  Ok(())
}

#[test]
fn test_missing_int_last() -> Result<()> {
  for reverse in [true, false] {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;

    let analyzer = MockAnalyzer::new(&mut random);
    let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;

    let mut sort_field = SortField::with_reverse(Some("foo"), SortFieldType::Int, reverse)?;
    sort_field.set_missing_value(i32::MAX)?;
    let index_sort = Sort::with_fields(vec![sort_field])?;
    iwc.set_index_sort(index_sort)?;

    let writer = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("foo", 18));
    writer.add_document(doc)?;
    writer.commit()?;

    writer.add_document(Document::new())?;
    writer.commit()?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("foo", 7));
    writer.add_document(doc)?;
    writer.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&writer)?;
    let leaf = get_only_leaf_reader(&reader)?;
    assert_eq!(3, leaf.max_doc()?);

    let mut values = leaf.get_numeric_doc_values("foo")?.unwrap();

    if reverse {
      assert_eq!(1, values.next_doc()?);
      assert_eq!(18_i64, values.long_value()?);

      assert_eq!(2, values.next_doc()?);
      assert_eq!(7_i64, values.long_value()?);
    } else {
      assert_eq!(0, values.next_doc()?);
      assert_eq!(7_i64, values.long_value()?);

      assert_eq!(1, values.next_doc()?);
      assert_eq!(18_i64, values.long_value()?);
    }

    assert_eq!(NO_MORE_DOCS, values.next_doc()?);
    writer.close()?;
  }

  Ok(())
}

#[test]
fn test_missing_multi_valued_int_last() -> Result<()> {
  for reverse in [true, false] {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;

    let analyzer = MockAnalyzer::new(&mut random);
    let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;

    let mut sort_field = SortedNumericSortField::with_reverse("foo", SortFieldType::Int, reverse)?;
    sort_field.set_missing_value(i32::MAX)?;
    let index_sort = Sort::with_fields(vec![sort_field])?;
    iwc.set_index_sort(index_sort)?;

    let writer = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("id", 2));
    doc.add(SortedNumericDocValuesField::new("foo", 18));
    doc.add(SortedNumericDocValuesField::new("foo", 6372));
    writer.add_document(doc)?;
    writer.commit()?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("id", 3));
    writer.add_document(doc)?;
    writer.commit()?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("id", 1));
    doc.add(SortedNumericDocValuesField::new("foo", 7));
    doc.add(SortedNumericDocValuesField::new("foo", 8));
    writer.add_document(doc)?;
    writer.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&writer)?;
    let leaf = get_only_leaf_reader(&reader)?;
    assert_eq!(3, leaf.max_doc()?);

    let mut values = leaf.get_numeric_doc_values("id")?.unwrap();

    if reverse {
      assert_eq!(0, values.next_doc()?);
      assert_eq!(3_i64, values.long_value()?);

      assert_eq!(1, values.next_doc()?);
      assert_eq!(2_i64, values.long_value()?);

      assert_eq!(2, values.next_doc()?);
      assert_eq!(1_i64, values.long_value()?);
    } else {
      assert_eq!(0, values.next_doc()?);
      assert_eq!(1_i64, values.long_value()?);

      assert_eq!(1, values.next_doc()?);
      assert_eq!(2_i64, values.long_value()?);

      assert_eq!(2, values.next_doc()?);
      assert_eq!(3_i64, values.long_value()?);
    }

    writer.close()?;
  }

  Ok(())
}

#[test]
fn test_basic_double() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let index_sort = Sort::with_fields(vec![SortField::new(Some("foo"), SortFieldType::Double)?])?;
  iwc.set_index_sort(index_sort)?;

  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();
  doc.add(DoubleDocValuesField::new("foo", 18.0));
  writer.add_document(doc)?;
  writer.commit()?;

  let mut doc = Document::new();
  doc.add(DoubleDocValuesField::new("foo", -1.0));
  writer.add_document(doc)?;
  writer.commit()?;

  let mut doc = Document::new();
  doc.add(DoubleDocValuesField::new("foo", 7.0));
  writer.add_document(doc)?;
  writer.force_merge(1)?;

  let reader = directory_reader::open_from_writer(&writer)?;
  let leaf = get_only_leaf_reader(&reader)?;
  assert_eq!(3, leaf.max_doc()?);

  let mut values = leaf.get_numeric_doc_values("foo")?.unwrap();

  assert_eq!(0, values.next_doc()?);
  assert_eq!(-1.0, f64::from_bits(values.long_value()? as u64));

  assert_eq!(1, values.next_doc()?);
  assert_eq!(7.0, f64::from_bits(values.long_value()? as u64));

  assert_eq!(2, values.next_doc()?);
  assert_eq!(18.0, f64::from_bits(values.long_value()? as u64));

  writer.close()?;
  Ok(())
}
#[test]
fn test_basic_multi_valued_double() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let index_sort = Sort::with_fields(vec![SortedNumericSortField::new(
    "foo",
    SortFieldType::Double,
  )?])?;
  iwc.set_index_sort(index_sort)?;

  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("id", 3));
  doc.add(SortedNumericDocValuesField::new(
    "foo",
    NumericUtils::double_to_sortable_long(7.54),
  ));
  doc.add(SortedNumericDocValuesField::new(
    "foo",
    NumericUtils::double_to_sortable_long(27.0),
  ));
  writer.add_document(doc)?;
  writer.commit()?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("id", 1));
  doc.add(SortedNumericDocValuesField::new(
    "foo",
    NumericUtils::double_to_sortable_long(-1.0),
  ));
  doc.add(SortedNumericDocValuesField::new(
    "foo",
    NumericUtils::double_to_sortable_long(0.0),
  ));
  writer.add_document(doc)?;
  writer.commit()?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("id", 2));
  doc.add(SortedNumericDocValuesField::new(
    "foo",
    NumericUtils::double_to_sortable_long(7.0),
  ));
  doc.add(SortedNumericDocValuesField::new(
    "foo",
    NumericUtils::double_to_sortable_long(7.67),
  ));
  writer.add_document(doc)?;
  writer.force_merge(1)?;

  let reader = directory_reader::open_from_writer(&writer)?;
  let leaf = get_only_leaf_reader(&reader)?;
  assert_eq!(3, leaf.max_doc()?);

  let mut values = leaf.get_numeric_doc_values("id")?.unwrap();

  assert_eq!(0, values.next_doc()?);
  assert_eq!(1_i64, values.long_value()?);

  assert_eq!(1, values.next_doc()?);
  assert_eq!(2_i64, values.long_value()?);

  assert_eq!(2, values.next_doc()?);
  assert_eq!(3_i64, values.long_value()?);

  writer.close()?;
  Ok(())
}

#[test]
fn test_missing_double_first() -> Result<()> {
  for reverse in [true, false] {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;

    let analyzer = MockAnalyzer::new(&mut random);
    let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;

    let mut sort_field = SortField::with_reverse(Some("foo"), SortFieldType::Double, reverse)?;
    sort_field.set_missing_value(f64::NEG_INFINITY)?;
    let index_sort = Sort::with_fields(vec![sort_field])?;
    iwc.set_index_sort(index_sort)?;

    let writer = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc = Document::new();
    doc.add(DoubleDocValuesField::new("foo", 18.0));
    writer.add_document(doc)?;
    writer.commit()?;

    writer.add_document(Document::new())?;
    writer.commit()?;

    let mut doc = Document::new();
    doc.add(DoubleDocValuesField::new("foo", 7.0));
    writer.add_document(doc)?;
    writer.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&writer)?;
    let leaf = get_only_leaf_reader(&reader)?;
    assert_eq!(3, leaf.max_doc()?);

    let mut values = leaf.get_numeric_doc_values("foo")?.unwrap();

    if reverse {
      assert_eq!(0, values.next_doc()?);
      assert_eq!(18.0, f64::from_bits(values.long_value()? as u64));

      assert_eq!(1, values.next_doc()?);
      assert_eq!(7.0, f64::from_bits(values.long_value()? as u64));
    } else {
      assert_eq!(1, values.next_doc()?);
      assert_eq!(7.0, f64::from_bits(values.long_value()? as u64));

      assert_eq!(2, values.next_doc()?);
      assert_eq!(18.0, f64::from_bits(values.long_value()? as u64));
    }

    writer.close()?;
  }

  Ok(())
}

#[test]
fn test_missing_multi_valued_double_first() -> Result<()> {
  for reverse in [true, false] {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;

    let analyzer = MockAnalyzer::new(&mut random);
    let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;

    let mut sort_field =
      SortedNumericSortField::with_reverse("foo", SortFieldType::Double, reverse)?;
    sort_field.set_missing_value(f64::NEG_INFINITY)?;
    let index_sort = Sort::with_fields(vec![sort_field])?;
    iwc.set_index_sort(index_sort)?;

    let writer = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("id", 3));
    doc.add(SortedNumericDocValuesField::new(
      "foo",
      NumericUtils::double_to_sortable_long(18.0),
    ));
    doc.add(SortedNumericDocValuesField::new(
      "foo",
      NumericUtils::double_to_sortable_long(18.76),
    ));
    writer.add_document(doc)?;
    writer.commit()?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("id", 1));
    writer.add_document(doc)?;
    writer.commit()?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("id", 2));
    doc.add(SortedNumericDocValuesField::new(
      "foo",
      NumericUtils::double_to_sortable_long(7.0),
    ));
    doc.add(SortedNumericDocValuesField::new(
      "foo",
      NumericUtils::double_to_sortable_long(70.0),
    ));
    writer.add_document(doc)?;
    writer.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&writer)?;
    let leaf = get_only_leaf_reader(&reader)?;
    assert_eq!(3, leaf.max_doc()?);

    let mut values = leaf.get_numeric_doc_values("id")?.unwrap();

    if reverse {
      assert_eq!(0, values.next_doc()?);
      assert_eq!(3_i64, values.long_value()?);

      assert_eq!(1, values.next_doc()?);
      assert_eq!(2_i64, values.long_value()?);

      assert_eq!(2, values.next_doc()?);
      assert_eq!(1_i64, values.long_value()?);
    } else {
      assert_eq!(0, values.next_doc()?);
      assert_eq!(1_i64, values.long_value()?);

      assert_eq!(1, values.next_doc()?);
      assert_eq!(2_i64, values.long_value()?);

      assert_eq!(2, values.next_doc()?);
      assert_eq!(3_i64, values.long_value()?);
    }

    writer.close()?;
  }

  Ok(())
}

#[test]
fn test_missing_double_last() -> Result<()> {
  for reverse in [true, false] {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;

    let analyzer = MockAnalyzer::new(&mut random);
    let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;

    let mut sort_field = SortField::with_reverse(Some("foo"), SortFieldType::Double, reverse)?;
    sort_field.set_missing_value(f64::INFINITY)?;
    let index_sort = Sort::with_fields(vec![sort_field])?;
    iwc.set_index_sort(index_sort)?;

    let writer = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc = Document::new();
    doc.add(DoubleDocValuesField::new("foo", 18.0));
    writer.add_document(doc)?;
    writer.commit()?;

    writer.add_document(Document::new())?;
    writer.commit()?;

    let mut doc = Document::new();
    doc.add(DoubleDocValuesField::new("foo", 7.0));
    writer.add_document(doc)?;
    writer.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&writer)?;
    let leaf = get_only_leaf_reader(&reader)?;
    assert_eq!(3, leaf.max_doc()?);

    let mut values = leaf.get_numeric_doc_values("foo")?.unwrap();

    if reverse {
      assert_eq!(1, values.next_doc()?);
      assert_eq!(18.0, f64::from_bits(values.long_value()? as u64));

      assert_eq!(2, values.next_doc()?);
      assert_eq!(7.0, f64::from_bits(values.long_value()? as u64));
    } else {
      assert_eq!(0, values.next_doc()?);
      assert_eq!(7.0, f64::from_bits(values.long_value()? as u64));

      assert_eq!(1, values.next_doc()?);
      assert_eq!(18.0, f64::from_bits(values.long_value()? as u64));
    }

    assert_eq!(NO_MORE_DOCS, values.next_doc()?);
    writer.close()?;
  }

  Ok(())
}
#[test]
fn test_missing_multi_valued_double_last() -> Result<()> {
  for reverse in [true, false] {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;

    let analyzer = MockAnalyzer::new(&mut random);
    let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;

    let mut sort_field =
      SortedNumericSortField::with_reverse("foo", SortFieldType::Double, reverse)?;
    sort_field.set_missing_value(f64::INFINITY)?;
    let index_sort = Sort::with_fields(vec![sort_field])?;
    iwc.set_index_sort(index_sort)?;

    let writer = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("id", 2));
    doc.add(SortedNumericDocValuesField::new(
      "foo",
      NumericUtils::double_to_sortable_long(18.0),
    ));
    doc.add(SortedNumericDocValuesField::new(
      "foo",
      NumericUtils::double_to_sortable_long(8262.0),
    ));
    writer.add_document(doc)?;
    writer.commit()?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("id", 3));
    writer.add_document(doc)?;
    writer.commit()?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("id", 1));
    doc.add(SortedNumericDocValuesField::new(
      "foo",
      NumericUtils::double_to_sortable_long(7.0),
    ));
    doc.add(SortedNumericDocValuesField::new(
      "foo",
      NumericUtils::double_to_sortable_long(7.87),
    ));
    writer.add_document(doc)?;
    writer.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&writer)?;
    let leaf = get_only_leaf_reader(&reader)?;
    assert_eq!(3, leaf.max_doc()?);

    let mut values = leaf.get_numeric_doc_values("id")?.unwrap();

    if reverse {
      assert_eq!(0, values.next_doc()?);
      assert_eq!(3_i64, values.long_value()?);

      assert_eq!(1, values.next_doc()?);
      assert_eq!(2_i64, values.long_value()?);

      assert_eq!(2, values.next_doc()?);
      assert_eq!(1_i64, values.long_value()?);
    } else {
      assert_eq!(0, values.next_doc()?);
      assert_eq!(1_i64, values.long_value()?);

      assert_eq!(1, values.next_doc()?);
      assert_eq!(2_i64, values.long_value()?);

      assert_eq!(2, values.next_doc()?);
      assert_eq!(3_i64, values.long_value()?);
    }

    writer.close()?;
  }

  Ok(())
}

#[test]
fn test_basic_float() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let index_sort = Sort::with_fields(vec![SortField::new(Some("foo"), SortFieldType::Float)?])?;
  iwc.set_index_sort(index_sort)?;

  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();
  doc.add(FloatDocValuesField::new("foo", 18.0));
  writer.add_document(doc)?;
  writer.commit()?;

  let mut doc = Document::new();
  doc.add(FloatDocValuesField::new("foo", -1.0));
  writer.add_document(doc)?;
  writer.commit()?;

  let mut doc = Document::new();
  doc.add(FloatDocValuesField::new("foo", 7.0));
  writer.add_document(doc)?;
  writer.force_merge(1)?;

  let reader = directory_reader::open_from_writer(&writer)?;
  let leaf = get_only_leaf_reader(&reader)?;
  assert_eq!(3, leaf.max_doc()?);

  let mut values = leaf.get_numeric_doc_values("foo")?.unwrap();

  assert_eq!(0, values.next_doc()?);
  assert_eq!(-1.0f32, f32::from_bits(values.long_value()? as u32));

  assert_eq!(1, values.next_doc()?);
  assert_eq!(7.0f32, f32::from_bits(values.long_value()? as u32));

  assert_eq!(2, values.next_doc()?);
  assert_eq!(18.0f32, f32::from_bits(values.long_value()? as u32));

  writer.close()?;
  Ok(())
}

#[test]
fn test_basic_multi_valued_float() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let index_sort = Sort::with_fields(vec![SortedNumericSortField::new(
    "foo",
    SortFieldType::Float,
  )?])?;
  iwc.set_index_sort(index_sort)?;

  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("id", 3));
  doc.add(SortedNumericDocValuesField::new(
    "foo",
    NumericUtils::float_to_sortable_int(18.0) as i64,
  ));
  doc.add(SortedNumericDocValuesField::new(
    "foo",
    NumericUtils::float_to_sortable_int(29.0) as i64,
  ));
  writer.add_document(doc)?;
  writer.commit()?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("id", 1));
  doc.add(SortedNumericDocValuesField::new(
    "foo",
    NumericUtils::float_to_sortable_int(-1.0) as i64,
  ));
  doc.add(SortedNumericDocValuesField::new(
    "foo",
    NumericUtils::float_to_sortable_int(34.0) as i64,
  ));
  writer.add_document(doc)?;
  writer.commit()?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("id", 2));
  doc.add(SortedNumericDocValuesField::new(
    "foo",
    NumericUtils::float_to_sortable_int(7.0) as i64,
  ));
  writer.add_document(doc)?;
  writer.force_merge(1)?;

  let reader = directory_reader::open_from_writer(&writer)?;
  let leaf = get_only_leaf_reader(&reader)?;
  assert_eq!(3, leaf.max_doc()?);

  let mut values = leaf.get_numeric_doc_values("id")?.unwrap();

  assert_eq!(0, values.next_doc()?);
  assert_eq!(1_i64, values.long_value()?);

  assert_eq!(1, values.next_doc()?);
  assert_eq!(2_i64, values.long_value()?);

  assert_eq!(2, values.next_doc()?);
  assert_eq!(3_i64, values.long_value()?);

  writer.close()?;
  Ok(())
}

#[test]
fn test_missing_float_first() -> Result<()> {
  for reverse in [true, false] {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;

    let analyzer = MockAnalyzer::new(&mut random);
    let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;

    let mut sort_field = SortField::with_reverse(Some("foo"), SortFieldType::Float, reverse)?;
    sort_field.set_missing_value(f32::NEG_INFINITY)?;
    let index_sort = Sort::with_fields(vec![sort_field])?;
    iwc.set_index_sort(index_sort)?;

    let writer = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc = Document::new();
    doc.add(FloatDocValuesField::new("foo", 18.0));
    writer.add_document(doc)?;
    writer.commit()?;

    writer.add_document(Document::new())?;
    writer.commit()?;

    let mut doc = Document::new();
    doc.add(FloatDocValuesField::new("foo", 7.0));
    writer.add_document(doc)?;
    writer.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&writer)?;
    let leaf = get_only_leaf_reader(&reader)?;
    assert_eq!(3, leaf.max_doc()?);

    let mut values = leaf.get_numeric_doc_values("foo")?.unwrap();

    if reverse {
      assert_eq!(0, values.next_doc()?);
      assert_eq!(18.0f32, f32::from_bits(values.long_value()? as u32));

      assert_eq!(1, values.next_doc()?);
      assert_eq!(7.0f32, f32::from_bits(values.long_value()? as u32));
    } else {
      assert_eq!(1, values.next_doc()?);
      assert_eq!(7.0f32, f32::from_bits(values.long_value()? as u32));

      assert_eq!(2, values.next_doc()?);
      assert_eq!(18.0f32, f32::from_bits(values.long_value()? as u32));
    }

    writer.close()?;
  }

  Ok(())
}

#[test]
fn test_missing_multi_valued_float_first() -> Result<()> {
  for reverse in [true, false] {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;

    let analyzer = MockAnalyzer::new(&mut random);
    let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;

    let mut sort_field =
      SortedNumericSortField::with_reverse("foo", SortFieldType::Float, reverse)?;
    sort_field.set_missing_value(f32::NEG_INFINITY)?;
    let index_sort = Sort::with_fields(vec![sort_field])?;
    iwc.set_index_sort(index_sort)?;

    let writer = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("id", 3));
    doc.add(SortedNumericDocValuesField::new(
      "foo",
      NumericUtils::float_to_sortable_int(18.0) as i64,
    ));
    doc.add(SortedNumericDocValuesField::new(
      "foo",
      NumericUtils::float_to_sortable_int(726.0) as i64,
    ));
    writer.add_document(doc)?;
    writer.commit()?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("id", 1));
    writer.add_document(doc)?;
    writer.commit()?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("id", 2));
    doc.add(SortedNumericDocValuesField::new(
      "foo",
      NumericUtils::float_to_sortable_int(7.0) as i64,
    ));
    doc.add(SortedNumericDocValuesField::new(
      "foo",
      NumericUtils::float_to_sortable_int(18.0) as i64,
    ));
    writer.add_document(doc)?;
    writer.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&writer)?;
    let leaf = get_only_leaf_reader(&reader)?;
    assert_eq!(3, leaf.max_doc()?);

    let mut values = leaf.get_numeric_doc_values("id")?.unwrap();

    if reverse {
      assert_eq!(0, values.next_doc()?);
      assert_eq!(3_i64, values.long_value()?);

      assert_eq!(1, values.next_doc()?);
      assert_eq!(2_i64, values.long_value()?);

      assert_eq!(2, values.next_doc()?);
      assert_eq!(1_i64, values.long_value()?);
    } else {
      assert_eq!(0, values.next_doc()?);
      assert_eq!(1_i64, values.long_value()?);

      assert_eq!(1, values.next_doc()?);
      assert_eq!(2_i64, values.long_value()?);

      assert_eq!(2, values.next_doc()?);
      assert_eq!(3_i64, values.long_value()?);
    }

    writer.close()?;
  }

  Ok(())
}

#[test]
fn test_missing_float_last() -> Result<()> {
  for reverse in [true, false] {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;

    let analyzer = MockAnalyzer::new(&mut random);
    let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;

    let mut sort_field = SortField::with_reverse(Some("foo"), SortFieldType::Float, reverse)?;
    sort_field.set_missing_value(f32::INFINITY)?;
    let index_sort = Sort::with_fields(vec![sort_field])?;
    iwc.set_index_sort(index_sort)?;

    let writer = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc = Document::new();
    doc.add(FloatDocValuesField::new("foo", 18.0));
    writer.add_document(doc)?;
    writer.commit()?;

    writer.add_document(Document::new())?;
    writer.commit()?;

    let mut doc = Document::new();
    doc.add(FloatDocValuesField::new("foo", 7.0));
    writer.add_document(doc)?;
    writer.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&writer)?;
    let leaf = get_only_leaf_reader(&reader)?;
    assert_eq!(3, leaf.max_doc()?);

    let mut values = leaf.get_numeric_doc_values("foo")?.unwrap();

    if reverse {
      assert_eq!(1, values.next_doc()?);
      assert_eq!(18.0f32, f32::from_bits(values.long_value()? as u32));

      assert_eq!(2, values.next_doc()?);
      assert_eq!(7.0f32, f32::from_bits(values.long_value()? as u32));
    } else {
      assert_eq!(0, values.next_doc()?);
      assert_eq!(7.0f32, f32::from_bits(values.long_value()? as u32));

      assert_eq!(1, values.next_doc()?);
      assert_eq!(18.0f32, f32::from_bits(values.long_value()? as u32));
    }

    assert_eq!(NO_MORE_DOCS, values.next_doc()?);
    writer.close()?;
  }

  Ok(())
}
#[test]
fn test_missing_multi_valued_float_last() -> Result<()> {
  for reverse in [true, false] {
    let mut random = random();
    let dir = new_directory_shared(&mut random)?;

    let analyzer = MockAnalyzer::new(&mut random);
    let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;

    let mut sort_field =
      SortedNumericSortField::with_reverse("foo", SortFieldType::Float, reverse)?;
    sort_field.set_missing_value(f32::INFINITY)?;
    let index_sort = Sort::with_fields(vec![sort_field])?;
    iwc.set_index_sort(index_sort)?;

    let writer = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("id", 2));
    doc.add(SortedNumericDocValuesField::new(
      "foo",
      NumericUtils::float_to_sortable_int(726.0) as i64,
    ));
    doc.add(SortedNumericDocValuesField::new(
      "foo",
      NumericUtils::float_to_sortable_int(18.0) as i64,
    ));
    writer.add_document(doc)?;
    writer.commit()?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("id", 3));
    writer.add_document(doc)?;
    writer.commit()?;

    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("id", 1));
    doc.add(SortedNumericDocValuesField::new(
      "foo",
      NumericUtils::float_to_sortable_int(12.67) as i64,
    ));
    doc.add(SortedNumericDocValuesField::new(
      "foo",
      NumericUtils::float_to_sortable_int(7.0) as i64,
    ));
    writer.add_document(doc)?;
    writer.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&writer)?;
    let leaf = get_only_leaf_reader(&reader)?;
    assert_eq!(3, leaf.max_doc()?);

    let mut values = leaf.get_numeric_doc_values("id")?.unwrap();

    if reverse {
      assert_eq!(0, values.next_doc()?);
      assert_eq!(3_i64, values.long_value()?);

      assert_eq!(1, values.next_doc()?);
      assert_eq!(2_i64, values.long_value()?);

      assert_eq!(2, values.next_doc()?);
      assert_eq!(1_i64, values.long_value()?);
    } else {
      assert_eq!(0, values.next_doc()?);
      assert_eq!(1_i64, values.long_value()?);

      assert_eq!(1, values.next_doc()?);
      assert_eq!(2_i64, values.long_value()?);

      assert_eq!(2, values.next_doc()?);
      assert_eq!(3_i64, values.long_value()?);
    }

    writer.close()?;
  }

  Ok(())
}
#[test]
fn test_random1() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let index_sort = Arc::new(Sort::with_fields(vec![SortField::new(
    Some("foo"),
    SortFieldType::Long,
  )?])?);
  iwc.set_index_sort(index_sort.clone())?;

  let writer = IndexWriter::new(dir.clone(), iwc)?;
  let num_docs = at_least_usize(&mut random, 200);
  let mut deleted = FixedBitSet::new(num_docs);

  for i in 0..num_docs {
    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new(
      "foo",
      random.random_range(0..20),
    ));
    doc.add(StringField::from_string("id", i.to_string(), Store::Yes)?);
    doc.add(NumericDocValuesField::new("id", i as i64));
    writer.add_document(doc)?;

    if random.random_range(0..5) == 0 {
      directory_reader::open_from_writer(&writer)?.close()?;
    } else if random.random_range(0..30) == 0 {
      writer.force_merge(2)?;
    } else if random.random_range(0..4) == 0 {
      let id = TestUtil::next_usize(&mut random, 0, i);
      deleted.set(id);
      writer.delete_documents_with_terms(vec![Term::from_text("id", id.to_string())])?;
    }
  }

  let reader = directory_reader::open_from_writer(&writer)?;
  let irc = (&reader).get_context()?;
  for ctx in irc.leaves()? {
    let leaf = ctx.reader();

    let info = &leaf.get_segment_info().info;
    let source = info.get_diagnostics().get(SOURCE);

    match source {
      Some(src) if src == SOURCE_FLUSH || src == SOURCE_MERGE => {
        assert!(Arc::ptr_eq(&index_sort, &info.get_index_sort().unwrap()));

        let mut values = leaf.get_numeric_doc_values("foo")?.unwrap();
        let mut previous = i64::MIN;

        for doc_id in 0..leaf.max_doc()? {
          assert_eq!(doc_id, values.next_doc()?);
          let value = values.long_value()?;
          assert!(value >= previous);
          previous = value;
        }
      },
      _ => unreachable!("unexpected segment source"),
    }
  }

  let mut stored_fields = reader.stored_fields()?;
  let searcher = new_searcher_with_reader(reader)?;

  for i in 0..num_docs {
    let term_query = TermQuery::new(Term::from_text("id", i.to_string()));
    let top_docs = searcher.search(term_query, 1)?;

    if deleted.get(i)? {
      assert_eq!(0, top_docs.total_hits.value());
    } else {
      assert_eq!(1, top_docs.total_hits.value());

      let mut values =
        MultiDocValues::get_numeric_values(searcher.reader_context.reader(), "id")?.unwrap();
      assert_eq!(
        top_docs.score_docs[0].doc,
        values.advance(top_docs.score_docs[0].doc)?
      );
      assert_eq!(i as i64, values.long_value()?);
      let document = stored_fields.document(top_docs.score_docs[0].doc)?;
      assert_eq!(&i.to_string(), document.get("id")?.unwrap().as_ref());
    }
  }

  searcher.reader_context.reader().close()?;
  writer.close()?;
  Ok(())
}
#[test]
fn test_multi_valued_random1() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let index_sort = Arc::new(Sort::with_fields(vec![SortedNumericSortField::new(
    "foo",
    SortFieldType::Long,
  )?])?);
  iwc.set_index_sort(index_sort.clone())?;

  let writer = IndexWriter::new(dir.clone(), iwc)?;
  let num_docs = at_least_usize(&mut random, 200);
  let mut deleted = FixedBitSet::new(num_docs);

  for i in 0..num_docs {
    let mut doc = Document::new();
    let num = random.random_range(0..10);
    for _ in 0..num {
      doc.add(SortedNumericDocValuesField::new(
        "foo",
        random.random_range(0..2000),
      ));
    }
    doc.add(StringField::from_string("id", i.to_string(), Store::Yes)?);
    doc.add(NumericDocValuesField::new("id", i as i64));
    writer.add_document(doc)?;

    if random.random_range(0..5) == 0 {
      directory_reader::open_from_writer(&writer)?.close()?;
    } else if random.random_range(0..30) == 0 {
      writer.force_merge(2)?;
    } else if random.random_range(0..4) == 0 {
      let id = TestUtil::next_usize(&mut random, 0, i);
      deleted.set(id);
      writer.delete_documents_with_terms(vec![Term::from_text("id", id.to_string())])?;
    }
  }

  let reader = directory_reader::open_from_writer(&writer)?;
  let mut stored_fields = reader.stored_fields()?;
  let searcher = new_searcher_with_reader(reader)?;

  for i in 0..num_docs {
    let term_query = TermQuery::new(Term::from_text("id", i.to_string()));
    let top_docs = searcher.search(term_query, 1)?;

    if deleted.get(i)? {
      assert_eq!(0, top_docs.total_hits.value());
    } else {
      assert_eq!(1, top_docs.total_hits.value());

      let mut values =
        MultiDocValues::get_numeric_values(searcher.reader_context.reader(), "id")?.unwrap();
      assert_eq!(
        top_docs.score_docs[0].doc,
        values.advance(top_docs.score_docs[0].doc)?
      );
      assert_eq!(i as i64, values.long_value()?);
      let document = stored_fields.document(top_docs.score_docs[0].doc)?;
      assert_eq!(&i.to_string(), document.get("id")?.unwrap().as_ref());
    }
  }

  searcher.reader_context.reader().close()?;
  writer.close()?;
  Ok(())
}

#[test]
fn test_concurrent_updates() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let index_sort = Sort::with_fields(vec![SortField::new(Some("foo"), SortFieldType::Long)?])?;
  iwc.set_index_sort(index_sort)?;

  let writer = IndexWriter::new(dir.clone(), iwc)?;
  let values = Arc::new(Mutex::new(HashMap::new()));

  let num_docs = at_least(&mut random, 100) as usize;
  let update_count = AtomicI32::new(at_least(&mut random, 1000));
  let latch = Arc::new(Barrier::new(3));

  thread::scope(|scope| -> Result<()> {
    let mut handles = Vec::new();
    for _ in 0..2 {
      let seed = random.random();
      let mut thread_random = random_from_seed(seed);
      let latch = latch.clone();
      let writer = writer.clone();
      let values = values.clone();
      let update_count = &update_count;
      handles.push(scope.spawn(move || -> Result<()> {
        latch.wait();
        while update_count.fetch_sub(1, Ordering::SeqCst) > 0 {
          let id = thread_random.random_range(0..num_docs);
          let value = thread_random.random_range(0..20);
          let mut doc = Document::new();
          doc.add(StringField::from_string("id", id.to_string(), Store::No)?);
          doc.add(NumericDocValuesField::new("foo", value));

          {
            let mut values = values.lock().expect("values mutex poisoned");
            writer.update_document_with_term(Term::from_text("id", id.to_string()), doc)?;
            values.insert(id, value);
          }

          match thread_random.random_range(0..10) {
            0 | 1 => {
              directory_reader::open_from_writer(&writer)?.close()?;
            },
            2 => {
              writer.force_merge(3)?;
            },
            _ => {},
          }
        }
        Ok(())
      }));
    }

    latch.wait();

    for handle in handles {
      handle
        .join()
        .map_err(|_| LuceneError::illegal_state("update thread panicked"))??;
    }
    Ok(())
  })?;

  writer.force_merge(1)?;
  let reader = directory_reader::open_from_writer(&writer)?;
  let searcher = new_searcher_with_reader(reader)?;
  let values = values.lock().expect("values mutex poisoned");

  for i in 0..num_docs {
    let top_docs = searcher.search(TermQuery::new(Term::from_text("id", i.to_string())), 1)?;
    if let Some(value) = values.get(&i) {
      assert_eq!(1, top_docs.total_hits.value());
      let mut dvs =
        MultiDocValues::get_numeric_values(searcher.reader_context.reader(), "foo")?.unwrap();
      let doc_id = top_docs.score_docs[0].doc;
      assert_eq!(doc_id, dvs.advance(doc_id)?);
      assert_eq!(*value, dvs.long_value()?);
    } else {
      assert_eq!(0, top_docs.total_hits.value());
    }
  }

  searcher.reader_context.reader().close()?;
  writer.close()?;
  Ok(())
}
#[test]
fn test_bad_dv_update() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let index_sort = Sort::with_fields(vec![SortField::new(Some("foo"), SortFieldType::Long)?])?;
  iwc.set_index_sort(index_sort)?;

  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();
  doc.add(StringField::from_bytes_ref(
    "id",
    BytesRef::from_string("0"),
    Store::No,
  )?);
  doc.add(NumericDocValuesField::new("foo", random.random::<i64>()));
  writer.add_document(doc)?;
  writer.commit()?;

  let err = writer
    .update_doc_values(
      Term::from_text("id", "0"),
      vec![NumericDocValuesField::new("foo", -1).into()],
    )
    .unwrap_err();
  match err {
    LuceneError::IllegalArgument(msg) => {
      assert_eq!(
        "cannot update docvalues field involved in the index sort, field=foo, sort=<long: \"foo\">",
        msg.to_string()
      );
    },
    _ => unreachable!("expected IllegalArgument"),
  }

  let err = writer
    .update_numeric_doc_value(Term::from_text("id", "0"), "foo", -1)
    .unwrap_err();
  match err {
    LuceneError::IllegalArgument(msg) => {
      assert_eq!(
        "cannot update docvalues field involved in the index sort, field=foo, sort=<long: \"foo\">",
        msg.to_string()
      );
    },
    _ => unreachable!("expected IllegalArgument"),
  }

  writer.close()?;
  Ok(())
}

#[test]
fn test_concurrent_dv_updates() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let index_sort = Sort::with_fields(vec![SortField::new(Some("foo"), SortFieldType::Long)?])?;
  iwc.set_index_sort(index_sort)?;

  let writer = IndexWriter::new(dir.clone(), iwc)?;
  let values = Arc::new(Mutex::new(HashMap::new()));

  let num_docs = at_least(&mut random, 100) as usize;
  for i in 0..num_docs {
    let mut doc = Document::new();
    doc.add(StringField::from_string("id", i.to_string(), Store::No)?);
    doc.add(NumericDocValuesField::new(
      "foo",
      random.random::<i32>() as i64,
    ));
    doc.add(NumericDocValuesField::new("bar", -1));
    writer.add_document(doc)?;
    values.lock().expect("values mutex poisoned").insert(i, -1);
  }

  let update_count = AtomicI32::new(at_least(&mut random, 1000));
  let latch = Arc::new(Barrier::new(3));

  thread::scope(|scope| -> Result<()> {
    let mut handles = Vec::new();
    for _ in 0..2 {
      let seed = random.random();
      let mut thread_random = random_from_seed(seed);
      let latch = latch.clone();
      let writer = writer.clone();
      let values = values.clone();
      let update_count = &update_count;
      handles.push(scope.spawn(move || -> Result<()> {
        latch.wait();
        while update_count.fetch_sub(1, Ordering::SeqCst) > 0 {
          let id = thread_random.random_range(0..num_docs);
          let value = thread_random.random_range(0..20);

          {
            let mut values = values.lock().expect("values mutex poisoned");
            writer.update_doc_values(
              Term::from_text("id", id.to_string()),
              vec![NumericDocValuesField::new("bar", value).into()],
            )?;
            values.insert(id, value);
          }

          match thread_random.random_range(0..10) {
            0 | 1 => {
              directory_reader::open_from_writer(&writer)?.close()?;
            },
            2 => {
              writer.force_merge(3)?;
            },
            _ => {},
          }
        }
        Ok(())
      }));
    }

    latch.wait();

    for handle in handles {
      handle
        .join()
        .map_err(|_| LuceneError::illegal_state("dv update thread panicked"))??;
    }
    Ok(())
  })?;

  writer.force_merge(1)?;
  let reader = directory_reader::open_from_writer(&writer)?;
  let searcher = new_searcher_with_reader(reader)?;
  let values = values.lock().expect("values mutex poisoned");

  for i in 0..num_docs {
    let top_docs = searcher.search(TermQuery::new(Term::from_text("id", i.to_string())), 1)?;
    assert_eq!(1, top_docs.total_hits.value());
    let mut dvs =
      MultiDocValues::get_numeric_values(searcher.reader_context.reader(), "bar")?.unwrap();
    let hit_doc = top_docs.score_docs[0].doc;
    assert_eq!(hit_doc, dvs.advance(hit_doc)?);
    assert_eq!(*values.get(&i).unwrap(), dvs.long_value()?);
  }

  searcher.reader_context.reader().close()?;
  writer.close()?;
  Ok(())
}
#[test]
fn test_bad_add_indexes() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let index_sort = Sort::with_fields(vec![SortField::new(Some("foo"), SortFieldType::Long)?])?;
  let mut iwc1 = new_index_writer_config(&mut random)?;
  iwc1.set_index_sort(index_sort)?;
  let w = IndexWriter::new(dir.clone(), iwc1)?;
  w.add_document(Document::new())?;

  let index_sorts = vec![
    None,
    Some(Sort::with_fields(vec![SortField::new(
      Some("bar"),
      SortFieldType::Long,
    )?])?),
  ];

  for sort in index_sorts {
    let dir2 = new_directory_shared(&mut random)?;
    let mut iwc2 = new_index_writer_config(&mut random)?;
    if let Some(sort) = sort {
      iwc2.set_index_sort(sort)?;
    }
    let w2 = IndexWriter::new(dir2.clone(), iwc2)?;
    w2.add_document(Document::new())?;
    let reader = directory_reader::open_from_writer(&w2)?;
    w2.close()?;
    drop(w2);

    let err = w.add_indexes_from_directory(std::slice::from_ref(&dir2));
    assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
    assert!(
      err
        .unwrap_err()
        .to_string()
        .contains("cannot change index sort")
    );
    let reader_context = (&reader).get_context()?;
    let leaves = reader_context.leaves()?;
    let mut codec_readers = Vec::with_capacity(leaves.len());
    for leaf in leaves {
      codec_readers.push(leaf.reader().clone());
    }

    let err = w.add_indexes_from_codec_readers(codec_readers);
    assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
    assert!(
      err
        .unwrap_err()
        .to_string()
        .contains("cannot change index sort")
    );

    reader.close()?;
    dir2.close()?;
  }

  w.close()?;
  dir.close()?;
  Ok(())
}
fn do_test_add_indexes<R>(random: &mut R, with_deletes: bool, use_readers: bool) -> Result<()>
where
  R: Rng + ?Sized,
{
  let dir = new_directory_shared(random)?;
  let mut iwc1 = new_index_writer_config(random)?;
  let use_parent = rarely(random);
  if use_parent {
    iwc1.set_parent_field("___parent");
  }
  let index_sort = Sort::with_fields(vec![
    SortField::new(Some("foo"), SortFieldType::Long)?,
    SortField::new(Some("bar"), SortFieldType::Long)?,
  ])?;
  iwc1.set_index_sort(index_sort.clone())?;
  let w = RandomIndexWriter::with_config(random, dir.clone(), iwc1);

  let num_docs = at_least(random, 100);
  for i in 0..num_docs {
    let mut doc = Document::new();
    doc.add(StringField::from_string("id", i.to_string(), Store::No)?);
    doc.add(NumericDocValuesField::new(
      "foo",
      random.random_range(0..20) as i64,
    ));
    doc.add(NumericDocValuesField::new(
      "bar",
      random.random_range(0..20) as i64,
    ));
    w.add_document(random, doc)?;
  }

  if with_deletes {
    let mut i = random.random_range(0..5);
    while i < num_docs {
      w.delete_documents_with_terms(random, vec![Term::from_text("id", i.to_string())])?;
      i += TestUtil::next_int(random, 1, 5);
    }
  }

  if random.random_bool(0.5) {
    w.force_merge(random, 1)?;
  }

  let reader = w.get_reader(random)?;
  w.close(random)?;
  drop(w);

  let dir2 = new_directory_shared(random)?;
  let analyzer = MockAnalyzer::new(random);
  let mut iwc = new_index_writer_config_with_analyzer(random, analyzer)?;
  let use_prefix_sort = random.random_bool(0.5);
  if use_prefix_sort {
    iwc.set_index_sort(Sort::with_fields(vec![SortField::new(
      Some("foo"),
      SortFieldType::Long,
    )?])?)?;
  } else {
    iwc.set_index_sort(index_sort)?;
  }
  if use_parent {
    iwc.set_parent_field("___parent");
  }
  let w2 = IndexWriter::new(dir2.clone(), iwc)?;

  if use_readers {
    let reader_context = (&reader).get_context()?;
    let leaves = reader_context.leaves()?;
    let mut codec_readers = Vec::with_capacity(leaves.len());
    for leaf in leaves {
      codec_readers.push(leaf.reader().clone());
    }
    w2.add_indexes_from_codec_readers(codec_readers)?;
  } else {
    w2.add_indexes_from_directory(std::slice::from_ref(&dir))?;
  }

  let reader2 = directory_reader::open_from_writer(&w2)?;
  let searcher = new_searcher_with_reader(reader)?;
  let searcher2 = new_searcher_with_reader(reader2)?;

  for i in 0..num_docs {
    let query = TermQuery::new(Term::from_text("id", i.to_string()));
    let top_docs = searcher.search(query.clone(), 1)?;
    let top_docs2 = searcher2.search(query, 1)?;
    assert_eq!(top_docs.total_hits.value(), top_docs2.total_hits.value());

    if top_docs.total_hits.value() == 1 {
      let mut dvs1 =
        MultiDocValues::get_numeric_values(searcher.reader_context.reader(), "foo")?.unwrap();
      let hit_doc1 = top_docs.score_docs[0].doc;
      assert_eq!(hit_doc1, dvs1.advance(hit_doc1)?);
      let value1 = dvs1.long_value()?;

      let mut dvs2 =
        MultiDocValues::get_numeric_values(searcher2.reader_context.reader(), "foo")?.unwrap();
      let hit_doc2 = top_docs2.score_docs[0].doc;
      assert_eq!(hit_doc2, dvs2.advance(hit_doc2)?);
      let value2 = dvs2.long_value()?;

      assert_eq!(value1, value2);
    }
  }
  searcher.reader_context.reader().close()?;
  searcher2.reader_context.reader().close()?;
  w2.close()?;
  dir.close()?;
  dir2.close()?;
  Ok(())
}
#[test]
fn test_add_indexes() -> Result<()> {
  let mut random = random();
  do_test_add_indexes(&mut random, false, true)
}
#[test]
fn test_add_indexes_with_deletions() -> Result<()> {
  let mut random = random();
  do_test_add_indexes(&mut random, true, true)
}

#[test]
fn test_add_indexes_with_directory() -> Result<()> {
  let mut random = random();
  do_test_add_indexes(&mut random, false, false)
}

#[test]
fn test_add_indexes_with_deletions_and_directory() -> Result<()> {
  let mut random = random();
  do_test_add_indexes(&mut random, true, false)
}
#[test]
fn test_bad_sort() -> Result<()> {
  let mut random = random();
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::<DirEnum>::with_analyzer(analyzer)?;

  let err = iwc.set_index_sort(Sort::get_relevance()?).err().unwrap();
  match err {
    LuceneError::IllegalArgument(msg) => {
      assert_eq!("Cannot sort index with sort field <score>", msg.to_string());
    },
    _ => unreachable!("expected IllegalArgument"),
  }

  Ok(())
}
#[test]
fn test_illegal_change_sort() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let index_sort = Sort::with_fields(vec![SortField::new(Some("foo"), SortFieldType::Long)?])?;
  iwc.set_index_sort(index_sort)?;
  {
    let writer = IndexWriter::new(dir.clone(), iwc)?;
    writer.add_document(Document::new())?;
    directory_reader::open_from_writer(&writer)?.close()?;
    writer.add_document(Document::new())?;
    writer.force_merge(1)?;
    writer.close()?;
  }

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc2 = IndexWriterConfig::with_analyzer(analyzer)?;
  let index_sort = Sort::with_fields(vec![SortField::new(Some("bar"), SortFieldType::Long)?])?;
  iwc2.set_index_sort(index_sort)?;

  let err = IndexWriter::new(dir.clone(), iwc2).err().unwrap();
  match err {
    LuceneError::IllegalArgument(msg) => {
      let message = msg.to_string();
      assert!(message.contains("cannot change previous indexSort=<long: \"foo\">"));
      assert!(message.contains("to new indexSort=<long: \"bar\">"));
    },
    _ => unreachable!("expected IllegalArgument"),
  }

  Ok(())
}

struct NormsSimilarity {
  in_: SimilarityEnum,
}

impl NormsSimilarity {
  fn new(in_: SimilarityEnum) -> Self {
    Self { in_ }
  }
}

impl Display for NormsSimilarity {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "NormsSimilarity({})", self.in_)
  }
}

impl Similarity for NormsSimilarity {
  fn compute_norm(&self, state: &FieldInvertState) -> Result<i64> {
    if state.get_name() == "norms" {
      Ok(state.get_length() as i64)
    } else {
      self.in_.compute_norm(state)
    }
  }

  type SimScorer = BoxSimScorer;

  fn scorer(
    &self,
    boost: f32,
    collection_stats: &CollectionStatistics,
    term_stats: &[TermStatistics],
  ) -> Result<Self::SimScorer> {
    Ok(Box::new(self.in_.scorer(
      boost,
      collection_stats,
      term_stats,
    )?))
  }
}

struct PositionsTokenStream {
  attrs: Attributes,
  pos: i32,
  off: i32,
}

impl PositionsTokenStream {
  fn new() -> Self {
    Self {
      attrs: Attributes::default(),
      pos: 0,
      off: 0,
    }
  }

  fn with_id(id: i32) -> Self {
    let mut stream = Self::new();
    stream.set_id(id);
    stream
  }

  fn set_id(&mut self, id: i32) {
    self.pos = id / 10 + 1;
    self.off = 0;
  }
}

impl crate::core::util::close::Closeable for PositionsTokenStream {}

impl TokenStream for PositionsTokenStream {
  fn increment_token(&mut self) -> Result<bool> {
    if self.pos == 0 {
      return Ok(false);
    }

    self.attrs.clear_attributes()?;
    self.attrs.append_str(Some("#all#"))?;
    self
      .attrs
      .set_payload(Some(BytesRef::from_string(&self.pos.to_string())))?;
    self.attrs.set_offset(self.off, self.off)?;
    self.pos -= 1;
    self.off += 1;
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

struct TestRandom2Analyzer {
  random: Mutex<StdRng>,
  stored_value: AnalyzerStoredValue,
}

impl TestRandom2Analyzer {
  fn new(seed: u64) -> Self {
    Self {
      random: Mutex::new(StdRng::seed_from_u64(seed)),
      stored_value: AnalyzerStoredValue::new(),
    }
  }

  fn next_random(&self) -> StdRng {
    StdRng::seed_from_u64(self.random.lock().expect("random mutex poisoned").random())
  }
}

impl Analyzer for TestRandom2Analyzer {
  fn create_components(&self, _field: &str) -> Result<TokenStreamComponents> {
    let tokenizer: Box<dyn TokenStream + Send + Sync> =
      Box::new(MockTokenizer::new(self.next_random()));
    Ok(TokenStreamComponents::new(tokenizer, None))
  }

  fn stored_value(&self) -> &AnalyzerStoredValue {
    &self.stored_value
  }
}

crate::impl_analyzer_close!(TestRandom2Analyzer);

#[test]
fn test_random2() -> Result<()> {
  let mut random = random();
  let num_docs = at_least(&mut random, 100);

  let mut positions_type = FieldType::from_ref(&*TYPE_NOT_STORED)?;
  positions_type.set_index_options(IndexOptions::DocsAndFreqsAndPositionsAndOffsets)?;
  positions_type.freeze();

  let mut term_vectors_type = FieldType::from_ref(&*TYPE_NOT_STORED)?;
  term_vectors_type.set_store_term_vectors(true)?;
  term_vectors_type.freeze();

  let mut docs: Vec<i32> = Vec::new();
  for i in 0..num_docs {
    docs.push(i * 10);
  }

  let seed = random.random::<u64>();
  let analyzer_seed = random.random::<u64>();

  let dir1 = new_fs_directory(&mut random, create_temp_dir()?)?;

  let mut random1 = StdRng::seed_from_u64(seed);
  let mut iwc1 = new_index_writer_config_with_analyzer(
    &mut random1,
    Box::new(TestRandom2Analyzer::new(analyzer_seed)) as Box<dyn Analyzer>,
  )?;
  iwc1.set_similarity(SimilarityEnum::custom(NormsSimilarity::new(
    get_default_similarity()?,
  )));
  iwc1.set_merge_policy(new_log_merge_policy(&mut random1)?);
  let w1 = RandomIndexWriter::with_config(&mut random1, dir1.clone(), iwc1);
  #[allow(clippy::explicit_counter_loop)]
  for id in &docs {
    let mut doc = Document::new();
    doc.add(StringField::from_string("id", id.to_string(), Store::Yes)?);
    doc.add(StringField::from_string("docs", "#all#", Store::No)?);
    doc.add(Field::from_token_stream(
      "positions",
      FieldTokenStreamEnum::custom(PositionsTokenStream::with_id(*id)),
      positions_type.clone(),
    )?);
    doc.add(NumericDocValuesField::new("numeric", *id as i64));
    let value = (0..*id)
      .map(|_| id.to_string())
      .collect::<Vec<_>>()
      .join(" ");
    doc.add(TextField::from_string("norms", value, Store::No)?);
    doc.add(BinaryDocValuesField::new(
      "binary",
      BytesRef::from_string(&id.to_string()),
    ));
    doc.add(SortedDocValuesField::new(
      "sorted",
      BytesRef::from_string(&id.to_string()),
    ));
    doc.add(SortedSetDocValuesField::new(
      "multi_valued_string",
      BytesRef::from_string(&id.to_string()),
    ));
    doc.add(SortedSetDocValuesField::new(
      "multi_valued_string",
      BytesRef::from_string(&(*id + 1).to_string()),
    ));
    doc.add(SortedNumericDocValuesField::new(
      "multi_valued_numeric",
      *id as i64,
    ));
    doc.add(SortedNumericDocValuesField::new(
      "multi_valued_numeric",
      (*id + 1) as i64,
    ));
    doc.add(Field::new(
      "term_vectors",
      id.to_string(),
      term_vectors_type.clone(),
    ));
    let mut bytes = vec![0u8; 4];
    NumericUtils::int_to_sortable_bytes(*id, &mut bytes, 0);
    doc.add(BinaryPoint::new("points", vec![bytes])?);
    w1.add_document(&mut random1, doc)?;
  }

  let dir2 = new_fs_directory(&mut random, create_temp_dir()?)?;

  let mut random2 = StdRng::seed_from_u64(seed);
  let mut iwc2 = new_index_writer_config_with_analyzer(
    &mut random2,
    Box::new(TestRandom2Analyzer::new(analyzer_seed)) as Box<dyn Analyzer>,
  )?;
  iwc2.set_similarity(SimilarityEnum::custom(NormsSimilarity::new(
    get_default_similarity()?,
  )));

  let sort = Arc::new(Sort::with_fields(vec![SortField::new(
    Some("numeric"),
    SortFieldType::Int,
  )?])?);
  iwc2.set_index_sort(sort.clone())?;

  docs.shuffle(&mut random);
  let w2 = RandomIndexWriter::with_config(&mut random2, dir2.clone(), iwc2);
  let mut count = 0;
  let commit_at_count = TestUtil::next_int(&mut random, 1, num_docs - 1);
  #[allow(clippy::explicit_counter_loop)]
  for id in &docs {
    if count == commit_at_count {
      w2.commit(&mut random2)?;
    }
    count += 1;

    let mut doc = Document::new();
    doc.add(StringField::from_string("id", id.to_string(), Store::Yes)?);
    doc.add(StringField::from_string("docs", "#all#", Store::No)?);
    doc.add(Field::from_token_stream(
      "positions",
      FieldTokenStreamEnum::custom(PositionsTokenStream::with_id(*id)),
      positions_type.clone(),
    )?);
    doc.add(NumericDocValuesField::new("numeric", *id as i64));
    let value = (0..*id)
      .map(|_| id.to_string())
      .collect::<Vec<_>>()
      .join(" ");
    doc.add(TextField::from_string("norms", value, Store::No)?);
    doc.add(BinaryDocValuesField::new(
      "binary",
      BytesRef::from_string(&id.to_string()),
    ));
    doc.add(SortedDocValuesField::new(
      "sorted",
      BytesRef::from_string(&id.to_string()),
    ));
    doc.add(SortedSetDocValuesField::new(
      "multi_valued_string",
      BytesRef::from_string(&id.to_string()),
    ));
    doc.add(SortedSetDocValuesField::new(
      "multi_valued_string",
      BytesRef::from_string(&(*id + 1).to_string()),
    ));
    doc.add(SortedNumericDocValuesField::new(
      "multi_valued_numeric",
      *id as i64,
    ));
    doc.add(SortedNumericDocValuesField::new(
      "multi_valued_numeric",
      (*id + 1) as i64,
    ));
    doc.add(Field::new(
      "term_vectors",
      id.to_string(),
      term_vectors_type.clone(),
    ));
    let mut bytes = vec![0u8; 4];
    NumericUtils::int_to_sortable_bytes(*id, &mut bytes, 0);
    doc.add(BinaryPoint::new("points", vec![bytes])?);
    w2.add_document(&mut random2, doc)?;
  }
  w2.force_merge(&mut random2, 1)?;

  let r1 = w1.get_reader(&mut random2)?;
  let r2 = w2.get_reader(&mut random2)?;
  let leaf_reader = get_only_leaf_reader(&r2)?;
  assert!(
    leaf_reader
      .get_metadata()?
      .get_sort()
      .as_ref()
      .map(|actual_sort| actual_sort.as_ref())
      == Some(sort.as_ref())
  );
  assert_eq!(r1.max_doc()?, r2.max_doc()?);
  assert_eq!(r1.num_docs()?, r2.num_docs()?);
  let mut stored_fields1 = r1.stored_fields()?;
  let mut stored_fields2 = r2.stored_fields()?;
  for doc_id in 0..r1.max_doc()? {
    let doc1 = stored_fields1.document(doc_id)?;
    let doc2 = stored_fields2.document(doc_id)?;
    assert_eq!(doc1.get("id")?, doc2.get("id")?);
  }
  w1.close(&mut random2)?;
  w2.close(&mut random2)?;
  r1.close()?;
  r2.close()?;
  Ok(())
}

fn random_index_sort_field<R: Rng + ?Sized>(random: &mut R) -> Result<SortFieldEnum> {
  let reversed = random.random::<bool>();
  let sort_field = match random.random_range(0..10) {
    0 => {
      let mut s = SortField::with_reverse(Some("int"), SortFieldType::Int, reversed)?;
      if random.random_bool(0.5) {
        s.set_missing_value(random.random::<i32>())?;
      }
      SortFieldEnum::from(s)
    },
    1 => {
      let mut s =
        SortedNumericSortField::with_reverse("multi_valued_int", SortFieldType::Int, reversed)?;
      if random.random_bool(0.5) {
        s.set_missing_value(random.random::<i32>())?;
      }
      SortFieldEnum::from(s)
    },
    2 => {
      let mut s = SortField::with_reverse(Some("long"), SortFieldType::Long, reversed)?;
      if random.random_bool(0.5) {
        s.set_missing_value(random.random::<i64>())?;
      }
      SortFieldEnum::from(s)
    },
    3 => {
      let mut s =
        SortedNumericSortField::with_reverse("multi_valued_long", SortFieldType::Long, reversed)?;
      if random.random_bool(0.5) {
        s.set_missing_value(random.random::<i64>())?;
      }
      SortFieldEnum::from(s)
    },
    4 => {
      let mut s = SortField::with_reverse(Some("float"), SortFieldType::Float, reversed)?;
      if random.random_bool(0.5) {
        s.set_missing_value(random.random::<f32>())?;
      }
      SortFieldEnum::from(s)
    },
    5 => {
      let mut s =
        SortedNumericSortField::with_reverse("multi_valued_float", SortFieldType::Float, reversed)?;
      if random.random_bool(0.5) {
        s.set_missing_value(random.random::<f32>())?;
      }
      SortFieldEnum::from(s)
    },
    6 => {
      let mut s = SortField::with_reverse(Some("double"), SortFieldType::Double, reversed)?;
      if random.random_bool(0.5) {
        s.set_missing_value(random.random::<f64>())?;
      }
      SortFieldEnum::from(s)
    },
    7 => {
      let mut s = SortedNumericSortField::with_reverse(
        "multi_valued_double",
        SortFieldType::Double,
        reversed,
      )?;
      if random.random_bool(0.5) {
        s.set_missing_value(random.random::<f64>())?;
      }
      SortFieldEnum::from(s)
    },
    8 => {
      let mut s = SortField::with_reverse(Some("bytes"), SortFieldType::String, reversed)?;
      if random.random_bool(0.5) {
        s.set_missing_value(StringLast)?;
      }
      SortFieldEnum::from(s)
    },
    9 => {
      let mut s = SortedSetSortField::new("multi_valued_bytes", reversed)?;
      if random.random_bool(0.5) {
        s.set_missing_value(StringLast)?;
      }
      SortFieldEnum::from(s)
    },
    _ => unreachable!("random_range(0..10) should only produce values in [0, 10)"),
  };
  Ok(sort_field)
}

fn random_sort<R: Rng + ?Sized>(random: &mut R) -> Result<Sort> {
  let num_fields = random.random_range(2..=4);
  let mut sort_fields = Vec::with_capacity(num_fields);
  for _ in 0..(num_fields - 1) {
    sort_fields.push(random_index_sort_field(random)?);
  }

  sort_fields.push(SortField::new(Some("id"), SortFieldType::Int)?.into());
  Sort::with_fields(sort_fields)
}

#[derive(Clone)]
struct RandomDoc {
  id: i32,
  int_value: i32,
  int_values: Vec<i32>,
  long_value: i64,
  long_values: Vec<i64>,
  float_value: f32,
  float_values: Vec<f32>,
  double_value: f64,
  double_values: Vec<f64>,
  bytes_value: String,
  bytes_values: Vec<String>,
}

impl RandomDoc {
  fn new<R: Rng + ?Sized>(random: &mut R, id: i32) -> Self {
    let num_values = random.random_range(0..10);

    let mut int_values = Vec::with_capacity(num_values);
    let mut long_values = Vec::with_capacity(num_values);
    let mut float_values = Vec::with_capacity(num_values);
    let mut double_values = Vec::with_capacity(num_values);
    let mut bytes_values = Vec::with_capacity(num_values);

    for _ in 0..num_values {
      int_values.push(random.random::<i32>());
      long_values.push(random.random::<i64>());
      float_values.push(random.random::<f32>());
      double_values.push(random.random::<f64>());
      bytes_values.push(TestUtil::random_simple_string_range(random, 0, 10));
    }

    Self {
      id,
      int_value: random.random::<i32>(),
      int_values,
      long_value: random.random::<i64>(),
      long_values,
      float_value: random.random::<f32>(),
      float_values,
      double_value: random.random::<f64>(),
      double_values,
      bytes_value: TestUtil::random_simple_string_range(random, 0, 10),
      bytes_values,
    }
  }

  fn into_document<R>(self, random: &mut R) -> Result<Document>
  where
    R: Rng + ?Sized,
  {
    let mut doc = Document::new();
    doc.add(StringField::from_string(
      "id",
      self.id.to_string(),
      Store::Yes,
    )?);
    doc.add(NumericDocValuesField::new("id", self.id as i64));
    doc.add(NumericDocValuesField::new("int", self.int_value as i64));
    doc.add(NumericDocValuesField::new("long", self.long_value));
    doc.add(DoubleDocValuesField::new("double", self.double_value));
    doc.add(FloatDocValuesField::new("float", self.float_value));
    doc.add(SortedDocValuesField::new(
      "bytes",
      new_bytes_ref_from_string(random, &self.bytes_value)?,
    ));

    for value in self.int_values {
      doc.add(SortedNumericDocValuesField::new(
        "multi_valued_int",
        value as i64,
      ));
    }

    for value in self.long_values {
      doc.add(SortedNumericDocValuesField::new("multi_valued_long", value));
    }

    for value in self.float_values {
      doc.add(SortedNumericDocValuesField::new(
        "multi_valued_float",
        NumericUtils::float_to_sortable_int(value) as i64,
      ));
    }

    for value in self.double_values {
      doc.add(SortedNumericDocValuesField::new(
        "multi_valued_double",
        NumericUtils::double_to_sortable_long(value),
      ));
    }

    for value in self.bytes_values {
      doc.add(SortedSetDocValuesField::new(
        "multi_valued_bytes",
        BytesRef::from_string(&value),
      ));
    }
    Ok(doc)
  }
}
#[test]
fn test_random3() -> Result<()> {
  let mut random = random();
  let num_docs = at_least(&mut random, 1000);

  let sort = Arc::new(random_sort(&mut random)?);

  let dir1 = new_fs_directory(&mut random, create_temp_dir()?)?;
  let analyzer1 = MockAnalyzer::new(&mut random);
  let iwc1 = new_index_writer_config_with_analyzer(&mut random, analyzer1)?;
  let w1 = IndexWriter::new(dir1.clone(), iwc1)?;

  let dir2 = new_fs_directory(&mut random, create_temp_dir()?)?;
  let analyzer2 = MockAnalyzer::new(&mut random);
  let mut iwc2 = new_index_writer_config_with_analyzer(&mut random, analyzer2)?;
  iwc2.set_index_sort(sort.clone())?;
  let w2 = IndexWriter::new(dir2.clone(), iwc2)?;

  let mut to_delete = HashSet::new();
  let delete_chance = random.random::<f64>();

  for id in 0..num_docs {
    let random_doc = RandomDoc::new(&mut random, id);
    let doc1 = random_doc.clone().into_document(&mut random)?;
    let doc2 = random_doc.into_document(&mut random)?;

    w1.add_document(doc1)?;
    w2.add_document(doc2)?;

    if random.random::<f64>() < delete_chance {
      to_delete.insert(id);
    }
  }

  for id in to_delete {
    w1.delete_documents_with_terms(vec![Term::from_text("id", id.to_string())])?;
    w2.delete_documents_with_terms(vec![Term::from_text("id", id.to_string())])?;
  }

  let r1 = directory_reader::open_from_writer(&w1)?;
  let s1 = new_searcher_with_reader(r1)?;

  if random.random::<bool>() {
    let max_segment_count = TestUtil::next_int(&mut random, 1, 5);
    w2.force_merge(max_segment_count)?;
  }

  let r2 = directory_reader::open_from_writer(&w2)?;
  let s2 = new_searcher_with_reader(r2)?;

  for _ in 0..100 {
    let num_hits = TestUtil::next_int(&mut random, 1, num_docs) as usize;

    let collector_manager1 =
      TopFieldCollectorManager::new(sort.clone(), num_hits, i32::MAX as usize)?;
    let hits1 = s1.search_with_collector_manager(MatchAllDocsQuery::new(), &collector_manager1)?;

    let collector_manager2 = TopFieldCollectorManager::new(sort.clone(), num_hits, 1)?;
    let hits2 = s2.search_with_collector_manager(MatchAllDocsQuery::new(), &collector_manager2)?;

    assert_eq!(hits2.score_docs().len(), hits1.score_docs().len());

    let mut stored_fields1 = s1.reader_context.reader().stored_fields()?;
    let mut stored_fields2 = s2.reader_context.reader().stored_fields()?;

    for i in 0..hits2.score_docs().len() {
      let hit1 = &hits1.score_docs()[i];
      let hit2 = &hits2.score_docs()[i];

      let doc1 = stored_fields1.document(hit1.doc())?;
      let doc2 = stored_fields2.document(hit2.doc())?;

      assert_eq!(doc1.get("id")?.as_deref(), doc2.get("id")?.as_deref());
      assert_eq!(hit1.fields()?, hit2.fields()?);
    }
  }
  s1.reader_context.reader().close()?;
  s2.reader_context.reader().close()?;
  w1.close()?;
  w2.close()?;
  Ok(())
}
#[test]
fn test_tie_break() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let index_sort = Sort::with_fields(vec![SortField::new(Some("foo"), SortFieldType::String)?])?;
  iwc.set_index_sort(index_sort)?;
  iwc.set_merge_policy(new_log_merge_policy(&mut random)?);

  let writer = IndexWriter::new(dir.clone(), iwc)?;

  for id in 0..1000 {
    let mut doc = Document::new();
    doc.add(StoredField::from_i32("id", id)?);

    let value = if id < 500 { "bar2" } else { "bar1" };
    doc.add(SortedDocValuesField::new(
      "foo",
      BytesRef::from_string(value),
    ));
    writer.add_document(doc)?;

    if id == 500 {
      writer.commit()?;
    }
  }

  writer.force_merge(1)?;

  let reader = directory_reader::open_from_writer(&writer)?;
  let mut stored_fields = reader.stored_fields()?;

  for doc_id in 0..1000 {
    let expected_id = if doc_id < 500 {
      500 + doc_id
    } else {
      doc_id - 500
    };

    let document = stored_fields.document(doc_id)?;
    let field = document.get_field("id");
    assert_eq!(
      expected_id,
      field.unwrap().numeric_value()?.unwrap().to_i32().unwrap()
    );
  }

  writer.close()?;
  Ok(())
}

#[test]
fn test_index_sort_with_sparse_field() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;

  let sort_field = SortField::with_reverse(Some("dense_int"), SortFieldType::Int, true)?;
  let index_sort = Sort::with_fields(vec![sort_field])?;
  iwc.set_index_sort(index_sort)?;
  let mut field_to_type = HashMap::new();
  let writer = IndexWriter::new(dir.clone(), iwc)?;

  for i in 0..128 {
    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("dense_int", i));

    if i < 64 {
      doc.add(NumericDocValuesField::new("sparse_int", i));
      doc.add(BinaryDocValuesField::new(
        "sparse_binary",
        BytesRef::from_string(&i.to_string()),
      ));
      doc.add(new_text_field(
        &mut random,
        "sparse_text",
        "foo",
        Store::No,
        &mut field_to_type,
      )?);
    }

    writer.add_document(doc)?;
  }

  writer.commit()?;
  writer.force_merge(1)?;

  let reader = (directory_reader::open_from_writer(&writer)?).get_context()?;
  let leaves = reader.leaves()?;
  assert_eq!(1, leaves.len());

  let leaf_reader = leaves[0].reader();

  let mut dense_values = leaf_reader.get_numeric_doc_values("dense_int")?.unwrap();
  let mut sparse_values = leaf_reader.get_numeric_doc_values("sparse_int")?.unwrap();
  let mut sparse_binary_values = leaf_reader.get_binary_doc_values("sparse_binary")?.unwrap();
  let mut norms_values = leaf_reader.get_norm_values("sparse_text")?.unwrap();

  for doc_id in 0..128 {
    assert!(dense_values.advance_exact(doc_id)?);
    assert_eq!((127 - doc_id) as i64, dense_values.long_value()?);

    if doc_id >= 64 {
      assert!(dense_values.advance_exact(doc_id)?);
      assert!(sparse_values.advance_exact(doc_id)?);
      assert!(sparse_binary_values.advance_exact(doc_id)?);
      assert!(norms_values.advance_exact(doc_id)?);

      assert_eq!(1_i64, norms_values.long_value()?);
      assert_eq!((127 - doc_id) as i64, sparse_values.long_value()?);
      assert_eq!(
        &BytesRef::from_string(&(127 - doc_id).to_string()),
        sparse_binary_values.binary_value()?.as_ref()
      );
    } else {
      assert!(!sparse_binary_values.advance_exact(doc_id)?);
      assert!(!sparse_values.advance_exact(doc_id)?);
      assert!(!norms_values.advance_exact(doc_id)?);
    }
  }

  writer.close()?;
  Ok(())
}
#[test]
fn test_index_sort_on_sparse_field() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;

  let mut sort_field = SortField::with_reverse(Some("sparse"), SortFieldType::Int, false)?;
  sort_field.set_missing_value(i32::MIN)?;
  let index_sort = Sort::with_fields(vec![sort_field])?;
  iwc.set_index_sort(index_sort)?;

  let writer = IndexWriter::new(dir.clone(), iwc)?;

  for i in 0..128 {
    let mut doc = Document::new();
    if i < 64 {
      doc.add(NumericDocValuesField::new("sparse", i));
    }
    writer.add_document(doc)?;
  }

  writer.commit()?;
  writer.force_merge(1)?;

  let reader = (directory_reader::open_from_writer(&writer)?).get_context()?;
  let leaves = reader.leaves()?;
  assert_eq!(1, leaves.len());

  let leaf_reader = leaves[0].reader();
  let mut sparse_values = leaf_reader.get_numeric_doc_values("sparse")?.unwrap();

  for doc_id in 0..128 {
    if doc_id >= 64 {
      assert!(sparse_values.advance_exact(doc_id)?);
      assert_eq!((doc_id - 64) as i64, sparse_values.long_value()?);
    } else {
      assert!(!sparse_values.advance_exact(doc_id)?);
    }
  }

  writer.close()?;
  Ok(())
}

#[test]
fn test_wrong_sort_field_type() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let dvs: Vec<Fields> = vec![
    SortedDocValuesField::new("field", new_bytes_ref_from_string(&mut random, "")?).into(),
    SortedSetDocValuesField::new("field", new_bytes_ref_from_string(&mut random, "")?).into(),
    NumericDocValuesField::new("field", 42).into(),
    SortedNumericDocValuesField::new("field", 42).into(),
  ];

  let sort_fields: Vec<SortFieldEnum> = vec![
    SortField::new(Some("field"), SortFieldType::String)?.into(),
    SortedSetSortField::new("field", false)?.into(),
    SortField::new(Some("field"), SortFieldType::Int)?.into(),
    SortedNumericSortField::new("field", SortFieldType::Int)?.into(),
  ];

  for (i, sort_field) in sort_fields.iter().enumerate() {
    for (j, dv) in dvs.iter().enumerate() {
      if i == j {
        continue;
      }

      let index_sort = Sort::with_fields(vec![sort_field.clone()])?;
      let analyzer = MockAnalyzer::new(&mut random);
      let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
      iwc.set_index_sort(index_sort)?;
      let writer = IndexWriter::new(dir.clone(), iwc)?;

      let mut doc = Document::new();
      doc.add(dv.clone());
      let err = writer.add_document(doc.clone()).unwrap_err();
      match err {
        LuceneError::IllegalArgument(msg) => {
          assert!(msg.to_string().contains("expected field [field] to be "));
        },
        _ => unreachable!("expected IllegalArgument"),
      }

      doc.clear();
      doc.add(dvs[i].clone());
      writer.add_document(doc.clone())?;
      doc.add(dv.clone());
      let err = writer.add_document(doc).unwrap_err();
      match err {
        LuceneError::IllegalArgument(msg) => {
          assert_eq!(
            format!(
              "Inconsistency of field data structures across documents for field [field] of doc [2]. doc values type: expected '{}', but it has '{}'.",
              dvs[i].field_type().doc_values_type(),
              dv.field_type().doc_values_type()
            ),
            msg.to_string()
          );
        },
        _ => unreachable!("expected IllegalArgument"),
      }

      writer.rollback()?;
      writer.close()?;
    }
  }

  Ok(())
}

#[test]
fn test_delete_by_term_or_query() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mut config = new_index_writer_config(&mut random)?;
  config.set_index_sort(Sort::with_fields(vec![SortField::new(
    Some("numeric"),
    SortFieldType::Long,
  )?])?)?;
  let writer = IndexWriter::new(dir.clone(), config)?;

  let num_docs = random.random_range(5..2005);
  let mut expected_values = vec![0i64; num_docs];

  for (i, item) in expected_values.iter_mut().enumerate().take(num_docs) {
    *item = random.random_range(0..i32::MAX as i64);

    let mut doc = Document::new();
    doc.add(StringField::from_string("id", i.to_string(), Store::Yes)?);
    doc.add(NumericDocValuesField::new("numeric", *item));
    writer.add_document(doc)?;
  }

  let num_deleted = random.random_range(1..(num_docs + 1));
  for _ in 0..num_deleted {
    let id_to_delete = random.random_range(0..num_docs);

    if random.random_bool(0.5) {
      writer.delete_documents_with_queries(vec![
        TermQuery::new(Term::from_text("id", id_to_delete.to_string())).into(),
      ])?;
    } else {
      writer.delete_documents_with_terms(vec![Term::from_text("id", id_to_delete.to_string())])?;
    }

    expected_values[id_to_delete] = -(random.random_range(0..i32::MAX as i64));

    let mut doc = Document::new();
    doc.add(StringField::from_string(
      "id",
      id_to_delete.to_string(),
      Store::Yes,
    )?);
    doc.add(NumericDocValuesField::new(
      "numeric",
      expected_values[id_to_delete],
    ));
    writer.add_document(doc)?;
  }

  let mut doc_count = 0;
  let reader = (directory_reader::open_from_writer(&writer)?).get_context()?;

  for leaf_ctx in reader.leaves()? {
    let leaf = leaf_ctx.reader();
    let live_docs = leaf.get_live_docs()?;
    let mut values = match leaf.get_numeric_doc_values("numeric")? {
      Some(v) => v,
      None => continue,
    };
    let mut stored_fields = leaf.stored_fields()?;

    for id in 0..leaf.max_doc()? {
      if let Some(live_docs) = live_docs.as_ref()
        && !live_docs.get(id as usize)?
      {
        continue;
      }
      if !values.advance_exact(id)? {
        continue;
      }

      let doc = stored_fields.document(id)?;
      let global_id = doc
        .get_field("id")
        .unwrap()
        .string_value()?
        .unwrap()
        .into_owned()
        .parse::<usize>()?;

      assert!(values.advance_exact(id)?);
      assert_eq!(expected_values[global_id], values.long_value()?);
      doc_count += 1;
    }
  }

  assert_eq!(doc_count, num_docs);

  writer.close()?;
  Ok(())
}
#[test]
fn test_sort_docs() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mut iwc = new_index_writer_config(&mut random)?;
  let index_sort = Sort::with_fields(vec![SortField::new(Some("sort"), SortFieldType::Long)?])?;
  iwc.set_index_sort(index_sort)?;
  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("sort", 0));
  doc.add(StringField::from_string("field", "a", Store::No)?);
  writer.add_document(doc)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("sort", 1));
  doc.add(StringField::from_string("field", "b", Store::No)?);
  writer.add_document(doc)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("sort", -1));
  doc.add(StringField::from_string("field", "a", Store::No)?);
  writer.add_document(doc)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("sort", 2));
  doc.add(StringField::from_string("field", "a", Store::No)?);
  writer.add_document(doc)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("sort", 3));
  doc.add(StringField::from_string("field", "b", Store::No)?);
  writer.add_document(doc)?;

  writer.force_merge(1)?;
  let reader = directory_reader::open_from_writer(&writer)?;
  writer.close()?;

  let leaf_reader = get_only_leaf_reader(&reader)?;
  let terms = leaf_reader.terms("field")?.unwrap();
  let mut field_terms = terms.iterator()?;

  assert_eq!(
    BytesRef::from_string("a"),
    field_terms.next()?.unwrap().into_owned()
  );
  let mut postings = field_terms.postings_with_flags(None, ALL as i32)?;
  assert_eq!(0, postings.next_doc()?);
  assert_eq!(1, postings.next_doc()?);
  assert_eq!(3, postings.next_doc()?);
  assert_eq!(NO_MORE_DOCS, postings.next_doc()?);

  assert_eq!(
    BytesRef::from_string("b"),
    field_terms.next()?.unwrap().into_owned()
  );
  postings = field_terms.postings_with_flags(Some(postings), ALL as i32)?;
  assert_eq!(2, postings.next_doc()?);
  assert_eq!(4, postings.next_doc()?);
  assert_eq!(NO_MORE_DOCS, postings.next_doc()?);

  assert!(field_terms.next()?.is_none());

  Ok(())
}

#[test]
fn test_sort_docs_and_freqs() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mut iwc = new_index_writer_config(&mut random)?;
  let index_sort = Sort::with_fields(vec![SortField::new(Some("sort"), SortFieldType::Long)?])?;
  iwc.set_index_sort(index_sort)?;
  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let mut ft = FieldType::new();
  ft.set_index_options(DocsAndFreqs)?;
  ft.set_tokenized(false)?;
  ft.freeze();

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("sort", 0));
  doc.add(Field::new("field", "a", ft.clone()));
  doc.add(Field::new("field", "a", ft.clone()));
  writer.add_document(doc)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("sort", 1));
  doc.add(Field::new("field", "b", ft.clone()));
  writer.add_document(doc)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("sort", -1));
  doc.add(Field::new("field", "a", ft.clone()));
  doc.add(Field::new("field", "a", ft.clone()));
  doc.add(Field::new("field", "a", ft.clone()));
  writer.add_document(doc)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("sort", 2));
  doc.add(Field::new("field", "a", ft.clone()));
  writer.add_document(doc)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("sort", 3));
  doc.add(Field::new("field", "b", ft.clone()));
  doc.add(Field::new("field", "b", ft.clone()));
  doc.add(Field::new("field", "b", ft.clone()));
  writer.add_document(doc)?;

  writer.force_merge(1)?;
  let reader = directory_reader::open_from_writer(&writer)?;
  writer.close()?;

  let leaf_reader = get_only_leaf_reader(&reader)?;
  let terms = leaf_reader.terms("field")?.unwrap();
  let mut field_terms = terms.iterator()?;

  assert_eq!(
    BytesRef::from_string("a"),
    field_terms.next()?.unwrap().into_owned()
  );
  let mut postings = field_terms.postings_with_flags(None, ALL as i32)?;
  assert_eq!(0, postings.next_doc()?);
  assert_eq!(3, postings.freq()?);
  assert_eq!(1, postings.next_doc()?);
  assert_eq!(2, postings.freq()?);
  assert_eq!(3, postings.next_doc()?);
  assert_eq!(1, postings.freq()?);
  assert_eq!(NO_MORE_DOCS, postings.next_doc()?);

  assert_eq!(
    BytesRef::from_string("b"),
    field_terms.next()?.unwrap().into_owned()
  );
  postings = field_terms.postings_with_flags(Some(postings), ALL as i32)?;
  assert_eq!(2, postings.next_doc()?);
  assert_eq!(1, postings.freq()?);
  assert_eq!(4, postings.next_doc()?);
  assert_eq!(3, postings.freq()?);
  assert_eq!(NO_MORE_DOCS, postings.next_doc()?);

  assert!(field_terms.next()?.is_none());

  Ok(())
}

#[test]
fn test_sort_docs_and_freqs_and_positions() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let index_sort = Sort::with_fields(vec![SortField::new(Some("sort"), SortFieldType::Long)?])?;
  iwc.set_index_sort(index_sort)?;
  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let mut ft = FieldType::new();
  ft.set_index_options(IndexOptions::DocsAndFreqsAndPositions)?;
  ft.set_tokenized(true)?;
  ft.freeze();

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("sort", 0));
  doc.add(Field::new("field", "a a b", ft.clone()));
  writer.add_document(doc)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("sort", 1));
  doc.add(Field::new("field", "b", ft.clone()));
  writer.add_document(doc)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("sort", -1));
  doc.add(Field::new("field", "b a b b", ft.clone()));
  writer.add_document(doc)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("sort", 2));
  doc.add(Field::new("field", "a", ft.clone()));
  writer.add_document(doc)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("sort", 3));
  doc.add(Field::new("field", "b b", ft.clone()));
  writer.add_document(doc)?;

  writer.force_merge(1)?;
  let reader = directory_reader::open_from_writer(&writer)?;
  writer.close()?;

  let leaf_reader = get_only_leaf_reader(&reader)?;
  let terms = leaf_reader.terms("field")?.unwrap();
  let mut field_terms = terms.iterator()?;

  assert_eq!(
    BytesRef::from_string("a"),
    field_terms.next()?.unwrap().into_owned()
  );
  let mut postings = field_terms.postings_with_flags(None, ALL as i32)?;
  assert_eq!(0, postings.next_doc()?);
  assert_eq!(1, postings.freq()?);
  assert_eq!(1, postings.next_position()?);

  assert_eq!(1, postings.next_doc()?);
  assert_eq!(2, postings.freq()?);
  assert_eq!(0, postings.next_position()?);
  assert_eq!(1, postings.next_position()?);

  assert_eq!(3, postings.next_doc()?);
  assert_eq!(1, postings.freq()?);
  assert_eq!(0, postings.next_position()?);

  assert_eq!(NO_MORE_DOCS, postings.next_doc()?);

  assert_eq!(
    BytesRef::from_string("b"),
    field_terms.next()?.unwrap().into_owned()
  );
  postings = field_terms.postings_with_flags(Some(postings), ALL as i32)?;
  assert_eq!(0, postings.next_doc()?);
  assert_eq!(3, postings.freq()?);
  assert_eq!(0, postings.next_position()?);
  assert_eq!(2, postings.next_position()?);
  assert_eq!(3, postings.next_position()?);

  assert_eq!(1, postings.next_doc()?);
  assert_eq!(1, postings.freq()?);
  assert_eq!(2, postings.next_position()?);

  assert_eq!(2, postings.next_doc()?);
  assert_eq!(1, postings.freq()?);
  assert_eq!(0, postings.next_position()?);

  assert_eq!(4, postings.next_doc()?);
  assert_eq!(2, postings.freq()?);
  assert_eq!(0, postings.next_position()?);
  assert_eq!(1, postings.next_position()?);

  assert_eq!(NO_MORE_DOCS, postings.next_doc()?);
  assert!(field_terms.next()?.is_none());

  Ok(())
}

#[test]
fn test_sort_docs_and_freqs_and_positions_and_offsets() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let index_sort = Sort::with_fields(vec![SortField::new(Some("sort"), SortFieldType::Long)?])?;
  iwc.set_index_sort(index_sort)?;
  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let mut ft = FieldType::new();
  ft.set_index_options(IndexOptions::DocsAndFreqsAndPositionsAndOffsets)?;
  ft.set_tokenized(true)?;
  ft.freeze();

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("sort", 0));
  doc.add(Field::new("field", "a a b", ft.clone()));
  writer.add_document(doc)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("sort", 1));
  doc.add(Field::new("field", "b", ft.clone()));
  writer.add_document(doc)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("sort", -1));
  doc.add(Field::new("field", "b a b b", ft.clone()));
  writer.add_document(doc)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("sort", 2));
  doc.add(Field::new("field", "a", ft.clone()));
  writer.add_document(doc)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("sort", 3));
  doc.add(Field::new("field", "b b", ft.clone()));
  writer.add_document(doc)?;

  writer.force_merge(1)?;
  let reader = directory_reader::open_from_writer(&writer)?;
  writer.close()?;

  let leaf_reader = get_only_leaf_reader(&reader)?;
  let terms = leaf_reader.terms("field")?.unwrap();
  let mut field_terms = terms.iterator()?;

  assert_eq!(
    BytesRef::from_string("a"),
    field_terms.next()?.unwrap().into_owned()
  );
  let mut postings = field_terms.postings_with_flags(None, ALL as i32)?;
  assert_eq!(0, postings.next_doc()?);
  assert_eq!(1, postings.freq()?);
  assert_eq!(1, postings.next_position()?);
  assert_eq!(2, postings.start_offset()?);
  assert_eq!(3, postings.end_offset()?);

  assert_eq!(1, postings.next_doc()?);
  assert_eq!(2, postings.freq()?);
  assert_eq!(0, postings.next_position()?);
  assert_eq!(0, postings.start_offset()?);
  assert_eq!(1, postings.end_offset()?);
  assert_eq!(1, postings.next_position()?);
  assert_eq!(2, postings.start_offset()?);
  assert_eq!(3, postings.end_offset()?);

  assert_eq!(3, postings.next_doc()?);
  assert_eq!(1, postings.freq()?);
  assert_eq!(0, postings.next_position()?);
  assert_eq!(0, postings.start_offset()?);
  assert_eq!(1, postings.end_offset()?);

  assert_eq!(NO_MORE_DOCS, postings.next_doc()?);

  assert_eq!(
    BytesRef::from_string("b"),
    field_terms.next()?.unwrap().into_owned()
  );
  postings = field_terms.postings_with_flags(Some(postings), ALL as i32)?;
  assert_eq!(0, postings.next_doc()?);
  assert_eq!(3, postings.freq()?);
  assert_eq!(0, postings.next_position()?);
  assert_eq!(0, postings.start_offset()?);
  assert_eq!(1, postings.end_offset()?);
  assert_eq!(2, postings.next_position()?);
  assert_eq!(4, postings.start_offset()?);
  assert_eq!(5, postings.end_offset()?);
  assert_eq!(3, postings.next_position()?);
  assert_eq!(6, postings.start_offset()?);
  assert_eq!(7, postings.end_offset()?);

  assert_eq!(1, postings.next_doc()?);
  assert_eq!(1, postings.freq()?);
  assert_eq!(2, postings.next_position()?);
  assert_eq!(4, postings.start_offset()?);
  assert_eq!(5, postings.end_offset()?);

  assert_eq!(2, postings.next_doc()?);
  assert_eq!(1, postings.freq()?);
  assert_eq!(0, postings.next_position()?);
  assert_eq!(0, postings.start_offset()?);
  assert_eq!(1, postings.end_offset()?);

  assert_eq!(4, postings.next_doc()?);
  assert_eq!(2, postings.freq()?);
  assert_eq!(0, postings.next_position()?);
  assert_eq!(0, postings.start_offset()?);
  assert_eq!(1, postings.end_offset()?);
  assert_eq!(1, postings.next_position()?);
  assert_eq!(2, postings.start_offset()?);
  assert_eq!(3, postings.end_offset()?);

  assert_eq!(NO_MORE_DOCS, postings.next_doc()?);
  assert!(field_terms.next()?.is_none());

  Ok(())
}

#[test]
fn test_parent_field_not_configured() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let index_sort = Sort::with_fields(vec![SortField::new(Some("foo"), SortFieldType::Int)?])?;
  iwc.set_index_sort(index_sort)?;

  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let err = writer
    .add_documents(vec![Document::new(), Document::new()])
    .unwrap_err();

  match err {
    LuceneError::IllegalArgument(msg) => {
      assert_eq!(
        "a parent field must be set in order to use document blocks with index sorting; see IndexWriterConfig#setParentField",
        msg.to_string()
      );
    },
    _ => unreachable!("expected IllegalArgument"),
  }

  writer.close()?;
  Ok(())
}

#[test]
fn test_block_contains_parent_field() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let parent_field = "parent";
  iwc.set_parent_field(parent_field);
  let index_sort = Sort::with_fields(vec![SortField::new(Some("foo"), SortFieldType::Int)?])?;
  iwc.set_index_sort(index_sort)?;

  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let cases = if random.random_bool(0.5) {
    vec![0, 1]
  } else {
    vec![1, 0]
  };
  let latch = Arc::new(Barrier::new(cases.len() + 1));
  thread::scope(|scope| -> Result<()> {
    let mut handles = Vec::new();
    for case in cases {
      let writer = writer.clone();
      let latch = latch.clone();
      handles.push(scope.spawn(move || -> Result<()> {
        latch.wait();
        let err = if case == 0 {
          let mut doc = Document::new();
          doc.add(NumericDocValuesField::new("parent", 0));
          writer
            .add_documents(vec![doc, Document::new()])
            .unwrap_err()
        } else {
          let mut doc = Document::new();
          doc.add(NumericDocValuesField::new("parent", 0));
          writer
            .add_documents(vec![Document::new(), doc])
            .unwrap_err()
        };

        match err {
          LuceneError::IllegalArgument(msg) => {
            assert_eq!(
              "\"parent\" is a reserved field and should not be added to any document",
              msg.to_string()
            );
          },
          _ => unreachable!("expected IllegalArgument"),
        }
        Ok(())
      }));
    }

    latch.wait();

    for handle in handles {
      handle
        .join()
        .map_err(|_| LuceneError::illegal_state("block parent field thread panicked"))??;
    }
    Ok(())
  })?;

  writer.close()?;
  Ok(())
}
#[test]
fn test_index_sort_with_blocks() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;

  let parent_field = "parent";
  let index_sort = Sort::with_fields(vec![SortField::new(Some("foo"), SortFieldType::Int)?])?;
  iwc.set_index_sort(index_sort)?;
  iwc.set_parent_field(parent_field);

  let mut policy = new_log_merge_policy(&mut random)?;
  match policy {
    MergePolicyEnum::LogBytesSize(ref mut p) => {
      if p.get_merge_factor() <= 2 {
        p.set_merge_factor(3)?;
      }
    },
    MergePolicyEnum::LogDoc(ref mut p) => {
      if p.get_merge_factor() <= 2 {
        p.set_merge_factor(3)?;
      }
    },
    _ => unreachable!("expected LogByteSizeMergePolicy or LogDocMergePolicy"),
  }
  iwc.set_merge_policy(policy);

  {
    let writer = IndexWriter::new(dir.clone(), iwc)?;
    let num_docs = random.random_range(50..100);

    for i in 0..num_docs {
      let mut child1 = Document::new();
      child1.add(StringField::from_string("id", i.to_string(), Store::Yes)?);
      child1.add(NumericDocValuesField::new("id", i as i64));
      child1.add(NumericDocValuesField::new("child", 1));
      child1.add(NumericDocValuesField::new(
        "foo",
        random.random::<i32>() as i64,
      ));

      let mut child2 = Document::new();
      child2.add(StringField::from_string("id", i.to_string(), Store::Yes)?);
      child2.add(NumericDocValuesField::new("id", i as i64));
      child2.add(NumericDocValuesField::new("child", 2));
      child2.add(NumericDocValuesField::new(
        "foo",
        random.random::<i32>() as i64,
      ));

      let mut parent = Document::new();
      parent.add(StringField::from_string("id", i.to_string(), Store::Yes)?);
      parent.add(NumericDocValuesField::new("id", i as i64));
      parent.add(NumericDocValuesField::new(
        "foo",
        random.random::<i32>() as i64,
      ));

      writer.add_documents(vec![child1, child2, parent])?;
      if rarely(&mut random) {
        writer.commit()?;
      }
    }

    writer.commit()?;
    if random.random_bool(0.5) {
      writer.force_merge_with_wait(1, true)?;
    }
    writer.close()?;
  }

  let reader = (directory_reader::open(dir.clone())?).get_context()?;
  for ctx in reader.leaves()? {
    let leaf = ctx.reader();
    let mut parent_disi = leaf.get_numeric_doc_values(parent_field)?.unwrap();
    let mut ids = leaf.get_numeric_doc_values("id")?.unwrap();
    let mut children = leaf.get_numeric_doc_values("child")?.unwrap();

    let mut expected_doc_id = 2;
    loop {
      let doc = parent_disi.next_doc()?;
      if doc == NO_MORE_DOCS {
        break;
      }

      assert_eq!(-1_i64, parent_disi.long_value()?);
      assert_eq!(expected_doc_id, doc);

      let id = ids.next_doc()?;
      let child1_id = ids.long_value()?;
      assert_eq!(id, children.next_doc()?);
      let child1 = children.long_value()?;
      assert_eq!(1_i64, child1);

      let id = ids.next_doc()?;
      let child2_id = ids.long_value()?;
      assert_eq!(id, children.next_doc()?);
      let child2 = children.long_value()?;
      assert_eq!(2_i64, child2);

      let id_parent = ids.next_doc()?;
      assert_eq!(id + 1, id_parent);
      let parent = ids.long_value()?;
      assert_eq!(child1_id, parent);
      assert_eq!(child2_id, parent);

      expected_doc_id += 3;
    }
  }

  Ok(())
}

#[test]
fn test_mix_random_documents_with_blocks() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let parent_field = "parent";
  let index_sort = Sort::with_fields(vec![SortField::new(Some("foo"), SortFieldType::Int)?])?;
  iwc.set_index_sort(index_sort)?;
  iwc.set_parent_field(parent_field);

  let random_index_writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);
  let num_docs = random.random_range(100..1000);
  for i in 0..num_docs {
    if rarely(&mut random) {
      let id_to_delete = random.random_range(0..=i).to_string();
      random_index_writer
        .delete_documents_with_terms(&mut random, vec![Term::from_text("id", id_to_delete)])?;
    }

    let mut docs = Vec::new();
    let case = random.random_range(0..100) % 5;
    if case >= 4 {
      let mut child3 = Document::new();
      child3.add(StringField::from_string("id", i.to_string(), Store::Yes)?);
      child3.add(NumericDocValuesField::new("type", 2));
      child3.add(NumericDocValuesField::new("child_ord", 3));
      child3.add(NumericDocValuesField::new(
        "foo",
        random.random::<i32>() as i64,
      ));
      docs.push(child3);
    }
    if case >= 3 {
      let mut child2 = Document::new();
      child2.add(StringField::from_string("id", i.to_string(), Store::Yes)?);
      child2.add(NumericDocValuesField::new("type", 2));
      child2.add(NumericDocValuesField::new("child_ord", 2));
      child2.add(NumericDocValuesField::new(
        "foo",
        random.random::<i32>() as i64,
      ));
      docs.push(child2);
    }
    if case >= 2 {
      let mut child1 = Document::new();
      child1.add(StringField::from_string("id", i.to_string(), Store::Yes)?);
      child1.add(NumericDocValuesField::new("type", 2));
      child1.add(NumericDocValuesField::new("child_ord", 1));
      child1.add(NumericDocValuesField::new(
        "foo",
        random.random::<i32>() as i64,
      ));
      docs.push(child1);
    }
    if case >= 1 {
      let mut root = Document::new();
      root.add(StringField::from_string("id", i.to_string(), Store::Yes)?);
      root.add(NumericDocValuesField::new("type", 1));
      root.add(NumericDocValuesField::new(
        "num_children",
        docs.len() as i64,
      ));
      root.add(NumericDocValuesField::new(
        "foo",
        random.random::<i32>() as i64,
      ));
      docs.push(root);
      random_index_writer.w.add_documents(docs)?;
    } else {
      let mut single = Document::new();
      single.add(StringField::from_string("id", i.to_string(), Store::Yes)?);
      single.add(NumericDocValuesField::new("type", 0));
      single.add(NumericDocValuesField::new(
        "foo",
        random.random::<i32>() as i64,
      ));
      random_index_writer.add_document(&mut random, single)?;
    }

    if rarely(&mut random) {
      random_index_writer.force_merge(&mut random, 1)?;
    }
    random_index_writer.commit(&mut random)?;
  }

  random_index_writer.close(&mut random)?;
  let reader = (directory_reader::open(dir.clone())?).get_context()?;
  for ctx in reader.leaves()? {
    let leaf = ctx.reader();
    let mut parent_disi = leaf.get_numeric_doc_values(parent_field)?.unwrap();
    let mut type_values = leaf.get_numeric_doc_values("type")?.unwrap();
    let mut child_ord = leaf.get_numeric_doc_values("child_ord")?;
    let mut num_children = leaf.get_numeric_doc_values("num_children")?;
    let live_docs = leaf.get_live_docs()?;
    let mut stored_fields = leaf.stored_fields()?;

    let mut num_current_children = 0;
    let mut total_pending_children = 0_i64;
    let mut child_id: Option<String> = None;
    for i in 0..leaf.max_doc()? {
      if let Some(live_docs) = live_docs.as_ref()
        && !live_docs.get(i as usize)?
      {
        continue;
      }

      assert!(type_values.advance_exact(i)?);
      let type_value = type_values.long_value()? as i32;
      match type_value {
        2 => {
          assert!(!parent_disi.advance_exact(i)?);
          let child_ord = child_ord.as_mut().unwrap();
          assert!(child_ord.advance_exact(i)?);
          if num_current_children == 0 {
            let doc = stored_fields.document(i)?;
            child_id = Some(doc.get("id")?.unwrap().into_owned());
            total_pending_children = child_ord.long_value()? - 1;
          } else {
            assert!(child_id.is_some());
            assert_eq!(total_pending_children, child_ord.long_value()?);
            total_pending_children -= 1;
            let doc = stored_fields.document(i)?;
            assert_eq!(child_id.as_ref().unwrap(), doc.get("id")?.unwrap().as_ref());
          }
          num_current_children += 1;
        },
        1 => {
          assert!(parent_disi.advance_exact(i)?);
          assert_eq!(-1_i64, parent_disi.long_value()?);
          if let Some(child_ord) = child_ord.as_mut() {
            assert!(!child_ord.advance_exact(i)?);
          }
          let num_children = num_children.as_mut().unwrap();
          assert!(num_children.advance_exact(i)?);
          assert_eq!(0_i64, total_pending_children);
          assert_eq!(num_current_children as i64, num_children.long_value()?);
          if num_current_children > 0 {
            let doc = stored_fields.document(i)?;
            assert_eq!(child_id.as_ref().unwrap(), doc.get("id")?.unwrap().as_ref());
          } else {
            assert!(child_id.is_none());
          }
          num_current_children = 0;
          child_id = None;
        },
        0 => {
          assert!(parent_disi.advance_exact(i)?);
          assert_eq!(-1_i64, parent_disi.long_value()?);
          if let Some(child_ord) = child_ord.as_mut() {
            assert!(!child_ord.advance_exact(i)?);
          }
          if let Some(num_children) = num_children.as_mut() {
            assert!(!num_children.advance_exact(i)?);
          }
        },
        _ => panic!("unexpected type value {type_value}"),
      }
    }
  }

  Ok(())
}
