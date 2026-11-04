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
use crate::core::codecs::fields_producer::FieldsProducer;
use crate::core::codecs::knn_vectors_reader::KnnVectorsReader;
use crate::core::codecs::norms_producer::{NormsProducer, NormsProducerEnum2};
use crate::core::codecs::points_reader::PointsReader;
use crate::core::codecs::stored_fields_reader::StoredFieldsReader;
use crate::core::codecs::term_vectors_reader::TermVectorsReader;
use crate::core::index::codec_reader::{
  CRBits, CRDocValuesProducer, CRFieldsProducer, CRKnnVectorReader, CRNormsProducer,
  CRPointsReader, CRStoredFieldsReader, CRTermVectorsReader, CodecReader,
};

use crate::core::index::field_infos::{EMPTY, FieldInfos};
use crate::core::index::index_writer::is_congruent_sort;
use crate::core::index::multi_sorter::MultiSorter;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::search::task_executor::TaskExecutor;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::info_stream::{InfoStream, InfoStreamEnum};
use crate::core::util::long_values::LongValues;
use crate::core::util::packed::PackedInts;
use crate::core::util::packed::packed_long_values::PackedLongValues;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

type MergeNormsProducer<CR> =
  NormsProducerEnum2<CRNormsProducer<CR>, <CRNormsProducer<CR> as NormsProducer>::MergeInstance>;

/// Holds common state used during segment merging.
///
/// @lucene.experimental
pub struct MergeState<'a, D, CR, DocMaps = Rc<Vec<MergeStateDocMap<CR>>>>
where
  CR: CodecReader,
{
  /// [SegmentInfo] of the newly merged segment.
  pub(crate) segment_info: &'a mut SegmentInfo<D>,
  /// Maps document IDs from old segments to document IDs in the new segment
  pub(crate) doc_maps: DocMaps,
  /// [FieldInfos] of the newly merged segment.
  pub(crate) merge_field_infos: Arc<FieldInfos>,
  /// Stored field producers being merged
  pub(crate) stored_fields_readers: Vec<Option<CRStoredFieldsReader<CR>>>,
  /// Term vector producers being merged
  pub(crate) term_vectors_readers: Vec<Option<CRTermVectorsReader<CR>>>,
  /// Norms producers being merged
  pub(crate) norms_producers: Vec<Option<MergeNormsProducer<CR>>>,
  /// DocValues producers being merged
  pub(crate) doc_values_producers: Vec<Option<CRDocValuesProducer<CR>>>,
  /// Postings to merge
  pub(crate) fields_producers: Vec<Option<CRFieldsProducer<CR>>>,
  /// Point readers to merge
  pub(crate) points_readers: Vec<Option<CRPointsReader<CR>>>,
  /// Vector readers to merge
  pub(crate) knn_vectors_readers: Vec<Option<CRKnnVectorReader<CR>>>,
  /// FieldInfos being merged
  pub(crate) field_infos: Rc<Vec<Arc<FieldInfos>>>,
  /// Live docs for each reader
  pub(crate) live_docs: Vec<Option<CRBits<CR>>>,
  /// Indicates if the index needs to be sorted
  pub(crate) needs_index_sort: bool,
  /// Max docs per reader
  pub(crate) max_docs: smallvec::SmallVec<[i32; 4]>,
  /// [InfoStream] for debugging messages.
  pub(crate) info_stream: Arc<InfoStreamEnum>,
  /// Executor for intra-merge activity.
  pub(crate) intra_merge_task_executor: Arc<TaskExecutor>,
}

/// Access to the portion of a [`MergeState`] used by per-field codecs.
///
/// Per-field codecs restrict field infos and fields producers to one format's fields.
/// This trait exposes that restricted view while keeping the producer type
/// statically dispatched.
pub trait MergeStateAccess {
  type FieldsProducer: FieldsProducer;
  type DocValuesProducer: DocValuesProducer;
  type LiveDocs: Bits;
  type DocMap: DocMap;

  fn fields_producers(&self) -> &[Option<Self::FieldsProducer>];

  fn doc_values_producers(&self) -> &[Option<Self::DocValuesProducer>];

