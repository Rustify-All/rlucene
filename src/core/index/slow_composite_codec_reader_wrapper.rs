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
use crate::core::codecs::dummy::dummy_doc_values_skipper::DummyDocValuesSkipper;
use crate::core::codecs::dummy::dummy_mutable_point_tree::DummyMutablePointTree;
use crate::core::codecs::fields_producer::FieldsProducer;
use crate::core::codecs::hnsw::hnsw_graph_provider::HnswGraphProvider;
use crate::core::codecs::knn_vectors_reader::KnnVectorsReader;
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::codecs::points_reader::PointsReader;
use crate::core::codecs::stored_fields_reader::{DefaultStoredFieldsReader, StoredFieldsReader};
use crate::core::codecs::stored_fields_writer::StoredFieldsWriter;
use crate::core::codecs::term_vectors_reader::{DefaultTermVectorsReader, TermVectorsReader};
use crate::core::index::byte_vector_values::ByteVectorValue;
use crate::core::index::byte_vector_values::ByteVectorValues;
use crate::core::index::codec_reader::{
  CRDocValuesProducer, CRFieldsProducer, CRKnnVectorReader, CRNormsProducer, CRPointsReader,
  CRStoredFieldsReader, CRTermVectorsReader, CodecReader, SlowCompositeCodecReader,
};
use crate::core::index::doc_values::{DocValues, SortedDocValuesWithEmpty};
use crate::core::index::dummy::dummy_byte_vector_values::DummyByteVectorValues;
use crate::core::index::dummy::dummy_cache_helper::DummyCacheHelper;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::{FieldInfos, get_merged_field_infos};
use crate::core::index::fields::Fields;
use crate::core::index::float_vector_values::{FloatVectorValue, FloatVectorValues};
use crate::core::index::index_reader::{IndexReader, IndexReaderBase, LeafReaderContextKind};
use crate::core::index::knn_vector_values::{
  BitsImpl, DocIndexIterator, DocIndexIteratorEnum2, KnnVectorValues, KnnVectorValuesEnm2,
};
use crate::core::index::leaf_metadata::LeafMetaData;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::multi_bits::{BitsType, get_live_docs};
use crate::core::index::multi_doc_values::{
  MultiBinaryDocValues, MultiDocValues, MultiNormNumericDocValues, MultiNumericDocValues,
  MultiSortedDocValues, MultiSortedDocValuesType, MultiSortedNumericDocValues,
  MultiSortedSetDocValues, MultiSortedSetDocValuesType,
};
use crate::core::index::multi_fields::{MultiFields, MultiFieldsTerms};
use crate::core::index::multi_reader::MultiReader;
use crate::core::index::ordinal_map::OrdinalMap;
use crate::core::index::point_values::{
  IntersectVisitor, PointTree, PointTreeEnum, PointTreeEnum2, PointValues, Relation,
};
use crate::core::index::reader_slice::ReaderSlice;
use crate::core::index::sorted_doc_values::SortedDocValuesEnum2WithUnsupportedSecondPostingsAndAttributes;
use crate::core::index::sorted_set_doc_values_writer::{
  SingletonOrMultiSortedSetDocValuesEnum, SortedSetDocValuesWithEmpty,
};
use crate::core::index::stored_field_visitor::{Status, StoredFieldVisitor};
use crate::core::index::stored_fields::{RawStoredFieldsReader, StoredFields};
use crate::core::index::term::Term;
use crate::core::index::term_vectors::{OptionalTermVectors, RawTermVectors, TermVectors};
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::dummy::dummy_vector_scorer::DummyVectorScorer;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::util::array_util::{ArrayUtil, ByteArrayComparator};
use crate::core::util::bits::{Bits, BitsEnum2};
use crate::core::util::clone::TryClone;
use crate::core::util::close::CloseableRef;
use crate::core::util::dummy::dummy_hnsw_graph::DummyHnswGraph;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::io_utils::IOUtils;
use crate::core::util::merged_iterator::MergedIterator;
use crate::core::util::{CoreHelper, SliceCopyOps, TryIntoInt};
use parking_lot::Mutex;
use std::borrow::Cow;
use std::cell::Cell;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::rc::Rc;
use std::sync::Arc;

pub(crate) enum SlowCompositeFloatVectorValues<T, U> {
  A(T),
  B(U),
}

