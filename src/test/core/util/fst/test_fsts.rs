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
use crate::core::index::BytesRefValue;
use std::borrow::Cow;

use crate::core::document::document::Document;
use crate::core::document::field::{FieldBase, Store};
use crate::core::document::fields::Fields;
use crate::core::document::string_field::StringField;
use crate::core::index::{BytesRef, directory_reader};
use crate::core::store::IO_CONTEXT_DEFAULT;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, at_least_usize, create_temp_dir_with_prefix, is_night_mode, new_bytes_ref_from_string,
  new_directory_shared, new_fs_directory, new_index_writer_config_with_analyzer,
  new_mock_directory, new_searcher_with_reader, random, random_from_seed, random_multiplier,
};

use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::{IndexWriter, MAX_TERM_LENGTH};
use crate::core::index::index_writer_config::OpenMode;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::multi_terms;
use crate::core::index::term::Term;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::search::index_searcher::{self, IndexSearcher};
use crate::core::search::term_query::TermQuery;
use crate::core::store::ByteArrayDataInput;
use crate::core::store::directory::Directory;
use crate::core::store::directory::MockDirWrapper;
use crate::core::store::dummy::dummy_index_output::DummyIndexOutput;
use crate::core::store::output_stream_data_output::OutputStreamDataOutput;
use crate::core::util::Comparator;
use crate::core::util::automation::automata::Automata;
use crate::core::util::automation::compiled_automaton::CompiledAutomaton;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::fst_impl::byte_sequence_outputs::ByteSequenceOutputs;
use crate::core::util::fst_impl::bytes_ref_fst_enum::BytesRefFSTEnum;
use crate::core::util::fst_impl::fst::{FST, InputType, read_metadata, target_has_arcs};
use crate::core::util::fst_impl::fst_compiler::{
  Builder, CompiledNode, DataOutputEnum, FIXED_LENGTH_ARC_DEEP_NUM_ARCS,
  FIXED_LENGTH_ARC_SHALLOW_DEPTH, FIXED_LENGTH_ARC_SHALLOW_NUM_ARCS, NodeEnum, UnCompiledNode,
};
use crate::core::util::fst_impl::fst_reader::FstReader;
use crate::core::util::fst_impl::int_sequence_outputs::IntSequenceOutputs;
use crate::core::util::fst_impl::no_outputs::NoOutputs;
use crate::core::util::fst_impl::outputs::Outputs;
use crate::core::util::fst_impl::positive_int_outputs::PositiveIntOutputs;
use crate::core::util::fst_impl::util::{TopNSearcher, TopNSearcherBase, TopResult, Util};
use crate::core::util::ints_ref::IntsRef;
use crate::core::util::ints_ref_builder::IntsRefBuilder;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::util::fst_tester::{
  DummyFSTTesterBaseImpl, FSTTester, InputOutput, get_random_string, simple_random_string,
  to_ints_ref_from_string,
};
use crate::test_framework::core::util::line_file_docs::LineFileDocs;
use crate::test_framework::core::util::test_util::TestUtil;
use rand::Rng;
use rand::RngExt;
use rand::seq::SliceRandom;
use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::rc::Rc;
use std::sync::Arc;

