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
use crate::core::codecs::fields_producer::FieldsProducer;
use crate::core::codecs::lucene101::lucene101_postings_format::Lucene101PostingsFormat;
use crate::core::codecs::postings_format::PostingsFormat;
use crate::core::index::BytesRef;
use crate::core::index::bytes_ref::{BytesRefValue, BytesRefValueEnum};
use crate::core::index::fields::{FieldIterEnum2, Fields};
use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_reader::Identity;
use crate::core::index::ord_term_state::OrdTermState;
use crate::core::index::postings_enum::{ALL, POSITIONS, PostingsEnum, feature_requested};
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::slow_impacts_enum::SlowImpactsEnum;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::{
  SeekStatus, TermsEnum, TermsEnumWithUnsupportedSecondAttributes2,
};
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, NO_MORE_DOCS};
use crate::core::store::directory::Directory;
use crate::core::store::io_context::Context;
use crate::core::store::{IndexInput, IndexOutput};
use crate::core::util::HasIdentity;
use crate::core::util::access::ByteSource;
use crate::core::util::accountable::Accountable;
use crate::core::util::automation::compiled_automaton::{
  AutomatonEnum, AutomatonType, CompiledAutomaton,
};
use crate::core::util::automation::transition::Transition;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::close::CloseableRef;
use crate::core::util::dummy::dummy_attribute_source::DummyAttributeSource;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::iterator::IteratorExt;
use crate::core::util::ram_usage_estimator::size_of_vec;
use std::collections::BTreeMap;
use std::collections::btree_map::Keys;
use std::fmt::{Display, Formatter};
use std::mem::size_of_val;
use std::sync::{Arc, OnceLock};

/// Wraps [`Lucene101PostingsFormat`] for on-disk storage, but then at read time loads and stores
/// all terms and postings directly in RAM as byte and integer arrays.
///
/// **WARNING**: This is exceptionally RAM intensive: it makes no effort to compress the postings
/// data, storing terms as separate byte arrays and postings as separate integer arrays, but as a
/// result it gives a substantial increase in search performance.
///
/// This postings format supports [`TermsEnum::ord`] and [`TermsEnum::seek_exact_with_ord`].
///
/// Because this holds all term bytes as a single byte array, a single segment cannot have more
/// than 2.1GB worth of term bytes.
///
/// # Experimental
pub struct DirectPostingsFormat {
  min_skip_count: i32,
  low_freq_cutoff: i32,
  identity: Identity,
}

pub enum DirectFieldsProducer<P> {
  Lucene101(P),
  Direct(DirectFields),
}

impl<P> CloseableRef for DirectFieldsProducer<P>
where
  P: FieldsProducer,
{
  fn close(&self) -> Result<()> {
    match self {
      Self::Lucene101(producer) => producer.close(),
      Self::Direct(producer) => producer.close(),
    }
  }
}

impl<P> Fields for DirectFieldsProducer<P>
where
  P: FieldsProducer,
{
  type FieldIter<'a>
    = FieldIterEnum2<P::FieldIter<'a>, <DirectFields as Fields>::FieldIter<'a>>
  where
    Self: 'a;

  fn iterator(&self) -> Result<Self::FieldIter<'_>> {
    match self {
      Self::Lucene101(producer) => producer.iterator().map(FieldIterEnum2::A),
      Self::Direct(producer) => producer.iterator().map(FieldIterEnum2::B),
    }
  }

  type Terms = DirectFieldsTerms<P::Terms>;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    match self {
      Self::Lucene101(producer) => producer
        .terms(field)
        .map(|terms| terms.map(DirectFieldsTerms::Lucene101)),
      Self::Direct(producer) => producer
        .terms(field)
        .map(|terms| terms.map(DirectFieldsTerms::Direct)),
    }
  }

  fn size(&self) -> Result<i32> {
    match self {
      Self::Lucene101(producer) => producer.size(),
      Self::Direct(producer) => producer.size(),
    }
  }
}

impl<P> FieldsProducer for DirectFieldsProducer<P>
where
  P: FieldsProducer,
{
  fn check_integrity(&self) -> Result<()> {
    match self {
      Self::Lucene101(producer) => producer.check_integrity(),
      Self::Direct(producer) => producer.check_integrity(),
    }
  }

  fn get_merge_instance(&self) -> Result<Option<Self>> {
    match self {
      Self::Lucene101(producer) => Ok(producer.get_merge_instance()?.map(Self::Lucene101)),
      Self::Direct(producer) => Ok(producer.get_merge_instance()?.map(Self::Direct)),
    }
  }
}

pub enum DirectFieldsTerms<T> {
  Lucene101(T),
  Direct(DirectField),
}

impl<T> Terms for DirectFieldsTerms<T>
where
  T: Terms,
{
  type TermsEnum = TermsEnumWithUnsupportedSecondAttributes2<T::TermsEnum, DirectTermsEnum>;

  fn iterator(&self) -> Result<Self::TermsEnum> {
    match self {
      Self::Lucene101(terms) => terms
        .iterator()
        .map(TermsEnumWithUnsupportedSecondAttributes2::WithAttributes),
      Self::Direct(terms) => terms
        .iterator()
        .map(TermsEnumWithUnsupportedSecondAttributes2::WithoutAttributes),
    }
  }

  type IntersectIter =
    TermsEnumWithUnsupportedSecondAttributes2<T::IntersectIter, DirectIntersectTermsEnum>;

  fn intersect(
    &self,
    compiled: &CompiledAutomaton,
    start_term: Option<&BytesRef<Vec<u8>>>,
  ) -> Result<Self::IntersectIter> {
    match self {
      Self::Lucene101(terms) => terms
        .intersect(compiled, start_term)
        .map(TermsEnumWithUnsupportedSecondAttributes2::WithAttributes),
      Self::Direct(terms) => terms
        .intersect(compiled, start_term)
        .map(TermsEnumWithUnsupportedSecondAttributes2::WithoutAttributes),
    }
  }

  fn size(&self) -> Result<i64> {
    match self {
      Self::Lucene101(terms) => terms.size(),
      Self::Direct(terms) => terms.size(),
    }
  }

  fn get_sum_total_term_freq(&self) -> Result<i64> {
    match self {
      Self::Lucene101(terms) => terms.get_sum_total_term_freq(),
      Self::Direct(terms) => terms.get_sum_total_term_freq(),
    }
  }

  fn get_sum_doc_freq(&self) -> Result<i64> {
    match self {
      Self::Lucene101(terms) => terms.get_sum_doc_freq(),
      Self::Direct(terms) => terms.get_sum_doc_freq(),
    }
  }

  fn get_doc_count(&self) -> Result<i32> {
    match self {
      Self::Lucene101(terms) => terms.get_doc_count(),
      Self::Direct(terms) => terms.get_doc_count(),
    }
  }

  fn has_freqs(&self) -> bool {
    match self {
      Self::Lucene101(terms) => terms.has_freqs(),
      Self::Direct(terms) => terms.has_freqs(),
    }
  }

  fn has_offsets(&self) -> bool {
    match self {
      Self::Lucene101(terms) => terms.has_offsets(),
      Self::Direct(terms) => terms.has_offsets(),
    }
  }

  fn has_positions(&self) -> bool {
    match self {
      Self::Lucene101(terms) => terms.has_positions(),
      Self::Direct(terms) => terms.has_positions(),
    }
  }

  fn has_payloads(&self) -> bool {
    match self {
      Self::Lucene101(terms) => terms.has_payloads(),
      Self::Direct(terms) => terms.has_payloads(),
    }
  }
}

const DEFAULT_MIN_SKIP_COUNT: i32 = 8;
const DEFAULT_LOW_FREQ_CUTOFF: i32 = 32;

impl Default for DirectPostingsFormat {
  fn default() -> Self {
    Self::new()
  }
}

impl DirectPostingsFormat {
  pub fn new() -> Self {
    Self::with_params(DEFAULT_MIN_SKIP_COUNT, DEFAULT_LOW_FREQ_CUTOFF)
  }

  /// `min_skip_count` is how many terms in a row must have the same prefix before a skip pointer
  /// is added. Terms with `doc_freq <= low_freq_cutoff` use one integer array to hold all docs,
  /// freqs, positions and offsets; higher-frequency terms use separate arrays.
  pub fn with_params(min_skip_count: i32, low_freq_cutoff: i32) -> Self {
    Self {
      min_skip_count,
      low_freq_cutoff,
      identity: Identity::new(),
    }
  }
}

impl HasIdentity for DirectPostingsFormat {
  fn identity(&self) -> &Identity {
    &self.identity
  }
}

impl PostingsFormat for DirectPostingsFormat {
  fn get_name(&self) -> &str {
    "Direct"
  }

  type FieldsConsumer<O: IndexOutput> =
    <Lucene101PostingsFormat as PostingsFormat>::FieldsConsumer<O>;

  fn fields_consumer<D1, D2>(
    &self,
    state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::FieldsConsumer<D1::IndexOutput>>
  where
    D1: Directory,
  {
    Lucene101PostingsFormat::new()?.fields_consumer(state, segment_info)
  }

  type FieldsProducer<I: IndexInput> =
    DirectFieldsProducer<<Lucene101PostingsFormat as PostingsFormat>::FieldsProducer<I>>;

  fn fields_producer<D1, D2>(
    &self,
    state: &SegmentReadState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::FieldsProducer<D1::IndexInput>>
  where
    D1: Directory,
  {
    let postings = Lucene101PostingsFormat::new()?.fields_producer(state, segment_info)?;
    if state.context.get_context() != &Context::Merge {
      let load_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        postings.check_integrity()?;
        DirectFields::new(state, &postings, self.min_skip_count, self.low_freq_cutoff)
      }));
      let close_result = postings.close();
      close_result?;
      unwrap_caught_result!(load_result).map(DirectFieldsProducer::Direct)
    } else {
      // Don't load postings for merge:
      Ok(DirectFieldsProducer::Lucene101(postings))
    }
  }

  fn for_name(name: &str) -> Result<Arc<Self>> {
    static FORMAT: OnceLock<Arc<DirectPostingsFormat>> = OnceLock::new();
    match name {
      "Direct" => Ok(Arc::clone(
        FORMAT.get_or_init(|| Arc::new(DirectPostingsFormat::new())),
      )),
      _ => Err(LuceneError::illegal_argument(format!(
        "Could not load postings format named \"{name}\""
      ))),
    }
  }
}

