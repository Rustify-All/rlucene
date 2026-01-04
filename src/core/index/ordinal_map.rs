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
use crate::core::index::index_reader::CacheKey;
use crate::core::index::sorted_doc_values::SortedDocValues;
use crate::core::index::sorted_set_doc_values::SortedSetDocValues;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::index::terms_enum_index::{TermState, TermsEnumIndex};
use crate::core::util::Sorter;
use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::in_place_merge_sorter::InPlaceMergeSorter;
use crate::core::util::long_values::{LongValues, LongValuesEnum2, LongValuesEnum3, Zeroes};
use crate::core::util::packed::mutable_packed64_enum::MutablePacked64Enum;
use crate::core::util::packed::packed_long_values::{PackedLongValues, PackedLongValuesBuilder};
use crate::core::util::packed::{Mutable, PackedInts, Reader};
use crate::core::util::priority_queue::{Compare, PriorityQueue};
use std::rc::Rc;

pub(crate) type SegmentToGlobalOrds =
    LongValuesEnum3<crate::core::util::long_values::Identity, LongValuesImpl, LongValuesImpl1>;

impl OrdinalMap {
    /// Create an ordinal map that uses the number of unique values of each
    /// [`SortedDocValues`] instance as a weight.
    ///
    /// See [`OrdinalMap::build`].
    pub fn build_from_sorted<DV>(
        owner: Option<CacheKey>,
        values: &mut [DV],
        acceptable_overhead_ratio: f32,
    ) -> Result<Self>
    where
        DV: SortedDocValues,
    {
        let len = values.len();
        let mut subs = Vec::with_capacity(len);
        let mut weights = Vec::with_capacity(len);

        for dv in values.iter_mut() {
            let count = dv.get_value_count()? as i64;
            subs.push(Some(dv.terms_enum()?));
            weights.push(count);
        }

        OrdinalMap::build(owner, subs, &weights, acceptable_overhead_ratio)
    }
    /// Create an ordinal map that uses the number of unique values of each
    /// [`SortedSetDocValues`] instance as a weight.
    ///
    /// See [`OrdinalMap::build`].
    pub fn build_from_sorted_set<DV>(
        owner: Option<CacheKey>,
        values: &mut [DV],
        acceptable_overhead_ratio: f32,
    ) -> Result<Self>
    where
        DV: SortedSetDocValues,
    {
        let len = values.len();
        let mut subs = Vec::with_capacity(len);
        let mut weights = Vec::with_capacity(len);

        for dv in values.iter_mut() {
            let count = dv.get_value_count()?;
            subs.push(Some(dv.terms_enum()?));
            weights.push(count);
        }

        OrdinalMap::build(owner, subs, &weights, acceptable_overhead_ratio)
    }

    /// Creates an ordinal map that allows mapping ords to/from a merged space from `subs`.
    ///
    /// - `owner`: a cache key.
    /// - `subs`: [`TermsEnum`]s that support [`TermsEnum::ord`]. They need not be dense
    ///   (for example, they can be filtered term enums).
    /// - `weights`: a weight for each sub. This is ideally correlated with the number
    ///   of unique terms that each sub introduces compared to the other subs.
    ///
    /// # Errors
    ///
    /// Returns an error if an I/O error occurs while building the map.
    pub fn build<TE>(
        owner: Option<CacheKey>,
        subs: Vec<Option<TE>>,
        weights: &[i64],
        acceptable_overhead_ratio: f32,
    ) -> Result<Self>
    where
        TE: TermsEnum,
    {
        if subs.len() != weights.len() {
            return Err(LuceneError::illegal_argument(
                "subs and weights must have the same length",
            ));
        }
        // enums are not sorted, so let's sort to save memory
        let segment_map = SegmentMap::new(weights)?;
        OrdinalMap::new(owner, subs, segment_map, acceptable_overhead_ratio)
    }
}

