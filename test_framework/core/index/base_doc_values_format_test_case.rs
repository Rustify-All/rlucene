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
use crate::core::document::binary_doc_values_field::BinaryDocValuesField;
use crate::core::document::document::Document;
use crate::core::document::field::Store;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::sorted_doc_values_field::SortedDocValuesField;
use crate::core::document::sorted_numeric_doc_values_field::SortedNumericDocValuesField;
use crate::core::document::sorted_set_doc_values_field::SortedSetDocValuesField;
use crate::core::index::BytesRef;
use crate::core::index::binary_doc_values::BinaryDocValues;
use crate::core::index::check_index::CheckIndex;
use crate::core::index::doc_values_skipper::DocValuesSkipper;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::serial_merge_scheduler::SerialMergeScheduler;
use crate::core::index::sorted_doc_values::SortedDocValues;
use crate::core::index::sorted_numeric_doc_values::SortedNumericDocValues;
use crate::core::index::sorted_set_doc_values::SortedSetDocValues;
use crate::core::index::term::Term;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::store::directory::DirectoryEnum;
use crate::core::store::lock::LockEnum;
use crate::core::util::IOUtils;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::legacy_base_doc_values_format_test_case::LegacyBaseDocValuesFormatTestCase;
use crate::test_framework::core::index::mismatched_codec_reader::MismatchedCodecReader;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, get_only_leaf_reader, new_bytes_ref_from_string, new_directory_shared,
  new_index_writer_config, new_index_writer_config_with_analyzer, new_log_merge_policy,
  new_string_field, rarely,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::Rng;
use rand::RngExt;
use std::collections::HashMap;
use std::io::Sink;

pub trait BaseDocValuesFormatTestCase: LegacyBaseDocValuesFormatTestCase {
  /// Return `false` if the [`DocValuesSkipper`] produced by this format
  /// sometimes returns documents in [`DocValuesSkipper::min_doc_id`] or
  /// [`DocValuesSkipper::max_doc_id`] that may not have a value.
  fn skipper_has_accurate_doc_bounds(&self) -> bool {
    true
  }
  /// Return `false` if the [`DocValuesSkipper`] produced by this format
  /// sometimes returns values in [`DocValuesSkipper::min_value`] or
  /// [`DocValuesSkipper::max_value`] that none of the documents in the range have.
  fn skipper_has_accurate_value_bounds(&self) -> bool {
    true
  }

