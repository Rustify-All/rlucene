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
use crate::core::index::field_invert_state::FieldInvertState;
use crate::core::index::index_options::IndexOptions;
use crate::core::search::collection_statistics::CollectionStatistics;
use crate::core::search::explanation::Explanation;
use crate::core::search::term_statistics::TermStatistics;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::small_float::SmallFloat;
use std::fmt::Display;
/// Similarity defines the components of Lucene scoring.  
///
/// *Expert: Scoring API.*  
///
/// This is a low-level API—only implement this trait if you want to provide a custom
/// information retrieval *model*. If you merely wish to tweak Lucene’s scoring, consider
/// using or extending [`BM25Similarity`](crate::core::search::similarities_impl::bm25_similarity::BM25Similarity) or [`SimilarityBase`](crate::core::search::similarities_impl::similarity_base::SimilarityBase), which simplify score computation
/// from index statistics.
///
/// Similarity determines how Lucene weights terms at both indexing-time and query-time.
///
/// ## Indexing Time  
///
/// At indexing time, the indexer calls  
/// [`compute_norm(field_state: &FieldInvertState)`](Self::compute_norm)  
/// to allow your implementation to set a per-document normalization value. This norm byte
/// is later accessible via  
/// [`LeafReader::get_norm_values(field: &str)`](crate::core::index::leaf_reader::LeafReader).
/// Lucene makes no assumption about its contents, but it’s most commonly used for length
/// normalization. Implementations should carefully choose how to encode this value—Lucene’s
/// default uses [`SmallFloat`] to pack length norms into a single byte, but other schemes
/// may be appropriate for your model.
///
/// Many scoring formulas require average document length, which you can compute via  
/// [`CollectionStatistics::sum_total_term_freq()`](CollectionStatistics::get_sum_total_term_freq) and [`CollectionStatistics::doc_count()`](CollectionStatistics::get_doc_count).
///
/// Additional, field-custom scoring factors can be stored in named
/// [`NumericDocValuesField`](crate::core::document::numeric_doc_values_field::NumericDocValuesField)s and accessed at query time via
/// [`LeafReader::get_numeric_doc_values(field: &str)`](crate::core::index::leaf_reader::LeafReader), though such logic should live
/// outside of this trait (e.g. in a `FunctionScoreQuery`).
///
/// Instead of using index-time boosts (folded into the norm byte or DocValues) for
/// constant per-field boosts, you can simply expose a constant boost parameter `c`
/// in your implementation and use [`PerFieldSimilarityWrapper`](crate::core::search::similarities_impl::per_field_similarity_wrapper::PerFieldSimilarityWrapper) to return
/// different `Similarity` instances per field name.
///
/// ## Query Time  
///
/// At query time, the following steps occur:
///
/// 1. The method  
///    [`scorer(boost: f32, stats: &CollectionStatistics, terms: &[TermStatistics])`](Self::scorer)  
///    is called once, allowing you to compute any global statistics (IDF, avg. length, etc.)
///    from the provided raw statistics without additional I/O. Return a
///    [`Similarity::SimScorer`] instance that encapsulates your scoring logic.
/// 2. For each matching document,  
///    [`SimScorer::score(freq: f32, doc_len: i64)`](SimScorer::score)  
///    is invoked to compute that document’s final score.
///
/// ## Explanations  
///
/// When [`IndexSearcher::explain(query: &Query, doc: i32)`](crate::core::search::index_searcher::IndexSearcher) is invoked, Lucene consults
/// your scorer’s explanation method to detail how the score was computed, passing in the
/// document ID and a frequency explanation.
pub trait Similarity: Display {
    /// Returns `true` if overlap tokens (tokens with a position increment of `0`) are
    /// discounted from the document’s length.
    fn get_discount_overlaps(&self) -> bool {
        true
    }
    /// Computes the normalization value for a field at index-time.
    ///
    /// The default implementation uses [`SmallFloat::int_to_byte4`] to encode the number of terms
    /// into a single byte.
    ///
    /// **Warning:** The default implementation is used by Lucene’s supplied similarity classes,
    /// allowing you to swap in a different `Similarity` at runtime without reindexing. If you
    /// override this method, you **must** reindex documents for your change to take effect.
    ///
    /// Matches in longer fields are less precise, so implementations typically emit smaller norm
    /// values when `state.length()` is large, and larger values when `state.length()` is small.
    ///
    /// For a given term-document frequency, greater unsigned norms must produce scores that are
    /// lower or equal. That is, for two encoded norms `n1` and `n2` (treated as unsigned) where
    /// `n1 > n2`, it must hold that:
    /// ```text
    /// SimScorer::score(freq, n1) <= SimScorer::score(freq, n2)
    /// ```
    /// for any valid `freq`.
    ///
    /// `0` is not a legal norm value; `1` produces the highest possible scores.
    ///
    /// # Experimental
    ///
    /// This API is experimental and may change in future releases.
    ///
    /// # Arguments
    ///
    /// - `state`: accumulated state of term processing for this field (`FieldInvertState`).
    ///
    /// # Returns
    ///
    /// A `u8` norm value, suitable for storage in the index.
    fn compute_norm(&self, state: &FieldInvertState) -> Result<i64> {
        let num_terms = if state.get_index_options() == IndexOptions::Docs {
            state.get_unique_term_count()
        } else if self.get_discount_overlaps() {
            state.length() - state.num_overlap()
        } else {
            state.length()
        };
        Ok(SmallFloat::int_to_byte4(num_terms)? as i64)
    }

