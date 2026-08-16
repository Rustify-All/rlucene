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
use crate::core::analysis::token_attributes::payload_attribute::PayloadAttribute;
use crate::core::analysis::token_stream::TokenStream;
use crate::core::document::document::Document;
use crate::core::document::field::{Field, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::fields::FieldTokenStreamEnum;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::string_field::StringField;
use crate::core::index::BytesRef;
use crate::core::index::directory_reader;
use crate::core::index::fields::Fields as IndexFields;
use crate::core::index::index_reader::{IndexReader, IndexReaderContextType};
use crate::core::index::index_reader_context::IRCLeafReader;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::index::postings_enum::{
  ALL, FREQS, NONE, OFFSETS, PAYLOADS, POSITIONS, PostingsEnum,
};
use crate::core::index::term::Term;
use crate::core::index::term_vectors::TermVectors;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::sort::Sort;
use crate::core::search::sort_field::{SortField, SortFieldType};
use crate::core::search::term_query::TermQuery;
use crate::core::util::attribute_source::{AttributeSource, Attributes};
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::iterator::IteratorExt;
use crate::test_framework::core::analysis::canned_token_stream::CannedTokenStream;
use crate::test_framework::core::analysis::mock_tokenizer::{MockTokenizer, WHITESPACE};
use crate::test_framework::core::analysis::token;
use crate::test_framework::core::index::base_index_file_format_test_case::{
  BaseIndexFileFormatTestCase, BaseIndexFileFormatTestCaseDefaults,
};
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
pub use crate::test_framework::core::index::term_vectors::RandomTokenStreamAttr;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, expect_panic, get_only_leaf_reader, is_night_mode, new_bytes_ref_from_bytes,
  new_directory_shared, new_index_writer_config, new_index_writer_config_with_analyzer,
  random_from_seed, rarely,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::prelude::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, RngExt};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::thread;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadPastLastPositionException {
  IllegalState,
  Assertion,
}

pub struct BaseTermVectorsFormatTestCaseDefaults;

impl<T> BaseIndexFileFormatTestCaseDefaults<T> for BaseTermVectorsFormatTestCaseDefaults
where
  T: BaseTermVectorsFormatTestCase,
{
  fn add_random_fields<R>(test_case: &T, random: &mut R, document: &mut Document) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    for options in test_case.valid_options() {
      let field_type = field_type(options)?;
      let num_fields = random.random_range(0..5);
      for _ in 0..num_fields {
        document.add(Field::from_string(
          format!("f_{options}"),
          TestUtil::random_simple_string_range(random, 0, 2),
          field_type.clone(),
        )?);
      }
    }
    Ok(())
  }
}

