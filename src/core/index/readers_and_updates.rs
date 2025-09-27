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
use crate::core::codecs::dummy::dummy_binary_doc_values::DummyBinaryDocValues;
use crate::core::codecs::dummy::dummy_doc_values_skipper::DummyDocValuesSkipper;
use crate::core::codecs::dummy::dummy_numeric_doc_values::DummyNumericDocValues;
use crate::core::codecs::dummy::dummy_sorted_doc_values::DummySortedDocValues;
use crate::core::codecs::dummy::dummy_sorted_numeric_doc_values::DummySortedNumericDocValues;
use crate::core::codecs::dummy::dummy_sorted_set_doc_values::DummySortedSetDocValues;
use crate::core::codecs::field_infos_format::FieldInfosFormat;
use crate::core::codecs::{Codec, get_default_code};
use crate::core::index::BytesRef;
use crate::core::index::binary_doc_values::BinaryDocValues;
use crate::core::index::doc_values_field_updates::merged_iterator;
use crate::core::index::doc_values_field_updates::{
    BinaryDocValuesDVFU, DocValuesFieldIterator, DocValuesFieldIteratorEnum,
    DocValuesFieldUpdatesEnum, MergedIterator, NumericDocValuesDVFU,
};
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::{FieldInfos, FieldNumbers};
use crate::core::index::index_reader::IndexReader;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::merge_policy::MergePolicy;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::pending_deletes::{DocBits, PendingDeletesBase, PendingDeletesEnum};
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_reader::SegmentReader;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::sorter::DocMapImpl;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::Either2DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::store::IOContext;
use crate::core::store::directory::Directory;
use crate::core::store::flush_info::FlushInfo;
use crate::core::store::lock_validating_directory_wrapper::LockValidatingDirectoryWrapper;
use crate::core::store::tracking_directory_wrapper::TrackingDirectoryWrapper;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::function::Function;
use crate::core::util::info_stream::InfoStream;
use crate::core::util::{CoreHelper, IOUtils};
use parking_lot::Mutex;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, AtomicI64, Ordering};

/// Used by IndexWriter to hold open SegmentReaders (for
/// searching or merging), plus pending deletes and updates,
/// for a given segment
pub(crate) struct ReadersAndUpdates<D>
where
    D: Directory,
{
    // Tracks how many consumers are using this instance:
    ref_count: AtomicI32, // starts at 1
    // the major version this index was created with
    index_created_version_major: i32,
    // Only set if there are doc values updates against this segment, and the index is sorted:
    pub(crate) sort_map: Option<Arc<DocMapImpl>>,
    pub(crate) ram_bytes_used: AtomicI64,
    pub(crate) inner: Mutex<Inner<D>>,
    pub(crate) info_id: String,
}

pub(crate) struct Inner<D>
where
    D: Directory,
{
    // Set once (None, and then maybe set, and never set again):
    pub(crate) reader: Option<SegmentReader<D>>,
    // How many further deletions we've done against
    // liveDocs vs when we loaded it or last wrote it:
    pending_deletes: PendingDeletesEnum,
    // Indicates whether this segment is currently being merged. While a segment
    // is merging, all field updates are also registered in the
    // mergingDVUpdates map. Also, calls to writeFieldUpdates merge the
    // updates with mergingDVUpdates.
    // That way, when the segment is done merging, IndexWriter can apply the
    // updates on the merged segment too.
    is_merging: bool,
    // Holds resolved (to docIDs) doc values updates that have not yet been
    // written to the index
    pending_dv_updates: HashMap<String, Vec<Arc<DocValuesFieldUpdatesEnum>>>,
    // Holds resolved (to docIDs) doc values updates that were resolved while
    // this segment was being merged; at the end of the merge we carry over
    // these updates (remapping their docIDs) to the newly merged segment
    merging_dv_updates: HashMap<String, Vec<Arc<DocValuesFieldUpdatesEnum>>>,
}

