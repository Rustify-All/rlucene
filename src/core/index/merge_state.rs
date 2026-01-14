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
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::codecs::points_reader::PointsReader;
use crate::core::codecs::stored_fields_reader::{DefaultStoredFieldsReader, StoredFieldsReader};
use crate::core::codecs::term_vectors_reader::{DefaultTermVectorsReader, TermVectorsReader};
use crate::core::index::codec_reader::{
    CRBits, CRDocValuesProducer, CRFieldsProducer, CRNormsProducer, CRPointsReader, CodecReader,
};

use crate::core::index::field_infos::FieldInfos;
use crate::core::index::index_writer::is_congruent_sort;
use crate::core::index::multi_sorter::{MultiSorter, MultiSorterDocMap};
use crate::core::index::segment_info::SegmentInfo;
use crate::core::store::directory::Directory;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::info_stream::{InfoStream, InfoStreamEnum};
use crate::core::util::long_values::LongValues;
use crate::core::util::packed::PackedInts;
use crate::core::util::packed::packed_long_values::PackedLongValues;
#[cfg(test)]
use crate::test::util::bkd::test_bkd::DocMapMock;
use std::rc::Rc;
use std::sync::Arc;
use std::time::SystemTime;

pub struct MergeState<'a, D, CR>
where
    D: Directory,
    CR: CodecReader,
{
    pub(crate) segment_info: &'a mut SegmentInfo<D>,
    pub(crate) doc_maps: Vec<Rc<MergeStateDocMap<CR>>>,
    pub(crate) merge_field_infos: Arc<FieldInfos>,
    pub(crate) stored_fields_readers: Vec<Option<DefaultStoredFieldsReader<D::IndexInput>>>,
    pub(crate) term_vectors_readers: Vec<Option<DefaultTermVectorsReader<D::IndexInput>>>,
    pub(crate) norms_producers: Vec<Option<CRNormsProducer<CR>>>,
    pub(crate) doc_values_producers: Vec<Option<CRDocValuesProducer<CR>>>,
    pub(crate) fields_producers: Vec<Option<CRFieldsProducer<CR>>>,
    pub(crate) points_readers: Vec<Option<CRPointsReader<CR>>>,
    pub(crate) field_infos: Vec<Arc<FieldInfos>>,
    pub(crate) live_docs: Vec<Option<CRBits<CR>>>,
    pub(crate) needs_index_sort: bool,
    pub(crate) max_docs: Vec<i32>,
    pub(crate) info_stream: Arc<InfoStreamEnum>,
}
impl<'a, D, CR> MergeState<'a, D, CR>
where
    D: Directory,
    CR: CodecReader,
{
    pub(crate) fn new(
        readers: &'a [CR],
        segment_info: &'a mut SegmentInfo<D>,
        info_stream: Arc<InfoStreamEnum>,
    ) -> Result<Self>
    where
        CR: CodecReader<
                StoredFieldsReader = DefaultStoredFieldsReader<D::IndexInput>,
                TermVectorsReader = DefaultTermVectorsReader<D::IndexInput>,
            >,
    {
        verify_index_sort(readers, segment_info)?;

        let num_readers = readers.len();

        let mut max_docs = Vec::with_capacity(num_readers);
        let mut fields_producers = Vec::with_capacity(num_readers);
        let mut norms_producers = Vec::with_capacity(num_readers);
        let mut stored_fields_readers = Vec::with_capacity(num_readers);
        let mut term_vectors_readers = Vec::with_capacity(num_readers);
        let mut points_readers = Vec::with_capacity(num_readers);
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
                    Some(n)
                } else {
                    Some(norms_reader)
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
                Some(
                    stored_reader
                        .get_merge_instance()?
                        .ok_or_else(|| LuceneError::illegal_argument("stored_reader is None"))?,
                )
            } else {
                None
            };

            stored_fields_readers.push(stored_fields);

            let term_vectors =
                if let Some(tv_reader) = reader.get_term_vectors_reader()? {
                    Some(tv_reader.get_merge_instance()?.ok_or_else(|| {
                        LuceneError::illegal_argument("term_verctors_reader is None")
                    })?)
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

            num_docs += reader.num_docs()?;

            // TODO IMPORTANT KNN未实现
        }

        segment_info.set_max_doc(num_docs)?;

        let doc_maps = Vec::new();
        // let doc_maps = build_doc_maps(readers, segment_info.index_sort());

        let mut merge_state = Self {
            segment_info,
            doc_maps,
            merge_field_infos: Arc::new(FieldInfos::default()),
            stored_fields_readers,
            term_vectors_readers,
            norms_producers,
            doc_values_producers,
            fields_producers,
            points_readers,
            field_infos,
            live_docs,
            needs_index_sort: false,
            max_docs,
            info_stream,
        };
        merge_state.build_doc_maps(readers)?;
        Ok(merge_state)
    }
    pub(crate) fn get_meta(&self) -> MergeStateMeta<CR> {
        MergeStateMeta {
            fields_producers_len: self.fields_producers.len(),
            doc_maps: self.doc_maps.clone(),
            needs_index_sort: self.needs_index_sort,
            merge_field_infos: self.merge_field_infos.clone(),
            field_infos: self.field_infos.clone(),
        }
    }
    fn build_doc_maps(&mut self, readers: &[CR]) -> Result<()>
    where
        CR: CodecReader,
    {
        let v = if let Some(ref sort) = self.segment_info.index_sort {
            // do a merge sort of the incoming leaves:
            let t0 = SystemTime::now();
            match MultiSorter::sort(sort, readers)? {
                None => {
                    // already sorted, fall back to deletion-only mapping
                    build_deletion_doc_maps(readers)?
                },
                Some(result) => {
                    self.needs_index_sort = true;

                    let t1 = SystemTime::now();
                    if self.info_stream.enabled("SM") {
                        let elapsed = t1.duration_since(t0).unwrap().as_secs_f64() * 1000.0;
                        self.info_stream.message(
                            "SM",
                            &format!("{:.2} msec to build merge sorted DocMaps", elapsed),
                        );
                    }
                    result
                },
            }
        } else {
            // no index sort ... we only must map around deletions, and rebase to the merged segment's
            // docID space
            build_deletion_doc_maps(readers)?
        };
        self.doc_maps = v
            .into_iter()
            .map(Rc::new)
            .collect::<Vec<Rc<MergeStateDocMap<CR>>>>();
        Ok(())
    }
}

