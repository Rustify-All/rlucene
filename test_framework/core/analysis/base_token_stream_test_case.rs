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
use crate::core::analysis::analyzer::{Analyzer, ReuseStrategy};
use crate::core::analysis::reader::ReaderEnum;
use crate::core::analysis::token_attributes::{
  char_term_attribute, flags_attribute, keyword_attribute, offset_attribute, payload_attribute,
  position_increment_attribute, position_length_attribute, term_to_bytes_ref_attribute,
  type_attribute,
};
use crate::core::analysis::token_stream::AnalyzerTokenStreams;
use crate::core::analysis::token_stream::TokenStream;
use crate::core::document::field::Field;
use crate::core::index::BytesRef;
use crate::core::search::boost_attribute;
use crate::core::util::attribute::Attribute;
use crate::core::util::attribute_impl::AttributeImpl;
use crate::core::util::attribute_source::AttributeSource;
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::io_utils::IOUtils;
use rand::Rng;
use std::collections::HashMap;

#[allow(dead_code)] // for quick search
struct BaseTokenStreamTestCase;

pub trait CheckClearAttributesAttribute: AttributeImpl {
  const ATTRIBUTE_NAME: &'static str = NAME;

  fn get_and_reset_clear_called(&mut self) -> bool;
}

pub const NAME: &str = "CheckClearAttributesAttribute";
pub struct CheckClearAttributesAttributeImpl {
  clear_called: bool,
}
impl Default for CheckClearAttributesAttributeImpl {
  fn default() -> Self {
    Self::new()
  }
}

impl CheckClearAttributesAttributeImpl {
  pub fn new() -> Self {
    CheckClearAttributesAttributeImpl {
      clear_called: false,
    }
  }
}

impl Attribute for CheckClearAttributesAttributeImpl {}

impl AttributeImpl for CheckClearAttributesAttributeImpl {
  fn clear(&mut self) {
    self.clear_called = true;
  }

  type AttributeImpl = CheckClearAttributesAttributeImpl;

  fn copy_to(&self, other: &mut Self::AttributeImpl) -> Result<()> {
    other.clear();
    Ok(())
  }
}

impl Clone for CheckClearAttributesAttributeImpl {
  fn clone(&self) -> Self {
    Self {
      clear_called: self.clear_called,
    }
  }
}

