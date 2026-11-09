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
use crate::core::search::doc_id_set::DocIdSet;
use crate::core::search::doc_id_set_iterator::{AllDISI, DocIdSetIteratorEnum2};
use crate::core::util::accountable::Accountable;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bit_set_iterator::BitSetIterator;
use crate::core::util::dummy::dummy_bits::DummyBits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use std::sync::Arc;

/// Accumulator for documents that have a value for a field.
/// This is optimized for the case where all documents have a value.
pub struct DocsWithFieldSet {
  set: Option<Arc<FixedBitSet>>,
  cardinality: i32,
  last_doc_id: i32,
  finish: bool,
}
impl Default for DocsWithFieldSet {
  fn default() -> Self {
    Self::new()
  }
}

impl DocsWithFieldSet {
  pub fn new() -> DocsWithFieldSet {
    DocsWithFieldSet {
      set: None,
      cardinality: 0,
      last_doc_id: -1,
      finish: false,
    }
  }
  /// Adds a document to the set.
  ///
  /// # Parameters
  /// - `doc_id`: The document ID to be added.
  pub fn add(&mut self, doc_id: i32) -> Result<()> {
    if doc_id <= self.last_doc_id {
      return Err(LuceneError::illegal_argument(format!(
        "Out of order doc ids: last= {}, next= {}",
        self.last_doc_id, doc_id
      )));
    }
    if self.finish {
      return Err(LuceneError::illegal_state(
        "DocsWithFieldSet must not be changed after finish() is called".to_string(),
      ));
    }
    let doc_index = doc_id as usize;
    if let Some(set) = self.set.as_mut() {
      let set = Arc::get_mut(set)
        .ok_or_else(|| LuceneError::illegal_state("cannot modify a shared document set"))?;
      set.ensure_capacity(doc_index)?;
      set.set(doc_index)?;
    } else if doc_id != self.cardinality {
      let mut set = FixedBitSet::with_set_prefix((doc_id + 1) as usize, self.cardinality as usize);
      set.set(doc_index)?;
      self.set = Some(Arc::new(set));
    }

    self.last_doc_id = doc_id;
    self.cardinality += 1;
    Ok(())
  }
  /// Returns the number of documents in this set.
  pub fn cardinality(&self) -> i32 {
    self.cardinality
  }
}

impl Accountable for DocsWithFieldSet {
  fn ram_bytes_used(&self) -> Result<i64> {
    if let Some(set) = self.set.as_ref() {
      let bytes = set.ram_bytes_used()?;
      return if self.finish {
        Ok((std::mem::size_of_val(set.as_ref()) as i64).saturating_add(bytes))
      } else {
        Ok(bytes)
      };
    }
    Ok(0)
  }
}

pub(crate) type DocsWithFieldSetDISI =
  DocIdSetIteratorEnum2<AllDISI, BitSetIterator<Arc<FixedBitSet>>>;

impl DocIdSet for DocsWithFieldSet {
  type DocIdSetIterator = DocsWithFieldSetDISI;

  fn iterator(&self) -> Result<Self::DocIdSetIterator> {
    if !self.finish {
      return Err(LuceneError::illegal_state(
        "DocsWithFieldSet must be call finish() before creating an iterator",
      ));
    }
    if let Some(set) = self.set.as_ref() {
      debug_assert!(self.cardinality > 0);
      Ok(DocIdSetIteratorEnum2::B(BitSetIterator::new(
        set.clone(),
        self.cardinality as i64,
      )?))
    } else {
      Ok(DocIdSetIteratorEnum2::A(AllDISI::new(self.cardinality)))
    }
  }

  type Bits = DummyBits;

  fn bits(&self) -> Option<Self::Bits> {
    None
  }

  fn finish(&mut self) {
    self.finish = true;
  }
}
