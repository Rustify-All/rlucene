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
use crate::core::codecs::{
  CodecBinaryDocValues, CodecDocValuesProducer, CodecDocValuesSkipper, CodecNumericDocValues,
  CodecSortedDocValues, CodecSortedNumericDocValues, CodecSortedSetDocValues,
};
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_doc_values::SegmentDocValues;
use crate::core::store::directory::Directory;
use crate::core::store::index_input::IndexInput;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{CaughtResultExt, LuceneError, Result};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

/// Encapsulates multiple producers when there are docvalues updates as one producer
pub struct SegmentDocValuesProducer<I>
where
  I: IndexInput,
{
  // Own each producer Arc once in this Vec and store its stable index in the field map.
  dv_producers_by_field: HashMap<i32, usize>,
  dv_producers: smallvec::SmallVec<[Arc<CodecDocValuesProducer<I>>; 2]>,
  pub(crate) dv_gens: smallvec::SmallVec<[i64; 2]>,
}
impl<I> SegmentDocValuesProducer<I>
where
  I: IndexInput,
{
  pub(crate) fn new<D, D1>(
    si: &SegmentCommitInfo<D>,
    dir: Option<&D1>,
    core_infos: &Arc<FieldInfos>,
    all_infos: &FieldInfos,
    seg_doc_values: &SegmentDocValues<I>,
  ) -> Result<Self>
  where
    D: Directory<IndexInput = I>,
    D1: Directory<IndexInput = I>,
  {
    let num_dv_fields = if all_infos.has_doc_values() {
      all_infos
        .iter()
        .filter(|fi| *fi.get_doc_values_type() != DocValuesType::None)
        .count()
    } else {
      0
    };
    let mut dv_producers_by_field = HashMap::with_capacity(num_dv_fields);
    let num_dv_gens = all_infos
      .iter()
      .filter(|fi| {
        *fi.get_doc_values_type() != DocValuesType::None && fi.get_doc_values_gen() != -1
      })
      .count()
      + usize::from(all_infos.iter().any(|fi| {
        *fi.get_doc_values_type() != DocValuesType::None && fi.get_doc_values_gen() == -1
      }));
    let mut dv_producers = smallvec::SmallVec::with_capacity(num_dv_gens);
    let mut producer_indices = (num_dv_gens > 2).then(|| HashMap::with_capacity(num_dv_gens));
    let mut dv_gens = smallvec::SmallVec::with_capacity(num_dv_gens);

    let mut base_producer_index = None;

    let mut result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      for fi in all_infos {
        if *fi.get_doc_values_type() == DocValuesType::None {
          continue;
        }
        let doc_values_gen = fi.get_doc_values_gen();

        if doc_values_gen == -1 {
          let producer_index = if let Some(producer_index) = base_producer_index {
            producer_index
          } else {
            // the base producer gets the original fieldinfos it wrote
            let producer = seg_doc_values.get_doc_values_producer(
              doc_values_gen,
              si,
              dir,
              core_infos.clone(),
            )?;
            dv_gens.push(doc_values_gen);
            let producer_index =
              Self::add_unique_producer(&mut dv_producers, &mut producer_indices, producer);
            base_producer_index = Some(producer_index);
            producer_index
          };
          dv_producers_by_field.insert(fi.number, producer_index);
        } else {
          debug_assert!(!dv_gens.contains(&doc_values_gen));
          // otherwise, producer sees only the one fieldinfo it wrote
          let field_infos = Arc::new(FieldInfos::new(vec![fi.clone()])?);
          let dvp = seg_doc_values.get_doc_values_producer(doc_values_gen, si, dir, field_infos)?;
          dv_gens.push(doc_values_gen);
          let producer_index =
            Self::add_unique_producer(&mut dv_producers, &mut producer_indices, dvp);
          dv_producers_by_field.insert(fi.number, producer_index);
        }
      }
      Ok(())
    }));

    if !matches!(&result, Ok(Ok(()))) {
      let dec_ref_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        seg_doc_values.dec_ref(&dv_gens)
      }));
      result.add_suppressed(
        dec_ref_result,
        "panic while releasing doc values producers after initialization failure",
      );
    }
    unwrap_caught_result!(result)?;

    Ok(Self {
      dv_producers_by_field,
      dv_producers,
      dv_gens,
    })
  }

  fn add_unique_producer(
    producers: &mut smallvec::SmallVec<[Arc<CodecDocValuesProducer<I>>; 2]>,
    producer_indices: &mut Option<HashMap<*const CodecDocValuesProducer<I>, usize>>,
    producer: Arc<CodecDocValuesProducer<I>>,
  ) -> usize {
    let Some(producer_indices) = producer_indices else {
      if let Some(index) = producers
        .iter()
        .position(|existing| Arc::ptr_eq(existing, &producer))
      {
        return index;
      }
      let index = producers.len();
      producers.push(producer);
      return index;
    };
    let identity = Arc::as_ptr(&producer);
    match producer_indices.entry(identity) {
      std::collections::hash_map::Entry::Occupied(entry) => *entry.get(),
      std::collections::hash_map::Entry::Vacant(entry) => {
        let producer_index = producers.len();
        producers.push(producer);
        entry.insert(producer_index);
        producer_index
      },
    }
  }

  fn producer_for_field(&self, field: &FieldInfo) -> Result<&CodecDocValuesProducer<I>> {
    let producer_index = self
      .dv_producers_by_field
      .get(&field.number)
      .ok_or_else(|| {
        LuceneError::illegal_state(format!(
          "missing doc values producer for field {} ({})",
          field.name, field.number
        ))
      })?;
    self
      .dv_producers
      .get(*producer_index)
      .map(Arc::as_ref)
      .ok_or_else(|| {
        LuceneError::illegal_state(format!(
          "invalid doc values producer index {producer_index} for field {} ({})",
          field.name, field.number
        ))
      })
  }
}