impl CheckClearAttributesAttribute for CheckClearAttributesAttributeImpl {
  fn get_and_reset_clear_called(&mut self) -> bool {
    let v = self.clear_called;
    self.clear_called = false;
    v
  }
}
#[allow(clippy::too_many_arguments)]
fn assert_token_stream_contents<TS>(
  ts: &mut TS,
  output: &[&str],
  start_offsets: Option<&[i32]>,
  end_offsets: Option<&[i32]>,
  types: Option<&[&str]>,
  pos_increments: Option<&[i32]>,
  pos_lengths: Option<&[i32]>,
  final_offset: Option<i32>,
  final_pos_inc: Option<i32>,
  keyword_atts: Option<&[bool]>,
  graph_offsets_are_correct: bool,
  payloads: Option<&[Option<Vec<u8>>]>,
  flags: Option<&[i32]>,
  boost: Option<&[f32]>,
) -> Result<()>
where
  TS: TokenStream,
{
  let (
    offset_att,
    type_att,
    pos_incr_att,
    pos_length_att,
    keyword_att,
    payload_att,
    flags_att,
    boost_att,
  ) = {
    let attr = ts.get_attribute_source();
    let attribute_names = attr.get_attribute_name()?;

    if !output.is_empty() {
      assert!(attribute_names.contains(char_term_attribute::NAME));
      assert!(attribute_names.contains(term_to_bytes_ref_attribute::NAME));
      // TODO IMPORTANT BytesRefBuilderTermAttributeImpl未实现
    }

    let mut offset_att = false;
    if start_offsets.is_some() || end_offsets.is_some() || final_offset.is_some() {
      assert!(attribute_names.contains(offset_attribute::NAME));
      offset_att = true;
    }

    let mut type_att = false;
    if types.is_some() {
      assert!(attribute_names.contains(type_attribute::NAME));
      type_att = true;
    }

    let mut pos_incr_att = false;
    if pos_increments.is_some() || final_pos_inc.is_some() {
      assert!(attribute_names.contains(position_increment_attribute::NAME));
      pos_incr_att = true;
    }

    let mut pos_length_att = false;
    if pos_lengths.is_some() {
      assert!(attribute_names.contains(position_length_attribute::NAME));
      pos_length_att = true;
    }

    let mut keyword_att = false;
    if keyword_atts.is_some() {
      assert!(attribute_names.contains(keyword_attribute::NAME));
      keyword_att = true;
    }

    let mut payload_att = false;
    if payloads.is_some() {
      assert!(attribute_names.contains(payload_attribute::NAME));
      payload_att = true;
    }

    let mut flags_att = false;
    if flags.is_some() {
      assert!(attribute_names.contains(flags_attribute::NAME));
      flags_att = true;
    }

    let mut boost_att = false;
    if boost.is_some() {
      assert!(attribute_names.contains(boost_attribute::NAME));
      boost_att = true;
    }

    (
      offset_att,
      type_att,
      pos_incr_att,
      pos_length_att,
      keyword_att,
      payload_att,
      flags_att,
      boost_att,
    )
  };

  let mut pos_to_start_offset: HashMap<i32, i32> = HashMap::new();
  let mut pos_to_end_offset: HashMap<i32, i32> = HashMap::new();

  ts.reset()?;
  let mut pos = -1;
  let mut last_start_offset = 0;

  for i in 0..output.len() {
    ts.get_attribute_source_mut().clear_attributes()?;
    {
      let attr = ts.get_attribute_source_mut();
      attr.set_empty()?.append_str(Some("bogusTerm"))?;

      if offset_att {
        attr.set_offset(14584724, 24683243)?;
      }
      if type_att {
        attr.set_type("bogusType")?;
      }
      if pos_incr_att {
        attr.set_position_increment(45987657)?;
      }
      if pos_length_att {
        attr.set_position_length(45987653)?;
      }
      if keyword_att {
        attr.set_keyword((i & 1) == 0)?;
      }
      if payload_att {
        attr.set_payload(Some(BytesRef::from_bytes(vec![
          0x00,
          (-0x21i8) as u8,
          0x12,
          (-0x43i8) as u8,
          0x24,
        ])))?;
      }
      if flags_att {
        attr.set_flags(!0)?;
      }
      if boost_att {
        attr.set_boost(-1.0)?;
      }
    }
    {
      let attr = ts.get_attribute_source_mut();
      attr.get_and_reset_clear_called()?;
    }
    assert!(ts.increment_token()?, "token {} does not exist", i);
    let attr = ts.get_attribute_source_mut();
    assert!(
      attr.get_and_reset_clear_called()?,
      "clearAttributes() was not called correctly in TokenStream chain at token {}",
      i
    );

    assert_eq!(output[i], attr.to_string(), "term {}", i);

    if let Some(start_offsets) = start_offsets {
      assert_eq!(start_offsets[i], attr.start_offset()?);
    }

    if let Some(end_offsets) = end_offsets {
      assert_eq!(end_offsets[i], attr.end_offset()?);
    }
    if let Some(types) = types {
      assert_eq!(types[i], attr.type_()?);
    }
    if let Some(pos_increments) = pos_increments {
      assert_eq!(pos_increments[i], attr.get_position_increment()?);
    }
    if let Some(pos_lengths) = pos_lengths {
      assert_eq!(pos_lengths[i], attr.get_position_length()?);
    }
    if let Some(keyword_atts) = keyword_atts {
      assert_eq!(keyword_atts[i], attr.is_keyword()?);
    }
    if let Some(flags) = flags {
      assert_eq!(flags[i], attr.get_flags()?);
    }
    if let Some(boost) = boost {
      assert!((boost[i] - attr.get_boost()?).abs() <= 0.001);
    }
    if let Some(payloads) = payloads
      && let Some(payload) = &payloads[i]
    {
      let actual = attr.get_payload()?.unwrap();
      assert_eq!(
        payload.as_slice(),
        &actual.bytes[actual.offset..actual.offset + actual.length]
      );
    }
    if pos_incr_att {
      if i == 0 {
        assert!(attr.get_position_increment()? >= 1);
      } else {
        assert!(attr.get_position_increment()? >= 0);
      }
    }
    if pos_length_att {
      assert!(attr.get_position_length()? >= 1);
    }

    if offset_att {
      let start_offset = attr.start_offset()?;
      let end_offset = attr.end_offset()?;

      if let Some(final_offset) = final_offset {
        assert!(start_offset <= final_offset);
        assert!(end_offset <= final_offset);
      }

      assert!(attr.start_offset()? >= last_start_offset);
      last_start_offset = attr.start_offset()?;

      if graph_offsets_are_correct && pos_length_att && pos_incr_att {
        let pos_inc = attr.get_position_increment()?;
        pos += pos_inc;

        let pos_length = attr.get_position_length()?;

        if let Some(expected_start_offset) = pos_to_start_offset.get(&pos) {
          assert_eq!(*expected_start_offset, start_offset);
        } else {
          pos_to_start_offset.insert(pos, start_offset);
        }

        let end_pos = pos + pos_length;

        if let Some(expected_end_offset) = pos_to_end_offset.get(&end_pos) {
          assert_eq!(*expected_end_offset, end_offset);
        } else {
          pos_to_end_offset.insert(end_pos, end_offset);
        }
      }
    }
  }

  if ts.increment_token()? {
    unreachable!("")
  }

  {
    let attr = ts.get_attribute_source_mut();
    attr.clear_attributes()?;
    if !output.is_empty() {
      attr.set_empty()?.append_str(Some("bogusTerm"))?;
    }
    if offset_att {
      attr.set_offset(14584724, 24683243)?;
    }
    if type_att {
      attr.set_type("bogusType")?;
    }
    if pos_incr_att {
      attr.set_position_increment(45987657)?;
    }
    if pos_length_att {
      attr.set_position_length(45987653)?;
    }
    if keyword_att {
      attr.set_keyword(true)?;
    }
    if payload_att {
      attr.set_payload(Some(BytesRef::from_bytes(vec![
        0x00,
        (-0x21i8) as u8,
        0x12,
        (-0x43i8) as u8,
        0x24,
      ])))?;
    }
    if flags_att {
      attr.set_flags(!0)?;
    }
    if boost_att {
      attr.set_boost(-1.0)?;
    }
    attr.get_and_reset_clear_called()?;
  }

  ts.end()?;
  assert!(ts.get_attribute_source_mut().get_and_reset_clear_called()?);
  let attr = ts.get_attribute_source();
  if let Some(final_offset) = final_offset {
    assert_eq!(final_offset, attr.end_offset()?);
  }
  if offset_att {
    assert!(attr.end_offset()? >= 0);
  }
  if let Some(final_pos_inc) = final_pos_inc {
    assert_eq!(final_pos_inc, attr.get_position_increment()?);
  }

  ts.close()?;
  Ok(())
}