impl<T, U> KnnVectorValues for SlowCompositeFloatVectorValues<T, U>
where
  T: FloatVectorValues,
  U: FloatVectorValues,
{
  fn dimension(&self) -> usize {
    match self {
      Self::A(values) => values.dimension(),
      Self::B(values) => values.dimension(),
    }
  }

  fn size(&self) -> usize {
    match self {
      Self::A(values) => values.size(),
      Self::B(values) => values.size(),
    }
  }

  fn ord_to_doc(&self, ord: usize) -> Result<usize> {
    match self {
      Self::A(values) => values.ord_to_doc(ord),
      Self::B(values) => values.ord_to_doc(ord),
    }
  }

  type KnnVectorValues = KnnVectorValuesEnm2<T::KnnVectorValues, U::KnnVectorValues>;

  fn copy(&self) -> Result<Self::KnnVectorValues> {
    match self {
      Self::A(values) => values.copy().map(KnnVectorValuesEnm2::A),
      Self::B(values) => values.copy().map(KnnVectorValuesEnm2::B),
    }
  }

  fn get_vector_byte_length(&self) -> usize {
    match self {
      Self::A(values) => values.get_vector_byte_length(),
      Self::B(values) => values.get_vector_byte_length(),
    }
  }

  fn get_encoding(&self) -> VectorEncoding {
    match self {
      Self::A(values) => KnnVectorValues::get_encoding(values),
      Self::B(values) => KnnVectorValues::get_encoding(values),
    }
  }

  type Bits<'a, B1>
    = BitsEnum2<T::Bits<'a, B1>, U::Bits<'a, B1>>
  where
    B1: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B1>(&'a self, accept_docs: Option<B1>) -> Option<Self::Bits<'a, B1>>
  where
    B1: Bits,
  {
    match self {
      Self::A(values) => values.get_accept_ords(accept_docs).map(BitsEnum2::A),
      Self::B(values) => values.get_accept_ords(accept_docs).map(BitsEnum2::B),
    }
  }

  type DocIndexIterator = DocIndexIteratorEnum2<T::DocIndexIterator, U::DocIndexIterator>;

  fn iterator(&self) -> Result<Self::DocIndexIterator> {
    match self {
      Self::A(values) => values.iterator().map(DocIndexIteratorEnum2::A),
      Self::B(values) => values.iterator().map(DocIndexIteratorEnum2::B),
    }
  }
}

impl<T, U> FloatVectorValues for SlowCompositeFloatVectorValues<T, U>
where
  T: FloatVectorValues,
  U: FloatVectorValues,
{
  type VectorValue<'v>
    = FloatVectorValue<'v>
  where
    Self: 'v;

  fn vector_value(&self, ord: usize) -> Result<Self::VectorValue<'_>> {
    match self {
      Self::A(values) => values.vector_value(ord).map(Into::into),
      Self::B(values) => values.vector_value(ord).map(Into::into),
    }
  }

  type FloatVectorValues =
    SlowCompositeFloatVectorValues<T::FloatVectorValues, U::FloatVectorValues>;

  fn float_copy(&self) -> Result<Option<Self::FloatVectorValues>> {
    match self {
      Self::A(values) => values
        .float_copy()
        .map(|values| values.map(SlowCompositeFloatVectorValues::A)),
      Self::B(values) => values
        .float_copy()
        .map(|values| values.map(SlowCompositeFloatVectorValues::B)),
    }
  }

  type VectorScorer = T::VectorScorer;

  fn scorer<'a, TV>(&self, target: TV) -> Result<Option<Self::VectorScorer>>
  where
    TV: Into<std::borrow::Cow<'a, [f32]>>,
  {
    let target: std::borrow::Cow<'a, [f32]> = target.into();
    match self {
      Self::A(values) => values.scorer(target),
      Self::B(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn get_encoding(&self) -> VectorEncoding {
    match self {
      Self::A(values) => FloatVectorValues::get_encoding(values),
      Self::B(values) => FloatVectorValues::get_encoding(values),
    }
  }

  fn get_vectors_mut(&mut self) -> Result<&mut Vec<Box<[f32]>>> {
    match self {
      Self::A(values) => values.get_vectors_mut(),
      Self::B(values) => values.get_vectors_mut(),
    }
  }

  fn get_vectors(&self) -> Result<&[Box<[f32]>]> {
    match self {
      Self::A(values) => values.get_vectors(),
      Self::B(values) => values.get_vectors(),
    }
  }

  fn get_vectors_capacity(&self) -> Result<usize> {
    match self {
      Self::A(values) => values.get_vectors_capacity(),
      Self::B(values) => values.get_vectors_capacity(),
    }
  }
}

pub(crate) enum SlowCompositeByteVectorValues<T, U> {
  A(T),
  B(U),
}

impl<T, U> KnnVectorValues for SlowCompositeByteVectorValues<T, U>
where
  T: ByteVectorValues,
  U: ByteVectorValues,
{
  fn dimension(&self) -> usize {
    match self {
      Self::A(values) => values.dimension(),
      Self::B(values) => values.dimension(),
    }
  }

  fn size(&self) -> usize {
    match self {
      Self::A(values) => values.size(),
      Self::B(values) => values.size(),
    }
  }

  fn ord_to_doc(&self, ord: usize) -> Result<usize> {
    match self {
      Self::A(values) => values.ord_to_doc(ord),
      Self::B(values) => values.ord_to_doc(ord),
    }
  }

  type KnnVectorValues = KnnVectorValuesEnm2<T::KnnVectorValues, U::KnnVectorValues>;

  fn copy(&self) -> Result<Self::KnnVectorValues> {
    match self {
      Self::A(values) => values.copy().map(KnnVectorValuesEnm2::A),
      Self::B(values) => values.copy().map(KnnVectorValuesEnm2::B),
    }
  }

  fn get_vector_byte_length(&self) -> usize {
    match self {
      Self::A(values) => values.get_vector_byte_length(),
      Self::B(values) => values.get_vector_byte_length(),
    }
  }

  fn get_encoding(&self) -> VectorEncoding {
    match self {
      Self::A(values) => KnnVectorValues::get_encoding(values),
      Self::B(values) => KnnVectorValues::get_encoding(values),
    }
  }

  type Bits<'a, B1>
    = BitsEnum2<T::Bits<'a, B1>, U::Bits<'a, B1>>
  where
    B1: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B1>(&'a self, accept_docs: Option<B1>) -> Option<Self::Bits<'a, B1>>
  where
    B1: Bits,
  {
    match self {
      Self::A(values) => values.get_accept_ords(accept_docs).map(BitsEnum2::A),
      Self::B(values) => values.get_accept_ords(accept_docs).map(BitsEnum2::B),
    }
  }

  type DocIndexIterator = DocIndexIteratorEnum2<T::DocIndexIterator, U::DocIndexIterator>;

  fn iterator(&self) -> Result<Self::DocIndexIterator> {
    match self {
      Self::A(values) => values.iterator().map(DocIndexIteratorEnum2::A),
      Self::B(values) => values.iterator().map(DocIndexIteratorEnum2::B),
    }
  }
}

impl<T, U> ByteVectorValues for SlowCompositeByteVectorValues<T, U>
where
  T: ByteVectorValues,
  U: ByteVectorValues,
{
  type VectorValue<'v>
    = ByteVectorValue<'v>
  where
    Self: 'v;

  fn vector_value(&self, ord: usize) -> Result<Self::VectorValue<'_>> {
    match self {
      Self::A(values) => values.vector_value(ord).map(Into::into),
      Self::B(values) => values.vector_value(ord).map(Into::into),
    }
  }

  type ByteVectorValues = SlowCompositeByteVectorValues<T::ByteVectorValues, U::ByteVectorValues>;

  fn byte_copy(&self) -> Result<Option<Self::ByteVectorValues>> {
    match self {
      Self::A(values) => values
        .byte_copy()
        .map(|values| values.map(SlowCompositeByteVectorValues::A)),
      Self::B(values) => values
        .byte_copy()
        .map(|values| values.map(SlowCompositeByteVectorValues::B)),
    }
  }

  type VectorScorer = T::VectorScorer;

  fn scorer<TV>(&self, target: TV) -> Result<Option<Self::VectorScorer>>
  where
    TV: Into<Vec<u8>>,
  {
    let target = target.into();
    match self {
      Self::A(values) => values.scorer(target),
      Self::B(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn get_encoding(&self) -> VectorEncoding {
    match self {
      Self::A(values) => ByteVectorValues::get_encoding(values),
      Self::B(values) => ByteVectorValues::get_encoding(values),
    }
  }

  fn get_vectors_mut(&mut self) -> Result<&mut Vec<Box<[u8]>>> {
    match self {
      Self::A(values) => values.get_vectors_mut(),
      Self::B(values) => values.get_vectors_mut(),
    }
  }

  fn get_vectors(&self) -> Result<&[Box<[u8]>]> {
    match self {
      Self::A(values) => values.get_vectors(),
      Self::B(values) => values.get_vectors(),
    }
  }

  fn get_vectors_capacity(&self) -> Result<usize> {
    match self {
      Self::A(values) => values.get_vectors_capacity(),
      Self::B(values) => values.get_vectors_capacity(),
    }
  }
}

pub(crate) enum SlowCompositePointValues<T, U> {
  A(T),
  B(U),
}

impl<T, U> PointValues for SlowCompositePointValues<T, U>
where
  T: PointValues,
  U: PointValues,
{
  fn get_min_packed_value(&self) -> Result<Option<Cow<'_, [u8]>>> {
    match self {
      Self::A(values) => values.get_min_packed_value(),
      Self::B(values) => values.get_min_packed_value(),
    }
  }

  fn get_max_packed_value(&self) -> Result<Option<Cow<'_, [u8]>>> {
    match self {
      Self::A(values) => values.get_max_packed_value(),
      Self::B(values) => values.get_max_packed_value(),
    }
  }

  fn get_num_dimensions(&self) -> Result<usize> {
    match self {
      Self::A(values) => values.get_num_dimensions(),
      Self::B(values) => values.get_num_dimensions(),
    }
  }

  fn get_num_index_dimensions(&self) -> Result<usize> {
    match self {
      Self::A(values) => values.get_num_index_dimensions(),
      Self::B(values) => values.get_num_index_dimensions(),
    }
  }

  fn get_bytes_per_dimension(&self) -> Result<usize> {
    match self {
      Self::A(values) => values.get_bytes_per_dimension(),
      Self::B(values) => values.get_bytes_per_dimension(),
    }
  }

  fn size(&self) -> Result<usize> {
    match self {
      Self::A(values) => values.size(),
      Self::B(values) => values.size(),
    }
  }

  fn get_doc_count(&self) -> Result<i32> {
    match self {
      Self::A(values) => values.get_doc_count(),
      Self::B(values) => values.get_doc_count(),
    }
  }

  type PointTree = PointTreeEnum<
    <T::PointTree as PointTree>::MutablePointTree,
    PointTreeEnum2<T::PointTree, U::PointTree>,
  >;

  fn get_point_tree(&self) -> Result<Self::PointTree> {
    match self {
      Self::A(values) => match values.get_point_tree()?.into_mutable() {
        PointTreeEnum::Mutable(tree) => Ok(PointTreeEnum::Mutable(tree)),
        PointTreeEnum::Other(tree) => Ok(PointTreeEnum::Other(PointTreeEnum2::A(tree))),
      },
      Self::B(values) => match values.get_point_tree()?.into_mutable() {
        PointTreeEnum::Mutable(_) => Err(LuceneError::unsupported_operation("")),
        PointTreeEnum::Other(tree) => Ok(PointTreeEnum::Other(PointTreeEnum2::B(tree))),
      },
    }
  }
}

pub(crate) enum SlowCompositePointsReader<T, U> {
  A(T),
  B(U),
}

impl<T, U> CloseableRef for SlowCompositePointsReader<T, U>
where
  T: CloseableRef,
  U: CloseableRef,
{
  fn close(&self) -> Result<()> {
    match self {
      Self::A(reader) => reader.close(),
      Self::B(reader) => reader.close(),
    }
  }
}

impl<T, U> PointsReader for SlowCompositePointsReader<T, U>
where
  T: PointsReader,
  U: PointsReader,
{
  fn check_integrity(&self) -> Result<()> {
    match self {
      Self::A(reader) => reader.check_integrity(),
      Self::B(reader) => reader.check_integrity(),
    }
  }

  type PointValuesType = SlowCompositePointValues<T::PointValuesType, U::PointValuesType>;

  fn get_values(&self, field: &str) -> Result<Option<Self::PointValuesType>> {
    match self {
      Self::A(reader) => reader
        .get_values(field)
        .map(|values| values.map(SlowCompositePointValues::A)),
      Self::B(reader) => reader
        .get_values(field)
        .map(|values| values.map(SlowCompositePointValues::B)),
    }
  }

  fn get_merge_instance(&self) -> Result<Option<Self>>
  where
    Self: Sized,
  {
    match self {
      Self::A(reader) => reader
        .get_merge_instance()
        .map(|reader| reader.map(SlowCompositePointsReader::A)),
      Self::B(reader) => reader
        .get_merge_instance()
        .map(|reader| reader.map(SlowCompositePointsReader::B)),
    }
  }
}

pub(crate) enum SlowCompositeKnnVectorsReader<T, U> {
  A(T),
  B(U),
}

impl<T, U> CloseableRef for SlowCompositeKnnVectorsReader<T, U>
where
  T: CloseableRef,
  U: CloseableRef,
{
  fn close(&self) -> Result<()> {
    match self {
      Self::A(reader) => reader.close(),
      Self::B(reader) => reader.close(),
    }
  }
}

impl<T, U> HnswGraphProvider for SlowCompositeKnnVectorsReader<T, U>
where
  T: HnswGraphProvider,
  U: HnswGraphProvider,
{
  type HnswGraph = T::HnswGraph;

  fn is_hnsw_graph_provider(&self, field: &str) -> bool {
    match self {
      Self::A(reader) => reader.is_hnsw_graph_provider(field),
      Self::B(reader) => reader.is_hnsw_graph_provider(field),
    }
  }

  fn get_graph(&self, field: &str) -> Result<Self::HnswGraph> {
    match self {
      Self::A(reader) => reader.get_graph(field),
      Self::B(_) => Err(LuceneError::unsupported_operation("")),
    }
  }
}

impl<T, U> KnnVectorsReader for SlowCompositeKnnVectorsReader<T, U>
where
  T: KnnVectorsReader,
  U: KnnVectorsReader,
{
  fn check_integrity(&self) -> Result<()> {
    match self {
      Self::A(reader) => reader.check_integrity(),
      Self::B(reader) => reader.check_integrity(),
    }
  }

  type FloatVectorValues =
    SlowCompositeFloatVectorValues<T::FloatVectorValues, U::FloatVectorValues>;

  fn get_float_vector_values(&self, field: &str) -> Result<Self::FloatVectorValues> {
    match self {
      Self::A(reader) => reader
        .get_float_vector_values(field)
        .map(SlowCompositeFloatVectorValues::A),
      Self::B(reader) => reader
        .get_float_vector_values(field)
        .map(SlowCompositeFloatVectorValues::B),
    }
  }

  type ByteVectorValues = SlowCompositeByteVectorValues<T::ByteVectorValues, U::ByteVectorValues>;

  fn get_byte_vector_values(&self, field: &str) -> Result<Self::ByteVectorValues> {
    match self {
      Self::A(reader) => reader
        .get_byte_vector_values(field)
        .map(SlowCompositeByteVectorValues::A),
      Self::B(reader) => reader
        .get_byte_vector_values(field)
        .map(SlowCompositeByteVectorValues::B),
    }
  }

  type QuantizedByteVectorValues = DummyByteVectorValues;

  fn get_quantization_state(
    &self,
    field: &str,
  ) -> Result<Option<crate::core::util::quantization::scalar_quantizer::ScalarQuantizer>> {
    match self {
      Self::A(reader) => reader.get_quantization_state(field),
      Self::B(reader) => reader.get_quantization_state(field),
    }
  }

  fn is_flat_vectors_reader(&self, field: &str) -> bool {
    match self {
      Self::A(reader) => reader.is_flat_vectors_reader(field),
      Self::B(reader) => reader.is_flat_vectors_reader(field),
    }
  }

  fn search_f32<B1, K, TV>(
    &self,
    field: &str,
    target: TV,
    knn_collector: &mut K,
    accept_docs: Option<B1>,
  ) -> Result<()>
  where
    TV: Into<Vec<f32>>,
    B1: Bits,
    K: KnnCollector,
  {
    let target = target.into();
    match self {
      Self::A(reader) => reader.search_f32(field, target, knn_collector, accept_docs),
      Self::B(reader) => reader.search_f32(field, target, knn_collector, accept_docs),
    }
  }

  fn search_u8<B1, K, TV>(
    &self,
    field: &str,
    target: TV,
    knn_collector: &mut K,
    accept_docs: Option<B1>,
  ) -> Result<()>
  where
    TV: Into<Vec<u8>>,
    B1: Bits,
    K: KnnCollector,
  {
    let target = target.into();
    match self {
      Self::A(reader) => reader.search_u8(field, target, knn_collector, accept_docs),
      Self::B(reader) => reader.search_u8(field, target, knn_collector, accept_docs),
    }
  }

  fn get_merge_instance(&self) -> Result<Option<Self>>
  where
    Self: Sized,
  {
    match self {
      Self::A(reader) => reader
        .get_merge_instance()
        .map(|reader| reader.map(SlowCompositeKnnVectorsReader::A)),
      Self::B(reader) => reader
        .get_merge_instance()
        .map(|reader| reader.map(SlowCompositeKnnVectorsReader::B)),
    }
  }

  fn finish_merge(&self) -> Result<()> {
    match self {
      Self::A(reader) => reader.finish_merge(),
      Self::B(reader) => reader.finish_merge(),
    }
  }
}

pub(crate) fn wrap<CR>(mut readers: Vec<CR>) -> Result<SlowCompositeCodecReader<CR>>
where
  CR: CodecReader + Clone,
{
  match readers.len() {
    0 => Err(LuceneError::illegal_argument(
      "Must take at least one reader, got 0",
    )),
    1 => readers
      .pop()
      .map(SlowCompositeCodecReader::A)
      .ok_or_else(|| LuceneError::illegal_state("single codec reader is missing")),
    _ => Ok(SlowCompositeCodecReader::B(
      SlowCompositeCodecReaderWrapper::new(readers)?,
    )),
  }
}

/// A merged [`CodecReader`] view of multiple [`CodecReader`]s.
///
/// This view is primarily targeted at merging, not searching.
pub(crate) struct SlowCompositeCodecReaderWrapper<CR>
where
  CR: CodecReader + Clone,
{
  meta: LeafMetaData,
  codec_readers: Arc<[CR]>,
  doc_stats: Arc<Vec<usize>>,
  field_infos: Arc<FieldInfos>,
  live_docs: Option<Arc<BitsType<MultiReader<CR>>>>,
  inner: Mutex<Inner>,
  index_base: IndexReaderBase,
}
struct Inner {
  num_docs: i32,
}
impl<CR> SlowCompositeCodecReaderWrapper<CR>
where
  CR: CodecReader + Clone,
{
  pub(crate) fn new(codec_readers: Vec<CR>) -> Result<Self> {
    let codec_readers: Arc<[CR]> = Arc::from(codec_readers);
    let mut doc_stats = Vec::with_capacity(codec_readers.len() + 1);

    doc_stats.push(0);
    let mut doc_start = 0;
    for reader in codec_readers.iter() {
      doc_start += reader.max_doc()?;
      doc_stats.push(doc_start as usize);
    }

    let mut major_version = -1;
    let mut min_version = None;
    let mut has_blocks = false;

    for reader in codec_readers.iter() {
      let reader_meta = reader.get_metadata()?;
      if major_version == -1 {
        major_version = reader_meta.get_created_version_major();
      } else if major_version != reader_meta.get_created_version_major() {
        return Err(LuceneError::illegal_argument(
          "Cannot combine leaf readers created with different major versions",
        ));
      }
      match (min_version, reader_meta.get_min_version()) {
        (None, reader_min_version) => min_version = reader_min_version.as_ref(),
        (Some(current_min_version), Some(reader_min_version))
          if current_min_version.on_or_after(reader_min_version) =>
        {
          min_version = Some(reader_min_version);
        },
        (Some(_), None) => {
          return Err(LuceneError::illegal_state(
            "cannot combine a reader without min_version after one with min_version",
          ));
        },
        _ => {},
      }
      has_blocks |= reader_meta.get_has_blocks();
    }

    let meta = LeafMetaData::new(major_version, min_version.cloned(), None, has_blocks)?;

    let multi_reader: MultiReader<CR> = MultiReader::new_shared(codec_readers.clone())?;
    let field_infos = get_merged_field_infos(&multi_reader)?;
    let live_docs = get_live_docs(multi_reader)?.map(Arc::new);
    let inner = Mutex::new(Inner { num_docs: -1 });
    Ok(Self {
      meta,
      codec_readers,
      doc_stats: Arc::new(doc_stats),
      field_infos,
      live_docs,
      inner,
      index_base: IndexReaderBase::new(),
    })
  }
}

impl<CR> LeafReader for SlowCompositeCodecReaderWrapper<CR>
where
  CR: Clone + CodecReader,
{
  type CacheHelper = DummyCacheHelper;

  fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
    Ok(None)
  }

  type Terms = <<Self as CodecReader>::FieldsProducer as Fields>::Terms;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    CodecReader::terms(self, field)
  }

  type NumericDocValues =
    <<Self as CodecReader>::DocValuesProducer as DocValuesProducer>::NumericDocValues;

  fn get_numeric_doc_values(&self, field: &str) -> Result<Option<Self::NumericDocValues>> {
    CodecReader::get_numeric_doc_values(self, field)
  }

  type BinaryDocValues =
    <<Self as CodecReader>::DocValuesProducer as DocValuesProducer>::BinaryDocValues;

  fn get_binary_doc_values(&self, field: &str) -> Result<Option<Self::BinaryDocValues>> {
    CodecReader::get_binary_doc_values(self, field)
  }

  type SortedDocValues =
    <<Self as CodecReader>::DocValuesProducer as DocValuesProducer>::SortedDocValues;

  fn get_sorted_doc_values(&self, field: &str) -> Result<Option<Self::SortedDocValues>> {
    CodecReader::get_sorted_doc_values(self, field)
  }

  type SortedNumericDocValues =
    <<Self as CodecReader>::DocValuesProducer as DocValuesProducer>::SortedNumericDocValues;

  fn get_sorted_numeric_doc_values(
    &self,
    field: &str,
  ) -> Result<Option<Self::SortedNumericDocValues>> {
    CodecReader::get_sorted_numeric_doc_values(self, field)
  }

  type SortedSetDocValues =
    <<Self as CodecReader>::DocValuesProducer as DocValuesProducer>::SortedSetDocValues;

  fn get_sorted_set_doc_values(&self, field: &str) -> Result<Option<Self::SortedSetDocValues>> {
    CodecReader::get_sorted_set_doc_values(self, field)
  }

  type NormNumericDocValues =
    <<Self as CodecReader>::NormsProducer as NormsProducer>::NumericDocValues;

  fn get_norm_values(&self, field: &str) -> Result<Option<Self::NormNumericDocValues>> {
    CodecReader::get_norm_values(self, field)
  }

  type DocValuesSkipper =
    <<Self as CodecReader>::DocValuesProducer as DocValuesProducer>::DocValuesSkipper;

  fn get_doc_values_skipper(&self, field: &str) -> Result<Option<Self::DocValuesSkipper>> {
    CodecReader::get_doc_values_skipper(self, field)
  }

  type FloatVectorValues =
    <<Self as CodecReader>::KnnVectorsReader as KnnVectorsReader>::FloatVectorValues;

  fn get_float_vector_values(&self, field: &str) -> Result<Option<Self::FloatVectorValues>> {
    CodecReader::get_float_vector_values(self, field)
  }

  type ByteVectorValues =
    <<Self as CodecReader>::KnnVectorsReader as KnnVectorsReader>::ByteVectorValues;

  fn get_byte_vector_values(&self, field: &str) -> Result<Option<Self::ByteVectorValues>> {
    CodecReader::get_byte_vector_values(self, field)
  }

  fn search_nearest_vectors_f32<B, K, TV>(
    &self,
    field: &str,
    target: TV,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
    TV: Into<Vec<f32>>,
  {
    let target = target.into();
    CodecReader::search_nearest_vectors_f32(self, field, target, knn_collector, accept_docs)
  }

  fn search_nearest_vectors_u8<B, K, TV>(
    &self,
    field: &str,
    target: TV,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
    TV: Into<Vec<u8>>,
  {
    let target = target.into();
    CodecReader::search_nearest_vectors_u8(self, field, target, knn_collector, accept_docs)
  }

  fn get_field_infos(&self) -> Result<Arc<FieldInfos>> {
    Ok(self.field_infos.clone())
  }

  type Bits = Arc<BitsType<MultiReader<CR>>>;

  fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
    Ok(self.live_docs.clone())
  }

  type PointValues = <<Self as CodecReader>::PointsReader as PointsReader>::PointValuesType;

  fn get_point_values(&self, field: &str) -> Result<Option<Self::PointValues>> {
    CodecReader::get_point_values(self, field)
  }

  fn get_metadata(&self) -> Result<&LeafMetaData> {
    Ok(&self.meta)
  }
}

impl<CR> IndexReader for SlowCompositeCodecReaderWrapper<CR>
where
  CR: Clone + CodecReader,
{
  type ContextKind = LeafReaderContextKind;

  type TermVectors = OptionalTermVectors<<Self as CodecReader>::TermVectorsReader>;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    match self.get_term_vectors_reader()? {
      Some(tvr) => Ok(OptionalTermVectors::Reader(tvr)),
      None => Ok(OptionalTermVectors::Empty),
    }
  }

  fn max_doc(&self) -> Result<i32> {
    Ok(self.doc_stats[self.doc_stats.len() - 1] as i32)
  }

  fn num_docs(&self) -> Result<i32> {
    // Compute the number of docs lazily, in case some leaves need to recompute it the first time it
    // is called; see `BaseCompositeReader::num_docs`.
    let mut inner = self.inner.lock();
    if inner.num_docs == -1 {
      let mut total = 0;
      for reader in self.codec_readers.iter() {
        total += reader.num_docs()?;
      }
      inner.num_docs = total;
    }

    Ok(inner.num_docs)
  }

  type StoredFields = StoredFieldsImpl<<Self as CodecReader>::StoredFieldsReader>;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    let reader = self
      .get_fields_reader()?
      .ok_or_else(|| LuceneError::illegal_state("fields reader is None"))?;
    Ok(StoredFieldsImpl::new(reader, self.max_doc()?))
  }

  type ReaderCacheHelper = DummyCacheHelper;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    Ok(None)
  }

  fn doc_freq(&self, term: &Term) -> Result<i32> {
    LeafReader::doc_freq(self, term)
  }

  fn total_term_freq(&self, term: &Term) -> Result<i64> {
    LeafReader::get_total_term_freq(self, term)
  }

  fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
    LeafReader::get_sum_doc_freq(self, field)
  }

  fn get_doc_count(&self, field: &str) -> Result<i32> {
    LeafReader::get_doc_count(self, field)
  }

  fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
    LeafReader::get_sum_total_term_freq(self, field)
  }

  fn index_base(&self) -> &IndexReaderBase {
    &self.index_base
  }
}

