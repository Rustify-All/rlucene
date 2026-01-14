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
use std::borrow::Cow;

use crate::core::index::BytesRef;
use crate::core::index::dummy::dummy_impacts_enum::DummyImpactsEnum;
use crate::core::index::dummy::dummy_postings_enum::DummyPostingsEnum;
use crate::core::index::dummy::dummy_term_state_type::DummyTermState;
use crate::core::index::impacts_enum::{ImpactsEnum, ImpactsEnumEnum2};
use crate::core::index::postings_enum::{FREQS, PostingsEnum, PostingsEnumEnum2};
use crate::core::index::term_state::{TermState, TermStateEnum2};
use crate::core::util::attribute_source::AttributeSource;
use crate::core::util::attribute_source::AttributeSourceEnum2;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::dummy::dummy_attribute_source::DummyAttributeSource;
use crate::core::util::error::lucene_error::{LuceneError, Result};

/// Iterator to seek [`seek_ceil(BytesRef)`](TermsEnum::seek_ceil),
/// [`seek_exact(BytesRef)`](TermsEnum::seek_exact) or step through
/// [`next`](BytesRefIterator::next) terms to obtain frequency information
/// [`doc_freq`](TermsEnum::doc_freq), [`PostingsEnum`] or [`ImpactsEnum`] for
/// the current term [`postings`](TermsEnum::postings).
///
/// Term enumerations are always ordered by `BytesRef::compare_to`, which is
/// Unicode sort order if the terms are UTF-8 bytes. Each term in the
/// enumeration is greater than the one before it.
///
/// The `TermsEnum` is unpositioned when you first obtain it, and you must first
/// successfully call [`next()`](BytesRefIterator::next) or one of the `seek`
/// methods.
pub trait TermsEnum: BytesRefIterator {
    type AttributeSource: AttributeSource;
    /// Returns the related attribute source.
    fn attributes(&self) -> Result<Self::AttributeSource> {
        Err(LuceneError::not_implemented(""))
    }
    /// Attempts to seek to the exact term.
    ///
    /// Returns `true` if the term is found; `false` if the enum is
    /// unpositioned.
    fn seek_exact(&mut self, _term: &BytesRef<Vec<u8>>) -> Result<bool> {
        Err(LuceneError::not_implemented(""))
    }
    /// Two-phase [`seek_exact`](TermsEnum::seek_exact). The first phase
    /// typically calls [`IndexInput::prefetch`](crate::core::store::index_input::IndexInput) on the right range of bytes
    /// under the hood, while the second phase
    /// [`see.exact`](TermsEnum::seek_exact) actually seeks the term within
    /// these bytes. This can be used to parallelize I/O across multiple
    /// terms by calling [`prepare_seek_exact`](TermsEnum::prepare_seek_exact)
    /// on multiple terms enums before calling `IOBooleanSupplier::get()`.
    ///
    /// **NOTE**: It is illegal to call other methods on this [`TermsEnum`]
    /// after calling this method until
    /// [`seek_exact`](TermsEnum::seek_exact) is called.
    ///
    /// ⚠️ **Warning:** After calling this method, you **must** call
    /// [`Self::get_prepare_seek_exact_status`] to retrieve the final result,
    /// otherwise the state remains incomplete.
    fn prepare_seek_exact(&mut self, _text: &BytesRef<Vec<u8>>) -> Result<Option<()>> {
        Err(LuceneError::not_implemented(""))
    }
    fn get_prepare_seek_exact_status(&mut self, _target: &BytesRef<Vec<u8>>) -> Result<bool> {
        Err(LuceneError::not_implemented(""))
    }

    /// Seeks to the specified term, if it exists, or to the next (ceiling)
    /// term. Returns `SeekStatus` to indicate whether the exact term was
    /// found, a different term was found, or EOF was hit.
    /// The target term may be before or after the current term.
    /// If this returns `SeekStatus::End`, the enum is unpositioned.
    fn seek_ceil(&mut self, _term: &BytesRef<Vec<u8>>) -> Result<SeekStatus> {
        Err(LuceneError::not_implemented(""))
    }

    /// Seeks to the specified term by ordinal (position) as previously returned
    /// by [`ord()`](TermsEnum::ord). The target ordinal may be before or
    /// after the current ordinal, and must be within bounds.
    fn seek_exact_with_ord(&mut self, _ord: i64) -> Result<()> {
        Err(LuceneError::not_implemented(""))
    }
    /// Expert: Seeks a specific position by [`TermState`] previously obtained
    /// from [`term_state()`](TermsEnum::term_state). Callers should
    /// maintain the [`TermState`] to use this method.
    /// Low-level implementations may position the [`TermsEnum`] without
    /// re-seeking the term dictionary.
    ///
    /// Seeking by [`TermState`] should only be used if the state was obtained
    /// from the same [`TermsEnum`] instance.
    ///
    /// **NOTE**: Using this method with an incompatible [`TermState`] might
    /// leave this [`TermsEnum`] in an undefined state. On a segment level,
    /// [`TermState`] instances are compatible only if the source and target
    /// [`TermsEnum`] operate on the same field. If operating on segment level,
    /// [`TermState`] instances must not be used across segments.
    ///
    /// **NOTE**: A seek by [`TermState`] might not restore the
    /// [`AttributeSource`]'s state. [`AttributeSource`] states must be
    /// maintained separately if this method is used.
    ///
    /// - `term`: the term the [`TermState`] corresponds to
    /// - `state`: the [`TermState`]
    fn seek_exact_with_state(
        &mut self,
        _term: &BytesRef<Vec<u8>>,
        _state: &Self::TermState,
    ) -> Result<()> {
        Err(LuceneError::not_implemented(""))
    }

    /// Returns current term. Do not call this when the enum is unpositioned.
    fn term(&self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
        Err(LuceneError::not_implemented(""))
    }
    /// Returns ordinal position for the current term.
    /// This is an optional method (the codec may return an error or indicate
    /// unsupported). Do not call this when the enum is unpositioned.
    fn ord(&self) -> Result<i64> {
        Err(LuceneError::not_implemented(""))
    }

    /// Returns the number of documents containing the current term.
    /// Do not call this when the enum is unpositioned.
    /// Equivalent to [`SeekStatus::End`] when exhausted.
    fn doc_freq(&mut self) -> Result<i32> {
        Err(LuceneError::not_implemented(""))
    }