impl<D> ReadersAndUpdates<D>
where
    D: Directory,
{
    pub(crate) fn new(
        index_created_version_major: i32,
        pending_deletes: PendingDeletesEnum,
    ) -> Self {
        let info_id = pending_deletes.get_info_id().to_string();
        let inner = Mutex::new(Inner {
            reader: None,
            pending_deletes,
            is_merging: false,
            pending_dv_updates: HashMap::new(),
            merging_dv_updates: HashMap::new(),
        });
        Self {
            ref_count: AtomicI32::new(1),
            index_created_version_major,
            sort_map: None,
            ram_bytes_used: AtomicI64::new(0),
            inner,
            info_id,
        }
    }
    /// Init from a previously opened SegmentReader.
    pub(crate) fn with_reader(
        index_created_version_major: i32,
        reader: SegmentReader<D>,
        info: &SegmentCommitInfo<D>,
        pending_deletes: PendingDeletesEnum,
    ) -> Result<Self> {
        debug_assert!(info.info.get_id_str() == reader.info_id);
        let v = Self::new(index_created_version_major, pending_deletes);
        {
            let mut inner = v.inner.lock();
            inner.pending_deletes.on_new_reader(&reader, info)?;
            inner.reader = Some(reader);
        }
        Ok(v)
    }
    pub fn inc_ref(&self) {
        let rc = self.ref_count.fetch_add(1, Ordering::SeqCst) + 1;
        debug_assert!(rc > 1);
    }

    pub fn dec_ref(&self) {
        let rc = self.ref_count.fetch_sub(1, Ordering::SeqCst) - 1;
        debug_assert!(rc >= 0);
    }

    pub fn ref_count(&self) -> i32 {
        let rc = self.ref_count.load(Ordering::SeqCst);
        debug_assert!(rc >= 0);
        rc
    }
    pub(crate) fn get_del_count(&self, info: &SegmentCommitInfo<D>) -> i32
    where
        D: Directory,
    {
        self.inner.lock().pending_deletes.get_del_count(info)
    }

    fn assert_no_dup_gen(
        &self,
        field_updates: &[Arc<DocValuesFieldUpdatesEnum>],
        update: &DocValuesFieldUpdatesEnum,
    ) -> bool {
        let dup = field_updates
            .iter()
            .any(|old_update| old_update.del_gen() == update.del_gen());
        debug_assert!(!dup, "duplicate delGen={}", update.del_gen());
        true
    }
    /// Adds a new resolved (meaning it maps docIDs to new values) doc values packet.
    /// We buffer these in RAM and write to disk when too much RAM is used or when a merge needs to kick off, or a commit/refresh.
    pub fn add_dv_update(&self, update: DocValuesFieldUpdatesEnum) -> Result<()> {
        let mut inner = self.inner.lock();
        if !update.get_finished()? {
            return Err(LuceneError::illegal_argument("call finish first"));
        }

        let field = update.field().to_string();
        let update_bytes = update.ram_bytes_used()?;

        let field_updates = inner.pending_dv_updates.entry(field.clone()).or_default();

        debug_assert!(self.assert_no_dup_gen(field_updates, &update));
        let update = Arc::new(update);
        self.ram_bytes_used
            .fetch_add(update_bytes, Ordering::Relaxed);

        field_updates.push(update.clone());

        if inner.is_merging {
            inner
                .merging_dv_updates
                .entry(field)
                .or_default()
                .push(update);
        }
        Ok(())
    }

    pub(crate) fn get_num_dv_updates(&self) -> i64 {
        let inner = self.inner.lock();
        inner
            .pending_dv_updates
            .values()
            .map(|v| v.len() as i64)
            .sum()
    }
    pub fn get_reader(
        &self,
        context: &IOContext,
        info: &SegmentCommitInfo<D>,
        inner: Option<&mut Inner<D>>,
    ) -> Result<()> {
        let inner = match inner {
            Some(inner) => inner,
            None => &mut *self.inner.lock(),
        };
        if inner.reader.is_none() {
            let reader = SegmentReader::new(info, self.index_created_version_major, context)?;
            inner.pending_deletes.on_new_reader(&reader, info)?;
            inner.reader = Some(reader);
        }
        // Ref for caller
        inner.reader.as_ref().unwrap().inc_ref()?;
        Ok(())
    }
    pub fn release(&self) -> Result<()> {
        // TODO
        self.inner.lock().reader.as_ref().unwrap().dec_ref()?;
        Ok(())
    }

    pub fn delete(&self, doc_id: i32, info: &SegmentCommitInfo<D>) -> Result<bool> {
        let mut inner = self.inner.lock();

        if inner.reader.is_none() && inner.pending_deletes.must_init_on_delete() {
            self.get_reader(&IOContext::default_io_context()?, info, Some(&mut inner))?; // pass a reader to initialize the pending deletes
        }

        inner.pending_deletes.delete(doc_id, info)
    }
    pub fn drop_readers(&self) -> Result<()> {
        let mut inner = self.inner.lock();
        // TODO: can we somehow use IOUtils here...?  problem is
        // we are calling .decRef not .close)...
        if let Some(reader) = &inner.reader {
            reader.dec_ref()?;
        }
        inner.reader = None;
        Ok(())
    }
    /// Returns a ref to a clone. NOTE: you should decRef() the reader when you're done (ie do not call close()).
    pub(crate) fn get_read_only_clone(
        &self,
        context: &IOContext,
        info: &SegmentCommitInfo<D>,
    ) -> Result<Option<SegmentReader<D>>> {
        let mut inner = self.inner.lock();
        if inner.reader.is_none() {
            self.get_reader(context, info, Some(&mut inner))?;
            debug_assert!(inner.reader.is_some());
            inner.reader.as_ref().unwrap().dec_ref()?;
        }

        // force new liveDocs
        if let Some(live_docs) = inner.pending_deletes.get_live_docs() {
            let hard_live_docs = inner.pending_deletes.get_hard_live_docs();
            let sr = SegmentReader::new_from_reader(
                info,
                inner.reader.as_ref().unwrap(),
                Some(live_docs),
                hard_live_docs,
                inner.pending_deletes.num_docs(info)?,
                true,
            )?;
            return Ok(Some(sr));
        }
        {
            // liveDocs == null and reader != null. That can only be if there are no deletes
            let r = inner.reader.as_ref().unwrap();
            debug_assert!(r.get_live_docs()?.is_none());
            r.inc_ref()?;
            // Self.inner.reader;
            Ok(None)
        }
    }

    fn get_latest_read(
        &self,
        info: &SegmentCommitInfo<D>,
        inner: Option<&mut Inner<D>>,
    ) -> Result<()> {
        let mut inner = match inner {
            Some(inner) => inner,
            None => &mut *self.inner.lock(),
        };
        if inner.reader.is_none() {
            // get a reader and dec the ref right away we just make sure we have a reader
            self.get_reader(&IOContext::default_io_context()?, info, Some(&mut inner))?;
            inner.reader.as_ref().unwrap().dec_ref()?;
        }
        // we should take the reader out of the struct temporarily, cause borrow check
        // it is safe take reader under lock because we put it back right away
        let reader = inner.reader.take();
        if inner
            .pending_deletes
            .needs_refresh(reader.as_ref().unwrap(), info)?
        {
            // we have a reader but its live-docs are out of sync. let's create a temporary one that we
            // never share
            self.swap_new_reader_with_latest_live_docs(inner, info)?;
        }
        // put reader back
        inner.reader = reader;
        debug_assert!(inner.reader.is_some());
        Ok(())
    }

    /// Returns a snapshot of the live docs.
    pub fn get_live_docs(&self) -> Option<DocBits> {
        let mut inner = self.inner.lock();
        inner.pending_deletes.get_live_docs()
    }

    /// Returns the live-docs bits excluding documents that are not live due to soft-deletes.
    pub fn get_hard_live_docs(&self) -> Option<DocBits> {
        let mut inner = self.inner.lock();
        inner.pending_deletes.get_hard_live_docs()
    }
    pub fn drop_changes(&self) {
        // Discard (don't save) changes when we are dropping
        // the reader; this is used only on the sub-readers
        // after a successful merge.  If deletes had
        // accumulated on those sub-readers while the merge
        // is running, by now we have carried forward those
        // deletes onto the newly merged segment, so we can
        // discard them on the sub-readers:
        let mut inner = self.inner.lock();
        inner.pending_deletes.drop_changes();
        self.drop_merging_updates(Some(&mut inner));
    }
    // Commit live docs (writes new _X_N.del files) and field updates (writes new
    // _X_N updates files) to the directory; returns true if it wrote any file
    // and false if there were no new deletes or updates to write:
    pub fn write_live_docs(
        &self,
        dir: Arc<LockValidatingDirectoryWrapper<D>>,
        info: &mut SegmentCommitInfo<D>,
    ) -> Result<bool>
    where
        D: Directory,
    {
        let mut inner = self.inner.lock();
        inner.pending_deletes.write_live_docs(dir, info)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn handle_dv_updates<F>(
        &self,
        infos: &FieldInfos,
        dir: Arc<TrackingDirectoryWrapper<LockValidatingDirectoryWrapper<D>>>,
        dv_format: &F,
        inner: &mut Inner<D>,
        field_files: &mut HashMap<i32, HashSet<String>>,
        max_del_gen: i64,
        info_stream: &impl InfoStream,
        info: &mut SegmentCommitInfo<D>,
    ) -> Result<()>
    where
        F: DocValuesFormat,
    {
        for (field, updates) in inner.pending_dv_updates.iter() {
            let ty = updates[0].tp();
            debug_assert!(
                matches!(ty, DocValuesType::Numeric | DocValuesType::Binary),
                "unsupported type: {:?}",
                ty
            );

            let mut updates_to_apply = Vec::new();
            let mut bytes: i64 = 0;

            for update in updates {
                if update.del_gen() <= max_del_gen {
                    // safe to apply this one
                    bytes += update.ram_bytes_used()?;
                    updates_to_apply.push(update.clone());
                }
            }

            if updates_to_apply.is_empty() {
                // nothing to apply yet
                continue;
            }

            if info_stream.enabled("BD") {
                info_stream.message(
                    "BD",
                    &format!(
                        "now write {} pending numeric DV updates for field={}, seg={}, bytes={:.3} MB",
                        updates_to_apply.len(),
                        field,
                        info,
                        (bytes as f64) / 1024.0 / 1024.0
                    ),
                );
            }

            let next_doc_values_gen = info.get_next_doc_values_gen();
            let segment_suffix = num_bigint::BigInt::from(next_doc_values_gen)
                .to_str_radix(36)
                .to_string();
            let updates_context =
                IOContext::with_flush(FlushInfo::new(info.info.max_doc()?, bytes))?;

            let field_info = infos
                .field_info_by_name(field)
                .ok_or_else(|| LuceneError::illegal_argument("fieldInfo is None"))?;
            field_info.set_doc_values_gen(next_doc_values_gen)?;

            let field_infos = Arc::new(FieldInfos::new(vec![field_info.clone()])?);

            let tracking_dir = TrackingDirectoryWrapper::new(dir.clone());

            let state = SegmentWriteState::with_suffix(
                None,
                &tracking_dir,
                field_infos,
                &updates_context,
                &segment_suffix,
            );

            {
                let mut fields_consumer = dv_format.fields_consumer(&state, &info.info)?;

                let update_supplier = FunctionImpl::new(field_info.clone(), updates_to_apply);

                inner
                    .pending_deletes
                    .on_doc_values_update(&field_info, update_supplier.apply(&field_info)?);
                if *ty == DocValuesType::Binary {
                    let v = DocValuesProducerBinary::new(
                        update_supplier,
                        field,
                        inner.reader.as_mut().unwrap(),
                        field_info.clone(),
                    );
                    fields_consumer.add_binary_field(&field_info, &v)?
                } else {
                    let v = DocValuesProducerNumeric::new(
                        update_supplier,
                        field,
                        inner.reader.as_mut().unwrap(),
                        field_info.clone(),
                    );
                    fields_consumer.add_numeric_field(&field_info, &v)?;
                }

                drop(fields_consumer);
            }

            info.advance_doc_values_gen();
            debug_assert!(!field_files.contains_key(&field_info.number));
            field_files.insert(
                field_info.number,
                state
                    .directory
                    .get_created_files()
                    .lock()
                    .created_filenames
                    .clone(),
            );
        }
        Ok(())
    }

    fn write_field_infos_gen<F>(
        &self,
        field_infos: &FieldInfos,
        dir: Arc<TrackingDirectoryWrapper<LockValidatingDirectoryWrapper<D>>>,
        infos_format: &F,
        info: &mut SegmentCommitInfo<D>,
    ) -> Result<HashSet<String>>
    where
        F: FieldInfosFormat,
    {
        let next_field_infos_gen = info.get_next_field_infos_gen();
        let segment_suffix = num_bigint::BigInt::from(next_field_infos_gen).to_str_radix(36);
        // we write approximately that many bytes (based on Lucene46DVF):
        // HEADER + FOOTER: 40
        // 90 bytes per-field (over estimating long name and attributes map)
        let est_infos_size = 40 + 90 * (field_infos.size() as i64);
        // IOContext for a flush with estimated size
        let flush_info = FlushInfo::new(info.info.max_doc()?, est_infos_size);
        let infos_context = IOContext::with_flush(flush_info)?;
        // separately also track which files were created for this gen
        let mut tracking_dir = TrackingDirectoryWrapper::new(dir);
        infos_format.write(
            &tracking_dir,
            &info.info,
            &segment_suffix,
            field_infos,
            &infos_context,
        )?;
        info.advance_field_infos_gen();
        Ok(tracking_dir.take_created_files())
    }
    pub fn write_field_updates(
        &self,
        dir: Arc<LockValidatingDirectoryWrapper<D>>,
        field_numbers: &FieldNumbers,
        max_del_gen: i64,
        info_stream: &impl InfoStream,
        info: &mut SegmentCommitInfo<D>,
    ) -> Result<bool> {
        let mut inner = self.inner.lock();
        let start_time_ns = std::time::Instant::now();

        let mut new_dv_files = HashMap::new();
        let mut field_infos_files: Option<HashSet<String>> = None;
        let mut field_infos = FieldInfos::default();

        let mut any = false;
        'outer: for updates in inner.pending_dv_updates.values() {
            for update in updates {
                if update.del_gen() <= max_del_gen && update.any() {
                    any = true;
                    break 'outer;
                }
            }
        }
        if !any {
            // no updates
            return Ok(false);
        }

        // Do this so we can delete any created files on
        // exception; this saves all codecs from having to do it:
        let tracking_dir = Arc::new(TrackingDirectoryWrapper::new(dir.clone()));

        let is_reader_none = inner.reader.is_none();
        let result = (|| -> Result<()> {
            let codec = get_default_code();

            if is_reader_none {
                let reader = SegmentReader::new(
                    info,
                    self.index_created_version_major,
                    &IOContext::read_once_io_context()?,
                )?;
                inner.pending_deletes.on_new_reader(&reader, info)?;
                inner.reader = Option::from(reader);
            }

            // clone FieldInfos so that we can update their dvGen separately from
            // the reader's infos and write them to a new fieldInfos_gen file.
            let mut max_field_number: i32 = -1;
            let mut by_name = HashMap::new();

            for fi in inner.reader.as_ref().unwrap().get_field_infos()?.iter() {
                // cannot use builder.add(fi) because it does not preserve
                // the local field number. Field numbers can be different from
                // the global ones if the segment was created externally (and added to
                // this index with IndexWriter#addIndexes(Directory)).
                by_name.insert(fi.name.to_string(), clone_field_info(fi, fi.number));
                max_field_number = max_field_number.max(fi.number);
            }

            // create new fields with the right DV type for updates whose field doesn't yet exist
            for updates in inner.pending_dv_updates.values() {
                if let Some(update) = updates.first() {
                    let field = update.field();
                    if by_name.contains_key(field) {
                        // the field already exists in this segment
                        let fi = by_name.get(field).expect("should not fail");
                        debug_assert_eq!(*fi.get_doc_values_type(), *update.get_type());
                    } else {
                        // the field is not present in this segment so we clone the global field
                        // (which is guaranteed to exist) and remaps its field number locally.
                        if let Some(fi) = field_numbers.construct_field_info(
                            field,
                            *update.get_type(),
                            max_field_number + 1,
                        )? {
                            max_field_number += 1;
                            by_name.insert(fi.name.to_string(), fi);
                        } else {
                            debug_assert!(false);
                        }
                    }
                }
            }

            field_infos = FieldInfos::new(by_name.into_values().map(Arc::new).collect())?;

            let dv_format = codec.doc_values_format();

            self.handle_dv_updates(
                &field_infos,
                tracking_dir.clone(),
                &dv_format,
                &mut inner,
                &mut new_dv_files,
                max_del_gen,
                info_stream,
                info,
            )?;

            let files = self.write_field_infos_gen(
                &field_infos,
                tracking_dir.clone(),
                &codec.field_infos_format(),
                info,
            )?;
            field_infos_files = Some(files);

            if is_reader_none {
                let _ = inner.reader.take();
            }
            Ok(())
        })();

        if let Err(e) = result {
            info.advance_next_write_field_infos_gen();
            info.advance_next_write_doc_values_gen();
            IOUtils::delete_files_ignoring_exceptions(
                dir.as_ref(),
                &tracking_dir.get_created_files().lock().created_filenames,
            );

            return Err(e);
        }
        // Prune the now-written DV updates:
        let mut bytes_freed: i64 = 0;
        inner.pending_dv_updates.retain(|_, updates| {
            let mut keep = Vec::with_capacity(updates.len());
            for u in updates.drain(..) {
                if u.del_gen() > max_del_gen {
                    keep.push(u);
                } else {
                    bytes_freed += u.ram_bytes_used().expect("should not fail");
                }
            }
            *updates = keep;
            !updates.is_empty()
        });

        let prev = self.ram_bytes_used.fetch_sub(bytes_freed, Ordering::SeqCst);
        let bytes_now = prev - bytes_freed;
        debug_assert!(bytes_now >= 0, "ram_bytes_used should not go negative");
        // writing field updates succeeded
        debug_assert!(field_infos_files.is_some());
        info.set_field_infos_files(field_infos_files.take().unwrap());
        // update the doc-values updates files. the files map each field to its set
        // of files, hence we copy from the existing map all fields w/ updates that
        // were not updated in this session, and add new mappings for fields that
        // were updated now.
        debug_assert!(!new_dv_files.is_empty());

        for (field_num, files_set) in info.get_doc_values_updates_files().iter() {
            new_dv_files
                .entry(*field_num)
                .or_insert_with(|| files_set.clone());
        }
        info.set_doc_values_updates_files(new_dv_files.clone());
        // if there is a reader open, reopen it to reflect the updates
        if !is_reader_none {
            self.swap_new_reader_with_latest_live_docs(&mut inner, info)?;
        }

        if info_stream.enabled("BD") {
            info_stream.message(
                "BD",
                &format!(
                    "done write field updates for seg={}; took {:.3}s; new files: {:?}",
                    info,
                    start_time_ns.elapsed().as_secs_f64(),
                    new_dv_files,
                ),
            );
        }
        Ok(true)
    }
    pub(crate) fn create_new_reader_with_latest_live_docs<'a>(
        &self,
        inner: &'a mut Inner<D>, // Same to Java's Thread.holdsLock(this)
        mut reader: &'a Option<SegmentReader<D>>,
        info: &SegmentCommitInfo<D>,
    ) -> Result<SegmentReader<D>> {
        if reader.is_none() {
            reader = &inner.reader;
        }

        let new_reader = SegmentReader::new_from_reader(
            info,
            reader.as_ref().unwrap(),
            inner.pending_deletes.get_live_docs(),
            inner.pending_deletes.get_hard_live_docs(),
            inner.pending_deletes.num_docs(info)?,
            true,
        )?;

        let res: Result<()> = (|| {
            inner.pending_deletes.on_new_reader(&new_reader, info)?;
            reader.as_ref().unwrap().dec_ref()?;
            Ok(())
        })();

        if res.is_err() {
            let _ = new_reader.dec_ref();
        }
        res?;
        Ok(new_reader)
    }

    fn swap_new_reader_with_latest_live_docs(
        &self,
        inner: &mut Inner<D>,
        info: &SegmentCommitInfo<D>,
    ) -> Result<()> {
        inner.reader = Some(self.create_new_reader_with_latest_live_docs(inner, &None, info)?);
        Ok(())
    }
    pub(crate) fn set_is_merging(&self) {
        let mut inner = self.inner.lock();
        if !inner.is_merging {
            inner.is_merging = true;
            debug_assert!(inner.merging_dv_updates.is_empty());
        }
    }

    pub(crate) fn is_merging(&self) -> bool {
        let inner = self.inner.lock();
        inner.is_merging
    }
    /// Drops all merging updates.
    /// Called from IndexWriter after this segment finished merging (whether successfully or not).
    pub fn drop_merging_updates(&self, inner: Option<&mut Inner<D>>) {
        let inner = match inner {
            Some(inner) => inner,
            None => &mut *self.inner.lock(),
        };
        inner.merging_dv_updates.clear();
        inner.is_merging = false;
    }
    pub fn take_merging_dv_updates(&self) -> HashMap<String, Vec<Arc<DocValuesFieldUpdatesEnum>>> {
        // We must atomically (in single sync'd block) clear isMerging when we return the DV updates
        // otherwise we can lose updates:
        let mut inner = self.inner.lock();
        inner.is_merging = false;
        inner.merging_dv_updates.clone()
    }
    pub fn is_fully_deleted(&self, info: &SegmentCommitInfo<D>) -> Result<bool> {
        let inner = self.inner.lock();
        inner
            .pending_deletes
            .is_fully_deleted(&IOSupplierImpl::new(self, info))
    }
    pub(crate) fn keep_fully_deleted_segment(
        &self,
        _merge_policy: &impl MergePolicy,
    ) -> Result<bool> {
        todo!()
    }
}
impl<D> Display for ReadersAndUpdates<D>
where
    D: Directory,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let inner = self.inner.lock();
        write!(
            f,
            "ReadersAndLiveDocs(seg={}, PendingDeletesEnum={})",
            self.info_id, inner.pending_deletes
        )
    }
}

