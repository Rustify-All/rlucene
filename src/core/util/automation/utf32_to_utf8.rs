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
use std::borrow::Cow;

use crate::core::util::automation::automaton::{Automaton, Builder};
use crate::core::util::automation::transition::Transition;
use crate::core::util::automation::transition_accessor::TransitionAccessor;
use crate::core::util::error::lucene_error::Result;

/// Converts UTF-32 automata to the equivalent UTF-8 representation.
pub struct UTF32ToUTF8 {
    start_utf8: UTF8Sequence,
    end_utf8: UTF8Sequence,
    tmp_utf8a: UTF8Sequence,
    tmp_utf8b: UTF8Sequence,
    utf8: Builder,
}
impl Default for UTF32ToUTF8 {
    fn default() -> Self {
        UTF32ToUTF8::new()
    }
}

impl UTF32ToUTF8 {
    const START_CODES: [i32; 4] = [0, 128, 2048, 65536];
    const END_CODES: [i32; 4] = [127, 2047, 65535, 0x10FFFF];
    pub fn new() -> Self {
        UTF32ToUTF8 {
            start_utf8: UTF8Sequence::new(),
            end_utf8: UTF8Sequence::new(),
            tmp_utf8a: UTF8Sequence::new(),
            tmp_utf8b: UTF8Sequence::new(),
            utf8: Builder::new(),
        }
    }
    /// Builds necessary UTF-8 edges between start and end code points.
    pub fn convert_one_edge(
        &mut self,
        start: i32,
        end: i32,
        start_code_point: i32,
        end_code_point: i32,
    ) {
        self.start_utf8.set(start_code_point);
        self.end_utf8.set(end_code_point);
        self.build(start, end, 0);
    }
    fn build(&mut self, start: i32, end: i32, upto: usize) {
        if self.start_utf8.byte_at(upto) == self.end_utf8.byte_at(upto) {
            // Degen case: lead with the same byte
            if upto == (self.start_utf8.len - 1) && upto == (self.end_utf8.len - 1) {
                // Super degen: just single edge, one UTF8 byte
                self.utf8.add_transition(
                    start,
                    end,
                    self.start_utf8.byte_at(upto),
                    self.end_utf8.byte_at(upto),
                );
            } else {
                debug_assert!(self.start_utf8.len > upto + 1);
                debug_assert!(self.end_utf8.len > upto + 1);

                let n = self.utf8.create_state();

                // Single value leading edge
                self.utf8
                    .add_transition_label(start, n, self.start_utf8.byte_at(upto));

                // Recurse for the rest
                self.build(n, end, upto + 1);
            }
        } else if self.start_utf8.len == self.end_utf8.len {
            if upto == (self.start_utf8.len - 1) {
                self.utf8.add_transition(
                    start,
                    end,
                    self.start_utf8.byte_at(upto),
                    self.end_utf8.byte_at(upto),
                );
            } else {
                self.start(start, end, upto, false);

                if self.end_utf8.byte_at(upto) - self.start_utf8.byte_at(upto) > 1 {
                    self.all(
                        start,
                        end,
                        self.start_utf8.byte_at(upto) + 1,
                        self.end_utf8.byte_at(upto) - 1,
                        self.start_utf8.len - upto - 1,
                    );
                }

                self.end(start, end, upto, false);
            }
        } else {
            // start
            self.start(start, end, upto, true);

            // possibly middle, spanning multiple num bytes
            let mut byte_count = 1 + self.start_utf8.len - upto;
            let limit = self.end_utf8.len - upto;

            while byte_count < limit {
                self.tmp_utf8a
                    .set_first_byte(Self::START_CODES[byte_count - 1]);
                self.tmp_utf8b
                    .set_first_byte(Self::END_CODES[byte_count - 1]);

                self.all(
                    start,
                    end,
                    self.tmp_utf8a.byte_at(0),
                    self.tmp_utf8b.byte_at(0),
                    self.tmp_utf8a.len - 1,
                );

                byte_count += 1;
            }

            // end
            self.end(start, end, upto, true);
        }
    }
    fn start(&mut self, start: i32, end: i32, upto: usize, do_all: bool) {
        if upto == (self.start_utf8.len - 1) {
            // Done recursing
            let b = self.start_utf8.byte_at(upto);
            let mask = MASKS[self.start_utf8.num_bits(upto) as usize] as i32;
            self.utf8.add_transition(start, end, b, b | mask); // type=start
        } else {
            let n = self.utf8.create_state();
            self.utf8
                .add_transition_label(start, n, self.start_utf8.byte_at(upto));
            self.start(n, end, upto + 1, true);

            let start_byte = self.start_utf8.byte_at(upto);
            let end_code = start_byte | (MASKS[self.start_utf8.num_bits(upto) as usize] as i32);
            if do_all && start_byte != end_code {
                self.all(
                    start,
                    end,
                    start_byte + 1,
                    end_code,
                    self.start_utf8.len - upto - 1,
                );
            }
        }
    }
    fn end(&mut self, start: i32, end: i32, upto: usize, do_all: bool) {
        if upto == (self.end_utf8.len - 1) {
            // Done recursing
            let b = self.end_utf8.byte_at(upto);
            let mask = MASKS[self.end_utf8.num_bits(upto) as usize] as i32;
            self.utf8.add_transition(start, end, b & !mask, b);
        } else {
            let start_code: i32;

            // GH-ISSUE#12472: UTF-8 special case for different start bytes for lengths
            // 2/3/4
            if self.end_utf8.len == 2 && upto == 0 {
                // The first length=2 UTF-8 Unicode character is C2 80,
                // so we must special case 0xC2 as the 1st byte.
                start_code = 0xC2;
            } else if self.end_utf8.len == 3 && upto == 1 && self.end_utf8.byte_at(0) == 0xE0 {
                // The first length=3 UTF-8 Unicode character is E0 A0 80,
                // so we must special case 0xA0 as the 2nd byte when E0 was the first byte.
                start_code = 0xA0;
            } else if self.end_utf8.len == 4 && upto == 1 && self.end_utf8.byte_at(0) == 0xF0 {
                // The first length=4 UTF-8 Unicode character is F0 90 80 80,
                // so we must special case 0x90 as the 2nd byte when F0 was the first byte.
                start_code = 0x90;
            } else {
                let b = self.end_utf8.byte_at(upto);
                let mask = MASKS[self.end_utf8.num_bits(upto) as usize] as i32;
                start_code = b & !mask;
            }

            let end_byte = self.end_utf8.byte_at(upto);

            if do_all && end_byte != start_code {
                self.all(
                    start,
                    end,
                    start_code,
                    end_byte - 1,
                    self.end_utf8.len - upto - 1,
                );
            }

            let n = self.utf8.create_state();
            self.utf8.add_transition_label(start, n, end_byte);
            self.end(n, end, upto + 1, true);
        }
    }
    fn all(&mut self, start: i32, end: i32, start_code: i32, end_code: i32, mut left: usize) {
        if left == 0 {
            self.utf8.add_transition(start, end, start_code, end_code);
        } else {
            let mut last_n = self.utf8.create_state();
            self.utf8
                .add_transition(start, last_n, start_code, end_code);

            while left > 1 {
                let n = self.utf8.create_state();
                self.utf8.add_transition(last_n, n, 128, 191); // continuation byte range
                left -= 1;
                last_n = n;
            }

            self.utf8.add_transition(last_n, end, 128, 191);
        }
    }
    /// Converts an incoming UTF-32 automaton to an equivalent UTF-8 one.
    /// The incoming automaton need not be deterministic.
    /// Note that the returned automaton will not generally be deterministic,
    /// so you must determinize it if that's required.
    pub fn convert<'a>(&mut self, utf32: &'a Automaton) -> Result<Cow<'a, Automaton>> {
        if utf32.get_num_states() == 0 {
            return Ok(Cow::Borrowed(utf32));
        }

        let mut map = vec![-1; utf32.get_num_states() as usize];
        let mut pending = Vec::new();

        let utf32_state = 0;
        pending.push(utf32_state);

        self.utf8 = Builder::new();

        let utf8_state = self.utf8.create_state();
        self.utf8
            .set_accept(utf8_state, utf32.is_accept(utf32_state));
        map[utf32_state as usize] = utf8_state;

        let mut scratch = Transition::default();

        while let Some(current_utf32) = pending.pop() {
            let current_utf8 = map[current_utf32 as usize];
            debug_assert!(current_utf8 != -1);

            let num_transitions = utf32.get_num_transitions_with_state(current_utf32);
            utf32.init_transition(current_utf32, &mut scratch);

            for _ in 0..num_transitions {
                utf32.get_next_transition(&mut scratch);

                let dest_utf32 = scratch.dest;
                let mut dest_utf8 = map[dest_utf32 as usize];
                if dest_utf8 == -1 {
                    dest_utf8 = self.utf8.create_state();
                    self.utf8.set_accept(dest_utf8, utf32.is_accept(dest_utf32));
                    map[dest_utf32 as usize] = dest_utf8;
                    pending.push(dest_utf32);
                }

                self.convert_one_edge(current_utf8, dest_utf8, scratch.min, scratch.max);
            }
        }
        Ok(Cow::Owned(self.utf8.finish()?))
    }
}

