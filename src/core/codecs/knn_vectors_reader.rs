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
use crate::core::codecs::DefaultKnnVectorsFormat;
use crate::core::codecs::hnsw::hnsw_graph_provider::HnswGraphProvider;
use crate::core::codecs::knn_vectors_format::KnnVectorsFormat;
use crate::core::index::byte_vector_values::ByteVectorValues;
use crate::core::index::byte_vector_values::ByteVectorValuesEnum2;
use crate::core::index::dummy::dummy_byte_vector_values::DummyByteVectorValues;
use crate::core::index::dummy::dummy_float_vector_values::DummyFloatVectorValues;
use crate::core::index::float_vector_values::FloatVectorValues;
use crate::core::index::float_vector_values::FloatVectorValuesEnum2;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::util::bits::Bits;
use crate::core::util::close::CloseableRef;
use crate::core::util::dummy::dummy_hnsw_graph::DummyHnswGraph;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::hnsw::hnsw_graph::HnswGraphEnum2;
use crate::core::util::quantization::scalar_quantizer::ScalarQuantizer;
use std::sync::Arc;

/// Reads vectors from an index.
pub trait KnnVectorsReader: HnswGraphProvider + CloseableRef {
  /// Checks consistency of this reader.
  ///
  /// Note that this may be costly in terms of I/O, e.g. may involve computing a checksum value
  /// against large data files.
  fn check_integrity(&self) -> Result<()>;

  type FloatVectorValues: FloatVectorValues;
  /// Returns the [`FloatVectorValues`] for the given `field`. The behavior is undefined if
  /// the given field doesn't have KNN vectors enabled on its `FieldInfo`. The return value is
  /// never `None`.
  fn get_float_vector_values(&self, field: &str) -> Result<Self::FloatVectorValues>;

  type ByteVectorValues: ByteVectorValues;
  /// Returns the [`ByteVectorValues`] for the given `field`. The behavior is undefined if
  /// the given field doesn't have KNN vectors enabled on its `FieldInfo`. The return value is
  /// never `None`.
  fn get_byte_vector_values(&self, field: &str) -> Result<Self::ByteVectorValues>;

  fn get_quantization_state(&self, _field: &str) -> Result<Option<ScalarQuantizer>> {
    Ok(None)
  }

  /// Returns whether this reader is a flat vectors reader.
  fn is_flat_vectors_reader(&self, _field: &str) -> bool {
    false
  }

  /// Return the k nearest neighbor documents as determined by comparison of their vector values for
  /// this field, to the given vector, by the field's similarity function. The score of each document
  /// is derived from the vector similarity in a way that ensures scores are positive and that a
  /// larger score corresponds to a higher ranking.
  ///
  /// The search is allowed to be approximate, meaning the results are not guaranteed to be the
  /// true k closest neighbors. For large values of k (for example when k is close to the total
  /// number of documents), the search may also retrieve fewer than k documents.
  ///
  /// The returned `TopDocs` will contain a `ScoreDoc` for each nearest neighbor, in
  /// order of their similarity to the query vector (decreasing scores). The `TotalHits`
  /// contains the number of documents visited during the search. If the search stopped early because
  /// it hit `visitedLimit`, it is indicated through the relation
  /// `TotalHits.Relation.GREATER_THAN_OR_EQUAL_TO`.
  ///
  /// The behavior is undefined if the given field doesn't have KNN vectors enabled on its `FieldInfo`.
  /// The return value is never `None`.
  ///
  /// # Arguments
  /// * `field` - the vector field to search
  /// * `target` - the vector-valued query
  /// * `knn_collector` - a KnnResults collector and relevant settings for gathering vector results
  /// * `accept_docs` - [`Bits`] that represents the allowed documents to match, or `None`
  ///   if they are all allowed to match.
  fn search_f32<B, K>(
    &self,
    field: &str,
    target: Vec<f32>,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector;

  /// Return the k nearest neighbor documents as determined by comparison of their vector values for
  /// this field, to the given vector, by the field's similarity function. The score of each document
  /// is derived from the vector similarity in a way that ensures scores are positive and that a
  /// larger score corresponds to a higher ranking.
  ///
  /// The search is allowed to be approximate, meaning the results are not guaranteed to be the
  /// true k closest neighbors. For large values of k (for example when k is close to the total
  /// number of documents), the search may also retrieve fewer than k documents.
  ///
  /// The returned `TopDocs` will contain a `ScoreDoc` for each nearest neighbor, in
  /// order of their similarity to the query vector (decreasing scores). The `TotalHits`
  /// contains the number of documents visited during the search. If the search stopped early because
  /// it hit `visitedLimit`, it is indicated through the relation
  /// `TotalHits.Relation.GREATER_THAN_OR_EQUAL_TO`.
  ///
  /// The behavior is undefined if the given field doesn't have KNN vectors enabled on its `FieldInfo`.
  /// The return value is never `None`.
  ///
  /// # Arguments
  /// * `field` - the vector field to search
  /// * `target` - the vector-valued query
  /// * `knn_collector` - a KnnResults collector and relevant settings for gathering vector results
  /// * `accept_docs` - [`Bits`] that represents the allowed documents to match, or `None`
  ///   if they are all allowed to match.
  fn search_u8<B, K>(
    &self,
    field: &str,
    target: Vec<u8>,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector;

  /// Returns an instance optimized for merging. This instance may only be consumed in the thread
  /// that called `get_merge_instance`.
  ///
  /// The default implementation returns `self`
  fn get_merge_instance(&self) -> Result<Option<Self>>
  where
    Self: Sized,
  {
    Ok(None)
  }

  /// Optional: reset or close merge resources used in the reader
  ///
  /// The default implementation is empty
  fn finish_merge(&self) -> Result<()> {
    Ok(())
  }
}

pub type DefaultKnnVectorsReader<T> =
  <DefaultKnnVectorsFormat as KnnVectorsFormat>::KnnVectorsReader<T>;

#[macro_export]
macro_rules! either_knn_vectors_reader {
    (
        $vis:vis $name:ident {
            float = $float_ty:ident,
            byte = $byte_ty:ident,
            graph = $graph_ty:ident;
            $( $Variant:ident : $T:ident ),+ $(,)?
        }
    ) => {
        $vis enum $name<$( $T ),+> {
            $( $Variant($T), )+
        }

        impl<$( $T ),+> $crate::core::util::close::CloseableRef for $name<$( $T ),+>
        where
            $( $T: $crate::core::codecs::knn_vectors_reader::KnnVectorsReader ),+
        {
            fn close(&self) -> $crate::core::util::error::lucene_error::Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.close(), )+
                }
            }
        }

