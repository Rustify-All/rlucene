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
use crate::core::index::dummy::dummy_impacts_enum::DummyImpactsEnum;
use crate::core::index::dummy::dummy_postings_enum::DummyPostingsEnum;
use crate::core::index::filtered_terms_enum::FilteredTermsEnum;
use crate::core::index::impacts_enum::{ImpactsEnum, ImpactsEnumEnum2};
use crate::core::index::postings_enum::{FREQS, PostingsEnum, PostingsEnumEnum2};
use crate::core::index::single_terms_enum::SingleTermsEnum;
use crate::core::index::terms::{Terms, TermsPosting};
use crate::core::index::{BytesRef, BytesRefValue, BytesRefValueEnum};
use crate::core::util::access::ByteSource;
use crate::core::util::attribute_source::AttributeSource;
use crate::core::util::attribute_source::AttributeSourceEnum2;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::dummy::dummy_attribute_source::DummyAttributeSource;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::io_boolean_supplier::IOBooleanSupplier;
use std::marker::PhantomData;

/// Iterator to seek [`seek_ceil(BytesRef)`](TermsEnum::seek_ceil),
/// [`seek_exact(BytesRef)`](TermsEnum::seek_exact) or step through
/// [`next`](BytesRefIterator::next) terms to obtain frequency information
/// [`doc_freq`](TermsEnum::doc_freq), [`PostingsEnum`] or [`ImpactsEnum`] for
/// the current term [`postings`](TermsEnum::postings).
///
/// Term enumerations are always ordered by [`BytesRef`], which is
/// Unicode sort order if the terms are UTF-8 bytes. Each term in the
/// enumeration is greater than the one before it.
///
/// The [`TermsEnum`] is unpositioned when you first obtain it, and you must first
/// successfully call [`next()`](BytesRefIterator::next) or one of the `seek`
/// methods.
pub trait TermsEnum: BytesRefIterator {
  type AttributeSource<'a>: AttributeSource
  where
    Self: 'a;
  type AttributeSourceMut<'a>: AttributeSource
  where
    Self: 'a;
  /// Returns the related attribute source.
  fn attributes(&self) -> Result<Self::AttributeSource<'_>>;
  /// Returns the related attribute source mutably.
  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>>;
  /// Attempts to seek to the exact term.
  ///
  /// Returns `true` if the term is found; `false` if the enum is
  /// unpositioned.
  fn seek_exact<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<bool>;
  type PreparedSeek<BS: ByteSource>: IOBooleanSupplier<Self, BS>;

  /// Two-phase [`seek_exact`](TermsEnum::seek_exact). The first phase typically
  /// prefetches the relevant bytes. The returned supplier completes the seek.
  ///
  /// Prepare multiple terms enums before completing their seeks to allow
  /// their I/O to run in parallel.
  ///
  /// Returns `None` if the term can be ruled out during preparation.
  ///
  /// Complete the seek on the same thread, with the original cursor and target,
  /// before calling other methods on this cursor.
  fn prepare_seek_exact<BS: ByteSource>(
    &mut self,
    text: &BytesRef<BS>,
  ) -> Result<Option<Self::PreparedSeek<BS>>>;

  /// Seeks to the specified term, if it exists, or to the next (ceiling)
  /// term. Returns [`SeekStatus`] to indicate whether the exact term was
  /// found, a different term was found, or EOF was hit.
  /// The target term may be before or after the current term.
  /// If this returns [`SeekStatus::End`], the enum is unpositioned.
  fn seek_ceil<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<SeekStatus>;

  /// Seeks to the specified term by ordinal (position) as previously returned
  /// by [`ord()`](TermsEnum::ord). The target ordinal may be before or
  /// after the current ordinal, and must be within bounds.
  fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()>;
  /// Expert: Seeks a specific position by [`TermState`](crate::core::index::term_state::TermState) previously obtained
  /// from [`term_state()`](TermsEnum::term_state). Callers should
  /// maintain the [`TermState`](crate::core::index::term_state::TermState) to use this method.
  /// Low-level implementations may position the [`TermsEnum`] without
  /// re-seeking the term dictionary.
  ///
  /// Seeking by [`TermState`](crate::core::index::term_state::TermState) should only be used if the state was obtained
  /// from the same [`TermsEnum`] instance.
  ///
  /// **NOTE**: Using this method with an incompatible [`TermState`](crate::core::index::term_state::TermState) might
  /// leave this [`TermsEnum`] in an undefined state. On a segment level,
  /// [`TermState`](crate::core::index::term_state::TermState) instances are compatible only if the source and target
  /// [`TermsEnum`] operate on the same field. If operating on segment level,
  /// [`TermState`](crate::core::index::term_state::TermState) instances must not be used across segments.
  ///
  /// **NOTE**: A seek by [`TermState`](crate::core::index::term_state::TermState) might not restore the
  /// [`AttributeSource`]'s state. [`AttributeSource`] states must be
  /// maintained separately if this method is used.
  ///
  /// - `term`: the term the [`TermState`](crate::core::index::term_state::TermState) corresponds to
  /// - `state`: the [`TermState`](crate::core::index::term_state::TermState)
  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    term: &BytesRef<BS>,
    state: &TermStateEnum,
  ) -> Result<()>;

  /// Returns current term. Do not call this when the enum is unpositioned.
  fn term(&self) -> Result<Self::Value<'_>>;
  /// Returns ordinal position for the current term.
  /// This is an optional method (the codec may return an error or indicate
  /// unsupported). Do not call this when the enum is unpositioned.
  fn ord(&self) -> Result<i64>;

  /// Returns the number of documents containing the current term.
  /// Do not call this when the enum is unpositioned.
  /// Equivalent to [`SeekStatus::End`] when exhausted.
  fn doc_freq(&mut self) -> Result<i32>;

  /// Returns the total number of occurrences of this term across all
  /// documents (the sum of `freq()` for each doc that has this term).
  ///
  /// Note: like other term measures, this does not take deleted documents
  /// into account.
  fn total_term_freq(&mut self) -> Result<i64>;

  type PostingsEnum: PostingsEnum;
  /// Get [`PostingsEnum`] for the current term. Do not call this when the
  /// enum is unpositioned. This method will not return `None`.
  ///
  /// **NOTE**: The returned iterator may include deleted documents.
  /// Deleted documents must be checked separately.
  ///
  /// Use this method if you only require documents and frequencies,
  /// and do not need any proximity data.
  /// This is equivalent to [`postings(reuse,
  /// PostingsEnum::FREQS)`](TermsEnum::postings_with_flags).
  ///
  /// - `reuse`: a prior [`PostingsEnum`] for possible reuse See also:
  ///   `postings_with_flags`.
  fn postings(&mut self, reuse: Option<Self::PostingsEnum>) -> Result<Self::PostingsEnum> {
    self.postings_with_flags(reuse, FREQS as i32)
  }

  /// Get [`PostingsEnum`] for the current term, with control over whether
  /// freqs, positions, offsets or payloads are required. Do not call this
  /// when the enum is unpositioned. This method will not return `None`.
  ///
  /// **NOTE**: The returned iterator may include deleted documents,
  /// so deleted documents must be checked on top of the [`PostingsEnum`].
  ///
  /// - `reuse`: a prior [`PostingsEnum`] for possible reuse
  /// - `flags`: specifies which optional per-document values you require (see
  ///   [`PostingsEnum::FREQS`](FREQS))
  fn postings_with_flags(
    &mut self,
    _reuse: Option<Self::PostingsEnum>,
    _flags: i32,
  ) -> Result<Self::PostingsEnum>;
  type ImpactsEnum: ImpactsEnum;
  /// Return an [`ImpactsEnum`].
  ///
  /// See also: [`postings_with_flags`](TermsEnum::postings_with_flags).
  fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum>;

  /// Expert: Returns the [`TermsEnum`]'s internal state to position the enum
  /// without re-seeking the term dictionary.
  ///
  /// **NOTE**: A seek by [`TermState`](crate::core::index::term_state::TermState) might not capture the
  /// [`AttributeSource`]'s state. Callers must maintain
  /// [`AttributeSource`] states separately.
  ///
  /// See also: [`TermState`](crate::core::index::term_state::TermState),
  /// [`seek_exact_with_state`](TermsEnum::seek_exact_with_state).
  fn term_state(&mut self) -> Result<TermStateEnum>;
}
/// Represents returned result from `seek_ceil`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekStatus {
  /// The term was not found, and the end of iteration was hit.
  End,
  /// The precise term was found.
  Found,
  /// A different term was found after the requested term.
  NotFound,
}

