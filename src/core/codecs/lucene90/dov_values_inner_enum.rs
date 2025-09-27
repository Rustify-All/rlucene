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
use crate::core::codecs::dummy::dummy_sorted_doc_values::DummySortedDocValues;
use crate::core::codecs::indexed_disi::IndexedDISI;
use crate::core::codecs::lucene90::lucene90_doc_values_producer::{
    BaseSortedDocValuesImpl, DenseBaseSortedDocValues, DenseBinaryDocValuesBase,
    DenseBinaryDocValuesBaseImpl, DenseBinaryDocValuesBaseImpl1, DenseNumericDocValuesBase,
    DenseNumericDocValuesBaseImpl, DenseNumericDocValuesBaseImpl1, DenseNumericDocValuesBaseImpl2,
    DenseNumericDocValuesBaseImpl3, DenseNumericDocValuesBaseImpl4, LongValuesImpl,
    LongValuesImpl1, LongValuesImpl2, LongValuesImpl3, LongValuesImpl4, SparseBaseSortedDocValues,
    SparseBinaryDocValuesBaseImpl, SparseBinaryDocValuesBaseImpl1, SparseNumericDocValuesBase,
    SparseNumericDocValuesBaseImpl, SparseNumericDocValuesBaseImpl1,
    SparseNumericDocValuesBaseImpl2, SparseNumericDocValuesBaseImpl3,
    SparseNumericDocValuesBaseImpl4,
};
use crate::core::codecs::lucene90_doc_values_producer::{
    BaseSortedSetDocValuesImpl, DenseBaseSortedSetDocValues, SparseBaseSortedSetDocValues,
    SparseBinaryDocValuesBase,
};
use crate::core::index::BytesRef;
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::dummy::dummy_terms_enum::DummyTermsEnum;
use crate::core::index::sorted_doc_values::SortedDocValues;
use crate::core::index::sorted_set_doc_values::SortedSetDocValues;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::store::IndexInput;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::long_values::Either5LongValues;
use std::borrow::Cow;

pub enum BaseSortedDocValuesEnum<I>
where
    I: IndexInput,
{
    Dense(DenseBaseSortedDocValues<I>),
    Sparse(SparseBaseSortedDocValues<I>),
    Impl(BaseSortedDocValuesImpl<I>),
}

impl<I> DocValuesIterator for BaseSortedDocValuesEnum<I>
where
    I: IndexInput,
{
    fn advance_exact(&mut self, target: i32) -> Result<bool> {
        match self {
            BaseSortedDocValuesEnum::Dense(sub) => sub.advance_exact(target),
            BaseSortedDocValuesEnum::Sparse(sub) => sub.advance_exact(target),
            BaseSortedDocValuesEnum::Impl(sub) => sub.advance_exact(target),
        }
    }
}

impl<I> DocIdSetIterator for BaseSortedDocValuesEnum<I>
where
    I: IndexInput,
{
    fn doc_id(&self) -> i32 {
        match self {
            BaseSortedDocValuesEnum::Dense(sub) => sub.doc_id(),
            BaseSortedDocValuesEnum::Sparse(sub) => sub.doc_id(),
            BaseSortedDocValuesEnum::Impl(sub) => sub.doc_id(),
        }
    }

    fn next_doc(&mut self) -> Result<i32> {
        match self {
            BaseSortedDocValuesEnum::Dense(sub) => sub.next_doc(),
            BaseSortedDocValuesEnum::Sparse(sub) => sub.next_doc(),
            BaseSortedDocValuesEnum::Impl(sub) => sub.next_doc(),
        }
    }

    fn advance(&mut self, target: i32) -> Result<i32> {
        match self {
            BaseSortedDocValuesEnum::Dense(sub) => sub.advance(target),
            BaseSortedDocValuesEnum::Sparse(sub) => sub.advance(target),
            BaseSortedDocValuesEnum::Impl(sub) => sub.advance(target),
        }
    }

    fn slow_advance(&mut self, target: i32) -> Result<i32> {
        match self {
            BaseSortedDocValuesEnum::Dense(sub) => sub.advance(target),
            BaseSortedDocValuesEnum::Sparse(sub) => sub.advance(target),
            BaseSortedDocValuesEnum::Impl(sub) => sub.advance(target),
        }
    }

    fn cost(&self) -> Result<i64> {
        match self {
            BaseSortedDocValuesEnum::Dense(sub) => sub.cost(),
            BaseSortedDocValuesEnum::Sparse(sub) => sub.cost(),
            BaseSortedDocValuesEnum::Impl(sub) => sub.cost(),
        }
    }
}