  fn doc_maps(&self) -> &Rc<Vec<Self::DocMap>>;

  fn doc_map(&self, index: usize) -> SharedDocMap<Self::DocMap> {
    SharedDocMap::new(self.doc_maps().clone(), index)
  }

  fn merge_field_infos(&self) -> &Arc<FieldInfos>;

  fn field_infos(&self) -> &[Arc<FieldInfos>];

  fn live_docs(&self) -> &[Option<Self::LiveDocs>];

  fn needs_index_sort(&self) -> bool;

  fn max_docs(&self) -> &[i32];

  fn intra_merge_task_executor(&self) -> &Arc<TaskExecutor>;

  fn get_meta(&self) -> MergeStateMeta<Self::DocMap>;
}

impl<D, CR> MergeStateAccess for MergeState<'_, D, CR>
where
  CR: CodecReader,
{
  type FieldsProducer = CRFieldsProducer<CR>;
  type DocValuesProducer = CRDocValuesProducer<CR>;
  type LiveDocs = CRBits<CR>;
  type DocMap = MergeStateDocMap<CR>;

  fn fields_producers(&self) -> &[Option<Self::FieldsProducer>] {
    &self.fields_producers
  }

  fn doc_values_producers(&self) -> &[Option<Self::DocValuesProducer>] {
    &self.doc_values_producers
  }

  fn doc_maps(&self) -> &Rc<Vec<Self::DocMap>> {
    &self.doc_maps
  }

  fn merge_field_infos(&self) -> &Arc<FieldInfos> {
    &self.merge_field_infos
  }

  fn field_infos(&self) -> &[Arc<FieldInfos>] {
    &self.field_infos
  }

  fn live_docs(&self) -> &[Option<Self::LiveDocs>] {
    &self.live_docs
  }

  fn needs_index_sort(&self) -> bool {
    self.needs_index_sort
  }

  fn max_docs(&self) -> &[i32] {
    &self.max_docs
  }

  fn intra_merge_task_executor(&self) -> &Arc<TaskExecutor> {
    &self.intra_merge_task_executor
  }

  fn get_meta(&self) -> MergeStateMeta<Self::DocMap> {
    MergeState::get_meta(self)
  }
}

