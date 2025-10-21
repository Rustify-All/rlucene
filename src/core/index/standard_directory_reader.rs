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
use crate::core::index::directory_reader::{DirectoryReader, DirectoryReaderBase};
use crate::core::index::dummy::dummy_directory_reader::DummyDirectoryReader;
use crate::core::index::dummy::dummy_index_commit::DummyIndexCommit;
use crate::core::index::index_commit::IndexCommit;
use crate::core::index::index_reader::{IndexReader, IndexReaderEnum};
use crate::core::index::index_writer::{IndexWriter, IndexWriterBase, Inner};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_infos::{FindSegmentsFile, SegmentInfos};
use crate::core::index::segment_reader::SegmentReader;
use crate::core::index::term::Term;
use crate::core::store::IOContext;
use crate::core::store::directory::Directory;
use crate::core::util::dummy::dummy_comparator::DummyComparator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::io_function::IOFunction;
use crate::core::util::{Comparator, LATEST, MIN_SUPPORTED_MAJOR};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub struct StandardDirectoryReader<LR, C, D>
where
    LR: LeafReader,
    C: Comparator<LR>,
    D: Directory,
{
    base_composite_reader_base: BaseCompositeReaderBase<LR, StandardDirectoryReader<LR, C, D>, C>,
    directory_reader_base: DirectoryReaderBase<D>,
    apply_all_deletes: bool,
    write_all_deletes: bool,
    // if Some, this reader owns the SegmentInfos, else from IndexWriter
    segment_infos: Option<SegmentInfos<D>>,
}
impl<LR, C, D> StandardDirectoryReader<LR, C, D>
where
    LR: LeafReader,
    C: Comparator<LR>,
    D: Directory,
{
    pub(crate) fn new(
        directory: Arc<D>,
        readers: Vec<Arc<LR>>,
        segment_infos: SegmentInfos<D>,
        leaf_sorter: Option<Arc<C>>,
        apply_all_deletes: bool,
        write_all_deletes: bool,
    ) -> Result<Self> {
        let base_composite_reader_base = BaseCompositeReaderBase::new(readers, leaf_sorter)?;
        let directory_reader_base = DirectoryReaderBase::new(directory);
        Ok(StandardDirectoryReader {
            base_composite_reader_base,
            directory_reader_base,
            apply_all_deletes,
            write_all_deletes,
            segment_infos: Some(segment_infos),
        })
    }
    pub(crate) fn open<IC>(
        directory: Arc<D>,
        commit: Option<&IC>,
        leaf_sorter: Option<Arc<C>>,
    ) -> Result<StandardDirectoryReader<SegmentReader<D>, C, D>>
    where
        D: Directory,
        C: Comparator<SegmentReader<D>>,
        IC: IndexCommit<Directory = D>,
    {
        Self::open_with_version(directory, *MIN_SUPPORTED_MAJOR, commit, leaf_sorter)
    }
    /// called from DirectoryReader.open(...) methods
    pub(crate) fn open_with_version<IC>(
        directory: Arc<D>,
        min_supported_major_version: i32,
        commit: Option<&IC>,
        leaf_sorter: Option<Arc<C>>,
    ) -> Result<StandardDirectoryReader<SegmentReader<D>, C, D>>
    where
        D: Directory,
        C: Comparator<SegmentReader<D>>,
        IC: IndexCommit<Directory = D>,
    {
        let finder =
            FindSegmentsFileImpl1::new(min_supported_major_version, directory.clone(), leaf_sorter);
        match commit {
            Some(c) => finder.run_with_commit(c),
            None => finder.run(),
        }
    }
}
pub type StandardDirectoryReaderType<D> =
    StandardDirectoryReader<SegmentReader<D>, DummyComparator<SegmentReader<D>>, D>;
