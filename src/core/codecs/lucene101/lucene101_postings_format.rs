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
use std::fmt::{Display, Formatter};

use crate::core::codecs::block_term_state::BlockTermState;
use crate::core::codecs::block_tree::lucene90_block_tree_terms_reader::Lucene90BlockTreeTermsReader;
use crate::core::codecs::block_tree::lucene90_block_tree_terms_writer::{
    DEFAULT_MAX_BLOCK_SIZE, DEFAULT_MIN_BLOCK_SIZE, Lucene90BlockTreeTermsWriter,
};
use crate::core::codecs::fields_consumer::FieldsConsumerEnum;
use crate::core::codecs::fields_producer::FieldsProducerEnum;
use crate::core::codecs::lucene101::for_util::ForUtil;
use crate::core::codecs::lucene101::lucene101_postings_reader::Lucene101PostingsReader;
use crate::core::codecs::lucene101::lucene101_postings_writer::Lucene101PostingsWriter;
use crate::core::codecs::postings_format::PostingsFormat;
use crate::core::codecs::push_postings_writer_base::PushPostingsWriterBase;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::term_state::TermState;
use crate::core::store::directory::Directory;
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::lucene_error::Result;
/// Lucene 10.1 postings format, which encodes postings in packed integer blocks for fast decode.
///
/// Basic idea:
///
/// - **Packed Blocks and VInt Blocks**:
///   In packed blocks, integers are encoded with the same bit width ([`PackedInts` packed format]): the block
///   size (i.e. number of integers inside block) is fixed (currently 128). Additionally blocks that are all
///   the same value are encoded in an optimized way.
///   In VInt blocks, integers are encoded as [`DataOutput::writeVInt` VInt]: the block size is variable.
///
/// - **Block structure**:
///   When the postings are long enough, `Lucene101PostingsFormat` will try to encode most integer data as a
///   packed block.
///   Take a term with 259 documents as an example, the first 256 document ids are encoded as two packed
///   blocks, while the remaining 3 are encoded as one VInt block.
///   Different kinds of data are always encoded separately into different packed blocks, but may possibly be
///   interleaved into the same VInt block.
///   This strategy is applied to pairs: `<document number, frequency>`, `<position, payload length>`,
///   `<position, offset start, offset length>`, and `<position, payload length, offsetstart, offset length>`.
///
/// - **Skipdata**:
///   Skipdata is interleaved with blocks on 2 levels. Level 0 skip data is interleaved between every packed
///   block. Level 1 skip data is interleaved between every 32 packed blocks.
///
/// - **Positions, Payloads, and Offsets**:
///   A position is an integer indicating where the term occurs within one document. A payload is a blob of
///   metadata associated with current position. An offset is a pair of integers indicating the tokenized
///   start/end offsets for given term in current position: it is essentially a specialized payload.
///   When payloads and offsets are not omitted, numPositions==numPayloads==numOffsets (assuming a null
///   payload contributes one count). As mentioned in block structure, it is possible to encode these three
///   either combined or separately.
///   In all cases, payloads and offsets are stored together. When encoded as a packed block, position data is
///   separated out as `.pos`, while payloads and offsets are encoded in `.pay` (payload metadata will also
///   be stored directly in `.pay`). When encoded as VInt blocks, all these three are stored interleaved into
///   the `.pos` (so is payload metadata).
///   With this strategy, the majority of payload and offset data will be outside the `.pos` file. So for
///   queries that require only position data, running on a full index with payloads and offsets, this
///   reduces disk pre-fetches.
///   Files and detailed format:
///
/// - `.tim`: Term Dictionary
/// - `.tip`: Term Index
/// - `.doc`: Frequencies and Skip Data
/// - `.pos`: Positions
/// - `.pay`: Payloads and Offsets
///
/// # Term Dictionary
///
/// The `.tim` file contains the list of terms in each field along with per-term statistics
/// (such as `docfreq`) and pointers to the frequencies, positions, payload and skip data in the
/// `.doc`, `.pos`, and `.pay` files. See `Lucene90BlockTreeTermsWriter` for more details on
/// the format.
///
/// **NOTE**: The term dictionary can plug into different postings implementations: the postings
/// writer/reader are actually responsible for encoding and decoding the PostingsHeader and
/// TermMetadata sections described here:
///
/// - **PostingsHeader** → Header, PackedBlockSize
/// - **TermMetadata** → (DocFPDelta \| SingletonDocID), PosFPDelta?, PosVIntBlockFPDelta?, PayFPDelta?
/// - **Header** → `CodecUtil::write_index_header`
/// - **PackedBlockSize**, **SingletonDocID** → `DataOutput::write_vint`
/// - **DocFPDelta**, **PosFPDelta**, **PayFPDelta**, **PosVIntBlockFPDelta** → `DataOutput::write_vlong`
/// - **Footer** → `CodecUtil::write_footer`
///
/// Notes:
///
/// - Header is a `CodecUtil::write_index_header` storing the version information for the postings.
/// - PackedBlockSize is the fixed block size for packed blocks. In packed blocks, bit width is
///   determined by the largest integer. Smaller block sizes result in smaller variance among
///   widths of integers hence smaller indexes. Larger block sizes result in more efficient bulk
///   I/O hence better acceleration. This value should always be a multiple of 64, currently fixed
///   at 128 as a tradeoff. It is also the skip interval used to accelerate
///   `PostingsEnum::advance`.
/// - DocFPDelta determines the position of this term’s TermFreqs within the `.doc` file. In
///   particular, it is the difference of file offset between this term’s data and the previous
///   term’s data (or zero, for the first term in the block). On disk it is stored as the
///   difference from the previous value in sequence.
/// - PosFPDelta determines the position of this term’s TermPositions within the `.pos` file,
///   while PayFPDelta determines the position of this term’s `<TermPayloads, TermOffsets?>` within
///   the `.pay` file. Similar to DocFPDelta, it is the difference between two file positions
///   (or omitted, for fields that omit payloads and offsets).
/// - PosVIntBlockFPDelta determines the position of this term’s last TermPosition in the last
///   pos packed block within the `.pos` file. It is a synonym for PayVIntBlockFPDelta or
///   OffsetVIntBlockFPDelta. This is actually used to indicate whether it is necessary to load
///   following payloads and offsets from `.pos` instead of `.pay`. Every time a new block of
///   positions is to be loaded, the PostingsReader will use this value to check whether the
///   current block is packed format or VInt. When packed format, payloads and offsets are fetched
///   from `.pay`; otherwise from `.pos`. (This value is neglected when the total number of
///   positions, i.e. totalTermFreq, is less than or equal to PackedBlockSize.)
/// - SingletonDocID is an optimization when a term only appears in one document. In this case,
///   instead of writing a file pointer to the `.doc` file (DocFPDelta), and then a VIntBlock at
///   that location, the single document ID is written to the term dictionary.
/// # Term Index
///
/// The `.tip` file contains an index into the term dictionary, so that it can be accessed
/// randomly. See `Lucene90BlockTreeTermsWriter` for more details on the format.
///
/// # Frequencies and Skip Data
///
/// The `.doc` file contains the lists of documents which contain each term, along with the
/// frequency of the term in that document (except when frequencies are omitted: `IndexOptions::DOCS`).
/// Skip data is saved at the end of each term’s postings. The skip data is saved once for the entire
/// postings list.
///
/// - `docFile(.doc)` → Header, `<TermFreqs>`^TermCount, Footer
/// - Header → `CodecUtil::write_index_header`
/// - TermFreqs → `<PackedBlock32>` ^(PackedDocBlockNum/32), VIntBlock?
/// - PackedBlock32 → Level1SkipData, `<PackedBlock>` ^32
/// - PackedBlock → Level0SkipData, PackedDocDeltaBlock, PackedFreqBlock?
/// - VIntBlock → `<DocDelta[,Freq?]>` ^(DocFreq − PackedBlockSize*PackedDocBlockNum)
/// - Level1SkipData → DocDelta, DocFPDelta, Skip1NumBytes?, ImpactLength?, Impacts?, PosFPDelta?, NextPosUpto?, PayFPDelta?, NextPayByteUpto?
/// - Level0SkipData → Skip0NumBytes, DocDelta, DocFPDelta, PackedBlockLength, ImpactLength?, Impacts?, PosFPDelta?, NextPosUpto?, PayFPDelta?, NextPayByteUpto?
/// - PackedFreqBlock → `PackedInts`, uses patching
/// - PackedDocDeltaBlock → `PackedInts`, does not use patching
/// - Footer → `CodecUtil::write_footer`
///
/// Notes:
///
/// - PackedDocDeltaBlock is theoretically generated from two steps:
///   1. Calculate the difference between each document number and the previous one, and get a d-gaps list (for the first document, use absolute value);
///   2. For those d-gaps from the first one to `PackedDocBlockNum*PackedBlockSize`<sup>th</sup>, separately encode as packed blocks.
///      If frequencies are not omitted, PackedFreqBlock will be generated without the d-gap step.
///
/// - VIntBlock stores remaining d-gaps (along with frequencies when possible) with a format that encodes DocDelta and Freq:
///
///   DocDelta: if frequencies are indexed, this determines both the document number and the frequency. In particular, DocDelta/2 is the difference between this document number and the previous document number (or zero when this is the first document in a TermFreqs). When DocDelta is odd, the frequency is one. When DocDelta is even, the frequency is read as another VInt. If frequencies are omitted, DocDelta contains the gap (not multiplied by 2) between document numbers and no frequency information is stored.
///
///   For example, the TermFreqs for a term which occurs once in document seven and three times in document eleven, with frequencies indexed, would be the following sequence of VInts:
///
///   15, 8, 3
///
///   If frequencies were omitted (`IndexOptions::DOCS`) it would be this sequence of VInts instead:
///
///   7, 4
///
/// - PackedDocBlockNum is the number of packed blocks for current term’s docids or frequencies. In particular, `PackedDocBlockNum = floor(DocFreq / PackedBlockSize)`.
///
/// - On skip data, DocDelta is the delta between the last doc of the previous block—or -1 if there is no previous block—and the last doc of this block. This helps know by how much the doc ID should be incremented in case the block gets skipped.
///
/// - Skip0Length is the length of skip data at level 0. Encoding it is useful when skip data is never needed to quickly skip over skip data, e.g. if only using `nextDoc()`. It is also used when only the first fields of skip data are needed, in order to skip over remaining fields without reading them.
///
/// - ImpactLength and Impacts are only stored if frequencies are indexed.
///
/// - Since positions and payloads are also block encoded, the skip should skip to the related block first, then fetch the values according to the in-block offset. PosFPSkip and PayFPSkip record the file offsets of the related block in `.pos` and `.pay`, respectively. While PosBlockOffset indicates which value to fetch inside the related block (PayBlockOffset is unnecessary since it is always equal to PosBlockOffset). Same as DocFPSkip, the file offsets are relative to the start of the current term’s TermFreqs, and stored as a difference sequence.
///
/// - PayByteUpto indicates the start offset of the current payload. It is equivalent to the sum of the payload lengths in the current block up to PosBlockOffset.
///
/// - ImpactLength is the total length of CompetitiveFreqDelta and CompetitiveNormDelta pairs. CompetitiveFreqDelta and CompetitiveNormDelta are used to safely skip score calculation for uncompetitive documents; see [`CompetitiveImpactAccumulator`](crate::core::codecs::competitive_impact_accumulator::CompetitiveImpactAccumulator) for more details.
/// # Positions
///
/// The `.pos` file contains the lists of positions that each term occurs at within documents.
/// It also sometimes stores part of payloads and offsets for speedup.
///
/// - **PosFile(.pos)** → Header, `<TermPositions>`^TermCount, Footer
/// - **Header** → `CodecUtil::write_index_header`
/// - **TermPositions** → `<PackedPosDeltaBlock>`^PackedPosBlockNum, VIntBlock?
/// - **VIntBlock** → `<PositionDelta[, PayloadLength?], PayloadData?, OffsetDelta?, OffsetLength?>`^PosVIntCount
/// - **PackedPosDeltaBlock** → `PackedInts`
/// - **PositionDelta**, **OffsetDelta**, **OffsetLength** → `DataOutput::write_vint`
/// - **PayloadData** → `DataOutput::write_byte`^PayLength
/// - **Footer** → `CodecUtil::write_footer`
///
/// Notes:
/// - TermPositions are ordered by term (terms are implicit, from the term dictionary), and position values for each term–document pair are incremental and ordered by document number.
/// - PackedPosBlockNum is the number of packed blocks for current term’s positions, payloads or offsets. In particular, `PackedPosBlockNum = floor(totalTermFreq / PackedBlockSize)`.
/// - PosVIntCount is the number of positions encoded as VInt format. In particular, `PosVIntCount = totalTermFreq - PackedPosBlockNum * PackedBlockSize`.
/// - The procedure for generating PackedPosDeltaBlock is the same as for PackedDocDeltaBlock in “Frequencies and Skip Data”.
/// - PositionDelta is, if payloads are disabled for the term’s field, the difference between the position of the current occurrence in the document and the previous occurrence (or zero, if this is the first occurrence in this document). If payloads are enabled, then `PositionDelta / 2` is the difference between the current and the previous position. If payloads are enabled and `PositionDelta` is odd, then `PayloadLength` is stored, indicating the length of the payload at the current term position.
/// - For example, the TermPositions for a term which occurs as the fourth term in one document, and as the fifth and ninth term in a subsequent document, would be the following sequence of VInts (payloads disabled):
///
///   `4, 5, 4`
/// - PayloadData is metadata associated with the current term position. If `PayloadLength` is stored at the current position, then it indicates the length of this payload. If `PayloadLength` is not stored, then this payload has the same length as the payload at the previous position.
/// - `OffsetDelta / 2` is the difference between this position’s `startOffset` and the previous occurrence (or zero, if this is the first occurrence in this document). If `OffsetDelta` is odd, then the length (`endOffset - startOffset`) differs from the previous occurrence and an `OffsetLength` follows. Offset data is only written for `IndexOptions::DOCS_AND_FREQS_AND_POSITIONS_AND_OFFSETS`.
/// # Payloads and Offsets
///
/// The `.pay` file will store payloads and offsets associated with certain term-document
/// positions. Some payloads and offsets will be separated out into the `.pos` file, for performance
/// reasons.
///
/// - **PayFile(.pay):** → Header, `<TermPayloads?, TermOffsets?>`^TermCount, Footer
/// - **Header** → `CodecUtil::write_index_header`
/// - **TermPayloads** → `<PackedPayLengthBlock, SumPayLength, PayData>`^PackedPayBlockNum
/// - **TermOffsets** → `<PackedOffsetStartDeltaBlock, PackedOffsetLengthBlock>`^PackedPayBlockNum
/// - **PackedPayLengthBlock, PackedOffsetStartDeltaBlock, PackedOffsetLengthBlock** → `PackedInts`
/// - **SumPayLength** → `DataOutput::write_vint`
/// - **PayData** → `DataOutput::write_byte`^SumPayLength
/// - **Footer** → `CodecUtil::write_footer`
///
/// Notes:
/// - The order of TermPayloads/TermOffsets will be the same as TermPositions; note that
///   part of payloads/offsets are stored in `.pos`.
/// - The procedure how `PackedPayLengthBlock` and `PackedOffsetLengthBlock` are generated is
///   the same as `PackedFreqBlock` in chapter “Frequencies and Skip Data”. While
///   `PackedOffsetStartDeltaBlock` follows the same procedure as `PackedDocDeltaBlock`.
/// - `PackedPayBlockNum` is always equal to `PackedPosBlockNum` for the same term. It is also a
///   synonym for `PackedOffsetBlockNum`.
/// - `SumPayLength` is the total length of payloads written within one block; it should be the
///   sum of PayLengths in that packed block.
/// - `PayLength` in `PackedPayLengthBlock` is the length of each payload associated with the
///   current position.
pub struct Lucene101PostingsFormat {
    min_term_block_size: i32,
    max_term_block_size: i32,
}

