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
#![allow(rustdoc::invalid_html_tags)]

use crate::core::search::collection_statistics::CollectionStatistics;
use crate::core::search::explanation::Explanation;
use crate::core::search::similarities_impl::classic_similarity::ClassicSimilarity;
use crate::core::search::similarities_impl::similarities::{SimScorer, Similarity};
use crate::core::search::term_statistics::TermStatistics;
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::small_float::SmallFloat;
use std::fmt::{Display, Formatter};
use std::sync::{Arc, LazyLock};

static LENGTH_TABLE: LazyLock<[i32; 256]> = LazyLock::new(|| {
  let mut table = [0i32; 256];
  for (i, slot) in table.iter_mut().enumerate() {
    *slot = expect_invariant!(
      SmallFloat::byte4_to_int(i as u8),
      "every u8 value is valid for the TF-IDF length decoding table"
    );
  }
  table
});
/// Implementation of [`Similarity`] with the Vector Space Model.
///
/// <p>Expert: Scoring API.
///
/// <p>TFIDFSimilarity defines the components of Lucene scoring. Customizing these
/// components is a convenient way to alter Lucene scoring.
///
/// <p>Suggested reading: <a
/// href="http://nlp.stanford.edu/IR-book/html/htmledition/queries-as-vectors-1.html">Introduction To
/// Information Retrieval, Chapter 6</a>.
///
/// <p>The following describes how Lucene scoring evolves from underlying information retrieval
/// models to (efficient) implementation. We first brief on <i>VSM Score</i>, then derive from it
/// <i>Lucene's Conceptual Scoring Formula</i>, from which, finally, evolves <i>Lucene's Practical
/// Scoring Function</i> (the latter is connected directly with Lucene types and methods).
///
/// <p>Lucene combines <a href="http://en.wikipedia.org/wiki/Standard_Boolean_model">Boolean model
/// (BM) of Information Retrieval</a> with <a href="http://en.wikipedia.org/wiki/Vector_Space_Model">
/// Vector Space Model (VSM) of Information Retrieval</a> - documents "approved" by BM are scored by
/// VSM.
///
/// <p>In VSM, documents and queries are represented as weighted vectors in a multi-dimensional
/// space, where each distinct index term is a dimension, and weights are <a
/// href="http://en.wikipedia.org/wiki/Tfidf">Tf-idf</a> values.
///
/// <p>VSM does not require weights to be <i>Tf-idf</i> values, but <i>Tf-idf</i> values are believed
/// to produce search results of high quality, and so Lucene is using <i>Tf-idf</i>. <i>Tf</i> and
/// <i>Idf</i> are described in more detail below, but for now, for completion, let's just say that
/// for given term <i>t</i> and document (or query) <i>x</i>, <i>Tf(t,x)</i> varies with the number
/// of occurrences of term <i>t</i> in <i>x</i> (when one increases so does the other) and
/// <i>idf(t)</i> similarly varies with the inverse of the number of index documents containing term
/// <i>t</i>.
///
/// <p><i>VSM score</i> of document <i>d</i> for query <i>q</i> is the <a
/// href="http://en.wikipedia.org/wiki/Cosine_similarity">Cosine Similarity</a> of the weighted query
/// vectors <i>V(q)</i> and <i>V(d)</i>: <br>
/// &nbsp;<br>
///
/// <table class="padding2" style="border-spacing: 2px; border-collapse: separate; border: 0; width:auto; margin-left:auto; margin-right:auto">
///    <caption>formatting only</caption>
///    <tr><td>
///    <table class="padding1" style="border-spacing: 0px; border-collapse: separate; border: 1px solid; margin-left:auto; margin-right:auto">
///      <caption>formatting only</caption>
///      <tr><td>
///      <table class="padding2" style="border-spacing: 2px; border-collapse: separate; border: 0; margin-left:auto; margin-right:auto">
///        <caption>cosine similarity formula</caption>
///        <tr>
///          <td style="vertical-align: middle; text-align: right" rowspan="1">
///            cosine-similarity(q,d) &nbsp; = &nbsp;
///          </td>
///          <td style="vertical-align: middle; text-align: center">
///            <table>
///               <caption>cosine similarity formula</caption>
///               <tr><td style="text-align: center"><small>V(q)&nbsp;&middot;&nbsp;V(d)</small></td></tr>
///               <tr><td style="text-align: center">&ndash;&ndash;&ndash;&ndash;&ndash;&ndash;&ndash;&ndash;&ndash;</td></tr>
///               <tr><td style="text-align: center"><small>|V(q)|&nbsp;|V(d)|</small></td></tr>
///            </table>
///          </td>
///        </tr>
///      </table>
///      </td></tr>
///    </table>
///    </td></tr>
///    <tr><td>
///    <u style="text-align: center">VSM Score</u>
///    </td></tr>
///  </table>
///
/// <br>
/// &nbsp;<br>
/// Where <i>V(q)</i> &middot; <i>V(d)</i> is the <a
/// href="http://en.wikipedia.org/wiki/Dot_product">dot product</a> of the weighted vectors, and
/// <i>|V(q)|</i> and <i>|V(d)|</i> are their <a
/// href="http://en.wikipedia.org/wiki/Euclidean_norm#Euclidean_norm">Euclidean norms</a>.
///
/// <p>Note: the above equation can be viewed as the dot product of the normalized weighted vectors,
/// in the sense that dividing <i>V(q)</i> by its euclidean norm is normalizing it to a unit vector.
///
/// <p>Lucene refines <i>VSM score</i> for both search quality and usability:
///
/// <ul>
///   <li>Normalizing <i>V(d)</i> to the unit vector is known to be problematic in that it removes
///       all document length information. For some documents removing this info is probably ok, e.g.
///       a document made by duplicating a certain paragraph <i>10</i> times, especially if that
///       paragraph is made of distinct terms. But for a document which contains no duplicated
///       paragraphs, this might be wrong. To avoid this problem, a different document length
///       normalization factor is used, which normalizes to a vector equal to or larger than the unit
///       vector: <i>doc-len-norm(d)</i>.
///   <li>At indexing, users can specify that certain documents are more important than others, by
///       assigning a document boost. For this, the score of each document is also multiplied by its
///       boost value <i>doc-boost(d)</i>.
///   <li>Lucene is field based, hence each query term applies to a single field, document length
///       normalization is by the length of the certain field, and in addition to document boost
///       there are also document fields boosts.
///   <li>The same field can be added to a document during indexing several times, and so the boost
///       of that field is the multiplication of the boosts of the separate additions (or parts) of
///       that field within the document.
///   <li>At search time users can specify boosts to each query, sub-query, and each query term,
///       hence the contribution of a query term to the score of a document is multiplied by the
///       boost of that query term <i>query-boost(q)</i>.
///   <li>A document may match a multi term query without containing all the terms of that query
///       (this is correct for some of the queries).
/// </ul>
///
/// <p>Under the simplifying assumption of a single field in the index, we get <i>Lucene's Conceptual
/// scoring formula</i>: <br>
/// &nbsp;<br>
///
/// <table class="padding2" style="border-spacing: 2px; border-collapse: separate; border: 0; width:auto; margin-left:auto; margin-right:auto">
///    <caption>formatting only</caption>
///    <tr><td>
///    <table class="padding1" style="border-spacing: 0px; border-collapse: separate; border: 1px solid; margin-left:auto; margin-right:auto">
///      <caption>formatting only</caption>
///      <tr><td>
///      <table class="padding2" style="border-spacing: 2px; border-collapse: separate; border: 0; margin-left:auto; margin-right:auto">
///        <caption>formatting only</caption>
///        <tr>
///          <td style="vertical-align: middle; text-align: right" rowspan="1">
///            score(q,d) &nbsp; = &nbsp;
///            <span style="color: #CCCC00">query-boost(q)</span> &middot; &nbsp;
///          </td>
///          <td style="vertical-align: middle; text-align: center">
///            <table>
///               <caption>Lucene conceptual scoring formula</caption>
///               <tr><td style="text-align: center"><small><span style="color: #993399">V(q)&nbsp;&middot;&nbsp;V(d)</span></small></td></tr>
///               <tr><td style="text-align: center">&ndash;&ndash;&ndash;&ndash;&ndash;&ndash;&ndash;&ndash;&ndash;</td></tr>
///               <tr><td style="text-align: center"><small><span style="color: #FF33CC">|V(q)|</span></small></td></tr>
///            </table>
///          </td>
///          <td style="vertical-align: middle; text-align: right" rowspan="1">
///            &nbsp; &middot; &nbsp; <span style="color: #3399FF">doc-len-norm(d)</span>
///            &nbsp; &middot; &nbsp; <span style="color: #3399FF">doc-boost(d)</span>
///          </td>
///        </tr>
///      </table>
///      </td></tr>
///    </table>
///    </td></tr>
///    <tr><td>
///    <u style="text-align: center">Lucene Conceptual Scoring Formula</u>
///    </td></tr>
///  </table>
///
/// <br>
/// &nbsp;<br>
///
/// <p>The conceptual formula is a simplification in the sense that (1) terms and documents are
/// fielded and (2) boosts are usually per query term rather than per query.
///
/// <p>We now describe how Lucene implements this conceptual scoring formula, and derive from it
/// <i>Lucene's Practical Scoring Function</i>.
///
/// <p>For efficient score computation some scoring components are computed and aggregated in
/// advance:
///
/// <ul>
///   <li><i>Query-boost</i> for the query (actually for each query term) is known when search
///       starts.
///   <li>Query Euclidean norm <i>|V(q)|</i> can be computed when search starts, as it is independent
///       of the document being scored. From search optimization perspective, it is a valid question
///       why bother to normalize the query at all, because all scored documents will be multiplied
///       by the same <i>|V(q)|</i>, and hence documents ranks (their order by score) will not be
///       affected by this normalization. There are two good reasons to keep this normalization:
///       <ul>
///         <li>Recall that <a href="http://en.wikipedia.org/wiki/Cosine_similarity">Cosine
///             Similarity</a> can be used find how similar two documents are. One can use Lucene for
///             e.g. clustering, and use a document as a query to compute its similarity to other
///             documents. In this use case it is important that the score of document <i>d3</i> for
///             query <i>d1</i> is comparable to the score of document <i>d3</i> for query <i>d2</i>.
///             In other words, scores of a document for two distinct queries should be comparable.
///             There are other applications that may require this. And this is exactly what
///             normalizing the query vector <i>V(q)</i> provides: comparability (to a certain
///             extent) of two or more queries.
///       </ul>
///   <li>Document length norm <i>doc-len-norm(d)</i> and document boost <i>doc-boost(d)</i> are
///       known at indexing time. They are computed in advance and their multiplication is saved as a
///       single value in the index: <i>norm(d)</i>. (In the equations below, <i>norm(t in d)</i>
///       means <i>norm(field(t) in doc d)</i> where <i>field(t)</i> is the field associated with
///       term <i>t</i>.)
/// </ul>
///
/// <p><i>Lucene's Practical Scoring Function</i> is derived from the above. The color codes
/// demonstrate how it relates to those of the <i>conceptual</i> formula:
///
/// <table class="padding2" style="border-spacing: 2px; border-collapse: separate; border: 0; width:auto; margin-left:auto; margin-right:auto">
///  <caption>formatting only</caption>
///  <tr><td>
///  <table style="border-spacing: 2px; border-collapse: separate; border: 2px solid; margin-left:auto; margin-right:auto">
///  <caption>formatting only</caption>
///  <tr><td>
///   <table class="padding2" style="border-spacing: 2px; border-collapse: separate; border: 0; margin-left:auto; margin-right:auto">
///   <caption>Lucene conceptual scoring formula</caption>
///   <tr>
///     <td style="vertical-align: middle; text-align: right" rowspan="1">
///       score(q,d) &nbsp; = &nbsp;
///       <span style="font-size: larger">&sum;</span>
///     </td>
///     <td style="vertical-align: middle; text-align: right" rowspan="1">
///       <span style="font-size: larger">(</span>
///       <A HREF="#formula_tf"><span style="color: #993399">tf(t in d)</span></A> &nbsp;&middot;&nbsp;
///       <A HREF="#formula_idf"><span style="color: #993399">idf(t)</span></A><sup>2</sup> &nbsp;&middot;&nbsp;
///       <A HREF="#formula_termBoost"><span style="color: #CCCC00">term boost</span></A>&nbsp;&middot;&nbsp;
///       <A HREF="#formula_norm"><span style="color: #3399FF">norm(t,d)</span></A>
///       <span style="font-size: larger">)</span>
///     </td>
///   </tr>
///   <tr style="vertical-align: top">
///    <td></td>
///    <td style="text-align: center"><small>t in q</small></td>
///    <td></td>
///   </tr>
///   </table>
///  </td></tr>
///  </table>
/// </td></tr>
/// <tr><td>
///  <u style="text-align: center">Lucene Practical Scoring Function</u>
/// </td></tr>
/// </table>
///
/// <p>where
///
/// <ol>
///   <li><a id="formula_tf"></A> <b><i>tf(t in d)</i></b> correlates to the term's <i>frequency</i>,
///       defined as the number of times term <i>t</i> appears in the currently scored document
///       <i>d</i>. Documents that have more occurrences of a given term receive a higher score. Note
///       that <i>tf(t in q)</i> is assumed to be <i>1</i> and therefore it does not appear in this
///       equation, However if a query contains twice the same term, there will be two term-queries
///       with that same term and hence the computation would still be correct (although not very
///       efficient). The default computation for <i>tf(t in d)</i> in [`ClassicSimilarity::tf`] is:
///       <br>
///       &nbsp;<br>
///       <table class="padding2" style="border-spacing: 2px; border-collapse: separate; border: 0; width:auto; margin-left:auto; margin-right:auto">
///        <caption>term frequency computation</caption>
///        <tr>
///          <td style="vertical-align: middle; text-align: right" rowspan="1">
///            [`ClassicSimilarity::tf`] &nbsp; = &nbsp;
///          </td>
///          <td style="vertical-align: top; text-align: center" rowspan="1">
///               frequency<sup><span style="font-size: larger">&frac12;</span></sup>
///          </td>
///        </tr>
///      </table>
///       <br>
///       &nbsp;<br>
///   <li><a id="formula_idf"></A> <b><i>idf(t)</i></b> stands for Inverse Document Frequency. This
///       value correlates to the inverse of <i>doc_freq</i> (the number of documents in which the
///       term <i>t</i> appears). This means rarer terms give higher contribution to the total score.
///       <i>idf(t)</i> appears for <i>t</i> in both the query and the document, hence it is squared
///       in the equation. The default computation for <i>idf(t)</i> in [`ClassicSimilarity::idf`] is:
///       <br>
///       &nbsp;<br>
///       <table class="padding2" style="border-spacing: 2px; border-collapse: separate; border: 0; width:auto; margin-left:auto; margin-right:auto">
///        <caption>inverse document frequency computation</caption>
///        <tr>
///          <td style="vertical-align: middle; text-align: right">
///            [`ClassicSimilarity::idf`]&nbsp; = &nbsp;
///          </td>
///          <td style="vertical-align: middle; text-align: center">
///            1 + log <span style="font-size: larger">(</span>
///          </td>
///          <td style="vertical-align: middle; text-align: center">
///            <table>
///               <caption>inverse document frequency computation</caption>
///               <tr><td style="text-align: center"><small>docCount+1</small></td></tr>
///               <tr><td style="text-align: center">&ndash;&ndash;&ndash;&ndash;&ndash;&ndash;&ndash;&ndash;&ndash;</td></tr>
///               <tr><td style="text-align: center"><small>doc_freq+1</small></td></tr>
///            </table>
///          </td>
///          <td style="vertical-align: middle; text-align: center">
///            <span style="font-size: larger">)</span>
///          </td>
///        </tr>
///      </table>
///       <br>
///       &nbsp;<br>
///   <li><a id="formula_termBoost"></A> The <b><i>term boost</i></b> is a search-time boost of term
///       <i>t</i> in query <i>q</i>, as specified by the query syntax or by wrapping with
///       [`BoostQuery`]. There is no direct API for accessing the boost of one term in
///       a multi term query, but rather multi terms are represented in a query as multi [`TermQuery`]
///       objects, and so the boost of a term in the query is accessible by calling the sub-query
///       [`BoostQuery::get_boost`]. <br>
///       &nbsp;<br>
///   <li><a id="formula_norm"></A> <b><i>norm(t,d)</i></b> is an index-time boost factor that solely
///       depends on the number of tokens of this field in the document, so that shorter fields
///       contribute more to the score.
/// </ol>
///
/// See also [`IndexWriter::add_document_with_similarity`](crate::core::index::index_writer::IndexWriter::add_document_with_similarity) and [`IndexSearcher::set_similarity`](crate::core::search::index_searcher::IndexSearcher::set_similarity).
#[derive(Clone)]
pub struct TFIDFSimilarity {
  sub: TFIDFSubEnum,
  discount_overlaps: bool,
}
impl TFIDFSimilarity {
  pub fn new(sub: TFIDFSubEnum) -> Self {
    Self {
      sub,
      discount_overlaps: true,
    }
  }
  pub fn with_discount_overlaps(sub: TFIDFSubEnum, discount_overlaps: bool) -> Self {
    Self {
      sub,
      discount_overlaps,
    }
  }
}