/// Represents one of the N UTF-8 bytes that (in sequence)
/// define a code point. `value` is the byte value; `bits` is
/// how many bits are "used" by UTF-8 at that byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UTF8Byte {
    pub value: u8,
    pub bits: u8,
}
/// Holds a single code point, as a sequence of 1–4 UTF-8 bytes.
/// TODO: maybe move to UnicodeUtil?
#[derive(Debug, Clone)]
pub struct UTF8Sequence {
    pub bytes: [UTF8Byte; 4],
    pub len: usize,
}

impl UTF8Sequence {
    pub fn new() -> Self {
        UTF8Sequence {
            bytes: [UTF8Byte { value: 0, bits: 0 }; 4],
            len: 0,
        }
    }
    pub fn byte_at(&self, idx: usize) -> i32 {
        self.bytes[idx].value as i32
    }
    pub fn num_bits(&self, idx: usize) -> i32 {
        self.bytes[idx].bits as i32
    }
    fn set(&mut self, code: i32) {
        if code < 0x80 {
            // 0xxxxxxx
            self.bytes[0].value = code as u8;
            self.bytes[0].bits = 7;
            self.len = 1;
        } else if code < 0x800 {
            // 110yyyxx 10xxxxxx
            self.bytes[0].value = ((0b110 << 5) | (code >> 6)) as u8;
            self.bytes[0].bits = 5;
            self.set_rest(code, 1);
            self.len = 2;
        } else if code < 0x10000 {
            // 1110yyyy 10yyyyxx 10xxxxxx
            self.bytes[0].value = ((0b1110 << 4) | (code >> 12)) as u8;
            self.bytes[0].bits = 4;
            self.set_rest(code, 2);
            self.len = 3;
        } else {
            // 11110zzz 10zzyyyy 10yyyyxx 10xxxxxx
            self.bytes[0].value = ((0b11110 << 3) | (code >> 18)) as u8;
            self.bytes[0].bits = 3;
            self.set_rest(code, 3);
            self.len = 4;
        }
    }
    /// Only set first byte value for tmp UTF-8.
    pub fn set_first_byte(&mut self, code: i32) {
        if code < 0x80 {
            // 0xxxxxxx
            self.bytes[0].value = code as u8;
            self.len = 1;
        } else if code < 0x800 {
            // 110yyyxx
            self.bytes[0].value = ((0b110 << 5) | (code >> 6)) as u8;
            self.len = 2;
        } else if code < 0x10000 {
            // 1110yyyy
            self.bytes[0].value = ((0b1110 << 4) | (code >> 12)) as u8;
            self.len = 3;
        } else {
            // 11110zzz
            self.bytes[0].value = ((0b11110 << 3) | (code >> 18)) as u8;
            self.len = 4;
        }
    }

