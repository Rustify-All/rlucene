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
use std::cmp::{Ordering, max, min};
use std::sync::Arc;

use crate::core::index::BytesRef;
use crate::core::index::doc_values_update::{DocValuesUpdate, DocValuesUpdateBase};
use crate::core::index::term::Term;
use crate::core::util::access::SharedAccess;
use crate::core::util::accountable::Accountable;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bits::{Bits, Either2Bits, MatchAllBits};
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::{
    BytesRefArray, Counter, CounterEnumLock, IndexedBytesRefIteratorImpl, MTBytesRefArray,
    NaturalOrder, SortState, SortableBytesRefArray,
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
    bytes_used: CounterEnumLock,
    num_updates: i32,
    // we use a very simple approach and store the update term values without
    // de-duplication which is also not a common case to keep updating the
    // same value more than once... we might pay a higher price in terms of
    // memory in certain cases but will gain on CPU for those. We also use
    // a stable sort to sort to apply the terms in order
    // since by definition we store them in order.
    term_values: MTBytesRefArray,
    term_sort_state: Arc<SortState>,
    byte_values: Option<MTBytesRefArray>, /* this will be null if we are
                                           * buffering numerics  */
    docs_upto: Vec<i32>,
    numeric_values: Option<Vec<i64>>, /* this will be null if we are
                                       * buffering binaries  */
    has_values: Option<FixedBitSet>,
    max_numeric: i64,
    min_numeric: i64,
    fields: Vec<String>,
    is_numeric: bool,
    finished: bool,
}

impl FieldUpdatesBuffer {
    const SELF_SHALLOW_SIZE: i64 = 0;

    const STRING_SHALLOW_SIZE: i64 = 0;
    fn new(
        bytes_used: CounterEnumLock,
        initial_value: &DocValuesUpdate,
        doc_upto: i32,
        is_numeric: bool,
    ) -> Result<Self> {
        let has_values = if !initial_value.has_value {
            Some(FixedBitSet::new(1))
        } else {
            None
        };
        bytes_used.access_mut(|bytes_used_guard| {
            bytes_used_guard.add_and_get(Self::size_of_string(&initial_value.term.field));
            if !initial_value.has_value {
                bytes_used_guard.add_and_get(has_values.as_ref().unwrap().ram_bytes_used()?);
            }
            // Help the compiler infer types.
            Ok::<(), LuceneError>(())
        })?;
        let mut buffer = FieldUpdatesBuffer {
            bytes_used: bytes_used.clone(),
            num_updates: 1,
            term_values: BytesRefArray::new_sync(bytes_used.clone())?,
            term_sort_state: Arc::new(SortState::new(None)),
            byte_values: if is_numeric {
                None
            } else {
                Some(BytesRefArray::new_sync(bytes_used.clone())?)
            },
            docs_upto: vec![doc_upto],
            numeric_values: if is_numeric { Some(vec![]) } else { None },
            has_values,
            max_numeric: i64::MIN,
            min_numeric: i64::MAX,
            // TODO: we should estimate the size of the fields array
            fields: vec![initial_value.term.field.clone()],
            is_numeric,
            finished: false,
        };
        buffer.term_values.append(&initial_value.term.bytes)?;
        Ok(buffer)
    }
    pub(crate) fn from_numeric_update(
        bytes_used: CounterEnumLock,
        initial_value: &DocValuesUpdate,
        doc_upto: i32,
    ) -> Result<Self> {
        let numeric = initial_value
            .sub_update
            .get_numeric()
            .ok_or_else(|| LuceneError::illegal_argument("Missing numeric value"))?;
        let has_values = numeric.has_value();
        let (numeric_values, max_numeric, min_numeric) = if has_values {
            let value = numeric.get_value();
            (vec![value], value, value)
        } else {
            (vec![0], i64::MIN, i64::MAX)
        };
        let mut buffer = Self::new(bytes_used, initial_value, doc_upto, true)?;
        buffer.numeric_values = Some(numeric_values);
        buffer.max_numeric = max_numeric;
        buffer.min_numeric = min_numeric;
        {
            buffer
                .bytes_used
                .lock()
                .add_and_get(BitUtil::LONG_BYTES as i64);
        }
        Ok(buffer)
    }

