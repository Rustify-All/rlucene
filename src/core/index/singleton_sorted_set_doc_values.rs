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
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::sorted_doc_values::SortedDocValues;
use crate::core::index::sorted_set_doc_values::SortedSetDocValues;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::util::error::lucene_error::{LuceneError, Result};

/// Exposes a multi-valued iterator view over a single-valued iterator.
///
/// This can be used if you want to have one multi-valued implementation that
/// works for both single-valued and multi-valued types.
pub struct SingletonSortedSetDocValues<S>
where
    S: SortedDocValues,
{
    pub(crate) inner: Option<S>,
    ord: i64,
}

impl<S> SingletonSortedSetDocValues<S>
where
    S: SortedDocValues,
{
    /// Creates a multi-valued view over the provided SortedDocValues.
    pub fn new(inner: S) -> Result<Self> {
        if inner.doc_id() != -1 {
            return Err(LuceneError::illegal_state(format!(
                "iterator has already been used: docID={}",
                inner.doc_id()
            )));
        }
        Ok(Self {
            inner: Some(inner),
            ord: -1,
        })
    }

    pub fn get_sorted_doc_values(&mut self) -> Result<S> {
        if self.inner.as_ref().unwrap().doc_id() != -1 {
            return Err(LuceneError::illegal_state(format!(
                "iterator has already been used: docID={}",
                self.inner.as_ref().unwrap().doc_id()
            )));
        }
        Ok(self.inner.take().unwrap())
    }
}

impl<S> DocIdSetIterator for SingletonSortedSetDocValues<S>
where
    S: SortedDocValues,
{
    fn doc_id(&self) -> i32 {
        self.inner.as_ref().unwrap().doc_id()
    }

    fn next_doc(&mut self) -> Result<i32> {
        let doc_id = self.inner.as_mut().unwrap().next_doc()?;
        if doc_id != NO_MORE_DOCS {
            self.ord = self.inner.as_mut().unwrap().ord_value()? as i64;
        }
        Ok(doc_id)
    }

    fn advance(&mut self, target: i32) -> Result<i32> {
        let doc_id = self.inner.as_mut().unwrap().advance(target)?;
        if doc_id != NO_MORE_DOCS {
            self.ord = self.inner.as_mut().unwrap().ord_value()? as i64;
        }
        Ok(doc_id)
    }

    fn cost(&self) -> Result<i64> {
        self.inner.as_ref().unwrap().cost()
    }
}

impl<S> DocValuesIterator for SingletonSortedSetDocValues<S>
where
    S: SortedDocValues,
{
    fn advance_exact(&mut self, target: i32) -> Result<bool> {
        if self.inner.as_mut().unwrap().advance_exact(target)? {
            self.ord = self.inner.as_mut().unwrap().ord_value()? as i64;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

impl<S> SortedSetDocValues for SingletonSortedSetDocValues<S>
where
    S: SortedDocValues,
{
    fn next_ord(&mut self) -> Result<i64> {
        Ok(self.ord)
    }

    fn doc_value_count(&mut self) -> Result<i32> {
        Ok(1)
    }

    fn lookup_ord(&mut self, ord: i64) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
        self.inner.as_mut().unwrap().lookup_ord(ord as i32)
    }

    fn get_value_count(&self) -> Result<i64> {
        Ok(self.inner.as_ref().unwrap().get_value_count()? as i64)
    }

    fn lookup_term(&mut self, key: &BytesRef<Vec<u8>>) -> Result<i64> {
        Ok(self.inner.as_mut().unwrap().lookup_term(key)? as i64)
    }

    type TermsEnumRef<'a>
        = S::TermsEnumRef<'a>
    where
        S: 'a;

    type TermsEnum = <S as SortedDocValues>::TermsEnum1;

    fn terms_enum(&mut self) -> Result<Self::TermsEnumRef<'_>> {
        self.inner.as_mut().unwrap().terms_enum()
    }

    fn take_terms_enum(self) -> Result<Self::TermsEnum> {
        self.inner.unwrap().take_terms_enum()
    }

    fn is_single_valued(&self) -> bool {
        true
    }

    type SortedDocValues = S;

    fn get_sorted_doc_values(&mut self) -> Result<Self::SortedDocValues> {
        self.get_sorted_doc_values()
    }
}
