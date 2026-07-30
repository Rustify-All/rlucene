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
use crate::analysis::common::analysis_impl::core::whitespace_analyzer::WhitespaceAnalyzer;
use crate::core::analysis::reader::{Reader, ReaderEnum};
use crate::core::analysis::reusable_string_reader::ReusableStringReader;
use crate::core::analysis::standard::standard_analyzer::StandardAnalyzer;
use crate::core::analysis::token_stream::{
  AnalyzerTokenStreams, NormalizeTokenStream, TokenStream,
};
use crate::core::index::BytesRef;
use crate::core::util::IOUtils;
use crate::core::util::attribute_source::{AttributeSource, Attributes};
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::impl_from_for_enum;
#[cfg(test)]
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use std::cell::{RefCell, RefMut};
use std::collections::HashMap;
use std::sync::Arc;
use thread_local::ThreadLocal;

pub struct AnalyzerStoredValue {
  reuse_strategy: ReuseStrategyFactory,
  stored_value: ThreadLocal<RefCell<ReuseStrategyEnum>>,
}

impl AnalyzerStoredValue {
  pub fn new() -> Self {
    Self::global()
  }

  pub fn global() -> Self {
    Self {
      reuse_strategy: ReuseStrategyFactory::Global,
      stored_value: ThreadLocal::new(),
    }
  }

  pub fn per_field() -> Self {
    Self {
      reuse_strategy: ReuseStrategyFactory::PerField,
      stored_value: ThreadLocal::new(),
    }
  }

  fn get(&self) -> &RefCell<ReuseStrategyEnum> {
    self
      .stored_value
      .get_or(|| RefCell::new(self.reuse_strategy.new_reuse_strategy()))
  }

  pub fn reuse_strategy(&self) -> RefMut<'_, ReuseStrategyEnum> {
    self.get().borrow_mut()
  }

  pub fn close(&mut self) -> Result<()> {
    self.stored_value.clear();
    Ok(())
  }
}

impl Default for AnalyzerStoredValue {
  fn default() -> Self {
    Self::new()
  }
}

#[macro_export]
macro_rules! impl_analyzer_close {
  ($analyzer:ty) => {
    impl $crate::core::util::close::Closeable for $analyzer {
      fn close(&mut self) -> $crate::core::util::error::lucene_error::Result<()> {
        self.stored_value.close()
      }
    }

    impl Drop for $analyzer {
      fn drop(&mut self) {
        let _ = $crate::core::util::close::Closeable::close(self);
      }
    }
  };
}

enum ReuseStrategyFactory {
  Global,
  PerField,
}

impl ReuseStrategyFactory {
  fn new_reuse_strategy(&self) -> ReuseStrategyEnum {
    match self {
      ReuseStrategyFactory::Global => ReuseStrategyEnum::Global(Box::default()),
      ReuseStrategyFactory::PerField => {
        ReuseStrategyEnum::PerField(PerFieldReuseStrategy::default())
      },
    }
  }
}

pub trait Analyzer: Closeable + Send + Sync {
  fn create_components(&self, field: &str) -> Result<TokenStreamComponents>;
  fn normalize_from_ts(
    &self,
    _field_name: &str,
    in_: NormalizeTokenStream,
  ) -> Result<NormalizeTokenStream> {
    Ok(in_)
  }

  fn token_stream(
    &self,
    field_name: &str,
    input: ReaderEnum,
  ) -> Result<RefMut<'_, AnalyzerTokenStreams>> {
    let reader = self.init_reader(field_name, input);
    let mut reuse_strategy = self.stored_value().reuse_strategy();
    if reuse_strategy
      .get_reusable_components(field_name)?
      .is_none()
    {
      let v = self.create_components(field_name)?;
      reuse_strategy.set_reusable_components(field_name, v)?;
    }

    reuse_strategy
      .get_reusable_components(field_name)?
      .ok_or_else(|| LuceneError::illegal_state("Analyzer token_stream is not initialized"))?
      .set_reader(reader)?;