pub struct DirectFields {
  fields: BTreeMap<String, DirectField>,
}

impl DirectFields {
  fn new<D, F>(
    state: &SegmentReadState<D>,
    fields: &F,
    min_skip_count: i32,
    low_freq_cutoff: i32,
  ) -> Result<Self>
  where
    D: Directory,
    F: Fields,
  {
    let mut direct_fields = BTreeMap::new();
    let mut iterator = fields.iterator()?;
    while let Some(field) = iterator.next()? {
      let terms = fields
        .terms(field)?
        .ok_or_else(|| LuceneError::illegal_state(format!("missing terms for field {field}")))?;
      direct_fields.insert(
        field.clone(),
        DirectField::new(state, field, &terms, min_skip_count, low_freq_cutoff)?,
      );
    }
    Ok(Self {
      fields: direct_fields,
    })
  }
}

impl<'a> IteratorExt for Keys<'a, String, DirectField> {
  type Item = &'a String;

  fn next(&mut self) -> Result<Option<Self::Item>> {
    Ok(Iterator::next(self))
  }

  fn has_next(&self) -> Result<bool> {
    Ok(self.len() != 0)
  }
}

impl Fields for DirectFields {
  type FieldIter<'a>
    = Keys<'a, String, DirectField>
  where
    Self: 'a;

  type Terms = DirectField;

  fn iterator(&self) -> Result<Self::FieldIter<'_>> {
    Ok(self.fields.keys())
  }

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    Ok(self.fields.get(field).cloned())
  }

  fn size(&self) -> Result<i32> {
    Ok(self.fields.len() as i32)
  }
}

impl CloseableRef for DirectFields {}

impl FieldsProducer for DirectFields {
  fn check_integrity(&self) -> Result<()> {
    // If we read entirely into RAM, we already validated.
    Ok(())
  }
}

impl Display for DirectFields {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "DirectFields(fields={})", self.fields.len())
  }
}

#[derive(Clone)]
pub struct DirectField {
  data: Arc<DirectFieldData>,
}

struct DirectFieldData {
  term_bytes: Vec<u8>,
  term_offsets: Vec<i32>,
  skips: Vec<i32>,
  skip_offsets: Vec<i32>,
  terms: Vec<DirectTerm>,
  low_terms: Vec<LowFreqTerm>,
  high_terms: Vec<HighFreqTerm>,
  has_freq: bool,
  has_pos: bool,
  has_offsets: bool,
  has_payloads: bool,
  sum_total_term_freq: i64,
  doc_count: i32,
  sum_doc_freq: i64,
  min_skip_count: i32,
}

enum PendingTerm {
  LowFreq(LowFreqTerm),
  HighFreq(HighFreqTerm),
}

enum DirectTerm {
  LowFreq(usize),
  HighFreq(usize),
}

struct LowFreqTerm {
  skips: Vec<i32>,
  postings: Vec<i32>,
  payloads: Option<Vec<u8>>,
  doc_freq: i32,
  total_term_freq: i32,
}

struct HighFreqTerm {
  skips: Vec<i32>,
  total_term_freq: i64,
  doc_ids: Vec<i32>,
  freqs: Option<Vec<i32>>,
  positions: Option<Vec<Vec<i32>>>,
  payloads: Option<Vec<Vec<Option<Vec<u8>>>>>,
}

impl Accountable for LowFreqTerm {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(
      size_of_vec(&self.skips)
        + size_of_vec(&self.postings)
        + self.payloads.as_ref().map_or(0, size_of_vec),
    )
  }
}

impl Accountable for HighFreqTerm {
  fn ram_bytes_used(&self) -> Result<i64> {
    let mut size = size_of_vec(&self.skips) + size_of_vec(&self.doc_ids);
    if let Some(freqs) = &self.freqs {
      size += size_of_vec(freqs);
    }
    if let Some(positions) = &self.positions {
      size += size_of_vec(positions);
      for position in positions {
        size += size_of_vec(position);
      }
    }
    if let Some(payloads) = &self.payloads {
      size += size_of_vec(payloads);
      for payload in payloads {
        size += size_of_vec(payload);
        for bytes in payload.iter().flatten() {
          size += size_of_vec(bytes);
        }
      }
    }
    Ok(size)
  }
}

// Each cursor retains the existing immutable field directory, rather than
// allocating a separate shared owner for every term.
struct LowFreqTermOwner {
  data: Arc<DirectFieldData>,
  ord: usize,
}

impl LowFreqTermOwner {
  fn new(data: &Arc<DirectFieldData>, ord: usize) -> Self {
    Self {
      data: Arc::clone(data),
      ord,
    }
  }
}

impl std::ops::Deref for LowFreqTermOwner {
  type Target = LowFreqTerm;
  fn deref(&self) -> &Self::Target {
    &self.data.low_terms[self.ord]
  }
}

struct HighFreqTermOwner {
  data: Arc<DirectFieldData>,
  ord: usize,
}

impl HighFreqTermOwner {
  fn new(data: &Arc<DirectFieldData>, ord: usize) -> Self {
    Self {
      data: Arc::clone(data),
      ord,
    }
  }
}

impl std::ops::Deref for HighFreqTermOwner {
  type Target = HighFreqTerm;
  fn deref(&self) -> &Self::Target {
    &self.data.high_terms[self.ord]
  }
}

struct IntArrayWriter {
  ints: Vec<i32>,
}

impl IntArrayWriter {
  fn new() -> Self {
    Self {
      ints: Vec::with_capacity(10),
    }
  }

  fn add(&mut self, value: i32) {
    self.ints.push(value);
  }

  fn get(&mut self) -> Vec<i32> {
    let result = self.ints.clone();
    self.ints.clear();
    result
  }
}

impl DirectField {
  fn new<D, T>(
    state: &SegmentReadState<D>,
    field: &str,
    terms_in: &T,
    min_skip_count: i32,
    low_freq_cutoff: i32,
  ) -> Result<Self>
  where
    D: Directory,
    T: Terms,
  {
    let field_info = state
      .field_infos
      .field_info_by_name(field)?
      .ok_or_else(|| LuceneError::illegal_state(format!("missing FieldInfo for {field}")))?;

    let sum_total_term_freq = terms_in.get_sum_total_term_freq()?;
    let sum_doc_freq = terms_in.get_sum_doc_freq()?;
    let doc_count = terms_in.get_doc_count()?;

    let num_terms = terms_in.size()?;
    if num_terms == -1 {
      return Err(LuceneError::illegal_argument(
        "codec does not provide Terms.size()",
      ));
    }
    let num_terms = usize::try_from(num_terms)
      .map_err(|_| LuceneError::illegal_argument("invalid negative Terms.size()"))?;
    let mut terms = Vec::with_capacity(num_terms);
    let mut term_offsets = vec![0; 1 + num_terms];
    let mut term_bytes = Vec::with_capacity(1024);
    let mut same_counts = vec![0; 10];
    let mut skip_count = 0usize;

    let index_options = *field_info.get_index_options();
    let has_freq = index_options > IndexOptions::Docs;
    let has_pos = index_options > IndexOptions::DocsAndFreqs;
    let has_offsets = index_options > IndexOptions::DocsAndFreqsAndPositions;
    let has_payloads = field_info.has_payloads();

    let mut postings_enum = None;
    let mut docs_and_positions_enum = None;
    let mut terms_enum = terms_in.iterator()?;
    let mut term_offset = 0usize;
    let mut scratch = IntArrayWriter::new();
    let mut payload_output = Vec::new();
    let mut count = 0usize;

    loop {
      let term = match terms_enum.next()? {
        Some(term) => term.into_owned(),
        None => break,
      };
      let doc_freq = terms_enum.doc_freq()?;
      let total_term_freq = terms_enum.total_term_freq()?;

      term_offsets[count] = term_offset as i32;
      let term_slice = &term.bytes[term.offset..term.offset + term.length];
      term_bytes.extend_from_slice(term_slice);
      term_offset += term.length;
      term_offsets[count + 1] = term_offset as i32;

      if has_pos {
        docs_and_positions_enum =
          Some(terms_enum.postings_with_flags(docs_and_positions_enum.take(), ALL as i32)?);
      } else {
        postings_enum = Some(terms_enum.postings(postings_enum.take())?);
      }
      let postings_enum2 = if has_pos {
        docs_and_positions_enum
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("positions postings enum is missing"))?
      } else {
        postings_enum
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("postings enum is missing"))?
      };

