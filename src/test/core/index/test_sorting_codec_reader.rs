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
use crate::core::codecs::doc_values_producer::DocValuesProducer;
use crate::core::codecs::knn_field_vectors_writer::VectorValueEnum;
use crate::core::codecs::term_vectors_reader::TermVectorsReader;
use crate::core::document::binary_doc_values_field::BinaryDocValuesField;
use crate::core::document::document::Document;
use crate::core::document::field::{Field, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::knn_float_vector_field::KnnFloatVectorField;
use crate::core::document::long_point::LongPoint;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::sorted_doc_values_field::SortedDocValuesField;
use crate::core::document::sorted_numeric_doc_values_field::SortedNumericDocValuesField;
use crate::core::document::sorted_set_doc_values_field::SortedSetDocValuesField;
use crate::core::document::string_field::StringField;
use crate::core::document::text_field::TextField;
use crate::core::index::BytesRef;
use crate::core::index::binary_doc_values::BinaryDocValues;
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::directory_reader;
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::float_vector_values::FloatVectorValues;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::knn_vector_values::{DocIndexIterator, KnnVectorValues};
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::sorted_doc_values::SortedDocValues;
use crate::core::index::sorted_numeric_doc_values::SortedNumericDocValues;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, get_only_leaf_reader, new_directory_shared, new_index_writer_config,
  new_index_writer_config_with_analyzer, random, rarely,
};

use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::slow_codec_reader_wrapper::SlowCodecReaderWrapper;
use crate::core::index::sorted_set_doc_values::SortedSetDocValues;
use crate::core::index::sorting_codec_reader::{SortingCodecReaderEnum, wrap};
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term::Term;
use crate::core::index::term_vectors::TermVectors;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::sort::Sort;
use crate::core::search::sort_field::{SortField, SortFieldType};
use crate::core::search::sort_field_enum::SortFieldEnum;
use crate::core::search::sorted_numeric_sort_field::SortedNumericSortField;
use crate::core::search::sorted_set_selector::SortedSetSelectorType::Min;
use crate::core::search::sorted_set_sort_field::SortedSetSortField;
use crate::core::search::term_query::TermQuery;
use crate::core::util::clone::TryClone;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::util::test_util::TestUtil;
use rand::RngExt;
use rand::seq::{IndexedRandom, SliceRandom};
#[allow(dead_code)] // for quick search
struct TestSortingCodecReader;
#[test]
fn test_sort_on_add_indices_ord() -> Result<()> {
  let mut random = random();
  let tmp_dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(mock)?;
  let w = IndexWriter::new(tmp_dir.clone(), iwc)?;

  let mut doc = Document::new();
  doc.add(SortedSetDocValuesField::new(
    "foo",
    BytesRef::from_string("b"),
  ));
  w.add_document(doc.clone())?;

  doc.add(SortedSetDocValuesField::new(
    "foo",
    BytesRef::from_string("a"),
  ));
  doc.add(SortedSetDocValuesField::new(
    "foo",
    BytesRef::from_string("b"),
  ));
  doc.add(SortedSetDocValuesField::new(
    "foo",
    BytesRef::from_string("b"),
  ));
  w.add_document(doc)?;

  w.commit()?;

  let index_sort = Sort::with_fields(vec![SortedSetSortField::with_selector("foo", false, Min)?])?;

  let reader = directory_reader::open(tmp_dir.clone())?;
  let reader = reader.get_context()?;
  for ctx in reader.leaves()? {
    let leaf_reader = ctx.reader().clone();
    let slow = SlowCodecReaderWrapper::wrap_leaf_reader(leaf_reader);
    let wrap = wrap(slow, index_sort.clone())?;

    let s = wrap.to_string();
    assert!(s.starts_with("SortingCodecReader("), "{}", s);
    match wrap {
      SortingCodecReaderEnum::Sorting(sorting_codec_reader) => {
        let fi = ctx
          .reader()
          .get_field_infos()?
          .field_info_by_name("foo")
          .expect("field foo must exist");

        let mut sorted_set_doc_values = sorting_codec_reader
          .get_doc_values_reader()?
          .expect("doc values reader must exist")
          .get_sorted_set(&fi)?;

        sorted_set_doc_values.next_doc()?;
        assert_eq!(sorted_set_doc_values.doc_value_count()?, 2);

        sorted_set_doc_values.next_doc()?;
        assert_eq!(sorted_set_doc_values.doc_value_count()?, 1);

        assert_eq!(sorted_set_doc_values.next_doc()?, NO_MORE_DOCS);
      },
      _ => unreachable!("wrap should be SortingCodecReader"),
    }
  }
  Ok(())
}

