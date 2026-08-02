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
use crate::core::codecs::stored_fields_writer::StoredFieldsWriter;
use crate::core::document::document::Document;
use crate::core::document::field::{Field, FieldBase, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::int_point::IntPoint;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::stored_field::StoredField;
use crate::core::document::string_field::StringField;
use crate::core::document::text_field::TYPE_STORED;
use crate::core::index::BytesRef;
use crate::core::index::base_composite_reader::{
  BCRStoredFieldsImpl, BCRTermVectorsImpl, BaseCompositeReader, BaseCompositeReaderBase,
};
use crate::core::index::composite_reader::CompositeReader;
use crate::core::index::directory_reader::{self, DirectoryReader, DirectoryReaderBase};
use crate::core::index::filter_directory_reader::{FilterDirectoryReader, SubReaderWrapper};
use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_reader::{
  CompositeReaderContextKind, IndexReader, IndexReaderBase, LeafReaderContextKind,
};
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::indexable_field::IndexableField;
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::index::leaf_metadata::LeafMetaData;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::stored_field_visitor::StoredFieldVisitor;
use crate::core::index::stored_fields::{RawStoredFieldsReader, StoredFields};
use crate::core::index::term::Term;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::search::sort::Sort;
use crate::core::search::sort_field::{SortField, SortFieldType};
use crate::core::search::term_query::TermQuery;
use crate::core::store::directory::DirEnum;
use crate::core::util::bits::Bits;
use crate::core::util::close::CloseableRef;
use crate::core::util::dummy::dummy_comparator::DummyComparator;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::io_utils::IOUtils;
use crate::core::util::number::Number;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::base_index_file_format_test_case::{
  BaseIndexFileFormatTestCase, BaseIndexFileFormatTestCaseDefaults,
};
use crate::test_framework::core::index::mismatched_codec_reader::MismatchedCodecReader;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::util::line_file_docs::LineFileDocs;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, create_temp_dir, create_temp_dir_with_prefix, get_only_leaf_reader,
  new_directory_shared, new_field, new_fs_directory, new_index_writer_config,
  new_index_writer_config_with_analyzer, new_searcher_with_reader, new_string_field,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::Rng;
use rand::RngExt;
use rand::seq::SliceRandom;
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::thread;

/// Base test support for [`StoredFieldsFormat`] implementations. To test a new format, register a
/// [`Codec`] that uses it, implement this trait, and implement [`Self::get_codec`].
pub struct BaseStoredFieldsFormatTestCaseDefaults;

impl<T> BaseIndexFileFormatTestCaseDefaults<T> for BaseStoredFieldsFormatTestCaseDefaults
where
  T: BaseStoredFieldsFormatTestCase,
{
  fn add_random_fields<R>(_test_case: &T, random: &mut R, document: &mut Document) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let num_values = random.random_range(0..3);
    for _ in 0..num_values {
      document.add(StoredField::from_string(
        "f",
        TestUtil::random_simple_string_range(random, 0, 100),
      )?);
    }
    Ok(())
  }
}

