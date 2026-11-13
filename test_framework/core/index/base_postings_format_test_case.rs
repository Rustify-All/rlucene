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
use crate::core::codecs::codec_formats::{
  BaseCodecFieldsConsumer, BaseCodecFieldsProducer, CodecCompoundFormat, CodecDocValuesFormat,
  CodecFieldInfosFormat, CodecKnnVectorsFormat, CodecLiveDocsFormat, CodecNormsFormat,
  CodecPointsFormat, CodecPostingsFormat, CodecSegmentInfoFormat, CodecStoredFieldsFormat,
  CodecTermVectorsFormat,
};
use crate::core::codecs::fields_consumer::FieldsConsumer;
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::codecs::postings_format::PostingsFormat;
use crate::core::codecs::{Codec, Codecs};
use crate::core::document::document::Document;
use crate::core::document::field::Store::No;
use crate::core::document::field::{Field, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::fields::FieldTokenStreamEnum;
use crate::core::document::string_field::StringField;
use crate::core::document::text_field::TextField;
use crate::core::index::BytesRef;
use crate::core::index::BytesRefValue;
use crate::core::index::directory_reader;
use crate::core::index::fields::Fields;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_reader::{Identity, IndexReader};
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::{IndexWriter, MAX_TERM_LENGTH};
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::indexable_field::IndexableField;
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::postings_enum::{
  ALL, FREQS, NONE, OFFSETS, PAYLOADS, POSITIONS, PostingsEnum,
};
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::serial_merge_scheduler::SerialMergeScheduler;
use crate::core::index::term::Term;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::term_query::TermQuery;
use crate::core::store::directory::Directory;
use crate::core::store::{Context, IndexInput, IndexOutput};
use crate::core::util::HasIdentity;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind, Result};
use crate::test_framework::core::analysis::canned_token_stream::CannedTokenStream;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::analysis::mock_tokenizer::MockTokenizer;
use crate::test_framework::core::analysis::token;
use crate::test_framework::core::index::base_index_file_format_test_case::{
  BaseIndexFileFormatTestCase, BaseIndexFileFormatTestCaseDefaults,
};
use crate::test_framework::core::index::mismatched_codec_reader::MismatchedCodecReader;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::index::random_postings_tester::Option_;
use crate::test_framework::core::index::random_postings_tester::RandomPostingsTester;
use crate::test_framework::core::util::line_file_docs::LineFileDocs;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, create_temp_dir, create_temp_dir_with_prefix, get_only_leaf_reader,
  new_directory_shared, new_fs_directory, new_index_writer_config,
  new_index_writer_config_with_analyzer, new_log_merge_policy, new_string_field, new_text_field,
  new_tiered_merge_policy, random_from_seed,
};
use crate::test_framework::core::util::test_util::TestUtil;
use parking_lot::Mutex;
use rand::prelude::{SliceRandom, StdRng};
use rand::{Rng, RngExt};
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::thread::ThreadId;
use strum::IntoEnumIterator;

#[derive(Default)]
struct TermFreqs {
  total_term_freq: i64,
  doc_freq: i32,
}

struct InvertedWriteState {
  term_freqs: Mutex<HashMap<String, TermFreqs>>,
  sum_doc_freq: AtomicI64,
  sum_total_term_freq: AtomicI64,
  random: Mutex<StdRng>,
  main_thread: ThreadId,
}

impl InvertedWriteState {
  fn new(random: StdRng) -> Self {
    Self {
      term_freqs: Mutex::new(HashMap::new()),
      sum_doc_freq: AtomicI64::new(0),
      sum_total_term_freq: AtomicI64::new(0),
      random: Mutex::new(random),
      main_thread: std::thread::current().id(),
    }
  }

  fn random_bool(&self) -> bool {
    self.random.lock().random()
  }

  fn next_int(&self, min: i32, max: i32) -> i32 {
    TestUtil::next_int(&mut *self.random.lock(), min, max)
  }

  fn random_realistic_unicode_string(&self) -> String {
    TestUtil::random_realistic_unicode_string(&mut *self.random.lock())
  }
}

/// Rust representation of the anonymous `FilterCodec` in `testInvertedWrite`.
pub struct InvertedWriteCodec {
  delegate: Box<Codecs>,
  state: Arc<InvertedWriteState>,
  postings_identity: Identity,
}

impl InvertedWriteCodec {
  fn new(delegate: Codecs, state: Arc<InvertedWriteState>) -> Result<Self> {
    if matches!(&delegate, Codecs::InvertedWrite(_)) {
      return Err(LuceneError::illegal_argument(
        "InvertedWriteCodec cannot wrap itself",
      ));
    }
    Ok(Self {
      delegate: Box::new(delegate),
      state,
      postings_identity: Identity::new(),
    })
  }
}

impl Clone for InvertedWriteCodec {
  fn clone(&self) -> Self {
    Self {
      delegate: Box::new((*self.delegate).clone()),
      state: Arc::clone(&self.state),
      postings_identity: self.postings_identity.clone(),
    }
  }
}

impl Display for InvertedWriteCodec {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    Display::fmt(self.delegate.as_ref(), f)
  }
}

impl Codec for InvertedWriteCodec {
  type PostingsFormat = InvertedWritePostingsFormat;
  type DocValuesFormat = CodecDocValuesFormat;
  type StoredFieldsFormat = CodecStoredFieldsFormat;
  type TermVectorsFormat = CodecTermVectorsFormat;
  type FieldInfosFormat = CodecFieldInfosFormat;
  type SegmentInfoFormat = CodecSegmentInfoFormat;
  type NormsFormat = CodecNormsFormat;
  type LiveDocsFormat = CodecLiveDocsFormat;
  type CompoundFormat = CodecCompoundFormat;
  type PointsFormat = CodecPointsFormat;
  type KnnVectorsFormat = CodecKnnVectorsFormat;

  fn postings_format(&self) -> Self::PostingsFormat {
    InvertedWritePostingsFormat::new(
      self.delegate.postings_format(),
      Arc::clone(&self.state),
      self.postings_identity.clone(),
    )
  }

  fn doc_values_format(&self) -> Self::DocValuesFormat {
    self.delegate.doc_values_format()
  }

  fn stored_fields_format(&self) -> Self::StoredFieldsFormat {
    self.delegate.stored_fields_format()
  }

  fn term_vectors_format(&self) -> Self::TermVectorsFormat {
    self.delegate.term_vectors_format()
  }

  fn field_infos_format(&self) -> Self::FieldInfosFormat {
    self.delegate.field_infos_format()
  }

  fn segment_info_format(&self) -> Self::SegmentInfoFormat {
    self.delegate.segment_info_format()
  }

  fn norms_format(&self) -> Self::NormsFormat {
    self.delegate.norms_format()
  }

  fn live_docs_format(&self) -> Self::LiveDocsFormat {
    self.delegate.live_docs_format()
  }

  fn compound_format(&self) -> Self::CompoundFormat {
    self.delegate.compound_format()
  }

  fn points_format(&self) -> Self::PointsFormat {
    self.delegate.points_format()
  }

  fn knn_vectors_format(&self) -> Result<Self::KnnVectorsFormat> {
    self.delegate.knn_vectors_format()
  }

  fn get_name(&self) -> &str {
    self.delegate.get_name()
  }
}