impl<CR> Display for SlowCompositeCodecReaderWrapper<CR>
where
  CR: Clone + CodecReader,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "SlowCompositeCodecReaderWrapper({} readers)",
      self.codec_readers.len()
    )
  }
}

impl<CR> CodecReader for SlowCompositeCodecReaderWrapper<CR>
where
  CR: CodecReader + Clone,
{
  type StoredFieldsReader = SlowCompositeStoredFieldsReaderWrapper<CRStoredFieldsReader<CR>>;
  type TermVectorsReader = SlowCompositeTermVectorsReaderWrapper<CRTermVectorsReader<CR>>;
  type NormsProducer = SlowCompositeNormsProducer<CR>;
  type DocValuesProducer = SlowCompositeDocValuesProducerWrapper<CR>;
  type FieldsProducer = SlowCompositeFieldsProducerWrapper<CRFieldsProducer<CR>>;
  type PointsReader = SlowCompositePointsReaderWrapper<CR>;
  type KnnVectorsReader = SlowCompositeKnnVectorsReaderWrapper<CR>;

  fn get_fields_reader(&self) -> Result<Option<Self::StoredFieldsReader>> {
    let mut readers = Vec::with_capacity(self.codec_readers.len());
    for cr in self.codec_readers.iter() {
      readers.push(cr.get_fields_reader()?);
    }
    Ok(Some(SlowCompositeStoredFieldsReaderWrapper::new(
      self.doc_stats.clone(),
      readers,
      self.field_infos.clone(),
    )))
  }

  fn get_term_vectors_reader(&self) -> Result<Option<Self::TermVectorsReader>> {
    let mut readers = Vec::with_capacity(self.codec_readers.len());
    for cr in self.codec_readers.iter() {
      readers.push(cr.get_term_vectors_reader()?);
    }
    Ok(Some(SlowCompositeTermVectorsReaderWrapper::new(
      self.doc_stats.clone(),
      readers,
    )))
  }

  fn get_norms_reader(&self) -> Result<Option<Self::NormsProducer>> {
    Ok(Some(SlowCompositeNormsProducer::new(
      self.codec_readers.clone(),
    )?))
  }

  fn get_doc_values_reader(&self) -> Result<Option<Self::DocValuesProducer>> {
    Ok(Some(SlowCompositeDocValuesProducerWrapper::new(
      self.codec_readers.clone(),
      self.doc_stats.clone(),
    )?))
  }

  fn get_postings_reader(&self) -> Result<Option<Self::FieldsProducer>> {
    let mut readers = Vec::with_capacity(self.codec_readers.len());
    for cr in self.codec_readers.iter() {
      readers.push(cr.get_postings_reader()?);
    }
    Ok(Some(SlowCompositeFieldsProducerWrapper::new(
      readers,
      self.doc_stats.as_slice(),
    )?))
  }

  fn get_points_reader(&self) -> Result<Option<Self::PointsReader>> {
    Ok(Some(SlowCompositePointsReaderWrapper::new(
      self.codec_readers.clone(),
      self.doc_stats.clone(),
    )?))
  }

  fn get_vector_reader(&self) -> Result<Option<Self::KnnVectorsReader>> {
    Ok(Some(SlowCompositeKnnVectorsReaderWrapper::new(
      self.codec_readers.clone(),
      self.doc_stats.clone(),
    )?))
  }
}