#[allow(unused)]
#[allow(clippy::too_many_arguments)]
pub fn assert_token_stream_contents1<TS>(
  ts: &mut TS,
  output: &[&str],
  start_offsets: Option<&[i32]>,
  end_offsets: Option<&[i32]>,
  types: Option<&[&str]>,
  pos_increments: Option<&[i32]>,
  pos_lengths: Option<&[i32]>,
  final_offset: Option<i32>,
  final_pos_inc: Option<i32>,
  keyword_atts: Option<&[bool]>,
  graph_offsets_are_correct: bool,
  payloads: Option<&[Option<Vec<u8>>]>,
  flags: Option<&[i32]>,
) -> Result<()>
where
  TS: TokenStream,
{
  assert_token_stream_contents(
    ts,
    output,
    start_offsets,
    end_offsets,
    types,
    pos_increments,
    pos_lengths,
    final_offset,
    final_pos_inc,
    keyword_atts,
    graph_offsets_are_correct,
    payloads,
    flags,
    None,
  )
}

#[allow(unused)]
#[allow(clippy::too_many_arguments)]
pub fn assert_token_stream_contents2<TS>(
  ts: &mut TS,
  output: &[&str],
  start_offsets: Option<&[i32]>,
  end_offsets: Option<&[i32]>,
  types: Option<&[&str]>,
  pos_increments: Option<&[i32]>,
  pos_lengths: Option<&[i32]>,
  final_offset: Option<i32>,
  keyword_atts: Option<&[bool]>,
  graph_offsets_are_correct: bool,
) -> Result<()>
where
  TS: TokenStream,
{
  assert_token_stream_contents3(
    ts,
    output,
    start_offsets,
    end_offsets,
    types,
    pos_increments,
    pos_lengths,
    final_offset,
    keyword_atts,
    graph_offsets_are_correct,
    None,
  )
}

