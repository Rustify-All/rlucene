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
use crate::core::index::automaton_terms_enum::AutomatonTermsEnum;
use crate::core::index::base_terms_enum::BaseTermsEnumTermStateImpl;
use crate::core::index::byte_slice_reader::ByteSliceReader;
use crate::core::index::bytes_ref::BytesRefValueEnum;
use crate::core::index::dummy::dummy_impacts_enum::DummyImpactsEnum;
use crate::core::index::fields::Fields;
use crate::core::index::filtered_terms_enum::FilteredTermsEnum;
use crate::core::index::freq_prox_terms_writer_per_field::FreqProxTermsWriterPerField;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::postings_enum::PostingsEnum;
use crate::core::index::postings_enum::{
  FREQS, OFFSETS, POSITIONS, PostingsEnumEnum2, feature_requested,
};
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::index::{BytesRef, BytesRefBuilder};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::store::DataInput;
use crate::core::util::access::ByteSource;
use crate::core::util::attribute_source::EmptyAttributeSource;
use crate::core::util::automation::compiled_automaton::CompiledAutomaton;
use crate::core::util::bytes_ref_block_pool::{BytesRefBlockPool, BytesRefBlockPoolPosition};
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::int_block_pool::IntBlockPool;
use crate::core::util::iterator::IteratorExt;
use crate::core::util::{ByteBlockPool, ToInt};
use std::borrow::Cow;

/// Provides a limited [`Fields`] implementation (iterators only, no statistics) over the in-memory buffered
/// fields/terms/postings, to flush postings through the PostingsFormat.
pub(crate) struct FreqProxFields {
  fields: Vec<FreqProxTermsWriterPerField>,
  int_pool: IntBlockPool,
  byte_pool: ByteBlockPool,
}
impl FreqProxFields {
  pub fn new(
    field_list: Vec<FreqProxTermsWriterPerField>,
    int_pool: IntBlockPool,
    byte_pool: ByteBlockPool,
  ) -> Self {
    // NOTE: fields are already sorted by field name
    Self {
      fields: field_list,
      int_pool,
      byte_pool,
    }
  }
}
impl<'data> Fields for &'data FreqProxFields {
  type FieldIter<'a>
    = FreqProxFieldIter<'a>
  where
    Self: 'a;

  fn iterator(&self) -> Result<Self::FieldIter<'_>> {
    Ok(FreqProxFieldIter {
      fields: self.fields.as_slice(),
      pos: 0,
    })
  }

  type Terms = FreqProxTerms<'data>;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    let owner = *self;
    let field_index = owner
      .fields
      .binary_search_by(|per_field| per_field.field_info.name.as_str().cmp(field));
    match field_index {
      Ok(index) => Ok(Some(FreqProxTerms::new(
        &owner.fields[index],
        &owner.int_pool,
        &owner.byte_pool,
      ))),
      Err(_) => Ok(None),
    }
  }

  fn size(&self) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }
}

pub(crate) struct FreqProxTerms<'a> {
  terms: &'a FreqProxTermsWriterPerField,
  int_pool: &'a IntBlockPool,
  byte_pool: &'a ByteBlockPool,
}
impl<'data> FreqProxTerms<'data> {
  pub fn new(
    terms: &'data FreqProxTermsWriterPerField,
    int_pool: &'data IntBlockPool,
    byte_pool: &'data ByteBlockPool,
  ) -> Self {
    Self {
      terms,
      int_pool,
      byte_pool,
    }
  }
}
impl<'data> Terms for FreqProxTerms<'data> {
  type TermsEnum = FreqProxTermsEnum<'data>;

  fn iterator(&self) -> Result<Self::TermsEnum> {
    let mut v = FreqProxTermsEnum::new(self.terms, self.int_pool, self.byte_pool);
    v.reset();
    Ok(v)
  }

  type IntersectIter = FilteredTermsEnum<Self::TermsEnum, AutomatonTermsEnum>;

  fn intersect(
    &self,
    compiled: &CompiledAutomaton,
    start_term: Option<&BytesRef<Vec<u8>>>,
  ) -> Result<Self::IntersectIter> {
    self.default_intersect(compiled, start_term)
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
    self
      .terms
      .base
      .index_options
      .cmp(&IndexOptions::DocsAndFreqs)
      .to_int()
      >= 0
  }

