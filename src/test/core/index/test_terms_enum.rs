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
use crate::core::document::document::Document;
use crate::core::document::field::Store::{No, Yes};
use crate::core::document::field_type::FieldType;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::index::BytesRef;
use crate::core::index::BytesRefValue;
use crate::core::index::automaton_terms_enum::AutomatonTermsEnum;
use crate::core::index::index_reader::{IndexReader, IndexReaderContextKind};
use crate::core::index::index_writer::MAX_TERM_LENGTH;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, at_least_usize, get_only_leaf_reader, new_bytes_ref_from_string, new_directory_shared,
  new_index_writer_config_with_analyzer, new_string_field, new_text_field, random,
  random_from_seed, random_multiplier,
};

#[cfg(feature = "nightly")]
use crate::core::index::index_reader_context::IndexReaderContext;
#[cfg(feature = "nightly")]
use crate::core::index::index_writer_config::OpenMode;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::log_merge_policy::LogMergePolicy;
use crate::core::index::multi_doc_values::MultiDocValues;
use crate::core::index::multi_terms::get_terms;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::postings_enum::NONE;
use crate::core::index::standard_directory_reader::StandardDirectoryReader;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term::Term;
use crate::core::index::term_state::TermState;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::{EmptyTermsEnum, SeekStatus, TermsEnum};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::store::directory::{DirEnum, Directory};
use crate::core::util::automation::automata::Automata;
use crate::core::util::automation::byte_runnable::ByteRunnable;
use crate::core::util::automation::compiled_automaton::CompiledAutomaton;
use crate::core::util::automation::operations::Operations;
use crate::core::util::automation::reg_exp::RegExp;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind, Result};
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::per_thread_pk_lookup::PerThreadPKLookup;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::util::line_file_docs::LineFileDocs;
use crate::test_framework::core::util::test_util::TestUtil;
use rand::Rng;
use rand::RngExt;
use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, HashSet};

#[allow(dead_code)] // for quick search
struct TestTermsEnum;

const FIELD: &str = "field";

#[test]
fn test() -> Result<()> {
  let mut random = random();
  let seed = random.random::<u64>();
  let mut line_random = random_from_seed(seed);
  let mut docs = LineFileDocs::new(&mut line_random)?;
  let d = new_directory_shared(&mut random)?;
  let mut analyzer = MockAnalyzer::new(&mut random);
  analyzer.set_max_token_length(TestUtil::next_int(&mut random, 1, MAX_TERM_LENGTH));
  let w = RandomIndexWriter::with_analyzer(&mut random, d.clone(), analyzer)?;
  let num_docs = at_least(&mut random, 10);
  for _doc_count in 0..num_docs {
    w.add_document(&mut line_random, docs.next_doc()?)?;
  }
  let r = w.get_reader(&mut line_random)?;
  w.close(&mut line_random)?;

  let mut terms: Vec<BytesRef<Vec<u8>>> = Vec::new();
  let mut terms_enum = get_terms(&r, "body")?
    .expect("body terms should exist")
    .iterator()?;
  while let Some(term) = terms_enum.next()? {
    terms.push(term.into_owned());
  }

  let mut upto: isize = -1;
  let iters = at_least(&mut random, 200);
  for _iter in 0..iters {
    if upto != -1 && random.random_bool(0.5) {
      let is_end = terms_enum.next()?.is_none();
      upto += 1;
      if is_end {
        assert_eq!(upto as usize, terms.len());
        upto = -1;
      } else {
        assert!(upto < terms.len() as isize);
        assert_eq!(
          terms[upto as usize].as_byte_slice(),
          terms_enum.term()?.as_bytes()
        );
      }
    } else {
      let target = if random.random_bool(0.5) {
        if random.random_bool(0.5) {
          let s = TestUtil::random_simple_string(&mut random);
          new_bytes_ref_from_string(&mut random, &s)?
        } else {
          let s = TestUtil::random_realistic_unicode_string(&mut random);
          new_bytes_ref_from_string(&mut random, &s)?
        }
      } else {
        terms[random.random_range(0..terms.len())].clone()
      };

      upto = match terms.binary_search(&target) {
        Ok(pos) => pos as isize,
        Err(pos) => -(pos as isize) - 1,
      };

      if random.random_bool(0.5) {
        let status = terms_enum.seek_ceil(&target)?;

        if upto < 0 {
          upto = -(upto + 1);
          if upto >= terms.len() as isize {
            assert_eq!(SeekStatus::End, status);
            upto = -1;
          } else {
            assert_eq!(SeekStatus::NotFound, status);
            assert_eq!(
              terms[upto as usize].as_byte_slice(),
              terms_enum.term()?.as_bytes()
            );
          }
        } else {
          assert_eq!(SeekStatus::Found, status);
          assert_eq!(
            terms[upto as usize].as_byte_slice(),
            terms_enum.term()?.as_bytes()
          );
        }
      } else {
        let result = terms_enum.seek_exact(&target)?;
        if upto < 0 {
          assert!(!result);
          upto = -1;
        } else {
          assert!(result);
          assert_eq!(target.as_byte_slice(), terms_enum.term()?.as_bytes());
        }
      }
    }
  }

  r.close()?;
  d.close()?;
  docs.close();
  Ok(())
}
fn add_doc<D, R>(
  random: &mut R,
  writer: &RandomIndexWriter<D>,
  terms: &mut Vec<String>,
  term_to_id: &mut HashMap<BytesRef<Vec<u8>>, i32>,
  id: i32,
  field_to_type: &mut HashMap<String, FieldType>,
) -> Result<()>
where
  R: Rng + ?Sized,
  D: Directory + 'static,
{
  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("id", id as i64));

  for s in terms.iter() {
    doc.add(new_string_field(random, "f", s, No, field_to_type)?);
    term_to_id.insert(new_bytes_ref_from_string(random, s.as_ref())?, id);
  }

  writer.add_document(random, doc)?;
  terms.clear();
  Ok(())
}
fn accepts(c: &mut CompiledAutomaton, b: &BytesRef<Vec<u8>>) -> Result<bool> {
  let mut state: i32 = 0;

  for idx in 0..b.length {
    assert!(state != -1);
    let byte = b.bytes[b.offset + idx];
    state = c.run_automaton.as_mut().unwrap().step(state, byte as i32)?;
  }

  c.run_automaton.as_ref().unwrap().is_accept(state)
}

