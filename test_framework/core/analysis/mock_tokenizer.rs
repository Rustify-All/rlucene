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
use crate::core::analysis::token_stream::TokenStream;
use crate::core::analysis::tokenizer::{Tokenizer, TokenizerBase};
use crate::core::util::attribute_source::{AttributeSource, Attributes};
use crate::core::util::automation::character_run_automaton::CharacterRunAutomaton;
use crate::core::util::automation::operations::Operations;
use crate::core::util::automation::reg_exp::RegExp;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use rand::{Rng, RngExt};
use std::sync::LazyLock;

/// Acts Similar to WhitespaceTokenizer
pub static WHITESPACE: LazyLock<CharacterRunAutomaton> = LazyLock::new(|| {
  CharacterRunAutomaton::new(
    Operations::determinize(
      &RegExp::from_string("[^ \t\r\n]+")
        .expect("")
        .to_automaton()
        .expect(""),
      Operations::DEFAULT_DETERMINIZE_WORK_LIMIT,
    )
    .expect("")
    .into_owned(),
  )
  .expect("")
});
/// Acts Similar to KeywordTokenizer.
pub static KEYWORD: LazyLock<CharacterRunAutomaton> = LazyLock::new(|| {
  CharacterRunAutomaton::new(
    Operations::determinize(
      &RegExp::from_string(".*")
        .expect("")
        .to_automaton()
        .expect(""),
      Operations::DEFAULT_DETERMINIZE_WORK_LIMIT,
    )
    .expect("")
    .into_owned(),
  )
  .expect("")
});

/// Acts like LetterTokenizer.
// the ugly regex below is incomplete Unicode 5.2 [:Letter:]
pub static SIMPLE: LazyLock<CharacterRunAutomaton> = LazyLock::new(|| {
  CharacterRunAutomaton::new(
    Operations::determinize(
      &RegExp::from_string("[A-Za-zªµºÀ-ÖØ-öø-ˁ一-鿌]+")
        .expect("")
        .to_automaton()
        .expect(""),
      Operations::DEFAULT_DETERMINIZE_WORK_LIMIT,
    )
    .expect("")
    .into_owned(),
  )
  .expect("")
});
/// Limit the default token length to a size that doesn't cause random analyzer failures on
/// unpredictable data like the enwiki data set.
///
/// This value defaults to `CharTokenizer.DEFAULT_MAX_WORD_LEN` (255).
///
/// See <https://issues.apache.org/jira/browse/LUCENE-10541>.
pub const DEFAULT_MAX_TOKEN_LENGTH: i32 = 255;
/// Tokenizer for testing.
///
/// This tokenizer is a replacement for [`WHITESPACE`], [`SIMPLE`], and [`KEYWORD`]
/// tokenizers. If you are writing a component such as a TokenFilter, it's a great idea to test it
/// wrapping this tokenizer instead for extra checks. This tokenizer has the following behavior:
///
/// - An internal state-machine is used for checking consumer consistency. These checks can be
///   disabled with [`MockTokenizer::set_enable_checks`].
/// - For convenience, optionally lowercases terms that it outputs.
pub struct MockTokenizer<R> {
  tokenizer_base: TokenizerBase,
  run_automaton: CharacterRunAutomaton,
  lower_case: bool,
  max_token_length: i32,
  state: i32,
  off: i32,

  // buffered state (previous codepoint and offset). we replay this once we
  // hit a reject state in case it's permissible as the start of a new term.
  // -1 indicates empty buffer
  buffered_code_point: i32,
  buffered_off: i32,