  fn has_offsets(&self) -> bool {
    // NOTE: the in-memory buffer may have indexed offsets
    // because that's what FieldInfo said when we started,
    // but during indexing this may have been downgraded:
    self
      .terms
      .base
      .index_options
      .cmp(&IndexOptions::DocsAndFreqsAndPositionsAndOffsets)
      .to_int()
      >= 0
  }

  fn has_positions(&self) -> bool {
    // NOTE: the in-memory buffer may have indexed positions
    // because that's what FieldInfo said when we started,
    // but during indexing this may have been downgraded:
    self
      .terms
      .base
      .index_options
      .cmp(&IndexOptions::DocsAndFreqsAndPositions)
      .to_int()
      >= 0
  }

  fn has_payloads(&self) -> bool {
    self.terms.saw_payloads
  }
}

pub(crate) struct FreqProxTermsEnum<'a> {
  attributes: EmptyAttributeSource,
  terms: &'a FreqProxTermsWriterPerField,
  int_pool: &'a IntBlockPool,
  byte_pool: &'a ByteBlockPool,
  terms_pool: BytesRefBlockPool,
  current_term_position: Option<BytesRefBlockPoolPosition>,
  num_terms: usize,
  ord: Option<usize>,
}
impl<'data> FreqProxTermsEnum<'data> {
  fn new(
    terms: &'data FreqProxTermsWriterPerField,
    int_pool: &'data IntBlockPool,
    byte_pool: &'data ByteBlockPool,
  ) -> Self {
    let (num_terms, terms_pool) = {
      let num_terms = terms.base.get_num_terms();
      let terms_pool = BytesRefBlockPool::new();
      (num_terms, terms_pool)
    };
    Self {
      terms,
      int_pool,
      byte_pool,
      terms_pool,
      attributes: EmptyAttributeSource,
      current_term_position: None,
      num_terms,
      ord: Some(0),
    }
  }
  pub fn reset(&mut self) {
    self.ord = None;
  }

  fn term_ref(&self) -> BytesRef<&[u8]> {
    let Some(position) = self.current_term_position.as_ref() else {
      return BytesRef {
        bytes: &[],
        offset: 0,
        length: 0,
      };
    };
    let block = self.byte_pool.get_buffer(position.block_index);
    BytesRef {
      bytes: block.as_slice(),
      offset: position.offset,
      length: position.length,
    }
  }
}

impl<'data> BytesRefIterator for FreqProxTermsEnum<'data> {
  type Value<'a>
    = BytesRef<&'a [u8]>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    let ord = self.ord.map_or(0, |ord| ord + 1);
    self.ord = Some(ord);
    if ord >= self.num_terms {
      return Ok(None);
    }

    let term_id = self.terms.base.get_sorted_term_ids()[ord];

    let postings_array_enum = self.terms.base.postings_array();

    let Some(p) = postings_array_enum else {
      return Err(LuceneError::illegal_state(
        "Expected FreqProx postings array",
      ));
    };

    let text_start = p.parent.starts[term_id as usize][0];
    let position = self.terms_pool.fill_bytes_ref(text_start, self.byte_pool);
    self.current_term_position = Some(position);

    Ok(Some(self.term_ref()))
  }
}