#[test]
fn test_intersect_random() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = RandomIndexWriter::new(&mut random, dir.clone())?;

  let num_terms = at_least_usize(&mut random, 300);

  let mut terms: HashSet<String> = HashSet::new();
  let mut pending_terms: Vec<String> = Vec::new();
  let mut term_to_id: HashMap<BytesRef<Vec<u8>>, i32> = HashMap::new();
  let mut id: i32 = 0;
  let mut field_to_type: HashMap<String, FieldType> = HashMap::new();
  while terms.len() != num_terms {
    let s = get_random_string(&mut random);
    if !terms.contains(&s) {
      terms.insert(s.clone());
      pending_terms.push(s);
      if random.random_range(0..20) == 7 {
        add_doc(
          &mut random,
          &writer,
          &mut pending_terms,
          &mut term_to_id,
          id,
          &mut field_to_type,
        )?;
        id += 1;
      }
    }
  }

  add_doc(
    &mut random,
    &writer,
    &mut pending_terms,
    &mut term_to_id,
    id,
    &mut field_to_type,
  )?;

  let mut terms_array: Vec<BytesRef<Vec<u8>>> = Vec::with_capacity(terms.len());
  let mut terms_set: HashSet<BytesRef<Vec<u8>>> = HashSet::new();

  for s in &terms {
    let b = new_bytes_ref_from_string(&mut random, s.as_str())?;
    terms_array.push(b.clone());
    terms_set.insert(b);
  }
  terms_array.sort_unstable();

  let r = writer.get_reader(&mut random)?;
  writer.close(&mut random)?;

  let max_doc = r.max_doc()?;
  let mut doc_id_to_id = vec![0i32; max_doc as usize];
  let mut values = MultiDocValues::get_numeric_values(&r, "id")?.unwrap();

  for i in 0..max_doc {
    assert_eq!(i, values.next_doc()?);
    doc_id_to_id[i as usize] = values.long_value()? as i32;
  }

  let num_iterations = at_least(&mut random, 3);
  for iter in 0..num_iterations {
    let mut accept_terms: HashSet<String> = HashSet::new();
    let mut sorted_accept_terms: BTreeSet<BytesRef<Vec<u8>>> = BTreeSet::new();

    let keep_pct: f64 = random.random();
    let automaton = if iter == 0 {
      Automata::make_empty()?
    } else {
      for s in &terms {
        let s2 = if random.random::<f64>() <= keep_pct {
          Cow::Borrowed(s.as_str())
        } else {
          Cow::Owned(get_random_string(&mut random))
        };
        accept_terms.insert(s2.as_ref().to_owned());
        sorted_accept_terms.insert(new_bytes_ref_from_string(&mut random, &s2)?);
      }
      let v: Vec<BytesRef<Vec<u8>>> = sorted_accept_terms.into_iter().collect();
      Automata::make_string_union(v.as_ref())?
    };

    let mut c = CompiledAutomaton::with_binary(automaton, true, false, false)?;

    let mut accept_terms_array: Vec<BytesRef<Vec<u8>>> = Vec::with_capacity(accept_terms.len());
    let mut accept_terms_set: HashSet<BytesRef<Vec<u8>>> = HashSet::new();

    for s in &accept_terms {
      let b = new_bytes_ref_from_string(&mut random, s)?;
      assert!(accepts(&mut c, &b)?);
      accept_terms_array.push(b.clone());
      accept_terms_set.insert(b);
    }
    accept_terms_array.sort();

    for _ in 0..100 {
      let start_term = if accept_terms_array.is_empty() || random.random_bool(0.5) {
        None
      } else {
        Some(&accept_terms_array[random.random_range(0..accept_terms_array.len())])
      };

      if let Some(start_term) = start_term {
        let mut state: i32 = 0;

        for idx in 0..start_term.length {
          let label = start_term.bytes[start_term.offset + idx] as i32 & 0xff;
          state = c.run_automaton.as_mut().unwrap().step(state, label)?;
          assert_ne!(state, -1);
        }
      }

      let mut te = get_terms(&r, "f")?.unwrap().intersect(&c, start_term)?;
      let mut loc = if let Some(st) = start_term {
        match terms_array.binary_search(st) {
          Ok(p) => p + 1,
          Err(p) => p,
        }
      } else {
        0
      };

      while loc < terms_array.len() && !accept_terms_set.contains(&terms_array[loc]) {
        loc += 1;
      }

      let mut postings_enum = None;
      while loc < terms_array.len() {
        let expected = &terms_array[loc];
        let actual = te.next()?;
        assert_eq!(
          expected.as_byte_slice(),
          actual.as_ref().unwrap().as_bytes()
        );

        assert_eq!(1, te.doc_freq()?);

        postings_enum = Some(TestUtil::docs(
          &mut random,
          &mut te,
          postings_enum,
          NONE as i32,
        )?);

        let pe = postings_enum.as_mut().unwrap();
        let doc_id = pe.next_doc()?;
        assert_ne!(doc_id, NO_MORE_DOCS);

        assert_eq!(
          doc_id_to_id[doc_id as usize],
          *term_to_id.get(expected).unwrap()
        );

        loop {
          loc += 1;
          if loc < terms_array.len() && !accept_terms_set.contains(&terms_array[loc]) {
            continue;
          } else {
            break;
          }
        }
      }
      assert!(te.next()?.is_none());
    }
  }
  r.close()?;
  dir.close()
}

