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
use crate::core::document::lat_lon_doc_values_field::LatLonDocValuesField;
use crate::core::geo::geo_encoding_utils::GeoEncodingUtils;
use crate::core::geo::rectangle::Rectangle;
use crate::core::index::doc_values::{DocValues, SortedNumeric};
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::sorted_numeric_doc_values::SortedNumericDocValues;
use crate::core::search::dummy::dummy_disi::DummyDISI;
use crate::core::search::field_comparator::FieldComparator;
use crate::core::search::leaf_field_comparator::LeafFieldComparator;
use crate::core::search::scorable::Scorable;
use crate::core::util::ToInt;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::core_helper::CoreHelper;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::sloppy_math::SloppyMath;

/// Compares documents by distance from an origin point
///
/// When the least competitive item on the priority queue changes (`set_bottom`), we recompute a
/// bounding box representing competitive distance to the top-N. Then in `compare_bottom`, we can
/// quickly reject hits based on bounding box alone without computing distance for every element.
pub struct LatLonPointDistanceComparator {
  field: String,
  latitude: f64,
  longitude: f64,

  values: Vec<f64>,
  bottom: f64,
  top_value: f64,

  // current bounding box(es) for the bottom distance on the PQ.
  // these are pre-encoded with LatLonPoint's encoding and
  // used to exclude uncompetitive hits faster.
  min_lon: i32,
  max_lon: i32,
  min_lat: i32,
  max_lat: i32,

  // second set of longitude ranges to check (for cross-dateline case)
  min_lon2: i32,

  // the number of times setBottom has been called (adversary protection)
  set_bottom_counter: i32,

  current_values: smallvec::SmallVec<[i64; 4]>,
  values_doc_id: i32,
}

impl LatLonPointDistanceComparator {
  pub(crate) fn new(field: String, latitude: f64, longitude: f64, num_hits: usize) -> Self {
    Self {
      field,
      latitude,
      longitude,
      values: vec![0.0; num_hits],
      bottom: 0.0,
      top_value: 0.0,
      min_lon: i32::MIN,
      max_lon: i32::MAX,
      min_lat: i32::MIN,
      max_lat: i32::MAX,
      min_lon2: i32::MAX,
      set_bottom_counter: 0,
      current_values: smallvec::smallvec![0; 4],
      values_doc_id: -1,
    }
  }

  fn haversin2(partial: f64) -> f64 {
    if partial.is_infinite() {
      partial
    } else {
      SloppyMath::haversin_meters_from_sort_key(partial)
    }
  }
}

impl FieldComparator for LatLonPointDistanceComparator {
  type V = f64;

  fn compare(&self, slot1: usize, slot2: usize) -> i32 {
    CoreHelper::compare_f64(self.values[slot1], self.values[slot2]).to_int()
  }

  fn set_top_value(&mut self, value: Self::V) -> Result<()> {
    self.top_value = value;
    Ok(())
  }

  fn value(&self, slot: usize) -> Option<Self::V> {
    Some(Self::haversin2(self.values[slot]))
  }

  fn compare_values(&self, first: Option<&Self::V>, second: Option<&Self::V>) -> Result<i32> {
    Ok(match (first, second) {
      (None, None) => 0,
      (None, Some(_)) => -1,
      (Some(_), None) => 1,
      (Some(first), Some(second)) => CoreHelper::compare_f64(*first, *second).to_int(),
    })
  }

  type LeafFieldComparator<LR>
    = LatLonPointDistanceLeafComparator<SortedNumeric<LR>>
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
      LatLonDocValuesField::check_compatible(info.as_ref())?;
    }

    let current_docs = DocValues::get_sorted_numeric(reader, &self.field)?;
    self.values_doc_id = -1;
    Ok(LatLonPointDistanceLeafComparator { current_docs })
  }
}

pub struct LatLonPointDistanceLeafComparator<DVS> {
  current_docs: DVS,
}