impl Display for TFIDFSimilarity {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>(),)
  }
}

impl Similarity for TFIDFSimilarity {
  fn get_discount_overlaps(&self) -> bool {
    self.discount_overlaps
  }

  type SimScorer = TFIDFScorer;

  fn scorer(
    &self,
    boost: f32,
    collection_stats: &CollectionStatistics,
    term_stats: &[TermStatistics],
  ) -> Result<Self::SimScorer> {
    let idf = if term_stats.len() == 1 {
      self.sub.idf_explain(collection_stats, &term_stats[0])
    } else {
      self
        .sub
        .idf_explain_from_multi_ts(collection_stats, term_stats)
    };

    let mut norm_table = vec![0f32; 256];
    let length_table = &*LENGTH_TABLE;
    for i in 1..256 {
      let norm = self.sub.length_norm(length_table[i]);
      norm_table[i] = norm;
    }
    norm_table[0] = 1f32 / norm_table[255];

    TFIDFScorer::new(boost, idf, norm_table, self.sub.clone())
  }
}

pub struct TFIDFScorer {
  idf: Explanation,
  boost: f32,
  query_weight: f32,
  pub(crate) norm_table: Box<[f32]>,
  base: TFIDFSubEnum,
}
impl TFIDFScorer {
  pub fn new(
    boost: f32,
    idf: Explanation,
    norm_table: Vec<f32>,
    base: TFIDFSubEnum,
  ) -> Result<Self> {
    let idf_value = idf
      .value
      .to_f32()
      .ok_or_else(|| LuceneError::illegal_argument(format!("invalid idf#value: {}", idf.value)))?;

    let query_weight = boost * idf_value;

    Ok(Self {
      idf,
      boost,
      query_weight,
      norm_table: norm_table.into_boxed_slice(),
      base,
    })
  }
  fn explain_score(
    &self,
    freq: Explanation,
    encoded_norm: i64,
    norm_table: &[f32],
  ) -> Result<Explanation> {
    let mut subs = Vec::new();

    if self.boost != 1.0 {
      subs.push(Explanation::match_no_details(self.boost, "boost"));
    }

    subs.push(self.idf.clone());
    let freq_value = freq
      .value
      .to_f32()
      .ok_or_else(|| LuceneError::illegal_argument(format!("invalid idf#value: {}", freq.value)))?;
    let value = self.base.tf(freq_value);
    let tf = Explanation::match_(
      value,
      format!("tf(freq={}), with freq of:", freq_value),
      vec![freq],
    );

    let tf_value = tf.value.to_f32().ok_or_else(|| {
      LuceneError::illegal_argument(format!("invalid idf#value: {}", tf.get_details()[0].value))
    })?;
    subs.push(tf);

    let idx = (encoded_norm & 0xFF) as usize;
    let norm = norm_table[idx];

    let field_norm = Explanation::match_no_details(norm, "fieldNorm");
    subs.push(field_norm);

    let score = self.query_weight * tf_value * norm;
    Ok(Explanation::match_(
      score,
      format!("score(freq={}), product of:", freq_value),
      subs,
    ))
  }
}
impl SimScorer for TFIDFScorer {
  fn score(&self, freq: f32, norm: i64) -> f32 {
    let raw = self.base.tf(freq) * self.query_weight;
    let norm_value = self.norm_table[(norm & 0xFF) as usize];
    raw * norm_value
  }