fn make_index<R, T>(
  random: &mut R,
  terms: &[T],
) -> Result<(StandardDirectoryReader<DirEnum>, std::sync::Arc<DirEnum>)>
where
  R: Rng + ?Sized,
  T: AsRef<str>,
{
  let dir = new_directory_shared(random)?;
  let mock = MockAnalyzer::new(random);
  let iwc = new_index_writer_config_with_analyzer(random, mock)?;

  let writer = RandomIndexWriter::with_config(random, dir.clone(), iwc);
  let mut field_to_type: HashMap<String, FieldType> = HashMap::new();
  for term in terms {
    let mut doc = Document::new();
    let field = new_string_field(random, FIELD, term.as_ref(), No, &mut field_to_type)?;
    doc.add(field);
    writer.add_document(random, doc)?;
  }
  let reader = writer.get_reader(random)?;
  writer.close(random)?;
  Ok((reader, dir))
}
fn doc_freq<CR>(reader: CR, term: &str) -> Result<i32>
where
  CR: IndexReader,
{
  reader.doc_freq(&Term::from_text(FIELD, term))
}
#[test]
fn test_easy() -> Result<()> {
  let mut random = random();

  // No floor arcs:
  let (reader, dir) = make_index(
    &mut random,
    &["aa0", "aa1", "aa2", "aa3", "bb0", "bb1", "bb2", "bb3", "aa"],
  )?;

  // First term in block:
  assert_eq!(1, doc_freq(&reader, "aa0")?);

  // Scan forward to another term in same block
  assert_eq!(1, doc_freq(&reader, "aa2")?);

  assert_eq!(1, doc_freq(&reader, "aa")?);

  // Reset same block then scan forwards
  assert_eq!(1, doc_freq(&reader, "aa1")?);

  // Not found, in same block
  assert_eq!(0, doc_freq(&reader, "aa5")?);

  // Found, in same block
  assert_eq!(1, doc_freq(&reader, "aa2")?);

  // Not found in index:
  assert_eq!(0, doc_freq(&reader, "b0")?);

  // Found:
  assert_eq!(1, doc_freq(&reader, "aa2")?);

  // Found, rewind:
  assert_eq!(1, doc_freq(&reader, "aa0")?);

  // First term in block:
  assert_eq!(1, doc_freq(&reader, "bb0")?);

  // Scan forward to another term in same block
  assert_eq!(1, doc_freq(&reader, "bb2")?);

  // Reset same block then scan forwards
  assert_eq!(1, doc_freq(&reader, "bb1")?);

  // Not found, in same block
  assert_eq!(0, doc_freq(&reader, "bb5")?);

  // Found, in same block
  assert_eq!(1, doc_freq(&reader, "bb2")?);

  // Not found in index:
  assert_eq!(0, doc_freq(&reader, "b0")?);

  // Found:
  assert_eq!(1, doc_freq(&reader, "bb2")?);

  // Found, rewind:
  assert_eq!(1, doc_freq(&reader, "bb0")?);

  reader.close()?;
  dir.close()
}
#[test]
fn test_floor_blocks() -> Result<()> {
  let mut random = random();

  let terms = [
    "aa0", "aa1", "aa2", "aa3", "aa4", "aa5", "aa6", "aa7", "aa8", "aa9", "aa", "xx",
  ];

  let (reader, dir) = make_index(&mut random, &terms)?;

  // First term in first block:
  assert_eq!(1, doc_freq(&reader, "aa0")?);
  assert_eq!(1, doc_freq(&reader, "aa4")?);

  // No block
  assert_eq!(0, doc_freq(&reader, "bb0")?);

  // Second block
  assert_eq!(1, doc_freq(&reader, "aa4")?);

  // Backwards to prior floor block:
  assert_eq!(1, doc_freq(&reader, "aa0")?);

  // Forwards to last floor block:
  assert_eq!(1, doc_freq(&reader, "aa9")?);

  assert_eq!(0, doc_freq(&reader, "a")?);
  assert_eq!(1, doc_freq(&reader, "aa")?);
  assert_eq!(0, doc_freq(&reader, "a")?);
  assert_eq!(1, doc_freq(&reader, "aa")?);

  // Forwards to last floor block:
  assert_eq!(1, doc_freq(&reader, "xx")?);
  assert_eq!(1, doc_freq(&reader, "aa1")?);
  assert_eq!(0, doc_freq(&reader, "yy")?);

  assert_eq!(1, doc_freq(&reader, "xx")?);
  assert_eq!(1, doc_freq(&reader, "aa9")?);

  assert_eq!(1, doc_freq(&reader, "xx")?);
  assert_eq!(1, doc_freq(&reader, "aa4")?);

  let terms_enum = get_terms(&reader, FIELD)?.unwrap().iterator()?;
  let mut te = terms_enum;

  while te.next()?.is_some() {
    // iterate all terms
  }

  assert!(seek_exact(&mut random, &mut te, "aa1")?);
  assert_eq!(Some("aa2".to_string()), next_term(&mut te)?);

  assert!(seek_exact(&mut random, &mut te, "aa8")?);
  assert_eq!(Some("aa9".to_string()), next_term(&mut te)?);
  assert_eq!(Some("xx".to_string()), next_term(&mut te)?);

  test_random_seeks(&mut random, &reader, &terms)?;
  reader.close()?;
  dir.close()
}
fn seek_exact<R, T>(random: &mut R, te: &mut T, term: &str) -> Result<bool>
where
  R: Rng + ?Sized,
  T: TermsEnum,
{
  te.seek_exact(&new_bytes_ref_from_string::<_, Vec<u8>>(random, term)?)
}
fn next_term<T>(te: &mut T) -> Result<Option<String>>
where
  T: TermsEnum,
{
  match te.next()? {
    Some(br) => Ok(Some(br.utf8_to_string()?.to_owned())),
    None => Ok(None),
  }
}