pub trait BaseTermVectorsFormatTestCase:
  BaseIndexFileFormatTestCase<Defaults = BaseTermVectorsFormatTestCaseDefaults>
{
  fn valid_options(&self) -> Vec<Options> {
    vec![
      Options::None,
      Options::Positions,
      Options::Offsets,
      Options::PositionsAndOffsets,
      Options::PositionsAndPayloads,
      Options::PositionsOffsetsPayloads,
    ]
  }

  fn random_options<R>(&self, random: &mut R) -> Options
  where
    R: Rng + ?Sized,
  {
    let options = self.valid_options();
    options[random.random_range(0..options.len())]
  }

  fn get_read_past_last_position_exception_class(&self) -> ReadPastLastPositionException {
    ReadPastLastPositionException::IllegalState
  }

  fn test_rare_vectors<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let doc_factory = RandomDocumentFactory::new(random, 10, 20);
    for options in self.valid_options() {
      let num_docs = at_least(random, 200);
      let doc_with_vectors = random.random_range(0..num_docs);
      let empty_doc = Document::new();
      let dir = new_directory_shared(random)?;
      let writer = RandomIndexWriter::new(random, dir)?;
      let field_count = TestUtil::next_int(random, 1, 3) as usize;
      let doc = doc_factory.new_document(random, field_count, 20, options)?;
      for i in 0..num_docs {
        if i == doc_with_vectors {
          writer.add_document(random, add_id(doc.to_document()?, "42")?)?;
        } else {
          writer.add_document(random, empty_doc.clone())?;
        }
      }
      let reader = Arc::new(writer.get_reader(random)?);
      let mut term_vectors = reader.term_vectors()?;
      let doc_with_vectors_id = doc_id(reader.clone(), "42")?;
      for _ in 0..10 {
        let doc_id = random.random_range(0..num_docs);
        let fields = term_vectors.get(doc_id)?;
        if doc_id == doc_with_vectors_id {
          assert_random_document_equals(
            random,
            &doc,
            fields.expect("term vectors should exist"),
            self.get_read_past_last_position_exception_class(),
          )?;
        } else {
          assert!(fields.is_none());
        }
      }
      let fields = term_vectors
        .get(doc_with_vectors_id)?
        .expect("term vectors should exist");
      assert_random_document_equals(
        random,
        &doc,
        fields,
        self.get_read_past_last_position_exception_class(),
      )?;
      writer.close(random)?;
    }
    Ok(())
  }

  fn test_high_freqs<R>(&self, _random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let doc_factory = RandomDocumentFactory::new(_random, 3, 5);
    for options in self.valid_options() {
      if options == Options::None {
        continue;
      }
      let dir = new_directory_shared(_random)?;
      let writer = RandomIndexWriter::new(_random, dir)?;
      let field_count = TestUtil::next_int(_random, 1, 2) as usize;
      let max_term_count = at_least(_random, 2000) as usize;
      let doc = doc_factory.new_document(_random, field_count, max_term_count, options)?;
      writer.add_document(_random, doc.to_document()?)?;
      let reader = writer.get_reader(_random)?;
      let mut term_vectors = reader.term_vectors()?;
      let fields = term_vectors.get(0)?.expect("term vectors should exist");
      assert_random_document_equals(
        _random,
        &doc,
        fields,
        self.get_read_past_last_position_exception_class(),
      )?;
      reader.close()?;
      writer.close(_random)?;
    }
    Ok(())
  }

  fn test_lots_of_fields<R>(&self, _random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let field_count = if is_night_mode() {
      at_least(_random, 100)
    } else {
      at_least(_random, 10)
    } as usize;
    let doc_factory = RandomDocumentFactory::new(_random, field_count, 10);
    for options in self.valid_options() {
      let dir = new_directory_shared(_random)?;
      let writer = RandomIndexWriter::new(_random, dir)?;
      let doc_field_count = TestUtil::next_int(_random, 5, field_count as i32) as usize;
      let doc = doc_factory.new_document(_random, doc_field_count, 5, options)?;
      writer.add_document(_random, doc.to_document()?)?;
      let reader = writer.get_reader(_random)?;
      let mut term_vectors = reader.term_vectors()?;
      let fields = term_vectors.get(0)?.expect("term vectors should exist");
      assert_random_document_equals(
        _random,
        &doc,
        fields,
        self.get_read_past_last_position_exception_class(),
      )?;
      reader.close()?;
      writer.close(_random)?;
    }
    Ok(())
  }

  fn test_mixed_options<R>(&self, _random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_fields = TestUtil::next_int(_random, 1, 3) as usize;
    let doc_factory = RandomDocumentFactory::new(_random, num_fields, 10);
    for options1 in self.valid_options() {
      for options2 in self.valid_options() {
        if options1 == options2 {
          continue;
        }
        let dir = new_directory_shared(_random)?;
        let writer = RandomIndexWriter::new(_random, dir)?;
        let doc1 = doc_factory.new_document(_random, num_fields, 20, options1)?;
        let doc2 = doc_factory.new_document(_random, num_fields, 20, options2)?;
        writer.add_document(_random, add_id(doc1.to_document()?, "1")?)?;
        writer.add_document(_random, add_id(doc2.to_document()?, "2")?)?;

        let reader = Arc::new(writer.get_reader(_random)?);
        let doc1_id = doc_id(reader.clone(), "1")?;
        let doc2_id = doc_id(reader.clone(), "2")?;
        let mut term_vectors = reader.term_vectors()?;

        let fields1 = term_vectors
          .get(doc1_id)?
          .expect("term vectors should exist");
        assert_random_document_equals(
          _random,
          &doc1,
          fields1,
          self.get_read_past_last_position_exception_class(),
        )?;
        let fields2 = term_vectors
          .get(doc2_id)?
          .expect("term vectors should exist");
        assert_random_document_equals(
          _random,
          &doc2,
          fields2,
          self.get_read_past_last_position_exception_class(),
        )?;
        reader.close()?;
        writer.close(_random)?;
      }
    }
    Ok(())
  }

  fn test_random<R>(&self, _random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let doc_factory = RandomDocumentFactory::new(_random, 5, 20);
    let num_docs = at_least(_random, 50) as usize;
    let mut docs = Vec::with_capacity(num_docs);
    for _ in 0..num_docs {
      let field_count = TestUtil::next_int(_random, 1, 3) as usize;
      let max_term_count = TestUtil::next_int(_random, 10, 50) as usize;
      let options = self.random_options(_random);
      docs.push(doc_factory.new_document(_random, field_count, max_term_count, options)?);
    }
    let dir = new_directory_shared(_random)?;
    let writer = RandomIndexWriter::new(_random, dir)?;
    for (i, doc) in docs.iter().enumerate() {
      writer.add_document(_random, add_id(doc.to_document()?, &i.to_string())?)?;
    }
    let reader = Arc::new(writer.get_reader(_random)?);
    let mut term_vectors = reader.term_vectors()?;
    for (i, doc) in docs.iter().enumerate() {
      let doc_id = doc_id(reader.clone(), &i.to_string())?;
      let fields = term_vectors
        .get(doc_id)?
        .expect("term vectors should exist");
      assert_random_document_equals(
        _random,
        doc,
        fields,
        self.get_read_past_last_position_exception_class(),
      )?;
    }
    reader.close()?;
    writer.close(_random)?;
    Ok(())
  }
  fn do_test_merge<R>(
    &self,
    random: &mut R,
    index_sort: Option<Sort>,
    allow_deletes: bool,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let doc_factory = RandomDocumentFactory::new(random, 5, 20);
    let num_docs = if is_night_mode() {
      at_least(random, 100)
    } else {
      at_least(random, 10)
    } as usize;
    for options in self.valid_options() {
      let mut docs = HashMap::new();
      for i in 0..num_docs {
        let field_count = TestUtil::next_int(random, 1, 3) as usize;
        let max_term_count = at_least(random, 10) as usize;
        docs.insert(
          i.to_string(),
          doc_factory.new_document(random, field_count, max_term_count, options)?,
        );
      }
      let dir = new_directory_shared(random)?;
      let mut iwc = new_index_writer_config(random)?;
      if let Some(sort) = index_sort.clone() {
        iwc.set_index_sort(sort)?;
      }
      let writer = RandomIndexWriter::with_config(random, dir, iwc);
      let mut live_doc_ids = Vec::new();
      let mut ids = docs.keys().cloned().collect::<Vec<_>>();
      ids.shuffle(random);
      let verify_term_vectors = |random: &mut R,
                                 docs: &HashMap<String, RandomDocument>,
                                 live_doc_ids: &[String]|
       -> Result<()> {
        let reader = Arc::new(self.maybe_wrap_with_merging_reader(writer.get_reader(random)?)?);
        let mut term_vectors = reader.term_vectors()?;
        for id in live_doc_ids {
          let doc_id = doc_id(reader.clone(), id)?;
          let fields = term_vectors
            .get(doc_id)?
            .expect("term vectors should exist");
          assert_random_document_equals(
            random,
            docs.get(id).expect("live doc id must exist"),
            fields,
            self.get_read_past_last_position_exception_class(),
          )?;
        }
        reader.close()?;
        Ok(())
      };
      for id in ids {
        let mut doc = add_id(
          docs
            .get(&id)
            .expect("document id must exist")
            .to_document()?,
          &id,
        )?;
        if let Some(index_sort) = index_sort.as_ref() {
          for sort_field in index_sort.get_sort() {
            if let Some(field) = sort_field.get_field() {
              doc.add(NumericDocValuesField::new(
                field.to_string(),
                TestUtil::next_int(random, 0, 1024) as i64,
              ));
            }
          }
        }
        if random.random_range(0..100) < 5 {
          // add via foreign writer
          let mut other_iwc = new_index_writer_config(random)?;
          if let Some(sort) = index_sort.clone() {
            other_iwc.set_index_sort(sort)?;
          }
          let other_dir = new_directory_shared(random)?;
          let other_iw = RandomIndexWriter::with_config(random, other_dir, other_iwc);
          other_iw.add_document(random, doc)?;
          let other_reader = other_iw.get_reader(random)?;
          TestUtil::add_indexes_slowly(&writer.w, &[&other_reader])?;
          other_reader.close()?;
          other_iw.close(random)?;
        } else {
          writer.add_document(random, doc)?;
        }
        live_doc_ids.push(id);
        if allow_deletes && random.random_range(0..100) < 20 {
          let delete_id = live_doc_ids.remove(random.random_range(0..live_doc_ids.len()));
          writer.delete_documents_with_terms(random, vec![Term::from_text("id", delete_id)])?;
        }
        if rarely(random) {
          writer.commit(random)?;
          verify_term_vectors(random, &docs, &live_doc_ids)?;
        }
        if rarely(random) {
          writer.force_merge(random, 1)?;
          verify_term_vectors(random, &docs, &live_doc_ids)?;
        }
      }
      verify_term_vectors(random, &docs, &live_doc_ids)?;
      writer.force_merge(random, 1)?;
      verify_term_vectors(random, &docs, &live_doc_ids)?;
      writer.close(random)?;
    }
    Ok(())
  }

  fn test_merge<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    self.do_test_merge(random, None, false)
  }

  fn test_merge_with_deletes<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    self.do_test_merge(random, None, true)
  }

  fn test_merge_with_index_sort<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let mut sort_fields = Vec::new();
    for i in 0..TestUtil::next_int(random, 1, 2) {
      sort_fields.push(SortField::new(
        Some(format!("sort_field_{i}")),
        SortFieldType::Long,
      )?);
    }
    self.do_test_merge(random, Some(Sort::with_fields(sort_fields)?), false)
  }

  fn test_merge_with_index_sort_and_deletes<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let mut sort_fields = Vec::new();
    for i in 0..TestUtil::next_int(random, 1, 2) {
      sort_fields.push(SortField::new(
        Some(format!("sort_field_{i}")),
        SortFieldType::Long,
      )?);
    }
    self.do_test_merge(random, Some(Sort::with_fields(sort_fields)?), true)
  }

  // run random tests from different threads to make sure the per-thread clones
  // don't share mutable data
  fn test_clone<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let doc_factory = RandomDocumentFactory::new(random, 5, 20);
    let num_docs = at_least(random, 50) as usize;
    for options in self.valid_options() {
      let mut docs = Vec::with_capacity(num_docs);
      for _ in 0..num_docs {
        let field_count = TestUtil::next_int(random, 1, 3) as usize;
        let max_term_count = at_least(random, 10) as usize;
        docs.push(doc_factory.new_document(random, field_count, max_term_count, options)?);
      }
      let dir = new_directory_shared(random)?;
      let writer = RandomIndexWriter::new(random, dir)?;
      for (i, doc) in docs.iter().enumerate() {
        writer.add_document(random, add_id(doc.to_document()?, &i.to_string())?)?;
      }
      let reader = Arc::new(writer.get_reader(random)?);
      let mut term_vectors = reader.term_vectors()?;
      for (i, doc) in docs.iter().enumerate() {
        let doc_id = doc_id(reader.clone(), &i.to_string())?;
        let fields = term_vectors
          .get(doc_id)?
          .expect("term vectors should exist");
        assert_random_document_equals(
          random,
          doc,
          fields,
          self.get_read_past_last_position_exception_class(),
        )?;
      }
      drop(term_vectors);

      let thread_seeds = [random.random(), random.random()];
      let read_past_last_position_exception = self.get_read_past_last_position_exception_class();
      thread::scope(|scope| -> Result<()> {
        let mut threads = Vec::with_capacity(thread_seeds.len());
        for seed in thread_seeds {
          let reader = reader.clone();
          let docs = &docs;
          threads.push(scope.spawn(move || -> Result<()> {
            let mut thread_random = random_from_seed(seed);
            let mut term_vectors = reader.term_vectors()?;
            let mut i = 0;
            while i < at_least(&mut thread_random, 100) {
              let idx = thread_random.random_range(0..docs.len());
              let doc_id = doc_id(reader.clone(), &idx.to_string())?;
              let fields = term_vectors
                .get(doc_id)?
                .expect("term vectors should exist");
              assert_random_document_equals(
                &mut thread_random,
                &docs[idx],
                fields,
                read_past_last_position_exception,
              )?;
              i += 1;
            }
            Ok(())
          }));
        }
        for thread in threads {
          thread.join().map_err(|payload| {
            LuceneError::tragedy_from_panic("test thread panicked", payload.as_ref())
          })??;
        }
        Ok(())
      })?;
      reader.close()?;
      writer.close(random)?;
    }
    Ok(())
  }
  fn test_postings_enum_freqs<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let analyzer = MockTokenizerAnalyzer::new(random);
    let iwc = new_index_writer_config_with_analyzer(random, analyzer)?;
    let iw = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc = Document::new();
    let mut ft = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
    ft.set_store_term_vectors(true)?;
    doc.add(Field::new("foo", "bar bar", ft));
    iw.add_document(doc)?;

    let reader = directory_reader::open_from_writer(&iw)?;

    let leaf = get_only_leaf_reader(&reader)?;
    let mut term_vectors = leaf.term_vectors()?;
    let terms = term_vectors.get_field_terms(0, "foo")?.unwrap();
    let mut terms_enum = terms.iterator()?;
    assert_eq!(
      &BytesRef::from_string("bar"),
      terms_enum.next()?.unwrap().as_ref()
    );
    // simple use (FREQS)
    let mut postings = terms_enum.postings(None)?;
    assert_eq!(-1, postings.doc_id());
    assert_eq!(0, postings.next_doc()?);
    assert_eq!(2, postings.freq()?);
    assert_eq!(NO_MORE_DOCS, postings.next_doc()?);

    let mut postings2 = terms_enum.postings(Some(postings))?;
    assert_eq!(-1, postings2.doc_id());
    assert_eq!(0, postings2.next_doc()?);
    assert_eq!(2, postings2.freq()?);
    assert_eq!(NO_MORE_DOCS, postings2.next_doc()?);
    // asking for docs only: ok
    let mut docs_only = terms_enum.postings_with_flags(None, NONE as i32)?;
    assert_eq!(-1, docs_only.doc_id());
    assert_eq!(0, docs_only.next_doc()?);
    assert!(docs_only.freq()? == 1 || docs_only.freq()? == 2);
    assert_eq!(NO_MORE_DOCS, docs_only.next_doc()?);
    // reuse that too
    let mut docs_only2 = terms_enum.postings_with_flags(Some(docs_only), NONE as i32)?;
    // and it had better work
    assert_eq!(-1, docs_only2.doc_id());
    assert_eq!(0, docs_only2.next_doc()?);
    // we don't define what it is, but if its something else, we should look into it?
    assert!(docs_only2.freq()? == 1 || docs_only2.freq()? == 2);
    assert_eq!(NO_MORE_DOCS, docs_only2.next_doc()?);

    for flag in [NONE, FREQS, POSITIONS, PAYLOADS, OFFSETS, ALL] {
      let mut postings = terms_enum.postings_with_flags(None, flag as i32)?;
      assert_eq!(-1, postings.doc_id());
      assert_eq!(0, postings.next_doc()?);
      if flag != NONE {
        assert_eq!(2, postings.freq()?);
      }
      assert_eq!(NO_MORE_DOCS, postings.next_doc()?);

      let mut postings2 = terms_enum.postings_with_flags(Some(postings), flag as i32)?;
      assert_eq!(-1, postings2.doc_id());
      assert_eq!(0, postings2.next_doc()?);
      if flag != NONE {
        assert_eq!(2, postings2.freq()?);
      }
      assert_eq!(NO_MORE_DOCS, postings2.next_doc()?);
    }
    reader.close()?;
    iw.close()?;
    Ok(())
  }
  fn test_postings_enum_positions<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let analyzer = MockTokenizerAnalyzer::new(random);
    let iwc = new_index_writer_config_with_analyzer(random, analyzer)?;
    let iw = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc = Document::new();
    let mut ft = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
    ft.set_store_term_vectors(true)?;
    ft.set_store_term_vector_positions(true)?;
    doc.add(Field::new("foo", "bar bar", ft));
    iw.add_document(doc)?;

    let reader = directory_reader::open_from_writer(&iw)?;

    let leaf = get_only_leaf_reader(&reader)?;
    let mut term_vectors = leaf.term_vectors()?;
    let terms = term_vectors.get_field_terms(0, "foo")?.unwrap();
    let mut terms_enum = terms.iterator()?;
    assert_eq!(
      &BytesRef::from_string("bar"),
      terms_enum.next()?.unwrap().as_ref()
    );

    let mut postings = terms_enum.postings(None)?;
    assert_eq!(-1, postings.doc_id());
    assert_eq!(0, postings.next_doc()?);
    assert_eq!(2, postings.freq()?);
    assert_eq!(NO_MORE_DOCS, postings.next_doc()?);

    let mut postings2 = terms_enum.postings(Some(postings))?;
    assert_eq!(-1, postings2.doc_id());
    assert_eq!(0, postings2.next_doc()?);
    assert_eq!(2, postings2.freq()?);
    assert_eq!(NO_MORE_DOCS, postings2.next_doc()?);

    let mut docs_only = terms_enum.postings_with_flags(None, NONE as i32)?;
    assert_eq!(-1, docs_only.doc_id());
    assert_eq!(0, docs_only.next_doc()?);
    assert!(docs_only.freq()? == 1 || docs_only.freq()? == 2);
    assert_eq!(NO_MORE_DOCS, docs_only.next_doc()?);

    let mut docs_only2 = terms_enum.postings_with_flags(Some(docs_only), NONE as i32)?;
    assert_eq!(-1, docs_only2.doc_id());
    assert_eq!(0, docs_only2.next_doc()?);
    assert!(docs_only2.freq()? == 1 || docs_only2.freq()? == 2);
    assert_eq!(NO_MORE_DOCS, docs_only2.next_doc()?);

    let mut docs_and_positions_enum = terms_enum.postings_with_flags(None, POSITIONS as i32)?;
    assert_eq!(-1, docs_and_positions_enum.doc_id());
    assert_eq!(0, docs_and_positions_enum.next_doc()?);
    assert_eq!(2, docs_and_positions_enum.freq()?);
    assert_eq!(0, docs_and_positions_enum.next_position()?);
    assert_eq!(-1, docs_and_positions_enum.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum.end_offset()?);
    assert!(docs_and_positions_enum.get_payload()?.is_none());
    assert_eq!(1, docs_and_positions_enum.next_position()?);
    assert_eq!(-1, docs_and_positions_enum.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum.end_offset()?);
    assert!(docs_and_positions_enum.get_payload()?.is_none());
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum.next_doc()?);

    let mut docs_and_positions_enum2 =
      terms_enum.postings_with_flags(Some(docs_and_positions_enum), POSITIONS as i32)?;
    assert_eq!(-1, docs_and_positions_enum2.doc_id());
    assert_eq!(0, docs_and_positions_enum2.next_doc()?);
    assert_eq!(2, docs_and_positions_enum2.freq()?);
    assert_eq!(0, docs_and_positions_enum2.next_position()?);
    assert_eq!(-1, docs_and_positions_enum2.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum2.end_offset()?);
    assert!(docs_and_positions_enum2.get_payload()?.is_none());
    assert_eq!(1, docs_and_positions_enum2.next_position()?);
    assert_eq!(-1, docs_and_positions_enum2.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum2.end_offset()?);
    assert!(docs_and_positions_enum2.get_payload()?.is_none());
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum2.next_doc()?);

    let mut docs_and_positions_enum = terms_enum.postings_with_flags(None, PAYLOADS as i32)?;
    assert_eq!(-1, docs_and_positions_enum.doc_id());
    assert_eq!(0, docs_and_positions_enum.next_doc()?);
    assert_eq!(2, docs_and_positions_enum.freq()?);
    assert_eq!(0, docs_and_positions_enum.next_position()?);
    assert_eq!(-1, docs_and_positions_enum.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum.end_offset()?);
    assert!(docs_and_positions_enum.get_payload()?.is_none());
    assert_eq!(1, docs_and_positions_enum.next_position()?);
    assert_eq!(-1, docs_and_positions_enum.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum.end_offset()?);
    assert!(docs_and_positions_enum.get_payload()?.is_none());
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum.next_doc()?);

    let mut docs_and_positions_enum2 =
      terms_enum.postings_with_flags(Some(docs_and_positions_enum), PAYLOADS as i32)?;
    assert_eq!(-1, docs_and_positions_enum2.doc_id());
    assert_eq!(0, docs_and_positions_enum2.next_doc()?);
    assert_eq!(2, docs_and_positions_enum2.freq()?);
    assert_eq!(0, docs_and_positions_enum2.next_position()?);
    assert_eq!(-1, docs_and_positions_enum2.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum2.end_offset()?);
    assert!(docs_and_positions_enum2.get_payload()?.is_none());
    assert_eq!(1, docs_and_positions_enum2.next_position()?);
    assert_eq!(-1, docs_and_positions_enum2.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum2.end_offset()?);
    assert!(docs_and_positions_enum2.get_payload()?.is_none());
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum2.next_doc()?);

    let mut docs_and_positions_enum = terms_enum.postings_with_flags(None, OFFSETS as i32)?;
    assert_eq!(-1, docs_and_positions_enum.doc_id());
    assert_eq!(0, docs_and_positions_enum.next_doc()?);
    assert_eq!(2, docs_and_positions_enum.freq()?);
    assert_eq!(0, docs_and_positions_enum.next_position()?);
    assert_eq!(-1, docs_and_positions_enum.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum.end_offset()?);
    assert!(docs_and_positions_enum.get_payload()?.is_none());
    assert_eq!(1, docs_and_positions_enum.next_position()?);
    assert_eq!(-1, docs_and_positions_enum.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum.end_offset()?);
    assert!(docs_and_positions_enum.get_payload()?.is_none());
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum.next_doc()?);

    let mut docs_and_positions_enum2 =
      terms_enum.postings_with_flags(Some(docs_and_positions_enum), OFFSETS as i32)?;
    assert_eq!(-1, docs_and_positions_enum2.doc_id());
    assert_eq!(0, docs_and_positions_enum2.next_doc()?);
    assert_eq!(2, docs_and_positions_enum2.freq()?);
    assert_eq!(0, docs_and_positions_enum2.next_position()?);
    assert_eq!(-1, docs_and_positions_enum2.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum2.end_offset()?);
    assert!(docs_and_positions_enum2.get_payload()?.is_none());
    assert_eq!(1, docs_and_positions_enum2.next_position()?);
    assert_eq!(-1, docs_and_positions_enum2.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum2.end_offset()?);
    assert!(docs_and_positions_enum2.get_payload()?.is_none());
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum2.next_doc()?);

    let mut docs_and_positions_enum = terms_enum.postings_with_flags(None, ALL as i32)?;
    assert_eq!(-1, docs_and_positions_enum.doc_id());
    assert_eq!(0, docs_and_positions_enum.next_doc()?);
    assert_eq!(2, docs_and_positions_enum.freq()?);
    assert_eq!(0, docs_and_positions_enum.next_position()?);
    assert_eq!(-1, docs_and_positions_enum.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum.end_offset()?);
    assert!(docs_and_positions_enum.get_payload()?.is_none());
    assert_eq!(1, docs_and_positions_enum.next_position()?);
    assert_eq!(-1, docs_and_positions_enum.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum.end_offset()?);
    assert!(docs_and_positions_enum.get_payload()?.is_none());
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum.next_doc()?);

    let mut docs_and_positions_enum2 =
      terms_enum.postings_with_flags(Some(docs_and_positions_enum), ALL as i32)?;
    assert_eq!(-1, docs_and_positions_enum2.doc_id());
    assert_eq!(0, docs_and_positions_enum2.next_doc()?);
    assert_eq!(2, docs_and_positions_enum2.freq()?);
    assert_eq!(0, docs_and_positions_enum2.next_position()?);
    assert_eq!(-1, docs_and_positions_enum2.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum2.end_offset()?);
    assert!(docs_and_positions_enum2.get_payload()?.is_none());
    assert_eq!(1, docs_and_positions_enum2.next_position()?);
    assert_eq!(-1, docs_and_positions_enum2.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum2.end_offset()?);
    assert!(docs_and_positions_enum2.get_payload()?.is_none());
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum2.next_doc()?);

    reader.close()?;
    iw.close()?;
    Ok(())
  }
  fn test_postings_enum_offsets<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let analyzer = MockTokenizerAnalyzer::new(random);
    let iwc = new_index_writer_config_with_analyzer(random, analyzer)?;
    let iw = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc = Document::new();
    let mut ft = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
    ft.set_store_term_vectors(true)?;
    ft.set_store_term_vector_positions(true)?;
    ft.set_store_term_vector_offsets(true)?;
    doc.add(Field::new("foo", "bar bar", ft));
    iw.add_document(doc)?;

    let reader = directory_reader::open_from_writer(&iw)?;

    let leaf = get_only_leaf_reader(&reader)?;
    let mut term_vectors = leaf.term_vectors()?;
    let terms = term_vectors.get_field_terms(0, "foo")?.unwrap();
    let mut terms_enum = terms.iterator()?;
    assert_eq!(
      &BytesRef::from_string("bar"),
      terms_enum.next()?.unwrap().as_ref()
    );

    let mut postings = terms_enum.postings(None)?;
    assert_eq!(-1, postings.doc_id());
    assert_eq!(0, postings.next_doc()?);
    assert_eq!(2, postings.freq()?);
    assert_eq!(NO_MORE_DOCS, postings.next_doc()?);

    let mut postings2 = terms_enum.postings(Some(postings))?;
    assert_eq!(-1, postings2.doc_id());
    assert_eq!(0, postings2.next_doc()?);
    assert_eq!(2, postings2.freq()?);
    assert_eq!(NO_MORE_DOCS, postings2.next_doc()?);

    let mut docs_only = terms_enum.postings_with_flags(None, NONE as i32)?;
    assert_eq!(-1, docs_only.doc_id());
    assert_eq!(0, docs_only.next_doc()?);
    assert!(docs_only.freq()? == 1 || docs_only.freq()? == 2);
    assert_eq!(NO_MORE_DOCS, docs_only.next_doc()?);

    let mut docs_only2 = terms_enum.postings_with_flags(Some(docs_only), NONE as i32)?;
    assert_eq!(-1, docs_only2.doc_id());
    assert_eq!(0, docs_only2.next_doc()?);
    assert!(docs_only2.freq()? == 1 || docs_only2.freq()? == 2);
    assert_eq!(NO_MORE_DOCS, docs_only2.next_doc()?);

    let mut docs_and_positions_enum = terms_enum.postings_with_flags(None, POSITIONS as i32)?;
    assert_eq!(-1, docs_and_positions_enum.doc_id());
    assert_eq!(0, docs_and_positions_enum.next_doc()?);
    assert_eq!(2, docs_and_positions_enum.freq()?);
    assert_eq!(0, docs_and_positions_enum.next_position()?);
    assert!(
      docs_and_positions_enum.start_offset()? == -1 || docs_and_positions_enum.start_offset()? == 0
    );
    assert!(
      docs_and_positions_enum.end_offset()? == -1 || docs_and_positions_enum.end_offset()? == 3
    );
    assert!(docs_and_positions_enum.get_payload()?.is_none());
    assert_eq!(1, docs_and_positions_enum.next_position()?);
    assert!(
      docs_and_positions_enum.start_offset()? == -1 || docs_and_positions_enum.start_offset()? == 4
    );
    assert!(
      docs_and_positions_enum.end_offset()? == -1 || docs_and_positions_enum.end_offset()? == 7
    );
    assert!(docs_and_positions_enum.get_payload()?.is_none());
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum.next_doc()?);

    let mut docs_and_positions_enum2 =
      terms_enum.postings_with_flags(Some(docs_and_positions_enum), POSITIONS as i32)?;
    assert_eq!(-1, docs_and_positions_enum2.doc_id());
    assert_eq!(0, docs_and_positions_enum2.next_doc()?);
    assert_eq!(2, docs_and_positions_enum2.freq()?);
    assert_eq!(0, docs_and_positions_enum2.next_position()?);
    assert!(
      docs_and_positions_enum2.start_offset()? == -1
        || docs_and_positions_enum2.start_offset()? == 0
    );
    assert!(
      docs_and_positions_enum2.end_offset()? == -1 || docs_and_positions_enum2.end_offset()? == 3
    );
    assert!(docs_and_positions_enum2.get_payload()?.is_none());
    assert_eq!(1, docs_and_positions_enum2.next_position()?);
    assert!(
      docs_and_positions_enum2.start_offset()? == -1
        || docs_and_positions_enum2.start_offset()? == 4
    );
    assert!(
      docs_and_positions_enum2.end_offset()? == -1 || docs_and_positions_enum2.end_offset()? == 7
    );
    assert!(docs_and_positions_enum2.get_payload()?.is_none());
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum2.next_doc()?);

    let mut docs_and_positions_enum = terms_enum.postings_with_flags(None, PAYLOADS as i32)?;
    assert_eq!(-1, docs_and_positions_enum.doc_id());
    assert_eq!(0, docs_and_positions_enum.next_doc()?);
    assert_eq!(2, docs_and_positions_enum.freq()?);
    assert_eq!(0, docs_and_positions_enum.next_position()?);
    assert!(
      docs_and_positions_enum.start_offset()? == -1 || docs_and_positions_enum.start_offset()? == 0
    );
    assert!(
      docs_and_positions_enum.end_offset()? == -1 || docs_and_positions_enum.end_offset()? == 3
    );
    assert!(docs_and_positions_enum.get_payload()?.is_none());
    assert_eq!(1, docs_and_positions_enum.next_position()?);
    assert!(
      docs_and_positions_enum.start_offset()? == -1 || docs_and_positions_enum.start_offset()? == 4
    );
    assert!(
      docs_and_positions_enum.end_offset()? == -1 || docs_and_positions_enum.end_offset()? == 7
    );
    assert!(docs_and_positions_enum.get_payload()?.is_none());
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum.next_doc()?);

    let mut docs_and_positions_enum2 =
      terms_enum.postings_with_flags(Some(docs_and_positions_enum), PAYLOADS as i32)?;
    assert_eq!(-1, docs_and_positions_enum2.doc_id());
    assert_eq!(0, docs_and_positions_enum2.next_doc()?);
    assert_eq!(2, docs_and_positions_enum2.freq()?);
    assert_eq!(0, docs_and_positions_enum2.next_position()?);
    assert!(
      docs_and_positions_enum2.start_offset()? == -1
        || docs_and_positions_enum2.start_offset()? == 0
    );
    assert!(
      docs_and_positions_enum2.end_offset()? == -1 || docs_and_positions_enum2.end_offset()? == 3
    );
    assert!(docs_and_positions_enum2.get_payload()?.is_none());
    assert_eq!(1, docs_and_positions_enum2.next_position()?);
    assert!(
      docs_and_positions_enum2.start_offset()? == -1
        || docs_and_positions_enum2.start_offset()? == 4
    );
    assert!(
      docs_and_positions_enum2.end_offset()? == -1 || docs_and_positions_enum2.end_offset()? == 7
    );
    assert!(docs_and_positions_enum2.get_payload()?.is_none());
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum2.next_doc()?);

    let mut docs_and_positions_enum = terms_enum.postings_with_flags(None, OFFSETS as i32)?;
    assert_eq!(-1, docs_and_positions_enum.doc_id());
    assert_eq!(0, docs_and_positions_enum.next_doc()?);
    assert_eq!(2, docs_and_positions_enum.freq()?);
    assert_eq!(0, docs_and_positions_enum.next_position()?);
    assert_eq!(0, docs_and_positions_enum.start_offset()?);
    assert_eq!(3, docs_and_positions_enum.end_offset()?);
    assert!(docs_and_positions_enum.get_payload()?.is_none());
    assert_eq!(1, docs_and_positions_enum.next_position()?);
    assert_eq!(4, docs_and_positions_enum.start_offset()?);
    assert_eq!(7, docs_and_positions_enum.end_offset()?);
    assert!(docs_and_positions_enum.get_payload()?.is_none());
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum.next_doc()?);

    let mut docs_and_positions_enum2 =
      terms_enum.postings_with_flags(Some(docs_and_positions_enum), OFFSETS as i32)?;
    assert_eq!(-1, docs_and_positions_enum2.doc_id());
    assert_eq!(0, docs_and_positions_enum2.next_doc()?);
    assert_eq!(2, docs_and_positions_enum2.freq()?);
    assert_eq!(0, docs_and_positions_enum2.next_position()?);
    assert_eq!(0, docs_and_positions_enum2.start_offset()?);
    assert_eq!(3, docs_and_positions_enum2.end_offset()?);
    assert!(docs_and_positions_enum2.get_payload()?.is_none());
    assert_eq!(1, docs_and_positions_enum2.next_position()?);
    assert_eq!(4, docs_and_positions_enum2.start_offset()?);
    assert_eq!(7, docs_and_positions_enum2.end_offset()?);
    assert!(docs_and_positions_enum2.get_payload()?.is_none());
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum2.next_doc()?);

    let mut docs_and_positions_enum = terms_enum.postings_with_flags(None, ALL as i32)?;
    assert_eq!(-1, docs_and_positions_enum.doc_id());
    assert_eq!(0, docs_and_positions_enum.next_doc()?);
    assert_eq!(2, docs_and_positions_enum.freq()?);
    assert_eq!(0, docs_and_positions_enum.next_position()?);
    assert_eq!(0, docs_and_positions_enum.start_offset()?);
    assert_eq!(3, docs_and_positions_enum.end_offset()?);
    assert!(docs_and_positions_enum.get_payload()?.is_none());
    assert_eq!(1, docs_and_positions_enum.next_position()?);
    assert_eq!(4, docs_and_positions_enum.start_offset()?);
    assert_eq!(7, docs_and_positions_enum.end_offset()?);
    assert!(docs_and_positions_enum.get_payload()?.is_none());
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum.next_doc()?);

    let mut docs_and_positions_enum2 =
      terms_enum.postings_with_flags(Some(docs_and_positions_enum), ALL as i32)?;
    assert_eq!(-1, docs_and_positions_enum2.doc_id());
    assert_eq!(0, docs_and_positions_enum2.next_doc()?);
    assert_eq!(2, docs_and_positions_enum2.freq()?);
    assert_eq!(0, docs_and_positions_enum2.next_position()?);
    assert_eq!(0, docs_and_positions_enum2.start_offset()?);
    assert_eq!(3, docs_and_positions_enum2.end_offset()?);
    assert!(docs_and_positions_enum2.get_payload()?.is_none());
    assert_eq!(1, docs_and_positions_enum2.next_position()?);
    assert_eq!(4, docs_and_positions_enum2.start_offset()?);
    assert_eq!(7, docs_and_positions_enum2.end_offset()?);
    assert!(docs_and_positions_enum2.get_payload()?.is_none());
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum2.next_doc()?);

    reader.close()?;
    iw.close()?;
    Ok(())
  }

  fn test_postings_enum_offsets_without_positions<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let analyzer = MockTokenizerAnalyzer::new(random);
    let iwc = new_index_writer_config_with_analyzer(random, analyzer)?;
    let iw = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc = Document::new();
    let mut ft = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
    ft.set_store_term_vectors(true)?;
    ft.set_store_term_vector_offsets(true)?;
    doc.add(Field::new("foo", "bar bar", ft));
    iw.add_document(doc)?;

    let reader = directory_reader::open_from_writer(&iw)?;

    let leaf = get_only_leaf_reader(&reader)?;
    let mut term_vectors = leaf.term_vectors()?;
    let terms = term_vectors.get_field_terms(0, "foo")?.unwrap();
    let mut terms_enum = terms.iterator()?;
    assert_eq!(
      &BytesRef::from_string("bar"),
      terms_enum.next()?.unwrap().as_ref()
    );

    let mut postings = terms_enum.postings(None)?;
    assert_eq!(-1, postings.doc_id());
    assert_eq!(0, postings.next_doc()?);
    assert_eq!(2, postings.freq()?);
    assert_eq!(NO_MORE_DOCS, postings.next_doc()?);

    let mut postings2 = terms_enum.postings(Some(postings))?;
    assert_eq!(-1, postings2.doc_id());
    assert_eq!(0, postings2.next_doc()?);
    assert_eq!(2, postings2.freq()?);
    assert_eq!(NO_MORE_DOCS, postings2.next_doc()?);

    let mut docs_only = terms_enum.postings_with_flags(None, NONE as i32)?;
    assert_eq!(-1, docs_only.doc_id());
    assert_eq!(0, docs_only.next_doc()?);
    assert!(docs_only.freq()? == 1 || docs_only.freq()? == 2);
    assert_eq!(NO_MORE_DOCS, docs_only.next_doc()?);

    let mut docs_only2 = terms_enum.postings_with_flags(Some(docs_only), NONE as i32)?;
    assert_eq!(-1, docs_only2.doc_id());
    assert_eq!(0, docs_only2.next_doc()?);
    assert!(docs_only2.freq()? == 1 || docs_only2.freq()? == 2);
    assert_eq!(NO_MORE_DOCS, docs_only2.next_doc()?);

    let mut docs_and_positions_enum = terms_enum.postings_with_flags(None, POSITIONS as i32)?;
    assert_eq!(-1, docs_and_positions_enum.doc_id());
    assert_eq!(0, docs_and_positions_enum.next_doc()?);
    assert_eq!(2, docs_and_positions_enum.freq()?);
    assert_eq!(-1, docs_and_positions_enum.next_position()?);
    assert!(
      docs_and_positions_enum.start_offset()? == -1 || docs_and_positions_enum.start_offset()? == 0
    );
    assert!(
      docs_and_positions_enum.end_offset()? == -1 || docs_and_positions_enum.end_offset()? == 3
    );
    assert!(docs_and_positions_enum.get_payload()?.is_none());
    assert_eq!(-1, docs_and_positions_enum.next_position()?);
    assert!(
      docs_and_positions_enum.start_offset()? == -1 || docs_and_positions_enum.start_offset()? == 4
    );
    assert!(
      docs_and_positions_enum.end_offset()? == -1 || docs_and_positions_enum.end_offset()? == 7
    );
    assert!(docs_and_positions_enum.get_payload()?.is_none());
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum.next_doc()?);

    let mut docs_and_positions_enum2 =
      terms_enum.postings_with_flags(Some(docs_and_positions_enum), POSITIONS as i32)?;
    assert_eq!(-1, docs_and_positions_enum2.doc_id());
    assert_eq!(0, docs_and_positions_enum2.next_doc()?);
    assert_eq!(2, docs_and_positions_enum2.freq()?);
    assert_eq!(-1, docs_and_positions_enum2.next_position()?);
    assert!(
      docs_and_positions_enum2.start_offset()? == -1
        || docs_and_positions_enum2.start_offset()? == 0
    );
    assert!(
      docs_and_positions_enum2.end_offset()? == -1 || docs_and_positions_enum2.end_offset()? == 3
    );
    assert!(docs_and_positions_enum2.get_payload()?.is_none());
    assert_eq!(-1, docs_and_positions_enum2.next_position()?);
    assert!(
      docs_and_positions_enum2.start_offset()? == -1
        || docs_and_positions_enum2.start_offset()? == 4
    );
    assert!(
      docs_and_positions_enum2.end_offset()? == -1 || docs_and_positions_enum2.end_offset()? == 7
    );
    assert!(docs_and_positions_enum2.get_payload()?.is_none());
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum2.next_doc()?);

    let mut docs_and_positions_enum = terms_enum.postings_with_flags(None, PAYLOADS as i32)?;
    assert_eq!(-1, docs_and_positions_enum.doc_id());
    assert_eq!(0, docs_and_positions_enum.next_doc()?);
    assert_eq!(2, docs_and_positions_enum.freq()?);
    assert_eq!(-1, docs_and_positions_enum.next_position()?);
    assert!(
      docs_and_positions_enum.start_offset()? == -1 || docs_and_positions_enum.start_offset()? == 0
    );
    assert!(
      docs_and_positions_enum.end_offset()? == -1 || docs_and_positions_enum.end_offset()? == 3
    );
    assert!(docs_and_positions_enum.get_payload()?.is_none());
    assert_eq!(-1, docs_and_positions_enum.next_position()?);
    assert!(
      docs_and_positions_enum.start_offset()? == -1 || docs_and_positions_enum.start_offset()? == 4
    );
    assert!(
      docs_and_positions_enum.end_offset()? == -1 || docs_and_positions_enum.end_offset()? == 7
    );
    assert!(docs_and_positions_enum.get_payload()?.is_none());
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum.next_doc()?);

    let mut docs_and_positions_enum2 =
      terms_enum.postings_with_flags(Some(docs_and_positions_enum), PAYLOADS as i32)?;
    assert_eq!(-1, docs_and_positions_enum2.doc_id());
    assert_eq!(0, docs_and_positions_enum2.next_doc()?);
    assert_eq!(2, docs_and_positions_enum2.freq()?);
    assert_eq!(-1, docs_and_positions_enum2.next_position()?);
    assert!(
      docs_and_positions_enum2.start_offset()? == -1
        || docs_and_positions_enum2.start_offset()? == 0
    );
    assert!(
      docs_and_positions_enum2.end_offset()? == -1 || docs_and_positions_enum2.end_offset()? == 3
    );
    assert!(docs_and_positions_enum2.get_payload()?.is_none());
    assert_eq!(-1, docs_and_positions_enum2.next_position()?);
    assert!(
      docs_and_positions_enum2.start_offset()? == -1
        || docs_and_positions_enum2.start_offset()? == 4
    );
    assert!(
      docs_and_positions_enum2.end_offset()? == -1 || docs_and_positions_enum2.end_offset()? == 7
    );
    assert!(docs_and_positions_enum2.get_payload()?.is_none());
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum2.next_doc()?);

    let mut docs_and_positions_enum = terms_enum.postings_with_flags(None, OFFSETS as i32)?;
    assert_eq!(-1, docs_and_positions_enum.doc_id());
    assert_eq!(0, docs_and_positions_enum.next_doc()?);
    assert_eq!(2, docs_and_positions_enum.freq()?);
    assert_eq!(-1, docs_and_positions_enum.next_position()?);
    assert_eq!(0, docs_and_positions_enum.start_offset()?);
    assert_eq!(3, docs_and_positions_enum.end_offset()?);
    assert!(docs_and_positions_enum.get_payload()?.is_none());
    assert_eq!(-1, docs_and_positions_enum.next_position()?);
    assert_eq!(4, docs_and_positions_enum.start_offset()?);
    assert_eq!(7, docs_and_positions_enum.end_offset()?);
    assert!(docs_and_positions_enum.get_payload()?.is_none());
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum.next_doc()?);

    let mut docs_and_positions_enum2 =
      terms_enum.postings_with_flags(Some(docs_and_positions_enum), OFFSETS as i32)?;
    assert_eq!(-1, docs_and_positions_enum2.doc_id());
    assert_eq!(0, docs_and_positions_enum2.next_doc()?);
    assert_eq!(2, docs_and_positions_enum2.freq()?);
    assert_eq!(-1, docs_and_positions_enum2.next_position()?);
    assert_eq!(0, docs_and_positions_enum2.start_offset()?);
    assert_eq!(3, docs_and_positions_enum2.end_offset()?);
    assert!(docs_and_positions_enum2.get_payload()?.is_none());
    assert_eq!(-1, docs_and_positions_enum2.next_position()?);
    assert_eq!(4, docs_and_positions_enum2.start_offset()?);
    assert_eq!(7, docs_and_positions_enum2.end_offset()?);
    assert!(docs_and_positions_enum2.get_payload()?.is_none());
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum2.next_doc()?);

    let mut docs_and_positions_enum = terms_enum.postings_with_flags(None, ALL as i32)?;
    assert_eq!(-1, docs_and_positions_enum.doc_id());
    assert_eq!(0, docs_and_positions_enum.next_doc()?);
    assert_eq!(2, docs_and_positions_enum.freq()?);
    assert_eq!(-1, docs_and_positions_enum.next_position()?);
    assert_eq!(0, docs_and_positions_enum.start_offset()?);
    assert_eq!(3, docs_and_positions_enum.end_offset()?);
    assert!(docs_and_positions_enum.get_payload()?.is_none());
    assert_eq!(-1, docs_and_positions_enum.next_position()?);
    assert_eq!(4, docs_and_positions_enum.start_offset()?);
    assert_eq!(7, docs_and_positions_enum.end_offset()?);
    assert!(docs_and_positions_enum.get_payload()?.is_none());
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum.next_doc()?);

    let mut docs_and_positions_enum2 =
      terms_enum.postings_with_flags(Some(docs_and_positions_enum), ALL as i32)?;
    assert_eq!(-1, docs_and_positions_enum2.doc_id());
    assert_eq!(0, docs_and_positions_enum2.next_doc()?);
    assert_eq!(2, docs_and_positions_enum2.freq()?);
    assert_eq!(-1, docs_and_positions_enum2.next_position()?);
    assert_eq!(0, docs_and_positions_enum2.start_offset()?);
    assert_eq!(3, docs_and_positions_enum2.end_offset()?);
    assert!(docs_and_positions_enum2.get_payload()?.is_none());
    assert_eq!(-1, docs_and_positions_enum2.next_position()?);
    assert_eq!(4, docs_and_positions_enum2.start_offset()?);
    assert_eq!(7, docs_and_positions_enum2.end_offset()?);
    assert!(docs_and_positions_enum2.get_payload()?.is_none());
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum2.next_doc()?);

    reader.close()?;
    iw.close()?;
    Ok(())
  }
  fn test_postings_enum_payloads<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let iwc = new_index_writer_config(random)?;
    let w = IndexWriter::new(dir, iwc)?;

    let mut token1 = token::with_range(Some("bar"), 0, 3)?;
    token1
      .sub
      .token
      .set_payload(Some(BytesRef::from_string("pay1")));

    let mut token2 = token::with_range(Some("bar"), 4, 7)?;
    token2
      .sub
      .token
      .set_payload(Some(BytesRef::from_string("pay2")));

    let mut ft = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
    ft.set_store_term_vectors(true)?;
    ft.set_store_term_vector_positions(true)?;
    ft.set_store_term_vector_payloads(true)?;

    let mut doc = Document::new();
    doc.add(Field::from_token_stream(
      "foo",
      FieldTokenStreamEnum::custom(CannedTokenStream::new(vec![token1, token2])),
      ft,
    )?);
    w.add_document(doc)?;

    let reader = directory_reader::open_from_writer(&w)?;
    let leaf = get_only_leaf_reader(reader)?;

    let mut term_vectors = leaf.term_vectors()?;
    let terms = term_vectors.get_field_terms(0, "foo")?.unwrap();
    let mut terms_enum = terms.iterator()?;
    assert_eq!(
      &BytesRef::from_string("bar"),
      terms_enum.next()?.unwrap().as_ref()
    );

    let mut postings = terms_enum.postings(None)?;
    assert_eq!(-1, postings.doc_id());
    assert_eq!(0, postings.next_doc()?);
    assert_eq!(2, postings.freq()?);
    assert_eq!(NO_MORE_DOCS, postings.next_doc()?);

    let mut postings2 = terms_enum.postings(Some(postings))?;
    assert_eq!(-1, postings2.doc_id());
    assert_eq!(0, postings2.next_doc()?);
    assert_eq!(2, postings2.freq()?);
    assert_eq!(NO_MORE_DOCS, postings2.next_doc()?);

    let mut docs_only = terms_enum.postings_with_flags(None, NONE as i32)?;
    assert_eq!(-1, docs_only.doc_id());
    assert_eq!(0, docs_only.next_doc()?);
    assert!(docs_only.freq()? == 1 || docs_only.freq()? == 2);
    assert_eq!(NO_MORE_DOCS, docs_only.next_doc()?);

    let mut docs_only2 = terms_enum.postings_with_flags(Some(docs_only), NONE as i32)?;
    assert_eq!(-1, docs_only2.doc_id());
    assert_eq!(0, docs_only2.next_doc()?);
    assert!(docs_only2.freq()? == 1 || docs_only2.freq()? == 2);
    assert_eq!(NO_MORE_DOCS, docs_only2.next_doc()?);

    let mut docs_and_positions_enum = terms_enum.postings_with_flags(None, POSITIONS as i32)?;
    assert_eq!(-1, docs_and_positions_enum.doc_id());
    assert_eq!(0, docs_and_positions_enum.next_doc()?);
    assert_eq!(2, docs_and_positions_enum.freq()?);
    assert_eq!(0, docs_and_positions_enum.next_position()?);
    assert_eq!(-1, docs_and_positions_enum.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum.end_offset()?);
    assert!(
      docs_and_positions_enum.get_payload()?.is_none()
        || docs_and_positions_enum.get_payload()?.unwrap().as_ref()
          == &BytesRef::from_string("pay1")
    );
    assert_eq!(1, docs_and_positions_enum.next_position()?);
    assert_eq!(-1, docs_and_positions_enum.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum.end_offset()?);
    assert!(
      docs_and_positions_enum.get_payload()?.is_none()
        || docs_and_positions_enum.get_payload()?.unwrap().as_ref()
          == &BytesRef::from_string("pay2")
    );
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum.next_doc()?);

    let mut docs_and_positions_enum2 =
      terms_enum.postings_with_flags(Some(docs_and_positions_enum), POSITIONS as i32)?;
    assert_eq!(-1, docs_and_positions_enum2.doc_id());
    assert_eq!(0, docs_and_positions_enum2.next_doc()?);
    assert_eq!(2, docs_and_positions_enum2.freq()?);
    assert_eq!(0, docs_and_positions_enum2.next_position()?);
    assert_eq!(-1, docs_and_positions_enum2.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum2.end_offset()?);
    assert!(
      docs_and_positions_enum2.get_payload()?.is_none()
        || docs_and_positions_enum2.get_payload()?.unwrap().as_ref()
          == &BytesRef::from_string("pay1")
    );
    assert_eq!(1, docs_and_positions_enum2.next_position()?);
    assert_eq!(-1, docs_and_positions_enum2.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum2.end_offset()?);
    assert!(
      docs_and_positions_enum2.get_payload()?.is_none()
        || docs_and_positions_enum2.get_payload()?.unwrap().as_ref()
          == &BytesRef::from_string("pay2")
    );
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum2.next_doc()?);

    let mut docs_and_positions_enum = terms_enum.postings_with_flags(None, PAYLOADS as i32)?;
    assert_eq!(-1, docs_and_positions_enum.doc_id());
    assert_eq!(0, docs_and_positions_enum.next_doc()?);
    assert_eq!(2, docs_and_positions_enum.freq()?);
    assert_eq!(0, docs_and_positions_enum.next_position()?);
    assert_eq!(-1, docs_and_positions_enum.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum.end_offset()?);
    assert_eq!(
      &BytesRef::from_string("pay1"),
      docs_and_positions_enum.get_payload()?.unwrap().as_ref()
    );
    assert_eq!(1, docs_and_positions_enum.next_position()?);
    assert_eq!(-1, docs_and_positions_enum.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum.end_offset()?);
    assert_eq!(
      &BytesRef::from_string("pay2"),
      docs_and_positions_enum.get_payload()?.unwrap().as_ref()
    );
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum.next_doc()?);

    let mut docs_and_positions_enum2 =
      terms_enum.postings_with_flags(Some(docs_and_positions_enum), PAYLOADS as i32)?;
    assert_eq!(-1, docs_and_positions_enum2.doc_id());
    assert_eq!(0, docs_and_positions_enum2.next_doc()?);
    assert_eq!(2, docs_and_positions_enum2.freq()?);
    assert_eq!(0, docs_and_positions_enum2.next_position()?);
    assert_eq!(-1, docs_and_positions_enum2.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum2.end_offset()?);
    assert_eq!(
      &BytesRef::from_string("pay1"),
      docs_and_positions_enum2.get_payload()?.unwrap().as_ref()
    );
    assert_eq!(1, docs_and_positions_enum2.next_position()?);
    assert_eq!(-1, docs_and_positions_enum2.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum2.end_offset()?);
    assert_eq!(
      &BytesRef::from_string("pay2"),
      docs_and_positions_enum2.get_payload()?.unwrap().as_ref()
    );
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum2.next_doc()?);

    let mut docs_and_positions_enum = terms_enum.postings_with_flags(None, OFFSETS as i32)?;
    assert_eq!(-1, docs_and_positions_enum.doc_id());
    assert_eq!(0, docs_and_positions_enum.next_doc()?);
    assert_eq!(2, docs_and_positions_enum.freq()?);
    assert_eq!(0, docs_and_positions_enum.next_position()?);
    assert_eq!(-1, docs_and_positions_enum.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum.end_offset()?);
    assert!(
      docs_and_positions_enum.get_payload()?.is_none()
        || docs_and_positions_enum.get_payload()?.unwrap().as_ref()
          == &BytesRef::from_string("pay1")
    );
    assert_eq!(1, docs_and_positions_enum.next_position()?);
    assert_eq!(-1, docs_and_positions_enum.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum.end_offset()?);
    assert!(
      docs_and_positions_enum.get_payload()?.is_none()
        || docs_and_positions_enum.get_payload()?.unwrap().as_ref()
          == &BytesRef::from_string("pay2")
    );
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum.next_doc()?);

    let mut docs_and_positions_enum2 =
      terms_enum.postings_with_flags(Some(docs_and_positions_enum), OFFSETS as i32)?;
    assert_eq!(-1, docs_and_positions_enum2.doc_id());
    assert_eq!(0, docs_and_positions_enum2.next_doc()?);
    assert_eq!(2, docs_and_positions_enum2.freq()?);
    assert_eq!(0, docs_and_positions_enum2.next_position()?);
    assert_eq!(-1, docs_and_positions_enum2.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum2.end_offset()?);
    assert!(
      docs_and_positions_enum2.get_payload()?.is_none()
        || docs_and_positions_enum2.get_payload()?.unwrap().as_ref()
          == &BytesRef::from_string("pay1")
    );
    assert_eq!(1, docs_and_positions_enum2.next_position()?);
    assert_eq!(-1, docs_and_positions_enum2.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum2.end_offset()?);
    assert!(
      docs_and_positions_enum2.get_payload()?.is_none()
        || docs_and_positions_enum2.get_payload()?.unwrap().as_ref()
          == &BytesRef::from_string("pay2")
    );
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum2.next_doc()?);

    let mut docs_and_positions_enum = terms_enum.postings_with_flags(None, ALL as i32)?;
    assert_eq!(-1, docs_and_positions_enum.doc_id());
    assert_eq!(0, docs_and_positions_enum.next_doc()?);
    assert_eq!(2, docs_and_positions_enum.freq()?);
    assert_eq!(0, docs_and_positions_enum.next_position()?);
    assert_eq!(-1, docs_and_positions_enum.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum.end_offset()?);
    assert_eq!(
      &BytesRef::from_string("pay1"),
      docs_and_positions_enum.get_payload()?.unwrap().as_ref()
    );
    assert_eq!(1, docs_and_positions_enum.next_position()?);
    assert_eq!(-1, docs_and_positions_enum.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum.end_offset()?);
    assert_eq!(
      &BytesRef::from_string("pay2"),
      docs_and_positions_enum.get_payload()?.unwrap().as_ref()
    );
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum.next_doc()?);

    let mut docs_and_positions_enum2 =
      terms_enum.postings_with_flags(Some(docs_and_positions_enum), ALL as i32)?;
    assert_eq!(-1, docs_and_positions_enum2.doc_id());
    assert_eq!(0, docs_and_positions_enum2.next_doc()?);
    assert_eq!(2, docs_and_positions_enum2.freq()?);
    assert_eq!(0, docs_and_positions_enum2.next_position()?);
    assert_eq!(-1, docs_and_positions_enum2.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum2.end_offset()?);
    assert_eq!(
      &BytesRef::from_string("pay1"),
      docs_and_positions_enum2.get_payload()?.unwrap().as_ref()
    );
    assert_eq!(1, docs_and_positions_enum2.next_position()?);
    assert_eq!(-1, docs_and_positions_enum2.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum2.end_offset()?);
    assert_eq!(
      &BytesRef::from_string("pay2"),
      docs_and_positions_enum2.get_payload()?.unwrap().as_ref()
    );
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum2.next_doc()?);

    Ok(())
  }

  fn test_postings_enum_all<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let iwc = new_index_writer_config(random)?;
    let w = IndexWriter::new(dir, iwc)?;

    let mut token1 = token::with_range(Some("bar"), 0, 3)?;
    token1
      .sub
      .token
      .set_payload(Some(BytesRef::from_string("pay1")));

    let mut token2 = token::with_range(Some("bar"), 4, 7)?;
    token2
      .sub
      .token
      .set_payload(Some(BytesRef::from_string("pay2")));

    let mut ft = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
    ft.set_store_term_vectors(true)?;
    ft.set_store_term_vector_positions(true)?;
    ft.set_store_term_vector_payloads(true)?;
    ft.set_store_term_vector_offsets(true)?;

    let mut doc = Document::new();
    doc.add(Field::from_token_stream(
      "foo",
      FieldTokenStreamEnum::custom(CannedTokenStream::new(vec![token1, token2])),
      ft,
    )?);
    w.add_document(doc)?;

    let reader = directory_reader::open_from_writer(&w)?;
    let leaf = get_only_leaf_reader(reader)?;

    let mut term_vectors = leaf.term_vectors()?;
    let terms = term_vectors.get_field_terms(0, "foo")?.unwrap();
    let mut terms_enum = terms.iterator()?;
    assert_eq!(
      &BytesRef::from_string("bar"),
      terms_enum.next()?.unwrap().as_ref()
    );

    let mut postings = terms_enum.postings(None)?;
    assert_eq!(-1, postings.doc_id());
    assert_eq!(0, postings.next_doc()?);
    assert_eq!(2, postings.freq()?);
    assert_eq!(NO_MORE_DOCS, postings.next_doc()?);

    let mut postings2 = terms_enum.postings(Some(postings))?;
    assert_eq!(-1, postings2.doc_id());
    assert_eq!(0, postings2.next_doc()?);
    assert_eq!(2, postings2.freq()?);
    assert_eq!(NO_MORE_DOCS, postings2.next_doc()?);

    let mut docs_only = terms_enum.postings_with_flags(None, NONE as i32)?;
    assert_eq!(-1, docs_only.doc_id());
    assert_eq!(0, docs_only.next_doc()?);
    assert!(docs_only.freq()? == 1 || docs_only.freq()? == 2);
    assert_eq!(NO_MORE_DOCS, docs_only.next_doc()?);

    let mut docs_only2 = terms_enum.postings_with_flags(Some(docs_only), NONE as i32)?;
    assert_eq!(-1, docs_only2.doc_id());
    assert_eq!(0, docs_only2.next_doc()?);
    assert!(docs_only2.freq()? == 1 || docs_only2.freq()? == 2);
    assert_eq!(NO_MORE_DOCS, docs_only2.next_doc()?);

    let mut docs_and_positions_enum = terms_enum.postings_with_flags(None, POSITIONS as i32)?;
    assert_eq!(-1, docs_and_positions_enum.doc_id());
    assert_eq!(0, docs_and_positions_enum.next_doc()?);
    assert_eq!(2, docs_and_positions_enum.freq()?);
    assert_eq!(0, docs_and_positions_enum.next_position()?);
    assert!(
      docs_and_positions_enum.start_offset()? == -1 || docs_and_positions_enum.start_offset()? == 0
    );
    assert!(
      docs_and_positions_enum.end_offset()? == -1 || docs_and_positions_enum.end_offset()? == 3
    );
    assert!(
      docs_and_positions_enum.get_payload()?.is_none()
        || docs_and_positions_enum.get_payload()?.unwrap().as_ref()
          == &BytesRef::from_string("pay1")
    );
    assert_eq!(1, docs_and_positions_enum.next_position()?);
    assert!(
      docs_and_positions_enum.start_offset()? == -1 || docs_and_positions_enum.start_offset()? == 4
    );
    assert!(
      docs_and_positions_enum.end_offset()? == -1 || docs_and_positions_enum.end_offset()? == 7
    );
    assert!(
      docs_and_positions_enum.get_payload()?.is_none()
        || docs_and_positions_enum.get_payload()?.unwrap().as_ref()
          == &BytesRef::from_string("pay2")
    );
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum.next_doc()?);

    let mut docs_and_positions_enum2 =
      terms_enum.postings_with_flags(Some(docs_and_positions_enum), POSITIONS as i32)?;
    assert_eq!(-1, docs_and_positions_enum2.doc_id());
    assert_eq!(0, docs_and_positions_enum2.next_doc()?);
    assert_eq!(2, docs_and_positions_enum2.freq()?);
    assert_eq!(0, docs_and_positions_enum2.next_position()?);
    assert!(
      docs_and_positions_enum2.start_offset()? == -1
        || docs_and_positions_enum2.start_offset()? == 0
    );
    assert!(
      docs_and_positions_enum2.end_offset()? == -1 || docs_and_positions_enum2.end_offset()? == 3
    );
    assert!(
      docs_and_positions_enum2.get_payload()?.is_none()
        || docs_and_positions_enum2.get_payload()?.unwrap().as_ref()
          == &BytesRef::from_string("pay1")
    );
    assert_eq!(1, docs_and_positions_enum2.next_position()?);
    assert!(
      docs_and_positions_enum2.start_offset()? == -1
        || docs_and_positions_enum2.start_offset()? == 4
    );
    assert!(
      docs_and_positions_enum2.end_offset()? == -1 || docs_and_positions_enum2.end_offset()? == 7
    );
    assert!(
      docs_and_positions_enum2.get_payload()?.is_none()
        || docs_and_positions_enum2.get_payload()?.unwrap().as_ref()
          == &BytesRef::from_string("pay2")
    );
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum2.next_doc()?);

    let mut docs_and_positions_enum = terms_enum.postings_with_flags(None, PAYLOADS as i32)?;
    assert_eq!(-1, docs_and_positions_enum.doc_id());
    assert_eq!(0, docs_and_positions_enum.next_doc()?);
    assert_eq!(2, docs_and_positions_enum.freq()?);
    assert_eq!(0, docs_and_positions_enum.next_position()?);
    assert!(
      docs_and_positions_enum.start_offset()? == -1 || docs_and_positions_enum.start_offset()? == 0
    );
    assert!(
      docs_and_positions_enum.end_offset()? == -1 || docs_and_positions_enum.end_offset()? == 3
    );
    assert_eq!(
      &BytesRef::from_string("pay1"),
      docs_and_positions_enum.get_payload()?.unwrap().as_ref()
    );
    assert_eq!(1, docs_and_positions_enum.next_position()?);
    assert!(
      docs_and_positions_enum.start_offset()? == -1 || docs_and_positions_enum.start_offset()? == 4
    );
    assert!(
      docs_and_positions_enum.end_offset()? == -1 || docs_and_positions_enum.end_offset()? == 7
    );
    assert_eq!(
      &BytesRef::from_string("pay2"),
      docs_and_positions_enum.get_payload()?.unwrap().as_ref()
    );
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum.next_doc()?);

    let mut docs_and_positions_enum2 =
      terms_enum.postings_with_flags(Some(docs_and_positions_enum), PAYLOADS as i32)?;
    assert_eq!(-1, docs_and_positions_enum2.doc_id());
    assert_eq!(0, docs_and_positions_enum2.next_doc()?);
    assert_eq!(2, docs_and_positions_enum2.freq()?);
    assert_eq!(0, docs_and_positions_enum2.next_position()?);
    assert!(
      docs_and_positions_enum2.start_offset()? == -1
        || docs_and_positions_enum2.start_offset()? == 0
    );
    assert!(
      docs_and_positions_enum2.end_offset()? == -1 || docs_and_positions_enum2.end_offset()? == 3
    );
    assert_eq!(
      &BytesRef::from_string("pay1"),
      docs_and_positions_enum2.get_payload()?.unwrap().as_ref()
    );
    assert_eq!(1, docs_and_positions_enum2.next_position()?);
    assert!(
      docs_and_positions_enum2.start_offset()? == -1
        || docs_and_positions_enum2.start_offset()? == 4
    );
    assert!(
      docs_and_positions_enum2.end_offset()? == -1 || docs_and_positions_enum2.end_offset()? == 7
    );
    assert_eq!(
      &BytesRef::from_string("pay2"),
      docs_and_positions_enum2.get_payload()?.unwrap().as_ref()
    );
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum2.next_doc()?);

    let mut docs_and_positions_enum = terms_enum.postings_with_flags(None, OFFSETS as i32)?;
    assert_eq!(-1, docs_and_positions_enum.doc_id());
    assert_eq!(0, docs_and_positions_enum.next_doc()?);
    assert_eq!(2, docs_and_positions_enum.freq()?);
    assert_eq!(0, docs_and_positions_enum.next_position()?);
    assert_eq!(0, docs_and_positions_enum.start_offset()?);
    assert_eq!(3, docs_and_positions_enum.end_offset()?);
    assert!(
      docs_and_positions_enum.get_payload()?.is_none()
        || docs_and_positions_enum.get_payload()?.unwrap().as_ref()
          == &BytesRef::from_string("pay1")
    );
    assert_eq!(1, docs_and_positions_enum.next_position()?);
    assert_eq!(4, docs_and_positions_enum.start_offset()?);
    assert_eq!(7, docs_and_positions_enum.end_offset()?);
    assert!(
      docs_and_positions_enum.get_payload()?.is_none()
        || docs_and_positions_enum.get_payload()?.unwrap().as_ref()
          == &BytesRef::from_string("pay2")
    );
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum.next_doc()?);

    let mut docs_and_positions_enum2 =
      terms_enum.postings_with_flags(Some(docs_and_positions_enum), OFFSETS as i32)?;
    assert_eq!(-1, docs_and_positions_enum2.doc_id());
    assert_eq!(0, docs_and_positions_enum2.next_doc()?);
    assert_eq!(2, docs_and_positions_enum2.freq()?);
    assert_eq!(0, docs_and_positions_enum2.next_position()?);
    assert_eq!(0, docs_and_positions_enum2.start_offset()?);
    assert_eq!(3, docs_and_positions_enum2.end_offset()?);
    assert!(
      docs_and_positions_enum2.get_payload()?.is_none()
        || docs_and_positions_enum2.get_payload()?.unwrap().as_ref()
          == &BytesRef::from_string("pay1")
    );
    assert_eq!(1, docs_and_positions_enum2.next_position()?);
    assert_eq!(4, docs_and_positions_enum2.start_offset()?);
    assert_eq!(7, docs_and_positions_enum2.end_offset()?);
    assert!(
      docs_and_positions_enum2.get_payload()?.is_none()
        || docs_and_positions_enum2.get_payload()?.unwrap().as_ref()
          == &BytesRef::from_string("pay2")
    );
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum2.next_doc()?);

    let mut docs_and_positions_enum = terms_enum.postings_with_flags(None, ALL as i32)?;
    assert_eq!(-1, docs_and_positions_enum.doc_id());
    assert_eq!(0, docs_and_positions_enum.next_doc()?);
    assert_eq!(2, docs_and_positions_enum.freq()?);
    assert_eq!(0, docs_and_positions_enum.next_position()?);
    assert_eq!(0, docs_and_positions_enum.start_offset()?);
    assert_eq!(3, docs_and_positions_enum.end_offset()?);
    assert_eq!(
      &BytesRef::from_string("pay1"),
      docs_and_positions_enum.get_payload()?.unwrap().as_ref()
    );
    assert_eq!(1, docs_and_positions_enum.next_position()?);
    assert_eq!(4, docs_and_positions_enum.start_offset()?);
    assert_eq!(7, docs_and_positions_enum.end_offset()?);
    assert_eq!(
      &BytesRef::from_string("pay2"),
      docs_and_positions_enum.get_payload()?.unwrap().as_ref()
    );
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum.next_doc()?);

    let mut docs_and_positions_enum2 =
      terms_enum.postings_with_flags(Some(docs_and_positions_enum), ALL as i32)?;
    assert_eq!(-1, docs_and_positions_enum2.doc_id());
    assert_eq!(0, docs_and_positions_enum2.next_doc()?);
    assert_eq!(2, docs_and_positions_enum2.freq()?);
    assert_eq!(0, docs_and_positions_enum2.next_position()?);
    assert_eq!(0, docs_and_positions_enum2.start_offset()?);
    assert_eq!(3, docs_and_positions_enum2.end_offset()?);
    assert_eq!(
      &BytesRef::from_string("pay1"),
      docs_and_positions_enum2.get_payload()?.unwrap().as_ref()
    );
    assert_eq!(1, docs_and_positions_enum2.next_position()?);
    assert_eq!(4, docs_and_positions_enum2.start_offset()?);
    assert_eq!(7, docs_and_positions_enum2.end_offset()?);
    assert_eq!(
      &BytesRef::from_string("pay2"),
      docs_and_positions_enum2.get_payload()?.unwrap().as_ref()
    );
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum2.next_doc()?);

    Ok(())
  }
}