impl<DVS> LatLonPointDistanceLeafComparator<DVS>
where
  DVS: SortedNumericDocValues,
{
  fn set_values(
    &mut self,
    comparator: &mut <LatLonPointDistanceLeafComparator<DVS> as LeafFieldComparator>::FieldComparator,
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
    comparator: &mut <LatLonPointDistanceLeafComparator<DVS> as LeafFieldComparator>::FieldComparator,
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
        let doc_latitude = GeoEncodingUtils::decode_latitude((encoded >> 32) as i32);
        let doc_longitude = GeoEncodingUtils::decode_longitude((encoded & 0xFFFF_FFFF) as i32);
        min_value = CoreHelper::min_f64(
          min_value,
          SloppyMath::haversin_sort_key(
            comparator.latitude,
            comparator.longitude,
            doc_latitude,
            doc_longitude,
          ),
        );
      }
    }

    Ok(min_value)
  }
}

impl<DVS> LeafFieldComparator for LatLonPointDistanceLeafComparator<DVS>
where
  DVS: SortedNumericDocValues,
{
  type FieldComparator = LatLonPointDistanceComparator;

  fn set_bottom(&mut self, slot: usize, comparator: &mut Self::FieldComparator) -> Result<()> {
    comparator.bottom = comparator.values[slot];

    // make bounding box(es) to exclude non-competitive hits, but start
    // sampling if we get called way too much: don't make gobs of bounding
    // boxes if comparator hits a worst case order (e.g. backwards distance order)
    if comparator.set_bottom_counter < 1024 || (comparator.set_bottom_counter & 0x3F) == 0x3F {
      let box_ = Rectangle::from_point_distance(
        comparator.latitude,
        comparator.longitude,
        LatLonPointDistanceComparator::haversin2(comparator.bottom),
      )?;

      // pre-encode our box to our integer encoding, so we don't have to decode
      // to `f64` values for uncompetitive hits. This has some cost!
      comparator.min_lat = GeoEncodingUtils::encode_latitude(box_.min_lat)?;
      comparator.max_lat = GeoEncodingUtils::encode_latitude(box_.max_lat)?;
      if box_.crosses_dateline() {
        // box1
        comparator.min_lon = i32::MIN;
        comparator.max_lon = GeoEncodingUtils::encode_longitude(box_.max_lon)?;
        // box2
        comparator.min_lon2 = GeoEncodingUtils::encode_longitude(box_.min_lon)?;
      } else {
        comparator.min_lon = GeoEncodingUtils::encode_longitude(box_.min_lon)?;
        comparator.max_lon = GeoEncodingUtils::encode_longitude(box_.max_lon)?;
        // disable box2
        comparator.min_lon2 = i32::MAX;
      }
    }

    comparator.set_bottom_counter = comparator.set_bottom_counter.wrapping_add(1);
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
      let latitude_bits = (encoded >> 32) as i32;
      if latitude_bits < comparator.min_lat || latitude_bits > comparator.max_lat {
        continue;
      }
      let longitude_bits = (encoded & 0xFFFF_FFFF) as i32;
      if (longitude_bits < comparator.min_lon || longitude_bits > comparator.max_lon)
        && longitude_bits < comparator.min_lon2
      {
        continue;
      }

      // only compute actual distance if its inside "competitive bounding box"
      let doc_latitude = GeoEncodingUtils::decode_latitude(latitude_bits);
      let doc_longitude = GeoEncodingUtils::decode_longitude(longitude_bits);
      cmp = cmp.max(
        CoreHelper::compare_f64(
          comparator.bottom,
          SloppyMath::haversin_sort_key(
            comparator.latitude,
            comparator.longitude,
            doc_latitude,
            doc_longitude,
          ),
        )
        .to_int(),
      );

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
    Ok(
      CoreHelper::compare_f64(
        comparator.top_value,
        LatLonPointDistanceComparator::haversin2(v),
      )
      .to_int(),
    )
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
