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
use crate::core::codecs::compressing::lucene90_compressing_stored_fields_writer::BULK_MERGE_ENABLED;
use crate::core::codecs::compressing::lucene90_compressing_term_vectors_reader::Lucene90CompressingTermVectorsReader;
use crate::core::codecs::compression::compression_mode::{
  CompressionModeBase, CompressionModeEnum, CompressorEnum,
};
use crate::core::codecs::compression::compressor::Compressor;
use crate::core::codecs::compression::matching_readers::MatchingReaders;
use crate::core::codecs::lucene90::fields_index::FieldsIndex;
use crate::core::codecs::lucene90::fields_index_writer::FieldsIndexWriter;
use crate::core::codecs::term_vectors_reader::TermVectorsReader;
use crate::core::codecs::term_vectors_writer::TermVectorsWriter;
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::merge_state::{DocMap, MergeState, MergeStateDocMap};
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::term_vectors::{RawTermVectors, TermVectors};
use crate::core::index::{BytesRef, DocIDMerger, IndexFileNames, Sub, SubBase, of};
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::store::directory::Directory;
use crate::core::store::{
  ByteBuffersDataOutput, DataInput, DataOutput, IOContext, IndexInput, IndexOutput,
};
use crate::core::util::accountable::Accountable;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::io_utils::IOUtils;
use crate::core::util::packed::Format::Packed;
use crate::core::util::packed::abstract_block_packed_writer::AbstractBlockPackedWriter;
use crate::core::util::packed::block_packed_writer::BlockPackedWriter;
use crate::core::util::packed::direct_writer::{DirectWriter, bits_required};
use crate::core::util::packed::{PackedImpl, PackedInts, Writer};
use crate::core::util::ram_usage_estimator::size_of_vec;
use crate::core::util::{SliceCopyOps, StringHelper, TryIntoInt};
use std::collections::{HashSet, VecDeque};
use std::rc::Rc;
use std::sync::LazyLock;

pub(crate) static FLAGS_BITS: LazyLock<i32> =
  LazyLock::new(|| bits_required((POSITIONS | OFFSETS | PAYLOADS) as i64).unwrap());