struct TestFSTs {
  dir: Rc<MockDirWrapper>,
}
impl TestFSTs {
  fn new<R>(random: &mut R) -> Result<Self>
  where
    R: Rng + ?Sized,
  {
    let dir = new_mock_directory(random)?;
    Ok(Self { dir: Rc::new(dir) })
  }
  fn do_test<R>(
    &self,
    random: &mut R,
    input_mode: i32,
    mut terms: Vec<IntsRef<Vec<i32>>>,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    terms.sort();
    let random_seed = random.random();
    {
      // NoOutputs (simple FSA)
      let outputs = NoOutputs::get_singleton().clone();

      let pairs = terms
        .iter()
        .map(|term| InputOutput {
          input: term.clone(),
          output: None,
        })
        .collect();

      let mut tester: FSTTester<_, _, _, DummyFSTTesterBaseImpl> = FSTTester::new(
        random_from_seed(random_seed),
        self.dir.clone(),
        input_mode,
        pairs,
        outputs,
      );
      tester.do_test()?;
    }

    // PositiveIntOutput (ord)
    {
      let outputs = PositiveIntOutputs::get_singleton();
      let pairs = terms
        .clone()
        .into_iter()
        .enumerate()
        .map(|(idx, term)| InputOutput {
          input: term,
          output: (idx != 0).then_some(idx as i64),
        })
        .collect();

      let mut tester: FSTTester<_, _, _, DummyFSTTesterBaseImpl> = FSTTester::new(
        random_from_seed(random_seed),
        self.dir.clone(),
        input_mode,
        pairs,
        outputs.clone(),
      );
      tester.do_test()?;
    }
    // PositiveIntOutputs (random monotonically increasing positive number)
    {
      let outputs = PositiveIntOutputs::get_singleton();
      let mut last_output = 0i64;
      let pairs = terms
        .iter()
        .map(|term| {
          let delta = random.random_range(1..=1000);
          last_output += delta;
          InputOutput {
            input: term.clone(),
            output: Some(last_output),
          }
        })
        .collect();

      let mut tester: FSTTester<_, _, _, DummyFSTTesterBaseImpl> = FSTTester::new(
        random_from_seed(random_seed),
        self.dir.clone(),
        input_mode,
        pairs,
        outputs.clone(),
      );
      tester.do_test()?;
    }
    // PositiveIntOutputs (random positive number)
    {
      let outputs = PositiveIntOutputs::get_singleton();
      let pairs = terms
        .iter()
        .map(|term| InputOutput {
          input: term.clone(),
          output: {
            let value = random.random_range(0..=i64::MAX);
            (value != 0).then_some(value)
          },
        })
        .collect();

      let mut tester: FSTTester<_, _, _, DummyFSTTesterBaseImpl> = FSTTester::new(
        random_from_seed(random_seed),
        self.dir.clone(),
        input_mode,
        pairs,
        outputs.clone(),
      );
      tester.do_test()?;
    }
    // Pair<ord, (random monotonically increasing positive number>
    // TODO: PairOutputs 未实现

    // Sequence-of-bytes
    {
      let outputs = ByteSequenceOutputs::get_singleton();

      let pairs = terms
        .iter()
        .enumerate()
        .map(|(idx, term)| {
          let output = if random.random_range(0..30) == 17 {
            None
          } else {
            let s = idx.to_string();
            let v: BytesRef<Arc<Vec<u8>>> = new_bytes_ref_from_string(random, &s).unwrap();
            Some(v)
          };
          InputOutput {
            input: term.clone(),
            output,
          }
        })
        .collect();

      let mut tester: FSTTester<_, _, _, DummyFSTTesterBaseImpl> = FSTTester::new(
        random_from_seed(random_seed),
        self.dir.clone(),
        input_mode,
        pairs,
        outputs.clone(),
      );
      tester.do_test()?;
    }
    // // Sequence-of-ints
    {
      let outputs = IntSequenceOutputs::get_singleton();
      let pairs = terms
        .into_iter()
        .enumerate()
        .map(|(idx, term)| {
          let s = idx.to_string();
          let vec = s.chars().map(|ch| ch as i32).collect();
          InputOutput {
            input: term,
            output: Some(IntsRef::from_slice(Arc::new(vec), 0, s.len())),
          }
        })
        .collect();

      let mut tester: FSTTester<_, _, _, DummyFSTTesterBaseImpl> = FSTTester::new(
        random_from_seed(random_seed),
        self.dir.clone(),
        input_mode,
        pairs,
        outputs.clone(),
      );
      tester.do_test()?;
    }

    Ok(())
  }
  fn test_random_words_impl<R>(
    &self,
    random: &mut R,
    max_num_words: usize,
    num_iter: usize,
  ) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let mut terms_random = random_from_seed(random.random());
    for iter in 0..num_iter {
      if cfg!(feature = "test_log_verbose") {
        println!("\nTEST: iter {iter}");
      }

      for input_mode in 0..2 {
        let num_words = terms_random.random_range(0..=max_num_words);
        let mut terms_set = HashSet::new();

        while terms_set.len() < num_words {
          let term = get_random_string(&mut terms_random);
          let ints_ref = to_ints_ref_from_string(&term, input_mode);
          terms_set.insert(ints_ref);
        }

        let terms: Vec<_> = Vec::from_iter(terms_set);
        self.do_test(random, input_mode, terms)?;
      }
    }
    Ok(())
  }
}
#[test]
fn test_basic_fsa() -> Result<()> {
  let mut random = random();
  let strings = [
    "station",
    "commotion",
    "elation",
    "elastic",
    "plastic",
    "stop",
    "ftop",
    "ftation",
    "stat",
  ];
  let strings2 = [
    "station",
    "commotion",
    "elation",
    "elastic",
    "plastic",
    "stop",
    "ftop",
    "ftation",
  ];
  let random_seed = random.random();
  for input_mode in 0..2 {
    let terms: Vec<_> = strings
      .iter()
      .map(|s| to_ints_ref_from_string::<Vec<i32>>(s, input_mode))
      .collect();
    let mut terms2: Vec<_> = strings2
      .iter()
      .map(|s| to_ints_ref_from_string(s, input_mode))
      .collect();
    terms2.sort();
    let test_fsts = TestFSTs::new(&mut random)?;
    test_fsts.do_test(&mut random, input_mode, terms)?;

    // Test pre-determined FST sizes to make sure we haven't lost minimality (at
    // least on this trivial set of terms):
    // FSA
    {
      let outputs = NoOutputs::get_singleton().clone();

      let pairs = terms2
        .iter()
        .map(|term| InputOutput {
          input: term.clone(),
          output: None,
        })
        .collect();

      let mut tester: FSTTester<_, _, _, DummyFSTTesterBaseImpl> = FSTTester::new(
        random_from_seed(random_seed),
        test_fsts.dir.clone(),
        input_mode,
        pairs,
        outputs,
      );

      let _ = tester.do_test()?;
      assert_eq!(tester.node_count, 22);
      assert_eq!(tester.arc_count, 27);
    }

    // FST ord pos int
    {
      let outputs = PositiveIntOutputs::get_singleton();
      let pairs = terms2
        .iter()
        .enumerate()
        .map(|(idx, term)| InputOutput {
          input: term.clone(),
          output: (idx != 0).then_some(idx as i64),
        })
        .collect();

      let mut tester: FSTTester<_, _, _, DummyFSTTesterBaseImpl> = FSTTester::new(
        random_from_seed(random_seed),
        test_fsts.dir.clone(),
        input_mode,
        pairs,
        outputs.clone(),
      );

      let _ = tester.do_test()?;
      assert_eq!(tester.node_count, 22);
      assert_eq!(tester.arc_count, 27);
    }

    // ByteSequenceOutputs ordinal position string
    {
      let outputs = ByteSequenceOutputs::get_singleton();
      let pairs = terms2
        .into_iter()
        .enumerate()
        .map(|(idx, term)| {
          let output = new_bytes_ref_from_string(&mut random, &idx.to_string()).expect("");
          InputOutput {
            input: term,
            output: Some(output),
          }
        })
        .collect();

      let mut tester: FSTTester<_, _, _, DummyFSTTesterBaseImpl> = FSTTester::new(
        random_from_seed(random_seed),
        test_fsts.dir.clone(),
        input_mode,
        pairs,
        outputs.clone(),
      );

      let _ = tester.do_test()?;
      assert_eq!(tester.node_count, 24);
      assert_eq!(tester.arc_count, 30);
    }
  }

  Ok(())
}

#[test]
fn test_random_words() -> Result<()> {
  let mut random = random();
  let test = TestFSTs {
    dir: Rc::new(new_mock_directory(&mut random)?),
  };
  if is_night_mode() {
    let num_iter = at_least_usize(&mut random, 2);
    test.test_random_words_impl(&mut random, 1000, num_iter)
  } else {
    test.test_random_words_impl(&mut random, 100, 1)
  }
}

#[cfg(feature = "nightly")]
#[test]
#[ignore = "nightly"]
fn test_big_set() -> Result<()> {
  let mut random = random();
  let test = TestFSTs::new(&mut random)?;
  let max_num_words = TestUtil::next_usize(&mut random, 50000, 60000);
  test.test_random_words_impl(&mut random, max_num_words, 1)
}