impl Default for Lucene101PostingsFormat {
    fn default() -> Self {
        Self::new()
    }
}

impl Lucene101PostingsFormat {
    /// Filename extension for some small metadata about how postings are
    /// encoded.
    pub const META_EXTENSION: &'static str = "psm";
    /// Filename extension for document number, frequencies, and skip data.
    /// See chapter: `Frequencies and Skip Data`
    pub const DOC_EXTENSION: &'static str = "doc";

    /// Filename extension for positions.
    /// See chapter: `Positions`
    pub const POS_EXTENSION: &'static str = "pos";

    /// Filename extension for payloads and offsets.
    /// See chapter: `Payloads and Offsets`
    pub const PAY_EXTENSION: &'static str = "pay";

    /// Size of blocks.
    pub const BLOCK_SIZE: usize = ForUtil::BLOCK_SIZE;

    #[allow(dead_code)]
    pub const BLOCK_MASK: usize = Self::BLOCK_SIZE - 1;

    /// We insert skip data on every block and every SKIP_FACTOR=32 blocks.
    pub const LEVEL1_FACTOR: i32 = 32;

    /// Total number of docs covered by level 1 skip data: 32 * 128 = 4,096
    pub const LEVEL1_NUM_DOCS: i32 = Self::LEVEL1_FACTOR * Self::BLOCK_SIZE as i32;