    fn set_rest(&mut self, mut code: i32, num_bytes: usize) {
        for i in 0..num_bytes {
            let idx = num_bytes - i;
            self.bytes[idx].value = (0b1000_0000 | (code & MASKS[6] as i32)) as u8;
            self.bytes[idx].bits = 6;
            code >>= 6;
        }
    }
}

use once_cell::sync::Lazy;

static MASKS: Lazy<[u8; 8]> = Lazy::new(|| {
    let mut masks = [0u8; 8];
    for i in 0..7 {
        masks[i + 1] = ((2 << i) - 1) as u8;
    }
    masks
});

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::string::FromUtf16Error;

    use rand::Rng;

    use crate::core::util::automation::automata::Automata;
    use crate::core::util::automation::automaton::Automaton;
    use crate::core::util::automation::byte_run_automaton::ByteRunAutomaton;
    use crate::core::util::automation::byte_runnable::ByteRunnable;
    use crate::core::util::automation::character_run_automaton::CharacterRunAutomaton;
    use crate::core::util::automation::operations::Operations;
    use crate::core::util::automation::operations::tests::TestOperations;
    use crate::core::util::automation::reg_exp::RegExp;
    use crate::core::util::automation::utf32_to_utf8::UTF32ToUTF8;
    use crate::core::util::error::lucene_error::{LuceneError, Result};
    use crate::core::util::fst_impl::util::Util;
    use crate::core::util::ints_ref_builder::IntsRefBuilder;
    use crate::core::util::unicode_util::UnicodeUtil;
    use crate::test::util::automaton::automaton_test_util::{
        AutomatonTestUtil, RandomAcceptedStrings,
    };
    use crate::test::util::lucene_test_case::lucene_test_case_util::{
        TestRng, at_least, at_least_usize, new_bytes_ref_from_string, random,
    };
    use crate::test::util::test_util::TestUtil;
    #[allow(dead_code)] // for quick search
    struct TestUTF32ToUTF8;
    const MAX_UNICODE: i32 = 0x10FFFF;
    fn matches(a: &ByteRunAutomaton, code: i32) -> Result<bool> {
        let ch = std::char::from_u32(code as u32)
            .ok_or_else(|| LuceneError::illegal_argument("Invalid Unicode code point"))?;
        let len = UnicodeUtil::max_utf8_length(code)?;
        let mut buf = vec![0; len];
        let _ = ch.encode_utf8(&mut buf);
        a.run(buf.as_slice(), 0, len)
    }
    fn test_one<R: Rng + ?Sized>(
        random: &mut R,
        a: &ByteRunAutomaton,
        start_code: i32,
        end_code: i32,
        iters: usize,
    ) -> Result<()> {
        let sur_start = UnicodeUtil::UNI_SUR_HIGH_START;
        let sur_end = UnicodeUtil::UNI_SUR_LOW_END;

        let (non_surrogate_count, ov_sur_start) = if end_code < sur_start || start_code > sur_end {
            (end_code - start_code + 1, false)
        } else if is_surrogate(start_code) {
            (
                end_code - start_code + 1 - (sur_end - start_code + 1),
                false,
            )
        } else if is_surrogate(end_code) {
            (end_code - start_code + 1 - (end_code - sur_start + 1), true)
        } else {
            (end_code - start_code + 1 - (sur_end - sur_start + 1), true)
        };

        assert!(non_surrogate_count > 0);

        for _ in 0..iters {
            let mut code = start_code + random.random_range(0..non_surrogate_count);
            if is_surrogate(code) {
                if ov_sur_start {
                    code = sur_end + 1 + (code - sur_start);
                } else {
                    code = sur_end + 1 + (code - start_code);
                }
            }

            assert!(
                code >= start_code && code <= end_code,
                "code={} start={} end={}",
                code,
                start_code,
                end_code
            );
            assert!(!is_surrogate(code));

            assert!(
                matches(a, code)?,
                "DFA for range {}-{} failed to match code={}",
                start_code,
                end_code,
                code
            );
        }

        // check out-of-range values are NOT accepted
        let invalid_range = MAX_UNICODE - (end_code - start_code + 1);
        if invalid_range > 0 {
            for _ in 0..iters {
                let x = random.random_range(0..invalid_range);
                let code = if x >= start_code {
                    end_code + 1 + x - start_code
                } else {
                    x
                };

                if is_surrogate(code) {
                    continue;
                }

                assert!(
                    !matches(a, code)?,
                    "DFA for range {}-{} matched invalid code={}",
                    start_code,
                    end_code,
                    code
                );
            }
        }
        Ok(())
    }
    fn get_code_start<R: Rng + ?Sized>(random: &mut R) -> i32 {
        match random.random_range(0..4) {
            0 => random.random_range(0..128),
            1 => random.random_range(128..2048),
            2 => random.random_range(2048..65536),
            _ => random.random_range(65536..=MAX_UNICODE),
        }
    }
    fn is_surrogate(code: i32) -> bool {
        (UnicodeUtil::UNI_SUR_HIGH_START..=UnicodeUtil::UNI_SUR_HIGH_END).contains(&code)
            || (UnicodeUtil::UNI_SUR_LOW_START..=UnicodeUtil::UNI_SUR_LOW_END).contains(&code)
    }

    #[test]
    fn test_random_ranges() -> Result<()> {
        let mut random = random();
        let iters = at_least(&mut random, 10);
        let iters_per_dfa = at_least_usize(&mut random, 100);

        for _ in 0..iters {
            let x1 = get_code_start(&mut random);
            let x2 = get_code_start(&mut random);
            let (start_code, end_code) = if x1 < x2 { (x1, x2) } else { (x2, x1) };

            if is_surrogate(start_code) && is_surrogate(end_code) {
                continue;
            }

            let a = Automata::make_char_range(start_code, end_code)?;
            let dfa = ByteRunAutomaton::new(a)?;
            test_one(&mut random, &dfa, start_code, end_code, iters_per_dfa)?;
        }

        Ok(())
    }
    #[test]
    fn test_special_case() -> Result<()> {
        let re = RegExp::from_string(".?")?;
        let automaton = re.to_automaton()?;

        let cra = CharacterRunAutomaton::new(automaton.clone())?;
        let bra = ByteRunAutomaton::new(automaton)?;

        // make sure character dfa accepts empty string
        assert!(cra.base.is_accept(0)?);
        assert!(cra.run_str("")?);
        assert!(cra.run_chars(&[], 0, 0)?);

        // make sure byte dfa accepts empty string
        assert!(bra.is_accept(0)?);
        assert!(bra.run(&[], 0, 0)?);

        Ok(())
    }
    #[test]
    fn test_special_case2() -> Result<()> {
        let utf16: [u16; 12] = [
            0xfadc, 0xfffd, 0xb80b, 0xda5a, 0xdc68, 0xf234, 0x0056, 0xda5b, 0xdcc1, 0xfffd, 0xfffd,
            0x0775,
        ];

        let input = String::from_utf16(&utf16).map_err(|e: FromUtf16Error| {
            LuceneError::illegal_argument(format!("invalid UTF-16 input: {e}"))
        })?;

        let re = RegExp::from_string(".+\u{0775}")?;
        let mut automaton = re.to_automaton()?;
        automaton =
            Operations::determinize(&automaton, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?
                .into_owned();

        let cra = CharacterRunAutomaton::new(automaton.clone())?;
        let bra = ByteRunAutomaton::new(automaton)?;

        assert!(cra.run_str(&input)?);

        let bytes = input.as_bytes();
        assert!(bra.run(bytes, 0, bytes.len())?);

        Ok(())
    }
    #[test]
    fn test_special_case3() -> Result<()> {
        let utf16_input: [u16; 15] = [
            0x5cfd, 0xfffd, 0xb2f7, 0x0033, 0xe304, 0x51d7, 0x3692, 0xdb50, 0xdfb3, 0x0576, 0xdae2,
            0xdc62, 0x0053, 0x0449, 0x04d4,
        ];
        let input = String::from_utf16(&utf16_input)
            .map_err(|e| LuceneError::illegal_argument(format!("invalid UTF-16 input: {e}")))?;

        let utf16_regex: [u16; 11] = [
            0x0028, 0x005c, 0x9bfa, 0x0029, 0x002a, 0x0028, 0x002e, 0x0029, 0x002a, 0x005c, 0x04d4,
        ];
        let regex_str = String::from_utf16(&utf16_regex)
            .map_err(|e| LuceneError::illegal_argument(format!("invalid UTF-16 regex: {e}")))?;

        let re = RegExp::from_string(&regex_str)?;
        let mut automaton = re.to_automaton()?;
        automaton =
            Operations::determinize(&automaton, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?
                .into_owned();

        let cra = CharacterRunAutomaton::new(automaton.clone())?;
        let bra = ByteRunAutomaton::new(automaton)?;

        assert!(cra.run_str(&input)?);

        let bytes = input.as_bytes();
        assert!(bra.run(bytes, 0, bytes.len())?);

        Ok(())
    }

    #[test]
    fn test_random_regexes() -> Result<()> {
        let mut random = random();
        let num = at_least(&mut random, 50);

        for _ in 0..num {
            let s = AutomatonTestUtil::random_regexp(&mut random)?;
            let mut automaton = RegExp::from_str_with_flags(&s, RegExp::NONE)?.to_automaton()?;
            automaton =
                Operations::determinize(&automaton, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?
                    .into_owned();
            assert_automaton(&mut random, &automaton)?;
        }

        Ok(())
    }

    #[test]
    fn test_singleton() -> Result<()> {
        let mut random = random();
        let iters = at_least(&mut random, 100);

        for _ in 0..iters {
            let s = TestUtil::random_realistic_unicode_string(&mut random);
            let a = Automata::make_string(&s)?;
            let utf8 = UTF32ToUTF8::new().convert(&a)?.into_owned();

            let mut ints = IntsRefBuilder::new();
            Util::get_ints_ref(
                &new_bytes_ref_from_string::<TestRng, Vec<u8>>(&mut random, &s)?,
                &mut ints,
            );
            let mut set = HashSet::new();
            set.insert(ints.get_owner());

            let actual = TestOperations::get_finite_strings(&utf8)?;
            assert_eq!(set, actual, "Failed for input string: {:?}", s);
        }

        Ok(())
    }

    fn assert_automaton<R: Rng + ?Sized>(random: &mut R, a: &Automaton) -> Result<()> {
        let cra = CharacterRunAutomaton::new(a.clone())?;
        let bra = ByteRunAutomaton::new(a.clone())?;
        let ras = RandomAcceptedStrings::new(a)?;

        let num = at_least(random, 1000);

        for _ in 0..num {
            let string = if random.random_bool(0.5) {
                // likely not accepted
                TestUtil::random_unicode_string(random)
            } else {
                // will be accepted
                let codepoints = ras.get_random_accepted_string(random)?;
                UnicodeUtil::new_string(&codepoints, 0, codepoints.len())?
            };

            let bytes = string.as_bytes();
            let cra_result = cra.run_str(&string)?;
            let bra_result = bra.run(bytes, 0, bytes.len())?;

            assert_eq!(
                cra_result, bra_result,
                "Mismatch on input: {:?} (UTF-8: {:?})",
                string, bytes
            );
        }

        Ok(())
    }
}
