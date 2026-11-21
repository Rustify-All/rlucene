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
use crate::core::codecs::block_term_state::TermStateEnum;
use crate::core::codecs::codec;
use crate::core::codecs::fields_consumer::FieldsConsumer;
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::codecs::postings_format::PostingsFormat;
use crate::core::index::BytesRef;
use crate::core::index::automaton_terms_enum::AutomatonTermsEnum;
use crate::core::index::bytes_ref::{BytesRefValue, BytesRefValueEnum};
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::doc_values_skip_index_type::DocValuesSkipIndexType;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::dummy::dummy_impacts_enum::DummyImpactsEnum;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::fields::Fields;
use crate::core::index::filtered_terms_enum::FilteredTermsEnum;
use crate::core::index::impact::Impact;
use crate::core::index::impacts::Impacts;
use crate::core::index::impacts_source::ImpactsSource;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::postings_enum::PostingsEnum;
use crate::core::index::postings_enum::{
  FREQS, NONE, OFFSETS, PAYLOADS, POSITIONS, feature_requested,
};
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::store::IOContext;
use crate::core::store::directory::{DirEnum, Directory};
use crate::core::store::flush_info::FlushInfo;
use crate::core::util::ToInt;
use crate::core::util::access::ByteSource;
use crate::core::util::automation::byte_runnable::ByteRunnable;
use crate::core::util::automation::compiled_automaton::AutomatonType;
use crate::core::util::automation::compiled_automaton::CompiledAutomaton;
use crate::core::util::automation::operations::Operations;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::dummy::dummy_attribute_source::DummyAttributeSource;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind};
use crate::core::util::info_stream::get_default_info_stream;
use crate::core::util::iterator::{IteratorExt, VecIter, VecIteratorExt};
use crate::core::util::string_helper::StringHelper;
use crate::core::util::unicode_util::UnicodeUtil;
use crate::core::util::version::LATEST;
use crate::test_framework::core::util::automaton::automaton_test_util::{
  AutomatonTestUtil, RandomAcceptedStrings,
};
use crate::test_framework::core::util::index_package_access::{
  IndexPackageAccess, IndexPackageAccessImpl,
};
use crate::test_framework::core::util::lucene_test_case::{
  at_least_usize, is_night_mode, new_fs_directory, random_from_seed, random_multiplier,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::prelude::SliceRandom;
use rand::rngs::StdRng;
use rand::{Rng, RngExt};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::{thread, vec};
use strum::IntoEnumIterator;
use strum_macros::EnumIter;
use tempfile::TempDir;

/// Which features to test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter)]
#[repr(u8)]
pub enum Option_ {
  /// Sometimes use `.advance()`.
  Skipping,

  /// Sometimes reuse the `PostingsEnum` across terms.
  ReuseEnums,

  /// Sometimes pass present live docs.
  LiveDocs,

  /// Sometimes seek to term using previously saved `TermState`.
  TermState,

  /// Sometimes don't fully consume docs from the enum.
  PartialDocConsume,

  /// Sometimes don't fully consume positions at each doc.
  PartialPosConsume,

  /// Sometimes check payloads.
  Payloads,

  /// Test w/ multiple threads.
  Threads,
}

pub struct ThreadState<P> {
  // Only used with REUSE option:
  pub reuse_postings_enum: Option<P>,
}

impl<P> Default for ThreadState<P> {
  fn default() -> Self {
    Self {
      reuse_postings_enum: None,
    }
  }
}

/// Helper struct extracted from `BasePostingsFormatTestCase` to exercise a postings format.
pub struct RandomPostingsTester {
  fields: HashMap<String, BTreeMap<BytesRef<Vec<u8>>, SeedAndOrd>>,
  field_infos: Arc<FieldInfos>,
  current_field_infos: Option<Arc<FieldInfos>>,
  all_terms: Vec<FieldAndTerm>,
  max_doc: i32,
  total_postings: i64,
  total_payload_bytes: i64,
  random: u64,
}

impl RandomPostingsTester {
  pub fn new<R>(random: &mut R) -> Result<Self>
  where
    R: Rng + ?Sized,
  {
    let seed = random.next_u64();
    let mut random = random_from_seed(seed);

    let mut fields: HashMap<String, BTreeMap<BytesRef<Vec<u8>>, SeedAndOrd>> = HashMap::new();
    let num_fields = TestUtil::next_usize(&mut random, 1, 5);

    let mut field_info_array: Vec<Arc<FieldInfo>> = Vec::with_capacity(num_fields);
    let mut max_doc = 0;
    let mut total_postings = 0i64;
    let mut total_payload_bytes = 0i64;

    for field_upto in 0..num_fields {
      let field = loop {
        let field = TestUtil::random_simple_string(&mut random);
        if !fields.contains_key(&field) {
          break field;
        }
      };

      field_info_array.push(Arc::new(FieldInfo::new(
        field.clone(),
        field_upto as i32,
        false,
        false,
        true,
        IndexOptions::DocsAndFreqsAndPositionsAndOffsets,
        DocValuesType::None,
        DocValuesSkipIndexType::None,
        -1,
        HashMap::new(),
        0,
        0,
        0,
        0,
        VectorEncoding::FLOAT32(4),
        VectorSimilarityFunction::Euclidean,
        false,
        false,
      )?));

      let mut postings: BTreeMap<BytesRef<Vec<u8>>, SeedAndOrd> = BTreeMap::new();
      let mut seen_terms: HashSet<String> = HashSet::new();

      let num_terms = if random.random_range(0..10) == 7 {
        at_least_usize(&mut random, 50)
      } else {
        TestUtil::next_usize(&mut random, 2, 20)
      };

      while postings.len() < num_terms {
        let term_upto = postings.len();
        let mut term = loop {
          let term = TestUtil::random_simple_string(&mut random);
          if !seen_terms.contains(&term) {
            break term;
          }
        };
        seen_terms.insert(term.clone());

        if is_night_mode() && term_upto == 0 && field_upto == 0 {
          term = format!("big_{term}");
        } else if term_upto == 1 && field_upto == 0 {
          term = format!("medium_{term}");
        } else if random.random_bool(0.5) {
          term = format!("low_{term}");
        } else {
          term = format!("verylow_{term}");
        }

        let term_seed = random.next_u64();
        postings.insert(BytesRef::from(term.clone()), SeedAndOrd::new(term_seed));

        let mut docs_enum = get_seed_postings(&term, term_seed, IndexOptions::Docs, true);
        let mut last_doc = 0;
        let mut doc_count = 0i64;
        loop {
          let doc = docs_enum.next_doc()?;
          if doc == NO_MORE_DOCS {
            break;
          }
          doc_count += 1;
          last_doc = doc;
        }
        max_doc = max_doc.max(last_doc);
        total_postings += doc_count;
        total_payload_bytes += doc_count * docs_enum.payload_size as i64;
      }

      let mut ord = 0i64;
      #[allow(clippy::explicit_counter_loop)]
      for ent in postings.values_mut() {
        ent.ord = ord;
        ord += 1;
      }

      fields.insert(field, postings);
    }

    let field_infos = Arc::new(FieldInfos::new(field_info_array)?);
    max_doc += 1;

    let mut all_terms = Vec::new();
    let mut field_names: Vec<&String> = fields.keys().collect();
    field_names.sort();
    for field in field_names {
      if let Some(field_terms) = fields.get(field) {
        for (ord, term) in field_terms.keys().enumerate() {
          all_terms.push(FieldAndTerm::new(field.clone(), term, ord as i64)?);
        }
      }
    }

    Ok(Self {
      fields,
      field_infos,
      current_field_infos: None,
      all_terms,
      max_doc,
      total_postings,
      total_payload_bytes,
      random: seed,
    })
  }

  pub fn build_index<C, D>(
    &mut self,
    codec: &C,
    dir: Arc<D>,
    max_allowed: IndexOptions,
    allow_payloads: bool,
    always_test_max: bool,
  ) -> Result<<C::PostingsFormat as PostingsFormat>::FieldsProducer<D::IndexInput>>
  where
    C: crate::core::codecs::codec::Codec,
    C::PostingsFormat: PostingsFormat,
    D: Directory,
  {
    let segment_info = SegmentInfo::new(
      Arc::clone(&dir),
      Some(LATEST.clone()),
      Some(LATEST.clone()),
      "_0",
      self.max_doc,
      false,
      false,
      Some(codec::get_default()),
      HashMap::new(),
      StringHelper::random_id(),
      HashMap::new(),
      None,
    )?;

    let values: Vec<IndexOptions> = IndexOptions::values().collect();
    let max_index_option = values
      .iter()
      .position(|v| *v == max_allowed)
      .ok_or_else(|| {
        LuceneError::illegal_argument(format!("unsupported maxAllowed: {max_allowed}"))
      })?;
    if max_index_option == 0 {
      return Err(LuceneError::illegal_argument(
        "maxAllowed must be at least Docs".to_string(),
      ));
    }

    let mut random = random_from_seed(self.random);
    let mut new_field_info_array: Vec<Arc<FieldInfo>> = Vec::with_capacity(self.fields.len());
    for (field_upto, old_field_info) in self.field_infos.iter().enumerate() {
      let index_options = if always_test_max {
        values[max_index_option]
      } else {
        values[TestUtil::next_usize(&mut random, 1, max_index_option)]
      };
      let do_payloads = index_options >= IndexOptions::DocsAndFreqsAndPositions && allow_payloads;

      new_field_info_array.push(Arc::new(FieldInfo::new(
        old_field_info.name.clone(),
        field_upto as i32,
        false,
        false,
        do_payloads,
        index_options,
        DocValuesType::None,
        DocValuesSkipIndexType::None,
        -1,
        HashMap::new(),
        0,
        0,
        0,
        0,
        VectorEncoding::FLOAT32(4),
        VectorSimilarityFunction::Euclidean,
        false,
        false,
      )?));
    }

    let new_field_infos = Arc::new(FieldInfos::new(new_field_info_array)?);
    self.current_field_infos = Some(Arc::clone(&new_field_infos));

    let bytes = self.total_postings * 8 + self.total_payload_bytes;
    let io_context = IOContext::with_flush(FlushInfo::new(self.max_doc, bytes))?;
    let write_state = SegmentWriteState::new(
      get_default_info_stream(),
      dir.as_ref(),
      Arc::clone(&new_field_infos),
      &io_context,
    );

    let mut seed_fields = SeedFields::new(
      self.fields.clone(),
      Arc::clone(&new_field_infos),
      max_allowed,
      allow_payloads,
    );
    {
      let fake_norms = NormsProducerImpl::new(Arc::clone(&new_field_infos), self.max_doc);

      let mut consumer = codec
        .postings_format()
        .fields_consumer(&write_state, &segment_info)?;
      let write_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
          consumer.write(
            &write_state,
            &segment_info,
            &mut seed_fields,
            Some(&fake_norms),
          )
        }));
      let close_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| consumer.close()));
      match write_result {
        Ok(Ok(())) => match close_result {
          Ok(result) => result?,
          Err(payload) => std::panic::resume_unwind(payload),
        },
        Ok(Err(error)) => return Err(error),
        Err(payload) => std::panic::resume_unwind(payload),
      }
    }

    let read_state = SegmentReadState::new(dir.as_ref(), Arc::clone(&new_field_infos), &io_context);
    codec
      .postings_format()
      .fields_producer(&read_state, &segment_info)
  }

  #[allow(clippy::too_many_arguments)]
  pub fn verify_enum<R, TE>(
    &self,
    random: &mut R,
    thread_state: &mut ThreadState<TE::PostingsEnum>,
    field: &str,
    term: &BytesRef<Vec<u8>>,
    terms_enum: &mut TE,
    max_test_options: IndexOptions,
    max_index_options: IndexOptions,
    options: &std::collections::HashSet<Option_>,
    always_test_max: bool,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
    TE: TermsEnum,
    TE::PostingsEnum: PostingsEnum,
  {
    let verbose = cfg!(feature = "test_log_verbose");

    if verbose {
      println!("  verifyEnum: options={options:?} maxTestOptions={max_test_options:?}");
    }

    assert_eq!(term.as_byte_slice(), terms_enum.term()?.as_bytes());

    let field_infos = self.current_field_infos.as_ref().unwrap();
    let field_info = field_infos.field_info_by_name(field)?.unwrap();

    let mut expected = get_seed_postings(
      &term.utf8_to_string()?,
      self
        .fields
        .get(field)
        .and_then(|terms| terms.get(term))
        .ok_or_else(|| {
          LuceneError::illegal_state(format!(
            "missing seed postings for field={field} term={term}"
          ))
        })?
        .seed,
      max_index_options,
      true,
    );
    assert_eq!(expected.doc_freq, terms_enum.doc_freq()?);

    let allow_freqs = *field_info.get_index_options() >= IndexOptions::DocsAndFreqs
      && max_test_options >= IndexOptions::DocsAndFreqs;
    let do_check_freqs = allow_freqs && (always_test_max || random.random_range(0..3) <= 2);

    let allow_positions = *field_info.get_index_options() >= IndexOptions::DocsAndFreqsAndPositions
      && max_test_options >= IndexOptions::DocsAndFreqsAndPositions;
    let do_check_positions = allow_positions && (always_test_max || random.random_range(0..3) <= 2);

    let allow_offsets = *field_info.get_index_options()
      >= IndexOptions::DocsAndFreqsAndPositionsAndOffsets
      && max_test_options >= IndexOptions::DocsAndFreqsAndPositionsAndOffsets;
    let do_check_offsets = allow_offsets && (always_test_max || random.random_range(0..3) <= 2);

    let do_check_payloads = options.contains(&Option_::Payloads)
      && allow_positions
      && field_info.has_payloads()
      && (always_test_max || random.random_range(0..3) <= 2);

    let prev_postings_enum =
      if options.contains(&Option_::ReuseEnums) && random.random_range(0..10) < 9 {
        thread_state.reuse_postings_enum.take()
      } else {
        None
      };

    let postings_enum = if !do_check_positions {
      if allow_positions && random.random_range(0..10) == 7 {
        let mut flags = POSITIONS as i32;
        if always_test_max || random.random_bool(0.5) {
          flags |= OFFSETS as i32;
        }
        if always_test_max || random.random_bool(0.5) {
          flags |= PAYLOADS as i32;
        }
        if verbose {
          println!("  get DocsEnum (but we won't check positions) flags={flags}");
        }
        let postings_enum = terms_enum.postings_with_flags(prev_postings_enum, flags)?;
        thread_state.reuse_postings_enum = Some(postings_enum);
        thread_state
          .reuse_postings_enum
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("missing reused postings enum"))?
      } else {
        if verbose {
          println!("  get DocsEnum");
        }
        let flags = if do_check_freqs {
          FREQS as i32
        } else {
          NONE as i32
        };
        let postings_enum = terms_enum.postings_with_flags(prev_postings_enum, flags)?;
        thread_state.reuse_postings_enum = Some(postings_enum);
        thread_state
          .reuse_postings_enum
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("missing reused postings enum"))?
      }
    } else {
      let mut flags = POSITIONS as i32;
      if always_test_max || do_check_offsets || random.random_range(0..3) == 1 {
        flags |= OFFSETS as i32;
      }
      if always_test_max || do_check_payloads || random.random_range(0..3) == 1 {
        flags |= PAYLOADS as i32;
      }
      if verbose {
        println!("  get DocsEnum flags={flags}");
      }
      let postings_enum = terms_enum.postings_with_flags(prev_postings_enum, flags)?;
      thread_state.reuse_postings_enum = Some(postings_enum);
      thread_state
        .reuse_postings_enum
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("missing reused postings enum"))?
    };

    assert_eq!(-1, postings_enum.doc_id());

    let stop_at = if !always_test_max
      && options.contains(&Option_::PartialDocConsume)
      && expected.doc_freq > 1
      && random.random_range(0..10) == 7
    {
      let stop_at = random.random_range(0..(expected.doc_freq - 1));
      if verbose {
        println!(
          "  will not consume all docs ({stop_at} vs {})",
          expected.doc_freq
        );
      }
      stop_at
    } else {
      if verbose {
        println!("  consume all docs");
      }
      expected.doc_freq
    };

    let skip_chance: f64 = if always_test_max {
      0.5
    } else {
      random.random::<f64>()
    };
    let num_skips = if expected.doc_freq < 3 {
      1
    } else {
      TestUtil::next_int(random, 1, std::cmp::min(20, expected.doc_freq / 3))
    };
    let skip_inc = expected.doc_freq / num_skips;
    let skip_doc_inc = self.max_doc / num_skips;
    let do_all_skipping = options.contains(&Option_::Skipping) && random.random_range(0..7) == 1;

    let freq_ask_chance: f64 = if always_test_max {
      1.0
    } else {
      random.random::<f64>()
    };
    let payload_check_chance: f64 = if always_test_max {
      1.0
    } else {
      random.random::<f64>()
    };
    let offset_check_chance: f64 = if always_test_max {
      1.0
    } else {
      random.random::<f64>()
    };

    if verbose {
      if options.contains(&Option_::Skipping) {
        println!("  skipChance={skip_chance} numSkips={num_skips}");
      } else {
        println!("  no skipping");
      }
      if do_check_freqs {
        println!("  freqAskChance={freq_ask_chance}");
      }
      if do_check_payloads {
        println!("  payloadCheckChance={payload_check_chance}");
      }
      if do_check_offsets {
        println!("  offsetCheckChance={offset_check_chance}");
      }
    }

    while expected.upto <= stop_at {
      if expected.upto == stop_at {
        if stop_at == expected.doc_freq {
          assert_eq!(
            NO_MORE_DOCS,
            postings_enum.next_doc()?,
            "DocsEnum should have ended but didn't"
          );
          assert_eq!(
            NO_MORE_DOCS,
            postings_enum.doc_id(),
            "DocsEnum should have ended but didn't"
          );
        }
        break;
      }

      if options.contains(&Option_::Skipping)
        && (do_all_skipping || random.random::<f64>() <= skip_chance)
      {
        let mut target_doc_id = -1;
        if expected.upto < stop_at && random.random_bool(0.5) {
          let skip_count = TestUtil::next_int(random, 1, skip_inc);
          for _ in 0..skip_count {
            if expected.next_doc()? == NO_MORE_DOCS {
              break;
            }
          }
        } else {
          let skip_doc_ids = TestUtil::next_int(random, 1, skip_doc_inc);
          if skip_doc_ids > 0 {
            target_doc_id = expected.doc_id() + skip_doc_ids;
            expected.advance(target_doc_id)?;
          }
        }

        if expected.upto >= stop_at {
          let target = if random.random_bool(0.5) {
            self.max_doc
          } else {
            NO_MORE_DOCS
          };
          if verbose {
            println!("  now advance to end (target={target})");
          }
          assert_eq!(
            NO_MORE_DOCS,
            postings_enum.advance(target)?,
            "DocsEnum should have ended but didn't"
          );
          break;
        } else {
          if verbose {
            if target_doc_id != -1 {
              println!(
                "  now advance to random target={target_doc_id} ({} of {}) current={}",
                expected.upto,
                stop_at,
                postings_enum.doc_id()
              )
            } else {
              println!(
                "  now advance to known-exists target={} ({} of {}) current={}",
                expected.doc_id(),
                expected.upto,
                stop_at,
                postings_enum.doc_id()
              )
            }
          }
          let v = if target_doc_id != -1 {
            target_doc_id
          } else {
            expected.doc_id()
          };
          let doc_id = postings_enum.advance(v)?;
          assert_eq!(expected.doc_id(), doc_id);
        }
      } else {
        expected.next_doc()?;
        if verbose {
          println!(
            "  now nextDoc to {} ({} of {})",
            expected.doc_id(),
            expected.upto,
            stop_at
          );
        }
        let doc_id = postings_enum.next_doc()?;
        assert_eq!(expected.doc_id(), doc_id);
        if doc_id == NO_MORE_DOCS {
          break;
        }
      }

      if do_check_freqs && random.random::<f64>() <= freq_ask_chance {
        if verbose {
          println!("    now freq()={}", expected.freq()?);
        }
        let expected_freq = expected.freq()?;
        assert_eq!(expected_freq, postings_enum.freq()?, "freq is wrong");
      }

      if do_check_positions {
        let freq = postings_enum.freq()?;
        let num_pos_to_consume = if !always_test_max
          && options.contains(&Option_::PartialPosConsume)
          && random.random_range(0..5) == 1
        {
          random.random_range(0..freq)
        } else {
          freq
        };

        for _ in 0..num_pos_to_consume {
          let pos = expected.next_position()?;
          if verbose {
            println!("    now nextPosition to {pos}");
          }
          assert_eq!(pos, postings_enum.next_position()?);

          if do_check_payloads {
            let expected_payload = expected.get_payload()?;
            let expected_payload = expected_payload.as_ref().map(BytesRefValue::as_bytes_ref);
            if random.random::<f64>() <= payload_check_chance {
              if verbose {
                println!(
                  "      now check expectedPayload length={}",
                  expected_payload.as_ref().map_or(0, |p| p.length)
                );
              }
              match expected_payload {
                None => {
                  assert!(
                    postings_enum.get_payload()?.is_none(),
                    "should not have payload"
                  );
                },
                Some(expected_payload) if expected_payload.length == 0 => {
                  assert!(
                    postings_enum.get_payload()?.is_none(),
                    "should not have payload"
                  );
                },
                Some(expected_payload) => {
                  let payload = postings_enum.get_payload()?;
                  let payload = payload
                    .as_ref()
                    .ok_or_else(|| LuceneError::illegal_state("should have payload but doesn't"))?;
                  let payload = payload.as_bytes_ref();
                  assert_eq!(expected_payload.length, payload.length,);
                  for byte_upto in 0..expected_payload.length {
                    assert_eq!(
                      expected_payload.bytes[expected_payload.offset + byte_upto],
                      payload.bytes[payload.offset + byte_upto],
                      "payload bytes are wrong"
                    );
                  }

                  let payload_copy = payload.as_bytes().to_vec();
                  assert_eq!(
                    payload_copy,
                    postings_enum
                      .get_payload()?
                      .ok_or_else(|| LuceneError::illegal_state("missing payload"))?
                      .as_bytes(),
                    "2nd call to getPayload returns something different!"
                  );
                },
              }
            } else if verbose {
              println!(
                "      skip check payload length={}",
                expected_payload.as_ref().map_or(0, |p| p.length)
              );
            }
          }

          if do_check_offsets {
            if random.random::<f64>() <= offset_check_chance {
              if verbose {
                println!(
                  "      now check offsets: startOff={} endOffset={}",
                  expected.start_offset()?,
                  expected.end_offset()?
                );
              }
              let expected_start = expected.start_offset()?;
              let expected_end = expected.end_offset()?;
              assert_eq!(
                expected_start,
                postings_enum.start_offset()?,
                "startOffset is wrong"
              );
              assert_eq!(
                expected_end,
                postings_enum.end_offset()?,
                "endOffset is wrong"
              );
            } else if verbose {
              println!("      skip check offsets");
            }
          } else if *field_info.get_index_options()
            < IndexOptions::DocsAndFreqsAndPositionsAndOffsets
          {
            if verbose {
              println!("      now check offsets are -1");
            }
            assert_eq!(-1, postings_enum.start_offset()?);
            assert_eq!(-1, postings_enum.end_offset()?);
          }
        }
      }
    }

    if options.contains(&Option_::Skipping) {
      let doc_to_norm = |doc: i32| -> i64 {
        if field_info.has_norms() {
          (1 + (doc & 0x0f)) as i64
        } else {
          1
        }
      };

      let mut max = -1;
      let mut impacts_copy: Option<Vec<Impact>> = None;
      let flags = if do_check_positions {
        let mut flags = POSITIONS as i32;
        if do_check_offsets {
          flags |= OFFSETS as i32;
        }
        if do_check_payloads {
          flags |= PAYLOADS as i32;
        }
        flags
      } else {
        FREQS as i32
      };
      let mut impacts_enum = terms_enum.impacts(flags)?;
      let mut postings = terms_enum.postings_with_flags(None, flags)?;

      loop {
        let doc = impacts_enum.next_doc()?;
        assert_eq!(postings.next_doc()?, doc);
        if doc == NO_MORE_DOCS {
          break;
        }
        let freq = postings.freq()?;
        let impacts_freq = impacts_enum.freq()?;
        assert_eq!(freq, impacts_freq, "freq is wrong");
        for _ in 0..freq {
          let pos = postings.next_position()?;
          assert_eq!(pos, impacts_enum.next_position()?);
          if do_check_offsets {
            assert_eq!(postings.start_offset()?, impacts_enum.start_offset()?,);
            assert_eq!(postings.end_offset()?, impacts_enum.end_offset()?,);
          }
          if do_check_payloads {
            assert_eq!(
              postings.get_payload()?.map(BytesRefValue::into_owned),
              impacts_enum.get_payload()?.map(BytesRefValue::into_owned),
            );
          }
        }

        if doc > max {
          impacts_enum.advance_shallow(doc)?;
          let impacts = impacts_enum.get_impacts()?;
          IndexPackageAccessImpl.check_impacts(&impacts, doc)?;
          let impacts_vec = impacts.get_impacts(0)?;
          impacts_copy = Some(impacts_vec);
        }
        let freq = impacts_enum.freq()?;
        let norm = doc_to_norm(doc);
        let impacts_copy_ref = impacts_copy
          .as_ref()
          .ok_or_else(|| LuceneError::illegal_state("impacts copy should have been initialized"))?;
        let idx = match impacts_copy_ref.binary_search_by(|impact| impact.freq.cmp(&freq)) {
          Ok(idx) | Err(idx) => idx,
        };
        assert!(
          idx < impacts_copy_ref.len() && impacts_copy_ref[idx].norm <= norm,
          "Got {} in postings, but no impact triggers equal or better scores in {:?}",
          Impact::new(freq, norm),
          impacts_copy_ref
        );
      }

      impacts_enum = terms_enum.impacts(flags)?;
      postings = terms_enum.postings_with_flags(Some(postings), flags)?;

      max = -1;
      loop {
        let doc = impacts_enum.doc_id();
        let (advance, target) = if random.random_bool(0.5) {
          (false, doc + 1)
        } else {
          let delta = std::cmp::min(
            1 + random.random_range(0..512),
            NO_MORE_DOCS.wrapping_sub(doc),
          );
          (true, doc.wrapping_add(delta))
        };

        if target > max && random.random_bool(0.5) {
          let delta = std::cmp::min(
            random.random_range(0..512),
            NO_MORE_DOCS.wrapping_sub(target),
          );
          max = target.wrapping_add(delta);

          impacts_enum.advance_shallow(target)?;
          let impacts = impacts_enum.get_impacts()?;
          IndexPackageAccessImpl.check_impacts(&impacts, target)?;
          impacts_copy = Some(vec![Impact::new(i32::MAX, 1_i64)]);
          for level in 0..impacts.num_levels() {
            if impacts.get_doc_id_upto(level) >= max {
              impacts_copy = Some(
                impacts
                  .get_impacts(level)?
                  .iter()
                  .map(|i| Impact::new(i.freq, i.norm))
                  .collect(),
              );
              break;
            }
          }
        }

        let doc = if advance {
          impacts_enum.advance(target)?
        } else {
          impacts_enum.next_doc()?
        };

        assert_eq!(postings.advance(target)?, doc);
        if doc == NO_MORE_DOCS {
          break;
        }

        let freq = postings.freq()?;
        let impacts_freq = impacts_enum.freq()?;
        assert_eq!(freq, impacts_freq, "freq is wrong");
        for _ in 0..postings.freq()? {
          let pos = postings.next_position()?;
          assert_eq!(pos, impacts_enum.next_position()?);
          if do_check_offsets {
            assert_eq!(postings.start_offset()?, impacts_enum.start_offset()?,);
            assert_eq!(postings.end_offset()?, impacts_enum.end_offset()?,);
          }
          if do_check_payloads {
            assert_eq!(
              postings.get_payload()?.map(BytesRefValue::into_owned),
              impacts_enum.get_payload()?.map(BytesRefValue::into_owned),
            );
          }
        }

        if doc > max {
          let delta = std::cmp::min(
            1 + random.random_range(0..512),
            NO_MORE_DOCS.wrapping_sub(doc),
          );
          max = doc.wrapping_add(delta);
          let impacts = impacts_enum.get_impacts()?;
          IndexPackageAccessImpl.check_impacts(&impacts, doc)?;
          impacts_copy = Some(vec![Impact::new(i32::MAX, 1_i64)]);
          for level in 0..impacts.num_levels() {
            if impacts.get_doc_id_upto(level) >= max {
              impacts_copy = Some(
                impacts
                  .get_impacts(level)?
                  .iter()
                  .map(|i| Impact::new(i.freq, i.norm))
                  .collect(),
              );
              break;
            }
          }
        }

        let freq = impacts_enum.freq()?;
        let norm = doc_to_norm(doc);
        let impacts_copy = impacts_copy
          .as_ref()
          .ok_or_else(|| LuceneError::illegal_state("impacts copy should have been initialized"))?;
        let idx = match impacts_copy.binary_search_by(|impact| impact.freq.cmp(&freq)) {
          Ok(idx) | Err(idx) => idx,
        };
        assert!(
          idx < impacts_copy.len() && impacts_copy[idx].norm <= norm,
          "Got {} in postings, but no impact triggers equal or better scores in {:?}",
          Impact::new(freq, norm),
          impacts_copy
        );
      }
    }

    Ok(())
  }
}
#[derive(Clone)]
struct SeedAndOrd {
  seed: u64,
  ord: i64,
}

