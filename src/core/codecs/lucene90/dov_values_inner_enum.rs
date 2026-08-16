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
use crate::core::codecs::indexed_disi::IndexedDISIImpl;
use crate::core::codecs::lucene90::lucene90_doc_values_producer::{
  BaseSortedDocValuesOrdinals, DenseBaseSortedDocValues, DenseBinaryDocValuesBase,
  DenseBinaryDocValuesBaseImpl, DenseBinaryDocValuesBaseImpl1, DenseNumericDocValuesBase,
  DenseNumericDocValuesBaseImpl, DenseNumericDocValuesBaseImpl1, DenseNumericDocValuesBaseImpl2,
  DenseNumericDocValuesBaseImpl3, DenseNumericDocValuesBaseImpl4, LongValuesImpl, LongValuesImpl1,
  LongValuesImpl2, LongValuesImpl3, LongValuesImpl4, SparseBaseSortedDocValuesImpl,
  SparseBinaryDocValuesBaseImpl, SparseBinaryDocValuesBaseImpl1, SparseNumericDocValuesBase,
  SparseNumericDocValuesBaseImpl, SparseNumericDocValuesBaseImpl1, SparseNumericDocValuesBaseImpl2,
  SparseNumericDocValuesBaseImpl3, SparseNumericDocValuesBaseImpl4,
};
use crate::core::codecs::lucene90_doc_values_producer::{
  BaseSortedSetDocValuesOrdinals, DenseBaseSortedSetDocValues,
  SparseBaseSortedSetDocValuesImpl, SparseBinaryDocValuesBase,
};
use crate::core::index::BytesRef;
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::dummy::dummy_terms_enum::DummyTermsEnum;
use crate::core::index::sorted_doc_values::SortedDocValues;
use crate::core::index::sorted_set_doc_values::SortedSetDocValues;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::store::IndexInput;
use crate::core::store::random_access_input::RandomAccessInput;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::long_values::LongValues;
use crate::core::util::packed::direct_reader::DirectPackedEnum;
use std::borrow::Cow;

pub enum BaseSortedDocValuesEnumImpl<I, R>
where
  I: IndexInput,
  R: RandomAccessInput,
{
  Dense(DenseBaseSortedDocValues<R>),
  Sparse(SparseBaseSortedDocValuesImpl<I, R>),
  Impl(BaseSortedDocValuesOrdinals<I, R>),
}

pub type BaseSortedDocValuesEnum<I> = BaseSortedDocValuesEnumImpl<
  <I as IndexInput>::IndexInput,
  <I as IndexInput>::RandomAccessSlice,
>;

impl<I, R> DocValuesIterator for BaseSortedDocValuesEnumImpl<I, R>
where
  I: IndexInput,
  R: RandomAccessInput,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      Self::Dense(sub) => sub.advance_exact(target),
      Self::Sparse(sub) => sub.advance_exact(target),
      Self::Impl(sub) => sub.advance_exact(target),
    }
  }
}

impl<I, R> DocIdSetIterator for BaseSortedDocValuesEnumImpl<I, R>
where
  I: IndexInput,
  R: RandomAccessInput,
{
  fn doc_id(&self) -> i32 {
    match self {
      Self::Dense(sub) => sub.doc_id(),
      Self::Sparse(sub) => sub.doc_id(),
      Self::Impl(sub) => sub.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::Dense(sub) => sub.next_doc(),
      Self::Sparse(sub) => sub.next_doc(),
      Self::Impl(sub) => sub.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Dense(sub) => sub.advance(target),
      Self::Sparse(sub) => sub.advance(target),
      Self::Impl(sub) => sub.advance(target),
    }
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Dense(sub) => sub.advance(target),
      Self::Sparse(sub) => sub.advance(target),
      Self::Impl(sub) => sub.advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::Dense(sub) => sub.cost(),
      Self::Sparse(sub) => sub.cost(),
      Self::Impl(sub) => sub.cost(),
    }
  }
}