    /// Returns the total number of occurrences of this term across all
    /// documents (the sum of `freq()` for each doc that has this term).
    ///
    /// Note: like other term measures, this does not take deleted documents
    /// into account.
    fn total_term_freq(&mut self) -> Result<i64> {
        Err(LuceneError::not_implemented(""))
    }

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
    ) -> Result<Self::PostingsEnum> {
        Err(LuceneError::not_implemented(""))
    }
    type ImpactsEnum: ImpactsEnum;
    /// Return an `ImpactsEnum`.
    ///
    /// See also: [`postings_with_flags`](TermsEnum::postings_with_flags).
    fn impacts(&mut self, _flags: i32) -> Result<Self::ImpactsEnum> {
        Err(LuceneError::not_implemented(""))
    }

    type TermState: TermState;
    /// Expert: Returns the [`TermsEnum`]'s internal state to position the enum
    /// without re-seeking the term dictionary.
    ///
    /// **NOTE**: A seek by [`TermState`] might not capture the
    /// [`AttributeSource`]'s state. Callers must maintain
    /// [`AttributeSource`] states separately.
    ///
    /// See also: [`TermState`],
    /// [`seek_exact_with_state`](TermsEnum::seek_exact_with_state).
    fn term_state(&mut self) -> Result<Self::TermState> {
        Err(LuceneError::not_implemented(""))
    }
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

pub enum PrepareSeekStatus {
    Pending,
    Found,
    NotFound,
}
#[derive(Default)]
pub struct EmptyTermsEnum;

impl BytesRefIterator for EmptyTermsEnum {
    fn next(&mut self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
        Ok(None)
    }
}

impl TermsEnum for EmptyTermsEnum {
    type AttributeSource = DummyAttributeSource;

    fn seek_ceil(&mut self, _term: &BytesRef<Vec<u8>>) -> Result<SeekStatus> {
        Ok(SeekStatus::End)
    }

    fn seek_exact_with_ord(&mut self, _ord: i64) -> Result<()> {
        Ok(())
    }

    fn term(&self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
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

    type TermState = DummyTermState;

    fn term_state(&mut self) -> Result<Self::TermState> {
        Err(LuceneError::illegal_state(
            "this method should never be called",
        ))
    }
}

impl<T> TermsEnum for &mut T
where
    T: TermsEnum + ?Sized,
{
    type AttributeSource = T::AttributeSource;
    type PostingsEnum = T::PostingsEnum;
    type ImpactsEnum = T::ImpactsEnum;
    type TermState = T::TermState;

    #[inline]
    fn attributes(&self) -> Result<Self::AttributeSource> {
        (**self).attributes()
    }

    #[inline]
    fn seek_exact(&mut self, term: &BytesRef<Vec<u8>>) -> Result<bool> {
        (**self).seek_exact(term)
    }

    #[inline]
    fn prepare_seek_exact(&mut self, text: &BytesRef<Vec<u8>>) -> Result<Option<()>> {
        (**self).prepare_seek_exact(text)
    }

    #[inline]
    fn get_prepare_seek_exact_status(&mut self, target: &BytesRef<Vec<u8>>) -> Result<bool> {
        (**self).get_prepare_seek_exact_status(target)
    }

    #[inline]
    fn seek_ceil(&mut self, term: &BytesRef<Vec<u8>>) -> Result<SeekStatus> {
        (**self).seek_ceil(term)
    }

    #[inline]
    fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
        (**self).seek_exact_with_ord(ord)
    }

    #[inline]
    fn seek_exact_with_state(
        &mut self,
        term: &BytesRef<Vec<u8>>,
        state: &Self::TermState,
    ) -> Result<()> {
        (**self).seek_exact_with_state(term, state)
    }

    #[inline]
    fn term(&self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
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
    fn term_state(&mut self) -> Result<Self::TermState> {
        (**self).term_state()
    }
}

// TermsEnum
pub enum TermsEnumEnum2<A, B> {
    A(A),
    B(B),
}

impl<A, B> BytesRefIterator for TermsEnumEnum2<A, B>
where
    A: TermsEnum,
    B: TermsEnum,
{
    fn next(&mut self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
        match self {
            TermsEnumEnum2::A(t) => t.next(),
            TermsEnumEnum2::B(s) => s.next(),
        }
    }

    fn set_next(&mut self) -> Result<bool> {
        match self {
            TermsEnumEnum2::A(t) => t.set_next(),
            TermsEnumEnum2::B(s) => s.set_next(),
        }
    }
}

pub type TermsEnumPostingsEnumType<A, B> = PostingsEnumEnum2<A, B>;
impl<A, B> TermsEnum for TermsEnumEnum2<A, B>
where
    A: TermsEnum,
    B: TermsEnum,
{
    type AttributeSource = AttributeSourceEnum2<A::AttributeSource, B::AttributeSource>;

    fn attributes(&self) -> Result<Self::AttributeSource> {
        match self {
            TermsEnumEnum2::A(t) => Ok(AttributeSourceEnum2::A(t.attributes()?)),
            TermsEnumEnum2::B(s) => Ok(AttributeSourceEnum2::B(s.attributes()?)),
        }
    }

    fn seek_exact(&mut self, _term: &BytesRef<Vec<u8>>) -> Result<bool> {
        match self {
            TermsEnumEnum2::A(t) => t.seek_exact(_term),
            TermsEnumEnum2::B(s) => s.seek_exact(_term),
        }
    }

    fn prepare_seek_exact(&mut self, _text: &BytesRef<Vec<u8>>) -> Result<Option<()>> {
        match self {
            TermsEnumEnum2::A(t) => t.prepare_seek_exact(_text),
            TermsEnumEnum2::B(s) => s.prepare_seek_exact(_text),
        }
    }

    fn seek_ceil(&mut self, _term: &BytesRef<Vec<u8>>) -> Result<SeekStatus> {
        match self {
            TermsEnumEnum2::A(t) => t.seek_ceil(_term),
            TermsEnumEnum2::B(s) => s.seek_ceil(_term),
        }
    }

    fn seek_exact_with_ord(&mut self, _ord: i64) -> Result<()> {
        match self {
            TermsEnumEnum2::A(t) => t.seek_exact_with_ord(_ord),
            TermsEnumEnum2::B(s) => s.seek_exact_with_ord(_ord),
        }
    }

    fn seek_exact_with_state(
        &mut self,
        _term: &BytesRef<Vec<u8>>,
        _state: &Self::TermState,
    ) -> Result<()> {
        match self {
            TermsEnumEnum2::A(t) => match _state {
                TermStateEnum2::A(state) => t.seek_exact_with_state(_term, state),
                _ => Err(LuceneError::illegal_state(
                    "TermsEnumEnum::A expected TermStateEnum::A",
                )),
            },
            TermsEnumEnum2::B(s) => match _state {
                TermStateEnum2::B(state) => s.seek_exact_with_state(_term, state),
                _ => Err(LuceneError::illegal_state(
                    "TermsEnumEnum::B expected TermStateEnum::B",
                )),
            },
        }
    }

    fn term(&self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
        match self {
            TermsEnumEnum2::A(t) => t.term(),
            TermsEnumEnum2::B(s) => s.term(),
        }
    }

    fn ord(&self) -> Result<i64> {
        match self {
            TermsEnumEnum2::A(t) => t.ord(),
            TermsEnumEnum2::B(s) => s.ord(),
        }
    }

    fn doc_freq(&mut self) -> Result<i32> {
        match self {
            TermsEnumEnum2::A(t) => t.doc_freq(),
            TermsEnumEnum2::B(s) => s.doc_freq(),
        }
    }

    fn total_term_freq(&mut self) -> Result<i64> {
        match self {
            TermsEnumEnum2::A(t) => t.total_term_freq(),
            TermsEnumEnum2::B(s) => s.total_term_freq(),
        }
    }

    type PostingsEnum = TermsEnumPostingsEnumType<A::PostingsEnum, B::PostingsEnum>;

    fn postings(&mut self, reuse: Option<Self::PostingsEnum>) -> Result<Self::PostingsEnum> {
        match self {
            TermsEnumEnum2::A(t) => match reuse {
                Some(PostingsEnumEnum2::A(v)) => {
                    let postings_enum = t.postings(Some(v))?;
                    Ok(PostingsEnumEnum2::A(postings_enum))
                },
                None => {
                    let postings_enum = t.postings(None)?;
                    Ok(PostingsEnumEnum2::A(postings_enum))
                },
                _ => Err(LuceneError::illegal_state(
                    "TermsEnumEnum::F expected PostingsEnumEnum::F for reuse",
                )),
            },
            TermsEnumEnum2::B(s) => match reuse {
                Some(PostingsEnumEnum2::B(v)) => {
                    let postings_enum = s.postings(Some(v))?;
                    Ok(PostingsEnumEnum2::B(postings_enum))
                },
                None => {
                    let postings_enum = s.postings(None)?;
                    Ok(PostingsEnumEnum2::B(postings_enum))
                },
                _ => Err(LuceneError::illegal_state(
                    "TermsEnumEnum::S expected PostingsEnumEnum::S for reuse",
                )),
            },
        }
    }

    fn postings_with_flags(
        &mut self,
        reuse: Option<Self::PostingsEnum>,
        flags: i32,
    ) -> Result<Self::PostingsEnum> {
        match self {
            TermsEnumEnum2::A(t) => match reuse {
                Some(PostingsEnumEnum2::A(v)) => {
                    let postings_enum = t.postings_with_flags(Some(v), flags)?;
                    Ok(PostingsEnumEnum2::A(postings_enum))
                },
                None => {
                    let postings_enum = t.postings_with_flags(None, flags)?;
                    Ok(PostingsEnumEnum2::A(postings_enum))
                },
                _ => Err(LuceneError::illegal_state(
                    "TermsEnumEnum::F expected PostingsEnumEnum::F for reuse",
                )),
            },
            TermsEnumEnum2::B(s) => match reuse {
                Some(PostingsEnumEnum2::B(v)) => {
                    let postings_enum = s.postings_with_flags(Some(v), flags)?;
                    Ok(PostingsEnumEnum2::B(postings_enum))
                },
                None => {
                    let postings_enum = s.postings_with_flags(None, flags)?;
                    Ok(PostingsEnumEnum2::B(postings_enum))
                },
                _ => Err(LuceneError::illegal_state(
                    "TermsEnumEnum::S expected PostingsEnumEnum::S for reuse",
                )),
            },
        }
    }

    type ImpactsEnum = ImpactsEnumEnum2<A::ImpactsEnum, B::ImpactsEnum>;

    fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
        match self {
            TermsEnumEnum2::A(t) => {
                let impacts_enum = t.impacts(flags)?;
                Ok(ImpactsEnumEnum2::A(impacts_enum))
            },
            TermsEnumEnum2::B(s) => {
                let impacts_enum = s.impacts(flags)?;
                Ok(ImpactsEnumEnum2::B(impacts_enum))
            },
        }
    }

