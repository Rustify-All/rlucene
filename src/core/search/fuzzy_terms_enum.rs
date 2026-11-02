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
use crate::core::index::term::Term;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::index::{BytesRef, BytesRefBuilder, BytesRefValue};
use crate::core::search::boost_attribute::BoostAttribute;
use crate::core::search::boost_attribute_impl::BoostAttributeImpl;
use crate::core::search::fuzzy_automaton_builder::FuzzyAutomatonBuilder;
use crate::core::search::max_non_competitive_boost_attribute::MaxNonCompetitiveBoostAttribute;
use crate::core::search::max_non_competitive_boost_attribute_impl::MaxNonCompetitiveBoostAttributeImpl;
use crate::core::util::access::ByteSource;
use crate::core::util::attribute::Attribute;
use crate::core::util::attribute_source::AttributeSource;
use crate::core::util::automation::byte_runnable::ByteRunnable;
use crate::core::util::automation::compiled_automaton::CompiledAutomaton;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::{CoreHelper, ToInt};
/// [`TermsEnum`] implementation that enumerates terms similar to the specified filter term.
///
/// Term enumerations are always ordered by [`BytesRef::cmp`]. Each term in the
/// enumeration is greater than all that precede it.
pub struct FuzzyTermsEnum<T>
where
  T: Terms,
{
  actual_enum: T::IntersectIter,
  attrs: FuzzyTermsEnumAttributeSource,
  terms: T,
  term_length: usize,
  term: BytesRef<Vec<u8>>,
  bottom: f32,
  bottom_term: Option<BytesRef<Vec<u8>>>,
  queued_bottom: Option<BytesRef<Vec<u8>>>,
  max_edits: usize,
}
impl<T> FuzzyTermsEnum<T>
where
  T: Terms,
{
  /// Creates enumeration of all terms from specified `reader` which share a
  /// prefix of length `prefixLength` with `term` and which have at most `maxEdits`
  /// edits.
  ///
  /// After creation, the enumeration already points to the first valid term if
  /// such a term exists.
  ///
  /// # Parameters
  ///
  /// - `terms` - Delivers terms.
  /// - `term` - Pattern term.
  /// - `max_edits` - Maximum edit distance.
  /// - `prefix_length` - the length of the required common prefix
  /// - `transpositions` - whether transpositions should count as a single edit
  ///
  /// # Errors
  ///
  /// Returns an error if there is a low-level IO error.
  pub fn new_with_max_edits(
    terms: T,
    term: Term,
    max_edits: i32,
    prefix_length: usize,
    transpositions: bool,
  ) -> Result<Self> {
    let text = term.text()?.into_owned();
    Self::with_builder(terms, term.bytes, || {
      FuzzyAutomatonBuilder::new(text, max_edits, prefix_length, transpositions)
    })
  }

  /// Creates enumeration of all terms from specified `reader` which share a
  /// prefix of length `prefixLength` with `term` and which have at most `maxEdits`
  /// edits.
  ///
  /// After creation, the enumeration already points to the first valid term if
  /// such a term exists.
  ///
  /// # Parameters
  ///
  /// - `terms` - Delivers terms.
  /// - `term` - Pattern term.
  /// - `max_edits` - Maximum edit distance.
  /// - `prefix_length` - the length of the required common prefix
  /// - `transpositions` - whether transpositions should count as a single edit
  ///
  /// # Errors
  ///
  /// Returns an error if there is a low-level IO error.
  pub(crate) fn new_with_attrs(
    terms: T,
    term: &Term,
    max_edits: i32,
    prefix_length: usize,
    transpositions: bool,
  ) -> Result<Self> {
    let text = term.text()?.into_owned();
    Self::with_builder(terms, term.bytes().clone(), || {
      FuzzyAutomatonBuilder::new(text, max_edits, prefix_length, transpositions)
    })
  }
  fn with_builder<B>(terms: T, term: BytesRef<Vec<u8>>, automaton_builder: B) -> Result<Self>
  where
    B: FnOnce() -> Result<FuzzyAutomatonBuilder>,
  {
    let mut attrs = FuzzyTermsEnumAttributeSource::new();
    attrs.automaton_att.init(automaton_builder)?;

    let automata = attrs.automaton_att.get_automata();
    let term_length = attrs.automaton_att.get_term_length();
    let mut max_edits = automata.len() - 1;

    let bottom = attrs.get_max_non_competitive_boost()?;
    let bottom_term = attrs.get_competitive_term()?.cloned();
    Self::update_max_edits(&mut max_edits, term_length, bottom, &bottom_term, None);
    let actual_enum = Self::get_automaton_enum(&terms, &mut attrs, max_edits, None)?;

    Ok(Self {
      actual_enum,
      attrs,
      terms,
      term_length,
      term,
      bottom,
      bottom_term,
      queued_bottom: None,
      max_edits,
    })
  }
  /// fired when the max non-competitive boost has changed. this is the hook to swap in a smarter actualEnum.
  fn bottom_changed(&mut self, last_term: Option<&BytesRef<Vec<u8>>>) -> Result<()> {
    let old_max_edits = self.max_edits;

    Self::update_max_edits(
      &mut self.max_edits,
      self.term_length,
      self.bottom,
      &self.bottom_term,
      last_term,
    );

    if old_max_edits != self.max_edits {
      // This is a very powerful optimization: the maximum edit distance has changed.  This happens
      // because we collect only the top scoring
      // N (= 50, by default) terms, and if e.g. maxEdits=2, and the queue is now full of matching
      // terms, and we notice that the worst entry
      // in that queue is ed=1, then we can switch the automata here to ed=1 which is a big speedup.
      self.actual_enum =
        Self::get_automaton_enum(&self.terms, &mut self.attrs, self.max_edits, last_term)?;
    }
    Ok(())
  }

  fn update_max_edits(
    max_edits: &mut usize,
    term_length: usize,
    bottom: f32,
    bottom_term: &Option<BytesRef<Vec<u8>>>,
    last_term: Option<&BytesRef<Vec<u8>>>,
  ) {
    let term_after = match (bottom_term, last_term) {
      (None, _) => true,
      (Some(bottom_term), Some(last_term)) => last_term.cmp(bottom_term).to_int() >= 0,
      (Some(_), None) => false,
    };
    // as long as the max non-competitive boost is >= the max boost
    // for some edit distance, keep dropping the max edit distance.
    while *max_edits > 0 {
      let max_boost = 1.0 - (*max_edits as f32 / term_length as f32);
      if bottom < max_boost || (bottom == max_boost && !term_after) {
        break;
      }
      *max_edits -= 1;
    }
  }

  fn get_automaton_enum(
    terms: &T,
    attrs: &mut FuzzyTermsEnumAttributeSource,
    edit_distance: usize,
    last_term: Option<&BytesRef<Vec<u8>>>,
  ) -> Result<T::IntersectIter> {
    let automata = attrs.get_automata_mut();
    debug_assert!(edit_distance < automata.len());
    let compiled = &mut automata[edit_distance];

    let initial_seek_term = match last_term {
      // This is the first enum we are pulling:
      None => None,
      // We are pulling this enum (e.g., ed=1) after iterating for a while already (e.g., ed=2):
      Some(last_term) => compiled.floor(last_term, &mut BytesRefBuilder::new())?,
    };
    terms.intersect(compiled, initial_seek_term.as_ref())
  }

  /// returns true if term is within k edits of the query term
  fn matches<BS: ByteSource>(
    attrs: &mut FuzzyTermsEnumAttributeSource,
    query_term: &BytesRef<Vec<u8>>,
    term_in: &BytesRef<BS>,
    k: usize,
  ) -> Result<bool> {
    let bytes = term_in.bytes.as_slice();
    if k == 0 {
      return Ok(term_in.as_byte_slice() == query_term.as_byte_slice());
    }

    let automata = attrs.get_automata_mut();
    let runnable = automata[k].run_automaton.as_mut().ok_or_else(|| {
      LuceneError::illegal_state(format!(
        "FuzzyTermsEnum automaton for edit distance {} is not initialized",
        k
      ))
    })?;
    runnable.run(bytes, term_in.offset, term_in.length)
  }
}