pub struct OrdinalMap {
    /// Cache key of whoever asked for this awful thing
    owner: Option<CacheKey>,
    /// number of global ordinals
    value_count: i64,
    /// globalOrd -> (globalOrd - segmentOrd) where segmentOrd is the ordinal in the first segment
    /// that contains this term
    global_ord_deltas: LongValuesEnum2<PackedLongValues, Zeroes>,
    /// globalOrd -> first segment container
    first_segments: LongValuesEnum2<PackedLongValues, Zeroes>,
    segment_to_global_ords: Vec<Rc<SegmentToGlobalOrds>>,
    /// the map from/to segment ids
    segment_map: SegmentMap,
    /// ram usage
    ram_bytes_used: i64,
}
impl OrdinalMap {
    /// Here is how the [`OrdinalMap`] encodes the mapping from global ords to local segment ords.
    /// Assume we have the following global mapping for a doc values field:
    /// `bar -> 0`, `cat -> 1`, `dog -> 2`, `foo -> 3`
    ///
    /// And our index is split into 2 segments with the following local mappings for that same doc
    /// values field:
    ///
    /// - Segment 0: `bar -> 0`, `foo -> 1`
    /// - Segment 1: `cat -> 0`, `dog -> 1`
    ///
    /// We will then encode the delta between the local and global mapping in a packed 2D array keyed by
    /// `(segmentIndex, segmentOrd)`. So the following 2D array will be created by [`OrdinalMap`]:
    ///
    /// `[[0, 2], [1, 1]]`
    ///
    /// The general algorithm for creating an [`OrdinalMap`] (skipping over some implementation details
    /// and optimizations) is as follows:
    ///
    /// 1. Create and populate a PQ with (`[`TermsEnum`]`, index) tuples where index is the
    ///    position of the `termEnum` in an array of `termEnum`s sorted by descending size.
    ///    The PQ itself will be ordered by [`TermsEnum::term`].
    ///
    /// 2. We will iterate through every term in the index now. In order to do so, we will start
    ///    with the first term at the top of the PQ. We keep track of a global ord, and track the
    ///    difference between the global ord and [`TermsEnum::ord`] in `ordDeltas`, which maps:
    ///
    ///    `(segmentIndex, TermsEnum::ord()) -> globalTermOrdinal - TermsEnum::ord()`
    ///
    ///    We then call [`TermsEnum::next`] and update the PQ to iterate（remember the PQ maintains
    ///    an order based on [`TermsEnum::term`] which changes on the `next()` calls）。If the current
    ///    term exists in some other segment, the top of the queue will contain that segment.
    ///    If not, the top of the queue will contain a segment with the next term in the index and the
    ///    global ord will also be incremented.
    ///
    /// 3. We use some information gathered in the previous step to perform optimizations on memory
    ///    usage and building time in the following steps, for more detail on those, look at the code.
    ///
    /// 4. We will then populate `segment_to_global_ords`, which maps
    ///    `(segmentIndex, segmentOrd) -> globalOrd`. Using the information we tracked in `ordDeltas`,
    ///    we can construct this information relatively easily.
    ///
    /// # Parameters
    ///
    /// * `owner` – For caching purposes.
    /// * `subs` – A `TermsEnum[]`, where each index corresponds to a segment.
    /// * `segment_map` – Provides two maps, `newToOld` which lists segments in descending "weight"
    ///   order（see [`SegmentMap`] for more details）and an `oldToNew` map which maps each original
    ///   segment index to their position in `newToOld`.
    /// * `acceptable_overhead_ratio` – Acceptable overhead memory usage for some packed data structures.
    ///
    /// # Errors
    ///
    /// May return an error corresponding to I/O failures (equivalent to throwing `IOException` in Java).
    fn new<TE>(
        owner: Option<CacheKey>,
        // wrap with Option for easy taken
        mut subs: Vec<Option<TE>>,
        segment_map: SegmentMap,
        acceptable_overhead_ratio: f32,
    ) -> Result<Self>
    where
        TE: TermsEnum,
    {
        // create the ordinal mappings by pulling a termsenum over each sub's
        // unique terms, and walking a multitermsenum over those

        let mut global_ord_deltas =
            PackedLongValues::monotonic_long_values_builder_default(PackedInts::COMPACT)?;

        let mut first_segments =
            PackedLongValues::packed_long_values_builder_default(PackedInts::COMPACT)?;

        let mut first_segment_bits: i64 = 0;

        let sub_len = subs.len();
        let mut ord_deltas: Vec<PackedLongValuesBuilder> = Vec::with_capacity(sub_len);
        for _ in 0..subs.len() {
            ord_deltas.push(PackedLongValues::monotonic_long_values_builder_default(
                acceptable_overhead_ratio,
            )?);
        }

        let mut ord_delta_bits = vec![0i64; sub_len];
        let mut segment_ords = vec![0i64; sub_len];

        //  queue of term enums
        let mut queue = PriorityQueue::new(subs.len().try_into()?, TermsEnumPriorityQueueCmp)?;
        for i in 0..sub_len {
            let mapped = segment_map.new_to_old(i);
            let mut sub = TermsEnumIndex::new(subs[mapped as usize].take(), i as i32);
            if sub.next()?.is_some() {
                queue.add(sub)?;
            }
        }

        let mut top_state = TermState::new();
        let mut global_ord: i64 = 0;
        let mut ord_delta_bits_has_value = false;
        while queue.size() != 0 {
            let mut top = queue.top_mut().unwrap();
            top_state.copy_from(top)?;

            let mut first_segment_index = i32::MAX;
            let mut global_ord_delta = i64::MAX;
            // Advance past this term, recording the per-segment ord deltas:
            loop {
                let segment_ord = top.terms_enum.as_ref().unwrap().ord()?;
                let delta = global_ord - segment_ord;
                let segment_index = top.sub_index as usize;
                // We compute the least segment where the term occurs. In case the
                // first segment contains most (or better all) values, this will
                // help save significant memory
                if (segment_index as i32) < first_segment_index {
                    first_segment_index = segment_index as i32;
                    global_ord_delta = delta;
                }
                ord_delta_bits[segment_index] |= delta;
                ord_delta_bits_has_value = true;
                // for each per-segment ord, map it back to the global term; the while loop is needed
                // in case the incoming TermsEnums don't have compact ordinals (some ordinal values
                // are skipped), which can happen e.g. with a FilteredTermsEnum:
                debug_assert!(segment_ords[segment_index] <= segment_ord);

                loop {
                    ord_deltas[segment_index].add(delta)?;
                    segment_ords[segment_index] += 1;
                    if segment_ords[segment_index] > segment_ord {
                        break;
                    }
                }

                if top.next()?.is_none() {
                    queue.pop()?;
                    if queue.size() == 0 {
                        break;
                    }
                    top = queue.top_mut().unwrap();
                } else {
                    top = queue.update_top()?;
                }

                if !top.term_equals(&top_state)? {
                    break;
                }
            }

            first_segments.add(first_segment_index as i64)?;
            first_segment_bits |= first_segment_index as i64;
            global_ord_deltas.add(global_ord_delta)?;
            global_ord += 1;
        }
        // TODO: memory calculation not implemented
        let mut ram_bytes_used = segment_map.ram_bytes_used()?;
        let value_count = global_ord;

        // If the first segment contains all of the global ords, then we can apply a small optimization
        // and hardcode the first segment indices and global ord deltas as all zeroes.
        let (first_segments_lv, global_ord_deltas_lv) =
            if ord_delta_bits_has_value && ord_delta_bits[0] == 0 && first_segment_bits == 0 {
                (LongValuesEnum2::B(Zeroes {}), LongValuesEnum2::B(Zeroes {}))
            } else {
                let packed_first_segments = first_segments.build()?;
                let packed_global_ord_deltas = global_ord_deltas.build()?;
                ram_bytes_used += packed_first_segments.ram_bytes_used()?
                    + packed_global_ord_deltas.ram_bytes_used()?;
                (
                    LongValuesEnum2::A(packed_first_segments),
                    LongValuesEnum2::A(packed_global_ord_deltas),
                )
            };

        // ordDeltas is typically the bottleneck, so let's see what we can do to make it faster
        let mut segment_to_global_ords = Vec::with_capacity(sub_len);
        // TODO: memory calculation not implemented
        // ram_bytes_used += 0;
        for i in 0..ord_deltas.len() {
            let deltas = ord_deltas[i].build()?;
            if ord_delta_bits[i] == 0 {
                // segment ords perfectly match global ordinals
                // likely in case of low cardinalities and large segments
                segment_to_global_ords.push(Rc::new(LongValuesEnum3::A(
                    crate::core::util::long_values::Identity,
                )));
            } else {
                let bits_required = if ord_delta_bits[i] < 0 {
                    64
                } else {
                    PackedInts::bits_required(ord_delta_bits[i])?
                };

                let monotonic_bits = deltas.ram_bytes_used()? * 8;
                let packed_bits = bits_required as i64 * deltas.size();

                if deltas.size() <= i32::MAX as i64
                    && (packed_bits as f32)
                        <= (monotonic_bits as f32) * (1.0 + acceptable_overhead_ratio)
                {
                    // monotonic compression mostly adds overhead, let's keep the mapping in plain packed ints
                    let size = deltas.size() as i32;
                    let mut new_deltas =
                        PackedInts::get_mutable(size, bits_required, acceptable_overhead_ratio);

                    let mut it = deltas.iterator();
                    for ord in 0..size {
                        let v = it.next_value();
                        new_deltas.set(ord, v);
                    }
                    debug_assert!(!it.has_next());
                    ram_bytes_used += new_deltas.ram_bytes_used()?;
                    segment_to_global_ords
                        .push(Rc::new(LongValuesEnum3::B(LongValuesImpl::new(new_deltas))));
                } else {
                    ram_bytes_used += deltas.ram_bytes_used()?;
                    segment_to_global_ords
                        .push(Rc::new(LongValuesEnum3::C(LongValuesImpl1::new(deltas))));
                }
                // TODO: memory calculation not implemented
                // ram_bytes_used += 0;
            }
        }
        Ok(OrdinalMap {
            owner,
            value_count,
            global_ord_deltas: global_ord_deltas_lv,
            first_segments: first_segments_lv,
            segment_map,
            ram_bytes_used,
            segment_to_global_ords,
        })
    }
    /// Given a segment number, return a [`LongValues`] instance that maps segment ordinals
    /// to global ordinals.
    pub(crate) fn get_global_ords(&self, segment_index: i32) -> &Rc<SegmentToGlobalOrds> {
        let mapped = self.segment_map.old_to_new(segment_index as usize) as usize;
        &self.segment_to_global_ords[mapped]
    }