  fn test_sorted_merge_away_all_values_with_skipper<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwconfig = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwconfig.set_merge_policy(new_log_merge_policy(random)?);
    let iwriter = RandomIndexWriter::with_config(random, directory, iwconfig);
    let mut field_to_type = HashMap::new();

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "0",
      Store::No,
      &mut field_to_type,
    )?);
    iwriter.add_document(random, doc)?;

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "1",
      Store::No,
      &mut field_to_type,
    )?);
    doc.add(SortedDocValuesField::indexed_field(
      "field",
      new_bytes_ref_from_string(random, "hello")?,
    ));
    iwriter.add_document(random, doc)?;
    iwriter.commit(random)?;
    iwriter.delete_documents_with_terms(random, vec![Term::from_text("id", "1")])?;
    iwriter.force_merge(random, 1)?;

    let ireader = iwriter.get_reader(random)?;
    iwriter.close(random)?;

    let leaf = get_only_leaf_reader(&ireader)?;
    let mut dv = leaf.get_sorted_doc_values("field")?.unwrap();
    assert_eq!(NO_MORE_DOCS, dv.next_doc()?);

    let mut skipper = leaf.get_doc_values_skipper("field")?.unwrap();
    assert_eq!(0, skipper.doc_count());
    skipper.advance(0)?;
    assert_eq!(NO_MORE_DOCS, skipper.min_doc_id_with_level(0));

    let mut terms_enum = dv.terms_enum()?;
    let lucene = new_bytes_ref_from_string(random, "lucene")?;
    assert!(!terms_enum.seek_exact(&lucene)?);
    assert_eq!(SeekStatus::End, terms_enum.seek_ceil(&lucene)?);
    assert_eq!(-1, dv.lookup_term(&lucene)?);

    ireader.close()?;
    Ok(())
  }

  fn test_sorted_set_merge_away_all_values_with_skipper<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwconfig = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwconfig.set_merge_policy(new_log_merge_policy(random)?);
    let iwriter = RandomIndexWriter::with_config(random, directory, iwconfig);
    let mut field_to_type = HashMap::new();

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "0",
      Store::No,
      &mut field_to_type,
    )?);
    iwriter.add_document(random, doc)?;

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "1",
      Store::No,
      &mut field_to_type,
    )?);
    doc.add(SortedSetDocValuesField::indexed_field(
      "field",
      new_bytes_ref_from_string(random, "hello")?,
    ));
    iwriter.add_document(random, doc)?;
    iwriter.commit(random)?;
    iwriter.delete_documents_with_terms(random, vec![Term::from_text("id", "1")])?;
    iwriter.force_merge(random, 1)?;

    let ireader = iwriter.get_reader(random)?;
    iwriter.close(random)?;

    let leaf = get_only_leaf_reader(&ireader)?;
    let mut dv = leaf.get_sorted_set_doc_values("field")?.unwrap();
    assert_eq!(0, dv.get_value_count()?);

    let mut skipper = leaf.get_doc_values_skipper("field")?.unwrap();
    assert_eq!(0, skipper.doc_count());
    skipper.advance(0)?;
    assert_eq!(NO_MORE_DOCS, skipper.min_doc_id_with_level(0));

    let mut terms_enum = dv.terms_enum()?;
    let lucene = new_bytes_ref_from_string(random, "lucene")?;
    assert!(!terms_enum.seek_exact(&lucene)?);
    assert_eq!(SeekStatus::End, terms_enum.seek_ceil(&lucene)?);
    assert_eq!(-1, dv.lookup_term(&lucene)?);

    ireader.close()?;
    Ok(())
  }

  fn test_number_merge_away_all_values_with_skipper<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwconfig = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwconfig.set_merge_policy(new_log_merge_policy(random)?);
    let iwriter = RandomIndexWriter::with_config(random, directory, iwconfig);
    let mut field_to_type = HashMap::new();

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "0",
      Store::No,
      &mut field_to_type,
    )?);
    iwriter.add_document(random, doc)?;

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "1",
      Store::No,
      &mut field_to_type,
    )?);
    doc.add(NumericDocValuesField::indexed_field("field", 5));
    iwriter.add_document(random, doc)?;
    iwriter.commit(random)?;
    iwriter.delete_documents_with_terms(random, vec![Term::from_text("id", "1")])?;
    iwriter.force_merge(random, 1)?;

    let ireader = iwriter.get_reader(random)?;
    iwriter.close(random)?;

    let leaf = get_only_leaf_reader(&ireader)?;
    let mut dv = leaf.get_numeric_doc_values("field")?.unwrap();
    assert_eq!(NO_MORE_DOCS, dv.next_doc()?);

    let mut skipper = leaf.get_doc_values_skipper("field")?.unwrap();
    assert_eq!(0, skipper.doc_count());
    skipper.advance(0)?;
    assert_eq!(NO_MORE_DOCS, skipper.min_doc_id_with_level(0));

    ireader.close()?;
    Ok(())
  }

  fn test_sorted_number_merge_away_all_values_with_skipper<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwconfig = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwconfig.set_merge_policy(new_log_merge_policy(random)?);
    let iwriter = RandomIndexWriter::with_config(random, directory, iwconfig);
    let mut field_to_type = HashMap::new();

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "0",
      Store::No,
      &mut field_to_type,
    )?);
    iwriter.add_document(random, doc)?;

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "1",
      Store::No,
      &mut field_to_type,
    )?);
    doc.add(SortedNumericDocValuesField::indexed_field("field", 5));
    iwriter.add_document(random, doc)?;
    iwriter.commit(random)?;
    iwriter.delete_documents_with_terms(random, vec![Term::from_text("id", "1")])?;
    iwriter.force_merge(random, 1)?;

    let ireader = iwriter.get_reader(random)?;
    iwriter.close(random)?;

    let leaf = get_only_leaf_reader(&ireader)?;
    let mut dv = leaf.get_sorted_numeric_doc_values("field")?.unwrap();
    assert_eq!(NO_MORE_DOCS, dv.next_doc()?);

    let mut skipper = leaf.get_doc_values_skipper("field")?.unwrap();
    assert_eq!(0, skipper.doc_count());
    skipper.advance(0)?;
    assert_eq!(NO_MORE_DOCS, skipper.min_doc_id_with_level(0));

    ireader.close()?;
    Ok(())
  }

  fn test_sorted_merge_away_all_values_large_segment_with_skipper<R>(
    &self,
    random: &mut R,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwconfig = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwconfig.set_merge_policy(new_log_merge_policy(random)?);
    let iwriter = RandomIndexWriter::with_config(random, directory, iwconfig);
    let mut field_to_type = HashMap::new();

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "1",
      Store::No,
      &mut field_to_type,
    )?);
    doc.add(SortedDocValuesField::indexed_field(
      "field",
      new_bytes_ref_from_string(random, "hello")?,
    ));
    iwriter.add_document(random, doc)?;

    let num_empty_docs = at_least(random, 1024);
    for _ in 0..num_empty_docs {
      iwriter.add_document(random, Document::new())?;
    }

    iwriter.commit(random)?;
    iwriter.delete_documents_with_terms(random, vec![Term::from_text("id", "1")])?;
    iwriter.force_merge(random, 1)?;

    let ireader = iwriter.get_reader(random)?;
    iwriter.close(random)?;

    let leaf = get_only_leaf_reader(&ireader)?;
    let mut dv = leaf.get_sorted_doc_values("field")?.unwrap();
    assert_eq!(NO_MORE_DOCS, dv.next_doc()?);

    let mut skipper = leaf.get_doc_values_skipper("field")?.unwrap();
    assert_eq!(0, skipper.doc_count());
    skipper.advance(0)?;
    assert_eq!(NO_MORE_DOCS, skipper.min_doc_id_with_level(0));

    let mut terms_enum = dv.terms_enum()?;
    let lucene = new_bytes_ref_from_string(random, "lucene")?;
    assert!(!terms_enum.seek_exact(&lucene)?);
    assert_eq!(SeekStatus::End, terms_enum.seek_ceil(&lucene)?);
    assert_eq!(-1, dv.lookup_term(&lucene)?);

    ireader.close()?;
    Ok(())
  }

  fn test_sorted_set_merge_away_all_values_large_segment_with_skipper<R>(
    &self,
    random: &mut R,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwconfig = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwconfig.set_merge_policy(new_log_merge_policy(random)?);
    let iwriter = RandomIndexWriter::with_config(random, directory, iwconfig);
    let mut field_to_type = HashMap::new();

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "1",
      Store::No,
      &mut field_to_type,
    )?);
    doc.add(SortedSetDocValuesField::indexed_field(
      "field",
      new_bytes_ref_from_string(random, "hello")?,
    ));
    iwriter.add_document(random, doc)?;

    let num_empty_docs = at_least(random, 1024);
    for _ in 0..num_empty_docs {
      iwriter.add_document(random, Document::new())?;
    }

    iwriter.commit(random)?;
    iwriter.delete_documents_with_terms(random, vec![Term::from_text("id", "1")])?;
    iwriter.force_merge(random, 1)?;

    let ireader = iwriter.get_reader(random)?;
    iwriter.close(random)?;

    let leaf = get_only_leaf_reader(&ireader)?;
    let mut dv = leaf.get_sorted_set_doc_values("field")?.unwrap();
    assert_eq!(NO_MORE_DOCS, dv.next_doc()?);

    let mut skipper = leaf.get_doc_values_skipper("field")?.unwrap();
    assert_eq!(0, skipper.doc_count());
    skipper.advance(0)?;
    assert_eq!(NO_MORE_DOCS, skipper.min_doc_id_with_level(0));

    let mut terms_enum = dv.terms_enum()?;
    let lucene = new_bytes_ref_from_string(random, "lucene")?;
    assert!(!terms_enum.seek_exact(&lucene)?);
    assert_eq!(SeekStatus::End, terms_enum.seek_ceil(&lucene)?);
    assert_eq!(-1, dv.lookup_term(&lucene)?);

    ireader.close()?;
    Ok(())
  }

  fn test_numeric_merge_away_all_values_large_segment_with_skipper<R>(
    &self,
    random: &mut R,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwconfig = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwconfig.set_merge_policy(new_log_merge_policy(random)?);
    let iwriter = RandomIndexWriter::with_config(random, directory, iwconfig);
    let mut field_to_type = HashMap::new();

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "1",
      Store::No,
      &mut field_to_type,
    )?);
    doc.add(NumericDocValuesField::indexed_field("field", 42));
    iwriter.add_document(random, doc)?;

    let num_empty_docs = at_least(random, 1024);
    for _ in 0..num_empty_docs {
      iwriter.add_document(random, Document::new())?;
    }

    iwriter.commit(random)?;
    iwriter.delete_documents_with_terms(random, vec![Term::from_text("id", "1")])?;
    iwriter.force_merge(random, 1)?;

    let ireader = iwriter.get_reader(random)?;
    iwriter.close(random)?;

    let leaf = get_only_leaf_reader(&ireader)?;
    let mut dv = leaf.get_numeric_doc_values("field")?.unwrap();
    assert_eq!(NO_MORE_DOCS, dv.next_doc()?);

    let mut skipper = leaf.get_doc_values_skipper("field")?.unwrap();
    assert_eq!(0, skipper.doc_count());
    skipper.advance(0)?;
    assert_eq!(NO_MORE_DOCS, skipper.min_doc_id_with_level(0));

    ireader.close()?;
    Ok(())
  }

  fn test_sorted_numeric_merge_away_all_values_large_segment_with_skipper<R>(
    &self,
    random: &mut R,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwconfig = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwconfig.set_merge_policy(new_log_merge_policy(random)?);
    let iwriter = RandomIndexWriter::with_config(random, directory, iwconfig);
    let mut field_to_type = HashMap::new();

    let mut doc = Document::new();
    doc.add(new_string_field(
      random,
      "id",
      "1",
      Store::No,
      &mut field_to_type,
    )?);
    doc.add(SortedNumericDocValuesField::indexed_field("field", 42));
    iwriter.add_document(random, doc)?;

    let num_empty_docs = at_least(random, 1024);
    for _ in 0..num_empty_docs {
      iwriter.add_document(random, Document::new())?;
    }

    iwriter.commit(random)?;
    iwriter.delete_documents_with_terms(random, vec![Term::from_text("id", "1")])?;
    iwriter.force_merge(random, 1)?;

    let ireader = iwriter.get_reader(random)?;
    iwriter.close(random)?;

    let leaf = get_only_leaf_reader(&ireader)?;
    let mut dv = leaf.get_sorted_numeric_doc_values("field")?.unwrap();
    assert_eq!(NO_MORE_DOCS, dv.next_doc()?);

    let mut skipper = leaf.get_doc_values_skipper("field")?.unwrap();
    assert_eq!(0, skipper.doc_count());
    skipper.advance(0)?;
    assert_eq!(NO_MORE_DOCS, skipper.min_doc_id_with_level(0));

    ireader.close()?;
    Ok(())
  }
  fn test_numeric_doc_values_with_skipper_small<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let total_docs = random.random_range(1..1000);
    self.do_test_numeric_doc_values_with_skipper(random, total_docs)
  }

  fn test_numeric_doc_values_with_skipper_medium<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let total_docs = random.random_range(1000..20000);
    self.do_test_numeric_doc_values_with_skipper(random, total_docs)
  }

  fn test_numeric_doc_values_with_skipper_big<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let total_docs = random.random_range(50000..100000);
    self.do_test_numeric_doc_values_with_skipper(random, total_docs)
  }

  fn do_test_numeric_doc_values_with_skipper<R>(
    &self,
    random: &mut R,
    total_docs: i32,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    self.assert_doc_values_with_skipper(random, total_docs, NumericTestDocValueSkipper)
  }

  fn test_sorted_numeric_doc_values_with_skipper_small<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let total_docs = random.random_range(1..1000);
    self.do_test_sorted_numeric_doc_values_with_skipper(random, total_docs)
  }

  fn test_sorted_numeric_doc_values_with_skipper_medium<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let total_docs = random.random_range(1000..20000);
    self.do_test_sorted_numeric_doc_values_with_skipper(random, total_docs)
  }

  fn test_sorted_numeric_doc_values_with_skipper_big<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let total_docs = random.random_range(50000..100000);
    self.do_test_sorted_numeric_doc_values_with_skipper(random, total_docs)
  }

  fn do_test_sorted_numeric_doc_values_with_skipper<R>(
    &self,
    random: &mut R,
    total_docs: i32,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    self.assert_doc_values_with_skipper(random, total_docs, SortedNumericTestDocValueSkipper)
  }

  fn test_sorted_doc_values_with_skipper_small<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let total_docs = random.random_range(1..1000);
    self.do_test_sorted_doc_values_with_skipper(random, total_docs)
  }

  fn test_sorted_doc_values_with_skipper_medium<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let total_docs = random.random_range(1000..20000);
    self.do_test_sorted_doc_values_with_skipper(random, total_docs)
  }

  fn test_sorted_doc_values_with_skipper_big<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let total_docs = random.random_range(50000..100000);
    self.do_test_sorted_doc_values_with_skipper(random, total_docs)
  }

  fn do_test_sorted_doc_values_with_skipper<R>(&self, random: &mut R, total_docs: i32) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    self.assert_doc_values_with_skipper(random, total_docs, SortedTestDocValueSkipper)
  }

  fn test_sorted_set_doc_values_with_skipper_small<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let total_docs = random.random_range(1..1000);
    self.do_test_sorted_set_doc_values_with_skipper(random, total_docs)
  }

  fn test_sorted_set_doc_values_with_skipper_medium<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let total_docs = random.random_range(10000..20000);
    self.do_test_sorted_set_doc_values_with_skipper(random, total_docs)
  }

  fn test_sorted_set_doc_values_with_skipper_big<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let total_docs = random.random_range(50000..100000);
    self.do_test_sorted_set_doc_values_with_skipper(random, total_docs)
  }

  fn do_test_sorted_set_doc_values_with_skipper<R>(
    &self,
    random: &mut R,
    total_docs: i32,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    self.assert_doc_values_with_skipper(random, total_docs, SortedSetTestDocValueSkipper)
  }

  fn assert_doc_values_with_skipper<T, R>(
    &self,
    random: &mut R,
    total_docs: i32,
    test_doc_value_skipper: T,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
    T: TestDocValueSkipper,
  {
    let directory = new_directory_shared(random)?;
    let writer = RandomIndexWriter::new(random, directory.clone())?;
    let mode = random.random_range(0..3);
    let mut num_docs = 0;
    for _ in 0..total_docs {
      let mut doc = Document::new();
      let should_populate = match mode {
        0 => true,
        1 => random.random_bool(0.5),
        2 => random.random_bool(0.5) && random.random_bool(0.5),
        _ => unreachable!(),
      };
      if should_populate {
        test_doc_value_skipper.populate_doc(random, &mut doc)?;
        num_docs += 1;
      }
      writer.add_document(random, doc)?;
      if rarely(random) {
        writer.commit(random)?;
      }
    }

    writer.flush()?;
    if random.random_bool(0.5) {
      writer.force_merge(random, 1)?;
    }

    let ireader = writer.get_reader(random)?;
    let context = (&ireader).get_context()?;
    let mut read_docs = 0;
    for reader_context in context.leaves()? {
      let reader = reader_context.reader();
      let mut output = Vec::with_capacity(1024);
      let status = CheckIndex::<DirectoryEnum, LockEnum, Sink>::test_doc_values(
        reader,
        Some(&mut output),
        true,
      )?;
      if let Some(error) = status.error {
        return IOUtils::rethrow_always(error);
      }
      let skipper = test_doc_value_skipper.doc_values_skipper(reader)?;
      let wrapper = test_doc_value_skipper.doc_values_wrapper(reader)?;
      read_docs += self.assert_doc_values_skip_sequential(wrapper, skipper)?;
      for _ in 0..10 {
        let skipper = test_doc_value_skipper.doc_values_skipper(reader)?;
        self.assert_doc_values_skip_random(
          random,
          test_doc_value_skipper.doc_values_wrapper(reader)?,
          skipper,
          reader.max_doc()?,
        )?;
      }
    }

    assert_eq!(num_docs, read_docs);
    ireader.close()?;
    writer.close(random)?;
    Ok(())
  }

  fn assert_doc_values_skip_sequential<I, SK>(
    &self,
    iterator: Option<I>,
    skipper: Option<SK>,
  ) -> Result<i32>
  where
    I: DocValuesWrapper,
    SK: DocValuesSkipper,
  {
    let (mut iterator, mut skipper) = match (iterator, skipper) {
      (Some(iterator), Some(skipper)) => (iterator, skipper),
      (None, None) => return Ok(0),
      _ => unreachable!(""),
    };

    assert_eq!(-1, iterator.doc_id());
    assert_eq!(-1, skipper.min_doc_id_with_level(0));
    assert_eq!(-1, skipper.max_doc_id_with_level(0));

    iterator.advance(0)?;
    let mut doc_count = 0;
    loop {
      let previous_max_doc = skipper.max_doc_id_with_level(0);
      skipper.advance(previous_max_doc + 1)?;
      assert!(skipper.min_doc_id_with_level(0) > previous_max_doc);
      if self.skipper_has_accurate_doc_bounds() {
        assert_eq!(iterator.doc_id(), skipper.min_doc_id_with_level(0));
      } else {
        assert!(skipper.min_doc_id_with_level(0) <= iterator.doc_id());
      }

      if skipper.min_doc_id_with_level(0) == NO_MORE_DOCS {
        assert_eq!(NO_MORE_DOCS, skipper.max_doc_id_with_level(0));
        break;
      }
      assert!(skipper.doc_count_with_level(0) > 0);

      let mut max_doc = -1;
      let mut min_val = i64::MAX;
      let mut max_val = i64::MIN;
      for _ in 0..skipper.doc_count_with_level(0) {
        assert_ne!(NO_MORE_DOCS, iterator.doc_id());
        max_doc = max_doc.max(iterator.doc_id());
        min_val = min_val.min(iterator.min_value()?);
        max_val = max_val.max(iterator.max_value()?);
        iterator.advance(iterator.doc_id() + 1)?;
      }

      if self.skipper_has_accurate_doc_bounds() {
        assert_eq!(max_doc, skipper.max_doc_id_with_level(0));
      } else {
        assert!(skipper.max_doc_id_with_level(0) >= max_doc);
      }
      if self.skipper_has_accurate_value_bounds() {
        assert_eq!(min_val, skipper.min_value_with_level(0));
        assert_eq!(max_val, skipper.max_value_with_level(0));
      } else {
        assert!(min_val >= skipper.min_value_with_level(0));
        assert!(max_val <= skipper.max_value_with_level(0));
      }

      doc_count += skipper.doc_count_with_level(0);
      for level in 1..skipper.num_levels() {
        assert!(skipper.min_doc_id_with_level(0) >= skipper.min_doc_id_with_level(level));
        assert!(skipper.max_doc_id_with_level(0) <= skipper.max_doc_id_with_level(level));
        assert!(skipper.min_value_with_level(0) >= skipper.min_value_with_level(level));
        assert!(skipper.max_value_with_level(0) <= skipper.max_value_with_level(level));
        assert!(skipper.doc_count_with_level(0) < skipper.doc_count_with_level(level));
      }
    }

    assert_eq!(doc_count, skipper.doc_count());
    Ok(doc_count)
  }

  fn assert_doc_values_skip_random<I, SK, R>(
    &self,
    random: &mut R,
    iterator: Option<I>,
    skipper: Option<SK>,
    max_doc: i32,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
    I: DocValuesWrapper,
    SK: DocValuesSkipper,
  {
    let (mut iterator, mut skipper) = match (iterator, skipper) {
      (Some(iterator), Some(skipper)) => (iterator, skipper),
      (None, None) => return Ok(()),
      _ => unreachable!(""),
    };

    let mut next_level = 0;
    loop {
      let base = skipper.max_doc_id_with_level(next_level);
      let doc = random.random_range(base..(max_doc + 1)) + 1;
      skipper.advance(doc)?;
      if skipper.min_doc_id_with_level(0) == NO_MORE_DOCS {
        assert_eq!(NO_MORE_DOCS, skipper.max_doc_id_with_level(0));
        return Ok(());
      }
      if iterator.advance_exact(doc)? {
        for level in 0..skipper.num_levels() {
          assert!(iterator.doc_id() >= skipper.min_doc_id_with_level(level));
          assert!(iterator.doc_id() <= skipper.max_doc_id_with_level(level));
          assert!(iterator.min_value()? >= skipper.min_value_with_level(level));
          assert!(iterator.max_value()? <= skipper.max_value_with_level(level));
        }
      }
      next_level = random.random_range(0..skipper.num_levels());
    }
  }
  fn test_mismatched_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir1 = new_directory_shared(random)?;
    let w1 = crate::core::index::index_writer::IndexWriter::new(
      dir1.clone(),
      crate::test_framework::core::util::lucene_test_case::new_index_writer_config(random)?,
    )?;
    let mut doc = Document::new();
    doc.add(BinaryDocValuesField::new(
      "binary",
      new_bytes_ref_from_string(random, "lucene")?,
    ));
    doc.add(NumericDocValuesField::new("numeric", 0));
    doc.add(SortedDocValuesField::indexed_field(
      "sorted",
      new_bytes_ref_from_string(random, "search")?,
    ));
    doc.add(SortedNumericDocValuesField::new("sorted_numeric", 1));
    doc.add(SortedSetDocValuesField::indexed_field(
      "sorted_set",
      new_bytes_ref_from_string(random, "engine")?,
    ));
    w1.add_document(doc.clone())?;

    let dir2 = new_directory_shared(random)?;
    let mut iwc = new_index_writer_config(random)?;
    iwc.set_merge_scheduler(SerialMergeScheduler::new());
    let w2 = crate::core::index::index_writer::IndexWriter::new(dir2.clone(), iwc)?;
    w2.add_document(doc)?;
    w2.commit()?;

    let reader = crate::core::index::directory_reader::open_from_writer(&w1)?;
    w1.close()?;
    let leaf = get_only_leaf_reader(&reader)?;
    let mismatched = MismatchedCodecReader::new(leaf, random)?;
    w2.add_indexes_from_codec_readers(vec![mismatched])?;
    reader.close()?;
    w2.force_merge(1)?;

    let reader = crate::core::index::directory_reader::open_from_writer(&w2)?;
    w2.close()?;
    let leaf = get_only_leaf_reader(&reader)?;

    let mut binary = leaf
      .get_binary_doc_values("binary")?
      .expect("binary doc values should exist");
    assert_eq!(0, binary.next_doc()?);
    assert_eq!(
      &BytesRef::from_string("lucene"),
      binary.binary_value()?.as_ref()
    );
    assert_eq!(1, binary.next_doc()?);
    assert_eq!(
      &BytesRef::from_string("lucene"),
      binary.binary_value()?.as_ref()
    );
    assert_eq!(NO_MORE_DOCS, binary.next_doc()?);

    let mut numeric = leaf
      .get_numeric_doc_values("numeric")?
      .expect("numeric doc values should exist");
    assert_eq!(0, numeric.next_doc()?);
    assert_eq!(0, numeric.long_value()?);
    assert_eq!(1, numeric.next_doc()?);
    assert_eq!(0, numeric.long_value()?);
    assert_eq!(NO_MORE_DOCS, numeric.next_doc()?);

    let mut sorted = leaf
      .get_sorted_doc_values("sorted")?
      .expect("sorted doc values should exist");
    assert_eq!(0, sorted.next_doc()?);
    let ord = sorted.ord_value()?;
    assert_eq!(
      &BytesRef::from_string("search"),
      sorted.lookup_ord(ord)?.as_ref()
    );
    assert_eq!(1, sorted.next_doc()?);
    let ord = sorted.ord_value()?;
    assert_eq!(
      &BytesRef::from_string("search"),
      sorted.lookup_ord(ord)?.as_ref()
    );
    assert_eq!(NO_MORE_DOCS, sorted.next_doc()?);

    let mut sorted_numeric = leaf
      .get_sorted_numeric_doc_values("sorted_numeric")?
      .expect("sorted numeric doc values should exist");
    assert_eq!(0, sorted_numeric.next_doc()?);
    assert_eq!(1, sorted_numeric.next_value()?);
    assert_eq!(1, sorted_numeric.next_doc()?);
    assert_eq!(1, sorted_numeric.next_value()?);
    assert_eq!(NO_MORE_DOCS, sorted_numeric.next_doc()?);

    let mut sorted_set = leaf
      .get_sorted_set_doc_values("sorted_set")?
      .expect("sorted set doc values should exist");
    assert_eq!(0, sorted_set.next_doc()?);
    let ord = sorted_set.next_ord()?;
    assert_eq!(
      &BytesRef::from_string("engine"),
      sorted_set.lookup_ord(ord)?.as_ref()
    );
    assert_eq!(1, sorted_set.next_doc()?);
    let ord = sorted_set.next_ord()?;
    assert_eq!(
      &BytesRef::from_string("engine"),
      sorted_set.lookup_ord(ord)?.as_ref()
    );
    assert_eq!(NO_MORE_DOCS, sorted_set.next_doc()?);

    reader.close()?;
    dir1.close()?;
    dir2.close()?;
    Ok(())
  }
}
pub trait TestDocValueSkipper {
  fn populate_doc<R>(&self, random: &mut R, doc: &mut Document) -> Result<()>
  where
    R: Rng + ?Sized;

