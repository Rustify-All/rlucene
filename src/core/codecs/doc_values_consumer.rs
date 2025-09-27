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
use crate::core::codecs::doc_values_producer::DocValuesProducer;
use crate::core::codecs::dummy::dummy_binary_doc_values::DummyBinaryDocValues;
use crate::core::codecs::dummy::dummy_doc_values_skipper::DummyDocValuesSkipper;
use crate::core::codecs::dummy::dummy_numeric_doc_values::DummyNumericDocValues;
use crate::core::codecs::dummy::dummy_sorted_doc_values::DummySortedDocValues;
use crate::core::codecs::dummy::dummy_sorted_numeric_doc_values::DummySortedNumericDocValues;
use crate::core::codecs::dummy::dummy_sorted_set_doc_values::DummySortedSetDocValues;
use crate::core::codecs::lucene90::lucene90_doc_values_producer::{
    Lucene90BinaryDocValuesEnum, Lucene90NumericDocValuesEnum, Lucene90SortedNumericDocValuesEnum,
};
use crate::core::index::binary_doc_values::{BinaryDocValues, Either3BinaryDocValues};
use crate::core::index::doc_values::DocValues;
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::merge_state::{DocMapEnum, MergeState};
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::singleton_sorted_numeric_doc_values::SingletonSortedNumericDocValues;
use crate::core::index::sorted_numeric_doc_values::Either2SortedNumericDocValues;
use crate::core::index::sorted_numeric_doc_values::SortedNumericDocValues;
use crate::core::index::{BytesRef, DocIDMerger, DocIDMergerEnum, Sub, SubBase, of};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::store::IndexInput;
use crate::core::util::CoreHelper;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::borrow::Cow;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

pub trait DocValuesConsumer {
    fn add_numeric_field<D>(&mut self, field: &Arc<FieldInfo>, values_producer: &D) -> Result<()>
    where
        D: DocValuesProducer;
    fn add_binary_field<D>(&mut self, field: &Arc<FieldInfo>, values_producer: &D) -> Result<()>
    where
        D: DocValuesProducer;
    fn add_sorted_field<D>(&mut self, field: &Arc<FieldInfo>, values_producer: &D) -> Result<()>
    where
        D: DocValuesProducer;
    fn add_sorted_numeric_field<D>(
        &mut self,
        field: &Arc<FieldInfo>,
        values_producer: &D,
    ) -> Result<()>
    where
        D: DocValuesProducer;
    fn add_sorted_set_field<D>(
        &mut self,
        field: &Arc<FieldInfo>,
        values_producer: &D,
    ) -> Result<()>
    where
        D: DocValuesProducer;

    fn merge_numeric_field<I>(
        &mut self,
        merge_field_info: &Arc<FieldInfo>,
        merge_state: &mut MergeState<I>,
    ) -> Result<()>
    where
        I: IndexInput,
    {
        let producer = EmptyDocValuesProducerMerge1 {
            merge_field_info: merge_field_info.clone(),
            merge_state,
        };
        self.add_numeric_field(merge_field_info, &producer)?;
        Ok(())
    }
    fn merge_binary_filed<I: IndexInput>(
        &mut self,
        merge_field_info: &Arc<FieldInfo>,
        merge_state: &mut MergeState<I>,
    ) -> Result<()> {
        let producer = EmptyDocValuesProducerMerge2 {
            merge_field_info: merge_field_info.clone(),
            merge_state,
        };
        self.add_binary_field(merge_field_info, &producer)
    }
    fn merge_sorted_numeric_field<I: IndexInput>(
        &mut self,
        merge_field_info: &Arc<FieldInfo>,
        merge_state: &mut MergeState<I>,
    ) -> Result<()> {
        let producer = EmptyDocValuesProducerMerge3 {
            merge_field_info: merge_field_info.clone(),
            merge_state,
        };
        self.add_sorted_numeric_field(merge_field_info, &producer)
    }
    fn merge_sorted_field<I: IndexInput>(
        &mut self,
        _merge_field_info: &Arc<FieldInfo>,
        _merge_state: &mut MergeState<I>,
    ) -> Result<()> {
        todo!()
    }
    fn merge_sorted_set_field<I: IndexInput>(
        &mut self,
        _merge_field_info: &Arc<FieldInfo>,
        _merge_state: &mut MergeState<I>,
    ) -> Result<()> {
        todo!()
    }
}