fn get_non_exist_term<R>(random: &mut R, terms: &[BytesRef<Vec<u8>>]) -> Result<BytesRef<Vec<u8>>>
where
  R: Rng + ?Sized,
{
  loop {
    let ts = get_random_string(random);
    let t = new_bytes_ref_from_string(random, &ts)?;
    if terms.binary_search(&t).is_err() {
      return Ok(t);
    }
  }
}
fn test_random_seeks<'a, R, IR, S>(
  random: &mut R,
  reader: &'a IR,
  valid_term_strings: &[S],
) -> Result<()>
where
  R: Rng + ?Sized,
  IR: IndexReader,
  IR::ContextKind: IndexReaderContextKind<&'a IR>,
  S: AsRef<str>,
{
  let mut valid_terms: Vec<BytesRef<Vec<u8>>> = valid_term_strings
    .iter()
    .map(|s| new_bytes_ref_from_string(random, s.as_ref()))
    .collect::<Result<_>>()?;
  valid_terms.sort();

  let mut te = get_terms(reader, FIELD)?.unwrap().iterator()?;

  let end_loc: isize = -(valid_terms.len() as isize) - 1;

  let mut term_states: Vec<(&BytesRef<Vec<u8>>, _)> = Vec::new();

  for _iter in 0..(100 * random_multiplier()) {
    let (t, mut loc, term_state) = if random.random_range(0..6) == 4 {
      // pick non-existing term
      let t = get_non_exist_term(random, &valid_terms)?;
      let loc = match valid_terms.binary_search(&t) {
        Ok(p) => p as isize,
        Err(p) => -(p as isize) - 1,
      };
      (Cow::Owned(t), loc, None)
    } else if !term_states.is_empty() && random.random_range(0..4) == 1 {
      let (t, st) = &term_states[random.random_range(0..term_states.len())];
      let loc = valid_terms.binary_search(t).unwrap() as isize;

      (Cow::Borrowed(*t), loc, Some(st))
    } else {
      // pick valid term
      let idx = random.random_range(0..valid_terms.len());
      let t = Cow::Borrowed(&valid_terms[idx]);
      (t, idx as isize, None)
    };

    // seekExact or seekCeil
    let do_seek_exact = random.random_bool(0.5);
    if let Some(state) = term_state {
      te.seek_exact_with_state(&t, state)?;
    } else if do_seek_exact {
      assert_eq!(loc >= 0, te.seek_exact(&t)?);
    } else {
      let result = te.seek_ceil(&t)?;

      if loc >= 0 {
        assert_eq!(SeekStatus::Found, result);
      } else if loc == end_loc {
        assert_eq!(SeekStatus::End, result);
      } else {
        assert!(loc >= -(valid_terms.len() as isize));
        assert_eq!(SeekStatus::NotFound, result);
      }
    }

    #[allow(clippy::if_same_then_else)]
    if loc >= 0 {
      assert_eq!(t.as_bytes(), te.term()?.as_bytes());
    } else if do_seek_exact {
      continue;
    } else if loc == end_loc {
      continue;
    } else {
      loc = -loc - 1;
      assert_eq!(
        valid_terms[loc as usize].as_byte_slice(),
        te.term()?.as_bytes()
      );
    }

    // do a bunch of next()
    let mut loc = loc as usize;
    let num_next = random.random_range(0..valid_terms.len());
    for _ in 0..num_next {
      let t2 = te.next()?;
      loc += 1;
      if loc == valid_terms.len() {
        assert!(t2.is_none());
        break;
      } else {
        assert_eq!(valid_terms[loc].as_byte_slice(), t2.unwrap().as_bytes());
        if random.random_range(0..40) == 17 && term_states.len() < 100 {
          term_states.push((&valid_terms[loc], te.term_state()?));
        }
      }
    }
  }

  Ok(())
}