    RefMut::filter_map(reuse_strategy, |reuse_strategy| {
      match reuse_strategy.get_reusable_components(field_name) {
        Ok(Some(components)) => Some(components.get_token_stream()),
        _ => None,
      }
    })
    .map_err(|_| LuceneError::illegal_state("Analyzer token_stream is not initialized"))
  }

  fn normalize(&self, field_name: &str, text: &str) -> Result<BytesRef<Vec<u8>>> {
    let mut str_reader = ReusableStringReader::new();
    str_reader.set_value(text);
    let mut reader =
      self.init_reader_for_normalization(field_name, ReaderEnum::ReusedString(str_reader));

    let filtered_result =
      std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<String> {
        let mut buf = ['\0'; 64];
        let mut filtered = String::new();
        loop {
          let len = buf.len();
          let read = reader.read_range(&mut buf, 0, len)?;
          if read == -1 {
            break;
          }
          for &ch in &buf[..read as usize] {
            filtered.push(ch);
          }
        }
        Ok(filtered)
      }));
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| reader.close()));
    let filtered = IOUtils::use_or_suppress_caught_result(filtered_result, close_result)?;

    let att = self.attribute_factory(field_name);
    debug_assert!(text.len() <= i32::MAX as usize);
    let mut ts = self.normalize_from_ts(
      field_name,
      StringTokenStream::new(att, &filtered, text.len() as i32).into(),
    )?;

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
      || -> Result<BytesRef<Vec<u8>>> {
        ts.reset()?;
        if !ts.increment_token()? {
          return Err(LuceneError::illegal_state(format!(
            "expected 1 token but got 0 for analyzer and input \"{}\"",
            text
          )));
        }
        let term_att = ts.get_attribute_source_mut();
        let term = match term_att.get_bytes_ref()? {
          Some(t) => BytesRef::deep_copy_of(&*t),
          None => {
            return Err(LuceneError::illegal_state(format!(
              "CharTermAttribute is missing for analyzer and input \"{}\"",
              text
            )));
          },
        };
        if ts.increment_token()? {
          return Err(LuceneError::illegal_state(format!(
            "expected 1 token but got more for analyzer and input \"{}\"",
            text
          )));
        }
        ts.end()?;
        Ok(term)
      },
    ));
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ts.close()));
    IOUtils::use_or_suppress_caught_result(result, close_result)
  }

  fn init_reader(&self, _filed_name: &str, reader: ReaderEnum) -> ReaderEnum {
    reader
  }

  fn init_reader_for_normalization(&self, _filed_name: &str, reader: ReaderEnum) -> ReaderEnum {
    reader
  }

  fn attribute_factory(&self, _field_name: &str) -> Attributes {
    Attributes::default()
  }
  fn get_position_increment_gap(&self, _field_name: &str) -> i32 {
    DEFAULT_POSITION_INCREMENT_GAP
  }
  fn get_offset_gap(&self, _field_name: &str) -> i32 {
    DEFAULT_OFFSET_GAP
  }

  fn stored_value(&self) -> &AnalyzerStoredValue;
}
pub const DEFAULT_OFFSET_GAP: i32 = 1;
pub const DEFAULT_POSITION_INCREMENT_GAP: i32 = 0;