    pub(crate) fn from_binary_update(
        bytes_used: CounterEnumLock,
        initial_value: &DocValuesUpdate,
        doc_upto: i32,
    ) -> Result<Self> {
        let binary = initial_value
            .sub_update
            .get_binary()
            .ok_or_else(|| LuceneError::illegal_argument("Missing binary value"))?;
        let has_values = binary.has_value();
        let value = if has_values {
            binary.get_value()
        } else {
            BytesRef::default()
        };
        let mut buffer = Self::new(bytes_used, initial_value, doc_upto, false)?;
        if has_values {
            debug_assert!(buffer.byte_values.is_some());
            buffer.byte_values.as_mut().unwrap().append(&value)?;
        }
        Ok(buffer)
    }

    fn size_of_string(_s: &str) -> i64 {
        //TODO: memory calculation not implemented
        0
    }

    pub(crate) fn get_max_numeric(&self) -> i64 {
        debug_assert!(self.is_numeric);
        if self.min_numeric == i64::MAX && self.max_numeric == i64::MIN {
            return 0;
        }
        self.max_numeric
    }

    pub(crate) fn get_min_numeric(&self) -> i64 {
        debug_assert!(self.is_numeric);
        if self.min_numeric == i64::MAX && self.max_numeric == i64::MIN {
            return 0;
        }
        self.min_numeric
    }
    pub(crate) fn add(
        &mut self,
        field: String,
        doc_upto: i32,
        ord: i32,
        has_value: bool,
    ) -> Result<()> {
        debug_assert!(!self.finished, "buffer was finished already");
        let fields_len = self.fields.len();
        if self.fields[0] != field || fields_len != 1 {
            let mut bytes_used = self.bytes_used.lock();
            if fields_len <= ord as usize {
                ArrayUtil::grow_with_len(&mut self.fields, (ord + 1) as usize);
                if fields_len == 1 {
                    for i in 1..ord as usize {
                        self.fields[i] = self.fields[0].clone();
                    }
                }
                // TODO: memory calculation not implemented
                bytes_used.add_and_get(0);
            }
            if self.fields[0] != field {
                bytes_used.add_and_get(field.len() as i64);
            }
            self.fields[ord as usize] = field;
        }

        let docs_upto_len = self.docs_upto.len();
        if self.docs_upto[0] != doc_upto || docs_upto_len != 1 {
            if docs_upto_len <= ord as usize {
                ArrayUtil::grow_with_len(&mut self.docs_upto, (ord + 1) as usize);
                if docs_upto_len == 1 {
                    for i in 1..ord as usize {
                        self.docs_upto[i] = self.docs_upto[0];
                    }
                }
                // TODO: memory calculation not implemented
                self.bytes_used.lock().add_and_get(0);
            }
            self.docs_upto[ord as usize] = doc_upto;
        }

        if !has_value || self.has_values.is_some() {
            let mut bytes_used = self.bytes_used.lock();
            if self.has_values.is_none() {
                let mut new_bitset = FixedBitSet::new(ord + 1);
                new_bitset.set_with_range(0, ord);
                bytes_used.add_and_get(new_bitset.ram_bytes_used()?);
                self.has_values = Some(new_bitset);
            } else if self.has_values.as_ref().unwrap().length() <= ord {
                let bitset = self.has_values.as_mut().unwrap();
                bitset.ensure_capacity(ord + 1);
                // TODO: memory calculation not implemented
                bytes_used.add_and_get(0);
            }
            if has_value {
                self.has_values.as_mut().unwrap().set(ord);
            }
        }
        Ok(())
    }
    pub fn add_update_with_long(&mut self, term: &Term, value: i64, doc_upto: i32) -> Result<()> {
        debug_assert!(self.is_numeric);
        let ord = self.append(term)?;
        let field = term.field.clone();
        self.add(field, doc_upto, ord, true)?;
        self.min_numeric = min(self.min_numeric, value);
        self.max_numeric = max(self.max_numeric, value);
        let numeric_values = self.numeric_values.as_mut().unwrap();
        let numeric_values_len = numeric_values.len();
        if numeric_values[0] != value || numeric_values_len != 1 {
            if numeric_values_len <= ord as usize {
                ArrayUtil::grow_with_len(numeric_values, (ord + 1) as usize);
                if numeric_values_len == 1 {
                    for i in 1..ord as usize {
                        numeric_values[i] = numeric_values[0];
                    }
                }
                // TODO: memory calculation not implemented
                self.bytes_used.lock().add_and_get(0);
            }
            numeric_values[ord as usize] = value;
        }
        Ok(())
    }