  type DocValuesWrapper<LR>: DocValuesWrapper
  where
    LR: LeafReader;

  fn doc_values_wrapper<LR>(&self, leaf_reader: &LR) -> Result<Option<Self::DocValuesWrapper<LR>>>
  where
    LR: LeafReader;

  type DocValuesSkipper<LR>: DocValuesSkipper
  where
    LR: LeafReader;

  fn doc_values_skipper<LR>(&self, leaf_reader: &LR) -> Result<Option<Self::DocValuesSkipper<LR>>>
  where
    LR: LeafReader;
}

pub trait DocValuesWrapper {
  fn advance(&mut self, target: i32) -> Result<i32>;

  fn advance_exact(&mut self, target: i32) -> Result<bool>;

  fn max_value(&mut self) -> Result<i64>;

  fn min_value(&mut self) -> Result<i64>;

  fn doc_id(&self) -> i32;
}

pub struct NumericDocValuesWrapper<DV> {
  numeric_doc_values: DV,
}

impl<DV> NumericDocValuesWrapper<DV>
where
  DV: NumericDocValues,
{
  fn new(numeric_doc_values: DV) -> Self {
    Self { numeric_doc_values }
  }
}

impl<DV> DocValuesWrapper for NumericDocValuesWrapper<DV>
where
  DV: NumericDocValues,
{
  fn advance(&mut self, target: i32) -> Result<i32> {
    self.numeric_doc_values.advance(target)
  }

  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    self.numeric_doc_values.advance_exact(target)
  }

  fn max_value(&mut self) -> Result<i64> {
    self.numeric_doc_values.long_value()
  }

  fn min_value(&mut self) -> Result<i64> {
    self.numeric_doc_values.long_value()
  }

  fn doc_id(&self) -> i32 {
    self.numeric_doc_values.doc_id()
  }
}