pub struct StoredFieldsImpl<SF> {
  reader: SF,
  max_doc: i32,
}
impl<SF> StoredFieldsImpl<SF>
where
  SF: StoredFields,
{
  pub fn new(reader: SF, max_doc: i32) -> Self {
    Self { reader, max_doc }
  }
}
impl<SF> StoredFields for StoredFieldsImpl<SF>
where
  SF: StoredFields,
{
  fn prefetch(&mut self, doc_id: i32) -> Result<()> {
    CoreHelper::check_index(doc_id, self.max_doc)?;
    self.reader.prefetch(doc_id)
  }

  fn document_with_visitor<S, V>(
    &mut self,
    doc_id: i32,
    visitor: &mut V,
    writer: Option<&mut S>,
  ) -> Result<()>
  where
    S: StoredFieldsWriter,
    V: StoredFieldVisitor,
  {
    CoreHelper::check_index(doc_id, self.max_doc)?;
    self.reader.document_with_visitor(doc_id, visitor, writer)
  }
}

impl<SF> RawStoredFieldsReader for StoredFieldsImpl<SF>
where
  SF: RawStoredFieldsReader + StoredFields,
{
  type IndexInput = SF::IndexInput;

  fn raw_stored_fields_mut(&mut self) -> Result<&mut DefaultStoredFieldsReader<Self::IndexInput>> {
    self.reader.raw_stored_fields_mut()
  }

  fn raw_stored_fields(&self) -> Result<&DefaultStoredFieldsReader<Self::IndexInput>> {
    self.reader.raw_stored_fields()
  }
}

fn doc_id_to_reader_id(doc: i32, doc_starts: &[usize]) -> Result<usize> {
  CoreHelper::check_index(doc, doc_starts[doc_starts.len() - 1].try_convert()?)?;
  match doc_starts.binary_search(&(doc as usize)) {
    Ok(reader_id) => Ok(reader_id),
    Err(insert_pos) => Ok(insert_pos - 1),
  }
}

pub struct SlowCompositeStoredFieldsReaderWrapper<SFR> {
  doc_starts: Arc<Vec<usize>>,
  readers: Vec<Option<SFR>>,
  field_infos: Arc<FieldInfos>,
}
impl<SFR> SlowCompositeStoredFieldsReaderWrapper<SFR> {
  pub fn new(
    doc_starts: Arc<Vec<usize>>,
    readers: Vec<Option<SFR>>,
    field_infos: Arc<FieldInfos>,
  ) -> Self {
    Self {
      doc_starts,
      readers,
      field_infos,
    }
  }
}

impl<SFR> StoredFields for SlowCompositeStoredFieldsReaderWrapper<SFR>
where
  SFR: StoredFieldsReader,
{
  fn prefetch(&mut self, doc_id: i32) -> Result<()> {
    let reader_id = doc_id_to_reader_id(doc_id, self.doc_starts.as_slice())?;
    let reader = self.readers[reader_id]
      .as_mut()
      .ok_or_else(|| LuceneError::illegal_state("StoredFieldsReader is None"))?;
    reader.prefetch(doc_id - self.doc_starts[reader_id] as i32)
  }

  fn document_with_visitor<S, V>(
    &mut self,
    doc_id: i32,
    visitor: &mut V,
    writer: Option<&mut S>,
  ) -> Result<()>
  where
    S: StoredFieldsWriter,
    V: StoredFieldVisitor,
  {
    let reader_id = doc_id_to_reader_id(doc_id, self.doc_starts.as_slice())?;
    let mut sf_visitor = StoredFieldVisitorImpl::new(visitor, &self.field_infos);
    let reader = &mut self.readers[reader_id]
      .as_mut()
      .ok_or_else(|| LuceneError::illegal_state("StoredFieldsReader is None"))?;
    reader.document_with_visitor(
      doc_id - self.doc_starts[reader_id] as i32,
      &mut sf_visitor,
      writer,
    )
  }
}

impl<SFR> TryClone for SlowCompositeStoredFieldsReaderWrapper<SFR>
where
  SFR: StoredFieldsReader,
{
  fn try_clone(&self) -> Result<Self>
  where
    Self: Sized,
  {
    let mut readers = Vec::with_capacity(self.readers.len());
    for reader in &self.readers {
      match reader {
        Some(reader) => readers.push(Some(reader.try_clone()?)),
        None => readers.push(None),
      }
    }
    Ok(SlowCompositeStoredFieldsReaderWrapper::new(
      self.doc_starts.clone(),
      readers,
      self.field_infos.clone(),
    ))
  }
}

impl<SFR> CloseableRef for SlowCompositeStoredFieldsReaderWrapper<SFR>
where
  SFR: CloseableRef,
{
  fn close(&self) -> Result<()> {
    IOUtils::close_with(self.readers.iter().flatten(), |reader| reader.close())
  }
}

impl<SFR> StoredFieldsReader for SlowCompositeStoredFieldsReaderWrapper<SFR>
where
  SFR: StoredFieldsReader,
{
  fn check_integrity(&self) -> Result<()> {
    for r in self.readers.iter().flatten() {
      r.check_integrity()?;
    }
    Ok(())
  }
}

impl<SFR> RawStoredFieldsReader for SlowCompositeStoredFieldsReaderWrapper<SFR>
where
  SFR: RawStoredFieldsReader + StoredFieldsReader,
{
  type IndexInput = SFR::IndexInput;
}

pub struct StoredFieldVisitorImpl<'a, SFV> {
  visitor: &'a mut SFV,
  field_infos: &'a FieldInfos,
}
impl<'a, SFV> StoredFieldVisitorImpl<'a, SFV>
where
  SFV: StoredFieldVisitor,
{
  fn new(visitor: &'a mut SFV, field_infos: &'a FieldInfos) -> Self {
    Self {
      visitor,
      field_infos,
    }
  }
  fn remap(&self, field_info: &FieldInfo) -> Result<&'a FieldInfo> {
    let field_infos: &'a FieldInfos = self.field_infos;
    let fi = field_infos.field_info_by_name(&field_info.name)?;
    match fi {
      Some(fi) => Ok(fi),
      None => Err(LuceneError::illegal_state(format!(
        "FieldInfo not found by {}",
        field_info.name
      ))),
    }
  }
}