impl SeedAndOrd {
  fn new(seed: u64) -> Self {
    Self { seed, ord: 0 }
  }
}

pub struct SeedFields {
  fields: HashMap<String, BTreeMap<BytesRef<Vec<u8>>, SeedAndOrd>>,
  field_infos: Arc<FieldInfos>,
  max_allowed: IndexOptions,
  allow_payloads: bool,
  keys: Vec<String>,
}

impl SeedFields {
  fn new(
    fields: HashMap<String, BTreeMap<BytesRef<Vec<u8>>, SeedAndOrd>>,
    field_infos: Arc<FieldInfos>,
    max_allowed: IndexOptions,
    allow_payloads: bool,
  ) -> Self {
    let mut keys = fields.keys().cloned().collect::<Vec<_>>();
    keys.sort();
    Self {
      fields,
      field_infos,
      max_allowed,
      allow_payloads,
      keys,
    }
  }
}

impl Fields for SeedFields {
  type FieldIter<'a>
    = VecIter<'a, String>
  where
    Self: 'a;

  fn iterator(&self) -> Result<Self::FieldIter<'_>> {
    Ok(self.keys.iter_ext())
  }

  type Terms = SeedTerms;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    match self.fields.get(field) {
      Some(terms) => {
        let field_info = self.field_infos.field_info_by_name(field)?.ok_or_else(|| {
          LuceneError::illegal_state(format!("missing FieldInfo for field {field}"))
        })?;
        Ok(Some(SeedTerms::new(
          terms.clone(),
          field_info.clone(),
          self.max_allowed,
          self.allow_payloads,
        )))
      },
      None => Ok(None),
    }
  }

  fn size(&self) -> Result<i32> {
    Ok(self.fields.len() as i32)
  }
}

