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
use crate::core::codecs::doc_values_consumer::DocValuesConsumer;
use crate::core::codecs::doc_values_format::DocValuesFormat;
use crate::core::codecs::doc_values_producer::DocValuesProducer;
use crate::core::index::binary_doc_values::BinaryDocValues;
use crate::core::index::doc_values_skip_index_type::DocValuesSkipIndexType;
use crate::core::index::doc_values_skipper::DocValuesSkipperEnum2;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::index_reader::Identity;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::sorted_doc_values::SortedDocValues;
use crate::core::index::sorted_numeric_doc_values::SortedNumericDocValues;
use crate::core::index::sorted_set_doc_values::SortedSetDocValues;
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, NO_MORE_DOCS};
use crate::core::store::directory::Directory;
use crate::core::store::{IndexInput, IndexOutput};
use crate::core::util::HasIdentity;
use crate::core::util::bit_set::BitSet;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::Result;
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::long_bit_set::LongBitSet;
use crate::test_framework::core::codecs::asserting_codec::assert_thread;
use crate::test_framework::core::index::asserting_leaf_reader::{
  AssertingBinaryDocValues, AssertingDocValuesSkipper, AssertingNumericDocValues,
  AssertingSortedDocValues, AssertingSortedNumericDocValues, AssertingSortedSetDocValues,
};
use crate::test_framework::core::util::test_util::{DefaultDocValuesFormat, TestUtil};
use std::fmt::{Display, Formatter};
use std::sync::{Arc, OnceLock};
use std::thread::ThreadId;

/// Just like the default but with additional asserts.
pub struct AssertingDocValuesFormat {
  in_: DefaultDocValuesFormat,
  identity: Identity,
}

impl AssertingDocValuesFormat {
  pub fn new() -> Self {
    Self {
      in_: TestUtil::get_default_doc_values_format(),
      identity: Identity::new(),
    }
  }
}

impl Display for AssertingDocValuesFormat {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "AssertingDocValuesFormat")
  }
}

impl HasIdentity for AssertingDocValuesFormat {
  fn identity(&self) -> &Identity {
    &self.identity
  }
}

impl DocValuesFormat for AssertingDocValuesFormat {
  fn get_name(&self) -> &str {
    "Asserting"
  }

  type DocValuesConsumer<T: IndexOutput> =
    AssertingDocValuesConsumer<<DefaultDocValuesFormat as DocValuesFormat>::DocValuesConsumer<T>>;

  fn fields_consumer<D1, D2>(
    &self,
    state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::DocValuesConsumer<D1::IndexOutput>>
  where
    D1: Directory,
  {
    Ok(AssertingDocValuesConsumer::new(
      self.in_.fields_consumer(state, segment_info)?,
      segment_info.max_doc()?,
    ))
  }

  type DocValuesProducer<T: IndexInput> =
    AssertingDocValuesProducer<<DefaultDocValuesFormat as DocValuesFormat>::DocValuesProducer<T>>;

  fn fields_producer<D1, D2>(
    &self,
    state: &SegmentReadState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::DocValuesProducer<D1::IndexInput>>
  where
    D1: Directory,
  {
    assert!(state.field_infos.has_doc_values());
    Ok(AssertingDocValuesProducer::new(
      self.in_.fields_producer(state, segment_info)?,
      state.field_infos.clone(),
      segment_info.max_doc()?,
      false,
    ))
  }

  fn for_name(name: &str) -> Result<Arc<Self>> {
    static FORMAT: OnceLock<Arc<AssertingDocValuesFormat>> = OnceLock::new();

    match name {
      "Asserting" => Ok(Arc::clone(
        FORMAT.get_or_init(|| Arc::new(AssertingDocValuesFormat::new())),
      )),
      _ => Err(
        crate::core::util::error::lucene_error::LuceneError::illegal_argument(format!(
          "Could not load doc values format named \"{name}\""
        )),
      ),
    }
  }
}

pub struct AssertingDocValuesConsumer<DVC> {
  in_: DVC,
  max_doc: i32,
}

impl<DVC> AssertingDocValuesConsumer<DVC>
where
  DVC: DocValuesConsumer,
{
  fn new(in_: DVC, max_doc: i32) -> Self {
    Self { in_, max_doc }
  }
}

impl<DVC> DocValuesConsumer for AssertingDocValuesConsumer<DVC>
where
  DVC: DocValuesConsumer,
{
  type IndexOutput = DVC::IndexOutput;

  fn add_numeric_field<D1, D2, D>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    field: &Arc<FieldInfo>,
    values_producer: &D,
  ) -> Result<()>
  where
    D1: Directory<IndexOutput = Self::IndexOutput>,
    D: DocValuesProducer,
  {
    let mut values = values_producer.get_numeric(field)?;
    let mut last_doc_id = -1;
    loop {
      let doc_id = values.next_doc()?;
      if doc_id == NO_MORE_DOCS {
        break;
      }
      assert!(doc_id >= 0 && doc_id < self.max_doc);
      assert!(doc_id > last_doc_id);
      last_doc_id = doc_id;
      values.long_value()?;
    }
    self
      .in_
      .add_numeric_field(write_state, segment_info, field, values_producer)
  }

  fn add_binary_field<D1, D2, D>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    field: &Arc<FieldInfo>,
    values_producer: &D,
  ) -> Result<()>
  where
    D1: Directory<IndexOutput = Self::IndexOutput>,
    D: DocValuesProducer,
  {
    let mut values = values_producer.get_binary(field)?;
    let mut last_doc_id = -1;
    loop {
      let doc_id = values.next_doc()?;
      if doc_id == NO_MORE_DOCS {
        break;
      }
      assert!(doc_id >= 0 && doc_id < self.max_doc);
      assert!(doc_id > last_doc_id);
      last_doc_id = doc_id;
      assert!(values.binary_value()?.is_valid()?);
    }
    self
      .in_
      .add_binary_field(write_state, segment_info, field, values_producer)
  }