    type TermState = TermStateEnum2<A::TermState, B::TermState>;

    fn term_state(&mut self) -> Result<Self::TermState> {
        match self {
            TermsEnumEnum2::A(t) => {
                let term_state = t.term_state()?;
                Ok(TermStateEnum2::A(term_state))
            },
            TermsEnumEnum2::B(s) => {
                let term_state = s.term_state()?;
                Ok(TermStateEnum2::B(term_state))
            },
        }
    }
}
#[cfg(test)]
mod tests {
    use crate::core::document::document::Document;
    use crate::core::document::field::Store::No;
    use crate::core::document::field_type::FieldType;
    use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
    use crate::core::index::BytesRef;
    use crate::core::index::automaton_terms_enum::AutomatonTermsEnum;
    use crate::core::index::index_reader::IndexReader;
    use crate::core::index::index_writer_config::IndexWriterConfig;
    use crate::core::index::leaf_reader::LeafReader;
    use crate::core::index::multi_doc_values::MultiDocValues;
    use crate::core::index::multi_terms::get_terms;
    use crate::core::index::numeric_doc_values::NumericDocValues;
    use crate::core::index::postings_enum::NONE;
    use crate::core::index::standard_directory_reader::StandardDirectoryReaderType;
    use crate::core::index::term::Term;
    use crate::core::index::term_state::TermState;
    use crate::core::index::terms::Terms;
    use crate::core::index::terms_enum::{EmptyTermsEnum, TermsEnum};
    use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
    use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
    use crate::core::store::directory::Directory;
    use crate::core::util::automation::automata::Automata;
    use crate::core::util::automation::byte_runnable::ByteRunnable;
    use crate::core::util::automation::compiled_automaton::CompiledAutomaton;
    use crate::core::util::automation::operations::Operations;
    use crate::core::util::automation::reg_exp::RegExp;
    use crate::core::util::bytes_ref_iterator::BytesRefIterator;
    use crate::core::util::error::lucene_error::{LuceneError, Result};
    use crate::test::index::random_index_writer::RandomIndexWriter;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{
        DirType, at_least, get_only_leaf_reader, new_bytes_ref_from_string, new_directory_shared,
        new_index_writer_config, new_string_field, new_text_field, random,
    };
    use crate::test::util::test_util::TestUtil;
    use rand::Rng;
    use std::borrow::Cow;
    use std::collections::{BTreeSet, HashMap, HashSet};

    #[allow(dead_code)] // for quick search
    struct TestTermsEnum;

    const FIELD: &str = "field";

    #[test]
    fn test() -> Result<()> {
        // TODO LineFileDocs未实现
        Ok(())
    }
    fn add_doc<D, R: Rng + ?Sized>(
        random: &mut R,
        writer: &RandomIndexWriter<D>,
        terms: &mut Vec<String>,
        term_to_id: &mut HashMap<BytesRef<Vec<u8>>, i32>,
        id: i32,
        field_to_type: &mut HashMap<String, FieldType>,
    ) -> Result<()>
    where
        D: Directory,
    {
        let mut doc = Document::new();
        doc.add(NumericDocValuesField::new("id", id as i64));

        for s in terms.iter() {
            doc.add(new_string_field("f", s, No, field_to_type)?);
            term_to_id.insert(new_bytes_ref_from_string(random, s.as_ref())?, id);
        }

        writer.add_document(doc)?;
        terms.clear();
        Ok(())
    }
    fn accepts(c: &CompiledAutomaton, b: &BytesRef<Vec<u8>>) -> Result<bool> {
        let mut state: i32 = 0;

        for idx in 0..b.length {
            debug_assert!(state != -1);
            let byte = b.bytes[b.offset + idx];
            state = c.run_automaton.as_ref().unwrap().step(state, byte as i32);
        }

        c.run_automaton.as_ref().unwrap().is_accept(state)
    }