impl<'a, SFV> StoredFieldVisitor for StoredFieldVisitorImpl<'a, SFV>
where
  SFV: StoredFieldVisitor,
{
  fn binary_field<S>(
    &mut self,
    field_info: &FieldInfo,
    value: std::borrow::Cow<'_, [u8]>,
    writer: Option<&mut S>,
  ) -> Result<()>
  where
    S: StoredFieldsWriter,
  {
    let field_info = self.remap(field_info)?;
    self.visitor.binary_field(field_info, value, writer)
  }

  fn string_field<S>(
    &mut self,
    field_info: &FieldInfo,
    value: std::borrow::Cow<'_, str>,
    writer: Option<&mut S>,
  ) -> Result<()>
  where
    S: StoredFieldsWriter,
  {
    let field_info = self.remap(field_info)?;
    self.visitor.string_field(field_info, value, writer)
  }

  fn int_field<S>(
    &mut self,
    field_info: &FieldInfo,
    value: i32,
    writer: Option<&mut S>,
  ) -> Result<()>
  where
    S: StoredFieldsWriter,
  {
    let field_info = self.remap(field_info)?;
    self.visitor.int_field(field_info, value, writer)
  }

  fn long_field<S>(
    &mut self,
    field_info: &FieldInfo,
    value: i64,
    writer: Option<&mut S>,
  ) -> Result<()>
  where
    S: StoredFieldsWriter,
  {
    let field_info = self.remap(field_info)?;
    self.visitor.long_field(field_info, value, writer)
  }

  fn float_field<S>(
    &mut self,
    field_info: &FieldInfo,
    value: f32,
    writer: Option<&mut S>,
  ) -> Result<()>
  where
    S: StoredFieldsWriter,
  {
    let field_info = self.remap(field_info)?;
    self.visitor.float_field(field_info, value, writer)
  }

  fn double_field<S>(
    &mut self,
    field_info: &FieldInfo,
    value: f64,
    writer: Option<&mut S>,
  ) -> Result<()>
  where
    S: StoredFieldsWriter,
  {
    let field_info = self.remap(field_info)?;
    self.visitor.double_field(field_info, value, writer)
  }

  fn needs_field<S>(&mut self, field_info: &FieldInfo, writer: Option<&mut S>) -> Result<Status>
  where
    S: StoredFieldsWriter,
  {
    let field_info = self.remap(field_info)?;
    self.visitor.needs_field(field_info, writer)
  }
}

pub struct SlowCompositeTermVectorsReaderWrapper<TVR> {
  doc_starts: Arc<Vec<usize>>,
  readers: Vec<Option<TVR>>,
}

impl<TVR> SlowCompositeTermVectorsReaderWrapper<TVR> {
  pub fn new(doc_starts: Arc<Vec<usize>>, readers: Vec<Option<TVR>>) -> Self {
    Self {
      doc_starts,
      readers,
    }
  }
}

impl<TVR> TryClone for SlowCompositeTermVectorsReaderWrapper<TVR>
where
  TVR: TermVectorsReader,
{
  fn try_clone(&self) -> Result<Self>
  where
    Self: Sized,
  {
    let mut readers = Vec::with_capacity(self.readers.len());
    for reader in &self.readers {
      match reader {
        Some(reader) => readers.push(Some(reader.try_clone()?)),
        None => readers.push(None),
      }
    }
    Ok(Self {
      doc_starts: self.doc_starts.clone(),
      readers,
    })
  }
}

impl<TVR> TermVectors for SlowCompositeTermVectorsReaderWrapper<TVR>
where
  TVR: TermVectorsReader,
{
  fn prefetch(&mut self, doc: i32) -> Result<()> {
    let reader_id = doc_id_to_reader_id(doc, self.doc_starts.as_slice())?;
    if let Some(reader) = &mut self.readers[reader_id] {
      let local_doc = doc - self.doc_starts[reader_id] as i32;
      reader.prefetch(local_doc)?;
    }
    Ok(())
  }

  type Fields = <TVR as TermVectors>::Fields;

  fn get(&mut self, doc: i32) -> Result<Option<Self::Fields>> {
    let reader_id = doc_id_to_reader_id(doc, self.doc_starts.as_slice())?;
    match &mut self.readers[reader_id] {
      None => Ok(None),
      Some(reader) => {
        let local_doc = doc - self.doc_starts[reader_id] as i32;
        reader.get(local_doc)
      },
    }
  }

  type Terms = <Self::Fields as Fields>::Terms;

  fn get_field_terms(
    &mut self,
    doc: i32,
    field: &str,
  ) -> Result<Option<<Self::Fields as Fields>::Terms>> {
    self.default_get_field_terms(doc, field)
  }
}

impl<TVR> TermVectorsReader for SlowCompositeTermVectorsReaderWrapper<TVR>
where
  TVR: TermVectorsReader,
{
  fn check_integrity(&self) -> Result<()> {
    for r in self.readers.iter().flatten() {
      r.check_integrity()?;
    }
    Ok(())
  }
}

impl<TVR> CloseableRef for SlowCompositeTermVectorsReaderWrapper<TVR>
where
  TVR: CloseableRef,
{
  fn close(&self) -> Result<()> {
    IOUtils::close_with(self.readers.iter().flatten(), |reader| reader.close())
  }
}

impl<TVR> RawTermVectors for SlowCompositeTermVectorsReaderWrapper<TVR>
where
  TVR: TermVectorsReader,
{
  type IndexInput = <TVR as RawTermVectors>::IndexInput;

  fn raw_term_vectors_mut(&mut self) -> Result<&mut DefaultTermVectorsReader<Self::IndexInput>> {
    Err(LuceneError::illegal_state(
      "raw term vectors reader is not available".to_string(),
    ))
  }

  fn raw_term_vectors(&self) -> Result<&DefaultTermVectorsReader<Self::IndexInput>> {
    Err(LuceneError::illegal_state(
      "raw term vectors reader is not available".to_string(),
    ))
  }
}

pub struct SlowCompositeNormsProducer<CR>
where
  CR: CodecReader,
{
  codec_readers: Arc<[CR]>,
  producers: Vec<Option<CRNormsProducer<CR>>>,
}

impl<CR> SlowCompositeNormsProducer<CR>
where
  CR: CodecReader + Clone,
{
  pub fn new(codec_readers: Arc<[CR]>) -> Result<Self> {
    let mut producers = Vec::with_capacity(codec_readers.len());
    for reader in codec_readers.iter() {
      producers.push(reader.get_norms_reader()?);
    }
    Ok(Self {
      codec_readers,
      producers,
    })
  }
}

impl<CR> CloseableRef for SlowCompositeNormsProducer<CR>
where
  CR: CodecReader,
{
  fn close(&self) -> Result<()> {
    IOUtils::close_with(self.producers.iter().flatten(), |producer| producer.close())
  }
}

impl<CR> NormsProducer for SlowCompositeNormsProducer<CR>
where
  CR: CodecReader + Clone,
{
  type MergeInstance = Self;
  type NumericDocValues = MultiNormNumericDocValues<MultiReader<CR>>;

  fn get_norms(&self, field: &Arc<FieldInfo>) -> Result<Self::NumericDocValues> {
    let multi_reader = MultiReader::new_shared(self.codec_readers.clone())?;
    match MultiDocValues::get_norm_values(multi_reader, &field.name)? {
      Some(norms) => Ok(norms),
      None => Err(LuceneError::illegal_state(format!(
        "No norms found for field {}",
        field.name
      ))),
    }
  }

  fn check_integrity(&self) -> Result<()> {
    for p in self.producers.iter().flatten() {
      p.check_integrity()?;
    }
    Ok(())
  }
}

pub struct SlowCompositeDocValuesProducerWrapper<CR>
where
  CR: CodecReader,
{
  codec_readers: Arc<[CR]>,
  producers: Vec<Option<CRDocValuesProducer<CR>>>,
  doc_starts: Arc<Vec<usize>>,
  cached_ord_maps: Mutex<HashMap<String, Arc<OrdinalMap>>>,
}

impl<CR> SlowCompositeDocValuesProducerWrapper<CR>
where
  CR: CodecReader + Clone,
{
  pub fn new(codec_readers: Arc<[CR]>, doc_starts: Arc<Vec<usize>>) -> Result<Self> {
    let mut producers = Vec::with_capacity(codec_readers.len());
    for reader in codec_readers.iter() {
      producers.push(reader.get_doc_values_reader()?);
    }
    Ok(Self {
      codec_readers,
      producers,
      doc_starts,
      cached_ord_maps: Mutex::new(HashMap::new()),
    })
  }
}

impl<CR> CloseableRef for SlowCompositeDocValuesProducerWrapper<CR>
where
  CR: CodecReader,
{
  fn close(&self) -> Result<()> {
    IOUtils::close_with(self.producers.iter().flatten(), |producer| producer.close())
  }
}

