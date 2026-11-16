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
use crate::core::document::document::Document;
use crate::core::document::field::Store;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::string_field::StringField;
use crate::core::document::text_field::TextField;
use crate::core::index::directory_reader;
use crate::core::index::field_invert_state::FieldInvertState;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::multi_doc_values::MultiDocValues;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::term::Term;
use crate::core::search::collection_statistics::CollectionStatistics;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::similarities_impl::similarities::{BoxSimScorer, Similarity};
use crate::core::search::term_statistics::TermStatistics;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::base_index_file_format_test_case::{
  BaseIndexFileFormatTestCase, BaseIndexFileFormatTestCaseDefaults,
};
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, at_least_usize, get_only_leaf_reader, new_directory, new_log_merge_policy,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::{Rng, RngExt};
use std::borrow::Borrow;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::thread;

pub struct BaseNormsFormatTestCaseDefaults;

pub trait BaseNormsFormatTestCase:
  BaseIndexFileFormatTestCase<Defaults = BaseNormsFormatTestCaseDefaults>
{
  fn codec_supports_sparsity(&self) -> bool {
    true
  }

  fn test_byte_range<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let iterations = at_least(random, 1);
    for _ in 0..iterations {
      self.do_test_norms_versus_doc_values(random, 1.0, |random| {
        TestUtil::next_long(random, i8::MIN as i64, i8::MAX as i64)
      })?;
    }
    Ok(())
  }

  fn test_sparse_byte_range<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    assert!(self.codec_supports_sparsity());
    let iterations = at_least(random, 1);
    for _ in 0..iterations {
      let density = random.random::<f64>();
      self.do_test_norms_versus_doc_values(random, density, |random| {
        TestUtil::next_long(random, i8::MIN as i64, i8::MAX as i64)
      })?;
    }
    Ok(())
  }

  fn test_short_range<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let iterations = at_least(random, 1);
    for _ in 0..iterations {
      self.do_test_norms_versus_doc_values(random, 1.0, |random| {
        TestUtil::next_long(random, i16::MIN as i64, i16::MAX as i64)
      })?;
    }
    Ok(())
  }

  fn test_sparse_short_range<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    assert!(self.codec_supports_sparsity());
    let iterations = at_least(random, 1);
    for _ in 0..iterations {
      let density = random.random::<f64>();
      self.do_test_norms_versus_doc_values(random, density, |random| {
        TestUtil::next_long(random, i16::MIN as i64, i16::MAX as i64)
      })?;
    }
    Ok(())
  }

  fn test_long_range<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let iterations = at_least(random, 1);
    for _ in 0..iterations {
      self.do_test_norms_versus_doc_values(random, 1.0, |random| {
        TestUtil::next_long(random, i64::MIN, i64::MAX)
      })?;
    }
    Ok(())
  }

  fn test_sparse_long_range<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    assert!(self.codec_supports_sparsity());
    let iterations = at_least(random, 1);
    for _ in 0..iterations {
      let density = random.random::<f64>();
      self.do_test_norms_versus_doc_values(random, density, |random| {
        TestUtil::next_long(random, i64::MIN, i64::MAX)
      })?;
    }
    Ok(())
  }

  fn test_full_long_range<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let iterations = at_least(random, 1);
    for _ in 0..iterations {
      self.do_test_norms_versus_doc_values(random, 1.0, |random| {
        match random.random_range(0..3) {
          0 => i64::MIN,
          1 => i64::MAX,
          _ => TestUtil::next_long(random, i64::MIN, i64::MAX),
        }
      })?;
    }
    Ok(())
  }

  fn test_sparse_full_long_range<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    assert!(self.codec_supports_sparsity());
    let iterations = at_least(random, 1);
    for _ in 0..iterations {
      let density = random.random::<f64>();
      self.do_test_norms_versus_doc_values(random, density, |random| {
        match random.random_range(0..3) {
          0 => i64::MIN,
          1 => i64::MAX,
          _ => TestUtil::next_long(random, i64::MIN, i64::MAX),
        }
      })?;
    }
    Ok(())
  }

  fn test_few_values<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let iterations = at_least(random, 1);
    for _ in 0..iterations {
      self.do_test_norms_versus_doc_values(random, 1.0, |random| {
        if random.random_bool(0.5) { 20 } else { 3 }
      })?;
    }
    Ok(())
  }

  fn test_few_sparse_values<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    assert!(self.codec_supports_sparsity());
    let iterations = at_least(random, 1);
    for _ in 0..iterations {
      let density = random.random::<f64>();
      self.do_test_norms_versus_doc_values(random, density, |random| {
        if random.random_bool(0.5) { 20 } else { 3 }
      })?;
    }
    Ok(())
  }

  fn test_few_large_values<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let iterations = at_least(random, 1);
    for _ in 0..iterations {
      self.do_test_norms_versus_doc_values(random, 1.0, |random| {
        if random.random_bool(0.5) {
          1_000_000
        } else {
          -5_000
        }
      })?;
    }
    Ok(())
  }

  fn test_few_sparse_large_values<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    assert!(self.codec_supports_sparsity());
    let iterations = at_least(random, 1);
    for _ in 0..iterations {
      let density = random.random::<f64>();
      self.do_test_norms_versus_doc_values(random, density, |random| {
        if random.random_bool(0.5) {
          1_000_000
        } else {
          -5_000
        }
      })?;
    }
    Ok(())
  }

  fn test_all_zeros<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let iterations = at_least(random, 1);
    for _ in 0..iterations {
      self.do_test_norms_versus_doc_values(random, 1.0, |_| 0)?;
    }
    Ok(())
  }

  fn test_sparse_all_zeros<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    assert!(self.codec_supports_sparsity());
    let iterations = at_least(random, 1);
    for _ in 0..iterations {
      let density = random.random::<f64>();
      self.do_test_norms_versus_doc_values(random, density, |_| 0)?;
    }
    Ok(())
  }

  fn test_most_zeros<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let iterations = at_least(random, 1);
    for _ in 0..iterations {
      self.do_test_norms_versus_doc_values(random, 1.0, |random| {
        if random.random_range(0..100) == 0 {
          TestUtil::next_long(random, i8::MIN as i64, i8::MAX as i64)
        } else {
          0
        }
      })?;
    }
    Ok(())
  }

  fn test_outliers<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let iterations = at_least(random, 1);
    for _ in 0..iterations {
      let common_value = TestUtil::next_long(random, i8::MIN as i64, i8::MAX as i64);
      self.do_test_norms_versus_doc_values(random, 1.0, move |random| {
        if random.random_range(0..100) == 0 {
          TestUtil::next_long(random, i8::MIN as i64, i8::MAX as i64)
        } else {
          common_value
        }
      })?;
    }
    Ok(())
  }

  fn test_sparse_outliers<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    assert!(self.codec_supports_sparsity());
    let iterations = at_least(random, 1);
    for _ in 0..iterations {
      let common_value = TestUtil::next_long(random, i8::MIN as i64, i8::MAX as i64);
      let density = random.random::<f64>();
      self.do_test_norms_versus_doc_values(random, density, move |random| {
        if random.random_range(0..100) == 0 {
          TestUtil::next_long(random, i8::MIN as i64, i8::MAX as i64)
        } else {
          common_value
        }
      })?;
    }
    Ok(())
  }

  fn test_outliers2<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let iterations = at_least(random, 1);
    for _ in 0..iterations {
      let common_value = TestUtil::next_long(random, i8::MIN as i64, i8::MAX as i64);
      let uncommon_value = TestUtil::next_long(random, i8::MIN as i64, i8::MAX as i64);
      self.do_test_norms_versus_doc_values(random, 1.0, move |random| {
        if random.random_range(0..100) == 0 {
          uncommon_value
        } else {
          common_value
        }
      })?;
    }
    Ok(())
  }

  fn test_sparse_outliers2<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    assert!(self.codec_supports_sparsity());
    let iterations = at_least(random, 1);
    for _ in 0..iterations {
      let common_value = TestUtil::next_long(random, i8::MIN as i64, i8::MAX as i64);
      let uncommon_value = TestUtil::next_long(random, i8::MIN as i64, i8::MAX as i64);
      let density = random.random::<f64>();
      self.do_test_norms_versus_doc_values(random, density, move |random| {
        if random.random_range(0..100) == 0 {
          uncommon_value
        } else {
          common_value
        }
      })?;
    }
    Ok(())
  }

  fn test_n_common<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let n = TestUtil::next_usize(random, 2, 15);
    let common_values = build_random_values(random, n);
    let num_other_values = TestUtil::next_usize(random, 2, 256 - n);
    let other_values = build_random_values(random, num_other_values);
    self.do_test_norms_versus_doc_values(random, 1.0, move |random| {
      if random.random_range(0..100) == 0 {
        other_values[random.random_range(0..other_values.len())]
      } else {
        common_values[random.random_range(0..common_values.len())]
      }
    })
  }

  fn test_sparse_n_common<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    assert!(self.codec_supports_sparsity());
    let n = TestUtil::next_usize(random, 2, 15);
    let common_values = build_random_values(random, n);
    let num_other_values = TestUtil::next_usize(random, 2, 256 - n);
    let other_values = build_random_values(random, num_other_values);
    let density = random.random::<f64>();
    self.do_test_norms_versus_doc_values(random, density, move |random| {
      if random.random_range(0..100) == 0 {
        other_values[random.random_range(0..other_values.len())]
      } else {
        common_values[random.random_range(0..common_values.len())]
      }
    })
  }

  #[cfg_attr(not(feature = "nightly"), allow(dead_code))]
  fn test_n_common_big<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let iterations = at_least(random, 1);
    for _ in 0..iterations {
      for n in 2..16 {
        let common_values = build_random_values(random, n);
        let num_other_values = TestUtil::next_usize(random, 2, 256 - n);
        let other_values = build_random_values(random, num_other_values);
        self.do_test_norms_versus_doc_values(random, 1.0, move |random| {
          if random.random_range(0..100) == 0 {
            other_values[random.random_range(0..other_values.len())]
          } else {
            common_values[random.random_range(0..common_values.len())]
          }
        })?;
      }
    }
    Ok(())
  }

  #[cfg_attr(not(feature = "nightly"), allow(dead_code))]
  fn test_sparse_n_common_big<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    assert!(self.codec_supports_sparsity());
    let iterations = at_least(random, 1);
    for _ in 0..iterations {
      for n in 2..16 {
        let common_values = build_random_values(random, n);
        let num_other_values = TestUtil::next_usize(random, 2, 256 - n);
        let other_values = build_random_values(random, num_other_values);
        let density = random.random::<f64>();
        self.do_test_norms_versus_doc_values(random, density, move |random| {
          if random.random_range(0..100) == 0 {
            other_values[random.random_range(0..other_values.len())]
          } else {
            common_values[random.random_range(0..common_values.len())]
          }
        })?;
      }
    }
    Ok(())
  }

  fn do_test_norms_versus_doc_values<R, F>(
    &self,
    random: &mut R,
    density: f64,
    mut longs: F,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
    F: FnMut(&mut R) -> i64,
  {
    let num_docs = at_least_usize(random, 500);
    let mut docs_with_field = FixedBitSet::new(num_docs);
    let num_docs_with_field = std::cmp::max(1, (density * num_docs as f64) as usize);
    if num_docs_with_field == num_docs {
      docs_with_field.set_with_range(0, num_docs);
    } else {
      let mut count = 0;
      while count < num_docs_with_field {
        let doc = random.random_range(0..num_docs);
        if !docs_with_field.get(doc)? {
          docs_with_field.set(doc)?;
          count += 1;
        }
      }
    }

    let mut norms = Vec::with_capacity(num_docs_with_field);
    for _ in 0..num_docs_with_field {
      norms.push(longs(random));
    }

    let dir = new_directory(random)?;
    let dir = Arc::new(self.apply_created_version_major(random, dir)?);
    let analyzer = MockAnalyzer::new(random);
    let norms = Arc::new(norms);
    let indexing_similarity_conf = CannedNormSimilarity::new(norms.clone());
    let conf =
      crate::test_framework::core::util::lucene_test_case::new_index_writer_config_with_analyzer(
        random, analyzer,
      )?;

    let writer = RandomIndexWriter::with_config(random, dir.clone(), conf);

    let mut norm_ord = 0usize;
    let mut doc = Document::new();
    for i in 0..num_docs {
      if !docs_with_field.get(i)? {
        let mut doc2 = Document::new();
        doc2.add(StringField::from_string("id", i.to_string(), Store::No)?);
        writer.add_document_with_similarity(random, doc2, &indexing_similarity_conf)?;
      } else {
        doc.clear();
        doc.add(StringField::from_string("id", i.to_string(), Store::No)?);
        let value = norms[norm_ord];
        norm_ord += 1;
        doc.add(NumericDocValuesField::indexed_field("dv", value));
        doc.add(TextField::from_string(
          "indexed",
          if value == 0 { "" } else { "a" },
          Store::No,
        )?);
        writer.add_document_with_similarity(random, &mut doc, &indexing_similarity_conf)?;
      }

      if random.random_range(0..31) == 0 {
        writer.commit(random)?;
      }
    }

    let max_deletions = std::cmp::max(1, num_docs / 20);
    let num_deletions = random.random_range(0..max_deletions);
    for _ in 0..num_deletions {
      let id = random.random_range(0..num_docs);
      writer.delete_documents_with_terms(random, vec![Term::from_text("id", id.to_string())])?;
    }

    writer.commit(random)?;

    let reader = self.maybe_wrap_with_merging_reader(directory_reader::open(dir.clone())?)?;
    self.check_norms_vs_doc_values(&reader)?;
    reader.close()?;

    writer.force_merge(random, 1)?;

    let reader = self.maybe_wrap_with_merging_reader(directory_reader::open(dir)?)?;
    self.check_norms_vs_doc_values(&reader)?;
    reader.close()?;

    writer.close(random)?;
    Ok(())
  }

  fn check_norms_vs_doc_values<IR>(&self, reader: IR) -> Result<()>
  where
    IR: IndexReader,
  {
    let context = reader.get_context()?;
    for leaf in context.leaves()? {
      let leaf_reader = leaf.reader();
      let expected = leaf_reader.get_numeric_doc_values("dv")?;
      let actual = leaf_reader.get_norm_values("indexed")?;
      assert_eq!(expected.is_none(), actual.is_none());

      if let (Some(mut expected), Some(mut actual)) = (expected, actual) {
        let mut doc = expected.next_doc()?;
        while doc != NO_MORE_DOCS {
          assert_eq!(doc, actual.next_doc()?);
          assert_eq!(expected.long_value()?, actual.long_value()?, "doc {}", doc);
          doc = expected.next_doc()?;
        }
        assert_eq!(NO_MORE_DOCS, actual.next_doc()?);
      }
    }
    Ok(())
  }

  #[allow(dead_code)]
  fn test_merge_stability(&self) -> Result<()> {
    Ok(())
  }

  fn test_undead_norms<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory(random)?;
    let dir = Arc::new(self.apply_created_version_major(random, dir)?);
    let writer = RandomIndexWriter::new(random, dir.clone())?;
    let num_docs = at_least(random, 500);
    let mut to_delete = Vec::new();

    for i in 0..num_docs {
      let mut doc = Document::new();
      doc.add(StringField::from_string("id", i.to_string(), Store::No)?);
      if random.random_range(0..5) == 1 {
        to_delete.push(i);
        doc.add(TextField::from_string(
          "content",
          "some content",
          Store::No,
        )?);
      }
      writer.add_document(random, doc)?;
    }

    for id in to_delete {
      writer.delete_documents_with_terms(random, vec![Term::from_text("id", id.to_string())])?;
    }

    writer.force_merge(random, 1)?;
    let reader = self.maybe_wrap_with_merging_reader(writer.get_reader(random)?)?;
    assert!(!reader.has_deletions()?);

    let mut norms = MultiDocValues::get_norm_values(&reader, "content")?
      .ok_or_else(|| LuceneError::illegal_state("norms should not be null"))?;
    if self.codec_supports_sparsity() {
      assert_eq!(NO_MORE_DOCS, norms.next_doc()?);
    } else {
      for i in 0..reader.max_doc()? {
        assert_eq!(i, norms.next_doc()?);
        assert_eq!(0, norms.long_value()?);
      }
    }

    reader.close()?;
    writer.close(random)?;
    Ok(())
  }
  fn test_threads<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
    Self: Sync,
  {
    let density = if !self.codec_supports_sparsity() || random.random::<bool>() {
      1.0
    } else {
      random.random::<f64>()
    };
    let num_docs = at_least_usize(random, 500);
    let mut docs_with_field = FixedBitSet::new(num_docs);
    let num_docs_with_field = std::cmp::max(1, (density * num_docs as f64) as usize);
    if num_docs_with_field == num_docs {
      docs_with_field.set_with_range(0, num_docs);
    } else {
      let mut count = 0;
      while count < num_docs_with_field {
        let doc = random.random_range(0..num_docs);
        if !docs_with_field.get(doc)? {
          docs_with_field.set(doc)?;
          count += 1;
        }
      }
    }

    let mut norms = Vec::with_capacity(num_docs_with_field);
    for _ in 0..num_docs_with_field {
      norms.push(random.random::<i64>());
    }

    let dir = new_directory(random)?;
    let dir = Arc::new(self.apply_created_version_major(random, dir)?);
    let analyzer = MockAnalyzer::new(random);
    let norms = Arc::new(norms);
    let indexing_similarity_conf = CannedNormSimilarity::new(norms.clone());
    let mut conf =
      crate::test_framework::core::util::lucene_test_case::new_index_writer_config_with_analyzer(
        random, analyzer,
      )?;
    conf.set_merge_policy(NoMergePolicy::default());
    let writer = RandomIndexWriter::with_config(random, dir.clone(), conf);

    let mut norm_ord = 0usize;
    let mut doc = Document::new();
    for i in 0..num_docs {
      if docs_with_field.get(i)? {
        doc.clear();
        doc.add(StringField::from_string("id", i.to_string(), Store::No)?);
        let value = norms[norm_ord];
        norm_ord += 1;
        doc.add(TextField::from_string(
          "indexed",
          if value == 0 { "" } else { "a" },
          Store::No,
        )?);
        doc.add(NumericDocValuesField::indexed_field("dv", value));
        writer.add_document_with_similarity(random, &mut doc, &indexing_similarity_conf)?;
      } else {
        let mut doc2 = Document::new();
        doc2.add(StringField::from_string("id", i.to_string(), Store::No)?);
        writer.add_document_with_similarity(random, doc2, &indexing_similarity_conf)?;
      }

      if random.random_range(0..31) == 0 {
        writer.commit(random)?;
      }
    }

    let reader = Arc::new(self.maybe_wrap_with_merging_reader(writer.get_reader(random)?)?);
    writer.close(random)?;

    let num_threads = TestUtil::next_int(random, 3, 30);
    thread::scope(|scope| -> Result<()> {
      let mut handles = Vec::new();
      for _ in 0..num_threads {
        let reader = reader.clone();
        handles.push(scope.spawn(move || -> Result<()> {
          self.check_norms_vs_doc_values(&reader)?;
          TestUtil::check_reader(&reader)?;
          Ok(())
        }));
      }

      for handle in handles {
        handle
          .join()
          .map_err(|_| LuceneError::illegal_state("norms thread panicked".to_string()))??;
      }
      Ok(())
    })?;

    reader.close()?;
    Ok(())
  }

  fn test_independant_iterators<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = Arc::new(new_directory(random)?);
    let analyzer = MockAnalyzer::new(random);
    let indexing_similarity_conf = CannedNormSimilarity::new(vec![42, 10, 20]);
    let mut conf =
      crate::test_framework::core::util::lucene_test_case::new_index_writer_config_with_analyzer(
        random, analyzer,
      )?;
    conf.set_merge_policy(new_log_merge_policy(random)?);
    let writer = RandomIndexWriter::with_config(random, dir, conf);

    let mut doc = Document::new();
    doc.add(TextField::from_string("indexed", "a", Store::No)?);
    for _ in 0..3 {
      writer.add_document_with_similarity(random, &mut doc, &indexing_similarity_conf)?;
    }

    writer.force_merge(random, 1)?;
    let reader = self.maybe_wrap_with_merging_reader(writer.get_reader(random)?)?;
    let leaf = get_only_leaf_reader(&reader)?;
    let mut n1 = leaf
      .get_norm_values("indexed")?
      .ok_or_else(|| LuceneError::illegal_state("missing norms"))?;
    let mut n2 = leaf
      .get_norm_values("indexed")?
      .ok_or_else(|| LuceneError::illegal_state("missing norms"))?;

    assert_eq!(0, n1.next_doc()?);
    assert_eq!(42, n1.long_value()?);
    assert_eq!(1, n1.next_doc()?);
    assert_eq!(10, n1.long_value()?);
    assert_eq!(0, n2.next_doc()?);
    assert_eq!(42, n2.long_value()?);
    assert_eq!(1, n2.next_doc()?);
    assert_eq!(10, n2.long_value()?);
    assert_eq!(2, n2.next_doc()?);
    assert_eq!(20, n2.long_value()?);
    assert_eq!(2, n1.next_doc()?);
    assert_eq!(20, n1.long_value()?);
    assert_eq!(NO_MORE_DOCS, n1.next_doc()?);
    assert_eq!(NO_MORE_DOCS, n2.next_doc()?);

    reader.close()?;
    writer.close(random)?;
    Ok(())
  }

  fn test_independant_sparse_iterators<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = Arc::new(new_directory(random)?);
    let analyzer = MockAnalyzer::new(random);
    let indexing_similarity_conf = CannedNormSimilarity::new(vec![42, 10, 20]);
    let mut conf =
      crate::test_framework::core::util::lucene_test_case::new_index_writer_config_with_analyzer(
        random, analyzer,
      )?;
    conf.set_merge_policy(new_log_merge_policy(random)?);
    let writer = RandomIndexWriter::with_config(random, dir, conf);

    let mut doc = Document::new();
    doc.add(TextField::from_string("indexed", "a", Store::No)?);
    let mut empty_doc = Document::new();
    for _ in 0..3 {
      writer.add_document_with_similarity(random, &mut doc, &indexing_similarity_conf)?;
      writer.add_document_with_similarity(random, &mut empty_doc, &indexing_similarity_conf)?;
    }

    writer.force_merge(random, 1)?;
    let reader = self.maybe_wrap_with_merging_reader(writer.get_reader(random)?)?;
    let leaf = get_only_leaf_reader(&reader)?;
    let mut n1 = leaf
      .get_norm_values("indexed")?
      .ok_or_else(|| LuceneError::illegal_state("missing norms"))?;
    let mut n2 = leaf
      .get_norm_values("indexed")?
      .ok_or_else(|| LuceneError::illegal_state("missing norms"))?;

    assert_eq!(0, n1.next_doc()?);
    assert_eq!(42, n1.long_value()?);
    assert_eq!(2, n1.next_doc()?);
    assert_eq!(10, n1.long_value()?);
    assert_eq!(0, n2.next_doc()?);
    assert_eq!(42, n2.long_value()?);
    assert_eq!(2, n2.next_doc()?);
    assert_eq!(10, n2.long_value()?);
    assert_eq!(4, n2.next_doc()?);
    assert_eq!(20, n2.long_value()?);
    assert_eq!(4, n1.next_doc()?);
    assert_eq!(20, n1.long_value()?);
    assert_eq!(NO_MORE_DOCS, n1.next_doc()?);
    assert_eq!(NO_MORE_DOCS, n2.next_doc()?);

    reader.close()?;
    writer.close(random)?;
    Ok(())
  }
}