pub struct SeedTerms {
  terms: BTreeMap<BytesRef<Vec<u8>>, SeedAndOrd>,
  field_info: Arc<FieldInfo>,
  max_allowed: IndexOptions,
  allow_payloads: bool,
}

impl SeedTerms {
  fn new(
    terms: BTreeMap<BytesRef<Vec<u8>>, SeedAndOrd>,
    field_info: Arc<FieldInfo>,
    max_allowed: IndexOptions,
    allow_payloads: bool,
  ) -> Self {
    Self {
      terms,
      field_info,
      max_allowed,
      allow_payloads,
    }
  }
}

impl Terms for SeedTerms {
  type TermsEnum = SeedTermsEnum;
  type IntersectIter = FilteredTermsEnum<Self::TermsEnum, AutomatonTermsEnum>;

  fn iterator(&self) -> Result<Self::TermsEnum> {
    let mut terms_enum =
      SeedTermsEnum::new(self.terms.clone(), self.max_allowed, self.allow_payloads);
    terms_enum.reset();
    Ok(terms_enum)
  }

  fn intersect(
    &self,
    compiled: &CompiledAutomaton,
    start_term: Option<&BytesRef<Vec<u8>>>,
  ) -> Result<Self::IntersectIter> {
    self.default_intersect(compiled, start_term)
  }

