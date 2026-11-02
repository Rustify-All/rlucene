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
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::ordinal_map::{OrdinalMap, SegmentToGlobalOrds};
use crate::core::index::sorted_doc_values::{SortedDocValues, SortedDocValuesVisitor};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::sort_field::MissingValueEnum;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::long_values::LongValues;
use crate::core::util::numeric_utils::NumericUtils;
use crate::core::util::packed::PackedInts;
use crate::core::util::{CoreHelper, ToInt};
use std::borrow::Cow;
/// Handles how documents should be sorted in an index, both within a segment
/// and between segments.
pub trait IndexSorter {
  fn get_provider_name(&self) -> &str;

  type ComparableProvider<LR>: ComparableProvider
  where
    LR: LeafReader;
  fn get_comparable_providers<LR>(
    &self,
    readers: &[LR],
  ) -> Result<Vec<Self::ComparableProvider<LR>>>
  where
    LR: LeafReader,
  {
    let missing_value = self.get_missing_value();
    let ordinal_map = self.get_ordinal_map(readers)?;
    let mut providers = Vec::with_capacity(readers.len());
    for (reader_index, reader) in readers.iter().enumerate() {
      providers.push(self.get_comparable_providers_per_reader(
        reader,
        reader_index,
        &missing_value,
        ordinal_map.as_ref(),
      )?);
    }
    Ok(providers)
  }
  /// Visits the provider vector before its concrete provider type is erased.
  fn visit_comparable_providers<LR, V>(&self, readers: &[LR], visitor: V) -> Result<V::Output>
  where
    LR: LeafReader,
    V: ComparableProvidersVisitor,
  {
    visitor.visit(self.get_comparable_providers(readers)?)
  }

  fn get_comparable_providers_per_reader<LR>(
    &self,
    _reader: &LR,
    _reader_index: usize,
    _formated_missing_value: &MissingValueEnum,
    _ordinal_map: Option<&OrdinalMap>,
  ) -> Result<Self::ComparableProvider<LR>>
  where
    LR: LeafReader,
  {
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_ordinal_map<LR>(&self, _readers: &[LR]) -> Result<Option<OrdinalMap>>
  where
    LR: LeafReader,
  {
    Ok(None)
  }
  fn get_missing_value(&self) -> MissingValueEnum;
  type DocComparator: DocComparator;
  /// Get a comparator that determines the sort order of documents within a single reader.
  ///
  /// **NB**: We cannot simply use the [`FieldComparator`](crate::core::search::field_comparator::FieldComparator) API because it requires
  /// document IDs to be provided in-order. The default implementations allocate
  /// an array of size `max_doc` to store native values for comparison, but:
  ///
  /// 1. They are transient, only living while sorting a single segment.
  /// 2. In typical index-sorting scenarios, they are only used to sort newly
  ///    flushed segments, which are usually much smaller than merged segments.
  ///
  /// # Parameters
  ///
  /// - `reader`: the reader whose documents should be sorted.
  /// - `max_doc`: the number of documents in the reader.
  fn get_doc_comparator<LR>(&self, leaf_reader: &LR, max_doc: i32) -> Result<Self::DocComparator>
  where
    LR: LeafReader;
}

struct IndexSortOrdValues<'a>(&'a mut [i32]);

impl SortedDocValuesVisitor for IndexSortOrdValues<'_> {
  type Output = Result<()>;

  fn visit<D: SortedDocValues>(self, values: &mut D) -> Self::Output {
    loop {
      let doc_id = values.next_doc()?;
      if doc_id == NO_MORE_DOCS {
        break;
      }
      self.0[doc_id as usize] = values.ord_value()?;
    }
    Ok(())
  }
}

