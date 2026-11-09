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
use crate::test_framework::core::util::lucene_test_case::{new_bytes_ref_from_string, random};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use crate::core::index::BytesRef;
use crate::core::index::byte_slice_reader::ByteSliceReader;
use crate::core::index::indexing_chain::IntBlockAllocator;
use crate::core::index::terms_hash_per_field::TermsHashPerField;
use crate::core::store::DataInput;

use crate::core::util::allocator_byte::DirectAllocatorByte;
use crate::core::util::attribute_source::EmptyAttributeSource;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::int_block_pool::IntBlockPool;
use crate::core::util::{AtomicCounter, ByteBlockPool};

use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_invert_state::FieldInvertState;
use crate::core::index::freq_prox_terms_writer::FreqProxTermsWriter;
use crate::core::index::freq_prox_terms_writer_per_field::{
  FreqProxPostingsArray, FreqProxTermsWriterPerField,
};
use crate::core::index::index_options::IndexOptions;
use crate::core::index::term_vectors_consumer::TermVectorsConsumer;
use crate::test_framework::core::index::test_terms_hash_per_field::{
  TermsHashPerFieldMock, new_terms_hash_per_field_mock,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::prelude::SliceRandom;
use rand::{Rng, RngExt};

#[allow(dead_code)] // for quick search
struct TestTermsHashPerField;

fn create_new_hash(new_called: AtomicI64, add_called: AtomicI64) -> TermsHashPerFieldMock {
  new_terms_hash_per_field_mock(new_called, add_called)
}

fn assert_doc_and_freq(
  reader: &mut ByteSliceReader<'_>,
  postings_array: &FreqProxPostingsArray,
  prev_doc: i32,
  term_id: i32,
  doc: i32,
  frequency: i32,
) -> Result<bool> {
  assert!(term_id >= 0);
  let term_id = term_id as usize;
  let mut doc_id = prev_doc;
  let freq: i32;
  let eof = reader.eof();
  if eof {
    doc_id = postings_array.last_docs[term_id][0];
    match &postings_array.term_freqs {
      Some(term_freqs) => {
        freq = term_freqs[term_id];
      },
      _ => {
        return Err(LuceneError::illegal_state(
          "term_freqs is None.".to_string(),
        ));
      },
    }
  } else {
    let code = reader.read_vint()?;
    doc_id += ((code as u32) >> 1) as i32;
    if (code & 1) != 0 {
      freq = 1;
    } else {
      freq = reader.read_vint()?;
    }
  }
  assert_eq!(doc, doc_id, "docID mismatch eof: {}", eof);
  assert_eq!(frequency, freq, "freq mismatch eof: {}", eof);
  Ok(eof)
}

#[test]
fn test_add_and_update_term() -> Result<()> {
  let mut random = random();
  let new_called = AtomicI64::new(0);
  let add_called = AtomicI64::new(0);
  let mut hash: TermsHashPerFieldMock = create_new_hash(new_called, add_called);
  let mut byte_pool = ByteBlockPool::new(DirectAllocatorByte::new());
  let mut next_byte_pool = ByteBlockPool::new(DirectAllocatorByte::new());
  let mut base = hash.base.take().unwrap();
  let mut int_pool = IntBlockPool::with_allocator(IntBlockAllocator::allocator_enum(Arc::new(
    AtomicCounter::new(),
  )));

  let attribute_source = EmptyAttributeSource;
  base.add_with_bytes_ref_with_test(
    &new_bytes_ref_from_string(&mut random, "start")?,
    0,
    &mut hash,
    &attribute_source,
    &mut int_pool,
    &mut byte_pool,
    &mut next_byte_pool,
  )?;
  base.add_with_bytes_ref_with_test(
    &new_bytes_ref_from_string(&mut random, "foo")?,
    0,
    &mut hash,
    &attribute_source,
    &mut int_pool,
    &mut byte_pool,
    &mut next_byte_pool,
  )?;
  base.add_with_bytes_ref_with_test(
    &new_bytes_ref_from_string(&mut random, "bar")?,
    0,
    &mut hash,
    &attribute_source,
    &mut int_pool,
    &mut byte_pool,
    &mut next_byte_pool,
  )?;
  // base.finish();
  base.add_with_bytes_ref_with_test(
    &new_bytes_ref_from_string(&mut random, "bar")?,
    1,
    &mut hash,
    &attribute_source,
    &mut int_pool,
    &mut byte_pool,
    &mut next_byte_pool,
  )?;
  base.add_with_bytes_ref_with_test(
    &new_bytes_ref_from_string(&mut random, "foobar")?,
    1,
    &mut hash,
    &attribute_source,
    &mut int_pool,
    &mut byte_pool,
    &mut next_byte_pool,
  )?;
  base.add_with_bytes_ref_with_test(
    &new_bytes_ref_from_string(&mut random, "bar")?,
    1,
    &mut hash,
    &attribute_source,
    &mut int_pool,
    &mut byte_pool,
    &mut next_byte_pool,
  )?;
  base.add_with_bytes_ref_with_test(
    &new_bytes_ref_from_string(&mut random, "bar")?,
    1,
    &mut hash,
    &attribute_source,
    &mut int_pool,
    &mut byte_pool,
    &mut next_byte_pool,
  )?;
  base.add_with_bytes_ref_with_test(
    &new_bytes_ref_from_string(&mut random, "foobar")?,
    1,
    &mut hash,
    &attribute_source,
    &mut int_pool,
    &mut byte_pool,
    &mut next_byte_pool,
  )?;
  base.add_with_bytes_ref_with_test(
    &new_bytes_ref_from_string(&mut random, "verylongfoobarbaz")?,
    1,
    &mut hash,
    &attribute_source,
    &mut int_pool,
    &mut byte_pool,
    &mut next_byte_pool,
  )?;
  // base.finish();
  base.add_with_bytes_ref_with_test(
    &new_bytes_ref_from_string(&mut random, "verylongfoobarbaz")?,
    2,
    &mut hash,
    &attribute_source,
    &mut int_pool,
    &mut byte_pool,
    &mut next_byte_pool,
  )?;
  base.add_with_bytes_ref_with_test(
    &new_bytes_ref_from_string(&mut random, "boom")?,
    2,
    &mut hash,
    &attribute_source,
    &mut int_pool,
    &mut byte_pool,
    &mut next_byte_pool,
  )?;
  // base.finish();
  base.add_with_bytes_ref_with_test(
    &new_bytes_ref_from_string(&mut random, "verylongfoobarbaz")?,
    3,
    &mut hash,
    &attribute_source,
    &mut int_pool,
    &mut byte_pool,
    &mut next_byte_pool,
  )?;
  base.add_with_bytes_ref_with_test(
    &new_bytes_ref_from_string(&mut random, "end")?,
    3,
    &mut hash,
    &attribute_source,
    &mut int_pool,
    &mut byte_pool,
    &mut next_byte_pool,
  )?;
  // base.finish();

  assert_eq!(7, hash.new_called.load(Ordering::SeqCst));
  assert_eq!(6, hash.add_called.load(Ordering::SeqCst));

  let mut reader = ByteSliceReader::new(&byte_pool);
  base.base.init_reader(&mut reader, 0, 0, &int_pool)?;

  let postings_array = base.base.postings_array().unwrap();

  assert!(assert_doc_and_freq(
    &mut reader,
    postings_array,
    0,
    0,
    0,
    1
  )?);
  base.base.init_reader(&mut reader, 1, 0, &int_pool)?;
  assert!(assert_doc_and_freq(
    &mut reader,
    postings_array,
    0,
    1,
    0,
    1
  )?);
  base.base.init_reader(&mut reader, 2, 0, &int_pool)?;
  assert!(!assert_doc_and_freq(
    &mut reader,
    postings_array,
    0,
    2,
    0,
    1
  )?);
  assert!(assert_doc_and_freq(
    &mut reader,
    postings_array,
    2,
    2,
    1,
    3
  )?);
  base.base.init_reader(&mut reader, 3, 0, &int_pool)?;
  assert!(assert_doc_and_freq(
    &mut reader,
    postings_array,
    0,
    3,
    1,
    2
  )?);
  base.base.init_reader(&mut reader, 4, 0, &int_pool)?;
  assert!(!assert_doc_and_freq(
    &mut reader,
    postings_array,
    0,
    4,
    1,
    1
  )?);
  assert!(!assert_doc_and_freq(
    &mut reader,
    postings_array,
    1,
    4,
    2,
    1
  )?);
  assert!(assert_doc_and_freq(
    &mut reader,
    postings_array,
    2,
    4,
    3,
    1
  )?);
  base.base.init_reader(&mut reader, 5, 0, &int_pool)?;
  assert!(assert_doc_and_freq(
    &mut reader,
    postings_array,
    0,
    5,
    2,
    1
  )?);
  base.base.init_reader(&mut reader, 6, 0, &int_pool)?;
  assert!(assert_doc_and_freq(
    &mut reader,
    postings_array,
    0,
    6,
    3,
    1
  )?);
  Ok(())
}

#[test]
fn test_add_and_update_random() -> Result<()> {
  let mut random = random();
  let new_called = AtomicI64::new(0);
  let add_called = AtomicI64::new(0);
  let mut hash = create_new_hash(new_called, add_called);
  let mut byte_pool = ByteBlockPool::new(DirectAllocatorByte::new());
  let mut next_byte_pool = ByteBlockPool::new(DirectAllocatorByte::new());
  let mut int_pool = IntBlockPool::with_allocator(IntBlockAllocator::allocator_enum(Arc::new(
    AtomicCounter::new(),
  )));

  #[derive(Clone)]
  struct Posting {
    term_id: i32,
    doc_and_freq: BTreeMap<i32, i32>,
  }
  impl Posting {
    fn new() -> Self {
      Self {
        term_id: -1,
        doc_and_freq: BTreeMap::new(),
      }
    }
  }

  let mut posting_map: HashMap<BytesRef<Vec<u8>>, Posting> = HashMap::new();
  let num_strings = 1 + random.random_range(0..200);

  for _ in 0..num_strings {
    let random_string = TestUtil::random_realistic_unicode_string_range(&mut random, 1, 10);
    posting_map
      .entry(new_bytes_ref_from_string(&mut random, &random_string)?)
      .or_insert_with(Posting::new);
  }

  let mut bytes_refs: Vec<_> = posting_map.keys().cloned().collect();
  let vec_len = bytes_refs.len();
  bytes_refs.sort();

  let num_docs = 1 + random.random_range(0..200);
  let mut term_ord = 0;
  let mut base = hash.base.take().unwrap();
  for doc in 0..num_docs {
    let num_terms = 1 + random.random_range(0..200);
    for _ in 0..num_terms {
      let ref_ = bytes_refs.get(random.random_range(0..vec_len)).unwrap();
      let posting = posting_map.get_mut(ref_).unwrap();

      if posting.term_id == -1 {
        posting.term_id = term_ord;
        term_ord += 1;
      }

      posting
        .doc_and_freq
        .entry(doc)
        .and_modify(|v| *v += 1)
        .or_insert(1);
      base.add_with_bytes_ref_with_test(
        ref_,
        doc,
        &mut hash,
        &EmptyAttributeSource,
        &mut int_pool,
        &mut byte_pool,
        &mut next_byte_pool,
      )?;
    }
    // base.finish();
  }

  let mut values: Vec<_> = posting_map
    .into_values()
    .filter(|x| x.term_id != -1)
    .collect();
  values.shuffle(&mut random);
  let mut reader = ByteSliceReader::new(&byte_pool);

  let postings_array = base.base.postings_array().unwrap();
  for posting in values {
    base
      .base
      .init_reader(&mut reader, posting.term_id as usize, 0, &int_pool)?;

    let mut eof = false;
    let mut pref_doc = 0;

    for (doc, freq) in posting.doc_and_freq {
      assert!(!eof, "the reader must not be EOF here");

      eof = assert_doc_and_freq(
        &mut reader,
        postings_array,
        pref_doc,
        posting.term_id,
        doc,
        freq,
      )?;

      pref_doc = doc;
    }

    assert!(eof, "the last posting must be EOF on the reader");
  }

  Ok(())
}
#[test]
fn test_write_bytes() -> Result<()> {
  let mut random = random();
  for _ in 0..100 {
    let new_called = AtomicI64::new(0);
    let add_called = AtomicI64::new(0);
    let mut hash = create_new_hash(new_called, add_called);
    let mut byte_pool = ByteBlockPool::new(DirectAllocatorByte::new());
    let mut next_byte_pool = ByteBlockPool::new(DirectAllocatorByte::new());
    let mut base = hash.base.take().unwrap();
    let mut int_pool = IntBlockPool::with_allocator(IntBlockAllocator::allocator_enum(Arc::new(
      AtomicCounter::new(),
    )));
    let attribute_source = EmptyAttributeSource;
    base.add_with_bytes_ref_with_test(
      &new_bytes_ref_from_string(&mut random, "start")?,
      0,
      &mut hash,
      &attribute_source,
      &mut int_pool,
      &mut byte_pool,
      &mut next_byte_pool,
    )?; // tid = 0

    let size = random.random_range(50_000..=100_000);
    let mut random_data = vec![0_u8; size];
    random.fill(&mut random_data[..]);
    let mut offset = 0;
    while offset < random_data.len() {
      let write_length = std::cmp::min(random_data.len() - offset, random.random_range(1..=200));
      base.base.write_bytes(
        0,
        &random_data,
        offset,
        write_length,
        &mut int_pool,
        &mut byte_pool,
      )?;
      offset += write_length;
    }

    let mut reader = ByteSliceReader::new(&byte_pool);
    // The pool is shared with term bytes; initialize from the recorded stream
    // boundaries instead of assuming the first postings slice starts at zero.
    base.base.init_reader(&mut reader, 0, 0, &int_pool)?;
    for expected in random_data {
      assert_eq!(expected, reader.read_byte()?);
    }
    assert!(reader.eof());
  }
  Ok(())
}