impl<I> SortedDocValues for BaseSortedDocValuesEnum<I>
where
    I: IndexInput,
{
    fn ord_value(&mut self) -> Result<i32> {
        match self {
            BaseSortedDocValuesEnum::Dense(sub) => sub.ord_value(),
            BaseSortedDocValuesEnum::Sparse(sub) => sub.ord_value(),
            BaseSortedDocValuesEnum::Impl(sub) => sub.ord_value(),
        }
    }

    type TermsEnum = DummyTermsEnum;
}

pub enum BaseSortedSetDocValuesEnum<I>
where
    I: IndexInput,
{
    Dense(DenseBaseSortedSetDocValues<I>),
    Sparse(SparseBaseSortedSetDocValues<I>),
    Impl(BaseSortedSetDocValuesImpl<I>),
}

impl<I> DocValuesIterator for BaseSortedSetDocValuesEnum<I>
where
    I: IndexInput,
{
    fn advance_exact(&mut self, target: i32) -> Result<bool> {
        match self {
            BaseSortedSetDocValuesEnum::Dense(sub) => sub.advance_exact(target),
            BaseSortedSetDocValuesEnum::Sparse(sub) => sub.advance_exact(target),
            BaseSortedSetDocValuesEnum::Impl(sub) => sub.advance_exact(target),
        }
    }
}

impl<I> DocIdSetIterator for BaseSortedSetDocValuesEnum<I>
where
    I: IndexInput,
{
    fn doc_id(&self) -> i32 {
        match self {
            BaseSortedSetDocValuesEnum::Dense(sub) => sub.doc_id(),
            BaseSortedSetDocValuesEnum::Sparse(sub) => sub.doc_id(),
            BaseSortedSetDocValuesEnum::Impl(sub) => sub.doc_id(),
        }
    }

    fn next_doc(&mut self) -> Result<i32> {
        match self {
            BaseSortedSetDocValuesEnum::Dense(sub) => sub.next_doc(),
            BaseSortedSetDocValuesEnum::Sparse(sub) => sub.next_doc(),
            BaseSortedSetDocValuesEnum::Impl(sub) => sub.next_doc(),
        }
    }

    fn advance(&mut self, target: i32) -> Result<i32> {
        match self {
            BaseSortedSetDocValuesEnum::Dense(sub) => sub.advance(target),
            BaseSortedSetDocValuesEnum::Sparse(sub) => sub.advance(target),
            BaseSortedSetDocValuesEnum::Impl(sub) => sub.advance(target),
        }
    }

    fn cost(&self) -> Result<i64> {
        match self {
            BaseSortedSetDocValuesEnum::Dense(sub) => sub.cost(),
            BaseSortedSetDocValuesEnum::Sparse(sub) => sub.cost(),
            BaseSortedSetDocValuesEnum::Impl(sub) => sub.cost(),
        }
    }
}

impl<I> SortedSetDocValues for BaseSortedSetDocValuesEnum<I>
where
    I: IndexInput,
{
    fn next_ord(&mut self) -> Result<i64> {
        match self {
            BaseSortedSetDocValuesEnum::Dense(sub) => sub.next_ord(),
            BaseSortedSetDocValuesEnum::Sparse(sub) => sub.next_ord(),
            BaseSortedSetDocValuesEnum::Impl(sub) => sub.next_ord(),
        }
    }

    fn doc_value_count(&mut self) -> Result<i32> {
        match self {
            BaseSortedSetDocValuesEnum::Dense(sub) => sub.doc_value_count(),
            BaseSortedSetDocValuesEnum::Sparse(sub) => sub.doc_value_count(),
            BaseSortedSetDocValuesEnum::Impl(sub) => sub.doc_value_count(),
        }
    }
    type TermsEnum = DummyTermsEnum;
    type SortedDocValues = DummySortedDocValues;
}

