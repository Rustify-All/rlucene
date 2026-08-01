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
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::lucene_error::Result;
/// Contains statistics for a collection (field).
///
/// This struct holds statistics across all documents for scoring purposes:
///
/// - `max_doc()`: number of documents.
/// - `doc_count()`: number of documents that contain this field.
/// - `sum_doc_freq()`: number of postings-list entries.
/// - `sum_total_term_freq()`: number of tokens.
///
/// # Conditions
///
/// - All statistics are positive integers: never zero or negative.  
/// - `doc_count` ≤ `max_doc`  
/// - `doc_count` ≤ `sum_doc_freq` ≤ `sum_total_term_freq`  
///
/// Values may include statistics on deleted documents that have not yet been merged away.
///
/// Be careful when performing calculations on these values because they are represented as 64-bit
/// integer values; you may need to cast to `f64` for your use.
///
/// # Arguments
///
/// - `field`: Field’s name. This value is never `None`.  
/// - `max_doc`: Total number of documents in the range `[1 .. i64::MAX]`, regardless of whether they all contain values for this field.  
///   This value is always positive. See [`IndexReader::max_doc()`](crate::core::index::index_reader::IndexReader::max_doc).
/// - `doc_count`: Total number of documents that have at least one term for this field, in the range `[1 .. Max_doc()]`.  
///   This value is always positive and never exceeds `max_doc()`. See [`Terms::doc_count()`](crate::core::index::terms::Terms::get_doc_count).
/// - `sum_total_term_freq`: Total number of tokens for this field, in the range `[sum_doc_freq() .. i64::MAX]`.  
///   This is the “word count” for this field across all documents—the sum of [`TermStatistics::total_term_freq()`](crate::core::search::term_statistics::TermStatistics::get_total_term_freq) across all terms,
///   and also the sum of each document’s field length. Always positive and at least `sum_doc_freq()`.  
///   See [`Terms::sum_total_term_freq()`](crate::core::index::terms::Terms::get_sum_total_term_freq).
/// - `sum_doc_freq`: Total number of posting-list entries for this field, in the range `[doc_count() .. Sum_total_term_freq()]`.  
///   This is the sum of term-document pairs—the sum of [`TermStatistics::doc_freq()`](crate::core::search::term_statistics::TermStatistics::get_doc_freq) across all terms,
///   and also the sum of each document’s unique term count. Always positive, at least `doc_count()`,  
///   and never exceeds `sum_total_term_freq()`. See [`Terms::sum_doc_freq()`](crate::core::index::terms::Terms::get_sum_doc_freq).
pub struct CollectionStatistics {
  field: String,
  max_doc: i64,
  doc_count: i64,
  sum_total_term_freq: i64,
  sum_doc_freq: i64,
}

impl CollectionStatistics {
  /// Creates statistics instance for a collection (field).
  pub fn new<T>(
    field: T,
    max_doc: i64,
    doc_count: i64,
    sum_total_term_freq: i64,
    sum_doc_freq: i64,
  ) -> Result<Self>
  where
    T: Into<String>,
  {
    let field = field.into();
    if max_doc <= 0 {
      return Err(LuceneError::illegal_argument(format!(
        "maxDoc must be positive, maxDoc: {max_doc}"
      )));
    }
    if doc_count <= 0 {
      return Err(LuceneError::illegal_argument(format!(
        "docCount must be positive, docCount: {doc_count}"
      )));
    }
    if doc_count > max_doc {
      return Err(LuceneError::illegal_argument(format!(
        "docCount must not exceed maxDoc, docCount: {doc_count}, maxDoc: {max_doc}"
      )));
    }
    if sum_doc_freq <= 0 {
      return Err(LuceneError::illegal_argument(format!(
        "sumDocFreq must be positive, sumDocFreq: {sum_doc_freq}"
      )));
    }
    if sum_doc_freq < doc_count {
      return Err(LuceneError::illegal_argument(format!(
        "sumDocFreq must be at least docCount, sumDocFreq: {sum_doc_freq}, docCount: {doc_count}"
      )));
    }
    if sum_total_term_freq <= 0 {
      return Err(LuceneError::illegal_argument(format!(
        "sumTotalTermFreq must be positive, sumTotalTermFreq: {sum_total_term_freq}"
      )));
    }
    if sum_total_term_freq < sum_doc_freq {
      return Err(LuceneError::illegal_argument(format!(
        "sumTotalTermFreq must be at least sumDocFreq, sumTotalTermFreq: {sum_total_term_freq}, sumDocFreq: {sum_doc_freq}"
      )));
    }

    Ok(CollectionStatistics {
      field,
      max_doc,
      doc_count,
      sum_total_term_freq,
      sum_doc_freq,
    })
  }

  pub fn get_field(&self) -> &String {
    &self.field
  }

  pub fn get_max_doc(&self) -> i64 {
    self.max_doc
  }

  pub fn get_doc_count(&self) -> i64 {
    self.doc_count
  }

  pub fn get_sum_total_term_freq(&self) -> i64 {
    self.sum_total_term_freq
  }

  pub fn get_sum_doc_freq(&self) -> i64 {
    self.sum_doc_freq
  }
}