#[allow(clippy::too_many_arguments)]
pub fn assert_token_stream_contents3<TS>(
  ts: &mut TS,
  output: &[&str],
  start_offsets: Option<&[i32]>,
  end_offsets: Option<&[i32]>,
  types: Option<&[&str]>,
  pos_increments: Option<&[i32]>,
  pos_lengths: Option<&[i32]>,
  final_offset: Option<i32>,
  keyword_atts: Option<&[bool]>,
  graph_offsets_are_correct: bool,
  boost: Option<&[f32]>,
) -> Result<()>
where
  TS: TokenStream,
{
  assert_token_stream_contents(
    ts,
    output,
    start_offsets,
    end_offsets,
    types,
    pos_increments,
    pos_lengths,
    final_offset,
    None,
    keyword_atts,
    graph_offsets_are_correct,
    None,
    None,
    boost,
  )
}

#[allow(clippy::too_many_arguments)]
pub fn assert_token_stream_contents4<TS>(
  ts: &mut TS,
  output: &[&str],
  start_offsets: Option<&[i32]>,
  end_offsets: Option<&[i32]>,
  types: Option<&[&str]>,
  pos_increments: Option<&[i32]>,
  pos_lengths: Option<&[i32]>,
  final_offset: Option<i32>,
  final_pos_inc: Option<i32>,
  keyword_atts: Option<&[bool]>,
  graph_offsets_are_correct: bool,
  payloads: Option<&[Option<Vec<u8>>]>,
) -> Result<()>
where
  TS: TokenStream,
{
  assert_token_stream_contents(
    ts,
    output,
    start_offsets,
    end_offsets,
    types,
    pos_increments,
    pos_lengths,
    final_offset,
    final_pos_inc,
    keyword_atts,
    graph_offsets_are_correct,
    payloads,
    None,
    None,
  )
}

#[allow(clippy::too_many_arguments)]
pub fn assert_token_stream_contents5<TS>(
  ts: &mut TS,
  output: &[&str],
  start_offsets: Option<&[i32]>,
  end_offsets: Option<&[i32]>,
  types: Option<&[&str]>,
  pos_increments: Option<&[i32]>,
  pos_lengths: Option<&[i32]>,
  final_offset: Option<i32>,
  graph_offsets_are_correct: bool,
  boost: Option<&[f32]>,
) -> Result<()>
where
  TS: TokenStream,
{
  assert_token_stream_contents3(
    ts,
    output,
    start_offsets,
    end_offsets,
    types,
    pos_increments,
    pos_lengths,
    final_offset,
    None,
    graph_offsets_are_correct,
    boost,
  )
}

#[allow(clippy::too_many_arguments)]
pub fn assert_token_stream_contents6<TS>(
  ts: &mut TS,
  output: &[&str],
  start_offsets: Option<&[i32]>,
  end_offsets: Option<&[i32]>,
  types: Option<&[&str]>,
  pos_increments: Option<&[i32]>,
  pos_lengths: Option<&[i32]>,
  final_offset: Option<i32>,
  graph_offsets_are_correct: bool,
) -> Result<()>
where
  TS: TokenStream,
{
  assert_token_stream_contents3(
    ts,
    output,
    start_offsets,
    end_offsets,
    types,
    pos_increments,
    pos_lengths,
    final_offset,
    None,
    graph_offsets_are_correct,
    None,
  )
}

#[allow(clippy::too_many_arguments)]
pub fn assert_token_stream_contents7<TS>(
  ts: &mut TS,
  output: &[&str],
  start_offsets: Option<&[i32]>,
  end_offsets: Option<&[i32]>,
  types: Option<&[&str]>,
  pos_increments: Option<&[i32]>,
  pos_lengths: Option<&[i32]>,
  final_offset: Option<i32>,
) -> Result<()>
where
  TS: TokenStream,
{
  assert_token_stream_contents6(
    ts,
    output,
    start_offsets,
    end_offsets,
    types,
    pos_increments,
    pos_lengths,
    final_offset,
    true,
  )
}