struct MockTokenizerAnalyzer {
  seed: u64,
  stored_value: AnalyzerStoredValue,
}

impl MockTokenizerAnalyzer {
  fn new<R>(random: &mut R) -> Self
  where
    R: Rng + ?Sized,
  {
    Self {
      seed: random.random(),
      stored_value: AnalyzerStoredValue::new(),
    }
  }

  fn next_random(&self) -> StdRng {
    random_from_seed(self.seed)
  }
}

impl Analyzer for MockTokenizerAnalyzer {
  fn create_components(&self, _field_name: &str) -> Result<TokenStreamComponents> {
    Ok(TokenStreamComponents::new(
      Box::new(MockTokenizer::with_default_max_token_length(
        self.next_random(),
        WHITESPACE.clone(),
        true,
      )) as Box<dyn TokenStream + Send + Sync>,
      None,
    ))
  }

  fn stored_value(&self) -> &AnalyzerStoredValue {
    &self.stored_value
  }
}

crate::impl_analyzer_close!(MockTokenizerAnalyzer);

impl From<MockTokenizerAnalyzer> for AnalyzerEnum {
  fn from(analyzer: MockTokenizerAnalyzer) -> Self {
    AnalyzerEnum::Custom(Box::new(analyzer))
  }
}

pub struct RandomTokenStream {
  attr: Attributes,
  terms: Vec<String>,
  term_bytes: Vec<BytesRef<Vec<u8>>>,
  positions_increments: Vec<i32>,
  positions: Vec<i32>,
  start_offsets: Vec<i32>,
  end_offsets: Vec<i32>,
  payloads: Vec<Option<BytesRef<Vec<u8>>>>,