// 1. NumericDocValues
/// Tracks state of one numeric sub-reader that we are merging.
pub(crate) struct NumericDocValuesSub<I>
where
    I: IndexInput,
{
    values: Lucene90NumericDocValuesEnum<I>,
    doc_map: Rc<DocMapEnum>,
}

impl<I> NumericDocValuesSub<I>
where
    I: IndexInput,
{
    fn new(doc_map: Rc<DocMapEnum>, values: Lucene90NumericDocValuesEnum<I>) -> Self {
        debug_assert!(values.doc_id() == -1);
        NumericDocValuesSub { values, doc_map }
    }
}
impl<I> SubBase for NumericDocValuesSub<I>
where
    I: IndexInput,
{
    fn next_doc(&mut self) -> Result<i32> {
        self.values.next_doc()
    }

    fn get_doc_map(&self) -> Result<&Rc<DocMapEnum>> {
        Ok(&self.doc_map)
    }
}
impl<I> Default for NumericDocValuesSub<I>
where
    I: IndexInput,
{
    fn default() -> Self {
        NumericDocValuesSub {
            values: Lucene90NumericDocValuesEnum::C(Default::default()),
            doc_map: Rc::new(DocMapEnum::default()),
        }
    }
}
pub struct NumericDocValuesMerge<I>
where
    I: IndexInput,
{
    doc_id: i32,
    current: Option<Rc<RefCell<Sub<NumericDocValuesSub<I>>>>>,
    doc_id_merger: DocIDMergerEnum<NumericDocValuesSub<I>>,
    final_cost: i64,
}

impl<I> DocValuesIterator for NumericDocValuesMerge<I>
where
    I: IndexInput,
{
    fn advance_exact(&mut self, _target: i32) -> Result<bool> {
        Err(LuceneError::unsupported_operation(""))
    }
}

impl<I> DocIdSetIterator for NumericDocValuesMerge<I>
where
    I: IndexInput,
{
    fn doc_id(&self) -> i32 {
        self.doc_id
    }

    fn next_doc(&mut self) -> Result<i32> {
        self.current = self.doc_id_merger.next()?;
        match &self.current {
            Some(current) => {
                self.doc_id = current.borrow_mut().mapped_doc_id;
                Ok(self.doc_id)
            },
            None => {
                self.doc_id = NO_MORE_DOCS;
                Ok(NO_MORE_DOCS)
            },
        }
    }

    fn advance(&mut self, _target: i32) -> Result<i32> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn cost(&self) -> Result<i64> {
        Ok(self.final_cost)
    }
}

impl<I> NumericDocValues for NumericDocValuesMerge<I>
where
    I: IndexInput,
{
    fn long_value(&mut self) -> Result<i64> {
        match self.current {
            Some(ref current) => {
                let mut current = current.borrow_mut();
                current.sub.values.long_value()
            },
            None => Err(LuceneError::unreachable("should not be here")),
        }
    }
}
pub(crate) struct EmptyDocValuesProducerMerge1<'a, I>
where
    I: IndexInput,
{
    merge_field_info: Arc<FieldInfo>,
    merge_state: &'a mut MergeState<I>,
}

impl<I> Clone for EmptyDocValuesProducerMerge1<'_, I>
where
    I: IndexInput,
{
    fn clone(&self) -> Self {
        unreachable!(
            "{} {}",
            std::any::type_name::<Self>(),
            CoreHelper::CLONE_WARRING
        )
    }
}

