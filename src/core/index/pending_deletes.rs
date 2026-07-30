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
use crate::core::codecs::live_docs_format::LiveDocsFormat;
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::doc_values_field_updates::{DocValuesFieldIteratorEnum, MergedIterator};
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::index_reader::{Identity, IndexReader};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::pending_soft_deletes::PendingSoftDeletes;
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_reader::{DefaultLeafReader, SegmentReader};
use crate::core::store::IOContext;
use crate::core::store::directory::Directory;
use crate::core::store::tracking_directory_wrapper::TrackingDirectoryWrapper;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::{FixedBit, FixedBitSet};
use crate::core::util::{HasIdentity, IOUtils};
use std::fmt;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub(crate) type DocBits = Arc<FixedBit>;

enum LiveDocsState {
  // Read-only live docs.
  ReadOnly(DocBits),
  // Writable live docs.
  Writable(FixedBitSet),
}

impl HasIdentity for LiveDocsState {
  fn identity(&self) -> &Identity {
    match self {
      Self::ReadOnly(bits) => bits.identity(),
      Self::Writable(bits) => bits.identity(),
    }
  }
}

impl Bits for LiveDocsState {
  fn get(&self, index: usize) -> Result<bool> {
    match self {
      Self::ReadOnly(bits) => bits.get(index),
      Self::Writable(bits) => bits.get(index),
    }
  }

  fn length(&self) -> usize {
    match self {
      Self::ReadOnly(bits) => bits.length(),
      Self::Writable(bits) => bits.length(),
    }
  }

  fn copy_of(&self) -> Result<FixedBitSet> {
    match self {
      Self::ReadOnly(bits) => bits.copy_of(),
      Self::Writable(bits) => bits.copy_of(),
    }
  }
}

/// This struct handles accounting and applies pending deletes for live segment readers.
pub(crate) struct PendingDeletes {
  // SegmentInfo::id
  pub(crate) info_id: String,
  live_docs: Option<LiveDocsState>,
  pub(crate) pending_delete_count: i32,
  pub(crate) live_docs_initialized: bool,
  pub(crate) max_doc: i32,
}
impl PendingDeletes {
  pub(crate) fn from_reader<D>(
    reader: &SegmentReader<D>,
    info: &SegmentCommitInfo<D>,
  ) -> Result<Self>
  where
    D: Directory,
  {
    let mut v = Self::with(
      info.info.get_id_key().to_string(),
      reader.get_live_docs()?,
      true,
      info.info.max_doc()?,
    );
    v.pending_delete_count = reader.num_deleted_docs()? - info.get_del_count();
    Ok(v)
  }
  pub(crate) fn new<D>(info: &SegmentCommitInfo<D>) -> Result<Self>
  where
    D: Directory,
  {
    Ok(PendingDeletes::with(
      info.info.get_id_key().to_string(),
      None,
      !info.has_deletions(),
      info.info.max_doc()?,
    ))
    // if we don't have deletions we can mark it as initialized since we might receive deletes on a
    // segment
    // without having a reader opened on it ie. after a merge when we apply the deletes that IW
    // received while merging.
    // For segments that were published we enforce a reader in the
    // BufferedUpdatesStream.SegmentState ctor
  }

  pub(crate) fn with(
    info_id: String,
    live_docs: Option<DocBits>,
    live_docs_initialized: bool,
    max_doc: i32,
  ) -> Self {
    PendingDeletes {
      info_id,
      live_docs: live_docs.map(LiveDocsState::ReadOnly),
      pending_delete_count: 0,
      live_docs_initialized,
      max_doc,
    }
  }
  pub(crate) fn get_mutable_bits(&mut self) -> Result<&mut FixedBitSet> {
    // If we pull mutable bits but we haven't been initialized something is completely off.
    debug_assert!(
      self.live_docs_initialized,
      "can't delete if liveDocs are not initialized",
    );
    let writable_live_docs = match self.live_docs.as_ref() {
      Some(LiveDocsState::ReadOnly(bits)) => Some(bits.copy_of()?),
      Some(LiveDocsState::Writable(_)) => None,
      None => {
        let mut bits = FixedBitSet::new(self.max_doc as usize);
        bits.set_with_range(0, self.max_doc as usize);
        Some(bits)
      },
    };
    if let Some(writable_live_docs) = writable_live_docs {
      self.live_docs = Some(LiveDocsState::Writable(writable_live_docs));
    }
    match self.live_docs.as_mut() {
      Some(LiveDocsState::Writable(bits)) => Ok(bits),
      _ => Err(LuceneError::illegal_state(
        "live_docs should be FixedBitSet",
      )),
    }
  }

