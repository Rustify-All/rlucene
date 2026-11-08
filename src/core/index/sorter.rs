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
use crate::core::index::index_sorter::{
  DocComparator, DocComparatorImpl, DocComparatorVisitor, IndexSorter,
};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::search::sort::Sort;
use crate::core::search::sort_field::SortFiledBase;
use crate::core::util::bit_set::{BitSet, of};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::long_values::LongValues;
use crate::core::util::packed::PackedInts;
use crate::core::util::packed::packed_long_values::PackedLongValues;
use crate::core::util::sorter::Sorter as ASorter;
use crate::core::util::{LUCENE_10_0_0, TimSorter, TimSorterBase, ToInt, TryIntoInt};
use std::fmt::{Display, Formatter};
use std::ops::Deref;
use std::sync::Arc;

/// Sorts documents of a given index by returning a permutation on the document IDs.
pub struct Sorter {
  pub(crate) sort: Sort,
}
impl Sorter {
  pub(crate) fn new(sort: Sort) -> Result<Self> {
    if sort.needs_scores() {
      return Err(LuceneError::illegal_argument(
        "Cannot sort an index with a Sort that refers to the relevance score",
      ));
    }
    Ok(Self { sort })
  }

  /// Check consistency of a [`DocMap`], useful for assertions.
  pub(crate) fn is_consistent<DM>(doc_map: &DM) -> Result<bool>
  where
    DM: DocMap,
  {
    let max_doc = doc_map.size();
    for i in 0..max_doc {
      let new_id = doc_map.old_to_new(i)?;
      let old_id = doc_map.new_to_old(new_id)?;
      debug_assert!(
        (0..max_doc).contains(&new_id),
        "doc IDs must be in [0-{max_doc}), got {new_id}"
      );
      debug_assert_eq!(
        { i },
        old_id,
        "mapping is inconsistent: {i} --oldToNew--> {new_id} --newToOld--> {old_id}"
      );
      if old_id != i || new_id < 0 || new_id >= max_doc {
        return Ok(false);
      }
    }
    Ok(true)
  }

  /// Returns the identifier of this [`Sorter`].
  pub fn get_id(&self) -> String {
    self.sort.to_string()
  }
  /// Computes the old-to-new permutation over the given comparator.
  fn sort_impl<F>(max_doc: i32, comparator: F) -> Result<Option<DocMapImpl>>
  where
    F: Fn(usize, usize) -> i32,
  {
    // check if the index is sorted
    let doc_count = max_doc as usize;
    let mut sorted = true;
    for i in 1..doc_count {
      if comparator(i - 1, i) > 0 {
        sorted = false;
        break;
      }
    }
    if sorted {
      return Ok(None);
    }

    // sort doc IDs
    let mut docs = smallvec::SmallVec::<[i32; 8]>::with_capacity(doc_count);
    docs.extend(0..max_doc);
    let mut sorter = DocValueSorter::new(&mut docs, comparator);
    // It can be common to sort a reader, add docs, sort it again, ... and in
    // that case timSort can save a lot of time
    sorter.sort(0, doc_count)?; // docs is now the newToOld mapping

    // The reason why we use MonotonicAppendingLongBuffer here is that it
    // wastes very little memory if the index is in random order but can save
    // a lot of memory if the index is already "almost" sorted
    let mut new_to_old_builder =
      PackedLongValues::monotonic_long_values_builder_default(PackedInts::COMPACT)?;
    for &doc in &docs {
      new_to_old_builder.add(doc as i64)?;
    }
    let new_to_old = new_to_old_builder.build()?;

    // invert the docs mapping:
    for i in 0..max_doc {
      let old = new_to_old.get(i as usize)?;
      docs[old as usize] = i;
    } // docs is now the oldToNew mapping

    let mut old_to_new_builder =
      PackedLongValues::monotonic_long_values_builder_default(PackedInts::COMPACT)?;
    for &doc in &docs {
      old_to_new_builder.add(doc as i64)?;
    }
    let old_to_new = old_to_new_builder.build()?;

    Ok(Some(DocMapImpl::new(old_to_new, new_to_old, max_doc)))
  }
  /// Returns a mapping from the old document ID to its new location in the sorted index.
  ///
  /// Implementations can use [`sort(max_doc, comparator)`] to compute the old-to-new permutation
  /// given a list of documents and their corresponding values.
  ///
  /// A return value of `None` indicates that the reader is already sorted.
  ///
  /// **Note:** Deleted documents are expected to appear in the mapping as well; they will
  /// still be marked as deleted in the sorted view.
  pub(crate) fn sort_with_reader<LR>(&self, reader: &LR) -> Result<Option<DocMapImpl>>
  where
    LR: LeafReader,
  {
    let fields = self.sort.get_sort();

    let meta_data = reader.get_metadata()?;
    let field_infos = reader.get_field_infos()?;

    let parents_opt = if meta_data.get_has_blocks() {
      match field_infos.get_parent_field() {
        None => None,
        Some(parent_field) => {
          let mut dv = reader
            .get_numeric_doc_values(parent_field)?
            .ok_or_else(|| LuceneError::illegal_state("numeric doc values is None"))?;
          Some(of(&mut dv, reader.max_doc()? as usize)?)
        },
      }
    } else {
      None
    };

    if meta_data.get_has_blocks()
      && field_infos.get_parent_field().is_none()
      && meta_data.get_created_version_major() >= LUCENE_10_0_0.major
    {
      return Err(LuceneError::corrupt_index(format!(
        "parent field is not set but the index has blocks. indexCreatedVersionMajor: {}",
        meta_data.get_created_version_major()
      )));
    }

    let mut comparators = smallvec::SmallVec::<[_; 1]>::with_capacity(fields.len());
    for field in fields {
      let sorter = field.get_index_sorter()?.ok_or_else(|| {
        LuceneError::illegal_argument(format!("Cannot use sortfield {} to sort indexes", field))
      })?;

      let comparator = sorter.get_doc_comparator(reader, reader.max_doc()?)?;

      match parents_opt {
        Some(ref parents) => comparators.push(DocComparatorEnum::BS(DocComparatorWrapper::new(
          comparator, parents,
        ))),
        None => comparators.push(DocComparatorEnum::Plain(comparator)),
      }
    }

    Self::sort(reader.max_doc()?, &comparators)
  }