impl<'a, D, CR> MergeState<'a, D, CR>
where
  CR: CodecReader,
{
  /// Sole constructor.
  pub(crate) fn new(
    readers: &'a [CR],
    segment_info: &'a mut SegmentInfo<D>,
    info_stream: Arc<InfoStreamEnum>,
    intra_merge_task_executor: Arc<TaskExecutor>,
  ) -> Result<Self>
  where
    CR: CodecReader,
  {
    verify_index_sort(readers, segment_info)?;

    let num_readers = readers.len();

    let mut max_docs = smallvec::SmallVec::with_capacity(num_readers);
    let mut fields_producers = Vec::with_capacity(num_readers);
    let mut norms_producers = Vec::with_capacity(num_readers);
    let mut stored_fields_readers = Vec::with_capacity(num_readers);
    let mut term_vectors_readers = Vec::with_capacity(num_readers);
    let mut points_readers = Vec::with_capacity(num_readers);
    let mut knn_vectors_readers = Vec::with_capacity(num_readers);
    let mut doc_values_producers = Vec::with_capacity(num_readers);
    let mut field_infos = Vec::with_capacity(num_readers);
    let mut live_docs = Vec::with_capacity(num_readers);

    let mut num_docs = 0;

    for reader in readers {
      max_docs.push(reader.max_doc()?);
      live_docs.push(reader.get_live_docs()?);
      field_infos.push(reader.get_field_infos()?);

      let norms = if let Some(norms_reader) = reader.get_norms_reader()? {
        if let Some(n) = norms_reader.get_merge_instance()? {
          Some(NormsProducerEnum2::B(n))
        } else {
          Some(NormsProducerEnum2::A(norms_reader))
        }
      } else {
        None
      };
      norms_producers.push(norms);

      let doc_values = if let Some(dv_reader) = reader.get_doc_values_reader()? {
        if let Some(dv) = dv_reader.get_merge_instance()? {
          Some(dv)
        } else {
          Some(dv_reader)
        }
      } else {
        None
      };
      doc_values_producers.push(doc_values);

      let stored_fields = if let Some(stored_reader) = reader.get_fields_reader()? {
        if let Some(stored_fields) = stored_reader.get_merge_instance()? {
          Some(stored_fields)
        } else {
          Some(stored_reader)
        }
      } else {
        None
      };

      stored_fields_readers.push(stored_fields);

      let term_vectors = if let Some(tv_reader) = reader.get_term_vectors_reader()? {
        if let Some(term_vectors) = tv_reader.get_merge_instance()? {
          Some(term_vectors)
        } else {
          Some(tv_reader)
        }
      } else {
        None
      };
      term_vectors_readers.push(term_vectors);

      let postings = if let Some(postings_reader) = reader.get_postings_reader()? {
        if let Some(p) = postings_reader.get_merge_instance()? {
          Some(p)
        } else {
          Some(postings_reader)
        }
      } else {
        None
      };
      fields_producers.push(postings);

      let points = if let Some(points_reader) = reader.get_points_reader()? {
        if let Some(p) = points_reader.get_merge_instance()? {
          Some(p)
        } else {
          Some(points_reader)
        }
      } else {
        None
      };
      points_readers.push(points);

      let knn_vectors = if let Some(knn_vectors_reader) = reader.get_vector_reader()? {
        if let Some(v) = knn_vectors_reader.get_merge_instance()? {
          Some(v)
        } else {
          Some(knn_vectors_reader)
        }
      } else {
        None
      };
      knn_vectors_readers.push(knn_vectors);

      num_docs += reader.num_docs()?;
    }

    segment_info.set_max_doc(num_docs)?;

    // The construction stage owns all resources in their usual declaration order.
    let doc_maps = ();
    // let doc_maps = build_doc_maps(readers, segment_info.index_sort());

    let mut merge_state = MergeState {
      segment_info,
      doc_maps,
      merge_field_infos: Arc::clone(&EMPTY),
      stored_fields_readers,
      term_vectors_readers,
      norms_producers,
      doc_values_producers,
      fields_producers,
      points_readers,
      knn_vectors_readers,
      field_infos: Rc::new(field_infos),
      live_docs,
      needs_index_sort: false,
      max_docs,
      info_stream,
      intra_merge_task_executor,
    };
    let doc_maps = merge_state.build_doc_maps(readers)?;
    Ok(merge_state.with_doc_maps(doc_maps))
  }
  pub(crate) fn doc_map(&self, index: usize) -> SharedDocMap<MergeStateDocMap<CR>> {
    SharedDocMap::new(self.doc_maps.clone(), index)
  }

  pub(crate) fn get_meta(&self) -> MergeStateMeta<MergeStateDocMap<CR>> {
    MergeStateMeta {
      fields_producers_len: self.fields_producers.len(),
      doc_maps: self.doc_maps.clone(),
      needs_index_sort: self.needs_index_sort,
      merge_field_infos: self.merge_field_infos.clone(),
      field_infos: self.field_infos.clone(),
    }
  }
}

impl<D, CR, DocMaps> MergeState<'_, D, CR, DocMaps>
where
  CR: CodecReader,
{
  fn build_doc_maps(&mut self, readers: &[CR]) -> Result<Rc<Vec<MergeStateDocMap<CR>>>>
  where
    CR: CodecReader,
  {
    let v = if let Some(ref sort) = self.segment_info.index_sort {
      // do a merge sort of the incoming leaves:
      let t0 = Instant::now();
      match MultiSorter::sort(sort, readers)? {
        None => {
          // already sorted, fall back to deletion-only mapping
          build_deletion_doc_maps(readers)?
        },
        Some(result) => {
          self.needs_index_sort = true;

          let t1 = Instant::now();
          if self.info_stream.is_enabled("SM") {
            let elapsed = t1.duration_since(t0).as_secs_f64() * 1000.0;
            self.info_stream.message(
              "SM",
              &format!("{:.2} msec to build merge sorted DocMaps", elapsed),
            )?;
          }
          result
        },
      }
    } else {
      // no index sort ... we only must map around deletions, and rebase to the merged segment's
      // docID space
      build_deletion_doc_maps(readers)?
    };
    Ok(Rc::new(v))
  }
}