impl<I, R> SortedDocValues for BaseSortedDocValuesEnumImpl<I, R>
where
  I: IndexInput,
  R: RandomAccessInput,
{
  fn ord_value(&mut self) -> Result<i32> {
    match self {
      Self::Dense(sub) => sub.ord_value(),
      Self::Sparse(sub) => sub.ord_value(),
      Self::Impl(sub) => sub.ord_value(),
    }
  }

  type TermsEnum<'a>
    = DummyTermsEnum
  where
    I: 'a,
    R: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    Err(LuceneError::unsupported_operation(""))
  }
}

pub enum BaseSortedSetDocValuesEnumImpl<I, R>
where
  I: IndexInput,
  R: RandomAccessInput,
{
  Dense(DenseBaseSortedSetDocValues<R>),
  Sparse(SparseBaseSortedSetDocValuesImpl<I, R>),
  Impl(BaseSortedSetDocValuesOrdinals<I, R>),
}

pub type BaseSortedSetDocValuesEnum<I> = BaseSortedSetDocValuesEnumImpl<
  <I as IndexInput>::IndexInput,
  <I as IndexInput>::RandomAccessSlice,
>;

impl<I, R> DocValuesIterator for BaseSortedSetDocValuesEnumImpl<I, R>
where
  I: IndexInput,
  R: RandomAccessInput,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      Self::Dense(sub) => sub.advance_exact(target),
      Self::Sparse(sub) => sub.advance_exact(target),
      Self::Impl(sub) => sub.advance_exact(target),
    }
  }
}

impl<I, R> DocIdSetIterator for BaseSortedSetDocValuesEnumImpl<I, R>
where
  I: IndexInput,
  R: RandomAccessInput,
{
  fn doc_id(&self) -> i32 {
    match self {
      Self::Dense(sub) => sub.doc_id(),
      Self::Sparse(sub) => sub.doc_id(),
      Self::Impl(sub) => sub.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::Dense(sub) => sub.next_doc(),
      Self::Sparse(sub) => sub.next_doc(),
      Self::Impl(sub) => sub.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Dense(sub) => sub.advance(target),
      Self::Sparse(sub) => sub.advance(target),
      Self::Impl(sub) => sub.advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::Dense(sub) => sub.cost(),
      Self::Sparse(sub) => sub.cost(),
      Self::Impl(sub) => sub.cost(),
    }
  }
}

impl<I, R> SortedSetDocValues for BaseSortedSetDocValuesEnumImpl<I, R>
where
  I: IndexInput,
  R: RandomAccessInput,
{
  fn next_ord(&mut self) -> Result<i64> {
    match self {
      Self::Dense(sub) => sub.next_ord(),
      Self::Sparse(sub) => sub.next_ord(),
      Self::Impl(sub) => sub.next_ord(),
    }
  }

  fn doc_value_count(&mut self) -> Result<i32> {
    match self {
      Self::Dense(sub) => sub.doc_value_count(),
      Self::Sparse(sub) => sub.doc_value_count(),
      Self::Impl(sub) => sub.doc_value_count(),
    }
  }
  type TermsEnum<'a>
    = DummyTermsEnum
  where
    I: 'a,
    R: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    Err(LuceneError::unsupported_operation(""))
  }

  type SortedDocValues = DummySortedDocValues;
}

pub enum SparseBinaryDocValuesBaseEnum<R> {
  Sparse(SparseBinaryDocValuesBaseImpl<R>),
  Sparse1(SparseBinaryDocValuesBaseImpl1<R>),
}

impl<I, R> SparseBinaryDocValuesBase<I, R> for SparseBinaryDocValuesBaseEnum<R>
where
  I: IndexInput,
  R: RandomAccessInput,
{
  fn binary_value(
    &mut self,
    disi: &mut IndexedDISIImpl<I, R>,
  ) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
    match self {
      SparseBinaryDocValuesBaseEnum::Sparse(sub) => sub.binary_value(disi),
      SparseBinaryDocValuesBaseEnum::Sparse1(sub) => sub.binary_value(disi),
    }
  }
}