pub trait BaseStoredFieldsFormatTestCase:
  BaseIndexFileFormatTestCase<Defaults = BaseStoredFieldsFormatTestCaseDefaults>
{
  fn test_random_stored_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwc = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwc.set_max_buffered_docs(TestUtil::next_int(random, 5, 20));
    let writer = RandomIndexWriter::with_config(random, directory, iwc);

    let doc_count = at_least(random, 200);
    let field_count = TestUtil::next_int(random, 1, 5);
    let mut field_ids: Vec<i32> = (0..field_count).collect();
    let mut field_types = HashMap::new();
    let mut docs = HashMap::<String, Document>::new();

    let mut stored_only = FieldType::new();
    stored_only.set_stored(true)?;
    stored_only.freeze();

    for i in 0..doc_count {
      let id = i.to_string();
      let mut doc = Document::new();
      doc.add(new_string_field(
        random,
        "id",
        id.clone(),
        Store::No,
        &mut field_types,
      )?);

      for field in &field_ids {
        if random.random_range(0..4) != 3 {
          let value = TestUtil::random_unicode_string_with_len(random, 1000);
          doc.add(new_field(
            random,
            format!("f{field}"),
            value,
            &stored_only,
            &mut field_types,
          )?);
        }
      }

      docs.insert(id.clone(), doc.clone());
      writer.add_document(random, doc)?;

      if random.random_range(0..50) == 17 {
        field_ids.shuffle(random);
      }
      if random.random_range(0..5) == 3 && i > 0 {
        let del_id = random.random_range(0..i).to_string();
        writer.delete_documents_with_terms(random, vec![Term::from_text("id", del_id.clone())])?;
        docs.remove(&del_id);
      }
    }

    if !docs.is_empty() {
      let ids_list = docs.keys().cloned().collect::<Vec<_>>();
      for _ in 0..2 {
        let reader =
          self.maybe_wrap_with_merging_reader(directory_reader::open_from_writer(&writer.w)?)?;
        let searcher = new_searcher_with_reader(reader)?;
        let mut stored_fields = searcher.stored_fields()?;

        for _ in 0..at_least(random, 100) {
          let test_id = ids_list[random.random_range(0..ids_list.len())].clone();
          let hits = searcher.search(TermQuery::new(Term::from_text("id", test_id.clone())), 1)?;
          assert_eq!(1, hits.total_hits.value());

          let doc = stored_fields.document(hits.score_docs[0].doc)?;
          let expected = docs.get(&test_id).unwrap();
          for i in 0..field_count {
            assert_eq!(
              expected.get(&format!("f{i}"))?.map(|v| v.into_owned()),
              doc.get(&format!("f{i}"))?.map(|v| v.into_owned()),
              "doc {test_id}, field f{i} is wrong",
            );
          }
        }
        writer.force_merge(random, 1)?;
      }
    }

    writer.close(random)?;
    Ok(())
  }

  fn test_stored_fields_order<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let iwc = new_index_writer_config(random)?;
    let writer = RandomIndexWriter::with_config(random, directory, iwc);

    let mut stored_only = FieldType::new();
    stored_only.set_stored(true)?;
    stored_only.freeze();

    let mut doc = Document::new();
    doc.add(Field::new("zzz", "a b c", stored_only.clone()));
    doc.add(Field::new("aaa", "a b c", stored_only.clone()));
    doc.add(Field::new("zzz", "1 2 3", stored_only));
    writer.add_document(random, doc)?;

    let reader = self.maybe_wrap_with_merging_reader(writer.get_reader(random)?)?;
    let doc = reader.stored_fields()?.document(0)?;
    let fields = doc.get_fields();

    assert_eq!(3, fields.len());
    assert_eq!("zzz", fields[0].name());
    assert_eq!(
      Some("a b c"),
      fields[0].string_value()?.as_deref().map(|s| s.as_str())
    );
    assert_eq!("aaa", fields[1].name());
    assert_eq!(
      Some("a b c"),
      fields[1].string_value()?.as_deref().map(|s| s.as_str())
    );
    assert_eq!("zzz", fields[2].name());
    assert_eq!(
      Some("1 2 3"),
      fields[2].string_value()?.as_deref().map(|s| s.as_str())
    );

    reader.close()?;
    writer.close(random)?;
    Ok(())
  }

  fn test_binary_field_offset_length<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let iwc = new_index_writer_config(random)?;
    let writer = RandomIndexWriter::with_config(random, directory, iwc);

    let mut bytes = vec![0u8; 50];
    for (i, b) in bytes.iter_mut().enumerate() {
      *b = (i as u8) + 77;
    }

    let field = StoredField::from_binary_with_range("binary", bytes, 10, 17)?;
    let binary = field.binary_value()?.unwrap();
    assert_eq!(50, binary.bytes.len());
    assert_eq!(10, binary.offset);
    assert_eq!(17, binary.length);

    let mut doc = Document::new();
    doc.add(field);
    writer.add_document(random, doc)?;

    let reader = self.maybe_wrap_with_merging_reader(writer.get_reader(random)?)?;
    let doc = reader.stored_fields()?.document(0)?;
    let field = doc.get_field("binary").unwrap();
    let binary = field.binary_value()?.unwrap();
    assert_eq!(17, binary.length);
    assert_eq!(87, binary.bytes[binary.offset]);

    reader.close()?;
    writer.close(random)?;
    Ok(())
  }

  fn test_numeric_field<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let writer = RandomIndexWriter::new(random, directory)?;
    let num_docs = at_least(random, 500) as usize;
    let mut answers = vec![Number::I32(0); num_docs];
    let mut type_answers = vec![""; num_docs];

    for id in 0..num_docs {
      let (nf, answer, type_answer) = if random.random_bool(0.5) {
        if random.random_bool(0.5) {
          let value = random.random::<f32>();
          (
            StoredField::from_f32("nf", value)?,
            Number::F32(value),
            "f32",
          )
        } else {
          let value = random.random::<f64>();
          (
            StoredField::from_f64("nf", value)?,
            Number::F64(value),
            "f64",
          )
        }
      } else if random.random_bool(0.5) {
        let value = random.random::<i32>();
        (
          StoredField::from_i32("nf", value)?,
          Number::I32(value),
          "i32",
        )
      } else {
        let value = random.random::<i64>();
        (
          StoredField::from_i64("nf", value)?,
          Number::I64(value),
          "i64",
        )
      };

      let mut doc = Document::new();
      doc.add(nf);
      doc.add(StoredField::from_i32("id", id as i32)?);
      doc.add(IntPoint::new("id", [id as i32])?);
      doc.add(NumericDocValuesField::new("id", id as i64));
      answers[id] = answer;
      type_answers[id] = type_answer;
      writer.add_document(random, doc)?;
    }

    let reader =
      self.maybe_wrap_with_merging_reader(directory_reader::open_from_writer(&writer.w)?)?;
    writer.close(random)?;
    assert_eq!(num_docs as i32, reader.num_docs()?);

    for leaf in reader.get_context()?.leaves()? {
      let sub = leaf.reader().clone();
      let mut ids = sub.get_numeric_doc_values("id")?.unwrap();
      let mut stored_fields = sub.stored_fields()?;
      for doc_id in 0..sub.num_docs()? {
        let doc = stored_fields.document(doc_id)?;
        let field = doc.get_field("nf").unwrap();
        assert_eq!(doc_id, ids.next_doc()?);
        let idx = ids.long_value()? as usize;
        let actual = field.numeric_value()?.unwrap();
        assert_eq!(answers[idx], actual);
        let actual_type = match actual {
          Number::F32(_) => "f32",
          Number::F64(_) => "f64",
          Number::I32(_) => "i32",
          Number::I64(_) => "i64",
          Number::U8(_) => "u8",
          Number::I16(_) => "i16",
          Number::BigInt(_) => "bigint",
        };
        assert_eq!(type_answers[idx], actual_type);
      }
    }

    Ok(())
  }

  fn test_indexed_bit<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let writer = RandomIndexWriter::new(random, directory)?;

    let mut only_stored = FieldType::new();
    only_stored.set_stored(true)?;
    only_stored.freeze();

    let mut doc = Document::new();
    doc.add(Field::new("field", "value", only_stored));
    doc.add(StringField::from_string("field2", "value", Store::Yes)?);
    writer.add_document(random, doc)?;

    let reader = self.maybe_wrap_with_merging_reader(writer.get_reader(random)?)?;
    let doc = reader.stored_fields()?.document(0)?;
    assert_eq!(
      IndexOptions::None,
      *doc.get_field("field").unwrap().field_type().index_options()
    );
    assert_ne!(
      IndexOptions::None,
      *doc
        .get_field("field2")
        .unwrap()
        .field_type()
        .index_options()
    );

    reader.close()?;
    writer.close(random)?;
    Ok(())
  }

  fn test_read_skip<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwc = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwc.set_max_buffered_docs(TestUtil::next_int(random, 2, 30));
    let writer = RandomIndexWriter::with_config(random, directory, iwc);

    let mut ft = FieldType::new();
    ft.set_stored(true)?;
    ft.freeze();

    let string = TestUtil::random_simple_string_with_len(random, 50);
    let bytes = string.as_bytes().to_vec();
    let long_value = if random.random_bool(0.5) {
      random.random_range(0..42) as i64
    } else {
      random.random::<i64>()
    };
    let int_value = if random.random_bool(0.5) {
      random.random_range(0..42)
    } else {
      random.random::<i32>()
    };
    let float_value = random.random::<f32>();
    let double_value = random.random::<f64>();

    for _ in 0..100 {
      let mut doc = Document::new();
      doc.add(Field::from_binary("bytes", bytes.clone(), ft.clone())?);
      doc.add(Field::new("string", string.clone(), ft.clone()));
      doc.add(StoredField::from_i64("long", long_value)?);
      doc.add(StoredField::from_i32("int", int_value)?);
      doc.add(StoredField::from_f32("float", float_value)?);
      doc.add(StoredField::from_f64("double", double_value)?);
      writer.add_document(random, doc)?;
    }
    writer.commit(random)?;

    let reader = self.maybe_wrap_with_merging_reader(writer.get_reader(random)?)?;
    let mut stored_fields = reader.stored_fields()?;
    let doc_id = random.random_range(0..100);

    for field_name in ["bytes", "string", "long", "int", "float", "double"] {
      let mut fields = HashSet::new();
      fields.insert(field_name.to_string());
      let doc = stored_fields.document_with_fields(doc_id, &fields)?;
      let field = doc.get_field(field_name).unwrap();
      match field_name {
        "bytes" => {
          let binary = field.binary_value()?.unwrap();
          let actual = binary.bytes[binary.offset..binary.offset + binary.length].to_vec();
          assert_eq!(bytes, actual);
        },
        "string" => {
          assert_eq!(
            Some(string.as_str()),
            field.string_value()?.as_deref().map(|s| s.as_str())
          );
        },
        "long" => assert_eq!(Some(Number::I64(long_value)), field.numeric_value()?),
        "int" => assert_eq!(Some(Number::I32(int_value)), field.numeric_value()?),
        "float" => assert_eq!(Some(Number::F32(float_value)), field.numeric_value()?),
        "double" => assert_eq!(Some(Number::F64(double_value)), field.numeric_value()?),
        _ => unreachable!(),
      }
    }

    reader.close()?;
    writer.close(random)?;
    Ok(())
  }

  fn test_empty_docs<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwc = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwc.set_max_buffered_docs(TestUtil::next_int(random, 2, 30));
    let writer = RandomIndexWriter::with_config(random, directory, iwc);

    let num_docs = if random.random_bool(0.5) {
      1
    } else {
      at_least(random, 1000)
    };

    for _ in 0..num_docs {
      writer.add_document(random, Document::new())?;
    }
    writer.commit(random)?;

    let reader = self.maybe_wrap_with_merging_reader(writer.get_reader(random)?)?;
    let mut stored_fields = reader.stored_fields()?;
    for i in 0..num_docs {
      let doc = stored_fields.document(i)?;
      assert!(doc.get_fields().is_empty());
    }

    reader.close()?;
    writer.close(random)?;
    Ok(())
  }
  fn test_concurrent_reads<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwc = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwc.set_max_buffered_docs(TestUtil::next_int(random, 2, 30));
    let writer = RandomIndexWriter::with_config(random, directory.clone(), iwc);

    let num_docs = at_least(random, 1000);
    for i in 0..num_docs {
      let mut doc = Document::new();
      doc.add(StringField::from_string("fld", i.to_string(), Store::Yes)?);
      writer.add_document(random, doc)?;
    }
    writer.commit(random)?;

    let reader =
      Arc::new(self.maybe_wrap_with_merging_reader(directory_reader::open(directory.clone())?)?);
    let concurrent_reads = at_least(random, 5);
    let reads_per_thread = at_least(random, 50);
    let mut read_queries = Vec::new();
    for _ in 0..concurrent_reads {
      let mut queries = Vec::new();
      for _ in 0..reads_per_thread {
        queries.push(random.random_range(0..num_docs));
      }
      read_queries.push(queries);
    }

    let searcher = Arc::new(new_searcher_with_reader(reader.clone())?);
    thread::scope(|scope| -> Result<()> {
      let mut handles = Vec::new();
      for queries in read_queries {
        let rd = reader.clone();
        let searcher = searcher.clone();
        handles.push(scope.spawn(move || -> Result<()> {
          for q in queries {
            let mut stored_fields = rd.stored_fields()?;
            let top_docs = searcher
              .clone()
              .search(TermQuery::new(Term::from_text("fld", q.to_string())), 1)?;
            if top_docs.total_hits.value() != 1 {
              return Err(
                crate::core::util::error::lucene_error::LuceneError::illegal_state(format!(
                  "Expected 1 hit, got {}",
                  top_docs.total_hits.value()
                )),
              );
            }
            let s_doc = stored_fields.document(top_docs.score_docs[0].doc)?;
            let actual = s_doc.get("fld")?.ok_or_else(|| {
              crate::core::util::error::lucene_error::LuceneError::illegal_state(format!(
                "Could not find document {q}"
              ))
            })?;
            if actual.as_ref() != &q.to_string() {
              return Err(
                crate::core::util::error::lucene_error::LuceneError::illegal_state(format!(
                  "Expected {q}, but got {}",
                  actual.as_ref()
                )),
              );
            }
          }
          Ok(())
        }));
      }

      for handle in handles {
        handle.join().map_err(|_| {
          crate::core::util::error::lucene_error::LuceneError::illegal_state(
            "stored fields read thread panicked",
          )
        })??;
      }
      Ok(())
    })?;

    reader.close()?;
    writer.close(random)?;
    Ok(())
  }
  fn random_byte_array<R>(&self, random: &mut R, length: usize, max: i32) -> Vec<u8>
  where
    R: Rng + ?Sized,
  {
    let mut result = vec![0u8; length];
    for item in result.iter_mut().take(length) {
      *item = random.random_range(0..max) as u8;
    }
    result
  }
  fn test_write_read_merge<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let directory = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwc = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwc.set_max_buffered_docs(TestUtil::next_int(random, 2, 30));
    // TODO: set codec to a different implementation here so we can exercise
    // merging stored fields across codecs once codec switching is wired up.
    let writer = RandomIndexWriter::with_config(random, directory.clone(), iwc);

    let doc_count = at_least(random, 200);
    let mut data: Vec<Vec<Vec<u8>>> = vec![Vec::new(); doc_count as usize];
    #[allow(clippy::needless_range_loop)]
    for i in 0..doc_count as usize {
      let field_count = if random.random_bool(0.05) {
        TestUtil::next_int(random, 1, 500) as usize
      } else {
        TestUtil::next_int(random, 1, 5) as usize
      };
      let mut fields = Vec::with_capacity(field_count);
      for _ in 0..field_count {
        let length = if random.random_bool(0.05) {
          random.random_range(0..1000)
        } else {
          random.random_range(0..10)
        };
        let max = if random.random_bool(0.05) {
          256usize
        } else {
          2usize
        };
        let bytes = self.random_byte_array(random, length as usize, max as i32);
        fields.push(bytes);
      }
      data[i] = fields;
    }

    let mut type_ = FieldType::new();
    type_.set_stored(true)?;
    type_.freeze();

    let mut id = IntPoint::new("id", [0])?;
    let mut id_stored = StoredField::from_i32("id", 0)?;
    #[allow(clippy::needless_range_loop)]
    for i in 0..data.len() {
      id.set_int_value(i as i32)?;
      id_stored.set_int_value(i as i32)?;
      let mut doc = Document::new();
      doc.add(id.clone());
      doc.add(id_stored.clone());
      for (j, bytes) in data[i].iter().enumerate() {
        doc.add(Field::from_binary(
          format!("bytes{j}"),
          bytes.clone(),
          type_.clone(),
        )?);
      }
      writer.add_document(random, doc)?;
    }

    for _ in 0..10 {
      let min = random.random_range(0..data.len() as i32);
      let max = min + random.random_range(0..20);
      writer.delete_documents_with_queries(
        random,
        vec![IntPoint::new_range_query("id", min, max - 1)?.into()],
      )?;
    }

    writer.force_merge(random, 2)?;
    writer.commit(random)?;

    let reader = self.maybe_wrap_with_merging_reader(directory_reader::open(directory.clone())?)?;
    let mut stored_fields = reader.stored_fields()?;
    assert!(reader.num_docs()? > 0);
    let mut num_docs = 0;
    for i in 0..reader.max_doc()? {
      let doc = stored_fields.document(i)?;
      num_docs += 1;
      let doc_id = doc
        .get_field("id")
        .unwrap()
        .numeric_value()?
        .unwrap()
        .to_i32()
        .unwrap();
      assert_eq!(data[doc_id as usize].len() + 1, doc.get_fields().len());
      for (j, bytes) in data[doc_id as usize].iter().enumerate() {
        let actual = doc.get_binary_value(&format!("bytes{j}"))?.unwrap();
        let actual = actual.as_ref();
        assert_eq!(
          bytes,
          &actual.bytes[actual.offset..actual.offset + actual.length]
        );
      }
    }
    assert!(reader.num_docs()? <= num_docs);
    reader.close()?;

    writer.w.delete_all()?;
    writer.commit(random)?;
    writer.force_merge(random, 1)?;

    writer.close(random)?;
    Ok(())
  }
  fn test_merge_filter_reader<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let writer = RandomIndexWriter::new(random, dir.clone())?;
    let num_docs = at_least(random, 200) as usize;
    let mut string_values = Vec::with_capacity(10);
    for _ in 0..10 {
      string_values.push(TestUtil::random_realistic_unicode_string_with_len(
        random, 10,
      ));
    }
    let mut docs = Vec::with_capacity(num_docs);
    for i in 0..num_docs {
      let mut doc = Document::new();
      doc.add(StringField::from_string(
        "to_delete",
        if random.random_bool(0.5) { "yes" } else { "no" },
        Store::No,
      )?);
      doc.add(StoredField::from_i32("id", i as i32)?);
      doc.add(StoredField::from_i32("i", random.random_range(0..50))?);
      doc.add(StoredField::from_i64("l", random.random())?);
      doc.add(StoredField::from_f64("d", random.random())?);
      doc.add(StoredField::from_f32("f", random.random())?);
      let string_value = string_values[random.random_range(0..string_values.len())].clone();
      doc.add(StoredField::from_string("s", string_value)?);
      let binary_value = string_values[random.random_range(0..string_values.len())].clone();
      doc.add(StoredField::from_bytes_ref(
        "b",
        BytesRef::from_string(&binary_value),
      )?);
      docs.push(doc.clone());
      writer.add_document(random, doc)?;
    }
    if random.random_bool(0.5) {
      writer.delete_documents_with_terms(random, vec![Term::from_text("to_delete", "yes")])?;
    }
    writer.commit(random)?;
    writer.close(random)?;

    let reader = DummyFilterDirectoryReader::new(
      self.maybe_wrap_with_merging_reader(directory_reader::open(dir.clone())?)?,
    )?;

    let dir2 = new_directory_shared(random)?;
    let writer = RandomIndexWriter::new(random, dir2.clone())?;
    TestUtil::add_indexes_slowly(&writer.w, std::slice::from_ref(&reader))?;
    reader.close()?;
    dir.close()?;

    let reader = self.maybe_wrap_with_merging_reader(writer.get_reader(random)?)?;
    let mut stored_fields = reader.stored_fields()?;
    for i in 0..reader.max_doc()? {
      let doc = stored_fields.document(i)?;
      let id = doc
        .get_field("id")
        .expect("stored document must have an id")
        .numeric_value()?
        .expect("stored id must be numeric")
        .to_i32()
        .expect("stored id must fit in an i32");
      let expected = &docs[id as usize];
      assert_eq!(expected.get("s")?, doc.get("s")?);
      assert_eq!(
        expected.get_field("i").unwrap().numeric_value()?,
        doc.get_field("i").unwrap().numeric_value()?
      );
      assert_eq!(
        expected.get_field("l").unwrap().numeric_value()?,
        doc.get_field("l").unwrap().numeric_value()?
      );
      assert_eq!(
        expected.get_field("d").unwrap().numeric_value()?,
        doc.get_field("d").unwrap().numeric_value()?
      );
      assert_eq!(
        expected.get_field("f").unwrap().numeric_value()?,
        doc.get_field("f").unwrap().numeric_value()?
      );
      assert_eq!(
        expected.get_field("b").unwrap().binary_value()?,
        doc.get_field("b").unwrap().binary_value()?
      );
    }

    reader.close()?;
    writer.close(random)?;
    TestUtil::check_index(random, dir2.as_ref())?;
    dir2.close()
  }

  fn test_big_documents<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_fs_directory(random, create_temp_dir_with_prefix("testBigDocuments")?)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwc = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwc.set_max_buffered_docs(TestUtil::next_int(random, 2, 30));
    let writer = RandomIndexWriter::with_config(random, dir.clone(), iwc);

    let mut empty_doc = Document::new();
    let mut big_doc1 = Document::new();
    let mut big_doc2 = Document::new();

    let id_field = StringField::from_string("id", "", Store::No)?;
    empty_doc.add(id_field.clone());
    big_doc1.add(id_field.clone());
    big_doc2.add(id_field);

    let mut only_stored = FieldType::new();
    only_stored.set_stored(true)?;
    only_stored.set_index_options(IndexOptions::None)?;
    only_stored.freeze();

    let small_length = TestUtil::next_int(random, 0, 9) as usize;
    let small_field = Field::from_binary(
      "fld",
      self.random_byte_array(random, small_length, 256),
      only_stored.clone(),
    )?;
    let num_fields = TestUtil::next_int(random, 500_000, 1_000_000);
    for _ in 0..num_fields {
      big_doc1.add(small_field.clone());
    }

    let big_length = TestUtil::next_int(random, 1_000_000, 5_000_000) as usize;
    let big_field = Field::from_binary(
      "fld",
      self.random_byte_array(random, big_length, 2),
      only_stored,
    )?;
    big_doc2.add(big_field);

    let num_docs = at_least(random, 5) as usize;
    let docs = [empty_doc, big_doc1, big_doc2];
    let mut doc_templates = Vec::with_capacity(num_docs);
    for i in 0..num_docs {
      let template_idx = TestUtil::next_int(random, 0, (docs.len() - 1) as i32) as usize;
      doc_templates.push(template_idx);
      let mut doc = docs[template_idx].clone();
      doc.remove_field("id");
      doc.add(StringField::from_string("id", i.to_string(), Store::No)?);
      writer.add_document(random, doc)?;
      if random.random_bool(0.1) {
        writer.commit(random)?;
      }
    }

    writer.commit(random)?;
    writer.force_merge(random, 1)?;

    let reader = self.maybe_wrap_with_merging_reader(directory_reader::open(dir.clone())?)?;
    let searcher = new_searcher_with_reader(directory_reader::open(dir.clone())?)?;
    let mut stored_fields = reader.stored_fields()?;

    for i in 0..num_docs {
      let query = TermQuery::new(Term::from_text("id", i.to_string()));
      let top_docs = searcher.search(query, 1)?;
      assert_eq!(1, top_docs.total_hits.value());

      let doc = stored_fields.document(top_docs.score_docs[0].doc)?;
      let field_values = doc.get_fields_with_name("fld");
      let template = &docs[doc_templates[i]];
      assert_eq!(
        template.get_fields_with_name("fld").len(),
        field_values.len()
      );
      if !field_values.is_empty() {
        assert_eq!(
          template.get_fields_with_name("fld")[0].binary_value()?,
          field_values[0].binary_value()?
        );
      }
    }

    reader.close()?;
    writer.close(random)?;
    Ok(())
  }

  fn test_bulk_merge_with_deletes<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let analyzer = MockAnalyzer::new(random);
    let mut iwc = new_index_writer_config_with_analyzer(random, analyzer)?;
    iwc.set_merge_policy(NoMergePolicy::default());
    let writer = RandomIndexWriter::with_config(random, dir.clone(), iwc);

    let num_docs = at_least(random, 200) as usize;
    for i in 0..num_docs {
      let mut doc = Document::new();
      doc.add(StringField::from_string("id", i.to_string(), Store::Yes)?);
      doc.add(StoredField::from_string(
        "f",
        TestUtil::random_simple_string(random),
      )?);
      writer.add_document(random, doc)?;
    }

    let delete_count = TestUtil::next_int(random, 5, num_docs as i32) as usize;
    for _ in 0..delete_count {
      let id = TestUtil::next_int(random, 0, (num_docs - 1) as i32);
      writer.delete_documents_with_terms(random, vec![Term::from_text("id", id.to_string())])?;
    }

    writer.commit(random)?;
    writer.close(random)?;
    drop(writer);

    let writer = RandomIndexWriter::new(random, dir.clone())?;
    let max_num_segments = TestUtil::next_int(random, 1, 3);
    writer.force_merge(random, max_num_segments)?;
    writer.commit(random)?;
    writer.close(random)?;

    TestUtil::check_index(random, dir)?;
    Ok(())
  }

  /// Mix up field numbers, merge, and check that data is correct.
  fn test_mismatched_fields<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let mut dirs = Vec::with_capacity(10);
    for _ in 0..10 {
      let dir = new_directory_shared(random)?;
      let iw = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;
      let mut doc = Document::new();
      for j in 0..10 {
        // Add fields where name=value (e.g. 3=3) so we can detect if stuff gets screwed up.
        doc.add(StringField::from_string(
          j.to_string(),
          j.to_string(),
          Store::Yes,
        )?);
      }
      for _ in 0..10 {
        iw.add_document(doc.clone())?;
      }

      let reader = self.maybe_wrap_with_merging_reader(directory_reader::open_from_writer(&iw)?)?;
      let target_dir = new_directory_shared(random)?;
      let adder = IndexWriter::new(target_dir.clone(), IndexWriterConfig::new()?)?;
      if random.random_bool(0.5) {
        // Mix up fields explicitly. Rust expresses Java's MismatchedDirectoryReader at the
        // CodecReader boundary used by addIndexesSlowly.
        let leaf = get_only_leaf_reader(&reader)?;
        let mismatched = MismatchedCodecReader::new(leaf, random)?;
        adder.add_indexes_from_codec_readers(vec![mismatched])?;
      } else {
        TestUtil::add_indexes_slowly(&adder, std::slice::from_ref(&reader))?;
      }
      adder.commit()?;
      adder.close()?;

      let close_result = reader.close();
      let close_result = IOUtils::use_or_suppress_result(close_result, iw.close());
      IOUtils::use_or_suppress_result(close_result, dir.close())?;
      dirs.push(target_dir);
    }

    let everything = new_directory_shared(random)?;
    let iw = IndexWriter::new(everything.clone(), IndexWriterConfig::new()?)?;
    iw.add_indexes_from_directory(&dirs)?;
    iw.force_merge(1)?;

    let reader = directory_reader::open_from_writer(&iw)?;
    let leaf = get_only_leaf_reader(&reader)?;
    let mut stored_fields = leaf.stored_fields()?;
    for i in 0..leaf.max_doc()? {
      let doc = stored_fields.document(i)?;
      assert_eq!(10, doc.get_fields().len());
      for j in 0..10 {
        assert_eq!(
          Some(j.to_string()),
          doc.get(&j.to_string())?.map(|value| value.into_owned())
        );
      }
    }

    let close_result = iw.close();
    let close_result = IOUtils::use_or_suppress_result(close_result, reader.close());
    let close_result = IOUtils::use_or_suppress_result(close_result, everything.close());

    dirs.iter().fold(close_result, |result, dir| {
      IOUtils::use_or_suppress_result(result, dir.close())
    })
  }

  fn test_random_stored_fields_with_index_sort<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let sort_fields = if random.random_bool(0.5) {
      vec![
        SortField::new(Some("sort-1"), SortFieldType::Long)?,
        SortField::new(Some("sort-2"), SortFieldType::Int)?,
      ]
    } else {
      vec![SortField::new(Some("sort-1"), SortFieldType::Long)?]
    };
    let mut stored_fields = Vec::new();
    let num_fields = TestUtil::next_int(random, 1, 10);
    for i in 0..num_fields {
      stored_fields.push(format!("f-{i}"));
    }
    let mut store_type = FieldType::from_ref(&*TYPE_STORED)?;
    store_type.set_stored(true)?;
    let document_factory = |random: &mut R,
                            stored_fields: &mut Vec<String>,
                            field_types: &mut HashMap<String, FieldType>,
                            id: &str|
     -> Result<Document> {
      let mut doc = Document::new();
      doc.add(StringField::from_string(
        "id",
        id,
        if random.random_bool(0.5) {
          Store::Yes
        } else {
          Store::No
        },
      )?);
      if random.random_range(0..100) <= 5 {
        stored_fields.shuffle(random);
      }
      for field_name in stored_fields.iter() {
        if random.random_bool(0.5) {
          let value = TestUtil::random_unicode_string_with_len(random, 100);
          doc.add(new_field(
            random,
            field_name,
            value,
            &store_type,
            field_types,
          )?);
        }
      }
      for sort_field in &sort_fields {
        doc.add(NumericDocValuesField::new(
          sort_field
            .get_field()
            .expect("index sort field must have a name"),
          TestUtil::next_int(random, 0, 10_000) as i64,
        ));
      }
      Ok(doc)
    };

    let mut docs = HashMap::new();
    let mut field_types = HashMap::new();
    let num_docs = at_least(random, 100);
    for i in 0..num_docs {
      let id = i.to_string();
      docs.insert(
        id.clone(),
        document_factory(random, &mut stored_fields, &mut field_types, &id)?,
      );
    }

    let dir = new_directory_shared(random)?;
    let mut iwc = new_index_writer_config(random)?;
    iwc.set_max_buffered_docs(TestUtil::next_int(random, 5, 20));
    iwc.set_index_sort(Sort::with_fields(sort_fields.clone())?)?;
    let iw = RandomIndexWriter::with_config(random, dir.clone(), iwc);
    let mut added_ids = Vec::new();
    let verify_stored_fields = |random: &mut R,
                                iw: &RandomIndexWriter<DirEnum>,
                                added_ids: &[String],
                                docs: &HashMap<String, Document>|
     -> Result<()> {
      if added_ids.is_empty() {
        return Ok(());
      }
      let reader = self.maybe_wrap_with_merging_reader(iw.get_reader(random)?)?;
      let searcher = new_searcher_with_reader(reader)?;
      let body_result = (|| {
        let mut actual_stored_fields = searcher.stored_fields()?;
        let iters = TestUtil::next_int(random, 1, 10);
        for _ in 0..iters {
          let test_id = &added_ids[random.random_range(0..added_ids.len())];
          if cfg!(feature = "test_log_verbose") {
            println!("TEST: test id={test_id}");
          }
          let hits = searcher.search(TermQuery::new(Term::from_text("id", test_id)), 1)?;
          assert_eq!(1, hits.total_hits.value());
          let expected_fields = docs[test_id]
            .get_fields()
            .iter()
            .filter(|field| field.field_type().stored())
            .collect::<Vec<_>>();
          let actual_doc = actual_stored_fields.document(hits.score_docs[0].doc)?;
          assert_eq!(expected_fields.len(), actual_doc.get_fields().len());
          for expected_field in expected_fields {
            let actual_fields = actual_doc.get_fields_with_name(expected_field.name());
            assert_eq!(1, actual_fields.len());
            assert_eq!(
              expected_field.string_value()?,
              actual_fields[0].string_value()?
            );
          }
        }
        Ok(())
      })();
      IOUtils::use_or_suppress_result(body_result, searcher.get_index_reader().close())
    };

    let mut ids = docs.keys().cloned().collect::<Vec<_>>();
    ids.shuffle(random);
    for id in ids {
      if random.random_range(0..100) < 5 {
        // Add via foreign reader.
        let other_dir = new_directory_shared(random)?;
        let mut other_iwc = new_index_writer_config(random)?;
        other_iwc.set_index_sort(Sort::with_fields(sort_fields.clone())?)?;
        let other_iw = RandomIndexWriter::with_config(random, other_dir.clone(), other_iwc);
        other_iw.add_document(random, docs[&id].clone())?;
        let other_reader = other_iw.get_reader(random)?;
        let body_result = TestUtil::add_indexes_slowly(&iw.w, std::slice::from_ref(&other_reader));
        let close_result = IOUtils::use_or_suppress_result(body_result, other_reader.close());
        let close_result = IOUtils::use_or_suppress_result(close_result, other_iw.close(random));
        IOUtils::use_or_suppress_result(close_result, other_dir.close())?;
      } else {
        // Add normally.
        iw.add_document(random, docs[&id].clone())?;
      }
      added_ids.push(id.clone());
      if random.random_range(0..100) < 5 {
        let deleting_id = added_ids.remove(random.random_range(0..added_ids.len()));
        if random.random_bool(0.5) {
          iw.delete_documents_with_queries(
            random,
            vec![TermQuery::new(Term::from_text("id", &deleting_id)).into()],
          )?;
        } else {
          let new_doc =
            document_factory(random, &mut stored_fields, &mut field_types, &deleting_id)?;
          docs.insert(deleting_id.clone(), new_doc.clone());
          iw.update_document_with_term(random, Term::from_text("id", deleting_id), new_doc)?;
        }
      }
      if random.random_range(0..100) < 5 {
        verify_stored_fields(random, &iw, &added_ids, &docs)?;
      }
      if random.random_range(0..100) < 2 {
        let max_num_segments = TestUtil::next_int(random, 1, 3);
        iw.force_merge(random, max_num_segments)?;
      }
    }
    verify_stored_fields(random, &iw, &added_ids, &docs)?;
    let max_num_segments = TestUtil::next_int(random, 1, 3);
    iw.force_merge(random, max_num_segments)?;
    verify_stored_fields(random, &iw, &added_ids, &docs)?;
    let close_result = iw.close(random);
    IOUtils::use_or_suppress_result(close_result, dir.close())
  }

  fn test_line_file_docs<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    // Use an FS dir and a non-randomized IWC to not slow down indexing
    let dir = new_fs_directory(random, create_temp_dir()?)?;

    {
      let mut docs = LineFileDocs::new(random)?;
      let w = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;

      let num_docs = at_least(random, 10_000);

      for _ in 0..num_docs {
        // Only keep stored fields
        let doc = docs.next_doc()?;
        let mut stored_doc = Document::new();

        for field in doc.get_fields() {
          if field.field_type().stored() {
            if let Some(value) = field.string_value()? {
              // Disable indexing
              stored_doc.add(StoredField::from_string(field.name(), value.into_owned())?);
            } else {
              stored_doc.add(field.clone());
            }
          }
        }

        w.add_document(stored_doc)?;
      }

      w.force_merge(1)?;
      w.close()?;
    }

    TestUtil::check_index(random, dir)?;

    Ok(())
  }
}

