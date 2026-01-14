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
use crate::core::codecs::doc_values_consumer::DocValuesConsumer;
use crate::core::codecs::doc_values_format::DocValuesFormat;
use crate::core::codecs::field_infos_format::FieldInfosFormat;
use crate::core::codecs::fields_consumer::FieldsConsumer;
use crate::core::codecs::norms_consumer::NormsConsumer;
use crate::core::codecs::norms_format::NormsFormat;
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::codecs::points_format::PointsFormat;
use crate::core::codecs::points_writer::PointsWriter;
use crate::core::codecs::postings_format::PostingsFormat;
use crate::core::codecs::stored_fields_format::StoredFieldsFormat;
use crate::core::codecs::stored_fields_reader::DefaultStoredFieldsReader;
use crate::core::codecs::stored_fields_writer::StoredFieldsWriter;
use crate::core::codecs::term_vectors_format::TermVectorsFormat;
use crate::core::codecs::term_vectors_reader::DefaultTermVectorsReader;
use crate::core::codecs::term_vectors_writer::TermVectorsWriter;
use crate::core::codecs::{Codec, LATEST_CODEC};
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::field_infos::FieldNumbersLock;
use crate::core::index::field_infos::build::Builder;
use crate::core::index::merge_state::MergeState;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::store::Context::Merge;
use crate::core::store::IOContext;
use crate::core::store::directory::Directory;
use crate::core::util::LATEST;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::info_stream::{InfoStream, InfoStreamMT};
use std::sync::Arc;
use std::time::Instant;
/// The `SegmentMerger` class combines two or more segments, represented by
/// `IndexReader`s, into a single segment. Call the `merge` method to combine
/// the segments.
///
/// See [`SegmentMerger::merge`].
pub(crate) struct SegmentMerger<'a, D, CR>
where
    D: Directory,
    CR: CodecReader<
            StoredFieldsReader = DefaultStoredFieldsReader<D::IndexInput>,
            TermVectorsReader = DefaultTermVectorsReader<D::IndexInput>,
        >,
{
    directory: &'a D,
    context: &'a IOContext,
    merge_state: MergeState<'a, D, CR>,
    field_infos_builder: Builder,
}

