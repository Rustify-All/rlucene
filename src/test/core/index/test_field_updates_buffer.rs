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
use crate::test_framework::core::util::lucene_test_case::{random, rarely};
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::core::index::BytesRef;
use crate::core::index::buffered_updates::MAX_INT;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::doc_values_update::{
  BinaryDocValuesUpdate, DocValuesUpdate, DocValuesUpdateEnum, NumericDocValuesUpdate,
};
use crate::core::index::field_updates_buffer::FieldUpdatesBuffer;
use crate::core::index::term::Term;
use crate::core::util::AtomicCounter;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::util::test_util::TestUtil;

use rand::Rng;
use rand::RngExt;

#[allow(dead_code)] // for quick search
struct TestFieldUpdatesBuffer;

#[test]
pub fn test_basics() -> Result<()> {
  let counter = Arc::new(AtomicCounter::new());
  let update = DocValuesUpdate::new(
    DocValuesType::Numeric,
    Term::from_text("id", "1"),
    "age",
    MAX_INT,
    DocValuesUpdateEnum::Numeric(NumericDocValuesUpdate::new(Option::from(6))),
  );
  let mut buffer = FieldUpdatesBuffer::from_numeric_update(counter, &update, 15)?;
  buffer.add_update_with_long(&Term::from_text("id", "10"), 6, 15)?;
  assert!(buffer.has_single_value());
  buffer.add_update_with_long(&Term::from_text("id", "8"), 12, 15)?;
  assert!(!buffer.has_single_value());
  buffer.add_update_with_long(&Term::from_text("some_other_field", "8"), 13, 17)?;
  assert!(!buffer.has_single_value());
  buffer.add_update_with_long(&Term::from_text("id", "8"), 12, 16)?;
  assert!(!buffer.has_single_value());
  assert!(buffer.is_numeric());
  assert_eq!(buffer.get_max_numeric(), 13);
  assert_eq!(buffer.get_min_numeric(), 6);
  buffer.finish()?;
  let mut iterator = buffer.iterator()?;
  let mut count = 0;
  while let Some(value) = iterator.next_value()? {
    match count {
      0 => {
        assert_eq!(value.term_field, "id");
        assert_eq!(value.term_value.as_ref().unwrap().utf8_to_string()?, "1");
        assert_eq!(value.numeric_value, 6);
        assert_eq!(value.doc_upto, 15);
      },
      1 => {
        assert_eq!(value.term_field, "id");
        assert_eq!(value.term_value.as_ref().unwrap().utf8_to_string()?, "10");
        assert_eq!(value.numeric_value, 6);
        assert_eq!(value.doc_upto, 15);
      },
      2 => {
        assert_eq!(value.term_field, "id");
        assert_eq!(value.term_value.as_ref().unwrap().utf8_to_string()?, "8");
        assert_eq!(value.numeric_value, 12);
        assert_eq!(value.doc_upto, 15);
      },
      3 => {
        assert_eq!(value.term_field, "some_other_field");
        assert_eq!(value.term_value.as_ref().unwrap().utf8_to_string()?, "8");
        assert_eq!(value.numeric_value, 13);
        assert_eq!(value.doc_upto, 17);
      },
      4 => {
        assert_eq!(value.term_field, "id");
        assert_eq!(value.term_value.as_ref().unwrap().utf8_to_string()?, "8");
        assert_eq!(value.numeric_value, 12);
        assert_eq!(value.doc_upto, 16);
      },
      _ => unreachable!(),
    }
    count += 1;
  }
  assert_eq!(count, 5);
  Ok(())
}
#[test]
fn test_update_share_values() -> Result<()> {
  let mut random = random();
  let counter = Arc::new(AtomicCounter::new());
  let int_value = random.random::<i32>() as i64;
  let value_for_three = random.random_bool(0.5);
  let sub_update =
    DocValuesUpdateEnum::Numeric(NumericDocValuesUpdate::new(Option::from(int_value)));
  let update = DocValuesUpdate::new(
    DocValuesType::Numeric,
    Term::from_text("id", "0"),
    "enabled",
    MAX_INT,
    sub_update,
  );
  let mut buffer = FieldUpdatesBuffer::from_numeric_update(counter, &update, i32::MAX)?;
  buffer.add_update_with_long(&Term::from_text("id", "1"), int_value, i32::MAX)?;
  buffer.add_update_with_long(&Term::from_text("id", "2"), int_value, i32::MAX)?;
  if value_for_three {
    buffer.add_update_with_long(&Term::from_text("id", "3"), int_value, i32::MAX)?;
  } else {
    buffer.add_no_value(&Term::from_text("id", "3"), i32::MAX)?;
  }
  buffer.add_update_with_long(&Term::from_text("id", "4"), int_value, i32::MAX)?;
  buffer.finish()?;

  let mut iterator = buffer.iterator()?;
  let mut count = 0;
  while let Some(value) = iterator.next_value()? {
    let has_value = count != 3 || value_for_three;
    assert_eq!(
      count.to_string(),
      value.term_value.as_ref().unwrap().utf8_to_string()?
    );
    assert_eq!("id", value.term_field);
    assert_eq!(has_value, value.has_value);
    if has_value {
      assert_eq!(int_value, value.numeric_value);
    } else {
      assert_eq!(0, value.numeric_value);
    }
    assert_eq!(i32::MAX, value.doc_upto);
    count += 1;
  }
  assert!(buffer.is_numeric());
  Ok(())
}
#[test]
pub fn test_update_share_values_binary() -> Result<()> {
  let mut random = random();
  let counter = Arc::new(AtomicCounter::new());
  let value_for_three = random.random_bool(0.5);
  let sub_update = DocValuesUpdateEnum::Binary(BinaryDocValuesUpdate::new(Option::from(
    BytesRef::from_string(""),
  )));
  let update = DocValuesUpdate::new(
    DocValuesType::Binary,
    Term::from_text("id", "0"),
    "enabled",
    MAX_INT,
    sub_update,
  );
  let mut buffer = FieldUpdatesBuffer::from_binary_update(counter, &update, i32::MAX)?;
  buffer.add_update_with_bytes_ref(
    &Term::from_text("id", "1"),
    &BytesRef::from_string(""),
    i32::MAX,
  )?;
  buffer.add_update_with_bytes_ref(
    &Term::from_text("id", "2"),
    &BytesRef::from_string(""),
    i32::MAX,
  )?;
  if value_for_three {
    buffer.add_update_with_bytes_ref(
      &Term::from_text("id", "3"),
      &BytesRef::from_string(""),
      i32::MAX,
    )?;
  } else {
    buffer.add_no_value(&Term::from_text("id", "3"), i32::MAX)?;
  }

  buffer.add_update_with_bytes_ref(
    &Term::from_text("id", "4"),
    &BytesRef::from_string(""),
    i32::MAX,
  )?;
  buffer.finish()?;
  let mut iterator = buffer.iterator()?;
  let mut count = 0;
  while let Some(value) = iterator.next_value()? {
    let has_value = count != 3 || value_for_three;
    assert_eq!(
      count.to_string(),
      value.term_value.as_ref().unwrap().utf8_to_string()?
    );
    assert_eq!("id", value.term_field);
    assert_eq!(has_value, value.has_value);

    if has_value {
      assert_eq!(
        b"".as_slice(),
        value.binary_value.as_ref().unwrap().as_ref()
      );
    } else {
      assert!(value.binary_value.is_none());
    }
    assert_eq!(i32::MAX, value.doc_upto);
    count += 1;
  }
  assert!(!buffer.is_numeric());
  Ok(())
}
pub fn random_from<T, R>(random: &mut R, items: &[T]) -> T
where
  T: Clone,
  R: Rng + ?Sized,
{
  let index = random.random_range(0..items.len());
  items[index].clone()
}
pub fn get_random_binary_update<R>(random: &mut R, doc_id_upto: i32) -> DocValuesUpdate
where
  R: Rng + ?Sized,
{
  let term_field = random_from(random, &["id", "_id", "some_other_field"]);
  let doc_id = random.random_range(0..10).to_string();

  let value = if rarely(random) {
    None
  } else {
    Some(BytesRef::from_string(
      &TestUtil::random_realistic_unicode_string(random),
    ))
  };

  let sub_update = DocValuesUpdateEnum::Binary(BinaryDocValuesUpdate::new(value));
  let mut update = DocValuesUpdate::new(
    DocValuesType::Binary,
    Term::from_text(term_field.to_string(), &doc_id),
    "enabled",
    MAX_INT,
    sub_update,
  );
  if rarely(random) {
    let result = update.prepare_for_apply(doc_id_upto);
    result.unwrap_or(update)
  } else {
    update
  }
}
pub fn get_random_numeric_update<R>(random: &mut R, doc_id_upto: i32) -> DocValuesUpdate
where
  R: Rng + ?Sized,
{
  let term_field = random_from(random, &["id", "_id", "some_other_field"]);
  let doc_id = random.random_range(0..10).to_string();

  let value = if rarely(random) {
    None
  } else {
    Some(random.random_range(0..100) as i64)
  };

  let sub_update = DocValuesUpdateEnum::Numeric(NumericDocValuesUpdate::new(value));
  let mut update = DocValuesUpdate::new(
    DocValuesType::Numeric,
    Term::from_text(term_field.to_string(), &doc_id),
    "numeric".to_string(),
    MAX_INT,
    sub_update,
  );

  if rarely(random) {
    let result = update.prepare_for_apply(doc_id_upto);
    result.unwrap_or(update)
  } else {
    update
  }
}