/// A dummy filter reader that reverses the order of documents in stored fields.
struct DummyFilterLeafReader<LR>
where
  LR: LeafReader,
{
  in_: LR,
  index_base: IndexReaderBase,
}

impl<LR> DummyFilterLeafReader<LR>
where
  LR: LeafReader,
{
  fn new(in_: LR) -> Result<Self> {
    let index_base = IndexReaderBase::new();
    in_.register_parent_reader(&index_base)?;
    Ok(Self { in_, index_base })
  }
}

impl<LR> Clone for DummyFilterLeafReader<LR>
where
  LR: LeafReader + Clone,
{
  fn clone(&self) -> Self {
    Self {
      in_: self.in_.clone(),
      index_base: self.index_base.clone(),
    }
  }
}

impl<LR> Display for DummyFilterLeafReader<LR>
where
  LR: LeafReader,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "DummyFilterLeafReader({})", self.in_)
  }
}

struct ReversedStoredFields<S>
where
  S: StoredFields,
{
  in_: S,
  max_doc: i32,
}

impl<S> RawStoredFieldsReader for ReversedStoredFields<S>
where
  S: StoredFields,
{
  type IndexInput = S::IndexInput;
}

impl<S> StoredFields for ReversedStoredFields<S>
where
  S: StoredFields,
{
  fn document_with_visitor<W>(
    &mut self,
    doc_id: i32,
    visitor: &mut impl StoredFieldVisitor,
    writer: Option<&mut W>,
  ) -> Result<()>
  where
    W: StoredFieldsWriter,
  {
    self
      .in_
      .document_with_visitor(self.max_doc - 1 - doc_id, visitor, writer)
  }
}

