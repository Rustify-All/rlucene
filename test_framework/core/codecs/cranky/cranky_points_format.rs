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
use crate::core::codecs::dummy::dummy_mutable_point_tree::DummyMutablePointTree;
use crate::core::codecs::points_format::PointsFormat;
use crate::core::codecs::points_reader::PointsReader;
use crate::core::codecs::points_writer::PointsWriter;
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::merge_state::MergeState;
use crate::core::index::point_values::{IntersectVisitor, PointTree, PointTreeEnum, PointValues};
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::store::directory::Directory;
use crate::core::store::{IndexInput, IndexOutput};
use crate::core::util::clone::TryClone;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use parking_lot::Mutex;
use rand::RngExt;
use rand::prelude::StdRng;
use std::borrow::Cow;
use std::io::Error;
use std::sync::Arc;

pub struct CrankyPointsFormat<PF> {
  delegate: PF,
  random: Arc<Mutex<StdRng>>,
}

impl<PF> CrankyPointsFormat<PF> {
  pub fn new(delegate: PF, random: Arc<Mutex<StdRng>>) -> Self {
    Self { delegate, random }
  }
}

impl<PF> PointsFormat for CrankyPointsFormat<PF>
where
  PF: PointsFormat,
{
  type PointsWriter<T: IndexOutput> = CrankyPointsWriter<PF::PointsWriter<T>>;

  fn fields_writer<D1, D2>(
    &self,
    state: &SegmentWriteState<D1>,
    info: &SegmentInfo<D2>,
  ) -> Result<Self::PointsWriter<D1::IndexOutput>>
  where
    D1: Directory,
  {
    Ok(CrankyPointsWriter::new(
      self.delegate.fields_writer(state, info)?,
      Arc::clone(&self.random),
    ))
  }

  type PointsReader<T: IndexInput> = CrankyPointsReader<PF::PointsReader<T>>;

  fn fields_reader<D1, D2>(
    &self,
    state: &SegmentReadState<D1>,
    info: &SegmentInfo<D2>,
  ) -> Result<Self::PointsReader<D1::IndexInput>>
  where
    D1: Directory,
  {
    Ok(CrankyPointsReader::new(
      self.delegate.fields_reader(state, info)?,
      Arc::clone(&self.random),
    ))
  }
}

pub struct CrankyPointsWriter<PW> {
  delegate: PW,
  random: Arc<Mutex<StdRng>>,
}

impl<PW> CrankyPointsWriter<PW> {
  fn new(delegate: PW, random: Arc<Mutex<StdRng>>) -> Self {
    Self { delegate, random }
  }
}

impl<PW> PointsWriter for CrankyPointsWriter<PW>
where
  PW: PointsWriter,
{
  fn write_field<PR, D1, D2>(
    &mut self,
    field_info: &Arc<FieldInfo>,
    values: &mut PR,
    dir: &D1,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<()>
  where
    PR: PointsReader,
    D1: Directory,
  {
    if self.random.lock().random_range(0..100) == 0 {
      return Err(LuceneError::io(Error::other("Fake IOException")));
    }
    self
      .delegate
      .write_field(field_info, values, dir, segment_info)
  }

  fn finish(&mut self) -> Result<()> {
    if self.random.lock().random_range(0..100) == 0 {
      return Err(LuceneError::io(Error::other("Fake IOException")));
    }
    self.delegate.finish()?;
    if self.random.lock().random_range(0..100) == 0 {
      return Err(LuceneError::io(Error::other("Fake IOException")));
    }
    Ok(())
  }

  fn merge<D1, D2, CR>(&mut self, merge_state: &MergeState<D1, CR>, dir: &D2) -> Result<()>
  where
    D2: Directory,
    CR: CodecReader,
  {
    if self.random.lock().random_range(0..100) == 0 {
      return Err(LuceneError::io(Error::other("Fake IOException")));
    }
    self.delegate.merge(merge_state, dir)?;
    if self.random.lock().random_range(0..100) == 0 {
      return Err(LuceneError::io(Error::other("Fake IOException")));
    }
    Ok(())
  }
}

impl<PW> Closeable for CrankyPointsWriter<PW>
where
  PW: PointsWriter,
{
  fn close(&mut self) -> Result<()> {
    self.delegate.close()?;
    if self.random.lock().random_range(0..100) == 0 {
      return Err(LuceneError::io(Error::other("Fake IOException")));
    }
    Ok(())
  }
}

pub struct CrankyPointsReader<PR> {
  delegate: PR,
  random: Arc<Mutex<StdRng>>,
}

impl<PR> CrankyPointsReader<PR> {
  fn new(delegate: PR, random: Arc<Mutex<StdRng>>) -> Self {
    Self { delegate, random }
  }
}

impl<PR> CloseableRef for CrankyPointsReader<PR>
where
  PR: PointsReader,
{
  fn close(&self) -> Result<()> {
    self.delegate.close()?;
    if self.random.lock().random_range(0..100) == 0 {
      return Err(LuceneError::io(Error::other("Fake IOException")));
    }
    Ok(())
  }
}

impl<PR> PointsReader for CrankyPointsReader<PR>
where
  PR: PointsReader,
{
  fn check_integrity(&self) -> Result<()> {
    if self.random.lock().random_range(0..100) == 0 {
      return Err(LuceneError::io(Error::other("Fake IOException")));
    }
    self.delegate.check_integrity()?;
    if self.random.lock().random_range(0..100) == 0 {
      return Err(LuceneError::io(Error::other("Fake IOException")));
    }
    Ok(())
  }

  type PointValuesType = CrankyPointValues<PR::PointValuesType>;

  fn get_values(&self, field: &str) -> Result<Option<Self::PointValuesType>> {
    Ok(
      self
        .delegate
        .get_values(field)?
        .map(|delegate| CrankyPointValues::new(delegate, Arc::clone(&self.random))),
    )
  }
}

pub struct CrankyPointValues<PV> {
  delegate: PV,
  random: Arc<Mutex<StdRng>>,
}

impl<PV> CrankyPointValues<PV> {
  fn new(delegate: PV, random: Arc<Mutex<StdRng>>) -> Self {
    Self { delegate, random }
  }
}

impl<PV> PointValues for CrankyPointValues<PV>
where
  PV: PointValues,
{
  fn get_min_packed_value(&self) -> Result<Option<Cow<'_, [u8]>>> {
    if self.random.lock().random_range(0..100) == 0 {
      return Err(LuceneError::io(Error::other("Fake IOException")));
    }
    self.delegate.get_min_packed_value()
  }

  fn get_max_packed_value(&self) -> Result<Option<Cow<'_, [u8]>>> {
    if self.random.lock().random_range(0..100) == 0 {
      return Err(LuceneError::io(Error::other("Fake IOException")));
    }
    self.delegate.get_max_packed_value()
  }

  fn get_num_dimensions(&self) -> Result<usize> {
    if self.random.lock().random_range(0..100) == 0 {
      return Err(LuceneError::io(Error::other("Fake IOException")));
    }
    self.delegate.get_num_dimensions()
  }

  fn get_num_index_dimensions(&self) -> Result<usize> {
    if self.random.lock().random_range(0..100) == 0 {
      return Err(LuceneError::io(Error::other("Fake IOException")));
    }
    self.delegate.get_num_index_dimensions()
  }

  fn get_bytes_per_dimension(&self) -> Result<usize> {
    if self.random.lock().random_range(0..100) == 0 {
      return Err(LuceneError::io(Error::other("Fake IOException")));
    }
    self.delegate.get_bytes_per_dimension()
  }

  fn size(&self) -> Result<usize> {
    self.delegate.size()
  }

  fn get_doc_count(&self) -> Result<i32> {
    self.delegate.get_doc_count()
  }

  type PointTree = CrankyPointTree<PointTreeEnum<PV>>;
  type MutablePointTree = DummyMutablePointTree;

  fn get_point_tree(&self) -> Result<PointTreeEnum<Self>> {
    Ok(PointTreeEnum::Other(CrankyPointTree::Cranky {
      delegate: self.delegate.get_point_tree()?,
      random: Arc::clone(&self.random),
    }))
  }
}

