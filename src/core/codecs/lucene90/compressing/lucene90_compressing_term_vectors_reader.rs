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
use crate::core::codecs::CodecUtil;
use crate::core::codecs::compressing::lucene90_compressing_term_vectors_writer::FLAGS_BITS;
use crate::core::codecs::compressing::lucene90_compressing_term_vectors_writer::{
    META_VERSION_START, OFFSETS, PACKED_BLOCK_SIZE, PAYLOADS, POSITIONS, VECTORS_EXTENSION,
    VECTORS_INDEX_CODEC_NAME, VECTORS_INDEX_EXTENSION, VECTORS_META_EXTENSION, VERSION_CURRENT,
    VERSION_START,
};
use crate::core::codecs::compression::compression_mode::{
    CompressionModeBase, CompressionModeEnum, DecompressorEnum,
};
use crate::core::codecs::compression::decompressor::Decompressor;
use crate::core::codecs::lucene90::fields_index::{FieldsIndex, FieldsIndexEnum};
use crate::core::codecs::lucene90::fields_index_reader::FieldsIndexReader;
use crate::core::codecs::term_vectors_reader::{DefaultTermVectorsReader, TermVectorsReader};
use crate::core::index::automaton_terms_enum::AutomatonTermsEnum;
use crate::core::index::base_terms_enum::BaseTermsEnum;
use crate::core::index::dummy::dummy_term_state_type::DummyTermState;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::fields::Fields;
use crate::core::index::filtered_terms_enum::{FilteredTermsEnum, FilteredTermsEnumBase};
use crate::core::index::postings_enum::{FREQS, PostingsEnum};
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::slow_impacts_enum::SlowImpactsEnum;
use crate::core::index::term_vectors::{RawTermVectors, TermVectors};
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::index::{BytesRef, IndexFileNames};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::store::byte_buffers_data_input::{
    ByteBuffersDataInput, ByteBuffersDataInputOwned,
};
use crate::core::store::directory::Directory;
use crate::core::store::{
    ByteArrayDataInput, ByteBuffersDataOutput, DataInput, IOContext, IndexInput, ReadAdvice,
};
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::automation::compiled_automaton::CompiledAutomaton;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::clone::TryClone;
use crate::core::util::dummy::dummy_attribute_source::DummyAttributeSource;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::iterator::{VecIter, VecIteratorExt};
use crate::core::util::long_values::LongValues;
use crate::core::util::packed::Format::Packed;
use crate::core::util::packed::block_packed_reader_iterator::BlockPackedReaderIterator;
use crate::core::util::packed::direct_reader::DirectReader;
use crate::core::util::packed::direct_writer::{DirectWriter, bits_required};
use crate::core::util::packed::{PackedImpl, PackedInts, ReaderIterator};
use crate::core::util::{ToInt, TryIntoInt};
use std::borrow::Cow;
use std::io::Cursor;
use std::rc::Rc;
use std::sync::Arc;

pub struct Lucene90CompressingTermVectorsReader<I>
where
    I: IndexInput,
{
    field_infos: Arc<FieldInfos>,
    pub(crate) index_reader: FieldsIndexEnum<I>,
    pub(crate) vectors_stream: I,
    version: i32,
    packed_ints_version: i32,
    compression_mode: CompressionModeEnum,
    decompressor: DecompressorEnum,
    chunk_size: i32,
    num_docs: i32,
    closed: bool,
    // number of written blocks
    num_chunks: i64,
    // number of incomplete compressed blocks written
    num_dirty_chunks: i64,
    // cumulative number of docs in incomplete chunks
    num_dirty_docs: i64,
    // end of the data section
    max_pointer: usize,
    block_state: BlockState,
    // Cache of recently prefetched block IDs. This helps reduce chances of prefetching the same block
    // multiple times, which is otherwise likely due to index sorting or recursive graph bisection
    // clustering similar documents together. NOTE: this cache must be small since it's fully scanned.
    prefetched_block_id_cache: [i64; PREFETCH_CACHE_SIZE],
    prefetched_block_id_cache_index: usize,
}