    type SimScorer: SimScorer;
    /// Computes any collection-level weight (e.g., IDF, average document length, etc.) needed
    /// for scoring a query.
    ///
    /// # Arguments
    ///
    /// - `boost`: A multiplicative factor to apply to the produced scores (`f32`).
    /// - `collection_stats`: Collection-level statistics, such as the total number of tokens
    ///   and document counts (`&CollectionStatistics`).
    /// - `term_stats`: Term-level statistics, such as document frequency and total term frequency
    ///   for each query term (`&[TermStatistics]`).
    ///
    /// # Returns
    ///
    /// A `SimWeight` instance containing all information this `Similarity` needs to score the query.
    fn scorer(
        &self,
        boost: f32,
        collection_stats: &CollectionStatistics,
        term_stats: &[TermStatistics],
    ) -> Self::SimScorer;
}
/// Stores the weight for a query across the indexed collection.
///
/// This trait is a marker for query‐weight implementations. The base implementation is empty;
/// your `Similarity` should define a concrete `SimWeight` type that captures whatever statistics
/// it needs (e.g., IDF, average field length, etc.).
///
/// # Usage
///
/// Implement this trait for your weight struct and return it from
pub trait SimScorer {
    /// Scores a single document.
    ///
    /// - `freq` is the sloppy term frequency for this document; it must be finite and positive.
    /// - `norm` is the encoded normalization factor (as returned by
    ///   [`Similarity::compute_norm`]
    ///   or `1` if norms are disabled; it is never `0`.
    ///
    /// # Scoring Guarantees
    ///
    /// - Scores must not decrease when `freq` increases. That is, if `freq1 > freq2`, then
    ///   `score(freq1, norm) >= score(freq2, norm)` for any valid `norm`.
    /// - Scores must not increase when the unsigned `norm` increases. That is, for two norms
    ///   `n1` and `n2` (treated as unsigned) with `n1 > n2`, it must hold that
    ///   `score(freq, n1) <= score(freq, n2)` for any valid `freq`.
    /// - Consequently, the maximum possible score is bound by `score(f32::MAX, 1)`.
    ///
    /// # Arguments
    ///
    /// - `freq`: sloppy term frequency (`f32`), finite and > 0.
    /// - `norm`: normalization byte (`u8`), never `0`.
    ///
    /// # Returns
    ///
    /// A `f32` score for this document.
    fn score(&self, freq: f32, norm: i64) -> f32;

    /// Explains the score for a single document.
    ///
    /// # Arguments
    ///
    /// - `freq`: Explanation of how the sloppy term frequency was computed (`&Explanation`).
    /// - `norm`: Encoded normalization factor (as returned by `Similarity::compute_norm`), or `1` if norms are disabled (`u8`).
    ///
    /// # Returns
    ///
    /// An `Explanation` detailing how the document’s score was derived.
    fn explain(&self, freq: Explanation, norm: i64) -> Explanation {
        let value = self.score(freq.get_value().to_f32().unwrap(), norm);
        let description = format!("score(freq={}), with freq of:", freq.get_value());
        Explanation::match_(value, description, vec![freq])
    }
}
