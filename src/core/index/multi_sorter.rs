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
use crate::core::index::codec_reader::{CRBits, CodecReader};
use crate::core::index::index_sorter::{ComparableProvider, ComparableProviderEnum2, IndexSorter};
use crate::core::index::merge_state::{DocMap, DocMapEnum2, MergeStateDocMap};
use crate::core::search::sort::Sort;
use crate::core::search::sort_field::SortFiledBase;
use crate::core::util::bit_set::{BitSet, of};
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::long_values::LongValues;
use crate::core::util::packed::PackedInts;
use crate::core::util::packed::packed_long_values::PackedLongValues;
use crate::core::util::priority_queue::{Compare, PriorityQueue};
use crate::core::util::{LUCENE_10_0_0, ToInt};
/// Does a merge sort of the leaves of the incoming readers, returning [`DocMap`]s
/// to map each leaf's documents into the merged segment.
///
/// The documents for each incoming leaf reader must already be sorted by the same
/// sort. Returns `None` if the merge sort is not needed (segments are already in
/// index sort order).
pub struct MultiSorter;
impl MultiSorter {
    pub(crate) fn sort<CR>(sort: &Sort, readers: &[CR]) -> Result<Option<Vec<MergeStateDocMap<CR>>>>
    where
        CR: CodecReader,
    {
        let fields = sort.get_sort();
        let mut comparables = Vec::with_capacity(fields.len());
        let mut reverse_muls = Vec::with_capacity(fields.len());

        for field in fields.iter() {
            let sorter = field.get_index_sorter()?.ok_or_else(|| {
                LuceneError::illegal_argument(format!(
                    "Cannot use sort field {} for index sorting",
                    field
                ))
            })?;

            let mut providers = sorter.get_comparable_providers(readers)?;
            let mut new_providers = Vec::with_capacity(providers.len());
            #[allow(clippy::needless_range_loop)]
            for j in 0..readers.len() {
                let reader = &readers[j];
                let field_infos = reader.get_field_infos()?;
                let meta = reader.get_metadata()?;

                let inner = providers.remove(0);
                if meta.get_has_blocks() {
                    if let Some(parent_field) = field_infos.get_parent_field() {
                        let mut parent_docs = CodecReader::get_numeric_doc_values(reader, parent_field)?
                            .ok_or_else(|| {
                                LuceneError::illegal_state(format!(
                                    "parent field {} must be present if index sorting is used with blocks",
                                    parent_field
                                ))
                            })?;

                        let parents = of(&mut parent_docs, reader.max_doc()? as usize)?;
                        let cp = ComparableProviderEnum2::A1(ComparableProviderImpl::new(
                            parents, inner,
                        ));
                        new_providers.push(cp);
                    } else if meta.get_created_version_major() >= LUCENE_10_0_0.major {
                        return Err(LuceneError::corrupt_index(format!(
                            "parent field is not set but the index has blocks and uses index sorting. indexCreatedVersionMajor: {}",
                            meta.get_created_version_major()
                        )));
                    }
                } else {
                    new_providers.push(ComparableProviderEnum2::B1(inner));
                }
            }
            reverse_muls.push(if field.get_reverse() { -1 } else { 1 });
            comparables.push(new_providers);
        }
        let leaf_count = readers.len();
        let comparables_len = comparables.len();
        let cmp = LeafAndDocIDCmp::new(comparables_len, &reverse_muls);
        let mut queue = PriorityQueue::new(leaf_count, cmp)?;
        let mut builders = Vec::with_capacity(leaf_count);

        for i in 0..leaf_count {
            let reader = &readers[i];
            let mut leaf = LeafAndDocId::new(
                i,
                reader.get_live_docs()?,
                reader.max_doc()?,
                comparables.len(),
            );
            for (j, comps_per_field) in comparables.iter_mut().enumerate() {
                leaf.values_as_comparable_longs[j] =
                    comps_per_field[i].get_as_comparable_long(leaf.doc_id)?;
            }

            queue.add(leaf)?;
            builders.push(PackedLongValues::monotonic_long_values_builder_default(
                PackedInts::COMPACT,
            )?);
        }
        // merge sort
        let mut mapped_doc_id = 0;
        let mut last_reader_index = 0;
        let mut is_sorted = true;

        while queue.size() > 0 {
            let top = queue.top_mut().unwrap();

            if last_reader_index > top.reader_index {
                // merge sort is needed
                is_sorted = false;
            }
            last_reader_index = top.reader_index;

            builders[top.reader_index].add(mapped_doc_id)?;

            if match top.live_docs {
                None => true,
                Some(ref bits) => bits.get(top.doc_id as usize)?,
            } {
                mapped_doc_id += 1;
            }
            top.doc_id += 1;

            if top.doc_id < top.max_doc {
                for (j, comps_per_field) in comparables.iter_mut().enumerate() {
                    top.values_as_comparable_longs[j] =
                        comps_per_field[top.reader_index].get_as_comparable_long(top.doc_id)?;
                }
                queue.update_top()?;
            } else {
                queue.pop()?;
            }
        }

        if is_sorted {
            return Ok(None);
        }
        let mut doc_maps = Vec::with_capacity(leaf_count);
        for i in 0..leaf_count {
            let remapped = builders[i].build()?;
            let live_docs = readers[i].get_live_docs()?;

            let doc_map = DocMapImpl1::new(live_docs, remapped);
            doc_maps.push(DocMapEnum2::A(doc_map));
        }
        Ok(Some(doc_maps))
    }
}
pub type MultiSorterDocMap<CR> = DocMapImpl1<CRBits<CR>>;

