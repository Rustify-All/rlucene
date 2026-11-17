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
use crate::core::index::base_composite_reader::{
  BCRStoredFieldsImpl, BCRTermVectorsImpl, BaseCompositeReader, BaseCompositeReaderBase,
};
use crate::core::index::composite_reader::CompositeReader;
use crate::core::index::index_reader::{CompositeReaderContextKind, IndexReader, IndexReaderBase};
use crate::core::index::parallel_leaf_reader::{ParallelLeafReader, ParallelLeafReaderHook};
use crate::core::index::term::Term;
use crate::core::util::dummy::dummy_comparator::DummyComparator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::fmt::{Display, Formatter};

/// A [`CompositeReader`] which reads multiple, parallel indexes. Each index
/// added must have the same number of documents, and exactly the same number
/// of leaves (with equal `maxDoc`), but typically each contains different
/// fields. Deletions are taken from the first reader. Each document contains
/// the union of the fields of all documents with the same document number.
/// When searching, matches for a query term are from the first index added
/// that has the field.
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
/// to do so results in undefined behavior. A good strategy to create suitable
/// indexes with [`IndexWriter`](crate::core::index::index_writer::IndexWriter)
/// is to use
/// [`LogDocMergePolicy`](crate::core::index::log_doc_merge_policy::LogDocMergePolicy),
/// since it does not reorder documents during merging and triggers merges by
/// number of documents per segment. Using different merge policies may make
/// the segment structure of the indexes unpredictable.
pub struct ParallelCompositeReader<R>
where
  R: CompositeReader,
{
  base_composite_reader_base: BaseCompositeReaderBase<ParallelLeafReader<R::LeafReader>>,
  index_reader_base: IndexReaderBase,
  close_sub_readers: bool,
  complete_reader_set: Vec<R>,
  cache_helper_reader_index: Option<usize>,
}