    pub const LEVEL1_MASK: i32 = Self::LEVEL1_NUM_DOCS - 1;

    pub(crate) const TERMS_CODEC: &'static str = "Lucene90PostingsWriterTerms";
    pub(crate) const META_CODEC: &'static str = "Lucene101PostingsWriterMeta";
    pub(crate) const DOC_CODEC: &'static str = "Lucene101PostingsWriterDoc";
    pub(crate) const POS_CODEC: &'static str = "Lucene101PostingsWriterPos";
    pub(crate) const PAY_CODEC: &'static str = "Lucene101PostingsWriterPay";

    pub(crate) const VERSION_START: i32 = 0;
    pub(crate) const VERSION_CURRENT: i32 = Self::VERSION_START;

    pub fn new() -> Self {
        Self::with_iterm_num(DEFAULT_MIN_BLOCK_SIZE, DEFAULT_MAX_BLOCK_SIZE).unwrap()
    }
    /// Creates a `Lucene101PostingsFormat` with custom values for `min_block_size` and `max_block_size`
    /// passed to the block terms dictionary.
    ///
    /// See [`Lucene90BlockTreeTermsWriter::new`](Lucene90BlockTreeTermsWriter)
    /// for details.
    pub fn with_iterm_num(min_items_in_block: i32, max_items_in_block: i32) -> Result<Self> {
        Self::validate_settings(min_items_in_block, max_items_in_block)?;
        Ok(Self {
            min_term_block_size: min_items_in_block,
            max_term_block_size: max_items_in_block,
        })
    }