    // TODO IMPORTANT 测试未通过
    fn test_intersect_random() -> Result<()> {
        let mut random = random();
        let dir = new_directory_shared(&mut random)?;
        let writer = RandomIndexWriter::new(&mut random, dir.clone());

        let num_terms = at_least(&mut random, 300);

        let mut terms: HashSet<String> = HashSet::new();
        let mut pending_terms: Vec<String> = Vec::new();
        let mut term_to_id: HashMap<BytesRef<Vec<u8>>, i32> = HashMap::new();
        let mut id: i32 = 0;
        let mut field_to_type: HashMap<String, FieldType> = HashMap::new();
        while terms.len() != num_terms as usize {
            let s = get_random_string(&mut random);
            if !terms.contains(&s) {
                terms.insert(s.clone());
                pending_terms.push(s);
                if random.random_range(0..20) == 7 {
                    add_doc(
                        &mut random,
                        &writer,
                        &mut pending_terms,
                        &mut term_to_id,
                        id,
                        &mut field_to_type,
                    )?;
                    id += 1;
                }
            }
        }

        add_doc(
            &mut random,
            &writer,
            &mut pending_terms,
            &mut term_to_id,
            id,
            &mut field_to_type,
        )?;

        let mut terms_array: Vec<BytesRef<Vec<u8>>> = Vec::with_capacity(terms.len());
        let mut terms_set: HashSet<BytesRef<Vec<u8>>> = HashSet::new();

        for s in &terms {
            let b = new_bytes_ref_from_string(&mut random, s.as_str())?;
            terms_array.push(b.clone());
            terms_set.insert(b);
        }
        terms_array.sort_unstable();

        let r = writer.get_reader()?;
        writer.close()?;

        let max_doc = r.max_doc()?;
        let mut doc_id_to_id = vec![0i32; max_doc as usize];
        let mut values = MultiDocValues::get_numeric_values(&r, "id")?.unwrap();

        for i in 0..max_doc {
            assert_eq!(i, values.next_doc()?);
            doc_id_to_id[i as usize] = values.long_value()? as i32;
        }

        let num_iterations = at_least(&mut random, 3);
        for iter in 0..num_iterations {
            let mut accept_terms: HashSet<String> = HashSet::new();
            let mut sorted_accept_terms: BTreeSet<BytesRef<Vec<u8>>> = BTreeSet::new();

            let keep_pct: f64 = random.random();
            let automaton = if iter == 0 {
                Automata::make_empty()?
            } else {
                for s in &terms {
                    let s2 = if random.random::<f64>() <= keep_pct {
                        s.clone()
                    } else {
                        get_random_string(&mut random)
                    };
                    accept_terms.insert(s2.clone());
                    sorted_accept_terms.insert(new_bytes_ref_from_string(&mut random, &s2)?);
                }
                let v: Vec<BytesRef<Vec<u8>>> = sorted_accept_terms.into_iter().collect();
                Automata::make_string_union(v.as_ref())?
            };

            let mut c = CompiledAutomaton::with_binary(automaton, true, false, false)?;

            let mut accept_terms_array: Vec<BytesRef<Vec<u8>>> =
                Vec::with_capacity(accept_terms.len());
            let mut accept_terms_set: HashSet<BytesRef<Vec<u8>>> = HashSet::new();

            for s in &accept_terms {
                let b = new_bytes_ref_from_string(&mut random, s)?;
                assert!(accepts(&c, &b)?);
                accept_terms_array.push(b.clone());
                accept_terms_set.insert(b);
            }
            accept_terms_array.sort();

            for _ in 0..100 {
                let start_term = if accept_terms_array.is_empty() || random.random_bool(0.5) {
                    None
                } else {
                    Some(&accept_terms_array[random.random_range(0..accept_terms_array.len())])
                };

                if let Some(start_term) = start_term {
                    let mut state: i32 = 0;

                    for idx in 0..start_term.length {
                        let label = start_term.bytes[start_term.offset + idx] as i32 & 0xff;
                        state = c.run_automaton.as_ref().unwrap().step(state, label);
                        assert_ne!(state, -1);
                    }
                }

                let mut te = get_terms(&r, "f")?.unwrap().intersect(&mut c, start_term)?;
                let mut loc = if let Some(st) = start_term {
                    match terms_array.binary_search(st) {
                        Ok(p) => p + 1,
                        Err(p) => p,
                    }
                } else {
                    0
                };

                while loc < terms_array.len() && !accept_terms_set.contains(&terms_array[loc]) {
                    loc += 1;
                }

                let mut postings_enum = None;
                while loc < terms_array.len() {
                    let expected = &terms_array[loc];
                    let actual = te.next()?;
                    assert_eq!(expected, actual.as_ref().unwrap().as_ref());

                    assert_eq!(1, te.doc_freq()?);

                    postings_enum = Some(TestUtil::docs(
                        &mut random,
                        &mut te,
                        postings_enum,
                        NONE as i32,
                    )?);

                    let pe = postings_enum.as_mut().unwrap();
                    let doc_id = pe.next_doc()?;
                    assert_ne!(doc_id, NO_MORE_DOCS);

                    assert_eq!(
                        doc_id_to_id[doc_id as usize],
                        *term_to_id.get(expected).unwrap()
                    );

                    loop {
                        loc += 1;
                        if loc < terms_array.len() && !accept_terms_set.contains(&terms_array[loc])
                        {
                            continue;
                        } else {
                            break;
                        }
                    }
                }
                assert!(te.next()?.is_none());
            }
        }
        Ok(())
    }