    pub(crate) fn add_no_value(&mut self, term: &Term, doc_upto: i32) -> Result<()> {
        let ord = self.append(term)?;
        self.add(term.field.clone(), doc_upto, ord, false)
    }
    pub(crate) fn add_update_with_bytes_ref(
        &mut self,
        term: &Term,
        value: &BytesRef<Vec<u8>>,
        doc_upto: i32,
    ) -> Result<()> {
        debug_assert!(!self.is_numeric);
        debug_assert!(self.byte_values.is_some());
        let ord = self.append(term)?;
        self.byte_values.as_mut().unwrap().append(value)?;
        self.add(term.field.clone(), doc_upto, ord, true)?;
        Ok(())
    }

    fn append(&mut self, term: &Term) -> Result<i32> {
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
        let sorted_terms =
            self.has_single_value() && self.has_values.is_none() && self.fields.len() == 1;
        if sorted_terms {
            self.term_sort_state = Arc::new(self.term_values.sort(NaturalOrder::default(), true)?);
            debug_assert!(self.assert_term_and_doc_in_order());
            // TODO: memory calculation not implemented
            self.bytes_used.lock().add_and_get(0);
        }

        Ok(())
    }
    fn assert_term_and_doc_in_order(&mut self) -> bool {
        // it's used for debug_assert! , so we roughly copy data
        let mut iterator = self
            .term_values
            .iterator_with_state(self.term_sort_state.clone());
        let mut last = None;
        let mut last_ord = 0;

        let result: Result<()> = (|| {
            while let Some(current) = iterator.next()? {
                let current = current.into_owned();
                if let Some(last_term) = &last {
                    let cmp = current.cmp(last_term);
                    debug_assert_ne!(cmp, Ordering::Less, "term in reverse order");
                    let last_doc_upto = self.docs_upto
                        [Self::get_array_index(self.docs_upto.len() as i32, last_ord) as usize];
                    let current_doc_upto = self.docs_upto[Self::get_array_index(
                        self.docs_upto.len() as i32,
                        iterator.ord(),
                    ) as usize];
                    debug_assert!(
                        cmp != Ordering::Equal || last_doc_upto <= current_doc_upto,
                        "doc id in reverse order"
                    );
                }
                last = Some(current);
                last_ord = iterator.ord();
            }
            Ok(())
        })();
        debug_assert!(
            result.is_ok(),
            "assert_term_and_doc_in_order failed: {:?}",
            result.err()
        );
        true
    }
    pub(crate) fn iterator(&self) -> Result<BufferedUpdateIterator<'_>> {
        if !self.finished {
            return Err(LuceneError::illegal_state("Buffer was not finished"));
        }
        Ok(BufferedUpdateIterator::new(self))
    }
    pub(crate) fn is_numeric(&self) -> bool {
        debug_assert!(self.is_numeric || self.byte_values.is_some());
        self.is_numeric
    }
    pub(crate) fn has_single_value(&self) -> bool {
        // we only do this optimization for numerics so far.
        self.is_numeric && self.numeric_values.as_ref().unwrap().len() == 1
    }
    pub(crate) fn get_numeric_value(&self, idx: i32) -> i64 {
        if let Some(ref has_values) = self.has_values
            && !has_values.get(idx)
        {
            return 0;
        }
        assert!(self.numeric_values.is_some());
        let length = self.numeric_values.as_ref().unwrap().len();
        debug_assert!(length <= i32::MAX as usize);
        self.numeric_values.as_ref().unwrap()[Self::get_array_index(length as i32, idx) as usize]
    }
    fn get_array_index(array_length: i32, index: i32) -> i32 {
        assert!(
            array_length == 1 || array_length > index,
            "illegal array index length: {array_length} index: {index}"
        );
        min(array_length - 1, index)
    }
}
/// An iterator that iterates over all updates in insertion order.
pub struct BufferedUpdateIterator<'a> {
    term_values_iterator: IndexedBytesRefIteratorImpl<'a, CounterEnumLock>,
    look_ahead_term_iterator: Option<IndexedBytesRefIteratorImpl<'a, CounterEnumLock>>,
    byte_values_iterator: Option<IndexedBytesRefIteratorImpl<'a, CounterEnumLock>>,
    buffered_update: BufferedUpdate,
    updates_with_value: Option<UpdateBits<'a>>,
    fields_length: i32,
    docs_upto_length: i32,
    numeric_values_length: i32,
    field_updates_buffer: &'a FieldUpdatesBuffer,
}