    pub fn validate_settings(min_items_in_block: i32, max_items_in_block: i32) -> Result<()> {
        if min_items_in_block <= 1 {
            return Err(LuceneError::illegal_argument(format!(
                "min_items_in_block must be >= 2; got {min_items_in_block}"
            )));
        }
        if max_items_in_block < min_items_in_block {
            return Err(LuceneError::illegal_argument(format!(
                "max_items_in_block must be >= min_items_in_block; got max_items_in_block={max_items_in_block} min_items_in_block={min_items_in_block}"
            )));
        }
        if max_items_in_block < 2 * (min_items_in_block - 1) {
            return Err(LuceneError::illegal_argument(format!(
                "max_items_in_block must be at least 2*(min_items_in_block-1); got max_items_in_block={max_items_in_block} min_items_in_block={min_items_in_block}"
            )));
        }
        Ok(())
    }
}

impl PostingsFormat for Lucene101PostingsFormat {
    fn fields_consumer<D1, D2>(
        &self,
        state: &SegmentWriteState<D1>,
        segment_info: &SegmentInfo<D2>,
    ) -> Result<FieldsConsumerEnum<D1::IndexOutput>>
    where
        D1: Directory,
        D2: Directory,
    {
        let posting_writer =
            PushPostingsWriterBase::new(Lucene101PostingsWriter::new(state, segment_info)?);
        let ret = FieldsConsumerEnum::Lucene90(Lucene90BlockTreeTermsWriter::new(
            state,
            posting_writer,
            self.min_term_block_size,
            self.max_term_block_size,
            segment_info,
        )?);
        Ok(ret)
    }

