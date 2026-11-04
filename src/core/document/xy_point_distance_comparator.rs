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
use crate::core::document::xy_doc_values_field::XYDocValuesField;
use crate::core::geo::xy_encoding_utils::XYEncodingUtils;
use crate::core::geo::xy_rectangle::XYRectangle;
use crate::core::index::doc_values::{DocValues, SortedNumeric};
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::sorted_numeric_doc_values::SortedNumericDocValues;
use crate::core::search::dummy::dummy_disi::DummyDISI;
use crate::core::search::field_comparator::FieldComparator;
use crate::core::search::leaf_field_comparator::LeafFieldComparator;
use crate::core::search::scorable::Scorable;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::{CoreHelper, ToInt};
/// Compares documents by distance from an origin point
///
/// When the least competitive item on the priority queue changes (`set_bottom`), we recompute a
/// bounding box representing competitive distance to the top-N. Then in `compare_bottom`, we can
/// quickly reject hits based on bounding box alone without computing distance for every element.
pub struct XYPointDistanceComparator {
  field: String,
  x: f64,
  y: f64,

  // distances needs to be calculated with square root to
  // avoid numerical issues (square distances are different but
  // actual distances are equal)
  values: Vec<f64>,
  bottom: f64,
  top_value: f64,

  // current bounding box(es) for the bottom distance on the PQ.
  // these are pre-encoded with XYPoint's encoding and
  // used to exclude uncompetitive hits faster.
  min_x: i32,
  max_x: i32,
  min_y: i32,
  max_y: i32,

  // the number of times setBottom has been called (adversary protection)
  set_bottom_counter: i32,

  current_values: smallvec::SmallVec<[i64; 4]>,
  values_doc_id: i32,
}

impl XYPointDistanceComparator {
  pub(crate) fn new(field: String, x: f32, y: f32, num_hits: usize) -> Self {
    Self {
      field,
      x: x as f64,
      y: y as f64,
      values: vec![0.0; num_hits],
      bottom: 0.0,
      top_value: 0.0,
      min_x: i32::MIN,
      max_x: i32::MAX,
      min_y: i32::MIN,
      max_y: i32::MAX,
      set_bottom_counter: 0,
      current_values: smallvec::smallvec![0; 4],
      values_doc_id: -1,
    }
  }
}
impl FieldComparator for XYPointDistanceComparator {
  type V = f64;

  fn compare(&self, slot1: usize, slot2: usize) -> i32 {
    CoreHelper::compare_f64(self.values[slot1], self.values[slot2]).to_int()
  }

  fn set_top_value(&mut self, value: Self::V) -> Result<()> {
    self.top_value = value;
    Ok(())
  }

  fn value(&self, slot: usize) -> Option<Self::V> {
    Some(self.values[slot])
  }

  fn compare_values(&self, first: Option<&Self::V>, second: Option<&Self::V>) -> Result<i32> {
    match (first, second) {
      (None, None) => Ok(0),
      (None, Some(_)) => Ok(-1),
      (Some(_), None) => Ok(1),
      (Some(&first), Some(&second)) => Ok(CoreHelper::compare_f64(first, second).to_int()),
    }
  }

  type LeafFieldComparator<LR>
    = XYPointDistanceLeafComparator<SortedNumeric<LR>>
  where
    LR: LeafReader;

  fn get_leaf_comparator<LR>(
    &mut self,
    context: &LeafReaderContext<LR>,
  ) -> Result<Self::LeafFieldComparator<LR>>
  where
    LR: LeafReader,
  {
    let reader = context.reader();
    if let Some(info) = reader.get_field_infos()?.field_info_by_name(&self.field)? {
      XYDocValuesField::check_compatible(info.as_ref())?;
    }

    let current_docs = DocValues::get_sorted_numeric(reader, &self.field)?;
    self.values_doc_id = -1;
    Ok(XYPointDistanceLeafComparator { current_docs })
  }
}