impl<LR> IndexReader for DummyFilterLeafReader<LR>
where
  LR: LeafReader,
{
  type ContextKind = LeafReaderContextKind;

  type TermVectors = LR::TermVectors;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    self.in_.term_vectors()
  }

  fn max_doc(&self) -> Result<i32> {
    self.in_.max_doc()
  }

  fn num_docs(&self) -> Result<i32> {
    self.in_.num_docs()
  }

  type StoredFields = ReversedStoredFields<LR::StoredFields>;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    Ok(ReversedStoredFields {
      in_: self.in_.stored_fields()?,
      max_doc: self.max_doc()?,
    })
  }

  fn do_close(&self) -> Result<()> {
    self.in_.close()
  }

  type ReaderCacheHelper = LR::ReaderCacheHelper;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    Ok(None)
  }

  fn doc_freq(&self, term: &Term) -> Result<i32> {
    IndexReader::doc_freq(&self.in_, term)
  }

  fn total_term_freq(&self, term: &Term) -> Result<i64> {
    self.in_.total_term_freq(term)
  }

  fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
    IndexReader::get_sum_doc_freq(&self.in_, field)
  }

  fn get_doc_count(&self, field: &str) -> Result<i32> {
    IndexReader::get_doc_count(&self.in_, field)
  }

  fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
    IndexReader::get_sum_total_term_freq(&self.in_, field)
  }

  fn index_base(&self) -> &IndexReaderBase {
    &self.index_base
  }
}

