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
use crate::test_framework::core::util::lucene_test_case::{at_least, random};
use std::collections::HashMap;

use rand::{Rng, RngExt};

use crate::core::index::BytesRef;
use crate::core::index::buffered_updates::{BufferedUpdates, DeletedTerms};
use crate::core::index::term::Term;

use crate::core::search::term_query::TermQuery;
use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::Result;

/// Unit test for BufferedUpdate
#[allow(dead_code)] // for quick search
struct TestBufferedUpdates;

#[test]
fn test_ram_bytes_used() -> Result<()> {
  let mut random = random();
  let mut bu = BufferedUpdates::new();

  assert_eq!(bu.ram_bytes_used()?, 0);
  assert!(!bu.any());

  let queries = at_least(&mut random, 1);
  for _ in 0..queries {
    let doc_id_upto = if random.random_bool(0.5) {
      i32::MAX
    } else {
      random.random_range(0..100000)
    };
    let value = format!("{}", random.random_range(0..100));
    let term = Term::new("id", BytesRef::from_string(&value));
    bu.add_query(&TermQuery::new(term).into(), doc_id_upto)?;
  }

  let terms = at_least(&mut random, 1);
  for _ in 0..terms {
    let doc_id_upto = if random.random_bool(0.5) {
      i32::MAX
    } else {
      random.random_range(0..100000)
    };
    let value = format!("{}", random.random_range(0..100));
    let term = Term::new("id", BytesRef::from_string(&value));
    bu.add_term(&term, doc_id_upto)?;
  }

  assert!(
    bu.any(),
    "We have added a lot of docIds, terms, and queries, but `any()` returned false."
  );

  let total_used = bu.ram_bytes_used()?;
  assert!(total_used > 0);

  bu.clear_delete_terms();
  assert!(
    bu.any(),
    "Only terms and docIds are cleaned, the queries should still be in memory."
  );
  assert!(
    total_used > bu.ram_bytes_used()?,
    "Terms are cleaned, so memory usage should decrease."
  );

  bu.clear();
  assert!(!bu.any());
  assert_eq!(bu.ram_bytes_used()?, 0);

  Ok(())
}
#[test]
fn test_deleted_terms() -> Result<()> {
  let mut random = random();
  let iters = at_least(&mut random, 10);
  let fields = ["a".to_string(), "b".to_string(), "c".to_string()];
  let mut actual = DeletedTerms::new();

  for _ in 0..iters {
    let mut expected = HashMap::new();
    assert!(actual.is_empty());

    let term_count = at_least(&mut random, 5000);
    let max_bytes_num = random.random_range(1..=3);

    for _ in 0..term_count {
      let byte_num = random.random_range(1..=max_bytes_num);
      let mut bytes = vec![0u8; byte_num];
      random.fill_bytes(&mut bytes);

      let field = &fields[random.random_range(0..fields.len())];
      let term = Term::new(field.clone(), BytesRef::from_bytes(bytes));
      let value = random.random_range(0..10_000_000);

      expected.insert(term.clone(), value);
      actual.put(&term, value)?;
    }

    assert_eq!(expected.len(), actual.size() as usize);

    for (term, expected_value) in &expected {
      assert_eq!(*expected_value, actual.get(term)?);
    }

    let mut expected_sorted: Vec<(Term, i32)> = expected.into_iter().collect();
    expected_sorted.sort_by(|left, right| left.0.cmp(&right.0));

    let mut actual_sorted: Vec<_> = Vec::new();
    actual.for_each_ordered(|field, bytes, doc_id| {
      let copy = Term::new(field.to_owned(), BytesRef::from_bytes(bytes.bytes.to_vec()));
      actual_sorted.push((copy, doc_id));
      Ok(())
    })?;

    assert_eq!(expected_sorted, actual_sorted);

    actual.clear();
    assert_eq!(actual.size(), 0);
    assert_eq!(actual.ram_bytes_used()?, 0);
    let pool = actual.get_pool();
    assert_eq!(pool.buffer_upto, None);
  }

  Ok(())
}