#[test]
fn test_zero_terms() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = RandomIndexWriter::new(&mut random, dir.clone())?;

  let mut doc = Document::new();
  let mut field_to_type = HashMap::new();
  doc.add(new_text_field(
    &mut random,
    "field",
    "one two three",
    No,
    &mut field_to_type,
  )?);

  // doc with "field2"
  let mut doc = Document::new();
  doc.add(new_text_field(
    &mut random,
    "field2",
    "one two three",
    No,
    &mut field_to_type,
  )?);
  writer.add_document(&mut random, doc)?;

  writer.commit(&mut random)?;
  writer.delete_documents_with_terms(&mut random, vec![Term::from_text("field", "one")])?;
  writer.force_merge(&mut random, 1)?;

  let reader = writer.get_reader(&mut random)?;
  writer.close(&mut random)?;

  assert_eq!(1, reader.num_docs()?);
  assert_eq!(1, reader.max_doc()?);

  if let Some(terms) = get_terms(&reader, "field")? {
    let mut te = terms.iterator()?;
    assert!(te.next()?.is_none());
  }

  reader.close()?;
  dir.close()
}
fn get_random_string<R>(random: &mut R) -> String
where
  R: Rng + ?Sized,
{
  TestUtil::random_realistic_unicode_string(random)
}
#[test]
fn test_random_terms() -> Result<()> {
  let mut random = random();
  let upper = at_least_usize(&mut random, 1000);
  let terms_len = TestUtil::next_usize(&mut random, 1, upper);
  let mut terms: Vec<String> = Vec::with_capacity(terms_len);
  let mut seen: HashSet<String> = HashSet::with_capacity(terms_len);

  let allow_empty_string = random.random_bool(0.5);

  if random.random_range(0..10) == 7 && terms_len > 2 {
    // Sometimes add a bunch of terms sharing a long-ish common prefix.
    let num_terms_same_prefix = random.random_range(0..(terms_len / 2));
    if num_terms_same_prefix > 0 {
      let prefix = loop {
        let p = get_random_string(&mut random);
        if p.len() >= 5 {
          break p;
        }
      };

      while seen.len() < num_terms_same_prefix {
        let t = format!("{}{}", prefix, get_random_string(&mut random));
        if seen.insert(t.clone()) {
          terms.push(t);
        }
      }
    }
  }

  while seen.len() < terms_len {
    let t = get_random_string(&mut random);
    if !seen.contains(&t) && (allow_empty_string || !t.is_empty()) {
      seen.insert(t.clone());
      terms.push(t);
    }
  }

  let (reader, dir) = make_index(&mut random, &terms)?;
  test_random_seeks(&mut random, &reader, &terms)?;
  reader.close()?;
  dir.close()
}
#[test]
fn test_intersect_basic() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  iwc.set_merge_policy(LogMergePolicy::log_doc());
  let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

  let mut field_to_type = HashMap::new();
  let mut doc = Document::new();
  doc.add(new_text_field(
    &mut random,
    "field",
    "aaa",
    No,
    &mut field_to_type,
  )?);
  writer.add_document(&mut random, doc)?;

  let mut doc = Document::new();
  doc.add(new_text_field(
    &mut random,
    "field",
    "bbb",
    No,
    &mut field_to_type,
  )?);
  writer.add_document(&mut random, doc)?;

  let mut doc = Document::new();
  doc.add(new_text_field(
    &mut random,
    "field",
    "ccc",
    No,
    &mut field_to_type,
  )?);
  writer.add_document(&mut random, doc)?;

  writer.force_merge(&mut random, 1)?;

  let reader = writer.get_reader(&mut random)?;
  writer.close(&mut random)?;

  let sub = get_only_leaf_reader(&reader)?;
  let terms = sub.terms("field")?.expect("terms must exist");

  let automaton = RegExp::from_str_with_flags(".*", RegExp::NONE)?.to_automaton()?;
  let ca = CompiledAutomaton::new(automaton, false, false)?;

  let mut te = terms.intersect(&ca, None)?;
  assert_eq!("aaa", te.next()?.unwrap().utf8_to_string()?);
  assert_eq!(0, te.postings_with_flags(None, NONE.into())?.next_doc()?);
  assert_eq!("bbb", te.next()?.unwrap().utf8_to_string()?);
  assert_eq!(1, te.postings_with_flags(None, NONE.into())?.next_doc()?);
  assert_eq!("ccc", te.next()?.unwrap().utf8_to_string()?);
  assert_eq!(2, te.postings_with_flags(None, NONE.into())?.next_doc()?);
  assert!(te.next()?.is_none());

  let mut te = terms.intersect(&ca, Some(&BytesRef::from_string("abc")))?;
  assert_eq!("bbb", te.next()?.unwrap().utf8_to_string()?);
  assert_eq!(1, te.postings_with_flags(None, NONE.into())?.next_doc()?);
  assert_eq!("ccc", te.next()?.unwrap().utf8_to_string()?);
  assert_eq!(2, te.postings_with_flags(None, NONE.into())?.next_doc()?);
  assert!(te.next()?.is_none());

  let mut te = terms.intersect(&ca, Some(&BytesRef::from_string("aaa")))?;
  assert_eq!("bbb", te.next()?.unwrap().utf8_to_string()?);
  assert_eq!(1, te.postings_with_flags(None, NONE.into())?.next_doc()?);
  assert_eq!("ccc", te.next()?.unwrap().utf8_to_string()?);
  assert_eq!(2, te.postings_with_flags(None, NONE.into())?.next_doc()?);
  assert!(te.next()?.is_none());
  reader.close()?;
  dir.close()
}
#[test]
fn test_intersect_start_term() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  iwc.set_merge_policy(LogMergePolicy::log_doc());
  let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

  let mut field_to_type = HashMap::new();

  let mut doc = Document::new();
  doc.add(new_string_field(
    &mut random,
    "field",
    "abc",
    No,
    &mut field_to_type,
  )?);
  writer.add_document(&mut random, doc)?;

  let mut doc = Document::new();
  doc.add(new_string_field(
    &mut random,
    "field",
    "abd",
    No,
    &mut field_to_type,
  )?);
  writer.add_document(&mut random, doc)?;

  let mut doc = Document::new();
  doc.add(new_string_field(
    &mut random,
    "field",
    "acd",
    No,
    &mut field_to_type,
  )?);
  writer.add_document(&mut random, doc)?;

  let mut doc = Document::new();
  doc.add(new_string_field(
    &mut random,
    "field",
    "bcd",
    No,
    &mut field_to_type,
  )?);
  writer.add_document(&mut random, doc)?;

  writer.force_merge(&mut random, 1)?;

  let reader = writer.get_reader(&mut random)?;
  writer.close(&mut random)?;

  let sub = get_only_leaf_reader(&reader)?;
  let terms = sub.terms("field")?.expect("terms must exist");

  let automaton = RegExp::from_str_with_flags(".*d", RegExp::NONE)?.to_automaton()?;
  let v = match Operations::determinize(&automaton, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)? {
    Cow::Borrowed(_) => automaton,
    Cow::Owned(v) => v,
  };

  let ca = CompiledAutomaton::new(v, false, false)?;

  // should seek to startTerm
  let mut te = terms.intersect(&ca, Some(&BytesRef::from_string("aad")))?;
  assert_eq!("abd", te.next()?.unwrap().utf8_to_string()?);
  assert_eq!(1, te.postings_with_flags(None, NONE.into())?.next_doc()?);
  assert_eq!("acd", te.next()?.unwrap().utf8_to_string()?);
  assert_eq!(2, te.postings_with_flags(None, NONE.into())?.next_doc()?);
  assert_eq!("bcd", te.next()?.unwrap().utf8_to_string()?);
  assert_eq!(3, te.postings_with_flags(None, NONE.into())?.next_doc()?);
  assert!(te.next()?.is_none());

  // should fail to find ceil label on second arc, rewind
  let mut te = terms.intersect(&ca, Some(&BytesRef::from_string("add")))?;
  assert_eq!("bcd", te.next()?.unwrap().utf8_to_string()?);
  assert_eq!(3, te.postings_with_flags(None, NONE.into())?.next_doc()?);
  assert!(te.next()?.is_none());

  // should reach end
  let mut te = terms.intersect(&ca, Some(&BytesRef::from_string("bcd")))?;
  assert!(te.next()?.is_none());

  let mut te = terms.intersect(&ca, Some(&BytesRef::from_string("ddd")))?;
  assert!(te.next()?.is_none());

  reader.close()?;
  dir.close()
}
#[test]
fn test_intersect_empty_string() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  iwc.set_merge_policy(LogMergePolicy::log_doc());
  let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

  let mut field_to_type = HashMap::new();

  let mut doc = Document::new();
  doc.add(new_string_field(
    &mut random,
    "field",
    "",
    No,
    &mut field_to_type,
  )?);
  doc.add(new_string_field(
    &mut random,
    "field",
    "abc",
    No,
    &mut field_to_type,
  )?);
  writer.add_document(&mut random, doc)?;

  let mut doc = Document::new();
  doc.add(new_string_field(
    &mut random,
    "field",
    "abc",
    No,
    &mut field_to_type,
  )?);
  doc.add(new_string_field(
    &mut random,
    "field",
    "",
    No,
    &mut field_to_type,
  )?);
  writer.add_document(&mut random, doc)?;

  writer.force_merge(&mut random, 1)?;

  let reader = writer.get_reader(&mut random)?;
  writer.close(&mut random)?;

  let sub = get_only_leaf_reader(&reader)?;
  let terms = sub.terms("field")?.expect("terms must exist");

  let automaton = RegExp::from_str_with_flags(".*", RegExp::NONE)?.to_automaton()?;
  let ca = CompiledAutomaton::new(automaton, false, false)?;

  let mut te = terms.intersect(&ca, None)?;
  let mut de;

  assert_eq!("", te.next()?.unwrap().utf8_to_string()?);
  de = te.postings_with_flags(None, NONE.into())?;
  assert_eq!(0, de.next_doc()?);
  assert_eq!(1, de.next_doc()?);

  assert_eq!("abc", te.next()?.unwrap().utf8_to_string()?);
  de = te.postings_with_flags(None, NONE.into())?;
  assert_eq!(0, de.next_doc()?);
  assert_eq!(1, de.next_doc()?);

  assert!(te.next()?.is_none());

  // pass empty string as start term
  let mut te = terms.intersect(&ca, Some(&BytesRef::from_string("")))?;
  assert_eq!("abc", te.next()?.unwrap().utf8_to_string()?);
  de = te.postings_with_flags(None, NONE.into())?;
  assert_eq!(0, de.next_doc()?);
  assert_eq!(1, de.next_doc()?);

  assert!(te.next()?.is_none());

  reader.close()?;
  dir.close()
}
#[test]
fn test_common_prefix_terms() -> Result<()> {
  let mut random = random();
  let d = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, d.clone())?;
  let mut terms: HashSet<String> = HashSet::new();
  let prefix = TestUtil::random_realistic_unicode_string_range(&mut random, 1, 20);
  let num_terms = at_least_usize(&mut random, 100);

  while terms.len() < num_terms {
    terms.insert(format!(
      "{}{}",
      prefix,
      TestUtil::random_realistic_unicode_string_range(&mut random, 1, 20)
    ));
  }

  let mut field_to_type = HashMap::new();
  for term in &terms {
    let mut doc = Document::new();
    doc.add(new_string_field(
      &mut random,
      "id",
      term,
      Yes,
      &mut field_to_type,
    )?);
    w.add_document(&mut random, doc)?;
  }

  let r = w.get_reader(&mut random)?;
  let mut terms_enum = get_terms(&r, "id")?.unwrap().iterator()?;
  let mut postings_enum = None;
  let context = (&r).get_context()?;
  let mut pk_lookup = PerThreadPKLookup::new(&context, "id")?;
  let mut stored_fields = r.stored_fields()?;

  let iters = at_least_usize(&mut random, num_terms * 3);
  let terms_list: Vec<&str> = terms.iter().map(String::as_str).collect();
  for _iter in 0..iters {
    let term;
    let should_exist;
    if random.random_bool(0.5) {
      term = Cow::Borrowed(terms_list[random.random_range(0..terms.len())]);
      should_exist = true;
    } else {
      term = Cow::Owned(format!(
        "{}{}",
        prefix,
        TestUtil::random_simple_string_range(&mut random, 1, 20)
      ));
      should_exist = terms.contains(term.as_ref());
    }

    let term_bytes_ref = BytesRef::from_string(&term);

    let actual_result = terms_enum.seek_exact(&term_bytes_ref)?;
    assert_eq!(should_exist, actual_result);
    if should_exist {
      postings_enum = Some(terms_enum.postings_with_flags(postings_enum, NONE as i32)?);
      let doc_id = postings_enum.as_mut().unwrap().next_doc()?;
      assert_ne!(doc_id, NO_MORE_DOCS);
      assert_eq!(doc_id, pk_lookup.lookup(&term_bytes_ref)?);
      let doc = stored_fields.document(doc_id)?;
      assert_eq!(term.as_ref(), doc.get("id")?.unwrap().as_ref());

      if random.random_range(0..7) == 1 {
        terms_enum.next()?;
      }
    } else {
      assert_eq!(-1, pk_lookup.lookup(&term_bytes_ref)?);
    }

    if random.random_range(0..7) == 1 {
      let status = terms_enum.seek_ceil(&term_bytes_ref)?;
      if should_exist {
        assert_eq!(SeekStatus::Found, status);
      } else {
        assert_ne!(SeekStatus::Found, status);
      }
    }
  }

  r.close()?;
  w.close(&mut random)?;
  d.close()
}
#[cfg(feature = "nightly")]
#[test]
#[ignore = "nightly"]
fn test_varying_terms_per_segment() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut terms: HashSet<BytesRef<Vec<u8>>> = HashSet::new();
  let max_terms = at_least_usize(&mut random, 1000);
  while terms.len() < max_terms {
    let term = TestUtil::random_simple_string_range(&mut random, 1, 40);
    terms.insert(BytesRef::from_string(&term));
  }

  let terms_list: Vec<BytesRef<Vec<u8>>> = terms.into_iter().collect();
  let mut text = String::new();
  let mut field_to_type = HashMap::new();
  for term_count in 0..max_terms {
    text.push(' ');
    text.push_str(&terms_list[term_count].utf8_to_string()?);

    let mock = MockAnalyzer::new(&mut random);
    let mut iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
    iwc.set_open_mode(OpenMode::Create);
    let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

    let mut doc = Document::new();
    doc.add(new_text_field(
      &mut random,
      "field",
      text.as_str(),
      No,
      &mut field_to_type,
    )?);
    writer.add_document(&mut random, doc)?;

    let reader = writer.get_reader(&mut random)?;
    let context = (&reader).get_context()?;
    let leaves = context.leaves()?;
    assert_eq!(1, leaves.len());
    let terms = leaves[0]
      .reader()
      .terms("field")?
      .expect("terms must exist");
    let mut te = terms.iterator()?;

    for term in terms_list.iter().take(term_count + 1) {
      assert!(
        te.seek_exact(term)?,
        "term '{}' should exist but doesn't",
        term.utf8_to_string()?
      );
    }
    for term in terms_list.iter().skip(term_count + 1) {
      assert!(
        !te.seek_exact(term)?,
        "term '{term}' shouldn't exist but does"
      );
    }

    reader.close()?;
    writer.close(&mut random)?;
  }
  dir.close()
}
#[test]
fn test_intersect_regexp() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = RandomIndexWriter::new(&mut random, dir.clone())?;

  let mut field_to_type = HashMap::new();
  let mut doc = Document::new();
  doc.add(new_string_field(
    &mut random,
    "field",
    "foobar",
    No,
    &mut field_to_type,
  )?);
  writer.add_document(&mut random, doc)?;

  let reader = writer.get_reader(&mut random)?;
  let terms = get_terms(&reader, "field")?.expect("terms must exist");

  let automaton = RegExp::from_string("do_not_match_anything")?.to_automaton()?;
  let ca = CompiledAutomaton::from_automaton(automaton)?;

  let err = terms.intersect(&ca, None);
  assert!(matches!(
    err.as_ref().map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  if let Err(LuceneErrorKind::IllegalArgument(msg)) = err.as_ref().map_err(LuceneError::kind) {
    assert_eq!(
      "please use CompiledAutomaton.getTermsEnum instead",
      msg.to_string()
    );
  }

  reader.close()?;
  writer.close(&mut random)?;
  dir.close()
}
#[test]
fn test_invalid_automaton_terms_enum() -> Result<()> {
  let automaton = Automata::make_string("foo")?;
  let ca = CompiledAutomaton::from_automaton(automaton)?;

  let err = AutomatonTermsEnum::new(EmptyTermsEnum, &ca);
  assert!(matches!(
    err.as_ref().map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  Ok(())
}