    fn fields_producer<D1: Directory, D2: Directory>(
        &self,
        state: &SegmentReadState<D1>,
        segment_info: &SegmentInfo<D2>,
    ) -> Result<FieldsProducerEnum<D1::IndexInput>> {
        let postings_reader = Lucene101PostingsReader::new(state, segment_info)?;
        let ret = FieldsProducerEnum::Lucene90(Lucene90BlockTreeTermsReader::new(
            postings_reader,
            state,
            segment_info,
        )?);
        Ok(ret)
    }
}

/// Holds all state required for
/// [`Lucene101PostingsReader`](crate::core::codecs::lucene101::lucene101_postings_reader)
/// to produce a [`PostingsEnum`](crate::core::index::postings_enum::PostingsEnum)
/// without re-seeking the terms dict.
#[derive(Default, Clone)]
pub struct IntBlockTermState {
    /// file pointer to the start of the doc ids enumeration, in
    /// [`DOC_EXTENSION`](Lucene101PostingsFormat::DOC_EXTENSION) file
    pub doc_start_fp: i64,

    /// file pointer to the start of the positions enumeration, in
    /// [`POS_EXTENSION`](Lucene101PostingsFormat::POS_EXTENSION) file
    pub pos_start_fp: i64,

    /// file pointer to the start of the payloads enumeration, in
    /// [`PAY_EXTENSION`](Lucene101PostingsFormat::PAY_EXTENSION) file
    pub pay_start_fp: i64,

