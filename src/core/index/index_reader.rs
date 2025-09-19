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
use crate::core::index::term::Term;
use crate::core::util::error::lucene_error::Result;
pub trait IndexReader {
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
    fn sum_doc_freq(&self, field: &str) -> Result<i64>;
    /// Returns the number of documents that have at least one term for this field.
    /// Note that, just like other term measures, this measure does not take
    /// deleted documents into account.
    ///
    /// See [`Terms::get_doc_count`](crate::core::index::terms::Terms::get_doc_count).
    fn doc_count(&self, field: &str) -> Result<i32>;

    /// Returns the sum of [`TermsEnum::total_term_freq`] for all terms in this field.
    /// Note that, just like other term measures, this measure does not take
    /// deleted documents into account.
    ///
    /// See [`Terms::get_sum_total_term_freq`](crate::core::index::terms::Terms::get_sum_total_term_freq).
    fn sum_total_term_freq(&self, field: &str) -> Result<i64>;
}

pub enum IndexReaderEnum {}
impl IndexReader for IndexReaderEnum {
    fn max_doc(&self) -> Result<i32> {
        todo!()
    }

    fn num_docs(&self) -> Result<i32> {
        todo!()
    }

    fn num_deleted_docs(&self) -> Result<i32> {
        todo!()
    }

    fn inc_ref(&self) -> Result<()> {
        todo!()
    }

    fn dec_ref(&self) -> Result<()> {
        todo!()
    }

    fn ensure_open(&self) -> Result<()> {
        todo!()
    }

    fn has_deletions(&self) -> Result<bool> {
        todo!()
    }

    fn do_close(&mut self) -> Result<()> {
        todo!()
    }

    fn doc_freq(&self, term: &Term) -> Result<i32> {
        todo!()
    }

    fn total_term_freq(&self, term: &Term) -> Result<i64> {
        todo!()
    }

    fn sum_doc_freq(&self, field: &str) -> Result<i64> {
        todo!()
    }

    fn doc_count(&self, field: &str) -> Result<i32> {
        todo!()
    }

    fn sum_total_term_freq(&self, field: &str) -> Result<i64> {
        todo!()
    }
}