        impl<$( $T ),+> $crate::core::codecs::hnsw::hnsw_graph_provider::HnswGraphProvider for $name<$( $T ),+>
        where
            $( $T: $crate::core::codecs::knn_vectors_reader::KnnVectorsReader ),+
        {
            type HnswGraph =
                $graph_ty<$( < $T as $crate::core::codecs::hnsw::hnsw_graph_provider::HnswGraphProvider >::HnswGraph ),+>;

            #[inline]
            fn is_hnsw_graph_provider(&self, field: &str) -> bool {
                match self {
                    $( Self::$Variant(inner) => inner.is_hnsw_graph_provider(field), )+
                }
            }

            #[inline]
            fn get_graph(&self, field: &str) -> $crate::core::util::error::lucene_error::Result<Self::HnswGraph> {
                match self {
                    $( Self::$Variant(inner) => inner.get_graph(field).map($graph_ty::$Variant), )+
                }
            }
        }

        impl<$( $T ),+> $crate::core::codecs::knn_vectors_reader::KnnVectorsReader for $name<$( $T ),+>
        where
            $( $T: $crate::core::codecs::knn_vectors_reader::KnnVectorsReader ),+
        {
            #[inline]
            fn check_integrity(&self) -> $crate::core::util::error::lucene_error::Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.check_integrity(), )+
                }
            }

            type FloatVectorValues =
                $float_ty<$( < $T as $crate::core::codecs::knn_vectors_reader::KnnVectorsReader >::FloatVectorValues ),+>;

            #[inline]
            fn get_float_vector_values(&self, field: &str) -> $crate::core::util::error::lucene_error::Result<Self::FloatVectorValues> {
                match self {
                    $( Self::$Variant(inner) => inner.get_float_vector_values(field).map($float_ty::$Variant), )+
                }
            }

            type ByteVectorValues =
                $byte_ty<$( < $T as $crate::core::codecs::knn_vectors_reader::KnnVectorsReader >::ByteVectorValues ),+>;

            #[inline]
            fn get_byte_vector_values(&self, field: &str) -> $crate::core::util::error::lucene_error::Result<Self::ByteVectorValues> {
                match self {
                    $( Self::$Variant(inner) => inner.get_byte_vector_values(field).map($byte_ty::$Variant), )+
                }
            }

            #[inline]
            fn get_quantization_state(&self, field: &str) -> $crate::core::util::error::lucene_error::Result<Option<$crate::core::util::quantization::scalar_quantizer::ScalarQuantizer>> {
                match self {
                    $( Self::$Variant(inner) => inner.get_quantization_state(field), )+
                }
            }

            #[inline]
            fn is_flat_vectors_reader(&self, field: &str) -> bool {
                match self {
                    $( Self::$Variant(inner) => inner.is_flat_vectors_reader(field), )+
                }
            }

            #[inline]
            fn search_f32<AcceptDocs, K>(
                &self,
                field: &str,
                target: Vec<f32>,
                knn_collector: &mut K,
                accept_docs: Option<AcceptDocs>,
            ) -> $crate::core::util::error::lucene_error::Result<()>
            where
                AcceptDocs: $crate::core::util::bits::Bits,
                K: $crate::core::search::knn_collector::KnnCollector,
            {
                match self {
                    $( Self::$Variant(inner) => inner.search_f32(field, target, knn_collector, accept_docs), )+
                }
            }

            #[inline]
            fn search_u8<AcceptDocs, K>(
                &self,
                field: &str,
                target: Vec<u8>,
                knn_collector: &mut K,
                accept_docs: Option<AcceptDocs>,
            ) -> $crate::core::util::error::lucene_error::Result<()>
            where
                AcceptDocs: $crate::core::util::bits::Bits,
                K: $crate::core::search::knn_collector::KnnCollector,
            {
                match self {
                    $( Self::$Variant(inner) => inner.search_u8(field, target, knn_collector, accept_docs), )+
                }
            }

            #[inline]
            fn get_merge_instance(&self) -> $crate::core::util::error::lucene_error::Result<Option<Self>>
            where
                Self: Sized,
            {
                match self {
                    $( Self::$Variant(inner) => inner.get_merge_instance().map(|opt| opt.map($name::$Variant)), )+
                }
            }

            #[inline]
            fn finish_merge(&self) -> $crate::core::util::error::lucene_error::Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.finish_merge(), )+
                }
            }
        }
    };
}

