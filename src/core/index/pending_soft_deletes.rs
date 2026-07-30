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
use crate::core::codecs::field_infos_format::FieldInfosFormat;
use crate::core::codecs::{Codec, CompoundFormat};
use crate::core::index::doc_values_field_updates::{
  DocValuesFieldIterator, DocValuesFieldIteratorEnum, MergedIterator,
};
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::pending_deletes::{DocBits, PendingDeletes, PendingDeletesBase};
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_reader::{DefaultLeafReader, SegmentReader};
use crate::core::search::field_exists_query::get_doc_values_doc_id_set_iterator;
use crate::core::store::IOContext;
use crate::core::store::directory::Directory;
use crate::core::util::bit_set::BitSet;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use num_bigint::BigInt;
use std::sync::Arc;

pub(crate) struct PendingSoftDeletes {
  pub(crate) field: String,
  pub(crate) dv_generation: i64,
  pub(crate) hard_deletes: PendingDeletes,
  pub(crate) base: PendingDeletes,
}
impl PendingSoftDeletes {
  pub(crate) fn new<D>(field: &str, info: &SegmentCommitInfo<D>) -> Result<Self>
  where
    D: Directory,
  {
    let base = PendingDeletes::with(
      info.info.get_id_key().to_string(),
      None,
      info.get_del_count_with_soft_deletes(true) == 0,
      info.info.max_doc()?,
    );
    let hard_deletes = PendingDeletes::new(info)?;
    Ok(Self {
      field: field.to_string(),
      dv_generation: -2,
      hard_deletes,
      base,
    })
  }

  pub(crate) fn from_reader<D>(
    field: &str,
    reader: &SegmentReader<D>,
    info: &SegmentCommitInfo<D>,
  ) -> Result<Self>
  where
    D: Directory,
  {
    let base = PendingDeletes::from_reader(reader, info)?;
    let hard_deletes = PendingDeletes::from_reader(reader, info)?;
    Ok(Self {
      field: field.to_string(),
      dv_generation: -2,
      hard_deletes,
      base,
    })
  }

  fn assert_pending_deletes<D>(&self, info: &SegmentCommitInfo<D>) -> Result<bool>
  where
    D: Directory,
  {
    let sum = self.base.pending_delete_count + info.get_soft_del_count();
    debug_assert!(sum >= 0, "illegal pending delete count: {sum}");
    debug_assert!(info.info.max_doc()? >= self.get_del_count(info));
    Ok(true)
  }

  fn ensure_initialized<D>(
    &mut self,
    info: &SegmentCommitInfo<D>,
    reader: Option<&DefaultLeafReader<D>>,
    field_infos: Option<FieldInfos>,
    on_new_reader: bool,
  ) -> Result<()>
  where
    D: Directory,
  {
    if self.dv_generation == -2 {
      let field_infos =
        field_infos.ok_or_else(|| LuceneError::illegal_state("field_infos should not be None"))?;
      let field_info = field_infos.field_info_by_name(&self.field)?;

      // we try to only open a reader if it's really necessary i.e. indices that are mainly append
      // only might have
      // big segments that don't even have any docs in the soft deletes field. In such a case it's
      // simply
      // enough to look at the FieldInfo for the field and check if the field has DocValues
      debug_assert_eq!(do_on_new_reader(field_info.as_ref()), on_new_reader);
      if on_new_reader {
        // in order to get accurate numbers we need to have at least one reader see here.
        let reader =
          reader.ok_or_else(|| LuceneError::illegal_state("reader should not be None"))?;
        self.on_new_reader(reader, info)?;
      } else {
        // we are safe here since we don't have any doc values for the soft-delete field on disk
        // no need to open a new reader
        self.dv_generation = match field_info {
          None => -1,
          Some(field_info) => field_info.get_doc_values_gen(),
        };
      }
    }
    Ok(())
  }
}
impl std::fmt::Display for PendingSoftDeletes {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "{}(seg={:?} numPendingDeletes={} field={:?} dvGeneration={} hardDeletes={})",
      std::any::type_name::<Self>(),
      self.base.info_id,
      self.base.pending_delete_count,
      self.field,
      self.dv_generation,
      self.hard_deletes
    )
  }
}
impl PendingDeletesBase for PendingSoftDeletes {
  fn get_info_id(&self) -> &str {
    self.base.get_info_id()
  }