impl<'a, D, CR> MergeState<'a, D, CR, ()>
where
  CR: CodecReader,
{
  fn with_doc_maps(self, doc_maps: Rc<Vec<MergeStateDocMap<CR>>>) -> MergeState<'a, D, CR> {
    MergeState {
      segment_info: self.segment_info,
      doc_maps,
      merge_field_infos: self.merge_field_infos,
      stored_fields_readers: self.stored_fields_readers,
      term_vectors_readers: self.term_vectors_readers,
      norms_producers: self.norms_producers,
      doc_values_producers: self.doc_values_producers,
      fields_producers: self.fields_producers,
      points_readers: self.points_readers,
      knn_vectors_readers: self.knn_vectors_readers,
      field_infos: self.field_infos,
      live_docs: self.live_docs,
      needs_index_sort: self.needs_index_sort,
      max_docs: self.max_docs,
      info_stream: self.info_stream,
      intra_merge_task_executor: self.intra_merge_task_executor,
    }
  }
}

pub type MergeStateDocMap<CR> = MergeStateDocMapImpl<CRBits<CR>>;

pub struct MergeStateDocMapImpl<B> {
  live_docs: Option<B>,
  hook: MergeStateDocMapHook,
}

enum MergeStateDocMapHook {
  Sorted {
    remapped: PackedLongValues,
  },
  Deletions {
    del_doc_map: Option<PackedLongValues>,
    doc_base: i32,
  },
}

impl<B> MergeStateDocMapImpl<B> {
  pub(crate) fn new_sorted(live_docs: Option<B>, remapped: PackedLongValues) -> Self {
    Self {
      live_docs,
      hook: MergeStateDocMapHook::Sorted { remapped },
    }
  }

  fn new_deletions(
    live_docs: Option<B>,
    del_doc_map: Option<PackedLongValues>,
    doc_base: i32,
  ) -> Self {
    Self {
      live_docs,
      hook: MergeStateDocMapHook::Deletions {
        del_doc_map,
        doc_base,
      },
    }
  }
}

impl<B> DocMap for MergeStateDocMapImpl<B>
where
  B: Bits,
{
  fn get(&self, doc_id: i32) -> Result<i32> {
    let doc_index = doc_id as usize;
    match &self.hook {
      MergeStateDocMapHook::Sorted { remapped } => {
        if match self.live_docs {
          None => true,
          Some(ref bits) => bits.get(doc_index)?,
        } {
          Ok(remapped.get(doc_index)? as i32)
        } else {
          Ok(-1)
        }
      },
      MergeStateDocMapHook::Deletions {
        del_doc_map,
        doc_base,
      } => match (&self.live_docs, del_doc_map) {
        (None, None) => Ok(doc_base + doc_id),
        (Some(bits), Some(map)) => {
          if bits.get(doc_index)? {
            Ok(doc_base + map.get(doc_index)? as i32)
          } else {
            Ok(-1)
          }
        },
        _ => Err(LuceneError::illegal_state("should not be here")),
      },
    }
  }
}

// Remap docIDs around deletions
fn build_deletion_doc_maps<CR>(readers: &[CR]) -> Result<Vec<MergeStateDocMap<CR>>>
where
  CR: CodecReader,
{
  let mut total_docs: i32 = 0;
  let num_readers = readers.len();
  let mut doc_maps = Vec::with_capacity(num_readers);

  for reader in readers.iter() {
    let live_docs = reader.get_live_docs()?;

    let del_doc_map = if let Some(ref bits) = live_docs {
      Some(remove_deletes(reader.max_doc()?, bits)?)
    } else {
      None
    };

    let doc_base = total_docs;

    doc_maps.push(MergeStateDocMapImpl::new_deletions(
      live_docs,
      del_doc_map,
      doc_base,
    ));

    total_docs += reader.num_docs()?;
  }

  Ok(doc_maps)
}
fn verify_index_sort<CR, D>(readers: &[CR], segment_info: &SegmentInfo<D>) -> Result<()>
where
  CR: CodecReader,
{
  let index_sort = match segment_info.get_index_sort() {
    Some(sort) => sort,
    None => return Ok(()),
  };

  for leaf in readers {
    let segment_sort = leaf.get_metadata()?.get_sort();
    if !segment_sort
      .as_ref()
      .map(|s| is_congruent_sort(&index_sort, s))
      .unwrap_or(false)
    {
      return Err(LuceneError::illegal_argument(format!(
        "index sort mismatch: merged segment has sort={} but to-be-merged segment has sort={}",
        index_sort,
        segment_sort
          .as_ref()
          .map(|s| s.to_string())
          .unwrap_or_else(|| "null".to_string())
      )));
    }
  }

  Ok(())
}