impl<LR> LeafReader for DummyFilterLeafReader<LR>
where
  LR: LeafReader,
{
  type CacheHelper = LR::CacheHelper;

  fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
    Ok(None)
  }

  type Terms = LR::Terms;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    self.in_.terms(field)
  }

  type NumericDocValues = LR::NumericDocValues;

  fn get_numeric_doc_values(&self, field: &str) -> Result<Option<Self::NumericDocValues>> {
    self.in_.get_numeric_doc_values(field)
  }

  type BinaryDocValues = LR::BinaryDocValues;

  fn get_binary_doc_values(&self, field: &str) -> Result<Option<Self::BinaryDocValues>> {
    self.in_.get_binary_doc_values(field)
  }

  type SortedDocValues = LR::SortedDocValues;

  fn get_sorted_doc_values(&self, field: &str) -> Result<Option<Self::SortedDocValues>> {
    self.in_.get_sorted_doc_values(field)
  }

  type SortedNumericDocValues = LR::SortedNumericDocValues;

  fn get_sorted_numeric_doc_values(
    &self,
    field: &str,
  ) -> Result<Option<Self::SortedNumericDocValues>> {
    self.in_.get_sorted_numeric_doc_values(field)
  }

  type SortedSetDocValues = LR::SortedSetDocValues;

  fn get_sorted_set_doc_values(&self, field: &str) -> Result<Option<Self::SortedSetDocValues>> {
    self.in_.get_sorted_set_doc_values(field)
  }

  type NormNumericDocValues = LR::NormNumericDocValues;

  fn get_norm_values(&self, field: &str) -> Result<Option<Self::NormNumericDocValues>> {
    self.in_.get_norm_values(field)
  }

  type DocValuesSkipper = LR::DocValuesSkipper;

  fn get_doc_values_skipper(&self, field: &str) -> Result<Option<Self::DocValuesSkipper>> {
    self.in_.get_doc_values_skipper(field)
  }

  type FloatVectorValues = LR::FloatVectorValues;

  fn get_float_vector_values(&self, field: &str) -> Result<Option<Self::FloatVectorValues>> {
    self.in_.get_float_vector_values(field)
  }

  type ByteVectorValues = LR::ByteVectorValues;

  fn get_byte_vector_values(&self, field: &str) -> Result<Option<Self::ByteVectorValues>> {
    self.in_.get_byte_vector_values(field)
  }

  fn search_nearest_vectors_f32<B, K>(
    &self,
    field: &str,
    target: Vec<f32>,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
  {
    self
      .in_
      .search_nearest_vectors_f32(field, target, knn_collector, accept_docs)
  }

  fn search_nearest_vectors_u8<B, K>(
    &self,
    field: &str,
    target: Vec<u8>,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
  {
    self
      .in_
      .search_nearest_vectors_u8(field, target, knn_collector, accept_docs)
  }

  fn get_field_infos(&self) -> Result<Arc<crate::core::index::field_infos::FieldInfos>> {
    self.in_.get_field_infos()
  }

  type Bits = LR::Bits;

  fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
    self.in_.get_live_docs()
  }

  type PointValues = LR::PointValues;

  fn get_point_values(&self, field: &str) -> Result<Option<Self::PointValues>> {
    self.in_.get_point_values(field)
  }

  fn check_integrity(&self) -> Result<()> {
    self.in_.check_integrity()
  }

  fn get_metadata(&self) -> Result<&LeafMetaData> {
    self.in_.get_metadata()
  }
}