impl<I> DocValuesProducer for EmptyDocValuesProducerMerge1<'_, I>
where
    I: IndexInput,
{
    type NumericDocValues = NumericDocValuesMerge<I>;

    fn get_numeric(&self, field_info: &Arc<FieldInfo>) -> Result<Self::NumericDocValues> {
        if Arc::ptr_eq(field_info, &self.merge_field_info) {
            return Err(LuceneError::illegal_argument("wrong fieldInfo"));
        }

        let mut subs = vec![];
        debug_assert!(
            self.merge_state.doc_maps.len() == self.merge_state.doc_values_producers.len()
        );
        for i in 0..self.merge_state.doc_values_producers.len() {
            let mut values = None;
            let doc_values_producer_opt = &self.merge_state.doc_values_producers[i];
            if let Some(doc_values_producer) = doc_values_producer_opt {
                let reader_field_info =
                    self.merge_state.field_infos[i].field_info_by_name(&self.merge_field_info.name);
                if let Some(reader_field_info) = &reader_field_info
                    && *reader_field_info.get_doc_values_type() == DocValuesType::Numeric
                {
                    values = Some(doc_values_producer.get_numeric(reader_field_info)?);
                }
            }

            if let Some(values) = values {
                let doc_map = self.merge_state.doc_maps[i].clone();
                subs.push(Rc::new(RefCell::new(Sub::new(NumericDocValuesSub::new(
                    doc_map, values,
                )))));
            }
        }
        merge_numeric_values(subs, self.merge_state.needs_index_sort)
    }

    type BinaryDocValues = DummyBinaryDocValues;
    type SortedDocValues = DummySortedDocValues;

    type SortedNumericDocValues = DummySortedNumericDocValues;
    type SortedSetDocValues = DummySortedSetDocValues;
    type DocValuesSkipper = DummyDocValuesSkipper;
}
// 2. BinaryDocValues
/// Tracks state of one binary sub-reader that we are merging.
struct BinaryDocValuesSub<I>
where
    I: IndexInput,
{
    values: Lucene90BinaryDocValuesEnum<I>,
    doc_map: Rc<DocMapEnum>,
}

impl<I> BinaryDocValuesSub<I>
where
    I: IndexInput,
{
    fn new(doc_map: Rc<DocMapEnum>, values: Lucene90BinaryDocValuesEnum<I>) -> Self {
        debug_assert!(values.doc_id() == -1);
        BinaryDocValuesSub { values, doc_map }
    }
}

impl<I> SubBase for BinaryDocValuesSub<I>
where
    I: IndexInput,
{
    fn next_doc(&mut self) -> Result<i32> {
        self.values.next_doc()
    }

    fn get_doc_map(&self) -> Result<&Rc<DocMapEnum>> {
        Ok(&self.doc_map)
    }
}
impl<I> Default for BinaryDocValuesSub<I>
where
    I: IndexInput,
{
    fn default() -> Self {
        BinaryDocValuesSub {
            values: Either3BinaryDocValues::C(Default::default()),
            doc_map: Rc::new(DocMapEnum::default()),
        }
    }
}

pub struct BinaryDocValuesMerge<I>
where
    I: IndexInput,
{
    doc_id: i32,
    current: Option<Rc<RefCell<Sub<BinaryDocValuesSub<I>>>>>,
    doc_id_merger: DocIDMergerEnum<BinaryDocValuesSub<I>>,
    final_cost: i64,
}

impl<I> DocValuesIterator for BinaryDocValuesMerge<I>
where
    I: IndexInput,
{
    fn advance_exact(&mut self, _target: i32) -> Result<bool> {
        Err(LuceneError::unsupported_operation(""))
    }
}