/// [`TermVectorsWriter`] for [`Lucene90CompressingTermVectorsFormat`](crate::core::codecs::lucene90::compressing::lucene90_compressing_term_vectors_format::Lucene90CompressingTermVectorsFormat).
pub struct Lucene90CompressingTermVectorsWriter<D>
where
  D: Directory,
{
  segment: String,
  index_writer: FieldsIndexWriter<D>,
  meta_stream: D::IndexOutput,
  vectors_stream: D::IndexOutput,
  closed: bool,
  compression_mode: CompressionModeEnum,
  compressor: CompressorEnum,
  chunk_size: i32,
  // number of chunks
  num_chunks: i64,
  // number of incomplete compressed blocks written
  num_dirty_chunks: i64,
  // cumulative number of docs in incomplete chunks
  num_dirty_docs: i64,

  // total number of docs seen
  num_docs: i32,
  // pending docs
  pending_docs: VecDeque<DocData>,
  // current document
  cur_doc: usize,
  // current field
  cur_field: usize,
  last_term: BytesRef<Vec<u8>>,

  positions_buf: Vec<i32>,
  start_offsets_buf: Vec<i32>,
  lengths_buf: Vec<i32>,
  payload_lengths_buf: Vec<i32>,
  // buffered term suffixes
  term_suffixes: ByteBuffersDataOutput,
  // buffered term payloads
  payload_bytes: ByteBuffersDataOutput,
  writer: AbstractBlockPackedWriter<BlockPackedWriter>,
  max_docs_per_chunk: i32,
  scratch_buffer: ByteBuffersDataOutput,
}
impl<D> Lucene90CompressingTermVectorsWriter<D>
where
  D: Directory,
{
  #[allow(clippy::too_many_arguments)]
  pub fn new<D2>(
    directory: D,
    si: &SegmentInfo<D2>,
    segment_suffix: &str,
    context: &IOContext,
    format_name: &str,
    compression_mode: CompressionModeEnum,
    chunk_size: i32,
    max_docs_per_chunk: i32,
    block_shift: i32,
  ) -> Result<Self>
  where
    D2: Directory,
  {
    debug_assert!(chunk_size > 0);
    debug_assert!(max_docs_per_chunk > 0);

    let segment = si.name.clone();
    let compressor = compression_mode.new_compressor();
    let term_suffixes = ByteBuffersDataOutput::new_resettable_instance();
    let payload_bytes = ByteBuffersDataOutput::new_resettable_instance();
    let scratch_buffer = ByteBuffersDataOutput::new_resettable_instance();
    let last_term = BytesRef::with_capacity(ArrayUtil::oversize(30, 1)?)?;

    let mut directory = Some(directory);
    let mut meta_stream = None;
    let mut vectors_stream = None;
    let mut index_writer = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<_> {
      let dir = directory
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("directory is missing"))?;
      meta_stream = Some(dir.create_output(
        &IndexFileNames::segment_file_name(&segment, segment_suffix, VECTORS_META_EXTENSION),
        context,
      )?);
      let meta = meta_stream
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("metadata output is missing"))?;
      CodecUtil::write_index_header(
        meta,
        &format!("{}Meta", VECTORS_INDEX_CODEC_NAME),
        VERSION_CURRENT,
        si.get_id(),
        segment_suffix,
      )?;
      debug_assert_eq!(
        CodecUtil::index_header_length(
          &format!("{}Meta", VECTORS_INDEX_CODEC_NAME),
          segment_suffix
        ),
        meta.get_file_pointer()?
      );

      vectors_stream = Some(dir.create_output(
        &IndexFileNames::segment_file_name(&segment, segment_suffix, VECTORS_EXTENSION),
        context,
      )?);
      let vectors = vectors_stream
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("term vectors output is missing"))?;
      CodecUtil::write_index_header(
        vectors,
        format_name,
        VERSION_CURRENT,
        si.get_id(),
        segment_suffix,
      )?;
      debug_assert_eq!(
        CodecUtil::index_header_length(format_name, segment_suffix),
        vectors.get_file_pointer()?
      );

      index_writer = Some(FieldsIndexWriter::new(
        directory
          .take()
          .ok_or_else(|| LuceneError::illegal_state("directory is missing"))?,
        &segment,
        segment_suffix,
        VECTORS_INDEX_EXTENSION,
        VECTORS_INDEX_CODEC_NAME,
        *si.get_id(),
        block_shift,
        context.clone(),
      )?);

      meta_stream
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("metadata output is missing"))?
        .write_vint(PackedInts::VERSION_CURRENT)?;
      meta_stream
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("metadata output is missing"))?
        .write_vint(chunk_size)?;

      Ok((
        AbstractBlockPackedWriter::new(PACKED_BLOCK_SIZE, BlockPackedWriter)?,
        vec![0; 1024],
        vec![0; 1024],
        vec![0; 1024],
        vec![0; 1024],
      ))
    }));
    let (writer, positions_buf, start_offsets_buf, lengths_buf, payload_lengths_buf) = match result
    {
      Ok(Ok(values)) => values,
      result => {
        IOUtils::close_while_handling_error(0..4, |operation| match operation {
          0 => meta_stream.as_mut().map_or(Ok(()), Closeable::close),
          1 => vectors_stream.as_mut().map_or(Ok(()), Closeable::close),
          2 | 3 => index_writer.as_mut().map_or(Ok(()), Closeable::close),
          _ => unreachable!(),
        })?;
        return match result {
          Ok(Err(error)) => Err(error),
          Err(payload) => std::panic::resume_unwind(payload),
          Ok(Ok(_)) => Err(LuceneError::illegal_state(
            "term vectors writer initialization entered failure handling after success",
          )),
        };
      },
    };
    let meta_stream =
      meta_stream.ok_or_else(|| LuceneError::illegal_state("metadata output is missing"))?;
    let vectors_stream =
      vectors_stream.ok_or_else(|| LuceneError::illegal_state("term vectors output is missing"))?;
    let index_writer =
      index_writer.ok_or_else(|| LuceneError::illegal_state("fields index writer is missing"))?;

    Ok(Self {
      segment,
      index_writer,
      meta_stream,
      vectors_stream,
      closed: false,
      compression_mode,
      compressor,
      chunk_size,
      num_chunks: 0,
      num_dirty_chunks: 0,
      num_dirty_docs: 0,
      num_docs: 0,
      pending_docs: VecDeque::new(),
      cur_doc: 0,
      cur_field: 0,
      last_term,
      positions_buf,
      start_offsets_buf,
      lengths_buf,
      payload_lengths_buf,
      term_suffixes,
      payload_bytes,
      writer,
      max_docs_per_chunk,
      scratch_buffer,
    })
  }
  fn add_doc_data(&mut self, num_vector_fields: i32) -> usize {
    let mut last: Option<&FieldData> = None;

    for doc in self.pending_docs.iter().rev() {
      if let Some(field) = doc.fields.back() {
        last = Some(field);
        break;
      }
    }

    let (pos_start, off_start, pay_start) = if let Some(last_field) = last {
      let total = last_field.total_positions;
      let pos_start = last_field.pos_start + if last_field.has_positions { total } else { 0 };
      let off_start = last_field.off_start + if last_field.has_offsets { total } else { 0 };
      let pay_start = last_field.pay_start + if last_field.has_payloads { total } else { 0 };
      (pos_start, off_start, pay_start)
    } else {
      (0, 0, 0)
    };

    let doc = DocData::new(num_vector_fields, pos_start, off_start, pay_start);

    let index = self.pending_docs.len();
    self.pending_docs.push_back(doc);
    index
  }
  fn trigger_flush(&self) -> bool {
    self.term_suffixes.size() >= self.chunk_size as usize
      || self.pending_docs.len() >= self.max_docs_per_chunk as usize
  }
  fn flush(&mut self, force: bool) -> Result<()> {
    debug_assert_ne!(force, self.trigger_flush());

    let chunk_docs = self.pending_docs.len();
    debug_assert!(chunk_docs > 0);
    debug_assert!(chunk_docs <= i32::MAX as usize);

    self.num_chunks += 1;
    if force {
      // incomplete: we had to force this flush
      self.num_dirty_chunks += 1;
      self.num_dirty_docs += chunk_docs as i64;
    }

    // write the index file
    self
      .index_writer
      .write_index(chunk_docs as i32, self.vectors_stream.get_file_pointer()?)?;

    let doc_base = self.num_docs - chunk_docs as i32;
    self.vectors_stream.write_vint(doc_base)?;

    let dirty_bit = if force { 1 } else { 0 };
    self
      .vectors_stream
      .write_vint(((chunk_docs as i32) << 1) | dirty_bit)?;
    // total number of fields of the chunk
    let total_fields = self.flush_num_fields(chunk_docs)?;

    if total_fields > 0 {
      // unique field numbers (sorted)
      let field_nums = self.flush_field_nums()?;
      // offsets in the array of unique field numbers
      self.flush_fields(total_fields, &field_nums)?;
      // flags (does the field have positions, offsets, payloads?)
      self.flush_flags(total_fields, &field_nums)?;
      // number of terms of each field
      self.flush_num_terms(total_fields)?;
      // prefix and suffix lengths for each field
      self.flush_term_lengths()?;
      // term freqs - 1 (because termFreq is always >=1) for each term
      self.flush_term_freqs()?;
      // positions for all terms, when enabled
      self.flush_positions()?;
      // offsets for all terms, when enabled
      self.flush_offsets(&field_nums)?;
      // payload lengths for all terms, when enabled
      self.flush_payload_lengths()?;

      // compress terms and payloads and write them to the output
      // using ByteBuffersDataInput reduce memory copy
      let mut input = self.term_suffixes.get_data_input_ref()?;
      self
        .compressor
        .compress(&mut input, &mut self.vectors_stream)?;
    }

    // reset state
    self.pending_docs.clear();
    self.cur_doc = 0;
    self.cur_field = 0;
    self.term_suffixes.reset();

    Ok(())
  }

  fn flush_num_fields(&mut self, chunk_docs: usize) -> Result<i32> {
    if chunk_docs == 1 {
      let num_fields = self.pending_docs.front().unwrap().num_fields;
      self.vectors_stream.write_vint(num_fields)?;
      Ok(num_fields)
    } else {
      self.writer.reset();
      let mut total_fields = 0;
      for doc in &self.pending_docs {
        self
          .writer
          .add(doc.num_fields as i64, &mut self.vectors_stream)?;
        total_fields += doc.num_fields;
      }
      self.writer.finish(&mut self.vectors_stream)?;
      Ok(total_fields)
    }
  }
  /// Returns a sorted array containing unique field numbers
  pub(crate) fn flush_field_nums(&mut self) -> Result<Vec<i32>> {
    // 1. Collect unique field numbers
    let mut field_nums_set = HashSet::new();
    for doc in &self.pending_docs {
      for field in &doc.fields {
        field_nums_set.insert(field.field_num);
      }
    }

    let mut field_nums: Vec<i32> = field_nums_set.into_iter().collect();
    field_nums.sort_unstable();

    let num_distinct_fields = field_nums.len();
    debug_assert!(num_distinct_fields > 0);

    let max_field_num = field_nums[num_distinct_fields - 1];
    let bits_required = PackedInts::bits_required(max_field_num as i64)?;

    let token = ((num_distinct_fields - 1).min(0x07) << 5) as u8 | (bits_required as u8);
    self.vectors_stream.write_byte(token)?;

    if num_distinct_fields > 0x07 {
      self
        .vectors_stream
        .write_vint((num_distinct_fields - 1 - 0x07) as i32)?;
    }
    debug_assert!(num_distinct_fields <= i32::MAX as usize);
    let mut writer = PackedInts::get_writer_no_header(
      &mut self.vectors_stream,
      Packed(PackedImpl::new(0)),
      num_distinct_fields as i32,
      bits_required,
      1,
    );

    for &field_num in &field_nums {
      writer.add(field_num as i64)?;
    }
    writer.finish()?;

    Ok(field_nums)
  }
  fn flush_fields(&mut self, total_fields: i32, field_nums: &[i32]) -> Result<()> {
    self.scratch_buffer.reset();

    let bits_required = bits_required((field_nums.len() - 1) as i64)?;
    let mut writer =
      DirectWriter::get_instance(&mut self.scratch_buffer, total_fields as i64, bits_required)?;

    for doc in &self.pending_docs {
      for field in &doc.fields {
        let field_num = field.field_num;
        let field_index = match field_nums.binary_search(&field_num) {
          Ok(index) => index,
          Err(_) => {
            return Err(LuceneError::illegal_state(format!(
              "Field number {field_num} not found in field_nums"
            )));
          },
        };
        writer.add(field_index as i64)?
      }
    }

    writer.finish()?;

    self
      .vectors_stream
      .write_vlong(self.scratch_buffer.size() as i64)?;
    self.scratch_buffer.copy_to(&mut self.vectors_stream)?;

    Ok(())
  }
  fn flush_flags(&mut self, total_fields: i32, field_nums: &[i32]) -> Result<()> {
    // check if fields always have the same flags
    let mut non_changing_flags = true;
    let mut field_flags = vec![-1; field_nums.len()];

    'outer: for doc in &self.pending_docs {
      for fd in &doc.fields {
        let field_num = fd.field_num;
        let field_index = match field_nums.binary_search(&field_num) {
          Ok(index) => index,
          Err(_) => {
            return Err(LuceneError::illegal_state(format!(
              "field_num {field_num} not found in field_nums"
            )));
          },
        };

        if field_flags[field_index] == -1 {
          field_flags[field_index] = fd.flags;
        } else if field_flags[field_index] != fd.flags {
          non_changing_flags = false;
          break 'outer;
        }
      }
    }

    self.scratch_buffer.reset();

    if non_changing_flags {
      // write one flag per field num
      self.vectors_stream.write_vint(0)?;
      let mut writer = DirectWriter::get_instance(
        &mut self.scratch_buffer,
        field_flags.len() as i64,
        *FLAGS_BITS,
      )?;

      for &flags in &field_flags {
        debug_assert!(flags >= 0);
        writer.add(flags as i64)?;
      }

      writer.finish()?;

      self
        .vectors_stream
        .write_vint(self.scratch_buffer.size().try_convert()?)?;
      self.scratch_buffer.copy_to(&mut self.vectors_stream)?;
    } else {
      // write one flag for every field instance
      self.vectors_stream.write_vint(1)?;
      let mut writer =
        DirectWriter::get_instance(&mut self.scratch_buffer, total_fields as i64, *FLAGS_BITS)?;

      for doc in &self.pending_docs {
        for field in &doc.fields {
          writer.add(field.flags as i64)?;
        }
      }

      writer.finish()?;

      self
        .vectors_stream
        .write_vint(self.scratch_buffer.size() as i32)?;
      self.scratch_buffer.copy_to(&mut self.vectors_stream)?;
    }

    Ok(())
  }
  fn flush_num_terms(&mut self, total_fields: i32) -> Result<()> {
    let mut max_num_terms = 0;
    for doc in &self.pending_docs {
      for field in &doc.fields {
        max_num_terms |= field.num_terms;
      }
    }

    let bits_required = bits_required(max_num_terms as i64)?;
    self.vectors_stream.write_vint(bits_required)?;

    self.scratch_buffer.reset();

    let mut writer =
      DirectWriter::get_instance(&mut self.scratch_buffer, total_fields as i64, bits_required)?;

    for doc in &self.pending_docs {
      for field in &doc.fields {
        writer.add(field.num_terms as i64)?;
      }
    }

    writer.finish()?;

    self
      .vectors_stream
      .write_vint(self.scratch_buffer.size().try_convert()?)?;
    self.scratch_buffer.copy_to(&mut self.vectors_stream)?;

    Ok(())
  }
  fn flush_term_lengths(&mut self) -> Result<()> {
    self.writer.reset();

    for doc in &self.pending_docs {
      for field in &doc.fields {
        for i in 0..field.num_terms {
          self
            .writer
            .add(field.prefix_lengths[i] as i64, &mut self.vectors_stream)?;
        }
      }
    }

    self.writer.finish(&mut self.vectors_stream)?;

    self.writer.reset();

    for doc in &self.pending_docs {
      for field in &doc.fields {
        for i in 0..field.num_terms {
          self
            .writer
            .add(field.suffix_lengths[i] as i64, &mut self.vectors_stream)?;
        }
      }
    }

    self.writer.finish(&mut self.vectors_stream)
  }
  fn flush_term_freqs(&mut self) -> Result<()> {
    self.writer.reset();

    for doc in &self.pending_docs {
      for field in &doc.fields {
        for i in 0..field.num_terms {
          self
            .writer
            .add((field.freqs[i] - 1) as i64, &mut self.vectors_stream)?;
        }
      }
    }

    self.writer.finish(&mut self.vectors_stream)
  }
  fn flush_positions(&mut self) -> Result<()> {
    self.writer.reset();

    for dd in &self.pending_docs {
      for fd in &dd.fields {
        if fd.has_positions {
          let mut pos = 0;
          for i in 0..fd.num_terms {
            let freq = fd.freqs[i];
            let mut previous_position = 0;
            for _ in 0..freq {
              let index = fd.pos_start + pos;
              let position = self.positions_buf[index];
              self.writer.add(
                (position - previous_position) as i64,
                &mut self.vectors_stream,
              )?;
              previous_position = position;
              pos += 1;
            }
          }
          debug_assert_eq!(pos, fd.total_positions);
        }
      }
    }

    self.writer.finish(&mut self.vectors_stream)
  }
  fn flush_offsets(&mut self, field_nums: &[i32]) -> Result<()> {
    let mut has_offsets = false;
    let mut sum_pos = vec![0u64; field_nums.len()];
    let mut sum_offsets = vec![0u64; field_nums.len()];

    for doc in &self.pending_docs {
      for field in &doc.fields {
        has_offsets |= field.has_offsets;
        if field.has_offsets && field.has_positions {
          let field_index = match field_nums.binary_search(&field.field_num) {
            Ok(idx) => idx,
            Err(_) => {
              return Err(LuceneError::illegal_state(format!(
                "field_num {} not found in field_nums",
                field.field_num
              )));
            },
          };

          let mut pos = 0;
          for i in 0..field.num_terms {
            let freq = field.freqs[i] as usize;
            let position_idx = field.pos_start + pos + freq - 1;
            let offset_idx = field.off_start + pos + freq - 1;
            sum_pos[field_index] += self.positions_buf[position_idx] as u64;
            sum_offsets[field_index] += self.start_offsets_buf[offset_idx] as u64;
            pos += freq;
          }
          debug_assert_eq!(pos, field.total_positions);
        }
      }
    }

    if !has_offsets {
      // nothing to do
      return Ok(());
    }

    let mut chars_per_term = vec![0f32; field_nums.len()];
    for i in 0..field_nums.len() {
      chars_per_term[i] = if sum_pos[i] == 0 || sum_offsets[i] == 0 {
        0.0
      } else {
        (sum_offsets[i] as f64 / sum_pos[i] as f64) as f32
      };
      // start offsets
      self
        .vectors_stream
        .write_int(chars_per_term[i].to_bits() as i32)?;
    }

    // start offsets
    self.writer.reset();

    for doc in &self.pending_docs {
      for field in &doc.fields {
        if field.flags & OFFSETS != 0 {
          let field_num_off = match field_nums.binary_search(&field.field_num) {
            Ok(idx) => idx,
            Err(_) => {
              return Err(LuceneError::illegal_state(format!(
                "field_num {} not found",
                field.field_num
              )));
            },
          };
          let cpt = chars_per_term[field_num_off];
          let mut pos = 0;
          for i in 0..field.num_terms {
            let freq = field.freqs[i];
            let mut previous_pos = 0;
            let mut previous_off = 0;
            for _ in 0..freq {
              let position = if field.has_positions {
                self.positions_buf[field.pos_start + pos]
              } else {
                0
              };
              let start_offset = self.start_offsets_buf[field.off_start + pos];
              let predicted = (cpt * (position - previous_pos) as f32) as i32;
              self.writer.add(
                (start_offset - previous_off - predicted) as i64,
                &mut self.vectors_stream,
              )?;
              previous_pos = position;
              previous_off = start_offset;
              pos += 1;
            }
          }
        }
      }
    }

    self.writer.finish(&mut self.vectors_stream)?;

    // lengths
    self.writer.reset();

    for doc in &self.pending_docs {
      for field in &doc.fields {
        if field.flags & OFFSETS != 0 {
          let mut pos = 0;
          for i in 0..field.num_terms {
            let prefix = field.prefix_lengths[i];
            let suffix = field.suffix_lengths[i];
            for _ in 0..field.freqs[i] {
              let length = self.lengths_buf[field.off_start + pos];
              self.writer.add(
                (length - prefix as i32 - suffix as i32) as i64,
                &mut self.vectors_stream,
              )?;
              pos += 1;
            }
          }
          debug_assert_eq!(pos, field.total_positions);
        }
      }
    }

    self.writer.finish(&mut self.vectors_stream)
  }
  fn flush_payload_lengths(&mut self) -> Result<()> {
    self.writer.reset();

    for doc in &self.pending_docs {
      for field in &doc.fields {
        if field.has_payloads {
          for i in 0..field.total_positions {
            let value = self.payload_lengths_buf[field.pay_start + i];
            self.writer.add(value as i64, &mut self.vectors_stream)?;
          }
        }
      }
    }

    self.writer.finish(&mut self.vectors_stream)
  }
  fn copy_chunks<MD, CR>(
    &mut self,
    merge_state: &mut MergeState<MD, CR>,
    sub: &CompressingTermVectorsSub<MergeStateDocMap<CR>>,
    from_doc_id: i32,
    to_doc_id: i32,
  ) -> Result<()>
  where
    MD: Directory,
    CR: CodecReader,
  {
    let merge_state_meta = merge_state.get_meta();
    let reader_wrap = merge_state.term_vectors_readers[sub.reader_index]
      .as_mut()
      .ok_or_else(|| LuceneError::illegal_state("TermVectorsReader is None"))?;
    let reader = reader_wrap.raw_term_vectors_mut()?;

    debug_assert_eq!(reader.get_version(), VERSION_CURRENT);
    debug_assert_eq!(reader.get_chunk_size(), self.chunk_size);
    debug_assert_eq!(*reader.get_compression_mode(), self.compression_mode);
    debug_assert!(!self.too_dirty(reader)?);
    debug_assert!(merge_state.live_docs[sub.reader_index].is_none());

    let mut doc_id = from_doc_id;

    // copy docs that belong to the previous chunk
    while doc_id < to_doc_id && reader.is_loaded(doc_id) {
      let fields = reader.get(doc_id)?;
      self.add_all_doc_vectors(fields.as_ref(), &merge_state_meta)?;
      doc_id += 1;
    }

    if doc_id >= to_doc_id {
      return Ok(());
    }

    // copy chunks
    let mut from_pointer = reader.index_reader.get_start_pointer(doc_id)?;
    let to_pointer = if to_doc_id == sub.max_doc {
      reader.get_max_pointer()
    } else {
      reader.index_reader.get_start_pointer(to_doc_id)?
    };

    if from_pointer < to_pointer {
      // flush any pending chunks
      if !self.pending_docs.is_empty() {
        self.flush(true)?;
      }

      reader.vectors_stream.seek(from_pointer)?;

      loop {
        // iterate over each chunk. we use the vectors index to find chunk boundaries,
        // read the docstart + doccount from the chunk header (we write a new header, since doc
        // numbers will change),
        // and just copy the bytes directly.
        // read header
        let base = reader.vectors_stream.read_vint()?;
        if base != doc_id {
          return Err(LuceneError::corrupt_index(format!(
            "invalid state: base={}, docID={}",
            base, doc_id
          )));
        }

        let code = reader.vectors_stream.read_vint()?;
        let buffered_docs = (code as u32 >> 1) as i32;

        // write a new index entry and new header for this chunk.
        self
          .index_writer
          .write_index(buffered_docs, self.vectors_stream.get_file_pointer()?)?;
        self.vectors_stream.write_vint(self.num_docs)?;
        self.vectors_stream.write_vint(code)?;

        doc_id += buffered_docs;
        self.num_docs += buffered_docs;

        if doc_id > to_doc_id {
          return Err(LuceneError::corrupt_index(format!(
            "invalid state: base={}, count={}, toDocID={}",
            base, buffered_docs, to_doc_id
          )));
        }

        // copy bytes until the next chunk boundary (or end of chunk data).
        // using the stored fields index for this isn't the most efficient, but fast enough
        // and is a source of redundancy for detecting bad things.
        let end = if doc_id == sub.max_doc {
          reader.get_max_pointer()
        } else {
          reader.index_reader.get_start_pointer(doc_id)?
        };

        let len = end - reader.vectors_stream.get_file_pointer()?;
        self
          .vectors_stream
          .copy_bytes(&mut reader.vectors_stream, len)?;
        self.num_chunks += 1;
        let dirty_chunk = (code & 1) != 0;
        if dirty_chunk {
          self.num_dirty_chunks += 1;
          self.num_dirty_docs += buffered_docs as i64;
        }
        from_pointer = end;
        if from_pointer >= to_pointer {
          break;
        }
      }
    }

    // copy leftover docs that don't form a complete chunk
    debug_assert!(!reader.is_loaded(doc_id));
    while doc_id < to_doc_id {
      let fields = reader.get(doc_id)?;
      self.add_all_doc_vectors(fields.as_ref(), &merge_state_meta)?;
      doc_id += 1;
    }

    Ok(())
  }
  /// Returns `true` if this reader should be recompressed, even though bulk
  /// merging of compressed data would otherwise be possible.
  ///
  /// The last chunk written for a segment is typically incomplete, so without
  /// recompressing, in some worst-case situations (for example frequent reopen
  /// operations with very small flushes), the compression ratio can degrade
  /// over time. This method acts as a safety switch.
  ///
  /// A segment is considered *dirty* only if:
  /// - it has enough dirty documents to fill at least one full chunk, **and**
  /// - more than 1% of its chunks are dirty.
  fn too_dirty<I>(&self, candidate: &Lucene90CompressingTermVectorsReader<I>) -> Result<bool>
  where
    I: IndexInput,
  {
    Ok(
      candidate.get_num_dirty_docs()? > self.max_docs_per_chunk as i64
        && candidate.get_num_dirty_chunks()? * 100 > candidate.get_num_chunks()?,
    )
  }
  fn can_perform_bulk_merge<MD, CR>(
    &self,
    merge_state: &MergeState<MD, CR>,
    matching_readers: &MatchingReaders,
    reader_index: usize,
  ) -> Result<bool>
  where
    MD: Directory,
    CR: CodecReader,
  {
    match merge_state.term_vectors_readers[reader_index] {
      Some(ref reader) => {
        let reader = match reader.raw_term_vectors() {
          Ok(reader) => reader,
          Err(LuceneError::UnsupportedOperation(_)) => return Ok(false),
          Err(err) => return Err(err),
        };
        let v = *BULK_MERGE_ENABLED
          && matching_readers.matching_readers[reader_index]
          && *reader.get_compression_mode() == self.compression_mode
          && reader.get_chunk_size() == self.chunk_size
          && reader.get_version() == VERSION_CURRENT
          && reader.get_packed_ints_version() == PackedInts::VERSION_CURRENT
          && merge_state.live_docs[reader_index].is_none()
          && !self.too_dirty(reader)?;
        Ok(v)
      },
      None => Ok(false),
    }
  }
}

