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
use crate::codec::bitvectors::hnsw_bit_vectors_format::{
  HnswBitVectorsFormat, NAME as HNSW_BIT_VECTORS_FORMAT_NAME,
};
use crate::core::codecs::hnsw::hnsw_graph_provider::HnswGraphProvider;
use crate::core::codecs::knn_field_vectors_writer::VectorValueEnum;
use crate::core::codecs::knn_vectors_format::KnnVectorsFormat;
use crate::core::codecs::knn_vectors_reader::KnnVectorsReader;
use crate::core::codecs::knn_vectors_writer::KnnVectorsWriter;
use crate::core::codecs::lucene99::lucene99_hnsw_scalar_quantized_vectors_format::{
  Lucene99HnswScalarQuantizedVectorsFormat,
  NAME as LUCENE99_HNSW_SCALAR_QUANTIZED_VECTORS_FORMAT_NAME,
};
use crate::core::codecs::lucene99::lucene99_hnsw_vectors_format::Lucene99HnswVectorsFormat;
use crate::core::codecs::lucene99::lucene99_scalar_quantized_vectors_format::{
  Lucene99ScalarQuantizedVectorsFormat, NAME as LUCENE99_SCALAR_QUANTIZED_VECTORS_FORMAT_NAME,
};
use crate::core::index::byte_vector_values::ByteVectorValues;
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::float_vector_values::FloatVectorValues;
use crate::core::index::index_reader::Identity;
use crate::core::index::knn_vector_values::KnnVectorValues;
use crate::core::index::merge_state::MergeState;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::sorter::DocMap;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::search::vector_scorer::VectorScorer;
use crate::core::store::directory::Directory;
use crate::core::store::{IndexInput, IndexOutput};
use crate::core::util::HasIdentity;
use crate::core::util::accountable::Accountable;
use crate::core::util::bits::Bits;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::hnsw::hnsw_graph::{HnswGraph, NodesIterator};
use crate::core::util::hnsw::neighbor_array::NeighborArray;
use crate::core::util::quantization::scalar_quantizer::ScalarQuantizer;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

const LUCENE99_HNSW_VECTORS_FORMAT_NAME: &str = "Lucene99HnswVectorsFormat";

/// Static-dispatch registry for the production [`KnnVectorsFormat`] implementations.
#[derive(Clone)]
pub enum KnnVectorsFormats {
  Lucene99Hnsw(Arc<Lucene99HnswVectorsFormat>),
  Lucene99ScalarQuantized(Arc<Lucene99ScalarQuantizedVectorsFormat>),
  Lucene99HnswScalarQuantized(Arc<Lucene99HnswScalarQuantizedVectorsFormat>),
  HnswBit(Arc<HnswBitVectorsFormat>),
}

impl From<Lucene99HnswVectorsFormat> for KnnVectorsFormats {
  fn from(format: Lucene99HnswVectorsFormat) -> Self {
    Self::Lucene99Hnsw(Arc::new(format))
  }
}

impl From<Lucene99ScalarQuantizedVectorsFormat> for KnnVectorsFormats {
  fn from(format: Lucene99ScalarQuantizedVectorsFormat) -> Self {
    Self::Lucene99ScalarQuantized(Arc::new(format))
  }
}

impl From<Lucene99HnswScalarQuantizedVectorsFormat> for KnnVectorsFormats {
  fn from(format: Lucene99HnswScalarQuantizedVectorsFormat) -> Self {
    Self::Lucene99HnswScalarQuantized(Arc::new(format))
  }
}

impl From<HnswBitVectorsFormat> for KnnVectorsFormats {
  fn from(format: HnswBitVectorsFormat) -> Self {
    Self::HnswBit(Arc::new(format))
  }
}

impl Display for KnnVectorsFormats {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Lucene99Hnsw(format) => Display::fmt(format.as_ref(), f),
      Self::Lucene99ScalarQuantized(format) => Display::fmt(format.as_ref(), f),
      Self::Lucene99HnswScalarQuantized(format) => Display::fmt(format.as_ref(), f),
      Self::HnswBit(format) => Display::fmt(format.as_ref(), f),
    }
  }
}

impl HasIdentity for KnnVectorsFormats {
  fn identity(&self) -> &Identity {
    match self {
      Self::Lucene99Hnsw(format) => format.identity(),
      Self::Lucene99ScalarQuantized(format) => format.identity(),
      Self::Lucene99HnswScalarQuantized(format) => format.identity(),
      Self::HnswBit(format) => format.identity(),
    }
  }
}

type Lucene99HnswVectorsWriter<O> =
  <Lucene99HnswVectorsFormat as KnnVectorsFormat>::KnnVectorsWriter<O>;
type Lucene99ScalarQuantizedVectorsWriter<O> =
  <Lucene99ScalarQuantizedVectorsFormat as KnnVectorsFormat>::KnnVectorsWriter<O>;
type Lucene99HnswScalarQuantizedVectorsWriter<O> =
  <Lucene99HnswScalarQuantizedVectorsFormat as KnnVectorsFormat>::KnnVectorsWriter<O>;
type HnswBitVectorsWriter<O> = <HnswBitVectorsFormat as KnnVectorsFormat>::KnnVectorsWriter<O>;

type Lucene99HnswVectorsReader<I> =
  <Lucene99HnswVectorsFormat as KnnVectorsFormat>::KnnVectorsReader<I>;
type Lucene99ScalarQuantizedVectorsReader<I> =
  <Lucene99ScalarQuantizedVectorsFormat as KnnVectorsFormat>::KnnVectorsReader<I>;
