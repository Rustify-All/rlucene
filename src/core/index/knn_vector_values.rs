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
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::util::TryIntoInt;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};

/// This struct abstracts addressing of document vector values indexed as
/// [`KnnFloatVectorField`](crate::core::document::knn_float_vector_field::KnnFloatVectorField) or `KnnByteVectorField`.
pub trait KnnVectorValues {
    /// Return the dimension of the vectors
    fn dimension(&self) -> i32;
    /// Return the number of vectors for this field.
    ///
    /// # Returns
    /// The number of vectors returned by this iterator.
    fn size(&self) -> i32;
    /// Return the docid of the document indexed with the given vector ordinal.
    /// This default implementation returns the argument and is appropriate for
    /// dense values implementations where every doc has a single value.
    fn ord_to_doc(&self, ord: i32) -> i32 {
        ord
    }
    /// Creates a new copy of this [`KnnVectorValues`]. This is helpful when you
    /// need to access different values at once, to avoid overwriting the
    /// underlying vector returned.
    fn copy(&self) -> Result<Self>
    where
        Self: Sized;
    /// Returns the vector byte length, defaults to dimension multiplied by
    /// float byte size
    fn get_vector_byte_length(&self) -> usize {
        (self.dimension() * self.get_encoding().byte_size()) as usize
    }
    /// The vector encoding of these values.
    fn get_encoding(&self) -> VectorEncoding;

    type Bits: Bits;
    /// Returns a Bits accepting docs accepted by the argument and having a
    /// vector value
    fn get_accept_ords<B>(&self, accept_docs: B) -> Self::Bits
    where
        B: Bits;

    type DocIndexIterator: DocIndexIterator;
    ///  Create an iterator for this instance.
    fn iterator(&self) -> Result<Self::DocIndexIterator> {
        Err(LuceneError::unsupported_operation(""))
    }
}

pub(crate) struct BitsImpl<B, T>
where
    B: Bits,
    T: OrdToDoc,
{
    accept_docs: B,
    size: usize,
    map: T,
}
impl<B, T> BitsImpl<B, T>
where
    B: Bits,
    T: OrdToDoc,
{
    pub(crate) fn new(accept_docs: B, size: usize, map: T) -> Self {
        Self {
            accept_docs,
            size,
            map,
        }
    }
}
impl<B, T> Bits for BitsImpl<B, T>
where
    B: Bits,
    T: OrdToDoc,
{
    fn get(&self, index: usize) -> Result<bool> {
        self.accept_docs.get(self.map.ord_to_doc(index) as usize)
    }

    fn length(&self) -> usize {
        self.size
    }
}

/// A DocIdSetIterator that also provides an index() method tracking a distinct
/// ordinal for a vector associated with each doc.
pub trait DocIndexIterator: DocIdSetIterator {
    /// return the value index (aka "ordinal" or "ord") corresponding to the
    /// current doc
    fn index(&self) -> Result<i32>;
}

pub(crate) struct DocIndexIteratorImpl1 {
    doc: i32,
    size: i32,
}
impl DocIndexIteratorImpl1 {
    pub(crate) fn new(size: i32) -> Self {
        Self { doc: -1, size }
    }
}

impl DocIdSetIterator for DocIndexIteratorImpl1 {
    fn doc_id(&self) -> i32 {
        self.doc
    }

    fn next_doc(&mut self) -> Result<i32> {
        if self.doc >= self.size - 1 {
            self.doc = NO_MORE_DOCS;
        } else {
            self.doc += 1;
        }
        Ok(self.doc)
    }

    fn advance(&mut self, target: i32) -> Result<i32> {
        if target >= self.size {
            self.doc = NO_MORE_DOCS;
        } else {
            self.doc = target;
        }
        Ok(self.doc)
    }

    fn cost(&self) -> Result<i64> {
        Ok(self.size as i64)
    }
}

impl DocIndexIterator for DocIndexIteratorImpl1 {
    fn index(&self) -> Result<i32> {
        Ok(self.doc)
    }
}

pub(crate) struct DocIndexIteratorImpl2<D> {
    ord: i32,
    docs_with_field: D,
}

impl<D> DocIndexIteratorImpl2<D>
where
    D: DocIdSetIterator,
{
    pub(crate) fn new(docs_with_field: D) -> Self {
        Self {
            ord: -1,
            docs_with_field,
        }
    }
}

impl<D> DocIdSetIterator for DocIndexIteratorImpl2<D>
where
    D: DocIdSetIterator,
{
    fn doc_id(&self) -> i32 {
        self.docs_with_field.doc_id()
    }

    fn next_doc(&mut self) -> Result<i32> {
        if self.doc_id() == NO_MORE_DOCS {
            return Ok(NO_MORE_DOCS);
        }
        self.ord += 1;
        self.docs_with_field.next_doc()
    }

    fn advance(&mut self, target: i32) -> Result<i32> {
        self.docs_with_field.advance(target)
    }

    fn cost(&self) -> Result<i64> {
        self.docs_with_field.cost()
    }
}

impl<D> DocIndexIterator for DocIndexIteratorImpl2<D>
where
    D: DocIdSetIterator,
{
    fn index(&self) -> Result<i32> {
        Ok(self.ord)
    }
}

pub(crate) struct DocIndexIteratorImpl3<T>
where
    T: OrdToDoc,
{
    ord: i32,
    size: usize,
    map: T,
}

impl<T> DocIndexIteratorImpl3<T>
where
    T: OrdToDoc,
{
    pub(crate) fn new(size: usize, ord_to_doc: T) -> Self {
        Self {
            ord: -1,
            size,
            map: ord_to_doc,
        }
    }
}

impl<T> DocIdSetIterator for DocIndexIteratorImpl3<T>
where
    T: OrdToDoc,
{
    fn doc_id(&self) -> i32 {
        if self.ord == -1 {
            -1
        } else if self.ord == NO_MORE_DOCS {
            NO_MORE_DOCS
        } else {
            self.map.ord_to_doc(self.ord as usize)
        }
    }

    fn next_doc(&mut self) -> Result<i32> {
        if (self.ord + 1).try_convert()? >= self.size {
            self.ord = NO_MORE_DOCS;
        } else {
            self.ord += 1;
        }
        Ok(self.doc_id())
    }

    fn advance(&mut self, target: i32) -> Result<i32> {
        self.slow_advance(target)
    }

    fn cost(&self) -> Result<i64> {
        Ok(self.size as i64)
    }
}

impl<T> DocIndexIterator for DocIndexIteratorImpl3<T>
where
    T: OrdToDoc,
{
    fn index(&self) -> Result<i32> {
        Ok(self.ord)
    }
}

pub trait OrdToDoc {
    fn ord_to_doc(&self, ord: usize) -> i32;
}

pub(crate) fn create_dense_iterator(size: i32) -> DocIndexIteratorImpl1 {
    DocIndexIteratorImpl1::new(size)
}

/// creates an iterator from a docidsetiterator indicating which docs have
/// values, and for which ordinals increase monotonically with docid.
pub(crate) fn from_disi<D>(disi: D) -> DocIndexIteratorImpl2<D>
where
    D: DocIdSetIterator,
{
    DocIndexIteratorImpl2::new(disi)
}

///  Creates an iterator from this instance's ordinal-to-docid mapping which
/// must be monotonic (docid increases when ordinal does).
pub(crate) fn create_sparse_iterator<T>(size: usize, map: T) -> DocIndexIteratorImpl3<T>
where
    T: OrdToDoc,
{
    DocIndexIteratorImpl3::new(size, map)
}
