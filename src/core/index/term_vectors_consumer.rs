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
#[cfg(test)]
use crate::core::codecs::codec;
use crate::core::codecs::term_vectors_format::TermVectorsFormat;
use crate::core::codecs::term_vectors_writer::TermVectorsWriter;
use crate::core::codecs::{Codec, CodecTermVectorsWriter, Codecs};
use crate::core::index::field_info::FieldInfo;
use crate::core::index::indexing_chain::PerField;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::sorter::DocMap;
use crate::core::index::sorting_term_vectors_consumer::SortingTermVectorsConsumer;
use crate::core::index::term_vectors_consumer_per_field::TermVectorsConsumerPerField;
use crate::core::index::terms_hash::TermsHash;
use crate::core::store::IOContext;
use crate::core::store::directory::Directory;
#[cfg(test)]
use crate::core::store::dummy::dummy_directory::DummyDirectory;
use crate::core::store::flush_info::FlushInfo;
use crate::core::util::accountable::Accountable;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::comparator::Comparator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::int_block_pool::IntBlockPool;
use crate::core::util::{AtomicCounter, ByteBlockPool, Counter, IOUtils, TryIntoInt};
#[cfg(test)]
use crate::test_framework::core::util::failure_context::{
  ExecutionMethod, ExecutionOwner, ExecutionScope,
};
use std::cmp::Ordering;
use std::sync::Arc;

pub(crate) struct TermVectorsConsumer<D>
where
  D: Directory,
{
  directory: D,
  codec: Codecs,
  has_vectors: bool,
  num_vector_fields: usize,
  pub(crate) last_doc_id: i32,
  per_fields_idxs: smallvec::SmallVec<[PerFieldMeta; 1]>,
  hook: TermVectorsConsumerHook<D>,
  pub(crate) base: TermsHash,
}

pub(crate) enum TermVectorsConsumerHook<D>
where
  D: Directory,
{
  Default {
    writer: Option<CodecTermVectorsWriter<D>>,
  },
  Sorting(SortingTermVectorsConsumer<D>),
}

pub(crate) struct TermVectorsConsumerDefaults;

/// Parameter `idx` is the index of the [`PerField`] where the [`TermVectorsConsumerPerField`] resides.
/// [`PerField`] itself is located in the [`IndexingChain`](crate::core::index::indexing_chain::IndexingChain)'s `doc_fields` array.
///
/// The field name is read from the corresponding [`PerField`] while sorting.
#[derive(Clone, Copy, Default)]
pub(crate) struct PerFieldMeta {
  pub(crate) idx: usize,
}

struct PerFieldMetaComparator<'a> {
  per_fields: &'a [PerField],
}

impl Comparator<PerFieldMeta> for PerFieldMetaComparator<'_> {
  const TYPE: &'static str = "PerFieldMetaComparator";

  fn compare(&self, a: &PerFieldMeta, b: &PerFieldMeta) -> Result<i32> {
    let name = self.per_fields[a.idx]
      .field_info
      .as_ref()
      .map_or("", |info| info.name.as_str());
    let other_name = self.per_fields[b.idx]
      .field_info
      .as_ref()
      .map_or("", |info| info.name.as_str());
    Ok(match name.cmp(other_name) {
      Ordering::Less => -1,
      Ordering::Equal => 0,
      Ordering::Greater => 1,
    })
  }
}

#[cfg(test)]
impl Default for TermVectorsConsumer<DummyDirectory> {
  fn default() -> Self {
    let directory = DummyDirectory;
    TermVectorsConsumer::new(
      codec::get_default(),
      directory,
      TermVectorsConsumerHook::default(),
    )
  }
}

impl<D> Default for TermVectorsConsumerHook<D>
where
  D: Directory,
{
  fn default() -> Self {
    Self::Default { writer: None }
  }
}