type Lucene99HnswScalarQuantizedVectorsReader<I> =
  <Lucene99HnswScalarQuantizedVectorsFormat as KnnVectorsFormat>::KnnVectorsReader<I>;
type HnswBitVectorsReader<I> = <HnswBitVectorsFormat as KnnVectorsFormat>::KnnVectorsReader<I>;

type Lucene99HnswFloatVectorValues<I> =
  <Lucene99HnswVectorsReader<I> as KnnVectorsReader>::FloatVectorValues;
type Lucene99ScalarQuantizedFloatVectorValues<I> =
  <Lucene99ScalarQuantizedVectorsReader<I> as KnnVectorsReader>::FloatVectorValues;
type HnswBitFloatVectorValues<I> = <HnswBitVectorsReader<I> as KnnVectorsReader>::FloatVectorValues;

type Lucene99HnswFloatVectorScorer<I> =
  <Lucene99HnswFloatVectorValues<I> as FloatVectorValues>::VectorScorer;
type Lucene99ScalarQuantizedFloatVectorScorer<I> =
  <Lucene99ScalarQuantizedFloatVectorValues<I> as FloatVectorValues>::VectorScorer;
type HnswBitFloatVectorScorer<I> = <HnswBitFloatVectorValues<I> as FloatVectorValues>::VectorScorer;

pub enum KnnVectorsFormatsFloatVectorScorer<I: IndexInput> {
  Lucene99Hnsw(Lucene99HnswFloatVectorScorer<I>),
  Lucene99ScalarQuantized(Lucene99ScalarQuantizedFloatVectorScorer<I>),
  HnswBit(HnswBitFloatVectorScorer<I>),
}

impl<I: IndexInput> VectorScorer for KnnVectorsFormatsFloatVectorScorer<I> {
  fn score(&self) -> Result<f32> {
    match self {
      Self::Lucene99Hnsw(scorer) => scorer.score(),
      Self::Lucene99ScalarQuantized(scorer) => scorer.score(),
      Self::HnswBit(scorer) => scorer.score(),
    }
  }

  type DocIdSetIteratorRef<'a>
    = <Lucene99HnswFloatVectorScorer<I> as VectorScorer>::DocIdSetIteratorRef<'a>
  where
    Self: 'a;

  fn iterator(&self) -> Self::DocIdSetIteratorRef<'_> {
    match self {
      Self::Lucene99Hnsw(scorer) => scorer.iterator(),
      Self::Lucene99ScalarQuantized(scorer) => scorer.iterator(),
      Self::HnswBit(scorer) => scorer.iterator(),
    }
  }

  type DocIdSetIteratorMut<'a>
    = <Lucene99HnswFloatVectorScorer<I> as VectorScorer>::DocIdSetIteratorMut<'a>
  where
    Self: 'a;

  fn iterator_mut(&mut self) -> Self::DocIdSetIteratorMut<'_> {
    match self {
      Self::Lucene99Hnsw(scorer) => scorer.iterator_mut(),
      Self::Lucene99ScalarQuantized(scorer) => scorer.iterator_mut(),
      Self::HnswBit(scorer) => scorer.iterator_mut(),
    }
  }
}

pub enum KnnVectorsFormatsFloatVectorValues<I: IndexInput> {
  Lucene99Hnsw(Lucene99HnswFloatVectorValues<I>),
  Lucene99ScalarQuantized(Lucene99ScalarQuantizedFloatVectorValues<I>),
  HnswBit(HnswBitFloatVectorValues<I>),
}

impl<I: IndexInput> KnnVectorValues for KnnVectorsFormatsFloatVectorValues<I> {
  fn dimension(&self) -> usize {
    match self {
      Self::Lucene99Hnsw(values) => values.dimension(),
      Self::Lucene99ScalarQuantized(values) => values.dimension(),
      Self::HnswBit(values) => values.dimension(),
    }
  }

  fn size(&self) -> usize {
    match self {
      Self::Lucene99Hnsw(values) => values.size(),
      Self::Lucene99ScalarQuantized(values) => values.size(),
      Self::HnswBit(values) => values.size(),
    }
  }

  fn ord_to_doc(&self, ord: usize) -> Result<usize> {
    match self {
      Self::Lucene99Hnsw(values) => values.ord_to_doc(ord),
      Self::Lucene99ScalarQuantized(values) => values.ord_to_doc(ord),
      Self::HnswBit(values) => values.ord_to_doc(ord),
    }
  }

  type KnnVectorValues = Self;

  fn copy(&self) -> Result<Self::KnnVectorValues> {
    match self {
      Self::Lucene99Hnsw(values) => values.copy().map(Self::Lucene99Hnsw),
      Self::Lucene99ScalarQuantized(values) => values.copy().map(Self::Lucene99ScalarQuantized),
      Self::HnswBit(values) => values.copy().map(Self::HnswBit),
    }
  }

  fn get_vector_byte_length(&self) -> usize {
    match self {
      Self::Lucene99Hnsw(values) => values.get_vector_byte_length(),
      Self::Lucene99ScalarQuantized(values) => values.get_vector_byte_length(),
      Self::HnswBit(values) => values.get_vector_byte_length(),
    }
  }

  fn get_encoding(&self) -> crate::core::index::vector_encoding::VectorEncoding {
    match self {
      Self::Lucene99Hnsw(values) => KnnVectorValues::get_encoding(values),
      Self::Lucene99ScalarQuantized(values) => KnnVectorValues::get_encoding(values),
      Self::HnswBit(values) => KnnVectorValues::get_encoding(values),
    }
  }

