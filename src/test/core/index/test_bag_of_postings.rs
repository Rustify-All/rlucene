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
use crate::core::document::field::Field;
use crate::core::document::field_type::FieldType;
use crate::core::document::fields::Fields;
use crate::core::index::BytesRefValue;
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::merge_policy::MergePolicyEnum;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, create_temp_dir_with_prefix, is_night_mode, new_fs_directory,
  new_index_writer_config_with_analyzer, random, random_from_seed, random_multiplier,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::RngExt;
use rand::seq::SliceRandom;
use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;

/// Simple test that adds numeric terms, where each term has the docFreq of its integer value, and
/// checks that the docFreq is correct.
#[allow(dead_code)] // for quick search
pub struct TestBagOfPostings;

#[test]
fn test() -> Result<()> {
  let mut random = random();
  let mut postings_list: Vec<String> = Vec::new();
  let mut num_terms = at_least(&mut random, 300);
  let max_terms_per_doc = TestUtil::next_int(&mut random, 10, 20);
  let analyzer = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  if matches!(&*iwc.get_merge_policy(), MergePolicyEnum::MockRandom(_))
    && (is_night_mode() || random_multiplier() > 1)
  {
    // Otherwise test can take way too long (> 2 hours).
    num_terms /= 2;
  }

  for i in 0..num_terms {
    let term = i.to_string();
    for _ in 0..i {
      postings_list.push(term.clone());
    }
  }
  postings_list.shuffle(&mut random);

  let postings = Arc::new(Mutex::new(VecDeque::from(postings_list)));
  let dir = new_fs_directory(&mut random, create_temp_dir_with_prefix("bagofpostings")?)?;

  let iw = Arc::new(RandomIndexWriter::with_config(&mut random, dir, iwc));

  let thread_count = TestUtil::next_usize(&mut random, 1, 5);
  let field_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_NOT_STORED)?;
  let barrier = Arc::new(Barrier::new(thread_count + 1));
  let mut handles = Vec::new();

  for _thread_id in 0..thread_count {
    let postings = postings.clone();
    let iw = iw.clone();
    let barrier = barrier.clone();
    let field_type = field_type.clone();
    let seed = random.random();

    handles.push(thread::spawn(move || {
      let mut thread_random = random_from_seed(seed);
      let mut doc = Document::new();
      doc.add(Field::new("field", "", field_type.clone()));
      let _ = barrier.wait();

      loop {
        {
          let queue = postings.lock().unwrap();
          if queue.is_empty() {
            break;
          }
        }

        let mut text = String::new();
        let mut visited = HashSet::new();
        for _ in 0..max_terms_per_doc {
          let token = {
            let mut queue = postings.lock().unwrap();
            queue.pop_front()
          };
          let token = match token {
            Some(t) => t,
            None => break,
          };

          if !visited.insert(token.clone()) {
            let mut queue = postings.lock().unwrap();
            queue.push_back(token);
            break;
          }

          text.push(' ');
          text.push_str(&token);
        }

        let Some(Fields::Field(field)) = doc.get_field_mut("field") else {
          unreachable!("field must exist")
        };
        if let Err(e) = field.set_string_value(text) {
          panic!("field value failed: {:?}", e);
        }
        if let Err(e) = iw.add_document(&mut thread_random, &mut doc) {
          panic!("thread indexing failed: {:?}", e);
        }
      }
    }));
  }

  let _ = barrier.wait();

  for handle in handles {
    handle.join().expect("thread panicked");
  }

  iw.force_merge(&mut random, 1)?;
  let ir = iw.get_reader(&mut random)?;
  let top_reader_context = (&ir).get_context()?;
  let leaves = top_reader_context.leaves()?;
  assert_eq!(1, leaves.len());
  let leaf_reader = leaves[0].reader();
  let terms_opt = leaf_reader.terms("field")?;
  let terms = terms_opt.expect("terms must exist");
  // numTerms-1 because there cannot be a term 0 with 0 postings:
  assert_eq!((num_terms - 1) as i64, terms.size()?);
  let mut terms_enum = terms.iterator()?;
  loop {
    let term = terms_enum.next()?;
    let term = match term {
      Some(t) => t,
      None => break,
    };
    let value: i32 = term.utf8_to_string()?.parse()?;
    assert_eq!(value, terms_enum.doc_freq()?);
  }

  drop(ir);
  iw.close(&mut random)?;

  Ok(())
}