impl<D> TermVectorsConsumerHook<D>
where
  D: Directory,
{
  fn fill(&mut self, last_doc_id: &mut i32, doc_id: i32) -> Result<()> {
    match self {
      Self::Default { writer } => TermVectorsConsumerDefaults::fill(writer, last_doc_id, doc_id),
      Self::Sorting(hook) => {
        TermVectorsConsumerDefaults::fill(&mut hook.writer, last_doc_id, doc_id)
      },
    }
  }

  fn start_document(&mut self, num_vector_fields: i32) -> Result<()> {
    match self {
      Self::Default { writer } => writer
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("writer not initialized"))?
        .start_document(num_vector_fields),
      Self::Sorting(hook) => hook
        .writer
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("writer not initialized"))?
        .start_document(num_vector_fields),
    }
  }

  fn finish_document(&mut self) -> Result<()> {
    match self {
      Self::Default { writer } => writer
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("writer not initialized"))?
        .finish_document(),
      Self::Sorting(hook) => hook
        .writer
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("writer not initialized"))?
        .finish_document(),
    }
  }

  fn write_per_field(
    &mut self,
    per_field: &mut TermVectorsConsumerPerField,
    int_pool: &mut IntBlockPool,
    byte_pool: &ByteBlockPool,
    term_byte_pool: &ByteBlockPool,
  ) -> Result<()> {
    match self {
      Self::Default { writer } => {
        let writer = writer
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("writer not initialized"))?;
        per_field.write_to_writer(writer, int_pool, byte_pool, term_byte_pool)
      },
      Self::Sorting(hook) => {
        let writer = hook
          .writer
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("writer not initialized"))?;
        per_field.write_to_writer(writer, int_pool, byte_pool, term_byte_pool)
      },
    }
  }

  fn ram_bytes_used(&self) -> Result<i64> {
    match self {
      Self::Default { writer } => writer.as_ref().map_or(Ok(0), Accountable::ram_bytes_used),
      Self::Sorting(hook) => hook
        .writer
        .as_ref()
        .map_or(Ok(0), Accountable::ram_bytes_used),
    }
  }
}