impl<I> Lucene90CompressingTermVectorsReader<I>
where
    I: IndexInput,
{
    pub fn new<D1, D2>(
        dir: &D1,
        si: &SegmentInfo<D2>,
        segment_suffix: &str,
        field_infos: Arc<FieldInfos>,
        context: &IOContext,
        format_name: &str,
        compression_mode: CompressionModeEnum,
    ) -> Result<Self>
    where
        D1: Directory<IndexInput = I>,
        D2: Directory,
    {
        let segment = &si.name;
        let num_docs = si.max_doc()?;
        let mut meta_in = None;

        let result: Result<Self> = (|| {
            let vectors_stream_fn =
                IndexFileNames::segment_file_name(segment, segment_suffix, VECTORS_EXTENSION);
            let mut vectors_stream = dir.open_input(
                &vectors_stream_fn,
                &context.with_read_advice_self(ReadAdvice::Random)?,
            )?;

            let version = CodecUtil::check_index_header(
                &mut vectors_stream,
                format_name,
                VERSION_START,
                VERSION_CURRENT,
                si.get_id(),
                segment_suffix,
            )?;
            debug_assert_eq!(
                CodecUtil::index_header_length(format_name, segment_suffix),
                vectors_stream.get_file_pointer()?
            );

            let meta_stream_fn =
                IndexFileNames::segment_file_name(segment, segment_suffix, VECTORS_META_EXTENSION);
            let mut meta = dir.open_checksum_input(&meta_stream_fn)?;

            CodecUtil::check_index_header(
                &mut meta,
                &format!("{}Meta", VECTORS_INDEX_CODEC_NAME),
                META_VERSION_START,
                version,
                si.get_id(),
                segment_suffix,
            )?;

            let packed_ints_version = meta.read_vint()?;
            let chunk_size = meta.read_vint()?;
            // NOTE: data file is too costly to verify checksum against all the bytes on open,
            // but for now we at least verify proper structure of the checksum footer: which looks
            // for FOOTER_MAGIC + algorithmID. This is cheap and can detect some forms of corruption
            // such as file truncation.
            CodecUtil::retrieve_checksum(&mut vectors_stream)?;

            let fields_index_reader = FieldsIndexReader::new(
                dir,
                si.name.clone(),
                segment_suffix,
                VECTORS_INDEX_EXTENSION,
                VECTORS_INDEX_CODEC_NAME,
                si.get_id(),
                &mut meta,
                context,
            )?;
            let max_pointer = fields_index_reader.get_max_pointer();
            let index_reader = FieldsIndexEnum::Lucene90(fields_index_reader);

            let num_chunks = meta.read_vlong()?;
            let num_dirty_chunks = meta.read_vlong()?;
            let num_dirty_docs = meta.read_vlong()?;

            if num_chunks < num_dirty_chunks {
                return Err(LuceneError::corrupt_index(format!(
                    "Cannot have more dirty chunks than chunks: numChunks={num_chunks}, numDirtyChunks={num_dirty_chunks} (resource={meta})"
                )));
            }
            if (num_dirty_chunks == 0) != (num_dirty_docs == 0) {
                return Err(LuceneError::corrupt_index(format!(
                    "Cannot have dirty chunks without dirty docs or vice-versa: numDirtyChunks={num_dirty_chunks}, numDirtyDocs={num_dirty_docs} (resource={meta})"
                )));
            }
            if num_dirty_docs < num_dirty_chunks {
                return Err(LuceneError::corrupt_index(format!(
                    "Cannot have more dirty chunks than documents within dirty chunks: numDirtyChunks={num_dirty_chunks}, numDirtyDocs={num_dirty_docs} (resource={meta})"
                )));
            }

            let decompressor = compression_mode.new_decompressor();

            CodecUtil::check_footer(&mut meta)?;
            meta_in = Some(meta);

            let prefetched_block_id_cache = [-1i64; PREFETCH_CACHE_SIZE];

            Ok(Self {
                field_infos,
                compression_mode,
                version,
                packed_ints_version,
                chunk_size,
                num_docs,
                vectors_stream,
                index_reader,
                max_pointer,
                num_chunks,
                num_dirty_chunks,
                num_dirty_docs,
                decompressor,
                prefetched_block_id_cache,
                prefetched_block_id_cache_index: 0,
                closed: false,
                block_state: BlockState::new(None, None, 0),
            })
        })();

        match result {
            Ok(reader) => Ok(reader),
            Err(e) => {
                if let Some(ref mut meta) = meta_in {
                    return Err(CodecUtil::check_footer_with_error(meta, e));
                }
                Err(e)
            },
        }
    }

    pub fn with_reader(reader: &Lucene90CompressingTermVectorsReader<I>) -> Result<Self> {
        Ok(Self {
            field_infos: Arc::clone(&reader.field_infos),
            vectors_stream: reader.vectors_stream.try_clone()?,
            index_reader: reader.index_reader.try_clone()?,
            packed_ints_version: reader.packed_ints_version,
            compression_mode: reader.compression_mode.clone(),
            decompressor: reader.decompressor.clone(),
            chunk_size: reader.chunk_size,
            num_docs: reader.num_docs,
            version: reader.version,
            num_chunks: reader.num_chunks,
            num_dirty_chunks: reader.num_dirty_chunks,
            num_dirty_docs: reader.num_dirty_docs,
            max_pointer: reader.max_pointer,
            closed: false,
            block_state: BlockState::new(None, None, 0),
            prefetched_block_id_cache: [-1; PREFETCH_CACHE_SIZE],
            prefetched_block_id_cache_index: 0,
        })
    }

    pub(crate) fn get_compression_mode(&self) -> &CompressionModeEnum {
        &self.compression_mode
    }

    pub(crate) fn get_chunk_size(&self) -> i32 {
        self.chunk_size
    }

    pub(crate) fn get_packed_ints_version(&self) -> i32 {
        self.packed_ints_version
    }

    pub(crate) fn get_version(&self) -> i32 {
        self.version
    }

    pub(crate) fn get_max_pointer(&self) -> usize {
        self.max_pointer
    }

    pub(crate) fn get_num_dirty_docs(&self) -> Result<i64> {
        if self.version != VERSION_CURRENT {
            return Err(LuceneError::illegal_state(
                "get_num_dirty_docs should only ever get called when the reader is on the current version",
            ));
        }
        debug_assert!(self.num_dirty_docs >= 0);
        Ok(self.num_dirty_docs)
    }

    pub(crate) fn get_num_dirty_chunks(&self) -> Result<i64> {
        if self.version != VERSION_CURRENT {
            return Err(LuceneError::illegal_state(
                "get_num_dirty_chunks should only ever get called when the reader is on the current version",
            ));
        }
        debug_assert!(self.num_dirty_chunks >= 0);
        Ok(self.num_dirty_chunks)
    }

    pub(crate) fn get_num_chunks(&self) -> Result<i64> {
        if self.version != VERSION_CURRENT {
            return Err(LuceneError::illegal_state(
                "get_num_chunks should only ever get called when the reader is on the current version",
            ));
        }
        debug_assert!(self.num_chunks >= 0);
        Ok(self.num_chunks)
    }

    #[allow(dead_code)]
    pub(crate) fn num_docs(&self) -> i32 {
        // not used in Java Lucene, so we did not impl it
        0
    }
    pub fn ensure_open(&self) -> Result<()> {
        if self.closed {
            Err(LuceneError::already_closed("this FieldsReader is closed"))
        } else {
            Ok(())
        }
    }
    /// # Note
    /// `indexReader` and `fieldsStream` will automatically release resource in
    /// Rust Lucene, but we still keep this method for compatibility with
    /// Java Lucene.
    pub fn close(&mut self) {
        if !self.closed {
            self.closed = true;
        }
    }
    fn slice(input: &mut I) -> Result<ByteBuffersDataInputOwned> {
        let length = input.read_vint()?.try_convert()?;
        let mut buf = vec![0; length];
        input.read_bytes(&mut buf, 0, length)?;
        ByteBuffersDataInput::new(vec![Cursor::new(buf)], length)
    }
    pub(crate) fn is_loaded(&self, doc_id: i32) -> bool {
        let bs = &self.block_state;
        let doc_base = match bs.doc_base {
            Some(v) => v as i32,
            None => -1,
        };
        doc_base <= doc_id && doc_id < doc_base + bs.chunk_docs
    }
    fn position_index(
        skip: usize,
        num_fields: usize,
        num_terms: &mut impl LongValues,
        term_freqs: &[usize],
    ) -> Result<Vec<Vec<usize>>> {
        let mut position_index = vec![Vec::new(); num_fields];
        let mut term_index = 0;
        for i in 0..skip {
            let term_count = num_terms.get_mut(i)?;
            term_index += term_count;
        }
        let mut term_index = term_index as usize;
        for (i, slot) in position_index.iter_mut().enumerate().take(num_fields) {
            let term_count = num_terms.get_mut(skip + i)? as usize;
            let mut arr = vec![0; term_count + 1];
            for j in 0..term_count {
                let freq = term_freqs[term_index + j];
                arr[j + 1] = arr[j] + freq;
            }
            term_index += term_count;
            *slot = arr;
        }
        Ok(position_index)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn read_positions(
        &mut self,
        skip: usize,
        num_fields: usize,
        flags: &mut impl LongValues,
        num_terms: &mut impl LongValues,
        term_freqs: &[usize],
        flag: i32,
        total_positions: usize,
        position_index: &[Vec<usize>],
    ) -> Result<Vec<Vec<usize>>> {
        let mut positions = vec![Vec::new(); num_fields];
        // reset reader
        let mut reader =
            BlockPackedReaderIterator::new(self.packed_ints_version, PACKED_BLOCK_SIZE, 0)?;
        reader.reset(total_positions);

        // skip
        let mut to_skip = 0;
        let mut term_index = 0;
        for i in 0..skip {
            let f = flags.get_mut(i)? as i32;
            let term_count = num_terms.get_mut(i)? as usize;
            if (f & flag) != 0 {
                for j in 0..term_count {
                    to_skip += term_freqs[term_index + j];
                }
            }
            term_index += term_count;
        }
        reader.skip(to_skip, &mut self.vectors_stream)?;
        // read doc positions
        for i in 0..num_fields {
            let f = flags.get_mut(skip + i)? as i32;
            let term_count = num_terms.get_mut(skip + i)? as usize;

            if (f & flag) != 0 {
                let total_freq = position_index[i][term_count];
                let mut field_positions = vec![0; total_freq];
                let mut j = 0;
                while j < total_freq {
                    let next_positions =
                        reader.next_batch(total_freq - j, &mut self.vectors_stream)?;
                    let slice = &next_positions.longs
                        [next_positions.offset..next_positions.offset + next_positions.length];
                    for &val in slice {
                        field_positions[j] = val as usize;
                        j += 1;
                    }
                }
                positions[i] = field_positions;
            }
        }
        let read = reader.ord();
        reader.skip(total_positions - read, &mut self.vectors_stream)?;
        Ok(positions)
    }
}
impl<I> TermVectors for Lucene90CompressingTermVectorsReader<I>
where
    I: IndexInput,
{
    fn prefetch(&mut self, doc_id: i32) -> Result<()> {
        let block_id = self.index_reader.get_block_id(doc_id)?;
        for &prefetched_block_id in &self.prefetched_block_id_cache {
            if prefetched_block_id == block_id {
                return Ok(());
            }
        }
        let block_start_pointer = self.index_reader.get_block_start_pointer(block_id)?;
        let block_length = self.index_reader.get_block_length(block_id)?;
        self.vectors_stream
            .prefetch(block_start_pointer, block_length)?;
        let idx = self.prefetched_block_id_cache_index & PREFETCH_CACHE_MASK;
        self.prefetched_block_id_cache[idx] = block_id;
        self.prefetched_block_id_cache_index += 1;
        Ok(())
    }

    type Fields = TVFields;

    fn get(&mut self, doc: i32) -> Result<Option<Self::Fields>> {
        self.ensure_open()?;
        // seek to the right place
        let start_pointer = if self.is_loaded(doc) {
            self.block_state
                .start_pointer
                .ok_or_else(|| LuceneError::illegal_state("start_pointer was None"))?
        } else {
            self.index_reader.get_start_pointer(doc)?
        };
        self.vectors_stream.seek(start_pointer)?;
        // decode
        // - docBase: first doc ID of the chunk
        // - chunkDocs: number of docs of the chunk
        let doc_base = self.vectors_stream.read_vint()?;
        let chunk_docs = ((self.vectors_stream.read_vint()? as u32) >> 1) as i32;
        if doc < doc_base || doc >= doc_base + chunk_docs || (doc_base + chunk_docs) > self.num_docs
        {
            return Err(LuceneError::corrupt_index(format!(
                "docBase={},chunkDocs={},doc={},resource={}",
                doc_base, chunk_docs, doc, &self.vectors_stream
            )));
        }
        self.block_state =
            BlockState::new(Some(start_pointer), Some(doc_base as usize), chunk_docs);
        let mut reader =
            BlockPackedReaderIterator::new(self.packed_ints_version, PACKED_BLOCK_SIZE, 0)?;
        let (skip, num_fields, total_fields) = if chunk_docs == 1 {
            let nf = self.vectors_stream.read_vint()? as usize;
            (0, nf, nf)
        } else {
            reader.reset(chunk_docs as usize);
            let mut sum = 0;
            for _ in doc_base..doc {
                sum += reader.next_value(&mut self.vectors_stream)? as usize;
            }
            let skip = sum;
            let num_fields = reader.next_value(&mut self.vectors_stream)? as usize;
            sum += num_fields;
            for _ in (doc + 1)..(doc_base + chunk_docs) {
                sum += reader.next_value(&mut self.vectors_stream)? as usize;
            }
            (skip, num_fields, sum)
        };

        if num_fields == 0 {
            return Ok(None);
        }

        // read field numbers that have term vectors
        let field_nums = {
            let token = self.vectors_stream.read_byte()?;
            debug_assert!(token != 0); // means no term vectors, cannot happen since we checked for numFields == 0
            let bits_per_field_num = (token & 0x1F) as i32;
            let mut total_distinct_fields = (token >> 5) as i32;
            if total_distinct_fields == 0x07 {
                total_distinct_fields += self.vectors_stream.read_vint()?;
            }
            total_distinct_fields += 1;
            let mut it = PackedInts::get_reader_iterator_no_header(
                &mut self.vectors_stream,
                Packed(PackedImpl::new(0)),
                self.packed_ints_version,
                total_distinct_fields,
                bits_per_field_num,
                1,
            )?;
            let mut field_nums = vec![0; total_distinct_fields as usize];
            for slot in field_nums.iter_mut().take(total_distinct_fields as usize) {
                *slot = it.next()? as i32;
            }
            field_nums
        };

        // read field numbers and flags
        let mut field_num_offs = vec![0; num_fields];
        let mut flags = {
            let bits_per_off = bits_required((field_nums.len() - 1) as i64)?;
            let mut all_field_num_offs =
                DirectReader::get_instance(Self::slice(&mut self.vectors_stream)?, bits_per_off)?;
            let v = self.vectors_stream.read_vint()?;
            let flags = match v {
                0 => {
                    let mut field_flags = DirectReader::get_instance(
                        Self::slice(&mut self.vectors_stream)?,
                        *FLAGS_BITS,
                    )?;
                    let mut out = ByteBuffersDataOutput::new();
                    let mut writer =
                        DirectWriter::get_instance(&mut out, total_fields as i64, *FLAGS_BITS)?;
                    for i in 0..total_fields {
                        let field_num_off = all_field_num_offs.get_mut(i)? as usize;
                        debug_assert!(field_num_off < field_nums.len());
                        writer.add(field_flags.get_mut(field_num_off)?)?;
                    }
                    writer.finish()?;
                    DirectReader::get_instance(out.get_data_input_owner()?, *FLAGS_BITS)?
                },
                1 => {
                    DirectReader::get_instance(Self::slice(&mut self.vectors_stream)?, *FLAGS_BITS)?
                },
                _ => {
                    return Err(LuceneError::illegal_state(format!(
                        "invalid flag selector: {v}"
                    )));
                },
            };
            for (slot, off) in field_num_offs.iter_mut().zip((skip..).take(num_fields)) {
                *slot = all_field_num_offs.get_mut(off)? as usize;
            }
            flags
        };

        // number of terms per field for all fields
        let (mut num_terms, total_terms) = {
            let bits_required = self.vectors_stream.read_vint()?;
            let mut num_terms =
                DirectReader::get_instance(Self::slice(&mut self.vectors_stream)?, bits_required)?;
            let mut sum = 0;
            for i in 0..total_fields {
                sum += num_terms.get_mut(i)?;
            }
            (num_terms, sum as usize)
        };

        // term lengths
        let mut doc_off = 0;
        let mut doc_len = 0;
        #[allow(unused)]
        let mut total_len = 0;
        let mut field_lengths = vec![0; num_fields];
        let mut prefix_lengths = vec![Vec::new(); num_fields];
        let mut suffix_lengths = vec![Vec::new(); num_fields];
        {
            reader.reset(total_terms);

            // skip
            let mut to_skip = 0;
            for i in 0..skip {
                to_skip += num_terms.get_mut(i)? as usize;
            }
            reader.skip(to_skip, &mut self.vectors_stream)?;

            // read prefix lengths
            for (i, slot) in prefix_lengths.iter_mut().enumerate().take(num_fields) {
                let term_count = num_terms.get_mut(skip + i)? as usize;
                let mut field_prefix_lengths = vec![0; term_count];
                let mut j = 0;

                while j < term_count {
                    let next = reader.next_batch(term_count - j, &mut self.vectors_stream)?;
                    let src = &next.longs[next.offset..][..next.length];
                    for (k, &val) in src.iter().enumerate() {
                        field_prefix_lengths[j + k] = val as usize;
                    }
                    j += next.length;
                }

                *slot = field_prefix_lengths;
            }

            reader.skip(total_terms - reader.ord(), &mut self.vectors_stream)?;

            reader.reset(total_terms);

            for i in 0..skip {
                let term_count = num_terms.get_mut(i)? as usize;
                for _ in 0..term_count {
                    doc_off += reader.next_value(&mut self.vectors_stream)? as usize;
                }
            }

            for i in 0..num_fields {
                let term_count = num_terms.get_mut(skip + i)? as usize;
                let mut field_suffix_lengths = vec![0; term_count];
                let mut j = 0;
                while j < term_count {
                    let next = reader.next_batch(term_count - j, &mut self.vectors_stream)?;
                    for k in 0..next.length {
                        field_suffix_lengths[j] = next.longs[next.offset + k] as usize;
                        j += 1;
                    }
                }
                doc_len += sum(&field_suffix_lengths);
                field_lengths[i] = sum(&field_suffix_lengths);
                suffix_lengths[i] = field_suffix_lengths;
            }

            total_len = doc_off + doc_len;
            for i in (skip + num_fields)..total_fields {
                let term_count = num_terms.get_mut(i)? as usize;
                for _ in 0..term_count {
                    total_len += reader.next_value(&mut self.vectors_stream)? as usize;
                }
            }
        }

        // term freqs
        let term_freqs = {
            let mut term_freqs = vec![0; total_terms];
            reader.reset(total_terms);
            let mut i = 0;
            debug_assert!((total_terms - i) <= i32::MAX as usize);
            while i < total_terms {
                let next = reader.next_batch(total_terms - i, &mut self.vectors_stream)?;
                for k in 0..next.length {
                    term_freqs[i] = 1 + next.longs[next.offset + k] as usize;
                    i += 1;
                }
            }
            term_freqs
        };

        // total number of positions, offsets and payloads
        let mut total_positions = 0;
        let mut total_offsets = 0;
        let mut total_payloads = 0;
        let mut term_index = 0;
        for i in 0..total_fields {
            let f = flags.get_mut(i)? as i32;
            let term_count = num_terms.get_mut(i)? as usize;
            for _ in 0..term_count {
                let freq = term_freqs[term_index];
                term_index += 1;
                if (f & POSITIONS) != 0 {
                    total_positions += freq;
                }
                if (f & OFFSETS) != 0 {
                    total_offsets += freq;
                }
                if (f & PAYLOADS) != 0 {
                    total_payloads += freq;
                }
            }
            debug_assert!(i != total_fields - 1 || term_index == total_terms);
        }

        // position index
        let position_index = Self::position_index(skip, num_fields, &mut num_terms, &term_freqs)?;

        // positions
        let mut positions = if total_positions > 0 {
            self.read_positions(
                skip,
                num_fields,
                &mut flags,
                &mut num_terms,
                &term_freqs,
                POSITIONS,
                total_positions,
                &position_index,
            )?
        } else {
            vec![vec![]; num_fields]
        };
        let (start_offsets, lengths) = if total_offsets > 0 {
            // average number of chars per term
            let mut chars_per_term = vec![0f32; field_nums.len()];
            for v in chars_per_term.iter_mut() {
                *v = f32::from_bits(self.vectors_stream.read_int()? as u32);
            }

            let mut start_offsets = self.read_positions(
                skip,
                num_fields,
                &mut flags,
                &mut num_terms,
                &term_freqs,
                OFFSETS,
                total_offsets,
                &position_index,
            )?;

            let mut lengths = self.read_positions(
                skip,
                num_fields,
                &mut flags,
                &mut num_terms,
                &term_freqs,
                OFFSETS,
                total_offsets,
                &position_index,
            )?;

            for i in 0..num_fields {
                let f_start_offsets = &mut start_offsets[i];
                let f_positions = &positions[i];
                // patch offsets from positions
                if !f_start_offsets.is_empty() && !f_positions.is_empty() {
                    let field_chars_per_term = chars_per_term[field_num_offs[i]];
                    for j in 0..f_start_offsets.len() {
                        f_start_offsets[j] +=
                            (field_chars_per_term * f_positions[j] as f32) as usize;
                    }
                }

                if !f_start_offsets.is_empty() {
                    let f_prefix_lengths = &prefix_lengths[i];
                    let f_suffix_lengths = &suffix_lengths[i];
                    let f_lengths = &mut lengths[i];
                    let term_count = num_terms.get_mut(skip + i)? as usize;
                    for j in 0..term_count {
                        // delta-decode start offsets and  patch lengths using term lengths
                        let term_length = f_prefix_lengths[j] + f_suffix_lengths[j];
                        let pos_start = position_index[i][j];
                        let pos_end = position_index[i][j + 1];
                        f_lengths[pos_start] += term_length;
                        for k in (pos_start + 1)..pos_end {
                            f_start_offsets[k] += f_start_offsets[k - 1];
                            f_lengths[k] += term_length;
                        }
                    }
                }
            }

            (start_offsets, lengths)
        } else {
            (vec![vec![]; num_fields], vec![vec![]; num_fields])
        };

        if total_positions > 0 {
            // delta-decode positions
            for i in 0..num_fields {
                let f_positions = &mut positions[i];
                let f_position_index = &position_index[i];
                if !f_positions.is_empty() {
                    let term_count = num_terms.get_mut(skip + i)? as usize;
                    for j in 0..term_count {
                        // delta-decode start offsets
                        for k in (f_position_index[j] + 1)..f_position_index[j + 1] {
                            f_positions[k] += f_positions[k - 1];
                        }
                    }
                }
            }
        }
        let mut payload_index = vec![Vec::new(); num_fields];
        let mut total_payload_length = 0;
        let mut payload_off = 0;
        let mut payload_len = 0;

        if total_payloads > 0 {
            reader.reset(total_payloads);
            // skip
            let mut term_index = 0;
            for i in 0..skip {
                let f = flags.get_mut(i)? as i32;
                let term_count = num_terms.get_mut(i)? as usize;
                if (f & PAYLOADS) != 0 {
                    for j in 0..term_count {
                        let freq = term_freqs[term_index + j];
                        for _ in 0..freq {
                            let l = reader.next_value(&mut self.vectors_stream)? as usize;
                            payload_off += l;
                        }
                    }
                }
                term_index += term_count;
            }
            total_payload_length = payload_off;

            // read doc payload lengths
            for i in 0..num_fields {
                let f = flags.get_mut(skip + i)? as i32;
                let term_count = num_terms.get_mut(skip + i)? as usize;
                if (f & PAYLOADS) != 0 {
                    let total_freq = position_index[i][term_count];
                    let mut field_payload_index = vec![0; total_freq + 1];
                    let mut pos_idx = 0;
                    field_payload_index[pos_idx] = payload_len;
                    for j in 0..term_count {
                        let freq = term_freqs[term_index + j];
                        for _ in 0..freq {
                            let payload_length =
                                reader.next_value(&mut self.vectors_stream)? as usize;
                            payload_len += payload_length;
                            pos_idx += 1;
                            field_payload_index[pos_idx] = payload_len;
                        }
                    }
                    debug_assert_eq!(pos_idx, total_freq);
                    payload_index[i] = field_payload_index;
                }
                term_index += term_count;
            }
            total_payload_length += payload_len;
            for i in (skip + num_fields)..total_fields {
                let f = flags.get_mut(i)? as i32;
                let term_count = num_terms.get_mut(i)? as usize;
                if (f & PAYLOADS) != 0 {
                    for j in 0..term_count {
                        let freq = term_freqs[term_index + j];
                        for _ in 0..freq {
                            total_payload_length +=
                                reader.next_value(&mut self.vectors_stream)? as usize;
                        }
                    }
                }
                term_index += term_count;
            }

            debug_assert_eq!(term_index, total_terms);
        }
        // decompress data
        let mut suffix_bytes = BytesRef::new();
        self.decompressor.decompress(
            &mut self.vectors_stream,
            (total_len + total_payload_length) as i32,
            (doc_off + payload_off) as i32,
            (doc_len + payload_len) as i32,
            &mut suffix_bytes,
        )?;
        suffix_bytes.length = doc_len;
        let suffix_bytes = BytesRef::from_slice(
            Rc::new(suffix_bytes.bytes),
            suffix_bytes.offset,
            suffix_bytes.length,
        );
        let payload_bytes = BytesRef::from_slice(
            suffix_bytes.bytes.clone(),
            suffix_bytes.offset + doc_len,
            payload_len,
        );

        let mut field_flags = vec![0i32; num_fields];
        let mut field_num_terms = vec![0; num_fields];

        for (i, (flag_slot, term_slot)) in field_flags
            .iter_mut()
            .zip(field_num_terms.iter_mut())
            .enumerate()
        {
            *flag_slot = flags.get_mut(skip + i)? as i32;
            *term_slot = num_terms.get_mut(skip + i)? as usize;
        }

        let mut field_term_freqs = vec![Vec::new(); num_fields];
        {
            let mut term_idx = 0;
            for n in 0..skip {
                term_idx += num_terms.get_mut(n)? as usize;
            }

            for (i, slot) in field_term_freqs.iter_mut().enumerate().take(num_fields) {
                let term_count = num_terms.get_mut(skip + i)? as usize;
                let mut v = Vec::with_capacity(term_count);
                for _ in 0..term_count {
                    v.push(term_freqs[term_idx]);
                    term_idx += 1;
                }
                *slot = v;
            }
        }

        debug_assert_eq!(sum(&field_lengths), doc_len);

        Ok(Some(TVFields::new(
            field_nums,
            field_flags,
            field_num_offs,
            field_num_terms,
            field_lengths,
            prefix_lengths.into_iter().map(Rc::new).collect(),
            suffix_lengths.into_iter().map(Rc::new).collect(),
            field_term_freqs.into_iter().map(Rc::new).collect(),
            position_index.into_iter().map(Rc::new).collect(),
            positions.into_iter().map(Rc::new).collect(),
            start_offsets.into_iter().map(Rc::new).collect(),
            lengths.into_iter().map(Rc::new).collect(),
            payload_bytes,
            payload_index.into_iter().map(Rc::new).collect(),
            suffix_bytes,
            self.field_infos.clone(),
        )?))
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

impl<I> RawTermVectors for Lucene90CompressingTermVectorsReader<I>
where
    I: IndexInput,
{
    type IndexInput = I;

    fn raw_term_vectors_mut(&mut self) -> Result<&mut DefaultTermVectorsReader<Self::IndexInput>> {
        Ok(self)
    }

    fn raw_term_vectors(&self) -> Result<&DefaultTermVectorsReader<Self::IndexInput>> {
        Ok(self)
    }
}

impl<I> Clone for Lucene90CompressingTermVectorsReader<I>
where
    I: IndexInput,
{
    fn clone(&self) -> Self {
        Lucene90CompressingTermVectorsReader::with_reader(self).expect("should be ok")
    }
}

impl<I> TermVectorsReader for Lucene90CompressingTermVectorsReader<I>
where
    I: IndexInput,
{
    fn check_integrity(&self) -> Result<()> {
        self.index_reader.check_integrity()?;
        let _ = CodecUtil::checksum_entire_file(&self.vectors_stream)?;
        Ok(())
    }

    fn get_merge_instance(&self) -> Result<Option<Self>>
    where
        Self: Sized,
    {
        Ok(Some(Lucene90CompressingTermVectorsReader::with_reader(
            self,
        )?))
    }
}

struct BlockState {
    start_pointer: Option<usize>,
    doc_base: Option<usize>,
    chunk_docs: i32,
}
impl BlockState {
    pub(crate) fn new(
        start_pointer: Option<usize>,
        doc_base: Option<usize>,
        chunk_docs: i32,
    ) -> Self {
        BlockState {
            start_pointer,
            doc_base,
            chunk_docs,
        }
    }
}

pub struct TVFields {
    field_nums: Vec<i32>,
    field_flags: Vec<i32>,
    field_num_offs: Vec<usize>,
    num_terms: Vec<usize>,
    field_lengths: Vec<usize>,

    prefix_lengths: Vec<Rc<Vec<usize>>>,
    suffix_lengths: Vec<Rc<Vec<usize>>>,
    term_freqs: Vec<Rc<Vec<usize>>>,
    position_index: Vec<Rc<Vec<usize>>>,
    positions: Vec<Rc<Vec<usize>>>,
    start_offsets: Vec<Rc<Vec<usize>>>,
    lengths: Vec<Rc<Vec<usize>>>,

    payload_bytes: BytesRef<Rc<Vec<u8>>>,
    payload_index: Vec<Rc<Vec<usize>>>,
    suffix_bytes: BytesRef<Rc<Vec<u8>>>,

    names: Vec<String>,
    field_infos: Arc<FieldInfos>,
}
impl TVFields {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        field_nums: Vec<i32>,
        field_flags: Vec<i32>,
        field_num_offs: Vec<usize>,
        num_terms: Vec<usize>,
        field_lengths: Vec<usize>,
        prefix_lengths: Vec<Rc<Vec<usize>>>,
        suffix_lengths: Vec<Rc<Vec<usize>>>,
        term_freqs: Vec<Rc<Vec<usize>>>,
        position_index: Vec<Rc<Vec<usize>>>,
        positions: Vec<Rc<Vec<usize>>>,
        start_offsets: Vec<Rc<Vec<usize>>>,
        lengths: Vec<Rc<Vec<usize>>>,
        payload_bytes: BytesRef<Rc<Vec<u8>>>,
        payload_index: Vec<Rc<Vec<usize>>>,
        suffix_bytes: BytesRef<Rc<Vec<u8>>>,
        field_infos: Arc<FieldInfos>,
    ) -> Result<Self> {
        let mut names = Vec::new();
        for i in 0..field_num_offs.len() {
            let field_num = field_nums[field_num_offs[i]];
            let field_info = field_infos.field_info_by_number(field_num)?;
            match field_info {
                Some(fi) => {
                    names.push(fi.name.clone());
                },
                None => {
                    return Err(LuceneError::illegal_state(format!(
                        "Field number {field_num} not found in field infos"
                    )));
                },
            }
        }

        Ok(Self {
            field_nums,
            field_flags,
            field_num_offs,
            num_terms,
            field_lengths,

            prefix_lengths,
            suffix_lengths,
            term_freqs,
            position_index,
            positions,
            start_offsets,
            lengths,
            payload_bytes,
            payload_index,
            suffix_bytes,
            names,
            field_infos,
        })
    }
}
impl Fields for TVFields {
    type FieldIter<'a> = VecIter<'a>;

    fn iterator(&self) -> Result<Self::FieldIter<'_>> {
        Ok(self.names.iter_ext())
    }

    type Terms = TVTerms;

    fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
        let field_info = match self.field_infos.field_info_by_name(field) {
            Some(info) => info,
            None => return Ok(None),
        };

        let mut idx = None;
        for (i, &off) in self.field_num_offs.iter().enumerate() {
            if self.field_nums[off] == field_info.number {
                idx = Some(i);
                break;
            }
        }
        if idx.is_none() || self.num_terms[idx.unwrap()] == 0 {
            // no term
            return Ok(None);
        }

        let mut field_off = 0;
        let mut field_len = None;
        for (i, &len) in self.field_lengths.iter().enumerate() {
            if i < idx.unwrap() {
                field_off += len;
            } else {
                field_len = Some(len);
                break;
            }
        }
        debug_assert!(field_len.is_some());

        let term_bytes = BytesRef::from_slice(
            self.suffix_bytes.bytes.clone(),
            self.suffix_bytes.offset + field_off,
            field_len.unwrap(),
        );
        let idx = idx.unwrap();
        let tv_terms = TVTerms::new(
            self.num_terms[idx],
            self.field_flags[idx],
            self.prefix_lengths[idx].clone(),
            self.suffix_lengths[idx].clone(),
            self.term_freqs[idx].clone(),
            self.position_index[idx].clone(),
            self.positions[idx].clone(),
            self.start_offsets[idx].clone(),
            self.lengths[idx].clone(),
            self.payload_index[idx].clone(),
            self.payload_bytes.clone(),
            term_bytes,
        );
        Ok(Some(tv_terms))
    }

    fn size(&self) -> Result<i32> {
        debug_assert!(self.field_num_offs.len() <= i32::MAX as usize);
        Ok(self.field_num_offs.len() as i32)
    }
}