      let entry = if doc_freq <= low_freq_cutoff {
        payload_output.clear();
        while postings_enum2.next_doc()? != NO_MORE_DOCS {
          scratch.add(postings_enum2.doc_id());
          if has_freq {
            let freq = postings_enum2.freq()?;
            scratch.add(freq);
            if has_pos {
              for _ in 0..freq {
                scratch.add(postings_enum2.next_position()?);
                if has_offsets {
                  scratch.add(postings_enum2.start_offset()?);
                  scratch.add(postings_enum2.end_offset()?);
                }
                if has_payloads {
                  if let Some(payload) = postings_enum2.get_payload()? {
                    let payload = payload.as_bytes_ref();
                    scratch.add(payload.length as i32);
                    payload_output.extend_from_slice(
                      &payload.bytes[payload.offset..payload.offset + payload.length],
                    );
                  } else {
                    scratch.add(0);
                  }
                }
              }
            }
          }
        }
        let payloads = has_payloads.then(|| payload_output.clone());
        PendingTerm::LowFreq(LowFreqTerm {
          skips: Vec::new(),
          postings: scratch.get(),
          payloads,
          doc_freq,
          total_term_freq: total_term_freq as i32,
        })
      } else {
        let doc_count = doc_freq as usize;
        let mut docs = vec![0; doc_count];
        let mut freqs = has_freq.then(|| vec![0; doc_count]);
        let mut positions = has_pos.then(|| vec![Vec::new(); doc_count]);
        let mut payloads = has_payloads.then(|| vec![Vec::new(); doc_count]);

        let mut upto = 0usize;
        while postings_enum2.next_doc()? != NO_MORE_DOCS {
          docs[upto] = postings_enum2.doc_id();
          if has_freq {
            let freq = postings_enum2.freq()?;
            freqs
              .as_mut()
              .ok_or_else(|| LuceneError::illegal_state("frequencies are missing"))?[upto] = freq;
            if has_pos {
              let mult = if has_offsets { 3 } else { 1 };
              let doc_positions = &mut positions
                .as_mut()
                .ok_or_else(|| LuceneError::illegal_state("positions are missing"))?[upto];
              let position_count = freq as usize;
              doc_positions.resize(mult * position_count, 0);
              if has_payloads {
                payloads
                  .as_mut()
                  .ok_or_else(|| LuceneError::illegal_state("payloads are missing"))?[upto] =
                  vec![None; position_count];
              }
              let mut pos_upto = 0usize;
              for pos in 0..position_count {
                doc_positions[pos_upto] = postings_enum2.next_position()?;
                if has_payloads && let Some(payload) = postings_enum2.get_payload()? {
                  let payload = payload.as_bytes_ref();
                  payloads
                    .as_mut()
                    .ok_or_else(|| LuceneError::illegal_state("payloads are missing"))?[upto]
                    [pos] =
                    Some(payload.bytes[payload.offset..payload.offset + payload.length].to_vec());
                }
                pos_upto += 1;
                if has_offsets {
                  doc_positions[pos_upto] = postings_enum2.start_offset()?;
                  pos_upto += 1;
                  doc_positions[pos_upto] = postings_enum2.end_offset()?;
                  pos_upto += 1;
                }
              }
            }
          }
          upto += 1;
        }
        debug_assert_eq!(upto, doc_count);
        PendingTerm::HighFreq(HighFreqTerm {
          skips: Vec::new(),
          total_term_freq,
          doc_ids: docs,
          freqs,
          positions,
          payloads,
        })
      };

      terms.push(entry);
      Self::set_skips(
        count,
        &term_bytes,
        &term_offsets,
        &mut same_counts,
        min_skip_count,
        &mut terms,
        &mut skip_count,
      );
      count += 1;
    }

    term_offsets[count] = term_offset as i32;
    Self::finish_skips(
      count,
      &term_offsets,
      &same_counts,
      min_skip_count,
      &mut terms,
      &mut skip_count,
    );

    let mut skips = Vec::with_capacity(skip_count);
    let mut skip_offsets = vec![0; 1 + num_terms];
    let mut skip_offset = 0usize;
    for (i, term) in terms.iter_mut().enumerate() {
      skip_offsets[i] = skip_offset as i32;
      let term_skips = match term {
        PendingTerm::LowFreq(term) => std::mem::take(&mut term.skips),
        PendingTerm::HighFreq(term) => std::mem::take(&mut term.skips),
      };
      skips.extend_from_slice(&term_skips);
      skip_offset += term_skips.len();
    }
    skip_offsets[num_terms] = skip_offset as i32;
    debug_assert_eq!(skip_offset, skip_count);
    term_bytes.shrink_to_fit();

    // Typed term slots let independent cursors project their data without a
    // self-reference or a runtime variant assertion. Only Vec control values move.
    let low_count = terms
      .iter()
      .filter(|term| matches!(term, PendingTerm::LowFreq(_)))
      .count();
    let mut low_terms = Vec::with_capacity(low_count);
    let mut high_terms = Vec::with_capacity(terms.len() - low_count);
    let terms = terms
      .into_iter()
      .map(|term| match term {
        PendingTerm::LowFreq(term) => {
          let index = low_terms.len();
          low_terms.push(term);
          DirectTerm::LowFreq(index)
        },
        PendingTerm::HighFreq(term) => {
          let index = high_terms.len();
          high_terms.push(term);
          DirectTerm::HighFreq(index)
        },
      })
      .collect();

    Ok(Self {
      data: Arc::new(DirectFieldData {
        term_bytes,
        term_offsets,
        skips,
        skip_offsets,
        terms,
        low_terms,
        high_terms,
        has_freq,
        has_pos,
        has_offsets,
        has_payloads,
        sum_total_term_freq,
        doc_count,
        sum_doc_freq,
        min_skip_count,
      }),
    })
  }

  // Compares in Unicode (UTF-8) order:
  fn compare<BS: ByteSource>(data: &DirectFieldData, ord: usize, other: &BytesRef<BS>) -> i32 {
    let other_bytes = other.as_byte_slice();
    let mut upto = data.term_offsets[ord] as usize;
    let term_len = data.term_offsets[ord + 1] as usize - upto;
    let mut other_upto = 0;
    let stop = upto + term_len.min(other.length);
    while upto < stop {
      let diff = data.term_bytes[upto] as i32 - other_bytes[other_upto] as i32;
      upto += 1;
      other_upto += 1;
      if diff != 0 {
        return diff;
      }
    }
    term_len as i32 - other.length as i32
  }

  fn set_skips(
    term_ord: usize,
    term_bytes: &[u8],
    term_offsets: &[i32],
    same_counts: &mut Vec<i32>,
    min_skip_count: i32,
    terms: &mut [PendingTerm],
    skip_count: &mut usize,
  ) {
    let term_length = (term_offsets[term_ord + 1] - term_offsets[term_ord]) as usize;
    if same_counts.len() < term_length {
      same_counts.resize(term_length, 0);
    }
    if term_ord > 0 {
      let last_term_length = (term_offsets[term_ord] - term_offsets[term_ord - 1]) as usize;
      let limit = term_length.min(last_term_length);
      let mut last_term_offset = term_offsets[term_ord - 1] as usize;
      let mut term_offset = term_offsets[term_ord] as usize;
      let mut i = 0usize;
      while i < limit {
        if term_bytes[last_term_offset] == term_bytes[term_offset] {
          same_counts[i] += 1;
          last_term_offset += 1;
          term_offset += 1;
          i += 1;
        } else {
          while i < limit {
            if same_counts[i] >= min_skip_count {
              Self::save_skip(term_ord, same_counts[i], terms, skip_count);
            }
            same_counts[i] = 1;
            i += 1;
          }
          break;
        }
      }
      while i < last_term_length {
        if same_counts[i] >= min_skip_count {
          Self::save_skip(term_ord, same_counts[i], terms, skip_count);
        }
        same_counts[i] = 0;
        i += 1;
      }
      for value in same_counts.iter_mut().take(term_length).skip(limit) {
        *value = 1;
      }
    } else {
      for value in same_counts.iter_mut().take(term_length) {
        *value += 1;
      }
    }
  }

  fn finish_skips(
    count: usize,
    term_offsets: &[i32],
    same_counts: &[i32],
    min_skip_count: i32,
    terms: &mut [PendingTerm],
    skip_count: &mut usize,
  ) {
    debug_assert_eq!(count, terms.len());
    if count == 0 {
      return;
    }
    let last_term_offset = term_offsets[count - 1];
    let last_term_length = (term_offsets[count] - last_term_offset) as usize;
    for (i, same_count) in same_counts.iter().enumerate().take(last_term_length) {
      if *same_count >= min_skip_count {
        let _ = i;
        Self::save_skip(count, *same_count, terms, skip_count);
      }
    }
    for term in terms {
      let skips = match term {
        PendingTerm::LowFreq(term) => &mut term.skips,
        PendingTerm::HighFreq(term) => &mut term.skips,
      };
      skips.reverse();
    }
  }

  fn save_skip(ord: usize, back_count: i32, terms: &mut [PendingTerm], skip_count: &mut usize) {
    let term = &mut terms[ord - back_count as usize];
    *skip_count += 1;
    match term {
      PendingTerm::LowFreq(term) => term.skips.push(ord as i32),
      PendingTerm::HighFreq(term) => term.skips.push(ord as i32),
    }
  }
}

impl Accountable for DirectField {
  fn ram_bytes_used(&self) -> Result<i64> {
    let data = self.data.as_ref();
    let mut size = size_of_val(data) as i64
      + size_of_vec(&data.term_bytes)
      + size_of_vec(&data.term_offsets)
      + size_of_vec(&data.skips)
      + size_of_vec(&data.skip_offsets)
      + size_of_vec(&data.terms)
      + size_of_vec(&data.low_terms)
      + size_of_vec(&data.high_terms);
    for term in &data.low_terms {
      size += term.ram_bytes_used()?;
    }
    for term in &data.high_terms {
      size += term.ram_bytes_used()?;
    }
    Ok(size)
  }
}

impl Display for DirectField {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "DirectTerms(terms={},postings={},positions={},docs={})",
      self.data.terms.len(),
      self.data.sum_doc_freq,
      self.data.sum_total_term_freq,
      self.data.doc_count
    )
  }
}

impl Terms for DirectField {
  type TermsEnum = DirectTermsEnum;

  fn iterator(&self) -> Result<Self::TermsEnum> {
    Ok(DirectTermsEnum::new(Arc::clone(&self.data)))
  }

  type IntersectIter = DirectIntersectTermsEnum;

  fn intersect(
    &self,
    compiled: &CompiledAutomaton,
    start_term: Option<&BytesRef<Vec<u8>>>,
  ) -> Result<Self::IntersectIter> {
    if compiled.type_ != AutomatonType::Normal {
      return Err(LuceneError::illegal_argument(
        "please use CompiledAutomaton.get_terms_enum instead",
      ));
    }
    DirectIntersectTermsEnum::new(Arc::clone(&self.data), compiled, start_term)
  }

  fn size(&self) -> Result<i64> {
    Ok(self.data.terms.len() as i64)
  }