impl<'data> TermsEnum for FreqProxTermsEnum<'data> {
  type AttributeSource<'a>
    = &'a EmptyAttributeSource
  where
    Self: 'a;
  type AttributeSourceMut<'a>
    = &'a mut EmptyAttributeSource
  where
    Self: 'a;

  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    Ok(&self.attributes)
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    Ok(&mut self.attributes)
  }

  fn seek_exact<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<bool> {
    Ok(self.seek_ceil(term)? == SeekStatus::Found)
  }

  fn prepare_seek_exact<BS: ByteSource>(&mut self, _text: &BytesRef<BS>) -> Result<Option<()>> {
    Ok(Some(()))
  }

  fn get_prepare_seek_exact_status<BS: ByteSource>(
    &mut self,
    target: &BytesRef<BS>,
  ) -> Result<bool> {
    self.seek_exact(target)
  }

  fn seek_ceil<BS: ByteSource>(&mut self, text: &BytesRef<BS>) -> Result<SeekStatus> {
    let postings_array_enum = self.terms.base.postings_array();
    let Some(postings_array) = postings_array_enum else {
      return Err(LuceneError::illegal_state("Postings array is none"));
    };

    let sorted_term_ids = self.terms.base.get_sorted_term_ids();

    let mut lo = 0;
    let mut hi = self.num_terms as i32 - 1;
    let mut last_position = None;

    while hi >= lo {
      let mid = (lo + hi) >> 1;
      let mid_index = mid as usize;
      let term_id = sorted_term_ids[mid_index];
      let text_start = postings_array.parent.starts[term_id as usize][0];

      let position = self.terms_pool.fill_bytes_ref(text_start, self.byte_pool);
      let block = self.byte_pool.get_buffer(position.block_index);
      let cmp = block[position.offset..position.offset + position.length]
        .cmp(text.as_byte_slice())
        .to_int();

      if cmp < 0 {
        lo = mid + 1;
      } else if cmp > 0 {
        hi = mid - 1;
      } else {
        self.current_term_position = Some(position);
        self.ord = Some(mid_index);
        debug_assert_eq!(self.term()?.compare_to(text).to_int(), 0);
        return Ok(SeekStatus::Found);
      }
      last_position = Some(position);
    }

    // Preserve the last probe at End, and the previous term for an empty dictionary.
    let lo_index = lo as usize;
    if lo_index >= self.num_terms
      && let Some(position) = last_position
    {
      self.current_term_position = Some(position);
    }

    // not found
    self.ord = Some(lo_index);
    if lo_index >= self.num_terms {
      Ok(SeekStatus::End)
    } else {
      let term_id = sorted_term_ids[lo_index];
      let text_start = postings_array.parent.starts[term_id as usize][0];
      let position = self.terms_pool.fill_bytes_ref(text_start, self.byte_pool);
      self.current_term_position = Some(position);
      debug_assert!(self.term()?.compare_to(text).to_int() > 0);
      Ok(SeekStatus::NotFound)
    }
  }

  fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
    let ord = ord as i32;
    self.ord = (ord != -1).then_some(ord as usize);
    let ord = ord as usize;

    let term_id = self.terms.base.get_sorted_term_ids()[ord];

    let postings_array_enum = self.terms.base.postings_array();

    let Some(p) = postings_array_enum else {
      return Err(LuceneError::illegal_state(
        "Expected FreqProx postings array",
      ));
    };

    let text_start = p.parent.starts[term_id as usize][0];
    let position = self.terms_pool.fill_bytes_ref(text_start, self.byte_pool);
    self.current_term_position = Some(position);

    Ok(())
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    term: &BytesRef<BS>,
    _state: &TermStateEnum,
  ) -> Result<()> {
    if !self.seek_exact(term)? {
      return Err(LuceneError::illegal_argument(format!(
        "term={} does not exist",
        term
      )));
    }
    Ok(())
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    Ok(self.term_ref())
  }

  fn ord(&self) -> Result<i64> {
    Ok(self.ord.map_or(-1, |ord| ord as i64))
  }

  fn doc_freq(&mut self) -> Result<i32> {
    // We do not store this per-term, and we cannot
    // implement this at merge time w/o an added pass
    // through the postings:
    Err(LuceneError::unsupported_operation(""))
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    // We do not store this per-term, and we cannot
    // implement this at merge time w/o an added pass
    // through the postings:
    Err(LuceneError::unsupported_operation(""))
  }

  type PostingsEnum =
    PostingsEnumEnum2<Box<FreqProxPostingsEnum<'data>>, Box<FreqProxDocsEnum<'data>>>;

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    let sorted_term_ids = self.terms.base.get_sorted_term_ids();
    let (has_prox, has_offsets, has_freq) = {
      (
        self.terms.has_prox,
        self.terms.has_offsets,
        self.terms.has_freq,
      )
    };
    if feature_requested(flags, POSITIONS) {
      if !has_prox {
        // Caller wants positions but we didn't index them;
        // don't lie:
        return Err(LuceneError::illegal_argument("did not index positions"));
      }
      if !has_offsets && feature_requested(flags, OFFSETS) {
        // Caller wants offsets but we didn't index them;
        // don't lie:
        return Err(LuceneError::illegal_argument("did not index offsets"));
      }

      let mut pos_enum = match reuse {
        Some(PostingsEnumEnum2::A(p)) if std::ptr::eq(p.terms, self.terms) => p,
        _ => Box::new(FreqProxPostingsEnum::new(
          self.terms,
          self.int_pool,
          self.byte_pool,
        )),
      };
      pos_enum.reset(
        sorted_term_ids[self
          .ord
          .ok_or_else(|| LuceneError::illegal_state("terms enum has no current term"))?],
      )?;
      return Ok(PostingsEnumEnum2::A(pos_enum));
    }

    if !has_freq && feature_requested(flags, FREQS) {
      // Caller wants freqs but we didn't index them;
      // don't lie:
      return Err(LuceneError::illegal_argument("did not index freq"));
    };
    let mut docs_enum = match reuse {
      Some(PostingsEnumEnum2::B(p)) if std::ptr::eq(p.terms, self.terms) => p,
      _ => Box::new(FreqProxDocsEnum::new(
        self.terms,
        self.int_pool,
        self.byte_pool,
      )),
    };
    docs_enum.reset(
      sorted_term_ids[self
        .ord
        .ok_or_else(|| LuceneError::illegal_state("terms enum has no current term"))?],
    )?;
    Ok(PostingsEnumEnum2::B(docs_enum))
  }

  type ImpactsEnum = DummyImpactsEnum;

  fn impacts(&mut self, _flags: i32) -> Result<Self::ImpactsEnum> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    Ok(BaseTermsEnumTermStateImpl.into())
  }
}