/// Deferred ordinary seek for cursors without a preparation step.
pub struct SeekExact;

impl<T: TermsEnum + ?Sized, BS: ByteSource> IOBooleanSupplier<T, BS> for SeekExact {
  fn get(&self, cursor: &mut T, target: &BytesRef<BS>) -> Result<bool> {
    cursor.seek_exact(target)
  }
}

#[doc(hidden)]
pub enum SeekSupplierEnum<A, B, C = (), D = (), E = (), F = ()> {
  A(A),
  B(B),
  C(C),
  D(D),
  E(E),
  F(F),
}

macro_rules! prepare_seek_exact_enum {
  ($($variant:ident($cursor:ty) => $case:ident),+ $(,)?) => {
    type PreparedSeek<BS: ByteSource> = $crate::core::util::io_boolean_supplier::MappedBooleanSupplier<
      Self,
      BS,
      $crate::core::index::terms_enum::SeekSupplierEnum<
        $(<$cursor as TermsEnum>::PreparedSeek<BS>),+
      >
    >;

    fn prepare_seek_exact<BS: ByteSource>(
      &mut self,
      text: &BytesRef<BS>,
    ) -> Result<Option<Self::PreparedSeek<BS>>> {
      use $crate::core::index::terms_enum::SeekSupplierEnum;
      use $crate::core::util::io_boolean_supplier::{IOBooleanSupplier, MappedBooleanSupplier};

      let delegate = match self {
        $(Self::$variant(cursor) => cursor.prepare_seek_exact(text)?.map(SeekSupplierEnum::$case),)+
      };
      Ok(delegate.map(|delegate| MappedBooleanSupplier {
        delegate,
        get: |delegate, cursor, target| match (delegate, cursor) {
          $((SeekSupplierEnum::$case(seek), Self::$variant(cursor)) => seek.get(cursor, target),)+
          _ => Err($crate::core::util::error::lucene_error::LuceneError::illegal_state(
            "prepared seek does not match the cursor",
          )),
        },
      }))
    }
  };
}
pub(crate) use prepare_seek_exact_enum;

macro_rules! prepare_seek_exact_delegate {
  ($cursor:ty => $field:ident) => {
    type PreparedSeek<BS: ByteSource> =
      $crate::core::util::io_boolean_supplier::MappedBooleanSupplier<
        Self,
        BS,
        <$cursor as TermsEnum>::PreparedSeek<BS>,
      >;

    fn prepare_seek_exact<BS: ByteSource>(
      &mut self,
      text: &BytesRef<BS>,
    ) -> Result<Option<Self::PreparedSeek<BS>>> {
      use $crate::core::util::io_boolean_supplier::{IOBooleanSupplier, MappedBooleanSupplier};

      Ok(
        self
          .$field
          .prepare_seek_exact(text)?
          .map(|delegate| MappedBooleanSupplier {
            delegate,
            get: |seek, cursor: &mut Self, target| seek.get(&mut cursor.$field, target),
          }),
      )
    }
  };
}
pub(crate) use prepare_seek_exact_delegate;
#[derive(Default)]
pub struct EmptyTermsEnum;
impl BytesRefIterator for EmptyTermsEnum {
  type Value<'a>
    = &'a BytesRef<Vec<u8>>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    Ok(None)
  }
}