  fn size(&self) -> Result<i64> {
    Ok(self.terms.len() as i64)
  }

  fn get_sum_total_term_freq(&self) -> Result<i64> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_sum_doc_freq(&self) -> Result<i64> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_doc_count(&self) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn has_freqs(&self) -> bool {
    *self.field_info.get_index_options() >= IndexOptions::DocsAndFreqs
  }

  fn has_offsets(&self) -> bool {
    *self.field_info.get_index_options() >= IndexOptions::DocsAndFreqsAndPositionsAndOffsets
  }

  fn has_positions(&self) -> bool {
    *self.field_info.get_index_options() >= IndexOptions::DocsAndFreqsAndPositions
  }

  fn has_payloads(&self) -> bool {
    self.allow_payloads && self.field_info.has_payloads()
  }
}

pub struct SeedPostings {
  // Used only to generate docIDs; this way if you pull w/
  // or w/o positions you get the same docID sequence:
  doc_random: StdRng,
  random: StdRng,
  pub doc_freq: i32,
  max_doc_spacing: i32,
  payload_size: usize,
  fixed_payloads: bool,
  payload: BytesRef<Vec<u8>>,
  do_positions: bool,
  allow_payloads: bool,

  doc_id: i32,
  freq: i32,
  pub upto: i32,