enum CurrentSource {
    OnDisk,
    Update,
}
/// This class merges the current on-disk DV with an incoming update DV instance and merges the two instances giving the incoming update precedence in terms of values,
/// in other words the values of the update always wins over the on-disk version.
struct MergedDocValues<DI>
where
    DI: DocValuesIterator,
{
    // merged docID
    doc_id_out: i32,
    // docID from our original doc values
    doc_id_on_disk: i32,
    // docID from our updates
    update_doc_id: i32,

    on_disk_doc_values: Option<DI>,
    update_doc_values: Either2DocIdSetIterator<
        BinaryDocValuesDVFU<MergedIterator<DocValuesFieldIteratorEnum>>,
        NumericDocValuesDVFU<MergedIterator<DocValuesFieldIteratorEnum>>,
    >,
    current_values_supplier: Option<CurrentSource>,
}
impl<DI> MergedDocValues<DI>
where
    DI: DocValuesIterator,
{
    pub fn new(
        on_disk_doc_values: Option<DI>,
        update_doc_values: Either2DocIdSetIterator<
            BinaryDocValuesDVFU<MergedIterator<DocValuesFieldIteratorEnum>>,
            NumericDocValuesDVFU<MergedIterator<DocValuesFieldIteratorEnum>>,
        >,
    ) -> Self {
        Self {
            doc_id_out: -1,
            doc_id_on_disk: -1,
            update_doc_id: -1,
            on_disk_doc_values,
            update_doc_values,
            current_values_supplier: None,
        }
    }
}
impl<DI> DocIdSetIterator for MergedDocValues<DI>
where
    DI: DocValuesIterator,
{
    fn doc_id(&self) -> i32 {
        self.doc_id_out
    }

    fn next_doc(&mut self) -> Result<i32> {
        let mut has_value = false;

        while !has_value {
            if self.doc_id_on_disk == self.doc_id_out {
                match self.on_disk_doc_values.as_mut() {
                    Some(dv) => {
                        self.doc_id_on_disk = dv.next_doc()?;
                    },
                    None => {
                        self.doc_id_on_disk = NO_MORE_DOCS;
                    },
                }
            }

            if self.update_doc_id == self.doc_id_out {
                self.update_doc_id = self.update_doc_values.next_doc()?;
            }

            if self.doc_id_on_disk < self.update_doc_id {
                // no update to this doc - we use the on-disk values
                self.doc_id_out = self.doc_id_on_disk;
                self.current_values_supplier = Some(CurrentSource::OnDisk);
                has_value = true;
            } else {
                self.doc_id_out = self.update_doc_id;
                if self.doc_id_out != NO_MORE_DOCS {
                    self.current_values_supplier = Some(CurrentSource::Update);
                    has_value = match self.update_doc_values {
                        Either2DocIdSetIterator::A(ref mut dv) => dv.iterator.has_value(),
                        Either2DocIdSetIterator::B(ref mut dv) => dv.iterator.has_value(),
                    };
                } else {
                    has_value = true;
                }
            }
        }
        Ok(self.doc_id_out)
    }

    fn advance(&mut self, _target: i32) -> Result<i32> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn cost(&self) -> Result<i64> {
        self.on_disk_doc_values.as_ref().unwrap().cost()
    }
}

