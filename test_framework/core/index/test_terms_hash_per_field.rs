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
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_invert_state::FieldInvertState;
use crate::core::index::freq_prox_terms_writer::FreqProxTermsWriter;
use crate::core::index::freq_prox_terms_writer_per_field::FreqProxTermsWriterPerField;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::term_vectors_consumer::TermVectorsConsumer;
use crate::core::index::terms_hash_per_field::TermsHashPerField;
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::int_block_pool::IntBlockPool;
use crate::core::util::{AtomicCounter, ByteBlockPool};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

pub(crate) struct TermsHashPerFieldMock {
  pub(crate) field_state: FieldInvertState,
  pub(crate) new_called: AtomicI64,
  pub(crate) add_called: AtomicI64,
  pub(crate) base: Option<FreqProxTermsWriterPerField>,
}

impl TermsHashPerFieldMock {
  pub(crate) fn new_term(
    &mut self,
    term_id: usize,
    doc_id: i32,
    base: &mut TermsHashPerField,
  ) -> crate::core::util::error::lucene_error::Result<()> {
    self.new_called.fetch_add(1, Ordering::SeqCst);
    let f = base.postings_array_mut().unwrap();
    f.last_docs[term_id][0] = doc_id;
    f.last_docs[term_id][1] = doc_id << 1;
    match &mut f.term_freqs {
      Some(term_freqs) => {
        term_freqs[term_id] = 1;
      },
      None => unreachable!(),
    }
    Ok(())
  }

  pub(crate) fn add_term(
    &mut self,
    term_id: usize,
    doc_id: i32,
    base: &mut TermsHashPerField,
    int_pool: &mut IntBlockPool,
    byte_pool: &mut ByteBlockPool,
  ) -> crate::core::util::error::lucene_error::Result<()> {
    self.add_called.fetch_add(1, Ordering::SeqCst);
    let mut v = Vec::new();
    let mut need_write = false;
    let postings = base.postings_array_mut().unwrap();
    if doc_id != postings.last_docs[term_id][0] {
      match &mut postings.term_freqs {
        Some(term_freqs) => {
          need_write = true;
          if 1 == term_freqs[term_id] {
            v.push(postings.last_docs[term_id][1] | 1);
          } else {
            v.push(postings.last_docs[term_id][1]);
            v.push(term_freqs[term_id]);
          }
          term_freqs[term_id] = 1;
        },
        None => unreachable!(),
      }
      postings.last_docs[term_id][1] = (doc_id - postings.last_docs[term_id][0]) << 1;
      postings.last_docs[term_id][0] = doc_id;
    } else {
      match &mut postings.term_freqs {
        Some(term_freqs) => {
          let value = term_freqs[term_id] as i64 + 1;
          if value > i32::MAX as i64 {
            return Err(LuceneError::number_overflow("term_freqs"));
          }
          term_freqs[term_id] += 1;
        },
        None => unreachable!(),
      }
    }
    if need_write {
      for x in v {
        base.write_vint(0, x, int_pool, byte_pool)?;
      }
    }
    Ok(())
  }
}

pub(crate) fn new_terms_hash_per_field_mock(
  new_called: AtomicI64,
  add_called: AtomicI64,
) -> TermsHashPerFieldMock {
  let bytes_used = Arc::new(AtomicCounter::new());
  let writer = FreqProxTermsWriter::new(bytes_used, TermVectorsConsumer::default());

  let field_state = FieldInvertState::default();
  let mut field_info = FieldInfo::default();
  field_info.index_options = IndexOptions::DocsAndFreqs;

  let base = FreqProxTermsWriterPerField::new(&writer, Arc::new(field_info), None).unwrap();

  TermsHashPerFieldMock {
    field_state,
    new_called,
    add_called,
    base: Option::from(base),
  }
}