impl<'a> BufferedUpdateIterator<'a> {
    pub fn new(field_updates_buffer: &'a FieldUpdatesBuffer) -> Self {
        let term_values_iterator = field_updates_buffer
            .term_values
            .iterator_with_state(field_updates_buffer.term_sort_state.clone());
        let look_ahead_term_iterator = if field_updates_buffer.term_sort_state.indices.is_some() {
            Some(
                field_updates_buffer
                    .term_values
                    .iterator_with_state(field_updates_buffer.term_sort_state.clone()),
            )
        } else {
            None
        };
        let byte_values_iterator = if field_updates_buffer.is_numeric {
            None
        } else {
            debug_assert!(field_updates_buffer.byte_values.is_some());
            Some(
                field_updates_buffer
                    .byte_values
                    .as_ref()
                    .unwrap()
                    .iterator(),
            )
        };
        let updates_with_value = if let Some(item) = &field_updates_buffer.has_values {
            UpdateBits::B(item)
        } else {
            UpdateBits::A(MatchAllBits::new(field_updates_buffer.num_updates))
        };
        let fields_length = field_updates_buffer.fields.len();
        let docs_upto_length = field_updates_buffer.docs_upto.len();
        let numeric_values_length = if field_updates_buffer.is_numeric {
            let length = field_updates_buffer.numeric_values.as_ref().unwrap().len();
            debug_assert!(length <= i32::MAX as usize);
            length as i32
        } else {
            0
        };
        debug_assert!(fields_length <= i32::MAX as usize);
        debug_assert!(docs_upto_length <= i32::MAX as usize);
        BufferedUpdateIterator {
            term_values_iterator,
            look_ahead_term_iterator,
            byte_values_iterator,
            buffered_update: BufferedUpdate::default(),
            updates_with_value: Some(updates_with_value),
            fields_length: fields_length as i32,
            docs_upto_length: docs_upto_length as i32,
            numeric_values_length,
            field_updates_buffer,
        }
    }
    /// If all updates update a single field to the same value, then we can
    /// apply these updates in the term order instead of the request order
    /// as both will yield the same result. This optimization allows us to
    /// iterate the term dictionary faster and de-duplicate updates.
    pub(crate) fn is_sorted_terms(&self) -> bool {
        self.field_updates_buffer.term_sort_state.indices.is_some()
    }
    /// Moves to the next BufferedUpdate or return null if all updates are
    /// consumed. The returned instance is a shared instance and must be
    /// fully consumed before the next call to this method.
    pub(crate) fn next_value(&mut self) -> Result<Option<BufferedUpdate>> {
        let mut buffered_update = BufferedUpdate::default();
        let next_term = self.next_term()?;

        if let Some(next) = next_term {
            let idx = self.term_values_iterator.ord();
            self.buffered_update.term_value = Some(next.clone());
            buffered_update.term_value = Some(next);
            buffered_update.has_value = self.updates_with_value.as_ref().unwrap().get(idx);
            buffered_update.term_field = self.field_updates_buffer.fields
                [FieldUpdatesBuffer::get_array_index(self.fields_length, idx) as usize]
                .clone();
            buffered_update.doc_upto = self.field_updates_buffer.docs_upto
                [FieldUpdatesBuffer::get_array_index(self.docs_upto_length, idx) as usize];

            if buffered_update.has_value {
                if self.field_updates_buffer.is_numeric {
                    buffered_update.numeric_value =
                        self.field_updates_buffer.numeric_values.as_ref().unwrap()
                            [FieldUpdatesBuffer::get_array_index(self.numeric_values_length, idx)
                                as usize];
                    buffered_update.binary_value = None;
                } else {
                    debug_assert!(self.numeric_values_length == 0);
                    match &mut self.byte_values_iterator {
                        Some(iterator) => match iterator.next()? {
                            Some(bytes_ref) => {
                                buffered_update.binary_value = Some(bytes_ref.into_owned());
                            },
                            None => {
                                buffered_update.binary_value = None;
                            },
                        },
                        None => {
                            buffered_update.binary_value = None;
                        },
                    }
                }
            } else {
                buffered_update.binary_value = None;
                buffered_update.numeric_value = 0;
            }
            Ok(Some(buffered_update))
        } else {
            Ok(None)
        }
    }