  pos: i32,
  offset: i32,
  start_offset: i32,
  end_offset: i32,
  pos_spacing: i32,
  pos_upto: i32,
}

impl SeedPostings {
  pub fn new(
    seed: u64,
    min_doc_freq: i32,
    max_doc_freq: i32,
    options: IndexOptions,
    allow_payloads: bool,
  ) -> Self {
    let mut random = random_from_seed(seed);
    let doc_random_seed = random.next_u64();
    let doc_random = random_from_seed(doc_random_seed);
    let doc_freq = TestUtil::next_int(&mut random, min_doc_freq, max_doc_freq);
    let max_doc_spacing = TestUtil::next_int(&mut random, 1, 100);

    let payload_size = if random.random_range(0..10) == 7 {
      1 + random.random_range(0..3)
    } else {
      1 + random.random_range(0..1)
    };

    let fixed_payloads = random.random_bool(0.5);
    let payload_bytes = vec![0u8; payload_size];
    let payload = BytesRef::from_bytes(payload_bytes);
    let do_positions = IndexOptions::DocsAndFreqsAndPositions
      .cmp(&options)
      .to_int()
      <= 0;

    Self {
      doc_random,
      random,
      doc_freq,
      max_doc_spacing,
      payload_size,
      fixed_payloads,
      payload,
      do_positions,
      allow_payloads,
      doc_id: -1,
      freq: 0,
      upto: 0,
      pos: 0,
      offset: 0,
      start_offset: 0,
      end_offset: 0,
      pos_spacing: 0,
      pos_upto: 0,
    }
  }
}

impl SeedPostings {
  fn _next_doc(&mut self) -> Result<i32> {
    if self.doc_id == -1 {
      self.doc_id = 0;
    }

    while self.pos_upto < self.freq {
      self.next_position()?;
    }

    if self.upto < self.doc_freq {
      if self.upto == 0 && self.doc_random.random_bool(0.5) {
      } else if self.max_doc_spacing == 1 {
        self.doc_id += 1;
      } else {
        self.doc_id += TestUtil::next_int(&mut self.doc_random, 1, self.max_doc_spacing);
      }

      if self.random.random_range(0..200) == 17 {
        self.freq = TestUtil::next_int(&mut self.random, 1, 1000);
      } else if self.random.random_range(0..10) == 17 {
        self.freq = TestUtil::next_int(&mut self.random, 1, 20);
      } else {
        self.freq = TestUtil::next_int(&mut self.random, 1, 4);
      }

      self.pos = 0;
      self.offset = 0;
      self.pos_upto = 0;
      self.pos_spacing = TestUtil::next_int(&mut self.random, 1, 100);

      self.upto += 1;
      Ok(self.doc_id)
    } else {
      self.doc_id = NO_MORE_DOCS;
      Ok(self.doc_id)
    }
  }
}