  pub(crate) fn sort<DC>(max_doc: i32, comparators: &[DC]) -> Result<Option<DocMapImpl>>
  where
    DC: DocComparator,
  {
    if let [comparator] = comparators {
      return comparator.visit(SingleComparatorVisitor { max_doc });
    }
    let composite = {
      #[inline(never)]
      move |doc_id1: usize, doc_id2: usize| {
        for cmp in comparators {
          let comp = cmp.compare(doc_id1, doc_id2);
          if comp != 0 {
            return comp;
          }
        }
        // docid order tiebreak
        doc_id1.cmp(&doc_id2).to_int()
      }
    };
    Self::sort_impl(max_doc, composite)
  }
}

struct SingleComparatorVisitor {
  max_doc: i32,
}

impl DocComparatorVisitor for SingleComparatorVisitor {
  type Output = Result<Option<DocMapImpl>>;

  fn visit<DC: DocComparator>(self, comparator: &DC) -> Self::Output {
    let composite = {
      #[inline(never)]
      move |doc_id1: usize, doc_id2: usize| {
        let comp = comparator.compare(doc_id1, doc_id2);
        if comp != 0 {
          return comp;
        }
        doc_id1.cmp(&doc_id2).to_int()
      }
    };
    Sorter::sort_impl(self.max_doc, composite)
  }
}

impl Display for Sorter {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", self.get_id())
  }
}

pub trait DocMap {
  /// Given a doc ID from the original index, return its ordinal in the sorted
  /// index.
  fn old_to_new(&self, doc_id: i32) -> Result<i32>;

  /// Given the ordinal of a doc ID, return its doc ID in the original index.
  fn new_to_old(&self, doc_id: i32) -> Result<i32>;

  /// Return the number of documents in this map.
  /// This must equal the number of documents in the sorted [`LeafReader`].
  fn size(&self) -> i32;
}
impl<T> DocMap for &T
where
  T: DocMap + ?Sized,
{
  fn old_to_new(&self, doc_id: i32) -> Result<i32> {
    (**self).old_to_new(doc_id)
  }

  fn new_to_old(&self, doc_id: i32) -> Result<i32> {
    (**self).new_to_old(doc_id)
  }

  fn size(&self) -> i32 {
    (**self).size()
  }
}
impl<T> DocMap for Arc<T>
where
  T: DocMap,
{
  fn old_to_new(&self, doc_id: i32) -> Result<i32> {
    (**self).old_to_new(doc_id)
  }

  fn new_to_old(&self, doc_id: i32) -> Result<i32> {
    (**self).new_to_old(doc_id)
  }

  fn size(&self) -> i32 {
    (**self).size()
  }
}

