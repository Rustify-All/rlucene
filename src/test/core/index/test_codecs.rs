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
use crate::core::codecs::block_term_state::TermStateEnum;
use crate::core::codecs::fields_consumer::FieldsConsumer;
use crate::core::codecs::lucene101_codec::Lucene101Codec;
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::codecs::postings_format::PostingsFormat;
use crate::core::codecs::{Codec, codec};
use crate::core::document::document::Document;
use crate::core::document::field::Store;
use crate::core::document::string_field::StringField;
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::doc_values_skip_index_type::DocValuesSkipIndexType;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::dummy::dummy_impacts_enum::DummyImpactsEnum;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::Builder;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::field_infos::FieldNumbers;
use crate::core::index::fields::Fields;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::postings_enum::{ALL, FREQS, NONE, PostingsEnum};
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::term::Term;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::index::{BytesRef, directory_reader};
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, NO_MORE_DOCS};
use crate::core::store::IOContext;
use crate::core::store::directory::DirEnum;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::dummy::dummy_attribute_source::DummyAttributeSource;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::info_stream::get_default_info_stream;
use crate::core::util::iterator::{IteratorExt, VecIter, VecIteratorExt};
use crate::core::util::string_helper::StringHelper;
use crate::core::util::version::LATEST;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, new_directory_shared, new_index_writer_config_with_analyzer, random, random_from_seed,
};
use crate::test_framework::core::util::test_util::TestUtil;
use num_bigint::BigInt;
use parking_lot::Mutex;
use rand::{Rng, RngExt};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::thread;
#[allow(dead_code)] // for quick search
struct TestCodecs;

static SEGMENT: &str = "0";
static FIELD_NAMES: [&str; 4] = ["one", "two", "three", "four"];
const NUM_TEST_THREADS: usize = 3;
const NUM_FIELDS: usize = 4;
const NUM_TERMS_RAND: i32 = 50; // must be > 16 to test skipping
const DOC_FREQ_RAND: i32 = 500; // must be > 16 to test skipping
const TERM_DOC_FREQ_RAND: i32 = 20;
#[test]
fn test_fixed_postings() -> Result<()> {
  let mut random = random();
  let num_terms = 100;
  let mut terms = Vec::with_capacity(num_terms);
  for i in 0..num_terms {
    let docs = vec![i as i32];
    let text = BigInt::from(i).to_str_radix(36);
    terms.push(TermData::new(text, docs, None));
  }

  let mut builder = Builder::new(Arc::new(Mutex::new(FieldNumbers::new::<String, String>(
    None, None,
  )?)));

  let field = Arc::new(FieldData::new("field", &mut builder, terms, true, false)?);
  let fields = vec![Arc::clone(&field)];
  let field_infos = Arc::new(builder.finish()?);
  let dir = new_directory_shared(&mut random)?;
  let codec = Lucene101Codec::default();
  let si = SegmentInfo::new(
    Arc::clone(&dir),
    Some((*LATEST).clone()),
    Some((*LATEST).clone()),
    SEGMENT,
    10000,
    false,
    false,
    Some(codec.clone()),
    HashMap::new(),
    StringHelper::random_id(),
    HashMap::new(),
    None,
  )?;

  write(&si, Arc::clone(&field_infos), dir.as_ref(), fields)?;

  let io_context = IOContext::default_io_context()?;
  let read_state = SegmentReadState::new(dir.as_ref(), field_infos, &io_context);
  let reader = codec.postings_format().fields_producer(&read_state, &si)?;

  let mut fields_enum = reader.iterator()?;
  let field_name = fields_enum
    .next()?
    .ok_or_else(|| LuceneError::illegal_state("field missing"))?;
  let terms2 = reader
    .terms(field_name)?
    .ok_or_else(|| LuceneError::illegal_state("terms missing"))?;
  let mut terms_enum = terms2.iterator()?;

  let mut postings_enum = None;
  for term_data in &field.terms {
    let term = terms_enum
      .next()?
      .ok_or_else(|| LuceneError::illegal_state("term missing"))?;
    assert_eq!(term_data.text2, term.utf8_to_string()?);

    // Do this twice to stress the codec's reuse/reset behavior, as in Java.
    for _ in 0..2 {
      let mut docs_enum = terms_enum.postings_with_flags(postings_enum.take(), NONE as i32)?;
      assert_eq!(term_data.docs[0], docs_enum.next_doc()?);
      assert_eq!(NO_MORE_DOCS, docs_enum.next_doc()?);
      postings_enum = Some(docs_enum);
    }
  }
  assert!(terms_enum.next()?.is_none());

  for term_data in &field.terms {
    assert_eq!(SeekStatus::Found, terms_enum.seek_ceil(&term_data.text)?);
  }

  assert!(!fields_enum.has_next()?);
  Ok(())
}