impl<DI> DocValuesIterator for MergedDocValues<DI>
where
    DI: DocValuesIterator,
{
    fn advance_exact(&mut self, _target: i32) -> Result<bool> {
        Err(LuceneError::unsupported_operation(""))
    }
}

struct BinaryDocValuesImpl<D>
where
    D: Directory,
{
    merged_doc_values: MergedDocValues<<SegmentReader<D> as LeafReader>::BinaryDocValues>,
}
impl<D> BinaryDocValuesImpl<D>
where
    D: Directory,
{
    fn new(
        merged_doc_values: MergedDocValues<<SegmentReader<D> as LeafReader>::BinaryDocValues>,
    ) -> Self {
        Self { merged_doc_values }
    }
}

impl<D> DocValuesIterator for BinaryDocValuesImpl<D>
where
    D: Directory,
{
    fn advance_exact(&mut self, target: i32) -> Result<bool> {
        self.merged_doc_values.advance_exact(target)
    }
}

impl<D> DocIdSetIterator for BinaryDocValuesImpl<D>
where
    D: Directory,
{
    fn doc_id(&self) -> i32 {
        self.merged_doc_values.doc_id()
    }

    fn next_doc(&mut self) -> Result<i32> {
        self.merged_doc_values.next_doc()
    }

    fn advance(&mut self, target: i32) -> Result<i32> {
        self.merged_doc_values.advance(target)
    }

    fn cost(&self) -> Result<i64> {
        self.merged_doc_values.cost()
    }
}