  freqs: HashMap<String, i32>,
  position_to_terms: HashMap<i32, HashSet<i32>>,
  start_offset_to_terms: HashMap<i32, HashSet<i32>>,
  i: usize,
}
impl RandomTokenStream {
  pub fn new<R>(
    random: &mut R,
    len: usize,
    sample_terms: &[String],
    sample_term_bytes: &[BytesRef<Vec<u8>>],
  ) -> Result<Self>
  where
    R: Rng + ?Sized,
  {
    Self::new_with_random_payload(
      random,
      len,
      sample_terms,
      sample_term_bytes,
      Self::random_payload,
    )
  }

  pub fn new_with_random_payload<R, P>(
    random: &mut R,
    len: usize,
    sample_terms: &[String],
    sample_term_bytes: &[BytesRef<Vec<u8>>],
    mut random_payload: P,
  ) -> Result<Self>
  where
    R: Rng + ?Sized,
    P: FnMut(&mut R) -> Option<BytesRef<Vec<u8>>>,
  {
    let mut terms = vec![String::new(); len];
    let mut term_bytes = vec![BytesRef::default(); len];
    let mut positions_increments = vec![0; len];
    let mut positions = vec![0; len];
    let mut start_offsets = vec![0; len];
    let mut end_offsets = vec![0; len];
    let mut payloads = vec![None; len];

    for i in 0..len {
      let o = random.random_range(0..sample_terms.len());
      terms[i] = sample_terms[o].clone();
      term_bytes[i] = sample_term_bytes[o].clone();
      positions_increments[i] = TestUtil::next_int(random, if i == 0 { 1 } else { 0 }, 10);

      if i == 0 {
        start_offsets[i] = TestUtil::next_int(random, 0, 1 << 16);
      } else {
        let v = if rarely(random) { 1 << 16 } else { 20 };
        start_offsets[i] = start_offsets[i - 1] + TestUtil::next_int(random, 0, v);
      }
      let v = if rarely(random) { 1 << 10 } else { 20 };
      end_offsets[i] = start_offsets[i] + TestUtil::next_int(random, 0, v);
    }

    for i in 0..len {
      if i == 0 {
        positions[i] = positions_increments[i] - 1;
      } else {
        positions[i] = positions[i - 1] + positions_increments[i];
      }
    }

    if rarely(random) {
      let payload = random_payload(random);
      payloads.fill(payload);
    } else {
      for payload in &mut payloads {
        *payload = random_payload(random);
      }
    }

    let mut position_to_terms: HashMap<i32, HashSet<i32>> = HashMap::with_capacity(len);
    let mut start_offset_to_terms: HashMap<i32, HashSet<i32>> = HashMap::with_capacity(len);

    for i in 0..len {
      position_to_terms
        .entry(positions[i])
        .or_insert_with(|| HashSet::with_capacity(1))
        .insert(i as i32);

      start_offset_to_terms
        .entry(start_offsets[i])
        .or_insert_with(|| HashSet::with_capacity(1))
        .insert(i as i32);
    }

    let mut freqs = HashMap::new();
    for term in &terms {
      *freqs.entry(term.clone()).or_insert(0) += 1;
    }

    Ok(Self {
      attr: RandomTokenStreamAttr::new()
        .expect("RandomTokenStreamAttr::new should not fail")
        .into(),
      terms,
      term_bytes,
      positions_increments,
      positions,
      start_offsets,
      end_offsets,
      payloads,
      freqs,
      position_to_terms,
      start_offset_to_terms,
      i: 0,
    })
  }
  fn random_payload<R>(random: &mut R) -> Option<BytesRef<Vec<u8>>>
  where
    R: Rng + ?Sized,
  {
    let len = random.random_range(0..5);
    if len == 0 {
      return None;
    }

    let mut bytes = vec![0u8; len];
    random.fill_bytes(&mut bytes);
    Some(new_bytes_ref_from_bytes(random, bytes.as_slice()).expect("valid bytes"))
  }
}

