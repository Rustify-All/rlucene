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
use crate::core::codecs::Codec;
use crate::core::codecs::norms_consumer::NormsConsumer;
use crate::core::codecs::norms_format::NormsFormat;
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, NO_MORE_DOCS};
use crate::core::store::directory::Directory;
use crate::core::store::{IndexInput, IndexOutput};
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::codecs::asserting_codec::assert_thread;
use crate::test_framework::core::index::asserting_leaf_reader::AssertingNumericDocValues;
use crate::test_framework::core::util::test_util::{DefaultNormsFormat, TestUtil};
use std::sync::Arc;
use std::thread::ThreadId;

/// Just like the default but with additional asserts.
pub struct AssertingNormsFormat {
  in_: DefaultNormsFormat,
}

impl AssertingNormsFormat {
  pub fn new() -> Self {
    Self {
      in_: TestUtil::get_default_codec().norms_format(),
    }
  }
}

impl NormsFormat for AssertingNormsFormat {
  type NormsConsumer<T: IndexOutput> =
    AssertingNormsConsumer<<DefaultNormsFormat as NormsFormat>::NormsConsumer<T>>;

  fn norms_consumer<D1, D2>(
    &self,
    state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::NormsConsumer<D1::IndexOutput>>
  where
    D1: Directory,
  {
    Ok(AssertingNormsConsumer::new(
      self.in_.norms_consumer(state, segment_info)?,
      segment_info.max_doc()?,
    ))
  }

  type NormsProducer<T: IndexInput> =
    AssertingNormsProducer<<DefaultNormsFormat as NormsFormat>::NormsProducer<T>>;

  fn norms_producer<D1, D2>(
    &self,
    state: &SegmentReadState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::NormsProducer<D1::IndexInput>>
  where
    D1: Directory,
  {
    assert!(state.field_infos.has_norms());
    Ok(AssertingNormsProducer::new(
      self.in_.norms_producer(state, segment_info)?,
      segment_info.max_doc()?,
      false,
    ))
  }
}

pub struct AssertingNormsConsumer<NC> {
  in_: NC,
  max_doc: i32,
}

impl<NC> AssertingNormsConsumer<NC>
where
  NC: NormsConsumer,
{
  fn new(in_: NC, max_doc: i32) -> Self {
    Self { in_, max_doc }
  }
}

impl<NC> NormsConsumer for AssertingNormsConsumer<NC>
where
  NC: NormsConsumer,
{
  fn add_norms_field(
    &mut self,
    field: &Arc<FieldInfo>,
    values_producer: &mut impl NormsProducer,
  ) -> Result<()> {
    let mut values = values_producer.get_norms(field)?;

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

    self.in_.add_norms_field(field, &mut *values_producer)
  }
}

impl<NC> Closeable for AssertingNormsConsumer<NC>
where
  NC: NormsConsumer,
{
  fn close(&mut self) -> Result<()> {
    self.in_.close()?;
    self.in_.close()
  }
}

pub struct AssertingNormsProducer<NP> {
  in_: Arc<NP>,
  max_doc: i32,
  merging: bool,
  creation_thread: ThreadId,
}

impl<NP> AssertingNormsProducer<NP>
where
  NP: NormsProducer,
{
  fn new(in_: NP, max_doc: i32, merging: bool) -> Self {
    Self {
      in_: Arc::new(in_),
      max_doc,
      merging,
      creation_thread: std::thread::current().id(),
    }
  }
}

impl<NP> CloseableRef for AssertingNormsProducer<NP>
where
  NP: NormsProducer,
{
  fn close(&self) -> Result<()> {
    self.in_.close()?;
    self.in_.close()
  }
}

impl<NP> NormsProducer for AssertingNormsProducer<NP>
where
  NP: NormsProducer,
{
  type NumericDocValues = AssertingNumericDocValues<NP::NumericDocValues>;

  fn get_norms(&self, field: &Arc<FieldInfo>) -> Result<Self::NumericDocValues> {
    if self.merging {
      assert_thread("NormsProducer", self.creation_thread);
    }
    assert!(field.has_norms());
    Ok(AssertingNumericDocValues::new(
      self.in_.get_norms(field)?,
      self.max_doc,
    ))
  }

  fn check_integrity(&self) -> Result<()> {
    self.in_.check_integrity()
  }

  fn get_merge_instance(&self) -> Result<Option<Self>>
  where
    Self: Sized,
  {
    let in_ = match self.in_.get_merge_instance()? {
      Some(in_) => in_,
      None => self.in_.clone(),
    };
    Ok(Some(Self {
      in_,
      max_doc: self.max_doc,
      merging: true,
      creation_thread: std::thread::current().id(),
    }))
  }
}
