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
use std::borrow::Cow;
use std::cmp::{Ordering, max, min};

use crate::core::index::BytesRef;
use crate::core::index::bytes_ref::{BytesRefValue, BytesRefValueEnum};
use crate::core::index::doc_values_update::{DocValuesUpdate, DocValuesUpdateBase};
use crate::core::index::term::Term;
use crate::core::util::accountable::Accountable;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bits::{Bits, BitsEnum2, MatchAllBits};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::ram_usage_estimator::{
  size_of_array_capacity, size_of_str as ram_size_of_str, size_of_string as ram_size_of_string,
};
use crate::core::util::{
  BytesRefArray, Counter, IndexedBytesRefIterator, IndexedBytesRefIteratorImpl, NaturalOrder,
  SharedCounter, SortState, SortableBytesRefArray,
};

/// This struct efficiently buffers numeric and binary field updates and stores
/// terms, values, and metadata in a memory-efficient way without creating large
/// amounts of objects.
///
/// Update terms are stored without de-duplicating the update term. In general,
/// we try to optimize for several use-cases. For instance, we try to use
/// constant space for update terms field since the common case always updates
/// on the same field. Also, for `docUpTo`, we try to optimize for the case when
/// updates should be applied to all docs, i.e., when `docUpTo = i32::MAX`. In
/// other cases, each update will likely have a different `docUpTo`.
///
/// Along the same lines, this implementation optimizes the case when all
/// updates have a value. Lastly, if all updates share the same value for a
/// numeric field, we only store the value once.
#[derive(Debug)]
pub(crate) struct FieldUpdatesBuffer {
  bytes_used: SharedCounter,
  num_updates: usize,
  // we use a very simple approach and store the update term values without
  // de-duplication which is also not a common case to keep updating the
  // same value more than once... we might pay a higher price in terms of
  // memory in certain cases but will gain on CPU for those. We also use
  // a stable sort to sort to apply the terms in order
  // since by definition we store them in order.
  term_values: BytesRefArray,
  term_sort_state: Option<SortState>,
  values: FieldUpdateValues,
  docs_upto: smallvec::SmallVec<[i32; 1]>,
  has_values: Option<FixedBitSet>,
  first_term_field: String,
  mixed_term_fields: Option<Box<MixedTermFields>>,
  finished: bool,
}

#[derive(Debug)]
struct MixedTermFields {
  indices: Vec<usize>,
  other_names: Vec<String>,
}

impl MixedTermFields {
  fn field_name<'a>(&'a self, first: &'a String, index: usize) -> &'a String {
    if index == 0 {
      first
    } else {
      &self.other_names[index - 1]
    }
  }

  // Keep the prior Vec<String> accounting for selector names and capacity.
  fn size_of_fields(&self, first: &String) -> i64 {
    self.indices.iter().fold(
      size_of_array_capacity::<String>(self.indices.capacity()),
      |size, &index| size.saturating_add(ram_size_of_string(self.field_name(first, index))),
    )
  }
}

#[derive(Debug)]
enum FieldUpdateValues {
  Binary(Option<Box<BytesRefArray>>),
  ConstantNumeric(Option<i64>),
  MixedNumeric(Box<MixedNumericUpdates>),
}

#[derive(Debug)]
struct MixedNumericUpdates {
  values: Vec<i64>,
  max: i64,
  min: i64,
}