pub enum CrankyPointTree<PT> {
  Cranky {
    delegate: PT,
    random: Arc<Mutex<StdRng>>,
  },
  Delegate(PT),
}

impl<PT> TryClone for CrankyPointTree<PT>
where
  PT: PointTree,
{
  fn try_clone(&self) -> Result<Self> {
    match self {
      Self::Cranky { delegate, .. } | Self::Delegate(delegate) => {
        Ok(Self::Delegate(delegate.try_clone()?))
      },
    }
  }
}

impl<PT> PointTree for CrankyPointTree<PT>
where
  PT: PointTree,
{
  fn move_to_child(&mut self) -> Result<bool> {
    match self {
      Self::Cranky { delegate, .. } | Self::Delegate(delegate) => delegate.move_to_child(),
    }
  }

  fn move_to_sibling(&mut self) -> Result<bool> {
    match self {
      Self::Cranky { delegate, .. } | Self::Delegate(delegate) => delegate.move_to_sibling(),
    }
  }

  fn move_to_parent(&mut self) -> Result<bool> {
    match self {
      Self::Cranky { delegate, .. } | Self::Delegate(delegate) => delegate.move_to_parent(),
    }
  }

  fn get_min_packed_value(&self) -> Result<Cow<'_, [u8]>> {
    match self {
      Self::Cranky { delegate, .. } | Self::Delegate(delegate) => delegate.get_min_packed_value(),
    }
  }

  fn get_max_packed_value(&self) -> Result<Cow<'_, [u8]>> {
    match self {
      Self::Cranky { delegate, .. } | Self::Delegate(delegate) => delegate.get_max_packed_value(),
    }
  }

  fn size(&self) -> Result<usize> {
    match self {
      Self::Cranky { delegate, .. } | Self::Delegate(delegate) => delegate.size(),
    }
  }

  fn visit_doc_ids<IV>(&mut self, visitor: &mut IV) -> Result<()>
  where
    IV: IntersectVisitor,
  {
    match self {
      Self::Cranky { delegate, random } => {
        if random.lock().random_range(0..100) == 0 {
          return Err(LuceneError::io(Error::other("Fake IOException")));
        }
        delegate.visit_doc_ids(visitor)?;
        if random.lock().random_range(0..100) == 0 {
          return Err(LuceneError::io(Error::other("Fake IOException")));
        }
        Ok(())
      },
      Self::Delegate(delegate) => delegate.visit_doc_ids(visitor),
    }
  }

  fn visit_doc_values<IV>(&mut self, visitor: &mut IV) -> Result<()>
  where
    IV: IntersectVisitor,
  {
    match self {
      Self::Cranky { delegate, random } => {
        if random.lock().random_range(0..100) == 0 {
          return Err(LuceneError::io(Error::other("Fake IOException")));
        }
        delegate.visit_doc_values(visitor)?;
        if random.lock().random_range(0..100) == 0 {
          return Err(LuceneError::io(Error::other("Fake IOException")));
        }
        Ok(())
      },
      Self::Delegate(delegate) => delegate.visit_doc_values(visitor),
    }
  }
}
