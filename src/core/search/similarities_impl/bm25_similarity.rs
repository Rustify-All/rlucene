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
use crate::core::search::collection_statistics::CollectionStatistics;
use crate::core::search::explanation::Explanation;
use crate::core::search::similarities_impl::similarities::{SimScorer, Similarity};
use crate::core::search::term_statistics::TermStatistics;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::number::Number;
use crate::core::util::small_float::SmallFloat;
use once_cell::sync::Lazy;
use std::fmt;

/// BM25 Similarity. Introduced in Stephen E. Robertson, Steve Walker, Susan Jones, Micheline
/// Hancock-Beaulieu, and Mike Gatford. Okapi at TREC-3. In Proceedings of the Third
/// **T**ext **RE**trieval **C**onference (TREC 1994). Gaithersburg, USA, November 1994.
pub struct BM25Similarity {
    k1: f32,
    b: f32,
    discount_overlaps: bool,
}

impl BM25Similarity {
    /// BM25 with the supplied parameter values.
    ///
    /// - `k1` Controls non-linear term frequency normalization (saturation).
    /// - `b` Controls to what degree document length normalizes tf values.
    /// - `discount_overlaps` True if overlap tokens (tokens with a position increment of zero)
    ///   are discounted from the document's length.
    ///
    /// # Errors
    /// Returns `Err(String)` if `k1` is infinite or negative,
    /// or if `b` is not within the range `[0..1]`.
    pub fn new(k1: f32, b: f32, discount_overlaps: bool) -> Result<Self> {
        if !k1.is_finite() || k1 < 0.0 {
            return Err(LuceneError::illegal_argument(format!(
                "illegal k1 value: {}, must be a non-negative finite value",
                k1
            )));
        }
        if b.is_nan() || !(0.0..=1.0).contains(&b) {
            return Err(LuceneError::illegal_argument(format!(
                "illegal b value: {}, must be between 0 and 1",
                b
            )));
        }
        Ok(Self {
            k1,
            b,
            discount_overlaps,
        })
    }
    /// BM25 with the supplied parameter values, defaulting `discount_overlaps = true`.
    pub fn with_k1_b(k1: f32, b: f32) -> Result<Self> {
        Self::new(k1, b, true)
    }

    /// BM25 with default `k1 = 1.2`, `b = 0.75`,
    /// and the supplied `discount_overlaps`.
    pub fn with_discount(discount_overlaps: bool) -> Result<Self> {
        Self::new(1.2, 0.75, discount_overlaps)
    }

    /// BM25 with default `k1 = 1.2`, `b = 0.75`, `discount_overlaps = true`.
    pub fn default() -> Result<Self> {
        Self::new(1.2, 0.75, true)
    }
    /// Implemented as `log(1 + (doc_count - doc_freq + 0.5)/(doc_freq + 0.5))`.
    pub fn idf(&self, doc_freq: i64, doc_count: i64) -> f32 {
        let numerator = (doc_count - doc_freq) as f64 + 0.5;
        let denominator = doc_freq as f64 + 0.5;
        (1.0 + numerator / denominator).ln() as f32
    }

    /// The default implementation computes the average as `sum_total_term_freq / doc_count`.
    pub fn avg_field_length(&self, collection_stats: &CollectionStatistics) -> f32 {
        (collection_stats.get_sum_total_term_freq() as f64
            / collection_stats.get_doc_count() as f64) as f32
    }
    /// Computes a score factor for a simple term and returns an explanation for that score factor.
    ///
    /// The default implementation uses:
    ///
    /// ```text
    /// idf(doc_freq, doc_count);
    /// ```
    ///
    /// Note that [`CollectionStatistics::get_doc_count`] is used instead of
    /// `IndexReader::numDocs()` because also [`TermStatistics::get_doc_freq`] is used,
    /// and when the latter is inaccurate, so is [`CollectionStatistics::get_doc_count`],
    /// and in the same direction. In addition, [`CollectionStatistics::get_doc_count`]
    /// does not skew when fields are sparse.
    pub fn idf_explain(
        &self,
        collection_stats: &CollectionStatistics,
        term_stats: &TermStatistics,
    ) -> Explanation {
        let df = term_stats.get_doc_freq();
        let doc_count = collection_stats.get_doc_count();
        let idf = self.idf(df, doc_count);

        Explanation::match_(
            idf,
            "idf, computed as log(1 + (N - n + 0.5) / (n + 0.5)) from:".to_string(),
            vec![
                Explanation::match_(
                    df,
                    "n, number of documents containing term".to_string(),
                    vec![],
                ),
                Explanation::match_(
                    doc_count,
                    "N, total number of documents with field".to_string(),
                    vec![],
                ),
            ],
        )
    }
    /// Computes a score factor for a phrase.
    ///
    /// The default implementation sums the idf factor for each term in the phrase.
    pub fn idf_explain_phrase(
        &self,
        collection_stats: &CollectionStatistics,
        term_stats: &[TermStatistics],
    ) -> Explanation {
        let mut idf_sum: f64 = 0.0; // sum into f64 before casting to f32
        let mut details = Vec::with_capacity(term_stats.len());

        for stat in term_stats {
            let idf_expl = self.idf_explain(collection_stats, stat);
            // it is ok to unwrap to f64 because idf is always a small number
            idf_sum += idf_expl.get_value().to_f64().unwrap();
            details.push(idf_expl);
        }

        Explanation::match_(idf_sum as f32, "idf, sum of:".to_string(), details)
    }
    pub fn gen_k1(&self) -> f32 {
        self.k1
    }
    pub fn get_b(&self) -> f32 {
        self.b
    }
}
impl fmt::Display for BM25Similarity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BM25(k1={},b={})", self.k1, self.b)
    }
}