pub(crate) fn open_with_reader_function<D, L, B, IO>(
    writer: &IndexWriter<D, L, B>,
    reader_function: &mut IO,
    infos: Option<&SegmentInfos<D>>,
    inner: &mut Inner<D>, // hold IndexWriter lock
    apply_all_deletes: bool,
    write_all_deletes: bool,
) -> Result<StandardDirectoryReaderType<D>>
where
    D: Directory,
    L: LiveIndexWriterConfig,
    B: IndexWriterBase,
    IO: IOFunction<SegmentCommitInfo<D>, Arc<SegmentReader<D>>>,
{
    let (segment_infos, dir, readers) = {
        let infos = match infos {
            Some(infos) => infos,
            None => &inner.segment_infos,
        };
        // IndexWriter synchronizes externally before calling
        // us, which ensures infos will not change; so there's
        // no need to process segments in reverse order
        let num_segments = infos.size();
        let mut readers = Vec::with_capacity(num_segments);
        let dir = writer.get_directory();
        let result = (|| {
            let mut segment_infos = infos.try_clone()?;
            let mut infos_upto = 0;
            for i in 0..num_segments {
                // NOTE: important that we use infos not
                // segmentInfos here, so that we are passing the
                // actual instance of SegmentInfoPerCommit in
                // IndexWriter's segmentInfos:
                let info = match infos.info_idx(i) {
                    Some(info) => info,
                    None => {
                        return Err(LuceneError::illegal_argument(
                            "SegmentInfoPerCommit at index {} is None".to_string(),
                        ));
                    },
                };
                debug_assert!(Arc::ptr_eq(&info.info.dir, &dir));
                let reader = reader_function.apply(info)?;
                // TODO: IMPROTANT 这里合并规则没有判断
                if reader.num_docs()? > 0 {
                    // Steal the ref
                    readers.push(reader);
                    infos_upto += 1;
                } else {
                    reader.dec_ref()?;
                    segment_infos.remove_at(infos_upto);
                }
            }
            Ok(segment_infos)
        })();
        match result {
            Ok(segment_infos) => (segment_infos, dir, readers),
            Err(e) => {
                for r in readers {
                    let _ = r.dec_ref();
                }
                return Err(e);
            },
        }
    };
    // Clone pointer should be cheap
    let readers_backup = readers.clone();
    let result: Result<_> = (|| {
        writer.inc_ref_deleter(&segment_infos, Some(inner))?;
        StandardDirectoryReader::new(
            dir,
            readers,
            segment_infos,
            // TODO IMPORTANT 这里不对 要从LiveIndexWriterConfig中获取
            None::<Arc<DummyComparator<SegmentReader<D>>>>,
            apply_all_deletes,
            write_all_deletes,
        )
    })();
    match result {
        Ok(r) => Ok(r),
        Err(e) => {
            for r in readers_backup {
                let _ = r.dec_ref();
            }
            Err(e)
        },
    }
}

impl<LR, C, D> BaseCompositeReader for StandardDirectoryReader<LR, C, D>
where
    C: Comparator<LR>,
    D: Directory,
    LR: LeafReader,
{
    type Comparator = C;

    fn base_composite_reader_base(
        &self,
    ) -> &BaseCompositeReaderBase<Self::IndexReader, Self, Self::Comparator> {
        &self.base_composite_reader_base
    }
}

impl<LR, C, D> CompositeReader for StandardDirectoryReader<LR, C, D>
where
    C: Comparator<LR>,
    D: Directory,
    LR: LeafReader,
{
    type IndexReader = LR;
    type SubCompositeReader = StandardDirectoryReader<LR, C, D>;

    fn get_sequential_sub_readers(
        &self,
    ) -> Vec<IndexReaderEnum<Arc<Self::IndexReader>, Self::SubCompositeReader>> {
        self.base_composite_reader_base.get_sequential_sub_readers()
    }
}

impl<LR, C, D> IndexReader for StandardDirectoryReader<LR, C, D>
where
    C: Comparator<LR>,
    D: Directory,
    LR: LeafReader,
{
    type TermVectors = BCRTermVectorsImpl<LR>;

    fn term_vectors(&self) -> Result<Self::TermVectors> {
        self.base_composite_reader_base.term_vector(self)
    }

    fn max_doc(&self) -> Result<i32> {
        Ok(self.base_composite_reader_base.max_doc())
    }

    fn num_docs(&self) -> Result<i32> {
        self.base_composite_reader_base.num_docs()
    }

    type StoredFields = BCRStoredFieldsImpl<LR>;

    fn stored_fields(&self) -> Result<Self::StoredFields> {
        self.base_composite_reader_base.stored_fields(self)
    }

    fn do_close(&mut self) -> Result<()> {
        todo!()
    }

    fn doc_freq(&self, term: &Term) -> Result<i32> {
        self.base_composite_reader_base.doc_freq(term, self)
    }

    fn total_term_freq(&self, term: &Term) -> Result<i64> {
        self.base_composite_reader_base.total_term_freq(term, self)
    }

    fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
        self.base_composite_reader_base
            .get_sum_doc_freq(field, self)
    }

    fn get_doc_count(&self, field: &str) -> Result<i32> {
        self.base_composite_reader_base.get_doc_count(field, self)
    }

    fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
        self.base_composite_reader_base
            .get_sum_total_term_freq(field, self)
    }
}

impl<LR, C, D> Display for StandardDirectoryReader<LR, C, D>
where
    C: Comparator<LR>,
    D: Directory,
    LR: LeafReader,
{
    fn fmt(&self, _f: &mut Formatter<'_>) -> std::fmt::Result {
        todo!()
    }
}