pub struct InvertedWritePostingsFormat {
  in_: Box<CodecPostingsFormat>,
  state: Arc<InvertedWriteState>,
  identity: Identity,
}

impl InvertedWritePostingsFormat {
  fn new(in_: CodecPostingsFormat, state: Arc<InvertedWriteState>, identity: Identity) -> Self {
    Self {
      in_: Box::new(in_),
      state,
      identity,
    }
  }
}

impl HasIdentity for InvertedWritePostingsFormat {
  fn identity(&self) -> &Identity {
    &self.identity
  }
}

impl PostingsFormat for InvertedWritePostingsFormat {
  fn get_name(&self) -> &str {
    self.in_.get_name()
  }

  type FieldsConsumer<O: IndexOutput> = InvertedWriteFieldsConsumer<BaseCodecFieldsConsumer<O>>;

  fn fields_consumer<D1, D2>(
    &self,
    state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::FieldsConsumer<D1::IndexOutput>>
  where
    D1: Directory,
  {
    Ok(InvertedWriteFieldsConsumer::new(
      self.in_.base_fields_consumer(state, segment_info)?,
      Arc::clone(&self.state),
    ))
  }

  type FieldsProducer<I: IndexInput> = BaseCodecFieldsProducer<I>;

  fn fields_producer<D1, D2>(
    &self,
    state: &SegmentReadState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::FieldsProducer<D1::IndexInput>>
  where
    D1: Directory,
  {
    self.in_.base_fields_producer(state, segment_info)
  }

  fn for_name(name: &str) -> Result<Arc<Self>> {
    Err(LuceneError::illegal_argument(format!(
      "Could not load postings format named \"{name}\""
    )))
  }
}

pub struct InvertedWriteFieldsConsumer<FC> {
  fields_consumer: FC,
  state: Arc<InvertedWriteState>,
}

impl<FC> InvertedWriteFieldsConsumer<FC> {
  fn new(in_: FC, state: Arc<InvertedWriteState>) -> Self {
    Self {
      fields_consumer: in_,
      state,
    }
  }
}

impl<FC> Closeable for InvertedWriteFieldsConsumer<FC>
where
  FC: FieldsConsumer,
{
  fn close(&mut self) -> Result<()> {
    self.fields_consumer.close()
  }
}

impl<FC> FieldsConsumer for InvertedWriteFieldsConsumer<FC>
where
  FC: FieldsConsumer,
{
  fn write<D1, D2, F, N>(
    &mut self,
    state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    fields: &mut F,
    norms: Option<&N>,
  ) -> Result<()>
  where
    D1: Directory,
    F: Fields,
    N: NormsProducer,
  {
    self
      .fields_consumer
      .write(state, segment_info, fields, norms)?;

    let is_merge = matches!(state.context.get_context(), Context::Merge);
    assert!(
      is_merge || std::thread::current().id() == self.state.main_thread,
      "flush must run on the test thread"
    );

    // We iterate the provided TermsEnum twice, so we exercise the freedom to revisit the inverted
    // API. If `add_on_second_pass` is true, term statistics are accumulated on the second pass.
    let add_on_second_pass = self.state.random_bool();
    let terms = fields
      .terms("body")?
      .expect("the indexed body field must have terms");
    let mut terms_enum = terms.iterator()?;
    let mut docs = None;

    loop {
      let term_string = match terms_enum.next()? {
        Some(term) => term.utf8_to_string()?.to_owned(),
        None => break,
      };
      let no_positions = self.state.random_bool();
      let reuse = if no_positions { docs.take() } else { None };
      docs = Some(terms_enum.postings_with_flags(
        reuse,
        if no_positions {
          FREQS as i32
        } else {
          POSITIONS as i32
        },
      )?);

      let postings = docs
        .as_mut()
        .expect("TermsEnum.postings must return an iterator");
      let mut doc_freq = 0;
      let mut total_term_freq = 0_i64;
      while postings.next_doc()? != NO_MORE_DOCS {
        doc_freq += 1;
        let freq = postings.freq()?;
        total_term_freq += i64::from(freq);
        let limit = self.state.next_int(1, freq);
        if !no_positions {
          for _ in 0..limit {
            postings.next_position()?;
          }
        }
      }

      let mut term_freqs = self.state.term_freqs.lock();
      assert!(
        !is_merge || term_freqs.contains_key(&term_string),
        "merge encountered a term that was not seen during flush"
      );
      if !is_merge {
        if !add_on_second_pass {
          let term_freqs = term_freqs.entry(term_string).or_default();
          term_freqs.doc_freq += doc_freq;
          term_freqs.total_term_freq += total_term_freq;
          self
            .state
            .sum_doc_freq
            .fetch_add(i64::from(doc_freq), Ordering::SeqCst);
          self
            .state
            .sum_total_term_freq
            .fetch_add(total_term_freq, Ordering::SeqCst);
        } else {
          term_freqs.entry(term_string).or_default();
        }
      }
    }

    // Also test seeking the TermsEnum.
    let terms_to_seek = self
      .state
      .term_freqs
      .lock()
      .keys()
      .cloned()
      .collect::<Vec<_>>();
    for term in terms_to_seek {
      if terms_enum.seek_exact(&BytesRef::<Vec<u8>>::from_string(&term))? {
        let no_positions = self.state.random_bool();
        let reuse = if no_positions { docs.take() } else { None };
        docs = Some(terms_enum.postings_with_flags(
          reuse,
          if no_positions {
            FREQS as i32
          } else {
            POSITIONS as i32
          },
        )?);

        let postings = docs
          .as_mut()
          .expect("TermsEnum.postings must return an iterator");
        let mut doc_freq = 0;
        let mut total_term_freq = 0_i64;
        while postings.next_doc()? != NO_MORE_DOCS {
          doc_freq += 1;
          let freq = postings.freq()?;
          total_term_freq += i64::from(freq);
          let limit = self.state.next_int(1, freq);
          if !no_positions {
            for _ in 0..limit {
              postings.next_position()?;
            }
          }
        }

        let mut term_freqs = self.state.term_freqs.lock();
        if !is_merge && add_on_second_pass {
          let term_freqs = term_freqs
            .get_mut(&term)
            .expect("the first pass must create a term entry");
          term_freqs.doc_freq += doc_freq;
          term_freqs.total_term_freq += total_term_freq;
          self
            .state
            .sum_doc_freq
            .fetch_add(i64::from(doc_freq), Ordering::SeqCst);
          self
            .state
            .sum_total_term_freq
            .fetch_add(total_term_freq, Ordering::SeqCst);
        }
        let term_freqs = term_freqs
          .get(&term)
          .expect("the term must have statistics");
        assert!(doc_freq <= term_freqs.doc_freq);
        assert!(total_term_freq <= term_freqs.total_term_freq);
      }
    }

    // Also test seekCeil.
    for _ in 0..10 {
      let term = BytesRef::<Vec<u8>>::from_string(&self.state.random_realistic_unicode_string());
      if terms_enum.seek_ceil(&term)? == SeekStatus::NotFound {
        assert!(term.as_bytes() < terms_enum.term()?.as_bytes());
      }
    }

    Ok(())
  }
}

pub struct BasePostingsFormatTestCaseDefaults;

pub trait BasePostingsFormatTestCase:
  BaseIndexFileFormatTestCase<Defaults = BasePostingsFormatTestCaseDefaults>
{
  fn create_postings<R>(&self, random: &mut R) -> &Mutex<RandomPostingsTester>
  where
    R: Rng + ?Sized;

  fn test_docs_only<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let mut postings_tester = self.create_postings(random).lock();
    postings_tester.test_full(
      random,
      &self.get_codec()?,
      create_temp_dir_with_prefix("testPostingsFormat.testExact")?,
      IndexOptions::Docs,
      false,
    )
  }

  fn test_docs_and_freqs<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let mut postings_tester = self.create_postings(random).lock();
    postings_tester.test_full(
      random,
      &self.get_codec()?,
      create_temp_dir_with_prefix("testPostingsFormat.testExact")?,
      IndexOptions::DocsAndFreqs,
      false,
    )
  }