impl FieldUpdatesBuffer {
  fn new(
    bytes_used: SharedCounter,
    initial_value: &DocValuesUpdate,
    doc_upto: i32,
    is_numeric: bool,
  ) -> Result<Self> {
    let term = initial_value.buffered_term()?;
    let mut has_values = None;
    if !initial_value.has_value {
      let bs = FixedBitSet::new(1);
      bytes_used.add_and_get(bs.ram_bytes_used()?);
      has_values = Some(bs);
    }
    bytes_used.add_and_get(Self::size_of_string(&term.field));

    // A field can have just one small selector term. Store its bytes without a full byte block.
    let term_values = BytesRefArray::new_compact(bytes_used.clone());

    let mut buffer = FieldUpdatesBuffer {
      bytes_used,
      num_updates: 1,
      term_values,
      term_sort_state: None,
      values: if is_numeric {
        FieldUpdateValues::ConstantNumeric(None)
      } else {
        FieldUpdateValues::Binary(None)
      },
      docs_upto: smallvec::smallvec![doc_upto],
      has_values,
      first_term_field: term.field.clone(),
      mixed_term_fields: None,
      finished: false,
    };
    buffer.term_values.append(&term.bytes)?;
    Ok(buffer)
  }
  pub(crate) fn from_numeric_update(
    bytes_used: SharedCounter,
    initial_value: &DocValuesUpdate,
    doc_upto: i32,
  ) -> Result<Self> {
    let numeric = initial_value
      .sub_update
      .get_numeric()
      .ok_or_else(|| LuceneError::illegal_argument("Missing numeric value"))?;
    let value = if numeric.has_value() {
      Some(numeric.get_value()?)
    } else {
      None
    };
    let mut buffer = Self::new(bytes_used, initial_value, doc_upto, true)?;
    buffer.values = FieldUpdateValues::ConstantNumeric(value);
    {
      buffer.bytes_used.add_and_get(BitUtil::LONG_BYTES as i64);
    }
    Ok(buffer)
  }

  pub(crate) fn from_binary_update(
    bytes_used: SharedCounter,
    initial_value: &DocValuesUpdate,
    doc_upto: i32,
  ) -> Result<Self> {
    let binary = initial_value
      .sub_update
      .get_binary()
      .ok_or_else(|| LuceneError::illegal_argument("Missing binary value"))?;
    let has_values = binary.has_value();
    let value = if has_values {
      binary.get_value()?
    } else {
      &BytesRef::default()
    };
    let mut buffer = Self::new(bytes_used, initial_value, doc_upto, false)?;
    if has_values {
      buffer.append_binary_value(value)?;
    }
    Ok(buffer)
  }

  fn size_of_string(s: &String) -> i64 {
    ram_size_of_string(s)
  }

  pub(crate) fn get_max_numeric(&self) -> i64 {
    debug_assert!(self.is_numeric());
    match &self.values {
      FieldUpdateValues::ConstantNumeric(value) => value.unwrap_or(0),
      FieldUpdateValues::MixedNumeric(values) => values.max,
      _ => 0,
    }
  }