    fn next_term(&mut self) -> Result<Option<BytesRef<Vec<u8>>>> {
        if let Some(look_ahead_term_iterator) = &mut self.look_ahead_term_iterator {
            if self.buffered_update.term_value.is_none() {
                look_ahead_term_iterator.next()?;
            }
            let mut last_term;
            let mut ahead_term;
            loop {
                ahead_term = look_ahead_term_iterator.next()?;
                match self.term_values_iterator.next()? {
                    Some(term) => {
                        last_term = Some(term.into_owned());
                    },
                    None => {
                        last_term = None;
                    },
                }

                if let Some(ahead) = ahead_term {
                    let ahead = ahead.into_owned();
                    // Shortcut to avoid equals, we did a stable sort before, so
                    // aheadTerm can only equal
                    // lastTerm when aheadTerm has a lager ord.
                    if look_ahead_term_iterator.ord() > self.term_values_iterator.ord()
                        && ahead == *last_term.as_mut().unwrap()
                    {
                        continue;
                    }
                }
                break;
            }
            Ok(last_term)
        } else {
            match self.term_values_iterator.next()? {
                Some(term) => Ok(Some(term.into_owned())),
                None => Ok(None),
            }
        }
    }
}
/// # Warning
/// this struct should not be use in map or other data-structures that use
/// hashCode / equals
#[derive(Default, Clone)]

pub struct BufferedUpdate {
    /// the max document ID this update should be applied to.
    pub doc_upto: i32,
    /// a numeric value or 0 if this buffer holds binary updates.
    pub numeric_value: i64,
    /// a binary value or null if this buffer holds numeric updates.
    pub binary_value: Option<BytesRef<Vec<u8>>>,
    /// true if this update has a value.
    pub has_value: bool,
    /// The update terms field. This will never be null.
    pub term_field: String,
    /// The update terms value. This will never be null.
    pub term_value: Option<BytesRef<Vec<u8>>>,
}

impl BufferedUpdate {
    pub fn new(
        doc_upto: i32,
        numeric_value: i64,
        binary_value: Option<BytesRef<Vec<u8>>>,
        has_value: bool,
        term_field: String,
        term_value: Option<BytesRef<Vec<u8>>>,
    ) -> Self {
        BufferedUpdate {
            doc_upto,
            numeric_value,
            binary_value,
            has_value,
            term_field,
            term_value,
        }
    }
    pub(crate) fn get_binary_value(&self) -> Option<&BytesRef<Vec<u8>>> {
        self.binary_value.as_ref()
    }
}

type UpdateBits<'a> = Either2Bits<MatchAllBits, &'a FixedBitSet>;

#[cfg(test)]
mod tests {
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
    use crate::core::util::CounterEnum;
    use crate::core::util::error::lucene_error::Result;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{random, rarely};
    use crate::test::util::test_util::TestUtil;
    use parking_lot::Mutex;
    use rand::Rng;

    #[allow(dead_code)] // for quick search
    pub struct TestFieldUpdatesBuffer;