  fn assert_check_live_docs(
    &self,
    bits: &impl Bits,
    expected_length: i32,
    expected_delete_count: i32,
  ) -> Result<bool> {
    debug_assert_eq!(
      bits.length(),
      expected_length as usize,
      "length: {} != expected: {}",
      bits.length(),
      expected_length
    );

    let mut deleted = 0;
    for i in 0..bits.length() {
      if !bits.get(i)? {
        deleted += 1;
      }
    }

    debug_assert_eq!(
      deleted, expected_delete_count,
      "deleted: {deleted} != expected: {expected_delete_count}"
    );

    Ok(true)
  }
}
impl PendingDeletesBase for PendingDeletes {
  fn get_info_id(&self) -> &str {
    &self.info_id
  }

  fn delete<D>(&mut self, doc_id: i32, _info: &SegmentCommitInfo<D>) -> Result<bool>
  where
    D: Directory,
  {
    debug_assert!(self.max_doc > 0);

    let mutable_bits = self.get_mutable_bits()?;
    debug_assert!(mutable_bits.length() > 0);

    debug_assert!(
      (0..mutable_bits.length()).contains(&(doc_id as usize)),
      "out of bounds: docID={} liveDocsLength={} seg={} maxDoc={}",
      doc_id,
      mutable_bits.length(),
      self.info_id,
      self.max_doc
    );

    let did_delete = mutable_bits.get_and_clear(doc_id as usize);
    if did_delete {
      self.pending_delete_count += 1;
    }
    Ok(did_delete)
  }

  fn get_hard_live_docs(&mut self) -> Option<DocBits> {
    self.get_live_docs()
  }

  fn get_live_docs(&mut self) -> Option<DocBits> {
    // Prevent modifications to the returned live docs
    self.live_docs.take().map(|live_docs| {
      let bits = match live_docs {
        LiveDocsState::ReadOnly(bits) => bits,
        LiveDocsState::Writable(bits) => Arc::new(bits.to_read_only_bits()),
      };
      self.live_docs = Some(LiveDocsState::ReadOnly(bits.clone()));
      bits
    })
  }

  fn num_pending_deletes(&self) -> i32 {
    self.pending_delete_count
  }

  fn on_new_reader<D>(
    &mut self,
    reader: &SegmentReader<D>,
    info: &SegmentCommitInfo<D>,
  ) -> Result<()>
  where
    D: Directory,
  {
    if !self.live_docs_initialized {
      debug_assert!(!matches!(&self.live_docs, Some(LiveDocsState::Writable(_))));
      if reader.has_deletions()? {
        // we only initialize this once either in the ctor or here
        // if we use the live docs from a reader it has to be in a situation where we don't
        // have any existing live docs
        debug_assert_eq!(
          self.pending_delete_count, 0,
          "pendingDeleteCount: {}",
          self.pending_delete_count
        );
        self.live_docs = reader.get_live_docs()?.map(LiveDocsState::ReadOnly);

        if let Some(bits) = &self.live_docs {
          let max_doc = info.info.max_doc()?;
          let del_count = info.get_del_count();
          debug_assert!(
            self
              .assert_check_live_docs(bits, max_doc, del_count)
              .unwrap_or(false)
          );
        }
      }
      self.live_docs_initialized = true;
    }
    Ok(())
  }
  /// Resets the pending docs
  fn drop_changes(&mut self) {
    self.pending_delete_count = 0;
  }