// DoubleSorter
/// Sorts documents based on `f64` values from a [`NumericDocValues`] instance.
pub struct DoubleSorter<NP> {
  provider_name: Cow<'static, str>,
  missing_value: Option<f64>,
  reverse_mul: i32,
  values_provider: NP,
}
impl<NP> DoubleSorter<NP> {
  pub fn new<FName>(
    provider_name: FName,
    missing_value: Option<MissingValueEnum>,
    reverse: bool,
    values_provider: NP,
  ) -> Result<Self>
  where
    FName: Into<Cow<'static, str>>,
  {
    let provider_name = provider_name.into();
    let missing_value = if let Some(mv) = missing_value {
      match mv {
        MissingValueEnum::Double(value) => Some(value),
        _ => {
          return Err(LuceneError::illegal_state(
            "Missing value type mismatch for Double.",
          ));
        },
      }
    } else {
      None
    };
    Ok(Self {
      provider_name,
      missing_value,
      reverse_mul: if reverse { -1 } else { 1 },
      values_provider,
    })
  }
}
impl<NP> IndexSorter for DoubleSorter<NP>
where
  NP: NumericDocValuesProvider,
{
  fn get_provider_name(&self) -> &str {
    &self.provider_name
  }

  type ComparableProvider<LR>
    = DoubleComparableProvider<ProviderNumeric<NP, LR>>
  where
    LR: LeafReader;

  fn get_comparable_providers_per_reader<LR>(
    &self,
    reader: &LR,
    _reader_index: usize,
    formated_missing_value: &MissingValueEnum,
    _ordinal_map: Option<&OrdinalMap>,
  ) -> Result<Self::ComparableProvider<LR>>
  where
    LR: LeafReader,
  {
    let missing_value_bits = match formated_missing_value {
      MissingValueEnum::Long(value) => value,
      _ => {
        return Err(LuceneError::illegal_state(
          "formated Missing value type mismatch for i64.",
        ));
      },
    };
    let values = self.values_provider.get(reader)?;
    Ok(DoubleComparableProvider::new(values, *missing_value_bits))
  }

  fn get_missing_value(&self) -> MissingValueEnum {
    let value = self.missing_value.unwrap_or(0.0);
    let bits = if value.is_nan() {
      BitUtil::DOUBLE_NAN_BITS
    } else {
      value.to_bits()
    };
    (bits as i64).into()
  }

  type DocComparator = DocComparatorImplDouble;

  fn get_doc_comparator<LR>(&self, leaf_reader: &LR, max_doc: i32) -> Result<Self::DocComparator>
  where
    LR: LeafReader,
  {
    let mut dvs = self.values_provider.get(leaf_reader)?;
    let mut values = vec![self.missing_value.unwrap_or(0f64); max_doc as usize];

    loop {
      let doc_id = dvs.next_doc()?;
      if doc_id == NO_MORE_DOCS {
        break;
      }
      values[doc_id as usize] = f64::from_bits(dvs.long_value()? as u64);
    }
    Ok(DocComparatorImplDouble::new(values, self.reverse_mul))
  }
}
pub struct DocComparatorImplDouble {
  values: Box<[f64]>,
  reverse_mul: i32,
}
impl DocComparatorImplDouble {
  pub fn new(values: Vec<f64>, reverse_mul: i32) -> Self {
    Self {
      values: values.into_boxed_slice(),
      reverse_mul,
    }
  }
}
impl DocComparator for DocComparatorImplDouble {
  fn compare(&self, doc_id1: usize, doc_id2: usize) -> i32 {
    self.reverse_mul
      * NumericUtils::double_to_sortable_long(self.values[doc_id1])
        .cmp(&NumericUtils::double_to_sortable_long(self.values[doc_id2]))
        .to_int()
  }
}
pub struct DoubleComparableProvider<N> {
  values: N,
  missing_value_bits: i64,
}

impl<N> DoubleComparableProvider<N> {
  pub fn new(values: N, missing_value_bits: i64) -> Self {
    Self {
      values,
      missing_value_bits,
    }
  }
}

impl<N> ComparableProvider for DoubleComparableProvider<N>
where
  N: NumericDocValues,
{
  fn get_as_comparable_long(&mut self, doc_id: i32) -> Result<i64> {
    let v = if self.values.advance_exact(doc_id)? {
      self.values.long_value()?
    } else {
      self.missing_value_bits
    };
    Ok(NumericUtils::sortable_double_bits(v))
  }
}