impl TermsEnum for EmptyTermsEnum {
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
    Ok(self.seek_ceil(term)? == SeekStatus::Found)
  }

  type PreparedSeek<BS: ByteSource> = SeekExact;

  fn prepare_seek_exact<BS: ByteSource>(
    &mut self,
    _text: &BytesRef<BS>,
  ) -> Result<Option<Self::PreparedSeek<BS>>> {
    Ok(Some(SeekExact))
  }

  fn seek_ceil<BS: ByteSource>(&mut self, _term: &BytesRef<BS>) -> Result<SeekStatus> {
    Ok(SeekStatus::End)
  }

  fn seek_exact_with_ord(&mut self, _ord: i64) -> Result<()> {
    Ok(())
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    _term: &BytesRef<BS>,
    _state: &TermStateEnum,
  ) -> Result<()> {
    Err(LuceneError::illegal_state(
      "this method should never be called",
    ))
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    Err(LuceneError::illegal_state(
      "this method should never be called",
    ))
  }

  fn ord(&self) -> Result<i64> {
    Err(LuceneError::illegal_state(
      "this method should never be called",
    ))
  }

  fn doc_freq(&mut self) -> Result<i32> {
    Err(LuceneError::illegal_state(
      "this method should never be called",
    ))
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    Err(LuceneError::illegal_state(
      "this method should never be called",
    ))
  }

  type PostingsEnum = DummyPostingsEnum;

  fn postings_with_flags(
    &mut self,
    _reuse: Option<Self::PostingsEnum>,
    _flags: i32,
  ) -> Result<Self::PostingsEnum> {
    Err(LuceneError::illegal_state(
      "this method should never be called",
    ))
  }

  type ImpactsEnum = DummyImpactsEnum;

  fn impacts(&mut self, _flags: i32) -> Result<Self::ImpactsEnum> {
    Err(LuceneError::illegal_state(
      "this method should never be called",
    ))
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    Err(LuceneError::illegal_state(
      "this method should never be called",
    ))
  }
}
pub struct EmptyTermsEnumTermsWrapper<T> {
  _terms_type: PhantomData<fn() -> T>,
}
impl<T> EmptyTermsEnumTermsWrapper<T> {
  pub fn new(_in: T) -> EmptyTermsEnumTermsWrapper<T> {
    Self {
      _terms_type: PhantomData,
    }
  }
}

impl<T> BytesRefIterator for EmptyTermsEnumTermsWrapper<T>
where
  T: Terms,
{
  type Value<'a>
    = &'a BytesRef<Vec<u8>>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    Ok(None)
  }
}

impl<T> TermsEnum for EmptyTermsEnumTermsWrapper<T>
where
  T: Terms,
{
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
    Ok(self.seek_ceil(term)? == SeekStatus::Found)
  }

  type PreparedSeek<BS: ByteSource> = SeekExact;

  fn prepare_seek_exact<BS: ByteSource>(
    &mut self,
    _text: &BytesRef<BS>,
  ) -> Result<Option<Self::PreparedSeek<BS>>> {
    Ok(Some(SeekExact))
  }

  fn seek_ceil<BS: ByteSource>(&mut self, _term: &BytesRef<BS>) -> Result<SeekStatus> {
    Ok(SeekStatus::End)
  }

  fn seek_exact_with_ord(&mut self, _ord: i64) -> Result<()> {
    Ok(())
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    _term: &BytesRef<BS>,
    _state: &TermStateEnum,
  ) -> Result<()> {
    Err(LuceneError::illegal_state(
      "this method should never be called",
    ))
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    Err(LuceneError::illegal_state(
      "this method should never be called",
    ))
  }

  fn ord(&self) -> Result<i64> {
    Err(LuceneError::illegal_state(
      "this method should never be called",
    ))
  }

  fn doc_freq(&mut self) -> Result<i32> {
    Err(LuceneError::illegal_state(
      "this method should never be called",
    ))
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    Err(LuceneError::illegal_state(
      "this method should never be called",
    ))
  }

  type PostingsEnum = TermsPosting<T>;

  fn postings_with_flags(
    &mut self,
    _reuse: Option<Self::PostingsEnum>,
    _flags: i32,
  ) -> Result<Self::PostingsEnum> {
    Err(LuceneError::illegal_state(
      "this method should never be called",
    ))
  }

  type ImpactsEnum = DummyImpactsEnum;

  fn impacts(&mut self, _flags: i32) -> Result<Self::ImpactsEnum> {
    Err(LuceneError::illegal_state(
      "this method should never be called",
    ))
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    Err(LuceneError::illegal_state(
      "this method should never be called",
    ))
  }
}

impl<T> TermsEnum for &mut T
where
  T: TermsEnum,
{
  type AttributeSource<'a>
    = T::AttributeSource<'a>
  where
    Self: 'a;
  type AttributeSourceMut<'a>
    = T::AttributeSourceMut<'a>
  where
    Self: 'a;
  type PostingsEnum = T::PostingsEnum;
  type ImpactsEnum = T::ImpactsEnum;

  #[inline]
  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    (**self).attributes()
  }

  #[inline]
  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    (**self).attributes_mut()
  }

  #[inline]
  fn seek_exact<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<bool> {
    (**self).seek_exact(term)
  }

  type PreparedSeek<BS: ByteSource> = crate::core::util::io_boolean_supplier::MappedBooleanSupplier<
    Self,
    BS,
    <T as TermsEnum>::PreparedSeek<BS>,
  >;

  fn prepare_seek_exact<BS: ByteSource>(
    &mut self,
    text: &BytesRef<BS>,
  ) -> Result<Option<Self::PreparedSeek<BS>>> {
    use crate::core::util::io_boolean_supplier::{IOBooleanSupplier, MappedBooleanSupplier};

    Ok(
      (**self)
        .prepare_seek_exact(text)?
        .map(|delegate| MappedBooleanSupplier {
          delegate,
          get: |seek, cursor: &mut Self, target| seek.get(&mut **cursor, target),
        }),
    )
  }

  #[inline]
  fn seek_ceil<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<SeekStatus> {
    (**self).seek_ceil(term)
  }

  #[inline]
  fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
    (**self).seek_exact_with_ord(ord)
  }

  #[inline]
  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    term: &BytesRef<BS>,
    state: &TermStateEnum,
  ) -> Result<()> {
    (**self).seek_exact_with_state(term, state)
  }

  #[inline]
  fn term(&self) -> Result<Self::Value<'_>> {
    (**self).term()
  }

  #[inline]
  fn ord(&self) -> Result<i64> {
    (**self).ord()
  }

  #[inline]
  fn doc_freq(&mut self) -> Result<i32> {
    (**self).doc_freq()
  }

  #[inline]
  fn total_term_freq(&mut self) -> Result<i64> {
    (**self).total_term_freq()
  }

  #[inline]
  fn postings(&mut self, reuse: Option<Self::PostingsEnum>) -> Result<Self::PostingsEnum> {
    (**self).postings(reuse)
  }

  #[inline]
  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    (**self).postings_with_flags(reuse, flags)
  }

  #[inline]
  fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
    (**self).impacts(flags)
  }

  #[inline]
  fn term_state(&mut self) -> Result<TermStateEnum> {
    (**self).term_state()
  }
}

