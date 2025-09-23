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
use crate::core::codecs::block_term_state::BlockTermStateEnum;
use crate::core::codecs::block_tree::lucene90_block_tree_terms_reader::VERSION_MSB_VLONG_OUTPUT;
use crate::core::codecs::lucene90::block_tree::lucene90_block_tree_terms_reader::TermsReader;
use crate::core::codecs::lucene90::block_tree::segment_terms_enum::SegmentTermsEnum;
use crate::core::codecs::postings_reader_base::PostingsReaderBase;
use crate::core::index::BytesRef;
use crate::core::index::automaton_terms_enum::AutomatonTermsEnum;
use crate::core::index::base_terms_enum::BaseTermsEnum;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::filtered_terms_enum::{FilteredTermsEnum, FilteredTermsEnumBase};
use crate::core::index::index_options::IndexOptions;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::store::{ByteArrayDataInput, DataInput, IndexInput};
use crate::core::util::ToInt;
use crate::core::util::automation::compiled_automaton::CompiledAutomaton;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::fst_impl::byte_sequence_outputs::ByteSequenceOutputs;
use crate::core::util::fst_impl::fst::{FST, read_metadata};
use crate::core::util::fst_impl::off_heap_fst_store::OffHeapFSTStore;
use parking_lot::Mutex;
use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;

