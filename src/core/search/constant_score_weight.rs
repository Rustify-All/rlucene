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
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::explanation::Explanation;
use crate::core::search::query::Query;
use crate::core::search::scorer::Scorer;
use crate::core::search::two_phase_iterator::TwoPhaseIterator;
use crate::core::util::error::lucene_error::Result;
/// A Weight that has a constant score equal to the boost of the wrapped query.
/// This is typically useful when building queries which do not produce
/// meaningful scores and are mostly useful for filtering.
pub struct ConstantScoreWeight<Q>
where
    Q: Query,
{
    score: f32,
    query: Q,
}
impl<Q> ConstantScoreWeight<Q>
where
    Q: Query,
{
    pub fn new(score: f32, query: Q) -> Self {
        Self { score, query }
    }
    /// Return the score produced by this Weight.
    pub fn score(&self) -> f32 {
        self.score
    }
}
pub fn explain<S>(
    scorer: Option<&mut S>,
    doc: i32,
    score: f32,
    query_str: &str,
) -> Result<Explanation>
where
    S: Scorer,
{
    let exists = match scorer {
        None => false,
        Some(s) => {
            let has_two_phase = s.two_phase_iterator().is_some();
            if has_two_phase {
                let mut two_phase = s.two_phase_iterator().unwrap();
                two_phase.approximation().advance(doc)? == doc && two_phase.matches()?
            } else {
                s.iterator().advance(doc)? == doc
            }
        },
    };

    if exists {
        if (score - 1.0).abs() < f32::EPSILON {
            Ok(Explanation::match_(score, query_str.to_string(), vec![]))
        } else {
            Ok(Explanation::match_(
                score,
                format!("{}^{}", query_str, score),
                vec![],
            ))
        }
    } else {
        Ok(Explanation::no_match(
            format!("{} doesn't match id {}", query_str, doc),
            vec![],
        ))
    }
}