  fn get_sum_total_term_freq(&self) -> Result<i64> {
    Ok(self.data.sum_total_term_freq)
  }

  fn get_sum_doc_freq(&self) -> Result<i64> {
    Ok(self.data.sum_doc_freq)
  }

  fn get_doc_count(&self) -> Result<i32> {
    Ok(self.data.doc_count)
  }

  fn has_freqs(&self) -> bool {
    self.data.has_freq
  }

  fn has_offsets(&self) -> bool {
    self.data.has_offsets
  }

  fn has_positions(&self) -> bool {
    self.data.has_pos
  }

  fn has_payloads(&self) -> bool {
    self.data.has_payloads
  }
}

pub struct DirectTermsEnum {
  data: Arc<DirectFieldData>,
  term_ord: Option<usize>,
  term_ref_ord: Option<usize>,
}

impl DirectTermsEnum {
  fn new(data: Arc<DirectFieldData>) -> Self {
    Self {
      data,
      term_ord: None,
      term_ref_ord: None,
    }
  }

  fn set_term_ref(&mut self) -> Result<()> {
    let term_ord = self
      .term_ord
      .ok_or_else(|| LuceneError::illegal_state("terms enum is not positioned"))?;
    if term_ord >= self.data.terms.len() {
      return Err(LuceneError::illegal_state("terms enum is not positioned"));
    }
    self.term_ref_ord = Some(term_ord);
    Ok(())
  }

  fn term_ref(&self) -> BytesRef<&[u8]> {
    let Some(term_ord) = self.term_ref_ord else {
      return BytesRef {
        bytes: &[],
        offset: 0,
        length: 0,
      };
    };
    let start = self.data.term_offsets[term_ord] as usize;
    let end = self.data.term_offsets[term_ord + 1] as usize;
    BytesRef {
      bytes: self.data.term_bytes.as_slice(),
      offset: start,
      length: end - start,
    }
  }

  // If non-negative, exact match; else, -ord-1, where ord is where the term would be inserted.
  fn find_term<BS: ByteSource>(&self, term: &BytesRef<BS>) -> i32 {
    let mut low = 0i32;
    let mut high = self.data.terms.len() as i32 - 1;
    while low <= high {
      let mid = ((low + high) as u32 >> 1) as i32;
      let cmp = DirectField::compare(&self.data, mid as usize, term);
      if cmp < 0 {
        low = mid + 1;
      } else if cmp > 0 {
        high = mid - 1;
      } else {
        return mid;
      }
    }
    -(low + 1)
  }

  fn current_term(&self) -> Result<&DirectTerm> {
    self
      .term_ord
      .and_then(|term_ord| self.data.terms.get(term_ord))
      .ok_or_else(|| LuceneError::illegal_state("terms enum is not positioned"))
  }
}

impl BytesRefIterator for DirectTermsEnum {
  type Value<'a>
    = BytesRef<&'a [u8]>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    let term_ord = self.term_ord.map_or(0, |term_ord| term_ord + 1);
    self.term_ord = Some(term_ord);
    if term_ord < self.data.terms.len() {
      self.set_term_ref()?;
      Ok(Some(self.term_ref()))
    } else {
      Ok(None)
    }
  }
}

impl TermsEnum for DirectTermsEnum {
  type AttributeSource<'a>
    = &'a DummyAttributeSource
  where
    Self: 'a;
  type AttributeSourceMut<'a>
    = &'a mut DummyAttributeSource
  where
    Self: 'a;

  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn seek_exact<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<bool> {
    let ord = self.find_term(term);
    if ord >= 0 {
      self.term_ord = Some(ord as usize);
      self.set_term_ref()?;
      Ok(true)
    } else {
      Ok(false)
    }
  }

  type PreparedSeek<BS: ByteSource> = crate::core::index::terms_enum::SeekExact;

  fn prepare_seek_exact<BS: ByteSource>(
    &mut self,
    _text: &BytesRef<BS>,
  ) -> Result<Option<Self::PreparedSeek<BS>>> {
    Ok(Some(crate::core::index::terms_enum::SeekExact))
  }

  fn seek_ceil<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<SeekStatus> {
    let ord = self.find_term(term);
    if ord >= 0 {
      self.term_ord = Some(ord as usize);
      self.set_term_ref()?;
      Ok(SeekStatus::Found)
    } else if ord == -(self.data.terms.len() as i32) - 1 {
      Ok(SeekStatus::End)
    } else {
      self.term_ord = Some((-ord - 1) as usize);
      self.set_term_ref()?;
      Ok(SeekStatus::NotFound)
    }
  }

  fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
    let term_ord = ord as i32;
    self.term_ord = (term_ord != -1).then_some(term_ord as usize);
    self.set_term_ref()?;
    Ok(())
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    term: &BytesRef<BS>,
    state: &TermStateEnum,
  ) -> Result<()> {
    let term_ord = state.ord()? as i32;
    self.term_ord = (term_ord != -1).then_some(term_ord as usize);
    self.set_term_ref()?;
    debug_assert_eq!(self.term_ref().compare_to(term), std::cmp::Ordering::Equal);
    Ok(())
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    Ok(self.term_ref())
  }

  fn ord(&self) -> Result<i64> {
    Ok(self.term_ord.map_or(-1, |ord| ord as i64))
  }

  fn doc_freq(&mut self) -> Result<i32> {
    Ok(match self.current_term()? {
      DirectTerm::LowFreq(index) => self.data.low_terms[*index].doc_freq,
      DirectTerm::HighFreq(index) => self.data.high_terms[*index].doc_ids.len() as i32,
    })
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    Ok(match self.current_term()? {
      DirectTerm::LowFreq(index) => self.data.low_terms[*index].total_term_freq as i64,
      DirectTerm::HighFreq(index) => self.data.high_terms[*index].total_term_freq,
    })
  }

  type PostingsEnum = DirectPostingsEnum;

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    DirectPostingsEnum::for_term(&self.data, self.term_ord, reuse, flags)
  }

  type ImpactsEnum = SlowImpactsEnum<DirectPostingsEnum>;

  fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
    Ok(SlowImpactsEnum::new(self.postings_with_flags(None, flags)?))
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    Ok(
      OrdTermState {
        ord: self.term_ord.map_or(-1, |ord| ord as i64),
      }
      .into(),
    )
  }
}

pub struct DirectIntersectTermsEnum {
  data: Arc<DirectFieldData>,
  automaton: AutomatonEnum,
  common_suffix_ref: Option<Arc<BytesRef<Vec<u8>>>>,
  term_ord: Option<usize>,
  term_ref_ord: Option<usize>,
  states: Vec<DirectIntersectState>,
  state_upto: usize,
}

struct DirectIntersectState {
  change_ord: usize,
  state: i32,
  transition_upto: i32,
  transition_count: i32,
  transition_max: i32,
  transition_min: i32,
  transition: Transition,
}

impl DirectIntersectState {
  fn new() -> Self {
    Self {
      change_ord: 0,
      state: 0,
      transition_upto: -1,
      transition_count: 0,
      transition_max: -1,
      transition_min: 0,
      transition: Transition::default(),
    }
  }
}

impl DirectIntersectTermsEnum {
  fn new(
    data: Arc<DirectFieldData>,
    compiled: &CompiledAutomaton,
    start_term: Option<&BytesRef<Vec<u8>>>,
  ) -> Result<Self> {
    let mut automaton = compiled.get_automaton()?;
    let mut first_state = DirectIntersectState::new();
    first_state.change_ord = data.terms.len();
    first_state.state = 0;
    first_state.transition_count =
      automaton.init_transition(first_state.state, &mut first_state.transition)?;
    let mut result = Self {
      data,
      automaton,
      common_suffix_ref: compiled.common_suffix_ref.clone(),
      term_ord: None,
      term_ref_ord: None,
      states: vec![first_state],
      state_upto: 0,
    };
    if let Some(start_term) = start_term {
      result.seek_to_start_term(start_term)?;
    }
    Ok(result)
  }

  fn grow(&mut self) {
    if self.states.len() == self.state_upto + 1 {
      self.states.push(DirectIntersectState::new());
    }
  }