struct DummyFilterDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  in_: DR,
  base: BaseCompositeReaderBase<DummyFilterLeafReader<DR::LeafReader>>,
  index_base: IndexReaderBase,
}

impl<DR> DummyFilterDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  fn new(in_: DR) -> Result<Self> {
    let wrapper = DummySubReaderWrapper;
    let readers = wrapper.wrap_readers(in_.get_sequential_sub_readers().to_vec())?;
    let index_base = IndexReaderBase::new();
    let base = BaseCompositeReaderBase::new::<DummyComparator>(readers, None, &index_base)?;
    Ok(Self {
      in_,
      base,
      index_base,
    })
  }
}

impl<DR> BaseCompositeReader for DummyFilterDirectoryReader<DR> where DR: DirectoryReader {}

impl<DR> CompositeReader for DummyFilterDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  type LeafReader = DummyFilterLeafReader<DR::LeafReader>;
  type SubReader = Self::LeafReader;

  fn get_sequential_sub_readers(&self) -> &[Self::SubReader] {
    self.base.get_sequential_sub_readers()
  }

  fn visit_leaves<F>(&self, visitor: &mut F) -> Result<()>
  where
    F: FnMut(&Self::LeafReader) -> Result<()>,
  {
    for reader in self.get_sequential_sub_readers() {
      visitor(reader)?;
    }
    Ok(())
  }

  fn to_string(&self) -> String {
    format!("DummyFilterDirectoryReader({})", self.in_.to_string())
  }
}