impl crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions for SeedPostings {}
impl crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for SeedPostings {}

impl DocIdSetIterator for SeedPostings {
  fn doc_id(&self) -> i32 {
    self.doc_id
  }

  fn next_doc(&mut self) -> Result<i32> {
    self._next_doc()?;
    Ok(self.doc_id)
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.slow_advance(target)
  }

  fn cost(&self) -> Result<i64> {
    Ok(self.doc_freq as i64)
  }
}

impl PostingsEnum for SeedPostings {
  fn freq(&mut self) -> Result<i32> {
    Ok(self.freq)
  }

  fn next_position(&mut self) -> Result<i32> {
    if !self.do_positions {
      self.pos_upto = self.freq;
      return Ok(-1);
    }

    assert!(self.pos_upto < self.freq);

    if self.pos_upto == 0 && self.random.random_bool(0.5) {
    } else if self.pos_spacing == 1 {
      self.pos += 1;
    } else {
      self.pos += TestUtil::next_int(&mut self.random, 1, self.pos_spacing);
    }

    if self.payload_size != 0 {
      if self.fixed_payloads {
        self.payload.length = self.payload_size;
        self.random.fill_bytes(&mut self.payload.bytes);
      } else {
        let this_payload_size = self.random.random_range(0..self.payload_size);
        if this_payload_size != 0 {
          self.payload.length = self.payload_size;
          self.random.fill_bytes(&mut self.payload.bytes);
        } else {
          self.payload.length = 0;
        }
      }
    } else {
      self.payload.length = 0;
    }

    if !self.allow_payloads {
      self.payload.length = 0;
    }

    self.start_offset = self.offset + self.random.random_range(0..5);
    self.end_offset = self.start_offset + self.random.random_range(0..10);
    self.offset = self.end_offset;

    self.pos_upto += 1;
    Ok(self.pos)
  }

  fn start_offset(&self) -> Result<i32> {
    Ok(self.start_offset)
  }

  fn end_offset(&self) -> Result<i32> {
    Ok(self.end_offset)
  }

  fn get_payload(&self) -> Result<std::option::Option<BytesRefValueEnum<'_>>> {
    if self.payload.length == 0 {
      Ok(None)
    } else {
      Ok(Some(Cow::Borrowed(&self.payload).into_value()))
    }
  }
}
/// Holds one field, term and ord.
#[derive(Clone)]
pub struct FieldAndTerm {
  field: String,
  term: BytesRef<Vec<u8>>,
  ord: i64,
}

impl FieldAndTerm {
  pub fn new(field: String, term: &BytesRef<Vec<u8>>, ord: i64) -> Result<Self> {
    Ok(Self {
      field,
      term: BytesRef::deep_copy_of(term)?,
      ord,
    })
  }

  pub fn field(&self) -> &str {
    &self.field
  }

  pub fn term(&self) -> &BytesRef<Vec<u8>> {
    &self.term
  }
}

impl RandomPostingsTester {
  pub fn all_terms(&self) -> &[FieldAndTerm] {
    &self.all_terms
  }
}

pub struct SeedTermsEnum {
  terms: Vec<(BytesRef<Vec<u8>>, SeedAndOrd)>,
  max_allowed: IndexOptions,
  allow_payloads: bool,
  next_index: usize,
  current_index: Option<usize>,
}
impl SeedTermsEnum {
  fn new(
    terms: BTreeMap<BytesRef<Vec<u8>>, SeedAndOrd>,
    max_allowed: IndexOptions,
    allow_payloads: bool,
  ) -> Self {
    Self {
      terms: terms.into_iter().collect(),
      max_allowed,
      allow_payloads,
      next_index: 0,
      current_index: None,
    }
  }

  fn reset(&mut self) {
    self.next_index = 0;
    self.current_index = None;
  }

  fn current_entry(&self) -> Result<&(BytesRef<Vec<u8>>, SeedAndOrd)> {
    let idx = self
      .current_index
      .ok_or_else(|| LuceneError::illegal_state("this terms enum is unpositioned"))?;
    Ok(&self.terms[idx])
  }

  fn seek_to_index(&mut self, index: usize) {
    self.current_index = Some(index);
    self.next_index = index + 1;
  }
}

impl BytesRefIterator for SeedTermsEnum {
  type Value<'a>
    = Cow<'a, BytesRef<Vec<u8>>>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
    if self.next_index >= self.terms.len() {
      self.current_index = None;
      return Ok(None);
    }
    self.current_index = Some(self.next_index);
    self.next_index += 1;
    Ok(Some(Cow::Borrowed(
      &self.terms[self.current_index.unwrap()].0,
    )))
  }
}

impl TermsEnum for SeedTermsEnum {
  type AttributeSource<'a>
    = &'a DummyAttributeSource
  where
    Self: 'a;
  type AttributeSourceMut<'a>
    = &'a mut DummyAttributeSource
  where
    Self: 'a;

  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn seek_exact<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<bool> {
    Ok(self.seek_ceil(term)? == SeekStatus::Found)
  }

  type PreparedSeek<BS: ByteSource> = crate::core::index::terms_enum::SeekExact;

  fn prepare_seek_exact<BS: ByteSource>(
    &mut self,
    _text: &BytesRef<BS>,
  ) -> Result<Option<Self::PreparedSeek<BS>>> {
    Ok(Some(crate::core::index::terms_enum::SeekExact))
  }

  fn seek_ceil<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<SeekStatus> {
    match self.terms.binary_search_by(|(t, _)| t.compare_to(term)) {
      Ok(index) => {
        self.seek_to_index(index);
        Ok(SeekStatus::Found)
      },
      Err(index) => {
        if index >= self.terms.len() {
          self.current_index = None;
          self.next_index = self.terms.len();
          Ok(SeekStatus::End)
        } else {
          self.seek_to_index(index);
          Ok(SeekStatus::NotFound)
        }
      },
    }
  }

  fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
    let index = self
      .terms
      .iter()
      .position(|(_, seed_and_ord)| seed_and_ord.ord == ord)
      .ok_or_else(|| LuceneError::illegal_argument(format!("ord= {ord} does not exist")))?;
    self.seek_to_index(index);
    Ok(())
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    term: &BytesRef<BS>,
    state: &TermStateEnum,
  ) -> Result<()> {
    let _ = state;
    if !self.seek_exact(term)? {
      return Err(LuceneError::illegal_argument(format!(
        "term= {term} does not exist"
      )));
    }
    Ok(())
  }

  fn term(&self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
    Ok(Cow::Borrowed(&self.current_entry()?.0))
  }

  fn ord(&self) -> Result<i64> {
    Ok(self.current_entry()?.1.ord)
  }

  fn doc_freq(&mut self) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    Err(LuceneError::unsupported_operation(""))
  }

  type PostingsEnum = SeedPostings;

  fn postings_with_flags(
    &mut self,
    _reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    if feature_requested(flags, POSITIONS) {
      if self
        .max_allowed
        .cmp(&IndexOptions::DocsAndFreqsAndPositions)
        .is_lt()
      {
        return Err(LuceneError::unsupported_operation(""));
      }
      if feature_requested(flags, OFFSETS)
        && self
          .max_allowed
          .cmp(&IndexOptions::DocsAndFreqsAndPositionsAndOffsets)
          .is_lt()
      {
        return Err(LuceneError::unsupported_operation(""));
      }
      if feature_requested(flags, PAYLOADS) && !self.allow_payloads {
        return Err(LuceneError::unsupported_operation(""));
      }
    }
    if feature_requested(flags, FREQS) && self.max_allowed.cmp(&IndexOptions::DocsAndFreqs).is_lt()
    {
      return Err(LuceneError::unsupported_operation(""));
    }

    let (term, seed_and_ord) = self.current_entry()?;
    Ok(get_seed_postings(
      &term.utf8_to_string()?,
      seed_and_ord.seed,
      self.max_allowed,
      self.allow_payloads,
    ))
  }

  type ImpactsEnum = DummyImpactsEnum;

  fn impacts(&mut self, _flags: i32) -> Result<Self::ImpactsEnum> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    Ok(crate::core::index::base_terms_enum::BaseTermsEnumTermStateImpl.into())
  }
}

pub fn get_seed_postings(
  term: &str,
  seed: u64,
  options: IndexOptions,
  allow_payloads: bool,
) -> SeedPostings {
  let random_multiplier = random_multiplier();
  let (min_doc_freq, max_doc_freq) = if term.starts_with("big_") {
    (random_multiplier * 50000, random_multiplier * 70000)
  } else if term.starts_with("medium_") {
    (random_multiplier * 3000, random_multiplier * 6000)
  } else if term.starts_with("low_") {
    (random_multiplier, random_multiplier * 40)
  } else {
    (1, 3)
  };

  SeedPostings::new(seed, min_doc_freq, max_doc_freq, options, allow_payloads)
}

struct NormsProducerImpl {
  new_field_infos: Arc<FieldInfos>,
  max_doc: i32,
}
impl NormsProducerImpl {
  fn new(field_infos: Arc<FieldInfos>, max_doc: i32) -> Self {
    Self {
      new_field_infos: field_infos,
      max_doc,
    }
  }
}

impl CloseableRef for NormsProducerImpl {}

impl NormsProducer for NormsProducerImpl {
  type MergeInstance = Self;
  type NumericDocValues = NumericDocValuesImpl;

  fn get_norms(&self, field: &Arc<FieldInfo>) -> Result<Self::NumericDocValues> {
    let field_info = self
      .new_field_infos
      .field_info_by_number(field.number)?
      .unwrap();
    assert!(field_info.has_norms());
    let max_doc = self.max_doc;
    Ok(NumericDocValuesImpl::new(max_doc))
  }

  fn check_integrity(&self) -> Result<()> {
    Ok(())
  }
}
struct NumericDocValuesImpl {
  max_doc: i32,
  doc: i32,
}
impl NumericDocValuesImpl {
  fn new(max_doc: i32) -> Self {
    Self { max_doc, doc: -1 }
  }
}

impl DocValuesIterator for NumericDocValuesImpl {
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    self.doc = target;
    Ok(true)
  }
}

impl crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions for NumericDocValuesImpl {}
impl crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for NumericDocValuesImpl {}

impl DocIdSetIterator for NumericDocValuesImpl {
  fn doc_id(&self) -> i32 {
    self.doc
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.doc += 1;
    if self.doc == self.max_doc {
      self.doc = NO_MORE_DOCS;
    }
    Ok(self.doc)
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.doc = if target >= self.max_doc {
      NO_MORE_DOCS
    } else {
      target
    };
    Ok(self.doc)
  }

  fn cost(&self) -> Result<i64> {
    Ok(self.max_doc as i64)
  }
}

impl NumericDocValues for NumericDocValuesImpl {
  fn long_value(&mut self) -> Result<i64> {
    Ok(DocToNorm.apply_as_long(self.doc))
  }
}