impl<I> DocIdSetIterator for BinaryDocValuesMerge<I>
where
    I: IndexInput,
{
    fn doc_id(&self) -> i32 {
        self.doc_id
    }

    fn next_doc(&mut self) -> Result<i32> {
        self.current = self.doc_id_merger.next()?;
        match &self.current {
            Some(current) => {
                self.doc_id = current.borrow_mut().mapped_doc_id;
                Ok(self.doc_id)
            },
            None => {
                self.doc_id = NO_MORE_DOCS;
                Ok(NO_MORE_DOCS)
            },
        }
    }

    fn advance(&mut self, _target: i32) -> Result<i32> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn cost(&self) -> Result<i64> {
        Ok(self.final_cost)
    }
}

impl<I> BinaryDocValues for BinaryDocValuesMerge<I>
where
    I: IndexInput,
{
    fn binary_value(&mut self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
        match self.current {
            Some(ref current) => {
                let mut current = current.borrow_mut();
                // TODO:IMPORTANT Since we need to return a reference, but cannot return a
                // temporary value created by borrowing,
                // we are forced to make a copy.Is there any way to avoid the
                // copy?
                let v = current.sub.values.binary_value()?.into_owned();
                Ok(Cow::Owned(v))
            },
            None => Err(LuceneError::unreachable("should not be here")),
        }
    }
}
pub(crate) struct EmptyDocValuesProducerMerge2<'a, I>
where
    I: IndexInput,
{
    merge_field_info: Arc<FieldInfo>,
    merge_state: &'a mut MergeState<I>,
}

impl<I> Clone for EmptyDocValuesProducerMerge2<'_, I>
where
    I: IndexInput,
{
    fn clone(&self) -> Self {
        unreachable!(
            "{} {}",
            std::any::type_name::<Self>(),
            CoreHelper::CLONE_WARRING
        )
    }
}

impl<I> DocValuesProducer for EmptyDocValuesProducerMerge2<'_, I>
where
    I: IndexInput,
{
    type NumericDocValues = DummyNumericDocValues;
    type BinaryDocValues = BinaryDocValuesMerge<I>;

    fn get_binary(&self, field_info: &Arc<FieldInfo>) -> Result<Self::BinaryDocValues> {
        if Arc::ptr_eq(field_info, &self.merge_field_info) {
            return Err(LuceneError::illegal_argument("wrong fieldInfo"));
        }

        let mut subs = vec![];
        let mut cost = 0;
        debug_assert!(
            self.merge_state.doc_maps.len() == self.merge_state.doc_values_producers.len()
        );

        for i in 0..self.merge_state.doc_values_producers.len() {
            let mut values = None;
            let doc_values_producer_opt = &self.merge_state.doc_values_producers[i];

            if let Some(doc_values_producer) = doc_values_producer_opt {
                let reader_field_info =
                    self.merge_state.field_infos[i].field_info_by_name(&self.merge_field_info.name);
                if let Some(reader_field_info) = &reader_field_info
                    && *reader_field_info.get_doc_values_type() == DocValuesType::Binary
                {
                    values = Some(doc_values_producer.get_binary(reader_field_info)?);
                }
            }

            if let Some(values) = values {
                cost += values.cost()?;
                let doc_map = self.merge_state.doc_maps[i].clone();
                subs.push(Rc::new(RefCell::new(Sub::new(BinaryDocValuesSub::new(
                    doc_map, values,
                )))));
            }
        }
        let doc_id_merger = of(subs, self.merge_state.needs_index_sort)?;
        let doc_value = BinaryDocValuesMerge {
            doc_id: -1,
            current: None,
            doc_id_merger,
            final_cost: cost,
        };
        Ok(doc_value)
    }

    type SortedDocValues = DummySortedDocValues;

    type SortedNumericDocValues = DummySortedNumericDocValues;
    type SortedSetDocValues = DummySortedSetDocValues;
    type DocValuesSkipper = DummyDocValuesSkipper;
}
// 3. SortedNumericDocValues
/// Tracks state of one sorted numeric sub-reader that we are merging.
struct SortedNumericDocValuesSub<I>
where
    I: IndexInput,
{
    values: Lucene90SortedNumericDocValuesEnum<I>,
    doc_map: Rc<DocMapEnum>,
}