pub struct NumericTestDocValueSkipper;

impl TestDocValueSkipper for NumericTestDocValueSkipper {
  fn populate_doc<R>(&self, random: &mut R, doc: &mut Document) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    doc.add(NumericDocValuesField::indexed_field(
      "test",
      random.random::<i64>(),
    ));
    Ok(())
  }

  type DocValuesWrapper<LR>
    = NumericDocValuesWrapper<LR::NumericDocValues>
  where
    LR: LeafReader;

  fn doc_values_wrapper<LR>(&self, leaf_reader: &LR) -> Result<Option<Self::DocValuesWrapper<LR>>>
  where
    LR: LeafReader,
  {
    match leaf_reader.get_numeric_doc_values("test")? {
      Some(numeric_doc_values) => Ok(Some(NumericDocValuesWrapper::new(numeric_doc_values))),
      None => Ok(None),
    }
  }

  type DocValuesSkipper<LR>
    = LR::DocValuesSkipper
  where
    LR: LeafReader;

  fn doc_values_skipper<LR>(&self, leaf_reader: &LR) -> Result<Option<Self::DocValuesSkipper<LR>>>
  where
    LR: LeafReader,
  {
    leaf_reader.get_doc_values_skipper("test")
  }
}

pub struct SortedNumericDocValuesWrapper<DV> {
  sorted_numeric_doc_values: DV,
  max: i64,
  min: i64,
}