// IntSorter
/// Sorts documents based on integer values from a NumericDocValues instance  */
pub struct IntSorter<NP> {
  provider_name: Cow<'static, str>,
  missing_value: Option<i32>,
  reverse_mul: i32,
  values_provider: NP,
}
impl<NP> IntSorter<NP> {
  pub fn new<FName>(
    provider_name: FName,
    missing_value: Option<MissingValueEnum>,
    reverse: bool,
    values_provider: NP,
  ) -> Result<Self>
  where
    FName: Into<Cow<'static, str>>,
  {
    let provider_name = provider_name.into();
    let missing_value = if let Some(mv) = missing_value {
      match mv {
        MissingValueEnum::Int(value) => Some(value),
        _ => {
          return Err(LuceneError::illegal_state(
            "Missing value type mismatch for INT.",
          ));
        },
      }
    } else {
      None
    };
    Ok(Self {
      provider_name,
      missing_value,
      reverse_mul: if reverse { -1 } else { 1 },
      values_provider,
    })
  }
}

impl<NP> IndexSorter for IntSorter<NP>
where
  NP: NumericDocValuesProvider,
{
  fn get_provider_name(&self) -> &str {
    &self.provider_name
  }

  type ComparableProvider<LR>
    = IntComparableProvider<ProviderNumeric<NP, LR>>
  where
    LR: LeafReader;

  fn get_comparable_providers_per_reader<LR>(
    &self,
    reader: &LR,
    _reader_index: usize,
    formated_missing_value: &MissingValueEnum,
    _ordinal_map: Option<&OrdinalMap>,
  ) -> Result<Self::ComparableProvider<LR>>
  where
    LR: LeafReader,
  {
    let missing_value = match formated_missing_value {
      MissingValueEnum::Long(value) => value,
      _ => {
        return Err(LuceneError::illegal_state(
          "formated Missing value type mismatch for i64.",
        ));
      },
    };
    let values = self.values_provider.get(reader)?;
    Ok(IntComparableProvider::new(values, *missing_value))
  }

  fn get_missing_value(&self) -> MissingValueEnum {
    (self.missing_value.unwrap_or(0) as i64).into()
  }

  type DocComparator = DocComparatorImplInt;

  fn get_doc_comparator<LR>(&self, leaf_reader: &LR, max_doc: i32) -> Result<Self::DocComparator>
  where
    LR: LeafReader,
  {
    let mut dvs = self.values_provider.get(leaf_reader)?;
    let mut values = vec![self.missing_value.unwrap_or(0i32); max_doc as usize];
    loop {
      let doc_id = dvs.next_doc()?;
      if doc_id == NO_MORE_DOCS {
        break;
      }
      values[doc_id as usize] = dvs.long_value()? as i32;
    }
    Ok(DocComparatorImplInt::new(values, self.reverse_mul))
  }
}
pub struct DocComparatorImplInt {
  values: Box<[i32]>,
  reverse_mul: i32,
}
impl DocComparatorImplInt {
  pub fn new(values: Vec<i32>, reverse_mul: i32) -> Self {
    Self {
      values: values.into_boxed_slice(),
      reverse_mul,
    }
  }
}
impl DocComparator for DocComparatorImplInt {
  fn compare(&self, doc_id1: usize, doc_id2: usize) -> i32 {
    self.reverse_mul * self.values[doc_id1].cmp(&self.values[doc_id2]).to_int()
  }
}
pub struct IntComparableProvider<N> {
  values: N,
  missing_value: i64,
}
impl<N> IntComparableProvider<N> {
  pub fn new(values: N, missing_value: i64) -> Self {
    Self {
      values,
      missing_value,
    }
  }
}
impl<N> ComparableProvider for IntComparableProvider<N>
where
  N: NumericDocValues,
{
  fn get_as_comparable_long(&mut self, doc_id: i32) -> Result<i64> {
    if self.values.advance_exact(doc_id)? {
      Ok(self.values.long_value()?)
    } else {
      Ok(self.missing_value)
    }
  }
}