    /**
     * file offset for the last position in the last block, if there are
     * more than [`BLOCK_SIZE`](crate::core::codecs::lucene101) positions;
     * otherwise -1
     *
     * One might think to use total term frequency to track how many
     * positions are left to read as we decode the blocks, and decode
     * the last block differently when num_left_positions < BLOCK_SIZE.
     * Unfortunately this won't work since the tracking will be messed up
     * when we skip blocks as the skipper will only tell us new
     * position offset (start of block) and number of positions to skip
     * for that block, without telling us how many positions it has
     * skipped.
     */
    pub last_pos_block_offset: i64,

    /**
     * docid when there is a single pulsed posting, otherwise -1. freq is
     * always implicitly totalTermFreq in this case.
     */
    pub singleton_doc_id: i32,

    /// Base block term state
    pub base: BlockTermState,
}

impl Display for IntBlockTermState {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} docStartFP={} posStartFP={} payStartFP={} lastPosBlockOffset={} singletonDocID={}",
            self.base,
            self.doc_start_fp,
            self.pos_start_fp,
            self.pay_start_fp,
            self.last_pos_block_offset,
            self.singleton_doc_id
        )
    }
}

impl TermState for IntBlockTermState {
    fn copy_from(&mut self, other: &Self) -> Result<()> {
        self.doc_start_fp = other.doc_start_fp;
        self.pos_start_fp = other.pos_start_fp;
        self.pay_start_fp = other.pay_start_fp;
        self.last_pos_block_offset = other.last_pos_block_offset;
        self.singleton_doc_id = other.singleton_doc_id;
        self.base.copy_from(&other.base)
    }
}

#[cfg(test)]
mod tests {
    use rand::Rng;

    use crate::core::codecs::competitive_impact_accumulator::CompetitiveImpactAccumulator;
    use crate::core::codecs::lucene101::lucene101_postings_reader::{
        MutableImpactList, read_impacts, read_vint15, read_vlong15,
    };
    use crate::core::codecs::lucene101::lucene101_postings_writer::{
        write_impacts, write_vint15, write_vlong15,
    };
    use crate::core::index::impact::Impact;
    use crate::core::store::directory::Directory;
    use crate::core::store::{
        ByteArrayDataInput, ByteArrayDataOutput, DataInput, IOContext, IndexInput,
    };
    use crate::core::util::error::lucene_error::Result;
    use crate::test::index::base_index_file_format_test_case::BaseIndexFileFormatTestCase;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{new_directory, random};