impl<D> Accountable for Lucene90CompressingTermVectorsWriter<D>
where
  D: Directory,
{
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(
      size_of_vec(&self.positions_buf)
        .saturating_add(size_of_vec(&self.start_offsets_buf))
        .saturating_add(size_of_vec(&self.lengths_buf))
        .saturating_add(size_of_vec(&self.payload_lengths_buf))
        .saturating_add(self.term_suffixes.ram_bytes_used()?)
        .saturating_add(self.payload_bytes.ram_bytes_used()?)
        .saturating_add(size_of_vec(&self.last_term.bytes))
        .saturating_add(self.scratch_buffer.ram_bytes_used()?),
    )
  }
}

impl<D> Closeable for Lucene90CompressingTermVectorsWriter<D>
where
  D: Directory,
{
  fn close(&mut self) -> Result<()> {
    if self.closed {
      return Ok(());
    }

    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      IOUtils::close(0..3, |operation| match operation {
        0 => self.meta_stream.close(),
        1 => self.vectors_stream.close(),
        2 => self.index_writer.close(),
        _ => unreachable!(),
      })
    }));
    self.closed = true;
    match close_result {
      Ok(result) => result,
      Err(payload) => std::panic::resume_unwind(payload),
    }
  }
}

impl<D> TermVectorsWriter for Lucene90CompressingTermVectorsWriter<D>
where
  D: Directory,
{
  fn start_document(&mut self, num_vector_fields: i32) -> Result<()> {
    self.cur_doc = self.add_doc_data(num_vector_fields);
    Ok(())
  }

  fn finish_document(&mut self) -> Result<()> {
    // append the payload bytes of the doc after its terms
    self.payload_bytes.copy_to(&mut self.term_suffixes)?;
    self.payload_bytes.reset();
    self.num_docs += 1;

    if self.trigger_flush() {
      self.flush(false)?;
    }

    self.cur_doc = 0;

    Ok(())
  }

  fn start_field(
    &mut self,
    field_info: &FieldInfo,
    num_terms: usize,
    positions: bool,
    offsets: bool,
    payloads: bool,
  ) -> Result<()> {
    self.cur_field = self.pending_docs[self.cur_doc].add_field(
      field_info.number,
      num_terms,
      positions,
      offsets,
      payloads,
    );
    self.last_term.length = 0;
    Ok(())
  }

  fn finish_field(&mut self) -> Result<()> {
    self.cur_field = 0;
    Ok(())
  }

  fn start_term(&mut self, term: &BytesRef<Vec<u8>>, freq: i32) -> Result<()> {
    debug_assert!(freq >= 1);

    let prefix: usize = if self.last_term.length == 0 {
      0
    } else {
      StringHelper::bytes_difference(&self.last_term, term)?
    };

    let suffix_len = term.length - prefix;
    debug_assert!(suffix_len <= i32::MAX as usize);

    let cur_field = match self.pending_docs[self.cur_doc]
      .fields
      .get_mut(self.cur_field)
    {
      Some(cur_field) => cur_field,
      None => {
        return Err(LuceneError::illegal_state(format!(
          "No field found at index {} for current doc {}",
          self.cur_field, self.cur_doc
        )));
      },
    };
    cur_field.add_term(freq, prefix, suffix_len);

    debug_assert!((term.offset + prefix) <= i32::MAX as usize);
    self
      .term_suffixes
      .write_bytes_range(&term.bytes, term.offset + prefix, suffix_len)?;
    // copy last term
    if self.last_term.bytes.len() < term.length {
      ArrayUtil::grow_no_copy(&mut self.last_term.bytes, term.length)?;
    }

    self.last_term.offset = 0;
    self.last_term.length = term.length;
    self
      .last_term
      .bytes
      .copy_from(&term.bytes[term.offset..term.offset + term.length], 0);

    Ok(())
  }

  fn add_position(
    &mut self,
    position: i32,
    start_offset: i32,
    end_offset: i32,
    payload: Option<&BytesRef<Vec<u8>>>,
  ) -> Result<()> {
    let cur_field = match self.pending_docs[self.cur_doc]
      .fields
      .get_mut(self.cur_field)
    {
      Some(cur_field) => cur_field,
      None => {
        return Err(LuceneError::illegal_state(format!(
          "No field found at index {} for current doc {}",
          self.cur_field, self.cur_doc
        )));
      },
    };
    debug_assert!(cur_field.flags != 0);

    let length = end_offset - start_offset;
    let payload_len = payload.map_or(0, |p| p.length) as i32;

    cur_field.add_position(
      position,
      start_offset,
      length,
      payload_len,
      &mut self.positions_buf,
      &mut self.start_offsets_buf,
      &mut self.lengths_buf,
      &mut self.payload_lengths_buf,
    )?;

    if cur_field.has_payloads
      && let Some(p) = payload
    {
      self
        .payload_bytes
        .write_bytes_range(&p.bytes, p.offset, p.length)?;
    }
    Ok(())
  }

  fn finish<D1>(&mut self, num_docs: i32, _dir: &D1) -> Result<()>
  where
    D1: Directory,
  {
    if !self.pending_docs.is_empty() {
      self.flush(true)?;
    }

    if num_docs != self.num_docs {
      return Err(LuceneError::illegal_state(format!(
        "Wrote {} docs, finish called with numDocs={}",
        self.num_docs, num_docs
      )));
    }

    self.index_writer.finish(
      num_docs,
      self.vectors_stream.get_file_pointer()?,
      &mut self.meta_stream,
    )?;

    self.meta_stream.write_vlong(self.num_chunks)?;
    self.meta_stream.write_vlong(self.num_dirty_chunks)?;
    self.meta_stream.write_vlong(self.num_dirty_docs)?;

    CodecUtil::write_footer(&mut self.meta_stream)?;
    CodecUtil::write_footer(&mut self.vectors_stream)?;

    Ok(())
  }

  fn finish_add_prox(&mut self, num_prox: usize) -> Result<()> {
    self.pending_docs[self.cur_doc]
      .fields
      .get_mut(self.cur_field)
      .unwrap()
      .total_positions += num_prox;
    Ok(())
  }

  fn add_positions(&mut self, num_prox: usize, positions: &mut impl DataInput) -> Result<()> {
    let cur_field = match self.pending_docs[self.cur_doc]
      .fields
      .get_mut(self.cur_field)
    {
      Some(cur_field) => cur_field,
      None => {
        return Err(LuceneError::illegal_state(format!(
          "No field found at index {} for current doc {}",
          self.cur_field, self.cur_doc
        )));
      },
    };

    if cur_field.has_positions {
      let pos_start = cur_field.pos_start + cur_field.total_positions;
      let len = pos_start + num_prox;
      if len > self.positions_buf.len() {
        ArrayUtil::grow_i32(&mut self.positions_buf, len)?;
      }

      let mut position = 0;
      if cur_field.has_payloads {
        let pay_start = cur_field.pay_start + cur_field.total_positions;
        let len = pay_start + num_prox;
        if len > self.payload_lengths_buf.len() {
          ArrayUtil::grow_i32(&mut self.payload_lengths_buf, len)?;
        }

        for i in 0..num_prox {
          let code = positions.read_vint()?;
          if (code & 1) != 0 {
            // This position has a payload
            let payload_len = positions.read_vint()?;
            self.payload_lengths_buf[pay_start + i] = payload_len;
            self
              .payload_bytes
              .copy_bytes(positions, payload_len as usize)?;
          } else {
            self.payload_lengths_buf[pay_start + i] = 0;
          }
          let code = ((code as u32) >> 1) as i32;
          position += code;
          self.positions_buf[pos_start + i] = position;
        }
      } else {
        for i in 0..num_prox {
          let code = positions.read_vint()?;
          position += (code as u32 >> 1) as i32;
          self.positions_buf[pos_start + i] = position;
        }
      }
    }
    Ok(())
  }

  fn add_offsets(&mut self, num_prox: usize, offsets: &mut impl DataInput) -> Result<()> {
    let cur_field = match self.pending_docs[self.cur_doc]
      .fields
      .get_mut(self.cur_field)
    {
      Some(cur_field) => cur_field,
      None => {
        return Err(LuceneError::illegal_state(format!(
          "No field found at index {} for current doc {}",
          self.cur_field, self.cur_doc
        )));
      },
    };
    if cur_field.has_offsets {
      let off_start = cur_field.off_start + cur_field.total_positions;
      let len = off_start + num_prox;
      if self.start_offsets_buf.len() < len {
        let new_length = ArrayUtil::oversize(len, 4)?;
        ArrayUtil::grow_exact(&mut self.start_offsets_buf, new_length)?;
        ArrayUtil::grow_exact(&mut self.lengths_buf, new_length)?;
      }

      let mut last_offset = 0;
      for i in 0..num_prox {
        let start_offset = last_offset + offsets.read_vint()?;
        let end_offset = start_offset + offsets.read_vint()?;
        last_offset = end_offset;
        debug_assert!(start_offset >= 0);
        debug_assert!((end_offset - start_offset) >= 0);
        self.start_offsets_buf[off_start + i] = start_offset;
        self.lengths_buf[off_start + i] = end_offset - start_offset;
      }
    }
    Ok(())
  }

  fn merge<MD, D1, CR>(&mut self, merge_state: &mut MergeState<MD, CR>, dir: &D1) -> Result<i32>
  where
    MD: Directory,
    D1: Directory,
    CR: CodecReader,
  {
    let num_readers = merge_state.term_vectors_readers.len();
    let matching_readers = MatchingReaders::new(merge_state)?;
    let mut subs = Vec::with_capacity(num_readers);
    for reader_index in 0..num_readers {
      if let Some(reader) = &merge_state.term_vectors_readers[reader_index] {
        reader.check_integrity()?;
      }

      let bulk_merge = self.can_perform_bulk_merge(merge_state, &matching_readers, reader_index)?;
      subs.push(Sub::new(CompressingTermVectorsSub::new(
        merge_state.doc_maps[reader_index].clone(),
        merge_state.max_docs[reader_index],
        bulk_merge,
        reader_index,
      )));
    }

    let mut doc_count = 0;

    let mut doc_id_merger = of(subs, merge_state.needs_index_sort)?;

    let mut sub_opt = doc_id_merger.next()?;
    while let Some(sub_idx) = sub_opt {
      let sub = &doc_id_merger.get_subs()[sub_idx];

      debug_assert!(
        sub.mapped_doc_id == doc_count,
        "{} != {}",
        sub.mapped_doc_id,
        doc_count
      );

      if sub.sub.can_perform_bulk_merge {
        let from_doc_id = sub.sub.doc_id;
        let mut to_doc_id = from_doc_id;
        let current_idx = sub_idx;
        // consume all contiguous docs from the same sub
        loop {
          let next = doc_id_merger.next()?;
          match next {
            Some(idx) if idx == current_idx => {
              to_doc_id += 1;
              debug_assert!(doc_id_merger.get_subs()[idx].sub.doc_id == to_doc_id);
            },
            other => {
              sub_opt = other;
              break;
            },
          }
        }

        to_doc_id += 1; // exclusive bound
        self.copy_chunks(
          merge_state,
          &doc_id_merger.get_subs()[current_idx].sub,
          from_doc_id,
          to_doc_id,
        )?;

        doc_count += to_doc_id - from_doc_id;
      } else {
        let vectors = match merge_state.term_vectors_readers[sub.sub.reader_index] {
          Some(ref mut reader) => reader.get(sub.sub.doc_id)?,
          None => None,
        };

        self.add_all_doc_vectors(vectors.as_ref(), &merge_state.get_meta())?;
        doc_count += 1;
        sub_opt = doc_id_merger.next()?;
      }
    }
    self.finish(doc_count, dir)?;
    Ok(doc_count)
  }
}
pub struct CompressingTermVectorsSub<DM>
where
  DM: DocMap,
{
  max_doc: i32,
  reader_index: usize,
  can_perform_bulk_merge: bool,
  doc_id: i32,
  doc_map: Rc<DM>,
}