/// BlockTree's implementation of [`Terms`].
#[allow(clippy::type_complexity)]
pub struct FieldReader<I, PR>
where
    I: IndexInput,
    PR: PostingsReaderBase,
{
    pub(crate) num_terms: i64,
    pub(crate) field_info: Arc<FieldInfo>,
    pub(crate) sum_total_term_freq: i64,
    pub(crate) sum_doc_freq: i64,
    pub(crate) doc_count: i32,
    pub(crate) root_block_fp: i64,
    pub(crate) root_code: BytesRef<Arc<Vec<u8>>>,
    pub(crate) min_term: Arc<BytesRef<Vec<u8>>>,
    pub(crate) max_term: Arc<BytesRef<Vec<u8>>>,
    pub(crate) parent: Arc<TermsReader<I, PR>>,
    pub(crate) index: Option<Arc<FST<ByteSequenceOutputs, OffHeapFSTStore<I>>>>,
}
impl<I, PR> FieldReader<I, PR>
where
    I: IndexInput,
    PR: PostingsReaderBase,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new<I1: IndexInput>(
        parent: Arc<TermsReader<I, PR>>,
        field_info: Arc<FieldInfo>,
        num_terms: i64,
        root_code: BytesRef<Vec<u8>>,
        sum_total_term_freq: i64,
        sum_doc_freq: i64,
        doc_count: i32,
        index_start_fp: i64,
        meta_in: &mut I1,
        index_in: Arc<Mutex<I>>,
        min_term: Arc<BytesRef<Vec<u8>>>,
        max_term: Arc<BytesRef<Vec<u8>>>,
    ) -> Result<Self> {
        assert!(num_terms > 0);
        // Read FST metadata and build the index
        let metadata = read_metadata(meta_in, ByteSequenceOutputs)?;
        let store = OffHeapFSTStore::new(index_in, index_start_fp, metadata.num_bytes);
        let index = FST::from_fst_reader(Some(metadata), Some(store))
            .expect("metadata and store are some, should not return None");
        let empty_output = index.metadata().empty_output().cloned();

        let mut v = Self {
            parent,
            field_info,
            num_terms,
            sum_total_term_freq,
            sum_doc_freq,
            doc_count,
            // init with padding value
            root_block_fp: 0,
            // init with padding value
            root_code: BytesRef::new(),
            min_term,
            max_term,
            index: Some(Arc::new(index)),
        };
        // ownership to ByteArrayDataInput
        let mut input =
            ByteArrayDataInput::with_range(root_code.bytes, root_code.offset, root_code.length);
        v.root_block_fp = v.read_vlong_output(&mut input)?;
        // ownership from ByteArrayDataInput
        let root_code = BytesRef {
            bytes: Arc::new(input.bytes),
            offset: root_code.offset,
            length: root_code.length,
        };
        // Get empty output and adjust rootCode
        let root_code_final = match empty_output {
            Some(empty_output) => {
                if root_code.bytes_equals(&empty_output) {
                    empty_output
                } else {
                    root_code
                }
            },
            None => root_code,
        };
        v.root_code = root_code_final;
        Ok(v)
    }
    pub(crate) fn read_vlong_output(&self, input: &mut impl DataInput) -> Result<i64> {
        let version = self.parent.version;
        if version >= VERSION_MSB_VLONG_OUTPUT {
            read_msb_vlong(input)
        } else {
            input.read_vlong()
        }
    }
}
impl<I, PR> Terms for FieldReader<I, PR>
where
    I: IndexInput,
    PR: PostingsReaderBase<TermState = BlockTermStateEnum>,
{
    type TermsEnum = BaseTermsEnum<SegmentTermsEnum<I, PR>>;

    fn iterator(&self) -> Result<Self::TermsEnum> {
        SegmentTermsEnum::new(self.clone())
    }

    type IntersectIter
        = FilteredTermsEnum<Self::TermsEnum, AutomatonTermsEnum>
    where
        Self::TermsEnum: BytesRefIterator,
        AutomatonTermsEnum: FilteredTermsEnumBase;
    fn intersect(
        &self,
        compiled: &mut CompiledAutomaton,
        start_term: Option<BytesRef<Vec<u8>>>,
    ) -> Result<Self::IntersectIter> {
        self.default_intersect(compiled, start_term)
    }

    fn size(&self) -> Result<i64> {
        Ok(self.num_terms)
    }

    fn get_sum_total_term_freq(&self) -> Result<i64> {
        Ok(self.sum_total_term_freq)
    }

    fn get_sum_doc_freq(&self) -> Result<i64> {
        Ok(self.sum_doc_freq)
    }

    fn get_doc_count(&self) -> Result<i32> {
        Ok(self.doc_count)
    }

    fn has_freqs(&self) -> bool {
        self.field_info
            .get_index_options()
            .cmp(&IndexOptions::DocsAndFreqs)
            .to_int()
            >= 0
    }

    fn has_offsets(&self) -> bool {
        self.field_info
            .get_index_options()
            .cmp(&IndexOptions::DocsAndFreqsAndPositionsAndOffsets)
            .to_int()
            >= 0
    }

    fn has_positions(&self) -> bool {
        self.field_info
            .get_index_options()
            .cmp(&IndexOptions::DocsAndFreqsAndPositions)
            .to_int()
            >= 0
    }

    fn has_payloads(&self) -> bool {
        self.field_info.has_payloads()
    }

    fn get_min<'a, T>(&'a self, _iterator: &'a mut T) -> Result<Option<Cow<'a, BytesRef<Vec<u8>>>>>
    where
        T: TermsEnum,
    {
        Ok(Option::from(Cow::Borrowed(self.min_term.as_ref())))
    }

    fn get_max<'a, T>(&'a self, _iterator: &'a mut T) -> Result<Option<Cow<'a, BytesRef<Vec<u8>>>>>
    where
        T: TermsEnum,
    {
        Ok(Option::from(Cow::Borrowed(self.max_term.as_ref())))
    }
}
impl<I, PR> fmt::Display for FieldReader<I, PR>
where
    I: IndexInput,
    PR: PostingsReaderBase,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "BlockTreeTerms(seg={} terms={} postings={} positions={} docs={})",
            self.parent.segment,
            self.num_terms,
            self.sum_doc_freq,
            self.sum_total_term_freq,
            self.doc_count
        )
    }
}
impl<I, PR> Clone for FieldReader<I, PR>
where
    I: IndexInput,
    PR: PostingsReaderBase,
{
    // used to init SegmentTermsEnum
    fn clone(&self) -> Self {
        Self {
            num_terms: self.num_terms,
            field_info: self.field_info.clone(),
            sum_total_term_freq: self.sum_total_term_freq,
            sum_doc_freq: self.sum_doc_freq,
            doc_count: self.doc_count,
            root_block_fp: self.root_block_fp,
            root_code: self.root_code.clone(),
            min_term: self.min_term.clone(),
            max_term: self.max_term.clone(),
            parent: Arc::clone(&self.parent),
            index: Some(Arc::clone(self.index.as_ref().unwrap())),
        }
    }
}

/// Decodes a variable-length `byte[]` in MSB order back to a `long`,
/// as written by
/// [`Lucene90BlockTreeTermsWriter::write_msb_vlong`](crate::core::codecs::lucene90::block_tree::lucene90_block_tree_terms_writer::write_msb_vlong).
///
///
/// Package-private for testing.
pub(crate) fn read_msb_vlong(input: &mut impl DataInput) -> Result<i64> {
    let mut l: i64 = 0;
    loop {
        let b = input.read_byte()?;
        l = (l << 7) | (b & 0x7F) as i64;
        if (b & 0x80) == 0 {
            break;
        }
    }
    Ok(l)
}