impl<T> Analyzer for Arc<T>
where
  T: Analyzer + ?Sized,
{
  fn create_components(&self, field: &str) -> Result<TokenStreamComponents> {
    (**self).create_components(field)
  }

  fn normalize_from_ts(
    &self,
    field_name: &str,
    in_: NormalizeTokenStream,
  ) -> Result<NormalizeTokenStream> {
    (**self).normalize_from_ts(field_name, in_)
  }

  fn token_stream(
    &self,
    field_name: &str,
    input: ReaderEnum,
  ) -> Result<RefMut<'_, AnalyzerTokenStreams>> {
    (**self).token_stream(field_name, input)
  }

  fn normalize(&self, field_name: &str, text: &str) -> Result<BytesRef<Vec<u8>>> {
    (**self).normalize(field_name, text)
  }

  fn init_reader(&self, field_name: &str, reader: ReaderEnum) -> ReaderEnum {
    (**self).init_reader(field_name, reader)
  }

  fn init_reader_for_normalization(&self, field_name: &str, reader: ReaderEnum) -> ReaderEnum {
    (**self).init_reader_for_normalization(field_name, reader)
  }

  fn attribute_factory(&self, field_name: &str) -> Attributes {
    (**self).attribute_factory(field_name)
  }

  fn get_position_increment_gap(&self, field_name: &str) -> i32 {
    (**self).get_position_increment_gap(field_name)
  }

  fn get_offset_gap(&self, field_name: &str) -> i32 {
    (**self).get_offset_gap(field_name)
  }

  fn stored_value(&self) -> &AnalyzerStoredValue {
    (**self).stored_value()
  }
}
impl_from_for_enum!(
    AnalyzerEnum,
    WhitespaceAnalyzer=> Whitespace,
    StandardAnalyzer => Standard,
);
#[cfg(test)]
impl From<MockAnalyzer> for AnalyzerEnum {
  fn from(v: MockAnalyzer) -> Self {
    AnalyzerEnum::Custom(Box::new(v))
  }
}
impl From<Box<dyn Analyzer>> for AnalyzerEnum {
  fn from(v: Box<dyn Analyzer>) -> Self {
    AnalyzerEnum::Custom(v)
  }
}

pub enum AnalyzerEnum {
  Whitespace(WhitespaceAnalyzer),
  Standard(StandardAnalyzer),
  Custom(Box<dyn Analyzer>),
}
impl Default for AnalyzerEnum {
  fn default() -> Self {
    StandardAnalyzer::default().into()
  }
}

impl Closeable for AnalyzerEnum {
  fn close(&mut self) -> Result<()> {
    match self {
      AnalyzerEnum::Whitespace(v) => v.close(),
      AnalyzerEnum::Standard(v) => v.close(),
      AnalyzerEnum::Custom(v) => v.as_mut().close(),
    }
  }
}

impl Drop for AnalyzerEnum {
  fn drop(&mut self) {
    let _ = self.close();
  }
}

impl Analyzer for AnalyzerEnum {
  fn create_components(&self, field: &str) -> Result<TokenStreamComponents> {
    match self {
      AnalyzerEnum::Whitespace(v) => v.create_components(field),
      AnalyzerEnum::Standard(v) => v.create_components(field),
      AnalyzerEnum::Custom(v) => v.create_components(field),
    }
  }

  fn stored_value(&self) -> &AnalyzerStoredValue {
    match self {
      AnalyzerEnum::Whitespace(v) => v.stored_value(),
      AnalyzerEnum::Standard(v) => v.stored_value(),
      AnalyzerEnum::Custom(v) => v.stored_value(),
    }
  }

  fn normalize_from_ts(
    &self,
    field_name: &str,
    in_: NormalizeTokenStream,
  ) -> Result<NormalizeTokenStream> {
    match self {
      AnalyzerEnum::Whitespace(v) => v.normalize_from_ts(field_name, in_),
      AnalyzerEnum::Standard(v) => v.normalize_from_ts(field_name, in_),
      AnalyzerEnum::Custom(v) => v.normalize_from_ts(field_name, in_),
    }
  }