// LongSorter
/// Sorts documents based on `i64` values from a [`NumericDocValues`] instance.
pub struct LongSorter<NP> {
  provider_name: Cow<'static, str>,
  missing_value: Option<i64>,
  reverse_mul: i32,
  values_provider: NP,
}
impl<NP> LongSorter<NP> {
  pub fn new<FName>(
    provider_name: FName,
    missing_value: Option<MissingValueEnum>,
    reverse: bool,
    values_provider: NP,
  ) -> Result<Self>
  where
    FName: Into<Cow<'static, str>>,
  {
    let provider_name = provider_name.into();
    let missing_value = if let Some(mv) = missing_value {
      match mv {
        MissingValueEnum::Long(value) => Some(value),
        _ => {
          return Err(LuceneError::illegal_state(
            "Missing value type mismatch for Long.",
          ));
        },
      }
    } else {
      None
    };
    Ok(Self {
      provider_name,
      missing_value,
      reverse_mul: if reverse { -1 } else { 1 },
      values_provider,
    })
  }
}

impl<NP> IndexSorter for LongSorter<NP>
where
  NP: NumericDocValuesProvider,
{
  fn get_provider_name(&self) -> &str {
    &self.provider_name
  }

  type ComparableProvider<LR>
    = LongComparableProvider<ProviderNumeric<NP, LR>>
  where
    LR: LeafReader;

  fn get_comparable_providers_per_reader<LR>(
    &self,
    reader: &LR,
    _reader_index: usize,
    formated_missing_value: &MissingValueEnum,
    _ordinal_map: Option<&OrdinalMap>,
  ) -> Result<Self::ComparableProvider<LR>>
  where
    LR: LeafReader,
  {
    let missing_value = match formated_missing_value {
      MissingValueEnum::Long(value) => value,
      _ => {
        return Err(LuceneError::illegal_state(
          "formated Missing value type mismatch for i64.",
        ));
      },
    };
    let values = self.values_provider.get(reader)?;
    Ok(LongComparableProvider::new(values, *missing_value))
  }

  fn get_missing_value(&self) -> MissingValueEnum {
    (self.missing_value.unwrap_or(0)).into()
  }

  type DocComparator = DocComparatorImplLong;

  fn get_doc_comparator<LR>(&self, leaf_reader: &LR, max_doc: i32) -> Result<Self::DocComparator>
  where
    LR: LeafReader,
  {
    let mut dvs = self.values_provider.get(leaf_reader)?;
    let mut values = vec![self.missing_value.unwrap_or(0i64); max_doc as usize];
    loop {
      let doc_id = dvs.next_doc()?;
      if doc_id == NO_MORE_DOCS {
        break;
      }
      values[doc_id as usize] = dvs.long_value()?;
    }
    Ok(DocComparatorImplLong::new(values, self.reverse_mul))
  }
}

pub struct DocComparatorImplLong {
  values: Box<[i64]>,
  reverse_mul: i32,
}

impl DocComparatorImplLong {
  pub fn new(values: Vec<i64>, reverse_mul: i32) -> Self {
    Self {
      values: values.into_boxed_slice(),
      reverse_mul,
    }
  }
}

impl DocComparator for DocComparatorImplLong {
  fn compare(&self, doc_id1: usize, doc_id2: usize) -> i32 {
    self.reverse_mul * self.values[doc_id1].cmp(&self.values[doc_id2]).to_int()
  }
}
pub struct LongComparableProvider<N> {
  values: N,
  missing_value: i64,
}

impl<N> LongComparableProvider<N> {
  pub fn new(values: N, missing_value: i64) -> Self {
    Self {
      values,
      missing_value,
    }
  }
}

impl<N> ComparableProvider for LongComparableProvider<N>
where
  N: NumericDocValues,
{
  fn get_as_comparable_long(&mut self, doc_id: i32) -> Result<i64> {
    if self.values.advance_exact(doc_id)? {
      Ok(self.values.long_value()?)
    } else {
      Ok(self.missing_value)
    }
  }
}

// FloatSorter
/// Sorts documents based on `f32` values from a [`NumericDocValues`] instance.
pub struct FloatSorter<NP> {
  provider_name: Cow<'static, str>,
  missing_value: Option<f32>,
  reverse_mul: i32,
  values_provider: NP,
}