  pub(crate) fn get_min_numeric(&self) -> i64 {
    debug_assert!(self.is_numeric());
    match &self.values {
      FieldUpdateValues::ConstantNumeric(value) => value.unwrap_or(0),
      FieldUpdateValues::MixedNumeric(values) => values.min,
      _ => 0,
    }
  }
  pub(crate) fn add(
    &mut self,
    field: &str,
    doc_upto: i32,
    ord: usize,
    has_value: bool,
  ) -> Result<()> {
    debug_assert!(!self.finished, "buffer was finished already");
    if self.first_term_field != field || self.mixed_term_fields.is_some() {
      let first_field = &self.first_term_field;
      let fields = self.mixed_term_fields.get_or_insert_with(|| {
        self
          .bytes_used
          .add_and_get(size_of::<MixedTermFields>() as i64);
        Box::new(MixedTermFields {
          indices: vec![0],
          other_names: Vec::new(),
        })
      });
      let old_size = (fields.indices.len() <= ord).then(|| fields.size_of_fields(first_field));
      let field_index = if first_field == field {
        0
      } else if let Some(index) = fields.other_names.iter().position(|name| name == field) {
        index + 1
      } else {
        fields.other_names.push(field.to_owned());
        fields.other_names.len()
      };
      if let Some(old_size) = old_size {
        ArrayUtil::oversize(ord + 1, size_of::<usize>())?;
        if fields.indices.capacity() < ord + 1 {
          let capacity = (ord + 1).max(fields.indices.capacity() * 2).max(4);
          fields
            .indices
            .reserve_exact(capacity - fields.indices.len());
        }
        fields.indices.resize(ord + 1, 0);
        self
          .bytes_used
          .add_and_get(fields.size_of_fields(first_field).saturating_sub(old_size));
      } else {
        self.bytes_used.add_and_get(ram_size_of_str(field));
      }
      fields.indices[ord] = field_index;
    }

    let docs_upto_len = self.docs_upto.len();
    if self.docs_upto[0] != doc_upto || docs_upto_len != 1 {
      if docs_upto_len <= ord {
        let old_size = size_of_array_capacity::<i32>(self.docs_upto.capacity());
        let fill = if docs_upto_len == 1 {
          self.docs_upto[0]
        } else {
          0
        };
        ArrayUtil::oversize(ord + 1, size_of::<i32>())?;
        if self.docs_upto.capacity() < ord + 1 {
          let capacity = (ord + 1).max(self.docs_upto.capacity() * 2).max(4);
          self
            .docs_upto
            .reserve_exact(capacity - self.docs_upto.len());
        }
        self.docs_upto.resize(ord + 1, fill);
        self.bytes_used.add_and_get(
          size_of_array_capacity::<i32>(self.docs_upto.capacity()).saturating_sub(old_size),
        );
      }
      self.docs_upto[ord] = doc_upto;
    }

    if !has_value || self.has_values.is_some() {
      if let Some(bitset) = self.has_values.as_mut() {
        if bitset.length() <= ord {
          let old_size = bitset.ram_bytes_used()?;
          bitset.ensure_capacity(ord + 1)?;
          self
            .bytes_used
            .add_and_get(bitset.ram_bytes_used()?.saturating_sub(old_size));
        }
      } else {
        let new_bitset = FixedBitSet::with_set_prefix(ord + 1, ord);
        self.bytes_used.add_and_get(new_bitset.ram_bytes_used()?);
        self.has_values = Some(new_bitset);
      }

      if has_value {
        self
          .has_values
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("has_values bitset is missing"))?
          .set(ord)?;
      }
    }
    Ok(())
  }
  pub fn add_update_with_long(&mut self, term: &Term, value: i64, doc_upto: i32) -> Result<()> {
    debug_assert!(self.is_numeric());
    let ord = self.append(term)?;
    let field = &term.field;
    self.add(field, doc_upto, ord, true)?;
    match &mut self.values {
      FieldUpdateValues::ConstantNumeric(current) => {
        // Clear entries used a zero placeholder in the shared value slot.
        // Preserve the slot until a set operation actually differs from it.
        let fill = current.unwrap_or(0);
        if fill == value {
          *current = Some(value);
        } else {
          ArrayUtil::oversize(ord + 1, BitUtil::LONG_BYTES)?;
          let mut values = Vec::with_capacity((ord + 1).max(4));
          values.resize(ord + 1, fill);
          values[ord] = value;
          self.bytes_used.add_and_get(
            (size_of::<MixedNumericUpdates>() + (values.capacity() - 1) * BitUtil::LONG_BYTES)
              as i64,
          );
          self.values = FieldUpdateValues::MixedNumeric(Box::new(MixedNumericUpdates {
            values,
            max: current.map_or(value, |previous| max(previous, value)),
            min: current.map_or(value, |previous| min(previous, value)),
          }));
        }
      },
      FieldUpdateValues::MixedNumeric(values) => {
        values.min = min(values.min, value);
        values.max = max(values.max, value);
        let numeric_values = &mut values.values;
        if numeric_values.len() <= ord {
          let old_size = size_of_array_capacity::<i64>(numeric_values.capacity());
          ArrayUtil::oversize(ord + 1, BitUtil::LONG_BYTES)?;
          if numeric_values.capacity() < ord + 1 {
            let capacity = (ord + 1).max(numeric_values.capacity() * 2).max(4);
            numeric_values.reserve_exact(capacity - numeric_values.len());
          }
          numeric_values.resize(ord + 1, 0);
          self.bytes_used.add_and_get(
            size_of_array_capacity::<i64>(numeric_values.capacity()).saturating_sub(old_size),
          );
        }
        numeric_values[ord] = value;
      },
      FieldUpdateValues::Binary(_) => {
        return Err(LuceneError::illegal_state(
          "numeric_values is missing for numeric updates",
        ));
      },
    }
    Ok(())
  }

  pub(crate) fn add_no_value(&mut self, term: &Term, doc_upto: i32) -> Result<()> {
    let ord = self.append(term)?;
    self.add(&term.field, doc_upto, ord, false)
  }
  pub(crate) fn add_update_with_bytes_ref(
    &mut self,
    term: &Term,
    value: &BytesRef<Vec<u8>>,
    doc_upto: i32,
  ) -> Result<()> {
    debug_assert!(!self.is_numeric());
    let ord = self.append(term)?;
    self.append_binary_value(value)?;
    self.add(&term.field, doc_upto, ord, true)?;
    Ok(())
  }

  fn append_binary_value(&mut self, value: &BytesRef<Vec<u8>>) -> Result<()> {
    let FieldUpdateValues::Binary(byte_values) = &mut self.values else {
      return Err(LuceneError::illegal_state("byte_values is None"));
    };
    let values = match byte_values {
      Some(values) => values,
      slot => {
        let values = Box::new(BytesRefArray::new(self.bytes_used.clone())?);
        self
          .bytes_used
          .add_and_get(size_of::<BytesRefArray>() as i64);
        slot.insert(values)
      },
    };
    values.append(value)?;
    Ok(())
  }

  fn append(&mut self, term: &Term) -> Result<usize> {
    self.term_values.append(&term.bytes)?;
    let ord = self.num_updates;
    self.num_updates += 1;
    Ok(ord)
  }
  pub(crate) fn finish(&mut self) -> Result<()> {
    if self.finished {
      return Err(LuceneError::illegal_state("Buffer was finished already"));
    }
    self.finished = true;
    let sorted_terms = self.has_single_value()
      && self.has_values.is_none()
      && self
        .mixed_term_fields
        .as_ref()
        .is_none_or(|fields| fields.indices.len() == 1);
    if sorted_terms {
      self.term_sort_state = Some(self.term_values.sort(NaturalOrder, true)?);
      debug_assert!(self.assert_term_and_doc_in_order()?);
      self.bytes_used.add_and_get(
        self
          .term_sort_state
          .as_ref()
          .ok_or_else(|| LuceneError::illegal_state("sorted term state is missing"))?
          .ram_bytes_used()?,
      );
    }

    Ok(())
  }
  fn assert_term_and_doc_in_order(&mut self) -> Result<bool> {
    // it's used for debug_assert! , so we roughly copy data
    let mut iterator =
      IndexedBytesRefIteratorImpl::new(self.term_sort_state.as_ref(), &self.term_values);
    let mut last = None;
    let mut last_ord = 0;

    let result: Result<()> = (|| {
      while let Some((current_ord, current)) = iterator.next()? {
        let current = current.into_owned();
        if let Some(last_term) = &last {
          let cmp = current.cmp(last_term);
          debug_assert_ne!(cmp, Ordering::Less, "term in reverse order");
          let last_doc_upto = self.docs_upto[Self::get_array_index(self.docs_upto.len(), last_ord)];
          let current_doc_upto =
            self.docs_upto[Self::get_array_index(self.docs_upto.len(), current_ord)];
          debug_assert!(
            cmp != Ordering::Equal || last_doc_upto <= current_doc_upto,
            "doc id in reverse order"
          );
        }
        last = Some(current);
        last_ord = current_ord;
      }
      Ok(())
    })();
    match result {
      Ok(()) => Ok(true),
      Err(error) if error.is_io_error() => {
        debug_assert!(false, "assert_term_and_doc_in_order failed: {error:?}");
        Ok(true)
      },
      Err(error) => Err(error),
    }
  }
  pub(crate) fn iterator(&self) -> Result<BufferedUpdateIterator<'_>> {
    if !self.finished {
      return Err(LuceneError::illegal_state("Buffer was not finished"));
    }
    BufferedUpdateIterator::new(self)
  }
  pub(crate) fn is_numeric(&self) -> bool {
    !matches!(self.values, FieldUpdateValues::Binary(_))
  }
  pub(crate) fn has_single_value(&self) -> bool {
    // we only do this optimization for numerics so far.
    matches!(self.values, FieldUpdateValues::ConstantNumeric(_))
  }
  pub(crate) fn get_numeric_value(&self, idx: usize) -> Result<i64> {
    if let Some(ref has_values) = self.has_values
      && !has_values.get(idx)?
    {
      return Ok(0);
    }
    self.numeric_value(idx)
  }
  fn numeric_value(&self, idx: usize) -> Result<i64> {
    match &self.values {
      FieldUpdateValues::ConstantNumeric(value) => Ok(value.unwrap_or(0)),
      FieldUpdateValues::MixedNumeric(values) => {
        let values = &values.values;
        Ok(values[Self::get_array_index(values.len(), idx)])
      },
      FieldUpdateValues::Binary(_) => Err(LuceneError::illegal_state("numeric_values is missing")),
    }
  }
  fn get_array_index(array_length: usize, index: usize) -> usize {
    debug_assert!(
      array_length == 1 || array_length > index,
      "illegal array index length: {array_length} index: {index}"
    );
    min(array_length - 1, index)
  }
}
/// An iterator that iterates over all updates in insertion order.
pub struct BufferedUpdateIterator<'a> {
  term_values_iterator: IndexedBytesRefIteratorImpl<'a, &'a SortState>,
  look_ahead_term_iterator: Option<IndexedBytesRefIteratorImpl<'a, &'a SortState>>,
  byte_values_iterator: Option<IndexedBytesRefIteratorImpl<'a>>,
  buffered_update: BufferedUpdate<'a>,
  updates_with_value: Option<UpdateBits<'a>>,
  docs_upto_length: usize,
  field_updates_buffer: &'a FieldUpdatesBuffer,
}