pub(crate) fn remove_deletes<B>(max_doc: i32, live_docs: &B) -> Result<PackedLongValues>
where
  B: Bits,
{
  let mut builder = PackedLongValues::monotonic_long_values_builder_default(PackedInts::COMPACT)?;

  let mut del = 0;
  for i in 0..max_doc {
    builder.add(i as i64 - del)?;
    if !live_docs.get(i as usize)? {
      del += 1;
    }
  }
  builder.build()
}

/// A map of doc IDs.
pub trait DocMap {
  /// Return the mapped docID or -1 if the given doc is not mapped.
  fn get(&self, doc_id: i32) -> Result<i32>;
}
/// An independently owned view of one immutable map in a shared directory.
///
/// Retaining a view keeps the whole directory alive without allocating a box per map.
pub struct SharedDocMap<DM> {
  directory: Rc<Vec<DM>>,
  index: usize,
}

impl<DM> SharedDocMap<DM> {
  /// Retain the directory and select one immutable map without copying its data.
  ///
  /// # Panics
  ///
  /// Panics if `index` is outside `directory`.
  pub fn new(directory: Rc<Vec<DM>>, index: usize) -> Self {
    let _ = &directory[index];
    Self { directory, index }
  }
}

impl<DM> Clone for SharedDocMap<DM> {
  fn clone(&self) -> Self {
    Self {
      directory: self.directory.clone(),
      index: self.index,
    }
  }
}

impl<DM> std::ops::Deref for SharedDocMap<DM> {
  type Target = DM;
  fn deref(&self) -> &DM {
    &self.directory[self.index]
  }
}

impl<DM> AsRef<DM> for SharedDocMap<DM> {
  fn as_ref(&self) -> &DM {
    self
  }
}

impl<DM: DocMap> DocMap for SharedDocMap<DM> {
  fn get(&self, doc_id: i32) -> Result<i32> {
    self.as_ref().get(doc_id)
  }
}

impl<T> DocMap for Rc<T>
where
  T: DocMap,
{
  fn get(&self, doc_id: i32) -> Result<i32> {
    (**self).get(doc_id)
  }
}
macro_rules! either_doc_map {
    ($vis:vis $name:ident { $( $Variant:ident : $T:ident ),+ $(,)? }) => {
        $vis enum $name<$( $T ),+> {
            $( $Variant($T), )+
        }

        impl<$( $T ),+> DocMap for $name<$( $T ),+>
        where
            $( $T: DocMap ),+
        {
            #[inline]
            fn get(&self, doc_id: i32) -> Result<i32> {
                match self {
                    $( Self::$Variant(inner) => inner.get(doc_id), )+
                }
            }
        }
    };
}
either_doc_map!(pub DocMapEnum2 { A: A, B: B});

// for shared
pub struct MergeStateMeta<DM> {
  pub(crate) fields_producers_len: usize,
  pub(crate) doc_maps: Rc<Vec<DM>>,
  pub needs_index_sort: bool,
  pub merge_field_infos: Arc<FieldInfos>,
  pub field_infos: Rc<Vec<Arc<FieldInfos>>>,
}
impl<DM> Clone for MergeStateMeta<DM> {
  fn clone(&self) -> Self {
    Self {
      fields_producers_len: self.fields_producers_len,
      doc_maps: self.doc_maps.clone(),
      needs_index_sort: self.needs_index_sort,
      merge_field_infos: self.merge_field_infos.clone(),
      field_infos: self.field_infos.clone(),
    }
  }
}