impl<NP> FloatSorter<NP> {
  pub fn new<FName>(
    provider_name: FName,
    missing_value: Option<MissingValueEnum>,
    reverse: bool,
    values_provider: NP,
  ) -> Result<Self>
  where
    FName: Into<Cow<'static, str>>,
  {
    let provider_name = provider_name.into();
    let missing_value = if let Some(mv) = missing_value {
      match mv {
        MissingValueEnum::Float(value) => Some(value),
        _ => {
          return Err(LuceneError::illegal_state(
            "Missing value type mismatch for Float.",
          ));
        },
      }
    } else {
      None
    };
    Ok(Self {
      provider_name,
      missing_value,
      reverse_mul: if reverse { -1 } else { 1 },
      values_provider,
    })
  }
}

impl<NP> IndexSorter for FloatSorter<NP>
where
  NP: NumericDocValuesProvider,
{
  fn get_provider_name(&self) -> &str {
    &self.provider_name
  }

  type ComparableProvider<LR>
    = FloatComparableProvider<ProviderNumeric<NP, LR>>
  where
    LR: LeafReader;

  fn get_comparable_providers_per_reader<LR>(
    &self,
    reader: &LR,
    _reader_index: usize,
    formated_missing_value: &MissingValueEnum,
    _ordinal_map: Option<&OrdinalMap>,
  ) -> Result<Self::ComparableProvider<LR>>
  where
    LR: LeafReader,
  {
    let missing_value_bits = match formated_missing_value {
      MissingValueEnum::Int(value) => value,
      _ => {
        return Err(LuceneError::illegal_state(
          "formated Missing value type mismatch for i32.",
        ));
      },
    };
    let values = self.values_provider.get(reader)?;
    Ok(FloatComparableProvider::new(values, *missing_value_bits))
  }

  fn get_missing_value(&self) -> MissingValueEnum {
    let value = self.missing_value.unwrap_or(0.0);
    let bits = if value.is_nan() {
      BitUtil::FLOAT_NAN_BITS
    } else {
      value.to_bits()
    };
    (bits as i32).into()
  }

  type DocComparator = DocComparatorImplFloat;

  fn get_doc_comparator<LR>(&self, leaf_reader: &LR, max_doc: i32) -> Result<Self::DocComparator>
  where
    LR: LeafReader,
  {
    let mut dvs = self.values_provider.get(leaf_reader)?;
    let mut values = vec![self.missing_value.unwrap_or(0f32); max_doc as usize];
    loop {
      let doc_id = dvs.next_doc()?;
      if doc_id == NO_MORE_DOCS {
        break;
      }
      let bits = dvs.long_value()? as u32;
      values[doc_id as usize] = f32::from_bits(bits);
    }
    Ok(DocComparatorImplFloat::new(values, self.reverse_mul))
  }
}

pub struct DocComparatorImplFloat {
  values: Box<[f32]>,
  reverse_mul: i32,
}

impl DocComparatorImplFloat {
  pub fn new(values: Vec<f32>, reverse_mul: i32) -> Self {
    Self {
      values: values.into_boxed_slice(),
      reverse_mul,
    }
  }
}

impl DocComparator for DocComparatorImplFloat {
  fn compare(&self, doc_id1: usize, doc_id2: usize) -> i32 {
    let v1 = self.values[doc_id1];
    let v2 = self.values[doc_id2];
    let ord = CoreHelper::compare_f32(v1, v2).to_int();
    self.reverse_mul * ord
  }
}
pub struct FloatComparableProvider<N> {
  values: N,
  missing_value_bits: i32,
}

impl<N> FloatComparableProvider<N> {
  pub fn new(values: N, missing_value_bits: i32) -> Self {
    Self {
      values,
      missing_value_bits,
    }
  }
}

