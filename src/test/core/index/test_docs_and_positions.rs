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
use crate::core::document::field::Store::No;
use crate::core::document::field::{Field, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::string_field::StringField;
use crate::core::index::BytesRef;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader::{LRPosting, LeafReader};
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::postings_enum::{ALL, FREQS, NONE, PostingsEnum, PostingsEnumEnum2};
use crate::core::index::single_leaf_composite_reader::SingleLeafCompositeReader;
use crate::core::index::term::Term;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::util::TryIntoInt;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, at_least_usize, get_only_leaf_reader, new_bytes_ref_from_string, new_directory_shared,
  new_field, new_index_writer_config, new_index_writer_config_with_analyzer, new_log_merge_policy,
  new_text_field, random,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::Rng;
use rand::RngExt;
use std::collections::HashMap;
use std::fmt::Write as _;

#[allow(dead_code)] // for quick search
pub struct TestDocsAndPositions;

fn field_name<R>(random: &mut R) -> String
where
  R: Rng + ?Sized,
{
  let v: i32 = random.random();
  format!("field{}", v)
}
/// Simple testcase for ``[`PostingsEnum`]
#[test]
fn test_positions_simple() -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = RandomIndexWriter::with_config(&mut random, directory, config);
  let field_name = field_name(&mut random);
  let mut field_types = HashMap::new();

  for _ in 0..39 {
    let mut doc = Document::new();
    let mut custom_type =
      FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
    custom_type.set_omit_norms(true)?;

    let text = concat!(
      "1 2 3 4 5 6 7 8 9 10 ",
      "1 2 3 4 5 6 7 8 9 10 ",
      "1 2 3 4 5 6 7 8 9 10 ",
      "1 2 3 4 5 6 7 8 9 10"
    );

    doc.add(new_field(
      &mut random,
      &field_name,
      text,
      &custom_type,
      &mut field_types,
    )?);
    writer.add_document(&mut random, doc)?;
  }

  let reader = writer.get_reader(&mut random)?;
  writer.close(&mut random)?;

  let num = at_least(&mut random, 13);
  for _ in 0..num {
    let bytes = new_bytes_ref_from_string(&mut random, "1")?;
    let top_reader_context = (&reader).get_context()?;

    for leaf_reader_context in top_reader_context.leaves()? {
      let leaf_reader = leaf_reader_context.reader();

      let mut docs_and_pos_enum = get_docs_and_positions(leaf_reader, &field_name, &bytes)?
        .expect("postings enum must exist");
      let max_doc = leaf_reader.max_doc()?;
      if max_doc == 0 {
        continue;
      }

      let target = random.random_range(0..max_doc);
      let advance_doc = docs_and_pos_enum.advance(target)?;

      loop {
        let msg = format!(
          "Advanced to {} current doc {}",
          advance_doc,
          docs_and_pos_enum.doc_id()
        );

        assert_eq!(docs_and_pos_enum.freq()?, 4, "{msg}");
        assert_eq!(docs_and_pos_enum.next_position()?, 0, "{msg}");

        assert_eq!(docs_and_pos_enum.freq()?, 4, "{msg}");
        assert_eq!(docs_and_pos_enum.next_position()?, 10, "{msg}");

        assert_eq!(docs_and_pos_enum.freq()?, 4, "{msg}");
        assert_eq!(docs_and_pos_enum.next_position()?, 20, "{msg}");

        assert_eq!(docs_and_pos_enum.freq()?, 4, "{msg}");
        assert_eq!(docs_and_pos_enum.next_position()?, 30, "{msg}");

        if docs_and_pos_enum.next_doc()? == NO_MORE_DOCS {
          break;
        }
      }
    }
  }
  Ok(())
}
fn get_docs_and_positions<LR>(
  reader: &LR,
  field_name: &str,
  bytes: &BytesRef<Vec<u8>>,
) -> Result<Option<LRPosting<LR>>>
where
  LR: LeafReader,
{
  let terms_opt = reader.terms(field_name)?;
  let terms = match terms_opt {
    None => return Ok(None),
    Some(t) => t,
  };

  let mut te = terms.iterator()?;

  if te.seek_exact(bytes)? {
    let pe = te.postings_with_flags(None, ALL as i32)?;
    Ok(Some(pe))
  } else {
    Ok(None)
  }
}

