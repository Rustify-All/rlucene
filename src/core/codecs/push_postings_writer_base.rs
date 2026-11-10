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
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::codecs::postings_writer_base::PostingsWriterBase;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::numeric_doc_values::{NumericDocValues, NumericDocValuesVisitor};
use crate::core::index::postings_enum::{
  FREQS, OFFSETS, PAYLOADS, POSITIONS, PostingsEnum, PostingsEnumVisitor,
};
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::index::{BytesRef, BytesRefValue};
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::store::directory::Directory;
use crate::core::store::{DataOutput, IndexOutput};
use crate::core::util::bit_set::BitSet;
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use std::borrow::Cow;
use std::sync::Arc;

/// Extension of [`PostingsWriterBase`], adding a push API for writing each
/// element of the postings. This API is somewhat analogous to an XML SAX API,
/// while [`PostingsWriterBase`] is more like an XML DOM API.
///
/// See also [`PostingsWriterBase`].
pub struct PushPostingsWriterBase<S> {
  enum_flags: i32,

  /// [`FieldInfo`](crate::core::index::field_info::FieldInfo) of current field being written.
  pub(crate) field_info: Option<Arc<FieldInfo>>,

  /// [`IndexOptions`](crate::core::index::index_options::IndexOptions) of current field being written.
  pub(crate) index_options: IndexOptions,

  options: FieldWriteOptions,

  sub: S,
}
pub struct FieldWriteOptions {
  /// True if the current field writes freqs.
  pub(crate) write_freqs: bool,
  /// True if the current field writes positions.
  pub(crate) write_positions: bool,
  /// True if the current field writes payloads.
  pub(crate) write_payloads: bool,
  /// True if the current field writes offsets.
  pub(crate) write_offsets: bool,
}

impl<S> PushPostingsWriterBase<S> {
  #[allow(clippy::too_many_arguments)]
  /// # Parameters
  /// - `field_info`: It is just a placeholder value; it should be initialized
  ///   as None, but I don't want to add extra wrapping around it. It would be
  ///   set in [`set_field`](Self::set_field) before used
  pub fn new(sub: S) -> Self {
    let options = FieldWriteOptions {
      write_freqs: false,
      write_positions: false,
      write_payloads: false,
      write_offsets: false,
    };
    Self {
      enum_flags: 0,
      field_info: None,
      index_options: Default::default(),
      options,
      sub,
    }
  }
}
/// Keeps the borrowed norms layout concrete for the original term loop.
struct WriteTermNormsVisitor<'a, S, PE> {
  sub: &'a mut S,
  options: &'a FieldWriteOptions,
  postings_enum: &'a mut PE,
  docs_seen: &'a mut FixedBitSet,
}

impl<S, PE> NumericDocValuesVisitor for WriteTermNormsVisitor<'_, S, PE>
where
  S: PushPostingsWriterBaseAbstract,
  PE: PostingsEnum,
{
  type Output = Result<(i32, i64)>;

  fn visit<D: NumericDocValues>(self, norm_values: Option<&mut D>) -> Self::Output {
    self.postings_enum.visit(WriteTermPostingsVisitor {
      sub: self.sub,
      options: self.options,
      docs_seen: self.docs_seen,
      norm_values,
    })
  }
}

struct WriteTermPostingsVisitor<'a, S, D> {
  sub: &'a mut S,
  options: &'a FieldWriteOptions,
  docs_seen: &'a mut FixedBitSet,
  norm_values: Option<&'a mut D>,
}

impl<S, D> PostingsEnumVisitor for WriteTermPostingsVisitor<'_, S, D>
where
  S: PushPostingsWriterBaseAbstract,
  D: NumericDocValues,
{
  type Output = Result<(i32, i64)>;

  fn visit<PE: PostingsEnum>(self, postings_enum: &mut PE) -> Self::Output {
    let visitor = WriteTermNormsVisitor {
      sub: self.sub,
      options: self.options,
      postings_enum,
      docs_seen: self.docs_seen,
    };
    if !self.options.write_payloads && !self.options.write_offsets {
      visitor.visit_with_position_mode::<D, true>(self.norm_values)
    } else {
      visitor.visit_with_position_mode::<D, false>(self.norm_values)
    }
  }
}

