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
use crate::core::codecs::stored_fields_writer::StoredFieldsWriter;
use crate::core::codecs::term_vectors_reader::DefaultTermVectorsReader;
use crate::core::index::field_infos::{Builder, FieldInfos, FieldNumbers};
use crate::core::index::fields::Fields;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_reader::{IndexReader, IndexReaderBase, LeafReaderContextKind};
use crate::core::index::leaf_metadata::LeafMetaData;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::stored_field_visitor::StoredFieldVisitor;
use crate::core::index::stored_fields::{RawStoredFieldsReader, StoredFields};
use crate::core::index::term::Term;
use crate::core::index::term_vectors::{RawTermVectors, TermVectors};
use crate::core::index::terms::Terms;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::store::dummy::dummy_index_input::DummyIndexInput;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::iterator::IteratorExt;
use crate::core::util::version::LATEST;
use parking_lot::Mutex;
use std::collections::{BTreeMap, HashMap};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

/// A [`LeafReader`] which reads multiple, parallel indexes. Each index added
/// must have the same number of documents, but typically each contains
/// different fields. Deletions are taken from the first reader. Each document
/// contains the union of the fields of all documents with the same document
/// number. When searching, matches for a query term are from the first index
/// added that has the field.
///
/// This is useful, for example, with collections that have large fields which
/// change rarely and small fields that change more frequently. The smaller
/// fields may be re-indexed in a new index and both indexes may be searched
/// together.
///
/// # Warning
///
/// It is up to the caller to make sure all indexes are created and modified in
/// the same way. For example, if documents are added to one index, the same
/// documents need to be added in the same order to the other indexes. Failure
/// to do so results in undefined behavior.
#[derive(Clone)]
pub struct ParallelLeafReader<R>
where
  R: LeafReader,
{
  field_infos: Arc<FieldInfos>,
  parallel_reader_indices: Box<[usize]>,
  stored_fields_reader_indices: Box<[usize]>,
  complete_reader_set: Vec<R>,
  close_sub_readers: bool,
  max_doc: i32,
  num_docs: i32,
  has_deletions: bool,
  meta_data: LeafMetaData,
  tv_field_to_reader: Arc<BTreeMap<Arc<str>, usize>>,
  field_to_reader: BTreeMap<Arc<str>, usize>,
  terms_field_to_reader: HashMap<Arc<str>, usize>,
  index_base: IndexReaderBase,
  hook: ParallelLeafReaderHook,
}

#[derive(Clone, Copy)]
pub(crate) enum ParallelLeafReaderHook {
  Default,
  // Represents the anonymous ParallelLeafReader in
  // The synthetic leaf readers prepared by `ParallelCompositeReader` have a no-op close.
  ParallelCompositeReader,
}