impl<D> BinaryDocValues for BinaryDocValuesImpl<D>
where
    D: Directory,
{
    fn binary_value(&mut self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
        match self.merged_doc_values.current_values_supplier {
            Some(CurrentSource::OnDisk) => {
                if let Some(dv) = &mut self.merged_doc_values.on_disk_doc_values {
                    dv.binary_value()
                } else {
                    Err(LuceneError::illegal_state(
                        "no on-disk doc values available",
                    ))
                }
            },
            Some(CurrentSource::Update) => match self.merged_doc_values.update_doc_values {
                Either2DocIdSetIterator::A(ref mut dv) => dv.binary_value(),
                Either2DocIdSetIterator::B(_) => Err(LuceneError::illegal_state(
                    "update doc values should be BinaryDocValuesDVFU",
                )),
            },
            None => Err(LuceneError::illegal_state("no current values supplier set")),
        }
    }
}
struct NumericDocValuesImpl<D>
where
    D: Directory,
{
    merged_doc_values: MergedDocValues<<SegmentReader<D> as LeafReader>::NumericDocValues>,
}

impl<D> NumericDocValuesImpl<D>
where
    D: Directory,
{
    fn new(
        merged_doc_values: MergedDocValues<<SegmentReader<D> as LeafReader>::NumericDocValues>,
    ) -> Self {
        Self { merged_doc_values }
    }
}