  type Bits<'a, B>
    = <Lucene99HnswFloatVectorValues<I> as KnnVectorValues>::Bits<'a, B>
  where
    B: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B>(&'a self, accept_docs: Option<B>) -> Option<Self::Bits<'a, B>>
  where
    B: Bits,
  {
    match self {
      Self::Lucene99Hnsw(values) => values.get_accept_ords(accept_docs),
      Self::Lucene99ScalarQuantized(values) => values.get_accept_ords(accept_docs),
      Self::HnswBit(values) => values.get_accept_ords(accept_docs),
    }
  }

  type DocIndexIterator = <Lucene99HnswFloatVectorValues<I> as KnnVectorValues>::DocIndexIterator;

  fn iterator(&self) -> Result<Self::DocIndexIterator> {
    match self {
      Self::Lucene99Hnsw(values) => values.iterator(),
      Self::Lucene99ScalarQuantized(values) => values.iterator(),
      Self::HnswBit(values) => values.iterator(),
    }
  }
}

impl<I: IndexInput> FloatVectorValues for KnnVectorsFormatsFloatVectorValues<I> {
  fn vector_value(&self, ord: usize) -> Result<std::borrow::Cow<'_, VectorValueEnum>> {
    match self {
      Self::Lucene99Hnsw(values) => values.vector_value(ord),
      Self::Lucene99ScalarQuantized(values) => values.vector_value(ord),
      Self::HnswBit(values) => values.vector_value(ord),
    }
  }

  type FloatVectorValues = Self;

  fn float_copy(&self) -> Result<Option<Self::FloatVectorValues>> {
    match self {
      Self::Lucene99Hnsw(values) => values
        .float_copy()
        .map(|values| values.map(Self::Lucene99Hnsw)),
      Self::Lucene99ScalarQuantized(values) => values
        .float_copy()
        .map(|values| values.map(Self::Lucene99ScalarQuantized)),
      Self::HnswBit(values) => values.float_copy().map(|values| values.map(Self::HnswBit)),
    }
  }

  type VectorScorer = KnnVectorsFormatsFloatVectorScorer<I>;

  fn scorer(&self, target: Vec<f32>) -> Result<Option<Self::VectorScorer>> {
    match self {
      Self::Lucene99Hnsw(values) => values
        .scorer(target)
        .map(|scorer| scorer.map(KnnVectorsFormatsFloatVectorScorer::Lucene99Hnsw)),
      Self::Lucene99ScalarQuantized(values) => values
        .scorer(target)
        .map(|scorer| scorer.map(KnnVectorsFormatsFloatVectorScorer::Lucene99ScalarQuantized)),
      Self::HnswBit(values) => values
        .scorer(target)
        .map(|scorer| scorer.map(KnnVectorsFormatsFloatVectorScorer::HnswBit)),
    }
  }

  fn get_encoding(&self) -> crate::core::index::vector_encoding::VectorEncoding {
    match self {
      Self::Lucene99Hnsw(values) => FloatVectorValues::get_encoding(values),
      Self::Lucene99ScalarQuantized(values) => FloatVectorValues::get_encoding(values),
      Self::HnswBit(values) => FloatVectorValues::get_encoding(values),
    }
  }

  fn get_vectors_mut(&mut self) -> Result<&mut Vec<VectorValueEnum>> {
    match self {
      Self::Lucene99Hnsw(values) => values.get_vectors_mut(),
      Self::Lucene99ScalarQuantized(values) => values.get_vectors_mut(),
      Self::HnswBit(values) => values.get_vectors_mut(),
    }
  }

  fn get_vectors(&self) -> Result<&[VectorValueEnum]> {
    match self {
      Self::Lucene99Hnsw(values) => values.get_vectors(),
      Self::Lucene99ScalarQuantized(values) => values.get_vectors(),
      Self::HnswBit(values) => values.get_vectors(),
    }
  }

  fn get_vectors_capacity(&self) -> Result<usize> {
    match self {
      Self::Lucene99Hnsw(values) => values.get_vectors_capacity(),
      Self::Lucene99ScalarQuantized(values) => values.get_vectors_capacity(),
      Self::HnswBit(values) => values.get_vectors_capacity(),
    }
  }
}

type Lucene99HnswByteVectorValues<I> =
  <Lucene99HnswVectorsReader<I> as KnnVectorsReader>::ByteVectorValues;
type HnswBitByteVectorValues<I> = <HnswBitVectorsReader<I> as KnnVectorsReader>::ByteVectorValues;

type Lucene99HnswByteVectorScorer<I> =
  <Lucene99HnswByteVectorValues<I> as ByteVectorValues>::VectorScorer;
type HnswBitByteVectorScorer<I> = <HnswBitByteVectorValues<I> as ByteVectorValues>::VectorScorer;

pub enum KnnVectorsFormatsByteVectorScorer<I: IndexInput> {
  Lucene99Hnsw(Lucene99HnswByteVectorScorer<I>),
  HnswBit(HnswBitByteVectorScorer<I>),
}

impl<I: IndexInput> VectorScorer for KnnVectorsFormatsByteVectorScorer<I> {
  fn score(&self) -> Result<f32> {
    match self {
      Self::Lucene99Hnsw(scorer) => scorer.score(),
      Self::HnswBit(scorer) => scorer.score(),
    }
  }

  type DocIdSetIteratorRef<'a>
    = <Lucene99HnswByteVectorScorer<I> as VectorScorer>::DocIdSetIteratorRef<'a>
  where
    Self: 'a;