impl<DM> CompressingTermVectorsSub<DM>
where
  DM: DocMap,
{
  pub fn new(
    doc_map: Rc<DM>,
    max_doc: i32,
    can_perform_bulk_merge: bool,
    reader_index: usize,
  ) -> Self {
    Self {
      max_doc,
      reader_index,
      can_perform_bulk_merge,
      doc_id: -1,
      doc_map,
    }
  }
}

impl<DM> SubBase for CompressingTermVectorsSub<DM>
where
  DM: DocMap,
{
  fn next_doc(&mut self) -> Result<i32> {
    self.doc_id += 1;
    if self.doc_id == self.max_doc {
      Ok(NO_MORE_DOCS)
    } else {
      Ok(self.doc_id)
    }
  }

  type DocMap = DM;

  fn get_doc_map(&self) -> Result<&Self::DocMap> {
    Ok(&self.doc_map)
  }
}

/// a pending doc
pub(crate) struct DocData {
  pub num_fields: i32,
  pub fields: VecDeque<FieldData>,
  pub pos_start: usize,
  pub off_start: usize,
  pub pay_start: usize,
}
impl DocData {
  pub(crate) fn new(num_fields: i32, pos_start: usize, off_start: usize, pay_start: usize) -> Self {
    Self {
      num_fields,
      fields: VecDeque::with_capacity(num_fields as usize),
      pos_start,
      off_start,
      pay_start,
    }
  }
  fn add_field(
    &mut self,
    field_num: i32,
    num_terms: usize,
    positions: bool,
    offsets: bool,
    payloads: bool,
  ) -> usize {
    let (pos_start, off_start, pay_start) = if let Some(last) = self.fields.back() {
      let total = last.total_positions;
      let pos_start = last.pos_start + if last.has_positions { total } else { 0 };
      let off_start = last.off_start + if last.has_offsets { total } else { 0 };
      let pay_start = last.pay_start + if last.has_payloads { total } else { 0 };
      (pos_start, off_start, pay_start)
    } else {
      (self.pos_start, self.off_start, self.pay_start)
    };

    let field = FieldData::new(
      field_num, num_terms, positions, offsets, payloads, pos_start, off_start, pay_start,
    );
    let index = self.fields.len();
    self.fields.push_back(field);
    index
  }
}
/// a pending field
pub(crate) struct FieldData {
  pub has_positions: bool,
  pub has_offsets: bool,
  pub has_payloads: bool,
  pub field_num: i32,
  pub flags: i32,
  pub num_terms: usize,
  pub freqs: Vec<i32>,
  pub prefix_lengths: Vec<usize>,
  pub suffix_lengths: Vec<usize>,
  pub pos_start: usize,
  pub off_start: usize,
  pub pay_start: usize,
  pub total_positions: usize,
  pub ord: usize,
}
impl FieldData {
  #[allow(clippy::too_many_arguments)]
  pub(crate) fn new(
    field_num: i32,
    num_terms: usize,
    positions: bool,
    offsets: bool,
    payloads: bool,
    pos_start: usize,
    off_start: usize,
    pay_start: usize,
  ) -> Self {
    let flags = (if positions { POSITIONS } else { 0 })
      | (if offsets { OFFSETS } else { 0 })
      | (if payloads { PAYLOADS } else { 0 });

    Self {
      has_positions: positions,
      has_offsets: offsets,
      has_payloads: payloads,
      field_num,
      flags,
      num_terms,
      freqs: vec![0; num_terms],
      prefix_lengths: vec![0; num_terms],
      suffix_lengths: vec![0; num_terms],
      pos_start,
      off_start,
      pay_start,
      total_positions: 0,
      ord: 0,
    }
  }
  pub(crate) fn add_term(&mut self, freq: i32, prefix_length: usize, suffix_length: usize) {
    self.freqs[self.ord] = freq;
    self.prefix_lengths[self.ord] = prefix_length;
    self.suffix_lengths[self.ord] = suffix_length;
    self.ord += 1;
  }
  #[allow(clippy::too_many_arguments)]
  pub(crate) fn add_position(
    &mut self,
    position: i32,
    start_offset: i32,
    length: i32,
    payload_length: i32,
    positions_buf: &mut Vec<i32>,
    start_offsets_buf: &mut Vec<i32>,
    lengths_buf: &mut Vec<i32>,
    payload_lengths_buf: &mut Vec<i32>,
  ) -> Result<()> {
    if self.has_positions {
      let required = self.pos_start + self.total_positions;
      if required == positions_buf.len() {
        ArrayUtil::grow(positions_buf)?;
      }
      positions_buf[required] = position;
    }

    if self.has_offsets {
      let required = self.off_start + self.total_positions;
      if required == start_offsets_buf.len() {
        let new_len = ArrayUtil::oversize(required, 4)?;
        ArrayUtil::grow_exact(start_offsets_buf, new_len)?;
        ArrayUtil::grow_exact(lengths_buf, new_len)?;
      }
      start_offsets_buf[required] = start_offset;
      lengths_buf[required] = length;
    }

    if self.has_payloads {
      let required = self.pay_start + self.total_positions;
      if required == payload_lengths_buf.len() {
        ArrayUtil::grow(payload_lengths_buf)?;
      }
      payload_lengths_buf[required] = payload_length;
    }
    self.total_positions += 1;
    Ok(())
  }
}

pub(crate) const VECTORS_EXTENSION: &str = "tvd";
pub(crate) const VECTORS_INDEX_EXTENSION: &str = "tvx";
pub(crate) const VECTORS_META_EXTENSION: &str = "tvm";
pub(crate) const VECTORS_INDEX_CODEC_NAME: &str = "Lucene90TermVectorsIndex";

pub(crate) const VERSION_START: i32 = 0;
pub(crate) const VERSION_CURRENT: i32 = VERSION_START;
pub(crate) const META_VERSION_START: i32 = 0;

pub(crate) const PACKED_BLOCK_SIZE: usize = 64;

pub(crate) const POSITIONS: i32 = 0x01;
pub(crate) const OFFSETS: i32 = 0x02;
pub(crate) const PAYLOADS: i32 = 0x04;