impl<S, PE> WriteTermNormsVisitor<'_, S, PE>
where
  S: PushPostingsWriterBaseAbstract,
  PE: PostingsEnum,
{
  fn visit_with_position_mode<D: NumericDocValues, const SIMPLE_POSITIONS: bool>(
    self,
    mut norm_values: Option<&mut D>,
  ) -> Result<(i32, i64)> {
    let mut doc_freq = 0;
    let mut total_term_freq = 0i64;
    loop {
      let doc_id = self.postings_enum.next_doc()?;
      if doc_id == NO_MORE_DOCS {
        break;
      }
      doc_freq += 1;
      self.docs_seen.set(doc_id as usize)?;

      let freq = if self.options.write_freqs {
        let f = self.postings_enum.freq()?;
        total_term_freq += f as i64;
        f
      } else {
        -1
      };

      self
        .sub
        .start_doc(norm_values.as_deref_mut(), doc_id, freq, self.options)?;

      if self.options.write_positions {
        for _ in 0..freq {
          let pos = self.postings_enum.next_position()?;
          let payload = if !SIMPLE_POSITIONS && self.options.write_payloads {
            self.postings_enum.get_payload()?
          } else {
            None
          };
          let (start_offset, end_offset) = if !SIMPLE_POSITIONS && self.options.write_offsets {
            (
              self.postings_enum.start_offset()?,
              self.postings_enum.end_offset()?,
            )
          } else {
            (-1, -1)
          };
          let payload = payload.as_ref().map(BytesRefValue::as_bytes_ref);
          if SIMPLE_POSITIONS {
            self
              .sub
              .add_position_without_payloads_or_offsets(pos, self.options)?;
          } else {
            self.sub.add_position(
              pos,
              payload.as_ref(),
              start_offset,
              end_offset,
              self.options,
            )?;
          }
        }
      }

      self.sub.finish_doc()?;
    }
    Ok((doc_freq, total_term_freq))
  }
}

impl<S> Closeable for PushPostingsWriterBase<S>
where
  S: Closeable,
{
  fn close(&mut self) -> Result<()> {
    Closeable::close(&mut self.sub)
  }
}
impl<S> PostingsWriterBase for PushPostingsWriterBase<S>
where
  S: PushPostingsWriterBaseAbstract + PostingsWriterBase,
{
  type TermState = S::TermState;

  fn init<D1, D2, IO>(
    &mut self,
    terms_out: &mut IO,
    state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<()>
  where
    D1: Directory,
    IO: IndexOutput,
  {
    self.sub.init(terms_out, state, segment_info)
  }

  #[inline]
  fn write_term<N, PE, T>(
    &mut self,
    _term: &BytesRef<Vec<u8>>,
    terms_enum: &mut T,
    docs_seen: &mut FixedBitSet,
    norms: Option<&N>,
    postings_enum: Option<PE>,
  ) -> Result<(Option<PE>, Option<Self::TermState>)>
  where
    N: NormsProducer,
    PE: PostingsEnum,
    T: TermsEnum<PostingsEnum = PE>,
  {
    let field_info = self
      .field_info
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("postings field info is not initialized"))?;
    let mut norm_values = if field_info.has_norms() {
      let norms = norms
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("norms producer is missing"))?;
      Some(norms.get_norms(field_info)?)
    } else {
      None
    };

    self.sub.start_term(&self.options)?;

    let mut postings_enum = terms_enum.postings_with_flags(postings_enum, self.enum_flags)?;

    let visitor = WriteTermNormsVisitor {
      sub: &mut self.sub,
      options: &self.options,
      postings_enum: &mut postings_enum,
      docs_seen,
    };
    let (doc_freq, total_term_freq) = match norm_values.as_mut() {
      Some(values) => values.visit(visitor),
      None => visitor.visit::<N::NumericDocValues>(None),
    }?;

    if doc_freq == 0 {
      return Ok((Some(postings_enum), None));
    }

    let mut upper = self.sub.new_term_state()?;
    let state = upper.as_mut();
    state.doc_freq = doc_freq;
    state.total_term_freq = if self.options.write_freqs {
      total_term_freq
    } else {
      -1
    };
    self.sub.finish_term(&mut upper, &self.options)?;
    Ok((Some(postings_enum), Some(upper)))
  }

  fn encode_term<DO>(
    &mut self,
    out: &mut DO,
    field_info: &FieldInfo,
    state: Cow<Self::TermState>,
    absolute: bool,
  ) -> Result<()>
  where
    DO: DataOutput,
  {
    self
      .sub
      .encode_term_with_option(out, field_info, state, absolute, &self.options)
  }
  /// Sets the current field for writing, and returns the fixed length of
  /// `&[i64]` metadata (which is fixed per field), called when the
  /// writing switches to another field.
  fn set_field(&mut self, field_info: &Arc<FieldInfo>) {
    self.index_options = *field_info.get_index_options();
    let options = &mut self.options;
    options.write_freqs = self.index_options >= IndexOptions::DocsAndFreqs;
    options.write_positions = self.index_options >= IndexOptions::DocsAndFreqsAndPositions;
    options.write_offsets = self.index_options >= IndexOptions::DocsAndFreqsAndPositionsAndOffsets;
    options.write_payloads = field_info.has_payloads();
    self.field_info = Option::from(field_info.clone());

    self.enum_flags = if !options.write_freqs {
      0
    } else if !options.write_positions {
      FREQS as i32
    } else if !options.write_offsets {
      if options.write_payloads {
        PAYLOADS as i32
      } else {
        POSITIONS as i32
      }
    } else if options.write_payloads {
      (PAYLOADS | OFFSETS) as i32
    } else {
      OFFSETS as i32
    };
    self.sub.set_field(field_info)
  }
}
pub trait PushPostingsWriterBaseAbstract: PostingsWriterBase {
  /// Return a newly created empty TermState
  fn new_term_state(&mut self) -> Result<Self::TermState>;