impl<DV> SortedNumericDocValuesWrapper<DV>
where
  DV: SortedNumericDocValues,
{
  fn new(sorted_numeric_doc_values: DV) -> Self {
    Self {
      sorted_numeric_doc_values,
      max: i64::MIN,
      min: i64::MAX,
    }
  }

  fn read_values(&mut self) -> Result<()> {
    self.max = i64::MIN;
    self.min = i64::MAX;
    for _ in 0..self.sorted_numeric_doc_values.doc_value_count()? {
      let value = self.sorted_numeric_doc_values.next_value()?;
      self.max = self.max.max(value);
      self.min = self.min.min(value);
    }
    Ok(())
  }
}

impl<DV> DocValuesWrapper for SortedNumericDocValuesWrapper<DV>
where
  DV: SortedNumericDocValues,
{
  fn advance(&mut self, target: i32) -> Result<i32> {
    let doc = self.sorted_numeric_doc_values.advance(target)?;
    if doc != NO_MORE_DOCS {
      self.read_values()?;
    }
    Ok(doc)
  }

  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    if self.sorted_numeric_doc_values.advance_exact(target)? {
      self.read_values()?;
      Ok(true)
    } else {
      Ok(false)
    }
  }

  fn max_value(&mut self) -> Result<i64> {
    Ok(self.max)
  }

  fn min_value(&mut self) -> Result<i64> {
    Ok(self.min)
  }

  fn doc_id(&self) -> i32 {
    self.sorted_numeric_doc_values.doc_id()
  }
}