#[test]
fn test_random_postings() -> Result<()> {
  let mut random = random();
  let num_test_iter = at_least(&mut random, 20);
  let mut builder = Builder::new(Arc::new(Mutex::new(FieldNumbers::new::<String, String>(
    None, None,
  )?)));

  let mut fields = Vec::with_capacity(NUM_FIELDS);
  for (i, &field_name) in FIELD_NAMES.iter().enumerate().take(NUM_FIELDS) {
    let omit_tf = 0 == (i % 3);
    let store_payloads = 1 == (i % 3);
    fields.push(Arc::new(FieldData::new(
      field_name,
      &mut builder,
      make_random_terms(&mut random, omit_tf, store_payloads)?,
      omit_tf,
      store_payloads,
    )?));
  }

  let dir = new_directory_shared(&mut random)?;
  let field_infos = Arc::new(builder.finish()?);

  let codec = codec::get_default();
  let si = SegmentInfo::new(
    Arc::clone(&dir),
    Some((*LATEST).clone()),
    Some((*LATEST).clone()),
    SEGMENT,
    10000,
    false,
    false,
    Some(codec.clone()),
    HashMap::new(),
    StringHelper::random_id(),
    HashMap::new(),
    None,
  )?;
  write(&si, Arc::clone(&field_infos), dir.as_ref(), fields.clone())?;

  let io_context = IOContext::default_io_context()?;
  let read_state = SegmentReadState::new(dir.as_ref(), field_infos, &io_context);
  let terms = codec.postings_format().fields_producer(&read_state, &si)?;

  let seeds: Vec<u64> = (0..NUM_TEST_THREADS - 1)
    .map(|_| random.random::<u64>())
    .collect();
  let thread_results = thread::scope(|scope| {
    let mut handles = Vec::with_capacity(seeds.len());
    for seed in seeds {
      let fields = &fields;
      let terms = &terms;
      handles.push(scope.spawn(move || {
        let mut thread_random = random_from_seed(seed);
        run_(&mut thread_random, fields, terms, num_test_iter)
      }));
    }
    let mut results = Vec::with_capacity(handles.len());
    for handle in handles {
      results.push(handle.join());
    }
    results
  });

  run_(&mut random, &fields, &terms, num_test_iter)?;

  for result in thread_results {
    result.map_err(|_| LuceneError::illegal_state("verify thread panicked"))??;
  }

  Ok(())
}

