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

use crate::core::index::automaton_terms_enum::AutomatonTermsEnum;
use crate::core::index::base_terms_enum::BaseTermsEnum;
use crate::core::index::filtered_terms_enum::{FilteredTermsEnum, FilteredTermsEnumBase};
use crate::core::index::terms_enum::{Either2TermsEnum, EmptyTermsEnum, SeekStatus, TermsEnum};
use crate::core::index::{BytesRef, BytesRefBuilder};
use crate::core::util::automation::compiled_automaton::CompiledAutomaton;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::error::lucene_error::Result;

/// Trait representing base term statistics and access.
pub trait Terms {
    type TermsEnum: TermsEnum;
    /// Returns an iterator that will step through all terms. This method will
    /// not return None.
    fn iterator(&self) -> Result<Self::TermsEnum>;

    type IntersectIter: TermsEnum;
    /// Returns a [`TermsEnum`] that iterates over all terms and documents
    /// accepted by the given [`CompiledAutomaton`].
    ///
    /// If `start_term` is provided, the returned enum will only return terms
    /// strictly greater than `start_term`, but you must still call `next()`
    /// first to advance to the first term. The provided `start_term` must
    /// be accepted by the automaton.
    ///
    /// This is an expert-level, low-level API that only works for
    /// [`AutomatonType::NORMAL`](crate::core::util::automation::compiled_automaton::AutomatonType::Normal) compiled automata. To handle any type of
    /// compiled automaton, use
    /// [`CompiledAutomaton::get_terms_enum`](CompiledAutomaton::get_byte_runnable)
    /// instead.
    ///
    /// **Note**: The returned `TermsEnum` does **not** support seeking.
    fn intersect(
        &self,
        compiled: &mut CompiledAutomaton,
        start_term: Option<BytesRef<Vec<u8>>>,
    ) -> Result<Self::IntersectIter>;

    fn default_intersect(
        &self,
        compiled: &mut CompiledAutomaton,
        start_term: Option<BytesRef<Vec<u8>>>,
    ) -> Result<FilteredTermsEnum<Self::TermsEnum, AutomatonTermsEnum>>
    where
        Self: Sized,
    {
        let terms_enum = self.iterator()?;
        let automaton_terms_enum = if start_term.is_some() {
            AutomatonTermsEnum::with_start_term(compiled, start_term)?
        } else {
            AutomatonTermsEnum::new(compiled)?
        };
        Ok(FilteredTermsEnum::new(terms_enum, automaton_terms_enum))
    }
    /// Returns the number of terms for this field, or `-1` if this measure
    /// isn't stored by the codec.
    ///
    /// Note that, like other term measures, this value does **not** take
    /// deleted documents into account.
    fn size(&self) -> Result<i64>;

    /// Returns the sum of
    /// [`TermsEnum::total_term_freq`]
    /// for all terms in this field. Note that, like other term measures,
    /// this value does **not** take deleted documents into account.
    fn get_sum_total_term_freq(&self) -> Result<i64>;

    /// Returns the sum of
    /// [`TermsEnum::doc_freq`]
    /// for all terms in this field. Note that, like other term measures,
    /// this value does **not** take deleted documents into account.
    fn get_sum_doc_freq(&self) -> Result<i64>;

    /// Returns the number of documents that have at least one term for this
    /// field. Note that, like other term measures, this value does **not**
    /// take deleted documents into account.
    fn get_doc_count(&self) -> Result<i32>;

    /// Returns `true` if documents in this field store per-document term
    /// frequency
    /// (see [`PostingsEnum::freq`](crate::core::index::postings_enum::PostingsEnum::freq)).
    fn has_freqs(&self) -> bool;

    /// Returns true if documents in this field store offsets.
    fn has_offsets(&self) -> bool;

    /// Returns true if documents in this field store positions.
    fn has_positions(&self) -> bool;

    /// Returns true if documents in this field store payloads.
    fn has_payloads(&self) -> bool;

    /// Returns the smallest term (in lexicographic order) in the field.  
    /// Note that, like other term measures, this does **not** take deleted
    /// documents into account. Returns `None` when there are no terms.
    fn get_min<'a, T>(&'a self, iterator: &'a mut T) -> Result<Option<Cow<'a, BytesRef<Vec<u8>>>>>
    where
        T: TermsEnum,
    {
        iterator.next()
    }

    /// Returns the largest term (in lexicographic order) in the field.  
    /// Note that, like other term measures, this does **not** take deleted
    /// documents into account. Returns `None` when there are no terms.
    fn get_max<'a, T>(&'a self, iterator: &'a mut T) -> Result<Option<Cow<'a, BytesRef<Vec<u8>>>>>
    where
        T: TermsEnum,
    {
        let size = self.size()?;
        match size.cmp(&0) {
            std::cmp::Ordering::Equal => return Ok(None),
            std::cmp::Ordering::Greater => {
                iterator.seek_exact_with_ord(size - 1)?;
                return Ok(Some(iterator.term()?));
            },
            std::cmp::Ordering::Less => {},
        }
        // otherwise: binary search
        let mut iterator = self.iterator()?;
        let v = iterator.next()?;
        if v.is_none() {
            return Ok(None);
        }

        let mut scratch = BytesRefBuilder::new();
        scratch.append_byte(0);
        // Iterates over digits:
        loop {
            let mut low = 0;
            let mut high = 256;
            // Binary search current digit to find the highest
            // digit before END:
            while low != high {
                let mid = (((low + high) as u32) >> 1) as i32;
                scratch.set_byte_at(scratch.length() - 1, mid as u8);
                match iterator.seek_ceil(scratch.get_bytes_mut_ref())? {
                    SeekStatus::End => {
                        if mid == 0 {
                            scratch.set_length(scratch.length() - 1);
                            return Ok(Some(Cow::Owned(scratch.get_bytes_owner())));
                        }
                        high = mid;
                    },
                    _ => {
                        if low == mid {
                            break;
                        }
                        low = mid;
                    },
                }
            }

            scratch.set_length(scratch.length() + 1);
            scratch.grow(scratch.length());
        }
    }

