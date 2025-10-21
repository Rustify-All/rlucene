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
use crate::core::index::composite_reader::CompositeReader;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::stored_fields::{Either2StoredFields, StoredFields};
use crate::core::index::term::Term;
use crate::core::index::term_vectors::{Either2TermVectors, TermVectors};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::fmt::{Display, Formatter};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

pub trait IndexReader: Display {
    type TermVectors: TermVectors;
    fn term_vectors(&self) -> Result<Self::TermVectors>;

    fn max_doc(&self) -> Result<i32>;

    fn num_docs(&self) -> Result<i32>;

    fn num_deleted_docs(&self) -> Result<i32> {
        Ok(self.max_doc()? - self.num_docs()?)
    }

    fn inc_ref(&self) -> Result<()> {
        todo!()
    }

    fn dec_ref(&self) -> Result<()> {
        todo!()
    }

    fn ensure_open(&self) -> Result<()> {
        // TODO
        Ok(())
    }

    type StoredFields: StoredFields;
    fn stored_fields(&self) -> Result<Self::StoredFields>;

    fn has_deletions(&self) -> Result<bool> {
        Ok(self.num_deleted_docs()? > 0)
    }

    fn do_close(&mut self) -> Result<()>;

    /// Returns the number of documents containing the `term`.
    /// This method returns `0` if the term or field does not exist.
    /// This method does not take into account deleted documents that
    /// have not yet been merged away.
    ///
    /// See [`TermsEnum::doc_freq`](crate::core::index::terms_enum::TermsEnum::doc_freq).
    fn doc_freq(&self, term: &Term) -> Result<i32>;

    /// Returns the total number of occurrences of `term` across all documents
    /// (the sum of the `freq()` for each doc that has this term).
    /// Note that, like other term measures, this measure does not take
    /// deleted documents into account.
    fn total_term_freq(&self, term: &Term) -> Result<i64>;
    /// Returns the sum of [`TermsEnum::doc_freq`](crate::core::index::terms_enum::TermsEnum::doc_freq) for all terms in this field.
    /// Note that, just like other term measures, this measure does not take
    /// deleted documents into account.
    ///
    /// See [`Terms::get_sum_doc_freq`](crate::core::index::terms::Terms::get_sum_doc_freq).
    fn get_sum_doc_freq(&self, field: &str) -> Result<i64>;
    /// Returns the number of documents that have at least one term for this field.
    /// Note that, just like other term measures, this measure does not take
    /// deleted documents into account.
    ///
    /// See [`Terms::get_doc_count`](crate::core::index::terms::Terms::get_doc_count).
    fn get_doc_count(&self, field: &str) -> Result<i32>;

    /// Returns the sum of [`TermsEnum::total_term_freq`] for all terms in this field.
    /// Note that, just like other term measures, this measure does not take
    /// deleted documents into account.
    ///
    /// See [`Terms::get_sum_total_term_freq`](crate::core::index::terms::Terms::get_sum_total_term_freq).
    fn get_sum_total_term_freq(&self, field: &str) -> Result<i64>;
}

pub trait CacheHelper {
    fn get_key(&self) -> CacheKey;
}
#[derive(Clone)]
pub struct CacheKey {
    identity: Arc<()>,
}
impl Default for CacheKey {
    fn default() -> Self {
        Self::new()
    }
}

impl CacheKey {
    pub fn new() -> Self {
        Self {
            identity: Arc::new(()),
        }
    }
}
impl PartialEq for CacheKey {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.identity, &other.identity)
    }
}
impl Eq for CacheKey {}

impl Hash for CacheKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (Arc::as_ptr(&self.identity) as usize).hash(state);
    }
}

pub type IRTermVectors<LR, CR> =
    Either2TermVectors<<LR as IndexReader>::TermVectors, <CR as IndexReader>::TermVectors>;
pub type IRStoredFields<LR, CR> =
    Either2StoredFields<<LR as IndexReader>::StoredFields, <CR as IndexReader>::StoredFields>;
pub enum IndexReaderEnum<LR, CR>
where
    LR: LeafReader,
    CR: CompositeReader,
{
    Leaf(LR),
    Composite(CR),
}

impl<LR, CR> Display for IndexReaderEnum<LR, CR>
where
    CR: CompositeReader,
    LR: LeafReader,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexReaderEnum::Leaf(leaf) => write!(f, "LeafReader: {}", leaf),
            IndexReaderEnum::Composite(comp) => write!(f, "CompositeReader: {}", comp),
        }
    }
}