pub enum TermsEnumWithUnsupportedPostingsAndAttributes2<A, B> {
  WithPostingsAndAttributes(A),
  WithoutPostingsAndAttributes(B),
}

pub enum TermsEnumWithUnsupportedSecondPostings2<A, B> {
  WithPostings(A),
  WithoutPostings(B),
}

impl<A, B> BytesRefIterator for TermsEnumWithUnsupportedSecondPostings2<A, B>
where
  A: TermsEnum,
  B: TermsEnum,
{
  type Value<'a>
    = BytesRefValueEnum<'a>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    match self {
      Self::WithPostings(terms) => terms
        .next()
        .map(|value| value.map(BytesRefValue::into_value)),
      Self::WithoutPostings(terms) => terms
        .next()
        .map(|value| value.map(BytesRefValue::into_value)),
    }
  }

  fn set_next(&mut self) -> Result<bool> {
    match self {
      Self::WithPostings(terms) => terms.set_next(),
      Self::WithoutPostings(terms) => terms.set_next(),
    }
  }
}

impl<A, B> TermsEnum for TermsEnumWithUnsupportedSecondPostings2<A, B>
where
  A: TermsEnum,
  B: TermsEnum,
{
  type AttributeSource<'a>
    = AttributeSourceEnum2<A::AttributeSource<'a>, B::AttributeSource<'a>>
  where
    Self: 'a;
  type AttributeSourceMut<'a>
    = AttributeSourceEnum2<A::AttributeSourceMut<'a>, B::AttributeSourceMut<'a>>
  where
    Self: 'a;

  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    match self {
      Self::WithPostings(terms) => terms.attributes().map(AttributeSourceEnum2::A),
      Self::WithoutPostings(terms) => terms.attributes().map(AttributeSourceEnum2::B),
    }
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    match self {
      Self::WithPostings(terms) => terms.attributes_mut().map(AttributeSourceEnum2::A),
      Self::WithoutPostings(terms) => terms.attributes_mut().map(AttributeSourceEnum2::B),
    }
  }

  fn seek_exact<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<bool> {
    match self {
      Self::WithPostings(terms) => terms.seek_exact(term),
      Self::WithoutPostings(terms) => terms.seek_exact(term),
    }
  }

  crate::core::index::terms_enum::prepare_seek_exact_enum!(
    WithPostings(A) => A,
    WithoutPostings(B) => B,
  );

  fn seek_ceil<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<SeekStatus> {
    match self {
      Self::WithPostings(terms) => terms.seek_ceil(term),
      Self::WithoutPostings(terms) => terms.seek_ceil(term),
    }
  }

  fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
    match self {
      Self::WithPostings(terms) => terms.seek_exact_with_ord(ord),
      Self::WithoutPostings(terms) => terms.seek_exact_with_ord(ord),
    }
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    term: &BytesRef<BS>,
    state: &TermStateEnum,
  ) -> Result<()> {
    match self {
      Self::WithPostings(terms) => terms.seek_exact_with_state(term, state),
      Self::WithoutPostings(terms) => terms.seek_exact_with_state(term, state),
    }
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    match self {
      Self::WithPostings(terms) => terms.term().map(BytesRefValue::into_value),
      Self::WithoutPostings(terms) => terms.term().map(BytesRefValue::into_value),
    }
  }

  fn ord(&self) -> Result<i64> {
    match self {
      Self::WithPostings(terms) => terms.ord(),
      Self::WithoutPostings(terms) => terms.ord(),
    }
  }

  fn doc_freq(&mut self) -> Result<i32> {
    match self {
      Self::WithPostings(terms) => terms.doc_freq(),
      Self::WithoutPostings(terms) => terms.doc_freq(),
    }
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    match self {
      Self::WithPostings(terms) => terms.total_term_freq(),
      Self::WithoutPostings(terms) => terms.total_term_freq(),
    }
  }

  type PostingsEnum = A::PostingsEnum;

  fn postings(&mut self, reuse: Option<Self::PostingsEnum>) -> Result<Self::PostingsEnum> {
    match self {
      Self::WithPostings(terms) => terms.postings(reuse),
      Self::WithoutPostings(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    match self {
      Self::WithPostings(terms) => terms.postings_with_flags(reuse, flags),
      Self::WithoutPostings(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  type ImpactsEnum = A::ImpactsEnum;

  fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
    match self {
      Self::WithPostings(terms) => terms.impacts(flags),
      Self::WithoutPostings(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    match self {
      Self::WithPostings(terms) => terms.term_state(),
      Self::WithoutPostings(terms) => terms.term_state(),
    }
  }
}

pub enum TermsEnumWithUnsupportedSecondAttributes2<A, B> {
  WithAttributes(A),
  WithoutAttributes(B),
}

impl<A, B> BytesRefIterator for TermsEnumWithUnsupportedSecondAttributes2<A, B>
where
  A: TermsEnum,
  B: TermsEnum,
{
  type Value<'a>
    = BytesRefValueEnum<'a>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    match self {
      Self::WithAttributes(terms) => terms
        .next()
        .map(|value| value.map(BytesRefValue::into_value)),
      Self::WithoutAttributes(terms) => terms
        .next()
        .map(|value| value.map(BytesRefValue::into_value)),
    }
  }

  fn set_next(&mut self) -> Result<bool> {
    match self {
      Self::WithAttributes(terms) => terms.set_next(),
      Self::WithoutAttributes(terms) => terms.set_next(),
    }
  }
}

impl<A, B> TermsEnum for TermsEnumWithUnsupportedSecondAttributes2<A, B>
where
  A: TermsEnum,
  B: TermsEnum,
{
  type AttributeSource<'a>
    = A::AttributeSource<'a>
  where
    Self: 'a;
  type AttributeSourceMut<'a>
    = A::AttributeSourceMut<'a>
  where
    Self: 'a;

  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    match self {
      Self::WithAttributes(terms) => terms.attributes(),
      Self::WithoutAttributes(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    match self {
      Self::WithAttributes(terms) => terms.attributes_mut(),
      Self::WithoutAttributes(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn seek_exact<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<bool> {
    match self {
      Self::WithAttributes(terms) => terms.seek_exact(term),
      Self::WithoutAttributes(terms) => terms.seek_exact(term),
    }
  }

  crate::core::index::terms_enum::prepare_seek_exact_enum!(
    WithAttributes(A) => A,
    WithoutAttributes(B) => B,
  );

  fn seek_ceil<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<SeekStatus> {
    match self {
      Self::WithAttributes(terms) => terms.seek_ceil(term),
      Self::WithoutAttributes(terms) => terms.seek_ceil(term),
    }
  }

  fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
    match self {
      Self::WithAttributes(terms) => terms.seek_exact_with_ord(ord),
      Self::WithoutAttributes(terms) => terms.seek_exact_with_ord(ord),
    }
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    term: &BytesRef<BS>,
    state: &TermStateEnum,
  ) -> Result<()> {
    match self {
      Self::WithAttributes(terms) => terms.seek_exact_with_state(term, state),
      Self::WithoutAttributes(terms) => terms.seek_exact_with_state(term, state),
    }
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    match self {
      Self::WithAttributes(terms) => terms.term().map(BytesRefValue::into_value),
      Self::WithoutAttributes(terms) => terms.term().map(BytesRefValue::into_value),
    }
  }

  fn ord(&self) -> Result<i64> {
    match self {
      Self::WithAttributes(terms) => terms.ord(),
      Self::WithoutAttributes(terms) => terms.ord(),
    }
  }

  fn doc_freq(&mut self) -> Result<i32> {
    match self {
      Self::WithAttributes(terms) => terms.doc_freq(),
      Self::WithoutAttributes(terms) => terms.doc_freq(),
    }
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    match self {
      Self::WithAttributes(terms) => terms.total_term_freq(),
      Self::WithoutAttributes(terms) => terms.total_term_freq(),
    }
  }

  type PostingsEnum = PostingsEnumEnum2<A::PostingsEnum, B::PostingsEnum>;

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    match self {
      Self::WithAttributes(terms) => {
        let reuse = match reuse {
          Some(PostingsEnumEnum2::A(reuse)) => Some(reuse),
          _ => None,
        };
        terms
          .postings_with_flags(reuse, flags)
          .map(PostingsEnumEnum2::A)
      },
      Self::WithoutAttributes(terms) => {
        let reuse = match reuse {
          Some(PostingsEnumEnum2::B(reuse)) => Some(reuse),
          _ => None,
        };
        terms
          .postings_with_flags(reuse, flags)
          .map(PostingsEnumEnum2::B)
      },
    }
  }

  type ImpactsEnum = ImpactsEnumEnum2<A::ImpactsEnum, B::ImpactsEnum>;

  fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
    match self {
      Self::WithAttributes(terms) => terms.impacts(flags).map(ImpactsEnumEnum2::A),
      Self::WithoutAttributes(terms) => terms.impacts(flags).map(ImpactsEnumEnum2::B),
    }
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    match self {
      Self::WithAttributes(terms) => terms.term_state(),
      Self::WithoutAttributes(terms) => terms.term_state(),
    }
  }
}

impl<A, B> BytesRefIterator for TermsEnumWithUnsupportedPostingsAndAttributes2<A, B>
where
  A: TermsEnum,
  B: TermsEnum,
{
  type Value<'a>
    = BytesRefValueEnum<'a>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms
        .next()
        .map(|value| value.map(BytesRefValue::into_value)),
      Self::WithoutPostingsAndAttributes(terms) => terms
        .next()
        .map(|value| value.map(BytesRefValue::into_value)),
    }
  }

  fn set_next(&mut self) -> Result<bool> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.set_next(),
      Self::WithoutPostingsAndAttributes(terms) => terms.set_next(),
    }
  }
}