    struct TestLucene101PostingsFormat;
    impl BaseIndexFileFormatTestCase for TestLucene101PostingsFormat {
        // TODO
    }
    #[test]
    fn test_vint15() -> Result<()> {
        let buffer = vec![0u8; 5];
        let mut out = ByteArrayDataOutput::with_bytes(buffer);
        for &i in &[0i32, 1, 127, 128, 32767, 32768, i32::MAX] {
            out.reset()?;
            write_vint15(&mut out, i)?;
            let mut inp = ByteArrayDataInput::with_bytes(out.bytes.clone());
            let v = read_vint15(&mut inp)?;
            assert_eq!(v, i);
            assert_eq!(inp.get_position(), out.get_position());
        }
        Ok(())
    }
    #[test]
    fn test_vlong15() -> Result<()> {
        // buffer size should accommodate the largest encoded value
        let mut out = ByteArrayDataOutput::with_bytes(vec![0u8; 9]);
        for &i in &[0i64, 1, 127, 128, 32_767, 32_768, i32::MAX as i64, i64::MAX] {
            out.reset()?;
            write_vlong15(&mut out, i)?;
            let mut inp = ByteArrayDataInput::with_bytes(out.bytes.clone());
            let v = read_vlong15(&mut inp)?;
            assert_eq!(v, i);
            assert_eq!(inp.get_position(), out.get_position());
        }
        Ok(())
    }
    #[test]
    fn test_final_block() -> Result<()> {
        // TODO
        Ok(())
    }
    #[test]
    fn test_impact_serialization() -> Result<()> {
        let cases = vec![
            vec![Impact { freq: 1, norm: 1 }],
            vec![Impact { freq: 1, norm: 42 }],
            vec![Impact {
                freq: 1,
                norm: -100,
            }],
            vec![Impact { freq: 30, norm: 1 }],
            vec![Impact { freq: 500, norm: 1 }],
            vec![
                Impact { freq: 1, norm: 7 },
                Impact { freq: 3, norm: 9 },
                Impact { freq: 7, norm: 10 },
                Impact { freq: 15, norm: 11 },
                Impact { freq: 20, norm: 13 },
                Impact { freq: 28, norm: 14 },
            ],
            vec![
                Impact { freq: 2, norm: 2 },
                Impact { freq: 10, norm: 10 },
                Impact { freq: 12, norm: 50 },
                Impact {
                    freq: 50,
                    norm: -100,
                },
                Impact {
                    freq: 1000,
                    norm: -80,
                },
                Impact {
                    freq: 1005,
                    norm: -3,
                },
            ],
        ];

        for impacts in cases {
            do_test_impact_serialization(&impacts)?;
        }

        Ok(())
    }
    fn do_test_impact_serialization(impacts: &[Impact]) -> Result<()> {
        let mut random = random();
        let mut acc = CompetitiveImpactAccumulator::new();
        for imp in impacts {
            acc.add(imp.freq, imp.norm);
        }
        let dir = new_directory(&mut random)?;
        {
            let mut out = dir.create_output("foo", &IOContext::default_io_context()?)?;
            write_impacts(&acc.get_competitive_freq_norm_pairs(), &mut out)?;
        }
        let mut input = dir.open_input("foo", &IOContext::default_io_context()?)?;
        let len = input.length();
        let mut buffer = vec![0u8; len as usize];
        input.read_bytes(&mut buffer, 0, len as i32)?;

        let mut data_in = ByteArrayDataInput::with_bytes(buffer);
        let mut mutable_impacts_list =
            MutableImpactList::with_capacity(impacts.len() + random.random_range(0..3));
        read_impacts(&mut data_in, &mut mutable_impacts_list)?;
        let len = mutable_impacts_list.length;
        assert_eq!(&mutable_impacts_list.impacts[0..len], impacts);
        Ok(())
    }
}