pub struct SortedNumericTestDocValueSkipper;

impl TestDocValueSkipper for SortedNumericTestDocValueSkipper {
  fn populate_doc<R>(&self, random: &mut R, doc: &mut Document) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    for _ in 0..random.random_range(1..5) {
      doc.add(SortedNumericDocValuesField::indexed_field(
        "test",
        random.random::<i64>(),
      ));
    }
    Ok(())
  }

  type DocValuesWrapper<LR>
    = SortedNumericDocValuesWrapper<LR::SortedNumericDocValues>
  where
    LR: LeafReader;

  fn doc_values_wrapper<LR>(&self, leaf_reader: &LR) -> Result<Option<Self::DocValuesWrapper<LR>>>
  where
    LR: LeafReader,
  {
    match leaf_reader.get_sorted_numeric_doc_values("test")? {
      Some(sorted_numeric_doc_values) => Ok(Some(SortedNumericDocValuesWrapper::new(
        sorted_numeric_doc_values,
      ))),
      None => Ok(None),
    }
  }

  type DocValuesSkipper<LR>
    = LR::DocValuesSkipper
  where
    LR: LeafReader;

  fn doc_values_skipper<LR>(&self, leaf_reader: &LR) -> Result<Option<Self::DocValuesSkipper<LR>>>
  where
    LR: LeafReader,
  {
    leaf_reader.get_doc_values_skipper("test")
  }
}

