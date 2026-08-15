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
use crate::core::index::filter_directory_reader::{FilterDirectoryReader, SubReaderWrapper};
use crate::core::index::index_reader::{CompositeReaderContextKind, IndexReader, IndexReaderBase};
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::term::Term;
use crate::core::util::dummy::dummy_comparator::DummyComparator;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::index::merging_codec_reader::{
  MergingCodecReader, MergingCodecReaderEnum,
};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

struct MergingSubReaderWrapper;

impl<CR> SubReaderWrapper<CR> for MergingSubReaderWrapper
where
  CR: CodecReader,
{
  type LeafReader1 = MergingCodecReaderEnum<CR>;

  fn wrap_readers(&self, readers: Vec<CR>) -> Result<Vec<Self::LeafReader1>> {
    Ok(
      readers
        .into_iter()
        .map(|reader| MergingCodecReaderEnum::Merging(MergingCodecReader::new(reader)))
        .collect(),
    )
  }

  type LeafReader2 = MergingCodecReaderEnum<CR>;

  fn wrap(&self, reader: CR) -> Result<Self::LeafReader2> {
    Ok(MergingCodecReaderEnum::Merging(MergingCodecReader::new(
      reader,
    )))
  }
}

/// [`DirectoryReader`] wrapper that uses the merge instances of the wrapped
/// [`CodecReader`]s. NOTE: This struct will fail to work if the leaves of the wrapped directory are
/// not codec readers.
pub struct MergingDirectoryReaderWrapper<DR>
where
  DR: DirectoryReader,
  DR::LeafReader: CodecReader + Clone,
{
  in_: DR,
  base: BaseCompositeReaderBase<MergingCodecReaderEnum<DR::LeafReader>>,
  index_base: IndexReaderBase,
}

impl<DR> MergingDirectoryReaderWrapper<DR>
where
  DR: DirectoryReader,
  DR::LeafReader: CodecReader + Clone,
{
  /// Wrap the given directory.
  pub fn new(in_: DR) -> Result<Self> {
    let wrapper = MergingSubReaderWrapper;
    let readers = wrapper.wrap_readers(in_.get_sequential_sub_readers().to_vec())?;
    let index_base = IndexReaderBase::new();
    let base = BaseCompositeReaderBase::new::<DummyComparator>(readers, None, &index_base)?;
    Ok(Self {
      in_,
      base,
      index_base,
    })
  }
}

impl<DR> BaseCompositeReader for MergingDirectoryReaderWrapper<DR>
where
  DR: DirectoryReader,
  DR::LeafReader: CodecReader + Clone,
{
}

impl<DR> CompositeReader for MergingDirectoryReaderWrapper<DR>
where
  DR: DirectoryReader,
  DR::LeafReader: CodecReader + Clone,
{
  type LeafReader = MergingCodecReaderEnum<DR::LeafReader>;
  type SubReader = Self::LeafReader;

  fn get_sequential_sub_readers(&self) -> &[Self::SubReader] {
    self.base.get_sequential_sub_readers()
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
    format!("MergingDirectoryReaderWrapper({})", self.in_.to_string())
  }
}

impl<DR> IndexReader for MergingDirectoryReaderWrapper<DR>
where
  DR: DirectoryReader,
  DR::LeafReader: CodecReader + Clone,
{
  type ContextKind = CompositeReaderContextKind;

  type TermVectors = BCRTermVectorsImpl<<Self as CompositeReader>::LeafReader>;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    self.base.term_vector(self)
  }

  fn max_doc(&self) -> Result<i32> {
    Ok(self.base.max_doc())
  }

  fn num_docs(&self) -> Result<i32> {
    self.base.num_docs()
  }

  type StoredFields = BCRStoredFieldsImpl<<Self as CompositeReader>::LeafReader>;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    self.base.stored_fields(self)
  }

  fn do_close(&self) -> Result<()> {
    self.in_.close()
  }

  type ReaderCacheHelper = DR::ReaderCacheHelper;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    // doesn't change the content: can delegate
    self.in_.get_reader_cache_helper()
  }

  fn doc_freq(&self, term: &Term) -> Result<i32> {
    self.base.doc_freq(term, self)
  }

  fn total_term_freq(&self, term: &Term) -> Result<i64> {
    self.base.total_term_freq(term, self)
  }

  fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
    self.base.get_sum_doc_freq(field, self)
  }

  fn get_doc_count(&self, field: &str) -> Result<i32> {
    self.base.get_doc_count(field, self)
  }

  fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
    self.base.get_sum_total_term_freq(field, self)
  }

  fn index_base(&self) -> &IndexReaderBase {
    &self.index_base
  }
}

impl<DR> Display for MergingDirectoryReaderWrapper<DR>
where
  DR: DirectoryReader,
  DR::LeafReader: CodecReader + Clone,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", CompositeReader::to_string(self))
  }
}

impl<DR> DirectoryReader for MergingDirectoryReaderWrapper<DR>
where
  DR: DirectoryReader<DirectoryReader = DR>,
  DR::LeafReader: CodecReader + Clone,
{
  type DirectoryReader = MergingDirectoryReaderWrapper<DR>;

  fn do_open_if_changed(&self) -> Result<Option<Self::DirectoryReader>> {
    self.wrap_directory_reader(self.in_.do_open_if_changed()?)
  }

  fn do_open_if_changed_with_commit<IC>(
    &self,
    commit: Option<&IC>,
  ) -> Result<Option<Self::DirectoryReader>>
  where
    IC: crate::core::index::index_commit::IndexCommit<Directory = Arc<Self::Directory>>,
  {
    self.wrap_directory_reader(self.in_.do_open_if_changed_with_commit(commit)?)
  }

  fn do_open_if_changed_with_deletes(
    &self,
    writer: &Arc<IndexWriter<Self::Directory>>,
    apply_deletes: bool,
  ) -> Result<Option<Self::DirectoryReader>> {
    self.wrap_directory_reader(
      self
        .in_
        .do_open_if_changed_with_deletes(writer, apply_deletes)?,
    )
  }

  fn get_version(&self) -> Result<i64> {
    self.in_.get_version()
  }

  fn is_current(&self) -> Result<bool> {
    self.in_.is_current()
  }

  type IndexCommit = DR::IndexCommit;

  fn get_index_commit(&self) -> Result<Self::IndexCommit> {
    self.in_.get_index_commit()
  }

  type Directory = DR::Directory;

  fn directory(&self) -> &DirectoryReaderBase<Self::Directory> {
    self.in_.directory()
  }
}

impl<DR> FilterDirectoryReader for MergingDirectoryReaderWrapper<DR>
where
  DR: DirectoryReader<DirectoryReader = DR>,
  DR::LeafReader: CodecReader + Clone,
{
  type Delegate = DR;

  fn get_delegate(&self) -> &Self::Delegate {
    &self.in_
  }

  type WrapDirectoryReader = MergingDirectoryReaderWrapper<DR>;

  fn do_wrap_directory_reader(
    &self,
    in_: Option<<Self::Delegate as DirectoryReader>::DirectoryReader>,
  ) -> Result<Option<Self::WrapDirectoryReader>> {
    in_.map(MergingDirectoryReaderWrapper::new).transpose()
  }
}