#[test]
pub fn test_binary_random() -> Result<()> {
  let mut random = random();
  let mut updates = Vec::new();
  let num_updates = 1 + random.random_range(0..1000);
  let counter = Arc::new(AtomicCounter::new());

  let mut random_update = get_random_binary_update(&mut random, 0);

  let doc_id_upto = random_update.doc_id_upto;
  let mut buffer = FieldUpdatesBuffer::from_binary_update(counter, &random_update, doc_id_upto)?;
  updates.push(random_update);

  for i in 0..num_updates {
    random_update = get_random_binary_update(&mut random, i + 1);
    let doc_id_upto = random_update.doc_id_upto;

    if random_update.has_value {
      buffer.add_update_with_bytes_ref(
        random_update.buffered_term()?,
        random_update.sub_update.get_binary().unwrap().get_value()?,
        doc_id_upto,
      )?;
    } else {
      buffer.add_no_value(random_update.buffered_term()?, doc_id_upto)?;
    }
    updates.push(random_update);
  }
  buffer.finish()?;

  let mut iterator = buffer.iterator()?;
  let mut count = 0;

  while let Some(value) = iterator.next_value()? {
    let random_update = &updates[count];
    count += 1;
    assert_eq!(
      random_update.buffered_term()?.bytes.utf8_to_string()?,
      value.term_value.as_ref().unwrap().utf8_to_string()?
    );
    assert_eq!(random_update.buffered_term()?.field, value.term_field);
    assert_eq!(random_update.has_value, value.has_value, "count: {}", count);

    if random_update.has_value {
      assert_eq!(
        random_update
          .sub_update
          .get_binary()
          .unwrap()
          .get_value()?
          .as_byte_slice(),
        value.binary_value.as_ref().unwrap().as_ref()
      );
    } else {
      assert!(value.binary_value.is_none());
    }
    assert_eq!(random_update.doc_id_upto, value.doc_upto);
  }
  assert_eq!(updates.len(), count);
  Ok(())
}
#[test]
pub fn test_numeric_random() -> Result<()> {
  let mut random = random();
  let mut updates = Vec::new();
  let num_updates = 1 + random.random_range(0..1000);
  let counter = Arc::new(AtomicCounter::new());

  let mut random_update = get_random_numeric_update(&mut random, 0);

  let doc_id_upto = random_update.doc_id_upto;
  let mut buffer = FieldUpdatesBuffer::from_numeric_update(counter, &random_update, doc_id_upto)?;
  updates.push(random_update);

  for i in 0..num_updates {
    random_update = get_random_numeric_update(&mut random, i + 1);
    // last
    let doc_id_upto = random_update.doc_id_upto;

    if random_update.has_value {
      buffer.add_update_with_long(
        random_update.buffered_term()?,
        random_update
          .sub_update
          .get_numeric()
          .unwrap()
          .get_value()?,
        doc_id_upto,
      )?;
    } else {
      buffer.add_no_value(random_update.buffered_term()?, doc_id_upto)?;
    }
    updates.push(random_update);
  }
  buffer.finish()?;
  let last_update = updates.last().unwrap();
  let last_term_field = &last_update.buffered_term()?.field;
  let terms_sorted = last_update.has_value
    && updates.iter().all(|update| {
      // The target DocValues field can match even when update term fields differ.
      update.buffered_term().unwrap().field.as_str() == last_term_field.as_str()
        && update.has_value
        && update
          .sub_update
          .get_numeric()
          .unwrap()
          .get_value()
          .unwrap()
          == last_update
            .sub_update
            .get_numeric()
            .unwrap()
            .get_value()
            .unwrap()
    });

  assert_buffer_updates(&buffer, updates, terms_sorted)?;

  Ok(())
}
#[test]
pub fn test_no_numeric_value() -> Result<()> {
  let update = DocValuesUpdate::new(
    DocValuesType::Numeric,
    Term::from_text("id", "1"),
    "age".to_string(),
    MAX_INT,
    DocValuesUpdateEnum::Numeric(NumericDocValuesUpdate::new(None)),
  );

  let counter = Arc::new(AtomicCounter::new());
  let doc_id_upto = update.doc_id_upto;
  let buffer = FieldUpdatesBuffer::from_numeric_update(counter, &update, doc_id_upto)?;

  assert_eq!(buffer.get_min_numeric(), 0);
  assert_eq!(buffer.get_max_numeric(), 0);

  Ok(())
}
#[test]
pub fn test_sort_and_dedup_numeric_updates_by_terms() -> Result<()> {
  let mut random = random();
  let mut updates = Vec::new();
  let num_updates = 1 + random.random_range(0..1000);
  let counter = Arc::new(AtomicCounter::new());

  let term_field = random_from(&mut random, &["id", "_id", "some_other_field"]);
  let doc_value = 1 + random.random_range(0..1000);

  let mut random_update = DocValuesUpdate::new(
    DocValuesType::Numeric,
    Term::from_text(
      term_field.to_string(),
      random.random_range(0..1000).to_string(),
    ),
    "numeric".to_string(),
    MAX_INT,
    DocValuesUpdateEnum::Numeric(NumericDocValuesUpdate::new(Some(doc_value))),
  );
  if let Some(v) = random_update.prepare_for_apply(0) {
    random_update = v
  }
  let doc_id_upto = random_update.doc_id_upto;
  let mut buffer = FieldUpdatesBuffer::from_numeric_update(counter, &random_update, doc_id_upto)?;
  updates.push(random_update);
  for i in 0..num_updates {
    random_update = DocValuesUpdate::new(
      DocValuesType::Numeric,
      Term::from_text(
        term_field.to_string(),
        random.random_range(0..1000).to_string(),
      ),
      "numeric".to_string(),
      MAX_INT,
      DocValuesUpdateEnum::Numeric(NumericDocValuesUpdate::new(Some(doc_value))),
    );
    if let Some(v) = random_update.prepare_for_apply(i + 1) {
      random_update = v
    }
    buffer.add_update_with_long(
      random_update.buffered_term()?,
      doc_value,
      random_update.doc_id_upto,
    )?;
    updates.push(random_update);
  }

  buffer.finish()?;

  // We can now assert that the buffer updates are correct after sorting
  // and deduplication
  assert_buffer_updates(&buffer, updates, true)?;

  Ok(())
}