impl<D> DocValuesIterator for NumericDocValuesImpl<D>
where
    D: Directory,
{
    fn advance_exact(&mut self, target: i32) -> Result<bool> {
        self.merged_doc_values.advance_exact(target)
    }
}

impl<D> DocIdSetIterator for NumericDocValuesImpl<D>
where
    D: Directory,
{
    fn doc_id(&self) -> i32 {
        self.merged_doc_values.doc_id()
    }

    fn next_doc(&mut self) -> Result<i32> {
        self.merged_doc_values.next_doc()
    }

    fn advance(&mut self, target: i32) -> Result<i32> {
        self.merged_doc_values.advance(target)
    }

    fn cost(&self) -> Result<i64> {
        self.merged_doc_values.cost()
    }
}

impl<D> NumericDocValues for NumericDocValuesImpl<D>
where
    D: Directory,
{
    fn long_value(&mut self) -> Result<i64> {
        match self.merged_doc_values.current_values_supplier {
            Some(CurrentSource::OnDisk) => {
                if let Some(dv) = &mut self.merged_doc_values.on_disk_doc_values {
                    dv.long_value()
                } else {
                    Err(LuceneError::illegal_state(
                        "no on-disk doc values available",
                    ))
                }
            },
            Some(CurrentSource::Update) => match self.merged_doc_values.update_doc_values {
                Either2DocIdSetIterator::A(_) => Err(LuceneError::illegal_state(
                    "update doc values should be BinaryDocValuesDVFU",
                )),
                Either2DocIdSetIterator::B(ref mut dv) => dv.long_value(),
            },
            None => Err(LuceneError::illegal_state("no current values supplier set")),
        }
    }
}

