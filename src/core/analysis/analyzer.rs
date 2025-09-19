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
use crate::core::analysis::reader::{Reader, ReaderEnum};
use crate::core::analysis::reusable_string_reader::ReusableStringReader;
use crate::core::analysis::token_attributes::char_term_attribute::CharTermAttribute;
use crate::core::analysis::token_attributes::offset_attribute::OffsetAttribute;
use crate::core::analysis::token_stream::{InnerTokenStreams, TokenStream};
use crate::core::index::BytesRef;
use crate::core::util::attribute_source::{AttributeSource, Attributes};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::cell::RefCell;
use std::collections::HashMap;
use std::marker::PhantomData;
thread_local! {
    pub static REUSE_STRATEGY: RefCell<Option<ReuseStrategyEnum>> = const { RefCell::new(None) };
}

pub trait Analyzer {
    fn create_components(&self, field: &str) -> Result<TokenStreamComponents<InnerTokenStreams>>;
    /// Default reuse strategy is GlobalReuseStrategy
    fn init_reuse_strategy(&self) -> ReuseStrategyEnum {
        ReuseStrategyEnum::Global(GlobalReuseStrategy::default())
    }
    fn normalize_with_ts<TS>(&self, _field_name: &str, in_: TS) -> Result<impl TokenStream>
    where
        TS: TokenStream,
    {
        Ok(in_)
    }
    fn ensure_reuse_strategy<'a>(
        &'a self,
        slot: &'a mut Option<ReuseStrategyEnum>,
    ) -> &'a mut ReuseStrategyEnum {
        if slot.is_none() {
            *slot = Some(self.init_reuse_strategy());
        }
        slot.as_mut().unwrap()
    }
    fn token_stream<R>(&self, field_name: &str, input: R) -> Result<()>
    where
        R: Into<ReaderEnum>,
    {
        let reader = self.init_reader(field_name, input.into());
        REUSE_STRATEGY.with(move |reuse_strategy| {
            (|| -> Result<()> {
                let mut reuse_strategy = reuse_strategy.borrow_mut();
                let reuse_strategy = self.ensure_reuse_strategy(&mut reuse_strategy);

                let mut components = reuse_strategy.get_reusable_components(field_name)?;
                if components.is_none() {
                    let v = self.create_components(field_name)?;
                    reuse_strategy.set_reusable_components(field_name, v)?;
                    components = reuse_strategy.get_reusable_components(field_name)?;
                }

                let components = components.as_mut().unwrap();
                components.set_reader(reader)?;
                Ok(())
            })()
        })?;
        Ok(())
    }

    fn normalize(&self, field_name: &str, text: &str) -> Result<BytesRef<Vec<u8>>> {
        let mut str_reader = ReusableStringReader::new();
        str_reader.set_value(text);
        let mut reader =
            self.init_reader_for_normalization(field_name, ReaderEnum::ReusedString(str_reader));

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

        let att = self.attribute_factory(field_name);
        debug_assert!(text.len() <= i32::MAX as usize);
        let mut ts = self.normalize_with_ts(
            field_name,
            StringTokenStream::new(att, &filtered, text.len() as i32),
        )?;

        ts.reset()?;
        if !ts.increment_token()? {
            return Err(LuceneError::illegal_state(format!(
                "expected 1 token but got 0 for analyzer and input \"{}\"",
                text
            )));
        }
        let term_att = ts.get_attribute_source_mut();
        let term = match term_att.get_bytes_ref() {
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
        0
    }
    fn get_offset_gap(&self, _field_name: &str) -> i32 {
        1
    }
}
pub enum ReuseStrategyEnum {
    Global(GlobalReuseStrategy<InnerTokenStreams>),
    PerField(PerFieldReuseStrategy<InnerTokenStreams>),
}
impl ReuseStrategy<InnerTokenStreams> for ReuseStrategyEnum {
    fn get_reusable_components(
        &mut self,
        field_name: &str,
    ) -> Result<Option<&mut TokenStreamComponents<InnerTokenStreams>>> {
        match self {
            ReuseStrategyEnum::Global(v) => v.get_reusable_components(field_name),
            ReuseStrategyEnum::PerField(v) => v.get_reusable_components(field_name),
        }
    }