  fn delete<D>(&mut self, doc_id: i32, info: &SegmentCommitInfo<D>) -> Result<bool>
  where
    D: Directory,
  {
    // we need to fetch this first it might be a shared instance with
    let mutable_bits = self.base.get_mutable_bits()?;
    // hardDeletes
    if self.hard_deletes.delete(doc_id, info)? {
      if mutable_bits.get_and_clear(doc_id as usize) {
        // delete it here too!
        debug_assert!(!self.hard_deletes.delete(doc_id, info)?);
      } else {
        // if it was deleted subtract the delCount
        self.base.pending_delete_count -= 1;
        debug_assert!(self.assert_pending_deletes(info)?);
      }
      Ok(true)
    } else {
      Ok(false)
    }
  }

  fn get_hard_live_docs(&mut self) -> Option<DocBits> {
    self.hard_deletes.get_live_docs()
  }

  fn get_live_docs(&mut self) -> Option<DocBits> {
    self.base.get_live_docs()
  }

  fn num_pending_deletes(&self) -> i32 {
    self.base.num_pending_deletes() + self.hard_deletes.num_pending_deletes()
  }

  fn on_new_reader<D>(
    &mut self,
    reader: &SegmentReader<D>,
    info: &SegmentCommitInfo<D>,
  ) -> Result<()>
  where
    D: Directory,
  {
    self.base.on_new_reader(reader, info)?;
    self.hard_deletes.on_new_reader(reader, info)?;
    // only re-calculate this if we haven't seen this generation
    if self.dv_generation < info.get_doc_values_gen() {
      let new_del_count;
      let mut iterator = get_doc_values_doc_id_set_iterator(&self.field, reader)?;
      if let Some(ref mut iter) = iterator {
        if iter.next_doc()? != NO_MORE_DOCS {
          iterator = get_doc_values_doc_id_set_iterator(&self.field, reader)?;
          let mut iter = iterator.unwrap();
          new_del_count =
            apply_soft_deletes(&mut iter, self.base.get_mutable_bits()?, |_| Ok(true))?;
        } else {
          new_del_count = 0;
        }
      } else {
        new_del_count = 0;
      }
      debug_assert!(
        new_del_count >= 0,
        "illegal pending delete count: {new_del_count}"
      );
      debug_assert_eq!(
        info.get_soft_del_count(),
        new_del_count,
        "softDeleteCount doesn't match {} != {}",
        info.get_soft_del_count(),
        new_del_count
      );
      self.dv_generation = info.get_doc_values_gen();
    }
    debug_assert!(
      self.get_del_count(info) <= info.info.max_doc()?,
      "{} > {}",
      self.get_del_count(info),
      info.info.max_doc()?
    );
    Ok(())
  }

  fn drop_changes(&mut self) {
    self.hard_deletes.drop_changes()
  }

  fn write_live_docs<D1, D2>(&mut self, dir: D1, info: &mut SegmentCommitInfo<D2>) -> Result<bool>
  where
    D1: Directory,
    D2: Directory,
  {
    // we need to set this here to make sure our stats in SCI are up-to-date otherwise we might hit
    // an assertion
    // when the hard deletes are set since we need to account for docs that used to be only
    // soft-delete but now hard-deleted
    info.set_soft_del_count(info.get_soft_del_count() + self.base.pending_delete_count)?;
    self.base.drop_changes();
    // delegate the write to the hard deletes - it will only write if somebody used it.
    self.hard_deletes.write_live_docs(dir, info)
  }

  fn is_fully_deleted<D>(
    &mut self,
    info: &SegmentCommitInfo<D>,
    reader: Option<&DefaultLeafReader<D>>,
    field_infos: Option<FieldInfos>,
    on_new_reader: bool,
  ) -> Result<bool>
  where
    D: Directory,
  {
    // initialize to ensure we have accurate counts - only needed in the soft-delete case
    self.ensure_initialized(info, reader, field_infos, on_new_reader)?;
    debug_assert!(info.info.max_doc()? == self.base.max_doc);
    Ok(self.get_del_count(info) == info.info.max_doc()?)
  }

  fn num_deletes_to_merge<D>(
    &mut self,
    info: &SegmentCommitInfo<D>,
    reader: Option<&DefaultLeafReader<D>>,
    field_infos: Option<FieldInfos>,
    on_new_reader: bool,
  ) -> Result<()>
  where
    D: Directory,
  {
    self.ensure_initialized(info, reader, field_infos, on_new_reader)
  }

  fn max_doc(&self) -> i32 {
    self.base.max_doc()
  }