either_knn_vectors_reader!(
    pub KnnVectorsReaderEnum2 {
        float = FloatVectorValuesEnum2,
        byte = ByteVectorValuesEnum2,
        graph = HnswGraphEnum2;
        A: A, B: B,
    }
);

pub enum KnnVectorsReaderEnum {}
impl CloseableRef for KnnVectorsReaderEnum {}
impl HnswGraphProvider for KnnVectorsReaderEnum {
  type HnswGraph = DummyHnswGraph;
}
impl KnnVectorsReader for KnnVectorsReaderEnum {
  fn check_integrity(&self) -> Result<()> {
    todo!()
  }

  type FloatVectorValues = DummyFloatVectorValues;

  fn get_float_vector_values(&self, _field: &str) -> Result<Self::FloatVectorValues> {
    todo!()
  }

  type ByteVectorValues = DummyByteVectorValues;

  fn get_byte_vector_values(&self, _field: &str) -> Result<Self::ByteVectorValues> {
    todo!()
  }

  fn search_f32<B, K>(
    &self,
    _field: &str,
    _target: Vec<f32>,
    _knn_collector: &mut K,
    _accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
  {
    todo!()
  }

  fn search_u8<B, K>(
    &self,
    _field: &str,
    _target: Vec<u8>,
    _knn_collector: &mut K,
    _accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
  {
    todo!()
  }

  fn get_merge_instance(&self) -> Result<Option<Self>>
  where
    Self: Sized,
  {
    todo!()
  }

  fn finish_merge(&self) -> Result<()> {
    todo!()
  }
}

impl<T> HnswGraphProvider for Arc<T>
where
  T: HnswGraphProvider,
{
  type HnswGraph = T::HnswGraph;

  fn is_hnsw_graph_provider(&self, field: &str) -> bool {
    (**self).is_hnsw_graph_provider(field)
  }

  fn get_graph(&self, field: &str) -> Result<Self::HnswGraph> {
    (**self).get_graph(field)
  }
}

impl<T> KnnVectorsReader for Arc<T>
where
  T: KnnVectorsReader,
{
  fn check_integrity(&self) -> Result<()> {
    (**self).check_integrity()
  }

  type FloatVectorValues = T::FloatVectorValues;

  fn get_float_vector_values(&self, field: &str) -> Result<Self::FloatVectorValues> {
    (**self).get_float_vector_values(field)
  }

  type ByteVectorValues = T::ByteVectorValues;

  fn get_byte_vector_values(&self, field: &str) -> Result<Self::ByteVectorValues> {
    (**self).get_byte_vector_values(field)
  }

  fn get_quantization_state(&self, field: &str) -> Result<Option<ScalarQuantizer>> {
    (**self).get_quantization_state(field)
  }

  fn is_flat_vectors_reader(&self, field: &str) -> bool {
    (**self).is_flat_vectors_reader(field)
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
    (**self).search_f32(field, target, knn_collector, accept_docs)
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
    (**self).search_u8(field, target, knn_collector, accept_docs)
  }

  fn get_merge_instance(&self) -> Result<Option<Self>>
  where
    Self: Sized,
  {
    let v = match (**self).get_merge_instance()? {
      Some(v) => Arc::new(v),
      None => return Ok(None),
    };
    Ok(Some(v))
  }

  fn finish_merge(&self) -> Result<()> {
    (**self).finish_merge()
  }
}