impl Similarity for BM25Similarity {
    fn get_discount_overlaps(&self) -> bool {
        self.discount_overlaps
    }

    type SimScorer = BM25Scorer;

    fn scorer(
        &self,
        boost: f32,
        collection_stats: &CollectionStatistics,
        term_stats: &[TermStatistics],
    ) -> Self::SimScorer {
        let idf = if term_stats.len() == 1 {
            self.idf_explain(collection_stats, &term_stats[0])
        } else {
            self.idf_explain_phrase(collection_stats, term_stats)
        };

        let avgdl = self.avg_field_length(collection_stats);

        let mut cache = [0f32; 256];
        for i in 0..256 {
            cache[i] = 1.0 / (self.k1 * ((1.0 - self.b) + self.b * LENGTH_TABLE[i] / avgdl));
        }

        BM25Scorer::new(boost, self.k1, self.b, idf, avgdl, cache)
    }
}

pub static LENGTH_TABLE: Lazy<[f32; 256]> = Lazy::new(|| {
    let mut table = [0.0; 256];
    for i in 0..256 {
        table[i] = SmallFloat::byte4_to_int(i as u8).expect("should not fail") as f32;
    }
    table
});

/// Collection statistics for the BM25 model.
pub struct BM25Scorer {
    /// query boost
    boost: f32,

    /// k1 value for scale factor
    k1: f32,

    /// b value for length normalization impact
    b: f32,

    /// BM25's idf
    idf: Explanation,

    /// The average document length
    avgdl: f32,

    /// precomputed norm[256] with k1 * ((1 - b) + b * dl / avgdl)
    cache: [f32; 256],

    /// weight (idf * boost)
    weight: f32,
}

impl BM25Scorer {
    pub fn new(
        boost: f32,
        k1: f32,
        b: f32,
        idf: Explanation,
        avgdl: f32,
        cache: [f32; 256],
    ) -> Self {
        let idf_value = idf.get_value().to_f32().unwrap();

        Self {
            boost,
            k1,
            b,
            idf,
            avgdl,
            cache,
            weight: boost * idf_value,
        }
    }
    fn explain_tf(&self, freq: Explanation, norm: i64) -> Explanation {
        let mut subs = Vec::new();
        let freq_value = freq.get_value().to_f32().unwrap();
        subs.push(freq);
        subs.push(Explanation::match_(
            self.k1,
            "k1, term saturation parameter".to_string(),
            vec![],
        ));

        let doclen = LENGTH_TABLE[(norm as u8) as usize];
        subs.push(Explanation::match_(
            self.b,
            "b, length normalization parameter".to_string(),
            vec![],
        ));

        if (norm & 0xFF) > 39 {
            subs.push(Explanation::match_(
                doclen,
                "dl, length of field (approximate)".to_string(),
                vec![],
            ));
        } else {
            subs.push(Explanation::match_(
                doclen,
                "dl, length of field".to_string(),
                vec![],
            ));
        }

        subs.push(Explanation::match_(
            self.avgdl,
            "avgdl, average length of field".to_string(),
            vec![],
        ));

        let norm_inverse = 1.0 / (self.k1 * ((1.0 - self.b) + self.b * doclen / self.avgdl));
        let tf_val = 1.0 - 1.0 / (1.0 + freq_value * norm_inverse);

        Explanation::match_(
            tf_val,
            "tf, computed as freq / (freq + k1 * (1 - b + b * dl / avgdl)) from:".to_string(),
            subs,
        )
    }

    fn explain_constant_factors(&self) -> Vec<Explanation> {
        let mut subs = Vec::new();
        if (self.boost - 1.0).abs() > f32::EPSILON {
            subs.push(Explanation::match_(
                Number::F32(self.boost),
                "boost".to_string(),
                vec![],
            ));
        }
        subs.push(self.idf.clone());
        subs
    }
}
impl SimScorer for BM25Scorer {
    /// Computes the BM25 score for a term frequency and an encoded norm.
    ///
    /// In order to guarantee monotonicity with both freq and norm without
    /// promoting to doubles, we rewrite `freq / (freq + norm)` to
    /// `1 - 1 / (1 + freq * 1/norm)`.
    ///
    /// Finally we expand `weight * (1 - 1 / (1 + freq * 1/norm))` to
    /// `weight - weight / (1 + freq * 1/norm)`, which runs slightly faster.
    fn score(&self, freq: f32, encoded_norm: i64) -> f32 {
        let norm_inverse = self.cache[(encoded_norm as u8) as usize];
        self.weight - self.weight / (1.0 + freq * norm_inverse)
    }

    fn explain(&self, freq: Explanation, encoded_norm: i64) -> Explanation {
        let mut subs = self.explain_constant_factors();
        let freq_value = freq.get_value().to_f32().unwrap();
        let tf_expl = self.explain_tf(freq, encoded_norm);
        subs.push(tf_expl);

        let norm_inverse = self.cache[(encoded_norm as u8) as usize];
        let score_val = self.weight - self.weight / (1.0 + freq_value * norm_inverse);
        // not using "product of" since the rewrite that we do in score()
        // introduces a small rounding error that CheckHits complains about
        Explanation::match_(
            score_val,
            format!(
                "score(freq={}), computed as boost * idf * tf from:",
                freq_value
            ),
            subs,
        )
    }
}