impl<R> ParallelCompositeReader<R>
where
  R: CompositeReader,
{
  /// Creates a [`ParallelCompositeReader`] based on the provided readers and
  /// automatically closes them when this reader is closed.
  pub fn new<Readers>(readers: Readers) -> Result<Self>
  where
    Readers: Into<Vec<R>>,
  {
    let readers = readers.into();
    Self::new_with_close_sub_readers(true, readers)
  }

  /// Creates a [`ParallelCompositeReader`] based on the provided readers.
  pub fn new_with_close_sub_readers<Readers>(
    close_sub_readers: bool,
    readers: Readers,
  ) -> Result<Self>
  where
    Readers: Into<Vec<R>>,
  {
    let readers = readers.into();
    Self::new_internal(close_sub_readers, readers, None)
  }

  /// Expert: creates a [`ParallelCompositeReader`] based on the provided readers
  /// and stored-fields readers. When a document is loaded, only
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
    Self::new_internal(close_sub_readers, readers, Some(stored_fields_readers))
  }

  fn new_internal(
    close_sub_readers: bool,
    readers: Vec<R>,
    stored_fields_readers: Option<Vec<R>>,
  ) -> Result<Self> {
    let stored_fields_readers_ref = stored_fields_readers.as_deref().unwrap_or(&readers);
    let wrapped_leaves = Self::prepare_leaf_readers(&readers, stored_fields_readers_ref)?;
    let index_reader_base = IndexReaderBase::new();
    let base_composite_reader_base =
      BaseCompositeReaderBase::new::<DummyComparator>(wrapped_leaves, None, &index_reader_base)?;

    let cache_helper_reader_index = if readers.len() == 1
      && stored_fields_readers_ref.len() == 1
      && readers[0]
        .index_base()
        .ptr_eq(stored_fields_readers_ref[0].index_base())
    {
      Some(0)
    } else {
      None
    };

    let complete_reader_capacity =
      readers.len() + stored_fields_readers.as_ref().map_or(0, Vec::len);
    let mut complete_reader_set = Vec::with_capacity(complete_reader_capacity);
    for reader in readers
      .into_iter()
      .chain(stored_fields_readers.into_iter().flatten())
    {
      if !complete_reader_set
        .iter()
        .any(|existing: &R| existing.index_base().ptr_eq(reader.index_base()))
      {
        complete_reader_set.push(reader);
      }
    }

    // Update ref-counts (like MultiReader).
    if !close_sub_readers {
      for reader in &complete_reader_set {
        reader.inc_ref()?;
      }
    }

    Ok(Self {
      base_composite_reader_base,
      index_reader_base,
      close_sub_readers,
      complete_reader_set,
      cache_helper_reader_index,
    })
  }

  fn prepare_leaf_readers(
    readers: &[R],
    stored_fields_readers: &[R],
  ) -> Result<Vec<ParallelLeafReader<R::LeafReader>>> {
    if readers.is_empty() {
      if !stored_fields_readers.is_empty() {
        return Err(LuceneError::illegal_argument(
          "There must be at least one main reader if storedFieldsReaders are used.",
        ));
      }
      return Ok(Vec::new());
    }

    // Check compatibility.
    let mut leaf_count = 0usize;
    readers[0].visit_leaves(&mut |_| {
      leaf_count += 1;
      Ok(())
    })?;
    let max_doc = readers[0].max_doc()?;
    let mut leaf_max_doc = Vec::with_capacity(leaf_count);
    readers[0].visit_leaves(&mut |reader| {
      leaf_max_doc.push(reader.max_doc()?);
      Ok(())
    })?;
    Self::validate(readers, max_doc, &leaf_max_doc)?;
    Self::validate(stored_fields_readers, max_doc, &leaf_max_doc)?;

    // Flatten the structure of each CompositeReader to LeafReaders and combine
    // the parallel structure with ParallelLeafReaders.
    let mut reader_leaves = Vec::with_capacity(readers.len());
    for reader in readers {
      let mut leaves = Vec::with_capacity(leaf_max_doc.len());
      reader.visit_leaves(&mut |leaf| {
        leaves.push(leaf.clone());
        Ok(())
      })?;
      reader_leaves.push(leaves);
    }
    let mut stored_reader_leaves = Vec::with_capacity(stored_fields_readers.len());
    for reader in stored_fields_readers {
      let mut leaves = Vec::with_capacity(leaf_max_doc.len());
      reader.visit_leaves(&mut |leaf| {
        leaves.push(leaf.clone());
        Ok(())
      })?;
      stored_reader_leaves.push(leaves);
    }

    // The flattened buffers are consumed exactly once. Pop from reversed
    // buffers so each leaf reader is moved into the synthetic reader instead
    // of cloned a second time.
    for leaves in &mut reader_leaves {
      leaves.reverse();
    }
    for leaves in &mut stored_reader_leaves {
      leaves.reverse();
    }

    let mut wrapped_leaves = Vec::with_capacity(leaf_max_doc.len());
    for _leaf_index in 0..leaf_max_doc.len() {
      let parallel_leaves = reader_leaves
        .iter_mut()
        .map(|leaves| {
          expect_invariant!(
            leaves.pop(),
            "validated readers have the same number of leaves"
          )
        })
        .collect();
      let stored_leaves = stored_reader_leaves
        .iter_mut()
        .map(|leaves| {
          expect_invariant!(
            leaves.pop(),
            "validated stored-fields readers have the same number of leaves"
          )
        })
        .collect();

      // Close sub-readers and prevent the synthetic disposable readers
      // from touching their sub-readers in `close()`. This makes them
      // completely invisible to ref-counting.
      wrapped_leaves.push(ParallelLeafReader::new_with_stored_fields_and_hook(
        true,
        parallel_leaves,
        stored_leaves,
        ParallelLeafReaderHook::ParallelCompositeReader,
      )?);
    }
    Ok(wrapped_leaves)
  }

  fn validate(readers: &[R], max_doc: i32, leaf_max_doc: &[i32]) -> Result<()> {
    for reader in readers {
      let actual_max_doc = reader.max_doc()?;
      if actual_max_doc != max_doc {
        return Err(LuceneError::illegal_argument(format!(
          "All readers must have same maxDoc: {max_doc}!={}",
          actual_max_doc
        )));
      }
      let mut leaf_count = 0usize;
      reader.visit_leaves(&mut |_| {
        leaf_count += 1;
        Ok(())
      })?;
      if leaf_count != leaf_max_doc.len() {
        return Err(LuceneError::illegal_argument(
          "All readers must have same number of leaf readers",
        ));
      }
      let mut leaf_index = 0usize;
      reader.visit_leaves(&mut |leaf| {
        if leaf.max_doc()? != leaf_max_doc[leaf_index] {
          return Err(LuceneError::illegal_argument(
            "All leaf readers must have same corresponding subReader maxDoc",
          ));
        }
        leaf_index += 1;
        Ok(())
      })?;
    }
    Ok(())
  }
}