pub struct TVTerms {
    num_terms: usize,
    flags: i32,
    total_term_freq: i64,

    prefix_lengths: Rc<Vec<usize>>,
    suffix_lengths: Rc<Vec<usize>>,
    term_freqs: Rc<Vec<usize>>,
    position_index: Rc<Vec<usize>>,
    positions: Rc<Vec<usize>>,
    start_offsets: Rc<Vec<usize>>,
    lengths: Rc<Vec<usize>>,
    payload_index: Rc<Vec<usize>>,

    payload_bytes: BytesRef<Rc<Vec<u8>>>,
    term_bytes: BytesRef<Rc<Vec<u8>>>,
}
impl TVTerms {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        num_terms: usize,
        flags: i32,
        prefix_lengths: Rc<Vec<usize>>,
        suffix_lengths: Rc<Vec<usize>>,
        term_freqs: Rc<Vec<usize>>,
        position_index: Rc<Vec<usize>>,
        positions: Rc<Vec<usize>>,
        start_offsets: Rc<Vec<usize>>,
        lengths: Rc<Vec<usize>>,
        payload_index: Rc<Vec<usize>>,
        payload_bytes: BytesRef<Rc<Vec<u8>>>,
        term_bytes: BytesRef<Rc<Vec<u8>>>,
    ) -> Self {
        let total_term_freq = term_freqs.iter().map(|&x| x as i64).sum();

        TVTerms {
            num_terms,
            flags,
            prefix_lengths,
            suffix_lengths,
            term_freqs,
            position_index,
            positions,
            start_offsets,
            lengths,
            payload_index,
            payload_bytes,
            term_bytes,
            total_term_freq,
        }
    }
}
impl Terms for TVTerms {
    type TermsEnum = BaseTermsEnum<TVTermsEnum>;