  fn test_docs_and_freqs_and_positions<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let mut postings_tester = self.create_postings(random).lock();
    postings_tester.test_full(
      random,
      &self.get_codec()?,
      create_temp_dir_with_prefix("testPostingsFormat.testExact")?,
      IndexOptions::DocsAndFreqsAndPositions,
      false,
    )
  }

  fn test_docs_and_freqs_and_positions_and_payloads<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let mut postings_tester = self.create_postings(random).lock();
    postings_tester.test_full(
      random,
      &self.get_codec()?,
      create_temp_dir_with_prefix("testPostingsFormat.testExact")?,
      IndexOptions::DocsAndFreqsAndPositions,
      true,
    )
  }

  fn test_docs_and_freqs_and_positions_and_offsets<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let mut postings_tester = self.create_postings(random).lock();
    postings_tester.test_full(
      random,
      &self.get_codec()?,
      create_temp_dir_with_prefix("testPostingsFormat.testExact")?,
      IndexOptions::DocsAndFreqsAndPositionsAndOffsets,
      false,
    )
  }

  fn test_docs_and_freqs_and_positions_and_offsets_and_payloads<R>(
    &self,
    random: &mut R,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let mut postings_tester = self.create_postings(random).lock();
    postings_tester.test_full(
      random,
      &self.get_codec()?,
      create_temp_dir_with_prefix("testPostingsFormat.testExact")?,
      IndexOptions::DocsAndFreqsAndPositionsAndOffsets,
      true,
    )
  }

  fn test_random<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let iters = 5;
    for _ in 0..iters {
      let path = create_temp_dir_with_prefix("testPostingsFormat")?;
      let dir = new_fs_directory(random, path)?;

      let index_payloads = random.random_bool(0.5);
      let mut postings_tester = self.create_postings(random).lock();
      let fields_producer = postings_tester.build_index(
        &self.get_codec()?,
        dir.clone(),
        IndexOptions::DocsAndFreqsAndPositionsAndOffsets,
        index_payloads,
        false,
      )?;

      postings_tester.test_fields(&fields_producer)?;

      let opts: HashSet<Option_> = Option_::iter().collect();

      postings_tester.test_terms(
        random,
        &fields_producer,
        &opts,
        IndexOptions::DocsAndFreqsAndPositionsAndOffsets,
        IndexOptions::DocsAndFreqsAndPositionsAndOffsets,
        false,
      )?;

      drop(fields_producer);
      drop(dir);
    }
    Ok(())
  }

  fn is_postings_enum_reuse_implemented(&self) -> bool {
    true
  }
  fn test_postings_enum_reuse<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let path = create_temp_dir_with_prefix("testPostingsEnumReuse")?;
    let dir = new_fs_directory(random, path)?;

    let mut postings_tester = self.create_postings(random).lock();
    let fields_producer = postings_tester.build_index(
      &self.get_codec()?,
      dir.clone(),
      IndexOptions::DocsAndFreqsAndPositionsAndOffsets,
      random.random_bool(0.5),
      true,
    )?;

    let mut all_terms = postings_tester.all_terms().iter().collect::<Vec<_>>();
    all_terms.shuffle(random);
    let field_and_term = all_terms.into_iter().next().unwrap();

    let terms = fields_producer.terms(field_and_term.field())?.unwrap();
    let mut terms_enum = terms.iterator()?;

    assert!(terms_enum.seek_exact(field_and_term.term())?);
    self.check_reuse(&mut terms_enum, FREQS as i32, ALL as i32, false)?;
    if self.is_postings_enum_reuse_implemented() {
      self.check_reuse(&mut terms_enum, ALL as i32, ALL as i32, true)?;
    }
    Ok(())
  }

  fn check_reuse<TE>(
    &self,
    terms_enum: &mut TE,
    first_flags: i32,
    second_flags: i32,
    _should_reuse: bool,
  ) -> Result<()>
  where
    TE: TermsEnum,
    TE::PostingsEnum: PostingsEnum,
  {
    let postings1 = terms_enum.postings_with_flags(None, first_flags)?;
    let _postings2 = terms_enum.postings_with_flags(Some(postings1), second_flags)?;
    Ok(())
  }

  fn test_just_empty_field<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let mut iwc = new_index_writer_config(random)?;
    iwc.set_codec(self.get_codec()?);
    let iw = RandomIndexWriter::with_config(random, dir, iwc);
    let mut doc = Document::new();
    let mut field_types = HashMap::new();
    doc.add(new_string_field(
      random,
      "",
      "something",
      No,
      &mut field_types,
    )?);
    iw.add_document(random, doc)?;
    let ir = iw.get_reader(random)?;
    let ar = get_only_leaf_reader(ir)?;
    assert_eq!(1, ar.get_field_infos()?.size());
    let terms = ar.terms("")?.unwrap();
    let mut terms_enum = terms.iterator()?;
    let term = terms_enum.next()?.unwrap();
    assert_eq!(term.as_bytes(), b"something");
    assert!(terms_enum.next()?.is_none());
    Ok(())
  }

  fn test_empty_field_and_empty_term<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let mut iwc = new_index_writer_config(random)?;
    iwc.set_codec(self.get_codec()?);
    let iw = RandomIndexWriter::with_config(random, dir, iwc);
    let mut doc = Document::new();
    let mut field_types = HashMap::new();
    doc.add(new_string_field(random, "", "", No, &mut field_types)?);
    iw.add_document(random, doc)?;
    let ir = iw.get_reader(random)?;
    let ar = get_only_leaf_reader(ir)?;
    assert_eq!(1, ar.get_field_infos()?.size());
    let terms = ar.terms("")?.unwrap();
    let mut terms_enum = terms.iterator()?;
    let term = terms_enum.next()?.unwrap();
    assert_eq!(term.as_bytes(), b"");
    assert!(terms_enum.next()?.is_none());
    Ok(())
  }

  fn test_didnt_want_freqs_but_asked_anyway<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let mut iwc = new_index_writer_config(random)?;
    iwc.set_codec(self.get_codec()?);
    let iw = RandomIndexWriter::with_config(random, dir, iwc);
    let mut doc = Document::new();
    let mut field_types = HashMap::new();
    doc.add(new_text_field(
      random,
      "field",
      "value",
      No,
      &mut field_types,
    )?);
    iw.add_document(random, &mut doc)?;
    iw.add_document(random, &mut doc)?;
    let ir = iw.get_reader(random)?;
    let ar = get_only_leaf_reader(ir)?;
    let mut terms_enum = ar.terms("field")?.unwrap().iterator()?;
    assert!(terms_enum.seek_exact(&BytesRef::<Vec<u8>>::from_string("value"))?);
    let mut docs_enum = terms_enum.postings_with_flags(None, NONE as i32)?;
    assert_eq!(0, docs_enum.next_doc()?);
    assert_eq!(1, docs_enum.freq()?);
    assert_eq!(1, docs_enum.next_doc()?);
    assert_eq!(1, docs_enum.freq()?);
    Ok(())
  }

  fn test_ask_for_positions_when_not_there<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let mut iwc = new_index_writer_config(random)?;
    iwc.set_codec(self.get_codec()?);
    let iw = RandomIndexWriter::with_config(random, dir, iwc);
    let mut doc = Document::new();
    let mut field_types = HashMap::new();
    doc.add(new_string_field(
      random,
      "field",
      "value",
      No,
      &mut field_types,
    )?);
    iw.add_document(random, &mut doc)?;
    iw.add_document(random, &mut doc)?;
    let ir = iw.get_reader(random)?;
    let ar = get_only_leaf_reader(ir)?;
    let mut terms_enum = ar.terms("field")?.unwrap().iterator()?;
    assert!(terms_enum.seek_exact(&BytesRef::<Vec<u8>>::from_string("value"))?);
    let mut docs_enum = terms_enum.postings_with_flags(None, POSITIONS as i32)?;
    assert_eq!(0, docs_enum.next_doc()?);
    assert_eq!(1, docs_enum.freq()?);
    assert_eq!(1, docs_enum.next_doc()?);
    assert_eq!(1, docs_enum.freq()?);
    Ok(())
  }

  // tests that ghost fields still work
  fn test_ghosts<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let mut iwc = new_index_writer_config(random)?;
    iwc.set_codec(self.get_codec()?);
    iwc.base.merge_policy = new_log_merge_policy(random)?;
    let iw = IndexWriter::new(dir.clone(), iwc)?;
    let doc = Document::new();
    iw.add_document(doc)?;
    let mut doc = Document::new();
    let mut field_types = HashMap::new();
    doc.add(new_string_field(
      random,
      "ghostField",
      "something",
      No,
      &mut field_types,
    )?);
    iw.add_document(doc)?;
    iw.force_merge(1)?;
    iw.delete_documents_with_terms(vec![Term::from_text("ghostField", "something")])?;
    iw.force_merge(1)?;
    let ir = iw.get_reader(true, false)?;
    let ar = get_only_leaf_reader(ir)?;
    assert!(ar.get_field_infos()?.size() <= 1);
    if let Some(terms) = ar.terms("ghostField")? {
      let mut terms_enum = terms.iterator()?;
      if terms_enum.next()?.is_some() {
        let mut postings_enum = terms_enum.postings(None)?;
        assert_eq!(NO_MORE_DOCS, postings_enum.next_doc()?);
      }
    }
    Ok(())
  }

  // Test seek in disorder.
  fn test_disorder<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let mut iwc = new_index_writer_config(random)?;
    iwc.set_codec(self.get_codec()?);
    iwc.base.merge_policy = new_tiered_merge_policy(random)?.into();
    let iw = IndexWriter::new(dir.clone(), iwc)?;

    for i in 0..10000 {
      let mut document = Document::new();
      document.add(StringField::from_string("id", i.to_string(), No)?);
      iw.add_document(document)?;
    }
    iw.commit()?;
    iw.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&iw)?;
    let mut terms_enum = get_only_leaf_reader(&reader)?
      .terms("id")?
      .unwrap()
      .iterator()?;

    for _ in 0..20000 {
      let n = random.random_range(0..10000);
      let target = BytesRef::<Vec<u8>>::from_string(&n.to_string());
      assert!(terms_enum.seek_exact(&target)?);
      assert_eq!(terms_enum.term()?.as_bytes(), target.as_byte_slice());
      assert_eq!(SeekStatus::Found, terms_enum.seek_ceil(&target)?);
      assert_eq!(terms_enum.term()?.as_bytes(), target.as_byte_slice());
    }

    reader.close()?;
    iw.close()?;
    dir.close()?;
    Ok(())
  }

  fn sub_check_binary_search<TE>(&self, _terms_enum: &mut TE) -> Result<()>
  where
    TE: TermsEnum,
    TE::PostingsEnum: PostingsEnum,
  {
    Ok(())
  }

  fn test_binary_search_term_leaf<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let mut iwc = new_index_writer_config(random)?;
    iwc.set_codec(self.get_codec()?);
    iwc.base.merge_policy = new_tiered_merge_policy(random)?.into();
    let iw = IndexWriter::new(dir.clone(), iwc)?;

    for i in 100000..=100400 {
      if i % 2 == 1 {
        let mut document = Document::new();
        document.add(StringField::from_string("id", i.to_string(), No)?);
        iw.add_document(document)?;
      }
    }
    iw.commit()?;
    iw.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&iw)?;
    let mut terms_enum = get_only_leaf_reader(&reader)?
      .terms("id")?
      .unwrap()
      .iterator()?;

    for i in 100000..=100400 {
      let target = BytesRef::<Vec<u8>>::from_string(&i.to_string());
      if i % 2 == 1 {
        assert!(terms_enum.seek_exact(&target)?);
        assert_eq!(terms_enum.term()?.as_bytes(), target.as_byte_slice());
      } else {
        assert!(!terms_enum.seek_exact(&target)?);
      }
    }

    self.sub_check_binary_search(&mut terms_enum)?;

    for i in 100000..100400 {
      let target = BytesRef::<Vec<u8>>::from_string(&i.to_string());
      if i % 2 == 1 {
        assert_eq!(SeekStatus::Found, terms_enum.seek_ceil(&target)?);
        assert_eq!(terms_enum.term()?.as_bytes(), target.as_byte_slice());
        if i <= 100397 {
          let next_term = terms_enum.next()?.unwrap();
          let expected_next = BytesRef::<Vec<u8>>::from_string(&(i + 2).to_string());
          assert_eq!(next_term.as_bytes(), expected_next.as_byte_slice());
        }
      } else {
        assert_eq!(SeekStatus::NotFound, terms_enum.seek_ceil(&target)?);
        assert_eq!(
          terms_enum.term()?.as_bytes(),
          BytesRef::<Vec<u8>>::from_string(&(i + 1).to_string()).as_byte_slice()
        );
      }
    }
    assert_eq!(
      SeekStatus::End,
      terms_enum.seek_ceil(&BytesRef::<Vec<u8>>::from_string("100400"))?
    );

    reader.close()?;
    iw.close()?;
    dir.close()?;
    Ok(())
  }

  // tests that level 2 ghost fields still work
  fn test_level2_ghosts<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let mut iwc = new_index_writer_config(random)?;
    iwc.set_codec(self.get_codec()?);
    iwc.base.merge_policy = new_log_merge_policy(random)?;
    let iw = IndexWriter::new(dir.clone(), iwc)?;

    let mut document = Document::new();
    document.add(StringField::from_string("id", "0", No)?);
    document.add(StringField::from_string("suggest_field", "apples", No)?);
    iw.add_document(document)?;
    iw.add_document(Document::new())?;
    iw.commit()?;

    let mut document = Document::new();
    document.add(StringField::from_string("id", "1", No)?);
    document.add(StringField::from_string("suggest_field2", "apples", No)?);
    iw.add_document(document)?;
    iw.commit()?;

    iw.delete_documents_with_terms(vec![Term::from_text("id", "0")])?;
    iw.force_merge(1)?;

    iw.add_document(Document::new())?;
    iw.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&iw)?;
    let searcher = IndexSearcher::new(reader.get_context()?)?;
    assert_eq!(
      1,
      searcher.count(TermQuery::new(Term::from_text("id", "1")))?
    );

    searcher.reader_context.reader().close()?;
    drop(searcher);
    iw.close()?;
    dir.close()?;
    Ok(())
  }

  // LUCENE-5123: make sure we can visit postings twice during flush/merge.
  fn test_inverted_write<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let mut analyzer = MockAnalyzer::new(random);
    analyzer.set_max_token_length(TestUtil::next_int(random, 1, MAX_TERM_LENGTH));
    let mut iwc = new_index_writer_config_with_analyzer(random, analyzer)?;

    // Must be concurrent because merge threads may iterate this map while the flush thread adds to
    // it. The random stream used by the consumer is derived from the top-level test random so a
    // failing seed remains reproducible across those threads.
    let state = Arc::new(InvertedWriteState::new(random_from_seed(random.random())));
    iwc.set_codec(InvertedWriteCodec::new(
      self.get_codec()?,
      Arc::clone(&state),
    )?);

    let writer = RandomIndexWriter::with_config(random, dir.clone(), iwc);
    let mut docs = LineFileDocs::new(random)?;
    let bytes_to_index = at_least(random, 100) * 1024;
    let mut bytes_indexed = 0;
    while bytes_indexed < bytes_to_index {
      let doc = docs.next_doc()?;
      let body = doc
        .get_field("body")
        .expect("LineFileDocs must have a body");
      let body_value = body
        .string_value()?
        .expect("the body field must have a string value");
      let mut just_body_doc = Document::new();
      just_body_doc.add(TextField::from_string(
        "body",
        body_value.as_ref(),
        Store::No,
      )?);
      writer.add_document(random, just_body_doc)?;
      // Count retained text bytes to choose a realistic amount of input for postings creation.
      bytes_indexed += body_value.len().max(1) as i32;
    }

    let reader = writer.get_reader(random)?;
    writer.close(random)?;

    let terms = crate::core::index::multi_terms::get_terms(&reader, "body")?
      .expect("the body field must have terms");
    assert_eq!(
      state.sum_doc_freq.load(Ordering::SeqCst),
      terms.get_sum_doc_freq()?
    );
    assert_eq!(
      state.sum_total_term_freq.load(Ordering::SeqCst),
      terms.get_sum_total_term_freq()?
    );

    let mut terms_enum = terms.iterator()?;
    let mut term_count = 0_i64;
    let mut supports_ords = true;
    while let Some(term) = terms_enum.next()? {
      let term_string = term.utf8_to_string()?.to_owned();
      let (expected_doc_freq, expected_total_term_freq) = {
        let term_freqs = state.term_freqs.lock();
        let term_freqs = term_freqs
          .get(&term_string)
          .expect("every indexed term must have collected statistics");
        (term_freqs.doc_freq, term_freqs.total_term_freq)
      };
      assert_eq!(expected_doc_freq, terms_enum.doc_freq()?);
      assert_eq!(expected_total_term_freq, terms_enum.total_term_freq()?);
      if supports_ords {
        let ord = match terms_enum.ord() {
          Ok(ord) => ord,
          Err(error) if matches!(error.kind(), LuceneErrorKind::UnsupportedOperation(_)) => {
            supports_ords = false;
            -1
          },
          Err(error) => return Err(error),
        };
        if ord != -1 {
          assert_eq!(term_count, ord);
        }
      }
      term_count += 1;
    }
    assert_eq!(state.term_freqs.lock().len() as i64, term_count);

    drop(terms_enum);
    drop(terms);
    reader.close()?;
    dir.close()?;
    Ok(())
  }

  fn test_postings_enum_docs_only<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let iwc = IndexWriterConfig::new()?;
    let w = IndexWriter::new(dir.clone(), iwc)?;
    let mut doc = Document::new();
    doc.add(StringField::from_string("foo", "bar", No)?);
    w.add_document(doc)?;

    let reader = directory_reader::open_from_writer(&w)?;
    let leaf = get_only_leaf_reader(&reader)?;
    let mut postings = leaf.postings(&Term::from_text("foo", "bar"))?.unwrap();
    assert_eq!(-1, postings.doc_id());
    assert_eq!(0, postings.next_doc()?);
    assert_eq!(1, postings.freq()?);
    assert_eq!(NO_MORE_DOCS, postings.next_doc()?);

    let mut terms_enum = leaf.terms("foo")?.unwrap().iterator()?;
    assert!(terms_enum.seek_exact(&BytesRef::<Vec<u8>>::from_string("bar"))?);
    let mut postings2 = terms_enum.postings(None)?;
    assert_eq!(-1, postings2.doc_id());
    assert_eq!(0, postings2.next_doc()?);
    assert_eq!(1, postings2.freq()?);
    assert_eq!(NO_MORE_DOCS, postings2.next_doc()?);

    for flag in [NONE as i32, FREQS as i32, POSITIONS as i32, ALL as i32] {
      let mut p = terms_enum.postings_with_flags(None, flag)?;
      assert_eq!(-1, p.doc_id());
      assert_eq!(0, p.next_doc()?);
      if flag != NONE as i32 {
        assert_eq!(1, p.freq()?);
      }
      assert_eq!(NO_MORE_DOCS, p.next_doc()?);
      let mut p2 = terms_enum.postings_with_flags(Some(p), flag)?;
      assert_eq!(-1, p2.doc_id());
      assert_eq!(0, p2.next_doc()?);
      if flag != NONE as i32 {
        assert_eq!(1, p2.freq()?);
      }
      assert_eq!(NO_MORE_DOCS, p2.next_doc()?);
    }

    drop(terms_enum);
    drop(leaf);
    w.close()?;
    reader.close()?;
    dir.close()?;
    Ok(())
  }

  fn test_postings_enum_freqs<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let analyzer = MockTokenizerAnalyzer::new(random);
    let iwc = IndexWriterConfig::with_analyzer(analyzer)?;
    let w = IndexWriter::new(dir.clone(), iwc)?;

    let mut ft = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
    ft.set_index_options(IndexOptions::DocsAndFreqs)?;
    let mut doc = Document::new();
    doc.add(Field::from_string("foo", "bar bar", ft)?);
    w.add_document(doc)?;

    let reader = directory_reader::open_from_writer(&w)?;
    let leaf = get_only_leaf_reader(&reader)?;
    let mut postings = leaf.postings(&Term::from_text("foo", "bar"))?.unwrap();
    assert_eq!(-1, postings.doc_id());
    assert_eq!(0, postings.next_doc()?);
    assert_eq!(2, postings.freq()?);
    assert_eq!(NO_MORE_DOCS, postings.next_doc()?);

    let mut terms_enum = leaf.terms("foo")?.unwrap().iterator()?;
    assert!(terms_enum.seek_exact(&BytesRef::<Vec<u8>>::from_string("bar"))?);
    let mut postings2 = terms_enum.postings(None)?;
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

    for flag in [NONE as i32, FREQS as i32, POSITIONS as i32, ALL as i32] {
      let mut p = terms_enum.postings_with_flags(None, flag)?;
      assert_eq!(-1, p.doc_id());
      assert_eq!(0, p.next_doc()?);
      if flag != NONE as i32 {
        assert_eq!(2, p.freq()?);
      }
      assert_eq!(NO_MORE_DOCS, p.next_doc()?);
      let mut p2 = terms_enum.postings_with_flags(Some(p), flag)?;
      assert_eq!(-1, p2.doc_id());
      assert_eq!(0, p2.next_doc()?);
      if flag != NONE as i32 {
        assert_eq!(2, p2.freq()?);
      }
      assert_eq!(NO_MORE_DOCS, p2.next_doc()?);
    }

    drop(terms_enum);
    drop(leaf);
    w.close()?;
    reader.close()?;
    dir.close()?;
    Ok(())
  }

  fn test_postings_enum_positions<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let analyzer = MockTokenizerAnalyzer::new(random);
    let iwc = IndexWriterConfig::with_analyzer(analyzer)?;
    let w = IndexWriter::new(dir.clone(), iwc)?;

    let mut doc = Document::new();
    doc.add(TextField::from_string("foo", "bar bar", Store::No)?);
    w.add_document(doc)?;

    let reader = directory_reader::open_from_writer(&w)?;
    let leaf = get_only_leaf_reader(&reader)?;

    let mut postings = leaf.postings(&Term::from_text("foo", "bar"))?.unwrap();
    assert_eq!(-1, postings.doc_id());
    assert_eq!(0, postings.next_doc()?);
    assert_eq!(2, postings.freq()?);
    assert_eq!(NO_MORE_DOCS, postings.next_doc()?);

    let mut terms_enum = leaf.terms("foo")?.unwrap().iterator()?;
    assert!(terms_enum.seek_exact(&BytesRef::<Vec<u8>>::from_string("bar"))?);

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

    drop(terms_enum);
    drop(leaf);
    w.close()?;
    reader.close()?;
    dir.close()?;
    Ok(())
  }

  fn test_postings_enum_offsets<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let analyzer = MockTokenizerAnalyzer::new(random);
    let iwc = IndexWriterConfig::with_analyzer(analyzer)?;
    let w = IndexWriter::new(dir.clone(), iwc)?;

    let mut ft = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
    ft.set_index_options(IndexOptions::DocsAndFreqsAndPositionsAndOffsets)?;
    let mut doc = Document::new();
    doc.add(Field::from_string("foo", "bar bar", ft)?);
    w.add_document(doc)?;

    let reader = directory_reader::open_from_writer(&w)?;
    let leaf = get_only_leaf_reader(&reader)?;

    let mut postings = leaf.postings(&Term::from_text("foo", "bar"))?.unwrap();
    assert_eq!(-1, postings.doc_id());
    assert_eq!(0, postings.next_doc()?);
    assert_eq!(2, postings.freq()?);
    assert_eq!(NO_MORE_DOCS, postings.next_doc()?);

    let mut terms_enum = leaf.terms("foo")?.unwrap().iterator()?;
    assert!(terms_enum.seek_exact(&BytesRef::<Vec<u8>>::from_string("bar"))?);

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

    drop(terms_enum);
    drop(leaf);
    w.close()?;
    reader.close()?;
    dir.close()?;
    Ok(())
  }

  fn test_postings_enum_payloads<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let iwc = IndexWriterConfig::new()?;
    let w = IndexWriter::new(dir.clone(), iwc)?;

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

    let mut doc = Document::new();
    doc.add(TextField::from_token_stream(
      "foo",
      FieldTokenStreamEnum::custom(CannedTokenStream::new(vec![token1, token2])),
    )?);
    w.add_document(doc)?;

    let reader = directory_reader::open_from_writer(&w)?;
    let leaf = get_only_leaf_reader(&reader)?;
    // sugar method (FREQS)
    let mut postings = leaf.postings(&Term::from_text("foo", "bar"))?.unwrap();
    assert_eq!(-1, postings.doc_id());
    assert_eq!(0, postings.next_doc()?);
    assert_eq!(2, postings.freq()?);
    assert_eq!(NO_MORE_DOCS, postings.next_doc()?);
    // termsenum reuse (FREQS)
    let mut terms_enum = leaf.terms("foo")?.unwrap().iterator()?;
    assert!(terms_enum.seek_exact(&BytesRef::<Vec<u8>>::from_string("bar"))?);

    let mut postings2 = terms_enum.postings(Some(postings))?;
    // and it had better work
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

    let mut docs_and_positions_enum = leaf
      .postings_with_flag(&Term::from_text("foo", "bar"), POSITIONS as i32)?
      .unwrap();
    assert_eq!(-1, docs_and_positions_enum.doc_id());
    assert_eq!(0, docs_and_positions_enum.next_doc()?);
    assert_eq!(2, docs_and_positions_enum.freq()?);
    assert_eq!(0, docs_and_positions_enum.next_position()?);
    assert_eq!(-1, docs_and_positions_enum.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum.end_offset()?);
    assert!(
      docs_and_positions_enum.get_payload()?.is_none()
        || docs_and_positions_enum.get_payload()?.unwrap().as_bytes()
          == BytesRef::<Vec<u8>>::from_string("pay1").as_bytes()
    );
    assert_eq!(1, docs_and_positions_enum.next_position()?);
    assert_eq!(-1, docs_and_positions_enum.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum.end_offset()?);
    assert!(
      docs_and_positions_enum.get_payload()?.is_none()
        || docs_and_positions_enum.get_payload()?.unwrap().as_bytes()
          == BytesRef::<Vec<u8>>::from_string("pay2").as_bytes()
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
        || docs_and_positions_enum2.get_payload()?.unwrap().as_bytes()
          == BytesRef::<Vec<u8>>::from_string("pay1").as_bytes()
    );
    assert_eq!(1, docs_and_positions_enum2.next_position()?);
    assert_eq!(-1, docs_and_positions_enum2.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum2.end_offset()?);
    assert!(
      docs_and_positions_enum2.get_payload()?.is_none()
        || docs_and_positions_enum2.get_payload()?.unwrap().as_bytes()
          == BytesRef::<Vec<u8>>::from_string("pay2").as_bytes()
    );
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum2.next_doc()?);

    let mut docs_and_positions_enum = leaf
      .postings_with_flag(&Term::from_text("foo", "bar"), PAYLOADS as i32)?
      .unwrap();

    assert_eq!(-1, docs_and_positions_enum.doc_id());
    assert_eq!(0, docs_and_positions_enum.next_doc()?);
    assert_eq!(2, docs_and_positions_enum.freq()?);
    assert_eq!(0, docs_and_positions_enum.next_position()?);
    assert_eq!(-1, docs_and_positions_enum.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum.end_offset()?);
    assert_eq!(
      BytesRef::<Vec<u8>>::from_string("pay1").as_bytes(),
      docs_and_positions_enum.get_payload()?.unwrap().as_bytes()
    );
    assert_eq!(1, docs_and_positions_enum.next_position()?);
    assert_eq!(-1, docs_and_positions_enum.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum.end_offset()?);
    assert_eq!(
      BytesRef::<Vec<u8>>::from_string("pay2").as_bytes(),
      docs_and_positions_enum.get_payload()?.unwrap().as_bytes()
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
      BytesRef::<Vec<u8>>::from_string("pay1").as_bytes(),
      docs_and_positions_enum2.get_payload()?.unwrap().as_bytes()
    );
    assert_eq!(1, docs_and_positions_enum2.next_position()?);
    assert_eq!(-1, docs_and_positions_enum2.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum2.end_offset()?);
    assert_eq!(
      BytesRef::<Vec<u8>>::from_string("pay2").as_bytes(),
      docs_and_positions_enum2.get_payload()?.unwrap().as_bytes()
    );
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum2.next_doc()?);

    let mut docs_and_positions_enum = leaf
      .postings_with_flag(&Term::from_text("foo", "bar"), OFFSETS as i32)?
      .unwrap();

    assert_eq!(-1, docs_and_positions_enum.doc_id());
    assert_eq!(0, docs_and_positions_enum.next_doc()?);
    assert_eq!(2, docs_and_positions_enum.freq()?);
    assert_eq!(0, docs_and_positions_enum.next_position()?);
    assert_eq!(-1, docs_and_positions_enum.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum.end_offset()?);
    assert!(
      docs_and_positions_enum.get_payload()?.is_none()
        || docs_and_positions_enum.get_payload()?.unwrap().as_bytes()
          == BytesRef::<Vec<u8>>::from_string("pay1").as_bytes()
    );
    assert_eq!(1, docs_and_positions_enum.next_position()?);
    assert_eq!(-1, docs_and_positions_enum.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum.end_offset()?);
    assert!(
      docs_and_positions_enum.get_payload()?.is_none()
        || docs_and_positions_enum.get_payload()?.unwrap().as_bytes()
          == BytesRef::<Vec<u8>>::from_string("pay2").as_bytes()
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
        || docs_and_positions_enum2.get_payload()?.unwrap().as_bytes()
          == BytesRef::<Vec<u8>>::from_string("pay1").as_bytes()
    );
    assert_eq!(1, docs_and_positions_enum2.next_position()?);
    assert_eq!(-1, docs_and_positions_enum2.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum2.end_offset()?);
    assert!(
      docs_and_positions_enum2.get_payload()?.is_none()
        || docs_and_positions_enum2.get_payload()?.unwrap().as_bytes()
          == BytesRef::<Vec<u8>>::from_string("pay2").as_bytes()
    );
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum2.next_doc()?);
    let mut docs_and_positions_enum = leaf
      .postings_with_flag(&Term::from_text("foo", "bar"), ALL as i32)?
      .unwrap();
    assert_eq!(-1, docs_and_positions_enum.doc_id());
    assert_eq!(0, docs_and_positions_enum.next_doc()?);
    assert_eq!(2, docs_and_positions_enum.freq()?);
    assert_eq!(0, docs_and_positions_enum.next_position()?);
    assert_eq!(-1, docs_and_positions_enum.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum.end_offset()?);
    assert_eq!(
      BytesRef::<Vec<u8>>::from_string("pay1").as_bytes(),
      docs_and_positions_enum.get_payload()?.unwrap().as_bytes()
    );
    assert_eq!(1, docs_and_positions_enum.next_position()?);
    assert_eq!(-1, docs_and_positions_enum.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum.end_offset()?);
    assert_eq!(
      BytesRef::<Vec<u8>>::from_string("pay2").as_bytes(),
      docs_and_positions_enum.get_payload()?.unwrap().as_bytes()
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
      BytesRef::<Vec<u8>>::from_string("pay1").as_bytes(),
      docs_and_positions_enum2.get_payload()?.unwrap().as_bytes()
    );
    assert_eq!(1, docs_and_positions_enum2.next_position()?);
    assert_eq!(-1, docs_and_positions_enum2.start_offset()?);
    assert_eq!(-1, docs_and_positions_enum2.end_offset()?);
    assert_eq!(
      BytesRef::<Vec<u8>>::from_string("pay2").as_bytes(),
      docs_and_positions_enum2.get_payload()?.unwrap().as_bytes()
    );
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum2.next_doc()?);

    drop(terms_enum);
    drop(leaf);
    w.close()?;
    reader.close()?;
    dir.close()?;
    Ok(())
  }

  fn test_postings_enum_all<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let iwc = IndexWriterConfig::new()?;
    let w = IndexWriter::new(dir.clone(), iwc)?;

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
    ft.set_index_options(IndexOptions::DocsAndFreqsAndPositionsAndOffsets)?;

    let mut doc = Document::new();
    doc.add(Field::from_token_stream(
      "foo",
      FieldTokenStreamEnum::custom(CannedTokenStream::new(vec![token1, token2])),
      ft,
    )?);
    w.add_document(doc)?;

    let reader = directory_reader::open_from_writer(&w)?;
    let leaf = get_only_leaf_reader(&reader)?;

    let mut postings = leaf.postings(&Term::from_text("foo", "bar"))?.unwrap();
    assert_eq!(-1, postings.doc_id());
    assert_eq!(0, postings.next_doc()?);
    assert_eq!(2, postings.freq()?);
    assert_eq!(NO_MORE_DOCS, postings.next_doc()?);

    let mut terms_enum = leaf.terms("foo")?.unwrap().iterator()?;
    assert!(terms_enum.seek_exact(&BytesRef::<Vec<u8>>::from_string("bar"))?);

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

    let mut docs_and_positions_enum = leaf
      .postings_with_flag(&Term::from_text("foo", "bar"), POSITIONS as i32)?
      .unwrap();
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
        || docs_and_positions_enum.get_payload()?.unwrap().as_bytes()
          == BytesRef::<Vec<u8>>::from_string("pay1").as_bytes()
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
        || docs_and_positions_enum.get_payload()?.unwrap().as_bytes()
          == BytesRef::<Vec<u8>>::from_string("pay2").as_bytes()
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
        || docs_and_positions_enum2.get_payload()?.unwrap().as_bytes()
          == BytesRef::<Vec<u8>>::from_string("pay1").as_bytes()
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
        || docs_and_positions_enum2.get_payload()?.unwrap().as_bytes()
          == BytesRef::<Vec<u8>>::from_string("pay2").as_bytes()
    );
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum2.next_doc()?);

    let mut docs_and_positions_enum = leaf
      .postings_with_flag(&Term::from_text("foo", "bar"), PAYLOADS as i32)?
      .unwrap();
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
      BytesRef::<Vec<u8>>::from_string("pay1").as_bytes(),
      docs_and_positions_enum.get_payload()?.unwrap().as_bytes()
    );
    assert_eq!(1, docs_and_positions_enum.next_position()?);
    assert!(
      docs_and_positions_enum.start_offset()? == -1 || docs_and_positions_enum.start_offset()? == 4
    );
    assert!(
      docs_and_positions_enum.end_offset()? == -1 || docs_and_positions_enum.end_offset()? == 7
    );
    assert_eq!(
      BytesRef::<Vec<u8>>::from_string("pay2").as_bytes(),
      docs_and_positions_enum.get_payload()?.unwrap().as_bytes()
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
      BytesRef::<Vec<u8>>::from_string("pay1").as_bytes(),
      docs_and_positions_enum2.get_payload()?.unwrap().as_bytes()
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
      BytesRef::<Vec<u8>>::from_string("pay2").as_bytes(),
      docs_and_positions_enum2.get_payload()?.unwrap().as_bytes()
    );
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum2.next_doc()?);

    let mut docs_and_positions_enum = leaf
      .postings_with_flag(&Term::from_text("foo", "bar"), OFFSETS as i32)?
      .unwrap();
    assert_eq!(-1, docs_and_positions_enum.doc_id());
    assert_eq!(0, docs_and_positions_enum.next_doc()?);
    assert_eq!(2, docs_and_positions_enum.freq()?);
    assert_eq!(0, docs_and_positions_enum.next_position()?);
    assert_eq!(0, docs_and_positions_enum.start_offset()?);
    assert_eq!(3, docs_and_positions_enum.end_offset()?);
    assert!(
      docs_and_positions_enum.get_payload()?.is_none()
        || docs_and_positions_enum.get_payload()?.unwrap().as_bytes()
          == BytesRef::<Vec<u8>>::from_string("pay1").as_bytes()
    );
    assert_eq!(1, docs_and_positions_enum.next_position()?);
    assert_eq!(4, docs_and_positions_enum.start_offset()?);
    assert_eq!(7, docs_and_positions_enum.end_offset()?);
    assert!(
      docs_and_positions_enum.get_payload()?.is_none()
        || docs_and_positions_enum.get_payload()?.unwrap().as_bytes()
          == BytesRef::<Vec<u8>>::from_string("pay2").as_bytes()
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
        || docs_and_positions_enum2.get_payload()?.unwrap().as_bytes()
          == BytesRef::<Vec<u8>>::from_string("pay1").as_bytes()
    );
    assert_eq!(1, docs_and_positions_enum2.next_position()?);
    assert_eq!(4, docs_and_positions_enum2.start_offset()?);
    assert_eq!(7, docs_and_positions_enum2.end_offset()?);
    assert!(
      docs_and_positions_enum2.get_payload()?.is_none()
        || docs_and_positions_enum2.get_payload()?.unwrap().as_bytes()
          == BytesRef::<Vec<u8>>::from_string("pay2").as_bytes()
    );
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum2.next_doc()?);

    let mut docs_and_positions_enum = leaf
      .postings_with_flag(&Term::from_text("foo", "bar"), ALL as i32)?
      .unwrap();
    assert_eq!(-1, docs_and_positions_enum.doc_id());
    assert_eq!(0, docs_and_positions_enum.next_doc()?);
    assert_eq!(2, docs_and_positions_enum.freq()?);
    assert_eq!(0, docs_and_positions_enum.next_position()?);
    assert_eq!(0, docs_and_positions_enum.start_offset()?);
    assert_eq!(3, docs_and_positions_enum.end_offset()?);
    assert_eq!(
      BytesRef::<Vec<u8>>::from_string("pay1").as_bytes(),
      docs_and_positions_enum.get_payload()?.unwrap().as_bytes()
    );
    assert_eq!(1, docs_and_positions_enum.next_position()?);
    assert_eq!(4, docs_and_positions_enum.start_offset()?);
    assert_eq!(7, docs_and_positions_enum.end_offset()?);
    assert_eq!(
      BytesRef::<Vec<u8>>::from_string("pay2").as_bytes(),
      docs_and_positions_enum.get_payload()?.unwrap().as_bytes()
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
      BytesRef::<Vec<u8>>::from_string("pay1").as_bytes(),
      docs_and_positions_enum2.get_payload()?.unwrap().as_bytes()
    );
    assert_eq!(1, docs_and_positions_enum2.next_position()?);
    assert_eq!(4, docs_and_positions_enum2.start_offset()?);
    assert_eq!(7, docs_and_positions_enum2.end_offset()?);
    assert_eq!(
      BytesRef::<Vec<u8>>::from_string("pay2").as_bytes(),
      docs_and_positions_enum2.get_payload()?.unwrap().as_bytes()
    );
    assert_eq!(NO_MORE_DOCS, docs_and_positions_enum2.next_doc()?);

    drop(terms_enum);
    drop(leaf);
    w.close()?;
    reader.close()?;
    dir.close()?;
    Ok(())
  }

  /// Test realistic data, which is often better at uncovering real bugs.
  #[cfg_attr(not(feature = "nightly"), allow(dead_code))]
  fn test_line_file_docs<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    // Use a FS dir and a non-randomized IWC to not slow down indexing
    let path = create_temp_dir()?;
    let dir = new_fs_directory(random, path)?;

    {
      let mut docs = LineFileDocs::new(random)?;
      let w = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;

      let num_docs = at_least(random, 10_000);

      for _ in 0..num_docs {
        // Only keep the body field, and don't index term vectors on it, we only care about
        // postings
        let doc = docs.next_doc()?;
        let body = doc.get_field("body").unwrap();
        let body_value = body.string_value()?.unwrap();

        assert_ne!(IndexOptions::None, *body.field_type().index_options());

        let body = TextField::from_string("body", body_value.into_owned(), Store::No)?;

        let mut new_doc = Document::new();
        new_doc.add(body);
        w.add_document(new_doc)?;
      }

      w.force_merge(1)?;
      w.close()?;
    }

    TestUtil::check_index(random, dir)?;

    Ok(())
  }

  fn test_mismatched_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir1 = new_directory_shared(random)?;
    let w1 = IndexWriter::new(dir1.clone(), new_index_writer_config(random)?)?;
    let mut doc = Document::new();
    doc.add(StringField::from_string("f", "a", No)?);
    doc.add(StringField::from_string("g", "b", No)?);
    w1.add_document(&mut doc)?;

    let dir2 = new_directory_shared(random)?;
    let mut iwc = new_index_writer_config(random)?;
    iwc.set_merge_scheduler(SerialMergeScheduler::new());
    let w2 = IndexWriter::new(dir2.clone(), iwc)?;
    w2.add_document(&mut doc)?;
    w2.commit()?;

    let reader = directory_reader::open_from_writer(&w1)?;
    w1.close()?;
    let leaf = get_only_leaf_reader(&reader)?;
    let mismatched = MismatchedCodecReader::new(leaf, random)?;
    w2.add_indexes_from_codec_readers(vec![mismatched])?;
    reader.close()?;
    w2.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&w2)?;
    w2.close()?;
    let leaf = get_only_leaf_reader(&reader)?;
    for (field, term) in [("f", "a"), ("g", "b")] {
      let terms = leaf.terms(field)?.expect("terms should exist");
      let mut terms_enum = terms.iterator()?;
      let actual = terms_enum.next()?.expect("term should exist");
      assert_eq!(term.as_bytes(), actual.as_bytes());
      assert_eq!(2, terms_enum.doc_freq()?);
      assert!(terms_enum.next()?.is_none());
    }

    reader.close()?;
    dir1.close()?;
    dir2.close()?;
    Ok(())
  }
}

impl<T> BaseIndexFileFormatTestCaseDefaults<T> for BasePostingsFormatTestCaseDefaults
where
  T: BasePostingsFormatTestCase,
{
  fn add_random_fields<R>(_test_case: &T, random: &mut R, document: &mut Document) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    for options in IndexOptions::values() {
      if options == IndexOptions::None {
        continue;
      }
      let mut field_type = FieldType::new();
      field_type.set_index_options(options)?;
      field_type.freeze();
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
      Box::new(MockTokenizer::new(self.next_random())) as Box<dyn TokenStream + Send + Sync>,
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
