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
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::lucene_error::Result;
use std::sync::Arc;

/// Contains statistics for a specific term
///
/// This struct holds statistics for this term across all documents for scoring purposes:
///
/// - `doc_freq`: number of documents this term occurs in.
/// - `total_term_freq`: number of tokens for this term.
///
/// The following conditions are always true:
///
/// - All statistics are positive integers: never zero or negative.
/// - `doc_freq <= total_term_freq`
/// - `doc_freq <= sum_doc_freq` of the collection
/// - `total_term_freq <= sumtotal_term_freq` of the collection
///
/// Values may include statistics on deleted documents that have not yet been merged away.
///
/// Be careful when performing calculations on these values because they are represented as 64-bit
/// integer values, you may need to cast to `double` for your use.
///
/// - **term**: Term bytes.  
///   This value is never `null`.
///
/// - **doc_freq**: number of documents containing the term in the collection, in the range  
///   `[1 .. total_term_freq()]`.  
///   This is the document-frequency for the term: the count of documents where the term appears  
///   at least one time.  
///   This value is always a positive number, and never exceeds `total_term_freq`.  
///   It also cannot exceed [`CollectionStatistics::sum_doc_freq()`](crate::core::search::collection_statistics::CollectionStatistics::get_sum_doc_freq).
///   See also: [`TermsEnum::doc_freq()`](crate::core::index::terms_enum::TermsEnum::doc_freq)
///
/// - **total_term_freq**: number of occurrences of the term in the collection, in the range  
///   `[doc_freq() .. CollectionStatistics::sum_total_term_freq()]`.  
///   This is the token count for the term: the number of times it appears in the field across  
///   all documents.  
///   This value is always a positive number, always at least `doc_freq()`,  
///   and never exceeds [`CollectionStatistics::sum_total_term_freq()`](crate::core::search::collection_statistics::CollectionStatistics::get_sum_total_term_freq).
///   See also: [`TermsEnum::total_term_freq()`](crate::core::index::terms_enum::TermsEnum::total_term_freq)
#[derive(Debug)]
pub struct TermStatistics {
  term: Arc<Term>,
  doc_freq: i64,
  total_term_freq: i64,
}

impl TermStatistics {
  /// Creates a new `TermStatistics` instance for a term.
  ///
  /// # Error
  ///
  /// - Error if `doc_freq` is zero or negative.  
  /// - Error if `total_term_freq` is less than `doc_freq`.  
  pub fn new<T>(term: T, doc_freq: i64, total_term_freq: i64) -> Result<Self>
  where
    T: Into<Arc<Term>>,
  {
    let term = term.into();
    if doc_freq <= 0 {
      return Err(LuceneError::illegal_argument(format!(
        "doc_freq must be positive, doc_freq: {doc_freq}"
      )));
    }
    if total_term_freq <= 0 {
      return Err(LuceneError::illegal_argument(format!(
        "total_term_freq must be positive, total_term_freq: {total_term_freq}"
      )));
    }
    if total_term_freq < doc_freq {
      return Err(LuceneError::illegal_argument(format!(
        "total_term_freq must be at least doc_freq, total_term_freq: {total_term_freq}, doc_freq: {doc_freq}"
      )));
    }
    Ok(TermStatistics {
      term,
      doc_freq,
      total_term_freq,
    })
  }

  pub fn get_term(&self) -> &Arc<Term> {
    &self.term
  }

  pub fn get_doc_freq(&self) -> i64 {
    self.doc_freq
  }

  pub fn get_total_term_freq(&self) -> i64 {
    self.total_term_freq
  }
}