struct DocValueSorter<'a, F> {
  docs: &'a mut [i32],
  comparator: F,
  tmp: Vec<i32>,
  pivot_index: usize,
}
impl<'a, F> DocValueSorter<'a, F>
where
  F: Fn(usize, usize) -> i32,
{
  pub fn new(docs: &'a mut [i32], comparator: F) -> TimSorter<DocValueSorter<'a, F>> {
    let max_temp_slots = docs.len() / 64;
    let sub = DocValueSorter {
      docs,
      comparator,
      tmp: Vec::new(),
      pivot_index: 0,
    };
    TimSorter::new(max_temp_slots, sub)
  }
}
impl<'a, F> TimSorterBase for DocValueSorter<'a, F>
where
  F: Fn(usize, usize) -> i32,
{
  fn copy(&mut self, src: usize, dest: usize) -> Result<()> {
    self.docs[dest] = self.docs[src];
    Ok(())
  }

  fn save(&mut self, i: usize, len: usize) -> Result<()> {
    if self.tmp.capacity() == 0 {
      self.tmp.reserve_exact(self.docs.len() / 64);
    }
    self.tmp.clear();
    self.tmp.extend_from_slice(&self.docs[i..i + len]);
    Ok(())
  }

  fn restore(&mut self, i: usize, j: usize) {
    self.docs[j] = self.tmp[i];
  }

  #[inline]
  fn compare_saved(&self, i: usize, j: usize) -> Result<i32> {
    Ok((self.comparator)(
      self.tmp[i] as usize,
      self.docs[j] as usize,
    ))
  }
}
impl<'a, F> crate::core::util::Sorter for DocValueSorter<'a, F>
where
  F: Fn(usize, usize) -> i32,
{
  #[inline]
  fn compare(&mut self, i: usize, j: usize) -> Result<i32> {
    Ok((self.comparator)(
      self.docs[i] as usize,
      self.docs[j] as usize,
    ))
  }

  fn swap(&mut self, i: usize, j: usize) -> Result<()> {
    self.docs.swap(i, j);
    Ok(())
  }

  fn set_pivot(&mut self, i: usize) -> Result<()> {
    self.pivot_index = i;
    Ok(())
  }

  fn compare_pivot(&mut self, j: usize) -> Result<i32> {
    self.compare(self.pivot_index, j)
  }
}
pub struct DocMapImpl {
  old_to_new: PackedLongValues,
  new_to_old: PackedLongValues,
  max_doc: i32,
}
impl DocMapImpl {
  pub fn new(old_to_new: PackedLongValues, new_to_old: PackedLongValues, max_doc: i32) -> Self {
    DocMapImpl {
      old_to_new,
      new_to_old,
      max_doc,
    }
  }
}
impl DocMap for DocMapImpl {
  fn old_to_new(&self, doc_id: i32) -> Result<i32> {
    let v = self.old_to_new.get(doc_id as usize)?.try_convert()?;
    Ok(v)
  }

  fn new_to_old(&self, doc_id: i32) -> Result<i32> {
    let v = self.new_to_old.get(doc_id as usize)?.try_convert()?;
    Ok(v)
  }

  fn size(&self) -> i32 {
    self.max_doc
  }
}

pub struct DocComparatorWrapper<DC, B> {
  in_: DC,
  parents: B,
}
impl<DC, B> DocComparatorWrapper<DC, B> {
  fn new(cmp: DC, parents: B) -> Self {
    DocComparatorWrapper { in_: cmp, parents }
  }
}
impl<DC, B> DocComparator for DocComparatorWrapper<DC, B>
where
  DC: DocComparator,
  B: Deref,
  B::Target: BitSet,
{
  fn compare(&self, doc_id1: usize, doc_id2: usize) -> i32 {
    self.in_.compare(
      self.parents.next_set_bit(doc_id1),
      self.parents.next_set_bit(doc_id2),
    )
  }
}

pub enum DocComparatorEnum<DC, B> {
  Plain(DocComparatorImpl),
  BS(DocComparatorWrapper<DC, B>),
}
impl<DC, B> DocComparator for DocComparatorEnum<DC, B>
where
  DC: DocComparator,
  B: Deref,
  B::Target: BitSet,
{
  fn compare(&self, doc_id1: usize, doc_id2: usize) -> i32 {
    match self {
      DocComparatorEnum::Plain(cmp) => cmp.compare(doc_id1, doc_id2),
      DocComparatorEnum::BS(cmp) => cmp.compare(doc_id1, doc_id2),
    }
  }
}