impl<A, B> TermsEnum for TermsEnumWithUnsupportedPostingsAndAttributes2<A, B>
where
  A: TermsEnum,
  B: TermsEnum,
{
  type AttributeSource<'a>
    = A::AttributeSource<'a>
  where
    Self: 'a;
  type AttributeSourceMut<'a>
    = A::AttributeSourceMut<'a>
  where
    Self: 'a;

  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.attributes(),
      Self::WithoutPostingsAndAttributes(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.attributes_mut(),
      Self::WithoutPostingsAndAttributes(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn seek_exact<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<bool> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.seek_exact(term),
      Self::WithoutPostingsAndAttributes(terms) => terms.seek_exact(term),
    }
  }

  crate::core::index::terms_enum::prepare_seek_exact_enum!(
    WithPostingsAndAttributes(A) => A,
    WithoutPostingsAndAttributes(B) => B,
  );

  fn seek_ceil<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<SeekStatus> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.seek_ceil(term),
      Self::WithoutPostingsAndAttributes(terms) => terms.seek_ceil(term),
    }
  }

  fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.seek_exact_with_ord(ord),
      Self::WithoutPostingsAndAttributes(terms) => terms.seek_exact_with_ord(ord),
    }
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    term: &BytesRef<BS>,
    state: &TermStateEnum,
  ) -> Result<()> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.seek_exact_with_state(term, state),
      Self::WithoutPostingsAndAttributes(terms) => terms.seek_exact_with_state(term, state),
    }
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.term().map(BytesRefValue::into_value),
      Self::WithoutPostingsAndAttributes(terms) => terms.term().map(BytesRefValue::into_value),
    }
  }

  fn ord(&self) -> Result<i64> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.ord(),
      Self::WithoutPostingsAndAttributes(terms) => terms.ord(),
    }
  }

  fn doc_freq(&mut self) -> Result<i32> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.doc_freq(),
      Self::WithoutPostingsAndAttributes(terms) => terms.doc_freq(),
    }
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.total_term_freq(),
      Self::WithoutPostingsAndAttributes(terms) => terms.total_term_freq(),
    }
  }

  type PostingsEnum = A::PostingsEnum;

  fn postings(&mut self, reuse: Option<Self::PostingsEnum>) -> Result<Self::PostingsEnum> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.postings(reuse),
      Self::WithoutPostingsAndAttributes(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.postings_with_flags(reuse, flags),
      Self::WithoutPostingsAndAttributes(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  type ImpactsEnum = A::ImpactsEnum;

  fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.impacts(flags),
      Self::WithoutPostingsAndAttributes(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.term_state(),
      Self::WithoutPostingsAndAttributes(terms) => terms.term_state(),
    }
  }
}

pub enum TermsEnumWithUnsupportedPostingsAndAttributesWithEmpty<T> {
  WithPostingsAndAttributes(T),
  WithoutPostingsAndAttributes(EmptyTermsEnum),
}