impl<T> BytesRefIterator for FuzzyTermsEnum<T>
where
  T: Terms,
{
  type Value<'a>
    = <T::IntersectIter as BytesRefIterator>::Value<'a>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    if let Some(queued_bottom) = self.queued_bottom.take() {
      self.bottom_changed(Some(&queued_bottom))?;
    }

    let term = match self.actual_enum.next()? {
      Some(term) => {
        debug_assert!(term.is_valid().is_ok());
        term
      },
      None => return Ok(None),
    };

    let term_ref = term.as_bytes_ref();
    let mut ed = self.max_edits;
    while ed > 0 {
      if Self::matches(&mut self.attrs, &self.term, &term_ref, ed - 1)? {
        ed -= 1;
      } else {
        break;
      }
    }

    if ed == 0 {
      self.attrs.set_boost(1.0)?;
    } else {
      CoreHelper::check_from_index_size(term_ref.offset, term_ref.length, term_ref.bytes.len())?;
      let code_point_count =
        std::str::from_utf8(&term_ref.bytes[term_ref.offset..term_ref.offset + term_ref.length])
          .map_err(LuceneError::from)?
          .chars()
          .count();
      let min_term_length = code_point_count.min(self.term_length);
      let similarity = 1.0 - (ed as f32 / min_term_length as f32);
      self.attrs.set_boost(similarity)?;
    }

    let bottom = self.attrs.get_max_non_competitive_boost()?;
    let bottom_term = self.attrs.get_competitive_term()?;
    #[cfg(debug_assertions)]
    if let Some(bytes_ref) = bottom_term {
      debug_assert!(bytes_ref.is_valid().is_ok());
    }
    if bottom != self.bottom || bottom_term != self.bottom_term.as_ref() {
      self.bottom = bottom;
      if let (Some(previous), Some(current)) = (&mut self.bottom_term, bottom_term) {
        previous.bytes.clone_from(&current.bytes);
        previous.offset = current.offset;
        previous.length = current.length;
      } else {
        self.bottom_term = bottom_term.cloned();
      }
      // clone the term before potentially doing something with it
      // this is a rare but wonderful occurrence anyway

      // We must delay bottomChanged until the next next() call otherwise we mess up docFreq(),
      // etc., for the current term:
      self.queued_bottom = Some(BytesRef::from_bytes(term_ref.as_bytes().to_vec()));
    }

    Ok(Some(term))
  }
}