fn assert_same<TE, O, F>(
  terms_enum: &mut TE,
  fst_enum: &BytesRefFSTEnum<O, F>,
  store_ord: bool,
) -> Result<()>
where
  TE: TermsEnum,
  O: Outputs<V = i64>,
  F: FstReader,
{
  let current = fst_enum.current();
  {
    let term = terms_enum.term()?;
    assert_eq!(
      term.as_bytes(),
      current.input.as_byte_slice(),
      "{} != {}",
      term.utf8_to_string()?,
      current.input.utf8_to_string()?
    );
  }
  if store_ord {
    assert_eq!(terms_enum.ord()?, current.output.unwrap_or(0));
  } else {
    assert_eq!(terms_enum.doc_freq()? as i64, current.output.unwrap_or(0));
  }
  Ok(())
}
#[test]
fn test_real_terms() -> Result<()> {
  let mut random = random();
  let mut docs = LineFileDocs::new(&mut random)?;
  let num_docs = at_least(&mut random, 50);
  let mut analyzer = MockAnalyzer::new(&mut random);
  analyzer.set_max_token_length(TestUtil::next_int(&mut random, 1, MAX_TERM_LENGTH));

  let mut conf = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  conf.set_max_buffered_docs(-1).set_ram_buffer_size_mb(64.0);
  let temp_dir = create_temp_dir_with_prefix("fstlines")?;
  let dir = new_fs_directory(&mut random, temp_dir)?;
  let writer = IndexWriter::new(dir, conf)?;
  let mut doc_count = 0;
  while doc_count < num_docs {
    writer.add_document(docs.next_doc()?)?;
    doc_count += 1;
  }
  let reader = directory_reader::open_from_writer(&writer)?;
  writer.close()?;

  let outputs = PositiveIntOutputs::get_singleton().clone();

  let mut builder = Builder::new(InputType::Byte1, outputs.clone());

  let suffix_ram_limit_mb = if random.random_range(0..10) == 4 {
    // no suffix sharing
    0.0
  } else if random.random_range(0..10) == 7 {
    // share all suffixes (minimal FST)
    f64::INFINITY
  } else {
    (random.random::<f64>() + 0.01) * 10.0
  };
  builder.suffix_ram_limit_mb(suffix_ram_limit_mb)?;

  let mut fst_compiler = builder.build()?;

  let mut store_ord = random.random_bool(0.5);
  let terms = multi_terms::get_terms(&reader, "body")?;
  if let Some(terms) = terms {
    let mut scratch_ints_ref = IntsRefBuilder::new();
    let mut terms_enum = terms.iterator()?;
    let automaton = Automata::make_any_string()?;
    let compiled = CompiledAutomaton::new(automaton, false, false)?;
    let mut terms_enum2 = terms.intersect(&compiled, None)?;
    let mut ord = 0;

    while let Some(term) = terms_enum.next()? {
      let term = term.into_owned();
      let term2 = terms_enum2
        .next()?
        .expect("intersect enum must return term");
      assert_eq!(term.as_byte_slice(), term2.as_bytes());
      assert_eq!(terms_enum.doc_freq()?, terms_enum2.doc_freq()?);
      assert_eq!(
        terms_enum.total_term_freq()?,
        terms_enum2.total_term_freq()?
      );

      if ord == 0 && terms_enum.ord().is_err() {
        store_ord = false;
      }
      let output = if store_ord {
        ord
      } else {
        terms_enum.doc_freq()? as i64
      };
      Util::to_ints_ref(&term, &mut scratch_ints_ref)?;
      fst_compiler.add(scratch_ints_ref.get(), Some(output))?;
      ord += 1;
    }

    let metadata = fst_compiler.compile()?.unwrap();
    let fst_reader: DataOutputEnum<DummyIndexOutput> = fst_compiler.get_fst_reader()?;
    let fst = FST::from_fst_reader(metadata, fst_reader).unwrap();

    if ord > 0 {
      let mut random = random_from_seed(random.random());
      let mut fst_enum = BytesRefFSTEnum::new(fst)?;
      let num = at_least(&mut random, 1000);
      for _ in 0..num {
        let v = get_random_string(&mut random);
        let random_term = new_bytes_ref_from_string(&mut random, &v)?;

        let seek_result = terms_enum.seek_ceil(&random_term)?;
        let fst_seek_result = fst_enum.seek_ceil(&random_term)?;

        if seek_result == SeekStatus::End {
          assert!(fst_seek_result.is_none());
        } else {
          assert_same(&mut terms_enum, &fst_enum, store_ord)?;
          for _ in 0..10 {
            if terms_enum.next()?.is_some() {
              assert!(fst_enum.next_value()?.is_some());
              assert_same(&mut terms_enum, &fst_enum, store_ord)?;
            } else {
              assert!(fst_enum.next_value()?.is_none());
              break;
            }
          }
        }
      }
    }
  }

  Ok(())
}

#[test]
fn test_single_string() -> Result<()> {
  let mut random = random();
  let outputs = NoOutputs::get_singleton();
  let mut fst_compiler = Builder::new(InputType::Byte1, outputs.clone()).build()?;

  let mut builder = IntsRefBuilder::new();
  let key: BytesRef<Vec<u8>> = new_bytes_ref_from_string(&mut random, "foobar")?;
  Util::to_ints_ref(&key, &mut builder)?;
  fst_compiler.add(builder.get(), None)?;

  let metadata = fst_compiler.compile()?.unwrap();
  let reader: DataOutputEnum<DummyIndexOutput> = fst_compiler.get_fst_reader()?;
  let fst = FST::from_fst_reader(metadata, reader).unwrap();

  let mut fst_enum = BytesRefFSTEnum::new(fst)?;

  let seek1 = fst_enum.seek_floor(&new_bytes_ref_from_string(&mut random, "foo")?)?;
  assert!(seek1.is_none());

  let seek2 = fst_enum.seek_ceil(&new_bytes_ref_from_string(&mut random, "foobaz")?)?;
  assert!(seek2.is_none());

  Ok(())
}
#[test]
fn test_duplicate_fsa_string() -> Result<()> {
  let mut random = random();
  let outputs = NoOutputs::get_singleton();
  let mut fst_compiler = Builder::new(InputType::Byte1, outputs.clone()).build()?;

  let str_key = "foobar";
  let mut builder = IntsRefBuilder::new();
  let key: BytesRef<Vec<u8>> = new_bytes_ref_from_string(&mut random, str_key)?;
  for _ in 0..10 {
    Util::to_ints_ref(&key, &mut builder)?;
    fst_compiler.add(builder.get(), None)?;
  }

  let metadata = fst_compiler.compile()?.unwrap();
  let reader: DataOutputEnum<DummyIndexOutput> = fst_compiler.get_fst_reader()?;
  let fst = FST::from_fst_reader(metadata, reader).unwrap();

  let actual = Util::get_from_bytes(&fst, &key)?;
  assert!(actual.is_some());

  let v: BytesRef<Vec<u8>> = new_bytes_ref_from_string(&mut random, "foobaz")?;

  let missing = Util::get_from_bytes(&fst, &v)?;
  assert!(missing.is_none());

  // Count the input paths
  let mut fst_enum = BytesRefFSTEnum::new(fst)?;
  let mut count = 0;
  while fst_enum.next_value()?.is_some() {
    count += 1;
  }
  assert_eq!(count, 1);

  Ok(())
}