impl<T> BytesRefIterator for TermsEnumWithUnsupportedPostingsAndAttributesWithEmpty<T>
where
  T: TermsEnum,
{
  type Value<'a>
    = BytesRefValueEnum<'a>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms
        .next()
        .map(|value| value.map(BytesRefValue::into_value)),
      Self::WithoutPostingsAndAttributes(terms) => terms
        .next()
        .map(|value| value.map(BytesRefValue::into_value)),
    }
  }

  fn set_next(&mut self) -> Result<bool> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.set_next(),
      Self::WithoutPostingsAndAttributes(terms) => terms.set_next(),
    }
  }
}

impl<T> TermsEnum for TermsEnumWithUnsupportedPostingsAndAttributesWithEmpty<T>
where
  T: TermsEnum,
{
  type AttributeSource<'a>
    = T::AttributeSource<'a>
  where
    Self: 'a;
  type AttributeSourceMut<'a>
    = T::AttributeSourceMut<'a>
  where
    Self: 'a;

  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.attributes(),
      Self::WithoutPostingsAndAttributes(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.attributes_mut(),
      Self::WithoutPostingsAndAttributes(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn seek_exact<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<bool> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.seek_exact(term),
      Self::WithoutPostingsAndAttributes(terms) => terms.seek_exact(term),
    }
  }

  crate::core::index::terms_enum::prepare_seek_exact_enum!(
    WithPostingsAndAttributes(T) => A,
    WithoutPostingsAndAttributes(EmptyTermsEnum) => B,
  );

  fn seek_ceil<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<SeekStatus> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.seek_ceil(term),
      Self::WithoutPostingsAndAttributes(terms) => terms.seek_ceil(term),
    }
  }

  fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.seek_exact_with_ord(ord),
      Self::WithoutPostingsAndAttributes(terms) => terms.seek_exact_with_ord(ord),
    }
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    term: &BytesRef<BS>,
    state: &TermStateEnum,
  ) -> Result<()> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.seek_exact_with_state(term, state),
      Self::WithoutPostingsAndAttributes(terms) => terms.seek_exact_with_state(term, state),
    }
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.term().map(BytesRefValue::into_value),
      Self::WithoutPostingsAndAttributes(terms) => terms.term().map(BytesRefValue::into_value),
    }
  }

  fn ord(&self) -> Result<i64> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.ord(),
      Self::WithoutPostingsAndAttributes(terms) => terms.ord(),
    }
  }

  fn doc_freq(&mut self) -> Result<i32> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.doc_freq(),
      Self::WithoutPostingsAndAttributes(terms) => terms.doc_freq(),
    }
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.total_term_freq(),
      Self::WithoutPostingsAndAttributes(terms) => terms.total_term_freq(),
    }
  }

  type PostingsEnum = T::PostingsEnum;

  fn postings(&mut self, reuse: Option<Self::PostingsEnum>) -> Result<Self::PostingsEnum> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.postings(reuse),
      Self::WithoutPostingsAndAttributes(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.postings_with_flags(reuse, flags),
      Self::WithoutPostingsAndAttributes(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  type ImpactsEnum = T::ImpactsEnum;

  fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.impacts(flags),
      Self::WithoutPostingsAndAttributes(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.term_state(),
      Self::WithoutPostingsAndAttributes(terms) => terms.term_state(),
    }
  }
}

#[allow(clippy::large_enum_variant)] // Keep the statically dispatched terms iterator allocation-free.
pub enum TermsEnumWithUnsupportedPostingsAndAttributesWithEmptyIntersect<T> {
  WithPostingsAndAttributes(T),
  WithoutPostingsAndAttributes(FilteredTermsEnum<EmptyTermsEnum, AutomatonTermsEnum>),
}

impl<T> BytesRefIterator for TermsEnumWithUnsupportedPostingsAndAttributesWithEmptyIntersect<T>
where
  T: TermsEnum,
{
  type Value<'a>
    = BytesRefValueEnum<'a>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms
        .next()
        .map(|value| value.map(BytesRefValue::into_value)),
      Self::WithoutPostingsAndAttributes(terms) => terms
        .next()
        .map(|value| value.map(BytesRefValue::into_value)),
    }
  }

  fn set_next(&mut self) -> Result<bool> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.set_next(),
      Self::WithoutPostingsAndAttributes(terms) => terms.set_next(),
    }
  }
}

impl<T> TermsEnum for TermsEnumWithUnsupportedPostingsAndAttributesWithEmptyIntersect<T>
where
  T: TermsEnum,
{
  type AttributeSource<'a>
    = T::AttributeSource<'a>
  where
    Self: 'a;
  type AttributeSourceMut<'a>
    = T::AttributeSourceMut<'a>
  where
    Self: 'a;

  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.attributes(),
      Self::WithoutPostingsAndAttributes(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.attributes_mut(),
      Self::WithoutPostingsAndAttributes(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn seek_exact<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<bool> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.seek_exact(term),
      Self::WithoutPostingsAndAttributes(terms) => terms.seek_exact(term),
    }
  }

  crate::core::index::terms_enum::prepare_seek_exact_enum!(
    WithPostingsAndAttributes(T) => A,
    WithoutPostingsAndAttributes(FilteredTermsEnum<EmptyTermsEnum, AutomatonTermsEnum>) => B,
  );

  fn seek_ceil<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<SeekStatus> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.seek_ceil(term),
      Self::WithoutPostingsAndAttributes(terms) => terms.seek_ceil(term),
    }
  }

  fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.seek_exact_with_ord(ord),
      Self::WithoutPostingsAndAttributes(terms) => terms.seek_exact_with_ord(ord),
    }
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    term: &BytesRef<BS>,
    state: &TermStateEnum,
  ) -> Result<()> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.seek_exact_with_state(term, state),
      Self::WithoutPostingsAndAttributes(terms) => terms.seek_exact_with_state(term, state),
    }
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.term().map(BytesRefValue::into_value),
      Self::WithoutPostingsAndAttributes(terms) => terms.term().map(BytesRefValue::into_value),
    }
  }

  fn ord(&self) -> Result<i64> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.ord(),
      Self::WithoutPostingsAndAttributes(terms) => terms.ord(),
    }
  }

  fn doc_freq(&mut self) -> Result<i32> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.doc_freq(),
      Self::WithoutPostingsAndAttributes(terms) => terms.doc_freq(),
    }
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.total_term_freq(),
      Self::WithoutPostingsAndAttributes(terms) => terms.total_term_freq(),
    }
  }

  type PostingsEnum = T::PostingsEnum;

  fn postings(&mut self, reuse: Option<Self::PostingsEnum>) -> Result<Self::PostingsEnum> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.postings(reuse),
      Self::WithoutPostingsAndAttributes(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.postings_with_flags(reuse, flags),
      Self::WithoutPostingsAndAttributes(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  type ImpactsEnum = T::ImpactsEnum;

  fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.impacts(flags),
      Self::WithoutPostingsAndAttributes(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.term_state(),
      Self::WithoutPostingsAndAttributes(terms) => terms.term_state(),
    }
  }
}