impl<T> TermsEnum for FuzzyTermsEnum<T>
where
  T: Terms,
{
  type AttributeSource<'a>
    = &'a FuzzyTermsEnumAttributeSource
  where
    Self: 'a;
  type AttributeSourceMut<'a>
    = &'a mut FuzzyTermsEnumAttributeSource
  where
    Self: 'a;
  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    Ok(&self.attrs)
  }
  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    Ok(&mut self.attrs)
  }

  fn seek_exact<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<bool> {
    self.actual_enum.seek_exact(term)
  }

  fn prepare_seek_exact<BS: ByteSource>(&mut self, text: &BytesRef<BS>) -> Result<Option<()>> {
    self.actual_enum.prepare_seek_exact(text)
  }

  fn get_prepare_seek_exact_status<BS: ByteSource>(
    &mut self,
    target: &BytesRef<BS>,
  ) -> Result<bool> {
    self.actual_enum.get_prepare_seek_exact_status(target)
  }

  fn seek_ceil<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<SeekStatus> {
    self.actual_enum.seek_ceil(term)
  }

  fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
    self.actual_enum.seek_exact_with_ord(ord)
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    term: &BytesRef<BS>,
    state: &TermStateEnum,
  ) -> Result<()> {
    self.actual_enum.seek_exact_with_state(term, state)
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    self.actual_enum.term()
  }

  fn ord(&self) -> Result<i64> {
    self.actual_enum.ord()
  }

  fn doc_freq(&mut self) -> Result<i32> {
    self.actual_enum.doc_freq()
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    self.actual_enum.total_term_freq()
  }

  type PostingsEnum = <T::IntersectIter as TermsEnum>::PostingsEnum;

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    self.actual_enum.postings_with_flags(reuse, flags)
  }

  type ImpactsEnum = <T::IntersectIter as TermsEnum>::ImpactsEnum;

  fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
    self.actual_enum.impacts(flags)
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    self.actual_enum.term_state()
  }
}