impl<CR> DocValuesProducer for SlowCompositeDocValuesProducerWrapper<CR>
where
  CR: CodecReader + Clone,
{
  type NumericDocValues = MultiNumericDocValues<MultiReader<CR>>;

  fn get_numeric(&self, field: &Arc<FieldInfo>) -> Result<Self::NumericDocValues> {
    let mr = MultiReader::new_shared(self.codec_readers.clone())?;
    match MultiDocValues::get_numeric_values(mr, &field.name)? {
      Some(numeric) => Ok(numeric),
      None => Err(LuceneError::illegal_state(format!(
        "No numeric doc values found for field {}",
        field.name
      ))),
    }
  }

  type BinaryDocValues = MultiBinaryDocValues<MultiReader<CR>>;

  fn get_binary(&self, field: &Arc<FieldInfo>) -> Result<Self::BinaryDocValues> {
    let mr = MultiReader::new_shared(self.codec_readers.clone())?;
    match MultiDocValues::get_binary_values(mr, &field.name)? {
      Some(binary) => Ok(binary),
      None => Err(LuceneError::illegal_state(format!(
        "No binary doc values found for field {}",
        field.name
      ))),
    }
  }

  type SortedDocValues = MultiSortedDocValuesType<MultiReader<CR>>;

  fn get_sorted(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedDocValues> {
    if let Some(map) = self.cached_ord_maps.lock().get(&field.name).cloned() {
      let mut values = Vec::with_capacity(self.codec_readers.len());
      let mut total_cost = 0;

      for reader in self.codec_readers.iter() {
        match LeafReader::get_sorted_doc_values(reader, &field.name)? {
          Some(v) => {
            total_cost += v.cost()?;
            values.push(SortedDocValuesWithEmpty::A(v));
          },
          None => {
            let v = DocValues::empty_sorted();
            total_cost += v.cost()?;
            values.push(SortedDocValuesWithEmpty::B(v));
          },
        }
      }

      return Ok(
        SortedDocValuesEnum2WithUnsupportedSecondPostingsAndAttributes::B(
          MultiSortedDocValues::new(self.doc_starts.clone(), values, map, total_cost),
        ),
      );
    }

    let mr: MultiReader<CR> = MultiReader::new_shared(self.codec_readers.clone())?;

    let dv = MultiDocValues::get_sorted_values(mr, &field.name)?.ok_or_else(|| {
      LuceneError::illegal_state(format!(
        "No sorted doc values found for field {}",
        field.name
      ))
    })?;

    if let SortedDocValuesEnum2WithUnsupportedSecondPostingsAndAttributes::B(ref multi) = dv {
      self
        .cached_ord_maps
        .lock()
        .insert(field.name.clone(), multi.mapping.clone());
    }

    Ok(dv)
  }

  type SortedNumericDocValues = MultiSortedNumericDocValues<MultiReader<CR>>;

  fn get_sorted_numeric(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedNumericDocValues> {
    let mr = MultiReader::new_shared(self.codec_readers.clone())?;
    match MultiDocValues::get_sorted_numeric_values(mr, &field.name)? {
      Some(sorted_numeric) => Ok(sorted_numeric),
      None => Err(LuceneError::illegal_state(format!(
        "No sorted numeric doc values found for field {}",
        field.name
      ))),
    }
  }

  type SortedSetDocValues = MultiSortedSetDocValuesType<MultiReader<CR>>;

  fn get_sorted_set(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedSetDocValues> {
    if let Some(map) = self.cached_ord_maps.lock().get(&field.name).cloned() {
      let mut values = Vec::with_capacity(self.codec_readers.len());
      let mut total_cost = 0;

      for reader in self.codec_readers.iter() {
        match LeafReader::get_sorted_set_doc_values(reader, &field.name)? {
          Some(v) => {
            total_cost += v.cost()?;
            values.push(SortedSetDocValuesWithEmpty::A(v));
          },
          None => {
            let v = DocValues::empty_sorted_set()?;
            total_cost += v.cost()?;
            values.push(SortedSetDocValuesWithEmpty::B(v));
          },
        }
      }

      return Ok(SingletonOrMultiSortedSetDocValuesEnum::Multi(
        MultiSortedSetDocValues::new(values, self.doc_starts.clone(), map, total_cost),
      ));
    }

    let mr: MultiReader<CR> = MultiReader::new_shared(self.codec_readers.clone())?;

    let dv = MultiDocValues::get_sorted_set_values(mr, &field.name)?.ok_or_else(|| {
      LuceneError::illegal_state(format!(
        "No sorted-set doc values found for field {}",
        field.name
      ))
    })?;

    if let SingletonOrMultiSortedSetDocValuesEnum::Multi(ref multi) = dv {
      self
        .cached_ord_maps
        .lock()
        .insert(field.name.clone(), multi.mapping.clone());
    }

    Ok(dv)
  }

  type DocValuesSkipper = DummyDocValuesSkipper;

  fn get_skipper(&self, _field: &Arc<FieldInfo>) -> Result<Option<Self::DocValuesSkipper>> {
    Err(LuceneError::unsupported_operation(
      "This method is for searching not for merging",
    ))
  }

  fn check_integrity(&self) -> Result<()> {
    for p in self.producers.iter().flatten() {
      p.check_integrity()?;
    }
    Ok(())
  }
}

pub struct SlowCompositeFieldsProducerWrapper<FP>
where
  FP: Fields,
{
  fields: MultiFields<FP>,
}

impl<FP> SlowCompositeFieldsProducerWrapper<FP>
where
  FP: Fields,
{
  pub fn new(producers: Vec<Option<FP>>, doc_starts: &[usize]) -> Result<Self> {
    let mut subs = Vec::with_capacity(producers.len());
    let mut slices = Vec::with_capacity(producers.len());

    for (i, producer) in producers.into_iter().enumerate() {
      if let Some(p) = producer {
        subs.push(p);
        slices.push(ReaderSlice::new(doc_starts[i], i as i32, doc_starts[i + 1]));
      }
    }

    let fields = MultiFields::new(subs, slices);

    Ok(Self { fields })
  }
}

impl<FP> CloseableRef for SlowCompositeFieldsProducerWrapper<FP>
where
  FP: Fields + CloseableRef,
{
  fn close(&self) -> Result<()> {
    IOUtils::close_with(self.fields.subs.iter(), |field| field.close())
  }
}

impl<FP> Fields for SlowCompositeFieldsProducerWrapper<FP>
where
  FP: FieldsProducer,
{
  type FieldIter<'a>
    = MergedIterator<<FP as Fields>::FieldIter<'a>>
  where
    Self: 'a;

  fn iterator(&self) -> Result<Self::FieldIter<'_>> {
    self.fields.iterator()
  }

  type Terms = MultiFieldsTerms<<FP as Fields>::Terms>;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    self.fields.terms(field)
  }

  fn size(&self) -> Result<i32> {
    self.fields.size()
  }
}

impl<FP> FieldsProducer for SlowCompositeFieldsProducerWrapper<FP>
where
  FP: FieldsProducer,
{
  fn check_integrity(&self) -> Result<()> {
    for producer in self.fields.subs.iter() {
      producer.check_integrity()?;
    }
    Ok(())
  }
}

pub struct PointValuesSub<PV> {
  pub sub: PV,
  pub doc_base: i32,
}

impl<PV> PointValuesSub<PV> {
  pub fn new(sub: PV, doc_base: i32) -> Self {
    Self { sub, doc_base }
  }
}

pub struct SlowCompositeKnnVectorsReaderWrapper<CR>
where
  CR: CodecReader,
{
  codec_readers: Arc<[CR]>,
  readers: Vec<Option<CRKnnVectorReader<CR>>>,
  doc_starts: Arc<Vec<usize>>,
}
impl<CR> SlowCompositeKnnVectorsReaderWrapper<CR>
where
  CR: CodecReader + Clone,
{
  fn new(codec_readers: Arc<[CR]>, doc_starts: Arc<Vec<usize>>) -> Result<Self> {
    let mut readers = Vec::with_capacity(codec_readers.len());
    for reader in codec_readers.iter() {
      readers.push(reader.get_vector_reader()?);
    }
    Ok(Self {
      codec_readers,
      readers,
      doc_starts,
    })
  }
}

impl<CR> CloseableRef for SlowCompositeKnnVectorsReaderWrapper<CR>
where
  CR: CodecReader,
{
  fn close(&self) -> Result<()> {
    IOUtils::close_with(self.readers.iter().flatten(), |reader| reader.close())
  }
}

impl<CR> HnswGraphProvider for SlowCompositeKnnVectorsReaderWrapper<CR>
where
  CR: CodecReader + Clone,
{
  type HnswGraph = DummyHnswGraph;
}
impl<CR> KnnVectorsReader for SlowCompositeKnnVectorsReaderWrapper<CR>
where
  CR: CodecReader + Clone,
{
  fn check_integrity(&self) -> Result<()> {
    for reader in self.readers.iter().flatten() {
      reader.check_integrity()?;
    }
    Ok(())
  }

  type FloatVectorValues =
    MergedFloatVectorValues<<CRKnnVectorReader<CR> as KnnVectorsReader>::FloatVectorValues>;

  fn get_float_vector_values(&self, field: &str) -> Result<Self::FloatVectorValues> {
    let mut subs = Vec::with_capacity(self.codec_readers.len());
    let mut dimension = None;
    let mut size = 0;
    let mut i = 0;
    while i < self.codec_readers.len() {
      let reader = &self.codec_readers[i];
      let values = CodecReader::get_float_vector_values(reader, field)?;
      let ord_start = size;
      if let Some(values) = &values {
        if dimension.is_none() {
          dimension = Some(values.dimension());
        }
        size += values.size();
      }
      subs.push(DocValuesSub::new(
        values,
        self.doc_starts[i] as i32,
        ord_start as i32,
      ));
      i += 1;
    }
    MergedFloatVectorValues::new(
      dimension.ok_or_else(|| LuceneError::illegal_state("field has no float vector values"))?,
      size,
      subs,
    )
  }

  type ByteVectorValues =
    MergedByteVectorValues<<CRKnnVectorReader<CR> as KnnVectorsReader>::ByteVectorValues>;

  fn get_byte_vector_values(&self, field: &str) -> Result<Self::ByteVectorValues> {
    let mut subs = Vec::with_capacity(self.codec_readers.len());
    let mut dimension = None;
    let mut size = 0;
    let mut i = 0;
    while i < self.codec_readers.len() {
      let reader = &self.codec_readers[i];
      let values = CodecReader::get_byte_vector_values(reader, field)?;
      let ord_start = size;
      if let Some(values) = &values {
        if dimension.is_none() {
          dimension = Some(values.dimension());
        }
        size += values.size();
      }
      subs.push(DocValuesSub::new(
        values,
        self.doc_starts[i] as i32,
        ord_start as i32,
      ));
      i += 1;
    }
    MergedByteVectorValues::new(
      dimension.ok_or_else(|| LuceneError::illegal_state("field has no byte vector values"))?,
      size,
      subs,
    )
  }

  type QuantizedByteVectorValues = DummyByteVectorValues;

  fn search_f32<B, K, TV>(
    &self,
    _field: &str,
    _target: TV,
    _knn_collector: &mut K,
    _accept_docs: Option<B>,
  ) -> Result<()>
  where
    TV: Into<Vec<f32>>,
    B: Bits,
    K: KnnCollector,
  {
    Err(LuceneError::unsupported_operation(""))
  }

  fn search_u8<B, K, TV>(
    &self,
    _field: &str,
    _target: TV,
    _knn_collector: &mut K,
    _accept_docs: Option<B>,
  ) -> Result<()>
  where
    TV: Into<Vec<u8>>,
    B: Bits,
    K: KnnCollector,
  {
    Err(LuceneError::unsupported_operation(""))
  }
}
pub struct DocValuesSub<T> {
  pub sub: Option<T>,
  pub doc_start: i32,
  pub ord_start: i32,
}

impl<T> DocValuesSub<T> {
  pub fn new(sub: Option<T>, doc_start: i32, ord_start: i32) -> Self {
    Self {
      sub,
      doc_start,
      ord_start,
    }
  }
}
pub struct MergedDocIterator<T> {
  subs: Vec<DocIteratorSub<T>>,
  current: usize,
  current_iter: Option<T>,
  ord: i32,
  doc: i32,
}

struct DocIteratorSub<T> {
  iterator: Option<T>,
  doc_start: i32,
  ord_start: i32,
}

impl<T> MergedDocIterator<T>
where
  T: DocIndexIterator,
{
  pub fn new<V>(values_subs: &[DocValuesSub<V>]) -> Result<Self>
  where
    V: KnnVectorValues<DocIndexIterator = T>,
  {
    let mut subs = Vec::with_capacity(values_subs.len());
    for sub in values_subs {
      subs.push(DocIteratorSub {
        iterator: match &sub.sub {
          Some(values) => Some(values.iterator()?),
          None => None,
        },
        doc_start: sub.doc_start,
        ord_start: sub.ord_start,
      });
    }
    if subs.is_empty() {
      return Err(LuceneError::illegal_argument(
        "at least one document must be supplied",
      ));
    }

    let current = 0;
    let current_iter = Self::current_iterator(&mut subs, current)?;

    Ok(Self {
      subs,
      current,
      current_iter,
      ord: -1,
      doc: -1,
    })
  }
  fn current_iterator(subs: &mut [DocIteratorSub<T>], current_idx: usize) -> Result<Option<T>> {
    Ok(subs[current_idx].iterator.take())
  }
}

impl<T> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for MergedDocIterator<T>
where
  T: DocIndexIterator,
{
}
impl<T> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for MergedDocIterator<T> where
  T: DocIndexIterator
{
}

impl<T> DocIdSetIterator for MergedDocIterator<T>
where
  T: DocIndexIterator,
{
  fn doc_id(&self) -> i32 {
    self.doc
  }

  fn next_doc(&mut self) -> Result<i32> {
    loop {
      if let Some(ref mut it) = self.current_iter {
        let next = it.next_doc()?;
        if next != NO_MORE_DOCS {
          self.ord += 1;
          self.doc = self.subs[self.current].doc_start + next;
          return Ok(self.doc);
        }
      }

      if self.current + 1 >= self.subs.len() {
        self.ord = NO_MORE_DOCS;
        self.doc = NO_MORE_DOCS;
        return Ok(NO_MORE_DOCS);
      }
      self.current += 1;
      self.current_iter = Self::current_iterator(&mut self.subs, self.current)?;
      self.ord = self.subs[self.current].ord_start - 1;
    }
  }

  fn advance(&mut self, _target: i32) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn cost(&self) -> Result<i64> {
    Err(LuceneError::unsupported_operation(""))
  }
}

impl<T> DocIndexIterator for MergedDocIterator<T>
where
  T: DocIndexIterator,
{
  fn index(&self) -> Result<i32> {
    Ok(self.ord)
  }
}

pub struct MergedFloatVectorValues<F>
where
  F: FloatVectorValues,
{
  dimension: usize,
  size: usize,
  subs: Vec<DocValuesSub<F>>,
  iterator: Cell<Option<MergedDocIterator<F::DocIndexIterator>>>,
  starts: Vec<i32>,
  last_sub_index: Cell<usize>,
}

impl<F> MergedFloatVectorValues<F>
where
  F: FloatVectorValues,
{
  fn new(dimension: usize, size: usize, subs: Vec<DocValuesSub<F>>) -> Result<Self> {
    let iterator = MergedDocIterator::new(&subs)?;
    let mut starts = Vec::with_capacity(subs.len() + 1);
    for sub in &subs {
      starts.push(sub.ord_start);
    }
    starts.push(size as i32);
    Ok(Self {
      dimension,
      size,
      subs,
      iterator: Cell::new(Some(iterator)),
      starts,
      last_sub_index: Cell::new(0),
    })
  }
}

impl<F> KnnVectorValues for MergedFloatVectorValues<F>
where
  F: FloatVectorValues,
{
  fn dimension(&self) -> usize {
    self.dimension
  }

  fn size(&self) -> usize {
    self.size
  }

  fn ord_to_doc(&self, ord: usize) -> Result<usize> {
    let sub_index = find_sub(ord, self.last_sub_index.get(), &self.starts);
    self.last_sub_index.set(sub_index);
    let sub = &self.subs[sub_index];
    let values = sub
      .sub
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("missing float vector values"))?;
    Ok(sub.doc_start as usize + values.ord_to_doc(ord - sub.ord_start as usize)?)
  }

  type KnnVectorValues = MergedFloatVectorValues<F::FloatVectorValues>;

  fn copy(&self) -> Result<Self::KnnVectorValues> {
    FloatVectorValues::float_copy(self)?.ok_or_else(|| LuceneError::unsupported_operation(""))
  }

  fn get_encoding(&self) -> VectorEncoding {
    FloatVectorValues::get_encoding(self)
  }

  type Bits<'a, B>
    = BitsImpl<B, &'a Self>
  where
    B: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B>(&'a self, accept_docs: Option<B>) -> Option<Self::Bits<'a, B>>
  where
    B: Bits,
  {
    self.default_get_accept_ords(accept_docs)
  }

  type DocIndexIterator = MergedDocIterator<F::DocIndexIterator>;

  fn iterator(&self) -> Result<Self::DocIndexIterator> {
    self.iterator.take().ok_or_else(|| {
      LuceneError::illegal_state("iterator() can only be called once on MergedFloatVectorValues")
    })
  }
}

impl<F> FloatVectorValues for MergedFloatVectorValues<F>
where
  F: FloatVectorValues,
{
  type VectorValue<'v>
    = F::VectorValue<'v>
  where
    Self: 'v;

  fn vector_value(&self, ord: usize) -> Result<Self::VectorValue<'_>> {
    debug_assert!(ord < self.size);
    let sub_index = find_sub(ord, self.last_sub_index.get(), &self.starts);
    self.last_sub_index.set(sub_index);
    let sub = &self.subs[sub_index];
    sub
      .sub
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("missing float vector values"))?
      .vector_value(ord - sub.ord_start as usize)
  }

  type FloatVectorValues = MergedFloatVectorValues<F::FloatVectorValues>;

  fn float_copy(&self) -> Result<Option<Self::FloatVectorValues>> {
    let mut subs = Vec::with_capacity(self.subs.len());
    for sub in &self.subs {
      let values = match &sub.sub {
        Some(values) => match values.float_copy()? {
          Some(values) => Some(values),
          None => return Ok(None),
        },
        None => None,
      };
      subs.push(DocValuesSub::new(values, sub.doc_start, sub.ord_start));
    }
    Ok(Some(MergedFloatVectorValues::new(
      self.dimension,
      self.size,
      subs,
    )?))
  }

  type VectorScorer = DummyVectorScorer;
}