  fn seek_to_start_term(&mut self, start_term: &BytesRef<Vec<u8>>) -> Result<()> {
    let mut skip_upto = 0usize;
    if start_term.length == 0 {
      if !self.data.terms.is_empty() && self.data.term_offsets[1] == 0 {
        self.term_ord = Some(0);
      }
    } else {
      self.term_ord = Some(self.term_ord.map_or(0, |term_ord| term_ord + 1));

      'next_label: for i in 0..start_term.length {
        let label = start_term.bytes[start_term.offset + i] as i32;
        while label > self.states[i].transition_max {
          self.states[i].transition_upto += 1;
          if self.states[i].transition_upto >= self.states[i].transition_count {
            // All transitions compare less than the required label.
            break;
          }
          let (automaton, states) = (&mut self.automaton, &mut self.states);
          automaton.get_next_transition(&mut states[i].transition)?;
          states[i].transition_min = states[i].transition.min;
          states[i].transition_max = states[i].transition.max;
          debug_assert!((0..=255).contains(&states[i].transition_min));
          debug_assert!((0..=255).contains(&states[i].transition_max));
        }

        // Skip forwards until we find a term matching the label at this position.
        while let Some(term_ord) = self
          .term_ord
          .filter(|&term_ord| term_ord < self.data.terms.len())
        {
          let skip_offset = self.data.skip_offsets[term_ord] as usize;
          let num_skips =
            (self.data.skip_offsets[term_ord + 1] - self.data.skip_offsets[term_ord]) as usize;
          let term_offset = self.data.term_offsets[term_ord] as usize;
          let term_length =
            (self.data.term_offsets[term_ord + 1] - self.data.term_offsets[term_ord]) as usize;

          if self.term_ord == Some(self.states[self.state_upto].change_ord) {
            self.state_upto -= 1;
            self.term_ord = self.term_ord.and_then(|term_ord| term_ord.checked_sub(1));
            return Ok(());
          }

          if term_length == i {
            self.term_ord = Some(self.term_ord.map_or(0, |term_ord| term_ord + 1));
            skip_upto = 0;
          } else if label < self.data.term_bytes[term_offset + i] as i32 {
            self.term_ord = self.term_ord.and_then(|term_ord| term_ord.checked_sub(1));
            self.state_upto -= skip_upto;
            return Ok(());
          } else if label == self.data.term_bytes[term_offset + i] as i32 {
            if skip_upto < num_skips {
              self.grow();
              let next_state = self
                .automaton
                .step(self.states[self.state_upto].state, label)?;
              debug_assert_ne!(next_state, -1);

              self.state_upto += 1;
              let state = &mut self.states[self.state_upto];
              state.change_ord = self.data.skips[skip_offset + skip_upto] as usize;
              skip_upto += 1;
              state.state = next_state;
              state.transition_count = self
                .automaton
                .init_transition(next_state, &mut state.transition)?;
              state.transition_upto = -1;
              state.transition_max = -1;
              continue 'next_label;
            } else {
              // Index exhausted: just scan now (the number of scans required will be less than
              // min_skip_count).
              let start_term_ord = term_ord;
              while let Some(term_ord) = self
                .term_ord
                .filter(|&term_ord| term_ord < self.data.terms.len())
                && DirectField::compare(&self.data, term_ord, start_term) <= 0
              {
                debug_assert!(
                  self.term_ord == Some(start_term_ord)
                    || self.data.skip_offsets[term_ord] == self.data.skip_offsets[term_ord + 1]
                );
                self.term_ord = Some(self.term_ord.map_or(0, |term_ord| term_ord + 1));
              }
              debug_assert!(
                (self
                  .term_ord
                  .ok_or_else(|| LuceneError::illegal_state("terms enum is not positioned"))?
                  as i32
                  - start_term_ord as i32)
                  < self.data.min_skip_count
              );
              self.term_ord = self.term_ord.and_then(|term_ord| term_ord.checked_sub(1));
              self.state_upto -= skip_upto;
              return Ok(());
            }
          } else {
            if skip_upto < num_skips {
              self.term_ord = Some(self.data.skips[skip_offset + skip_upto] as usize);
            } else {
              self.term_ord = Some(self.term_ord.map_or(0, |term_ord| term_ord + 1));
            }
            skip_upto = 0;
          }
        }

        // startTerm is >= last term so this enum will not return any terms.
        self.term_ord = self.term_ord.and_then(|term_ord| term_ord.checked_sub(1));
        return Ok(());
      }
    }

    if let Some(term_ord) = self.term_ord {
      let term_offset = self.data.term_offsets[term_ord] as usize;
      let term_len =
        (self.data.term_offsets[term_ord + 1] - self.data.term_offsets[term_ord]) as usize;
      let same = start_term.length == term_len
        && start_term.bytes[start_term.offset..start_term.offset + start_term.length]
          == self.data.term_bytes[term_offset..term_offset + term_len];
      if !same {
        self.state_upto -= skip_upto;
        self.term_ord = self.term_ord.and_then(|term_ord| term_ord.checked_sub(1));
      }
    }
    Ok(())
  }

  fn set_term_ref(&mut self) -> Result<()> {
    let term_ord = self
      .term_ord
      .ok_or_else(|| LuceneError::illegal_state("terms enum is not positioned"))?;
    if term_ord >= self.data.terms.len() {
      return Err(LuceneError::illegal_state("terms enum is not positioned"));
    }
    self.term_ref_ord = Some(term_ord);
    Ok(())
  }

  fn term_ref(&self) -> BytesRef<&[u8]> {
    let Some(term_ord) = self.term_ref_ord else {
      return BytesRef {
        bytes: &[],
        offset: 0,
        length: 0,
      };
    };
    let start = self.data.term_offsets[term_ord] as usize;
    let end = self.data.term_offsets[term_ord + 1] as usize;
    BytesRef {
      bytes: self.data.term_bytes.as_slice(),
      offset: start,
      length: end - start,
    }
  }

  fn current_term(&self) -> Result<&DirectTerm> {
    self
      .term_ord
      .and_then(|term_ord| self.data.terms.get(term_ord))
      .ok_or_else(|| LuceneError::illegal_state("terms enum is not positioned"))
  }
}

impl BytesRefIterator for DirectIntersectTermsEnum {
  type Value<'a>
    = BytesRef<&'a [u8]>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    self.term_ord = Some(self.term_ord.map_or(0, |term_ord| term_ord + 1));
    let mut skip_upto = 0usize;

    if self.term_ord == Some(0) && self.data.term_offsets[1] == 0 {
      // Special-case empty string:
      debug_assert_eq!(self.state_upto, 0);
      if self.automaton.is_accept(self.states[0].state)? {
        self.term_ref_ord = Some(0);
        return Ok(Some(self.term_ref()));
      }
      self.term_ord = Some(self.term_ord.map_or(0, |term_ord| term_ord + 1));
    }

    'next_term: loop {
      if self.term_ord == Some(self.data.terms.len()) {
        return Ok(None);
      }

      if self.term_ord == Some(self.states[self.state_upto].change_ord) {
        // Pop:
        self.state_upto -= 1;
        continue;
      }

      let term_ord = self
        .term_ord
        .ok_or_else(|| LuceneError::illegal_state("terms enum is not positioned"))?;
      let term_offset = self.data.term_offsets[term_ord] as usize;
      let term_length =
        (self.data.term_offsets[term_ord + 1] - self.data.term_offsets[term_ord]) as usize;
      let skip_offset = self.data.skip_offsets[term_ord] as usize;
      let num_skips =
        (self.data.skip_offsets[term_ord + 1] - self.data.skip_offsets[term_ord]) as usize;

      debug_assert!(term_ord < self.states[self.state_upto].change_ord);
      debug_assert!(self.state_upto <= term_length);
      let label = self.data.term_bytes[term_offset + self.state_upto] as i32;

      while label > self.states[self.state_upto].transition_max {
        self.states[self.state_upto].transition_upto += 1;
        if self.states[self.state_upto].transition_upto
          == self.states[self.state_upto].transition_count
        {
          // We've exhausted transitions leaving this state; force pop+next/skip now.
          if self.state_upto == 0 {
            self.term_ord = Some(self.data.terms.len());
            return Ok(None);
          }
          debug_assert!(self.states[self.state_upto].change_ord > term_ord);
          self.term_ord = Some(self.states[self.state_upto].change_ord);
          skip_upto = 0;
          self.state_upto -= 1;
          continue 'next_term;
        }
        let state_upto = self.state_upto;
        let (automaton, states) = (&mut self.automaton, &mut self.states);
        automaton.get_next_transition(&mut states[state_upto].transition)?;
        let state = &mut states[state_upto];
        state.transition_min = state.transition.min;
        state.transition_max = state.transition.max;
        debug_assert!((0..=255).contains(&state.transition_min));
        debug_assert!((0..=255).contains(&state.transition_max));
      }

      let target_label = self.states[self.state_upto].transition_min;
      if label < target_label {
        let mut low = term_ord as i32 + 1;
        let mut high = self.states[self.state_upto].change_ord as i32 - 1;
        loop {
          if low > high {
            // Label not found.
            self.term_ord = Some(low as usize);
            skip_upto = 0;
            continue 'next_term;
          }
          let mut mid = ((low + high) as u32 >> 1) as i32;
          let cmp = self.data.term_bytes
            [self.data.term_offsets[mid as usize] as usize + self.state_upto]
            as i32
            - target_label;
          if cmp < 0 {
            low = mid + 1;
          } else if cmp > 0 {
            high = mid - 1;
          } else {
            // Label found; walk backwards to the first occurrence.
            while mid > term_ord as i32
              && self.data.term_bytes
                [self.data.term_offsets[mid as usize - 1] as usize + self.state_upto]
                as i32
                == target_label
            {
              mid -= 1;
            }
            self.term_ord = Some(mid as usize);
            skip_upto = 0;
            continue 'next_term;
          }
        }
      }

      let mut next_state = self
        .automaton
        .step(self.states[self.state_upto].state, label)?;
      if next_state == -1 {
        // Skip.
        if skip_upto < num_skips {
          self.term_ord = Some(self.data.skips[skip_offset + skip_upto] as usize);
        } else {
          self.term_ord = Some(self.term_ord.map_or(0, |term_ord| term_ord + 1));
        }
        skip_upto = 0;
      } else if skip_upto < num_skips {
        // Push:
        self.grow();
        self.state_upto += 1;
        let state = &mut self.states[self.state_upto];
        state.state = next_state;
        state.change_ord = self.data.skips[skip_offset + skip_upto] as usize;
        skip_upto += 1;
        state.transition_count = self
          .automaton
          .init_transition(next_state, &mut state.transition)?;
        state.transition_upto = -1;
        state.transition_max = -1;

        if self.state_upto == term_length {
          if self.automaton.is_accept(next_state)? {
            self.set_term_ref()?;
            return Ok(Some(self.term_ref()));
          }
          self.term_ord = Some(self.term_ord.map_or(0, |term_ord| term_ord + 1));
          skip_upto = 0;
        }
      } else {
        // Run the non-indexed tail of this term.
        if let Some(common_suffix_ref) = &self.common_suffix_ref {
          debug_assert_eq!(common_suffix_ref.offset, 0);
          if term_length < common_suffix_ref.length {
            self.term_ord = Some(self.term_ord.map_or(0, |term_ord| term_ord + 1));
            skip_upto = 0;
            continue 'next_term;
          }
          let offset = term_offset + term_length - common_suffix_ref.length;
          if self.data.term_bytes[offset..offset + common_suffix_ref.length]
            != common_suffix_ref.bytes[..common_suffix_ref.length]
          {
            self.term_ord = Some(self.term_ord.map_or(0, |term_ord| term_ord + 1));
            skip_upto = 0;
            continue 'next_term;
          }
        }

        let mut upto = self.state_upto + 1;
        while upto < term_length {
          next_state = self
            .automaton
            .step(next_state, self.data.term_bytes[term_offset + upto] as i32)?;
          if next_state == -1 {
            self.term_ord = Some(self.term_ord.map_or(0, |term_ord| term_ord + 1));
            skip_upto = 0;
            continue 'next_term;
          }
          upto += 1;
        }

        if self.automaton.is_accept(next_state)? {
          self.set_term_ref()?;
          return Ok(Some(self.term_ref()));
        }
        self.term_ord = Some(self.term_ord.map_or(0, |term_ord| term_ord + 1));
        skip_upto = 0;
      }
    }
  }
}

