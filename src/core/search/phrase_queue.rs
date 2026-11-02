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
use crate::core::search::phrase_positions::PhrasePositions;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::priority_queue::Compare;

pub(crate) struct PhraseQueueCmp {
  pub(crate) phrase_positions: Box<[PhrasePositions]>,
}
impl PhraseQueueCmp {
  pub(crate) fn new(pp: Vec<PhrasePositions>) -> Self {
    Self {
      phrase_positions: pp.into_boxed_slice(),
    }
  }
}
impl Compare<usize> for PhraseQueueCmp {
  fn less_than(&self, a: &usize, b: &usize) -> Result<bool> {
    let pp1 = &self.phrase_positions[*a];
    let pp2 = &self.phrase_positions[*b];
    if pp1.position == pp2.position {
      // same doc and pp.position, so decide by actual term positions.
      // rely on: pp.position == tp.position - offset.
      if pp1.offset == pp2.offset {
        Ok(pp1.ord < pp2.ord)
      } else {
        Ok(pp1.offset < pp2.offset)
      }
    } else {
      Ok(pp1.position < pp2.position)
    }
  }
}