struct DocValuesProducerBinary<'a, D>
where
    D: Directory,
{
    update_supplier: FunctionImpl,
    field: &'a str,
    reader: &'a mut SegmentReader<D>,
    field_info: Arc<FieldInfo>,
}
impl<'a, D> DocValuesProducerBinary<'a, D>
where
    D: Directory,
{
    pub fn new(
        update_supplier: FunctionImpl,
        field: &'a str,
        reader: &'a mut SegmentReader<D>,
        field_info: Arc<FieldInfo>,
    ) -> Self {
        Self {
            update_supplier,
            field,
            reader,
            field_info,
        }
    }
}

impl<D> Clone for DocValuesProducerBinary<'_, D>
where
    D: Directory,
{
    fn clone(&self) -> Self {
        unreachable!(
            "{} {}",
            std::any::type_name::<Self>(),
            CoreHelper::CLONE_WARRING
        )
    }
}

impl<'a, D> DocValuesProducer for DocValuesProducerBinary<'a, D>
where
    D: Directory,
{
    type NumericDocValues = DummyNumericDocValues;
    type BinaryDocValues = BinaryDocValuesImpl<D>;

    fn get_binary(&self, _field: &Arc<FieldInfo>) -> Result<Self::BinaryDocValues> {
        let iterator = match self.update_supplier.apply(&self.field_info)? {
            Some(it) => it,
            None => {
                return Err(LuceneError::illegal_argument(
                    "iterator should never None here",
                ));
            },
        };
        let merged_doc_values = MergedDocValues::new(
            self.reader.get_binary_doc_values(self.field)?,
            Either2DocIdSetIterator::A(BinaryDocValuesDVFU::new(iterator)),
        );
        Ok(BinaryDocValuesImpl::new(merged_doc_values))
    }

    type SortedDocValues = DummySortedDocValues;
    type SortedNumericDocValues = DummySortedNumericDocValues;
    type SortedSetDocValues = DummySortedSetDocValues;
    type DocValuesSkipper = DummyDocValuesSkipper;
}
struct DocValuesProducerNumeric<'a, D>
where
    D: Directory,
{
    update_supplier: FunctionImpl,
    field: &'a str,
    reader: &'a mut SegmentReader<D>,
    field_info: Arc<FieldInfo>,
}