#[test]
fn test_simple() -> Result<()> {
  let mut random = random();
  // Get outputs -- passing true means FST will share
  // (delta code) the outputs.  This should result in
  // smaller FST if the outputs grow monotonically.  But
  // if numbers are "random", false should give smaller
  // final size:

  let outputs = PositiveIntOutputs::get_singleton();
  // Build an FST mapping BytesRef -> Long

  let mut fst_compiler = Builder::new(InputType::Byte1, outputs.clone()).build()?;

  let a = new_bytes_ref_from_string(&mut random, "a")?;
  let b = new_bytes_ref_from_string(&mut random, "b")?;
  let c: BytesRef<Rc<Vec<u8>>> = new_bytes_ref_from_string(&mut random, "c")?;

  let mut v = IntsRefBuilder::new();
  Util::to_ints_ref(&a, &mut v)?;
  fst_compiler.add(v.get(), Some(17))?;
  let mut v = IntsRefBuilder::new();
  Util::to_ints_ref(&b, &mut v)?;
  fst_compiler.add(v.get(), Some(42))?;
  let mut v = IntsRefBuilder::new();
  Util::to_ints_ref(&c, &mut v)?;
  fst_compiler.add(v.get(), Some(13824324872317238))?;

  let fst_metadata = fst_compiler.compile()?.unwrap();
  let fst_reader: DataOutputEnum<DummyIndexOutput> = fst_compiler.get_fst_reader()?;
  let fst = FST::from_fst_reader(fst_metadata, fst_reader).unwrap();

  assert_eq!(
    Util::get_from_bytes(&fst, &c)?.unwrap(),
    Some(13824324872317238)
  );
  assert_eq!(Util::get_from_bytes(&fst, &b)?.unwrap(), Some(42));
  assert_eq!(Util::get_from_bytes(&fst, &a)?.unwrap(), Some(17));

  let mut fst_enum = BytesRefFSTEnum::new(fst)?;
  let mut seek_result = fst_enum.seek_floor(&a)?;
  assert!(seek_result.is_some());
  assert_eq!(seek_result.as_ref().unwrap().output, Some(17));

  // seekFloor("aa") -> goes to "a"
  let aa = new_bytes_ref_from_string(&mut random, "aa")?;
  seek_result = fst_enum.seek_floor(&aa)?;
  assert!(seek_result.is_some());
  assert_eq!(seek_result.as_ref().unwrap().output, Some(17));

  // seekCeil("aa") -> goes to "b"
  let aa_ceil = new_bytes_ref_from_string(&mut random, "aa")?;
  debug_assert!(aa_ceil.is_valid().is_ok());
  seek_result = fst_enum.seek_ceil(&aa_ceil)?;
  assert!(seek_result.is_some());
  let result = seek_result.unwrap();
  assert_eq!(result.input, b);
  assert_eq!(result.output, Some(42));

  Ok(())
}
#[test]
fn test_primary_keys() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  for cycle in 0..2 {
    if cfg!(feature = "test_log_verbose") {
      println!("TEST: cycle={}", cycle);
    }

    let analyzer = MockAnalyzer::new(&mut random);
    let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
    config.set_open_mode(OpenMode::Create);
    let w = RandomIndexWriter::with_config(&mut random, dir.clone(), config);

    let mut doc = Document::new();
    doc.add(StringField::from_string("id", "", Store::No)?);

    let num_ids = at_least_usize(&mut random, 200);
    if cfg!(feature = "test_log_verbose") {
      println!("TEST: NUM_IDS={}", num_ids);
    }
    let mut all_ids = HashSet::new();
    for id in 0..num_ids {
      let id_string = if cycle == 0 {
        format!("{:07}", id)
      } else {
        loop {
          let s = random.random::<i64>().to_string();
          if !all_ids.contains(&s) {
            break s;
          }
        }
      };
      all_ids.insert(id_string.clone());
      let Some(Fields::String(id_field)) = doc.get_field_mut("id") else {
        unreachable!("id field must exist")
      };
      id_field.set_string_value(id_string)?;
      w.add_document(&mut random, &mut doc)?;
    }

    // turn writer into reader:
    let r = w.get_reader(&mut random)?;
    let s = new_searcher_with_reader(r)?;
    w.close(&mut random)?;

    let mut all_ids_list: Vec<Cow<'_, str>> = all_ids
      .iter()
      .map(|id| Cow::Borrowed(id.as_str()))
      .collect();
    let mut sorted_all_ids_list = all_ids_list.clone();
    sorted_all_ids_list.sort();

    // Sprinkle in some non-existent PKs:
    let mut out_of_bounds = HashSet::new();
    for idx in 0..num_ids / 10 {
      let id_string = if cycle == 0 {
        format!("{:07}", num_ids + idx)
      } else {
        loop {
          let s = random.random::<i64>().to_string();
          if !all_ids.contains(&s) {
            break s;
          }
        }
      };
      out_of_bounds.insert(id_string.clone());
      all_ids_list.push(Cow::Owned(id_string));
    }

    // Verify w/ TermQuery
    for _ in 0..2 * num_ids {
      let id = &all_ids_list[random.random_range(0..all_ids_list.len())];
      let exists = !out_of_bounds.contains(id.as_ref());
      if cfg!(feature = "test_log_verbose") {
        println!(
          "TEST: TermQuery {}id={}",
          if exists { "" } else { "non-exist " },
          id
        );
      }
      assert_eq!(
        if exists { 1 } else { 0 },
        s.count(TermQuery::new(Term::from_text("id", id)))?,
        "{}id={}",
        if exists { "" } else { "non-exist " },
        id
      );
    }

    // Verify w/ MultiTermsEnum
    let mut terms_enum = multi_terms::get_terms(s.reader_context.reader(), "id")?
      .expect("terms should exist")
      .iterator()?;
    for _ in 0..2 * num_ids {
      let (id, next_id, exists): (Cow<'_, str>, Option<Cow<'_, str>>, bool) =
        if random.random::<bool>() {
          let id = Cow::Borrowed(all_ids_list[random.random_range(0..all_ids_list.len())].as_ref());
          let exists = !out_of_bounds.contains(id.as_ref());
          if cfg!(feature = "test_log_verbose") {
            println!(
              "TEST: exactOnly {}id={}",
              if exists { "" } else { "non-exist " },
              id
            );
          }
          (id, None, exists)
        } else {
          // Pick ID between two IDs:
          let idv = TestUtil::next_usize(&mut random, 0, num_ids - 2);
          let (id, next_id) = if cycle == 0 {
            (
              Cow::Owned(format!("{:07}a", idv)),
              Cow::Owned(format!("{:07}", idv + 1)),
            )
          } else {
            (
              Cow::Owned(format!("{}a", sorted_all_ids_list[idv])),
              Cow::Borrowed(sorted_all_ids_list[idv + 1].as_ref()),
            )
          };
          if cfg!(feature = "test_log_verbose") {
            println!("TEST: not exactOnly id={} nextID={}", id, next_id);
          }
          (id, Some(next_id), false)
        };

      let status = if next_id.is_none() {
        if terms_enum.seek_exact(&new_bytes_ref_from_string::<_, Vec<u8>>(&mut random, &id)?)? {
          SeekStatus::Found
        } else {
          SeekStatus::NotFound
        }
      } else {
        terms_enum.seek_ceil(&new_bytes_ref_from_string::<_, Vec<u8>>(&mut random, &id)?)?
      };

      if let Some(next_id) = next_id {
        assert_eq!(SeekStatus::NotFound, status);
        let expected = new_bytes_ref_from_string::<_, Vec<u8>>(&mut random, &next_id)?;
        let actual = terms_enum.term()?;
        assert_eq!(
          expected.as_byte_slice(),
          actual.as_bytes(),
          "expected={} actual={}",
          next_id,
          actual.utf8_to_string()?
        );
      } else if !exists {
        assert_eq!(SeekStatus::NotFound, status);
      } else {
        assert_eq!(SeekStatus::Found, status);
      }
    }
    s.reader_context.reader().close()?;
  }
  dir.close()?;

  Ok(())
}
#[test]
fn test_random_term_lookup() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  // build writer
  let writer = RandomIndexWriter::new(&mut random, dir.clone())?;
  let mut doc = Document::new();

  doc.add(StringField::from_string("field", "", Store::No)?);

  // compute NUM_TERMS
  let num_terms = (1000.0 * random_multiplier() as f64 * (1.0 + random.random::<f64>())) as usize;

  let mut all_terms = HashSet::new();
  while all_terms.len() < num_terms {
    all_terms.insert(simple_random_string(&mut random));
  }

  for term in &all_terms {
    let Some(Fields::String(field)) = doc.get_field_mut("field") else {
      unreachable!("field must exist")
    };
    field.set_string_value(term)?;
    writer.add_document(&mut random, &mut doc)?;
  }

  let reader = writer.get_reader(&mut random)?;
  let searcher = index_searcher::from_reader(reader)?;
  writer.close(&mut random)?;

  let mut all_terms_list: Vec<&String> = all_terms.iter().collect();
  all_terms_list.shuffle(&mut random);

  for term in all_terms_list {
    let query = TermQuery::new(Term::from_text("field", term));
    let count = searcher.count(query)?;
    assert_eq!(
      count, 1,
      "term={term} -- expected exactly 1 match, got {count}"
    );
  }

  Ok(())
}