#[allow(clippy::too_many_arguments)]
pub fn assert_token_stream_contents8<TS>(
  ts: &mut TS,
  output: &[&str],
  start_offsets: Option<&[i32]>,
  end_offsets: Option<&[i32]>,
  types: Option<&[&str]>,
  pos_increments: Option<&[i32]>,
  pos_lengths: Option<&[i32]>,
  final_offset: Option<i32>,
  boost: Option<&[f32]>,
) -> Result<()>
where
  TS: TokenStream,
{
  assert_token_stream_contents5(
    ts,
    output,
    start_offsets,
    end_offsets,
    types,
    pos_increments,
    pos_lengths,
    final_offset,
    true,
    boost,
  )
}

#[allow(unused)]
pub fn assert_token_stream_contents9<TS>(
  ts: &mut TS,
  output: &[&str],
  start_offsets: Option<&[i32]>,
  end_offsets: Option<&[i32]>,
  types: Option<&[&str]>,
  pos_increments: Option<&[i32]>,
  final_offset: Option<i32>,
) -> Result<()>
where
  TS: TokenStream,
{
  assert_token_stream_contents7(
    ts,
    output,
    start_offsets,
    end_offsets,
    types,
    pos_increments,
    None,
    final_offset,
  )
}

#[allow(unused)]
pub fn assert_token_stream_contents10<TS>(
  ts: &mut TS,
  output: &[&str],
  start_offsets: Option<&[i32]>,
  end_offsets: Option<&[i32]>,
  types: Option<&[&str]>,
  pos_increments: Option<&[i32]>,
) -> Result<()>
where
  TS: TokenStream,
{
  assert_token_stream_contents7(
    ts,
    output,
    start_offsets,
    end_offsets,
    types,
    pos_increments,
    None,
    None,
  )
}

#[allow(unused)]
pub fn assert_token_stream_contents11<TS>(
  ts: &mut TS,
  output: &[&str],
  start_offsets: Option<&[i32]>,
  end_offsets: Option<&[i32]>,
  types: Option<&[&str]>,
  pos_increments: Option<&[i32]>,
  pos_lengths: Option<&[i32]>,
) -> Result<()>
where
  TS: TokenStream,
{
  assert_token_stream_contents7(
    ts,
    output,
    start_offsets,
    end_offsets,
    types,
    pos_increments,
    pos_lengths,
    None,
  )
}

#[allow(non_snake_case)]
pub fn assert_token_stream_contents12<TS>(ts: &mut TS, output: &[&str]) -> Result<()>
where
  TS: TokenStream,
{
  assert_token_stream_contents7(ts, output, None, None, None, None, None, None)
}

#[allow(unused)]
pub fn assert_token_stream_contents13<TS>(
  ts: &mut TS,
  output: &[&str],
  types: Option<&[&str]>,
) -> Result<()>
where
  TS: TokenStream,
{
  assert_token_stream_contents7(ts, output, None, None, types, None, None, None)
}

#[allow(unused)]
pub fn assert_token_stream_contents14<TS>(
  ts: &mut TS,
  output: &[&str],
  pos_increments: Option<&[i32]>,
) -> Result<()>
where
  TS: TokenStream,
{
  assert_token_stream_contents7(ts, output, None, None, None, pos_increments, None, None)
}

#[allow(unused)]
pub fn assert_token_stream_contents15<TS>(
  ts: &mut TS,
  output: &[&str],
  start_offsets: Option<&[i32]>,
  end_offsets: Option<&[i32]>,
) -> Result<()>
where
  TS: TokenStream,
{
  assert_token_stream_contents7(
    ts,
    output,
    start_offsets,
    end_offsets,
    None,
    None,
    None,
    None,
  )
}

#[allow(unused)]
pub fn assert_token_stream_contents16<TS>(
  ts: &mut TS,
  output: &[&str],
  start_offsets: Option<&[i32]>,
  end_offsets: Option<&[i32]>,
  final_offset: Option<i32>,
) -> Result<()>
where
  TS: TokenStream,
{
  assert_token_stream_contents7(
    ts,
    output,
    start_offsets,
    end_offsets,
    None,
    None,
    None,
    final_offset,
  )
}