impl<LR, CR> IndexReader for IndexReaderEnum<LR, CR>
where
    LR: LeafReader,
    CR: CompositeReader,
{
    type TermVectors = IRTermVectors<LR, CR>;

    fn term_vectors(&self) -> Result<Self::TermVectors> {
        match self {
            IndexReaderEnum::Leaf(leaf) => Ok(Either2TermVectors::A(leaf.term_vectors()?)),
            IndexReaderEnum::Composite(comp) => Ok(Either2TermVectors::B(comp.term_vectors()?)),
        }
    }

    fn max_doc(&self) -> Result<i32> {
        match self {
            IndexReaderEnum::Leaf(leaf) => leaf.max_doc(),
            IndexReaderEnum::Composite(comp) => comp.max_doc(),
        }
    }

    fn num_docs(&self) -> Result<i32> {
        match self {
            IndexReaderEnum::Leaf(leaf) => leaf.num_docs(),
            IndexReaderEnum::Composite(comp) => comp.num_docs(),
        }
    }

    fn num_deleted_docs(&self) -> Result<i32> {
        match self {
            IndexReaderEnum::Leaf(leaf) => leaf.num_deleted_docs(),
            IndexReaderEnum::Composite(comp) => comp.num_deleted_docs(),
        }
    }

    fn inc_ref(&self) -> Result<()> {
        match self {
            IndexReaderEnum::Leaf(leaf) => leaf.inc_ref(),
            IndexReaderEnum::Composite(comp) => comp.inc_ref(),
        }
    }

    fn dec_ref(&self) -> Result<()> {
        match self {
            IndexReaderEnum::Leaf(leaf) => leaf.dec_ref(),
            IndexReaderEnum::Composite(comp) => comp.dec_ref(),
        }
    }

    fn ensure_open(&self) -> Result<()> {
        match self {
            IndexReaderEnum::Leaf(leaf) => leaf.ensure_open(),
            IndexReaderEnum::Composite(comp) => comp.ensure_open(),
        }
    }

    type StoredFields = IRStoredFields<LR, CR>;

    fn stored_fields(&self) -> Result<Self::StoredFields> {
        match self {
            IndexReaderEnum::Leaf(leaf) => Ok(Either2StoredFields::A(leaf.stored_fields()?)),
            IndexReaderEnum::Composite(comp) => Ok(Either2StoredFields::B(comp.stored_fields()?)),
        }
    }

    fn has_deletions(&self) -> Result<bool> {
        match self {
            IndexReaderEnum::Leaf(leaf) => leaf.has_deletions(),
            IndexReaderEnum::Composite(comp) => comp.has_deletions(),
        }
    }

    fn do_close(&mut self) -> Result<()> {
        match self {
            IndexReaderEnum::Leaf(leaf) => leaf.do_close(),
            IndexReaderEnum::Composite(comp) => comp.do_close(),
        }
    }

    fn doc_freq(&self, term: &Term) -> Result<i32> {
        match self {
            IndexReaderEnum::Leaf(leaf) => <LR as IndexReader>::doc_freq(leaf, term),
            IndexReaderEnum::Composite(comp) => comp.doc_freq(term),
        }
    }

    fn total_term_freq(&self, term: &Term) -> Result<i64> {
        match self {
            IndexReaderEnum::Leaf(leaf) => <LR as IndexReader>::total_term_freq(leaf, term),
            IndexReaderEnum::Composite(comp) => comp.total_term_freq(term),
        }
    }

    fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
        match self {
            IndexReaderEnum::Leaf(leaf) => leaf.get_sum_doc_freq(field),
            IndexReaderEnum::Composite(comp) => comp.get_sum_doc_freq(field),
        }
    }

    fn get_doc_count(&self, field: &str) -> Result<i32> {
        match self {
            IndexReaderEnum::Leaf(leaf) => leaf.get_doc_count(field),
            IndexReaderEnum::Composite(comp) => comp.get_doc_count(field),
        }
    }

    fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
        match self {
            IndexReaderEnum::Leaf(leaf) => leaf.get_sum_total_term_freq(field),
            IndexReaderEnum::Composite(comp) => comp.get_sum_total_term_freq(field),
        }
    }
}

impl<IR> IndexReader for Arc<IR>
where
    IR: IndexReader,
{
    type TermVectors = IR::TermVectors;

    fn term_vectors(&self) -> Result<Self::TermVectors> {
        (**self).term_vectors()
    }

    fn max_doc(&self) -> Result<i32> {
        (**self).max_doc()
    }

    fn num_docs(&self) -> Result<i32> {
        (**self).num_docs()
    }

    fn num_deleted_docs(&self) -> Result<i32> {
        (**self).num_deleted_docs()
    }

    fn inc_ref(&self) -> Result<()> {
        (**self).inc_ref()
    }

    fn dec_ref(&self) -> Result<()> {
        (**self).dec_ref()
    }

    fn ensure_open(&self) -> Result<()> {
        (**self).ensure_open()
    }

    type StoredFields = IR::StoredFields;

    fn stored_fields(&self) -> Result<Self::StoredFields> {
        (**self).stored_fields()
    }

    fn has_deletions(&self) -> Result<bool> {
        (**self).has_deletions()
    }

    fn do_close(&mut self) -> Result<()> {
        match Arc::get_mut(self) {
            Some(inner) => inner.do_close(),
            None => Err(LuceneError::illegal_state(
                "Cannot close shared Arc<IndexReader> because it has multiple references",
            )),
        }
    }

    fn doc_freq(&self, term: &Term) -> Result<i32> {
        (**self).doc_freq(term)
    }

    fn total_term_freq(&self, term: &Term) -> Result<i64> {
        (**self).total_term_freq(term)
    }

    fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
        (**self).get_sum_doc_freq(field)
    }

    fn get_doc_count(&self, field: &str) -> Result<i32> {
        (**self).get_doc_count(field)
    }

    fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
        (**self).get_sum_total_term_freq(field)
    }
}