  fn add_sorted_field<D1, D2, D>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    field: &Arc<FieldInfo>,
    values_producer: &D,
  ) -> Result<()>
  where
    D1: Directory<IndexOutput = Self::IndexOutput>,
    D: DocValuesProducer,
  {
    let mut values = values_producer.get_sorted(field)?;

    let value_count = values.get_value_count()?;
    assert!(value_count <= self.max_doc);
    let mut last_value = None;
    for ord in 0..value_count {
      let value = values.lookup_ord(ord)?;
      assert!(value.is_valid()?);
      assert!(
        last_value
          .as_ref()
          .is_none_or(|last_value| value.as_ref() > last_value)
      );
      last_value = Some(value.into_owned());
    }

    let mut seen_ords = FixedBitSet::new(value_count as usize);
    let mut last_doc_id = -1;
    loop {
      let doc_id = values.next_doc()?;
      if doc_id == NO_MORE_DOCS {
        break;
      }
      assert!(doc_id >= 0 && doc_id < self.max_doc);
      assert!(doc_id > last_doc_id);
      last_doc_id = doc_id;
      let ord = values.ord_value()?;
      assert!(ord >= 0 && ord < value_count);
      seen_ords.set(ord as usize);
    }
    assert_eq!(seen_ords.cardinality(), value_count as usize);
    self
      .in_
      .add_sorted_field(write_state, segment_info, field, values_producer)
  }

  fn add_sorted_numeric_field<D1, D2, D>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    field: &Arc<FieldInfo>,
    values_producer: &D,
  ) -> Result<()>
  where
    D1: Directory<IndexOutput = Self::IndexOutput>,
    D: DocValuesProducer,
  {
    let mut values = values_producer.get_sorted_numeric(field)?;
    let mut last_doc_id = -1;
    loop {
      let doc_id = values.next_doc()?;
      if doc_id == NO_MORE_DOCS {
        break;
      }
      assert!(values.doc_id() > last_doc_id);
      last_doc_id = values.doc_id();
      let count = values.doc_value_count()?;
      assert!(count > 0);
      let mut previous = i64::MIN;
      for _ in 0..count {
        let next_value = values.next_value()?;
        assert!(next_value >= previous);
        previous = next_value;
      }
    }
    self
      .in_
      .add_sorted_numeric_field(write_state, segment_info, field, values_producer)
  }

  fn add_sorted_set_field<D1, D2, D>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    field: &Arc<FieldInfo>,
    values_producer: &D,
  ) -> Result<()>
  where
    D1: Directory<IndexOutput = Self::IndexOutput>,
    D: DocValuesProducer,
  {
    let mut values = values_producer.get_sorted_set(field)?;

    let value_count = values.get_value_count()?;
    let mut last_value = None;
    for ord in 0..value_count {
      let value = values.lookup_ord(ord)?;
      assert!(value.is_valid()?);
      assert!(
        last_value
          .as_ref()
          .is_none_or(|last_value| value.as_ref() > last_value)
      );
      last_value = Some(value.into_owned());
    }

    let mut seen_ords = LongBitSet::new(value_count as usize)?;
    loop {
      let doc_id = values.next_doc()?;
      if doc_id == NO_MORE_DOCS {
        break;
      }

      let mut last_ord = -1;
      for _ in 0..values.doc_value_count()? {
        let ord = values.next_ord()?;
        assert!(
          ord >= 0 && ord < value_count,
          "ord={ord} is not in bounds 0 ..{}",
          value_count - 1
        );
        assert!(ord > last_ord, "ord={ord},lastOrd={last_ord}");
        seen_ords.set(ord as usize);
        last_ord = ord;
      }
    }
    assert_eq!(seen_ords.cardinality(), value_count as usize);
    self
      .in_
      .add_sorted_set_field(write_state, segment_info, field, values_producer)
  }
}