#[allow(unused)]
pub fn assert_token_stream_contents17<TS>(
  ts: &mut TS,
  output: &[&str],
  start_offsets: Option<&[i32]>,
  end_offsets: Option<&[i32]>,
  pos_increments: Option<&[i32]>,
) -> Result<()>
where
  TS: TokenStream,
{
  assert_token_stream_contents7(
    ts,
    output,
    start_offsets,
    end_offsets,
    None,
    pos_increments,
    None,
    None,
  )
}

#[allow(non_snake_case)]
pub fn assert_token_stream_contents18<TS>(
  ts: &mut TS,
  output: &[&str],
  start_offsets: Option<&[i32]>,
  end_offsets: Option<&[i32]>,
  pos_increments: Option<&[i32]>,
  final_offset: Option<i32>,
) -> Result<()>
where
  TS: TokenStream,
{
  assert_token_stream_contents7(
    ts,
    output,
    start_offsets,
    end_offsets,
    None,
    pos_increments,
    None,
    final_offset,
  )
}

#[allow(unused)]
pub fn assert_token_stream_contents19<TS>(
  ts: &mut TS,
  output: &[&str],
  start_offsets: Option<&[i32]>,
  end_offsets: Option<&[i32]>,
  pos_increments: Option<&[i32]>,
  pos_lengths: Option<&[i32]>,
  final_offset: Option<i32>,
) -> Result<()>
where
  TS: TokenStream,
{
  assert_token_stream_contents7(
    ts,
    output,
    start_offsets,
    end_offsets,
    None,
    pos_increments,
    pos_lengths,
    final_offset,
  )
}

fn with_analyzer_token_stream<A, F>(a: &A, input: &str, f: F) -> Result<()>
where
  A: Analyzer,
  F: FnOnce(&mut AnalyzerTokenStreams) -> Result<()>,
{
  let field = "dummy";
  let mut ts = a.token_stream(field, ReaderEnum::from(input))?;
  f(&mut ts)
}

#[allow(unused)]
#[allow(clippy::too_many_arguments)]
pub fn assert_analyzes_to1<A, R>(
  random: &mut R,
  a: &A,
  input: &str,
  output: &[&str],
  start_offsets: Option<&[i32]>,
  end_offsets: Option<&[i32]>,
  types: Option<&[&str]>,
  pos_increments: Option<&[i32]>,
) -> Result<()>
where
  A: Analyzer,
  R: Rng + ?Sized,
{
  with_analyzer_token_stream(a, input, |ts| {
    assert_token_stream_contents10(
      ts,
      output,
      start_offsets,
      end_offsets,
      types,
      pos_increments,
    )
  })?;
  check_reset_exception(a, input)?;
  check_analysis_consistency1(random, a, true, input)
}

#[allow(clippy::too_many_arguments)]
pub fn assert_analyzes_to2<A, R>(
  random: &mut R,
  a: &A,
  input: &str,
  output: &[&str],
  start_offsets: Option<&[i32]>,
  end_offsets: Option<&[i32]>,
  types: Option<&[&str]>,
  pos_increments: Option<&[i32]>,
  pos_lengths: Option<&[i32]>,
) -> Result<()>
where
  A: Analyzer,
  R: Rng + ?Sized,
{
  assert_analyzes_to3(
    random,
    a,
    input,
    output,
    start_offsets,
    end_offsets,
    types,
    pos_increments,
    pos_lengths,
    None,
  )
}

#[allow(clippy::too_many_arguments)]
pub fn assert_analyzes_to3<A, R>(
  random: &mut R,
  a: &A,
  input: &str,
  output: &[&str],
  start_offsets: Option<&[i32]>,
  end_offsets: Option<&[i32]>,
  types: Option<&[&str]>,
  pos_increments: Option<&[i32]>,
  pos_lengths: Option<&[i32]>,
  boost: Option<&[f32]>,
) -> Result<()>
where
  A: Analyzer,
  R: Rng + ?Sized,
{
  with_analyzer_token_stream(a, input, |ts| {
    let len = input.encode_utf16().count();
    assert_token_stream_contents8(
      ts,
      output,
      start_offsets,
      end_offsets,
      types,
      pos_increments,
      pos_lengths,
      Some(len as i32),
      boost,
    )
  })?;
  check_reset_exception(a, input)?;
  check_analysis_consistency1(random, a, true, input)
}