pub struct FuzzyTermsEnumAttributeSource {
  // We use this to communicate the score (boost) of the current matched term we are on back to
  // MultiTermQuery.TopTermsBlendedFreqScoringRewrite that is collecting the best (default 50)
  // matched terms:
  boost_att: BoostAttributeImpl,
  // MultiTermQuery.TopTermsBlendedFreqScoringRewrite tells us the worst boost still in its queue
  // using this att,
  // which we use to know when we can reduce the automaton from ed=2 to ed=1, or ed=0 if only single
  // top term is collected:
  max_boost_att: MaxNonCompetitiveBoostAttributeImpl,
  automaton_att: AutomatonAttributeImpl,
}
impl FuzzyTermsEnumAttributeSource {
  fn new() -> Self {
    Self {
      boost_att: BoostAttributeImpl::new(),
      max_boost_att: MaxNonCompetitiveBoostAttributeImpl::new(),
      automaton_att: AutomatonAttributeImpl::new(),
    }
  }
  fn get_automata_mut(&mut self) -> &mut [CompiledAutomaton] {
    self.automaton_att.automata.as_mut()
  }
}
impl AttributeSource for FuzzyTermsEnumAttributeSource {
  fn set_boost(&mut self, _boost: f32) -> Result<()> {
    self.boost_att.set_boost(_boost);
    Ok(())
  }

  fn get_boost(&self) -> Result<f32> {
    Ok(self.boost_att.get_boost())
  }

  fn set_max_non_competitive_boost(&mut self, _max_non_competitive_boost: f32) -> Result<()> {
    self
      .max_boost_att
      .set_max_non_competitive_boost(_max_non_competitive_boost);
    Ok(())
  }

  fn get_max_non_competitive_boost(&self) -> Result<f32> {
    Ok(self.max_boost_att.get_max_non_competitive_boost())
  }

  fn set_competitive_term(&mut self, _competitive_term: Option<BytesRef<Vec<u8>>>) -> Result<()> {
    self.max_boost_att.set_competitive_term(_competitive_term);
    Ok(())
  }

  fn get_competitive_term(&self) -> Result<Option<&BytesRef<Vec<u8>>>> {
    Ok(self.max_boost_att.get_competitive_term())
  }

  fn clear_attributes(&mut self) -> Result<()> {
    Ok(())
  }
}
/// Used for sharing automata between segments
///
/// Levenshtein automata are large and expensive to build; we don't want to build them directly
/// on the query because this can blow up caches that use queries as keys; we also don't want to
/// rebuild them for every segment. This attribute allows the [`FuzzyTermsEnum`] to build the automata
/// once for its first segment and then share them for subsequent segment calls.
pub trait AutomatonAttribute: Attribute {
  fn get_automata(&self) -> &[CompiledAutomaton];

  fn get_term_length(&self) -> usize;

  fn init<B>(&mut self, builder: B) -> Result<()>
  where
    B: FnOnce() -> Result<FuzzyAutomatonBuilder>;
}

pub struct AutomatonAttributeImpl {
  automata: Box<[CompiledAutomaton]>,
  term_length: usize,
}
impl AutomatonAttributeImpl {
  pub fn new() -> Self {
    Self {
      automata: Box::default(),
      term_length: 0,
    }
  }
}

impl Attribute for AutomatonAttributeImpl {}

impl AutomatonAttribute for AutomatonAttributeImpl {
  fn get_automata(&self) -> &[CompiledAutomaton] {
    &self.automata
  }

  fn get_term_length(&self) -> usize {
    self.term_length
  }

  fn init<B>(&mut self, builder: B) -> Result<()>
  where
    B: FnOnce() -> Result<FuzzyAutomatonBuilder>,
  {
    let v = builder()?;
    self.term_length = v.get_term_length();
    self.automata = v.build_automaton_set()?.into_boxed_slice();
    Ok(())
  }
}