fn run_<R, F>(
  random: &mut R,
  fields: &[Arc<FieldData>],
  terms_dict: &F,
  num_test_iter: i32,
) -> Result<()>
where
  R: Rng + ?Sized,
  F: Fields + Sync,
{
  for _ in 0..num_test_iter {
    let field = &fields[random.random_range(0..fields.len())];
    let terms = terms_dict
      .terms(&field.field_info.name)?
      .ok_or_else(|| LuceneError::illegal_state("terms missing"))?;
    let mut terms_enum = terms.iterator()?;

    let mut upto = 0usize;
    while let Some(term) = terms_enum.next()? {
      let expected = BytesRef::from_string(&field.terms[upto].text2);
      assert_eq!(&expected, term.as_ref());
      upto += 1;
    }
    assert_eq!(upto, field.terms.len());

    let mut term = &field.terms[random.random_range(0..field.terms.len())];
    let mut status = terms_enum.seek_ceil(&BytesRef::from_string(&term.text2))?;
    assert_eq!(SeekStatus::Found, status);
    assert_eq!(term.docs.len() as i32, terms_enum.doc_freq()?);
    if field.omit_tf {
      let mut postings = TestUtil::docs(random, &mut terms_enum, None, NONE as i32)?;
      verify_docs(
        random,
        &term.docs,
        term.positions.as_deref(),
        &mut postings,
        false,
      )?;
    } else {
      let mut postings = terms_enum.postings_with_flags(None, ALL as i32)?;
      verify_docs(
        random,
        &term.docs,
        term.positions.as_deref(),
        &mut postings,
        true,
      )?;
    }

    let idx = random.random_range(0..field.terms.len());
    term = &field.terms[idx];
    if terms_enum.seek_exact_with_ord(idx as i64).is_ok() {
      assert_eq!(SeekStatus::Found, status);
      assert_eq!(
        &BytesRef::from_string(&term.text2),
        terms_enum.term()?.as_ref()
      );
      assert_eq!(term.docs.len() as i32, terms_enum.doc_freq()?);
      if field.omit_tf {
        let mut postings = TestUtil::docs(random, &mut terms_enum, None, NONE as i32)?;
        verify_docs(
          random,
          &term.docs,
          term.positions.as_deref(),
          &mut postings,
          false,
        )?;
      } else {
        let mut postings = terms_enum.postings_with_flags(None, ALL as i32)?;
        verify_docs(
          random,
          &term.docs,
          term.positions.as_deref(),
          &mut postings,
          true,
        )?;
      }
    }

    for _ in 0..100 {
      let text2 = TestUtil::random_unicode_string(random) + ".";
      status = terms_enum.seek_ceil(&BytesRef::from_string(&text2))?;
      assert!(status == SeekStatus::NotFound || status == SeekStatus::End);
    }

    for i in (0..field.terms.len()).rev() {
      assert_eq!(
        SeekStatus::Found,
        terms_enum.seek_ceil(&BytesRef::from_string(&field.terms[i].text2))?,
        "field={} term={}",
        field.field_info.name,
        field.terms[i].text2
      );
      assert_eq!(field.terms[i].docs.len() as i32, terms_enum.doc_freq()?);
    }

    for i in (0..field.terms.len()).rev() {
      if terms_enum.seek_exact_with_ord(i as i64).is_ok() {
        assert_eq!(field.terms[i].docs.len() as i32, terms_enum.doc_freq()?);
        assert_eq!(
          &BytesRef::from_string(&field.terms[i].text2),
          terms_enum.term()?.as_ref()
        );
      }
    }

    // status = terms_enum.seek_ceil(&BytesRef::from_string(""))?;
    assert_eq!(
      &BytesRef::from_string(&field.terms[0].text2),
      terms_enum.term()?.as_ref()
    );

    terms_enum.seek_ceil(&BytesRef::from_string(""))?;
    upto = 0;
    loop {
      term = &field.terms[upto];
      if random.random_range(0..3) == 1 {
        let mut postings = if !field.omit_tf {
          terms_enum.postings_with_flags(None, ALL as i32)?
        } else {
          TestUtil::docs(random, &mut terms_enum, None, FREQS as i32)?
        };
        let mut upto2 = -1i32;
        let mut ended = false;
        while upto2 < term.docs.len() as i32 - 1 {
          let left = term.docs.len() as i32 - upto2;
          let doc;
          if random.random_range(0..3) == 1 && left >= 1 {
            let inc = 1 + random.random_range(0..left - 1);
            upto2 += inc;
            if random.random_range(0..2) == 1 {
              doc = postings.advance(term.docs[upto2 as usize])?;
              assert_eq!(term.docs[upto2 as usize], doc);
            } else {
              doc = postings.advance(1 + term.docs[upto2 as usize])?;
              if doc == NO_MORE_DOCS {
                assert_eq!(upto2, term.docs.len() as i32 - 1);
                ended = true;
                break;
              } else {
                assert!(upto2 < term.docs.len() as i32 - 1);
                if doc >= term.docs[1 + upto2 as usize] {
                  upto2 += 1;
                }
              }
            }
          } else {
            doc = postings.next_doc()?;
            assert_ne!(-1, doc);
            upto2 += 1;
          }
          assert_eq!(term.docs[upto2 as usize], doc);
          if !field.omit_tf {
            assert_eq!(
              term.positions.as_ref().unwrap()[upto2 as usize].len() as i32,
              postings.freq()?
            );
            if random.random_range(0..2) == 1 {
              verify_positions(
                random,
                &term.positions.as_ref().unwrap()[upto2 as usize],
                &mut postings,
              )?;
            }
          }
        }

        if !ended {
          assert_eq!(NO_MORE_DOCS, postings.next_doc()?);
        }
      }
      upto += 1;
      if terms_enum.next()?.is_none() {
        break;
      }
    }
    assert_eq!(upto, field.terms.len());
  }

  Ok(())
}