pub(crate) struct FreqProxDocsEnum<'a> {
  pub(crate) terms: &'a FreqProxTermsWriterPerField,
  int_pool: &'a IntBlockPool,
  pub(crate) reader: ByteSliceReader<'a>,
  pub(crate) read_term_freq: bool,
  pub(crate) doc_id: i32,
  pub(crate) freq: i32,
  pub(crate) ended: bool,
  pub(crate) term_id: Option<usize>,
}
impl<'data> FreqProxDocsEnum<'data> {
  pub fn new(
    terms: &'data FreqProxTermsWriterPerField,
    int_pool: &'data IntBlockPool,
    byte_pool: &'data ByteBlockPool,
  ) -> Self {
    let read_term_freq = terms.has_freq;
    Self {
      terms,
      int_pool,
      reader: ByteSliceReader::new(byte_pool),
      read_term_freq,
      doc_id: -1,
      freq: 0,
      ended: false,
      term_id: None,
    }
  }
  pub fn reset(&mut self, term_id: i32) -> Result<()> {
    let term_index = term_id as usize;
    self.term_id = (term_id != -1).then_some(term_index);
    self
      .terms
      .base
      .init_reader(&mut self.reader, term_index, 0, self.int_pool)?;
    self.ended = false;
    self.doc_id = -1;
    Ok(())
  }
}

impl<'data> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for FreqProxDocsEnum<'data>
{
}
impl<'data> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for FreqProxDocsEnum<'data>
{
}

impl<'data> DocIdSetIterator for FreqProxDocsEnum<'data> {
  fn doc_id(&self) -> i32 {
    self.doc_id
  }

  fn next_doc(&mut self) -> Result<i32> {
    if self.doc_id == -1 {
      self.doc_id = 0;
    }

    if self.reader.eof() {
      if self.ended {
        return Ok(NO_MORE_DOCS);
      } else {
        self.ended = true;
        {
          let postings_array_enum = self.terms.base.postings_array();
          let Some(postings_array) = postings_array_enum else {
            return Err(LuceneError::illegal_state("Postings array is none"));
          };

          let p = postings_array;
          let term_id = self
            .term_id
            .ok_or_else(|| LuceneError::illegal_state("postings enum has no current term"))?;
          self.doc_id = p.last_docs[term_id][0];
          if self.read_term_freq {
            self.freq = p
              .term_freqs
              .as_ref()
              .ok_or_else(|| LuceneError::illegal_state("term_freqs not available"))?[term_id];
          }
        }
      }
    } else {
      let code = self.reader.read_vint()?;
      if !self.read_term_freq {
        self.doc_id += code;
      } else {
        self.doc_id += (code as u32 >> 1) as i32;
        if (code & 1) != 0 {
          self.freq = 1;
        } else {
          self.freq = self.reader.read_vint()?;
        }
      }
      debug_assert!(matches!(
        self.terms.base.postings_array(),
        Some(p) if self.term_id.is_some_and(|term_id| self.doc_id != p.last_docs[term_id][0])
      ));
    }

    Ok(self.doc_id)
  }