fn assert_buffer_updates(
  buffer: &FieldUpdatesBuffer,
  mut updates: Vec<DocValuesUpdate>,
  term_sorted: bool,
) -> Result<()> {
  let updates: Vec<_> = if term_sorted {
    updates.sort_by(|a, b| {
      a.buffered_term()
        .unwrap()
        .bytes
        .cmp(&b.buffered_term().unwrap().bytes)
    });

    let mut by_terms: BTreeMap<&BytesRef<Vec<u8>>, &DocValuesUpdate> = BTreeMap::new();
    for update in &updates {
      by_terms
        .entry(&update.buffered_term()?.bytes)
        .and_modify(|v| {
          if v.doc_id_upto < update.doc_id_upto {
            *v = update;
          }
        })
        .or_insert(update);
    }

    by_terms.values().copied().collect()
  } else {
    updates.iter().collect()
  };

  let mut iterator = buffer.iterator()?;
  let mut count = 0;
  let mut min = i64::MAX;
  let mut max = i64::MIN;
  let mut has_at_least_one_value = false;

  while let Some(value) = iterator.next_value()? {
    let v = buffer.get_numeric_value(count)?;
    let expected_update = &updates[count];
    count += 1;
    assert_eq!(
      expected_update.buffered_term()?.bytes.utf8_to_string()?,
      value.term_value.as_ref().unwrap().utf8_to_string()?
    );
    assert_eq!(expected_update.buffered_term()?.field, value.term_field);
    assert_eq!(expected_update.has_value, value.has_value);

    if expected_update.has_value {
      let expected_value = expected_update
        .sub_update
        .get_numeric()
        .unwrap()
        .get_value()?;
      assert_eq!(expected_value, value.numeric_value);
      assert_eq!(v, value.numeric_value);
      min = min.min(v);
      max = max.max(v);
      has_at_least_one_value = true;
    } else {
      assert_eq!(0, value.numeric_value);
      assert_eq!(0, v)
    }
    assert_eq!(expected_update.doc_id_upto, value.doc_upto);
  }
  if has_at_least_one_value {
    assert_eq!(max, buffer.get_max_numeric());
    assert_eq!(min, buffer.get_min_numeric());
  } else {
    assert_eq!(0, buffer.get_min_numeric());
    assert_eq!(0, buffer.get_max_numeric());
  }
  assert_eq!(updates.len(), count);
  Ok(())
}