  stream_state: State,
  last_offset: i32, // only for checks
  enable_checks: bool,
  random: R,
}
impl<R> MockTokenizer<R>
where
  R: Rng,
{
  pub fn with_automaton(
    random: R,
    run_automaton: CharacterRunAutomaton,
    lower_case: bool,
    max_token_length: i32,
  ) -> Self {
    let attr = Attributes::default();
    Self {
      tokenizer_base: TokenizerBase::new(attr),
      run_automaton,
      lower_case,
      max_token_length,
      state: 0,
      off: 0,
      buffered_code_point: -1,
      buffered_off: -1,
      stream_state: State::Close,
      last_offset: 0,
      enable_checks: true,
      random,
    }
  }
  pub fn with_default_max_token_length(
    random: R,
    run_automaton: CharacterRunAutomaton,
    lower_case: bool,
  ) -> Self {
    Self::with_automaton(random, run_automaton, lower_case, DEFAULT_MAX_TOKEN_LENGTH)
  }

  /// Calls [`MockTokenizer::with_default_max_token_length`] with `WHITESPACE` and `true`.
  pub fn new(random: R) -> Self {
    Self::with_default_max_token_length(random, WHITESPACE.clone(), true)
  }

  pub fn with_attribute_factory_default_max_token_length(
    random: R,
    run_automaton: CharacterRunAutomaton,
    lower_case: bool,
  ) -> Self {
    Self::with_automaton(random, run_automaton, lower_case, DEFAULT_MAX_TOKEN_LENGTH)
  }
  /// Toggle consumer workflow checking: if your test consumes tokenstreams normally you should leave this enabled.
  pub fn set_enable_checks(&mut self, enable_checks: bool) {
    self.enable_checks = enable_checks;
  }

  fn fail(&self, message: impl Into<String>) -> Result<()> {
    if self.enable_checks {
      Err(LuceneError::illegal_state(format!(
        "TokenStream contract violation: {}",
        message.into()
      )))
    } else {
      Ok(())
    }
  }

  fn fail_always(&self, message: impl Into<String>) -> Result<()> {
    Err(LuceneError::illegal_state(message.into()))
  }

  fn read_char(&mut self) -> Result<i32> {
    match self.random.random_range(0..10) {
      0 => {
        // read(char[])
        let mut c = vec!['\0'; 1];
        let ret = self.tokenizer_base.input.read_buf(&mut c)?;
        Ok(if ret < 0 { ret } else { c[0] as i32 })
      },
      1 => {
        // read(char[], int, int)
        let mut c = vec!['\0'; 2];
        let ret = self.tokenizer_base.input.read_range(&mut c, 1, 1)?;
        Ok(if ret < 0 { ret } else { c[1] as i32 })
      },
      2 => {
        // read(CharBuffer)
        let mut c = vec!['\0'; 1];
        let ret = self.tokenizer_base.input.read_buf(&mut c)?;
        Ok(if ret < 0 { ret } else { c[0] as i32 })
      },
      _ => {
        // read()
        self.tokenizer_base.input.read()
      },
    }
  }

  fn read_code_point(&mut self) -> Result<i32> {
    let ch = self.read_char()?;
    if ch < 0 {
      return Ok(ch);
    }
    self.off += 1;
    Ok(ch)
  }

  fn is_token_char(&mut self, c: i32) -> bool {
    if self.state < 0 {
      self.state = 0;
    }
    self.state = self.run_automaton.base.step(self.state, c);
    self.state >= 0
  }

  fn normalize(&self, c: i32) -> i32 {
    if self.lower_case {
      char::from_u32(c as u32)
        .and_then(|ch| ch.to_lowercase().next())
        .map(|ch| ch as i32)
        .unwrap_or(c)
    } else {
      c
    }
  }
}

impl<R> crate::core::util::close::Closeable for MockTokenizer<R>
where
  R: Rng,
{
  fn close(&mut self) -> Result<()> {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      crate::core::util::close::Closeable::close(&mut self.tokenizer_base)?;
      if self.stream_state != State::End && self.stream_state != State::Close {
        self.fail(format!(
          "close() called in wrong state: {:?}",
          self.stream_state
        ))?;
      }
      Ok(())
    }));
    self.stream_state = State::Close;
    match result {
      Ok(result) => result,
      Err(payload) => std::panic::resume_unwind(payload),
    }
  }
}

