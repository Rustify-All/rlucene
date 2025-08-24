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
use crate::index::terms::{Either2Terms, Terms};
use crate::util::error::lucene_error::Result;
/// Provides a [`Terms`] index for fields that have it, and lists which fields
/// do.
///
/// This is primarily an internal/experimental API (see
/// [`FieldsProducer`](crate::codecs::fields_producer::FieldsProducer)),
/// although it is also used to expose the set of term vectors per document.
pub trait Fields {
    /// Returns an iterator that will step through all field names.
    /// This will not return `None`.
    type FieldIter<'a>: Iterator<Item = &'a String>
    where
        Self: 'a;

    fn iterator(&self) -> Self::FieldIter<'_>;

    type Terms: Terms;
    /// Get the [`Terms`] for this field. This will return `None` if the field
    /// does not exist.
    fn terms(&self, field: &str) -> Result<Option<Self::Terms>>;

    /// Returns the number of fields or -1 if the number of distinct field names
    /// is unknown. If >= 0, [`iterator`](Self::iterator) will return as many field names.
    fn size(&self) -> Result<i32>;
}

/// Iterator used by [`Either2Fields`].
pub enum Either2FieldIter<'a, A, B>
where
    A: Iterator<Item = &'a String>,
    B: Iterator<Item = &'a String>,
{
    A(A),
    B(B),
}

impl<'a, A, B> Iterator for Either2FieldIter<'a, A, B>
where
    A: Iterator<Item = &'a String>,
    B: Iterator<Item = &'a String>,
{
    type Item = &'a String;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Either2FieldIter::A(it) => it.next(),
            Either2FieldIter::B(it) => it.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Either2FieldIter::A(it) => it.size_hint(),
            Either2FieldIter::B(it) => it.size_hint(),
        }
    }
}

macro_rules! either_fields {
    ($vis:vis $name:ident => { fi: $fi:ident, te: $te:ident } { $( $Variant:ident : $T:ident ),+ $(,)? }) => {
        $vis enum $name<$( $T ),+> {
            $( $Variant($T), )+
        }

        impl<$( $T ),+> Fields for $name<$( $T ),+>
        where
            $( $T: Fields ),+
        {
            type FieldIter<'a> = $fi<'a, $( <$T as Fields>::FieldIter<'a> ),+>
            where
                $( $T: 'a ),+;
            type Terms = $te<$( <$T as Fields>::Terms ),+>;

            fn iterator(&self) -> Self::FieldIter<'_> {
                match self {
                    $( Self::$Variant(inner) => $fi::$Variant(inner.iterator()), )+
                }
            }

            fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
                match self {
                    $( Self::$Variant(inner) => {
                        let terms = inner.terms(field)?;
                        Ok(terms.map($te::$Variant))
                    } ),+
                }
            }

            fn size(&self) -> Result<i32> {
                match self { $( Self::$Variant(inner) => inner.size(), )+ }
            }
        }
    };
}

either_fields!(
    pub Either2Fields => { fi: Either2FieldIter, te: Either2Terms }
    { A: A, B: B }
);

