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
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::index_sorter::ComparableProvidersVisitor;
use crate::core::index::index_sorter::{ComparableProvider, ComparableProviderEnum2, IndexSorter};
use crate::core::index::merge_state::{MergeStateDocMap, MergeStateDocMapImpl};
use crate::core::search::sort::Sort;
use crate::core::search::sort_field::SortFiledBase;
use crate::core::util::bit_set::{BitSet, of};
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::packed::PackedInts;
use crate::core::util::packed::packed_long_values::PackedLongValues;
use crate::core::util::priority_queue::{Compare, PriorityQueue};
use crate::core::util::{LUCENE_10_0_0, ToInt};
/// Does a merge sort of the leaves of the incoming readers, returning
/// [`DocMap`](crate::core::index::sorter::DocMap) implementations
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
    if fields.len() == 1 {
      let field = &fields[0];
      let sorter = field.get_index_sorter()?.ok_or_else(|| {
        LuceneError::illegal_argument(format!("Cannot use sort field {} for index sorting", field))
      })?;
      return sorter.visit_comparable_providers(
        readers,
        SingleFieldMergeSortVisitor {
          readers,
          reverse_mul: if field.get_reverse() { -1 } else { 1 },
        },
      );
    }
    let mut comparables = smallvec::SmallVec::<[_; 2]>::with_capacity(fields.len());
    let mut reverse_muls = smallvec::SmallVec::<[i8; 2]>::with_capacity(fields.len());

    for field in fields.iter() {
      let sorter = field.get_index_sorter()?.ok_or_else(|| {
        LuceneError::illegal_argument(format!("Cannot use sort field {} for index sorting", field))
      })?;

      let providers = sorter.get_comparable_providers(readers)?;
      let new_providers = Self::wrap_providers(readers, providers)?;
      reverse_muls.push(if field.get_reverse() { -1 } else { 1 });
      comparables.push(new_providers);
    }
    let comparables = comparables.as_mut_slice();
    Self::sort_with_providers::<CR, _, false>(readers, &reverse_muls, |reader, doc, values| {
      for (j, comps_per_field) in comparables.iter_mut().enumerate() {
        values[j] = comps_per_field[reader].get_as_comparable_long(doc)?;
      }
      Ok(())
    })
  }

  fn sort_with_providers<CR, F, const SINGLE: bool>(
    readers: &[CR],
    reverse_muls: &[i8],
    mut get_values: F,
  ) -> Result<Option<Vec<MergeStateDocMap<CR>>>>
  where
    CR: CodecReader,
    F: FnMut(usize, i32, &mut [i64]) -> Result<()>,
  {
    let leaf_count = readers.len();
    let comparables_len = reverse_muls.len();
    let cmp = LeafAndDocIDCmp::<SINGLE>::new(comparables_len, reverse_muls);
    let mut queue = PriorityQueue::new(leaf_count, cmp)?;
    let mut builders = Vec::with_capacity(leaf_count);

    #[allow(clippy::needless_range_loop)]
    for i in 0..leaf_count {
      let reader = &readers[i];
      let mut leaf = LeafAndDocId::new(
        i,
        reader.get_live_docs()?,
        reader.max_doc()?,
        comparables_len,
      );
      get_values(i, leaf.doc_id, &mut leaf.values_as_comparable_longs)?;

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
      let top = queue
        .top_mut()
        .ok_or_else(|| LuceneError::illegal_state("no top available"))?;

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
        get_values(
          top.reader_index,
          top.doc_id,
          &mut top.values_as_comparable_longs,
        )?;
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

      doc_maps.push(MergeStateDocMapImpl::new_sorted(live_docs, remapped));
    }
    Ok(Some(doc_maps))
  }
  fn wrap_providers<CR, CP>(
    readers: &[CR],
    providers: Vec<CP>,
  ) -> Result<Vec<ParentOrDirectComparable<CP>>>
  where
    CR: CodecReader,
    CP: ComparableProvider,
  {
    let mut new_providers = Vec::with_capacity(providers.len());
    let mut providers = providers.into_iter();
    #[allow(clippy::needless_range_loop)]
    for j in 0..readers.len() {
      let reader = &readers[j];
      let field_infos = reader.get_field_infos()?;
      let meta = reader.get_metadata()?;

      let inner = providers.next().ok_or_else(|| {
        LuceneError::illegal_state(format!(
          "index sorter did not return a comparable provider for reader {j}"
        ))
      })?;
      let parents = if meta.get_has_blocks() {
        match field_infos.get_parent_field() {
          Some(parent_field) => {
            let mut parent_docs = CodecReader::get_numeric_doc_values(reader, parent_field)?
              .ok_or_else(|| {
                LuceneError::illegal_state(format!(
                  "parent field {} must be present if index sorting is used with blocks",
                  parent_field
                ))
              })?;

            Some(of(&mut parent_docs, reader.max_doc()? as usize)?)
          },
          None if meta.get_created_version_major() >= LUCENE_10_0_0.major => {
            return Err(LuceneError::corrupt_index(format!(
              "parent field is not set but the index has blocks and uses index sorting. indexCreatedVersionMajor: {}",
              meta.get_created_version_major()
            )));
          },
          None => None,
        }
      } else {
        None
      };
      let provider = match parents {
        Some(parents) => ComparableProviderEnum2::A1(ComparableProviderImpl::new(parents, inner)),
        None => ComparableProviderEnum2::B1(inner),
      };
      new_providers.push(provider);
    }
    Ok(new_providers)
  }
}
type ParentOrDirectComparable<CP> = ComparableProviderEnum2<
  ComparableProviderImpl<crate::core::util::bit_set::SparseFixedBitSetBitSet, CP>,
  CP,