#[allow(unused)]
#[allow(clippy::too_many_arguments)]
pub fn assert_analyzes_to4<A, R>(
  random: &mut R,
  a: &A,
  input: &str,
  output: &[&str],
  start_offsets: Option<&[i32]>,
  end_offsets: Option<&[i32]>,
  types: Option<&[&str]>,
  pos_increments: Option<&[i32]>,
  pos_lengths: Option<&[i32]>,
  graph_offsets_are_correct: bool,
) -> Result<()>
where
  A: Analyzer,
  R: Rng + ?Sized,
{
  with_analyzer_token_stream(a, input, |ts| {
    let len = input.encode_utf16().count();
    assert_token_stream_contents6(
      ts,
      output,
      start_offsets,
      end_offsets,
      types,
      pos_increments,
      pos_lengths,
      Some(len as i32),
      graph_offsets_are_correct,
    )
  })?;
  check_reset_exception(a, input)?;
  check_analysis_consistency2(random, a, true, input, graph_offsets_are_correct)
}

#[allow(unused)]
#[allow(clippy::too_many_arguments)]
pub fn assert_analyzes_to5<A, R>(
  random: &mut R,
  a: &A,
  input: &str,
  output: &[&str],
  start_offsets: Option<&[i32]>,
  end_offsets: Option<&[i32]>,
  types: Option<&[&str]>,
  pos_increments: Option<&[i32]>,
  pos_lengths: Option<&[i32]>,
  graph_offsets_are_correct: bool,
  payloads: Option<&[Option<Vec<u8>>]>,
) -> Result<()>
where
  A: Analyzer,
  R: Rng + ?Sized,
{
  with_analyzer_token_stream(a, input, |ts| {
    let len = input.encode_utf16().count();
    assert_token_stream_contents4(
      ts,
      output,
      start_offsets,
      end_offsets,
      types,
      pos_increments,
      pos_lengths,
      Some(len as i32),
      None,
      None,
      graph_offsets_are_correct,
      payloads,
    )
  })?;
  check_reset_exception(a, input)?;
  check_analysis_consistency2(random, a, true, input, graph_offsets_are_correct)
}

pub fn assert_analyzes_to6<A, R>(random: &mut R, a: &A, input: &str, output: &[&str]) -> Result<()>
where
  A: Analyzer,
  R: Rng + ?Sized,
{
  assert_analyzes_to2(random, a, input, output, None, None, None, None, None)
}

pub fn assert_analyzes_to7<A, R>(
  random: &mut R,
  a: &A,
  input: &str,
  output: &[&str],
  types: Option<&[&str]>,
) -> Result<()>
where
  A: Analyzer,
  R: Rng + ?Sized,
{
  assert_analyzes_to2(random, a, input, output, None, None, types, None, None)
}

pub fn assert_analyzes_to8<A, R>(
  random: &mut R,
  a: &A,
  input: &str,
  output: &[&str],
  pos_increments: Option<&[i32]>,
) -> Result<()>
where
  A: Analyzer,
  R: Rng + ?Sized,
{
  assert_analyzes_to2(
    random,
    a,
    input,
    output,
    None,
    None,
    None,
    pos_increments,
    None,
  )
}

#[allow(unused)]
pub fn assert_analyzes_to_positions1<A, R>(
  random: &mut R,
  a: &A,
  input: &str,
  output: &[&str],
  pos_increments: Option<&[i32]>,
  pos_lengths: Option<&[i32]>,
) -> Result<()>
where
  A: Analyzer,
  R: Rng + ?Sized,
{
  assert_analyzes_to2(
    random,
    a,
    input,
    output,
    None,
    None,
    None,
    pos_increments,
    pos_lengths,
  )
}

#[allow(unused)]
pub fn assert_analyzes_to_positions2<A, R>(
  random: &mut R,
  a: &A,
  input: &str,
  output: &[&str],
  types: Option<&[&str]>,
  pos_increments: Option<&[i32]>,
  pos_lengths: Option<&[i32]>,
) -> Result<()>
where
  A: Analyzer,
  R: Rng + ?Sized,
{
  assert_analyzes_to2(
    random,
    a,
    input,
    output,
    None,
    None,
    types,
    pos_increments,
    pos_lengths,
  )
}