    #[test]
    pub fn test_basics() -> Result<()> {
        let counter = Arc::new(Mutex::new(CounterEnum::new_counter(false)));
        let update = DocValuesUpdate::new(
            DocValuesType::Numeric,
            Term::from_text("id", "1"),
            "age",
            MAX_INT,
            DocValuesUpdateEnum::Numeric(NumericDocValuesUpdate::new(Option::from(6))),
        );
        let mut buffer = FieldUpdatesBuffer::from_numeric_update(counter.clone(), &update, 15)?;
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
                    assert_eq!(value.term_value.unwrap().utf8_to_string()?, "1");
                    assert_eq!(value.numeric_value, 6);
                    assert_eq!(value.doc_upto, 15);
                },
                1 => {
                    assert_eq!(value.term_field, "id");
                    assert_eq!(value.term_value.unwrap().utf8_to_string()?, "10");
                    assert_eq!(value.numeric_value, 6);
                    assert_eq!(value.doc_upto, 15);
                },
                2 => {
                    assert_eq!(value.term_field, "id");
                    assert_eq!(value.term_value.unwrap().utf8_to_string()?, "8");
                    assert_eq!(value.numeric_value, 12);
                    assert_eq!(value.doc_upto, 15);
                },
                3 => {
                    assert_eq!(value.term_field, "some_other_field");
                    assert_eq!(value.term_value.unwrap().utf8_to_string()?, "8");
                    assert_eq!(value.numeric_value, 13);
                    assert_eq!(value.doc_upto, 17);
                },
                4 => {
                    assert_eq!(value.term_field, "id");
                    assert_eq!(value.term_value.unwrap().utf8_to_string()?, "8");
                    assert_eq!(value.numeric_value, 12);
                    assert_eq!(value.doc_upto, 16);
                },
                _ => unreachable!(),
            }
            count += 1;
        }
        Ok(())
    }
    #[test]
    fn test_update_share_values() -> Result<()> {
        let mut random = random();
        let counter = Arc::new(Mutex::new(CounterEnum::new_counter(false)));
        let int_value = random.random::<i32>();
        let value_for_three = random.random_bool(0.5);
        let sub_update = DocValuesUpdateEnum::Numeric(NumericDocValuesUpdate::new(Option::from(
            int_value as i64,
        )));
        let update = DocValuesUpdate::new(
            DocValuesType::Numeric,
            Term::from_text("id", "0"),
            "enabled",
            MAX_INT,
            sub_update,
        );
        let mut buffer =
            FieldUpdatesBuffer::from_numeric_update(counter.clone(), &update, i32::MAX)?;
        buffer.add_update_with_long(
            &Term::from_text("id".to_string(), "1"),
            int_value as i64,
            i32::MAX,
        )?;
        buffer.add_update_with_long(
            &Term::from_text("id".to_string(), "2"),
            int_value as i64,
            i32::MAX,
        )?;
        if value_for_three {
            buffer.add_update_with_long(
                &Term::from_text("id".to_string(), "3"),
                int_value as i64,
                i32::MAX,
            )?;
        } else {
            buffer.add_no_value(&Term::from_text("id".to_string(), "3"), i32::MAX)?;
        }
        buffer.add_update_with_long(
            &Term::from_text("id".to_string(), "4"),
            int_value as i64,
            i32::MAX,
        )?;
        buffer.finish()?;

        let mut iterator = buffer.iterator()?;
        let mut count = 0;
        while let Some(value) = iterator.next_value()? {
            let has_value = count != 3 || value_for_three;
            assert_eq!(
                count.to_string(),
                value.term_value.unwrap().utf8_to_string()?
            );
            assert_eq!("id", value.term_field);
            assert_eq!(has_value, value.has_value);
            if has_value {
                assert_eq!(int_value as i64, value.numeric_value);
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
        let counter = Arc::new(Mutex::new(CounterEnum::new_counter(false)));
        let value_for_three = random.random_bool(0.5);
        let sub_update = DocValuesUpdateEnum::Binary(BinaryDocValuesUpdate::new(Option::from(
            BytesRef::from_string(""),
        )));
        let update = DocValuesUpdate::new(
            DocValuesType::Binary,
            Term::from_text("id".to_string(), "0"),
            "enabled",
            MAX_INT,
            sub_update,
        );
        let mut buffer =
            FieldUpdatesBuffer::from_binary_update(counter.clone(), &update, i32::MAX)?;
        buffer.add_update_with_bytes_ref(
            &Term::from_text("id".to_string(), "1"),
            &BytesRef::from_string(""),
            i32::MAX,
        )?;
        buffer.add_update_with_bytes_ref(
            &Term::from_text("id".to_string(), "2"),
            &BytesRef::from_string(""),
            i32::MAX,
        )?;
        if value_for_three {
            buffer.add_update_with_bytes_ref(
                &Term::from_text("id".to_string(), "3"),
                &BytesRef::from_string(""),
                i32::MAX,
            )?;
        } else {
            buffer.add_no_value(&Term::from_text("id".to_string(), "3"), i32::MAX)?;
        }

        buffer.add_update_with_bytes_ref(
            &Term::from_text("id".to_string(), "4"),
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
                value.term_value.unwrap().utf8_to_string()?
            );
            assert_eq!("id", value.term_field);
            assert_eq!(has_value, value.has_value);

            if has_value {
                assert_eq!(BytesRef::from_string(""), value.binary_value.unwrap());
            } else {
                assert!(value.binary_value.is_none());
            }
            assert_eq!(i32::MAX, value.doc_upto);
            count += 1;
        }
        Ok(())
    }
    pub fn random_from<T>(items: Vec<T>) -> T
    where
        T: Clone,
    {
        let mut rng = rand::rng();
        let index = rng.random_range(0..items.len());
        items[index].clone()
    }
    pub fn get_random_binary_update<R: Rng + ?Sized>(
        random: &mut R,
        doc_id_upto: i32,
    ) -> DocValuesUpdate {
        let term_field = random_from(vec!["id", "_id", "some_other_field"]);
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
    pub fn get_random_numeric_update<R: Rng + ?Sized>(
        random: &mut R,
        doc_id_upto: i32,
    ) -> DocValuesUpdate {
        let term_field = random_from(vec!["id", "_id", "some_other_field"]);
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
        let counter = Arc::new(Mutex::new(CounterEnum::new_counter(false)));

        let mut random_update = get_random_binary_update(&mut random, 0);
        updates.push(random_update.clone());

        let doc_id_upto = random_update.doc_id_upto;
        let mut buffer =
            FieldUpdatesBuffer::from_binary_update(counter.clone(), &random_update, doc_id_upto)?;

        for i in 0..num_updates {
            random_update = get_random_binary_update(&mut random, i + 1);
            let doc_id_upto = random_update.doc_id_upto;
            updates.push(random_update.clone());

            if random_update.has_value {
                buffer.add_update_with_bytes_ref(
                    &random_update.term,
                    &random_update.sub_update.get_binary().unwrap().get_value(),
                    doc_id_upto,
                )?;
            } else {
                buffer.add_no_value(&random_update.term, doc_id_upto)?;
            }
        }
        buffer.finish()?;

        let mut iterator = buffer.iterator()?;
        let mut count = 0;

        while let Some(value) = iterator.next_value()? {
            let random_update = &updates[count];
            count += 1;
            assert_eq!(
                random_update.term.bytes.utf8_to_string()?,
                value.term_value.unwrap().utf8_to_string()?
            );
            assert_eq!(random_update.term.field, value.term_field);
            assert_eq!(random_update.has_value, value.has_value, "count: {}", count);

            if random_update.has_value {
                assert_eq!(
                    random_update.sub_update.get_binary().unwrap().get_value(),
                    value.binary_value.unwrap()
                );
            } else {
                assert!(value.binary_value.is_none());
            }
            assert_eq!(random_update.doc_id_upto, value.doc_upto);
        }

        Ok(())
    }
    #[test]
    pub fn test_numeric_random() -> Result<()> {
        let mut random = random();
        let mut updates = Vec::new();
        let num_updates = 1 + random.random_range(0..1000);
        let counter = Arc::new(Mutex::new(CounterEnum::new_counter(false)));

        let mut random_update = get_random_numeric_update(&mut random, 0);
        updates.push(random_update.clone());

        let doc_id_upto = random_update.doc_id_upto;
        let mut buffer =
            FieldUpdatesBuffer::from_numeric_update(counter.clone(), &random_update, doc_id_upto)?;

        let mut last_update: Option<DocValuesUpdate> = None;
        for i in 0..num_updates {
            random_update = get_random_numeric_update(&mut random, i + 1);
            // last
            if i == num_updates - 1 {
                last_update = Some(random_update.clone());
            }
            let doc_id_upto = random_update.doc_id_upto;
            updates.push(random_update.clone());

            if random_update.has_value {
                buffer.add_update_with_long(
                    &random_update.term,
                    random_update.sub_update.get_numeric().unwrap().get_value(),
                    doc_id_upto,
                )?;
            } else {
                buffer.add_no_value(&random_update.term, doc_id_upto)?;
            }
        }
        buffer.finish()?;
        assert!(last_update.is_some());
        let last_update = last_update.unwrap();
        let terms_sorted = last_update.has_value
            && updates.iter().all(|update| {
                update.field == last_update.field
                    && update.has_value
                    && update.sub_update.get_numeric().unwrap().get_value()
                        == last_update.sub_update.get_numeric().unwrap().get_value()
            });

        assert_buffer_updates(&buffer, &mut updates, terms_sorted)?;

        Ok(())
    }
    #[test]
    pub fn test_no_numeric_value() -> Result<()> {
        let update = DocValuesUpdate::new(
            DocValuesType::Numeric,
            Term::from_text("id".to_string(), "1"),
            "age".to_string(),
            MAX_INT,
            DocValuesUpdateEnum::Numeric(NumericDocValuesUpdate::new(None)),
        );

        let counter = Arc::new(Mutex::new(CounterEnum::new_counter(false)));
        let doc_id_upto = update.doc_id_upto;
        let buffer =
            FieldUpdatesBuffer::from_numeric_update(counter.clone(), &update, doc_id_upto)?;

        assert_eq!(buffer.get_min_numeric(), 0);
        assert_eq!(buffer.get_max_numeric(), 0);

        Ok(())
    }
    #[test]
    pub fn test_sort_and_dedup_numeric_updates_by_terms() -> Result<()> {
        let mut random = random();
        let mut updates = Vec::new();
        let num_updates = 1 + random.random_range(0..1000);
        let counter = Arc::new(Mutex::new(CounterEnum::new_counter(false)));

        let term_field = random_from(vec!["id", "_id", "some_other_field"]);
        let doc_value = 1 + random.random_range(0..1000);

        let mut random_update = DocValuesUpdate::new(
            DocValuesType::Numeric,
            Term::from_text(
                term_field.to_string(),
                &random.random_range(0..1000).to_string(),
            ),
            "numeric".to_string(),
            MAX_INT,
            DocValuesUpdateEnum::Numeric(NumericDocValuesUpdate::new(Some(doc_value))),
        );
        if let Some(v) = random_update.prepare_for_apply(0) {
            random_update = v
        }
        updates.push(random_update.clone());
        let doc_id_upto = random_update.doc_id_upto;
        let mut buffer =
            FieldUpdatesBuffer::from_numeric_update(counter.clone(), &random_update, doc_id_upto)?;

        for i in 0..num_updates {
            random_update = DocValuesUpdate::new(
                DocValuesType::Numeric,
                Term::from_text(
                    term_field.to_string(),
                    &random.random_range(0..1000).to_string(),
                ),
                "numeric".to_string(),
                MAX_INT,
                DocValuesUpdateEnum::Numeric(NumericDocValuesUpdate::new(Some(doc_value))),
            );
            if let Some(v) = random_update.prepare_for_apply(i + 1) {
                random_update = v
            }
            updates.push(random_update.clone());
            buffer.add_update_with_long(
                &random_update.term,
                doc_value,
                random_update.doc_id_upto,
            )?;
        }

        buffer.finish()?;

        // We can now assert that the buffer updates are correct after sorting
        // and deduplication
        assert_buffer_updates(&buffer, &mut updates, true)?;

        Ok(())
    }

    fn assert_buffer_updates(
        buffer: &FieldUpdatesBuffer,
        updates: &mut [DocValuesUpdate],
        term_sorted: bool,
    ) -> Result<()> {
        let mut updates = updates.to_owned();
        if term_sorted {
            updates.sort_by(|a, b| a.term.bytes.cmp(&b.term.bytes));
            let mut by_terms: BTreeMap<BytesRef<Vec<u8>>, DocValuesUpdate> = BTreeMap::new();

            for update in updates.iter() {
                by_terms
                    .entry(update.term.bytes.clone())
                    .or_insert_with(|| update.clone());
            }

            updates = by_terms.into_values().collect();
        }

        let mut iterator = buffer.iterator()?;
        let mut count = 0;
        let mut min = i64::MAX;
        let mut max = i64::MIN;
        let mut has_at_least_one_value = false;

        while let Some(value) = iterator.next_value()? {
            let v = buffer.get_numeric_value(count);
            let expected_update = &updates[count as usize];
            count += 1;
            assert_eq!(
                expected_update.term.bytes.utf8_to_string()?,
                value.term_value.unwrap().utf8_to_string()?
            );
            assert_eq!(expected_update.term.field, value.term_field);
            assert_eq!(expected_update.has_value, value.has_value);

            if expected_update.has_value {
                let expected_value = expected_update
                    .sub_update
                    .get_numeric()
                    .unwrap()
                    .get_value();
                assert_eq!(expected_value, value.numeric_value);
                min = min.min(expected_value);
                max = max.max(expected_value);
                has_at_least_one_value = true;
            } else {
                assert_eq!(0, value.numeric_value);
                assert_eq!(0, v)
            }
        }
        if has_at_least_one_value {
            assert_eq!(max, buffer.get_max_numeric());
            assert_eq!(min, buffer.get_min_numeric());
        } else {
            assert_eq!(0, buffer.get_min_numeric());
            assert_eq!(0, buffer.get_max_numeric());
        }
        assert_eq!(updates.len() as i32, count);
        Ok(())
    }
}