#[test]
fn test_sort_on_add_indices_int() -> Result<()> {
  let mut random = random();
  let tmp_dir = new_directory_shared(&mut random)?;
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let iwc = IndexWriterConfig::with_analyzer(mock)?;
  let w = IndexWriter::new(tmp_dir.clone(), iwc)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("foo", 18));
  w.add_document(doc)?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("foo", -1));
  w.add_document(doc)?;
  w.commit()?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("foo", 7));
  w.add_document(doc)?;
  w.commit()?;
  w.close()?;

  let index_sort = Sort::with_fields(vec![SortField::new(Some("foo"), SortFieldType::Int)?])?;
  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(mock)?;
  iwc.set_index_sort(index_sort.clone())?;
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let reader = directory_reader::open(tmp_dir.clone())?;
  let context = (&reader).get_context()?;
  let mut readers = Vec::with_capacity(context.leaves()?.len());
  for ctx in context.leaves()? {
    let leaf_reader = ctx.reader().clone();
    let slow = SlowCodecReaderWrapper::wrap_leaf_reader(leaf_reader);
    let reader = wrap(slow, index_sort.clone())?;
    assert!(reader.to_string().starts_with("SortingCodecReader("));
    readers.push(reader);
  }
  w.add_indexes_from_codec_readers(readers)?;
  reader.close()?;

  let r = directory_reader::open_from_writer(&w)?;
  let leaf = get_only_leaf_reader(&r)?;
  assert_eq!(3, leaf.max_doc()?);
  let mut values =
    LeafReader::get_numeric_doc_values(&leaf, "foo")?.expect("numeric doc values must exist");
  assert_eq!(0, values.next_doc()?);
  assert_eq!(-1, values.long_value()?);
  assert_eq!(1, values.next_doc()?);
  assert_eq!(7, values.long_value()?);
  assert_eq!(2, values.next_doc()?);
  assert_eq!(18, values.long_value()?);
  assert!(leaf.get_metadata()?.get_sort().is_some());

  r.close()?;
  w.close()?;
  dir.close()?;
  tmp_dir.close()?;
  Ok(())
}