pub enum SparseBinaryDocValuesBaseEnum<I>
where
    I: IndexInput,
{
    Sparse(SparseBinaryDocValuesBaseImpl<I>),
    Sparse1(SparseBinaryDocValuesBaseImpl1<I>),
}

impl<I> SparseBinaryDocValuesBase<I> for SparseBinaryDocValuesBaseEnum<I>
where
    I: IndexInput,
{
    fn binary_value(&mut self, disi: &mut IndexedDISI<I>) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
        match self {
            SparseBinaryDocValuesBaseEnum::Sparse(sub) => sub.binary_value(disi),
            SparseBinaryDocValuesBaseEnum::Sparse1(sub) => sub.binary_value(disi),
        }
    }
}

pub enum DenseBinaryDocValuesBaseEnum<I>
where
    I: IndexInput,
{
    Dense(DenseBinaryDocValuesBaseImpl<I>),
    Dense1(DenseBinaryDocValuesBaseImpl1<I>),
}

impl<I> DenseBinaryDocValuesBase for DenseBinaryDocValuesBaseEnum<I>
where
    I: IndexInput,
{
    fn binary_value(&mut self, doc: i32) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
        match self {
            DenseBinaryDocValuesBaseEnum::Dense(sub) => sub.binary_value(doc),
            DenseBinaryDocValuesBaseEnum::Dense1(sub) => sub.binary_value(doc),
        }
    }
}
pub type LongValuesEnums<I> = Either5LongValues<
    LongValuesImpl,
    LongValuesImpl1<I>,
    LongValuesImpl2<I>,
    LongValuesImpl3<I>,
    LongValuesImpl4<I>,
>;

pub enum SparseNumericDocValuesSubEnum<I>
where
    I: IndexInput,
{
    Sparse(SparseNumericDocValuesBaseImpl),
    Sparse1(SparseNumericDocValuesBaseImpl1<I>),
    Sparse2(SparseNumericDocValuesBaseImpl2<I>),
    Sparse3(SparseNumericDocValuesBaseImpl3<I>),
    Sparse4(SparseNumericDocValuesBaseImpl4<I>),
}

impl<I> SparseNumericDocValuesBase<I> for SparseNumericDocValuesSubEnum<I>
where
    I: IndexInput,
{
    fn long_value(&mut self, disi: &mut IndexedDISI<I>) -> Result<i64>
    where
        I: IndexInput,
    {
        match self {
            SparseNumericDocValuesSubEnum::Sparse(sub) => sub.long_value(disi),
            SparseNumericDocValuesSubEnum::Sparse1(sub) => sub.long_value(disi),
            SparseNumericDocValuesSubEnum::Sparse2(sub) => sub.long_value(disi),
            SparseNumericDocValuesSubEnum::Sparse3(sub) => sub.long_value(disi),
            SparseNumericDocValuesSubEnum::Sparse4(sub) => sub.long_value(disi),
        }
    }
}

pub enum DenseNumericDocValuesSubEnum<I>
where
    I: IndexInput,
{
    Dense(DenseNumericDocValuesBaseImpl),
    Dense1(DenseNumericDocValuesBaseImpl1<I>),
    Dense2(DenseNumericDocValuesBaseImpl2<I>),
    Dense3(DenseNumericDocValuesBaseImpl3<I>),
    Dense4(DenseNumericDocValuesBaseImpl4<I>),
}

impl<I> DenseNumericDocValuesBase for DenseNumericDocValuesSubEnum<I>
where
    I: IndexInput,
{
    fn long_value(&mut self, doc: i32) -> Result<i64> {
        match self {
            DenseNumericDocValuesSubEnum::Dense(sub) => sub.long_value(doc),
            DenseNumericDocValuesSubEnum::Dense1(sub) => sub.long_value(doc),
            DenseNumericDocValuesSubEnum::Dense2(sub) => sub.long_value(doc),
            DenseNumericDocValuesSubEnum::Dense3(sub) => sub.long_value(doc),
            DenseNumericDocValuesSubEnum::Dense4(sub) => sub.long_value(doc),
        }
    }
}