impl crate::core::util::close::Closeable for RandomTokenStream {}

impl TokenStream for RandomTokenStream {
  fn increment_token(&mut self) -> Result<bool> {
    if self.i < self.terms.len() {
      self.attr.clear_attributes()?;

      self
        .attr
        .set_length(0)?
        .append_str(Some(&self.terms[self.i]))?;

      self
        .attr
        .set_position_increment(self.positions_increments[self.i])?;

      self
        .attr
        .set_offset(self.start_offsets[self.i], self.end_offsets[self.i])?;

      self.attr.set_payload(self.payloads[self.i].clone())?;

      self.i += 1;
      Ok(true)
    } else {
      Ok(false)
    }
  }

  fn end(&mut self) -> Result<()> {
    self.attr.end_attributes();
    Ok(())
  }

  fn reset(&mut self) -> Result<()> {
    self.i = 0;
    Ok(())
  }

  fn get_attribute_source(&self) -> &Attributes {
    &self.attr
  }

  fn get_attribute_source_mut(&mut self) -> &mut Attributes {
    &mut self.attr
  }
}

impl RandomTokenStream {
  fn has_payloads(&self) -> bool {
    self.payloads.iter().any(Option::is_some)
  }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Options {
  None,
  Positions,
  Offsets,
  PositionsAndOffsets,
  PositionsAndPayloads,
  PositionsOffsetsPayloads,
}

impl std::fmt::Display for Options {
  fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    formatter.write_str(match self {
      Self::None => "NONE",
      Self::Positions => "POSITIONS",
      Self::Offsets => "OFFSETS",
      Self::PositionsAndOffsets => "POSITIONS_AND_OFFSETS",
      Self::PositionsAndPayloads => "POSITIONS_AND_PAYLOADS",
      Self::PositionsOffsetsPayloads => "POSITIONS_AND_OFFSETS_AND_PAYLOADS",
    })
  }
}