  fn write_live_docs<D1, D2>(&mut self, dir: D1, info: &mut SegmentCommitInfo<D2>) -> Result<bool>
  where
    D1: Directory,
    D2: Directory,
  {
    if self.pending_delete_count == 0 {
      return Ok(false);
    }

    let live_docs = match self.live_docs.as_ref() {
      Some(ld) => ld,
      None => return Err(LuceneError::illegal_state("liveDocs must be initialized")),
    };
    debug_assert!(info.info.max_doc()? == self.max_doc);
    // We have new deletes
    debug_assert_eq!(
      live_docs.length(),
      info.info.max_doc()? as usize,
      "liveDocs.length must match maxDoc"
    );
    // Do this so we can delete any created files on
    // error; this saves all codecs from having to do
    // it:
    let tracking_dir = TrackingDirectoryWrapper::new(dir);
    // We can write directly to the actual name (vs to a
    // .tmp & renaming it) because the file is not live
    // until segments file is written:
    let write_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      let codec = info.info.get_codec()?;
      codec.live_docs_format().write_live_docs(
        live_docs,
        &tracking_dir,
        info,
        self.pending_delete_count,
        &IOContext::default_io_context()?,
      )?;
      Ok(())
    }));

    if !matches!(&write_res, Ok(Ok(()))) {
      // Advance only the nextWriteDelGen so that a 2nd
      // attempt to write will write to a new file
      info.advance_next_write_del_gen();
      // Delete any partially created file(s):
      IOUtils::delete_files_ignoring_exceptions(
        &tracking_dir.in_,
        &tracking_dir.get_created_files().lock().created_filenames,
      );
    }
    match write_res {
      Ok(result) => result?,
      Err(payload) => std::panic::resume_unwind(payload),
    }
    // If we hit an exc in the line above (eg disk full)
    // then info's delGen remains pointing to the previous
    // (successfully written) del docs:
    info.advance_del_gen();
    let new_del_count = info.get_del_count() + self.pending_delete_count;
    info.set_del_count(new_del_count)?;
    self.drop_changes();

    Ok(true)
  }

  fn is_fully_deleted<D>(
    &mut self,
    info: &SegmentCommitInfo<D>,
    _reader: Option<&DefaultLeafReader<D>>,
    _field_infos: Option<FieldInfos>,
    _on_new_reader: bool,
  ) -> Result<bool>
  where
    D: Directory,
  {
    debug_assert!(info.info.max_doc()? == self.max_doc);
    Ok(self.get_del_count(info) == info.info.max_doc()?)
  }

  fn num_deletes_to_merge<D>(
    &mut self,
    _info: &SegmentCommitInfo<D>,
    _reader: Option<&DefaultLeafReader<D>>,
    _field_infos: Option<FieldInfos>,
    _on_new_reader: bool,
  ) -> Result<()>
  where
    D: Directory,
  {
    Ok(())
  }

  fn max_doc(&self) -> i32 {
    self.max_doc
  }

  fn must_init_on_delete(&self) -> bool {
    false
  }
}
impl fmt::Display for PendingDeletes {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(
      f,
      "{}(seg={} numPendingDeletes={} writeable={})",
      std::any::type_name::<Self>(),
      self.info_id,
      self.pending_delete_count,
      matches!(&self.live_docs, Some(LiveDocsState::Writable(_)))
    )
  }
}

pub(crate) trait PendingDeletesBase: Display {
  fn get_info_id(&self) -> &str;
  /// Marks a document as deleted in this segment and return true if a document got actually deleted or if the document was already deleted.
  fn delete<D>(&mut self, doc_id: i32, info: &SegmentCommitInfo<D>) -> Result<bool>
  where
    D: Directory;
  /// Returns a snapshot of the hard live docs.
  fn get_hard_live_docs(&mut self) -> Option<DocBits>;
  /// Returns a snapshot of the current live docs.
  fn get_live_docs(&mut self) -> Option<DocBits>;
  /// Returns the number of pending deletes that are not yet flushed to disk.
  fn num_pending_deletes(&self) -> i32;
  /// Called once a new reader is opened for this segment ie. when deletes or updates are applied.
  fn on_new_reader<D>(
    &mut self,
    reader: &SegmentReader<D>,
    info: &SegmentCommitInfo<D>,
  ) -> Result<()>
  where
    D: Directory;
  fn drop_changes(&mut self);
  /// Writes the live docs to disk and returns `true` if any new docs were written.
  fn write_live_docs<D1, D2>(&mut self, dir: D1, info: &mut SegmentCommitInfo<D2>) -> Result<bool>
  where
    D1: Directory,
    D2: Directory;
  fn is_fully_deleted<D>(
    &mut self,
    info: &SegmentCommitInfo<D>,
    reader: Option<&DefaultLeafReader<D>>,
    field_infos: Option<FieldInfos>,
    on_new_reader: bool,
  ) -> Result<bool>
  where
    D: Directory;

