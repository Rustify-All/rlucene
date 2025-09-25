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
use crate::core::codecs::compound_directory::CompoundDirectoryBase;
use crate::core::codecs::doc_values_producer::{DocValuesProducer, Either2DocValuesProducer};
use crate::core::codecs::field_infos_format::FieldInfosFormat;
use crate::core::codecs::fields_producer::FieldsProducerEnum;
use crate::core::codecs::live_docs_format::LiveDocsFormat;
use crate::core::codecs::lucene90_doc_values_producer::Lucene90DocValuesProducer;
use crate::core::codecs::norms_producer::{NormsProducer, NormsProducerEnum};
use crate::core::codecs::points_reader::{PointsReader, PointsReaderEnum};
use crate::core::codecs::stored_fields_reader::StoredFieldsReaderEnum;
use crate::core::codecs::term_vectors_reader::TermVectorsReaderEnum;
use crate::core::codecs::{Codec, get_default_code};
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::fields::Fields;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::leaf_metadata::LeafMetaData;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::pending_deletes::DocBits;
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_core_readers::{CfsOrBaseInput, SegmentCoreReaders};
use crate::core::index::segment_doc_values::SegmentDocValues;
use crate::core::index::segment_doc_values_producer::SegmentDocValuesProducer;
use crate::core::index::term::Term;
use crate::core::store::IOContext;
use crate::core::store::directory::Directory;
use crate::core::util::bits::{Bits, Either2Bits};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::borrow::Cow;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