impl Options {
  fn positions(self) -> bool {
    matches!(
      self,
      Options::Positions
        | Options::PositionsAndOffsets
        | Options::PositionsAndPayloads
        | Options::PositionsOffsetsPayloads
    )
  }

  fn offsets(self) -> bool {
    matches!(
      self,
      Options::Offsets | Options::PositionsAndOffsets | Options::PositionsOffsetsPayloads
    )
  }

  fn payloads(self) -> bool {
    matches!(
      self,
      Options::PositionsAndPayloads | Options::PositionsOffsetsPayloads
    )
  }
}

fn field_type(options: Options) -> Result<FieldType> {
  let mut ft = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  ft.set_store_term_vectors(true)?;
  if options.positions() {
    ft.set_store_term_vector_positions(true)?;
  }
  if options.offsets() {
    ft.set_store_term_vector_offsets(true)?;
  }
  if options.payloads() {
    ft.set_store_term_vector_payloads(true)?;
  }
  ft.freeze();
  Ok(ft)
}
/// Randomly generated document: call toDocument to index it
pub struct RandomDocument {
  field_names: Vec<String>,
  field_types: Vec<FieldType>,
  token_streams: Vec<RandomTokenStream>,
}

impl RandomDocument {
  pub fn new<R>(
    random: &mut R,
    field_count: usize,
    max_term_count: usize,
    options: Options,
    field_names: &[String],
    sample_terms: &[String],
    sample_term_bytes: &[BytesRef<Vec<u8>>],
  ) -> Result<Self>
  where
    R: Rng + ?Sized,
  {
    if field_count > field_names.len() {
      return Err(LuceneError::illegal_argument(""));
    }
    let mut doc_field_names = Vec::with_capacity(field_count);
    let mut doc_field_types = Vec::with_capacity(field_count);
    let mut token_streams = Vec::with_capacity(field_count);
    let mut used_field_names = HashSet::new();
    for _ in 0..field_count {
      let field_name = loop {
        let field_name = field_names[random.random_range(0..field_names.len())].clone();
        if used_field_names.insert(field_name.clone()) {
          break field_name;
        }
      };
      doc_field_names.push(field_name);
      doc_field_types.push(field_type(options)?);
      let term_count = TestUtil::next_int(random, 1, max_term_count as i32) as usize;
      token_streams.push(RandomTokenStream::new(
        random,
        term_count,
        sample_terms,
        sample_term_bytes,
      )?);
    }
    Ok(Self {
      field_names: doc_field_names,
      field_types: doc_field_types,
      token_streams,
    })
  }