#[allow(clippy::large_enum_variant)] // Keep the statically dispatched terms iterator allocation-free.
pub enum TermsEnumWithUnsupportedFirstPostings<T> {
  None(EmptyTermsEnum),
  All(T),
  Single(FilteredTermsEnum<T, SingleTermsEnum>),
  Normal(FilteredTermsEnum<T, AutomatonTermsEnum>),
}

impl<T> BytesRefIterator for TermsEnumWithUnsupportedFirstPostings<T>
where
  T: TermsEnum,
{
  type Value<'a>
    = BytesRefValueEnum<'a>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    match self {
      Self::None(terms) => terms
        .next()
        .map(|value| value.map(BytesRefValue::into_value)),
      Self::All(terms) => terms
        .next()
        .map(|value| value.map(BytesRefValue::into_value)),
      Self::Single(terms) => terms
        .next()
        .map(|value| value.map(BytesRefValue::into_value)),
      Self::Normal(terms) => terms
        .next()
        .map(|value| value.map(BytesRefValue::into_value)),
    }
  }

  fn set_next(&mut self) -> Result<bool> {
    match self {
      Self::None(terms) => terms.set_next(),
      Self::All(terms) => terms.set_next(),
      Self::Single(terms) => terms.set_next(),
      Self::Normal(terms) => terms.set_next(),
    }
  }
}

impl<T> TermsEnum for TermsEnumWithUnsupportedFirstPostings<T>
where
  T: TermsEnum,
{
  type AttributeSource<'a>
    = T::AttributeSource<'a>
  where
    Self: 'a;
  type AttributeSourceMut<'a>
    = T::AttributeSourceMut<'a>
  where
    Self: 'a;

  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    match self {
      Self::None(_) => Err(LuceneError::unsupported_operation("")),
      Self::All(terms) => terms.attributes(),
      Self::Single(terms) => terms.attributes(),
      Self::Normal(terms) => terms.attributes(),
    }
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    match self {
      Self::None(_) => Err(LuceneError::unsupported_operation("")),
      Self::All(terms) => terms.attributes_mut(),
      Self::Single(terms) => terms.attributes_mut(),
      Self::Normal(terms) => terms.attributes_mut(),
    }
  }

  fn seek_exact<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<bool> {
    match self {
      Self::None(terms) => terms.seek_exact(term),
      Self::All(terms) => terms.seek_exact(term),
      Self::Single(terms) => terms.seek_exact(term),
      Self::Normal(terms) => terms.seek_exact(term),
    }
  }

  crate::core::index::terms_enum::prepare_seek_exact_enum!(
    None(EmptyTermsEnum) => A,
    All(T) => B,
    Single(FilteredTermsEnum<T, SingleTermsEnum>) => C,
    Normal(FilteredTermsEnum<T, AutomatonTermsEnum>) => D,
  );

  fn seek_ceil<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<SeekStatus> {
    match self {
      Self::None(terms) => terms.seek_ceil(term),
      Self::All(terms) => terms.seek_ceil(term),
      Self::Single(terms) => terms.seek_ceil(term),
      Self::Normal(terms) => terms.seek_ceil(term),
    }
  }

  fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
    match self {
      Self::None(terms) => terms.seek_exact_with_ord(ord),
      Self::All(terms) => terms.seek_exact_with_ord(ord),
      Self::Single(terms) => terms.seek_exact_with_ord(ord),
      Self::Normal(terms) => terms.seek_exact_with_ord(ord),
    }
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    term: &BytesRef<BS>,
    state: &TermStateEnum,
  ) -> Result<()> {
    match self {
      Self::None(terms) => terms.seek_exact_with_state(term, state),
      Self::All(terms) => terms.seek_exact_with_state(term, state),
      Self::Single(terms) => terms.seek_exact_with_state(term, state),
      Self::Normal(terms) => terms.seek_exact_with_state(term, state),
    }
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    match self {
      Self::None(terms) => terms.term().map(BytesRefValue::into_value),
      Self::All(terms) => terms.term().map(BytesRefValue::into_value),
      Self::Single(terms) => terms.term().map(BytesRefValue::into_value),
      Self::Normal(terms) => terms.term().map(BytesRefValue::into_value),
    }
  }

  fn ord(&self) -> Result<i64> {
    match self {
      Self::None(terms) => terms.ord(),
      Self::All(terms) => terms.ord(),
      Self::Single(terms) => terms.ord(),
      Self::Normal(terms) => terms.ord(),
    }
  }

  fn doc_freq(&mut self) -> Result<i32> {
    match self {
      Self::None(terms) => terms.doc_freq(),
      Self::All(terms) => terms.doc_freq(),
      Self::Single(terms) => terms.doc_freq(),
      Self::Normal(terms) => terms.doc_freq(),
    }
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    match self {
      Self::None(terms) => terms.total_term_freq(),
      Self::All(terms) => terms.total_term_freq(),
      Self::Single(terms) => terms.total_term_freq(),
      Self::Normal(terms) => terms.total_term_freq(),
    }
  }

  type PostingsEnum = T::PostingsEnum;

  fn postings(&mut self, reuse: Option<Self::PostingsEnum>) -> Result<Self::PostingsEnum> {
    match self {
      Self::None(_) => Err(LuceneError::unsupported_operation("")),
      Self::All(terms) => terms.postings(reuse),
      Self::Single(terms) => terms.postings(reuse),
      Self::Normal(terms) => terms.postings(reuse),
    }
  }

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    match self {
      Self::None(_) => Err(LuceneError::unsupported_operation("")),
      Self::All(terms) => terms.postings_with_flags(reuse, flags),
      Self::Single(terms) => terms.postings_with_flags(reuse, flags),
      Self::Normal(terms) => terms.postings_with_flags(reuse, flags),
    }
  }

  type ImpactsEnum = T::ImpactsEnum;

  fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
    match self {
      Self::None(_) => Err(LuceneError::unsupported_operation("")),
      Self::All(terms) => terms.impacts(flags),
      Self::Single(terms) => terms.impacts(flags),
      Self::Normal(terms) => terms.impacts(flags),
    }
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    match self {
      Self::None(terms) => terms.term_state(),
      Self::All(terms) => terms.term_state(),
      Self::Single(terms) => terms.term_state(),
      Self::Normal(terms) => terms.term_state(),
    }
  }
}