impl<N> ComparableProvider for FloatComparableProvider<N>
where
  N: NumericDocValues,
{
  fn get_as_comparable_long(&mut self, doc_id: i32) -> Result<i64> {
    let v = if self.values.advance_exact(doc_id)? {
      self.values.long_value()? as i32
    } else {
      self.missing_value_bits
    };
    Ok(NumericUtils::sortable_float_bits(v) as i64)
  }
}

// StringSorter
/// Sorts documents based on `i16` values from a [`NumericDocValues`] instance.
pub struct StringSorter<SP> {
  provider_name: Cow<'static, str>,
  missing_value: Option<MissingValueEnum>,
  reverse_mul: i32,
  values_provider: SP,
}

impl<SP> StringSorter<SP> {
  pub fn new<FName>(
    provider_name: FName,
    missing_value: Option<MissingValueEnum>,
    reverse: bool,
    values_provider: SP,
  ) -> Self
  where
    FName: Into<Cow<'static, str>>,
  {
    let provider_name = provider_name.into();
    Self {
      provider_name,
      missing_value,
      reverse_mul: if reverse { -1 } else { 1 },
      values_provider,
    }
  }
}

impl<SP> IndexSorter for StringSorter<SP>
where
  SP: SortedDocValuesProvider,
{
  fn get_provider_name(&self) -> &str {
    &self.provider_name
  }

  type ComparableProvider<LR>
    = StringComparableProvider<ProviderString<SP, LR>>
  where
    LR: LeafReader;

  fn get_comparable_providers<LR>(
    &self,
    readers: &[LR],
  ) -> Result<Vec<Self::ComparableProvider<LR>>>
  where
    LR: LeafReader,
  {
    let mut values = Vec::with_capacity(readers.len());
    for reader in readers {
      values.push(self.values_provider.get(reader)?);
    }
    let ordinal_map = OrdinalMap::build_from_sorted(None, &mut values, PackedInts::DEFAULT)?;
    let missing_ord = match self.missing_value {
      Some(MissingValueEnum::StringLast) => i32::MAX,
      _ => i32::MIN,
    };
    let mut providers = Vec::with_capacity(readers.len());
    for (reader_index, reader_values) in values.into_iter().enumerate() {
      providers.push(StringComparableProvider {
        reader_values,
        global_ords: ordinal_map.get_global_ords(reader_index).clone(),
        missing_ord,
      });
    }
    Ok(providers)
  }

  fn get_comparable_providers_per_reader<LR>(
    &self,
    reader: &LR,
    reader_index: usize,
    formated_missing_value: &MissingValueEnum,
    ordinal_map: Option<&OrdinalMap>,
  ) -> Result<Self::ComparableProvider<LR>>
  where
    LR: LeafReader,
  {
    let missing_ord = match formated_missing_value {
      MissingValueEnum::Int(value) => value,
      _ => {
        return Err(LuceneError::illegal_state(
          "formated Missing value type mismatch for i32.",
        ));
      },
    };
    match ordinal_map {
      Some(omap) => {
        let global_ords = omap.get_global_ords(reader_index);
        let reader_values = self.values_provider.get(reader)?;
        Ok(StringComparableProvider {
          reader_values,
          global_ords: global_ords.clone(),
          missing_ord: *missing_ord,
        })
      },
      None => Err(LuceneError::illegal_state(
        "ordinal_map is required for StringSorter.",
      )),
    }
  }

  fn get_ordinal_map<LR>(&self, readers: &[LR]) -> Result<Option<OrdinalMap>>
  where
    LR: LeafReader,
  {
    let mut values = Vec::with_capacity(readers.len());
    for reader in readers {
      values.push(self.values_provider.get(reader)?);
    }
    Ok(Some(OrdinalMap::build_from_sorted(
      None,
      values.as_mut(),
      PackedInts::DEFAULT,
    )?))
  }

  fn get_missing_value(&self) -> MissingValueEnum {
    match self.missing_value {
      Some(MissingValueEnum::StringLast) => i32::MAX.into(),
      _ => i32::MIN.into(),
    }
  }

  type DocComparator = DocComparatorImplString;

  fn get_doc_comparator<LR>(&self, leaf_reader: &LR, max_doc: i32) -> Result<Self::DocComparator>
  where
    LR: LeafReader,
  {
    let mut sorted = self.values_provider.get(leaf_reader)?;
    let missing_ord = match self.missing_value {
      Some(MissingValueEnum::StringLast) => i32::MAX,
      _ => i32::MIN,
    };

    let mut ords = vec![missing_ord; max_doc as usize];
    sorted.visit(IndexSortOrdValues(&mut ords))?;
    Ok(DocComparatorImplString::new(ords, self.reverse_mul))
  }
}
pub struct StringComparableProvider<SDV> {
  reader_values: SDV,
  global_ords: SegmentToGlobalOrds,
  missing_ord: i32,
}