  fn iterator(&self) -> Self::DocIdSetIteratorRef<'_> {
    match self {
      Self::Lucene99Hnsw(scorer) => scorer.iterator(),
      Self::HnswBit(scorer) => scorer.iterator(),
    }
  }

  type DocIdSetIteratorMut<'a>
    = <Lucene99HnswByteVectorScorer<I> as VectorScorer>::DocIdSetIteratorMut<'a>
  where
    Self: 'a;

  fn iterator_mut(&mut self) -> Self::DocIdSetIteratorMut<'_> {
    match self {
      Self::Lucene99Hnsw(scorer) => scorer.iterator_mut(),
      Self::HnswBit(scorer) => scorer.iterator_mut(),
    }
  }
}

pub enum KnnVectorsFormatsByteVectorValues<I: IndexInput> {
  Lucene99Hnsw(Lucene99HnswByteVectorValues<I>),
  HnswBit(HnswBitByteVectorValues<I>),
}

impl<I: IndexInput> KnnVectorValues for KnnVectorsFormatsByteVectorValues<I> {
  fn dimension(&self) -> usize {
    match self {
      Self::Lucene99Hnsw(values) => values.dimension(),
      Self::HnswBit(values) => values.dimension(),
    }
  }

  fn size(&self) -> usize {
    match self {
      Self::Lucene99Hnsw(values) => values.size(),
      Self::HnswBit(values) => values.size(),
    }
  }

  fn ord_to_doc(&self, ord: usize) -> Result<usize> {
    match self {
      Self::Lucene99Hnsw(values) => values.ord_to_doc(ord),
      Self::HnswBit(values) => values.ord_to_doc(ord),
    }
  }

  type KnnVectorValues = Self;

  fn copy(&self) -> Result<Self::KnnVectorValues> {
    match self {
      Self::Lucene99Hnsw(values) => values.copy().map(Self::Lucene99Hnsw),
      Self::HnswBit(values) => values.copy().map(Self::HnswBit),
    }
  }

  fn get_vector_byte_length(&self) -> usize {
    match self {
      Self::Lucene99Hnsw(values) => values.get_vector_byte_length(),
      Self::HnswBit(values) => values.get_vector_byte_length(),
    }
  }

  fn get_encoding(&self) -> crate::core::index::vector_encoding::VectorEncoding {
    match self {
      Self::Lucene99Hnsw(values) => KnnVectorValues::get_encoding(values),
      Self::HnswBit(values) => KnnVectorValues::get_encoding(values),
    }
  }

  type Bits<'a, B>
    = <Lucene99HnswByteVectorValues<I> as KnnVectorValues>::Bits<'a, B>
  where
    B: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B>(&'a self, accept_docs: Option<B>) -> Option<Self::Bits<'a, B>>
  where
    B: Bits,
  {
    match self {
      Self::Lucene99Hnsw(values) => values.get_accept_ords(accept_docs),
      Self::HnswBit(values) => values.get_accept_ords(accept_docs),
    }
  }

  type DocIndexIterator = <Lucene99HnswByteVectorValues<I> as KnnVectorValues>::DocIndexIterator;

  fn iterator(&self) -> Result<Self::DocIndexIterator> {
    match self {
      Self::Lucene99Hnsw(values) => values.iterator(),
      Self::HnswBit(values) => values.iterator(),
    }
  }
}

impl<I: IndexInput> ByteVectorValues for KnnVectorsFormatsByteVectorValues<I> {
  fn vector_value(&self, ord: usize) -> Result<std::borrow::Cow<'_, VectorValueEnum>> {
    match self {
      Self::Lucene99Hnsw(values) => values.vector_value(ord),
      Self::HnswBit(values) => values.vector_value(ord),
    }
  }

  type ByteVectorValues = Self;

  fn byte_copy(&self) -> Result<Option<Self::ByteVectorValues>> {
    match self {
      Self::Lucene99Hnsw(values) => values
        .byte_copy()
        .map(|values| values.map(Self::Lucene99Hnsw)),
      Self::HnswBit(values) => values.byte_copy().map(|values| values.map(Self::HnswBit)),
    }
  }

  type VectorScorer = KnnVectorsFormatsByteVectorScorer<I>;

  fn scorer(&self, target: Vec<u8>) -> Result<Option<Self::VectorScorer>> {
    match self {
      Self::Lucene99Hnsw(values) => values
        .scorer(target)
        .map(|scorer| scorer.map(KnnVectorsFormatsByteVectorScorer::Lucene99Hnsw)),
      Self::HnswBit(values) => values
        .scorer(target)
        .map(|scorer| scorer.map(KnnVectorsFormatsByteVectorScorer::HnswBit)),
    }
  }

  fn get_encoding(&self) -> crate::core::index::vector_encoding::VectorEncoding {
    match self {
      Self::Lucene99Hnsw(values) => ByteVectorValues::get_encoding(values),
      Self::HnswBit(values) => ByteVectorValues::get_encoding(values),
    }
  }

  fn get_vectors_mut(&mut self) -> Result<&mut Vec<VectorValueEnum>> {
    match self {
      Self::Lucene99Hnsw(values) => values.get_vectors_mut(),
      Self::HnswBit(values) => values.get_vectors_mut(),
    }
  }

  fn get_vectors(&self) -> Result<&[VectorValueEnum]> {
    match self {
      Self::Lucene99Hnsw(values) => values.get_vectors(),
      Self::HnswBit(values) => values.get_vectors(),
    }
  }

  fn get_vectors_capacity(&self) -> Result<usize> {
    match self {
      Self::Lucene99Hnsw(values) => values.get_vectors_capacity(),
      Self::HnswBit(values) => values.get_vectors_capacity(),
    }
  }
}

type Lucene99HnswGraph<I> = <Lucene99HnswVectorsReader<I> as HnswGraphProvider>::HnswGraph;
type Lucene99ScalarQuantizedHnswGraph<I> =
  <Lucene99ScalarQuantizedVectorsReader<I> as HnswGraphProvider>::HnswGraph;

