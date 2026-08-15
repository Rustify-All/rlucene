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
use crate::core::codecs::fields_consumer::FieldsConsumer;
use crate::core::codecs::fields_producer::FieldsProducer;
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::codecs::postings_format::PostingsFormat;
use crate::core::index::fields::Fields;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_reader::Identity;
use crate::core::index::index_writer::MAX_POSITION;
use crate::core::index::postings_enum::{FREQS, OFFSETS, PAYLOADS, POSITIONS, PostingsEnum};
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, NO_MORE_DOCS};
use crate::core::store::directory::Directory;
use crate::core::store::{IndexInput, IndexOutput};
use crate::core::util::HasIdentity;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::iterator::IteratorExt;
use crate::test_framework::core::index::asserting_leaf_reader::AssertingTerms;
use crate::test_framework::core::util::test_util::{DefaultPostingsFormat, TestUtil};
use std::sync::{Arc, OnceLock};

/// Just like the default postings format but with additional asserts.
pub struct AssertingPostingsFormat {
  in_: DefaultPostingsFormat,
  identity: Identity,
}

impl AssertingPostingsFormat {
  pub fn new() -> Self {
    Self {
      in_: TestUtil::get_default_postings_format(),
      identity: Identity::new(),
    }
  }
}

impl HasIdentity for AssertingPostingsFormat {
  fn identity(&self) -> &Identity {
    &self.identity
  }
}

impl PostingsFormat for AssertingPostingsFormat {
  fn get_name(&self) -> &str {
    "Asserting"
  }

  type FieldsConsumer<O: IndexOutput> =
    AssertingFieldsConsumer<<DefaultPostingsFormat as PostingsFormat>::FieldsConsumer<O>>;

  fn fields_consumer<D1, D2>(
    &self,
    state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::FieldsConsumer<D1::IndexOutput>>
  where
    D1: Directory,
  {
    Ok(AssertingFieldsConsumer::new(
      self.in_.fields_consumer(state, segment_info)?,
    ))
  }

  type FieldsProducer<T: IndexInput> =
    AssertingFieldsProducer<<DefaultPostingsFormat as PostingsFormat>::FieldsProducer<T>>;

  fn fields_producer<D1, D2>(
    &self,
    state: &SegmentReadState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::FieldsProducer<D1::IndexInput>>
  where
    D1: Directory,
  {
    Ok(AssertingFieldsProducer::new(
      self.in_.fields_producer(state, segment_info)?,
    ))
  }

  fn for_name(name: &str) -> Result<Arc<Self>> {
    static FORMAT: OnceLock<Arc<AssertingPostingsFormat>> = OnceLock::new();

    match name {
      "Asserting" => Ok(Arc::clone(
        FORMAT.get_or_init(|| Arc::new(AssertingPostingsFormat::new())),
      )),
      _ => Err(LuceneError::illegal_argument(format!(
        "Could not load postings format named \"{name}\""
      ))),
    }
  }
}

pub struct AssertingFieldsProducer<FP> {
  in_: FP,
}

impl<FP> AssertingFieldsProducer<FP>
where
  FP: FieldsProducer,
{
  fn new(in_: FP) -> Self {
    Self { in_ }
  }

  pub(crate) fn into_inner(self) -> FP {
    self.in_
  }
}

impl<FP> CloseableRef for AssertingFieldsProducer<FP>
where
  FP: FieldsProducer,
{
  fn close(&self) -> Result<()> {
    self.in_.close()?;
    self.in_.close()
  }
}

impl<FP> Fields for AssertingFieldsProducer<FP>
where
  FP: FieldsProducer,
{
  type FieldIter<'a>
    = FP::FieldIter<'a>
  where
    Self: 'a;

  fn iterator(&self) -> Result<Self::FieldIter<'_>> {
    self.in_.iterator()
  }

  type Terms = AssertingTerms<FP::Terms>;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    Ok(self.in_.terms(field)?.map(AssertingTerms::new))
  }

  fn size(&self) -> Result<i32> {
    self.in_.size()
  }
}

impl<FP> FieldsProducer for AssertingFieldsProducer<FP>
where
  FP: FieldsProducer,
{
  fn check_integrity(&self) -> Result<()> {
    self.in_.check_integrity()
  }

  fn get_merge_instance(&self) -> Result<Option<Self>>
  where
    Self: Sized,
  {
    Ok(
      self
        .in_
        .get_merge_instance()?
        .map(AssertingFieldsProducer::new),
    )
  }
}