#[test]
fn test_expanded_close_to_root() -> Result<()> {
  fn generate(out: &mut Vec<String>, b: &mut String, from: char, to: char, depth: i32) {
    if depth == 0 || from == to {
      let seq = format!("{}_{}_end", b, out.len());
      out.push(seq);
    } else {
      let mut c = from as u32;
      let to_u = to as u32;
      while c <= to_u {
        let ch = std::char::from_u32(c).unwrap();
        b.push(ch);
        let next_to = if ch == to { to } else { from };
        generate(out, b, from, next_to, depth - 1);
        b.pop();
        c += 1;
      }
    }
  }

  fn compile(lines: &[String]) -> Result<FST<NoOutputs, DataOutputEnum<DummyIndexOutput>>> {
    let outputs = NoOutputs::get_singleton().clone();

    let mut fst_compiler = Builder::new(InputType::Byte1, outputs).build()?;

    for w in lines.iter() {
      let bytes: BytesRef<Vec<u8>> = BytesRef::from_string(w);
      let mut scratch = IntsRefBuilder::new();
      Util::to_ints_ref(&bytes, &mut scratch)?;
      fst_compiler.add(scratch.get(), None)?;
    }

    let metadata = fst_compiler.compile()?.unwrap();
    let fst_reader = fst_compiler.get_fst_reader()?;
    Ok(FST::from_fst_reader(metadata, fst_reader).unwrap())
  }

  fn verify_state_and_below<F: FstReader>(
    fst: &FST<NoOutputs, F>,
    arc: &mut crate::core::util::fst_impl::fst::Arc<()>,
    depth: usize,
  ) -> Result<usize> {
    if target_has_arcs(arc) {
      let mut child_count = 0usize;
      let mut fst_reader = fst.get_bytes_reader()?;

      fst.read_first_target_arc(&arc.clone(), arc, &mut fst_reader)?;
      loop {
        let expanded = fst.is_expanded_target(arc, &mut fst_reader)?;

        let mut child_arc = crate::core::util::fst_impl::fst::Arc::default();
        child_arc.copy_from(arc);
        let children = verify_state_and_below(fst, &mut child_arc, depth + 1)?;

        assert_eq!(
          (depth <= FIXED_LENGTH_ARC_SHALLOW_DEPTH
            && children >= FIXED_LENGTH_ARC_SHALLOW_NUM_ARCS)
            || children >= FIXED_LENGTH_ARC_DEEP_NUM_ARCS,
          expanded
        );

        if arc.is_last() {
          break;
        }

        fst.read_next_arc(arc, &mut fst_reader)?;
        child_count += 1;
      }

      Ok(child_count)
    } else {
      Ok(0)
    }
  }

  // Sanity check.
  const {
    assert!(FIXED_LENGTH_ARC_SHALLOW_NUM_ARCS < FIXED_LENGTH_ARC_DEEP_NUM_ARCS);
  }

  let mut out = Vec::new();
  let mut b = String::new();
  generate(&mut out, &mut b, 'a', 'i', 10);
  out.sort();

  let fst = compile(&out)?;
  let mut arc = crate::core::util::fst_impl::fst::Arc::default();
  fst.get_first_arc(&mut arc);
  verify_state_and_below(&fst, &mut arc, 1)?;

  Ok(())
}
#[test]
#[should_panic]
fn test_final_output_on_end_state() {
  let outputs = PositiveIntOutputs::get_singleton();
  let mut fst_compiler = Builder::new(InputType::Byte4, outputs.clone())
    .build()
    .expect("");

  let mut scratch = IntsRefBuilder::new();
  let _ = Util::to_utf32("slat", &mut scratch);
  fst_compiler.add(scratch.get(), Some(10)).expect("");
  let _ = Util::to_utf32("st", &mut scratch);
  fst_compiler.add(scratch.get(), Some(17)).expect("");

  let metadata = fst_compiler.compile().expect("").unwrap();
  let reader: DataOutputEnum<DummyIndexOutput> = fst_compiler.get_fst_reader().expect("");
  let fst = FST::from_fst_reader(metadata, reader).unwrap();
  let mut w = Vec::new();
  Util::to_dot(&fst, &mut w, false, false).expect("");
  let dot = String::from_utf8(w).expect("");
  println!("{dot}");
  assert!(dot.contains("label=\"t/[7]\""));
}

#[test]
fn test_internal_final_state() -> Result<()> {
  let mut random = random();
  let outputs = PositiveIntOutputs::get_singleton();
  let mut fst_compiler = Builder::new(InputType::Byte1, outputs.clone()).build()?;

  let stat: BytesRef<Vec<u8>> = new_bytes_ref_from_string(&mut random, "stat")?;
  let station: BytesRef<Vec<u8>> = new_bytes_ref_from_string(&mut random, "station")?;

  let mut scratch = IntsRefBuilder::new();
  Util::to_ints_ref(&stat, &mut scratch)?;
  fst_compiler.add(scratch.get(), None)?;
  Util::to_ints_ref(&station, &mut scratch)?;
  fst_compiler.add(scratch.get(), None)?;

  let metadata = fst_compiler.compile()?.unwrap();
  let reader: DataOutputEnum<DummyIndexOutput> = fst_compiler.get_fst_reader()?;
  let fst = FST::from_fst_reader(metadata, reader).unwrap();
  let mut w = Vec::new();
  Util::to_dot(&fst, &mut w, false, false)?;
  let dot = String::from_utf8(w)?;

  // check for accept state at label t
  assert!(dot.contains("[label=\"t\" style=\"bold\""));
  // check for accept state at label n
  assert!(dot.contains("[label=\"n\" style=\"bold\""));

  Ok(())
}