impl<R> ParallelLeafReader<R>
where
  R: LeafReader,
{
  /// Creates a [`ParallelLeafReader`] based on the provided readers and
  /// automatically closes them when this reader is closed.
  pub fn new<Readers>(readers: Readers) -> Result<Self>
  where
    Readers: Into<Vec<R>>,
  {
    let readers = readers.into();
    Self::new_with_close_sub_readers(true, readers)
  }

  /// Creates a [`ParallelLeafReader`] based on the provided readers.
  pub fn new_with_close_sub_readers<Readers>(
    close_sub_readers: bool,
    readers: Readers,
  ) -> Result<Self>
  where
    Readers: Into<Vec<R>>,
  {
    let readers = readers.into();
    Self::new_internal(
      close_sub_readers,
      readers,
      None,
      ParallelLeafReaderHook::Default,
    )
  }

  /// Expert: creates a [`ParallelLeafReader`] based on the provided readers and
  /// stored-fields readers. When a document is loaded, only
  /// `stored_fields_readers` are used.
  pub fn new_with_stored_fields<Readers, StoredReaders>(
    close_sub_readers: bool,
    readers: Readers,
    stored_fields_readers: StoredReaders,
  ) -> Result<Self>
  where
    Readers: Into<Vec<R>>,
    StoredReaders: Into<Vec<R>>,
  {
    let stored_fields_readers = stored_fields_readers.into();
    let readers = readers.into();
    Self::new_internal(
      close_sub_readers,
      readers,
      Some(stored_fields_readers),
      ParallelLeafReaderHook::Default,
    )
  }

  pub(crate) fn new_with_stored_fields_and_hook(
    close_sub_readers: bool,
    readers: Vec<R>,
    stored_fields_readers: Vec<R>,
    hook: ParallelLeafReaderHook,
  ) -> Result<Self> {
    Self::new_internal(
      close_sub_readers,
      readers,
      Some(stored_fields_readers),
      hook,
    )
  }

  fn new_internal(
    close_sub_readers: bool,
    readers: Vec<R>,
    stored_fields_readers: Option<Vec<R>>,
    hook: ParallelLeafReaderHook,
  ) -> Result<Self> {
    if readers.is_empty()
      && stored_fields_readers
        .as_ref()
        .is_some_and(|readers| !readers.is_empty())
    {
      return Err(LuceneError::illegal_argument(
        "There must be at least one main reader if storedFieldsReaders are used.",
      ));
    }

    let (max_doc, num_docs, has_deletions) = match readers.first() {
      Some(first) => (first.max_doc()?, first.num_docs()?, first.has_deletions()?),
      None => (0, 0, false),
    };

    let complete_reader_capacity = readers.len()
      + stored_fields_readers
        .as_ref()
        .map_or(0, |readers| readers.len());
    let mut complete_reader_set = Vec::with_capacity(complete_reader_capacity);
    let mut parallel_reader_indices = Vec::with_capacity(readers.len());
    for reader in readers {
      let reader_index = complete_reader_set
        .iter()
        .position(|existing: &R| existing.index_base().ptr_eq(reader.index_base()))
        .unwrap_or_else(|| {
          complete_reader_set.push(reader);
          complete_reader_set.len() - 1
        });
      parallel_reader_indices.push(reader_index);
    }

    let stored_fields_reader_indices = match stored_fields_readers {
      Some(stored_fields_readers) => {
        let mut indices = Vec::with_capacity(stored_fields_readers.len());
        for reader in stored_fields_readers {
          let reader_index = complete_reader_set
            .iter()
            .position(|existing| existing.index_base().ptr_eq(reader.index_base()))
            .unwrap_or_else(|| {
              complete_reader_set.push(reader);
              complete_reader_set.len() - 1
            });
          indices.push(reader_index);
        }
        indices
      },
      None => parallel_reader_indices.clone(),
    };

    // Check compatibility.
    for reader in &complete_reader_set {
      if reader.max_doc()? != max_doc {
        return Err(LuceneError::illegal_argument(format!(
          "All readers must have same maxDoc: {max_doc}!={}",
          reader.max_doc()?
        )));
      }
    }

    let mut soft_deletes_field = None;
    let mut parent_field = None;
    for reader in &complete_reader_set {
      let field_infos = reader.get_field_infos()?;
      if soft_deletes_field.is_none() {
        soft_deletes_field = field_infos.get_soft_deletes_field().cloned();
      }
      if parent_field.is_none() {
        parent_field = field_infos.get_parent_field().cloned();
      }
    }

    let mut builder = Builder::new(Arc::new(Mutex::new(FieldNumbers::new(
      soft_deletes_field,
      parent_field,
    )?)));

    let mut index_sort = None;
    let mut created_version_major = -1;
    let mut tv_field_to_reader = BTreeMap::new();
    let mut field_to_reader = BTreeMap::new();
    let mut terms_field_to_reader = HashMap::with_capacity(complete_reader_set.len());

    // Build FieldInfos and field-to-reader maps.
    for (parallel_reader_index, complete_reader_index) in parallel_reader_indices.iter().enumerate()
    {
      let reader = &complete_reader_set[*complete_reader_index];
      let leaf_meta_data = reader.get_metadata()?;
      let leaf_index_sort = leaf_meta_data.get_sort();
      if index_sort.is_none() {
        index_sort = leaf_index_sort.clone();
      } else if let Some(index_sort) = &index_sort
        && let Some(leaf_index_sort) = leaf_index_sort
        && index_sort != leaf_index_sort
      {
        return Err(LuceneError::illegal_argument(format!(
          "cannot combine LeafReaders that have different index sorts: saw both sort={} and {}",
          index_sort, leaf_index_sort
        )));
      }

      if created_version_major == -1 {
        created_version_major = leaf_meta_data.get_created_version_major();
      } else if created_version_major != leaf_meta_data.get_created_version_major() {
        return Err(LuceneError::illegal_argument(format!(
          "cannot combine LeafReaders that have different creation versions: saw both version={} and {}",
          created_version_major,
          leaf_meta_data.get_created_version_major()
        )));
      }

      let reader_field_infos = reader.get_field_infos()?;
      for field_info in reader_field_infos.iter() {
        // NOTE: the first reader having a given field wins.
        if !field_to_reader.contains_key(field_info.name.as_str()) {
          builder.add_with_dv_gen(field_info.as_ref(), field_info.get_doc_values_gen())?;
          let field_name: Arc<str> = Arc::from(field_info.name.as_str());
          // Only add these if the reader responsible for that field name is
          // the current reader.
          if field_info.has_term_vectors() {
            tv_field_to_reader.insert(Arc::clone(&field_name), parallel_reader_index);
          }
          if field_info.get_index_options() != &IndexOptions::None {
            terms_field_to_reader.insert(Arc::clone(&field_name), *complete_reader_index);
          }
          field_to_reader.insert(field_name, *complete_reader_index);
        }
      }
    }

    if created_version_major == -1 {
      // Empty reader.
      created_version_major = LATEST.major;
    }

    let mut min_version = Some((*LATEST).clone());
    let mut has_blocks = false;
    for reader_index in &parallel_reader_indices {
      let reader = &complete_reader_set[*reader_index];
      let leaf_meta_data = reader.get_metadata()?;
      has_blocks |= leaf_meta_data.get_has_blocks();
      match leaf_meta_data.get_min_version() {
        None => {
          min_version = None;
          break;
        },
        Some(leaf_version)
          if min_version
            .as_ref()
            .is_some_and(|version| version.on_or_after(leaf_version)) =>
        {
          min_version = Some(leaf_version.clone());
        },
        Some(_) => {},
      }
    }

    let field_infos = Arc::new(builder.finish_owned()?);
    let meta_data = LeafMetaData::new(created_version_major, min_version, index_sort, has_blocks)?;
    let index_base = IndexReaderBase::new();

    // Do this finally so errors above do not affect ref counts.
    for reader in &complete_reader_set {
      if !close_sub_readers {
        reader.inc_ref()?;
      }
      reader.register_parent_reader(&index_base)?;
    }

    Ok(Self {
      field_infos,
      parallel_reader_indices: parallel_reader_indices.into_boxed_slice(),
      stored_fields_reader_indices: stored_fields_reader_indices.into_boxed_slice(),
      complete_reader_set,
      close_sub_readers,
      max_doc,
      num_docs,
      has_deletions,
      meta_data,
      tv_field_to_reader: Arc::new(tv_field_to_reader),
      field_to_reader,
      terms_field_to_reader,
      index_base,
      hook,
    })
  }
}