macro_rules! define_terms_enum_enum {
    (
        $enum_name:ident,
        $attr_enum:ident,
        $postings_enum:ident,
        $impacts_enum:ident,
        [$($V:ident),+ $(,)?]
    ) => {
        pub enum $enum_name<$($V),+> {
            $($V($V)),+
        }

        impl<$($V),+> BytesRefIterator for $enum_name<$($V),+>
        where
            $($V: TermsEnum,)+
        {
          type Value<'a>
            = BytesRefValueEnum<'a>
          where
            Self: 'a;

            fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
                match self {
                    $(Self::$V(t) => t.next().map(|value| value.map(BytesRefValue::into_value)),)+
                }
            }

            fn set_next(&mut self) -> Result<bool> {
                match self {
                    $(Self::$V(t) => t.set_next(),)+
                }
            }
        }

        impl<$($V),+> TermsEnum for $enum_name<$($V),+>
        where
            $($V: TermsEnum,)+
        {
            type AttributeSource<'a> = $attr_enum<$($V::AttributeSource<'a>),+> where Self: 'a;
            type AttributeSourceMut<'a> = $attr_enum<$($V::AttributeSourceMut<'a>),+> where Self: 'a;

            fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
                match self {
                    $(Self::$V(t) => Ok($attr_enum::$V(t.attributes()?)),)+
                }
            }

            fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
                match self {
                    $(Self::$V(t) => Ok($attr_enum::$V(t.attributes_mut()?)),)+
                }
            }

            fn seek_exact<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<bool> {
                match self {
                    $(Self::$V(t) => t.seek_exact(term),)+
                }
            }

            type PreparedSeek<BS: ByteSource> = $enum_name<$($V::PreparedSeek<BS>),+>;

            fn prepare_seek_exact<BS: ByteSource>(
              &mut self,
              text: &BytesRef<BS>,
            ) -> Result<Option<Self::PreparedSeek<BS>>> {
                match self {
                    $(Self::$V(t) => Ok(t.prepare_seek_exact(text)?.map($enum_name::$V)),)+
                }
            }

            fn seek_ceil<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<SeekStatus> {
                match self {
                    $(Self::$V(t) => t.seek_ceil(term),)+
                }
            }

            fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
                match self {
                    $(Self::$V(t) => t.seek_exact_with_ord(ord),)+
                }
            }

            fn seek_exact_with_state<BS: ByteSource>(
                &mut self,
                term: &BytesRef<BS>,
                state: &TermStateEnum,
            ) -> Result<()> {
                match self {
                    $(Self::$V(t) => t.seek_exact_with_state(term, state),)+
                }
            }

            fn term(&self) -> Result<Self::Value<'_>> {
                match self {
                    $(Self::$V(t) => t.term().map(BytesRefValue::into_value),)+
                }
            }

            fn ord(&self) -> Result<i64> {
                match self {
                    $(Self::$V(t) => t.ord(),)+
                }
            }

            fn doc_freq(&mut self) -> Result<i32> {
                match self {
                    $(Self::$V(t) => t.doc_freq(),)+
                }
            }

            fn total_term_freq(&mut self) -> Result<i64> {
                match self {
                    $(Self::$V(t) => t.total_term_freq(),)+
                }
            }

            type PostingsEnum = $postings_enum<$($V::PostingsEnum),+>;

            fn postings(&mut self, reuse: Option<Self::PostingsEnum>) -> Result<Self::PostingsEnum> {
                match self {
                    $(
                        Self::$V(t) => match reuse {
                            Some($postings_enum::$V(v)) => Ok($postings_enum::$V(t.postings(Some(v))?)),
                            None => Ok($postings_enum::$V(t.postings(None)?)),
                            _ => Ok($postings_enum::$V(t.postings(None)?)),
                        },
                    )+
                }
            }

            fn postings_with_flags(
                &mut self,
                reuse: Option<Self::PostingsEnum>,
                flags: i32,
            ) -> Result<Self::PostingsEnum> {
                match self {
                    $(
                        Self::$V(t) => match reuse {
                            Some($postings_enum::$V(v)) => Ok($postings_enum::$V(t.postings_with_flags(Some(v), flags)?)),
                            None => Ok($postings_enum::$V(t.postings_with_flags(None, flags)?)),
                            _ => Ok($postings_enum::$V(t.postings_with_flags(None, flags)?)),
                        },
                    )+
                }
            }

            type ImpactsEnum = $impacts_enum<$($V::ImpactsEnum),+>;

            fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
                match self {
                    $(Self::$V(t) => Ok($impacts_enum::$V(t.impacts(flags)?)),)+
                }
            }

            fn term_state(&mut self) -> Result<TermStateEnum> {
                match self {
                    $(Self::$V(t) => t.term_state(),)+
                }
            }
        }

        impl<BS, $($V),+> IOBooleanSupplier<$enum_name<$($V),+>, BS>
            for $enum_name<$($V::PreparedSeek<BS>),+>
        where
            BS: ByteSource,
            $($V: TermsEnum,)+
        {
            fn get(&self, cursor: &mut $enum_name<$($V),+>, target: &BytesRef<BS>) -> Result<bool> {
                match (self, cursor) {
                    $((Self::$V(seek), $enum_name::$V(cursor)) => seek.get(cursor, target),)+
                    _ => Err(LuceneError::illegal_state("prepared seek does not match the cursor")),
                }
            }
        }
    };
}
define_terms_enum_enum!(
  TermsEnumEnum2,
  AttributeSourceEnum2,
  PostingsEnumEnum2,
  ImpactsEnumEnum2,
  [A, B]
);
