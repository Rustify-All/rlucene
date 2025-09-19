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
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::util::error::lucene_error::Result;

pub trait TwoPhaseIterator {
    type DocIdSetIterator: DocIdSetIterator;

    /// Return the approximation [`DocIdSetIterator`].
    ///
    /// The returned iterator must advance synchronously with this
    /// `TwoPhaseIterator`.
    fn approximation_mut(&mut self) -> &mut Self::DocIdSetIterator;
    fn approximation(&self) -> &Self::DocIdSetIterator;
    fn take_approximation(&mut self) -> Self::DocIdSetIterator;

    /// Set the approximation to an empty iterator
    fn set_empty(&mut self);

    /// Return whether the current doc ID that `approximation()` is on matches.
    ///
    /// This method should only be called when the iterator is positioned
    /// (i.e. not when `doc_id()` is `-1` or `NO_MORE_DOCS`) and at most once.
    ///
    /// # Errors
    /// Returns an error if an I/O error occurs.
    fn matches(&mut self) -> Result<bool>;

    /// An estimate of the expected cost to determine that a single
    /// document matches.
    ///
    /// This can be called before iterating the documents of
    /// `approximation()`. Returns an expected cost in number of simple
    /// operations (add, multiply, compare, array index). Must be positive.
    fn match_cost(&self) -> f32;
}

pub struct TwoPhaseIteratorAsDocIdSetIterator<TPI>
where
    TPI: TwoPhaseIterator,
{
    pub(crate) two_phase_iterator: TPI,
}

impl<TPI> TwoPhaseIteratorAsDocIdSetIterator<TPI>
where
    TPI: TwoPhaseIterator,
{
    pub fn new(two_phase_iterator: TPI) -> Self {
        Self { two_phase_iterator }
    }

    fn do_next(&mut self, mut doc: i32) -> Result<i32> {
        loop {
            if doc == NO_MORE_DOCS {
                return Ok(NO_MORE_DOCS);
            } else if self.two_phase_iterator.matches()? {
                return Ok(doc);
            }
            doc = self.two_phase_iterator.approximation_mut().next_doc()?;
        }
    }
}

impl<TPI> DocIdSetIterator for TwoPhaseIteratorAsDocIdSetIterator<TPI>
where
    TPI: TwoPhaseIterator,
{
    fn doc_id(&self) -> i32 {
        self.two_phase_iterator.approximation().doc_id()
    }

    fn next_doc(&mut self) -> Result<i32> {
        let doc = self.two_phase_iterator.approximation_mut().next_doc()?;
        self.do_next(doc)
    }

    fn advance(&mut self, target: i32) -> Result<i32> {
        let doc = self
            .two_phase_iterator
            .approximation_mut()
            .advance(target)?;
        self.do_next(doc)
    }

    fn cost(&self) -> Result<i64> {
        self.two_phase_iterator.approximation().cost()
    }
}
/// Return a DocIdSetIterator view of the provided TwoPhaseIterator.
pub fn as_doc_id_set_iterator<TPI>(tpi: TPI) -> TwoPhaseIteratorAsDocIdSetIterator<TPI>
where
    TPI: TwoPhaseIterator,
{
    TwoPhaseIteratorAsDocIdSetIterator::new(tpi)
}

pub fn unwrap<TPI>(tp: TwoPhaseIteratorAsDocIdSetIterator<TPI>) -> TPI
where
    TPI: TwoPhaseIterator,
{
    tp.two_phase_iterator
}

macro_rules! either_two_phase_iterator_homo {
    ($vis:vis $name:ident { $HeadVar:ident : $HeadT:ident $(, $Var:ident : $T:ident )+ $(,)? }) => {
        $vis enum $name<$HeadT, $( $T ),+> {
            $HeadVar($HeadT),
            $( $Var($T), )+
        }

        impl<$HeadT, $( $T ),+> TwoPhaseIterator for $name<$HeadT, $( $T ),+>
        where
            $HeadT: TwoPhaseIterator,
            $( $T: TwoPhaseIterator<DocIdSetIterator = <$HeadT as TwoPhaseIterator>::DocIdSetIterator> ),+
        {
            type DocIdSetIterator = <$HeadT as TwoPhaseIterator>::DocIdSetIterator;

            #[inline]
            fn approximation_mut(&mut self) -> &mut Self::DocIdSetIterator {
                match self {
                    Self::$HeadVar(inner) => inner.approximation_mut(),
                    $( Self::$Var(inner) => inner.approximation_mut(), )+
                }
            }

            #[inline]
            fn approximation(&self) -> &Self::DocIdSetIterator {
                match self {
                    Self::$HeadVar(inner) => inner.approximation(),
                    $( Self::$Var(inner) => inner.approximation(), )+
                }
            }

            #[inline]
            fn take_approximation(&mut self) -> Self::DocIdSetIterator {
                match self {
                    Self::$HeadVar(inner) => inner.take_approximation(),
                    $( Self::$Var(inner) => inner.take_approximation(), )+
                }
            }

            #[inline]
            fn set_empty(&mut self) {
                match self {
                    Self::$HeadVar(inner) => inner.set_empty(),
                    $( Self::$Var(inner) => inner.set_empty(), )+
                }
            }

            #[inline]
            fn matches(&mut self) -> Result<bool> {
                match self {
                    Self::$HeadVar(inner) => inner.matches(),
                    $( Self::$Var(inner) => inner.matches(), )+
                }
            }

            #[inline]
            fn match_cost(&self) -> f32 {
                match self {
                    Self::$HeadVar(inner) => inner.match_cost(),
                    $( Self::$Var(inner) => inner.match_cost(), )+
                }
            }
        }
    };
}
either_two_phase_iterator_homo!(pub Either2TwoPhaseIterator { A: A, B: B });