impl<R> Display for ParallelLeafReader<R>
where
  R: LeafReader,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "ParallelLeafReader(")?;
    for (index, reader) in self.complete_reader_set.iter().enumerate() {
      if index > 0 {
        write!(f, ", ")?;
      }
      write!(f, "{reader}")?;
    }
    write!(f, ")")
  }
}

// Single instance of this per ParallelLeafReader term-vectors instance.
pub struct ParallelFields<T> {
  fields: BTreeMap<String, Arc<T>>,
}

impl<T> ParallelFields<T>
where
  T: Terms,
{
  fn new() -> Self {
    Self {
      fields: BTreeMap::new(),
    }
  }

  fn add_field(&mut self, field_name: String, terms: T) {
    self.fields.insert(field_name, Arc::new(terms));
  }
}

impl<T> Fields for ParallelFields<T>
where
  T: Terms,
{
  type FieldIter<'a>
    = std::collections::btree_map::Keys<'a, String, Arc<T>>
  where
    Self: 'a;

  fn iterator(&self) -> Result<Self::FieldIter<'_>> {
    Ok(self.fields.keys())
  }

  type Terms = Arc<T>;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    Ok(self.fields.get(field).cloned())
  }

  fn size(&self) -> Result<i32> {
    debug_assert!(self.fields.len() <= i32::MAX as usize);
    Ok(self.fields.len() as i32)
  }
}