  fn advance(&mut self, _target: i32) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn cost(&self) -> Result<i64> {
    Err(LuceneError::unsupported_operation(""))
  }
}

impl<'data> PostingsEnum for FreqProxDocsEnum<'data> {
  fn freq(&mut self) -> Result<i32> {
    // Don't lie here ... don't want codecs writings lots
    // of wasted 1s into the index:
    if !self.read_term_freq {
      return Err(LuceneError::illegal_state("freq was not indexed"));
    }
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

pub(crate) struct FreqProxPostingsEnum<'a> {
  terms: &'a FreqProxTermsWriterPerField,
  int_pool: &'a IntBlockPool,
  reader: ByteSliceReader<'a>,
  pos_reader: ByteSliceReader<'a>,
  read_offsets: bool,
  doc_id: i32,
  freq: i32,
  pos: i32,
  start_offset: i32,
  end_offset: i32,
  pos_left: i32,
  term_id: usize,
  ended: bool,
  has_payload: bool,
  payload: BytesRefBuilder<Vec<u8>>,
}
impl<'data> FreqProxPostingsEnum<'data> {
  #[inline(never)]
  fn skip_positions(&mut self) -> Result<()> {
    while self.pos_left != 0 {
      self.next_position()?;
    }
    Ok(())
  }

  pub fn new(
    terms: &'data FreqProxTermsWriterPerField,
    int_pool: &'data IntBlockPool,
    byte_pool: &'data ByteBlockPool,
  ) -> Self {
    let has_offsets = terms.has_offsets;
    debug_assert!(terms.has_prox);
    debug_assert!(terms.has_freq);
    Self {
      terms,
      int_pool,
      reader: ByteSliceReader::new(byte_pool),
      pos_reader: ByteSliceReader::new(byte_pool),
      read_offsets: has_offsets,
      doc_id: -1,
      freq: 0,
      pos: 0,
      start_offset: 0,
      end_offset: 0,
      pos_left: 0,
      term_id: 0,
      ended: false,
      has_payload: false,
      payload: BytesRefBuilder::new(),
    }
  }
  pub fn reset(&mut self, term_id: i32) -> Result<()> {
    let term_index = term_id as usize;
    self.term_id = term_index;
    self
      .terms
      .base
      .init_reader(&mut self.reader, term_index, 0, self.int_pool)?;
    self
      .terms
      .base
      .init_reader(&mut self.pos_reader, term_index, 1, self.int_pool)?;
    self.ended = false;
    self.doc_id = -1;
    self.pos_left = 0;
    Ok(())
  }
}

impl<'data> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for FreqProxPostingsEnum<'data>
{
}
impl<'data> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for FreqProxPostingsEnum<'data>
{
}

impl<'data> DocIdSetIterator for FreqProxPostingsEnum<'data> {
  fn doc_id(&self) -> i32 {
    self.doc_id
  }

  fn next_doc(&mut self) -> Result<i32> {
    if self.doc_id == -1 {
      self.doc_id = 0;
    }

    if self.pos_left != 0 {
      self.skip_positions()?;
    }

    if self.reader.eof() {
      if self.ended {
        return Ok(NO_MORE_DOCS);
      } else {
        self.ended = true;
        {
          let postings_array_enum = self.terms.base.postings_array();
          let Some(postings_array) = postings_array_enum else {
            return Err(LuceneError::illegal_state("Postings array is none"));
          };

          let p = postings_array;

          self.doc_id = p.last_docs[self.term_id][0];
          self.freq = p
            .term_freqs
            .as_ref()
            .ok_or_else(|| LuceneError::illegal_state("term_freqs not available"))?[self.term_id];
        }
      }
    } else {
      let code = self.reader.read_vint()?;
      self.doc_id += ((code as u32) >> 1) as i32;
      if (code & 1) != 0 {
        self.freq = 1;
      } else {
        self.freq = self.reader.read_vint()?;
      }
      debug_assert!(matches!(
        self.terms.base.postings_array(),
        Some(p) if self.doc_id != p.last_docs[self.term_id][0]
      ));
    }

    self.pos_left = self.freq;
    self.pos = 0;
    self.start_offset = 0;

    Ok(self.doc_id)
  }

