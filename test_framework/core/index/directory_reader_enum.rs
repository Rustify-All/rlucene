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
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::composite_reader::CompositeReader;
use crate::core::index::directory_reader::{DirectoryReader, DirectoryReaderBase};
use crate::core::index::index_reader::{CompositeReaderContextKind, IndexReader, IndexReaderBase};
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::term::Term;
use crate::core::util::dummy::dummy_comparator::DummyComparator;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::index::merging_codec_reader::MergingCodecReaderEnum;
use crate::test_framework::core::index::merging_directory_reader_wrapper::MergingDirectoryReaderWrapper;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

/// Either an unchanged [`DirectoryReader`] or a [`MergingDirectoryReaderWrapper`].
pub struct DefaultDirectoryReader<DR>
where
  DR: DirectoryReader,
  DR::LeafReader: CodecReader + Clone,
{
  in_: DR,
  base: BaseCompositeReaderBase<MergingCodecReaderEnum<DR::LeafReader>>,
  index_base: IndexReaderBase,
}

pub enum DirectoryReaderEnum<DR>
where
  DR: DirectoryReader,
  DR::LeafReader: CodecReader + Clone,
{
  Default(DefaultDirectoryReader<DR>),
  Merging(MergingDirectoryReaderWrapper<DR>),
}

impl<DR> DirectoryReaderEnum<DR>
where
  DR: DirectoryReader,
  DR::LeafReader: CodecReader + Clone,
{
  pub fn new(in_: DR, use_merge_instance: bool) -> Result<Self> {
    if use_merge_instance {
      Ok(Self::Merging(MergingDirectoryReaderWrapper::new(in_)?))
    } else {
      Self::new_default(in_)
    }
  }

  fn new_default(in_: DR) -> Result<Self> {
    let readers = in_
      .get_sequential_sub_readers()
      .iter()
      .cloned()
      .map(MergingCodecReaderEnum::Default)
      .collect();
    let index_base = IndexReaderBase::new();
    let base = BaseCompositeReaderBase::new::<DummyComparator>(readers, None, &index_base)?;
    Ok(Self::Default(DefaultDirectoryReader {
      in_,
      base,
      index_base,
    }))
  }
}

impl<DR> BaseCompositeReader for DirectoryReaderEnum<DR>
where
  DR: DirectoryReader,
  DR::LeafReader: CodecReader + Clone,
{
}

impl<DR> CompositeReader for DirectoryReaderEnum<DR>
where
  DR: DirectoryReader,
  DR::LeafReader: CodecReader + Clone,
{
  type LeafReader = MergingCodecReaderEnum<DR::LeafReader>;
  type SubReader = Self::LeafReader;

  fn get_sequential_sub_readers(&self) -> &[Self::SubReader] {
    match self {
      Self::Default(reader) => reader.base.get_sequential_sub_readers(),
      Self::Merging(reader) => reader.get_sequential_sub_readers(),
    }
  }

  fn visit_leaves<F>(&self, visitor: &mut F) -> Result<()>
  where
    F: FnMut(&Self::LeafReader) -> Result<()>,
  {
    for reader in self.get_sequential_sub_readers() {
      visitor(reader)?;
    }
    Ok(())
  }

  fn to_string(&self) -> String {
    match self {
      Self::Default(reader) => reader.in_.to_string(),
      Self::Merging(reader) => CompositeReader::to_string(reader),
    }
  }
}