pub enum KnnVectorsFormatsNodesIterator<I: IndexInput> {
  Hnsw(<Lucene99HnswGraph<I> as HnswGraph>::NodeIterator),
  ScalarQuantized(<Lucene99ScalarQuantizedHnswGraph<I> as HnswGraph>::NodeIterator),
}

impl<I: IndexInput> Iterator for KnnVectorsFormatsNodesIterator<I> {
  type Item = usize;

  fn next(&mut self) -> Option<Self::Item> {
    match self {
      Self::Hnsw(iterator) => iterator.next(),
      Self::ScalarQuantized(iterator) => iterator.next(),
    }
  }
}

impl<I: IndexInput> NodesIterator for KnnVectorsFormatsNodesIterator<I> {
  fn size(&self) -> usize {
    match self {
      Self::Hnsw(iterator) => iterator.size(),
      Self::ScalarQuantized(iterator) => iterator.size(),
    }
  }

  fn consume(&mut self, dest: &mut [usize]) -> Result<usize> {
    match self {
      Self::Hnsw(iterator) => iterator.consume(dest),
      Self::ScalarQuantized(iterator) => iterator.consume(dest),
    }
  }

  fn has_next(&self) -> bool {
    match self {
      Self::Hnsw(iterator) => iterator.has_next(),
      Self::ScalarQuantized(iterator) => iterator.has_next(),
    }
  }
}

pub enum KnnVectorsFormatsHnswGraph<I: IndexInput> {
  Hnsw(Lucene99HnswGraph<I>),
  ScalarQuantized(Lucene99ScalarQuantizedHnswGraph<I>),
}

impl<I: IndexInput> HnswGraph for KnnVectorsFormatsHnswGraph<I> {
  fn seek(&mut self, level: usize, target: usize) -> Result<()> {
    match self {
      Self::Hnsw(graph) => graph.seek(level, target),
      Self::ScalarQuantized(graph) => graph.seek(level, target),
    }
  }

  fn size(&self) -> usize {
    match self {
      Self::Hnsw(graph) => graph.size(),
      Self::ScalarQuantized(graph) => graph.size(),
    }
  }

  fn max_node_id(&self) -> Option<usize> {
    match self {
      Self::Hnsw(graph) => graph.max_node_id(),
      Self::ScalarQuantized(graph) => graph.max_node_id(),
    }
  }

  fn next_neighbor(&mut self) -> Result<usize> {
    match self {
      Self::Hnsw(graph) => graph.next_neighbor(),
      Self::ScalarQuantized(graph) => graph.next_neighbor(),
    }
  }

  fn num_levels(&self) -> Result<usize> {
    match self {
      Self::Hnsw(graph) => graph.num_levels(),
      Self::ScalarQuantized(graph) => graph.num_levels(),
    }
  }

  fn entry_node(&self) -> Result<Option<usize>> {
    match self {
      Self::Hnsw(graph) => graph.entry_node(),
      Self::ScalarQuantized(graph) => graph.entry_node(),
    }
  }

  type NodeIterator = KnnVectorsFormatsNodesIterator<I>;

  fn get_nodes_on_level(&mut self, level: usize) -> Result<Self::NodeIterator> {
    match self {
      Self::Hnsw(graph) => graph
        .get_nodes_on_level(level)
        .map(KnnVectorsFormatsNodesIterator::Hnsw),
      Self::ScalarQuantized(graph) => graph
        .get_nodes_on_level(level)
        .map(KnnVectorsFormatsNodesIterator::ScalarQuantized),
    }
  }

  fn get_neighbors_mut(&mut self, level: usize, node: usize) -> Result<&mut NeighborArray> {
    match self {
      Self::Hnsw(graph) => graph.get_neighbors_mut(level, node),
      Self::ScalarQuantized(graph) => graph.get_neighbors_mut(level, node),
    }
  }

  fn get_neighbors(&self, level: usize, node: usize) -> Result<&NeighborArray> {
    match self {
      Self::Hnsw(graph) => graph.get_neighbors(level, node),
      Self::ScalarQuantized(graph) => graph.get_neighbors(level, node),
    }
  }
}

pub enum KnnVectorsFormatsWriter<O: IndexOutput> {
  Lucene99Hnsw(Lucene99HnswVectorsWriter<O>),
  Lucene99ScalarQuantized(Lucene99ScalarQuantizedVectorsWriter<O>),
  Lucene99HnswScalarQuantized(Lucene99HnswScalarQuantizedVectorsWriter<O>),
  HnswBit(HnswBitVectorsWriter<O>),
}

impl<O: IndexOutput> Closeable for KnnVectorsFormatsWriter<O> {
  fn close(&mut self) -> Result<()> {
    match self {
      Self::Lucene99Hnsw(writer) => writer.close(),
      Self::Lucene99ScalarQuantized(writer) => writer.close(),
      Self::Lucene99HnswScalarQuantized(writer) => writer.close(),
      Self::HnswBit(writer) => writer.close(),
    }
  }
}

impl<O: IndexOutput> Accountable for KnnVectorsFormatsWriter<O> {
  fn ram_bytes_used(&self) -> Result<i64> {
    match self {
      Self::Lucene99Hnsw(writer) => writer.ram_bytes_used(),
      Self::Lucene99ScalarQuantized(writer) => writer.ram_bytes_used(),
      Self::Lucene99HnswScalarQuantized(writer) => writer.ram_bytes_used(),
      Self::HnswBit(writer) => writer.ram_bytes_used(),
    }
  }
}