    /// Given a global ordinal, returns the ordinal of the first segment which contains this
    /// ordinal (the corresponding segment index is returned by [`Self::get_first_segment_number`]).
    pub fn get_first_segment_ord(&self, global_ord: i64) -> Result<i64> {
        Ok(global_ord - self.global_ord_deltas.get(global_ord)?)
    }

    /// Given a global ordinal, returns the index of the first segment that contains this term.
    pub fn get_first_segment_number(&self, global_ord: i64) -> Result<i32> {
        let idx = self.first_segments.get(global_ord)? as usize;
        Ok(self.segment_map.new_to_old(idx))
    }

    /// Returns the total number of unique terms in global ord space.
    pub fn get_value_count(&self) -> i64 {
        self.value_count
    }
}
impl Accountable for OrdinalMap {
    fn ram_bytes_used(&self) -> Result<i64> {
        Ok(self.ram_bytes_used)
    }

    fn get_child_resources<A>(&self) -> Vec<A>
    where
        A: Accountable,
    {
        todo!()
    }
}

pub(crate) struct LongValuesImpl {
    new_deltas: MutablePacked64Enum,
}
impl LongValuesImpl {
    fn new(new_deltas: MutablePacked64Enum) -> Self {
        Self { new_deltas }
    }
}
impl LongValues for LongValuesImpl {
    fn get(&self, ord: i64) -> Result<i64> {
        Ok(ord + self.new_deltas.get(ord.try_into()?))
    }
}