impl<DR> IndexReader for DummyFilterDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  type ContextKind = CompositeReaderContextKind;

  type TermVectors = BCRTermVectorsImpl<<Self as CompositeReader>::LeafReader>;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    self.base.term_vector(self)
  }

  fn max_doc(&self) -> Result<i32> {
    Ok(self.base.max_doc())
  }

  fn num_docs(&self) -> Result<i32> {
    self.base.num_docs()
  }

  type StoredFields = BCRStoredFieldsImpl<<Self as CompositeReader>::LeafReader>;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    self.base.stored_fields(self)
  }

  fn do_close(&self) -> Result<()> {
    self.in_.close()
  }

  type ReaderCacheHelper = DR::ReaderCacheHelper;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    Ok(None)
  }

  fn doc_freq(&self, term: &Term) -> Result<i32> {
    self.base.doc_freq(term, self)
  }

  fn total_term_freq(&self, term: &Term) -> Result<i64> {
    self.base.total_term_freq(term, self)
  }

  fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
    self.base.get_sum_doc_freq(field, self)
  }

  fn get_doc_count(&self, field: &str) -> Result<i32> {
    self.base.get_doc_count(field, self)
  }

  fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
    self.base.get_sum_total_term_freq(field, self)
  }

  fn index_base(&self) -> &IndexReaderBase {
    &self.index_base
  }
}