fn verify_docs<R, P>(
  random: &mut R,
  docs: &[i32],
  positions: Option<&[Vec<PositionData>]>,
  postings_enum: &mut P,
  do_pos: bool,
) -> Result<()>
where
  R: Rng + ?Sized,
  P: PostingsEnum,
{
  for (i, expected_doc) in docs.iter().enumerate() {
    let doc = postings_enum.next_doc()?;
    assert_ne!(NO_MORE_DOCS, doc);
    assert_eq!(*expected_doc, doc);
    if do_pos {
      verify_positions(random, &positions.unwrap()[i], postings_enum)?;
    }
  }
  assert_eq!(NO_MORE_DOCS, postings_enum.next_doc()?);
  Ok(())
}

fn verify_positions<R, P>(
  random: &mut R,
  positions: &[PositionData],
  pos_enum: &mut P,
) -> Result<()>
where
  R: Rng + ?Sized,
  P: PostingsEnum,
{
  for position in positions {
    let pos = pos_enum.next_position()?;
    assert_eq!(position.pos, pos);
    if let Some(payload) = &position.payload {
      let other_payload = pos_enum
        .get_payload()?
        .ok_or_else(|| LuceneError::illegal_state("payload missing"))?;
      if random.random_range(0..3) < 2 {
        assert_eq!(payload, other_payload.as_ref());
      }
    } else {
      assert!(pos_enum.get_payload()?.is_none());
    }
  }
  Ok(())
}

struct FieldData {
  field_info: Arc<FieldInfo>,
  terms: Vec<TermData>,
  omit_tf: bool,
  store_payloads: bool,
}

impl FieldData {
  fn new(
    name: &str,
    field_infos: &mut Builder,
    mut terms: Vec<TermData>,
    omit_tf: bool,
    store_payloads: bool,
  ) -> Result<Self> {
    let field_info = if let Some(field_info) = field_infos.field_info(name) {
      field_info
    } else {
      let index_options = if omit_tf {
        IndexOptions::Docs
      } else {
        IndexOptions::DocsAndFreqsAndPositions
      };
      field_infos.add(Arc::new(FieldInfo::new(
        name,
        -1,
        false,
        false,
        store_payloads,
        index_options,
        DocValuesType::None,
        DocValuesSkipIndexType::None,
        -1,
        HashMap::new(),
        0,
        0,
        0,
        0,
        VectorEncoding::FLOAT32(4),
        VectorSimilarityFunction::Euclidean,
        false,
        false,
      )?))?
    };
    for term in &mut terms {
      term.field_ = Some(Arc::clone(&field_info));
    }
    terms.sort_by(|left, right| left.text.cmp(&right.text));
    Ok(Self {
      field_info,
      terms,
      omit_tf,
      store_payloads,
    })
  }
}

struct PositionData {
  pos: i32,
  payload: Option<BytesRef<Vec<u8>>>,
}

struct TermData {
  text2: String,
  text: BytesRef<Vec<u8>>,
  docs: Vec<i32>,
  positions: Option<Vec<Vec<PositionData>>>,
  field_: Option<Arc<FieldInfo>>,
}