impl<'a, D> DocValuesProducerNumeric<'a, D>
where
    D: Directory,
{
    pub fn new(
        update_supplier: FunctionImpl,
        field: &'a str,
        reader: &'a mut SegmentReader<D>,
        field_info: Arc<FieldInfo>,
    ) -> Self {
        Self {
            update_supplier,
            field,
            reader,
            field_info,
        }
    }
}

impl<D> Clone for DocValuesProducerNumeric<'_, D>
where
    D: Directory,
{
    fn clone(&self) -> Self {
        unreachable!(
            "{} {}",
            std::any::type_name::<Self>(),
            CoreHelper::CLONE_WARRING
        )
    }
}

impl<'a, D> DocValuesProducer for DocValuesProducerNumeric<'a, D>
where
    D: Directory,
{
    type NumericDocValues = NumericDocValuesImpl<D>;
    fn get_numeric(&self, _field: &Arc<FieldInfo>) -> Result<Self::NumericDocValues> {
        let iterator = match self.update_supplier.apply(&self.field_info)? {
            Some(it) => it,
            None => {
                return Err(LuceneError::illegal_argument(
                    "iterator should never None here",
                ));
            },
        };

        let merged_doc_values = MergedDocValues::new(
            self.reader.get_numeric_doc_values(self.field)?,
            Either2DocIdSetIterator::B(NumericDocValuesDVFU::new(iterator)),
        );
        // Merge sort of the original doc values with updated doc values:
        Ok(NumericDocValuesImpl::new(merged_doc_values))
    }
    type BinaryDocValues = DummyBinaryDocValues;
    type SortedDocValues = DummySortedDocValues;
    type SortedNumericDocValues = DummySortedNumericDocValues;
    type SortedSetDocValues = DummySortedSetDocValues;

    type DocValuesSkipper = DummyDocValuesSkipper;
}

struct FunctionImpl {
    field_info: Arc<FieldInfo>,
    updates_to_apply: Vec<Arc<DocValuesFieldUpdatesEnum>>,
}
impl FunctionImpl {
    fn new(
        field_info: Arc<FieldInfo>,
        updates_to_apply: Vec<Arc<DocValuesFieldUpdatesEnum>>,
    ) -> Self {
        Self {
            field_info,
            updates_to_apply,
        }
    }
}
impl Function<Arc<FieldInfo>, Option<MergedIterator<DocValuesFieldIteratorEnum>>> for FunctionImpl {
    fn apply(
        &self,
        info: &Arc<FieldInfo>,
    ) -> Result<Option<MergedIterator<DocValuesFieldIteratorEnum>>> {
        if !std::ptr::eq(info, &self.field_info) {
            return Err(LuceneError::illegal_argument(format!(
                "expected field info for field: {} but got: {}",
                self.field_info.name, info.name
            )));
        }

        let mut subs = vec![];
        for v in &self.updates_to_apply {
            subs.push(v.iterator()?)
        }
        merged_iterator(subs)
    }
}

fn clone_field_info(fi: &FieldInfo, field_number: i32) -> FieldInfo {
    FieldInfo::new(
        fi.name.to_string(),
        field_number,
        fi.has_term_vectors(),
        fi.omits_norms(),
        fi.has_payloads(),
        *fi.get_index_options(),
        *fi.get_doc_values_type(),
        *fi.doc_values_skip_index_type(),
        fi.get_doc_values_gen(),
        fi.attributes().lock().attributes.clone(),
        fi.get_point_dimension_count(),
        fi.get_point_index_dimension_count(),
        fi.get_point_num_bytes(),
        fi.get_vector_dimension(),
        *fi.get_vector_encoding(),
        *fi.get_vector_similarity_function(),
        fi.is_soft_deletes_field(),
        fi.is_parent_field(),
    )
}

pub(crate) struct IOSupplierImpl<'a, D>
where
    D: Directory,
{
    pub(crate) rdl: &'a ReadersAndUpdates<D>,
    pub(crate) info: &'a SegmentCommitInfo<D>,
}
impl<'a, D> IOSupplierImpl<'a, D>
where
    D: Directory,
{
    pub(crate) fn new(rdl: &'a ReadersAndUpdates<D>, info: &'a SegmentCommitInfo<D>) -> Self {
        Self { rdl, info }
    }
    fn set(&mut self, inner: Option<&mut Inner<D>>) -> Result<()> {
        self.rdl.get_latest_read(self.info, inner)
    }
}