  fn num_deletes_to_merge<D>(
    &mut self,
    info: &SegmentCommitInfo<D>,
    reader: Option<&DefaultLeafReader<D>>,
    field_infos: Option<FieldInfos>,
    on_new_reader: bool,
  ) -> Result<()>
  where
    D: Directory;

  /// Returns true if the given reader needs to be refreshed to see the latest deletes
  fn needs_refresh<D>(
    &mut self,
    reader: &SegmentReader<D>,
    info: &SegmentCommitInfo<D>,
  ) -> Result<bool>
  where
    D: Directory,
  {
    let live_docs_changed = match (reader.get_live_docs()?, self.get_live_docs()) {
      (None, None) => false,
      (Some(reader_bits), Some(current_bits)) => reader_bits.identity() != current_bits.identity(),
      _ => true,
    };
    Ok(live_docs_changed || reader.num_deleted_docs()? != self.get_del_count(info))
  }
  /// Returns the number of deleted docs in the segment.
  fn get_del_count<D>(&self, info: &SegmentCommitInfo<D>) -> i32
  where
    D: Directory,
  {
    info.get_del_count() + info.get_soft_del_count() + self.num_pending_deletes()
  }
  /// Returns the number of live documents in this segment
  fn num_docs<D>(&self, info: &SegmentCommitInfo<D>) -> Result<i32>
  where
    D: Directory,
  {
    debug_assert!(info.info.max_doc()? == self.max_doc());
    let max_doc = info.info.max_doc()?;
    Ok(max_doc - self.get_del_count(info))
  }
  // Call only from assert!
  fn verify_doc_counts<D>(
    &mut self,
    reader: &impl CodecReader,
    info: &SegmentCommitInfo<D>,
  ) -> Result<bool>
  where
    D: Directory,
  {
    debug_assert!(info.info.max_doc()? == self.max_doc());
    let max_doc = info.info.max_doc()?;
    let mut count = 0;
    match self.get_live_docs() {
      Some(bits) => {
        for doc_id in 0..max_doc {
          if bits.get(doc_id as usize)? {
            count += 1;
          }
        }
      },
      _ => {
        count = max_doc;
      },
    }

    debug_assert_eq!(
      self.num_docs(info)?,
      count,
      "info.maxDoc={} info.getDelCount={} info.getSoftDelCount={} pendingDeletes={} count={} numDocs={}",
      max_doc,
      info.get_del_count(),
      info.get_soft_del_count(),
      self.num_pending_deletes(),
      count,
      self.num_docs(info)?
    );

    debug_assert_eq!(
      reader.num_docs()?,
      self.num_docs(info)?,
      "reader.numDocs={} numDocs={}",
      reader.num_docs()?,
      self.num_docs(info)?
    );

    debug_assert!(
      reader.num_deleted_docs()? <= max_doc,
      "delCount={} info.maxDoc={} pendingDeleteCount={} info.getDelCount={}",
      reader.num_deleted_docs()?,
      max_doc,
      self.num_pending_deletes(),
      info.get_del_count()
    );
    Ok(true)
  }
  fn max_doc(&self) -> i32;
  fn on_doc_values_update<D>(
    &mut self,
    _field_info: &FieldInfo,
    _iterator: Option<MergedIterator<DocValuesFieldIteratorEnum>>,
    _info: &mut SegmentCommitInfo<D>,
  ) -> Result<()>
  where
    D: Directory,
  {
    Ok(())
  }
  /// Returns `true` if this `PendingDeletes` must be initialized before [`delete`](Self::delete);
  /// otherwise it is ready to accept deletes.
  /// A `PendingDeletes` can be initialized by providing it a reader via [`on_new_reader`](Self::on_new_reader).
  fn must_init_on_delete(&self) -> bool;
}

pub(crate) enum PendingDeletesEnum {
  PD(PendingDeletes),
  Soft(PendingSoftDeletes),
}
impl PendingDeletesEnum {
  pub(crate) fn dv_gen(&self) -> Result<i64> {
    match self {
      PendingDeletesEnum::PD(_a) => Err(LuceneError::unsupported_operation("no dvGen for PD")),
      PendingDeletesEnum::Soft(b) => Ok(b.dv_generation),
    }
  }
  pub(crate) fn field(&self) -> Result<&str> {
    match self {
      PendingDeletesEnum::PD(_a) => Err(LuceneError::unsupported_operation("no dvGen for PD")),
      PendingDeletesEnum::Soft(b) => Ok(&b.field),
    }
  }
}

