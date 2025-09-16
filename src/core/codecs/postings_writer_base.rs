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
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::index::BytesRef;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::postings_enum::PostingsEnum;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::store::directory::Directory;
use crate::core::store::{DataOutput, IndexOutput};
use crate::core::util::error::lucene_error::Result;
use crate::core::util::fixed_bit_set::FixedBitSet;
use std::borrow::Cow;
use std::sync::Arc;

/// Trait that plugs into term dictionaries, such as
/// [`Lucene90BlockTreeTermsWriter`](crate::core::codecs::lucene90::block_tree::lucene90_block_tree_terms_writer::Lucene90BlockTreeTermsWriter),
/// and handles writing postings.
///
/// See also:
/// - [`PostingsReaderBase`](crate::core::codecs::postings_reader_base::PostingsReaderBase)
// TODO: find a better name; this defines the API that the
// terms dict impls use to talk to a postings impl.
// TermsDict + PostingsReader/WriterBase == FieldsProducer/Consumer
pub trait PostingsWriterBase {
    /// Called once after startup, before any terms have been added.
    /// Implementations typically write a header to the provided `termsOut`.
    fn init<D1, D2>(
        &mut self,
        terms_out: &mut impl IndexOutput,
        state: &SegmentWriteState<D1>,
        segment_info: &SegmentInfo<D2>,
    ) -> Result<()>
    where
        D1: Directory,
        D2: Directory;

    /// Write all postings for one term; use the provided [`TermsEnum`] to pull
    /// a [`PostingsEnum`]. This
    /// method should not re-position the `terms_enum`! It is already
    /// positioned on the term that should be written. This method must set the
    /// bit in the provided [`FixedBitSet`] for every docID written. If no
    /// docs were written, this method should return `None`, and the terms
    /// dict will skip the term.
    fn write_term<N: NormsProducer, PE: PostingsEnum>(
        &mut self,
        _term: &BytesRef<Vec<u8>>,
        _terms_enum: &mut impl TermsEnum<PostingsEnum = PE>,
        _docs_seen: &mut FixedBitSet,
        _norms: &Option<N>,
        _postings_enum: Option<PE>,
    ) -> Result<(Option<PE>, Option<BlockTermStateEnum>)> {
        unimplemented!()
    }

    /// Encode metadata as `&[i64]` and `&[u8]`. `absolute` controls whether the
    /// current term is delta encoded according to the latest term. Usually
    /// elements in `longs` are file pointers, so each one always increases
    /// when a new term is consumed. `out` is used to write generic bytes,
    /// which are not monotonic.
    fn encode_term(
        &mut self,
        out: &mut impl DataOutput,
        field_info: &FieldInfo,
        state: Cow<BlockTermStateEnum>,
        absolute: bool,
    ) -> Result<()>;

    /// Sets the current field for writing.
    fn set_field(&mut self, field_info: Arc<FieldInfo>);
}