  /// Start a new term.
  /// A matching call to [`finish_term`](Self::finish_term) will be done only
  /// if the term has at least one document.
  fn start_term(&mut self, options: &FieldWriteOptions) -> Result<()>;

  /// Finishes the current term. The provided [`BlockTermState`](crate::core::codecs::block_term_state::BlockTermState) contains
  /// the term's summary statistics and will hold metadata from PBF when
  /// returned.
  fn finish_term(&mut self, state: &mut Self::TermState, options: &FieldWriteOptions)
  -> Result<()>;

  /// Adds a new doc in this term. `freq` will be -1 when term
  /// frequencies are omitted for the field.
  fn start_doc<NDV>(
    &mut self,
    norms: Option<&mut NDV>,
    doc_id: i32,
    freq: i32,
    options: &FieldWriteOptions,
  ) -> Result<()>
  where
    NDV: NumericDocValues;

  /// Add a new position and payload, and start/end offset.
  /// An absent payload means no payload; a present payload with zero length
  /// also means no payload. Caller may reuse the [`BytesRef`] for the payload
  /// between calls (method must fully consume the payload).
  /// `start_offset` and `end_offset` will be -1 when offsets are not indexed.
  fn add_position(
    &mut self,
    position: i32,
    payload: Option<&BytesRef<&[u8]>>,
    start_offset: i32,
    end_offset: i32,
    options: &FieldWriteOptions,
  ) -> Result<()>;

  /// Adds a position for a field with neither payloads nor offsets.
  /// The caller must select this mode before the position loop. The default
  /// preserves custom writers' existing [`add_position`](Self::add_position) callback.
  fn add_position_without_payloads_or_offsets(
    &mut self,
    position: i32,
    options: &FieldWriteOptions,
  ) -> Result<()> {
    self.add_position(position, None, -1, -1, options)
  }

  /// Called when we are done adding positions and payloads for each doc.
  fn finish_doc(&mut self) -> Result<()>;
  fn encode_term_with_option<DO>(
    &mut self,
    out: &mut DO,
    field_info: &FieldInfo,
    state: Cow<Self::TermState>,
    absolute: bool,
    options: &FieldWriteOptions,
  ) -> Result<()>
  where
    DO: DataOutput;
}
