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
use crate::core::codecs::stored_fields_reader::DefaultStoredFieldsReader;
use crate::core::codecs::stored_fields_writer::StoredFieldsWriter;
use crate::core::codecs::term_vectors_reader::DefaultTermVectorsReader;
use crate::core::index::composite_reader::CompositeReader;
use crate::core::index::fields::Fields;
use crate::core::index::index_reader::{
    IRStoredFields, IRTermVectors, IndexReader, IndexReaderEnum,
};
use crate::core::index::index_writer::get_actual_max_docs;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::reader_util::ReaderUtil;
use crate::core::index::stored_field_visitor::StoredFieldVisitor;
use crate::core::index::stored_fields::{RawStoredFieldsReader, StoredFields};
use crate::core::index::term::Term;
use crate::core::index::term_vectors::{RawTermVectors, TermVectors};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::{Comparator, TryIntoInt};
use std::cmp::Ordering::Equal;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

/// Base trait for implementing [`CompositeReader`]s based on an array of sub-readers.
///
/// User code will most likely use `MultiReader` to build a composite reader
/// on a set of sub-readers (such as several `DirectoryReader`s).
///
/// For efficiency, in this API documents are often referred to via *document numbers*,
/// non-negative integers that uniquely identify documents in the index.
/// These document numbers are ephemeral — they may change as documents are added to
/// or deleted from an index. Clients should therefore **not rely** on a document
/// having the same number between sessions.
///
///
/// ## Thread Safety
///
/// **NOTE:** [`IndexReader`] instances are completely thread-safe, meaning multiple
/// threads can call any of its methods concurrently.
/// If your application requires external synchronization, you should **not**
/// synchronize on the `IndexReader` instance itself; instead, use your own (non-Lucene)
/// synchronization objects.
///
///
/// See also: `MultiReader`
///
/// *Lucene internal API*
pub trait BaseCompositeReader: CompositeReader {}
pub struct BaseCompositeReaderBase<LR, CR>
where
    LR: LeafReader + Clone,
    CR: CompositeReader,
{
    pub(crate) sub_reader: Arc<Vec<IndexReaderEnum<LR, CR>>>,
    starts: Arc<Vec<usize>>,
    max_doc: i32,
    num_docs: AtomicI32,
}
impl<LR, CR> BaseCompositeReaderBase<LR, CR>
where
    LR: LeafReader + Clone,
    CR: CompositeReader,
{
    pub fn with_leaf_readers<C>(
        sub_readers: Vec<LR>,
        sub_reader_sorter: &Option<Arc<C>>,
    ) -> Result<Self>
    where
        C: Comparator<LR>,
    {
        Self::build_from_sub_readers(sub_readers, sub_reader_sorter, IndexReaderEnum::Leaf)
    }

    pub fn with_composite_readers<C>(
        sub_readers: Vec<CR>,
        sub_reader_sorter: &Option<Arc<C>>,
    ) -> Result<Self>
    where
        C: Comparator<CR>,
    {
        Self::build_from_sub_readers(sub_readers, sub_reader_sorter, IndexReaderEnum::Composite)
    }
    /// Constructs a [`BaseCompositeReader`] on the given sub-readers.
    ///
    /// # Parameters
    ///
    /// * `sub_readers` – the wrapped sub-readers.
    ///   This vector is returned by [`get_sequential_sub_readers`](Self::get_sequential_sub_readers)
    ///   and used to resolve the correct sub-reader for docID-based methods.
    ///   **Please note:** this vector is **not** cloned and **not** protected for modification;
    ///   the subclass is responsible for doing this.
    ///
    /// * `sub_readers_sorter` – a comparator for sorting sub-readers.
    ///   If not `None`, this comparator is used to sort sub-readers before resolving doc IDs.
    fn build_from_sub_readers<IR, C, F>(
        mut sub_readers: Vec<IR>,
        sub_reader_sorter: &Option<Arc<C>>,
        to_index_reader_enum: F,
    ) -> Result<Self>
    where
        IR: IndexReader,
        C: Comparator<IR>,
        F: FnMut(IR) -> IndexReaderEnum<LR, CR>,
    {
        if let Some(sorter) = &sub_reader_sorter {
            let mut err: Option<LuceneError> = None;
            sub_readers.sort_by(|a, b| match sorter.compare(a, b) {
                Ok(v) => v.cmp(&0),
                Err(e) => {
                    if err.is_none() {
                        err = Some(e);
                    }
                    Equal
                },
            });
            if let Some(e) = err {
                return Err(e);
            }
        }

        let mut starts = vec![0usize; sub_readers.len() + 1];
        let mut max_doc: i64 = 0;

        for (i, reader) in sub_readers.iter().enumerate() {
            starts[i] = max_doc as usize;
            max_doc += reader.max_doc()? as i64;
        }

        let max_allowed = get_actual_max_docs();
        if max_doc > max_allowed as i64 {
            return Err(LuceneError::illegal_argument(format!(
                "Too many documents: composite IndexReaders cannot exceed {}, total maxDoc={}",
                max_allowed, max_doc
            )));
        }
        let max_doc_i32 = max_doc.try_convert()?;
        starts[sub_readers.len()] = max_doc_i32;

        let sub_reader = sub_readers.into_iter().map(to_index_reader_enum).collect();

        Ok(Self {
            sub_reader: Arc::new(sub_reader),
            starts: Arc::new(starts),
            max_doc: max_doc_i32 as i32,
            num_docs: AtomicI32::new(-1),
        })
    }

    pub fn term_vector(
        &self,
        reader: &impl BaseCompositeReader,
    ) -> Result<BCRTermVectorsImpl<LR, CR>> {
        reader.ensure_open()?;
        Ok(TermVectorsImpl::new(
            self.sub_reader.clone(),
            self.starts.clone(),
            self.max_doc,
        ))
    }
    pub fn num_docs(&self) -> Result<i32> {
        // Don't call ensureOpen() here (it could affect performance)
        // We want to compute numDocs() lazily so that creating a wrapper that hides
        // some documents isn't slow at wrapping time, but on the first time that
        // numDocs() is called. This can help as there are lots of use-cases of a
        // reader that don't involve calling numDocs().
        // However it's not crucial to make sure that we don't call numDocs() more
        // than once on the sub readers, since they likely cache numDocs() anyway,
        // hence the opaque read.
        // http://gee.cs.oswego.edu/dl/html/j9mm.html#opaquesec.
        let num_docs = self.num_docs.load(Ordering::Relaxed);
        if num_docs != -1 {
            return Ok(num_docs);
        }

        let mut num_docs: i32 = 0;
        for r in self.sub_reader.iter() {
            num_docs += r.num_docs()?;
        }

        debug_assert!(num_docs >= 0);
        self.num_docs.store(num_docs, Ordering::Relaxed);
        Ok(num_docs)
    }
    pub fn max_doc(&self) -> i32 {
        self.max_doc
    }
    pub fn stored_fields(
        &self,
        reader: &impl BaseCompositeReader,
    ) -> Result<BCRStoredFieldsImpl<LR, CR>> {
        reader.ensure_open()?;
        Ok(StoredFieldsImpl::new(
            self.sub_reader.clone(),
            self.starts.clone(),
            self.max_doc,
        ))
    }
    pub fn doc_freq(&self, term: &Term, reader: &impl BaseCompositeReader) -> Result<i32> {
        reader.ensure_open()?;

        let mut total: i32 = 0;
        for sub_reader in self.sub_reader.iter() {
            let sub = sub_reader.doc_freq(term)?;
            debug_assert!(sub >= 0);
            debug_assert!(sub <= sub_reader.get_doc_count(term.field())?);
            total += sub;
        }
        Ok(total)
    }
    pub fn total_term_freq(&self, term: &Term, reader: &impl BaseCompositeReader) -> Result<i64> {
        reader.ensure_open()?;

        let mut total: i64 = 0;
        for sub_reader in self.sub_reader.iter() {
            let sub = sub_reader.total_term_freq(term)?;
            debug_assert!(sub >= 0);
            debug_assert!(sub <= sub_reader.get_sum_total_term_freq(term.field())?);
            total += sub;
        }
        Ok(total)
    }

    pub fn get_sum_doc_freq(&self, field: &str, reader: &impl BaseCompositeReader) -> Result<i64> {
        reader.ensure_open()?;

        let mut total: i64 = 0;
        for sub_reader in self.sub_reader.iter() {
            let sub = sub_reader.get_sum_doc_freq(field)?;
            debug_assert!(sub >= 0);
            debug_assert!(sub <= sub_reader.get_sum_total_term_freq(field)?);
            total += sub;
        }
        Ok(total)
    }

    pub fn get_doc_count(&self, field: &str, reader: &impl BaseCompositeReader) -> Result<i32> {
        reader.ensure_open()?;

        let mut total: i32 = 0;
        for sub_reader in self.sub_reader.iter() {
            let sub = sub_reader.get_doc_count(field)?;
            debug_assert!(sub >= 0);
            debug_assert!(sub <= sub_reader.max_doc()?);
            total += sub;
        }
        Ok(total)
    }
    pub fn get_sum_total_term_freq(
        &self,
        field: &str,
        reader: &impl BaseCompositeReader,
    ) -> Result<i64> {
        reader.ensure_open()?;

        let mut total: i64 = 0;
        for sub_reader in self.sub_reader.iter() {
            let sub = sub_reader.get_sum_total_term_freq(field)?;
            debug_assert!(sub >= 0);
            debug_assert!(sub >= sub_reader.get_sum_doc_freq(field)?);
            total += sub;
        }
        Ok(total)
    }
    /// Helper method for subclasses to get the docBase of the given sub-reader index.
    pub fn reader_base(&self, reader_index: usize) -> Result<usize> {
        if reader_index >= self.sub_reader.len() {
            return Err(LuceneError::illegal_argument(
                "readerIndex must be >= 0 and < getSequentialSubReaders().size()",
            ));
        }
        Ok(self.starts[reader_index])
    }
    pub fn get_sequential_sub_readers(&self) -> &[IndexReaderEnum<LR, CR>] {
        self.sub_reader.as_slice()
    }
}
pub type BCRTermVectorsImpl<IR, CR> = TermVectorsImpl<IR, CR>;
pub type BCRStoredFieldsImpl<IR, CR> = StoredFieldsImpl<IR, CR>;