impl TermData {
  fn new(text: String, docs: Vec<i32>, positions: Option<Vec<Vec<PositionData>>>) -> Self {
    let bytes = BytesRef::from_string(&text);
    Self {
      text2: text,
      text: bytes,
      docs,
      positions,
      field_: None,
    }
  }
}
fn make_random_terms<R: Rng + ?Sized>(
  random: &mut R,
  omit_tf: bool,
  store_payloads: bool,
) -> Result<Vec<TermData>> {
  let num_terms = 1 + random.random_range(0..NUM_TERMS_RAND);
  let mut terms = Vec::with_capacity(num_terms as usize);
  let mut terms_seen = HashSet::new();

  for _ in 0..num_terms {
    let text2 = loop {
      let text2 = TestUtil::random_unicode_string(random);
      if !terms_seen.contains(&text2) && !text2.ends_with('.') {
        terms_seen.insert(text2.clone());
        break text2;
      }
    };

    let doc_freq = 1 + random.random_range(0..DOC_FREQ_RAND);
    let mut docs = Vec::with_capacity(doc_freq as usize);
    let mut positions = if omit_tf {
      None
    } else {
      Some(Vec::with_capacity(doc_freq as usize))
    };

    let mut doc_id = 0;
    for _ in 0..doc_freq {
      doc_id += TestUtil::next_int(random, 1, 10);
      docs.push(doc_id);

      if !omit_tf {
        let term_freq = 1 + random.random_range(0..TERM_DOC_FREQ_RAND);
        let mut doc_positions = Vec::with_capacity(term_freq as usize);
        let mut position = 0;

        for _ in 0..term_freq {
          position += TestUtil::next_int(random, 1, 10);

          let payload = if store_payloads && random.random_range(0..4) == 0 {
            let len = 1 + random.random_range(0..5);
            let mut bytes = Vec::with_capacity(len as usize);
            for _ in 0..len {
              bytes.push(random.random_range(0..255) as u8);
            }
            Some(BytesRef::from_bytes(bytes))
          } else {
            None
          };

          doc_positions.push(PositionData {
            pos: position,
            payload,
          });
        }

        positions.as_mut().unwrap().push(doc_positions);
      }
    }

    terms.push(TermData::new(text2, docs, positions));
  }

  Ok(terms)
}

#[derive(Clone)]
struct NormsProducerImpl {
  max_doc: i32,
}

impl CloseableRef for NormsProducerImpl {}

impl NormsProducer for NormsProducerImpl {
  type NumericDocValues = NumericDocValuesImpl;

  fn get_norms(&self, _field: &Arc<FieldInfo>) -> Result<Self::NumericDocValues> {
    Ok(NumericDocValuesImpl {
      doc: -1,
      max_doc: self.max_doc,
    })
  }

  fn check_integrity(&self) -> Result<()> {
    Ok(())
  }
}

struct NumericDocValuesImpl {
  doc: i32,
  max_doc: i32,
}

impl DocIdSetIterator for NumericDocValuesImpl {
  fn doc_id(&self) -> i32 {
    self.doc
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.advance(self.doc + 1)
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    if target >= self.max_doc {
      self.doc = NO_MORE_DOCS;
    } else {
      self.doc = target;
    }
    Ok(self.doc)
  }

  fn cost(&self) -> Result<i64> {
    Ok(self.max_doc as i64)
  }
}

impl DocValuesIterator for NumericDocValuesImpl {
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    self.doc = target;
    Ok(true)
  }
}

impl NumericDocValues for NumericDocValuesImpl {
  fn long_value(&mut self) -> Result<i64> {
    Ok(1)
  }
}

impl Clone for PositionData {
  fn clone(&self) -> Self {
    Self {
      pos: self.pos,
      payload: self.payload.clone(),
    }
  }
}

impl Clone for TermData {
  fn clone(&self) -> Self {
    Self {
      text2: self.text2.clone(),
      text: self.text.clone(),
      docs: self.docs.clone(),
      positions: self.positions.clone(),
      field_: self.field_.clone(),
    }
  }
}
struct DataFields {
  fields: Vec<Arc<FieldData>>,
  field_names: Vec<String>,
}

impl DataFields {
  fn new(mut fields: Vec<Arc<FieldData>>) -> Self {
    fields.sort_by(|left, right| left.field_info.name.cmp(&right.field_info.name));
    let field_names = fields
      .iter()
      .map(|field| field.field_info.name.clone())
      .collect();
    Self {
      fields,
      field_names,
    }
  }
}

impl Fields for DataFields {
  type FieldIter<'a>
    = VecIter<'a, String>
  where
    Self: 'a;