    /// Returns debugging statistics string.
    fn get_stats(&self) -> Result<String> {
        Ok(format!(
            "impl={},size={},docCount={},sumTotalTermFreq={},sumDocFreq={}",
            std::any::type_name::<Self>(),
            self.size()?,
            self.get_doc_count()?,
            self.get_sum_total_term_freq()?,
            self.get_sum_doc_freq()?
        ))
    }
}
pub mod terms_util {
    use crate::core::index::leaf_reader::LeafReader;
    use crate::core::index::terms::{EitherEmptyTerms, EmptyTerms};
    use crate::core::util::error::lucene_error::Result;

    /// Returns the [`Terms`] index for this field, or [`crate::core::index::terms::Terms::EMPTY`] if it
    /// has none.
    ///
    /// Returns:
    /// - A `Terms` instance, or an empty instance if the field does not exist
    ///   in this reader.
    ///
    /// Errors:
    /// - Returns an error if an I/O error occurs.
    pub(crate) fn get_terms<LR>(reader: &LR, field: &str) -> Result<EitherEmptyTerms<LR::Terms>>
    where
        LR: LeafReader,
    {
        let terms = reader.terms(field)?;
        match terms {
            Some(t) => Ok(EitherEmptyTerms::A(t)),
            None => Ok(EitherEmptyTerms::B(EmptyTerms)),
        }
    }
}
pub type EitherEmptyTerms<T> = Either2Terms<T, EmptyTerms>;

#[derive(Default)]
pub struct EmptyTerms;
impl Terms for EmptyTerms {
    type TermsEnum = BaseTermsEnum<EmptyTermsEnum>;

    fn iterator(&self) -> Result<Self::TermsEnum> {
        Ok(EmptyTermsEnum.into())
    }

    type IntersectIter
        = FilteredTermsEnum<Self::TermsEnum, AutomatonTermsEnum>
    where
        Self::TermsEnum: BytesRefIterator,
        AutomatonTermsEnum: FilteredTermsEnumBase;

    fn intersect(
        &self,
        compiled: &mut CompiledAutomaton,
        start_term: Option<BytesRef<Vec<u8>>>,
    ) -> Result<Self::IntersectIter> {
        self.default_intersect(compiled, start_term)
    }

    fn size(&self) -> Result<i64> {
        Ok(0)
    }

    fn get_sum_total_term_freq(&self) -> Result<i64> {
        Ok(0)
    }

    fn get_sum_doc_freq(&self) -> Result<i64> {
        Ok(0)
    }

    fn get_doc_count(&self) -> Result<i32> {
        Ok(0)
    }

    fn has_freqs(&self) -> bool {
        false
    }

    fn has_offsets(&self) -> bool {
        false
    }

    fn has_positions(&self) -> bool {
        false
    }

    fn has_payloads(&self) -> bool {
        false
    }
}

macro_rules! either_terms {
    ($vis:vis $name:ident => { te: $te:ident, ie: $ie:ident } { $( $Variant:ident : $T:ident ),+ $(,)? }) => {
        $vis enum $name<$( $T ),+> {
            $( $Variant($T), )+
        }

        impl<$( $T ),+> Terms for $name<$( $T ),+>
        where
            $( $T: Terms ),+
        {
            type TermsEnum     = $te<$( <$T as Terms>::TermsEnum ),+>;
            type IntersectIter = $ie<$( <$T as Terms>::IntersectIter ),+>;


            fn iterator(&self) -> Result<Self::TermsEnum> {
                match self {
                    $(
                        Self::$Variant(inner) => {
                            let it = inner.iterator()?;
                            Ok($te::$Variant(it))
                        }
                    ),+
                }
            }
            fn intersect(
                &self,
                ca: &mut CompiledAutomaton,
                start: Option<BytesRef<Vec<u8>>>
            ) -> Result<Self::IntersectIter> {
                match self {
                    $(
                        Self::$Variant(inner) => {
                            let it = inner.intersect(ca, start)?;
                            Ok($ie::$Variant(it))
                        }
                    ),+
                }
            }


            fn size(&self) -> Result<i64> {
                match self { $( Self::$Variant(inner) => inner.size(), )+ }
            }

            fn get_doc_count(&self) -> Result<i32> {
                match self { $( Self::$Variant(inner) => inner.get_doc_count(), )+ }
            }

            fn get_sum_doc_freq(&self) -> Result<i64> {
                match self { $( Self::$Variant(inner) => inner.get_sum_doc_freq(), )+ }
            }

            fn get_sum_total_term_freq(&self) -> Result<i64> {
                match self { $( Self::$Variant(inner) => inner.get_sum_total_term_freq(), )+ }
            }


            fn has_freqs(&self) -> bool {
                match self { $( Self::$Variant(inner) => inner.has_freqs(), )+ }
            }

            fn has_offsets(&self) -> bool {
                match self { $( Self::$Variant(inner) => inner.has_offsets(), )+ }
            }

            fn has_positions(&self) -> bool {
                match self { $( Self::$Variant(inner) => inner.has_positions(), )+ }
            }

            fn has_payloads(&self) -> bool {
                match self { $( Self::$Variant(inner) => inner.has_payloads(), )+ }
            }
        }
    };
}
either_terms!(
    pub Either2Terms
    => { te: Either2TermsEnum, ie: Either2TermsEnum }
    { A:A,B:B}
);