pub struct AssertingFieldsConsumer<FC> {
  in_: FC,
}

impl<FC> AssertingFieldsConsumer<FC> {
  fn new(in_: FC) -> Self {
    Self { in_ }
  }
}

impl<FC> FieldsConsumer for AssertingFieldsConsumer<FC>
where
  FC: FieldsConsumer,
{
  fn write<D1, D2, F, N>(
    &mut self,
    state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    fields: &mut F,
    norms: Option<&N>,
  ) -> Result<()>
  where
    D1: Directory,
    F: Fields,
    N: NormsProducer,
  {
    self.in_.write(state, segment_info, fields, norms)?;

    // TODO: more asserts?  can we somehow run a
    // "limited" CheckIndex here???  Or ... can we improve
    // AssertingFieldsProducer and us it also to wrap the
    // incoming Fields here?

    let mut last_field: Option<String> = None;
    let mut fields_iterator = fields.iterator()?;

    while fields_iterator.has_next()? {
      let field = fields_iterator
        .next()?
        .expect("Fields.iterator.has_next returned true");
      let field_info = state
        .field_infos
        .field_info_by_name(field)?
        .expect("field returned by Fields must have FieldInfo");
      assert!(
        last_field
          .as_ref()
          .is_none_or(|last_field| last_field < field)
      );
      last_field = Some(field.clone());

      let Some(terms) = fields.terms(field)? else {
        continue;
      };

      let mut terms_enum = terms.iterator()?;
      let mut last_term = None;
      let mut postings_enum = None;

      let has_freqs = field_info.get_index_options() >= &IndexOptions::DocsAndFreqs;
      let has_positions = field_info.get_index_options() >= &IndexOptions::DocsAndFreqsAndPositions;
      let has_offsets =
        field_info.get_index_options() >= &IndexOptions::DocsAndFreqsAndPositionsAndOffsets;
      let has_payloads = terms.has_payloads();

      assert_eq!(has_positions, terms.has_positions());
      assert_eq!(has_offsets, terms.has_offsets());

      while let Some(term) = terms_enum.next()? {
        let term = term.into_owned();
        assert!(last_term.as_ref().is_none_or(|last_term| last_term < &term));
        last_term = Some(term);

        #[allow(clippy::needless_late_init)]
        let flags;
        if !has_positions {
          flags = if has_freqs { FREQS as i32 } else { 0 };
        } else {
          let mut position_flags = POSITIONS as i32;
          if has_payloads {
            position_flags |= PAYLOADS as i32;
          }
          if has_offsets {
            position_flags |= OFFSETS as i32;
          }
          flags = position_flags;
        }
        postings_enum = Some(terms_enum.postings_with_flags(postings_enum, flags)?);

        let postings_enum = postings_enum
          .as_mut()
          .expect("TermsEnum.postings must return a postings iterator");
        let mut last_doc_id = -1;

        loop {
          let doc_id = postings_enum.next_doc()?;
          if doc_id == NO_MORE_DOCS {
            break;
          }
          assert!(doc_id > last_doc_id);
          last_doc_id = doc_id;
          if has_freqs {
            let freq = postings_enum.freq()?;
            assert!(freq > 0);

            if has_positions {
              let mut last_pos = -1;
              let mut last_start_offset = -1;
              for i in 0..freq {
                let pos = postings_enum.next_position()?;
                assert!(
                  pos >= last_pos,
                  "pos={pos} vs lastPos={last_pos} i={i} freq={freq}"
                );
                assert!(
                  pos <= MAX_POSITION,
                  "pos={pos} is > IndexWriter.MAX_POSITION={MAX_POSITION}"
                );
                last_pos = pos;

                if has_offsets {
                  let start_offset = postings_enum.start_offset()?;
                  let end_offset = postings_enum.end_offset()?;
                  assert!(end_offset >= start_offset);
                  assert!(start_offset >= last_start_offset);
                  last_start_offset = start_offset;
                }
              }
            }
          }
        }
      }
    }
    Ok(())
  }
}

impl<FC> Closeable for AssertingFieldsConsumer<FC>
where
  FC: Closeable,
{
  fn close(&mut self) -> Result<()> {
    self.in_.close()?;
    self.in_.close()
  }
}