impl<LR, C, D> DirectoryReader for StandardDirectoryReader<LR, C, D>
where
    LR: LeafReader,
    C: Comparator<LR>,
    D: Directory,
{
    type DirectoryReader = DummyDirectoryReader<D>;

    fn do_open_if_changed(&mut self) -> Result<Option<Self::DirectoryReader>> {
        self.do_open_if_changed_with_commit::<DummyIndexCommit<D>>(None)
    }

    fn do_open_if_changed_with_commit<IC>(
        &mut self,
        _commit: Option<&IC>,
    ) -> Result<Option<Self::DirectoryReader>>
    where
        IC: IndexCommit,
    {
        todo!()
    }

    fn do_open_if_changed_with_index_writer<L, B>(
        &self,
        _writer: IndexWriter<Self::Directory, L, B>,
        _apply_deletes: bool,
    ) -> Result<Self::DirectoryReader>
    where
        L: LiveIndexWriterConfig,
        B: IndexWriterBase,
    {
        todo!()
    }

    fn get_version(&self) -> i64 {
        todo!()
    }

    fn is_current(&self) -> Result<bool> {
        self.ensure_open()?;
        todo!()
    }

    type IndexCommit = DummyIndexCommit<D>;

    fn get_index_commit(&self) -> Result<Self::IndexCommit> {
        todo!()
    }

    type Directory = D;

    fn directory(&self) -> &DirectoryReaderBase<Self::Directory> {
        &self.directory_reader_base
    }
}
pub struct FindSegmentsFileImpl1<D, LR, C>
where
    D: Directory,
    LR: LeafReader,
    C: Comparator<LR>,
{
    min_supported_major_version: i32,
    directory: Arc<D>,
    leaf_sorter: Option<Arc<C>>,
    _marker: std::marker::PhantomData<LR>,
}
impl<D, LR, C> FindSegmentsFileImpl1<D, LR, C>
where
    D: Directory,
    LR: LeafReader,
    C: Comparator<LR>,
{
    pub fn new(
        min_supported_major_version: i32,
        directory: Arc<D>,
        leaf_sorter: Option<Arc<C>>,
    ) -> Self {
        FindSegmentsFileImpl1 {
            min_supported_major_version,
            directory,
            leaf_sorter,
            _marker: std::marker::PhantomData,
        }
    }
}
impl<D, C> FindSegmentsFile for FindSegmentsFileImpl1<D, SegmentReader<D>, C>
where
    D: Directory,
    C: Comparator<SegmentReader<D>>,
{
    type V = StandardDirectoryReader<SegmentReader<D>, C, D>;
    type D = D;

    fn get_directory_point(&self) -> Arc<Self::D> {
        self.directory.clone()
    }

    fn do_body(&self, segment_file_name: &str) -> Result<Self::V> {
        if self.min_supported_major_version > LATEST.major || self.min_supported_major_version < 0 {
            return Err(LuceneError::illegal_argument(format!(
                "minSupportedMajorVersion must be positive and <= {} but was: {}",
                LATEST.major, self.min_supported_major_version
            )));
        }

        let sis = SegmentInfos::read_commit_with_file_min_version(
            self.directory.clone(),
            segment_file_name,
            self.min_supported_major_version,
        )?;

        let mut readers = Vec::with_capacity(sis.size());

        // ensure cleanup on failure
        for i in (0..sis.size()).rev() {
            debug_assert!(sis.info_idx(i).is_some());
            let reader = SegmentReader::new(
                sis.info_idx(i).as_ref().unwrap(),
                sis.get_index_created_version_major(),
                &IOContext::default_io_context()?,
            )?;
            readers.push(Arc::new(reader));
        }
        // This may throw CorruptIndexException if there are too many docs, so
        // it must be inside try clause so we close readers in that case:
        let reader = StandardDirectoryReader::new(
            self.directory.clone(),
            readers,
            sis,
            self.leaf_sorter.clone(),
            false,
            false,
        )?;

        Ok(reader)
    }
}

pub struct FindSegmentsFileImpl2<D>
where
    D: Directory,
{
    directory: Arc<D>,
}
impl<D> FindSegmentsFileImpl2<D>
where
    D: Directory,
{
    pub fn new(directory: Arc<D>) -> Self {
        FindSegmentsFileImpl2 { directory }
    }
}
impl<D> FindSegmentsFile for FindSegmentsFileImpl2<D>
where
    D: Directory,
{
    type V = ();
    type D = D;

    fn get_directory_point(&self) -> Arc<Self::D> {
        self.directory.clone()
    }

    fn do_body(&self, segment_file_name: &str) -> Result<Self::V> {
        let _infos = SegmentInfos::read_commit(self.directory.clone(), segment_file_name)?;
        todo!()
    }
}