pub struct SortedDocValuesWrapper<DV> {
  sorted_doc_values: DV,
}

impl<DV> SortedDocValuesWrapper<DV>
where
  DV: SortedDocValues,
{
  fn new(sorted_doc_values: DV) -> Self {
    Self { sorted_doc_values }
  }
}

impl<DV> DocValuesWrapper for SortedDocValuesWrapper<DV>
where
  DV: SortedDocValues,
{
  fn advance(&mut self, target: i32) -> Result<i32> {
    self.sorted_doc_values.advance(target)
  }

  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    self.sorted_doc_values.advance_exact(target)
  }

  fn max_value(&mut self) -> Result<i64> {
    Ok(self.sorted_doc_values.ord_value()? as i64)
  }

  fn min_value(&mut self) -> Result<i64> {
    Ok(self.sorted_doc_values.ord_value()? as i64)
  }

  fn doc_id(&self) -> i32 {
    self.sorted_doc_values.doc_id()
  }
}

pub struct SortedTestDocValueSkipper;

impl TestDocValueSkipper for SortedTestDocValueSkipper {
  fn populate_doc<R>(&self, random: &mut R, doc: &mut Document) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    doc.add(SortedDocValuesField::indexed_field(
      "test",
      TestUtil::random_binary_term(random),
    ));
    Ok(())
  }

  type DocValuesWrapper<LR>
    = SortedDocValuesWrapper<LR::SortedDocValues>
  where
    LR: LeafReader;

  fn doc_values_wrapper<LR>(&self, leaf_reader: &LR) -> Result<Option<Self::DocValuesWrapper<LR>>>
  where
    LR: LeafReader,
  {
    match leaf_reader.get_sorted_doc_values("test")? {
      Some(sorted_doc_values) => Ok(Some(SortedDocValuesWrapper::new(sorted_doc_values))),
      None => Ok(None),
    }
  }

  type DocValuesSkipper<LR>
    = LR::DocValuesSkipper
  where
    LR: LeafReader;

  fn doc_values_skipper<LR>(&self, leaf_reader: &LR) -> Result<Option<Self::DocValuesSkipper<LR>>>
  where
    LR: LeafReader,
  {
    leaf_reader.get_doc_values_skipper("test")
  }
}