pub struct XYPointDistanceLeafComparator<DVS> {
  current_docs: DVS,
}
impl<DVS> XYPointDistanceLeafComparator<DVS>
where
  DVS: SortedNumericDocValues,
{
  fn set_values(
    &mut self,
    comparator: &mut <XYPointDistanceLeafComparator<DVS> as LeafFieldComparator>::FieldComparator,
  ) -> Result<()> {
    if comparator.values_doc_id != self.current_docs.doc_id() {
      debug_assert!(
        comparator.values_doc_id < self.current_docs.doc_id(),
        " valuesDocID={} vs {}",
        comparator.values_doc_id,
        self.current_docs.doc_id()
      );

      comparator.values_doc_id = self.current_docs.doc_id();
      let count = self.current_docs.doc_value_count()? as usize;
      let values = &mut comparator.current_values;
      if count > values.len() {
        let capacity = ArrayUtil::oversize(count, std::mem::size_of::<i64>())?;
        if count > values.capacity() {
          let mut replacement = smallvec::SmallVec::with_capacity(capacity);
          replacement.resize(count, 0);
          *values = replacement;
        } else {
          values.resize(count, 0);
        }
      }

      for i in 0..count {
        comparator.current_values[i] = self.current_docs.next_value()?;
      }
    }

    Ok(())
  }
  fn sort_key(
    &mut self,
    doc: i32,
    comparator: &mut <XYPointDistanceLeafComparator<DVS> as LeafFieldComparator>::FieldComparator,
  ) -> Result<f64> {
    if doc > self.current_docs.doc_id() {
      self.current_docs.advance(doc)?;
    }

    let mut min_value = f64::INFINITY;
    if doc == self.current_docs.doc_id() {
      self.set_values(comparator)?;

      let num_values = self.current_docs.doc_value_count()? as usize;
      for i in 0..num_values {
        let encoded = comparator.current_values[i];
        let doc_x = XYEncodingUtils::decode((encoded >> 32) as i32);
        let doc_y = XYEncodingUtils::decode((encoded & 0xFFFF_FFFF) as i32);
        let diff_x = comparator.x - doc_x as f64;
        let diff_y = comparator.y - doc_y as f64;
        let distance = (diff_x * diff_x + diff_y * diff_y).sqrt();
        min_value = CoreHelper::min_f64(min_value, distance);
      }
    }

    Ok(min_value)
  }
}
impl<DVS> LeafFieldComparator for XYPointDistanceLeafComparator<DVS>
where
  DVS: SortedNumericDocValues,
{
  type FieldComparator = XYPointDistanceComparator;

  fn set_bottom(&mut self, slot: usize, comparator: &mut Self::FieldComparator) -> Result<()> {
    comparator.bottom = comparator.values[slot];

    // make bounding box(es) to exclude non-competitive hits, but start
    // sampling if we get called way too much: don't make gobs of bounding
    // boxes if comparator hits a worst case order (e.g. backwards distance order)
    if comparator.bottom < f32::MAX as f64
      && (comparator.set_bottom_counter < 1024 || (comparator.set_bottom_counter & 0x3F) == 0x3F)
    {
      let rectangle = XYRectangle::from_point_distance(
        comparator.x as f32,
        comparator.y as f32,
        comparator.bottom as f32,
      )?;
      // pre-encode our box to our integer encoding, so we don't have to decode
      // to `f64` values for uncompetitive hits. This has some cost!
      comparator.min_x = XYEncodingUtils::encode(rectangle.min_x)?;
      comparator.max_x = XYEncodingUtils::encode(rectangle.max_x)?;
      comparator.min_y = XYEncodingUtils::encode(rectangle.min_y)?;
      comparator.max_y = XYEncodingUtils::encode(rectangle.max_y)?;
    }

    comparator.set_bottom_counter += 1;
    Ok(())
  }

  fn compare_bottom<S>(
    &mut self,
    doc: i32,
    _scorer: &mut S,
    comparator: &mut Self::FieldComparator,
  ) -> Result<i32>
  where
    S: Scorable + ?Sized,
  {
    if doc > self.current_docs.doc_id() {
      self.current_docs.advance(doc)?;
    }
    if doc < self.current_docs.doc_id() {
      return Ok(CoreHelper::compare_f64(comparator.bottom, f64::INFINITY).to_int());
    }

    self.set_values(comparator)?;

    let num_values = self.current_docs.doc_value_count()? as usize;

    let mut cmp = -1;
    for i in 0..num_values {
      let encoded = comparator.current_values[i];

      // test bounding box
      let x_bits = (encoded >> 32) as i32;
      if x_bits < comparator.min_x || x_bits > comparator.max_x {
        continue;
      }
      let y_bits = (encoded & 0xFFFF_FFFF) as i32;
      if y_bits < comparator.min_y || y_bits > comparator.max_y {
        continue;
      }

      // only compute actual distance if its inside "competitive bounding box"
      let doc_x = XYEncodingUtils::decode(x_bits);
      let doc_y = XYEncodingUtils::decode(y_bits);
      let diff_x = comparator.x - doc_x as f64;
      let diff_y = comparator.y - doc_y as f64;
      let distance = (diff_x * diff_x + diff_y * diff_y).sqrt();

      cmp = cmp.max(CoreHelper::compare_f64(comparator.bottom, distance).to_int());
      // once we compete in the PQ, no need to continue.
      if cmp > 0 {
        return Ok(cmp);
      }
    }

    Ok(cmp)
  }

  fn compare_top<S>(
    &mut self,
    doc: i32,
    _scorer: &mut S,
    comparator: &mut Self::FieldComparator,
  ) -> Result<i32>
  where
    S: Scorable + ?Sized,
  {
    let v = self.sort_key(doc, comparator)?;
    Ok(CoreHelper::compare_f64(comparator.top_value, v).to_int())
  }

  fn copy<S>(
    &mut self,
    slot: usize,
    doc: i32,
    _scorer: &mut S,
    comparator: &mut Self::FieldComparator,
  ) -> Result<()>
  where
    S: Scorable + ?Sized,
  {
    comparator.values[slot] = self.sort_key(doc, comparator)?;
    Ok(())
  }

  fn set_scorer<S>(
    &mut self,
    _scorer: &mut S,
    _comparator: &mut Self::FieldComparator,
  ) -> Result<()>
  where
    S: Scorable + ?Sized,
  {
    Ok(())
  }

  type DocIdSetIteratorRef<'a>
    = DummyDISI
  where
    Self: 'a;
}