impl<O: IndexOutput> KnnVectorsWriter<O> for KnnVectorsFormatsWriter<O> {
  fn add_field<D1, D2>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    field_info: Arc<FieldInfo>,
  ) -> Result<usize>
  where
    D1: Directory<IndexOutput = O>,
  {
    match self {
      Self::Lucene99Hnsw(writer) => writer.add_field(write_state, segment_info, field_info),
      Self::Lucene99ScalarQuantized(writer) => {
        writer.add_field(write_state, segment_info, field_info)
      },
      Self::Lucene99HnswScalarQuantized(writer) => {
        writer.add_field(write_state, segment_info, field_info)
      },
      Self::HnswBit(writer) => writer.add_field(write_state, segment_info, field_info),
    }
  }

  fn flush<DM>(&mut self, max_doc: i32, sort_map: Option<&DM>) -> Result<()>
  where
    DM: DocMap,
  {
    match self {
      Self::Lucene99Hnsw(writer) => writer.flush(max_doc, sort_map),
      Self::Lucene99ScalarQuantized(writer) => writer.flush(max_doc, sort_map),
      Self::Lucene99HnswScalarQuantized(writer) => writer.flush(max_doc, sort_map),
      Self::HnswBit(writer) => writer.flush(max_doc, sort_map),
    }
  }

  fn merge_one_field<D1, D2, CR>(
    &mut self,
    field_info: &Arc<FieldInfo>,
    merge_state: &MergeState<'_, D1, CR>,
    segment_write_state: &SegmentWriteState<&D2>,
  ) -> Result<()>
  where
    D2: Directory<IndexOutput = O>,
    CR: CodecReader,
  {
    match self {
      Self::Lucene99Hnsw(writer) => {
        writer.merge_one_field(field_info, merge_state, segment_write_state)
      },
      Self::Lucene99ScalarQuantized(writer) => {
        writer.merge_one_field(field_info, merge_state, segment_write_state)
      },
      Self::Lucene99HnswScalarQuantized(writer) => {
        writer.merge_one_field(field_info, merge_state, segment_write_state)
      },
      Self::HnswBit(writer) => writer.merge_one_field(field_info, merge_state, segment_write_state),
    }
  }

  fn finish(&mut self) -> Result<()> {
    match self {
      Self::Lucene99Hnsw(writer) => writer.finish(),
      Self::Lucene99ScalarQuantized(writer) => writer.finish(),
      Self::Lucene99HnswScalarQuantized(writer) => writer.finish(),
      Self::HnswBit(writer) => writer.finish(),
    }
  }

  fn merge<D1, D2, CR>(
    &mut self,
    merge_state: &MergeState<'_, D1, CR>,
    segment_write_state: &SegmentWriteState<&D2>,
  ) -> Result<i32>
  where
    D2: Directory<IndexOutput = O>,
    CR: CodecReader,
  {
    match self {
      Self::Lucene99Hnsw(writer) => writer.merge(merge_state, segment_write_state),
      Self::Lucene99ScalarQuantized(writer) => writer.merge(merge_state, segment_write_state),
      Self::Lucene99HnswScalarQuantized(writer) => writer.merge(merge_state, segment_write_state),
      Self::HnswBit(writer) => writer.merge(merge_state, segment_write_state),
    }
  }

  fn finish_merge<D, CR>(&self, merge_state: &MergeState<'_, D, CR>) -> Result<()>
  where
    CR: CodecReader,
  {
    match self {
      Self::Lucene99Hnsw(writer) => writer.finish_merge(merge_state),
      Self::Lucene99ScalarQuantized(writer) => writer.finish_merge(merge_state),
      Self::Lucene99HnswScalarQuantized(writer) => writer.finish_merge(merge_state),
      Self::HnswBit(writer) => writer.finish_merge(merge_state),
    }
  }

  fn add_value(
    &mut self,
    doc_id: i32,
    vector_value: &VectorValueEnum,
    field_vectors_writers_idx: usize,
  ) -> Result<()> {
    match self {
      Self::Lucene99Hnsw(writer) => {
        writer.add_value(doc_id, vector_value, field_vectors_writers_idx)
      },
      Self::Lucene99ScalarQuantized(writer) => {
        writer.add_value(doc_id, vector_value, field_vectors_writers_idx)
      },
      Self::Lucene99HnswScalarQuantized(writer) => {
        writer.add_value(doc_id, vector_value, field_vectors_writers_idx)
      },
      Self::HnswBit(writer) => writer.add_value(doc_id, vector_value, field_vectors_writers_idx),
    }
  }
}

pub enum KnnVectorsFormatsReader<I: IndexInput> {
  Lucene99Hnsw(Lucene99HnswVectorsReader<I>),
  Lucene99ScalarQuantized(Lucene99ScalarQuantizedVectorsReader<I>),
  Lucene99HnswScalarQuantized(Lucene99HnswScalarQuantizedVectorsReader<I>),
  HnswBit(HnswBitVectorsReader<I>),
}

impl<I: IndexInput> CloseableRef for KnnVectorsFormatsReader<I> {
  fn close(&self) -> Result<()> {
    match self {
      Self::Lucene99Hnsw(reader) => reader.close(),
      Self::Lucene99ScalarQuantized(reader) => reader.close(),
      Self::Lucene99HnswScalarQuantized(reader) => reader.close(),
      Self::HnswBit(reader) => reader.close(),
    }
  }
}

impl<I: IndexInput> HnswGraphProvider for KnnVectorsFormatsReader<I> {
  type HnswGraph = KnnVectorsFormatsHnswGraph<I>;