pub type MergeStateDocMap<CR> = DocMapEnum2<MultiSorterDocMap<CR>, DocMapImpl2<CRBits<CR>>>;

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

        doc_maps.push(DocMapEnum2::B(DocMapImpl2::new(
            live_docs,
            del_doc_map,
            doc_base,
        )));

        total_docs += reader.num_docs()?;
    }

    Ok(doc_maps)
}
fn verify_index_sort<CR, D>(readers: &[CR], segment_info: &SegmentInfo<D>) -> Result<()>
where
    CR: CodecReader,
    D: Directory,
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

pub struct DocMapImpl2<B>
where
    B: Bits,
{
    live_docs: Option<B>,
    del_doc_map: Option<PackedLongValues>,
    doc_base: i32,
}
impl<B> DocMapImpl2<B>
where
    B: Bits,
{
    fn new(live_docs: Option<B>, del_doc_map: Option<PackedLongValues>, doc_base: i32) -> Self {
        Self {
            live_docs,
            del_doc_map,
            doc_base,
        }
    }
}
impl<B> DocMap for DocMapImpl2<B>
where
    B: Bits,
{
    fn get(&self, doc_id: i32) -> Result<i32> {
        match (&self.live_docs, &self.del_doc_map) {
            (None, None) => Ok(self.doc_base + doc_id),
            (Some(bits), Some(map)) => {
                if bits.get(doc_id as usize)? {
                    Ok(self.doc_base + map.get(doc_id as usize)? as i32)
                } else {
                    Ok(-1)
                }
            },
            _ => Err(LuceneError::illegal_state("should not be here")),
        }
    }
}

/// A map of doc IDs.
pub trait DocMap {
    /// Return the mapped docID or -1 if the given doc is not mapped.
    fn get(&self, doc_id: i32) -> Result<i32>;
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

pub enum DocMapEnum<CR>
where
    CR: CodecReader,
{
    #[cfg(test)]
    Mock(DocMapMock),
    Merge(MergeStateDocMap<CR>),
}
impl<CR> DocMap for DocMapEnum<CR>
where
    CR: CodecReader,
{
    fn get(&self, doc_id: i32) -> Result<i32> {
        match self {
            #[cfg(test)]
            DocMapEnum::Mock(inner) => inner.get(doc_id),
            DocMapEnum::Merge(inner) => inner.get(doc_id),
        }
    }
}

// for shared
pub struct MergeStateMeta<CR>
where
    CR: CodecReader,
{
    pub(crate) fields_producers_len: usize,
    pub(crate) doc_maps: Vec<Rc<MergeStateDocMap<CR>>>,
    pub needs_index_sort: bool,
    pub merge_field_infos: Arc<FieldInfos>,
    pub field_infos: Vec<Arc<FieldInfos>>,
}
impl<CR> Clone for MergeStateMeta<CR>
where
    CR: CodecReader,
{
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