impl TermsEnum for DirectIntersectTermsEnum {
  type AttributeSource<'a>
    = &'a DummyAttributeSource
  where
    Self: 'a;
  type AttributeSourceMut<'a>
    = &'a mut DummyAttributeSource
  where
    Self: 'a;

  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn seek_exact<BS: ByteSource>(&mut self, _term: &BytesRef<BS>) -> Result<bool> {
    Err(LuceneError::unsupported_operation(""))
  }

  type PreparedSeek<BS: ByteSource> = crate::core::index::terms_enum::SeekExact;

  fn prepare_seek_exact<BS: ByteSource>(
    &mut self,
    _text: &BytesRef<BS>,
  ) -> Result<Option<Self::PreparedSeek<BS>>> {
    Ok(Some(crate::core::index::terms_enum::SeekExact))
  }

  fn seek_ceil<BS: ByteSource>(&mut self, _term: &BytesRef<BS>) -> Result<SeekStatus> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn seek_exact_with_ord(&mut self, _ord: i64) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    term: &BytesRef<BS>,
    _state: &TermStateEnum,
  ) -> Result<()> {
    if !self.seek_exact(term)? {
      return Err(LuceneError::illegal_argument(format!(
        "term= {term} does not exist"
      )));
    }
    Ok(())
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    Ok(self.term_ref())
  }

  fn ord(&self) -> Result<i64> {
    Ok(self.term_ord.map_or(-1, |term_ord| term_ord as i64))
  }

  fn doc_freq(&mut self) -> Result<i32> {
    Ok(match self.current_term()? {
      DirectTerm::LowFreq(index) => self.data.low_terms[*index].doc_freq,
      DirectTerm::HighFreq(index) => self.data.high_terms[*index].doc_ids.len() as i32,
    })
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    Ok(match self.current_term()? {
      DirectTerm::LowFreq(index) => self.data.low_terms[*index].total_term_freq as i64,
      DirectTerm::HighFreq(index) => self.data.high_terms[*index].total_term_freq,
    })
  }

  type PostingsEnum = DirectPostingsEnum;

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    DirectPostingsEnum::for_term(&self.data, self.term_ord, reuse, flags)
  }

  type ImpactsEnum = SlowImpactsEnum<DirectPostingsEnum>;

  fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
    Ok(SlowImpactsEnum::new(self.postings_with_flags(None, flags)?))
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    Ok(
      OrdTermState {
        ord: self.term_ord.map_or(-1, |term_ord| term_ord as i64),
      }
      .into(),
    )
  }
}

pub enum DirectPostingsEnum {
  LowFreqDocsNoTf(LowFreqDocsEnumNoTf),
  LowFreqDocsNoPos(LowFreqDocsEnumNoPos),
  LowFreqDocs(LowFreqDocsEnum),
  LowFreqPostings(LowFreqPostingsEnum),
  HighFreqDocs(HighFreqDocsEnum),
  HighFreqPostings(HighFreqPostingsEnum),
}

impl DirectPostingsEnum {
  fn for_term(
    data: &Arc<DirectFieldData>,
    term_ord: Option<usize>,
    reuse: Option<Self>,
    flags: i32,
  ) -> Result<Self> {
    let ord = term_ord
      .filter(|&ord| ord < data.terms.len())
      .ok_or_else(|| LuceneError::illegal_state("terms enum is not positioned"))?;
    let term = &data.terms[ord];
    let has_freq = data.has_freq;
    let has_pos = data.has_pos;
    let has_offsets = data.has_offsets;
    let has_payloads = data.has_payloads;
    if feature_requested(flags, POSITIONS) {
      return Ok(match term {
        DirectTerm::LowFreq(index) if !has_freq => {
          let term = LowFreqTermOwner::new(data, *index);
          let docs_enum = match reuse {
            Some(Self::LowFreqDocsNoTf(mut value)) => {
              value.reset(term);
              value
            },
            _ => LowFreqDocsEnumNoTf::new(term),
          };
          Self::LowFreqDocsNoTf(docs_enum)
        },
        DirectTerm::LowFreq(index) if !has_pos => {
          let term = LowFreqTermOwner::new(data, *index);
          let docs_enum = match reuse {
            Some(Self::LowFreqDocsNoPos(mut value)) => {
              value.reset(term);
              value
            },
            _ => LowFreqDocsEnumNoPos::new(term),
          };
          Self::LowFreqDocsNoPos(docs_enum)
        },
        DirectTerm::LowFreq(index) => Self::LowFreqPostings(LowFreqPostingsEnum::new(
          LowFreqTermOwner::new(data, *index),
          has_offsets,
          has_payloads,
        )),
        DirectTerm::HighFreq(index) if !has_pos => {
          let term = HighFreqTermOwner::new(data, *index);
          let docs_enum = match reuse {
            Some(Self::HighFreqDocs(mut value)) => {
              value.reset(term);
              value
            },
            _ => HighFreqDocsEnum::new(term),
          };
          Self::HighFreqDocs(docs_enum)
        },
        DirectTerm::HighFreq(index) => Self::HighFreqPostings(HighFreqPostingsEnum::new(
          HighFreqTermOwner::new(data, *index),
          has_offsets,
        )),
      });
    }

    Ok(match term {
      DirectTerm::LowFreq(index) if has_freq && has_pos => {
        let term = LowFreqTermOwner::new(data, *index);
        let mut pos_len = if has_offsets { 3 } else { 1 };
        if has_payloads {
          pos_len += 1;
        }
        let docs_enum = match reuse {
          Some(Self::LowFreqDocs(mut value)) if value.can_reuse(pos_len) => {
            value.reset(term);
            value
          },
          _ => LowFreqDocsEnum::new(term, pos_len),
        };
        Self::LowFreqDocs(docs_enum)
      },
      DirectTerm::LowFreq(index) if has_freq => {
        let term = LowFreqTermOwner::new(data, *index);
        let docs_enum = match reuse {
          Some(Self::LowFreqDocsNoPos(mut value)) => {
            value.reset(term);
            value
          },
          _ => LowFreqDocsEnumNoPos::new(term),
        };
        Self::LowFreqDocsNoPos(docs_enum)
      },
      DirectTerm::LowFreq(index) => {
        let term = LowFreqTermOwner::new(data, *index);
        let docs_enum = match reuse {
          Some(Self::LowFreqDocsNoTf(mut value)) => {
            value.reset(term);
            value
          },
          _ => LowFreqDocsEnumNoTf::new(term),
        };
        Self::LowFreqDocsNoTf(docs_enum)
      },
      DirectTerm::HighFreq(index) => {
        let term = HighFreqTermOwner::new(data, *index);
        let docs_enum = match reuse {
          Some(Self::HighFreqDocs(mut value)) => {
            value.reset(term);
            value
          },
          _ => HighFreqDocsEnum::new(term),
        };
        Self::HighFreqDocs(docs_enum)
      },
    })
  }
}

impl crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions for DirectPostingsEnum {}
impl crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for DirectPostingsEnum {}

impl DocIdSetIterator for DirectPostingsEnum {
  fn doc_id(&self) -> i32 {
    match self {
      Self::LowFreqDocsNoTf(value) => value.doc_id(),
      Self::LowFreqDocsNoPos(value) => value.doc_id(),
      Self::LowFreqDocs(value) => value.doc_id(),
      Self::LowFreqPostings(value) => value.doc_id(),
      Self::HighFreqDocs(value) => value.doc_id(),
      Self::HighFreqPostings(value) => value.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::LowFreqDocsNoTf(value) => value.next_doc(),
      Self::LowFreqDocsNoPos(value) => value.next_doc(),
      Self::LowFreqDocs(value) => value.next_doc(),
      Self::LowFreqPostings(value) => value.next_doc(),
      Self::HighFreqDocs(value) => value.next_doc(),
      Self::HighFreqPostings(value) => value.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::LowFreqDocsNoTf(value) => value.advance(target),
      Self::LowFreqDocsNoPos(value) => value.advance(target),
      Self::LowFreqDocs(value) => value.advance(target),
      Self::LowFreqPostings(value) => value.advance(target),
      Self::HighFreqDocs(value) => value.advance(target),
      Self::HighFreqPostings(value) => value.advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::LowFreqDocsNoTf(value) => value.cost(),
      Self::LowFreqDocsNoPos(value) => value.cost(),
      Self::LowFreqDocs(value) => value.cost(),
      Self::LowFreqPostings(value) => value.cost(),
      Self::HighFreqDocs(value) => value.cost(),
      Self::HighFreqPostings(value) => value.cost(),
    }
  }
}

impl PostingsEnum for DirectPostingsEnum {
  fn freq(&mut self) -> Result<i32> {
    match self {
      Self::LowFreqDocsNoTf(value) => value.freq(),
      Self::LowFreqDocsNoPos(value) => value.freq(),
      Self::LowFreqDocs(value) => value.freq(),
      Self::LowFreqPostings(value) => value.freq(),
      Self::HighFreqDocs(value) => value.freq(),
      Self::HighFreqPostings(value) => value.freq(),
    }
  }

  fn next_position(&mut self) -> Result<i32> {
    match self {
      Self::LowFreqDocsNoTf(value) => value.next_position(),
      Self::LowFreqDocsNoPos(value) => value.next_position(),
      Self::LowFreqDocs(value) => value.next_position(),
      Self::LowFreqPostings(value) => value.next_position(),
      Self::HighFreqDocs(value) => value.next_position(),
      Self::HighFreqPostings(value) => value.next_position(),
    }
  }