/// IndexReader implementation over a single segment.
/// Instances pointing to the same segment
/// (but with different deletes, etc.) may share the same core data
pub struct SegmentReader<D>
where
    D: Directory,
{
    pub(crate) info_id: String,
    meta_data: LeafMetaData,
    live_docs: Option<DocBits>,
    hard_live_docs: Option<DocBits>,
    // Normally set to si.maxDoc - si.delDocCount, unless we
    // were created as an NRT reader from IW, in which case IW
    // tells us the number of live docs:
    num_docs: i32,
    core: Arc<SegmentCoreReaders<D>>,
    seg_doc_values: Arc<SegmentDocValues<D>>,
    /// True if we are holding RAM only liveDocs or DV updates,
    /// i.e. the SegmentCommitInfo delGen doesn't match our liveDocs.
    is_nrt: bool,
    doc_values_producer: Option<DocValuesProducers<D>>,
    field_infos: Arc<FieldInfos>,
}
impl<D> SegmentReader<D>
where
    D: Directory,
{
    pub(crate) fn new(
        si: &SegmentCommitInfo<D>,
        created_version_major: i32,
        context: &IOContext,
    ) -> Result<Self> {
        let meta_data = LeafMetaData::new(
            created_version_major,
            Some(si.info.get_min_version().unwrap().clone()),
            Some(si.info.get_index_sort().unwrap().clone()),
            si.info.get_has_blocks(),
        )?;

        let is_nrt = false;
        let dir = si.info.dir.as_ref();
        let core = Arc::new(SegmentCoreReaders::new(dir, si, context)?);
        let seg_doc_values = Arc::new(SegmentDocValues::new());
        let num_docs = si.info.max_doc()? - si.get_del_count();
        let mut segment_reader = Self {
            info_id: si.info.get_id_str().clone(),
            meta_data,
            is_nrt,
            core,
            seg_doc_values,
            hard_live_docs: None,
            live_docs: None,
            num_docs,
            field_infos: Arc::new(FieldInfos::default()),
            doc_values_producer: None,
        };
        let result = (|| {
            let (hard_live_docs, live_docs) = if si.has_deletions() {
                // NOTE: the bitvector is stored using the regular directory, not cfs
                let ld = Arc::new(get_default_code().live_docs_format().read_live_docs(
                    dir,
                    si,
                    &IOContext::read_once_io_context()?,
                )?);
                (Some(DocBits::A(ld.clone())), Some(DocBits::A(ld)))
            } else {
                debug_assert_eq!(si.get_del_count(), 0);
                (None, None)
            };

            let field_infos =
                Self::init_field_infos(si, segment_reader.core.core_field_infos.clone())?;
            let doc_values_producer = Self::init_doc_values_producer(
                si,
                field_infos.clone(),
                &segment_reader.seg_doc_values,
                &segment_reader.core,
            )?;

            debug_assert!(Self::assert_live_docs(is_nrt, &hard_live_docs, &live_docs)?);

            Ok((hard_live_docs, live_docs, field_infos, doc_values_producer))
        })();
        match result {
            Ok(r) => {
                segment_reader.hard_live_docs = r.0;
                segment_reader.live_docs = r.1;
                segment_reader.field_infos = r.2;
                segment_reader.doc_values_producer = r.3;
                Ok(segment_reader)
            },
            Err(e) => {
                segment_reader.do_close()?;
                Err(e)
            },
        }
    }
    /// Create new SegmentReader sharing core from a previous SegmentReader and using the provided liveDocs,
    /// and recording whether those liveDocs were carried in ram (isNRT=true).
    pub(crate) fn new_from_reader(
        si: &SegmentCommitInfo<D>,
        sr: &SegmentReader<D>,
        live_docs: Option<DocBits>,
        hard_live_docs: Option<DocBits>,
        num_docs: i32,
        is_nrt: bool,
    ) -> Result<Self> {
        let max_doc = si.info.max_doc()?;
        if num_docs > max_doc {
            return Err(LuceneError::illegal_argument(format!(
                "numDocs={} but maxDoc={}",
                num_docs, max_doc
            )));
        }
        if let Some(ld) = &live_docs {
            let len = ld.length();
            if len != max_doc {
                return Err(LuceneError::illegal_argument(format!(
                    "maxDoc={} but liveDocs.size()={}",
                    max_doc, len
                )));
            }
        }

        let meta_data = sr.meta_data.clone();
        let core = sr.core.clone();
        let seg_doc_values = sr.seg_doc_values.clone();
        core.inc_ref()?;
        debug_assert!(Self::assert_live_docs(is_nrt, &hard_live_docs, &live_docs)?);
        let mut segment_reader = Self {
            info_id: si.info.get_id_str().clone(),
            meta_data,
            is_nrt,
            core: core.clone(),
            seg_doc_values: seg_doc_values.clone(),
            hard_live_docs,
            live_docs,
            num_docs,
            field_infos: Arc::new(FieldInfos::default()),
            doc_values_producer: None,
        };
        let result = (|| {
            let field_infos = Self::init_field_infos(si, core.core_field_infos.clone())?;
            let doc_values_producer =
                Self::init_doc_values_producer(si, field_infos.clone(), &seg_doc_values, &core)?;
            Ok((field_infos, doc_values_producer))
        })();
        match result {
            Ok(r) => {
                segment_reader.field_infos = r.0;
                segment_reader.doc_values_producer = r.1;
                Ok(segment_reader)
            },
            Err(e) => {
                segment_reader.do_close()?;
                Err(e)
            },
        }
    }
    fn assert_live_docs(
        is_nrt: bool,
        hard_live_docs: &Option<DocBits>,
        live_docs: &Option<DocBits>,
    ) -> Result<bool> {
        match is_nrt {
            true => debug_assert!(
                hard_live_docs.is_none() || live_docs.is_some(),
                "liveDocs must be non-null if hardLiveDocs are non-null"
            ),
            false => debug_assert!(
                match (hard_live_docs, live_docs) {
                    (None, None) => true,
                    (Some(reader_bits), Some(current_bits)) => match (reader_bits, current_bits) {
                        (Either2Bits::A(r_bits), Either2Bits::A(c_bits)) =>
                            Arc::ptr_eq(r_bits, c_bits),
                        (Either2Bits::B(r_bits), Either2Bits::B(c_bits)) =>
                            match (r_bits, c_bits) {
                                (Either2Bits::A(r_fixed), Either2Bits::A(c_fixed)) => {
                                    Arc::ptr_eq(r_fixed, c_fixed)
                                },
                                (Either2Bits::B(_), Either2Bits::B(_)) => {
                                    return Err(LuceneError::illegal_state(
                                        "live docs should be FixedBitSet",
                                    ));
                                },
                                _ => false,
                            },
                        _ => false,
                    },
                    _ => false,
                },
                "non-nrt case must have identical liveDocs"
            ),
        }
        Ok(true)
    }
    /// init most recent DocValues for the current commit
    fn init_doc_values_producer(
        si: &SegmentCommitInfo<D>,
        field_infos: Arc<FieldInfos>,
        seg_doc_values: &SegmentDocValues<D>,
        core: &SegmentCoreReaders<D>,
    ) -> Result<Option<DocValuesProducers<D>>> {
        if !field_infos.has_doc_values() {
            return Ok(None);
        }
        let dir = &core.cfs_reader;

        let producer = match si.has_field_updates() {
            true => Either2DocValuesProducer::A(SegmentDocValuesProducer::new(
                si,
                dir,
                Arc::clone(&core.core_field_infos),
                &field_infos,
                seg_doc_values,
            )?),
            // simple case, no DocValues updates
            false => Either2DocValuesProducer::B(seg_doc_values.get_doc_values_producer(
                -1,
                si,
                dir,
                field_infos,
            )?),
        };

        Ok(Some(producer))
    }
    /// init most recent FieldInfos for the current commit
    fn init_field_infos(
        si: &SegmentCommitInfo<D>,
        core_field_infos: Arc<FieldInfos>,
    ) -> Result<Arc<FieldInfos>> {
        if !si.has_field_updates() {
            return Ok(core_field_infos);
        }

        // updates always outside of CFS
        let fis_format = get_default_code().field_infos_format();
        let segment_suffix = num_bigint::BigInt::from(si.get_field_infos_gen()).to_str_radix(36);

        let infos = fis_format.read(
            si.info.dir.as_ref(),
            &si.info,
            &segment_suffix,
            &IOContext::read_once_io_context()?,
        )?;

        Ok(Arc::new(infos))
    }
}
pub type DocValuesProducers<D> = Either2DocValuesProducer<
    SegmentDocValuesProducer<D>,
    Arc<Lucene90DocValuesProducer<CfsOrBaseInput<D>>>,