  fn iterator(&self) -> Result<Self::FieldIter<'_>> {
    Ok(self.field_names.iter_ext())
  }

  type Terms = DataTerms;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    Ok(
      self
        .fields
        .iter()
        .find(|field_data| field_data.field_info.name == field)
        .map(|field_data| DataTerms::new(Arc::clone(field_data))),
    )
  }

  fn size(&self) -> Result<i32> {
    Ok(self.fields.len() as i32)
  }
}
struct DataTerms {
  field_data: Arc<FieldData>,
}

impl DataTerms {
  fn new(field_data: Arc<FieldData>) -> Self {
    Self { field_data }
  }
}

impl Terms for DataTerms {
  type TermsEnum = DataTermsEnum;

  fn iterator(&self) -> Result<Self::TermsEnum> {
    Ok(DataTermsEnum::new(Arc::clone(&self.field_data)))
  }

  type IntersectIter = DataTermsEnum;

  fn intersect(
    &self,
    _compiled: &crate::core::util::automation::compiled_automaton::CompiledAutomaton,
    _start_term: Option<&BytesRef<Vec<u8>>>,
  ) -> Result<Self::IntersectIter> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn size(&self) -> Result<i64> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_sum_total_term_freq(&self) -> Result<i64> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_sum_doc_freq(&self) -> Result<i64> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_doc_count(&self) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn has_freqs(&self) -> bool {
    self.field_data.field_info.get_index_options() >= &IndexOptions::DocsAndFreqs
  }

  fn has_offsets(&self) -> bool {
    self.field_data.field_info.get_index_options()
      >= &IndexOptions::DocsAndFreqsAndPositionsAndOffsets
  }

  fn has_positions(&self) -> bool {
    self.field_data.field_info.get_index_options() >= &IndexOptions::DocsAndFreqsAndPositions
  }

  fn has_payloads(&self) -> bool {
    self.field_data.field_info.has_payloads()
  }
}

struct DataTermsEnum {
  field_data: Arc<FieldData>,
  upto: i32,
  attributes: DummyAttributeSource,
}

impl DataTermsEnum {
  fn new(field_data: Arc<FieldData>) -> Self {
    Self {
      field_data,
      upto: -1,
      attributes: DummyAttributeSource,
    }
  }
}

impl BytesRefIterator for DataTermsEnum {
  fn next(&mut self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
    self.upto += 1;
    if self.upto as usize == self.field_data.terms.len() {
      return Ok(None);
    }
    Ok(Some(Cow::Borrowed(
      &self.field_data.terms[self.upto as usize].text,
    )))
  }
}

impl TermsEnum for DataTermsEnum {
  type AttributeSource<'a>
    = &'a DummyAttributeSource
  where
    Self: 'a;
  type AttributeSourceMut<'a>
    = &'a mut DummyAttributeSource
  where
    Self: 'a;

  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    Ok(&self.attributes)
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    Ok(&mut self.attributes)
  }

  fn seek_exact(&mut self, term: &BytesRef<Vec<u8>>) -> Result<bool> {
    Ok(self.seek_ceil(term)? == SeekStatus::Found)
  }

  fn prepare_seek_exact(&mut self, _text: &BytesRef<Vec<u8>>) -> Result<Option<()>> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_prepare_seek_exact_status(&mut self, _target: &BytesRef<Vec<u8>>) -> Result<bool> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn seek_ceil(&mut self, text: &BytesRef<Vec<u8>>) -> Result<SeekStatus> {
    for (i, term_data) in self.field_data.terms.iter().enumerate() {
      match term_data.text.cmp(text) {
        std::cmp::Ordering::Equal => {
          self.upto = i as i32;
          return Ok(SeekStatus::Found);
        },
        std::cmp::Ordering::Greater => {
          self.upto = i as i32;
          return Ok(SeekStatus::NotFound);
        },
        std::cmp::Ordering::Less => {},
      }
    }
    Ok(SeekStatus::End)
  }

  fn seek_exact_with_ord(&mut self, _ord: i64) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn seek_exact_with_state(
    &mut self,
    _term: &BytesRef<Vec<u8>>,
    _state: &TermStateEnum,
  ) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn term(&self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
    Ok(Cow::Borrowed(
      &self.field_data.terms[self.upto as usize].text,
    ))
  }

  fn ord(&self) -> Result<i64> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn doc_freq(&mut self) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    Err(LuceneError::unsupported_operation(""))
  }

  type PostingsEnum = DataPostingsEnum;

  fn postings_with_flags(
    &mut self,
    _reuse: Option<Self::PostingsEnum>,
    _flags: i32,
  ) -> Result<Self::PostingsEnum> {
    Ok(DataPostingsEnum::new(
      self.field_data.terms[self.upto as usize].clone(),
    ))
  }

  type ImpactsEnum = DummyImpactsEnum;

  fn impacts(&mut self, _flags: i32) -> Result<Self::ImpactsEnum> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    Err(LuceneError::unsupported_operation(""))
  }
}

