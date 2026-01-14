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
use crate::core::codecs::{Codec, CompoundFormat, get_default_code};
use crate::core::index::doc_values_field_updates::{DocValuesFieldIteratorEnum, MergedIterator};
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::pending_deletes::{DocBits, PendingDeletes, PendingDeletesBase};
use crate::core::index::readers_and_updates::IOSupplierImpl;
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_reader::SegmentReader;
use crate::core::store::IOContext;
use crate::core::store::directory::Directory;
use crate::core::util::error::lucene_error::Result;
use num_bigint::BigInt;

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
        let base = PendingDeletes::new(info)?;
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
        debug_assert!(info.info.max_doc()? >= self.base.get_del_count(info));
        Ok(true)
    }

    fn ensure_initialized<D>(&self, _reader_io_supplier: &IOSupplierImpl<D>)
    where
        D: Directory,
    {
        todo!()
    }

    fn read_field_infos<D>(&self, info: &SegmentCommitInfo<D>) -> Result<FieldInfos>
    where
        D: Directory,
    {
        let seg_info = &info.info;
        let codec = get_default_code();
        if !info.has_field_updates() {
            // updates always outside of CFS
            if seg_info.get_use_compound_file() {
                let cfs = codec
                    .compound_format()
                    .get_compound_reader(seg_info.dir.as_ref(), seg_info)?;
                codec.field_infos_format().read(
                    &cfs,
                    seg_info,
                    "",
                    &IOContext::read_once_io_context()?,
                )
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
        _reader: &SegmentReader<D>,
        _info: &SegmentCommitInfo<D>,
    ) -> Result<()>
    where
        D: Directory,
    {
        todo!()
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

    fn is_fully_deleted<D>(&self, reader_io_supplier: &IOSupplierImpl<D>) -> Result<bool>
    where
        D: Directory,
    {
        // initialize to ensure we have accurate counts - only needed in the soft-delete case
        self.ensure_initialized(reader_io_supplier);
        todo!()
    }

    fn max_doc(&self) -> i32 {
        self.base.max_doc()
    }

    fn on_doc_values_update(
        &self,
        _info: &FieldInfo,
        _iterator: Option<MergedIterator<DocValuesFieldIteratorEnum>>,
    ) {
        todo!()
    }

    fn must_init_on_delete(&self) -> bool {
        !self.base.live_docs_initialized
    }
}

use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
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