impl<'a> BufferedUpdateIterator<'a> {
  pub fn new(field_updates_buffer: &'a FieldUpdatesBuffer) -> Result<Self> {
    let term_values_iterator = IndexedBytesRefIteratorImpl::new(
      field_updates_buffer.term_sort_state.as_ref(),
      &field_updates_buffer.term_values,
    );
    let look_ahead_term_iterator = field_updates_buffer
      .term_sort_state
      .as_ref()
      .map(|state| field_updates_buffer.term_values.iterator_with_state(state));
    let byte_values_iterator = match &field_updates_buffer.values {
      FieldUpdateValues::Binary(values) => values.as_deref().map(BytesRefArray::iterator),
      _ => None,
    };
    let updates_with_value = if let Some(item) = &field_updates_buffer.has_values {
      UpdateBits::B(item)
    } else {
      UpdateBits::A(MatchAllBits::new(field_updates_buffer.num_updates))
    };
    let docs_upto_length = field_updates_buffer.docs_upto.len();
    debug_assert!(docs_upto_length <= i32::MAX as usize);
    Ok(BufferedUpdateIterator {
      term_values_iterator,
      look_ahead_term_iterator,
      byte_values_iterator,
      buffered_update: BufferedUpdate::default(),
      updates_with_value: Some(updates_with_value),
      docs_upto_length,
      field_updates_buffer,
    })
  }
  /// If all updates update a single field to the same value, then we can
  /// apply these updates in the term order instead of the request order
  /// as both will yield the same result. This optimization allows us to
  /// iterate the term dictionary faster and de-duplicate updates.
  pub(crate) fn is_sorted_terms(&self) -> bool {
    self.field_updates_buffer.term_sort_state.is_some()
  }
  /// Returns the next update view, or None when all updates are consumed.
  /// The view borrows the term and binary storage until it is consumed,
  /// preventing the iterator from advancing while those bytes remain in use.
  pub(crate) fn next_value(&mut self) -> Result<Option<BorrowedBufferedUpdate<'_>>> {
    if self.next_term()? {
      let idx = self.term_values_iterator.ord();
      let buffered_update = &mut self.buffered_update;
      buffered_update.has_value = self
        .updates_with_value
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("updates_with_value is missing"))?
        .get(idx)?;
      let first_field = &self.field_updates_buffer.first_term_field;
      buffered_update.term_field = match &self.field_updates_buffer.mixed_term_fields {
        Some(fields) => {
          let index =
            fields.indices[FieldUpdatesBuffer::get_array_index(fields.indices.len(), idx)];
          fields.field_name(first_field, index)
        },
        None => first_field,
      };
      buffered_update.doc_upto = self.field_updates_buffer.docs_upto
        [FieldUpdatesBuffer::get_array_index(self.docs_upto_length, idx)];