// https://github.com/apache/lucene/issues/12697
// Make sure the FST can be saved and loaded with different DataOutput for
// metadata
#[test]
fn test_save_different_meta_out() -> Result<()> {
  let mut random = random();
  let outputs = PositiveIntOutputs::get_singleton();
  let mut fst_compiler = Builder::new(InputType::Byte1, outputs.clone()).build()?;

  // Build the FST
  let mut scratch = IntsRefBuilder::new();
  let key1: BytesRef<Vec<u8>> = new_bytes_ref_from_string(&mut random, "aab")?;
  let key2: BytesRef<Vec<u8>> = new_bytes_ref_from_string(&mut random, "aac")?;
  let key3: BytesRef<Vec<u8>> = new_bytes_ref_from_string(&mut random, "ax")?;

  Util::to_ints_ref(&key1, &mut scratch)?;
  fst_compiler.add(scratch.get(), Some(22))?;
  Util::to_ints_ref(&key2, &mut scratch)?;
  fst_compiler.add(scratch.get(), Some(7))?;
  Util::to_ints_ref(&key3, &mut scratch)?;
  fst_compiler.add(scratch.get(), Some(17))?;

  // Compile and load once
  let metadata = fst_compiler.compile()?.unwrap();
  let fst_reader: DataOutputEnum<DummyIndexOutput> = fst_compiler.get_fst_reader()?;
  let fst = FST::from_fst_reader(metadata, fst_reader).unwrap();

  // Save into a single output
  let mut bytes = Vec::new();
  {
    let mut out = OutputStreamDataOutput::new(&mut bytes);
    fst.save_with_same_data_out(&mut out)?;
  }
  // Load it back using split input (force FSTStore path)
  let mut input = ByteArrayDataInput::with_bytes(bytes.as_slice());
  let metadata = read_metadata(&mut input, outputs.clone())?;
  let mut loaded_fst = FST::from_on_heap_store(metadata, &mut input)?;

  // Save again, now to separate outputs
  let mut metdata_os = Vec::new();
  let mut data_os_os = Vec::new();
  {
    let mut meta_out = OutputStreamDataOutput::new(&mut metdata_os);
    let mut data_out = OutputStreamDataOutput::new(&mut data_os_os);
    loaded_fst.save(&mut meta_out, &mut data_out)?;
  }

  // Load again using split inputs
  let mut meta_in = ByteArrayDataInput::with_bytes(metdata_os.as_slice());
  let mut data_in = ByteArrayDataInput::with_bytes(data_os_os.as_slice());
  let metadata = read_metadata(&mut meta_in, outputs.clone())?;
  let loaded_fst = FST::from_on_heap_store(metadata, &mut data_in)?;

  Util::to_ints_ref(&key1, &mut scratch)?;
  assert_eq!(Util::get_from_bytes(&loaded_fst, &key1)?.unwrap(), Some(22));

  Util::to_ints_ref(&key2, &mut scratch)?;
  assert_eq!(Util::get_from_bytes(&loaded_fst, &key2)?.unwrap(), Some(7));

  Util::to_ints_ref(&key3, &mut scratch)?;
  assert_eq!(Util::get_from_bytes(&loaded_fst, &key3)?.unwrap(), Some(17));

  Ok(())
}
// Make sure raw FST can differentiate between final vs
// non-final end nodes
#[test]
fn test_non_final_stop_node() -> Result<()> {
  let outputs = PositiveIntOutputs::get_singleton();

  let builder: Builder<_, DummyIndexOutput> = Builder::new(InputType::Byte1, outputs.clone());
  let mut fst_compiler = builder.build()?;

  // Root node
  let mut root_node = UnCompiledNode::new(0);

  // Add final stop node for 'a'
  {
    let mut node = UnCompiledNode::new(0);
    node.is_final = true;
    fst_compiler.frontier[0] = node;
    root_node.add_arc(b'a' as i32, NodeEnum::UnCompiledNode)?;
    let fronze = CompiledNode {
      node: fst_compiler.add_node(0)?,
    };

    root_node.arcs[0].next_final_output = Some(17);
    root_node.arcs[0].is_final = true;
    root_node.arcs[0].output = None;
    root_node.arcs[0].target = NodeEnum::CompiledNode(fronze);
  }

  // Add non-final stop node for 'b'
  {
    let node = UnCompiledNode::new(0);
    fst_compiler.frontier[1] = node;
    root_node.add_arc(b'b' as i32, NodeEnum::UnCompiledNode)?;
    let fronze = CompiledNode {
      node: fst_compiler.add_node(1)?,
    };

    root_node.arcs[1].next_final_output = None;
    root_node.arcs[1].output = Some(42);
    root_node.arcs[1].target = NodeEnum::CompiledNode(fronze);
  }
  // index = 2;
  fst_compiler.frontier[2] = root_node;

  // Finish FST
  // 2  =  root node
  let root = fst_compiler.add_node(2)?;
  fst_compiler.finish(root)?;

  // Construct FST
  let reader = fst_compiler.get_fst_reader()?;

  let fst = FST::new(fst_compiler.fst.metadata, reader);

  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  {
    let mut out = dir.create_output("fst", IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?)?;
    fst.save_with_same_data_out(&mut out)?;
  }
  // skip string writer
  check_stop_nodes(&fst, outputs.clone())?;

  let mut in_file = dir.open_input("fst", IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?)?;
  let metadata = read_metadata(&mut in_file, outputs.clone())?;
  let loaded_fst = FST::from_on_heap_store(metadata, &mut in_file)?;

  check_stop_nodes(&loaded_fst, outputs.clone())?;

  Ok(())
}

fn check_stop_nodes<F>(fst: &FST<PositiveIntOutputs, F>, _outputs: PositiveIntOutputs) -> Result<()>
where
  F: FstReader,
{
  let mut start_arc = crate::core::util::fst_impl::fst::Arc::default();
  fst.get_first_arc(&mut start_arc);
  assert!(start_arc.output.is_none());
  assert!(start_arc.next_final_output.is_none());

  let mut reader = fst.get_bytes_reader()?;
  let mut arc = crate::core::util::fst_impl::fst::Arc::default();
  fst.read_first_target_arc(&start_arc, &mut arc, &mut reader)?;
  assert_eq!(arc.label, b'a' as i32);
  assert_eq!(arc.next_final_output(), Some(17));
  assert!(arc.is_final());

  fst.read_next_arc(&mut arc, &mut reader)?;
  assert_eq!(arc.label, b'b' as i32);
  assert!(!arc.is_final());
  assert_eq!(arc.output(), Some(42));

  Ok(())
}

#[derive(Clone)]
struct MinLongComparator;

impl Comparator<Option<i64>> for MinLongComparator {
  const TYPE: &'static str = "MinLongComparator";