    fn make_index<R: Rng + ?Sized>(
        random: &mut R,
        terms: &[String],
    ) -> Result<StandardDirectoryReaderType<DirType>> {
        let dir = new_directory_shared(random)?;
        // TODO: 未实现MockAnalyzer
        let iwc = new_index_writer_config(random);

        let writer = RandomIndexWriter::with_config(random, dir.clone(), iwc);
        let mut field_to_type: HashMap<String, FieldType> = HashMap::new();
        for term in terms {
            let mut doc = Document::new();
            let field = new_string_field(FIELD, term, No, &mut field_to_type)?;
            doc.add(field);
            writer.add_document(doc)?;
        }
        let reader = writer.get_reader()?;
        writer.close()?;
        Ok(reader)
    }
    fn doc_freq<CR>(reader: CR, term: &str) -> Result<i32>
    where
        CR: IndexReader,
    {
        reader.doc_freq(&Term::from_text(FIELD, term))
    }
    #[test]
    fn test_easy() -> Result<()> {
        let mut random = random();

        // No floor arcs:
        let reader = make_index(
            &mut random,
            &[
                "aa0".to_string(),
                "aa1".to_string(),
                "aa2".to_string(),
                "aa3".to_string(),
                "bb0".to_string(),
                "bb1".to_string(),
                "bb2".to_string(),
                "bb3".to_string(),
                "aa".to_string(),
            ],
        )?;

        // First term in block:
        assert_eq!(1, doc_freq(&reader, "aa0")?);

        // Scan forward to another term in same block
        assert_eq!(1, doc_freq(&reader, "aa2")?);

        assert_eq!(1, doc_freq(&reader, "aa")?);

        // Reset same block then scan forwards
        assert_eq!(1, doc_freq(&reader, "aa1")?);

        // Not found, in same block
        assert_eq!(0, doc_freq(&reader, "aa5")?);

        // Found, in same block
        assert_eq!(1, doc_freq(&reader, "aa2")?);

        // Not found in index:
        assert_eq!(0, doc_freq(&reader, "b0")?);

        // Found:
        assert_eq!(1, doc_freq(&reader, "aa2")?);

        // Found, rewind:
        assert_eq!(1, doc_freq(&reader, "aa0")?);

        // First term in block:
        assert_eq!(1, doc_freq(&reader, "bb0")?);

        // Scan forward to another term in same block
        assert_eq!(1, doc_freq(&reader, "bb2")?);

        // Reset same block then scan forwards
        assert_eq!(1, doc_freq(&reader, "bb1")?);

        // Not found, in same block
        assert_eq!(0, doc_freq(&reader, "bb5")?);

        // Found, in same block
        assert_eq!(1, doc_freq(&reader, "bb2")?);

        // Not found in index:
        assert_eq!(0, doc_freq(&reader, "b0")?);

        // Found:
        assert_eq!(1, doc_freq(&reader, "bb2")?);

        // Found, rewind:
        assert_eq!(1, doc_freq(&reader, "bb0")?);

        // reader / dir 由 RAII 自动关闭
        Ok(())
    }
    #[test]
    fn test_floor_blocks() -> Result<()> {
        let mut random = random();

        let terms = vec![
            "aa0", "aa1", "aa2", "aa3", "aa4", "aa5", "aa6", "aa7", "aa8", "aa9", "aa", "xx",
        ]
        .into_iter()
        .map(String::from)
        .collect::<Vec<_>>();

        let reader = make_index(&mut random, &terms)?;

        // First term in first block:
        assert_eq!(1, doc_freq(&reader, "aa0")?);
        assert_eq!(1, doc_freq(&reader, "aa4")?);

        // No block
        assert_eq!(0, doc_freq(&reader, "bb0")?);

        // Second block
        assert_eq!(1, doc_freq(&reader, "aa4")?);

        // Backwards to prior floor block:
        assert_eq!(1, doc_freq(&reader, "aa0")?);

        // Forwards to last floor block:
        assert_eq!(1, doc_freq(&reader, "aa9")?);

        assert_eq!(0, doc_freq(&reader, "a")?);
        assert_eq!(1, doc_freq(&reader, "aa")?);
        assert_eq!(0, doc_freq(&reader, "a")?);
        assert_eq!(1, doc_freq(&reader, "aa")?);

        // Forwards to last floor block:
        assert_eq!(1, doc_freq(&reader, "xx")?);
        assert_eq!(1, doc_freq(&reader, "aa1")?);
        assert_eq!(0, doc_freq(&reader, "yy")?);

        assert_eq!(1, doc_freq(&reader, "xx")?);
        assert_eq!(1, doc_freq(&reader, "aa9")?);

        assert_eq!(1, doc_freq(&reader, "xx")?);
        assert_eq!(1, doc_freq(&reader, "aa4")?);

        let terms_enum = get_terms(&reader, FIELD)?.unwrap().iterator()?;
        let mut te = terms_enum;

        while te.next()?.is_some() {
            // iterate all terms
        }

        assert!(seek_exact(&mut random, &mut te, "aa1")?);
        assert_eq!(Some("aa2".to_string()), next_term(&mut te)?);

        assert!(seek_exact(&mut random, &mut te, "aa8")?);
        assert_eq!(Some("aa9".to_string()), next_term(&mut te)?);
        assert_eq!(Some("xx".to_string()), next_term(&mut te)?);

        // test_random_seeks(&mut random, &reader, &terms)?;
        Ok(())
    }
    fn seek_exact<R: Rng + ?Sized>(
        random: &mut R,
        te: &mut impl TermsEnum,
        term: &str,
    ) -> Result<bool> {
        te.seek_exact(&new_bytes_ref_from_string(random, term)?)
    }
    fn next_term(te: &mut impl TermsEnum) -> Result<Option<String>> {
        match te.next()? {
            Some(br) => Ok(Some(br.utf8_to_string()?)),
            None => Ok(None),
        }
    }