      let mut binary_value = None;
      if buffered_update.has_value {
        if self.field_updates_buffer.is_numeric() {
          buffered_update.numeric_value = self.field_updates_buffer.numeric_value(idx)?;
        } else {
          if let Some(iterator) = &mut self.byte_values_iterator {
            binary_value = match iterator.next()? {
              Some((_, BytesRefValueEnum::Buffer(Cow::Owned(mut value)))) => {
                if value.offset != 0 {
                  value
                    .bytes
                    .copy_within(value.offset..value.offset + value.length, 0);
                }
                value.bytes.truncate(value.length);
                Some(Cow::Owned(value.bytes))
              },
              Some((_, BytesRefValueEnum::Buffer(Cow::Borrowed(value)))) => {
                Some(Cow::Borrowed(value.as_byte_slice()))
              },
              Some((_, BytesRefValueEnum::Slice(value))) => Some(Cow::Borrowed(
                &value.bytes[value.offset..value.offset + value.length],
              )),
              None => None,
            };
          }
        }
      } else {
        buffered_update.numeric_value = 0;
      }
      Ok(Some(BorrowedBufferedUpdate {
        doc_upto: buffered_update.doc_upto,
        numeric_value: buffered_update.numeric_value,
        binary_value,
        has_value: buffered_update.has_value,
        term_field: buffered_update.term_field,
        term_value: buffered_update.term_value.as_ref(),
      }))
    } else {
      Ok(None)
    }
  }

  fn next_term(&mut self) -> Result<bool> {
    if let Some(look_ahead_term_iterator) = &mut self.look_ahead_term_iterator {
      if self.buffered_update.term_value.is_none() {
        look_ahead_term_iterator.next()?;
      }
      loop {
        let ahead_term = look_ahead_term_iterator.next()?;
        let current_term = self.term_values_iterator.next()?;

        if let Some((ahead_ord, ahead)) = ahead_term
          && let Some((current_ord, current)) = current_term.as_ref()
          // Shortcut to avoid equals: stable sorting puts a later duplicate at a larger ord.
          && ahead_ord > *current_ord
          && ahead.as_bytes() == current.as_bytes()
        {
          continue;
        }

        return match current_term {
          Some((_, term)) => {
            term.copy_or_move_into(
              self
                .buffered_update
                .term_value
                .get_or_insert_with(BytesRef::default),
            );
            Ok(true)
          },
          None => Ok(false),
        };
      }
    } else {
      match self.term_values_iterator.next()? {
        Some((_, term)) => {
          term.copy_or_move_into(
            self
              .buffered_update
              .term_value
              .get_or_insert_with(BytesRef::default),
          );
          Ok(true)
        },
        None => Ok(false),
      }
    }
  }
}
/// # Warning
/// This struct should not be used as a map key or in data structures that depend on `Hash` and `Eq`.
#[derive(Default, Clone)]

pub struct BufferedUpdate<'a> {
  /// the max document ID this update should be applied to.
  pub doc_upto: i32,
  /// a numeric value or 0 if this buffer holds binary updates.
  pub numeric_value: i64,
  /// true if this update has a value.
  pub has_value: bool,
  /// The update terms field. This will never be None.
  pub term_field: &'a str,
  /// The update terms value. This will never be None.
  pub term_value: Option<BytesRef<Vec<u8>>>,
}

pub(crate) struct BorrowedBufferedUpdate<'a> {
  pub(crate) doc_upto: i32,
  pub(crate) numeric_value: i64,
  pub(crate) binary_value: Option<Cow<'a, [u8]>>,
  pub(crate) has_value: bool,
  pub(crate) term_field: &'a str,
  pub(crate) term_value: Option<&'a BytesRef<Vec<u8>>>,
}

type UpdateBits<'a> = BitsEnum2<MatchAllBits, &'a FixedBitSet>;
