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
/// Different modes of search.
#[derive(Debug, Clone, Copy)]
pub enum ScoreMode {
    /// Produced scorers will allow visiting all matches and get their score.
    Complete,

    /// Produced scorers will allow visiting all matches but scores won't be available.
    CompleteNoScores,

    /// Produced scorers will optionally allow skipping over non-competitive hits
    /// using the [`Scorer::set_min_competitive_score`] API.
    TopScores,

    /// ScoreMode for top field collectors that can provide their own iterators,
    /// to optionally allow to skip for non-competitive docs.
    TopDocs,

    /// ScoreMode for top field collectors that can provide their own iterators,
    /// to optionally allow to skip for non-competitive docs.
    /// This mode is used when there is a secondary sort by `_score`.
    TopDocsWithScores,
}

impl ScoreMode {
    /// Whether this [`ScoreMode`] needs to compute scores.
    pub fn needs_scores(&self) -> bool {
        match self {
            ScoreMode::Complete => true,
            ScoreMode::CompleteNoScores => false,
            ScoreMode::TopScores => true,
            ScoreMode::TopDocs => false,
            ScoreMode::TopDocsWithScores => true,
        }
    }

    /// Returns `true` if for this [`ScoreMode`] it is necessary to process all documents,
    /// or `false` if it is enough to go through top documents only.
    pub fn is_exhaustive(&self) -> bool {
        match self {
            ScoreMode::Complete => true,
            ScoreMode::CompleteNoScores => true,
            ScoreMode::TopScores => false,
            ScoreMode::TopDocs => false,
            ScoreMode::TopDocsWithScores => false,
        }
    }
}