    fn get_non_exist_term<R: Rng + ?Sized>(
        random: &mut R,
        terms: &[BytesRef<Vec<u8>>],
    ) -> Result<BytesRef<Vec<u8>>> {
        loop {
            let ts = get_random_string(random);
            let t = new_bytes_ref_from_string(random, &ts)?;
            if terms.binary_search(&t).is_err() {
                return Ok(t);
            }
        }
    }
    struct TermAndState<TS>
    where
        TS: TermState,
    {
        term: BytesRef<Vec<u8>>,
        state: Option<TS>,
    }
    // TODO
    // fn test_random_seeks<R: Rng + ?Sized,CR>(
    //     random: &mut impl Rng,
    //     reader: CR,
    //     valid_term_strings: &[String],
    // ) -> Result<()>
    // where
    //     CR: CompositeReader,
    // {
    //     let mut valid_terms: Vec<BytesRef<Vec<u8>>> = valid_term_strings
    //         .iter()
    //         .map(|s| new_bytes_ref_from_string(random, s.as_str()))
    //         .collect::<Result<_>>()?;
    //     valid_terms.sort();
    //
    //     let mut te = get_terms(reader, FIELD)?
    //         .unwrap()
    //         .iterator()?;
    //
    //     let end_loc: isize = -(valid_terms.len() as isize) - 1;
    //
    //     let mut term_states = Vec::new();
    //
    //     for _iter in 0..(100 * random_multiplier()) {
    //         let (t, mut loc, term_state) =
    //             if random.random_range(0..6) == 4 {
    //                 // pick non-existing term
    //                 let t = get_non_exist_term(random, &valid_terms)?;
    //                 let loc = match valid_terms.binary_search(&t) {
    //                     Ok(p) => p as isize,
    //                     Err(p) => -(p as isize) - 1,
    //                 };
    //                 (t, loc, None)
    //             } else if !term_states.is_empty() && random.random_range(0..4) == 1 {
    //                 let (t, st) = term_states[random.random_range(0..term_states.len())].clone();
    //                 let loc = valid_terms.binary_search(&t).unwrap() as isize;
    //
    //                 (t, loc, Some(st))
    //             } else {
    //                 // pick valid term
    //                 let idx = random.random_range(0..valid_terms.len());
    //                 let t = valid_terms[idx].clone();
    //                 (t, idx as isize, None)
    //             };
    //
    //         // seekExact or seekCeil
    //         let do_seek_exact = random.random_bool(0.5);
    //         if let Some(state) = term_state {
    //             te.seek_exact_with_state(&t, &state)?;
    //         } else if do_seek_exact {
    //             assert_eq!(loc >= 0, te.seek_exact(&t)?);
    //         } else {
    //             let result = te.seek_ceil(&t)?;
    //
    //             if loc >= 0 {
    //                 assert_eq!(SeekStatus::Found, result);
    //             } else if loc == end_loc {
    //                 assert_eq!(SeekStatus::End, result);
    //             } else {
    //                 assert!(loc >= -(valid_terms.len() as isize));
    //                 assert_eq!(SeekStatus::NotFound, result);
    //             }
    //         }
    //
    //         // validate positioning
    //         if loc >= 0 {
    //             assert_eq!(&t, te.term()?.unwrap());
    //         } else if do_seek_exact {
    //             continue;
    //         } else if loc == end_loc {
    //             continue;
    //         } else {
    //             loc = -loc - 1;
    //             assert_eq!(&valid_terms[loc as usize], te.term()?.unwrap());
    //         }
    //
    //         // do a bunch of next()
    //         let num_next = random.random_range(0..valid_terms.len());
    //         for _ in 0..num_next {
    //             if VERBOSE {
    //                 println!(
    //                     "\nTEST: next loc={} of {}",
    //                     loc,
    //                     valid_terms.len()
    //                 );
    //             }
    //             let t2 = te.next()?;
    //             loc += 1;
    //             if loc as usize == valid_terms.len() {
    //                 assert!(t2.is_none());
    //                 break;
    //             } else {
    //                 assert_eq!(
    //                     &valid_terms[loc as usize],
    //                     t2.as_ref().unwrap()
    //                 );
    //                 if random.random_range(0..40) == 17 && term_states.len() < 100 {
    //                     term_states.push((
    //                         valid_terms[loc as usize].clone(),
    //                         te.term_state()?,
    //                     ));
    //                 }
    //             }
    //         }
    //     }
    //
    //     Ok(())
    // }