  pub fn to_document(&self) -> Result<Document> {
    let mut doc = Document::new();
    for i in 0..self.field_names.len() {
      doc.add(Field::from_token_stream(
        self.field_names[i].clone(),
        FieldTokenStreamEnum::custom(self.token_streams[i].clone()),
        self.field_types[i].clone(),
      )?);
    }
    Ok(doc)
  }
}

pub struct RandomDocumentFactory {
  field_names: Vec<String>,
  terms: Vec<String>,
  term_bytes: Vec<BytesRef<Vec<u8>>>,
}

impl RandomDocumentFactory {
  pub fn new<R>(random: &mut R, distinct_field_names: usize, distinct_terms: usize) -> Self
  where
    R: Rng + ?Sized,
  {
    let mut field_names = HashSet::new();
    while field_names.len() < distinct_field_names {
      let field_name = TestUtil::random_simple_string(random);
      if field_name != "id" {
        field_names.insert(field_name);
      }
    }
    let mut terms = Vec::with_capacity(distinct_terms);
    let mut term_bytes = Vec::with_capacity(distinct_terms);
    for _ in 0..distinct_terms {
      let term = TestUtil::random_realistic_unicode_string(random);
      term_bytes.push(BytesRef::from_string(&term));
      terms.push(term);
    }
    Self {
      field_names: field_names.into_iter().collect(),
      terms,
      term_bytes,
    }
  }