  fn explain(&self, freq: Explanation, norm: i64) -> Result<Explanation> {
    self.explain_score(freq, norm, &self.norm_table)
  }
}
#[derive(Clone)]
pub enum TFIDFSubEnum {
  Classic(ClassicSimilarity),
  Custom(Arc<dyn TFIDFSimilarityBase + Send + Sync>),
}
impl TFIDFSubEnum {
  pub fn custom<T>(similarity: T) -> Self
  where
    T: TFIDFSimilarityBase + Send + Sync + 'static,
  {
    Self::Custom(Arc::new(similarity))
  }
}
impl TFIDFSimilarityBase for TFIDFSubEnum {
  fn tf(&self, freq: f32) -> f32 {
    match self {
      TFIDFSubEnum::Classic(classic) => classic.tf(freq),
      TFIDFSubEnum::Custom(custom) => custom.tf(freq),
    }
  }

  fn idf_explain(
    &self,
    collection_stats: &CollectionStatistics,
    term_stats: &TermStatistics,
  ) -> Explanation {
    match self {
      TFIDFSubEnum::Classic(classic) => classic.idf_explain(collection_stats, term_stats),
      TFIDFSubEnum::Custom(custom) => custom.idf_explain(collection_stats, term_stats),
    }
  }

  fn idf_explain_from_multi_ts(
    &self,
    collection_stats: &CollectionStatistics,
    term_stats: &[TermStatistics],
  ) -> Explanation {
    match self {
      TFIDFSubEnum::Classic(classic) => {
        classic.idf_explain_from_multi_ts(collection_stats, term_stats)
      },
      TFIDFSubEnum::Custom(custom) => {
        custom.idf_explain_from_multi_ts(collection_stats, term_stats)
      },
    }
  }