#[test]
fn test_sort_on_add_indices_random() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let num_docs = at_least(&mut random, 200);
  let mut doc_ids: Vec<_> = (0..num_docs).collect();
  doc_ids.shuffle(&mut random);

  let iw = RandomIndexWriter::new(&mut random, dir.clone())?;
  for i in 0..num_docs {
    let doc_id = doc_ids[i as usize];
    let mut doc = Document::new();
    doc.add(StringField::from_string(
      "string_id",
      doc_id.to_string(),
      Store::Yes,
    )?);
    doc.add(LongPoint::new("point_id", [doc_id as i64])?);
    let s = TestUtil::random_realistic_unicode_string_range(&mut random, 25, 25);
    doc.add(TextField::from_string("text_field", &s, Store::Yes)?);
    doc.add(BinaryDocValuesField::new(
      "text_field",
      BytesRef::from_string(&s),
    ));
    doc.add(TextField::from_string(
      "another_text_field",
      &s,
      Store::Yes,
    )?);
    doc.add(BinaryDocValuesField::new(
      "another_text_field",
      BytesRef::from_string(&s),
    ));
    doc.add(SortedNumericDocValuesField::new(
      "sorted_numeric_dv",
      doc_id as i64,
    ));
    doc.add(SortedDocValuesField::new(
      "binary_sorted_dv",
      BytesRef::from_string(&doc_id.to_string()),
    ));
    doc.add(BinaryDocValuesField::new(
      "binary_dv",
      BytesRef::from_string(&doc_id.to_string()),
    ));
    doc.add(SortedSetDocValuesField::new(
      "sorted_set_dv",
      BytesRef::from_string(&doc_id.to_string()),
    ));
    doc.add(KnnFloatVectorField::new("vector", vec![doc_id as f32])?);
    doc.add(NumericDocValuesField::new(
      "foo",
      random.random_range(0..20),
    ));

    let mut ft = FieldType::from_ref(&*crate::core::document::string_field::TYPE_NOT_STORED)?;
    ft.set_store_term_vectors(true)?;
    doc.add(Field::new("term_vectors", format!("test{doc_id}"), ft));
    if !rarely(&mut random) {
      doc.add(NumericDocValuesField::new("id", doc_id as i64));
      doc.add(SortedSetDocValuesField::new(
        "sorted_set_sort_field",
        BytesRef::from_string(&format!("{doc_id:06}")),
      ));
      doc.add(SortedDocValuesField::new(
        "sorted_binary_sort_field",
        BytesRef::from_string(&format!("{doc_id:06}")),
      ));
      doc.add(SortedNumericDocValuesField::new(
        "sorted_numeric_sort_field",
        doc_id as i64,
      ));
    } else {
      doc.add(NumericDocValuesField::new("alt_id", doc_id as i64));
    }
    iw.add_document(&mut random, doc)?;
    if i > 0 && random.random_range(0..5) == 0 {
      let id = *doc_ids[..i as usize]
        .choose(&mut random)
        .expect("there must be an earlier document");
      iw.delete_documents_with_terms(
        &mut random,
        vec![Term::from_text("string_id", id.to_string())],
      )?;
    }
  }
  iw.commit(&mut random)?;
  let actual_num_docs = iw.get_doc_stats()?.num_docs;
  iw.close(&mut random)?;

  let index_sorts = [
    Sort::with_fields(vec![
      SortFieldEnum::from(SortField::new(Some("id"), SortFieldType::Int)?),
      SortFieldEnum::from(SortField::new(Some("alt_id"), SortFieldType::Int)?),
    ])?,
    Sort::with_fields(vec![
      SortFieldEnum::from(SortedSetSortField::new("sorted_set_sort_field", false)?),
      SortFieldEnum::from(SortField::new(Some("alt_id"), SortFieldType::Int)?),
    ])?,
    Sort::with_fields(vec![
      SortFieldEnum::from(SortedNumericSortField::new(
        "sorted_numeric_sort_field",
        SortFieldType::Int,
      )?),
      SortFieldEnum::from(SortField::new(Some("alt_id"), SortFieldType::Int)?),
    ])?,
    Sort::with_fields(vec![
      SortFieldEnum::from(SortField::with_reverse(
        Some("sorted_binary_sort_field"),
        SortFieldType::String,
        false,
      )?),
      SortFieldEnum::from(SortField::new(Some("alt_id"), SortFieldType::Int)?),
    ])?,
  ];
  let index_sort = index_sorts
    .choose(&mut random)
    .expect("index sort list must not be empty")
    .clone();

  let sort_dir = new_directory_shared(&mut random)?;
  let mut iwc = new_index_writer_config(&mut random)?;
  iwc.set_index_sort(index_sort.clone())?;
  let writer = IndexWriter::new(sort_dir.clone(), iwc)?;
  let reader = directory_reader::open(dir.clone())?;
  let context = (&reader).get_context()?;
  let mut readers = Vec::with_capacity(context.leaves()?.len());
  for ctx in context.leaves()? {
    let slow = SlowCodecReaderWrapper::wrap_leaf_reader(ctx.reader().clone());
    let wrap = wrap(slow, index_sort.clone())?;
    let term_vectors_reader = wrap
      .get_term_vectors_reader()?
      .expect("term vectors reader must exist");
    let clone = term_vectors_reader.try_clone()?;
    clone.close()?;
    readers.push(wrap);
  }
  writer.add_indexes_from_codec_readers(readers)?;
  reader.close()?;

  assert!(actual_num_docs > 0, "must have at least one doc");
  let r = directory_reader::open_from_writer(&writer)?;
  let leaf = get_only_leaf_reader(&r)?;
  assert_eq!(actual_num_docs, leaf.max_doc()?);
  let mut binary_dv =
    LeafReader::get_binary_doc_values(&leaf, "binary_dv")?.expect("binary_dv must exist");
  let mut sorted_numeric_dv =
    LeafReader::get_sorted_numeric_doc_values(&leaf, "sorted_numeric_dv")?
      .expect("sorted_numeric_dv must exist");
  let mut sorted_set_dv = LeafReader::get_sorted_set_doc_values(&leaf, "sorted_set_dv")?
    .expect("sorted_set_dv must exist");
  let mut binary_sorted_dv = LeafReader::get_sorted_doc_values(&leaf, "binary_sorted_dv")?
    .expect("binary_sorted_dv must exist");
  let mut vector_values =
    LeafReader::get_float_vector_values(&leaf, "vector")?.expect("vector values must exist");
  let mut ids = LeafReader::get_numeric_doc_values(&leaf, "id")?.expect("id values must exist");
  let mut prev_value = -1;
  let mut using_alt_ids = false;
  let mut values_iterator = vector_values.iterator()?;
  let searcher = IndexSearcher::new(r.get_context()?)?;
  let mut term_vectors = IndexReader::term_vectors(&leaf)?;
  let mut stored_fields = IndexReader::stored_fields(&leaf)?;

  for _ in 0..actual_num_docs {
    let mut id_next = ids.next_doc()?;
    if id_next == NO_MORE_DOCS {
      assert!(!using_alt_ids);
      using_alt_ids = true;
      ids = LeafReader::get_numeric_doc_values(&leaf, "alt_id")?.expect("alt_id values must exist");
      id_next = ids.next_doc()?;
      binary_dv =
        LeafReader::get_binary_doc_values(&leaf, "binary_dv")?.expect("binary_dv must exist");
      sorted_numeric_dv = LeafReader::get_sorted_numeric_doc_values(&leaf, "sorted_numeric_dv")?
        .expect("sorted_numeric_dv must exist");
      sorted_set_dv = LeafReader::get_sorted_set_doc_values(&leaf, "sorted_set_dv")?
        .expect("sorted_set_dv must exist");
      binary_sorted_dv = LeafReader::get_sorted_doc_values(&leaf, "binary_sorted_dv")?
        .expect("binary_sorted_dv must exist");
      vector_values =
        LeafReader::get_float_vector_values(&leaf, "vector")?.expect("vector values must exist");
      values_iterator = vector_values.iterator()?;
      prev_value = -1;
    }
    assert!(prev_value < ids.long_value()?);
    prev_value = ids.long_value()?;
    assert!(binary_dv.advance_exact(id_next)?);
    assert!(sorted_numeric_dv.advance_exact(id_next)?);
    assert!(sorted_set_dv.advance_exact(id_next)?);
    assert!(binary_sorted_dv.advance_exact(id_next)?);
    assert_eq!(id_next, values_iterator.advance(id_next)?);
    let expected = BytesRef::from_string(&ids.long_value()?.to_string());
    assert_eq!(&expected, binary_dv.binary_value()?.as_ref());
    let ord = binary_sorted_dv.ord_value()?;
    assert_eq!(&expected, binary_sorted_dv.lookup_ord(ord)?.as_ref());
    let ord = sorted_set_dv.next_ord()?;
    assert_eq!(&expected, sorted_set_dv.lookup_ord(ord)?.as_ref());
    assert_eq!(1, sorted_set_dv.doc_value_count()?);
    assert_eq!(1, sorted_numeric_dv.doc_value_count()?);
    assert_eq!(ids.long_value()?, sorted_numeric_dv.next_value()?);

    let vector_value = vector_values.vector_value(values_iterator.index()? as usize)?;
    match vector_value.as_ref() {
      VectorValueEnum::Float(vector) => {
        assert_eq!(1, vector.len());
        assert!((vector[0] - ids.long_value()? as f32).abs() < 0.001);
      },
      _ => unreachable!("float vector field must return float vectors"),
    }

    let terms = term_vectors
      .get_field_terms(id_next, "term_vectors")?
      .expect("term vectors must exist");
    let mut terms_enum = terms.iterator()?;
    assert!(terms_enum.seek_exact(&BytesRef::from_string(&format!(
      "test{}",
      ids.long_value()?
    )))?);
    assert_eq!(
      ids.long_value()?.to_string(),
      stored_fields
        .document(id_next)?
        .get("string_id")?
        .expect("string_id must be stored")
        .as_str()
    );

    let result = searcher.search(
      LongPoint::new_exact_query("point_id", ids.long_value()?)?,
      1,
    )?;
    assert_eq!(1, result.total_hits.value);
    assert_eq!(id_next, result.score_docs[0].doc);
    let result = searcher.search(
      TermQuery::new(Term::from_text("string_id", ids.long_value()?.to_string())),
      1,
    )?;
    assert_eq!(1, result.total_hits.value);
    assert_eq!(id_next, result.score_docs[0].doc);
  }
  assert_eq!(NO_MORE_DOCS, ids.next_doc()?);

  searcher.reader_context.reader().close()?;
  writer.close()?;
  sort_dir.close()?;
  dir.close()?;
  Ok(())
}
