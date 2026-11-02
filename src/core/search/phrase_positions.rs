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
use crate::core::index::postings_enum::PostingsEnum;
use crate::core::index::term::Term;
use crate::core::util::error::lucene_error::Result;

/// Position of a term in a document that takes into account the term offset
/// within the phrase.
pub struct PhrasePositions {
  /// Position in the document.
  pub(crate) position: i32,
  /// Remaining positions in this document.
  pub(crate) count: i32,
  /// Position in the phrase.
  pub(crate) offset: i32,
  /// Unique ordinal across all `PhrasePositions` instances.
  pub(crate) ord: usize,
  pub(crate) postings_idx: usize,
  /// Repetition group identifier.
  /// None indicates that this is not a repeating `PhrasePositions`.
  pub(crate) rpt_group: Option<usize>,
  /// Index within the repetition group.
  pub(crate) rpt_ind: usize,
  /// Terms associated with this position, used for repetition initialization.
  pub(crate) terms: Box<[Term]>,
}
impl PhrasePositions {
  pub(crate) fn new(postings: usize, offset: i32, ord: usize, terms: Vec<Term>) -> Result<Self> {
    Ok(Self {
      postings_idx: postings,
      offset,
      ord,
      terms: terms.into_boxed_slice(),
      position: 0,
      count: 0,
      rpt_group: None,
      rpt_ind: 0,
    })
  }

  pub(crate) fn first_position<PE: PostingsEnum>(&mut self, postings: &mut PE) -> Result<()> {
    // read first position
    self.count = postings.freq()?;
    self.next_position(postings)?;
    Ok(())
  }

  /// Go to next location of this term in the current document, and set
  /// `position` as `location - offset`, so that a matching exact phrase is
  /// easily identified when all `PhrasePositions` have exactly the same
  /// `position`.
  pub(crate) fn next_position<PE: PostingsEnum>(&mut self, postings: &mut PE) -> Result<bool> {
    let count = self.count;
    self.count = count.wrapping_sub(1);
    if count > 0 {
      self.position = postings.next_position()? - self.offset;
      Ok(true)
    } else {
      Ok(false)
    }
  }
}