impl<SDV> ComparableProvider for StringComparableProvider<SDV>
where
  SDV: SortedDocValues,
{
  fn get_as_comparable_long(&mut self, doc_id: i32) -> Result<i64> {
    if self.reader_values.advance_exact(doc_id)? {
      let seg_ord = self.reader_values.ord_value()?;
      Ok(self.global_ords.get(seg_ord as usize)?)
    } else {
      Ok(self.missing_ord as i64)
    }
  }
}

pub struct DocComparatorImplString {
  ords: Box<[i32]>,
  reverse_mul: i32,
}

impl DocComparatorImplString {
  pub fn new(ords: Vec<i32>, reverse_mul: i32) -> Self {
    Self {
      ords: ords.into_boxed_slice(),
      reverse_mul,
    }
  }
}

impl DocComparator for DocComparatorImplString {
  fn compare(&self, doc_id1: usize, doc_id2: usize) -> i32 {
    let o1 = self.ords[doc_id1];
    let o2 = self.ords[doc_id2];
    let cmp = o1.cmp(&o2).to_int();
    self.reverse_mul * cmp
  }
}

/// Used for sorting documents across segments
pub trait ComparableProvider {
  /// Returns an `i64` whose natural ordering matches the document-ID ordering for the comparator.
  fn get_as_comparable_long(&mut self, doc_id: i32) -> Result<i64>;
}
/// Receives comparable providers with the sorter's concrete provider type.
pub trait ComparableProvidersVisitor {
  type Output;
  fn visit<CP: ComparableProvider>(self, providers: Vec<CP>) -> Result<Self::Output>;
}

macro_rules! either_comparable_provider {
    ($vis:vis $name:ident { $( $Variant:ident : $T:ident ),+ $(,)? }) => {
        $vis enum $name<$( $T ),+> {
            $( $Variant($T), )+
        }

        impl<$( $T ),+> ComparableProvider for $name<$( $T ),+>
        where
            $( $T: ComparableProvider ),+
        {
            #[inline]
            fn get_as_comparable_long(&mut self, doc_id: i32) -> Result<i64> {
                match self {
                    $( Self::$Variant(inner) => inner.get_as_comparable_long(doc_id), )+
                }
            }
        }
    };
}
either_comparable_provider!(pub ComparableProviderEnum2 { A1: A, B1: B});
pub enum ComparableProviderEnum5<N, S> {
  Int(IntComparableProvider<N>),
  Long(LongComparableProvider<N>),
  Float(FloatComparableProvider<N>),
  Double(DoubleComparableProvider<N>),
  String(StringComparableProvider<S>),
}
impl<N, S> ComparableProvider for ComparableProviderEnum5<N, S>
where
  N: NumericDocValues,
  S: SortedDocValues,
{
  #[inline]
  fn get_as_comparable_long(&mut self, doc_id: i32) -> Result<i64> {
    match self {
      ComparableProviderEnum5::Int(inner) => inner.get_as_comparable_long(doc_id),
      ComparableProviderEnum5::Long(inner) => inner.get_as_comparable_long(doc_id),
      ComparableProviderEnum5::Float(inner) => inner.get_as_comparable_long(doc_id),
      ComparableProviderEnum5::Double(inner) => inner.get_as_comparable_long(doc_id),
      ComparableProviderEnum5::String(inner) => inner.get_as_comparable_long(doc_id),
    }
  }
}
pub enum ComparableProviderEnum4<N> {
  Int(IntComparableProvider<N>),
  Long(LongComparableProvider<N>),
  Float(FloatComparableProvider<N>),
  Double(DoubleComparableProvider<N>),
}
impl<N> ComparableProvider for ComparableProviderEnum4<N>
where
  N: NumericDocValues,
{
  #[inline]
  fn get_as_comparable_long(&mut self, doc_id: i32) -> Result<i64> {
    match self {
      ComparableProviderEnum4::Int(inner) => inner.get_as_comparable_long(doc_id),
      ComparableProviderEnum4::Long(inner) => inner.get_as_comparable_long(doc_id),
      ComparableProviderEnum4::Float(inner) => inner.get_as_comparable_long(doc_id),
      ComparableProviderEnum4::Double(inner) => inner.get_as_comparable_long(doc_id),
    }
  }
}
pub type CPEnumType1<NP, LR, SP> =
  ComparableProviderEnum5<ProviderNumeric<NP, LR>, ProviderString<SP, LR>>;