  fn start_offset(&self) -> Result<i32> {
    match self {
      Self::LowFreqDocsNoTf(value) => value.start_offset(),
      Self::LowFreqDocsNoPos(value) => value.start_offset(),
      Self::LowFreqDocs(value) => value.start_offset(),
      Self::LowFreqPostings(value) => value.start_offset(),
      Self::HighFreqDocs(value) => value.start_offset(),
      Self::HighFreqPostings(value) => value.start_offset(),
    }
  }

  fn end_offset(&self) -> Result<i32> {
    match self {
      Self::LowFreqDocsNoTf(value) => value.end_offset(),
      Self::LowFreqDocsNoPos(value) => value.end_offset(),
      Self::LowFreqDocs(value) => value.end_offset(),
      Self::LowFreqPostings(value) => value.end_offset(),
      Self::HighFreqDocs(value) => value.end_offset(),
      Self::HighFreqPostings(value) => value.end_offset(),
    }
  }

  fn get_payload(&self) -> Result<Option<BytesRefValueEnum<'_>>> {
    match self {
      Self::LowFreqDocsNoTf(value) => value.get_payload(),
      Self::LowFreqDocsNoPos(value) => value.get_payload(),
      Self::LowFreqDocs(value) => value.get_payload(),
      Self::LowFreqPostings(value) => value.get_payload(),
      Self::HighFreqDocs(value) => value.get_payload(),
      Self::HighFreqPostings(value) => value.get_payload(),
    }
  }
}

// Docs only:
pub struct LowFreqDocsEnumNoTf {
  term: LowFreqTermOwner,
  upto: Option<usize>,
}

impl LowFreqDocsEnumNoTf {
  fn new(term: LowFreqTermOwner) -> Self {
    Self { term, upto: None }
  }

  fn reset(&mut self, term: LowFreqTermOwner) {
    self.term = term;
    self.upto = None;
  }

  fn postings(&self) -> &[i32] {
    &self.term.postings
  }
}

impl crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions for LowFreqDocsEnumNoTf {}
impl crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for LowFreqDocsEnumNoTf {}

impl DocIdSetIterator for LowFreqDocsEnumNoTf {
  fn doc_id(&self) -> i32 {
    let Some(upto) = self.upto else {
      return -1;
    };
    if upto < self.postings().len() {
      self.postings()[upto]
    } else {
      NO_MORE_DOCS
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.upto = Some(self.upto.map_or(0, |upto| upto + 1));
    Ok(self.doc_id())
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.slow_advance(target)
  }

  fn cost(&self) -> Result<i64> {
    Ok(self.postings().len() as i64)
  }
}

impl PostingsEnum for LowFreqDocsEnumNoTf {
  fn freq(&mut self) -> Result<i32> {
    Ok(1)
  }

  fn next_position(&mut self) -> Result<i32> {
    Ok(-1)
  }

  fn start_offset(&self) -> Result<i32> {
    Ok(-1)
  }

  fn end_offset(&self) -> Result<i32> {
    Ok(-1)
  }

  fn get_payload(&self) -> Result<Option<BytesRefValueEnum<'_>>> {
    Ok(None)
  }
}

// Docs + freqs:
pub struct LowFreqDocsEnumNoPos {
  term: LowFreqTermOwner,
  upto: Option<usize>,
}

impl LowFreqDocsEnumNoPos {
  fn new(term: LowFreqTermOwner) -> Self {
    Self { term, upto: None }
  }

  fn reset(&mut self, term: LowFreqTermOwner) {
    self.term = term;
    self.upto = None;
  }

  fn postings(&self) -> &[i32] {
    &self.term.postings
  }
}

impl crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions for LowFreqDocsEnumNoPos {}
impl crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for LowFreqDocsEnumNoPos {}

impl DocIdSetIterator for LowFreqDocsEnumNoPos {
  fn doc_id(&self) -> i32 {
    match self.upto {
      None => -1,
      Some(upto) if upto < self.postings().len() => self.postings()[upto],
      Some(_) => NO_MORE_DOCS,
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.upto = Some(self.upto.map_or(0, |upto| upto + 2));
    Ok(self.doc_id())
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.slow_advance(target)
  }

  fn cost(&self) -> Result<i64> {
    Ok((self.postings().len() / 2) as i64)
  }
}

impl PostingsEnum for LowFreqDocsEnumNoPos {
  fn freq(&mut self) -> Result<i32> {
    let upto = self
      .upto
      .ok_or_else(|| LuceneError::illegal_state("postings enum has no current document"))?;
    Ok(self.postings()[upto + 1])
  }

  fn next_position(&mut self) -> Result<i32> {
    Ok(-1)
  }

  fn start_offset(&self) -> Result<i32> {
    Ok(-1)
  }

  fn end_offset(&self) -> Result<i32> {
    Ok(-1)
  }

  fn get_payload(&self) -> Result<Option<BytesRefValueEnum<'_>>> {
    Ok(None)
  }
}

// Docs + freqs + positions/offsets:
pub struct LowFreqDocsEnum {
  term: LowFreqTermOwner,
  pos_mult: usize,
  upto: Option<usize>,
  freq: i32,
}

impl LowFreqDocsEnum {
  fn new(term: LowFreqTermOwner, pos_mult: usize) -> Self {
    Self {
      term,
      pos_mult,
      upto: None,
      freq: 0,
    }
  }

  fn can_reuse(&self, pos_mult: usize) -> bool {
    self.pos_mult == pos_mult
  }

  fn reset(&mut self, term: LowFreqTermOwner) {
    self.term = term;
    self.upto = None;
    self.freq = 0;
  }

  fn postings(&self) -> &[i32] {
    &self.term.postings
  }
}

impl crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions for LowFreqDocsEnum {}
impl crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for LowFreqDocsEnum {}

impl DocIdSetIterator for LowFreqDocsEnum {
  fn doc_id(&self) -> i32 {
    match self.upto {
      None => -1,
      Some(upto) if upto < self.postings().len() => self.postings()[upto],
      Some(_) => NO_MORE_DOCS,
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    let upto = self
      .upto
      .map_or(0, |upto| upto + (2 + self.freq as usize * self.pos_mult));
    self.upto = Some(upto);
    if upto < self.postings().len() {
      self.freq = self.postings()[upto + 1];
      debug_assert!(self.freq > 0);
    }
    Ok(self.doc_id())
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.slow_advance(target)
  }

  fn cost(&self) -> Result<i64> {
    Ok((self.postings().len() / 2) as i64)
  }
}

impl PostingsEnum for LowFreqDocsEnum {
  fn freq(&mut self) -> Result<i32> {
    Ok(self.freq)
  }

  fn next_position(&mut self) -> Result<i32> {
    Ok(-1)
  }

  fn start_offset(&self) -> Result<i32> {
    Ok(-1)
  }

  fn end_offset(&self) -> Result<i32> {
    Ok(-1)
  }

  fn get_payload(&self) -> Result<Option<BytesRefValueEnum<'_>>> {
    Ok(None)
  }
}

pub struct LowFreqPostingsEnum {
  term: LowFreqTermOwner,
  pos_mult: usize,
  has_offsets: bool,
  has_payloads: bool,
  // Kept independently of cursors that next_doc may advance.
  payload_position: Option<(usize, usize)>,
  upto: usize,
  doc_id: i32,
  freq: i32,
  skip_positions: i32,
  pos: i32,
  start_offset: i32,
  end_offset: i32,
  payload_offset: usize,
  payload_length: usize,
}

impl LowFreqPostingsEnum {
  fn new(term: LowFreqTermOwner, has_offsets: bool, has_payloads: bool) -> Self {
    let pos_mult = if has_offsets {
      if has_payloads { 4 } else { 3 }
    } else if has_payloads {
      2
    } else {
      1
    };
    Self {
      term,
      pos_mult,
      has_offsets,
      has_payloads,
      payload_position: None,
      upto: 0,
      doc_id: -1,
      freq: 0,
      skip_positions: 0,
      pos: -1,
      start_offset: -1,
      end_offset: -1,
      payload_offset: 0,
      payload_length: 0,
    }
  }

  fn low_term(&self) -> &LowFreqTerm {
    &self.term
  }
}

impl crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions for LowFreqPostingsEnum {}
impl crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for LowFreqPostingsEnum {}

impl DocIdSetIterator for LowFreqPostingsEnum {
  fn doc_id(&self) -> i32 {
    self.doc_id
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.pos = -1;
    if self.has_payloads {
      for _ in 0..self.skip_positions {
        self.upto += 1;
        if self.has_offsets {
          self.upto += 2;
        }
        let length = self.low_term().postings[self.upto] as usize;
        self.upto += 1;
        self.payload_offset += length;
      }
    } else {
      self.upto += self.pos_mult * self.skip_positions as usize;
    }

    if self.upto < self.low_term().postings.len() {
      self.doc_id = self.low_term().postings[self.upto];
      self.upto += 1;
      self.freq = self.low_term().postings[self.upto];
      self.upto += 1;
      self.skip_positions = self.freq;
      Ok(self.doc_id)
    } else {
      self.doc_id = NO_MORE_DOCS;
      Ok(self.doc_id)
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.slow_advance(target)
  }

  fn cost(&self) -> Result<i64> {
    Ok((self.low_term().postings.len() / 2) as i64)
  }
}

impl PostingsEnum for LowFreqPostingsEnum {
  fn freq(&mut self) -> Result<i32> {
    Ok(self.freq)
  }