pub fn assert_analyzes_to9<A, R>(
  random: &mut R,
  a: &A,
  input: &str,
  output: &[&str],
  start_offsets: Option<&[i32]>,
  end_offsets: Option<&[i32]>,
) -> Result<()>
where
  A: Analyzer,
  R: Rng + ?Sized,
{
  assert_analyzes_to2(
    random,
    a,
    input,
    output,
    start_offsets,
    end_offsets,
    None,
    None,
    None,
  )
}

#[allow(unused)]
pub fn assert_analyzes_to10<A, R>(
  random: &mut R,
  a: &A,
  input: &str,
  output: &[&str],
  start_offsets: Option<&[i32]>,
  end_offsets: Option<&[i32]>,
  pos_increments: Option<&[i32]>,
) -> Result<()>
where
  A: Analyzer,
  R: Rng + ?Sized,
{
  assert_analyzes_to2(
    random,
    a,
    input,
    output,
    start_offsets,
    end_offsets,
    None,
    pos_increments,
    None,
  )
}

fn check_reset_exception<A>(a: &A, input: &str) -> Result<()>
where
  A: Analyzer,
{
  let field = "bogus";
  {
    let mut ts = a.token_stream(field, ReaderEnum::from(input))?;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ts.increment_token()));
    let finally_result =
      std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        ts.reset()?;
        while ts.increment_token()? {}
        ts.end()?;
        ts.close()
      }));
    let result = IOUtils::finally_caught_result(result, finally_result);
    match result {
      Err(error) if error.is_illegal_state_error() => {},
      Err(error) => unreachable!("got wrong error when reset() not called: {error}"),
      Ok(true) => {
        unreachable!("didn't get expected error when reset() not called")
      },
      Ok(false) => {},
    }
  }
  // check for a missing close()
  {
    let mut ts = a.token_stream(field, ReaderEnum::from(input))?;
    ts.reset()?;
    while ts.increment_token()? {}
    ts.end()?;
  }
  let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
    let _ts = a.token_stream(field, ReaderEnum::from(input))?;
    unreachable!("didn't get expected error when close() not called")
  }));
  let finally_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
    a.stored_value()
      .reuse_strategy()
      .get_reusable_components(field)?
      .ok_or_else(|| LuceneError::illegal_state("reusable components are missing"))?
      .get_token_stream()
      .close()
  }));
  let result = IOUtils::finally_caught_result(result, finally_result);
  match result {
    Err(error) if error.is_illegal_state_error() => {},
    Err(error) => unreachable!("didn't get expected error: {error}"),
    Ok(()) => unreachable!("didn't get expected error when close() not called"),
  }
  Ok(())
}
pub fn check_one_term<A, R>(random: &mut R, a: &A, input: &str, expect: &str) -> Result<()>
where
  A: Analyzer,
  R: Rng + ?Sized,
{
  assert_analyzes_to6(random, a, input, &[expect])?;
  Ok(())
}
pub fn check_analysis_consistency1<R, A>(
  random: &mut R,
  a: &A,
  use_char_filter: bool,
  text: &str,
) -> Result<()>
where
  R: Rng + ?Sized,
  A: Analyzer,
{
  check_analysis_consistency2(random, a, use_char_filter, text, true)
}

pub fn check_analysis_consistency2<R, A>(
  random: &mut R,
  a: &A,
  use_char_filter: bool,
  text: &str,
  graph_offsets_are_correct: bool,
) -> Result<()>
where
  R: Rng + ?Sized,
  A: Analyzer,
{
  check_analysis_consistenc3(
    random,
    a,
    use_char_filter,
    text,
    graph_offsets_are_correct,
    None,
  )
}
pub fn check_analysis_consistenc3<R, A>(
  _random: &mut R,
  _a: &A,
  _use_char_filter: bool,
  _text: &str,
  _graph_offsets_are_correct: bool,
  _field: Option<&mut Field>,
) -> Result<()>
where
  R: Rng + ?Sized,
  A: Analyzer,
{
  // TODO IMPORTANT 未实现
  Ok(())
}