    #[test]
    fn test_zero_terms() -> Result<()> {
        let mut random = random();
        let dir = new_directory_shared(&mut random)?;
        let writer = RandomIndexWriter::new(&mut random, dir.clone());

        let mut doc = Document::new();
        let mut field_to_type = HashMap::new();
        doc.add(new_text_field(
            "field",
            "one two three",
            No,
            &mut field_to_type,
        )?);

        // doc with "field2"
        let mut doc = Document::new();
        doc.add(new_text_field(
            "field2",
            "one two three",
            No,
            &mut field_to_type,
        )?);
        writer.add_document(doc)?;

        writer.commit()?;
        writer.delete_documents_with_terms(vec![Term::from_text("field", "one")])?;
        // TODO force_merge未实现
        // writer.force_merge(1)?;

        let reader = writer.get_reader()?;
        writer.close()?;

        assert_eq!(1, reader.num_docs()?);
        assert_eq!(1, reader.max_doc()?);

        if let Some(terms) = get_terms(&reader, "field")? {
            let mut te = terms.iterator()?;
            assert!(te.next()?.is_none());
        }

        Ok(())
    }
    fn get_random_string<R: Rng + ?Sized>(random: &mut R) -> String {
        TestUtil::random_realistic_unicode_string(random)
    }
    #[test]
    fn test_random_terms() -> Result<()> {
        // TODO test_random_seeks未实现
        Ok(())
    }
    #[test]
    fn test_intersect_basic() -> Result<()> {
        let mut random = random();
        let dir = new_directory_shared(&mut random)?;

        // TODO: 未实现MockAnalyzer/LogDocMergePolicy
        let iwc = IndexWriterConfig::new();
        let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

        let mut field_to_type = HashMap::new();
        let mut doc = Document::new();
        doc.add(new_text_field("field", "aaa", No, &mut field_to_type)?);
        writer.add_document(doc)?;

        let mut doc = Document::new();
        doc.add(new_text_field("field", "bbb", No, &mut field_to_type)?);
        writer.add_document(doc)?;

        let mut doc = Document::new();
        doc.add(new_text_field("field", "ccc", No, &mut field_to_type)?);
        writer.add_document(doc)?;

        // TODO: force_merge
        // writer.force_merge(1)?;

        let reader = writer.get_reader()?;
        writer.close()?;

        let sub = get_only_leaf_reader(&reader)?;
        let terms = sub.terms("field")?.expect("terms must exist");

        let automaton = RegExp::from_str_with_flags(".*", RegExp::NONE)?.to_automaton()?;
        let mut ca = CompiledAutomaton::new(automaton, false, false)?;

        let mut te = terms.intersect(&mut ca, None)?;
        assert_eq!("aaa", te.next()?.unwrap().utf8_to_string()?);
        assert_eq!(0, te.postings_with_flags(None, NONE.into())?.next_doc()?);
        assert_eq!("bbb", te.next()?.unwrap().utf8_to_string()?);
        assert_eq!(1, te.postings_with_flags(None, NONE.into())?.next_doc()?);
        assert_eq!("ccc", te.next()?.unwrap().utf8_to_string()?);
        assert_eq!(2, te.postings_with_flags(None, NONE.into())?.next_doc()?);
        assert!(te.next()?.is_none());

        let mut te = terms.intersect(&mut ca, Some(&BytesRef::from_string("abc")))?;
        assert_eq!("bbb", te.next()?.unwrap().utf8_to_string()?);
        assert_eq!(1, te.postings_with_flags(None, NONE.into())?.next_doc()?);
        assert_eq!("ccc", te.next()?.unwrap().utf8_to_string()?);
        assert_eq!(2, te.postings_with_flags(None, NONE.into())?.next_doc()?);
        assert!(te.next()?.is_none());

        let mut te = terms.intersect(&mut ca, Some(&BytesRef::from_string("aaa")))?;
        assert_eq!("bbb", te.next()?.unwrap().utf8_to_string()?);
        assert_eq!(1, te.postings_with_flags(None, NONE.into())?.next_doc()?);
        assert_eq!("ccc", te.next()?.unwrap().utf8_to_string()?);
        assert_eq!(2, te.postings_with_flags(None, NONE.into())?.next_doc()?);
        assert!(te.next()?.is_none());
        Ok(())
    }
    #[test]
    fn test_intersect_start_term() -> Result<()> {
        let mut random = random();
        let dir = new_directory_shared(&mut random)?;

        // TODO: 未实现 MockAnalyzer / LogDocMergePolicy
        let iwc = IndexWriterConfig::new();
        let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

        let mut field_to_type = HashMap::new();

        let mut doc = Document::new();
        doc.add(new_string_field("field", "abc", No, &mut field_to_type)?);
        writer.add_document(doc)?;

        let mut doc = Document::new();
        doc.add(new_string_field("field", "abd", No, &mut field_to_type)?);
        writer.add_document(doc)?;

        let mut doc = Document::new();
        doc.add(new_string_field("field", "acd", No, &mut field_to_type)?);
        writer.add_document(doc)?;

        let mut doc = Document::new();
        doc.add(new_string_field("field", "bcd", No, &mut field_to_type)?);
        writer.add_document(doc)?;

        // TODO: force_merge
        // writer.force_merge(1)?;

        let reader = writer.get_reader()?;
        writer.close()?;

        let sub = get_only_leaf_reader(&reader)?;
        let terms = sub.terms("field")?.expect("terms must exist");

        let automaton = RegExp::from_str_with_flags(".*d", RegExp::NONE)?.to_automaton()?;
        let v = match Operations::determinize(
            &automaton,
            Operations::DEFAULT_DETERMINIZE_WORK_LIMIT,
        )? {
            Cow::Borrowed(_) => automaton,
            Cow::Owned(v) => v,
        };

        let mut ca = CompiledAutomaton::new(v, false, false)?;

        // should seek to startTerm
        let mut te = terms.intersect(&mut ca, Some(&BytesRef::from_string("aad")))?;
        assert_eq!("abd", te.next()?.unwrap().utf8_to_string()?);
        assert_eq!(1, te.postings_with_flags(None, NONE.into())?.next_doc()?);
        assert_eq!("acd", te.next()?.unwrap().utf8_to_string()?);
        assert_eq!(2, te.postings_with_flags(None, NONE.into())?.next_doc()?);
        assert_eq!("bcd", te.next()?.unwrap().utf8_to_string()?);
        assert_eq!(3, te.postings_with_flags(None, NONE.into())?.next_doc()?);
        assert!(te.next()?.is_none());

        // should fail to find ceil label on second arc, rewind
        let mut te = terms.intersect(&mut ca, Some(&BytesRef::from_string("add")))?;
        assert_eq!("bcd", te.next()?.unwrap().utf8_to_string()?);
        assert_eq!(3, te.postings_with_flags(None, NONE.into())?.next_doc()?);
        assert!(te.next()?.is_none());

        // should reach end
        let mut te = terms.intersect(&mut ca, Some(&BytesRef::from_string("bcd")))?;
        assert!(te.next()?.is_none());

        let mut te = terms.intersect(&mut ca, Some(&BytesRef::from_string("ddd")))?;
        assert!(te.next()?.is_none());

        Ok(())
    }
    #[test]
    fn test_intersect_empty_string() -> Result<()> {
        let mut random = random();
        let dir = new_directory_shared(&mut random)?;

        // TODO: 未实现 MockAnalyzer / LogDocMergePolicy
        let iwc = IndexWriterConfig::new();
        let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

        let mut field_to_type = HashMap::new();

        let mut doc = Document::new();
        doc.add(new_string_field("field", "", No, &mut field_to_type)?);
        doc.add(new_string_field("field", "abc", No, &mut field_to_type)?);
        writer.add_document(doc)?;

        let mut doc = Document::new();
        doc.add(new_string_field("field", "abc", No, &mut field_to_type)?);
        doc.add(new_string_field("field", "", No, &mut field_to_type)?);
        writer.add_document(doc)?;

        // TODO: force_merge
        // writer.force_merge(1)?;

        let reader = writer.get_reader()?;
        writer.close()?;

        let sub = get_only_leaf_reader(&reader)?;
        let terms = sub.terms("field")?.expect("terms must exist");

        let automaton = RegExp::from_str_with_flags(".*", RegExp::NONE)?.to_automaton()?;
        let mut ca = CompiledAutomaton::new(automaton, false, false)?;

        let mut te = terms.intersect(&mut ca, None)?;
        let mut de;

        assert_eq!("", te.next()?.unwrap().utf8_to_string()?);
        de = te.postings_with_flags(None, NONE.into())?;
        assert_eq!(0, de.next_doc()?);
        assert_eq!(1, de.next_doc()?);

        assert_eq!("abc", te.next()?.unwrap().utf8_to_string()?);
        de = te.postings_with_flags(None, NONE.into())?;
        assert_eq!(0, de.next_doc()?);
        assert_eq!(1, de.next_doc()?);

        assert!(te.next()?.is_none());

        // pass empty string as start term
        let mut te = terms.intersect(&mut ca, Some(&BytesRef::from_string("")))?;
        assert_eq!("abc", te.next()?.unwrap().utf8_to_string()?);
        de = te.postings_with_flags(None, NONE.into())?;
        assert_eq!(0, de.next_doc()?);
        assert_eq!(1, de.next_doc()?);

        assert!(te.next()?.is_none());

        Ok(())
    }
    #[test]
    fn test_common_prefix_terms() -> Result<()> {
        // TODO PerThreadPKLookup未实现
        Ok(())
    }
    #[test]
    fn test_varying_terms_per_segment() -> Result<()> {
        // TODO
        Ok(())
    }
    #[test]
    fn test_intersect_regexp() -> Result<()> {
        let mut random = random();
        let dir = new_directory_shared(&mut random)?;
        let writer = RandomIndexWriter::new(&mut random, dir.clone());

        let mut field_to_type = HashMap::new();
        let mut doc = Document::new();
        doc.add(new_string_field("field", "foobar", No, &mut field_to_type)?);
        writer.add_document(doc)?;

        let reader = writer.get_reader()?;
        let terms = get_terms(&reader, "field")?.expect("terms must exist");

        let automaton = RegExp::from_string("do_not_match_anything")?.to_automaton()?;
        let mut ca = CompiledAutomaton::from_automaton(automaton)?;

        let err = terms.intersect(&mut ca, None);
        assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
        if let Err(LuceneError::IllegalArgument(msg)) = err {
            assert_eq!(
                "please use CompiledAutomaton.getTermsEnum instead",
                msg.to_string()
            );
        }

        Ok(())
    }
    #[test]
    fn test_invalid_automaton_terms_enum() -> Result<()> {
        let automaton = Automata::make_string("foo")?;
        let mut ca = CompiledAutomaton::from_automaton(automaton)?;

        let err = AutomatonTermsEnum::new(EmptyTermsEnum, &mut ca);
        assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
        Ok(())
    }
}