impl<R> TokenStream for MockTokenizer<R>
where
  R: Rng,
{
  fn increment_token(&mut self) -> Result<bool> {
    if self.stream_state != State::Reset && self.stream_state != State::Increment {
      self.fail(format!(
        "increment_token() called while in wrong state: {:?}",
        self.stream_state
      ))?;
    }

    self
      .tokenizer_base
      .token_stream_base
      .att
      .clear_attributes()?;
    loop {
      let start_offset;
      let mut cp;
      if self.buffered_code_point >= 0 {
        cp = self.buffered_code_point;
        start_offset = self.buffered_off;
        self.buffered_code_point = -1;
      } else {
        start_offset = self.off;
        cp = self.read_code_point()?;
      }

      if cp < 0 {
        break;
      } else if self.is_token_char(cp) {
        let mut end_offset;
        loop {
          let normalized = self.normalize(cp);
          let ch = char::from_u32(normalized as u32).ok_or_else(|| {
            LuceneError::illegal_state(format!("invalid code point: {normalized}"))
          })?;
          self.tokenizer_base.token_stream_base.att.append_char(ch)?;

          end_offset = self.off;
          if self.tokenizer_base.token_stream_base.att.length()? as i32 >= self.max_token_length {
            break;
          }

          cp = self.read_code_point()?;
          if cp < 0 || !self.is_token_char(cp) {
            break;
          }
        }

        if (self.tokenizer_base.token_stream_base.att.length()? as i32) < self.max_token_length {
          self.buffered_code_point = cp;
          self.buffered_off = end_offset;
        } else {
          self.buffered_code_point = -1;
        }

        let corrected_start_offset = self.correct_offset(start_offset);
        let corrected_end_offset = self.correct_offset(end_offset);
        if corrected_start_offset < 0 {
          self.fail_always(format!(
            "invalid start offset: {corrected_start_offset}, before correction: {start_offset}"
          ))?;
        }
        if corrected_end_offset < 0 {
          self.fail_always(format!(
            "invalid end offset: {corrected_end_offset}, before correction: {end_offset}"
          ))?;
        }
        if corrected_start_offset < self.last_offset {
          self.fail_always(format!(
            "start offset went backwards: {corrected_start_offset}, before correction: {start_offset}, lastOffset: {}",
            self.last_offset
          ))?;
        }
        self.last_offset = corrected_start_offset;
        if corrected_end_offset < corrected_start_offset {
          self.fail_always(format!(
            "end offset: {corrected_end_offset} is before start offset: {corrected_start_offset}"
          ))?;
        }

        self
          .tokenizer_base
          .token_stream_base
          .att
          .set_offset(corrected_start_offset, corrected_end_offset)?;

        if self.state == -1 || self.run_automaton.base.is_accept(self.state)? {
          self.stream_state = State::Increment;
          return Ok(true);
        }
      }
    }

    self.stream_state = State::IncrementFalse;
    Ok(false)
  }

  fn end(&mut self) -> Result<()> {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      self.tokenizer_base.end()?;
      let final_offset = self.correct_offset(self.off);
      self
        .tokenizer_base
        .token_stream_base
        .att
        .set_offset(final_offset, final_offset)?;
      if self.stream_state != State::IncrementFalse {
        self.fail(format!(
          "end() called in wrong state={:?}!",
          self.stream_state
        ))?;
      }
      Ok(())
    }));
    self.stream_state = State::End;
    match result {
      Ok(result) => result,
      Err(payload) => std::panic::resume_unwind(payload),
    }
  }

  fn reset(&mut self) -> Result<()> {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      self.tokenizer_base.reset()?;
      self.state = 0;
      self.last_offset = 0;
      self.off = 0;
      self.buffered_code_point = -1;
      if self.stream_state == State::Reset {
        self.fail("double reset()")?;
      }
      Ok(())
    }));
    self.stream_state = State::Reset;
    match result {
      Ok(result) => result,
      Err(payload) => std::panic::resume_unwind(payload),
    }
  }

  fn get_attribute_source(&self) -> &Attributes {
    self.tokenizer_base.get_attribute_source()
  }

  fn get_attribute_source_mut(&mut self) -> &mut Attributes {
    self.tokenizer_base.get_attribute_source_mut()
  }

  fn set_reader(&mut self, input: ReaderEnum) -> Result<()> {
    self.tokenizer_base.set_reader(input)
  }

  fn set_reader_test_point(&mut self) -> Result<()> {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      if self.stream_state != State::Close {
        self.fail(format!(
          "set_reader() called in wrong state: {:?}",
          self.stream_state
        ))?;
      }
      Ok(())
    }));
    self.stream_state = State::SetReader;
    match result {
      Ok(result) => result,
      Err(payload) => std::panic::resume_unwind(payload),
    }
  }
}

impl<R> Tokenizer for MockTokenizer<R>
where
  R: Rng,
{
  fn get_tokenizer_base_mut(&mut self) -> &mut TokenizerBase {
    &mut self.tokenizer_base
  }

  fn get_tokenizer_base(&self) -> &TokenizerBase {
    &self.tokenizer_base
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
  /// consumer set a reader input either via ctor or via reset(Reader)
  SetReader,

  /// consumer has called reset()
  Reset,

  /// consumer is consuming, has called incrementToken() == true
  Increment,

  /// consumer has called incrementToken() which returned false
  IncrementFalse,

  /// consumer has called end() to perform end of stream operations
  End,

  /// consumer has called close() to release any resources
  Close,
}