  fn is_hnsw_graph_provider(&self, field: &str) -> bool {
    match self {
      Self::Lucene99Hnsw(reader) => reader.is_hnsw_graph_provider(field),
      Self::Lucene99ScalarQuantized(reader) => reader.is_hnsw_graph_provider(field),
      Self::Lucene99HnswScalarQuantized(reader) => reader.is_hnsw_graph_provider(field),
      Self::HnswBit(reader) => reader.is_hnsw_graph_provider(field),
    }
  }

  fn get_graph(&self, field: &str) -> Result<Self::HnswGraph> {
    match self {
      Self::Lucene99Hnsw(reader) => reader
        .get_graph(field)
        .map(KnnVectorsFormatsHnswGraph::Hnsw),
      Self::Lucene99ScalarQuantized(reader) => reader
        .get_graph(field)
        .map(KnnVectorsFormatsHnswGraph::ScalarQuantized),
      Self::Lucene99HnswScalarQuantized(reader) => reader
        .get_graph(field)
        .map(KnnVectorsFormatsHnswGraph::Hnsw),
      Self::HnswBit(reader) => reader
        .get_graph(field)
        .map(KnnVectorsFormatsHnswGraph::Hnsw),
    }
  }
}

impl<I: IndexInput> KnnVectorsReader for KnnVectorsFormatsReader<I> {
  fn check_integrity(&self) -> Result<()> {
    match self {
      Self::Lucene99Hnsw(reader) => reader.check_integrity(),
      Self::Lucene99ScalarQuantized(reader) => reader.check_integrity(),
      Self::Lucene99HnswScalarQuantized(reader) => reader.check_integrity(),
      Self::HnswBit(reader) => reader.check_integrity(),
    }
  }

  type FloatVectorValues = KnnVectorsFormatsFloatVectorValues<I>;

  fn get_float_vector_values(&self, field: &str) -> Result<Self::FloatVectorValues> {
    match self {
      Self::Lucene99Hnsw(reader) => reader
        .get_float_vector_values(field)
        .map(KnnVectorsFormatsFloatVectorValues::Lucene99Hnsw),
      Self::Lucene99ScalarQuantized(reader) => reader
        .get_float_vector_values(field)
        .map(KnnVectorsFormatsFloatVectorValues::Lucene99ScalarQuantized),
      Self::Lucene99HnswScalarQuantized(reader) => reader
        .get_float_vector_values(field)
        .map(KnnVectorsFormatsFloatVectorValues::Lucene99ScalarQuantized),
      Self::HnswBit(reader) => reader
        .get_float_vector_values(field)
        .map(KnnVectorsFormatsFloatVectorValues::HnswBit),
    }
  }

  type ByteVectorValues = KnnVectorsFormatsByteVectorValues<I>;

  fn get_byte_vector_values(&self, field: &str) -> Result<Self::ByteVectorValues> {
    match self {
      Self::Lucene99Hnsw(reader) => reader
        .get_byte_vector_values(field)
        .map(KnnVectorsFormatsByteVectorValues::Lucene99Hnsw),
      Self::Lucene99ScalarQuantized(reader) => reader
        .get_byte_vector_values(field)
        .map(KnnVectorsFormatsByteVectorValues::Lucene99Hnsw),
      Self::Lucene99HnswScalarQuantized(reader) => reader
        .get_byte_vector_values(field)
        .map(KnnVectorsFormatsByteVectorValues::Lucene99Hnsw),
      Self::HnswBit(reader) => reader
        .get_byte_vector_values(field)
        .map(KnnVectorsFormatsByteVectorValues::HnswBit),
    }
  }

  fn get_quantization_state(&self, field: &str) -> Result<Option<ScalarQuantizer>> {
    match self {
      Self::Lucene99Hnsw(reader) => reader.get_quantization_state(field),
      Self::Lucene99ScalarQuantized(reader) => reader.get_quantization_state(field),
      Self::Lucene99HnswScalarQuantized(reader) => reader.get_quantization_state(field),
      Self::HnswBit(reader) => reader.get_quantization_state(field),
    }
  }

  fn is_flat_vectors_reader(&self, field: &str) -> bool {
    match self {
      Self::Lucene99Hnsw(reader) => reader.is_flat_vectors_reader(field),
      Self::Lucene99ScalarQuantized(reader) => reader.is_flat_vectors_reader(field),
      Self::Lucene99HnswScalarQuantized(reader) => reader.is_flat_vectors_reader(field),
      Self::HnswBit(reader) => reader.is_flat_vectors_reader(field),
    }
  }