pub struct SortedSetDocValuesWrapper<DV> {
  sorted_set_doc_values: DV,
  max: i64,
  min: i64,
}

impl<DV> SortedSetDocValuesWrapper<DV>
where
  DV: SortedSetDocValues,
{
  fn new(sorted_set_doc_values: DV) -> Self {
    Self {
      sorted_set_doc_values,
      max: i64::MIN,
      min: i64::MAX,
    }
  }

  fn read_values(&mut self) -> Result<()> {
    self.max = i64::MIN;
    self.min = i64::MAX;
    for _ in 0..self.sorted_set_doc_values.doc_value_count()? {
      let value = self.sorted_set_doc_values.next_ord()?;
      self.max = self.max.max(value);
      self.min = self.min.min(value);
    }
    Ok(())
  }
}

impl<DV> DocValuesWrapper for SortedSetDocValuesWrapper<DV>
where
  DV: SortedSetDocValues,
{
  fn advance(&mut self, target: i32) -> Result<i32> {
    let doc = self.sorted_set_doc_values.advance(target)?;
    if doc != NO_MORE_DOCS {
      self.read_values()?;
    }
    Ok(doc)
  }

  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    if self.sorted_set_doc_values.advance_exact(target)? {
      self.read_values()?;
      Ok(true)
    } else {
      Ok(false)
    }
  }

  fn max_value(&mut self) -> Result<i64> {
    Ok(self.max)
  }

  fn min_value(&mut self) -> Result<i64> {
    Ok(self.min)
  }

  fn doc_id(&self) -> i32 {
    self.sorted_set_doc_values.doc_id()
  }
}

pub struct SortedSetTestDocValueSkipper;

impl TestDocValueSkipper for SortedSetTestDocValueSkipper {
  fn populate_doc<R>(&self, random: &mut R, doc: &mut Document) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    for _ in 0..random.random_range(1..5) {
      doc.add(SortedSetDocValuesField::indexed_field(
        "test",
        TestUtil::random_binary_term(random),
      ));
    }
    Ok(())
  }

  type DocValuesWrapper<LR>
    = SortedSetDocValuesWrapper<LR::SortedSetDocValues>
  where
    LR: LeafReader;

  fn doc_values_wrapper<LR>(&self, leaf_reader: &LR) -> Result<Option<Self::DocValuesWrapper<LR>>>
  where
    LR: LeafReader,
  {
    match leaf_reader.get_sorted_set_doc_values("test")? {
      Some(sorted_set_doc_values) => {
        Ok(Some(SortedSetDocValuesWrapper::new(sorted_set_doc_values)))
      },
      None => Ok(None),
    }
  }

  type DocValuesSkipper<LR>
    = LR::DocValuesSkipper
  where
    LR: LeafReader;

  fn doc_values_skipper<LR>(&self, leaf_reader: &LR) -> Result<Option<Self::DocValuesSkipper<LR>>>
  where
    LR: LeafReader,
  {
    leaf_reader.get_doc_values_skipper("test")
  }
}