impl<'a, T> IteratorExt for std::collections::btree_map::Keys<'a, String, Arc<T>> {
  type Item = &'a String;

  fn next(&mut self) -> Result<Option<Self::Item>> {
    Ok(std::iter::Iterator::next(self))
  }

  fn has_next(&self) -> Result<bool> {
    Ok(self.len() != 0)
  }
}
pub struct ParallelStoredFields<S> {
  fields: Vec<S>,
}

impl<S> StoredFields for ParallelStoredFields<S>
where
  S: StoredFields,
{
  fn prefetch(&mut self, doc_id: i32) -> Result<()> {
    for reader in &mut self.fields {
      reader.prefetch(doc_id)?;
    }
    Ok(())
  }

  fn document_with_visitor<W, V>(
    &mut self,
    doc_id: i32,
    visitor: &mut V,
    writer: Option<&mut W>,
  ) -> Result<()>
  where
    W: StoredFieldsWriter,
    V: StoredFieldVisitor,
  {
    match writer {
      Some(writer) => {
        for reader in &mut self.fields {
          reader.document_with_visitor(doc_id, visitor, Some(&mut *writer))?;
        }
      },
      None => {
        for reader in &mut self.fields {
          reader.document_with_visitor::<W, _>(doc_id, visitor, None)?;
        }
      },
    }
    Ok(())
  }
}

impl<S> RawStoredFieldsReader for ParallelStoredFields<S>
where
  S: StoredFields,
{
  type IndexInput = DummyIndexInput;
}

pub struct ParallelTermVectors<TV> {
  reader_to_term_vectors: Vec<Option<TV>>,
  tv_field_to_reader: Arc<BTreeMap<Arc<str>, usize>>,
}

impl<TV> TermVectors for ParallelTermVectors<TV>
where
  TV: TermVectors,
{
  fn prefetch(&mut self, doc_id: i32) -> Result<()> {
    for term_vectors in self.reader_to_term_vectors.iter_mut().flatten() {
      term_vectors.prefetch(doc_id)?;
    }
    Ok(())
  }

  type Fields = ParallelFields<<TV::Fields as Fields>::Terms>;

  fn get(&mut self, doc: i32) -> Result<Option<Self::Fields>> {
    let mut parallel_fields = ParallelFields::new();
    for (field_name, reader_index) in self.tv_field_to_reader.iter() {
      if let Some(term_vectors) = self.reader_to_term_vectors[*reader_index].as_mut()
        && let Some(vector) = term_vectors.get_field_terms(doc, field_name.as_ref())?
      {
        parallel_fields.add_field(field_name.to_string(), vector);
      }
    }

    if parallel_fields.fields.is_empty() {
      Ok(None)
    } else {
      Ok(Some(parallel_fields))
    }
  }

  type Terms = Arc<<TV::Fields as Fields>::Terms>;

  fn get_field_terms(
    &mut self,
    doc: i32,
    field: &str,
  ) -> Result<Option<<Self::Fields as Fields>::Terms>> {
    self.default_get_field_terms(doc, field)
  }
}

impl<TV> RawTermVectors for ParallelTermVectors<TV>
where
  TV: TermVectors,
{
  type IndexInput = DummyIndexInput;

  fn raw_term_vectors_mut(&mut self) -> Result<&mut DefaultTermVectorsReader<Self::IndexInput>> {
    Err(LuceneError::illegal_state(
      "raw term vectors reader is not available",
    ))
  }

  fn raw_term_vectors(&self) -> Result<&DefaultTermVectorsReader<Self::IndexInput>> {
    Err(LuceneError::illegal_state(
      "raw term vectors reader is not available",
    ))
  }
}