>;

impl<D> Display for SegmentReader<D>
where
    D: Directory,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        // TODO
        write!(f, "SegmentReader({})", self.info_id)
    }
}

impl<D> IndexReader for SegmentReader<D>
where
    D: Directory,
{
    fn max_doc(&self) -> Result<i32> {
        todo!()
    }

    fn num_docs(&self) -> Result<i32> {
        Ok(self.num_docs)
    }

    fn do_close(&mut self) -> Result<()> {
        if self.core.dec_ref().is_err()
            && let Some(dv) = &self.doc_values_producer
        {
            match dv {
                Either2DocValuesProducer::A(a) => self.seg_doc_values.dec_ref(&a.dv_gens)?,
                Either2DocValuesProducer::B(_) => {
                    let gens = vec![-1_i64, 1];
                    self.seg_doc_values.dec_ref(&gens)?
                },
            }
        }
        Ok(())
    }

    fn doc_freq(&self, term: &Term) -> Result<i32> {
        LeafReader::doc_freq(self, term)
    }

    fn total_term_freq(&self, term: &Term) -> Result<i64> {
        LeafReader::total_term_freq(self, term)
    }

    fn sum_doc_freq(&self, field: &str) -> Result<i64> {
        LeafReader::sum_doc_freq(self, field)
    }

    fn doc_count(&self, field: &str) -> Result<i32> {
        LeafReader::doc_count(self, field)
    }

    fn sum_total_term_freq(&self, field: &str) -> Result<i64> {
        LeafReader::sum_total_term_freq(self, field)
    }
}
impl<D> LeafReader for SegmentReader<D>
where
    D: Directory,
{
    type Terms = <<Self as CodecReader>::FieldsProducer as Fields>::Terms;

    fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
        CodecReader::terms(self, field)
    }

    type NumericDocValues =
        <<Self as CodecReader>::DocValuesProducer as DocValuesProducer>::NumericDocValues;

    fn get_numeric_doc_values(&self, field: &str) -> Result<Option<Self::NumericDocValues>> {
        CodecReader::get_numeric_doc_values(self, field)
    }

    type BinaryDocValues =
        <<Self as CodecReader>::DocValuesProducer as DocValuesProducer>::BinaryDocValues;

    fn get_binary_doc_values(&self, field: &str) -> Result<Option<Self::BinaryDocValues>> {
        CodecReader::get_binary_doc_values(self, field)
    }

    type SortedDocValues =
        <<Self as CodecReader>::DocValuesProducer as DocValuesProducer>::SortedDocValues;

    fn get_sorted_doc_values(&self, field: &str) -> Result<Option<Self::SortedDocValues>> {
        CodecReader::get_sorted_doc_values(self, field)
    }

    type SortedNumericDocValues =
        <<Self as CodecReader>::DocValuesProducer as DocValuesProducer>::SortedNumericDocValues;

    fn get_sorted_numeric_doc_values(
        &self,
        field: &str,
    ) -> Result<Option<Self::SortedNumericDocValues>> {
        CodecReader::get_sorted_numeric_doc_values(self, field)
    }

    type SortedSetDocValues =
        <<Self as CodecReader>::DocValuesProducer as DocValuesProducer>::SortedSetDocValues;

    fn get_sorted_set_doc_values(&self, field: &str) -> Result<Option<Self::SortedSetDocValues>> {
        CodecReader::get_sorted_set_doc_values(self, field)
    }

    type NormNumericDocValues =
        <<Self as CodecReader>::NormsProducer as NormsProducer>::NumericDocValues;

    fn get_norm_values(&self, field: &str) -> Result<Option<Self::NormNumericDocValues>> {
        CodecReader::get_norm_values(self, field)
    }

    type DocValuesSkipper =
        <<Self as CodecReader>::DocValuesProducer as DocValuesProducer>::DocValuesSkipper;

    fn get_doc_values_skipper(&self, field: &str) -> Result<Option<Self::DocValuesSkipper>> {
        CodecReader::get_doc_values_skipper(self, field)
    }

    fn get_field_infos(&self) -> Result<Arc<FieldInfos>> {
        Ok(self.field_infos.clone())
    }

    type Bits = DocBits;

    fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
        match &self.live_docs {
            Some(DocBits::A(a)) => Ok(Some(DocBits::A(Arc::clone(a)))),
            Some(DocBits::B(b)) => match b {
                Either2Bits::A(a) => Ok(Some(DocBits::B(Either2Bits::A(Arc::clone(a))))),
                Either2Bits::B(_) => Err(LuceneError::illegal_state(
                    "live docs should be FixedBitSet",
                )),
            },
            None => Ok(None),
        }
    }

    type PointValuesType = <<Self as CodecReader>::PointsReader as PointsReader>::PointValuesType;

    fn get_point_values(&self, field: &str) -> Result<Option<Self::PointValuesType>> {
        CodecReader::get_point_values(self, field)
    }

    fn check_integrity(&self) -> Result<()> {
        CodecReader::default_check_integrity(self)?;
        if let Some(dv) = &self.core.cfs_reader {
            dv.sub_compound_dir.check_integrity()?;
        }
        Ok(())
    }
}
impl<D> CodecReader for SegmentReader<D>
where
    D: Directory,
{
    type StoredFieldsReader = StoredFieldsReaderEnum<CfsOrBaseInput<D>>;
    type TermVectorsReader = TermVectorsReaderEnum<CfsOrBaseInput<D>>;
    type NormsProducer = NormsProducerEnum<CfsOrBaseInput<D>>;
    type DocValuesProducer = DocValuesProducers<D>;
    type FieldsProducer = FieldsProducerEnum<CfsOrBaseInput<D>>;
    type PointsReader = PointsReaderEnum<CfsOrBaseInput<D>>;

    fn get_fields_reader(&self) -> Result<Cow<'_, Self::StoredFieldsReader>> {
        self.ensure_open()?;
        Ok(Cow::Owned(self.core.fields_reader_orig.clone()))
    }

    fn get_term_vectors_reader(&self) -> Result<Option<Cow<'_, Self::TermVectorsReader>>> {
        self.ensure_open()?;
        Ok(self
            .core
            .term_vectors_reader_orig
            .as_ref()
            .map(|tv| Cow::Owned(tv.clone())))
    }

    fn get_norms_reader(&self) -> Result<Option<Cow<'_, Self::NormsProducer>>> {
        self.ensure_open()?;
        Ok(self.core.norms_producer.as_ref().map(Cow::Borrowed))
    }

    fn get_doc_values_reader(&self) -> Result<Option<Cow<'_, Self::DocValuesProducer>>> {
        self.ensure_open()?;
        Ok(self.doc_values_producer.as_ref().map(Cow::Borrowed))
    }

    fn get_postings_reader(&self) -> Result<Option<Cow<'_, Self::FieldsProducer>>> {
        self.ensure_open()?;
        Ok(self.core.fields.as_ref().map(Cow::Borrowed))
    }

    fn get_points_reader(&self) -> Result<Option<Cow<'_, Self::PointsReader>>> {
        self.ensure_open()?;
        Ok(self.core.points_reader.as_ref().map(Cow::Borrowed))
    }
}
