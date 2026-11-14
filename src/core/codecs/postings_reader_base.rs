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
use crate::core::codecs::block_term_state::{BlockTermState, TermStateEnum};
use crate::core::index::field_info::FieldInfo;
use crate::core::index::impacts_enum::ImpactsEnum;
use crate::core::index::postings_enum::PostingsEnum;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::term_state::TermState;
use crate::core::store::directory::Directory;
use crate::core::store::{DataInput, IndexInput};
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::Result;
use std::fmt::Display;
use std::sync::Arc;

/// The core terms dictionaries (BlockTermsReader, BlockTreeTermsReader)
/// interact with a single instance of This struct to manage creation of
/// [`PostingsEnum`] and
/// [`ImpactsEnum`] instances. It
/// provides an IndexInput (`termsIn`) where This struct may read any previously
/// stored data that it had written in its corresponding
/// [`PostingsWriterBase`](crate::core::codecs::postings_writer_base::PostingsWriterBase) at indexing time.
pub trait PostingsReaderBase: Display + CloseableRef {
  type TermState: TermState + AsRef<BlockTermState> + AsMut<BlockTermState> + Into<TermStateEnum>;
  type TermStateRef<'a>: From<&'a Self::TermState>
    + TryFrom<&'a TermStateEnum, Error = &'static str>
    + std::ops::Deref<Target = Self::TermState>
  where
    Self::TermState: 'a;
  type TermStateMut<'a>: From<&'a mut Self::TermState>
    + TryFrom<&'a mut TermStateEnum, Error = &'static str>
    + std::ops::DerefMut<Target = Self::TermState>
  where
    Self::TermState: 'a;
  /// Performs any initialization, such as reading and verifying the header
  /// from the provided terms dictionary [`IndexInput`].
  fn init<D1, D2, II>(
    &self,
    terms_in: &mut II,
    state: &SegmentReadState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<()>
  where
    D1: Directory,
    II: IndexInput;

  /// Return a newly created empty [`TermState`].
  fn new_term_state(&self) -> Result<Self::TermState>;

  /// Actually decode metadata for next term
  ///
  /// See also:
  /// - [`PostingsWriterBase::encode_term`](crate::core::codecs::postings_writer_base::PostingsWriterBase::encode_term)
  fn decode_term<'a, DI, S, E>(
    &self,
    input: &mut DI,
    field_info: &Arc<FieldInfo>,
    state: S,
    absolute: bool,
  ) -> Result<()>
  where
    DI: DataInput,
    Self::TermState: 'a,
    S: TryInto<Self::TermStateMut<'a>, Error = E>,
    E: Display;

  /// Must fully consume `state`, since after this call that [`TermState`] may
  /// be reused.
  type PostingsEnum: PostingsEnum;
  fn postings<'a, S, E>(
    &self,
    field_info: &FieldInfo,
    state: S,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Option<Self::PostingsEnum>>
  where
    Self::TermState: 'a,
    S: TryInto<Self::TermStateRef<'a>, Error = E>,
    E: Display;

  type ImpactsEnum: ImpactsEnum;
  /// Return an [`ImpactsEnum`] that computes impacts
  /// with `scorer`.
  ///
  /// See also:
  /// - [`postings`](Self::postings)
  fn impacts<'a, S, E>(
    &self,
    field_info: &FieldInfo,
    state: S,
    flags: i32,
  ) -> Result<Self::ImpactsEnum>
  where
    Self::TermState: 'a,
    S: TryInto<Self::TermStateRef<'a>, Error = E>,
    E: Display;

  /// Checks consistency of this reader.
  ///
  /// Note that this may be costly in terms of I/O, e.g. may involve computing
  /// a checksum value against large data files.
  fn check_integrity(&self) -> Result<()>;
}