impl<I> SortedNumericDocValuesSub<I>
where
    I: IndexInput,
{
    fn new(doc_map: Rc<DocMapEnum>, values: Lucene90SortedNumericDocValuesEnum<I>) -> Self {
        debug_assert!(values.doc_id() == -1);
        SortedNumericDocValuesSub { values, doc_map }
    }
}

impl<I> SubBase for SortedNumericDocValuesSub<I>
where
    I: IndexInput,
{
    fn next_doc(&mut self) -> Result<i32> {
        self.values.next_doc()
    }

    fn get_doc_map(&self) -> Result<&Rc<DocMapEnum>> {
        Ok(&self.doc_map)
    }
}

pub struct SortedNumericDocValuesMerge<I>
where
    I: IndexInput,
{
    doc_id: i32,
    current_sub: Option<Rc<RefCell<Sub<SortedNumericDocValuesSub<I>>>>>,
    doc_id_merger: DocIDMergerEnum<SortedNumericDocValuesSub<I>>,
    final_cost: i64,
}

impl<I> DocValuesIterator for SortedNumericDocValuesMerge<I>
where
    I: IndexInput,
{
    fn advance_exact(&mut self, _target: i32) -> Result<bool> {
        Err(LuceneError::unsupported_operation(""))
    }
}

impl<I> DocIdSetIterator for SortedNumericDocValuesMerge<I>
where
    I: IndexInput,
{
    fn doc_id(&self) -> i32 {
        self.doc_id
    }

    fn next_doc(&mut self) -> Result<i32> {
        self.current_sub = self.doc_id_merger.next()?;
        match &self.current_sub {
            Some(current) => {
                self.doc_id = current.borrow_mut().mapped_doc_id;
                Ok(self.doc_id)
            },
            None => {
                self.doc_id = NO_MORE_DOCS;
                Ok(NO_MORE_DOCS)
            },
        }
    }

    fn advance(&mut self, _target: i32) -> Result<i32> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn cost(&self) -> Result<i64> {
        Ok(self.final_cost)
    }
}

impl<I> SortedNumericDocValues for SortedNumericDocValuesMerge<I>
where
    I: IndexInput,
{
    fn next_value(&mut self) -> Result<i64> {
        match self.current_sub {
            Some(ref current) => {
                let mut current = current.borrow_mut();
                current.sub.values.next_value()
            },
            None => Err(LuceneError::unreachable("should not be here")),
        }
    }

    fn doc_value_count(&mut self) -> Result<i32> {
        match self.current_sub {
            Some(ref current) => {
                let mut current = current.borrow_mut();
                current.sub.values.doc_value_count()
            },
            None => Err(LuceneError::unreachable("should not be here")),
        }
    }

    // TODO: is it correct?
    type NumericDocValues = DummyNumericDocValues;
}
pub(crate) struct EmptyDocValuesProducerMerge3<'a, I>
where
    I: IndexInput,
{
    merge_field_info: Arc<FieldInfo>,
    merge_state: &'a mut MergeState<I>,
}

impl<I> Clone for EmptyDocValuesProducerMerge3<'_, I>
where
    I: IndexInput,
{
    fn clone(&self) -> Self {
        unreachable!(
            "{} {}",
            std::any::type_name::<Self>(),
            CoreHelper::CLONE_WARRING
        )
    }
}