  fn token_stream(
    &self,
    field_name: &str,
    input: ReaderEnum,
  ) -> Result<RefMut<'_, AnalyzerTokenStreams>> {
    match self {
      AnalyzerEnum::Whitespace(v) => v.token_stream(field_name, input),
      AnalyzerEnum::Standard(v) => v.token_stream(field_name, input),
      AnalyzerEnum::Custom(v) => v.token_stream(field_name, input),
    }
  }

  fn normalize(&self, field_name: &str, text: &str) -> Result<BytesRef<Vec<u8>>> {
    match self {
      AnalyzerEnum::Whitespace(v) => v.normalize(field_name, text),
      AnalyzerEnum::Standard(v) => v.normalize(field_name, text),
      AnalyzerEnum::Custom(v) => v.normalize(field_name, text),
    }
  }

  fn init_reader(&self, _filed_name: &str, reader: ReaderEnum) -> ReaderEnum {
    match self {
      AnalyzerEnum::Whitespace(v) => v.init_reader(_filed_name, reader),
      AnalyzerEnum::Standard(v) => v.init_reader(_filed_name, reader),
      AnalyzerEnum::Custom(v) => v.init_reader(_filed_name, reader),
    }
  }

  fn init_reader_for_normalization(&self, _filed_name: &str, reader: ReaderEnum) -> ReaderEnum {
    match self {
      AnalyzerEnum::Whitespace(v) => v.init_reader_for_normalization(_filed_name, reader),
      AnalyzerEnum::Standard(v) => v.init_reader_for_normalization(_filed_name, reader),
      AnalyzerEnum::Custom(v) => v.init_reader_for_normalization(_filed_name, reader),
    }
  }

  fn attribute_factory(&self, field_name: &str) -> Attributes {
    match self {
      AnalyzerEnum::Whitespace(v) => v.attribute_factory(field_name),
      AnalyzerEnum::Standard(v) => v.attribute_factory(field_name),
      AnalyzerEnum::Custom(v) => v.attribute_factory(field_name),
    }
  }

  fn get_position_increment_gap(&self, field_name: &str) -> i32 {
    match self {
      AnalyzerEnum::Whitespace(v) => v.get_position_increment_gap(field_name),
      AnalyzerEnum::Standard(v) => v.get_position_increment_gap(field_name),
      AnalyzerEnum::Custom(v) => v.get_position_increment_gap(field_name),
    }
  }

  fn get_offset_gap(&self, field_name: &str) -> i32 {
    match self {
      AnalyzerEnum::Whitespace(v) => v.get_offset_gap(field_name),
      AnalyzerEnum::Standard(v) => v.get_offset_gap(field_name),
      AnalyzerEnum::Custom(v) => v.get_offset_gap(field_name),
    }
  }
}

pub enum ReuseStrategyEnum {
  Global(Box<GlobalReuseStrategy>),
  PerField(PerFieldReuseStrategy),
}

impl ReuseStrategy for ReuseStrategyEnum {
  fn get_reusable_components(
    &mut self,
    field_name: &str,
  ) -> Result<Option<&mut TokenStreamComponents>> {
    match self {
      ReuseStrategyEnum::Global(v) => v.get_reusable_components(field_name),
      ReuseStrategyEnum::PerField(v) => v.get_reusable_components(field_name),
    }
  }

  fn set_reusable_components(
    &mut self,
    field_name: &str,
    components: TokenStreamComponents,
  ) -> Result<()> {
    match self {
      ReuseStrategyEnum::Global(v) => v.set_reusable_components(field_name, components),
      ReuseStrategyEnum::PerField(v) => v.set_reusable_components(field_name, components),
    }
  }
}

pub trait ReuseStrategy {
  fn get_reusable_components(
    &mut self,
    field_name: &str,
  ) -> Result<Option<&mut TokenStreamComponents>>;
  fn set_reusable_components(
    &mut self,
    field_name: &str,
    components: TokenStreamComponents,
  ) -> Result<()>;
}
pub struct GlobalReuseStrategy {
  store_value: Option<TokenStreamComponents>,
  first: bool,
}
impl Default for GlobalReuseStrategy {
  fn default() -> Self {
    Self {
      store_value: None,
      first: true,
    }
  }
}
impl ReuseStrategy for GlobalReuseStrategy {
  fn get_reusable_components(
    &mut self,
    _field_name: &str,
  ) -> Result<Option<&mut TokenStreamComponents>> {
    match self.store_value {
      Some(ref mut v) => Ok(Some(v)),
      _ => Ok(None),
    }
  }