  fn next_position(&mut self) -> Result<i32> {
    debug_assert!(self.skip_positions > 0);
    self.skip_positions -= 1;
    self.pos = self.low_term().postings[self.upto];
    self.upto += 1;
    if self.has_offsets {
      self.start_offset = self.low_term().postings[self.upto];
      self.upto += 1;
      self.end_offset = self.low_term().postings[self.upto];
      self.upto += 1;
    }
    if self.has_payloads {
      self.payload_length = self.low_term().postings[self.upto] as usize;
      self.upto += 1;
      if self.payload_length > 0 {
        let payloads = self
          .term
          .payloads
          .as_ref()
          .ok_or_else(|| LuceneError::illegal_state("payloads are missing"))?;
        let _ = &payloads[self.payload_offset..self.payload_offset + self.payload_length];
        self.payload_position = Some((self.payload_offset, self.payload_length));
      } else {
        self.payload_position = None;
      }
      self.payload_offset += self.payload_length;
    }
    Ok(self.pos)
  }

  fn start_offset(&self) -> Result<i32> {
    Ok(self.start_offset)
  }

  fn end_offset(&self) -> Result<i32> {
    Ok(self.end_offset)
  }

  fn get_payload(&self) -> Result<Option<BytesRefValueEnum<'_>>> {
    let Some((offset, length)) = self.payload_position else {
      return Ok(None);
    };
    let payloads = self
      .term
      .payloads
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("payloads are missing"))?;
    Ok(Some(BytesRefValueEnum::Slice(BytesRef {
      bytes: &payloads[offset..offset + length],
      offset: 0,
      length,
    })))
  }
}

// Docs + freqs:
pub struct HighFreqDocsEnum {
  term: HighFreqTermOwner,
  upto: Option<usize>,
  doc_id: i32,
}

impl HighFreqDocsEnum {
  fn new(term: HighFreqTermOwner) -> Self {
    Self {
      term,
      upto: None,
      doc_id: -1,
    }
  }

  fn reset(&mut self, term: HighFreqTermOwner) {
    self.term = term;
    self.doc_id = -1;
    self.upto = None;
  }

  fn high_term(&self) -> &HighFreqTerm {
    &self.term
  }
}

impl crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions for HighFreqDocsEnum {}
impl crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for HighFreqDocsEnum {}

impl DocIdSetIterator for HighFreqDocsEnum {
  fn doc_id(&self) -> i32 {
    self.doc_id
  }

  fn next_doc(&mut self) -> Result<i32> {
    let upto = self.upto.map_or(0, |upto| upto + 1);
    self.upto = Some(upto);
    if upto < self.high_term().doc_ids.len() {
      self.doc_id = self.high_term().doc_ids[upto];
    } else {
      self.doc_id = NO_MORE_DOCS;
    }
    Ok(self.doc_id)
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    let mut upto = self.upto.map_or(0, |upto| upto + 1);
    self.upto = Some(upto);
    let length = self.high_term().doc_ids.len() as i32;
    if upto == length as usize {
      self.doc_id = NO_MORE_DOCS;
      return Ok(self.doc_id);
    }

    let mut inc = 10;
    let mut next_upto = upto as i32 + 10;
    let (mut low, mut high);
    loop {
      if next_upto >= length {
        low = next_upto - inc;
        high = length - 1;
        break;
      }
      if target <= self.high_term().doc_ids[next_upto as usize] {
        low = next_upto - inc;
        high = next_upto;
        break;
      }
      inc *= 2;
      next_upto += inc;
    }
    loop {
      if low > high {
        upto = low as usize;
        self.upto = Some(upto);
        break;
      }
      let mid = ((low + high) as u32 >> 1) as i32;
      let cmp = self.high_term().doc_ids[mid as usize] - target;
      if cmp < 0 {
        low = mid + 1;
      } else if cmp > 0 {
        high = mid - 1;
      } else {
        upto = mid as usize;
        self.upto = Some(upto);
        break;
      }
    }
    if upto == length as usize {
      self.doc_id = NO_MORE_DOCS;
    } else {
      self.doc_id = self.high_term().doc_ids[upto];
    }
    Ok(self.doc_id)
  }

  fn cost(&self) -> Result<i64> {
    Ok(self.high_term().doc_ids.len() as i64)
  }
}

impl PostingsEnum for HighFreqDocsEnum {
  fn freq(&mut self) -> Result<i32> {
    if let Some(freqs) = &self.high_term().freqs {
      let upto = self
        .upto
        .ok_or_else(|| LuceneError::illegal_state("postings have no current document"))?;
      Ok(freqs[upto])
    } else {
      Ok(1)
    }
  }

  fn next_position(&mut self) -> Result<i32> {
    Ok(-1)
  }

  fn start_offset(&self) -> Result<i32> {
    Ok(-1)
  }

  fn end_offset(&self) -> Result<i32> {
    Ok(-1)
  }

  fn get_payload(&self) -> Result<Option<BytesRefValueEnum<'_>>> {
    Ok(None)
  }
}

pub struct HighFreqPostingsEnum {
  term: HighFreqTermOwner,
  has_offsets: bool,
  pos_jump: i32,
  upto: Option<usize>,
  doc_id: i32,
  pos_upto: i32,
  // The last successful payload may belong to an earlier document.
  payload_position: Option<(usize, usize)>,
}

impl HighFreqPostingsEnum {
  fn new(term: HighFreqTermOwner, has_offsets: bool) -> Self {
    Self {
      term,
      has_offsets,
      pos_jump: if has_offsets { 3 } else { 1 },
      upto: None,
      doc_id: -1,
      pos_upto: 0,
      payload_position: None,
    }
  }

  fn high_term(&self) -> &HighFreqTerm {
    &self.term
  }
}

impl crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions for HighFreqPostingsEnum {}
impl crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for HighFreqPostingsEnum {}

impl DocIdSetIterator for HighFreqPostingsEnum {
  fn doc_id(&self) -> i32 {
    self.doc_id
  }

  fn next_doc(&mut self) -> Result<i32> {
    let upto = self.upto.map_or(0, |upto| upto + 1);
    self.upto = Some(upto);
    if upto < self.high_term().doc_ids.len() {
      self.pos_upto = -self.pos_jump;
      self.doc_id = self.high_term().doc_ids[upto];
    } else {
      self.doc_id = NO_MORE_DOCS;
    }
    Ok(self.doc_id)
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    let mut upto = self.upto.map_or(0, |upto| upto + 1);
    self.upto = Some(upto);
    let length = self.high_term().doc_ids.len() as i32;
    if upto == length as usize {
      self.doc_id = NO_MORE_DOCS;
      return Ok(self.doc_id);
    }

    let mut inc = 10;
    let mut next_upto = upto as i32 + 10;
    let (mut low, mut high);
    loop {
      if next_upto >= length {
        low = next_upto - inc;
        high = length - 1;
        break;
      }
      if target <= self.high_term().doc_ids[next_upto as usize] {
        low = next_upto - inc;
        high = next_upto;
        break;
      }
      inc *= 2;
      next_upto += inc;
    }
    loop {
      if low > high {
        upto = low as usize;
        self.upto = Some(upto);
        break;
      }
      let mid = ((low + high) as u32 >> 1) as i32;
      let cmp = self.high_term().doc_ids[mid as usize] - target;
      if cmp < 0 {
        low = mid + 1;
      } else if cmp > 0 {
        high = mid - 1;
      } else {
        upto = mid as usize;
        self.upto = Some(upto);
        break;
      }
    }
    if upto == length as usize {
      self.doc_id = NO_MORE_DOCS;
    } else {
      self.pos_upto = -self.pos_jump;
      self.doc_id = self.high_term().doc_ids[upto];
    }
    Ok(self.doc_id)
  }

  fn cost(&self) -> Result<i64> {
    Ok(self.high_term().doc_ids.len() as i64)
  }
}

impl PostingsEnum for HighFreqPostingsEnum {
  fn freq(&mut self) -> Result<i32> {
    let upto = self
      .upto
      .ok_or_else(|| LuceneError::illegal_state("postings have no current document"))?;
    Ok(
      self
        .high_term()
        .freqs
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("frequencies are missing"))?[upto],
    )
  }

  fn next_position(&mut self) -> Result<i32> {
    self.pos_upto += self.pos_jump;
    let upto = self
      .upto
      .ok_or_else(|| LuceneError::illegal_state("postings have no current document"))?;
    let position = self
      .high_term()
      .positions
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("positions are missing"))?[upto]
      [self.pos_upto as usize];
    if let Some(payloads) = &self.term.payloads {
      let position_index = (self.pos_upto / self.pos_jump) as usize;
      if payloads[upto][position_index].is_some() {
        self.payload_position = Some((upto, position_index));
      } else {
        self.payload_position = None;
      }
    } else {
      self.payload_position = None;
    }
    Ok(position)
  }

  fn start_offset(&self) -> Result<i32> {
    if self.has_offsets {
      let upto = self
        .upto
        .ok_or_else(|| LuceneError::illegal_state("postings have no current document"))?;
      Ok(
        self
          .high_term()
          .positions
          .as_ref()
          .ok_or_else(|| LuceneError::illegal_state("positions are missing"))?[upto]
          [self.pos_upto as usize + 1],
      )
    } else {
      Ok(-1)
    }
  }

  fn end_offset(&self) -> Result<i32> {
    if self.has_offsets {
      let upto = self
        .upto
        .ok_or_else(|| LuceneError::illegal_state("postings have no current document"))?;
      Ok(
        self
          .high_term()
          .positions
          .as_ref()
          .ok_or_else(|| LuceneError::illegal_state("positions are missing"))?[upto]
          [self.pos_upto as usize + 2],
      )
    } else {
      Ok(-1)
    }
  }

  fn get_payload(&self) -> Result<Option<BytesRefValueEnum<'_>>> {
    let Some((doc_index, position_index)) = self.payload_position else {
      return Ok(None);
    };
    let bytes = self
      .term
      .payloads
      .as_ref()
      .and_then(|payloads| payloads[doc_index][position_index].as_ref())
      .ok_or_else(|| LuceneError::illegal_state("payload value is missing"))?;
    Ok(Some(BytesRefValueEnum::Slice(BytesRef {
      bytes,
      offset: 0,
      length: bytes.len(),
    })))
  }
}
