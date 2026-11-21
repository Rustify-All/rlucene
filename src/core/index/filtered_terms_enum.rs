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
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::index::{BytesRef, BytesRefValue, BytesRefValueEnum};
use crate::core::util::access::ByteSource;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind};
use std::borrow::Cow;
use std::fmt::Debug;

/// Struct for enumerating a subset of all terms.
///
/// Term enumerations are always ordered by [`BytesRef::cmp`] Each term in the
/// enumeration is greater than all that precede it.
///
/// *Please note:* Consumers of this enum cannot call `seek()`, it is forward
/// only; it will return
/// [`UnsupportedOperationError`](LuceneError::unsupported_operation) when a
/// seeking method is called.
pub struct FilteredTermsEnum<T, F> {
  initial_seek_term: Option<BytesRef<Vec<u8>>>,
  do_seek: bool,
  has_actual_term: bool,
  pub tenum: T,
  hook: FilteredTermsEnumHook<F>,
}

enum FilteredTermsEnumHook<F> {
  Default,
  Filtered(F),
}

impl<F> FilteredTermsEnumHook<F>
where
  F: FilteredTermsEnumBase,
{
  fn next_seek_term<'a>(
    &'a mut self,
    current: Option<&BytesRef<&[u8]>>,
    initial_seek_term: &'a mut Option<BytesRef<Vec<u8>>>,
  ) -> Result<Option<Cow<'a, BytesRef<Vec<u8>>>>> {
    match self {
      Self::Default => Err(LuceneError::unsupported_operation(
        "unfiltered terms enum has no next seek term",
      )),
      Self::Filtered(sub) => match sub.next_seek_term(current) {
        Ok(value) => Ok(value),
        Err(error) if matches!(error.kind(), LuceneErrorKind::NotImplemented(_)) => {
          match initial_seek_term.take() {
            Some(mut value) => Ok(Some(Cow::Owned(BytesRef::from_slice(
              std::mem::take(&mut value.bytes),
              value.offset,
              value.length,
            )))),
            None => Ok(None),
          }
        },
        Err(error) => Err(error),
      },
    }
  }
}
impl<T, F> FilteredTermsEnum<T, F> {
  pub(crate) fn new(tenum: T, sub: F) -> Self {
    Self::with_seek(tenum, true, sub)
  }

  /// Creates a new filtered enumerator with control over initial seeking.
  pub(crate) fn with_seek(tenum: T, start_with_seek: bool, sub: F) -> Self {
    FilteredTermsEnum {
      initial_seek_term: None,
      do_seek: start_with_seek,
      has_actual_term: false,
      tenum,
      hook: FilteredTermsEnumHook::Filtered(sub),
    }
  }
  pub(crate) fn unfiltered(tenum: T) -> Self {
    FilteredTermsEnum {
      initial_seek_term: None,
      do_seek: false,
      has_actual_term: false,
      tenum,
      hook: FilteredTermsEnumHook::Default,
    }
  }
  #[cfg(test)]
  pub(crate) fn set_initial_seek_term(&mut self, term: BytesRef<Vec<u8>>) {
    self.initial_seek_term = Some(term);
  }
}

impl<T, F> BytesRefIterator for FilteredTermsEnum<T, F>
where
  T: TermsEnum,
  F: FilteredTermsEnumBase,
{
  type Value<'a>
    = BytesRefValueEnum<'a>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    if matches!(&self.hook, FilteredTermsEnumHook::Default) {
      return self
        .tenum
        .next()
        .map(|value| value.map(BytesRefValue::into_value));
    }
    loop {
      if self.do_seek {
        self.do_seek = false;
        let t = {
          let current_term = if self.has_actual_term {
            Some(self.tenum.term()?)
          } else {
            None
          };
          let current = current_term.as_ref().map(BytesRefValue::as_bytes_ref);
          let t = self
            .hook
            .next_seek_term(current.as_ref(), &mut self.initial_seek_term)?;
          if let (Some(actual), Some(term)) = (current.as_ref(), t.as_ref()) {
            debug_assert!(term.compare_to(actual).is_gt());
          }
          t
        };
        let Some(t) = t else {
          self.has_actual_term = false;
          return Ok(None);
        };
        if self.tenum.seek_ceil(t.as_ref())? == SeekStatus::End {
          self.has_actual_term = false;
          return Ok(None);
        }
        self.has_actual_term = true;
      } else if self.tenum.next()?.is_none() {
        self.has_actual_term = false;
        return Ok(None);
      } else {
        self.has_actual_term = true;
      }

      let need_ord = match &self.hook {
        FilteredTermsEnumHook::Default => false,
        FilteredTermsEnumHook::Filtered(sub) => sub.need_ord(),
      };
      let ord = if need_ord { self.tenum.ord()? } else { 0 };
      let accept_status = {
        let term = self.tenum.term()?;
        let term_ref = term.as_bytes_ref();
        match &mut self.hook {
          FilteredTermsEnumHook::Default => AcceptStatus::Yes,
          FilteredTermsEnumHook::Filtered(sub) => sub.accept(&term_ref, ord)?,
        }
      };
      match accept_status {
        AcceptStatus::YesAndSeek => {
          self.do_seek = true;
          return self.tenum.term().map(|term| Some(term.into_value()));
        },
        // term accepted, but we need to seek so fall-through
        AcceptStatus::Yes => {
          return self.tenum.term().map(|term| Some(term.into_value()));
        },
        AcceptStatus::NoAndSeek => {
          // invalid term, seek next time
          self.do_seek = true;
        },
        AcceptStatus::End => {
          // we are supposed to end the enum
          return Ok(None);
        },
        // we just iterate again
        AcceptStatus::No => {},
      }
    }
  }
}

