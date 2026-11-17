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
#[cfg(test)]
use crate::core::index::dummy::dummy_leaf_reader::DummyLeafReader;
use crate::core::index::index_reader::{
  CompositeReaderContextKind, IndexReader, IndexReaderBase, LeafReaderContextKind,
};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::term::Term;
use crate::core::util::dummy::dummy_comparator::DummyComparator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

/// A [`CompositeReader`] which reads multiple indexes, appending their content.
/// It can be used to create a view on several sub-readers (like [`DirectoryReader`](crate::core::index::directory_reader::DirectoryReader))
/// and execute searches on it.
///
/// For efficiency, in this API documents are often referred to via *document numbers*,
/// non-negative integers which each name a unique document in the index.
/// These document numbers are ephemeral — they may change as documents are added
/// to and deleted from an index. Clients should thus not rely on a given document
/// having the same number between sessions.
///
/// **NOTE**: [`IndexReader`] instances are completely thread safe, meaning multiple
/// threads can call any of its methods concurrently. If your application requires
/// external synchronization, you should **not** synchronize on the [`IndexReader`]
/// instance; instead, use your own (non-Lucene) objects.
pub struct MultiReader<R> {
  base_composite_reader_base: BaseCompositeReaderBase<R>,
  index_reader_base: IndexReaderBase,
  close_sub_readers: bool,
}

#[doc(hidden)]
pub trait MultiReaderKind<R> {
  type LeafReader: LeafReader + Clone;

  fn visit_leaves<F>(sub_readers: &[R], visitor: &mut F) -> Result<()>
  where
    F: FnMut(&Self::LeafReader) -> Result<()>;
}

impl<LR> MultiReaderKind<LR> for LeafReaderContextKind
where
  LR: LeafReader + Clone,
{
  type LeafReader = LR;

  fn visit_leaves<F>(sub_readers: &[LR], visitor: &mut F) -> Result<()>
  where
    F: FnMut(&Self::LeafReader) -> Result<()>,
  {
    for leaf_reader in sub_readers {
      visitor(leaf_reader)?;
    }
    Ok(())
  }
}

impl<CR> MultiReaderKind<CR> for CompositeReaderContextKind
where
  CR: CompositeReader,
{
  type LeafReader = CR::LeafReader;

  fn visit_leaves<F>(sub_readers: &[CR], visitor: &mut F) -> Result<()>
  where
    F: FnMut(&Self::LeafReader) -> Result<()>,
  {
    for composite_reader in sub_readers {
      composite_reader.visit_leaves(visitor)?;
    }
    Ok(())
  }
}

#[cfg(test)]
impl MultiReader<DummyLeafReader> {
  pub fn empty() -> Result<Self> {
    Self::new(vec![])
  }
}

impl<R> MultiReader<R>
where
  R: IndexReader,
  R::ContextKind: MultiReaderKind<R>,
{
  pub fn new<Readers>(sub_readers: Readers) -> Result<Self>
  where
    Readers: Into<Vec<R>>,
  {
    let sub_readers = sub_readers.into();
    Self::new_with_close_sub_readers(true, sub_readers)
  }

  pub(crate) fn new_shared(sub_readers: Arc<[R]>) -> Result<Self> {
    let index_reader_base = IndexReaderBase::new();
    let base_composite_reader_base =
      BaseCompositeReaderBase::new_shared(sub_readers, &index_reader_base)?;
    Ok(Self {
      base_composite_reader_base,
      index_reader_base,
      close_sub_readers: true,
    })
  }

  pub fn new_with_close_sub_readers<Readers>(
    close_sub_readers: bool,
    sub_readers: Readers,
  ) -> Result<Self>
  where
    Readers: Into<Vec<R>>,
  {
    let sub_readers = sub_readers.into();
    let index_reader_base = IndexReaderBase::new();
    let base_composite_reader_base =
      BaseCompositeReaderBase::new::<DummyComparator>(sub_readers, None, &index_reader_base)?;
    if !close_sub_readers {
      for reader in base_composite_reader_base.get_sequential_sub_readers() {
        reader.inc_ref()?;
      }
    }
    Ok(Self {
      base_composite_reader_base,
      index_reader_base,
      close_sub_readers,
    })
  }
}

impl<R> IndexReader for MultiReader<R>
where
  R: IndexReader,
  R::ContextKind: MultiReaderKind<R>,
{
  type ContextKind = CompositeReaderContextKind;

  type TermVectors = BCRTermVectorsImpl<R>;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    self.base_composite_reader_base.term_vector(self)
  }

  fn max_doc(&self) -> Result<i32> {
    Ok(self.base_composite_reader_base.max_doc())
  }

  fn num_docs(&self) -> Result<i32> {
    self.base_composite_reader_base.num_docs()
  }

  type StoredFields = BCRStoredFieldsImpl<R>;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    self.base_composite_reader_base.stored_fields(self)
  }

  fn do_close(&self) -> Result<()> {
    let _close_guard = self.index_reader_base.state.close_lock.lock();
    let mut first_err: Option<LuceneError> = None;

    for r in self.base_composite_reader_base.get_sequential_sub_readers() {
      let result: Result<()> = (|| {
        if self.close_sub_readers {
          r.close()?
        } else {
          r.dec_ref()?
        }
        Ok(())
      })();

      match result {
        Err(error) if error.is_io_error() => {
          if first_err.is_none() {
            first_err = Some(error);
          }
        },
        Err(error) => return Err(error),
        Ok(()) => {},
      }
    }
    if let Some(e) = first_err {
      Err(e)
    } else {
      Ok(())
    }
  }

  type ReaderCacheHelper = R::ReaderCacheHelper;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    let readers = self.get_sequential_sub_readers();
    if readers.len() == 1 {
      readers[0].get_reader_cache_helper()
    } else {
      Ok(None)
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

impl<R> Display for MultiReader<R>
where
  R: IndexReader,
  R::ContextKind: MultiReaderKind<R>,
{
  fn fmt(&self, _f: &mut Formatter<'_>) -> std::fmt::Result {
    todo!()
  }
}

impl<R> CompositeReader for MultiReader<R>
where
  R: IndexReader,
  R::ContextKind: MultiReaderKind<R>,
{
  type LeafReader = <R::ContextKind as MultiReaderKind<R>>::LeafReader;

  type SubReader = R;

  fn get_sequential_sub_readers(&self) -> &[Self::SubReader] {
    self.base_composite_reader_base.get_sequential_sub_readers()
  }

  fn visit_leaves<F>(&self, visitor: &mut F) -> Result<()>
  where
    F: FnMut(&Self::LeafReader) -> Result<()>,
  {
    R::ContextKind::visit_leaves(self.get_sequential_sub_readers(), visitor)
  }

  fn to_string(&self) -> String {
    todo!()
  }
}
impl<R> BaseCompositeReader for MultiReader<R>
where
  R: IndexReader,
  R::ContextKind: MultiReaderKind<R>,
{
}