  pub fn new_document<R>(
    &self,
    random: &mut R,
    field_count: usize,
    max_term_count: usize,
    options: Options,
  ) -> Result<RandomDocument>
  where
    R: Rng + ?Sized,
  {
    RandomDocument::new(
      random,
      field_count,
      max_term_count,
      options,
      &self.field_names,
      &self.terms,
      &self.term_bytes,
    )
  }
}

impl Clone for RandomTokenStream {
  fn clone(&self) -> Self {
    Self {
      attr: RandomTokenStreamAttr::new()
        .expect("RandomTokenStreamAttr::new should not fail")
        .into(),
      terms: self.terms.clone(),
      term_bytes: self.term_bytes.clone(),
      positions_increments: self.positions_increments.clone(),
      positions: self.positions.clone(),
      start_offsets: self.start_offsets.clone(),
      end_offsets: self.end_offsets.clone(),
      payloads: self.payloads.clone(),
      freqs: self.freqs.clone(),
      position_to_terms: self.position_to_terms.clone(),
      start_offset_to_terms: self.start_offset_to_terms.clone(),
      i: 0,
    }
  }
}

fn equals(o1: Option<&BytesRef<Vec<u8>>>, o2: Option<Cow<'_, BytesRef<Vec<u8>>>>) -> bool {
  match (o1, o2) {
    (None, None) => true,
    (Some(a), Some(b)) => a == b.as_ref(),
    _ => false,
  }
}

fn assert_random_token_stream_equals<R, T>(
  random: &mut R,
  tk: &RandomTokenStream,
  ft: &FieldType,
  terms: T,
  read_past_last_position_exception: ReadPastLastPositionException,
) -> Result<()>
where
  R: Rng + ?Sized,
  T: Terms,
{
  assert_eq!(1, terms.get_doc_count()?);
  let term_count = tk.terms.iter().collect::<HashSet<_>>().len();
  assert_eq!(term_count as i64, terms.size()?);
  assert_eq!(term_count as i64, terms.get_sum_doc_freq()?);
  assert_eq!(ft.store_term_vector_positions(), terms.has_positions());
  assert_eq!(ft.store_term_vector_offsets(), terms.has_offsets());
  assert_eq!(
    ft.store_term_vector_payloads() && tk.has_payloads(),
    terms.has_payloads()
  );
  let mut sorted_terms = tk
    .freqs
    .keys()
    .map(|term| BytesRef::from_string(term))
    .collect::<Vec<_>>();
  sorted_terms.sort();
  let mut terms_enum = terms.iterator()?;
  for sorted_term in &sorted_terms {
    let next_term = terms_enum.next()?;
    assert!(next_term.is_some());
    assert_eq!(sorted_term, next_term.unwrap().as_ref());
    assert_eq!(sorted_term, terms_enum.term()?.as_ref());
    assert_eq!(1, terms_enum.doc_freq()?);

    let mut postings_enum = terms_enum.postings(None)?;
    let v = if random.random_bool(0.5) {
      Some(postings_enum)
    } else {
      None
    };
    postings_enum = terms_enum.postings(v)?;
    assert_eq!(0, postings_enum.next_doc()?);
    assert_eq!(0, postings_enum.doc_id());
    assert_eq!(
      *tk
        .freqs
        .get(&terms_enum.term()?.utf8_to_string()?)
        .expect("term must exist"),
      postings_enum.freq()?
    );
    assert_eq!(NO_MORE_DOCS, postings_enum.next_doc()?);

    let mut docs_and_positions_enum = terms_enum.postings_with_flags(None, POSITIONS as i32)?;
    let v = if random.random_bool(0.5) {
      Some(docs_and_positions_enum)
    } else {
      None
    };
    docs_and_positions_enum = terms_enum.postings_with_flags(v, POSITIONS as i32)?;
    if terms.has_positions() || terms.has_offsets() {
      assert_eq!(0, docs_and_positions_enum.next_doc()?);
      let freq = docs_and_positions_enum.freq()?;
      assert_eq!(
        *tk
          .freqs
          .get(&terms_enum.term()?.utf8_to_string()?)
          .expect("term must exist"),
        freq
      );
      for _ in 0..freq {
        let position = docs_and_positions_enum.next_position()?;
        let indexes = if terms.has_positions() {
          tk.position_to_terms
            .get(&position)
            .expect("position must exist")
        } else {
          tk.start_offset_to_terms
            .get(&docs_and_positions_enum.start_offset()?)
            .expect("start offset must exist")
        };
        if terms.has_positions() {
          assert!(indexes.iter().any(|index| {
            let index = *index as usize;
            tk.term_bytes[index] == *terms_enum.term().expect("term must exist")
              && tk.positions[index] == position
          }));
        }
        if terms.has_offsets() {
          assert!(indexes.iter().any(|index| {
            let index = *index as usize;
            tk.term_bytes[index] == *terms_enum.term().expect("term must exist")
              && tk.start_offsets[index]
                == docs_and_positions_enum
                  .start_offset()
                  .expect("start offset must exist")
              && tk.end_offsets[index]
                == docs_and_positions_enum
                  .end_offset()
                  .expect("end offset must exist")
          }));
        }
        if terms.has_payloads() {
          assert!(indexes.iter().any(|index| {
            let index = *index as usize;
            tk.term_bytes[index] == *terms_enum.term().expect("term must exist")
              && equals(
                tk.payloads[index].as_ref(),
                docs_and_positions_enum
                  .get_payload()
                  .expect("payload must read"),
              )
          }));
        }
      }
      match read_past_last_position_exception {
        ReadPastLastPositionException::IllegalState => assert!(matches!(
          docs_and_positions_enum.next_position(),
          Err(LuceneError::IllegalState(_))
        )),
        ReadPastLastPositionException::Assertion => {
          expect_panic(|| docs_and_positions_enum.next_position())
        },
      }
      assert_eq!(NO_MORE_DOCS, docs_and_positions_enum.next_doc()?);
    }
  }
  assert!(terms_enum.next()?.is_none());
  for _ in 0..5 {
    let idx = random.random_range(0..tk.term_bytes.len());
    let term = &tk.term_bytes[idx];
    assert!(terms_enum.seek_exact(term)?);
    assert_eq!(SeekStatus::Found, terms_enum.seek_ceil(term)?);
  }
  Ok(())
}

fn assert_random_document_equals<R, F>(
  random: &mut R,
  doc: &RandomDocument,
  fields: F,
  read_past_last_position_exception: ReadPastLastPositionException,
) -> Result<()>
where
  R: Rng + ?Sized,
  F: IndexFields,
{
  assert_eq!(doc.field_names.len() as i32, fields.size()?);
  let fields1 = doc.field_names.iter().collect::<HashSet<_>>();
  let mut fields2 = HashSet::new();
  let mut field_iter = fields.iterator()?;
  while field_iter.has_next()? {
    let field = field_iter
      .next()?
      .ok_or_else(|| LuceneError::illegal_state("Fields.iterator().has_next returned true"))?;
    fields2.insert(field);
  }
  assert_eq!(fields1, fields2);

  for i in 0..doc.field_names.len() {
    let terms = fields
      .terms(&doc.field_names[i])?
      .expect("term vectors should exist for random document field");
    assert_random_token_stream_equals(
      random,
      &doc.token_streams[i],
      &doc.field_types[i],
      terms,
      read_past_last_position_exception,
    )?;
  }
  Ok(())
}

pub fn add_id(mut doc: Document, id: &str) -> Result<Document> {
  doc.add(StringField::from_string("id", id, Store::No)?);
  Ok(doc)
}

pub fn doc_id<IR>(reader: IR, id: &str) -> Result<i32>
where
  IR: IndexReader + 'static + std::marker::Sync,
  IndexReaderContextType<IR>: std::marker::Sync,
  IRCLeafReader<IndexReaderContextType<IR>>: std::marker::Sync + Send,
{
  let searcher = IndexSearcher::new(reader.get_context()?)?;
  let top_docs = searcher.search(TermQuery::new(Term::from_text("id", id)), 1)?;
  Ok(top_docs.score_docs[0].doc)
}