pub struct TermVectorsImpl<LR, CR>
where
    LR: LeafReader + Clone,
    CR: CompositeReader,
{
    sub_reader: Arc<Vec<IndexReaderEnum<LR, CR>>>,
    starts: Arc<Vec<usize>>,
    sub_term_vectors: Vec<Option<IRTermVectors<LR, CR>>>,
    max_doc: i32,
}
impl<LR, CR> TermVectorsImpl<LR, CR>
where
    LR: LeafReader + Clone,
    CR: CompositeReader,
{
    pub fn new(
        sub_reader: Arc<Vec<IndexReaderEnum<LR, CR>>>,
        starts: Arc<Vec<usize>>,
        max_doc: i32,
    ) -> Self {
        let mut sub_term_vectors = Vec::with_capacity(starts.len());
        for _ in 0..sub_reader.len() {
            sub_term_vectors.push(None);
        }
        Self {
            sub_reader,
            starts,
            sub_term_vectors,
            max_doc,
        }
    }
}
impl<LR, CR> TermVectors for TermVectorsImpl<LR, CR>
where
    LR: LeafReader + Clone,
    CR: CompositeReader,
{
    fn prefetch(&mut self, doc_id: i32) -> Result<()> {
        let i = reader_index(doc_id, self.max_doc, self.starts.as_ref())?;
        match self.sub_term_vectors[i] {
            Some(ref mut tv) => tv.prefetch(doc_id - self.starts[i] as i32)?,
            None => {
                let mut tv_reader = self.sub_reader[i].term_vectors()?;
                tv_reader.prefetch(doc_id - self.starts[i] as i32)?;
                self.sub_term_vectors[i] = Some(tv_reader);
            },
        }
        Ok(())
    }

    type Fields = <IRTermVectors<LR, CR> as TermVectors>::Fields;

    fn get(&mut self, doc_id: i32) -> Result<Option<Self::Fields>> {
        let i = reader_index(doc_id, self.max_doc, self.starts.as_ref())?;

        if self.sub_term_vectors[i].is_none() {
            let tv = self.sub_reader[i].term_vectors()?;
            self.sub_term_vectors[i] = Some(tv);
        }
        let tv = self.sub_term_vectors[i].as_mut().unwrap();
        tv.get(doc_id - self.starts[i] as i32)
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

impl<LR, CR> RawTermVectors for TermVectorsImpl<LR, CR>
where
    LR: LeafReader + Clone,
    CR: CompositeReader,
{
    type IndexInput = <IRTermVectors<LR, CR> as RawTermVectors>::IndexInput;

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
pub struct StoredFieldsImpl<LR, CR>
where
    LR: LeafReader + Clone,
    CR: CompositeReader,
{
    sub_reader: Arc<Vec<IndexReaderEnum<LR, CR>>>,
    starts: Arc<Vec<usize>>,
    sub_stored_fields: Vec<Option<IRStoredFields<LR, CR>>>,
    max_doc: i32,
}

impl<LR, CR> StoredFieldsImpl<LR, CR>
where
    LR: LeafReader + Clone,
    CR: CompositeReader,
{
    pub fn new(
        sub_reader: Arc<Vec<IndexReaderEnum<LR, CR>>>,
        starts: Arc<Vec<usize>>,
        max_doc: i32,
    ) -> Self {
        let mut sub_stored_fields = Vec::with_capacity(starts.len());
        for _ in 0..sub_reader.len() {
            sub_stored_fields.push(None);
        }
        Self {
            sub_reader,
            starts,
            sub_stored_fields,
            max_doc,
        }
    }
}

impl<LR, CR> StoredFields for StoredFieldsImpl<LR, CR>
where
    LR: LeafReader + Clone,
    CR: CompositeReader,
{
    fn prefetch(&mut self, doc_id: i32) -> Result<()> {
        let i = reader_index(doc_id, self.max_doc, self.starts.as_slice())?;

        match self.sub_stored_fields[i] {
            Some(ref mut sf) => sf.prefetch(doc_id - self.starts[i] as i32)?,
            None => {
                let mut sf_reader = self.sub_reader[i].stored_fields()?;
                sf_reader.prefetch(doc_id - self.starts[i] as i32)?;
                self.sub_stored_fields[i] = Some(sf_reader);
            },
        }

        Ok(())
    }

    fn document_with_visitor<S: StoredFieldsWriter>(
        &mut self,
        doc_id: i32,
        visitor: &mut impl StoredFieldVisitor,
        writer: Option<&mut S>,
    ) -> Result<()> {
        let i = reader_index(doc_id, self.max_doc, self.starts.as_ref())?;

        if self.sub_stored_fields[i].is_none() {
            let sf = self.sub_reader[i].stored_fields()?;
            self.sub_stored_fields[i] = Some(sf);
        }

        let sf = self.sub_stored_fields[i].as_mut().unwrap();
        sf.document_with_visitor(doc_id - self.starts[i] as i32, visitor, writer)
    }
}

impl<LR, CR> RawStoredFieldsReader for StoredFieldsImpl<LR, CR>
where
    LR: LeafReader + Clone,
    CR: CompositeReader,
    IRStoredFields<LR, CR>: RawStoredFieldsReader,
{
    type IndexInput = <IRStoredFields<LR, CR> as RawStoredFieldsReader>::IndexInput;

    fn raw_stored_fields(&mut self) -> Result<&mut DefaultStoredFieldsReader<Self::IndexInput>> {
        Err(LuceneError::illegal_state(
            "Raw stored fields are not available for composite readers",
        ))
    }
}
/// Helper method for subclasses to get the corresponding reader for a doc ID
pub fn reader_index(doc_id: i32, max_doc: i32, starts: &[usize]) -> Result<usize> {
    if doc_id < 0 || doc_id >= max_doc {
        return Err(LuceneError::illegal_argument(format!(
            "docID must be >= 0 and < maxDoc={} (got docID={})",
            max_doc, doc_id
        )));
    }
    let v = ReaderUtil::sub_index(doc_id as usize, starts);
    if v < 0 {
        return Err(LuceneError::illegal_state("index should >= 0"));
    }
    Ok(v as usize)
}