    fn iterator(&self) -> Result<Self::TermsEnum> {
        let terms_enum = TVTermsEnum::new(
            self.num_terms,
            self.flags,
            self.prefix_lengths.clone(),
            self.suffix_lengths.clone(),
            self.term_freqs.clone(),
            self.position_index.clone(),
            self.positions.clone(),
            self.start_offsets.clone(),
            self.lengths.clone(),
            self.payload_index.clone(),
            self.payload_bytes.clone(),
            ByteArrayDataInput::with_range(
                self.term_bytes.bytes.clone(),
                self.term_bytes.offset,
                self.term_bytes.length,
            ),
        );
        Ok(terms_enum.into())
    }

    type IntersectIter
        = FilteredTermsEnum<Self::TermsEnum, AutomatonTermsEnum>
    where
        Self::TermsEnum: BytesRefIterator,
        AutomatonTermsEnum: FilteredTermsEnumBase;
    fn intersect(
        &self,
        compiled: &mut CompiledAutomaton,
        start_term: Option<&BytesRef<Vec<u8>>>,
    ) -> Result<Self::IntersectIter> {
        self.default_intersect(compiled, start_term)
    }

    fn size(&self) -> Result<i64> {
        Ok(self.num_terms as i64)
    }

    fn get_sum_total_term_freq(&self) -> Result<i64> {
        Ok(self.total_term_freq)
    }

