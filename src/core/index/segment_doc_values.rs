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
use crate::core::codecs::CodecDocValuesProducer;
use crate::core::codecs::compound_directory::CompoundDirectoryEnum;
use crate::core::codecs::doc_values_format::DocValuesFormat;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::store::IO_CONTEXT_DEFAULT;

use crate::core::index::index_file_names::base36;
use crate::core::store::directory::Directory;
use crate::core::store::index_input::IndexInput;
use crate::core::util::IOUtils;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::ref_count::RefCount;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;

/// Manages the [`DocValuesProducer`](crate::core::codecs::doc_values_producer::DocValuesProducer) held by [`SegmentReader`](crate::core::index::segment_reader::SegmentReader) and keeps track of their reference counting.
pub(crate) struct SegmentDocValues<I>
where
  I: IndexInput,
{
  inner: Mutex<Inner<I>>,
}
pub(crate) struct Inner<I>
where
  I: IndexInput,
{
  gen_dv_producers: HashMap<i64, RefCount<Arc<CodecDocValuesProducer<I>>>>,
}

impl<I> SegmentDocValues<I>
where
  I: IndexInput,
{
  pub(crate) fn new() -> Self {
    SegmentDocValues {
      inner: Mutex::new(Inner {
        gen_dv_producers: HashMap::new(),
      }),
    }
  }
  pub(crate) fn new_doc_values_producer<D, D1>(
    &self,
    si: &SegmentCommitInfo<D>,
    dir: Option<&D1>,
    gen_: i64,
    infos: Arc<FieldInfos>,
  ) -> Result<RefCount<Arc<CodecDocValuesProducer<I>>>>
  where
    D: Directory<IndexInput = I>,
    D1: Directory<IndexInput = I>,
  {
    let mut dv_dir = match dir {
      Some(d) => CompoundDirectoryEnum::A(d),
      None => CompoundDirectoryEnum::B(si.info.dir.as_ref()),
    };
    let mut suffix_buffer = [0; 14];
    let segment_suffix = if gen_ == -1 {
      ""
    } else {
      // gen'd files are written outside CFS, so use SegInfo directory
      dv_dir = CompoundDirectoryEnum::B(si.info.dir.as_ref());
      base36(gen_, &mut suffix_buffer)
    };

    let io_context = IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?;
    // set SegmentReadState to list only the fields that are relevant to that gen
    let srs = SegmentReadState::with_suffix(&dv_dir, infos, io_context, segment_suffix);

    let dv_format = si.info.get_codec()?.doc_values_format();

    Ok(RefCount::new(Arc::new(
      dv_format.fields_producer(&srs, &si.info)?,
    )))
  }
  /// Returns the [`DocValuesProducer`](crate::core::codecs::doc_values_producer::DocValuesProducer) for the given generation.
  pub(crate) fn get_doc_values_producer<D, D1, F>(
    &self,
    gen_: i64,
    si: &SegmentCommitInfo<D>,
    dir: Option<&D1>,
    infos: F,
  ) -> Result<Arc<CodecDocValuesProducer<I>>>
  where
    D: Directory<IndexInput = I>,
    D1: Directory<IndexInput = I>,
    F: Into<Arc<FieldInfos>>,
  {
    let mut inner = self.inner.lock();

    match inner.gen_dv_producers.entry(gen_) {
      Entry::Occupied(mut entry) => {
        entry.get_mut().inc_ref();
        Ok(entry.get().get().clone())
      },
      Entry::Vacant(entry) => {
        let dvp = self.new_doc_values_producer(si, dir, gen_, infos.into())?;
        let v = dvp.get().clone();
        entry.insert(dvp);
        Ok(v)
      },
    }
  }
  ///  Decrement the reference count of the given [`DocValuesProducer`](crate::core::codecs::doc_values_producer::DocValuesProducer) generations.
  pub(crate) fn dec_ref(&self, gens: &[i64]) -> Result<()> {
    let mut inner = self.inner.lock();

    IOUtils::apply_to_all(gens.iter().map(Some), |&gen_| {
      if let Some(dvp) = inner.gen_dv_producers.get_mut(&gen_) {
        if dvp.dec_ref(|| dvp.get().close())? {
          inner.gen_dv_producers.remove(&gen_);
        }
      } else {
        debug_assert!(false, "gen={} not found in gen_dv_producers", gen_);
      }
      Ok(())
    })
  }
}