impl<DVC> Closeable for AssertingDocValuesConsumer<DVC>
where
  DVC: DocValuesConsumer,
{
  fn close(&mut self) -> Result<()> {
    self.in_.close()?;
    self.in_.close()
  }
}

pub struct AssertingDocValuesProducer<DVP> {
  in_: Arc<DVP>,
  asserting: bool,
  field_infos: Option<Arc<FieldInfos>>,
  max_doc: i32,
  merging: bool,
  creation_thread: ThreadId,
}

impl<DVP> AssertingDocValuesProducer<DVP>
where
  DVP: DocValuesProducer,
{
  fn new(in_: DVP, field_infos: Arc<FieldInfos>, max_doc: i32, merging: bool) -> Self {
    Self {
      in_: Arc::new(in_),
      asserting: true,
      field_infos: Some(field_infos),
      max_doc,
      merging,
      creation_thread: std::thread::current().id(),
    }
  }

  pub(crate) fn new_default(in_: DVP) -> Self {
    Self {
      in_: Arc::new(in_),
      asserting: false,
      field_infos: None,
      max_doc: 0,
      merging: false,
      creation_thread: std::thread::current().id(),
    }
  }
}

impl<DVP> CloseableRef for AssertingDocValuesProducer<DVP>
where
  DVP: DocValuesProducer,
{
  fn close(&self) -> Result<()> {
    self.in_.close()?;
    if self.asserting {
      self.in_.close()
    } else {
      Ok(())
    }
  }
}