pub(crate) struct LongValuesImpl1 {
    deltas: PackedLongValues,
}
impl LongValuesImpl1 {
    fn new(deltas: PackedLongValues) -> Self {
        Self { deltas }
    }
}
impl LongValues for LongValuesImpl1 {
    fn get(&self, ord: i64) -> Result<i64> {
        Ok(ord + self.deltas.get(ord)?)
    }
}

struct TermsEnumPriorityQueueCmp;
impl<TE> Compare<TermsEnumIndex<TE>> for TermsEnumPriorityQueueCmp
where
    TE: TermsEnum,
{
    fn less_than(&self, a: &TermsEnumIndex<TE>, b: &TermsEnumIndex<TE>) -> Result<bool> {
        Ok(a.compare_term_to(b)? < 0)
    }
}

pub struct SegmentMap {
    new_to_old: Vec<i32>,
    old_to_new: Vec<i32>,
}

impl SegmentMap {
    pub fn new(weights: &[i64]) -> Result<Self> {
        let new_to_old = map(weights)?;
        let old_to_new = inverse(&new_to_old);
        debug_assert_eq!(new_to_old, inverse(&old_to_new));

        Ok(Self {
            new_to_old,
            old_to_new,
        })
    }

    fn new_to_old(&self, segment: usize) -> i32 {
        self.new_to_old[segment]
    }

    fn old_to_new(&self, segment: usize) -> i32 {
        self.old_to_new[segment]
    }
}
impl Accountable for SegmentMap {
    fn ram_bytes_used(&self) -> Result<i64> {
        // TODO: memory calculation not implemented
        Ok(0)
    }
}