/// this test indexes random numbers within a range into a field and checks their occurrences by
/// searching for a number from that range selected at random. All positions for that number are
/// saved up front and compared to the enums positions.
#[test]
fn test_random_positions() -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  config.set_merge_policy(new_log_merge_policy(&mut random)?);
  let writer = RandomIndexWriter::with_config(&mut random, directory.clone(), config);

  let field_name = field_name(&mut random);
  let mut field_types: HashMap<String, FieldType> = HashMap::new();

  let num_docs = at_least_usize(&mut random, 47);
  let max = 1051;
  let term: i32 = random.random_range(0..max);

  let mut positions_in_doc: Vec<Vec<i32>> = vec![Vec::new(); num_docs];

  let mut custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  custom_type.set_omit_norms(true)?;

  for doc_positions in &mut positions_in_doc {
    let mut doc = Document::new();
    let mut positions = Vec::new();

    let num = at_least(&mut random, 131);

    let mut builder = String::new();
    for j in 0..num {
      let next_int: i32 = random.random_range(0..max);
      write!(builder, "{} ", next_int)?;
      if next_int == term {
        positions.push(j);
      }
    }

    if positions.is_empty() {
      write!(builder, "{}", term)?;
      positions.push(num);
    }

    doc.add(new_field(
      &mut random,
      &field_name,
      builder,
      &custom_type,
      &mut field_types,
    )?);
    *doc_positions = positions;

    writer.add_document(&mut random, doc)?;
  }

  let reader = writer.get_reader(&mut random)?;
  writer.close(&mut random)?;

  let num_outer = at_least(&mut random, 13);

  for i in 0..num_outer {
    let bytes = new_bytes_ref_from_string(&mut random, &format!("{}", term))?;
    let top_reader_context = (&reader).get_context()?;

    for leaf_ctx in top_reader_context.leaves()? {
      let leaf_reader = leaf_ctx.reader();
      let mut docs_and_pos_enum = get_docs_and_positions(leaf_reader, &field_name, &bytes)?
        .expect("postings enum must exist");

      let max_doc = leaf_reader.max_doc()?;
      if max_doc == 0 {
        continue;
      }
      // initially advance or do next doc
      let init_doc = if random.random_bool(0.5) {
        docs_and_pos_enum.next_doc()?
      } else {
        docs_and_pos_enum.advance(random.random_range(0..max_doc))?
      };
      // now run through the scorer and check if all positions are there...
      loop {
        let doc_id = docs_and_pos_enum.doc_id();
        if doc_id == NO_MORE_DOCS {
          break;
        }

        let global_doc = leaf_ctx.doc_base + doc_id.try_convert()?;
        let pos = &positions_in_doc[global_doc];

        assert_eq!(pos.len() as i32, docs_and_pos_enum.freq()?,);

        let read_all = random.random_range(0..20) != 0;
        // number of positions read should be random - don't read all of them
        // allways
        let how_many = if read_all {
          pos.len()
        } else {
          let remain = pos.len();
          remain - random.random_range(0..remain)
        };

        for j in 0..how_many {
          let expected = pos[j];
          let actual = docs_and_pos_enum.next_position()?;
          assert_eq!(
            expected, actual,
            "iteration {i}, initDoc={init_doc}, doc={doc_id}, base={}, positions={:?}",
            leaf_ctx.doc_base, pos
          );
        }

        if random.random_range(0..10) == 0 {
          // once is a while advance
          let advance_target = doc_id + 1 + random.random_range(0..(max_doc - doc_id));
          if docs_and_pos_enum.advance(advance_target)? == NO_MORE_DOCS {
            break;
          }
        }

        if docs_and_pos_enum.next_doc()? == NO_MORE_DOCS {
          break;
        }
      }
    }
  }

  Ok(())
}
#[test]
fn test_random_docs() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  iwc.set_merge_policy(new_log_merge_policy(&mut random)?);
  let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

  let num_docs = at_least_usize(&mut random, 49);
  let max = 15678;
  let term = random.random_range(0..max);
  let mut freq_in_doc = vec![0i32; num_docs];

  let mut custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  custom_type.set_omit_norms(true)?;

  let field_name = field_name(&mut random);

  for freq in freq_in_doc.iter_mut().take(num_docs) {
    let mut doc = Document::new();
    let mut builder = String::new();
    for _ in 0..199 {
      let next_int = random.random_range(0..max);
      builder.push_str(&next_int.to_string());
      builder.push(' ');
      if next_int == term {
        *freq += 1;
      }
    }
    doc.add(Field::new(field_name.clone(), builder, custom_type.clone()));
    writer.add_document(&mut random, doc)?;
  }

  let reader = writer.get_reader(&mut random)?;
  writer.close(&mut random)?;

  let num = at_least(&mut random, 13);
  for i in 0..num {
    let bytes = BytesRef::from_string(&term.to_string());
    let top_reader_context = (&reader).get_context()?;
    for context in top_reader_context.leaves()? {
      let max_doc = context.reader().max_doc()? as usize;
      let reader = SingleLeafCompositeReader::new(context.reader());
      let mut postings_enum = TestUtil::docs_with_reader(
        &mut random,
        &reader,
        &field_name,
        &bytes,
        None,
        FREQS as i32,
      )?;

      if find_next(&freq_in_doc, context.doc_base, context.doc_base + max_doc) == i32::MAX as usize
      {
        assert!(postings_enum.is_none());
        continue;
      }

      let postings_enum = postings_enum.as_mut().unwrap();
      assert_ne!(postings_enum.next_doc()?, NO_MORE_DOCS);

      for j in 0..max_doc {
        if freq_in_doc[context.doc_base + j] != 0 {
          assert_eq!(j, postings_enum.doc_id() as usize);
          assert_eq!(postings_enum.freq()?, freq_in_doc[context.doc_base + j]);

          if i % 2 == 0 && random.random_range(0..10) == 0 {
            let next = find_next(
              &freq_in_doc,
              context.doc_base + j + 1,
              context.doc_base + max_doc,
            ) - context.doc_base;

            let advanced_to = postings_enum.advance(next as i32)?;
            if next >= max_doc {
              assert_eq!(NO_MORE_DOCS, advanced_to);
            } else {
              assert!(
                next >= advanced_to as usize,
                "advanced to: {} but should be <= {}",
                advanced_to,
                next
              );
            }
          } else {
            postings_enum.next_doc()?;
          }
        }
      }

      assert_eq!(
        NO_MORE_DOCS,
        postings_enum.doc_id(),
        "docBase: {} maxDoc: {}",
        context.doc_base,
        max_doc
      );
    }
  }

  Ok(())
}
fn find_next(docs: &[i32], pos: usize, max: usize) -> usize {
  if let Some(i) = docs[pos..max].iter().position(|&x| x != 0) {
    return pos + i;
  }
  i32::MAX as usize
}
/// tests retrieval of positions for terms that have a large number of occurrences to force test of
//  buffer refill during positions iteration.
#[test]
fn test_large_number_of_positions() -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;

  let mock = MockAnalyzer::new(&mut random);
  let config = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = RandomIndexWriter::with_config(&mut random, directory.clone(), config);

  let field_name = field_name(&mut random);
  let mut field_types: HashMap<String, FieldType> = HashMap::new();

  let how_many = 1000;

  let mut custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  custom_type.set_omit_norms(true)?;

  for _i in 0..39 {
    let mut doc = Document::new();
    let mut builder = String::new();

    for j in 0..how_many {
      if j % 2 == 0 {
        builder.push_str("even ");
      } else {
        builder.push_str("odd ");
      }
    }

    doc.add(new_field(
      &mut random,
      &field_name,
      builder,
      &custom_type,
      &mut field_types,
    )?);
    writer.add_document(&mut random, doc)?;
  }
  // now do searches
  let reader = writer.get_reader(&mut random)?;
  writer.close(&mut random)?;

  let num_outer = at_least(&mut random, 13);

  for i in 0..num_outer {
    let bytes = new_bytes_ref_from_string(&mut random, "even")?;
    let top_reader_context = (&reader).get_context()?;

    for leaf_ctx in top_reader_context.leaves()? {
      let leaf_reader = leaf_ctx.reader();

      let mut docs_and_pos_enum = get_docs_and_positions(leaf_reader, &field_name, &bytes)?
        .expect("postings enum must exist");

      let max_doc = leaf_reader.max_doc()?;
      if max_doc == 0 {
        continue;
      }

      // initially advance or do next doc
      let init_doc = if random.random_bool(0.5) {
        docs_and_pos_enum.next_doc()?
      } else {
        docs_and_pos_enum.advance(random.random_range(0..max_doc))?
      };

      let msg = format!("Iteration: {} initDoc: {}", i, init_doc);

      assert_eq!(how_many / 2, docs_and_pos_enum.freq()?, "{msg}");

      for j in (0..how_many).step_by(2) {
        let pos = docs_and_pos_enum.next_position()?;
        assert_eq!(
          j,
          pos,
          "position missmatch index: {} with freq: {} -- {}",
          j,
          docs_and_pos_enum.freq()?,
          msg
        );
      }
    }
  }

  Ok(())
}
#[test]
fn test_docs_enum_start() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = RandomIndexWriter::new(&mut random, dir.clone())?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("foo", "bar", Store::No)?);
  writer.add_document(&mut random, doc)?;

  let reader = writer.get_reader(&mut random)?;
  let r = get_only_leaf_reader(&reader)?;
  let cr = SingleLeafCompositeReader::new(&r);
  let mut disi = TestUtil::docs_with_reader(
    &mut random,
    &cr,
    "foo",
    &BytesRef::from_string("bar"),
    None,
    NONE as i32,
  )?
  .unwrap();
  let doc_id = disi.doc_id();
  assert_eq!(-1, doc_id);
  assert_ne!(disi.next_doc()?, NO_MORE_DOCS);
  let disi = match disi {
    PostingsEnumEnum2::A(v) => v,
    PostingsEnumEnum2::B(_) => unreachable!(),
  };

  let mut te = r.terms("foo")?.unwrap().iterator()?;
  assert!(te.seek_exact(&BytesRef::<Vec<u8>>::from_string("bar"))?);
  let mut disi = TestUtil::docs(&mut random, &mut te, Some(disi), NONE as i32)?;
  let docid = disi.doc_id();
  assert_eq!(-1, docid);
  assert_ne!(disi.next_doc()?, NO_MORE_DOCS);

  writer.close(&mut random)?;
  r.close()?;
  Ok(())
}
#[test]
fn test_docs_and_positions_enum_start() -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;

  let config = new_index_writer_config(&mut random)?;
  let writer = RandomIndexWriter::with_config(&mut random, directory.clone(), config);

  let mut doc = Document::new();
  let mut field_types = HashMap::new();
  doc.add(new_text_field(
    &mut random,
    "foo",
    "bar",
    No,
    &mut field_types,
  )?);
  writer.add_document(&mut random, doc)?;

  let reader = writer.get_reader(&mut random)?;
  writer.close(&mut random)?;

  let r = get_only_leaf_reader(reader)?;

  let term = Term::from_text("foo", "bar");
  let mut disi = r.postings_with_flag(&term, ALL as i32)?.unwrap();
  let docid = disi.doc_id();
  assert_eq!(-1, docid);

  let next = disi.next_doc()?;
  assert_ne!(next, NO_MORE_DOCS);
  // now reuse and check again
  let terms = r.terms("foo")?.unwrap();
  let mut te = terms.iterator()?;

  assert!(te.seek_exact(&new_bytes_ref_from_string::<_, Vec<u8>>(
    &mut random,
    "bar"
  )?)?);
  let mut disi = te.postings_with_flags(Some(disi), ALL as i32)?;

  let docid = disi.doc_id();
  assert_eq!(-1, docid);

  let next2 = disi.next_doc()?;
  assert_ne!(next2, NO_MORE_DOCS);

  Ok(())
}