  fn compare(&self, a: &Option<i64>, b: &Option<i64>) -> Result<i32> {
    Ok(match a.unwrap_or(0).cmp(&b.unwrap_or(0)) {
      Ordering::Less => -1,
      Ordering::Equal => 0,
      Ordering::Greater => 1,
    })
  }
}

struct RejectNoLimitsBase {
  reject_count: Rc<Cell<i32>>,
}

impl TopNSearcherBase<i64> for RejectNoLimitsBase {
  fn accept_result(&mut self, _input: &IntsRef<Vec<i32>>, output: &Option<i64>) -> bool {
    let accept = *output == Some(7);
    if !accept {
      self.reject_count.set(self.reject_count.get() + 1);
    }
    accept
  }
}

#[test]
fn test_shortest_paths() -> Result<()> {
  let outputs = PositiveIntOutputs::get_singleton().clone();
  let mut fst_compiler = Builder::new(InputType::Byte1, outputs.clone()).build()?;

  let mut scratch = IntsRefBuilder::new();
  Util::to_ints_ref(&BytesRef::<Vec<u8>>::from_string("aab"), &mut scratch)?;
  fst_compiler.add(scratch.get(), Some(22))?;
  Util::to_ints_ref(&BytesRef::<Vec<u8>>::from_string("aac"), &mut scratch)?;
  fst_compiler.add(scratch.get(), Some(7))?;
  Util::to_ints_ref(&BytesRef::<Vec<u8>>::from_string("ax"), &mut scratch)?;
  fst_compiler.add(scratch.get(), Some(17))?;

  let metadata = fst_compiler.compile()?.unwrap();
  let fst_reader: DataOutputEnum<DummyIndexOutput> = fst_compiler.get_fst_reader()?;
  let fst = FST::from_fst_reader(metadata, fst_reader).unwrap();

  let mut first_arc = crate::core::util::fst_impl::fst::Arc::default();
  fst.get_first_arc(&mut first_arc);
  let res = Util::shortest_paths(&fst, &first_arc, None, MinLongComparator, 3, true)?;
  assert!(res.is_complete);
  assert_eq!(3, res.top_n.len());

  Util::to_ints_ref(&BytesRef::<Vec<u8>>::from_string("aac"), &mut scratch)?;
  assert_eq!(scratch.get(), &res.top_n[0].input);
  assert_eq!(Some(7), res.top_n[0].output);

  Util::to_ints_ref(&BytesRef::<Vec<u8>>::from_string("ax"), &mut scratch)?;
  assert_eq!(scratch.get(), &res.top_n[1].input);
  assert_eq!(Some(17), res.top_n[1].output);

  Util::to_ints_ref(&BytesRef::<Vec<u8>>::from_string("aab"), &mut scratch)?;
  assert_eq!(scratch.get(), &res.top_n[2].input);
  assert_eq!(Some(22), res.top_n[2].output);

  Ok(())
}
#[test]
fn test_reject_no_limits() -> Result<()> {
  let outputs = PositiveIntOutputs::get_singleton().clone();
  let mut fst_compiler = Builder::new(InputType::Byte1, outputs.clone()).build()?;

  let mut scratch = IntsRefBuilder::new();
  Util::to_ints_ref(&BytesRef::<Vec<u8>>::from_string("aab"), &mut scratch)?;
  fst_compiler.add(scratch.get(), Some(22))?;
  Util::to_ints_ref(&BytesRef::<Vec<u8>>::from_string("aac"), &mut scratch)?;
  fst_compiler.add(scratch.get(), Some(7))?;
  Util::to_ints_ref(&BytesRef::<Vec<u8>>::from_string("adcd"), &mut scratch)?;
  fst_compiler.add(scratch.get(), Some(17))?;
  Util::to_ints_ref(&BytesRef::<Vec<u8>>::from_string("adcde"), &mut scratch)?;
  fst_compiler.add(scratch.get(), Some(17))?;
  Util::to_ints_ref(&BytesRef::<Vec<u8>>::from_string("ax"), &mut scratch)?;
  fst_compiler.add(scratch.get(), Some(17))?;

  let metadata = fst_compiler.compile()?.unwrap();
  let fst_reader: DataOutputEnum<DummyIndexOutput> = fst_compiler.get_fst_reader()?;
  let fst = FST::from_fst_reader(metadata, fst_reader).unwrap();

  let reject_count = Rc::new(Cell::new(0));
  let mut searcher = TopNSearcher::new(&fst, 2, 6, MinLongComparator)?;
  searcher.set_base(RejectNoLimitsBase {
    reject_count: reject_count.clone(),
  });
  let mut first_arc = crate::core::util::fst_impl::fst::Arc::default();
  fst.get_first_arc(&mut first_arc);
  searcher.add_start_paths(&first_arc, None, true, IntsRefBuilder::new())?;
  let res = searcher.search()?;
  assert_eq!(reject_count.get(), 4);
  assert!(res.is_complete);

  assert_eq!(1, res.top_n.len());
  Util::to_ints_ref(&BytesRef::<Vec<u8>>::from_string("aac"), &mut scratch)?;
  assert_eq!(scratch.get(), &res.top_n[0].input);
  assert_eq!(Some(7), res.top_n[0].output);

  reject_count.set(0);
  let mut searcher = TopNSearcher::new(&fst, 2, 5, MinLongComparator)?;
  searcher.set_base(RejectNoLimitsBase {
    reject_count: reject_count.clone(),
  });
  searcher.add_start_paths(&first_arc, None, true, IntsRefBuilder::new())?;
  let res = searcher.search()?;
  assert_eq!(reject_count.get(), 4);
  assert!(!res.is_complete);

  Ok(())
}
#[test]
fn test_shortest_paths_wfst() {
  // TODO: PairOutputs 未实现
}
#[test]
fn test_shortest_paths_random() -> Result<()> {
  let mut random = random();
  let num_words = at_least_usize(&mut random, 1000);

  let mut slow_completor = BTreeMap::new();
  let mut all_prefixes = BTreeSet::new();

  let outputs = PositiveIntOutputs::get_singleton().clone();
  let mut fst_compiler = Builder::new(InputType::Byte1, outputs.clone()).build()?;
  let mut scratch = IntsRefBuilder::new();

  for _ in 0..num_words {
    let mut s;
    loop {
      s = TestUtil::random_simple_string(&mut random);
      if !slow_completor.contains_key(&s) {
        break;
      }
    }

    for j in 1..s.len() {
      if !all_prefixes.contains(&s[..j]) {
        all_prefixes.insert(s[..j].to_string());
      }
    }
    let weight = TestUtil::next_int(&mut random, 1, 100) as i64;
    slow_completor.insert(s, weight);
  }

  for (key, value) in &slow_completor {
    Util::to_ints_ref(&BytesRef::<Vec<u8>>::from_string(key), &mut scratch)?;
    fst_compiler.add(scratch.get(), (*value != 0).then_some(*value))?;
  }

  let metadata = fst_compiler.compile()?.unwrap();
  let fst_reader: DataOutputEnum<DummyIndexOutput> = fst_compiler.get_fst_reader()?;
  let fst = FST::from_fst_reader(metadata, fst_reader).unwrap();
  let mut reader = fst.get_bytes_reader()?;

  for prefix in all_prefixes {
    let mut prefix_output = 0;
    let mut arc = crate::core::util::fst_impl::fst::Arc::default();
    fst.get_first_arc(&mut arc);
    for label in prefix.bytes() {
      let follow = arc.clone();
      let found = fst.find_target_arc(label as i32, &follow, &mut arc, &mut reader)?;
      assert!(found.is_some());
      prefix_output += arc.output().unwrap_or(0);
    }

    let top_n = TestUtil::next_usize(&mut random, 1, 10);

    let r = Util::shortest_paths(&fst, &arc, None, MinLongComparator, top_n, true)?;
    assert!(r.is_complete);

    let mut matches = Vec::new();

    for (key, value) in &slow_completor {
      if key.starts_with(&prefix) {
        let suffix = &key[prefix.len()..];
        let mut input = IntsRefBuilder::new();
        Util::to_ints_ref(&BytesRef::<Vec<u8>>::from_string(suffix), &mut input)?;
        let output = value - prefix_output;
        matches.push(TopResult::new(
          input.to_ints_ref(),
          (output != 0).then_some(output),
        ));
      }
    }

    assert!(!matches.is_empty());
    matches.sort_by(|a, b| {
      let cmp = a.output.cmp(&b.output);
      if cmp == Ordering::Equal {
        a.input.cmp(&b.input)
      } else {
        cmp
      }
    });
    if matches.len() > top_n {
      matches.truncate(top_n);
    }

    assert_eq!(matches.len(), r.top_n.len());

    for (expected, actual) in matches.iter().zip(r.top_n.iter()) {
      assert_eq!(expected.input, actual.input);
      assert_eq!(expected.output, actual.output);
    }
  }

  Ok(())
}
#[test]
fn test_shortest_paths_wfst_random() {
  // TODO: PairOutputs 未实现
}
#[test]
fn test_large_outputs_on_array_arcs() -> Result<()> {
  let outputs = ByteSequenceOutputs::get_singleton();
  let mut fst_compiler = Builder::new(InputType::Byte1, outputs.clone()).build()?;

  let bytes = vec![0u8; 300];
  let mut input = IntsRefBuilder::new();
  input.append(0)?;
  let mut output = BytesRef::from_bytes(bytes);

  for arc in 0..6 {
    input.set_int_at(0, arc);
    output.bytes[0] = arc as u8;
    let v = BytesRef::from_slice(Arc::new(output.bytes.clone()), output.offset, output.length);
    fst_compiler.add(input.get(), Some(v))?;
  }

  let metadata = fst_compiler.compile()?.unwrap();
  let fst_reader: DataOutputEnum<DummyIndexOutput> = fst_compiler.get_fst_reader()?;
  let fst = FST::from_fst_reader(metadata, fst_reader).unwrap();

  for arc in 0..6 {
    input.set_int_at(0, arc);
    let result = Util::get_from_ints(&fst, input.get())?;
    assert!(result.is_some());
    let result = result.unwrap().unwrap();
    assert_eq!(result.length, 300);
    assert_eq!(result.bytes[result.offset], arc as u8);
    for i in 1..result.length {
      assert_eq!(result.bytes[result.offset + i], 0);
    }
  }

  Ok(())
}
#[test]
fn test_illegally_modify_root_arc() -> Result<()> {
  let mut random = random();
  let mut terms = HashSet::new();
  for i in 0..100 {
    let prefix = std::char::from_u32('a' as u32 + i as u32)
      .unwrap()
      .to_string();
    terms.insert(new_bytes_ref_from_string(&mut random, &prefix)?);
    if prefix != "m" {
      for _j in 0..20 {
        let suffix = TestUtil::random_unicode_string(&mut random);
        terms.insert(new_bytes_ref_from_string(
          &mut random,
          &format!("{}{}", prefix, suffix),
        )?);
      }
    }
  }

  let mut terms_list: Vec<_> = terms.into_iter().collect();
  terms_list.sort();

  let outputs = ByteSequenceOutputs::get_singleton();
  let mut fst_compiler = Builder::new(InputType::Byte1, outputs.clone()).build()?;

  let mut input = IntsRefBuilder::new();
  for term in terms_list {
    Util::to_ints_ref(&term, &mut input)?;
    debug_assert!(term.is_valid().is_ok());
    fst_compiler.add(input.get(), Some(term))?;
  }

  let metadata = fst_compiler.compile()?.unwrap();
  let reader: DataOutputEnum<DummyIndexOutput> = fst_compiler.get_fst_reader()?;
  let fst = FST::from_fst_reader(metadata, reader).unwrap();

  let mut arc = crate::core::util::fst_impl::fst::Arc::default();
  fst.get_first_arc(&mut arc);
  let mut reader = fst.get_bytes_reader()?;
  let found = fst.find_target_arc('m' as i32, &arc.clone(), &mut arc, &mut reader)?;
  assert!(found.is_some());
  assert_eq!(arc.output(), Some(BytesRef::from_string("m")));
  arc.output = None;
  fst.find_target_arc('m' as i32, &arc.clone(), &mut arc, &mut reader)?;
  Ok(())
}
#[test]
fn test_simple_depth() -> Result<()> {
  let mut random = random();
  let outputs = PositiveIntOutputs::get_singleton();
  let mut fst_compiler = Builder::new(InputType::Byte1, outputs.clone()).build()?;

  let ab: BytesRef<Vec<u8>> = new_bytes_ref_from_string(&mut random, "ab")?;
  let ac: BytesRef<Vec<u8>> = new_bytes_ref_from_string(&mut random, "ac")?;
  let bd: BytesRef<Vec<u8>> = new_bytes_ref_from_string(&mut random, "bd")?;

  let mut builder = IntsRefBuilder::new();
  Util::to_ints_ref(&ab, &mut builder)?;
  fst_compiler.add(builder.get(), Some(3))?;

  Util::to_ints_ref(&ac, &mut builder)?;
  fst_compiler.add(builder.get(), Some(5))?;

  Util::to_ints_ref(&bd, &mut builder)?;
  fst_compiler.add(builder.get(), Some(7))?;

  let metadata = fst_compiler.compile()?.unwrap();
  let reader: DataOutputEnum<DummyIndexOutput> = fst_compiler.get_fst_reader()?;
  let fst = FST::from_fst_reader(metadata, reader).unwrap();

  assert_eq!(Util::get_from_bytes(&fst, &ab)?.unwrap(), Some(3));
  assert_eq!(Util::get_from_bytes(&fst, &ac)?.unwrap(), Some(5));
  assert_eq!(Util::get_from_bytes(&fst, &bd)?.unwrap(), Some(7));

  Ok(())
}