pub type CPEnumType2<NP, LR> = ComparableProviderEnum4<ProviderNumeric<NP, LR>>;

/// A comparator of doc IDs, used for sorting documents within a segment
pub trait DocComparator {
  /// Compare docID1 against docID2.
  fn compare(&self, doc_id1: usize, doc_id2: usize) -> i32;

  /// Visits the comparator without changing its comparison semantics.
  fn visit<V: DocComparatorVisitor>(&self, visitor: V) -> V::Output
  where
    Self: Sized,
  {
    visitor.visit(self)
  }
}

/// An operation over a concrete, borrowed document comparator.
pub trait DocComparatorVisitor {
  type Output;

  fn visit<DC: DocComparator>(self, comparator: &DC) -> Self::Output;
}

macro_rules! either_doc_comparator {
    ($vis:vis $name:ident { $( $Variant:ident : $T:ident ),+ $(,)? }) => {
        $vis enum $name<$( $T ),+> {
            $( $Variant($T), )+
        }

        impl<$( $T ),+> DocComparator for $name<$( $T ),+>
        where
            $( $T: DocComparator ),+
        {
            #[inline]
            fn compare(&self, doc_id1: usize, doc_id2: usize) -> i32 {
                match self {
                    $( Self::$Variant(inner) => inner.compare(doc_id1, doc_id2), )+
                }
            }

            fn visit<V: DocComparatorVisitor>(&self, visitor: V) -> V::Output {
                match self {
                    $( Self::$Variant(inner) => inner.visit(visitor), )+
                }
            }
        }
    };
}
either_doc_comparator!(pub DocComparatorEnum2 { A: A, B: B });
either_doc_comparator!(pub DocComparatorEnum5 { Int: A, Long: B, Float: C, Double: D, String: E });
pub type DocComparatorImpl = DocComparatorEnum5<
  DocComparatorImplInt,
  DocComparatorImplLong,
  DocComparatorImplFloat,
  DocComparatorImplDouble,
  DocComparatorImplString,
>;
pub type ProviderNumeric<N, LR> = <N as NumericDocValuesProvider>::NumericDocValues<LR>;
pub type ProviderString<S, LR> = <S as SortedDocValuesProvider>::SortedDocValues<LR>;
/// Provide a NumericDocValues instance for a LeafReader
pub trait NumericDocValuesProvider {
  /// Returns the numeric value for the given doc ID.
  type NumericDocValues<LR>: NumericDocValues
  where
    LR: LeafReader;
  /// Returns the NumericDocValues instance for this LeafReader
  fn get<LR>(&self, leaf_reader: &LR) -> Result<Self::NumericDocValues<LR>>
  where
    LR: LeafReader;
}
/// Provide a SortedDocValues instance for a LeafReader
pub trait SortedDocValuesProvider {
  type SortedDocValues<LR>: SortedDocValues
  where
    LR: LeafReader;
  /// Returns the SortedDocValues instance for this LeafReader
  fn get<LR>(&self, leaf_reader: &LR) -> Result<Self::SortedDocValues<LR>>
  where
    LR: LeafReader;
}