impl<DR> IndexReader for DirectoryReaderEnum<DR>
where
  DR: DirectoryReader,
  DR::LeafReader: CodecReader + Clone,
{
  type ContextKind = CompositeReaderContextKind;

  type TermVectors = BCRTermVectorsImpl<<Self as CompositeReader>::LeafReader>;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    match self {
      Self::Default(reader) => reader.base.term_vector(self),
      Self::Merging(reader) => IndexReader::term_vectors(reader),
    }
  }

  fn max_doc(&self) -> Result<i32> {
    match self {
      Self::Default(reader) => Ok(reader.base.max_doc()),
      Self::Merging(reader) => reader.max_doc(),
    }
  }

  fn num_docs(&self) -> Result<i32> {
    match self {
      Self::Default(reader) => reader.base.num_docs(),
      Self::Merging(reader) => reader.num_docs(),
    }
  }

  type StoredFields = BCRStoredFieldsImpl<<Self as CompositeReader>::LeafReader>;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    match self {
      Self::Default(reader) => reader.base.stored_fields(self),
      Self::Merging(reader) => IndexReader::stored_fields(reader),
    }
  }

  fn do_close(&self) -> Result<()> {
    match self {
      Self::Default(reader) => reader.in_.do_close(),
      Self::Merging(reader) => reader.do_close(),
    }
  }

  type ReaderCacheHelper = DR::ReaderCacheHelper;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    match self {
      Self::Default(reader) => reader.in_.get_reader_cache_helper(),
      Self::Merging(reader) => reader.get_reader_cache_helper(),
    }
  }

  fn doc_freq(&self, term: &Term) -> Result<i32> {
    match self {
      Self::Default(reader) => reader.base.doc_freq(term, self),
      Self::Merging(reader) => IndexReader::doc_freq(reader, term),
    }
  }

  fn total_term_freq(&self, term: &Term) -> Result<i64> {
    match self {
      Self::Default(reader) => reader.base.total_term_freq(term, self),
      Self::Merging(reader) => IndexReader::total_term_freq(reader, term),
    }
  }

  fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
    match self {
      Self::Default(reader) => reader.base.get_sum_doc_freq(field, self),
      Self::Merging(reader) => IndexReader::get_sum_doc_freq(reader, field),
    }
  }

  fn get_doc_count(&self, field: &str) -> Result<i32> {
    match self {
      Self::Default(reader) => reader.base.get_doc_count(field, self),
      Self::Merging(reader) => IndexReader::get_doc_count(reader, field),
    }
  }

  fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
    match self {
      Self::Default(reader) => reader.base.get_sum_total_term_freq(field, self),
      Self::Merging(reader) => IndexReader::get_sum_total_term_freq(reader, field),
    }
  }

  fn index_base(&self) -> &IndexReaderBase {
    match self {
      Self::Default(reader) => &reader.index_base,
      Self::Merging(reader) => reader.index_base(),
    }
  }
}

impl<DR> Display for DirectoryReaderEnum<DR>
where
  DR: DirectoryReader,
  DR::LeafReader: CodecReader + Clone,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", CompositeReader::to_string(self))
  }
}

impl<DR> DirectoryReader for DirectoryReaderEnum<DR>
where
  DR: DirectoryReader<DirectoryReader = DR>,
  DR::LeafReader: CodecReader + Clone,
{
  type DirectoryReader = Self;

  fn do_open_if_changed(&self) -> Result<Option<Self::DirectoryReader>> {
    match self {
      Self::Default(reader) => reader
        .in_
        .do_open_if_changed()?
        .map(Self::new_default)
        .transpose(),
      Self::Merging(reader) => Ok(reader.do_open_if_changed()?.map(Self::Merging)),
    }
  }

  fn do_open_if_changed_with_commit<IC>(
    &self,
    commit: Option<&IC>,
  ) -> Result<Option<Self::DirectoryReader>>
  where
    IC: crate::core::index::index_commit::IndexCommit<Directory = Arc<Self::Directory>>,
  {
    match self {
      Self::Default(reader) => reader
        .in_
        .do_open_if_changed_with_commit(commit)?
        .map(Self::new_default)
        .transpose(),
      Self::Merging(reader) => Ok(
        reader
          .do_open_if_changed_with_commit(commit)?
          .map(Self::Merging),
      ),
    }
  }

  fn do_open_if_changed_with_deletes(
    &self,
    writer: &Arc<IndexWriter<Self::Directory>>,
    apply_deletes: bool,
  ) -> Result<Option<Self::DirectoryReader>> {
    match self {
      Self::Default(reader) => reader
        .in_
        .do_open_if_changed_with_deletes(writer, apply_deletes)?
        .map(Self::new_default)
        .transpose(),
      Self::Merging(reader) => Ok(
        reader
          .do_open_if_changed_with_deletes(writer, apply_deletes)?
          .map(Self::Merging),
      ),
    }
  }

  fn get_version(&self) -> Result<i64> {
    match self {
      Self::Default(reader) => reader.in_.get_version(),
      Self::Merging(reader) => reader.get_version(),
    }
  }

  fn is_current(&self) -> Result<bool> {
    match self {
      Self::Default(reader) => reader.in_.is_current(),
      Self::Merging(reader) => reader.is_current(),
    }
  }

  type IndexCommit = DR::IndexCommit;

  fn get_index_commit(&self) -> Result<Self::IndexCommit> {
    match self {
      Self::Default(reader) => reader.in_.get_index_commit(),
      Self::Merging(reader) => reader.get_index_commit(),
    }
  }

  type Directory = DR::Directory;

  fn directory(&self) -> &DirectoryReaderBase<Self::Directory> {
    match self {
      Self::Default(reader) => reader.in_.directory(),
      Self::Merging(reader) => reader.directory(),
    }
  }
}