pub enum DenseBinaryDocValuesBaseEnum<R> {
  Dense(DenseBinaryDocValuesBaseImpl<R>),
  Dense1(DenseBinaryDocValuesBaseImpl1<R>),
}

impl<R> DenseBinaryDocValuesBase for DenseBinaryDocValuesBaseEnum<R>
where
  R: RandomAccessInput,
{
  fn binary_value(&mut self, doc: i32) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
    match self {
      DenseBinaryDocValuesBaseEnum::Dense(sub) => sub.binary_value(doc),
      DenseBinaryDocValuesBaseEnum::Dense1(sub) => sub.binary_value(doc),
    }
  }
}
pub enum LongValuesEnums<R> {
  Constant(LongValuesImpl),
  Block(LongValuesImpl1<R>),
  Table(LongValuesImpl2<R>),
  Gcd(LongValuesImpl3<R>),
  Delta(LongValuesImpl4<R>),
  Direct(DirectPackedEnum<R>),
}

impl<R> LongValues for LongValuesEnums<R>
where
  R: RandomAccessInput,
{
  fn get_mut(&mut self, index: usize) -> Result<i64> {
    match self {
      Self::Constant(values) => values.get_mut(index),
      Self::Block(values) => values.get_mut(index),
      Self::Table(values) => values.get_mut(index),
      Self::Gcd(values) => values.get_mut(index),
      Self::Delta(values) => values.get_mut(index),
      Self::Direct(values) => values.get_mut(index),
    }
  }

  fn get(&self, index: usize) -> Result<i64> {
    match self {
      Self::Constant(values) => values.get(index),
      Self::Block(values) => values.get(index),
      Self::Table(values) => values.get(index),
      Self::Gcd(values) => values.get(index),
      Self::Delta(values) => values.get(index),
      Self::Direct(values) => values.get(index),
    }
  }
}

pub enum SparseNumericDocValuesSubEnum<R> {
  Sparse(SparseNumericDocValuesBaseImpl),
  Sparse1(SparseNumericDocValuesBaseImpl1<R>),
  Sparse2(SparseNumericDocValuesBaseImpl2<R>),
  Sparse3(SparseNumericDocValuesBaseImpl3<R>),
  Sparse4(SparseNumericDocValuesBaseImpl4<R>),
}

impl<I, R> SparseNumericDocValuesBase<I, R> for SparseNumericDocValuesSubEnum<R>
where
  I: IndexInput,
  R: RandomAccessInput,
{
  fn long_value(&mut self, disi: &mut IndexedDISIImpl<I, R>) -> Result<i64> {
    match self {
      SparseNumericDocValuesSubEnum::Sparse(sub) => sub.long_value(disi),
      SparseNumericDocValuesSubEnum::Sparse1(sub) => sub.long_value(disi),
      SparseNumericDocValuesSubEnum::Sparse2(sub) => sub.long_value(disi),
      SparseNumericDocValuesSubEnum::Sparse3(sub) => sub.long_value(disi),
      SparseNumericDocValuesSubEnum::Sparse4(sub) => sub.long_value(disi),
    }
  }
}

pub enum DenseNumericDocValuesSubEnum<R> {
  Dense(DenseNumericDocValuesBaseImpl),
  Dense1(DenseNumericDocValuesBaseImpl1<R>),
  Dense2(DenseNumericDocValuesBaseImpl2<R>),
  Dense3(DenseNumericDocValuesBaseImpl3<R>),
  Dense4(DenseNumericDocValuesBaseImpl4<R>),
}

impl<R> DenseNumericDocValuesBase for DenseNumericDocValuesSubEnum<R>
where
  R: RandomAccessInput,
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