impl<T, F> TermsEnum for FilteredTermsEnum<T, F>
where
  T: TermsEnum,
  F: FilteredTermsEnumBase,
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
    self.tenum.attributes()
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    self.tenum.attributes_mut()
  }

  fn seek_exact<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<bool> {
    match &self.hook {
      FilteredTermsEnumHook::Default => self.tenum.seek_exact(term),
      FilteredTermsEnumHook::Filtered(_) => Err(LuceneError::unsupported_operation("")),
    }
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

    match &self.hook {
      FilteredTermsEnumHook::Default => {
        Ok(
          self
            .tenum
            .prepare_seek_exact(text)?
            .map(|delegate| MappedBooleanSupplier {
              delegate,
              get: |seek, cursor: &mut Self, target| seek.get(&mut cursor.tenum, target),
            }),
        )
      },
      FilteredTermsEnumHook::Filtered(_) => Err(LuceneError::unsupported_operation(format!(
        "{} does not support seeking",
        std::any::type_name::<F>()
      ))),
    }
  }

  fn seek_ceil<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<SeekStatus> {
    match &self.hook {
      FilteredTermsEnumHook::Default => self.tenum.seek_ceil(term),
      FilteredTermsEnumHook::Filtered(_) => Err(LuceneError::unsupported_operation(
        "FilteredTermsEnum::seek_ceil",
      )),
    }
  }

  fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
    match &self.hook {
      FilteredTermsEnumHook::Default => self.tenum.seek_exact_with_ord(ord),
      FilteredTermsEnumHook::Filtered(_) => Err(LuceneError::unsupported_operation(
        "FilteredTermsEnum::seek_exact_with_ord",
      )),
    }
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    term: &BytesRef<BS>,
    state: &TermStateEnum,
  ) -> Result<()> {
    match &self.hook {
      FilteredTermsEnumHook::Default => self.tenum.seek_exact_with_state(term, state),
      FilteredTermsEnumHook::Filtered(_) => Err(LuceneError::unsupported_operation(
        "FilteredTermsEnum::seek_exact_with_state",
      )),
    }
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    self.tenum.term().map(BytesRefValue::into_value)
  }

  fn ord(&self) -> Result<i64> {
    self.tenum.ord()
  }

  fn doc_freq(&mut self) -> Result<i32> {
    self.tenum.doc_freq()
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    self.tenum.total_term_freq()
  }

  type PostingsEnum = T::PostingsEnum;

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    self.tenum.postings_with_flags(reuse, flags)
  }

  type ImpactsEnum = T::ImpactsEnum;

  fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
    self.tenum.impacts(flags)
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    self.tenum.term_state()
  }
}

/// Return value indicating whether the term should be accepted or the iteration
/// should end. The `*_SEEK` values denote that after handling the current term,
/// the enum should call [`next_seek_term`](FilteredTermsEnumBase::next_seek_term)
/// and step forward.
///
/// See also:
/// - [`accept`](FilteredTermsEnumBase::accept)
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AcceptStatus {
  /// Accept the term and continue.
  Yes,
  /// Accept the term then seek to the next term returned by
  /// `next_seek_term()`.
  YesAndSeek,
  /// Reject the term and continue.
  No,
  /// Reject the term then seek to the next term returned by
  /// `next_seek_term()`.
  NoAndSeek,
  /// Reject the term and terminate enumeration.
  End,
}
pub trait FilteredTermsEnumBase {
  /// Return if term is accepted, not accepted or the iteration should ended
  /// (and possibly seek).
  fn accept(&mut self, term: &BytesRef<&[u8]>, ord: i64) -> Result<AcceptStatus>;
  fn next_seek_term(
    &mut self,
    _current: Option<&BytesRef<&[u8]>>,
  ) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
    Err(LuceneError::not_implemented(""))
  }
  fn need_ord(&self) -> bool {
    false
  }
}