  fn set_reusable_components(
    &mut self,
    _field_name: &str,
    components: TokenStreamComponents,
  ) -> Result<()> {
    if self.first {
      self.first = false;
      self.store_value = Some(components);
      return Ok(());
    }
    let v = self
      .store_value
      .as_mut()
      .ok_or_else(|| LuceneError::already_closed("this Analyzer is closed"))?;
    *v = components;
    Ok(())
  }
}

pub struct PerFieldReuseStrategy {
  store_value: Option<HashMap<String, TokenStreamComponents>>,
}
impl Default for PerFieldReuseStrategy {
  fn default() -> Self {
    Self {
      store_value: Some(HashMap::new()),
    }
  }
}
impl ReuseStrategy for PerFieldReuseStrategy {
  fn get_reusable_components(
    &mut self,
    field_name: &str,
  ) -> Result<Option<&mut TokenStreamComponents>> {
    match self.store_value {
      Some(ref mut v) => Ok(v.get_mut(field_name)),
      _ => Ok(None),
    }
  }

  fn set_reusable_components(
    &mut self,
    field_name: &str,
    components: TokenStreamComponents,
  ) -> Result<()> {
    match self.store_value {
      Some(ref mut v) => {
        let _ = v.insert(field_name.to_string(), components);
        Ok(())
      },
      None => Err(LuceneError::already_closed("this Analyzer is closed")),
    }
  }
}

pub struct TokenStreamComponents {
  sink: AnalyzerTokenStreams,
  max_token_length: Option<usize>,
}
impl TokenStreamComponents {
  pub fn new<T>(sink: T, max_token_length: Option<usize>) -> Self
  where
    T: Into<AnalyzerTokenStreams>,
  {
    Self {
      sink: sink.into(),
      max_token_length,
    }
  }
  fn set_reader(&mut self, reader: ReaderEnum) -> Result<()> {
    match self.sink {
      AnalyzerTokenStreams::Standard(ref mut ts) => {
        let src = &mut ts.base.input.token_filter_base.input;
        src.set_reader(reader)?;
        let max_token_length = self
          .max_token_length
          .ok_or_else(|| LuceneError::illegal_state("max_token_length is not set"))?;
        src.set_max_token_length(max_token_length)?;
      },
      AnalyzerTokenStreams::Whitespace(ref mut ts) => {
        ts.set_reader(reader)?;
      },
      AnalyzerTokenStreams::Custom(ref mut ts) => {
        ts.set_reader(reader)?;
      },
      _ => return Err(LuceneError::unsupported_operation("")),
    }
    Ok(())
  }
  pub fn get_token_stream(&mut self) -> &mut AnalyzerTokenStreams {
    &mut self.sink
  }
}

pub struct StringTokenStream {
  value: String,
  length: i32,
  used: bool,
  att: Attributes,
}
impl StringTokenStream {
  fn new(att: Attributes, value: &str, length: i32) -> Self {
    Self {
      value: value.to_string(),
      length,
      used: true,
      att,
    }
  }
}

impl Drop for StringTokenStream {
  fn drop(&mut self) {
    let _ = self.close();
  }
}

impl Closeable for StringTokenStream {}

impl TokenStream for StringTokenStream {
  fn increment_token(&mut self) -> Result<bool> {
    if self.used {
      return Ok(false);
    }
    self.att.clear_attributes()?;
    self.att.append_str(Some(&self.value))?;
    self.att.set_offset(0, self.length)?;
    self.used = true;
    Ok(true)
  }

  fn end(&mut self) -> Result<()> {
    self.default_end()?;
    self.att.set_offset(self.length, self.length)
  }

  fn reset(&mut self) -> Result<()> {
    self.used = false;
    Ok(())
  }

  fn get_attribute_source(&self) -> &Attributes {
    &self.att
  }

  fn get_attribute_source_mut(&mut self) -> &mut Attributes {
    &mut self.att
  }
}