impl<DVP> DocValuesProducer for AssertingDocValuesProducer<DVP>
where
  DVP: DocValuesProducer,
{
  type NumericDocValues = AssertingNumericDocValues<DVP::NumericDocValues>;

  fn get_numeric(&self, field: &Arc<FieldInfo>) -> Result<Self::NumericDocValues> {
    if self.asserting {
      assert_eq!(
        self
          .field_infos
          .as_ref()
          .expect("field infos must exist when assertions are enabled")
          .field_info_by_name(&field.name)?
          .expect("field must exist")
          .number,
        field.number
      );
      if self.merging {
        assert_thread("DocValuesProducer", self.creation_thread);
      }
      assert_eq!(field.get_doc_values_type(), &DocValuesType::Numeric);
      Ok(AssertingNumericDocValues::new(
        self.in_.get_numeric(field)?,
        self.max_doc,
      ))
    } else {
      Ok(AssertingNumericDocValues::new_default(
        self.in_.get_numeric(field)?,
        self.max_doc,
      ))
    }
  }

  type BinaryDocValues = AssertingBinaryDocValues<DVP::BinaryDocValues>;

  fn get_binary(&self, field: &Arc<FieldInfo>) -> Result<Self::BinaryDocValues> {
    if self.asserting {
      assert_eq!(
        self
          .field_infos
          .as_ref()
          .expect("field infos must exist when assertions are enabled")
          .field_info_by_name(&field.name)?
          .expect("field must exist")
          .number,
        field.number
      );
      if self.merging {
        assert_thread("DocValuesProducer", self.creation_thread);
      }
      assert_eq!(field.get_doc_values_type(), &DocValuesType::Binary);
      Ok(AssertingBinaryDocValues::new(
        self.in_.get_binary(field)?,
        self.max_doc,
      ))
    } else {
      Ok(AssertingBinaryDocValues::new_default(
        self.in_.get_binary(field)?,
        self.max_doc,
      ))
    }
  }

  type SortedDocValues = AssertingSortedDocValues<DVP::SortedDocValues>;

  fn get_sorted(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedDocValues> {
    if self.asserting {
      assert_eq!(
        self
          .field_infos
          .as_ref()
          .expect("field infos must exist when assertions are enabled")
          .field_info_by_name(&field.name)?
          .expect("field must exist")
          .number,
        field.number
      );
      if self.merging {
        assert_thread("DocValuesProducer", self.creation_thread);
      }
      assert_eq!(field.get_doc_values_type(), &DocValuesType::Sorted);
      AssertingSortedDocValues::new(self.in_.get_sorted(field)?, self.max_doc)
    } else {
      AssertingSortedDocValues::new_default(self.in_.get_sorted(field)?, self.max_doc)
    }
  }

  type SortedNumericDocValues = AssertingSortedNumericDocValues<DVP::SortedNumericDocValues>;

  fn get_sorted_numeric(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedNumericDocValues> {
    if self.asserting {
      assert_eq!(
        self
          .field_infos
          .as_ref()
          .expect("field infos must exist when assertions are enabled")
          .field_info_by_name(&field.name)?
          .expect("field must exist")
          .number,
        field.number
      );
      if self.merging {
        assert_thread("DocValuesProducer", self.creation_thread);
      }
      assert_eq!(field.get_doc_values_type(), &DocValuesType::SortedNumeric);
      AssertingSortedNumericDocValues::create(self.in_.get_sorted_numeric(field)?, self.max_doc)
    } else {
      Ok(AssertingSortedNumericDocValues::create_default(
        self.in_.get_sorted_numeric(field)?,
      ))
    }
  }

  type SortedSetDocValues = AssertingSortedSetDocValues<DVP::SortedSetDocValues>;

  fn get_sorted_set(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedSetDocValues> {
    if self.asserting {
      assert_eq!(
        self
          .field_infos
          .as_ref()
          .expect("field infos must exist when assertions are enabled")
          .field_info_by_name(&field.name)?
          .expect("field must exist")
          .number,
        field.number
      );
      if self.merging {
        assert_thread("DocValuesProducer", self.creation_thread);
      }
      assert_eq!(field.get_doc_values_type(), &DocValuesType::SortedSet);
      AssertingSortedSetDocValues::create(self.in_.get_sorted_set(field)?, self.max_doc)
    } else {
      Ok(AssertingSortedSetDocValues::create_default(
        self.in_.get_sorted_set(field)?,
      ))
    }
  }

  type DocValuesSkipper =
    DocValuesSkipperEnum2<DVP::DocValuesSkipper, AssertingDocValuesSkipper<DVP::DocValuesSkipper>>;

  fn get_skipper(&self, field: &Arc<FieldInfo>) -> Result<Option<Self::DocValuesSkipper>> {
    if self.asserting {
      assert_eq!(
        self
          .field_infos
          .as_ref()
          .expect("field infos must exist when assertions are enabled")
          .field_info_by_name(&field.name)?
          .expect("field must exist")
          .number,
        field.number
      );
      assert!(field.doc_values_skip_index_type() != &DocValuesSkipIndexType::None);
      let skipper = self
        .in_
        .get_skipper(field)?
        .expect("doc-values skipper must not be None");
      Ok(Some(DocValuesSkipperEnum2::B(
        AssertingDocValuesSkipper::new(skipper),
      )))
    } else {
      Ok(self.in_.get_skipper(field)?.map(DocValuesSkipperEnum2::A))
    }
  }

  fn check_integrity(&self) -> Result<()> {
    self.in_.check_integrity()
  }

  fn get_merge_instance(&self) -> Result<Option<Self>>
  where
    Self: Sized,
  {
    let in_ = match (self.in_.get_merge_instance()?, self.asserting) {
      (Some(in_), _) => in_,
      (None, true) => self.in_.clone(),
      (None, false) => return Ok(None),
    };
    Ok(Some(Self {
      in_,
      asserting: self.asserting,
      field_infos: self.field_infos.clone(),
      max_doc: self.max_doc,
      merging: true,
      creation_thread: std::thread::current().id(),
    }))
  }
}