    fn get_sum_doc_freq(&self) -> Result<i64> {
        Ok(self.num_terms as i64)
    }

    fn get_doc_count(&self) -> Result<i32> {
        Ok(1)
    }

    fn has_freqs(&self) -> bool {
        true
    }

    fn has_offsets(&self) -> bool {
        (self.flags & OFFSETS) != 0
    }

    fn has_positions(&self) -> bool {
        (self.flags & POSITIONS) != 0
    }

    fn has_payloads(&self) -> bool {
        (self.flags & PAYLOADS) != 0
    }
}

pub struct TVTermsEnum {
    num_terms: usize,
    start_pos: usize,
    ord: Option<usize>,

    prefix_lengths: Rc<Vec<usize>>,
    suffix_lengths: Rc<Vec<usize>>,
    term_freqs: Rc<Vec<usize>>,
    position_index: Rc<Vec<usize>>,
    positions: Rc<Vec<usize>>,
    start_offsets: Rc<Vec<usize>>,
    lengths: Rc<Vec<usize>>,
    payload_index: Rc<Vec<usize>>,

    input: ByteArrayDataInput<Rc<Vec<u8>>>,
    payloads: BytesRef<Rc<Vec<u8>>>,
    term: BytesRef<Vec<u8>>,
}
impl TVTermsEnum {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        num_terms: usize,
        _flags: i32,
        prefix_lengths: Rc<Vec<usize>>,
        suffix_lengths: Rc<Vec<usize>>,
        term_freqs: Rc<Vec<usize>>,
        position_index: Rc<Vec<usize>>,
        positions: Rc<Vec<usize>>,
        start_offsets: Rc<Vec<usize>>,
        lengths: Rc<Vec<usize>>,
        payload_index: Rc<Vec<usize>>,
        payloads: BytesRef<Rc<Vec<u8>>>,
        input: ByteArrayDataInput<Rc<Vec<u8>>>,
    ) -> Self {
        let start_pos = input.get_position();
        debug_assert!(start_pos <= i32::MAX as usize);

        let mut term_enum = TVTermsEnum {
            num_terms,
            prefix_lengths,
            suffix_lengths,
            term_freqs,
            position_index,
            positions,
            start_offsets,
            lengths,
            payload_index,
            payloads,
            input,
            start_pos,
            ord: None,
            term: BytesRef::with_capacity(16),
        };

        term_enum.reset();
        term_enum
    }
    pub fn reset(&mut self) {
        self.term.length = 0;
        self.input.set_position(self.start_pos);
        self.ord = None;
    }
}