impl<R> IndexReader for ParallelCompositeReader<R>
where
  R: CompositeReader,
{
  type ContextKind = CompositeReaderContextKind;

  type TermVectors = BCRTermVectorsImpl<ParallelLeafReader<R::LeafReader>>;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    self.base_composite_reader_base.term_vector(self)
  }

  fn max_doc(&self) -> Result<i32> {
    Ok(self.base_composite_reader_base.max_doc())
  }

  fn num_docs(&self) -> Result<i32> {
    self.base_composite_reader_base.num_docs()
  }

  type StoredFields = BCRStoredFieldsImpl<ParallelLeafReader<R::LeafReader>>;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    self.base_composite_reader_base.stored_fields(self)
  }

  type ReaderCacheHelper = R::ReaderCacheHelper;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    match self.cache_helper_reader_index {
      Some(index) => self.complete_reader_set[index].get_reader_cache_helper(),
      None => Ok(None),
    }
  }

  fn do_close(&self) -> Result<()> {
    let _close_guard = self.index_reader_base.state.close_lock.lock();
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

    // Release our synthetic readers with the same ownership policy as the real
    // readers. Their do_close() is empty and never touches the real leaf readers.
    for reader in self.base_composite_reader_base.get_sequential_sub_readers() {
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

  fn doc_freq(&self, term: &Term) -> Result<i32> {
    self.base_composite_reader_base.doc_freq(term, self)
  }

  fn total_term_freq(&self, term: &Term) -> Result<i64> {
    self.base_composite_reader_base.total_term_freq(term, self)
  }

  fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
    self
      .base_composite_reader_base
      .get_sum_doc_freq(field, self)
  }

  fn get_doc_count(&self, field: &str) -> Result<i32> {
    self.base_composite_reader_base.get_doc_count(field, self)
  }

  fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
    self
      .base_composite_reader_base
      .get_sum_total_term_freq(field, self)
  }

  fn index_base(&self) -> &IndexReaderBase {
    &self.index_reader_base
  }
}

impl<R> Display for ParallelCompositeReader<R>
where
  R: CompositeReader,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", CompositeReader::to_string(self))
  }
}

impl<R> CompositeReader for ParallelCompositeReader<R>
where
  R: CompositeReader,
{
  type LeafReader = ParallelLeafReader<R::LeafReader>;

  type SubReader = ParallelLeafReader<R::LeafReader>;

  fn get_sequential_sub_readers(&self) -> &[Self::SubReader] {
    self.base_composite_reader_base.get_sequential_sub_readers()
  }

  fn visit_leaves<F>(&self, visitor: &mut F) -> Result<()>
  where
    F: FnMut(&Self::LeafReader) -> Result<()>,
  {
    for leaf_reader in self.get_sequential_sub_readers() {
      visitor(leaf_reader)?;
    }
    Ok(())
  }

  fn to_string(&self) -> String {
    let mut buffer = String::from("ParallelCompositeReader(");
    if let Some(first) = self.get_sequential_sub_readers().first() {
      buffer.push_str(&first.to_string());
      for reader in &self.get_sequential_sub_readers()[1..] {
        buffer.push(' ');
        buffer.push_str(&reader.to_string());
      }
    }
    buffer.push(')');
    buffer
  }
}

impl<R> BaseCompositeReader for ParallelCompositeReader<R> where R: CompositeReader {}