#[cfg(test)]
mod tests2 {
    use crate::core::document::document::Document;
    use crate::core::document::field::Store::Yes;
    use crate::core::index::BytesRef;
    use crate::core::index::composite_reader_context::CompositeReaderContext;
    use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
    use crate::core::index::multi_terms::get_terms;
    use crate::core::index::standard_directory_reader::StandardDirectoryReaderType;
    use crate::core::index::terms::Terms;
    use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
    use crate::core::search::index_searcher::DefaultIndexSearcher;
    use crate::core::util::automation::automata::Automata;
    use crate::core::util::automation::automaton::Automaton;
    use crate::core::util::automation::compiled_automaton::CompiledAutomaton;
    use crate::core::util::automation::operations::Operations;
    use crate::core::util::automation::reg_exp::RegExp;
    use crate::core::util::bytes_ref_iterator::BytesRefIterator;
    use crate::core::util::error::lucene_error::Result;
    use crate::test::index::random_index_writer::RandomIndexWriter;
    use crate::test::util::automaton::automaton_test_util::AutomatonTestUtil;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{
        DirType, at_least, new_directory_shared, new_index_writer_config, new_searcher_with_reader,
        new_string_field, random,
    };
    use crate::test::util::test_util::TestUtil;
    use rand::Rng;
    use rand::prelude::SliceRandom;
    use std::borrow::Cow;
    use std::collections::{BTreeSet, HashMap};
    use std::sync::Arc;

    #[allow(dead_code)] // for quick search
    struct TestTermsEnum2;

    #[allow(clippy::type_complexity)]
    fn set_up<R: Rng + ?Sized>(
        random: &mut R,
    ) -> Result<(
        i32,
        Arc<DirType>,
        BTreeSet<BytesRef<Vec<u8>>>,
        Automaton,
        Arc<StandardDirectoryReaderType<DirType>>,
        DefaultIndexSearcher<CompositeReaderContext<Arc<StandardDirectoryReaderType<DirType>>>>,
    )> {
        let num_iterations = at_least(random, 50);

        let dir = new_directory_shared(random)?;

        // TODO: 未实现 MockAnalyzer / LogDocMergePolicy
        let mut iwc = new_index_writer_config(random);
        iwc.set_max_buffered_docs(TestUtil::next_int(random, 50, 1000));
        let writer = RandomIndexWriter::with_config(random, dir.clone(), iwc);

        let mut doc = Document::new();
        let mut field = new_string_field("field", "", Yes, &mut HashMap::new())?;
        doc.add(field.clone());

        let mut terms: BTreeSet<BytesRef<Vec<u8>>> = BTreeSet::new();

        let num = at_least(random, 200);
        for _i in 0..num {
            let s = TestUtil::random_unicode_string(random);
            field.set_string_value(&s)?;
            terms.insert(BytesRef::from_string(&s));
            let mut doc = Document::new();
            doc.add(field.clone());
            writer.add_document(doc)?;
        }
        let v: Vec<BytesRef<Vec<u8>>> = terms.iter().cloned().collect();
        let terms_automaton = Automata::make_string_union(v.as_slice())?;

        let reader = Arc::new(writer.get_reader()?);
        let searcher = new_searcher_with_reader(reader.clone())?;
        writer.close()?;

        Ok((
            num_iterations,
            dir.clone(),
            terms,
            terms_automaton,
            reader,
            searcher,
        ))
    }
    #[test]
    fn test_finite_versus_infinite() -> Result<()> {
        // TODO AutomatonQuery未实现
        Ok(())
    }
    #[test]
    fn test_seeking() -> Result<()> {
        let mut random = random();
        let (num_iterations, _dir, terms, _terms_automaton, reader, _searcher) =
            set_up(&mut random)?;

        for _ in 0..num_iterations {
            let reg = AutomatonTestUtil::random_regexp(&mut random)?;
            let automaton = RegExp::from_str_with_flags(&reg, RegExp::NONE)?.to_automaton()?;
            let vv =
                Operations::determinize(&automaton, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;
            let v = match vv {
                Cow::Borrowed(_) => automaton,
                Cow::Owned(v1) => v1,
            };

            let mut te = get_terms(&reader, "field")?.unwrap().iterator()?;

            let mut unsorted_terms: Vec<&BytesRef<Vec<u8>>> = terms.iter().collect();
            unsorted_terms.shuffle(&mut random);

            for term in unsorted_terms {
                if Operations::run_str(&v, &term.utf8_to_string()?) {
                    if random.random_bool(0.5) {
                        assert!(te.seek_exact(term)?);
                    } else {
                        let status = te.seek_ceil(term)?;
                        assert_eq!(SeekStatus::Found, status);
                        assert_eq!(term, te.term()?.as_ref());
                    }
                }
            }
        }

        Ok(())
    }
    #[test]
    fn test_seeking_and_nexting() -> Result<()> {
        let mut random = random();
        let (num_iterations, _dir, terms, _terms_automaton, reader, _searcher) =
            set_up(&mut random)?;

        for _ in 0..num_iterations {
            let mut te = get_terms(&reader, "field")?.unwrap().iterator()?;

            for term in terms.iter() {
                let c = random.random_range(0..3);
                if c == 0 {
                    assert_eq!(term, te.next()?.unwrap().as_ref());
                } else if c == 1 {
                    let status = te.seek_ceil(term)?;
                    assert_eq!(SeekStatus::Found, status);
                    assert_eq!(term, te.term()?.as_ref());
                } else {
                    assert!(te.seek_exact(term)?);
                }
            }
        }

        Ok(())
    }
    // TODO IMPORTANT 测试未通过
    fn test_intersect() -> Result<()> {
        let mut random = random();
        let (num_iterations, _dir, _terms, terms_automaton, reader, _searcher) =
            set_up(&mut random)?;

        for _ in 0..num_iterations {
            let reg = AutomatonTestUtil::random_regexp(&mut random)?;
            let automaton = RegExp::from_str_with_flags(&reg, RegExp::NONE)?.to_automaton()?;
            let automaton =
                Operations::determinize(&automaton, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?
                    .into_owned();

            let mut ca = CompiledAutomaton::new(automaton.clone(), false, false)?;

            let mut te = get_terms(&reader, "field")?
                .unwrap()
                .intersect(&mut ca, None)?;
            let v = Operations::intersection(&terms_automaton, &automaton)?.into_owned();
            let expected =
                match Operations::determinize(&v, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)? {
                    Cow::Borrowed(_) => v,
                    Cow::Owned(v1) => v1,
                };
            let mut found: BTreeSet<BytesRef<Vec<u8>>> = BTreeSet::new();
            while let Some(term) = te.next()? {
                found.insert(BytesRef::deep_copy_of(&term));
            }

            let v: Vec<BytesRef<Vec<u8>>> = found.iter().cloned().collect();
            let actual = Operations::determinize(
                &Automata::make_string_union(v.as_slice())?,
                Operations::DEFAULT_DETERMINIZE_WORK_LIMIT,
            )?
            .into_owned();

            assert!(AutomatonTestUtil::same_language(&expected, &actual)?);
        }

        Ok(())
    }
}