impl BytesRefIterator for TVTermsEnum {
    fn next(&mut self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
        if self
            .ord
            .map_or(self.num_terms == 0, |v| v + 1 == self.num_terms)
        {
            return Ok(None);
        }
        let ord = self.ord.map_or(0, |v| v + 1);
        self.ord = Some(ord);

        debug_assert!(ord < self.num_terms + 1);

        let prefix_len = self.prefix_lengths[ord];
        let suffix_len = self.suffix_lengths[ord];
        let total_len = prefix_len + suffix_len;

        self.term.offset = 0;
        self.term.length = total_len;

        if self.term.bytes.len() < self.term.length {
            ArrayUtil::grow_with_len(&mut self.term.bytes, self.term.length);
        }

        self.input
            .read_bytes(&mut self.term.bytes, prefix_len, suffix_len)?;
        self.ord = Some(ord);
        Ok(Option::from(Cow::Borrowed(&self.term)))
    }
}

impl TermsEnum for TVTermsEnum {
    type AttributeSource = DummyAttributeSource;

    fn seek_ceil(&mut self, text: &BytesRef<Vec<u8>>) -> Result<SeekStatus> {
        if let Some(ord) = self.ord
            && ord < self.num_terms
        {
            let cmp = self.term.cmp(text).to_int();
            if cmp == 0 {
                return Ok(SeekStatus::Found);
            } else if cmp > 0 {
                self.reset();
            }
        }

        // linear scan
        loop {
            let term = self.next()?;
            match term {
                None => return Ok(SeekStatus::End),
                Some(t) => {
                    let cmp = (*t).cmp(text).to_int();
                    if cmp > 0 {
                        return Ok(SeekStatus::NotFound);
                    } else if cmp == 0 {
                        return Ok(SeekStatus::Found);
                    }
                },
            }
        }
    }