impl<R> IndexReader for ParallelLeafReader<R>
where
  R: LeafReader,
{
  type ContextKind = LeafReaderContextKind;

  type TermVectors = ParallelTermVectors<R::TermVectors>;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    self.ensure_open()?;
    let mut term_vectors = Vec::with_capacity(self.parallel_reader_indices.len());
    for reader_index in &self.parallel_reader_indices {
      let reader = &self.complete_reader_set[*reader_index];
      if reader.get_field_infos()?.has_term_vectors() {
        term_vectors.push(Some(reader.term_vectors()?));
      } else {
        term_vectors.push(None);
      }
    }
    Ok(ParallelTermVectors {
      reader_to_term_vectors: term_vectors,
      tv_field_to_reader: self.tv_field_to_reader.clone(),
    })
  }

  fn max_doc(&self) -> Result<i32> {
    Ok(self.max_doc)
  }

  fn num_docs(&self) -> Result<i32> {
    Ok(self.num_docs)
  }

  type StoredFields = ParallelStoredFields<R::StoredFields>;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    self.ensure_open()?;
    let mut fields = Vec::with_capacity(self.stored_fields_reader_indices.len());
    for reader_index in &self.stored_fields_reader_indices {
      fields.push(self.complete_reader_set[*reader_index].stored_fields()?);
    }
    Ok(ParallelStoredFields { fields })
  }

  fn has_deletions(&self) -> Result<bool> {
    Ok(self.has_deletions)
  }

  fn do_close(&self) -> Result<()> {
    match self.hook {
      ParallelLeafReaderHook::Default => {},
      ParallelLeafReaderHook::ParallelCompositeReader => return Ok(()),
    }

    let mut first_error = None;
    for reader in &self.complete_reader_set {
      let result = if self.close_sub_readers {
        reader.close()
      } else {
        reader.dec_ref()
      };
      match result {
        Err(error) if error.is_io_error() => {
          if first_error.is_none() {
            first_error = Some(error);
          }
        },
        Err(error) => return Err(error),
        Ok(()) => {},
      }
    }
    match first_error {
      Some(error) => Err(error),
      None => Ok(()),
    }
  }

  type ReaderCacheHelper = R::ReaderCacheHelper;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    if self.parallel_reader_indices.len() == 1
      && self.stored_fields_reader_indices.len() == 1
      && self.parallel_reader_indices[0] == self.stored_fields_reader_indices[0]
    {
      self.complete_reader_set[self.parallel_reader_indices[0]].get_reader_cache_helper()
    } else {
      Ok(None)
    }
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

