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
use crate::core::analysis::analyzer::{Analyzer, AnalyzerStoredValue, TokenStreamComponents};
use crate::core::analysis::reader::ReaderEnum;
use crate::core::analysis::token_filter::{TokenFilter, TokenFilterBase};
use crate::core::analysis::token_stream::TokenStream;
use crate::core::document::document::Document;
use crate::core::document::field::Field;
use crate::core::document::field_type::FieldType;
use crate::core::document::text_field::TYPE_NOT_STORED;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::util::attribute_source::Attributes;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind, Result};
use crate::core::util::info_stream::InfoStreamEnum;
use crate::core::util::print_stream_info_stream::PrintStreamInfoStream;
use crate::test_framework::core::analysis::mock_tokenizer::MockTokenizer;
use crate::test_framework::core::util::lucene_test_case::{
  new_directory_shared, random, random_from_seed,
};
use rand::RngExt;
use std::io::Cursor;
use std::sync::{Arc, LazyLock};

/** Test adding to the info stream when field analysis returns an error. */
#[allow(dead_code)] // for quick
struct TestDocInverterPerFieldErrorInfo;

static STORED_TEXT_TYPE: LazyLock<FieldType> =
  LazyLock::new(|| FieldType::from_ref(&*TYPE_NOT_STORED).expect("should not fail"));

#[test]
fn test_info_stream_gets_field_name() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut c = IndexWriterConfig::with_analyzer(Box::new(ThrowingAnalyzer {
    stored_value: AnalyzerStoredValue::new(),
    seed: random.random(),
  }) as Box<dyn Analyzer>)?;
  let print_stream_info_stream = Arc::new(PrintStreamInfoStream::new(Cursor::new(Vec::new())));
  c.set_info_stream(InfoStreamEnum::Custom(Box::new(
    print_stream_info_stream.clone(),
  )));
  let writer = IndexWriter::new(dir.clone(), c)?;
  let mut doc = Document::new();
  doc.add(Field::new(
    "distinctiveFieldName",
    "aaa ",
    STORED_TEXT_TYPE.clone(),
  ));
  let result = writer.add_document(doc);
  assert!(matches!(
    result.as_ref().map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalState(_))
  ));
  let info_stream = info_bytes_to_string(&print_stream_info_stream);
  assert!(info_stream.contains("distinctiveFieldName"));

  writer.close()?;
  dir.close()?;
  Ok(())
}

#[test]
fn test_no_extra_noise() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut c = IndexWriterConfig::with_analyzer(Box::new(ThrowingAnalyzer {
    stored_value: AnalyzerStoredValue::new(),
    seed: random.random(),
  }) as Box<dyn Analyzer>)?;
  let print_stream_info_stream = Arc::new(PrintStreamInfoStream::new(Cursor::new(Vec::new())));
  c.set_info_stream(InfoStreamEnum::Custom(Box::new(
    print_stream_info_stream.clone(),
  )));
  let writer = IndexWriter::new(dir.clone(), c)?;
  let mut doc = Document::new();
  doc.add(Field::new(
    "boringFieldName",
    "aaa ",
    STORED_TEXT_TYPE.clone(),
  ));
  // Should not return `BadNews`.
  writer.add_document(doc)?;
  let info_stream = info_bytes_to_string(&print_stream_info_stream);
  assert!(!info_stream.contains("boringFieldName"));

  writer.close()?;
  dir.close()?;
  Ok(())
}

struct ThrowingAnalyzer {
  stored_value: AnalyzerStoredValue,
  seed: u64,
}

impl Analyzer for ThrowingAnalyzer {
  fn create_components(&self, field_name: &str) -> Result<TokenStreamComponents> {
    let tokenizer = MockTokenizer::new(random_from_seed(self.seed));
    if field_name == "distinctiveFieldName" {
      Ok(TokenStreamComponents::new(
        Box::new(ThrowingTokenFilter::new(tokenizer)) as Box<dyn TokenStream + Send + Sync>,
        None,
      ))
    } else {
      Ok(TokenStreamComponents::new(
        Box::new(tokenizer) as Box<dyn TokenStream + Send + Sync>,
        None,
      ))
    }
  }

  fn stored_value(&self) -> &AnalyzerStoredValue {
    &self.stored_value
  }
}

crate::impl_analyzer_close!(ThrowingAnalyzer);

struct ThrowingTokenFilter<T>
where
  T: TokenStream,
{
  base: TokenFilterBase<T>,
}

impl<T> ThrowingTokenFilter<T>
where
  T: TokenStream,
{
  fn new(input: T) -> Self {
    Self {
      base: TokenFilterBase::new(input),
    }
  }
}

impl<T> crate::core::util::close::Closeable for ThrowingTokenFilter<T>
where
  T: TokenStream,
{
  fn close(&mut self) -> Result<()> {
    crate::core::util::close::Closeable::close(&mut self.base)
  }
}

impl<T> TokenStream for ThrowingTokenFilter<T>
where
  T: TokenStream,
{
  fn increment_token(&mut self) -> Result<bool> {
    Err(LuceneError::illegal_state("Something is icky."))
  }

  fn end(&mut self) -> Result<()> {
    self.base.end()
  }

  fn reset(&mut self) -> Result<()> {
    self.base.reset()
  }

  fn get_attribute_source(&self) -> &Attributes {
    self.base.input.get_attribute_source()
  }

  fn get_attribute_source_mut(&mut self) -> &mut Attributes {
    self.base.input.get_attribute_source_mut()
  }

  fn set_reader(&mut self, input: ReaderEnum) -> Result<()> {
    self.base.input.set_reader(input)
  }

  fn set_reader_test_point(&mut self) -> Result<()> {
    self.base.input.set_reader_test_point()
  }
}

impl<T> TokenFilter for ThrowingTokenFilter<T> where T: TokenStream {}

fn info_bytes_to_string(
  print_stream_info_stream: &PrintStreamInfoStream<Cursor<Vec<u8>>>,
) -> String {
  String::from_utf8(
    print_stream_info_stream
      .stream
      .lock()
      .borrow()
      .as_ref()
      .expect("info stream is still open")
      .get_ref()
      .clone(),
  )
  .expect("info stream should be valid UTF-8")
}