fn map(weights: &[i64]) -> Result<Vec<i32>> {
    let mut new_to_old: Vec<i32> = (0..weights.len() as i32).collect();
    let sub = InPlaceMergeSorterSorter::new(weights, &mut new_to_old);
    let mut sorter = InPlaceMergeSorter::new(sub);
    sorter.sort(0, weights.len().try_into()?)?;
    Ok(new_to_old)
}
/// Inverse the map.
fn inverse(map: &[i32]) -> Vec<i32> {
    let mut inverse = vec![0; map.len()];

    for (new_ord, &old_ord) in map.iter().enumerate() {
        inverse[old_ord as usize] = new_ord as i32;
    }

    inverse
}
struct InPlaceMergeSorterSorter<'a> {
    weights: &'a [i64],
    new_to_old: &'a mut [i32],
}
impl<'a> InPlaceMergeSorterSorter<'a> {
    pub fn new(weights: &'a [i64], new_to_old: &'a mut [i32]) -> Self {
        InPlaceMergeSorterSorter {
            weights,
            new_to_old,
        }
    }
}
impl<'a> Sorter for InPlaceMergeSorterSorter<'a> {
    fn compare(&mut self, i: usize, j: usize) -> Result<i32> {
        let wi = self.weights[self.new_to_old[i] as usize];
        let wj = self.weights[self.new_to_old[j] as usize];
        Ok(wj.cmp(&wi) as i32)
    }

    fn swap(&mut self, i: usize, j: usize) -> Result<()> {
        self.new_to_old.swap(i, j);

        Ok(())
    }
}
#[cfg(test)]
mod tests {

    use crate::core::util::error::lucene_error::Result;

    #[allow(dead_code)] // for quick search
    struct TestOrdinalMap;

    #[test]
    fn test_ram_bytes_used() -> Result<()> {
        // TODO: memory calculation not implemented
        Ok(())
    }

    /// Tests the case where one segment contains all of the global ords.
    /// In this case, we apply a small optimization and hardcode the first segment indices and global ord deltas as all zeroes.
    #[test]
    fn test_one_segment_with_all_values() -> Result<()> {
        // TODO 需要完成force_merge 才能正常运行
        // let mut random = random();
        //
        // let dir = new_directory_shared(&mut random)?;
        //
        // // TODO 这里需要使用带分词器的构造方法
        // // TODO NoMergePolicy 未实现/未接入
        // let cfg = new_index_writer_config(&mut random);
        //
        // let iw = IndexWriter::new(dir.clone(), cfg)?;
        //
        // let num_terms = 1000;
        //
        // for i in 0..num_terms {
        //     let mut d = Document::new();
        //     let term = i.to_string();
        //     d.add(SortedDocValuesField::new("sdv", BytesRef::from_string(term.as_str())));
        //     iw.add_document(d)?;
        // }
        //
        // // TODO force_merge 未实现
        // // iw.force_merge(1)?;
        //
        // for _ in 0..10 {
        //     let mut d = Document::new();
        //     let term = random.random_range(0..num_terms).to_string();
        //     d.add(SortedDocValuesField::new("sdv", BytesRef::from_string(term.as_str())));
        //     iw.add_document(d)?;
        // }
        //
        // iw.commit()?;
        //
        // let r = directory_reader_util::open_with_writer(&iw)?;
        //
        // let sdv = MultiDocValues::get_sorted_values(r, "sdv")?;
        // assert!(sdv.is_some());
        // let sdv = sdv.unwrap();
        //
        // // Check that the optimization kicks in.
        // let map = match sdv {
        //     SortedDocValuesEnum2::B(ref msdv) => &msdv.mapping,
        //     _ => unreachable!("sdv should be MultiSortedDocValues"),
        // };
        //
        // assert!(matches!(map.first_segments, LongValuesEnum2::B(_)));
        // assert!(matches!(map.global_ord_deltas, LongValuesEnum2::B(_)));
        //
        // // Check the map's basic behavior.
        // assert_eq!(num_terms as i64, map.get_value_count());
        // for i in 0..num_terms {
        //     assert_eq!(0, map.get_first_segment_number(i as i64)?);
        //     assert_eq!(i as i64, map.get_first_segment_ord(i as i64)?);
        // }
        //
        // iw.close()?;

        Ok(())
    }
}