impl<D> TermVectorsConsumer<D>
where
  D: Directory + Clone,
{
  pub(crate) fn new(codec: Codecs, directory: D, hook: TermVectorsConsumerHook<D>) -> Self {
    let base = TermsHash::new(Arc::new(AtomicCounter::new()));

    let per_fields = smallvec::smallvec![PerFieldMeta::default(); 1];

    TermVectorsConsumer {
      directory,
      codec,
      has_vectors: false,
      num_vector_fields: 0,
      last_doc_id: 0,
      per_fields_idxs: per_fields,
      base,
      hook,
    }
  }
  fn reset_fields(&mut self) {
    self.per_fields_idxs.clear();
    self.num_vector_fields = 0;
  }
  fn fill(&mut self, doc_id: i32) -> Result<()> {
    self.hook.fill(&mut self.last_doc_id, doc_id)
  }

  pub(crate) fn set_has_vectors(&mut self) {
    self.has_vectors = true;
  }
  pub(crate) fn finish_document<D1>(
    &mut self,
    doc_id: i32,
    info: &SegmentInfo<D1>,
    per_fields: &mut [PerField],
    int_pool: &mut IntBlockPool,
    byte_pool: &mut ByteBlockPool,
    term_byte_pool: &mut ByteBlockPool,
  ) -> Result<()> {
    #[cfg(test)]
    let _execution_scope = ExecutionScope::enter(
      ExecutionOwner::TermVectorsConsumer,
      ExecutionMethod::FinishDocument,
    );
    if !self.has_vectors {
      return Ok(());
    }

    ArrayUtil::do_intro_sort(
      &mut self.per_fields_idxs,
      0,
      self.num_vector_fields,
      PerFieldMetaComparator { per_fields },
    )?;

    self.init_term_vectors_writer(info)?;
    self.fill(doc_id)?;
    // Append term vectors to the real outputs:
    self
      .hook
      .start_document(self.num_vector_fields.try_convert()?)?;
    let fields_to_write = self.num_vector_fields.min(self.per_fields_idxs.len());
    for i in 0..fields_to_write {
      let v = &mut per_fields[self.per_fields_idxs[i].idx];
      let terms_hash_per_field = v
        .terms_hash_per_field
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("terms_hash_per_field not initialized"))?;
      let next_per_field = terms_hash_per_field
        .next_per_field
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("next_per_field not initialized"))?;
      next_per_field.finish_document(self, int_pool, byte_pool, term_byte_pool)?;
      next_per_field.reset(term_byte_pool)
    }

    self.hook.finish_document()?;
    debug_assert_eq!(
      self.last_doc_id, doc_id,
      "last_doc_id = {}, doc_id = {}",
      self.last_doc_id, doc_id
    );

    self.last_doc_id += 1;
    int_pool.reset(false, false);
    byte_pool.reset(false, false);
    self.reset_fields();
    Ok(())
  }
  pub(crate) fn start_document(&mut self) -> Result<()> {
    self.reset_fields();
    self.num_vector_fields = 0;
    Ok(())
  }
  pub(crate) fn add_field(
    &self,
    field_info: Arc<FieldInfo>,
  ) -> Result<TermVectorsConsumerPerField> {
    TermVectorsConsumerPerField::new(self, field_info)
  }
  pub(crate) fn write_per_field(
    &mut self,
    per_field: &mut TermVectorsConsumerPerField,
    int_pool: &mut IntBlockPool,
    byte_pool: &ByteBlockPool,
    term_byte_pool: &ByteBlockPool,
  ) -> Result<()> {
    self
      .hook
      .write_per_field(per_field, int_pool, byte_pool, term_byte_pool)
  }
  pub(crate) fn add_field_to_flush(&mut self, meta: PerFieldMeta) -> Result<()> {
    let num_vector_fields = self.num_vector_fields;
    if self.per_fields_idxs.len() == self.per_fields_idxs.capacity() {
      ArrayUtil::oversize(num_vector_fields + 1, std::mem::size_of::<PerFieldMeta>())?;
      self
        .per_fields_idxs
        .reserve(num_vector_fields + 1 - self.per_fields_idxs.len());
    }

    self.per_fields_idxs.push(meta);
    self.num_vector_fields += 1;
    Ok(())
  }
  pub(crate) fn flush<DM, D1>(
    &mut self,
    state: &SegmentWriteState<D>,
    sort_map: Option<&DM>,
    info: &SegmentInfo<D1>,
  ) -> Result<()>
  where
    DM: DocMap,
  {
    self
      .hook
      .flush(&self.codec, &mut self.last_doc_id, state, sort_map, info)
  }

  fn init_term_vectors_writer<D1>(&mut self, info: &SegmentInfo<D1>) -> Result<()> {
    #[cfg(test)]
    let _execution_scope = ExecutionScope::enter(
      ExecutionOwner::TermVectorsConsumer,
      ExecutionMethod::InitTermVectorsWriter,
    );
    self.hook.init_term_vectors_writer(
      &self.directory,
      &self.codec,
      &mut self.last_doc_id,
      info,
      self.base.bytes_used.get(),
    )
  }

  pub(crate) fn abort(
    &mut self,
    int_pool: &mut IntBlockPool,
    byte_pool: &mut ByteBlockPool,
  ) -> Result<()> {
    let reset_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      int_pool.reset(false, false);
      byte_pool.reset(false, false);
      Ok(())
    }));
    let finally_result =
      std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        self.hook.abort()?;
        int_pool.reset(false, false);
        byte_pool.reset(false, false);
        Ok(())
      }));
    IOUtils::finally_caught_result(reset_result, finally_result)
  }
}

impl<D> Accountable for TermVectorsConsumer<D>
where
  D: Directory,
{
  fn ram_bytes_used(&self) -> Result<i64> {
    self.hook.ram_bytes_used()
  }
}

impl TermVectorsConsumerDefaults {
  pub(crate) fn fill<TW>(writer: &mut Option<TW>, last_doc_id: &mut i32, doc_id: i32) -> Result<()>
  where
    TW: TermVectorsWriter,
  {
    while *last_doc_id < doc_id {
      let writer = writer.as_mut().ok_or_else(|| {
        LuceneError::illegal_state("TermVectorsConsumer writer is not initialized")
      })?;
      writer.start_document(0)?;
      writer.finish_document()?;
      *last_doc_id += 1;
    }
    Ok(())
  }