#[derive(Clone)]
struct DataPostingsEnum {
  term_data: TermData,
  doc_upto: i32,
  pos_upto: i32,
}

impl DataPostingsEnum {
  fn new(term_data: TermData) -> Self {
    Self {
      term_data,
      doc_upto: -1,
      pos_upto: 0,
    }
  }
}

impl DocIdSetIterator for DataPostingsEnum {
  fn doc_id(&self) -> i32 {
    self.term_data.docs[self.doc_upto as usize]
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.doc_upto += 1;
    if self.doc_upto as usize == self.term_data.docs.len() {
      return Ok(NO_MORE_DOCS);
    }
    self.pos_upto = -1;
    Ok(self.doc_id())
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    let mut doc = self.next_doc()?;
    while doc != NO_MORE_DOCS && doc < target {
      doc = self.next_doc()?;
    }
    Ok(doc)
  }

  fn cost(&self) -> Result<i64> {
    Err(LuceneError::unsupported_operation(""))
  }
}

impl PostingsEnum for DataPostingsEnum {
  fn freq(&mut self) -> Result<i32> {
    Ok(self.term_data.positions.as_ref().unwrap()[self.doc_upto as usize].len() as i32)
  }

  fn next_position(&mut self) -> Result<i32> {
    self.pos_upto += 1;
    Ok(
      self.term_data.positions.as_ref().unwrap()[self.doc_upto as usize][self.pos_upto as usize]
        .pos,
    )
  }

  fn start_offset(&self) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn end_offset(&self) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_payload(&self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
    Ok(
      self.term_data.positions.as_ref().unwrap()[self.doc_upto as usize][self.pos_upto as usize]
        .payload
        .as_ref()
        .map(Cow::Borrowed),
    )
  }
}

fn write(
  si: &SegmentInfo<DirEnum>,
  field_infos: Arc<FieldInfos>,
  dir: &DirEnum,
  fields: Vec<Arc<FieldData>>,
) -> Result<()> {
  let codec = codec::get_default();
  let io_context = IOContext::default_io_context()?;
  let state = SegmentWriteState::new(get_default_info_stream(), dir, field_infos, &io_context);
  let mut consumer = codec.postings_format().fields_consumer(&state, si)?;
  let fake_norms = NormsProducerImpl {
    max_doc: si.max_doc()?,
  };
  let mut data_fields = DataFields::new(fields);
  consumer.write(&state, si, &mut data_fields, Some(&fake_norms))?;
  consumer.close()
}
#[test]
fn test_docs_only_freq() -> Result<()> {
  // tests that when fields are indexed with DOCS_ONLY, the Codec
  // returns 1 in docsEnum.freq()
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), config)?;

  // we don't need many documents to assert this, but don't use one document either
  let num_docs = at_least(&mut random, 50);
  for _ in 0..num_docs {
    let mut doc = Document::new();
    doc.add(StringField::from_string("f", "doc", Store::No)?);
    writer.add_document(doc)?;
  }
  writer.close()?;

  let term = Term::new("f", BytesRef::from_string("doc"));
  let reader = directory_reader::open(dir.clone())?;
  let context = reader.get_context()?;
  for ctx in context.leaves()? {
    let mut de = ctx.reader().postings(&term)?.unwrap();
    while de.next_doc()? != NO_MORE_DOCS {
      assert_eq!(1, de.freq()?, "wrong freq for doc {}", de.doc_id());
    }
  }
  Ok(())
}