    fn seek_exact_with_ord(&mut self, _ord: i64) -> Result<()> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn term(&self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
        Ok(Cow::Borrowed(&self.term))
    }

    fn ord(&self) -> Result<i64> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn doc_freq(&mut self) -> Result<i32> {
        Ok(1)
    }

    fn total_term_freq(&mut self) -> Result<i64> {
        let ord = self
            .ord
            .ok_or_else(|| LuceneError::illegal_state("ord is None"))?;
        Ok(self.term_freqs[ord] as i64)
    }

    type PostingsEnum = TVPostingsEnum;

    fn postings_with_flags(
        &mut self,
        reuse: Option<Self::PostingsEnum>,
        _flags: i32,
    ) -> Result<Self::PostingsEnum> {
        let mut docs_enum = reuse.unwrap_or_else(TVPostingsEnum::new);
        let ord = self
            .ord
            .ok_or_else(|| LuceneError::illegal_state("ord is None"))?;
        docs_enum.reset(
            self.term_freqs[ord],
            self.position_index[ord],
            self.positions.clone(),
            self.start_offsets.clone(),
            self.lengths.clone(),
            self.payloads.clone(),
            self.payload_index.clone(),
        );
        Ok(docs_enum)
    }

    type ImpactsEnum = SlowImpactsEnum<TVPostingsEnum>;