  pub(crate) fn flush<TW, D1>(
    writer: &mut Option<TW>,
    last_doc_id: &mut i32,
    info: &SegmentInfo<D1>,
  ) -> Result<()>
  where
    TW: TermVectorsWriter,
  {
    if writer.is_some() {
      let num_docs = info.max_doc()?;
      debug_assert!(num_docs > 0);
      // At least one doc in this run had term vectors enabled
      let finish_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Self::fill(writer, last_doc_id, num_docs)?;
        writer
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("writer not initialized"))?
          .finish(num_docs)
      }));
      let close_result = writer
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("writer not initialized"))?
        .close();
      close_result?;
      unwrap_caught_result!(finish_result)?;
    }

    Ok(())
  }

  pub(crate) fn init_term_vectors_writer<D, D1>(
    writer: &mut Option<CodecTermVectorsWriter<D>>,
    directory: &D,
    codec: &Codecs,
    last_doc_id: &mut i32,
    info: &SegmentInfo<D1>,
    bytes_used: i64,
  ) -> Result<()>
  where
    D: Directory + Clone,
  {
    if writer.is_none() {
      let flush_info = FlushInfo::new(*last_doc_id, bytes_used);
      let context = IOContext::with_flush(flush_info)?;

      *writer = Some(codec.term_vectors_format().vectors_writer(
        directory.clone(),
        info,
        &context,
      )?);
      *last_doc_id = 0;
    }
    Ok(())
  }

  pub(crate) fn abort<TW>(writer: &mut Option<TW>) -> Result<()>
  where
    TW: TermVectorsWriter,
  {
    IOUtils::close_while_handling_exception(writer.as_mut());
    Ok(())
  }
}

pub(crate) trait TermVectorsConsumerBase {
  type Directory: Directory;
  fn flush<DM, D1>(
    &mut self,
    codec: &Codecs,
    last_doc_id: &mut i32,
    state: &SegmentWriteState<Self::Directory>,
    sort_map: Option<&DM>,
    info: &SegmentInfo<D1>,
  ) -> Result<()>
  where
    DM: DocMap;
  fn init_term_vectors_writer<D1>(
    &mut self,
    directory: &Self::Directory,
    codec: &Codecs,
    last_doc_id: &mut i32,
    info: &SegmentInfo<D1>,
    bytes_used: i64,
  ) -> Result<()>;
  fn abort(&mut self) -> Result<()>;
}

impl<D> TermVectorsConsumerBase for TermVectorsConsumerHook<D>
where
  D: Directory + Clone,
{
  type Directory = D;

  fn flush<DM, D1>(
    &mut self,
    codec: &Codecs,
    last_doc_id: &mut i32,
    state: &SegmentWriteState<Self::Directory>,
    sort_map: Option<&DM>,
    info: &SegmentInfo<D1>,
  ) -> Result<()>
  where
    DM: DocMap,
  {
    match self {
      Self::Default { writer } => TermVectorsConsumerDefaults::flush(writer, last_doc_id, info),
      Self::Sorting(hook) => hook.flush(codec, last_doc_id, state, sort_map, info),
    }
  }

  fn init_term_vectors_writer<D1>(
    &mut self,
    directory: &Self::Directory,
    codec: &Codecs,
    last_doc_id: &mut i32,
    info: &SegmentInfo<D1>,
    bytes_used: i64,
  ) -> Result<()> {
    match self {
      Self::Default { writer } => TermVectorsConsumerDefaults::init_term_vectors_writer(
        writer,
        directory,
        codec,
        last_doc_id,
        info,
        bytes_used,
      ),
      Self::Sorting(hook) => {
        hook.init_term_vectors_writer(directory, codec, last_doc_id, info, bytes_used)
      },
    }
  }

  fn abort(&mut self) -> Result<()> {
    match self {
      Self::Default { writer } => TermVectorsConsumerDefaults::abort(writer),
      Self::Sorting(hook) => hook.abort(),
    }
  }
}