impl RandomPostingsTester {
  pub fn test_terms<F, R>(
    &self,
    random: &mut R,
    fields_source: &F,
    options: &HashSet<Option_>,
    max_test_options: IndexOptions,
    max_index_options: IndexOptions,
    always_test_max: bool,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
    F: Fields + Sync,
  {
    if options.contains(&Option_::Threads) {
      let num_threads = if is_night_mode() {
        TestUtil::next_int(random, 2, 5)
      } else {
        2
      };
      let seeds: Vec<u64> = (0..num_threads).map(|_| random.next_u64()).collect();
      thread::scope(|scope| {
        for seed in seeds {
          scope.spawn(move || {
            let mut thread_random = random_from_seed(seed);
            if let Err(err) = self.test_terms_one_thread(
              &mut thread_random,
              fields_source,
              options,
              max_test_options,
              max_index_options,
              always_test_max,
            ) {
              panic!("{err}");
            }
          });
        }
      });
      Ok(())
    } else {
      self.test_terms_one_thread(
        random,
        fields_source,
        options,
        max_test_options,
        max_index_options,
        always_test_max,
      )
    }
  }

  fn test_terms_one_thread<F, R>(
    &self,
    random: &mut R,
    fields_source: &F,
    options: &HashSet<Option_>,
    max_test_options: IndexOptions,
    max_index_options: IndexOptions,
    always_test_max: bool,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
    F: Fields + Sync,
  {
    let mut thread_state = ThreadState::default();
    let mut term_states: Vec<TermStateEnum> = Vec::new();
    let mut term_state_terms: Vec<&FieldAndTerm> = Vec::new();
    let mut supports_ords = true;

    let mut all_terms: Vec<&FieldAndTerm> = self.all_terms.iter().collect();
    all_terms.shuffle(random);

    let mut upto = 0usize;
    while upto < all_terms.len() {
      let use_term_state = !term_states.is_empty() && random.random_range(0..5) == 1;
      let use_term_ord = supports_ords && !use_term_state && random.random_range(0..5) == 1;

      let (field_and_term, term_state) = if !use_term_state {
        let field_and_term = all_terms[upto];
        upto += 1;
        if cfg!(feature = "test_log_verbose") {
          if use_term_ord {
            println!(
              "\nTEST: seek to term={}:{} using ord={}",
              field_and_term.field,
              field_and_term.term.utf8_to_string()?,
              field_and_term.ord
            );
          } else {
            println!(
              "\nTEST: seek to term={}:{}",
              field_and_term.field,
              field_and_term.term.utf8_to_string()?
            );
          }
        }
        (field_and_term, None)
      } else {
        let idx = random.random_range(0..term_states.len());
        let field_and_term = term_state_terms[idx];
        if cfg!(feature = "test_log_verbose") {
          println!(
            "\nTEST: seek using TermState to term={}:{}",
            field_and_term.field,
            field_and_term.term.utf8_to_string()?
          );
        }
        (field_and_term, Some(term_states[idx].clone()))
      };

      let terms = fields_source.terms(&field_and_term.field)?.ok_or_else(|| {
        LuceneError::illegal_state(format!("missing terms for field {}", field_and_term.field))
      })?;
      let mut terms_enum = terms.iterator()?;

      if let Some(term_state) = term_state {
        terms_enum.seek_exact_with_state(&field_and_term.term, &term_state)?;
      } else if use_term_ord {
        match terms_enum.seek_exact_with_ord(field_and_term.ord) {
          Ok(()) => {},
          Err(error) if matches!(error.kind(), LuceneErrorKind::UnsupportedOperation(_)) => {
            supports_ords = false;
            assert!(terms_enum.seek_exact(&field_and_term.term)?);
          },
          Err(err) => return Err(err),
        }
      } else {
        assert!(terms_enum.seek_exact(&field_and_term.term)?);
      }

      assert_eq!(
        field_and_term.term.as_byte_slice(),
        terms_enum.term()?.as_bytes()
      );

      let term_ord = if supports_ords {
        match terms_enum.ord() {
          Ok(ord) => ord,
          Err(error) if matches!(error.kind(), LuceneErrorKind::UnsupportedOperation(_)) => {
            supports_ords = false;
            -1
          },
          Err(err) => return Err(err),
        }
      } else {
        -1
      };

      if term_ord != -1 {
        assert_eq!(field_and_term.ord, term_ord);
      }

      let mut saved_term_state = false;
      if options.contains(&Option_::TermState) && !use_term_state && random.random_range(0..5) == 1
      {
        term_states.push(terms_enum.term_state()?);
        term_state_terms.push(field_and_term);
        saved_term_state = true;
      }

      self.verify_enum(
        random,
        &mut thread_state,
        &field_and_term.field,
        &field_and_term.term,
        &mut terms_enum,
        max_test_options,
        max_index_options,
        options,
        always_test_max,
      )?;

      if options.contains(&Option_::TermState)
        && !use_term_state
        && !saved_term_state
        && random.random_range(0..5) == 1
      {
        term_states.push(terms_enum.term_state()?);
        term_state_terms.push(field_and_term);
      }

      if always_test_max || random.random_range(0..10) == 7 {
        if cfg!(feature = "test_log_verbose") {
          println!("TEST: try enum again on same term");
        }
        self.verify_enum(
          random,
          &mut thread_state,
          &field_and_term.field,
          &field_and_term.term,
          &mut terms_enum,
          max_test_options,
          max_index_options,
          options,
          always_test_max,
        )?;
      }
    }

    for field in self.fields.keys() {
      loop {
        let automaton = AutomatonTestUtil::random_automaton(random)?;
        let determinized = Operations::determinize(
          automaton.as_ref(),
          Operations::DEFAULT_DETERMINIZE_WORK_LIMIT,
        )?;
        let automaton = match determinized {
          Cow::Borrowed(_) => automaton.into_owned(),
          Cow::Owned(automaton) => automaton,
        };
        let mut ca = CompiledAutomaton::new(automaton.clone(), false, true)?;
        if ca.type_ != AutomatonType::Normal {
          continue;
        }

        let mut start_term = None;
        if random.random_bool(0.5) {
          let ras = RandomAcceptedStrings::new(&automaton)?;
          for _ in 0..100 {
            let code_points = ras.get_random_accepted_string(random)?;
            if code_points.is_empty() {
              continue;
            }
            let s = UnicodeUtil::new_string(&code_points, 0, code_points.len())?;
            start_term = Some(BytesRef::from_string(&s));
            break;
          }
          if start_term.is_none() {
            continue;
          }
        }

        let terms = fields_source
          .terms(field)?
          .ok_or_else(|| LuceneError::illegal_state(format!("missing terms for field {field}")))?;
        let mut intersected = terms.intersect(&ca, start_term.as_ref())?;
        let mut intersected_terms: HashSet<BytesRef<Vec<u8>>> = HashSet::new();

        loop {
          let term = match intersected.next()? {
            Some(term) => term.into_owned(),
            None => break,
          };
          if let Some(ref start_term) = start_term {
            assert!(start_term < &term);
          }
          intersected_terms.insert(BytesRef::deep_copy_of(&term)?);
          self.verify_enum(
            random,
            &mut thread_state,
            field,
            &term,
            &mut intersected,
            max_test_options,
            max_index_options,
            options,
            always_test_max,
          )?;
        }

        if let Some(run_automaton) = ca.run_automaton.as_mut() {
          let field_terms = self
            .fields
            .get(field)
            .ok_or_else(|| LuceneError::illegal_state(format!("missing field {field}")))?;
          for term2 in field_terms.keys() {
            let expected = if let Some(ref start_term) = start_term {
              if start_term >= term2 {
                false
              } else {
                run_automaton.run(term2.bytes.as_ref(), term2.offset, term2.length)?
              }
            } else {
              run_automaton.run(term2.bytes.as_ref(), term2.offset, term2.length)?
            };
            assert_eq!(expected, intersected_terms.contains(term2), "term={term2}");
          }
        } else {
          assert!(intersected_terms.is_empty());
        }

        break;
      }
    }

    Ok(())
  }

  pub fn test_fields<F>(&self, fields: &F) -> Result<()>
  where
    F: Fields,
  {
    let mut iterator = fields.iterator()?;
    while iterator.has_next()? {
      let _ = iterator.next()?;
    }
    assert!(!iterator.has_next()?);
    assert!(iterator.next()?.is_none());
    Ok(())
  }

  pub fn test_full<C, R>(
    &mut self,
    random: &mut R,
    codec: &C,
    path: TempDir,
    max_index_options: IndexOptions,
    with_payloads: bool,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
    C: crate::core::codecs::codec::Codec,
    C::PostingsFormat: PostingsFormat,
    <C::PostingsFormat as PostingsFormat>::FieldsProducer<<DirEnum as Directory>::IndexInput>:
      Send + Sync,
  {
    let dir = new_fs_directory(random, path)?;
    let fields_producer = Arc::new(self.build_index(
      codec,
      Arc::clone(&dir),
      max_index_options,
      with_payloads,
      true,
    )?);

    self.test_fields(&fields_producer)?;

    let all_options: HashSet<Option_> = Option_::iter().collect();

    let all_index_options: Vec<IndexOptions> = IndexOptions::values().collect();
    let max_index_option = all_index_options
      .iter()
      .position(|v| *v == max_index_options)
      .ok_or_else(|| {
        LuceneError::illegal_argument(format!("unsupported maxIndexOptions: {max_index_options}"))
      })?;

    for index_option in all_index_options.iter().take(max_index_option + 1).copied() {
      let mut opts = HashSet::new();
      opts.extend(all_options.iter().copied());
      self.test_terms(
        random,
        &fields_producer,
        &opts,
        index_option,
        max_index_options,
        true,
      )?;

      if with_payloads {
        let mut opts = HashSet::new();
        opts.extend(
          all_options
            .iter()
            .copied()
            .filter(|opt| *opt != Option_::Payloads),
        );
        self.test_terms(
          random,
          &fields_producer,
          &opts,
          index_option,
          max_index_options,
          true,
        )?;
      }
    }

    Ok(())
  }
}

trait IntToLongFunction {
  fn apply_as_long(self, i: i32) -> i64;
}
struct DocToNorm;
impl IntToLongFunction for DocToNorm {
  fn apply_as_long(self, doc: i32) -> i64 {
    (1 + (doc & 0x0f)) as i64
  }
}