  fn idf(&self, doc_freq: i64, doc_count: i64) -> f32 {
    match self {
      TFIDFSubEnum::Classic(classic) => classic.idf(doc_freq, doc_count),
      TFIDFSubEnum::Custom(custom) => custom.idf(doc_freq, doc_count),
    }
  }

  fn length_norm(&self, length: i32) -> f32 {
    match self {
      TFIDFSubEnum::Classic(classic) => classic.length_norm(length),
      TFIDFSubEnum::Custom(custom) => custom.length_norm(length),
    }
  }
}
pub trait TFIDFSimilarityBase {
  /// Computes a score factor based on a term or phrase's frequency in a document. This value is
  /// multiplied by the [`Self::idf`] factor for each term in the query and these products
  /// are then summed to form the initial score for a document.
  ///
  /// Terms and phrases repeated in a document indicate the topic of the document, so
  /// implementations of this method usually return larger values when `freq` is large,
  /// and smaller values when `freq` is small.
  ///
  /// # Arguments
  ///
  /// * `freq` - the frequency of a term within a document
  ///
  /// # Returns
  ///
  /// A score factor based on a term's within-document frequency
  fn tf(&self, freq: f32) -> f32;
  /// Computes a score factor for a simple term and returns an explanation for that score factor.
  ///
  /// The default implementation uses:
  ///
  /// ```text
  /// idf(doc_freq, doc_count)
  /// ```
  ///
  /// Note that [`CollectionStatistics::get_doc_count`] is used instead of
  /// `IndexReader::num_docs()` because also [`TermStatistics::get_doc_freq`] is used,
  /// and when the latter is inaccurate, so is [`CollectionStatistics::get_doc_count`],
  /// and in the same direction. In addition, [`CollectionStatistics::get_doc_count`]
  /// does not skew when fields are sparse.
  ///
  /// # Arguments
  ///
  /// * `collection_stats` - collection-level statistics
  /// * `term_stats` - term-level statistics for the term
  ///
  /// # Returns
  ///
  /// An [`Explanation`] that includes both an idf score factor and an explanation
  /// for the term.
  fn idf_explain(
    &self,
    collection_stats: &CollectionStatistics,
    term_stats: &TermStatistics,
  ) -> Explanation {
    let df = term_stats.get_doc_freq();
    let doc_count = collection_stats.get_doc_count();
    let idf = self.idf(df, doc_count);

    Explanation::match_(
      idf,
      "idf(doc_freq, doc_count)",
      vec![
        Explanation::match_no_details(df, "doc_freq, number of documents containing term"),
        Explanation::match_no_details(doc_count, "doc_count, total number of documents with field"),
      ],
    )
  }
  /// Computes a score factor for a phrase.
  ///
  /// The default implementation sums the idf factor for each term in the phrase.
  ///
  /// # Arguments
  ///
  /// * `collection_stats` - collection-level statistics
  /// * `term_stats` - term-level statistics for the terms in the phrase
  ///
  /// # Returns
  ///
  /// An [`Explanation`] that includes both an idf score factor for the phrase
  /// and an explanation for each term.
  fn idf_explain_from_multi_ts(
    &self,
    collection_stats: &CollectionStatistics,
    term_stats: &[TermStatistics],
  ) -> Explanation {
    let mut idf = 0f64;
    let mut subs = Vec::with_capacity(term_stats.len());

    for stat in term_stats {
      let idf_explain = self.idf_explain(collection_stats, stat);
      let v = match idf_explain.value.to_f32() {
        Some(v) => v,
        None => {
          return Explanation::error_explanation(format!(
            "idf value {} can not convert to f32",
            idf_explain.value
          ));
        },
      };
      idf += v as f64;
      subs.push(idf_explain);
    }
    Explanation::match_(idf as f32, "idf(), sum of:", subs)
  }
  /// Computes a score factor based on a term's document frequency (the number of documents which
  /// contain the term). This value is multiplied by the [`Self::tf`] factor for each term in
  /// the query and these products are then summed to form the initial score for a document.
  ///
  /// <p>Terms that occur in fewer documents are better indicators of topic, so implementations of
  /// this method usually return larger values for rare terms, and smaller values for common terms.
  ///
  /// # Arguments
  ///
  /// * `doc_freq` - the number of documents which contain the term
  /// * `doc_count` - the total number of documents in the collection
  ///
  /// # Returns
  ///
  /// A score factor based on the term's document frequency.
  fn idf(&self, doc_freq: i64, doc_count: i64) -> f32;
  /// Compute an index-time normalization value for this field instance.
  ///
  /// # Arguments
  ///
  /// * `length` - the number of terms in the field, optionally
  ///   [`Similarity::get_discount_overlaps`] discounting overlaps
  ///
  /// # Returns
  ///
  /// A length normalization value.
  fn length_norm(&self, length: i32) -> f32;
}