impl<DR> Display for DummyFilterDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", CompositeReader::to_string(self))
  }
}

impl<DR> DirectoryReader for DummyFilterDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  type DirectoryReader = DummyFilterDirectoryReader<DR::DirectoryReader>;

  fn do_open_if_changed(&self) -> Result<Option<Self::DirectoryReader>> {
    self.wrap_directory_reader(self.in_.do_open_if_changed()?)
  }

  fn do_open_if_changed_with_commit<IC>(
    &self,
    commit: Option<&IC>,
  ) -> Result<Option<Self::DirectoryReader>>
  where
    IC: crate::core::index::index_commit::IndexCommit<Directory = Arc<Self::Directory>>,
  {
    self.wrap_directory_reader(self.in_.do_open_if_changed_with_commit(commit)?)
  }

  fn do_open_if_changed_with_deletes(
    &self,
    writer: &Arc<IndexWriter<Self::Directory>>,
    apply_deletes: bool,
  ) -> Result<Option<Self::DirectoryReader>> {
    self.wrap_directory_reader(
      self
        .in_
        .do_open_if_changed_with_deletes(writer, apply_deletes)?,
    )
  }

  fn get_version(&self) -> Result<i64> {
    self.in_.get_version()
  }

  fn is_current(&self) -> Result<bool> {
    self.in_.is_current()
  }

  type IndexCommit = DR::IndexCommit;

  fn get_index_commit(&self) -> Result<Self::IndexCommit> {
    self.in_.get_index_commit()
  }

  type Directory = DR::Directory;

  fn directory(&self) -> &DirectoryReaderBase<Self::Directory> {
    self.in_.directory()
  }
}

impl<DR> FilterDirectoryReader for DummyFilterDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  type Delegate = DR;

  fn get_delegate(&self) -> &Self::Delegate {
    &self.in_
  }

  type WrapDirectoryReader = DummyFilterDirectoryReader<DR::DirectoryReader>;

  fn do_wrap_directory_reader(
    &self,
    in_: Option<<Self::Delegate as DirectoryReader>::DirectoryReader>,
  ) -> Result<Option<Self::WrapDirectoryReader>> {
    in_.map(DummyFilterDirectoryReader::new).transpose()
  }
}

struct DummySubReaderWrapper;

impl<LR> SubReaderWrapper<LR> for DummySubReaderWrapper
where
  LR: LeafReader,
{
  type LeafReader1 = Self::LeafReader2;

  fn wrap_readers(&self, readers: Vec<LR>) -> Result<Vec<Self::LeafReader1>> {
    self.default_wrap_readers(readers)
  }

  type LeafReader2 = DummyFilterLeafReader<LR>;

  fn wrap(&self, reader: LR) -> Result<Self::LeafReader2> {
    DummyFilterLeafReader::new(reader)
  }
}