  fn search_f32<B, K>(
    &self,
    field: &str,
    target: Vec<f32>,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
  {
    match self {
      Self::Lucene99Hnsw(reader) => reader.search_f32(field, target, knn_collector, accept_docs),
      Self::Lucene99ScalarQuantized(reader) => {
        reader.search_f32(field, target, knn_collector, accept_docs)
      },
      Self::Lucene99HnswScalarQuantized(reader) => {
        reader.search_f32(field, target, knn_collector, accept_docs)
      },
      Self::HnswBit(reader) => reader.search_f32(field, target, knn_collector, accept_docs),
    }
  }

  fn search_u8<B, K>(
    &self,
    field: &str,
    target: Vec<u8>,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
  {
    match self {
      Self::Lucene99Hnsw(reader) => reader.search_u8(field, target, knn_collector, accept_docs),
      Self::Lucene99ScalarQuantized(reader) => {
        reader.search_u8(field, target, knn_collector, accept_docs)
      },
      Self::Lucene99HnswScalarQuantized(reader) => {
        reader.search_u8(field, target, knn_collector, accept_docs)
      },
      Self::HnswBit(reader) => reader.search_u8(field, target, knn_collector, accept_docs),
    }
  }

  fn get_merge_instance(&self) -> Result<Option<Self>> {
    match self {
      Self::Lucene99Hnsw(reader) => reader
        .get_merge_instance()
        .map(|reader| reader.map(Self::Lucene99Hnsw)),
      Self::Lucene99ScalarQuantized(reader) => reader
        .get_merge_instance()
        .map(|reader| reader.map(Self::Lucene99ScalarQuantized)),
      Self::Lucene99HnswScalarQuantized(reader) => reader
        .get_merge_instance()
        .map(|reader| reader.map(Self::Lucene99HnswScalarQuantized)),
      Self::HnswBit(reader) => reader
        .get_merge_instance()
        .map(|reader| reader.map(Self::HnswBit)),
    }
  }

  fn finish_merge(&self) -> Result<()> {
    match self {
      Self::Lucene99Hnsw(reader) => reader.finish_merge(),
      Self::Lucene99ScalarQuantized(reader) => reader.finish_merge(),
      Self::Lucene99HnswScalarQuantized(reader) => reader.finish_merge(),
      Self::HnswBit(reader) => reader.finish_merge(),
    }
  }
}

impl KnnVectorsFormat for KnnVectorsFormats {
  fn get_name(&self) -> &str {
    match self {
      Self::Lucene99Hnsw(format) => format.get_name(),
      Self::Lucene99ScalarQuantized(format) => KnnVectorsFormat::get_name(format.as_ref()),
      Self::Lucene99HnswScalarQuantized(format) => format.get_name(),
      Self::HnswBit(format) => format.get_name(),
    }
  }

  type KnnVectorsWriter<O: IndexOutput> = KnnVectorsFormatsWriter<O>;

  fn fields_writer<D1, D2>(
    &self,
    state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::KnnVectorsWriter<D1::IndexOutput>>
  where
    D1: Directory,
  {
    match self {
      Self::Lucene99Hnsw(format) => {
        KnnVectorsFormat::fields_writer(format.as_ref(), state, segment_info)
          .map(KnnVectorsFormatsWriter::Lucene99Hnsw)
      },
      Self::Lucene99ScalarQuantized(format) => {
        KnnVectorsFormat::fields_writer(format.as_ref(), state, segment_info)
          .map(KnnVectorsFormatsWriter::Lucene99ScalarQuantized)
      },
      Self::Lucene99HnswScalarQuantized(format) => {
        KnnVectorsFormat::fields_writer(format.as_ref(), state, segment_info)
          .map(KnnVectorsFormatsWriter::Lucene99HnswScalarQuantized)
      },
      Self::HnswBit(format) => {
        KnnVectorsFormat::fields_writer(format.as_ref(), state, segment_info)
          .map(KnnVectorsFormatsWriter::HnswBit)
      },
    }
  }

  type KnnVectorsReader<I: IndexInput> = KnnVectorsFormatsReader<I>;

  fn fields_reader<D1, D2>(
    &self,
    state: &SegmentReadState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::KnnVectorsReader<D1::IndexInput>>
  where
    D1: Directory,
  {
    match self {
      Self::Lucene99Hnsw(format) => {
        KnnVectorsFormat::fields_reader(format.as_ref(), state, segment_info)
          .map(KnnVectorsFormatsReader::Lucene99Hnsw)
      },
      Self::Lucene99ScalarQuantized(format) => {
        KnnVectorsFormat::fields_reader(format.as_ref(), state, segment_info)
          .map(KnnVectorsFormatsReader::Lucene99ScalarQuantized)
      },
      Self::Lucene99HnswScalarQuantized(format) => {
        KnnVectorsFormat::fields_reader(format.as_ref(), state, segment_info)
          .map(KnnVectorsFormatsReader::Lucene99HnswScalarQuantized)
      },
      Self::HnswBit(format) => {
        KnnVectorsFormat::fields_reader(format.as_ref(), state, segment_info)
          .map(KnnVectorsFormatsReader::HnswBit)
      },
    }
  }

  fn get_max_dimensions(&self, field_name: &str) -> Result<usize> {
    match self {
      Self::Lucene99Hnsw(format) => format.get_max_dimensions(field_name),
      Self::Lucene99ScalarQuantized(format) => {
        KnnVectorsFormat::get_max_dimensions(format.as_ref(), field_name)
      },
      Self::Lucene99HnswScalarQuantized(format) => format.get_max_dimensions(field_name),
      Self::HnswBit(format) => format.get_max_dimensions(field_name),
    }
  }

  fn for_name(name: &str) -> Result<Arc<Self>> {
    match name {
      LUCENE99_HNSW_VECTORS_FORMAT_NAME => {
        Lucene99HnswVectorsFormat::for_name(name).map(|format| Arc::new(Self::Lucene99Hnsw(format)))
      },
      LUCENE99_SCALAR_QUANTIZED_VECTORS_FORMAT_NAME => {
        Lucene99ScalarQuantizedVectorsFormat::for_name(name)
          .map(|format| Arc::new(Self::Lucene99ScalarQuantized(format)))
      },
      LUCENE99_HNSW_SCALAR_QUANTIZED_VECTORS_FORMAT_NAME => {
        Lucene99HnswScalarQuantizedVectorsFormat::for_name(name)
          .map(|format| Arc::new(Self::Lucene99HnswScalarQuantized(format)))
      },
      HNSW_BIT_VECTORS_FORMAT_NAME => {
        HnswBitVectorsFormat::for_name(name).map(|format| Arc::new(Self::HnswBit(format)))
      },
      _ => Err(LuceneError::illegal_argument(format!(
        "Could not load vectors format named \"{name}\""
      ))),
    }
  }
}