    fn set_reusable_components(
        &mut self,
        field_name: &str,
        components: TokenStreamComponents<InnerTokenStreams>,
    ) -> Result<()> {
        match self {
            ReuseStrategyEnum::Global(v) => v.set_reusable_components(field_name, components),
            ReuseStrategyEnum::PerField(v) => v.set_reusable_components(field_name, components),
        }
    }
}
pub struct AnalyzerBase<TS, RS>
where
    TS: TokenStream,
    RS: ReuseStrategy<TS>,
{
    reuse_strategy: RS,
    _phantom: PhantomData<TS>,
}
impl<TS> AnalyzerBase<TS, GlobalReuseStrategy<TS>>
where
    TS: TokenStream,
{
    pub(crate) fn new() -> Self {
        Self {
            reuse_strategy: GlobalReuseStrategy::default(),
            _phantom: PhantomData,
        }
    }
}
impl<TS, RS> AnalyzerBase<TS, RS>
where
    TS: TokenStream,
    RS: ReuseStrategy<TS>,
{
    fn with_rs(reuse_strategy: RS) -> Self {
        Self {
            reuse_strategy,
            _phantom: PhantomData,
        }
    }
}

pub trait ReuseStrategy<TS>
where
    TS: TokenStream,
{
    fn get_reusable_components(
        &mut self,
        field_name: &str,
    ) -> Result<Option<&mut TokenStreamComponents<TS>>>;
    fn set_reusable_components(
        &mut self,
        field_name: &str,
        components: TokenStreamComponents<TS>,
    ) -> Result<()>;
}
pub struct GlobalReuseStrategy<TS>
where
    TS: TokenStream,
{
    store_value: Option<TokenStreamComponents<TS>>,
    first: bool,
}
impl<TS> Default for GlobalReuseStrategy<TS>
where
    TS: TokenStream,
{
    fn default() -> Self {
        Self {
            store_value: None,
            first: true,
        }
    }
}
impl<TS> ReuseStrategy<TS> for GlobalReuseStrategy<TS>
where
    TS: TokenStream,
{
    fn get_reusable_components(
        &mut self,
        _field_name: &str,
    ) -> Result<Option<&mut TokenStreamComponents<TS>>> {
        match self.store_value {
            Some(ref mut v) => Ok(Some(v)),
            _ => Ok(None),
        }
    }

    fn set_reusable_components(
        &mut self,
        _field_name: &str,
        components: TokenStreamComponents<TS>,
    ) -> Result<()> {
        if self.first {
            self.first = false;
            self.store_value = Some(components);
            return Ok(());
        }
        match self.store_value {
            Some(ref mut v) => {
                *v = components;
                Ok(())
            },
            None => Err(LuceneError::already_closed("this Analyzer is closed")),
        }
    }
}
#[derive(Default)]
pub struct PerFieldReuseStrategy<TS>
where
    TS: TokenStream,
{
    store_value: Option<HashMap<String, TokenStreamComponents<TS>>>,
}
impl<TS> ReuseStrategy<TS> for PerFieldReuseStrategy<TS>
where
    TS: TokenStream,
{
    fn get_reusable_components(
        &mut self,
        field_name: &str,
    ) -> Result<Option<&mut TokenStreamComponents<TS>>>
    where
        TS: TokenStream,
    {
        match self.store_value {
            Some(ref mut v) => Ok(v.get_mut(field_name)),
            _ => Ok(None),
        }
    }

    fn set_reusable_components(
        &mut self,
        field_name: &str,
        components: TokenStreamComponents<TS>,
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

pub struct TokenStreamComponents<TS>
where
    TS: TokenStream,
{
    sink: Option<TS>,
}
impl<TS> TokenStreamComponents<TS>
where
    TS: TokenStream,
{
    pub fn new(sink: TS) -> Self {
        Self { sink: Some(sink) }
    }
    fn set_reader(&mut self, reader: ReaderEnum) -> Result<()> {
        self.sink.as_mut().unwrap().set_reader(reader)
    }
    pub fn get_token_stream(&mut self) -> &mut TS {
        self.sink.as_mut().unwrap()
    }
    pub fn take_token_stream(&mut self) -> Option<TS> {
        self.sink.take()
    }
}

struct StringTokenStream {
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
        self.close().expect("should not fail");
    }
}

impl TokenStream for StringTokenStream {
    fn increment_token(&mut self) -> Result<bool> {
        if self.used {
            return Ok(false);
        }
        self.att.clear_attributes();
        self.att.append_str(Some(&self.value));
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