pub struct DocMapImpl1<B>
where
    B: Bits,
{
    live_docs: Option<B>,
    remapped: PackedLongValues,
}
impl<B> DocMapImpl1<B>
where
    B: Bits,
{
    fn new(live_docs: Option<B>, remapped: PackedLongValues) -> Self {
        DocMapImpl1 {
            live_docs,
            remapped,
        }
    }
}
impl<B> DocMap for DocMapImpl1<B>
where
    B: Bits,
{
    fn get(&self, doc_id: i32) -> Result<i32> {
        if match self.live_docs {
            None => true,
            Some(ref bits) => bits.get(doc_id as usize)?,
        } {
            Ok(self.remapped.get(doc_id as usize)? as i32)
        } else {
            Ok(-1)
        }
    }
}
pub(crate) struct LeafAndDocId<B>
where
    B: Bits,
{
    reader_index: usize,
    live_docs: Option<B>,
    max_doc: i32,
    values_as_comparable_longs: Vec<i64>,
    doc_id: i32,
}
impl<B> LeafAndDocId<B>
where
    B: Bits,
{
    fn new(
        reader_index: usize,
        live_docs: Option<B>,
        max_doc: i32,
        num_comparables: usize,
    ) -> Self {
        Self {
            reader_index,
            live_docs,
            max_doc,
            values_as_comparable_longs: vec![0i64; num_comparables],
            doc_id: 0,
        }
    }
}

struct LeafAndDocIDCmp<'a> {
    comparables_len: usize,
    reverse_muls: &'a [i32],
}
impl<'a> LeafAndDocIDCmp<'a> {
    fn new(comparables_len: usize, reverse_muls: &'a [i32]) -> Self {
        LeafAndDocIDCmp {
            comparables_len,
            reverse_muls,
        }
    }
}
impl<B> Compare<LeafAndDocId<B>> for LeafAndDocIDCmp<'_>
where
    B: Bits,
{
    fn less_than(&self, a: &LeafAndDocId<B>, b: &LeafAndDocId<B>) -> Result<bool> {
        for i in 0..self.comparables_len {
            let cmp = a.values_as_comparable_longs[i]
                .cmp(&b.values_as_comparable_longs[i])
                .to_int();

            if cmp != 0 {
                return Ok(self.reverse_muls[i] * cmp < 0);
            }
        }
        // tie-break by docID natural order:
        if a.reader_index != b.reader_index {
            return Ok(a.reader_index < b.reader_index);
        }

        Ok(a.doc_id < b.doc_id)
    }
}

pub struct ComparableProviderImpl<B, CP>
where
    B: BitSet,
    CP: ComparableProvider,
{
    parents: B,
    provider: CP,
}
impl<B, CP> ComparableProviderImpl<B, CP>
where
    B: BitSet,
    CP: ComparableProvider,
{
    fn new(parents: B, provider: CP) -> Self {
        ComparableProviderImpl { parents, provider }
    }
}
impl<B, CP> ComparableProvider for ComparableProviderImpl<B, CP>
where
    B: BitSet,
    CP: ComparableProvider,
{
    fn get_as_comparable_long(&mut self, doc_id: i32) -> Result<i64> {
        let v = self.parents.next_set_bit(doc_id as usize);
        self.provider.get_as_comparable_long(v as i32)
    }
}
