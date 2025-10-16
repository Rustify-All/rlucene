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
use crate::core::search::query::Query;
use crate::core::util::error::lucene_error::Result;
use std::sync::Arc;
/// An iterator over match positions (and optionally offsets) for a single document and field.
///
/// To iterate over the matches, call [`MatchesIterator::next`] until it returns `false`,
/// retrieving positions and/or offsets after each call. You should not call the position
/// or offset methods before `next()` has been called, or after `next()` has returned `false`.
///
/// Matches from some queries may span multiple positions. You can retrieve the positions of
/// individual matching terms on the current match by calling [`MatchesIterator::get_sub_matches`].
///
/// Matches are ordered by start position, and then by end position. Match intervals may overlap.
///
/// @lucene.experimental
pub trait MatchesIterator {
    /// Advance the iterator to the next match position.
    ///
    /// # Returns
    /// - `Ok(true)` if matches have not been exhausted
    /// - `Ok(false)` if no more matches
    fn next(&mut self) -> Result<bool>;

    /// The start position of the current match, or `-1` if positions are not available.
    ///
    /// Should only be called after [`MatchesIterator::next`] has returned `true`.
    fn start_position(&self) -> Result<i32>;

    /// The end position of the current match, or `-1` if positions are not available.
    ///
    /// Should only be called after [`MatchesIterator::next`] has returned `true`.
    fn end_position(&self) -> i32;

    /// The starting offset of the current match, or `-1` if offsets are not available.
    ///
    /// Should only be called after [`MatchesIterator::next`] has returned `true`.
    fn start_offset(&self) -> Result<i32>;

    /// The ending offset of the current match, or `-1` if offsets are not available.
    ///
    /// Should only be called after [`MatchesIterator::next`] has returned `true`.
    fn end_offset(&self) -> Result<i32>;

    type MatchesIterRef<'a>: MatchesIterator
    where
        Self: 'a;
    /// Returns a [`MatchesIterator`] that iterates over the positions and offsets
    /// of individual terms within the current match.
    ///
    /// Returns `None` if there are no submatches (i.e. the current iterator is at the leaf level).
    ///
    /// Should only be called after [`MatchesIterator::next`] has returned `true`.
    fn get_sub_matches(&mut self) -> Result<Option<Self::MatchesIterRef<'_>>>;

    /// Returns the [`Query`] causing the current match.
    ///
    /// If this [`MatchesIterator`] has been returned from a [`MatchesIterator::get_sub_matches`] call,
    /// then returns a `TermQuery` equivalent to the current match.
    ///
    /// Should only be called after [`MatchesIterator::next`] has returned `true`.
    fn get_query(&self) -> Arc<Query>;
}
macro_rules! either_matches_iterator {
    (
        $vis:vis $name:ident
        => { sub: $sub_mi:ident }
        { $( $Variant:ident : $T:ident ),+ $(,)? }
    ) => {
        $vis enum $name<$( $T ),+> {
            $( $Variant($T), )+
        }

        impl<$( $T ),+> MatchesIterator for $name<$( $T ),+>
        where
            $( $T: MatchesIterator ),+
        {
            #[inline]
            fn next(&mut self) -> Result<bool> {
                match self { $( Self::$Variant(inner) => inner.next(), )+ }
            }

            #[inline]
            fn start_position(&self) -> Result<i32> {
                match self { $( Self::$Variant(inner) => inner.start_position(), )+ }
            }

            #[inline]
            fn end_position(&self) -> i32 {
                match self { $( Self::$Variant(inner) => inner.end_position(), )+ }
            }

            #[inline]
            fn start_offset(&self) -> Result<i32> {
                match self { $( Self::$Variant(inner) => inner.start_offset(), )+ }
            }

            #[inline]
            fn end_offset(&self) -> Result<i32> {
                match self { $( Self::$Variant(inner) => inner.end_offset(), )+ }
            }

            type MatchesIterRef<'a> =
                $sub_mi<$( <$T as MatchesIterator>::MatchesIterRef<'a> ),+>
            where
                Self: 'a;

            #[inline]
            fn get_sub_matches(&mut self) -> Result<Option<Self::MatchesIterRef<'_>>> {
                match self {
                    $(
                        Self::$Variant(inner) => {
                            let opt = inner.get_sub_matches()?;
                            Ok(opt.map($sub_mi::$Variant))
                        }
                    ),+
                }
            }

            #[inline]
            fn get_query(&self) -> Arc<Query> {
                match self { $( Self::$Variant(inner) => inner.get_query(), )+ }
            }
        }
    };
}
either_matches_iterator!(
    pub Either2MatchesIterator
    => { sub: Either2MatchesIterator }
    { A: A, B: B }
);
