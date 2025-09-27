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
use crate::core::index::BytesRef;
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::borrow::Cow;

pub trait BinaryDocValues: DocValuesIterator {
    /// Returns the binary value for the current document ID.
    /// It is illegal to call this method after
    /// [`advanceExact`](DocValuesIterator::advance_exact) returned `false`.
    ///
    /// # Returns
    /// The binary value for the current document ID.
    fn binary_value(&mut self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
        Err(LuceneError::not_implemented("this method need implement"))
    }
}

// BinaryDocValues
macro_rules! either_binary_docvalues {
    ($vis:vis $name:ident { $( $Variant:ident : $T:ident ),+ $(,)? }) => {
        $vis enum $name<$( $T ),+> {
            $( $Variant($T), )+
        }

        impl<$( $T ),+> DocValuesIterator for $name<$( $T ),+>
        where
            $( $T: BinaryDocValues ),+
        {}

        impl<$( $T ),+> DocIdSetIterator for $name<$( $T ),+>
        where
            $( $T: BinaryDocValues ),+
        {
            fn doc_id(&self) -> i32 {
                match self {
                    $( Self::$Variant(inner) => inner.doc_id(), )+
                }
            }
            fn next_doc(&mut self) -> Result<i32> {
                match self {
                    $( Self::$Variant(inner) => inner.next_doc(), )+
                }
            }
            fn advance(&mut self, target: i32) -> Result<i32> {
                match self {
                    $( Self::$Variant(inner) => inner.advance(target), )+
                }
            }
            fn slow_advance(&mut self, target: i32) -> Result<i32> {
                match self {
                    $( Self::$Variant(inner) => inner.slow_advance(target), )+
                }
            }
            fn cost(&self) -> Result<i64> {
                match self {
                    $( Self::$Variant(inner) => inner.cost(), )+
                }
            }
        }

        impl<$( $T ),+> BinaryDocValues for $name<$( $T ),+>
        where
            $( $T: BinaryDocValues ),+
        {

            fn binary_value(&mut self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
                match self {
                    $( Self::$Variant(inner) => inner.binary_value(), )+
                }
            }
        }
    };
}
either_binary_docvalues!(pub Either2BinaryDocValues { A: A, B: B });
either_binary_docvalues!(pub Either3BinaryDocValues { A: A, B: B, C:C });