  fn advance(&mut self, _target: i32) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn cost(&self) -> Result<i64> {
    Err(LuceneError::unsupported_operation(""))
  }
}

impl<'data> PostingsEnum for FreqProxPostingsEnum<'data> {
  fn freq(&mut self) -> Result<i32> {
    Ok(self.freq)
  }

  fn next_position(&mut self) -> Result<i32> {
    debug_assert!(self.pos_left > 0);
    self.pos_left -= 1;

    let code = self.pos_reader.read_vint()?;
    self.pos += (code as u32 >> 1) as i32;

    if (code & 1) != 0 {
      self.has_payload = true;
      // has a payload
      let payload_len = self.pos_reader.read_vint()? as usize;
      self.payload.set_length(payload_len);
      self.payload.grow_no_copy(payload_len)?;

      debug_assert!(payload_len <= i32::MAX as usize);
      self
        .pos_reader
        .read_bytes(&mut self.payload.bytes_ref.bytes, 0, payload_len)?;
    } else {
      self.has_payload = false;
    }

    if self.read_offsets {
      self.start_offset += self.pos_reader.read_vint()?;
      self.end_offset = self.start_offset + self.pos_reader.read_vint()?;
    }
    Ok(self.pos)
  }

  fn start_offset(&self) -> Result<i32> {
    if !self.read_offsets {
      return Err(LuceneError::illegal_state("offsets were not indexed"));
    }
    Ok(self.start_offset)
  }

  fn end_offset(&self) -> Result<i32> {
    if !self.read_offsets {
      return Err(LuceneError::illegal_state("offsets were not indexed"));
    }
    Ok(self.end_offset)
  }

  fn get_payload(&self) -> Result<Option<BytesRefValueEnum<'_>>> {
    if self.has_payload {
      Ok(Some(BytesRefValueEnum::Buffer(Cow::Borrowed(
        &self.payload.bytes_ref,
      ))))
    } else {
      Ok(None)
    }
  }
}
pub(crate) struct FreqProxFieldIter<'a> {
  fields: &'a [FreqProxTermsWriterPerField],
  pos: usize,
}

impl<'data> IteratorExt for FreqProxFieldIter<'data> {
  type Item = &'data String;

  fn next(&mut self) -> Result<Option<Self::Item>> {
    let Some(field) = self.fields.get(self.pos) else {
      return Ok(None);
    };
    self.pos += 1;
    Ok(Some(&field.field_info.name))
  }

  fn has_next(&self) -> Result<bool> {
    Ok(self.pos < self.fields.len())
  }
}

impl PostingsEnum for Box<FreqProxPostingsEnum<'_>> {
  fn freq(&mut self) -> Result<i32> {
    (**self).freq()
  }

  #[inline]
  fn next_position(&mut self) -> Result<i32> {
    (**self).next_position()
  }

  fn start_offset(&self) -> Result<i32> {
    (**self).start_offset()
  }

  fn end_offset(&self) -> Result<i32> {
    (**self).end_offset()
  }

  fn get_payload(&self) -> Result<Option<BytesRefValueEnum<'_>>> {
    (**self).get_payload()
  }
}

impl PostingsEnum for Box<FreqProxDocsEnum<'_>> {
  #[inline]
  fn freq(&mut self) -> Result<i32> {
    (**self).freq()
  }

  fn next_position(&mut self) -> Result<i32> {
    (**self).next_position()
  }

  fn start_offset(&self) -> Result<i32> {
    (**self).start_offset()
  }

  fn end_offset(&self) -> Result<i32> {
    (**self).end_offset()
  }

  fn get_payload(&self) -> Result<Option<BytesRefValueEnum<'_>>> {
    (**self).get_payload()
  }
}