>;

struct SingleFieldMergeSortVisitor<'a, CR> {
  readers: &'a [CR],
  reverse_mul: i8,
}

impl<CR> ComparableProvidersVisitor for SingleFieldMergeSortVisitor<'_, CR>
where
  CR: CodecReader,
{
  type Output = Option<Vec<MergeStateDocMap<CR>>>;

  fn visit<CP: ComparableProvider>(self, providers: Vec<CP>) -> Result<Self::Output> {
    let readers = self.readers;
    let new_providers = MultiSorter::wrap_providers(readers, providers)?;
    if new_providers
      .iter()
      .all(|p| matches!(p, ComparableProviderEnum2::B1(_)))
    {
      let providers = new_providers
        .into_iter()
        .filter_map(|p| match p {
          ComparableProviderEnum2::B1(inner) => Some(inner),
          ComparableProviderEnum2::A1(_) => None,
        })
        .collect();
      Self::sort_single(readers, self.reverse_mul, providers)
    } else if new_providers
      .iter()
      .all(|p| matches!(p, ComparableProviderEnum2::A1(_)))
    {
      let providers = new_providers
        .into_iter()
        .filter_map(|p| match p {
          ComparableProviderEnum2::A1(inner) => Some(inner),
          ComparableProviderEnum2::B1(_) => None,
        })
        .collect();
      Self::sort_single(readers, self.reverse_mul, providers)
    } else {
      Self::sort_single(readers, self.reverse_mul, new_providers)
    }
  }
}

impl<CR> SingleFieldMergeSortVisitor<'_, CR>
where
  CR: CodecReader,
{
  fn sort_single<CP: ComparableProvider>(
    readers: &[CR],
    reverse_mul: i8,
    mut providers: Vec<CP>,
  ) -> Result<Option<Vec<MergeStateDocMap<CR>>>> {
    MultiSorter::sort_with_providers::<CR, _, true>(
      readers,
      &[reverse_mul],
      |reader, doc, values| {
        values[0] = providers[reader].get_as_comparable_long(doc)?;
        Ok(())
      },
    )
  }
}

pub(crate) struct LeafAndDocId<B> {
  reader_index: usize,
  live_docs: Option<B>,
  max_doc: i32,
  values_as_comparable_longs: smallvec::SmallVec<[i64; 1]>,
  doc_id: i32,
}
impl<B> LeafAndDocId<B> {
  fn new(reader_index: usize, live_docs: Option<B>, max_doc: i32, num_comparables: usize) -> Self {
    Self {
      reader_index,
      live_docs,
      max_doc,
      values_as_comparable_longs: smallvec::smallvec![0i64; num_comparables],
      doc_id: 0,
    }
  }
}

struct LeafAndDocIDCmp<'a, const SINGLE: bool> {
  comparables_len: usize,
  reverse_muls: &'a [i8],
}
impl<'a, const SINGLE: bool> LeafAndDocIDCmp<'a, SINGLE> {
  fn new(comparables_len: usize, reverse_muls: &'a [i8]) -> Self {
    LeafAndDocIDCmp {
      comparables_len,
      reverse_muls,
    }
  }
}
impl<B, const SINGLE: bool> Compare<LeafAndDocId<B>> for LeafAndDocIDCmp<'_, SINGLE>
where
  B: Bits,
{
  fn less_than(&self, a: &LeafAndDocId<B>, b: &LeafAndDocId<B>) -> Result<bool> {
    if SINGLE {
      let cmp = a.values_as_comparable_longs[0]
        .cmp(&b.values_as_comparable_longs[0])
        .to_int();
      if cmp != 0 {
        return Ok(i32::from(self.reverse_muls[0]) * cmp < 0);
      }
    } else {
      for i in 0..self.comparables_len {
        let cmp = a.values_as_comparable_longs[i]
          .cmp(&b.values_as_comparable_longs[i])
          .to_int();

        if cmp != 0 {
          return Ok(i32::from(self.reverse_muls[i]) * cmp < 0);
        }
      }
    }
    // tie-break by docID natural order:
    if a.reader_index != b.reader_index {
      return Ok(a.reader_index < b.reader_index);
    }

    Ok(a.doc_id < b.doc_id)
  }
}

pub struct ComparableProviderImpl<B, CP> {
  parents: B,
  provider: CP,
}
impl<B, CP> ComparableProviderImpl<B, CP> {
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