impl<I> DocValuesProducer for EmptyDocValuesProducerMerge3<'_, I>
where
    I: IndexInput,
{
    type NumericDocValues = DummyNumericDocValues;
    type BinaryDocValues = DummyBinaryDocValues;
    type SortedDocValues = DummySortedDocValues;
    type SortedNumericDocValues = Either2SortedNumericDocValues<
        SingletonSortedNumericDocValues<NumericDocValuesMerge<I>>,
        SortedNumericDocValuesMerge<I>,
    >;

    fn get_sorted_numeric(
        &self,
        field_info: &Arc<FieldInfo>,
    ) -> Result<Self::SortedNumericDocValues> {
        if Arc::ptr_eq(field_info, &self.merge_field_info) {
            return Err(LuceneError::illegal_argument("wrong FieldInfo"));
        }
        // We must make new iterators + DocIDMerger for each iterator:
        let mut subs = vec![];
        let mut cost = 0;
        let mut all_singletons = true;

        for i in 0..self.merge_state.doc_values_producers.len() {
            let mut values = None;
            let doc_values_producer_opt = &self.merge_state.doc_values_producers[i];
            if let Some(doc_values_producer) = doc_values_producer_opt {
                let reader_field_info =
                    self.merge_state.field_infos[i].field_info_by_name(&self.merge_field_info.name);
                if let Some(reader_field_info) = reader_field_info
                    && *reader_field_info.get_doc_values_type() == DocValuesType::SortedNumeric
                {
                    values = Some(doc_values_producer.get_sorted_numeric(&reader_field_info)?);
                }
            }

            if values.is_none() {
                values = Some(Lucene90SortedNumericDocValuesEnum::D(
                    DocValues::empty_sorted_numeric()?,
                ));
            }
            {
                let values_ref = values.as_ref().unwrap();
                cost += values_ref.cost()?;
                if all_singletons && matches!(values_ref, Lucene90SortedNumericDocValuesEnum::C(_))
                {
                    all_singletons = false;
                }
            }
            if let Some(values) = values {
                let doc_map = self.merge_state.doc_maps[i].clone();
                subs.push(Rc::new(RefCell::new(Sub::new(
                    SortedNumericDocValuesSub::new(doc_map, values),
                ))));
            }
        }

        if all_singletons {
            // All subs are single-valued.
            // We specialize for that case since it makes it easier for codecs
            // to optimize for single-valued fields.
            let mut single_valued_subs = vec![];
            for sub in &subs {
                let mut sub = sub.borrow_mut();
                let single_valued_values = match &mut sub.sub.values {
                    Lucene90SortedNumericDocValuesEnum::C(inner) => {
                        inner.get_numeric_doc_values()?.unwrap()
                    },
                    _ => return Err(LuceneError::unreachable("")),
                };
                single_valued_subs.push(Rc::new(RefCell::new(Sub::new(NumericDocValuesSub::new(
                    sub.sub.doc_map.clone(),
                    single_valued_values,
                )))));
            }
            let dv = merge_numeric_values(single_valued_subs, self.merge_state.needs_index_sort)?;
            return Ok(Either2SortedNumericDocValues::A(
                DocValues::singleton_numeric(dv)?,
            ));
        }
        let doc_id_merger = of(subs, self.merge_state.needs_index_sort)?;
        Ok(Either2SortedNumericDocValues::B(
            SortedNumericDocValuesMerge {
                doc_id: -1,
                current_sub: None,
                doc_id_merger,
                final_cost: cost,
            },
        ))
    }

    type SortedSetDocValues = DummySortedSetDocValues;
    type DocValuesSkipper = DummyDocValuesSkipper;
}

pub(crate) fn merge_numeric_values<I>(
    mut subs: Vec<Rc<RefCell<Sub<NumericDocValuesSub<I>>>>>,
    index_is_sorted: bool,
) -> Result<NumericDocValuesMerge<I>>
where
    I: IndexInput,
{
    let mut cost = 0;
    for sub in &mut subs {
        cost = sub.borrow().sub.values.cost()?;
    }
    let doc_id_merger = of(subs, index_is_sorted)?;
    Ok(NumericDocValuesMerge {
        doc_id: -1,
        current: None,
        doc_id_merger,
        final_cost: cost,
    })
}