impl<R> LeafReader for ParallelLeafReader<R>
where
  R: LeafReader,
{
  type CacheHelper = R::CacheHelper;

  fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
    // Parallel reader instances can be short-lived, which would make caching
    // trappy, so do not cache on them unless they wrap a single reader, in
    // which case delegate.
    if self.parallel_reader_indices.len() == 1
      && self.stored_fields_reader_indices.len() == 1
      && self.parallel_reader_indices[0] == self.stored_fields_reader_indices[0]
    {
      self.complete_reader_set[self.parallel_reader_indices[0]].get_core_cache_helper()
    } else {
      Ok(None)
    }
  }

  type Terms = R::Terms;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    self.ensure_open()?;
    match self.terms_field_to_reader.get(field) {
      Some(reader_index) => self.complete_reader_set[*reader_index].terms(field),
      None => Ok(None),
    }
  }

  type NumericDocValues = R::NumericDocValues;

  fn get_numeric_doc_values(&self, field: &str) -> Result<Option<Self::NumericDocValues>> {
    self.ensure_open()?;
    match self.field_to_reader.get(field) {
      Some(reader_index) => self.complete_reader_set[*reader_index].get_numeric_doc_values(field),
      None => Ok(None),
    }
  }

  type BinaryDocValues = R::BinaryDocValues;

  fn get_binary_doc_values(&self, field: &str) -> Result<Option<Self::BinaryDocValues>> {
    self.ensure_open()?;
    match self.field_to_reader.get(field) {
      Some(reader_index) => self.complete_reader_set[*reader_index].get_binary_doc_values(field),
      None => Ok(None),
    }
  }

  type SortedDocValues = R::SortedDocValues;

  fn get_sorted_doc_values(&self, field: &str) -> Result<Option<Self::SortedDocValues>> {
    self.ensure_open()?;
    match self.field_to_reader.get(field) {
      Some(reader_index) => self.complete_reader_set[*reader_index].get_sorted_doc_values(field),
      None => Ok(None),
    }
  }

  type SortedNumericDocValues = R::SortedNumericDocValues;

  fn get_sorted_numeric_doc_values(
    &self,
    field: &str,
  ) -> Result<Option<Self::SortedNumericDocValues>> {
    self.ensure_open()?;
    match self.field_to_reader.get(field) {
      Some(reader_index) => {
        self.complete_reader_set[*reader_index].get_sorted_numeric_doc_values(field)
      },
      None => Ok(None),
    }
  }

  type SortedSetDocValues = R::SortedSetDocValues;

  fn get_sorted_set_doc_values(&self, field: &str) -> Result<Option<Self::SortedSetDocValues>> {
    self.ensure_open()?;
    match self.field_to_reader.get(field) {
      Some(reader_index) => {
        self.complete_reader_set[*reader_index].get_sorted_set_doc_values(field)
      },
      None => Ok(None),
    }
  }

  type NormNumericDocValues = R::NormNumericDocValues;

  fn get_norm_values(&self, field: &str) -> Result<Option<Self::NormNumericDocValues>> {
    self.ensure_open()?;
    match self.field_to_reader.get(field) {
      Some(reader_index) => self.complete_reader_set[*reader_index].get_norm_values(field),
      None => Ok(None),
    }
  }

  type DocValuesSkipper = R::DocValuesSkipper;

  fn get_doc_values_skipper(&self, field: &str) -> Result<Option<Self::DocValuesSkipper>> {
    self.ensure_open()?;
    match self.field_to_reader.get(field) {
      Some(reader_index) => self.complete_reader_set[*reader_index].get_doc_values_skipper(field),
      None => Ok(None),
    }
  }

  type FloatVectorValues = R::FloatVectorValues;

  fn get_float_vector_values(&self, field: &str) -> Result<Option<Self::FloatVectorValues>> {
    self.ensure_open()?;
    match self.field_to_reader.get(field) {
      Some(reader_index) => self.complete_reader_set[*reader_index].get_float_vector_values(field),
      None => Ok(None),
    }
  }

  type ByteVectorValues = R::ByteVectorValues;

  fn get_byte_vector_values(&self, field: &str) -> Result<Option<Self::ByteVectorValues>> {
    self.ensure_open()?;
    match self.field_to_reader.get(field) {
      Some(reader_index) => self.complete_reader_set[*reader_index].get_byte_vector_values(field),
      None => Ok(None),
    }
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
    self.ensure_open()?;
    if let Some(reader_index) = self.field_to_reader.get(field) {
      self.complete_reader_set[*reader_index].search_nearest_vectors_f32(
        field,
        target,
        knn_collector,
        accept_docs,
      )?;
    }
    Ok(())
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
    self.ensure_open()?;
    if let Some(reader_index) = self.field_to_reader.get(field) {
      self.complete_reader_set[*reader_index].search_nearest_vectors_u8(
        field,
        target,
        knn_collector,
        accept_docs,
      )?;
    }
    Ok(())
  }

  fn get_field_infos(&self) -> Result<Arc<FieldInfos>> {
    Ok(self.field_infos.clone())
  }

  type Bits = R::Bits;

  fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
    self.ensure_open()?;
    if self.has_deletions {
      self.complete_reader_set[self.parallel_reader_indices[0]].get_live_docs()
    } else {
      Ok(None)
    }
  }

  type PointValues = R::PointValues;

  fn get_point_values(&self, field: &str) -> Result<Option<Self::PointValues>> {
    self.ensure_open()?;
    match self.field_to_reader.get(field) {
      Some(reader_index) => self.complete_reader_set[*reader_index].get_point_values(field),
      None => Ok(None),
    }
  }

  fn check_integrity(&self) -> Result<()> {
    self.ensure_open()?;
    for reader in &self.complete_reader_set {
      reader.check_integrity()?;
    }
    Ok(())
  }

  fn get_metadata(&self) -> Result<&LeafMetaData> {
    Ok(&self.meta_data)
  }
}

impl<R> ParallelLeafReader<R>
where
  R: LeafReader,
{
  /// Returns the [`LeafReader`]s that were passed on initialization.
  pub fn get_parallel_readers(&self) -> Result<Vec<&R>> {
    self.ensure_open()?;
    Ok(
      self
        .parallel_reader_indices
        .iter()
        .map(|reader_index| &self.complete_reader_set[*reader_index])
        .collect(),
    )
  }
}