pub struct MergedByteVectorValues<B>
where
  B: ByteVectorValues,
{
  dimension: usize,
  size: usize,
  subs: Vec<DocValuesSub<B>>,
  iterator: Cell<Option<MergedDocIterator<B::DocIndexIterator>>>,
  starts: Vec<i32>,
  last_sub_index: Cell<usize>,
}

impl<B> MergedByteVectorValues<B>
where
  B: ByteVectorValues,
{
  fn new(dimension: usize, size: usize, subs: Vec<DocValuesSub<B>>) -> Result<Self> {
    let iterator = MergedDocIterator::new(&subs)?;
    let mut starts = Vec::with_capacity(subs.len() + 1);
    for sub in &subs {
      starts.push(sub.ord_start);
    }
    starts.push(size as i32);
    Ok(Self {
      dimension,
      size,
      subs,
      iterator: Cell::new(Some(iterator)),
      starts,
      last_sub_index: Cell::new(0),
    })
  }
}

impl<B> KnnVectorValues for MergedByteVectorValues<B>
where
  B: ByteVectorValues,
{
  fn dimension(&self) -> usize {
    self.dimension
  }

  fn size(&self) -> usize {
    self.size
  }

  fn ord_to_doc(&self, ord: usize) -> Result<usize> {
    let sub_index = find_sub(ord, self.last_sub_index.get(), &self.starts);
    self.last_sub_index.set(sub_index);
    let sub = &self.subs[sub_index];
    let values = sub
      .sub
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("missing byte vector values"))?;
    Ok(sub.doc_start as usize + values.ord_to_doc(ord - sub.ord_start as usize)?)
  }

  type KnnVectorValues = MergedByteVectorValues<B::ByteVectorValues>;

  fn copy(&self) -> Result<Self::KnnVectorValues> {
    ByteVectorValues::byte_copy(self)?.ok_or_else(|| LuceneError::unsupported_operation(""))
  }

  fn get_encoding(&self) -> VectorEncoding {
    ByteVectorValues::get_encoding(self)
  }

  type Bits<'a, B1>
    = BitsImpl<B1, &'a Self>
  where
    B1: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B1>(&'a self, accept_docs: Option<B1>) -> Option<Self::Bits<'a, B1>>
  where
    B1: Bits,
  {
    self.default_get_accept_ords(accept_docs)
  }

  type DocIndexIterator = MergedDocIterator<B::DocIndexIterator>;

  fn iterator(&self) -> Result<Self::DocIndexIterator> {
    self.iterator.take().ok_or_else(|| {
      LuceneError::illegal_state("iterator() can only be called once on MergedByteVectorValues")
    })
  }
}

impl<B> ByteVectorValues for MergedByteVectorValues<B>
where
  B: ByteVectorValues,
{
  type VectorValue<'v>
    = B::VectorValue<'v>
  where
    Self: 'v;

  fn vector_value(&self, ord: usize) -> Result<Self::VectorValue<'_>> {
    debug_assert!(ord < self.size);
    let sub_index = find_sub(ord, self.last_sub_index.get(), &self.starts);
    self.last_sub_index.set(sub_index);
    let sub = &self.subs[sub_index];
    sub
      .sub
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("missing byte vector values"))?
      .vector_value(ord - sub.ord_start as usize)
  }

  type ByteVectorValues = MergedByteVectorValues<B::ByteVectorValues>;

  fn byte_copy(&self) -> Result<Option<Self::ByteVectorValues>> {
    let mut subs = Vec::with_capacity(self.subs.len());
    for sub in &self.subs {
      let values = match &sub.sub {
        Some(values) => match values.byte_copy()? {
          Some(values) => Some(values),
          None => return Ok(None),
        },
        None => None,
      };
      subs.push(DocValuesSub::new(values, sub.doc_start, sub.ord_start));
    }
    Ok(Some(MergedByteVectorValues::new(
      self.dimension,
      self.size,
      subs,
    )?))
  }

  type VectorScorer = DummyVectorScorer;
}

fn find_sub(ord: usize, last_sub_index: usize, starts: &[i32]) -> usize {
  let ord = ord as i32;
  if ord >= starts[last_sub_index] {
    if ord >= starts[last_sub_index + 1] {
      return binary_search_starts(starts, ord, last_sub_index + 1, starts.len());
    }
  } else {
    return binary_search_starts(starts, ord, 0, last_sub_index);
  }
  last_sub_index
}