impl Display for PendingDeletesEnum {
  fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
    match self {
      PendingDeletesEnum::PD(a) => a.fmt(f),
      PendingDeletesEnum::Soft(b) => b.fmt(f),
    }
  }
}

impl PendingDeletesBase for PendingDeletesEnum {
  fn get_info_id(&self) -> &str {
    match self {
      PendingDeletesEnum::PD(a) => a.get_info_id(),
      PendingDeletesEnum::Soft(b) => b.get_info_id(),
    }
  }

  fn delete<D>(&mut self, doc_id: i32, info: &SegmentCommitInfo<D>) -> Result<bool>
  where
    D: Directory,
  {
    match self {
      PendingDeletesEnum::PD(a) => a.delete(doc_id, info),
      PendingDeletesEnum::Soft(b) => b.delete(doc_id, info),
    }
  }

  fn get_hard_live_docs(&mut self) -> Option<DocBits> {
    match self {
      PendingDeletesEnum::PD(a) => a.get_hard_live_docs(),
      PendingDeletesEnum::Soft(b) => b.get_hard_live_docs(),
    }
  }

  fn get_live_docs(&mut self) -> Option<DocBits> {
    match self {
      PendingDeletesEnum::PD(a) => a.get_live_docs(),
      PendingDeletesEnum::Soft(b) => b.get_live_docs(),
    }
  }

  fn num_pending_deletes(&self) -> i32 {
    match self {
      PendingDeletesEnum::PD(a) => a.num_pending_deletes(),
      PendingDeletesEnum::Soft(b) => b.num_pending_deletes(),
    }
  }

  fn on_new_reader<D>(
    &mut self,
    reader: &SegmentReader<D>,
    info: &SegmentCommitInfo<D>,
  ) -> Result<()>
  where
    D: Directory,
  {
    match self {
      PendingDeletesEnum::PD(a) => a.on_new_reader(reader, info),
      PendingDeletesEnum::Soft(b) => b.on_new_reader(reader, info),
    }
  }

  fn drop_changes(&mut self) {
    match self {
      PendingDeletesEnum::PD(a) => a.drop_changes(),
      PendingDeletesEnum::Soft(b) => b.drop_changes(),
    }
  }

  fn write_live_docs<D1, D2>(&mut self, dir: D1, info: &mut SegmentCommitInfo<D2>) -> Result<bool>
  where
    D1: Directory,
    D2: Directory,
  {
    match self {
      PendingDeletesEnum::PD(a) => a.write_live_docs(dir, info),
      PendingDeletesEnum::Soft(b) => b.write_live_docs(dir, info),
    }
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
    match self {
      PendingDeletesEnum::PD(a) => a.is_fully_deleted(info, reader, field_infos, on_new_reader),
      PendingDeletesEnum::Soft(b) => b.is_fully_deleted(info, reader, field_infos, on_new_reader),
    }
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
    match self {
      PendingDeletesEnum::PD(a) => a.num_deletes_to_merge(info, reader, field_infos, on_new_reader),
      PendingDeletesEnum::Soft(b) => {
        b.num_deletes_to_merge(info, reader, field_infos, on_new_reader)
      },
    }
  }

  fn max_doc(&self) -> i32 {
    match self {
      PendingDeletesEnum::PD(a) => a.max_doc(),
      PendingDeletesEnum::Soft(b) => b.max_doc(),
    }
  }

  fn on_doc_values_update<D>(
    &mut self,
    info: &FieldInfo,
    iterator: Option<MergedIterator<DocValuesFieldIteratorEnum>>,
    segment_info: &mut SegmentCommitInfo<D>,
  ) -> Result<()>
  where
    D: Directory,
  {
    match self {
      PendingDeletesEnum::PD(a) => a.on_doc_values_update(info, iterator, segment_info),
      PendingDeletesEnum::Soft(b) => b.on_doc_values_update(info, iterator, segment_info),
    }
  }

  fn must_init_on_delete(&self) -> bool {
    match self {
      PendingDeletesEnum::PD(a) => a.must_init_on_delete(),
      PendingDeletesEnum::Soft(b) => b.must_init_on_delete(),
    }
  }
}