impl<T> BaseIndexFileFormatTestCaseDefaults<T> for BaseNormsFormatTestCaseDefaults
where
  T: BaseNormsFormatTestCase,
{
  fn add_random_fields<R>(_test_case: &T, random: &mut R, document: &mut Document) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    document.add(TextField::from_string(
      "foobar",
      TestUtil::random_simple_string(random),
      Store::No,
    )?);
    Ok(())
  }
}

fn build_random_values<R>(random: &mut R, count: usize) -> Vec<i64>
where
  R: Rng + ?Sized,
{
  let mut values = Vec::with_capacity(count);
  for _ in 0..count {
    values.push(TestUtil::next_long(random, i8::MIN as i64, i8::MAX as i64));
  }
  values
}

pub struct CannedNormSimilarity<N = Vec<i64>> {
  norms: N,
  index: AtomicUsize,
}

impl<N> CannedNormSimilarity<N> {
  pub fn new(norms: N) -> Self {
    CannedNormSimilarity {
      norms,
      index: AtomicUsize::new(0),
    }
  }
}

impl<N> Display for CannedNormSimilarity<N> {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    f.write_str(concat!(module_path!(), "::CannedNormSimilarity"))
  }
}

impl<N> Similarity for CannedNormSimilarity<N>
where
  N: Borrow<Vec<i64>> + Send + Sync + 'static,
{
  fn compute_norm(&self, state: &FieldInvertState) -> Result<i64> {
    assert!(state.get_length() > 0);
    loop {
      let idx = self.index.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
      let norm = self.norms.borrow()[idx];
      if norm != 0 {
        return Ok(norm);
      }
    }
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