fn binary_search_starts(starts: &[i32], ord: i32, from: usize, to: usize) -> usize {
  match starts[from..to].binary_search(&ord) {
    Ok(pos) => {
      let mut pos = from + pos;
      while pos < starts.len() - 1 && starts[pos + 1] == ord {
        pos += 1;
      }
      pos
    },
    Err(pos) => from + pos - 1,
  }
}

pub struct SlowCompositePointsReaderWrapper<CR>
where
  CR: CodecReader,
{
  codec_readers: Arc<[CR]>,
  readers: Vec<Option<CRPointsReader<CR>>>,
  doc_starts: Arc<Vec<usize>>,
}

impl<CR> SlowCompositePointsReaderWrapper<CR>
where
  CR: CodecReader + Clone,
{
  pub fn new(codec_readers: Arc<[CR]>, doc_starts: Arc<Vec<usize>>) -> Result<Self> {
    let mut readers = Vec::with_capacity(codec_readers.len());
    for reader in codec_readers.iter() {
      readers.push(reader.get_points_reader()?);
    }

    Ok(Self {
      codec_readers,
      readers,
      doc_starts,
    })
  }
}

impl<CR> CloseableRef for SlowCompositePointsReaderWrapper<CR>
where
  CR: CodecReader,
{
  fn close(&self) -> Result<()> {
    IOUtils::close_with(self.readers.iter().flatten(), |reader| reader.close())
  }
}

impl<CR> PointsReader for SlowCompositePointsReaderWrapper<CR>
where
  CR: CodecReader + Clone,
{
  fn check_integrity(&self) -> Result<()> {
    for r in self.readers.iter().flatten() {
      r.check_integrity()?;
    }
    Ok(())
  }

  type PointValuesType = PointValuesImpl<<CRPointsReader<CR> as PointsReader>::PointValuesType>;

  fn get_values(&self, field: &str) -> Result<Option<Self::PointValuesType>> {
    let mut values = Vec::with_capacity(self.readers.len());

    for i in 0..self.readers.len() {
      if let Some(fi) = self.codec_readers[i]
        .get_field_infos()?
        .field_info_by_name(field)?
        && fi.get_point_dimension_count() > 0
        && let Some(reader) = &self.readers[i]
        && let Some(v) = reader.get_values(field)?
      {
        // Apparently FieldInfo can claim a field has points, yet the returned
        // PointValues is None
        values.push(PointValuesSub::new(v, self.doc_starts[i] as i32));
      }
    }
    if values.is_empty() {
      return Ok(None);
    }
    Ok(Some(PointValuesImpl::new(values)))
  }
}

pub struct PointValuesImpl<PV> {
  values: Rc<Vec<PointValuesSub<PV>>>,
}
impl<PV> PointValuesImpl<PV>
where
  PV: PointValues,
{
  pub fn new<T>(values: T) -> Self
  where
    T: Into<Rc<Vec<PointValuesSub<PV>>>>,
  {
    let values = values.into();
    Self { values }
  }
}

impl<PV> PointValues for PointValuesImpl<PV>
where
  PV: PointValues,
{
  fn get_min_packed_value(&self) -> Result<Option<Cow<'_, [u8]>>> {
    let pt = self.get_point_tree()?;
    let v = pt.get_min_packed_value()?;

    Ok(Some(Cow::Owned(v.into_owned())))
  }

  fn get_max_packed_value(&self) -> Result<Option<Cow<'_, [u8]>>> {
    let pt = self.get_point_tree()?;
    let v = pt.get_max_packed_value()?;
    Ok(Some(Cow::Owned(v.into_owned())))
  }

  fn get_num_dimensions(&self) -> Result<usize> {
    self.values[0].sub.get_num_dimensions()
  }

  fn get_num_index_dimensions(&self) -> Result<usize> {
    self.values[0].sub.get_num_index_dimensions()
  }

  fn get_bytes_per_dimension(&self) -> Result<usize> {
    self.values[0].sub.get_bytes_per_dimension()
  }

  fn size(&self) -> Result<usize> {
    self.get_point_tree()?.size()
  }

  fn get_doc_count(&self) -> Result<i32> {
    let mut count = 0;
    for sub in self.values.iter() {
      count += sub.sub.get_doc_count()?;
    }
    Ok(count)
  }

  type PointTree = PointTreeImpl<PV>;

  fn get_point_tree(&self) -> Result<Self::PointTree> {
    Ok(PointTreeImpl::new(self.values.clone()))
  }
}

pub struct PointTreeImpl<PV> {
  values: Rc<Vec<PointValuesSub<PV>>>,
}
impl<PV> PointTreeImpl<PV>
where
  PV: PointValues,
{
  pub fn new<T>(values: T) -> Self
  where
    T: Into<Rc<Vec<PointValuesSub<PV>>>>,
  {
    Self {
      values: values.into(),
    }
  }
}

impl<PV> TryClone for PointTreeImpl<PV>
where
  PV: PointValues,
{
  fn try_clone(&self) -> Result<Self>
  where
    Self: Sized,
  {
    Ok(PointTreeImpl::new(self.values.clone()))
  }
}

impl<PV> PointTree for PointTreeImpl<PV>
where
  PV: PointValues,
{
  type MutablePointTree = DummyMutablePointTree;

  fn move_to_child(&mut self) -> Result<bool> {
    Ok(false)
  }

  fn move_to_sibling(&mut self) -> Result<bool> {
    Ok(false)
  }

  fn move_to_parent(&mut self) -> Result<bool> {
    Ok(false)
  }

  fn get_min_packed_value(&self) -> Result<Cow<'_, [u8]>> {
    let mut min_packed_value_opt: Option<Cow<'_, [u8]>> = None;

    for sub in self.values.iter() {
      let leaf_min_packed_value = sub
        .sub
        .get_min_packed_value()?
        .ok_or_else(|| LuceneError::illegal_state("PointValues returned no min packed value"))?;

      match &mut min_packed_value_opt {
        None => {
          min_packed_value_opt = Some(leaf_min_packed_value);
        },
        Some(min_packed_value) => {
          let num_index_dims = sub.sub.get_num_index_dimensions()?;
          let num_bytes_per_dim = sub.sub.get_bytes_per_dimension()?;
          let comparator = ArrayUtil::get_unsigned_comparator(num_bytes_per_dim);
          for i in 0..num_index_dims {
            let off = i * num_bytes_per_dim;
            let v = comparator.compare(
              leaf_min_packed_value.as_ref(),
              off,
              min_packed_value.as_ref(),
              off,
            );
            // unsigned byte-wise compare (lexicographic)
            if v < 0 {
              min_packed_value
                .to_mut()
                .copy_from(leaf_min_packed_value.as_ref(), off);
            }
          }
        },
      }
    }

    min_packed_value_opt.ok_or_else(|| LuceneError::illegal_state("min_packed_value_opt is None"))
  }

  fn get_max_packed_value(&self) -> Result<Cow<'_, [u8]>> {
    let mut max_packed_value_opt: Option<Cow<'_, [u8]>> = None;

    for sub in self.values.iter() {
      let leaf_max_packed_value = sub
        .sub
        .get_max_packed_value()?
        .ok_or_else(|| LuceneError::illegal_state("PointValues returned no max packed value"))?;

      match &mut max_packed_value_opt {
        None => {
          max_packed_value_opt = Some(leaf_max_packed_value);
        },
        Some(max_packed_value) => {
          let num_index_dims = sub.sub.get_num_index_dimensions()?;
          let num_bytes_per_dim = sub.sub.get_bytes_per_dimension()?;
          let comparator = ArrayUtil::get_unsigned_comparator(num_bytes_per_dim);

          for i in 0..num_index_dims {
            let off = i * num_bytes_per_dim;
            let v = comparator.compare(
              leaf_max_packed_value.as_ref(),
              off,
              max_packed_value.as_ref(),
              off,
            );
            // unsigned byte-wise compare (lexicographic)
            if v > 0 {
              max_packed_value
                .to_mut()
                .copy_from(leaf_max_packed_value.as_ref(), off);
            }
          }
        },
      }
    }

    max_packed_value_opt.ok_or_else(|| LuceneError::illegal_state("max_packed_value_opt is None"))
  }

  fn size(&self) -> Result<usize> {
    let mut size = 0;
    for sub in self.values.iter() {
      size += sub.sub.size()?;
    }
    Ok(size)
  }

  fn visit_doc_ids<IV>(&mut self, visitor: &mut IV) -> Result<()>
  where
    IV: IntersectVisitor,
  {
    for sub in self.values.iter() {
      let mut wrapped = wrap_intersect_visitor(visitor, sub.doc_base);
      sub.sub.get_point_tree()?.visit_doc_ids(&mut wrapped)?;
    }
    Ok(())
  }

  fn visit_doc_values<IV>(&mut self, visitor: &mut IV) -> Result<()>
  where
    IV: IntersectVisitor,
  {
    for sub in self.values.iter() {
      let mut wrapped = wrap_intersect_visitor(visitor, sub.doc_base);
      sub.sub.get_point_tree()?.visit_doc_values(&mut wrapped)?;
    }
    Ok(())
  }
}
fn wrap_intersect_visitor<IV>(visitor: &mut IV, doc_start: i32) -> IntersectVisitorImpl<'_, IV>
where
  IV: IntersectVisitor,
{
  IntersectVisitorImpl::new(visitor, doc_start)
}

struct IntersectVisitorImpl<'a, IV> {
  visitor: &'a mut IV,
  doc_start: i32,
}
impl<'a, IV> IntersectVisitorImpl<'a, IV>
where
  IV: IntersectVisitor,
{
  fn new(visitor: &'a mut IV, doc_start: i32) -> Self {
    Self { visitor, doc_start }
  }
}

impl<'a, IV> IntersectVisitor for IntersectVisitorImpl<'a, IV>
where
  IV: IntersectVisitor,
{
  fn visit(&mut self, doc_id: i32) -> Result<()> {
    self.visitor.visit(self.doc_start + doc_id)
  }

  fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()> {
    self
      .visitor
      .visit_with_packed_value(self.doc_start + doc_id, packed_value)
  }

  fn compare(&self, min_packed_value: &[u8], max_packed_value: &[u8]) -> Result<Relation> {
    self.visitor.compare(min_packed_value, max_packed_value)
  }
}