impl<I> CloseableRef for SegmentDocValuesProducer<I>
where
  I: IndexInput,
{
  fn close(&self) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }
}

impl<I> DocValuesProducer for SegmentDocValuesProducer<I>
where
  I: IndexInput,
{
  type NumericDocValues = CodecNumericDocValues<I>;

  fn get_numeric(&self, field: &Arc<FieldInfo>) -> Result<Self::NumericDocValues> {
    self.producer_for_field(field)?.get_numeric(field)
  }

  type BinaryDocValues = CodecBinaryDocValues<I>;

  fn get_binary(&self, field: &Arc<FieldInfo>) -> Result<Self::BinaryDocValues> {
    self.producer_for_field(field)?.get_binary(field)
  }

  type SortedDocValues = CodecSortedDocValues<I>;

  fn get_sorted(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedDocValues> {
    self.producer_for_field(field)?.get_sorted(field)
  }

  type SortedNumericDocValues = CodecSortedNumericDocValues<I>;

  fn get_sorted_numeric(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedNumericDocValues> {
    self.producer_for_field(field)?.get_sorted_numeric(field)
  }

  type SortedSetDocValues = CodecSortedSetDocValues<I>;

  fn get_sorted_set(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedSetDocValues> {
    self.producer_for_field(field)?.get_sorted_set(field)
  }

  type DocValuesSkipper = CodecDocValuesSkipper<I>;

  fn get_skipper(&self, field: &Arc<FieldInfo>) -> Result<Option<Self::DocValuesSkipper>> {
    self.producer_for_field(field)?.get_skipper(field)
  }

  fn check_integrity(&self) -> Result<()> {
    for dv_producer in &self.dv_producers {
      dv_producer.check_integrity()?;
    }
    Ok(())
  }
}

impl<I> Display for SegmentDocValuesProducer<I>
where
  I: IndexInput,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "SegmentDocValuesProducer (producers={})",
      self.dv_producers.len()
    )
  }
}