  fn on_doc_values_update<D>(
    &mut self,
    field_info: &FieldInfo,
    iterator: Option<MergedIterator<DocValuesFieldIteratorEnum>>,
    info: &mut SegmentCommitInfo<D>,
  ) -> Result<()>
  where
    D: Directory,
  {
    if self.field == field_info.name
      && let Some(mut iter) = iterator
    {
      let delta = apply_soft_deletes(&mut iter, self.base.get_mutable_bits()?, |iter| {
        iter.has_value()
      })?;
      self.base.pending_delete_count += delta;
      debug_assert!(self.assert_pending_deletes(info)?);
      info.set_soft_del_count(info.get_soft_del_count() + self.base.pending_delete_count)?;
      self.base.drop_changes();
    }
    debug_assert!(
      self.dv_generation < field_info.get_doc_values_gen(),
      "we have seen this generation update already: {} vs. {}",
      self.dv_generation,
      field_info.get_doc_values_gen()
    );
    debug_assert!(
      self.dv_generation != -2,
      "docValues generation is still uninitialized"
    );
    self.dv_generation = field_info.get_doc_values_gen();
    Ok(())
  }

  fn must_init_on_delete(&self) -> bool {
    !self.base.live_docs_initialized
  }
}
pub(crate) fn do_on_new_reader(field_info: Option<&Arc<FieldInfo>>) -> bool {
  matches!(
        &field_info,
        Some(field_info) if *field_info.get_doc_values_type() != DocValuesType::None)
}
pub(crate) fn read_field_infos<D>(info: &SegmentCommitInfo<D>) -> Result<FieldInfos>
where
  D: Directory,
{
  let seg_info = &info.info;
  let codec = seg_info.get_codec()?;
  if !info.has_field_updates() {
    // updates always outside of CFS
    if seg_info.get_use_compound_file() {
      let cfs = codec
        .compound_format()
        .get_compound_reader(seg_info.dir.as_ref(), seg_info)?;
      let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        codec
          .field_infos_format()
          .read(&cfs, seg_info, "", &IOContext::read_once_io_context()?)
      }));
      cfs.close()?;
      match result {
        Ok(result) => result,
        Err(payload) => std::panic::resume_unwind(payload),
      }
    } else {
      codec.field_infos_format().read(
        seg_info.dir.as_ref(),
        seg_info,
        "",
        &IOContext::read_once_io_context()?,
      )
    }
  } else {
    let segment_suffix = BigInt::from(info.get_field_infos_gen())
      .to_str_radix(36)
      .to_string();
    codec.field_infos_format().read(
      seg_info.dir.as_ref(),
      seg_info,
      &segment_suffix,
      &IOContext::read_once_io_context()?,
    )
  }
}

use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::util::bits::Bits;
pub(crate) fn count_soft_deletes(
  soft_deleted_docs: Option<&mut impl DocIdSetIterator>,
  hard_deletes: Option<&impl Bits>,
) -> Result<i32> {
  let mut count = 0;
  if let Some(docs) = soft_deleted_docs {
    loop {
      let doc = docs.next_doc()?;
      if doc == NO_MORE_DOCS {
        break;
      }
      let is_live = match hard_deletes {
        Some(bits) => bits.get(doc as usize)?,
        None => true,
      };
      if is_live {
        count += 1;
      }
    }
  }
  Ok(count)
}

/// Clears all bits in the given bitset that are set and are also in the given
/// [`DocIdSetIterator`].
///
/// # Arguments
///
/// * `iterator` - The doc ID set iterator to apply.
/// * `bits` - The bit set to apply the deletes to.
///
/// # Returns
///
/// The number of bits changed by this function.
pub(crate) fn apply_soft_deletes<I, F>(
  iterator: &mut I,
  bits: &mut FixedBitSet,
  mut has_value: F,
) -> Result<i32>
where
  I: DocIdSetIterator,
  F: FnMut(&I) -> Result<bool>,
{
  let mut new_deletes = 0;
  loop {
    let doc_id = iterator.next_doc()?;
    if doc_id == NO_MORE_DOCS {
      break;
    }
    if has_value(iterator)? {
      if bits.get_and_clear(doc_id as usize) {
        new_deletes += 1;
        // now that we know we deleted it and we fully control the hard deletes we can do correct
        // accounting
        // below.
      }
    } else if !bits.get_and_set(doc_id as usize) {
      new_deletes -= 1;
    }
  }
  Ok(new_deletes)
}