    fn impacts(&mut self, _flags: i32) -> Result<Self::ImpactsEnum> {
        let delegate = self.postings_with_flags(None, FREQS as i32)?;
        Ok(SlowImpactsEnum::new(delegate))
    }

    type TermState = DummyTermState;
}

pub struct TVPostingsEnum {
    doc: i32,
    term_freq: usize,
    position_index: usize,

    positions: Rc<Vec<usize>>,
    start_offsets: Rc<Vec<usize>>,
    lengths: Rc<Vec<usize>>,
    payload: BytesRef<Rc<Vec<u8>>>,
    payload_index: Rc<Vec<usize>>,
    base_payload_offset: usize,
    i: Option<usize>,

    payload_length: usize,
    payload_offset: usize,
}
impl Default for TVPostingsEnum {
    fn default() -> Self {
        Self::new()
    }
}

impl TVPostingsEnum {
    pub fn new() -> Self {
        TVPostingsEnum {
            doc: -1,
            term_freq: 0,
            position_index: 0,
            positions: Rc::new(Vec::new()),
            start_offsets: Rc::new(Vec::new()),
            lengths: Rc::new(Vec::new()),
            payload: BytesRef {
                bytes: Rc::new(Vec::new()),
                offset: 0,
                length: 0,
            },
            payload_index: Rc::new(Vec::new()),
            base_payload_offset: 0,
            i: None,
            payload_length: 0,
            payload_offset: 0,
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub fn reset(
        &mut self,
        freq: usize,
        position_index: usize,
        positions: Rc<Vec<usize>>,
        start_offsets: Rc<Vec<usize>>,
        lengths: Rc<Vec<usize>>,
        payloads: BytesRef<Rc<Vec<u8>>>,
        payload_index: Rc<Vec<usize>>,
    ) {
        self.term_freq = freq;
        self.position_index = position_index;
        self.positions = positions;
        self.start_offsets = start_offsets;
        self.lengths = lengths;

        self.base_payload_offset = payloads.offset;

        self.payload.bytes = payloads.bytes.clone();
        self.payload.offset = 0;
        self.payload.length = 0;

        self.payload_index = payload_index;

        self.doc = -1;
        self.i = None;
    }

    fn check_doc(&self) -> Result<()> {
        if self.doc == NO_MORE_DOCS {
            Err(LuceneError::illegal_state("DocsEnum exhausted"))
        } else if self.doc == -1 {
            Err(LuceneError::illegal_state("DocsEnum not started"))
        } else {
            Ok(())
        }
    }
    fn check_position(&self) -> Result<()> {
        self.check_doc()?;
        if self.i.is_none() {
            Err(LuceneError::illegal_state("Position enum not started"))
        } else if self.i.unwrap() >= self.term_freq {
            Err(LuceneError::illegal_state("Read past last position"))
        } else {
            Ok(())
        }
    }
}

impl DocIdSetIterator for TVPostingsEnum {
    fn doc_id(&self) -> i32 {
        self.doc
    }

    fn next_doc(&mut self) -> Result<i32> {
        if self.doc == -1 {
            self.doc = 0;
        } else {
            self.doc = NO_MORE_DOCS;
        }
        Ok(self.doc)
    }

    fn advance(&mut self, target: i32) -> Result<i32> {
        self.slow_advance(target)
    }

    fn cost(&self) -> Result<i64> {
        Ok(1)
    }
}

impl PostingsEnum for TVPostingsEnum {
    fn freq(&mut self) -> Result<i32> {
        self.check_doc()?;
        Ok(self.doc)
    }

    fn next_position(&mut self) -> Result<i32> {
        if self.doc != 0 {
            return Err(LuceneError::illegal_state(""));
        }
        let i;
        let equal = match self.i {
            Some(idx) => {
                i = idx + 1;
                idx + 1 >= self.term_freq
            },
            None => {
                // -1 + 1
                i = 0;
                self.term_freq == 0
            },
        };
        self.i = Some(i);
        if equal {
            return Err(LuceneError::illegal_state("Read past last position"));
        }
        if !self.payload_index.is_empty() {
            let index = self.position_index + i;
            self.payload_offset = self.base_payload_offset + self.payload_index[index];
            self.payload_length = self.payload_index[index + 1] - self.payload_index[index];
        }
        if self.positions.is_empty() {
            Ok(-1)
        } else {
            Ok(self.positions[self.position_index + i] as i32)
        }
    }

    fn start_offset(&self) -> Result<i32> {
        self.check_position()?;
        if self.start_offsets.is_empty() {
            Ok(-1)
        } else {
            Ok(self.start_offsets[self.position_index + self.i.unwrap()] as i32)
        }
    }

    fn end_offset(&self) -> Result<i32> {
        self.check_position()?;
        if self.start_offsets.is_empty() {
            Ok(-1)
        } else {
            let index = self.position_index + self.i.unwrap();
            Ok((self.start_offsets[index] + self.lengths[index]) as i32)
        }
    }

    fn get_payload(&self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
        self.check_position()?;
        if self.payload_index.is_empty() || self.payload.length == 0 {
            Ok(None)
        } else {
            // TODO: always data copy here
            let v = self.payload.bytes
                [self.payload_offset..self.payload_offset + self.payload_length]
                .to_vec();
            let v = BytesRef::from_slice(v, self.payload_offset, self.payload_length);
            Ok(Some(Cow::Owned(v)))
        }
    }
}
const PREFETCH_CACHE_SIZE: usize = 1 << 4;
pub(crate) const PREFETCH_CACHE_MASK: usize = PREFETCH_CACHE_SIZE - 1;
pub(crate) fn sum(arr: &[usize]) -> usize {
    let mut sum = 0;
    for &el in arr {
        sum += el;
    }
    sum
}