impl<'a, D, CR> SegmentMerger<'a, D, CR>
where
    D: Directory,
    CR: CodecReader<
            StoredFieldsReader = DefaultStoredFieldsReader<D::IndexInput>,
            TermVectorsReader = DefaultTermVectorsReader<D::IndexInput>,
        >,
{
    pub(crate) fn new(
        readers: &'a [CR],
        segment_info: &'a mut SegmentInfo<D>,
        info_stream: InfoStreamMT,
        directory: &'a D,
        field_numbers: FieldNumbersLock,
        context: &'a IOContext,
    ) -> Result<Self> {
        if *context.get_context() != Merge {
            return Err(LuceneError::illegal_argument(format!(
                "IOContext.context should be MERGE; got: {:?}",
                context.get_context()
            )));
        }

        let merge_state = MergeState::new(readers, segment_info, info_stream)?;

        let field_infos_builder = Builder::new(field_numbers);

        let mut min_version = Some(LATEST.clone());
        for reader in readers {
            let leaf_min_version = reader.get_metadata()?.get_min_version();
            match leaf_min_version {
                Some(v) => {
                    if let Some(cur) = &mut min_version
                        && cur.on_or_after(v)
                    {
                        *cur = v.clone();
                    }
                },
                None => {
                    min_version = None;
                    break;
                },
            }
        }

        debug_assert!(
            merge_state.segment_info.min_version.is_none(),
            "The min version should be set by SegmentMerger for merged segments"
        );
        merge_state.segment_info.min_version = min_version;

        if merge_state.info_stream.enabled("SM")
            && let Some(sort) = merge_state.segment_info.get_index_sort()
        {
            merge_state
                .info_stream
                .message("SM", &format!("index sort during merge: {}", sort));
        }

        Ok(Self {
            directory,
            context,
            merge_state,
            field_infos_builder,
        })
    }
    fn merge_field_infos_with_state(
        &self,
        _segment_write_state: &SegmentWriteState<&D>,
        _segment_read_state: &SegmentReadState<&D>,
    ) -> Result<()> {
        LATEST_CODEC.field_infos_format().write(
            &self.directory,
            self.merge_state.segment_info,
            "",
            &self.merge_state.merge_field_infos,
            self.context,
        )
    }

    fn merge_doc_values(&self, segment_write_state: &SegmentWriteState<&D>) -> Result<()> {
        let mut consumer = LATEST_CODEC
            .doc_values_format()
            .fields_consumer(segment_write_state, self.merge_state.segment_info)?;

        consumer.merge(&self.merge_state)?;

        Ok(())
    }
    fn merge_points(&self, segment_write_state: &SegmentWriteState<&D>) -> Result<()> {
        let mut writer = LATEST_CODEC
            .points_format()
            .fields_writer(segment_write_state, self.merge_state.segment_info)?;

        writer.merge(&self.merge_state, &self.directory)?;

        Ok(())
    }
    fn merge_norms(&self, segment_write_state: &SegmentWriteState<&D>) -> Result<()> {
        let mut consumer = LATEST_CODEC
            .norms_format()
            .norms_consumer(segment_write_state, self.merge_state.segment_info)?;

        consumer.merge(&self.merge_state)?;

        Ok(())
    }
    fn merge_terms(
        &self,
        segment_write_state: &SegmentWriteState<&D>,
        segment_read_state: &SegmentReadState<&D>,
    ) -> Result<()> {
        let mut norms = if self.merge_state.merge_field_infos.has_norms() {
            Some(
                LATEST_CODEC
                    .norms_format()
                    .norms_producer(segment_read_state, self.merge_state.segment_info)?,
            )
        } else {
            None
        };

        let mut norms_merge_instance = None;
        if let Some(ref mut norms) = norms {
            // Use the merge instance in order to reuse the same IndexInput for all terms
            norms_merge_instance = norms.get_merge_instance()?;
        }

        if self.merge_state.merge_field_infos.has_postings() {
            let mut consumer = LATEST_CODEC
                .postings_format()
                .fields_consumer(segment_write_state, self.merge_state.segment_info)?;

            consumer.merge(&self.merge_state, &norms_merge_instance)?;
        }

        Ok(())
    }
    fn merge_field_infos(&mut self) -> Result<()> {
        for reader_field_infos in &self.merge_state.field_infos {
            for fi in reader_field_infos.iter() {
                self.field_infos_builder.add(fi.clone())?;
            }
        }

        self.merge_state.merge_field_infos = Arc::new(self.field_infos_builder.finish()?);
        Ok(())
    }
    /// Merge stored fields from each of the segments into the new one.
    ///
    /// # Returns
    ///
    /// The number of documents in all of the readers.
    ///
    /// # Errors
    ///
    /// Returns an error if the index is corrupt or if there is a low-level I/O error.
    fn merge_fields(&mut self) -> Result<i32> {
        let mut fields_writer = LATEST_CODEC.stored_fields_format().fields_writer(
            &self.directory,
            self.merge_state.segment_info,
            self.context,
        )?;

        fields_writer.merge(&mut self.merge_state, &self.directory)
    }
    /// Merge the term vectors from each of the segments into the new one.
    /// # Errors
    ///
    /// Returns an error if there is a low-level I/O error.
    fn merge_term_vectors(&mut self) -> Result<i32> {
        let mut term_vectors_writer = LATEST_CODEC.term_vectors_format().vectors_writer(
            &self.directory,
            self.merge_state.segment_info,
            self.context,
        )?;

        let num_merged = term_vectors_writer.merge(&mut self.merge_state, &self.directory)?;

        debug_assert_eq!(num_merged, self.merge_state.segment_info.max_doc()?);

        Ok(num_merged)
    }
    fn merge_vector_values(&self, _segment_write_state: &SegmentWriteState<&D>) -> Result<()> {
        // let mut writer =
        //     LATEST_CODEC
        //         .knn_vectors_format()
        //         .fields_writer(segment_write_state)?;
        //
        // writer.merge(&mut self.merge_state)?;

        Ok(())
    }
    fn merge_with_logging<F, I>(merger: F, format_name: &str, info_stream: &I) -> Result<i32>
    where
        F: FnOnce() -> Result<i32>,
        I: InfoStream,
    {
        let mut t0 = None;

        if info_stream.enabled("SM") {
            t0 = Some(Instant::now());
        }

        let num_merged = merger()?;

        if let Some(t0) = t0 {
            let elapsed_ms = t0.elapsed().as_millis();
            info_stream.message(
                "SM",
                &format!(
                    "{} ms to merge {} [{} docs]",
                    elapsed_ms, format_name, num_merged
                ),
            );
        }

        Ok(num_merged)
    }
    fn merge_with_logging_with_name<F, I>(
        merger: F,
        segment_write_state: &SegmentWriteState<&D>,
        segment_read_state: &SegmentReadState<&D>,
        format_name: &str,
        num_merged: i32,
        info_stream: &I,
    ) -> Result<()>
    where
        F: FnOnce(&SegmentWriteState<&D>, &SegmentReadState<&D>) -> Result<()>,
        I: InfoStream,
    {
        let mut t0 = None;

        if info_stream.enabled("SM") {
            t0 = Some(Instant::now());
        }

        merger(segment_write_state, segment_read_state)?;

        if let Some(t0) = t0 {
            let elapsed_ms = t0.elapsed().as_millis();
            info_stream.message(
                "SM",
                &format!(
                    "{} ms to merge {} [{} docs]",
                    elapsed_ms, format_name, num_merged
                ),
            );
        }

        Ok(())
    }

    /// True if any merging should happen
    pub(crate) fn should_merge(&self) -> Result<bool> {
        Ok(self.merge_state.segment_info.max_doc()? > 0)
    }
    /// Merges the readers into the directory passed to the constructor.
    ///
    /// # Returns
    ///
    /// The number of documents that were merged.
    ///
    /// # Errors
    ///
    /// Returns an error if the index is corrupt or if there is a low-level I/O error.
    fn merge(&mut self) -> Result<()> {
        if !self.should_merge()? {
            return Err(LuceneError::illegal_state(
                "Merge would result in 0 document segment",
            ));
        }

        self.merge_field_infos()?;
        let info_stream = self.merge_state.info_stream.clone();
        let num_merged = Self::merge_with_logging(
            || self.merge_fields(),
            "stored fields",
            info_stream.as_ref(),
        )?;

        debug_assert_eq!(
            num_merged,
            self.merge_state.segment_info.max_doc()?,
            "numMerged={} vs mergeState.segmentInfo.maxDoc()={}",
            num_merged,
            self.merge_state.segment_info.max_doc()?
        );

        let segment_write_state = SegmentWriteState::new(
            self.merge_state.info_stream.clone(),
            &self.directory,
            self.merge_state.merge_field_infos.clone(),
            self.context,
        );

        let segment_read_state = SegmentReadState::new(
            &self.directory,
            self.merge_state.merge_field_infos.clone(),
            self.context,
        );
        {
            if self.merge_state.merge_field_infos.has_norms() {
                Self::merge_with_logging_with_name(
                    |sws, _srs| self.merge_norms(sws),
                    &segment_write_state,
                    &segment_read_state,
                    "norms",
                    num_merged,
                    info_stream.as_ref(),
                )?;
            }

            Self::merge_with_logging_with_name(
                |sws, srs| self.merge_terms(sws, srs),
                &segment_write_state,
                &segment_read_state,
                "postings",
                num_merged,
                info_stream.as_ref(),
            )?;

            if self.merge_state.merge_field_infos.has_doc_values() {
                Self::merge_with_logging_with_name(
                    |sws, _srs| self.merge_doc_values(sws),
                    &segment_write_state,
                    &segment_read_state,
                    "doc values",
                    num_merged,
                    info_stream.as_ref(),
                )?;
            }

            if self.merge_state.merge_field_infos.has_point_values() {
                Self::merge_with_logging_with_name(
                    |sws, _srs| self.merge_points(sws),
                    &segment_write_state,
                    &segment_read_state,
                    "points",
                    num_merged,
                    info_stream.as_ref(),
                )?;
            }

            if self.merge_state.merge_field_infos.has_vector_values() {
                Self::merge_with_logging_with_name(
                    |sws, _srs| self.merge_vector_values(sws),
                    &segment_write_state,
                    &segment_read_state,
                    "numeric vectors",
                    num_merged,
                    info_stream.as_ref(),
                )?;
            }
        }
        Self::merge_with_logging_with_name(
            |sws, srs| self.merge_field_infos_with_state(sws, srs),
            &segment_write_state,
            &segment_read_state,
            "field infos",
            num_merged,
            info_stream.as_ref(),
        )?;
        if self.merge_state.merge_field_infos.has_term_vectors() {
            Self::merge_with_logging(
                || self.merge_term_vectors(),
                "term vectors",
                info_stream.as_ref(),
            )?;
        }

        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use crate::core::document::document::Document;
    use crate::core::index::BytesRef;
    use crate::core::index::dummy::dummy_composite_reader::DummyCompositeReader;
    use crate::core::index::field_infos::FieldNumbers;
    use crate::core::index::fields::Fields;
    use crate::core::index::index_reader::IndexReader;
    use crate::core::index::leaf_reader::LeafReader;
    use crate::core::index::merge_state::remove_deletes;
    use crate::core::index::multi_reader::MultiReader;
    use crate::core::index::segment_commit_info::SegmentCommitInfo;
    use crate::core::index::segment_info::SegmentInfo;
    use crate::core::index::segment_merger::SegmentMerger;
    use crate::core::index::segment_reader::SegmentReader;
    use crate::core::index::segment_reader::tests::TestSegmentReader;
    use crate::core::index::stored_fields::StoredFields;
    use crate::core::index::term_vectors::TermVectors;
    use crate::core::index::terms::Terms;
    use crate::core::index::terms_enum::TermsEnum;
    use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
    use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
    use crate::core::store::IOContext;
    use crate::core::store::merge_info::MergeInfo;
    use crate::core::util::bit_set::BitSet;
    use crate::core::util::bits::Bits;
    use crate::core::util::bytes_ref_iterator::BytesRefIterator;
    use crate::core::util::error::lucene_error::Result;
    use crate::core::util::fixed_bit_set::FixedBitSet;
    use crate::core::util::info_stream::InfoStreamEnum;
    use crate::core::util::long_values::LongValues;
    use crate::core::util::{LATEST, StringHelper};
    use crate::test::index::doc_helper::{
        DATA, DocHelper, FIELD_2_FREQS, FIELD_2_TEXT, TEXT_FIELD_2_KEY,
    };
    use crate::test::util::lucene_test_case::lucene_test_case_util::{
        DirType, new_directory_shared, new_io_context, new_io_context_with_default, random,
    };
    use crate::test::util::test_util::TestUtil;
    use parking_lot::Mutex;
    use rand::Rng;
    use std::collections::HashMap;
    use std::sync::Arc;

    #[allow(dead_code)] // for quick search
    struct TestSegmentMerger;

    #[test]
    fn test_merge() -> Result<()> {
        let mut random = random();
        let merged_segment = "test";
        let mut doc1 = Document::new();
        DocHelper::setup_doc(&mut doc1);
        let dir = new_directory_shared(&mut random)?;
        let info1 = DocHelper::write_doc(dir, doc1.clone())?;
        let reader1 = SegmentReader::new(&info1, LATEST.major, &new_io_context(&mut random)?)?;

        let mut doc2 = Document::new();
        DocHelper::setup_doc(&mut doc2);
        let dir = new_directory_shared(&mut random)?;
        let info2 = DocHelper::write_doc(dir, doc2.clone())?;
        let reader2 = SegmentReader::new(&info2, LATEST.major, &new_io_context(&mut random)?)?;

        let merged_dir = new_directory_shared(&mut random)?;
        #[allow(clippy::vec_init_then_push)]
        let mut si = SegmentInfo::new(
            merged_dir.clone(),
            Some((*LATEST).clone()),
            None,
            merged_segment,
            -1,
            false,
            false,
            HashMap::new(),
            StringHelper::random_id(),
            HashMap::new(),
            None,
        )?;
        let info_stream = Arc::new(InfoStreamEnum::default());
        let readers = vec![reader1, reader2];
        let context = new_io_context_with_default(
            &mut random,
            &IOContext::with_merge(MergeInfo::new(-1, -1, false, -1))?,
        )?;
        let mut merger = SegmentMerger::new(
            readers.as_ref(),
            &mut si,
            info_stream,
            merged_dir.as_ref(),
            Arc::new(Mutex::new(FieldNumbers::new::<String, String>(None, None)?)),
            &context,
        )?;

        merger.merge()?;
        let docs_merged = merger.merge_state.segment_info.max_doc()?;
        assert_eq!(2, docs_merged);
        // Should be able to open a new SegmentReader against the new directory
        let merged_reader = Arc::new(SegmentReader::new(
            &SegmentCommitInfo::new(si, 0, 0, -1, -1, -1, Some(StringHelper::random_id()))?,
            LATEST.major,
            &new_io_context(&mut random)?,
        )?);

        assert_eq!(2, merged_reader.num_docs()?);

        let new_doc1 = merged_reader.stored_fields()?.document(0)?;
        assert_eq!(
            DocHelper::num_fields(&new_doc1),
            DocHelper::num_fields(&doc1) - DATA.unstored.len()
        );

        let new_doc2 = merged_reader.stored_fields()?.document(1)?;
        assert_eq!(
            DocHelper::num_fields(&new_doc2),
            DocHelper::num_fields(&doc2) - DATA.unstored.len()
        );
        let multi_readers: MultiReader<_, DummyCompositeReader<Arc<SegmentReader<DirType>>>> =
            MultiReader::with_leaf_reader(vec![merged_reader.clone()])?;

        let term_docs = TestUtil::docs_with_reader(
            &mut random,
            &multi_readers,
            TEXT_FIELD_2_KEY,
            &BytesRef::from_string("field"),
            None,
            0,
        )?;
        debug_assert!(term_docs.is_some());
        assert_ne!(NO_MORE_DOCS, term_docs.unwrap().next_doc()?);

        let mut tv_count = 0;
        for field_info in merged_reader.get_field_infos()?.iter() {
            if field_info.has_term_vectors() {
                tv_count += 1;
            }
        }
        assert_eq!(3, tv_count);
        let vector = merged_reader
            .term_vectors()?
            .get(0)?
            .unwrap()
            .terms(TEXT_FIELD_2_KEY)?;
        let v = vector.unwrap();
        assert_eq!(3, v.size()?);

        let mut terms_enum = v.iterator()?;
        let mut i = 0;
        while (terms_enum.next()?).is_some() {
            let term = terms_enum.term()?.as_ref().utf8_to_string()?;
            let freq = terms_enum.total_term_freq()? as i32;
            assert!(FIELD_2_TEXT.contains(&term));
            assert_eq!(FIELD_2_FREQS[i], freq);
            i += 1;
        }

        TestSegmentReader::check_norms(merged_reader)?;

        Ok(())
    }

    #[test]
    fn test_build_doc_map() -> Result<()> {
        let mut random = random();
        let max_doc = TestUtil::next_usize(&mut random, 1, 128);
        let num_docs = TestUtil::next_usize(&mut random, 0, max_doc);

        let mut live_docs = FixedBitSet::new(max_doc);
        for _ in 0..num_docs {
            loop {
                let doc_id = random.random_range(0..max_doc);
                if !live_docs.get(doc_id)? {
                    live_docs.set(doc_id);
                    break;
                }
            }
        }

        let doc_map = remove_deletes(max_doc as i32, &live_docs)?;

        let mut del = 0;
        for i in 0..max_doc {
            if !live_docs.get(i)? {
                del += 1;
            } else {
                assert_eq!(i - del, doc_map.get(i)? as usize);
            }
        }

        Ok(())
    }
    // TODO IMPORTANT 还有未完成的测试
}
