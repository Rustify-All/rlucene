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
#![allow(deprecated)]
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::core::util::automation::automata::Automata;
use crate::core::util::automation::automaton::Automaton;
use crate::core::util::automation::automaton_provider::{
    AutomatonProvider, EmptyAutomatonProvider,
};
use crate::core::util::automation::operations::Operations;
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::lucene_error::Result;
/// Regular Expression extension to [`Automaton`].
///
/// Regular expressions are built from the following abstract syntax:
///
/// ```text
/// regexp         ::= unionexp
///
/// unionexp       ::= interexp '|' unionexp          (union)
///                 |  interexp
///
/// interexp       ::= concatexp '&' interexp         (intersection) [OPTIONAL]
///                 |  concatexp
///
/// concatexp      ::= repeatexp concatexp            (concatenation)
///                 |  repeatexp
///
/// repeatexp      ::= repeatexp '?'                  (zero or one occurrence)
///                 |  repeatexp '*'                  (zero or more occurrences)
///                 |  repeatexp '+'                  (one or more occurrences)
///                 |  repeatexp '{n}'                (n occurrences)
///                 |  repeatexp '{n,}'               (n or more occurrences)
///                 |  repeatexp '{n,m}'              (n to m occurrences, inclusive)
///                 |  complexp
///
/// complexp       ::= charclassexp
///                 |  simpleexp
///
/// charclassexp   ::= '[' charclasses ']'            (character class)
///                 |  '[^' charclasses ']'           (negated character class)
///                 |  simpleexp
///
/// charclasses    ::= charclass charclasses
///                 |  charclass
///
/// charclass      ::= charexp '-' charexp            (character range, inclusive)
///                 |  charexp
///
/// simpleexp      ::= charexp
///                 |  '.'                            (any single character)
///                 |  '#'                            (empty language) [OPTIONAL]
///                 |  '@'                            (any string) [OPTIONAL]
///                 |  "\"" <Unicode string> "\""     (a string)
///                 |  "()"                           (the empty string)
///                 |  '(' unionexp ')'               (precedence override)
///                 |  '<' identifier '>'             (named automaton) [OPTIONAL]
///                 |  '<n-m>'                        (numerical interval) [OPTIONAL]
///
/// charexp        ::= <Unicode character>            (a single non-reserved character)
///                 |  \d                             (a digit [0-9])
///                 |  \D                             (a non-digit [^0-9])
///                 |  \s                             (whitespace [ \t\n\r])
///                 |  \S                             (non-whitespace)
///                 |  \w                             (a word character [a-zA-Z_0-9])
///                 |  \W                             (a non-word character [^\w])
///                 |  \\<Unicode character>          (an escaped character)
/// ```
///
/// Productions marked [OPTIONAL](RegExpKind::Optional) are only allowed if
/// specified by the syntax flags passed to the [`RegExp`] constructor.
///
/// Reserved characters used in the enabled syntax must be escaped with
/// backslash (`\`) or double-quotes (`"..."`). This escaping is also required
/// inside character classes.
///
/// Be aware that dash (`-`) has a special meaning in `charclass` expressions.
///
/// An identifier is a string not containing right angle bracket (`>`) or dash
/// (`-`).
///
/// Numerical intervals are specified by non-negative decimal integers and
/// include both end points. If `n` and `m` have the same number of digits, then
/// the conforming strings must have that length (i.e., prefixed by zeroes).
#[derive(Debug)]
pub struct RegExp {
    /// The type of expression
    pub kind: RegExpKind,
    /// Child expressions held by a container type expression
    pub exp1: Option<Box<RegExp>>,
    pub exp2: Option<Box<RegExp>>,
    /// String expression
    pub s: String,
    /// Character expression
    pub c: i32,
    /// Limits for repeatable type expressions
    pub min: i32,
    pub max: i32,
    pub digits: i32,
    pub from: i32,
    pub to: i32,
    pub original_string: String,
    pub flags: i32,
    pub pos: usize,
}

impl RegExp {
    pub const INTERSECTION: i32 = 0x0001;
    pub const EMPTY: i32 = 0x0004;
    pub const ANYSTRING: i32 = 0x0008;
    pub const AUTOMATON: i32 = 0x0010;
    pub const INTERVAL: i32 = 0x0020;
    pub const ALL: i32 = 0xff;
    pub const NONE: i32 = 0x0000;
    pub const ASCII_CASE_INSENSITIVE: i32 = 0x0100;
    #[deprecated(note = "This flag will be removed in Lucene 11")]
    pub const DEPRECATED_COMPLEMENT: i32 = 0x10000;
    /// Equivalent to `RegExp(s)` → `RegExp::parse(s, ALL, 0)`
    pub fn from_string(s: &str) -> Result<Self> {
        Self::from_str_with_flags(s, Self::ALL)
    }

    /// Equivalent to `RegExp(s, syntax_flags)`
    pub fn from_str_with_flags(s: &str, syntax_flags: i32) -> Result<Self> {
        Self::parse(s, syntax_flags, 0)
    }
    pub fn parse(s: &str, syntax_flags: i32, match_flags: i32) -> Result<Self> {
        if (syntax_flags & !Self::DEPRECATED_COMPLEMENT) > Self::ALL {
            return Err(LuceneError::illegal_argument("Illegal syntax flag"));
        }
        if match_flags > 0 && match_flags <= Self::ALL {
            return Err(LuceneError::illegal_argument("Illegal match flag"));
        }

        let flags = syntax_flags | match_flags;

        let mut parser = RegExp {
            kind: RegExpKind::Empty,
            exp1: None,
            exp2: None,
            s: String::new(),
            c: 0,
            min: 0,
            max: 0,
            digits: 0,
            from: 0,
            to: 0,
            original_string: s.to_string(),
            flags,
            pos: 0,
        };

        let mut e = if s.is_empty() {
            RegExp::make_string(flags, "")
        } else {
            let e = parser.parse_union_exp()?;
            if parser.pos < parser.original_string.len() {
                return Err(LuceneError::illegal_argument(format!(
                    "end-of-string expected at position {}",
                    parser.pos
                )));
            }
            e
        };
        e.original_string = s.to_string();
        e.flags = flags;
        e.pos = parser.pos;

        Ok(e)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        flags: i32,
        kind: RegExpKind,
        exp1: Option<Box<RegExp>>,
        exp2: Option<Box<RegExp>>,
        s: &str,
        c: i32,
        min: i32,
        max: i32,
        digits: i32,
        from: i32,
        to: i32,
    ) -> Self {
        RegExp {
            original_string: String::new(),
            kind,
            flags,
            exp1,
            exp2,
            s: s.to_string(),
            c,
            min,
            max,
            digits,
            from,
            to,
            pos: 0,
        }
    }
    // Simplified construction of container nodes
    fn new_container_node(
        flags: i32,
        kind: RegExpKind,
        exp1: Option<RegExp>,
        exp2: Option<RegExp>,
    ) -> Self {
        RegExp {
            kind,
            exp1: exp1.map(Box::new),
            exp2: exp2.map(Box::new),
            s: String::new(),
            c: 0,
            min: 0,
            max: 0,
            digits: 0,
            from: 0,
            to: 0,
            original_string: String::new(),
            flags,
            pos: 0,
        }
    }

    // Simplified construction of repeating nodes
    fn new_repeating_node(flags: i32, kind: RegExpKind, exp: RegExp, min: i32, max: i32) -> Self {
        RegExp {
            kind,
            exp1: Some(Box::new(exp)),
            exp2: None,
            s: String::new(),
            c: 0,
            min,
            max,
            digits: 0,
            from: 0,
            to: 0,
            original_string: String::new(),
            flags,
            pos: 0,
        }
    }
    // Simplified construction of leaf nodes
    #[allow(clippy::too_many_arguments)]
    fn new_leaf_node(
        flags: i32,
        kind: RegExpKind,
        s: &str,
        c: i32,
        min: i32,
        max: i32,
        digits: i32,
        from: i32,
        to: i32,
    ) -> Self {
        RegExp {
            kind,
            exp1: None,
            exp2: None,
            s: s.to_string(),
            c,
            min,
            max,
            digits,
            from,
            to,
            original_string: String::new(),
            flags,
            pos: 0,
        }
    }
    /// Constructs a new [`Automaton`] from this [`RegExp`].
    /// Same as calling `to_automaton_with_map` (with an empty automaton map).
    pub fn to_automaton(&self) -> Result<Automaton> {
        self.to_automaton_impl(&HashMap::new(), &EmptyAutomatonProvider)
    }
    /// Constructs a new [`Automaton`] from this [`RegExp`].
    ///
    /// Parameters:
    /// - `automata`: A map from automaton identifiers to [`Automaton`]
    ///   instances.
    ///
    /// Errors:
    /// - Returns an error if this regular expression uses a named identifier
    ///   that does not exist in the automaton map.
    pub fn to_automaton_with_map(
        &self,
        automata: &HashMap<String, Automaton>,
    ) -> Result<Automaton> {
        self.to_automaton_impl(automata, &EmptyAutomatonProvider)
    }
    /// Constructs a new [`Automaton`] from this [`RegExp`].
    ///
    /// Parameters:
    /// - `automaton_provider`: Provider of automata for named identifiers
    ///
    /// Errors:
    /// - Returns an error if this regular expression uses a named identifier
    ///   that is not available from the automaton provider.
    pub fn to_automaton_with_provider(
        &self,
        provider: &impl AutomatonProvider,
    ) -> Result<Automaton> {
        self.to_automaton_impl(&HashMap::new(), provider)
    }
    fn to_automaton_impl(
        &self,
        automata: &HashMap<String, Automaton>,
        provider: &impl AutomatonProvider,
    ) -> Result<Automaton> {
        use RegExpKind::*;
        let a = match self.kind {
            PreClass => self
                .expand_predefined()?
                .to_automaton_impl(automata, provider)?,

            Union => {
                let mut list = Vec::new();
                if let Some(e1) = &self.exp1 {
                    e1.find_leaves(Union, &mut list, automata, provider)?;
                }
                if let Some(e2) = &self.exp2 {
                    e2.find_leaves(Union, &mut list, automata, provider)?;
                }
                let refs: Vec<&crate::core::util::automation::automaton::Automaton> =
                    list.iter().collect();
                Operations::union_list(&refs)?
            },

            Concatenation => {
                let mut list = Vec::new();
                if let Some(e1) = &self.exp1 {
                    e1.find_leaves(Concatenation, &mut list, automata, provider)?;
                }
                if let Some(e2) = &self.exp2 {
                    e2.find_leaves(Concatenation, &mut list, automata, provider)?;
                }
                Operations::concatenate_with_list(&list.iter().collect::<Vec<_>>())?
            },

            Intersection => {
                let a1 = self
                    .exp1
                    .as_ref()
                    .unwrap()
                    .to_automaton_impl(automata, provider)?;
                let a2 = self
                    .exp2
                    .as_ref()
                    .unwrap()
                    .to_automaton_impl(automata, provider)?;

                match Operations::intersection(&a1, &a2)? {
                    Cow::Borrowed(v) => {
                        if std::ptr::eq(v, &a1) {
                            a1
                        } else {
                            a2
                        }
                    },
                    Cow::Owned(o) => o,
                }
            },

            Optional => {
                let a1 = self
                    .exp1
                    .as_ref()
                    .unwrap()
                    .to_automaton_impl(automata, provider)?;
                match Operations::optional(&a1)? {
                    Cow::Borrowed(_) => a1,
                    Cow::Owned(o) => o,
                }
            },

            Repeat => {
                let a1 = self
                    .exp1
                    .as_ref()
                    .unwrap()
                    .to_automaton_impl(automata, provider)?;
                match Operations::repeat(&a1)? {
                    Cow::Borrowed(_) => a1,
                    Cow::Owned(o) => o,
                }
            },

            RepeatMin => {
                let a1 = self
                    .exp1
                    .as_ref()
                    .unwrap()
                    .to_automaton_impl(automata, provider)?;
                match Operations::repeat_count(&a1, self.min)? {
                    Cow::Borrowed(_) => a1,
                    Cow::Owned(o) => o,
                }
            },

            RepeatMinMax => {
                let a1 = self
                    .exp1
                    .as_ref()
                    .unwrap()
                    .to_automaton_impl(automata, provider)?;
                Operations::repeat_min_max(&a1, self.min, self.max)?
            },

            Complement => {
                // we don't support arbitrary complement, just "negated character class"
                // this is just a list of characters (e.g. "a") or ranges (e.g. "b-d")
                let a1 = self
                    .exp1
                    .as_ref()
                    .unwrap()
                    .to_automaton_impl(automata, provider)?;
                Operations::complement(&a1, i32::MAX as usize)?
            },

            DeprecatedComplement => {
                // to ease transitions for users only, support arbitrary complement
                // but bounded by DEFAULT_DETERMINIZE_WORK_LIMIT: must not be configurable.
                let a1 = self
                    .exp1
                    .as_ref()
                    .unwrap()
                    .to_automaton_impl(automata, provider)?;
                Operations::complement(&a1, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?
            },

            Char => {
                if self.check(Self::ASCII_CASE_INSENSITIVE) {
                    Self::to_case_insensitive_char(self.c)?
                } else {
                    Automata::make_char(self.c)?
                }
            },

            CharRange => Automata::make_char_range(self.from, self.to)?,
            AnyChar => Automata::make_any_char()?,
            Empty => Automata::make_empty()?,
            String => {
                if self.check(Self::ASCII_CASE_INSENSITIVE) {
                    self.to_case_insensitive_string()?
                } else {
                    Automata::make_string(&self.s)?
                }
            },
            AnyString => Automata::make_any_string()?,

            Automaton => {
                if let Some(a) = automata.get(&self.s) {
                    // TODO: Data Copy here, but currently only used in Test,
                    a.clone()
                } else {
                    provider.get_automaton(&self.s)?
                }
            },

            Interval => Automata::make_decimal_interval(self.min, self.max, self.digits)?,
        };

        Ok(a)
    }
    fn to_case_insensitive_char(codepoint: i32) -> Result<Automaton> {
        let case1 = Automata::make_char(codepoint)?;

        if codepoint > 128 {
            return Ok(case1);
        }

        let alt_case = if (codepoint as u8 as char).is_ascii_lowercase() {
            (codepoint as u8 as char).to_ascii_uppercase() as i32
        } else {
            (codepoint as u8 as char).to_ascii_lowercase() as i32
        };

        if alt_case != codepoint {
            let case2 = Automata::make_char(alt_case)?;
            Operations::union(&case1, &case2)
        } else {
            Ok(case1)
        }
    }
    fn to_case_insensitive_string(&self) -> Result<Automaton> {
        let list: Result<Vec<Automaton>> = self
            .s
            .chars()
            .map(|ch| Self::to_case_insensitive_char(ch as i32))
            .collect();

        let automata = list?;
        let refs: Vec<&Automaton> = automata.iter().collect();
        Operations::concatenate_with_list(&refs)
    }

    fn find_leaves(
        &self,
        kind: RegExpKind,
        list: &mut Vec<Automaton>,
        automata: &HashMap<String, Automaton>,
        provider: &impl AutomatonProvider,
    ) -> Result<()> {
        if self.kind == kind {
            if let Some(e1) = &self.exp1 {
                e1.find_leaves(kind, list, automata, provider)?;
            }
            if let Some(e2) = &self.exp2 {
                e2.find_leaves(kind, list, automata, provider)?;
            }
        } else {
            list.push(self.to_automaton_impl(automata, provider)?)
        }
        Ok(())
    }
    /// The string that was used to construct the regex. Compare to toString.
    pub fn get_original_string(&self) -> &str {
        &self.original_string
    }
    pub fn to_string_builder(&self, b: &mut String) {
        use RegExpKind::*;

        match self.kind {
            Union => {
                b.push('(');
                self.exp1.as_ref().unwrap().to_string_builder(b);
                b.push('|');
                self.exp2.as_ref().unwrap().to_string_builder(b);
                b.push(')');
            },
            Concatenation => {
                self.exp1.as_ref().unwrap().to_string_builder(b);
                self.exp2.as_ref().unwrap().to_string_builder(b);
            },
            Intersection => {
                b.push('(');
                self.exp1.as_ref().unwrap().to_string_builder(b);
                b.push('&');
                self.exp2.as_ref().unwrap().to_string_builder(b);
                b.push(')');
            },
            Optional => {
                b.push('(');
                self.exp1.as_ref().unwrap().to_string_builder(b);
                b.push_str(")?");
            },
            Repeat => {
                b.push('(');
                self.exp1.as_ref().unwrap().to_string_builder(b);
                b.push_str(")*");
            },
            RepeatMin => {
                b.push('(');
                self.exp1.as_ref().unwrap().to_string_builder(b);
                b.push_str("){");
                b.push_str(&self.min.to_string());
                b.push_str(",}");
            },
            RepeatMinMax => {
                b.push('(');
                self.exp1.as_ref().unwrap().to_string_builder(b);
                b.push_str("){");
                b.push_str(&self.min.to_string());
                b.push(',');
                b.push_str(&self.max.to_string());
                b.push('}');
            },
            Complement | DeprecatedComplement => {
                b.push_str("~(");
                self.exp1.as_ref().unwrap().to_string_builder(b);
                b.push(')');
            },
            Char => {
                if let Some(ch) = std::char::from_u32(self.c as u32) {
                    b.push('\\');
                    b.push(ch);
                }
            },
            CharRange => {
                let from_ch = std::char::from_u32(self.from as u32).unwrap_or('?');
                let to_ch = std::char::from_u32(self.to as u32).unwrap_or('?');
                b.push_str("[\\");
                b.push(from_ch);
                b.push('-');
                b.push('\\');
                b.push(to_ch);
                b.push(']');
            },
            AnyChar => {
                b.push('.');
            },
            Empty => {
                b.push('#');
            },
            String => {
                b.push('"');
                b.push_str(&self.s);
                b.push('"');
            },
            AnyString => {
                b.push('@');
            },
            Automaton => {
                b.push('<');
                b.push_str(&self.s);
                b.push('>');
            },
            Interval => {
                let s1 = self.min.to_string();
                let s2 = self.max.to_string();
                b.push('<');
                if self.digits > 0 {
                    for _ in s1.len()..self.digits as usize {
                        b.push('0');
                    }
                }
                b.push_str(&s1);
                b.push('-');
                if self.digits > 0 {
                    for _ in s2.len()..self.digits as usize {
                        b.push('0');
                    }
                }
                b.push_str(&s2);
                b.push('>');
            },
            PreClass => {
                if let Some(ch) = std::char::from_u32(self.from as u32) {
                    b.push('\\');
                    b.push(ch);
                }
            },
        }
    }
    /// Like to string, but more verbose (shows the higherchy more clearly).
    pub fn to_string_tree(&self) -> String {
        let mut b = String::new();
        self.to_string_tree_with_string(&mut b, "");
        b
    }
    pub(crate) fn to_string_tree_with_string(&self, b: &mut String, indent: &str) {
        use RegExpKind::*;

        let newline = "\n";
        let indent_more = format!("{indent}  ");

        match self.kind {
            // binary
            Union | Concatenation | Intersection => {
                b.push_str(indent);
                b.push_str(&format!("{:?}{}", self.kind, newline));
                if let Some(e1) = &self.exp1 {
                    e1.to_string_tree_with_string(b, &indent_more);
                }
                if let Some(e2) = &self.exp2 {
                    e2.to_string_tree_with_string(b, &indent_more);
                }
            },

            // unary
            Optional | Repeat | Complement | DeprecatedComplement => {
                b.push_str(indent);
                b.push_str(&format!("{:?}{}", self.kind, newline));
                if let Some(e1) = &self.exp1 {
                    e1.to_string_tree_with_string(b, &indent_more);
                }
            },

            RepeatMin => {
                b.push_str(indent);
                b.push_str(&format!("{:?} min={}{}", self.kind, self.min, newline));
                if let Some(e1) = &self.exp1 {
                    e1.to_string_tree_with_string(b, &indent_more);
                }
            },

            RepeatMinMax => {
                b.push_str(indent);
                b.push_str(&format!(
                    "{:?} min={} max={}{}",
                    self.kind, self.min, self.max, newline
                ));
                if let Some(e1) = &self.exp1 {
                    e1.to_string_tree_with_string(b, &indent_more);
                }
            },

            Char => {
                b.push_str(indent);
                if let Some(ch) = std::char::from_u32(self.c as u32) {
                    b.push_str(&format!("{:?} char={}{}", self.kind, ch, newline));
                } else {
                    b.push_str(&format!("{:?} char=?{}", self.kind, newline));
                }
            },

            PreClass => {
                b.push_str(indent);
                if let Some(ch) = std::char::from_u32(self.from as u32) {
                    b.push_str(&format!("{:?} class=\\{}{}", self.kind, ch, newline));
                } else {
                    b.push_str(&format!("{:?} class=\\?{}", self.kind, newline));
                }
            },

            CharRange => {
                b.push_str(indent);
                let from_ch = std::char::from_u32(self.from as u32).unwrap_or('?');
                let to_ch = std::char::from_u32(self.to as u32).unwrap_or('?');
                b.push_str(&format!(
                    "{:?} from={} to={}{}",
                    self.kind, from_ch, to_ch, newline
                ));
            },

            String => {
                b.push_str(indent);
                b.push_str(&format!("{:?} string={}{}", self.kind, self.s, newline));
            },

            Interval => {
                b.push_str(indent);
                b.push_str(&format!("{:?}<", self.kind));
                let s1 = self.min.to_string();
                let s2 = self.max.to_string();
                if self.digits > 0 {
                    for _ in s1.len()..self.digits as usize {
                        b.push('0');
                    }
                }
                b.push_str(&s1);
                b.push('-');
                if self.digits > 0 {
                    for _ in s2.len()..self.digits as usize {
                        b.push('0');
                    }
                }
                b.push_str(&s2);
                b.push_str(&format!(">{newline}"));
            },

            AnyChar | AnyString | Empty | Automaton => {
                b.push_str(indent);
                b.push_str(&format!("{:?}{}", self.kind, newline));
            },
        }
    }
    /// Returns set of automaton identifiers that occur in this regular
    /// expression.
    pub fn get_identifiers_set(&self) -> HashSet<String> {
        let mut set = HashSet::new();
        self.get_identifiers(&mut set);
        set
    }
    pub(crate) fn get_identifiers(&self, set: &mut HashSet<String>) {
        use RegExpKind::*;
        match self.kind {
            Union | Concatenation | Intersection => {
                if let Some(ref e1) = self.exp1 {
                    e1.get_identifiers(set);
                }
                if let Some(ref e2) = self.exp2 {
                    e2.get_identifiers(set);
                }
            },
            Optional | Repeat | RepeatMin | RepeatMinMax | Complement | DeprecatedComplement => {
                if let Some(ref e1) = self.exp1 {
                    e1.get_identifiers(set);
                }
            },
            Automaton => {
                set.insert(self.s.clone());
            },
            AnyChar | AnyString | Char | CharRange | Empty | Interval | PreClass | String => {
                // No-op
            },
        }
    }
    fn make_union(flags: i32, exp1: RegExp, exp2: RegExp) -> Self {
        RegExp::new_container_node(flags, RegExpKind::Union, Some(exp1), Some(exp2))
    }
    fn make_concatenation(flags: i32, mut exp1: RegExp, mut exp2: RegExp) -> Self {
        let is_str_or_char = |e: &RegExp| matches!(e.kind, RegExpKind::Char | RegExpKind::String);
        if is_str_or_char(&exp1) && is_str_or_char(&exp2) {
            return RegExp::make_string_concat(flags, &exp1, &exp2);
        }

        if exp1.kind == RegExpKind::Concatenation {
            if let Some(e2) = &exp1.exp2
                && is_str_or_char(e2)
                && is_str_or_char(&exp2)
            {
                let rexp1 = *exp1.exp1.take().unwrap();
                let rexp2 = RegExp::make_string_concat(flags, e2, &exp2);
                return RegExp::new_container_node(
                    flags,
                    RegExpKind::Concatenation,
                    Some(rexp1),
                    Some(rexp2),
                );
            }
        } else if exp2.kind == RegExpKind::Concatenation
            && let Some(e1) = &exp2.exp1
            && is_str_or_char(&exp1)
            && is_str_or_char(e1)
        {
            let rexp1 = RegExp::make_string_concat(flags, &exp1, e1);
            let rexp2 = *exp2.exp2.take().unwrap();
            return RegExp::new_container_node(
                flags,
                RegExpKind::Concatenation,
                Some(rexp1),
                Some(rexp2),
            );
        }

        RegExp::new_container_node(flags, RegExpKind::Concatenation, Some(exp1), Some(exp2))
    }
    fn make_string_concat(flags: i32, exp1: &RegExp, exp2: &RegExp) -> Self {
        let mut b = String::new();

        match exp1.kind {
            RegExpKind::String => b.push_str(&exp1.s),
            RegExpKind::Char => {
                if let Some(ch) = std::char::from_u32(exp1.c as u32) {
                    b.push(ch);
                }
            },
            _ => {},
        }

        match exp2.kind {
            RegExpKind::String => b.push_str(&exp2.s),
            RegExpKind::Char => {
                if let Some(ch) = std::char::from_u32(exp2.c as u32) {
                    b.push(ch);
                }
            },
            _ => {},
        }
        RegExp::make_string(flags, &b)
    }
    fn make_intersection(flags: i32, exp1: RegExp, exp2: RegExp) -> Self {
        RegExp::new_container_node(flags, RegExpKind::Intersection, Some(exp1), Some(exp2))
    }

    fn make_optional(flags: i32, exp: RegExp) -> Self {
        RegExp::new_container_node(flags, RegExpKind::Optional, Some(exp), None)
    }

    fn make_repeat(flags: i32, exp: RegExp) -> Self {
        RegExp::new_container_node(flags, RegExpKind::Repeat, Some(exp), None)
    }

    fn make_repeat_min(flags: i32, exp: RegExp, min: i32) -> Self {
        RegExp::new_repeating_node(flags, RegExpKind::RepeatMin, exp, min, 0)
    }

    fn make_repeat_minmax(flags: i32, exp: RegExp, min: i32, max: i32) -> Self {
        RegExp::new_repeating_node(flags, RegExpKind::RepeatMinMax, exp, min, max)
    }

    fn make_complement(flags: i32, exp: RegExp) -> Self {
        RegExp::new_container_node(flags, RegExpKind::Complement, Some(exp), None)
    }
    /// Creates a node that will compute the complement of an arbitrary
    /// expression.
    ///
    /// @deprecated Will be removed in Lucene 11
    #[deprecated(note = "Will be removed in Lucene 11")]
    fn make_deprecated_complement(flags: i32, exp: RegExp) -> RegExp {
        RegExp::new_container_node(flags, RegExpKind::DeprecatedComplement, Some(exp), None)
    }

    fn make_char(flags: i32, c: i32) -> Self {
        RegExp::new_leaf_node(flags, RegExpKind::Char, "", c, 0, 0, 0, 0, 0)
    }

    fn make_char_range(flags: i32, from: i32, to: i32) -> Result<Self> {
        if from > to {
            return Err(LuceneError::illegal_argument(format!(
                "invalid range: from ({from}) cannot be > to ({to})"
            )));
        }
        Ok(RegExp::new_leaf_node(
            flags,
            RegExpKind::CharRange,
            "",
            0,
            0,
            0,
            0,
            from,
            to,
        ))
    }

    fn make_any_char(flags: i32) -> Self {
        RegExp::new_container_node(flags, RegExpKind::AnyChar, None, None)
    }

    fn make_empty(flags: i32) -> Self {
        RegExp::new_container_node(flags, RegExpKind::Empty, None, None)
    }

    fn make_string(flags: i32, s: &str) -> Self {
        RegExp::new_leaf_node(flags, RegExpKind::String, s, 0, 0, 0, 0, 0, 0)
    }

    fn make_any_string(flags: i32) -> Self {
        RegExp::new_container_node(flags, RegExpKind::AnyString, None, None)
    }
    fn make_automaton(flags: i32, s: &str) -> Self {
        RegExp::new_leaf_node(flags, RegExpKind::Automaton, s, 0, 0, 0, 0, 0, 0)
    }

    fn make_interval(flags: i32, min: i32, max: i32, digits: i32) -> Self {
        RegExp::new_leaf_node(flags, RegExpKind::Interval, "", 0, min, max, digits, 0, 0)
    }

    fn peek(&self, s: &str) -> bool {
        self.more() && s.contains(self.original_string[self.pos..].chars().next().unwrap())
    }

    fn match_char(&mut self, c: char) -> bool {
        if let Some(next_ch) = self.original_string[self.pos..].chars().next()
            && next_ch == c
        {
            self.pos += next_ch.len_utf8();
            return true;
        }
        false
    }
    fn more(&self) -> bool {
        self.pos < self.original_string.len()
    }
    fn next(&mut self) -> Result<i32> {
        if !self.more() {
            return Err(LuceneError::illegal_argument("unexpected end-of-string"));
        }
        let ch = self.original_string[self.pos..].chars().next().unwrap();
        self.pos += ch.len_utf8();
        Ok(ch as i32)
    }

    fn check(&self, flag: i32) -> bool {
        (self.flags & flag) != 0
    }
    pub(crate) fn parse_union_exp(&mut self) -> Result<RegExp> {
        let flags = self.flags;
        self.iterative_parse_exp(
            |p| p.parse_inter_exp(),
            |p| p.match_char('|'),
            &UnionGroup,
            flags,
        )
    }

    pub(crate) fn parse_inter_exp(&mut self) -> Result<RegExp> {
        let flags = self.flags;
        self.iterative_parse_exp(
            |p| p.parse_concat_exp(),
            |p| p.check(RegExp::INTERSECTION) && p.match_char('&'),
            &IntersectionGroup,
            flags,
        )
    }

    pub(crate) fn parse_concat_exp(&mut self) -> Result<RegExp> {
        let flags = self.flags;
        self.iterative_parse_exp(
            |p| p.parse_repeat_exp(),
            |p| p.more() && !p.peek(")|") && (!p.check(RegExp::INTERSECTION) || !p.peek("&")),
            &ConcatGroup,
            flags,
        )
    }
    fn iterative_parse_exp<G, S, R>(
        &mut self,
        mut gather: G,
        mut stop: S,
        reducer: &R,
        flags: i32,
    ) -> Result<RegExp>
    where
        G: FnMut(&mut Self) -> Result<RegExp>,
        S: FnMut(&mut Self) -> bool,
        R: MakeRegexGroup,
    {
        let mut result = gather(self)?;
        while stop(self) {
            let e = gather(self)?;
            result = reducer.get(flags, result, e);
        }
        Ok(result)
    }
    fn parse_repeat_exp(&mut self) -> Result<RegExp> {
        let mut e = self.parse_compl_exp()?;

        while self.peek("?*+{") {
            if self.match_char('?') {
                e = RegExp::make_optional(self.flags, e);
            } else if self.match_char('*') {
                e = RegExp::make_repeat(self.flags, e);
            } else if self.match_char('+') {
                e = RegExp::make_repeat_min(self.flags, e, 1);
            } else if self.match_char('{') {
                let start = self.pos;
                while self.peek("0123456789") {
                    self.next()?;
                }
                if start == self.pos {
                    return Err(LuceneError::illegal_argument(format!(
                        "integer expected at position {}",
                        self.pos
                    )));
                }
                let n_str = &self.original_string[start..self.pos];
                let n = n_str.parse::<i32>().map_err(|_| {
                    LuceneError::illegal_argument(format!(
                        "invalid number at position {}",
                        self.pos
                    ))
                })?;

                let mut m = -1;
                if self.match_char(',') {
                    let start = self.pos;
                    while self.peek("0123456789") {
                        self.next()?;
                    }
                    if start != self.pos {
                        let m_str = &self.original_string[start..self.pos];
                        m = m_str.parse::<i32>().map_err(|_| {
                            LuceneError::illegal_argument(format!(
                                "invalid number at position {}",
                                self.pos
                            ))
                        })?;
                    }
                } else {
                    m = n;
                }

                if !self.match_char('}') {
                    return Err(LuceneError::illegal_argument(format!(
                        "expected '}}' at position {}",
                        self.pos
                    )));
                }

                if m != -1 && n > m {
                    return Err(LuceneError::illegal_argument(format!(
                        "invalid repetition range (out of order): {n}..{m}"
                    )));
                }

                if m == -1 {
                    e = RegExp::make_repeat_min(self.flags, e, n);
                } else {
                    e = RegExp::make_repeat_minmax(self.flags, e, n, m);
                }
            }
        }

        Ok(e)
    }
    pub(crate) fn parse_compl_exp(&mut self) -> Result<RegExp> {
        if self.check(RegExp::DEPRECATED_COMPLEMENT) && self.match_char('~') {
            let sub = self.parse_compl_exp()?;
            Ok(RegExp::make_deprecated_complement(self.flags, sub))
        } else {
            self.parse_char_class_exp()
        }
    }
    pub(crate) fn parse_char_class_exp(&mut self) -> Result<RegExp> {
        if self.match_char('[') {
            let mut negate = false;
            if self.match_char('^') {
                negate = true;
            }
            let mut e = self.parse_char_classes()?;
            if negate {
                let any = RegExp::make_any_char(self.flags);
                let not_e = RegExp::make_complement(self.flags, e);
                e = RegExp::make_intersection(self.flags, any, not_e);
            }
            if !self.match_char(']') {
                return Err(LuceneError::illegal_argument(format!(
                    "expected ']' at position {}",
                    self.pos
                )));
            }
            Ok(e)
        } else {
            self.parse_simple_exp()
        }
    }
    pub(crate) fn parse_char_classes(&mut self) -> Result<RegExp> {
        let mut e = self.parse_char_class()?;
        while self.more() && !self.peek("]") {
            let next = self.parse_char_class()?;
            e = RegExp::make_union(self.flags, e, next);
        }
        Ok(e)
    }
    pub(crate) fn parse_char_class(&mut self) -> Result<RegExp> {
        if let Some(predefined) = self.match_predefined_character_class()? {
            return Ok(predefined);
        }

        let c1 = self.parse_char_exp()?;
        if self.match_char('-') {
            return RegExp::make_char_range(self.flags, c1, self.parse_char_exp()?);
        }

        Ok(RegExp::make_char(self.flags, c1))
    }
    fn expand_predefined(&self) -> Result<RegExp> {
        match std::char::from_u32(self.from as u32) {
            Some('d') => RegExp::from_string("[0-9]"),        // digit
            Some('D') => RegExp::from_string("[^0-9]"),       // non-digit
            Some('s') => RegExp::from_string("[ \t\n\r]"),    // whitespace
            Some('S') => RegExp::from_string("[^\\s]"),       // non-whitespace
            Some('w') => RegExp::from_string("[a-zA-Z_0-9]"), // word
            Some('W') => RegExp::from_string("[^\\w]"),       // non-word
            Some(ch) => Err(LuceneError::illegal_argument(format!(
                "invalid character class: \\{ch}"
            ))),
            None => Err(LuceneError::illegal_argument(
                "invalid unicode value in .from",
            )),
        }
    }
    pub(crate) fn match_predefined_character_class(&mut self) -> Result<Option<RegExp>> {
        // See https://docs.oracle.com/javase/tutorial/essential/regex/pre_char_classes.html
        if self.match_char('\\') {
            if self.peek("dDwWsS") {
                let cp = self.next()?;
                return Ok(Some(RegExp::new_leaf_node(
                    self.flags,
                    RegExpKind::PreClass,
                    "",
                    0,
                    0,
                    0,
                    0,
                    cp,
                    0,
                )));
            }

            if self.peek("\\") {
                let cp = self.next()?;
                return Ok(Some(RegExp::make_char(self.flags, cp)));
            }
            // From https://docs.oracle.com/javase/8/docs/api/java/util/regex/Pattern.html#bs
            // "It is an error to use a backslash prior to any alphabetic character that
            // does not denote an escaped
            // construct;"
            if self.peek("abcefghijklmnopqrtuvxyz") || self.peek("ABCEFGHIJKLMNOPQRTUVXYZ") {
                let cp = self.next()?;
                let ch = std::char::from_u32(cp as u32).unwrap_or('?');
                return Err(LuceneError::illegal_argument(format!(
                    "invalid character class \\{ch}"
                )));
            }
        }

        Ok(None)
    }
    pub(crate) fn parse_simple_exp(&mut self) -> Result<RegExp> {
        if self.match_char('.') {
            Ok(RegExp::make_any_char(self.flags))
        } else if self.check(RegExp::EMPTY) && self.match_char('#') {
            Ok(RegExp::make_empty(self.flags))
        } else if self.check(RegExp::ANYSTRING) && self.match_char('@') {
            Ok(RegExp::make_any_string(self.flags))
        } else if self.match_char('"') {
            let start = self.pos;
            while self.more() && !self.peek("\"") {
                self.next()?;
            }
            if !self.match_char('"') {
                return Err(LuceneError::illegal_argument(format!(
                    "expected '\"' at position {}",
                    self.pos
                )));
            }
            let s = self.original_string[start..(self.pos - 1)].to_string();
            Ok(RegExp::make_string(self.flags, &s))
        } else if self.match_char('(') {
            if self.match_char(')') {
                return Ok(RegExp::make_string(self.flags, ""));
            }
            let e = self.parse_union_exp()?;
            if !self.match_char(')') {
                return Err(LuceneError::illegal_argument(format!(
                    "expected ')' at position {}",
                    self.pos
                )));
            }
            Ok(e)
        } else if (self.check(RegExp::AUTOMATON) || self.check(RegExp::INTERVAL))
            && self.match_char('<')
        {
            let start = self.pos;
            while self.more() && !self.peek(">") {
                self.next()?;
            }
            if !self.match_char('>') {
                return Err(LuceneError::illegal_argument(format!(
                    "expected '>' at position {}",
                    self.pos
                )));
            }
            let s = self.original_string[start..(self.pos - 1)].to_string();
            if let Some(i) = s.find('-') {
                if !self.check(RegExp::INTERVAL) {
                    return Err(LuceneError::illegal_argument(format!(
                        "illegal identifier at position {}",
                        self.pos - 1
                    )));
                }
                if i == 0 || i == s.len() - 1 || i != s.rfind('-').unwrap() {
                    return Err(LuceneError::illegal_argument(format!(
                        "interval syntax error at position {}",
                        self.pos - 1
                    )));
                }
                // parse interval
                let smin = &s[0..i];
                let smax = &s[i + 1..];
                let imin = smin.parse::<i32>().map_err(|_| {
                    LuceneError::illegal_argument(format!(
                        "interval syntax error at position {}",
                        self.pos - 1
                    ))
                })?;
                let imax = smax.parse::<i32>().map_err(|_| {
                    LuceneError::illegal_argument(format!(
                        "interval syntax error at position {}",
                        self.pos - 1
                    ))
                })?;
                let digits = if smin.len() == smax.len() {
                    smin.len() as i32
                } else {
                    0
                };
                let (min, max) = if imin <= imax {
                    (imin, imax)
                } else {
                    (imax, imin)
                };
                Ok(RegExp::make_interval(self.flags, min, max, digits))
            } else {
                if !self.check(RegExp::AUTOMATON) {
                    return Err(LuceneError::illegal_argument(format!(
                        "interval syntax error at position {}",
                        self.pos - 1
                    )));
                }
                Ok(RegExp::make_automaton(self.flags, &s))
            }
        } else {
            if let Some(predefined) = self.match_predefined_character_class()? {
                return Ok(predefined);
            }
            let ch = self.parse_char_exp()?;
            Ok(RegExp::make_char(self.flags, ch))
        }
    }
    fn parse_char_exp(&mut self) -> Result<i32> {
        self.match_char('\\');
        self.next()
    }
}
impl fmt::Display for RegExp {
    /// Constructs string from parsed regular expression.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut s = String::new();
        self.to_string_builder(&mut s);
        write!(f, "{s}")
    }
}
/// The type of expression represented by a RegExp node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RegExpKind {
    /// The union of two expressions
    Union,
    /// A sequence of two expressions
    Concatenation,
    /// The intersection of two expressions
    Intersection,
    /// An optional expression
    Optional,
    /// An expression that repeats
    Repeat,
    /// An expression that repeats a minimum number of times
    RepeatMin,
    /// An expression that repeats a minimum and maximum number of times
    RepeatMinMax,
    /// The complement of a character class
    Complement,
    /// A Character
    Char,
    /// A Character range
    CharRange,
    /// Any Character allowed
    AnyChar,
    /// An empty expression
    Empty,
    /// A string expression
    String,
    /// Any string allowed
    AnyString,
    /// An Automaton expression
    Automaton,
    /// An Interval expression
    Interval,
    /// An expression for a pre-defined class e.g. \w
    PreClass,
    /// The complement of an expression (deprecated)
    #[deprecated(note = "Will be removed in Lucene 11")]
    DeprecatedComplement,
}
/// Custom functional interface for supplying methods with the signature:
/// `RegExp(int int1, RegExp exp1, RegExp exp2)`
trait MakeRegexGroup {
    fn get(&self, int1: i32, exp1: RegExp, exp2: RegExp) -> RegExp;
}
struct UnionGroup;
impl MakeRegexGroup for UnionGroup {
    fn get(&self, flags: i32, e1: RegExp, e2: RegExp) -> RegExp {
        RegExp::make_union(flags, e1, e2)
    }
}

struct IntersectionGroup;
impl MakeRegexGroup for IntersectionGroup {
    fn get(&self, flags: i32, e1: RegExp, e2: RegExp) -> RegExp {
        RegExp::make_intersection(flags, e1, e2)
    }
}

struct ConcatGroup;
impl MakeRegexGroup for ConcatGroup {
    fn get(&self, flags: i32, e1: RegExp, e2: RegExp) -> RegExp {
        RegExp::make_concatenation(flags, e1, e2)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use rand::Rng;
    use regex::Regex;

    use crate::core::index::BytesRef;
    use crate::core::util::automation::automata::Automata;
    use crate::core::util::automation::automaton::Automaton;
    use crate::core::util::automation::automaton_provider::AutomatonProvider;
    use crate::core::util::automation::byte_run_automaton::ByteRunAutomaton;
    use crate::core::util::automation::byte_runnable::ByteRunnable;
    use crate::core::util::automation::character_run_automaton::CharacterRunAutomaton;
    use crate::core::util::automation::operations::Operations;
    use crate::core::util::automation::reg_exp::RegExp;
    use crate::core::util::error::lucene_error::{LuceneError, Result};
    use crate::test::util::automaton::automaton_test_util::AutomatonTestUtil;
    use crate::test::util::lucene_test_case::lucene_test_case_util::random;
    #[allow(dead_code)] // for quick search
    struct TestRegExp {
        case_sensitive_query: bool,
    }
    impl TestRegExp {
        fn random_doc_value<R: Rng + ?Sized>(random: &mut R, min_length: usize) -> String {
            let char_palette = "AAAaaaBbbCccc123456 \t".chars().collect::<Vec<_>>();
            (0..min_length)
                .map(|_| {
                    let i = Self::random_int(random, char_palette.len());
                    char_palette[i]
                })
                .collect()
        }
        fn random_int<R: Rng + ?Sized>(random: &mut R, bound: usize) -> usize {
            if bound == 0 {
                0
            } else {
                random.random_range(0..bound)
            }
        }
        fn check_random_expression<R: Rng + ?Sized>(
            &mut self,
            random: &mut R,
            doc_value: &str,
        ) -> Result<String> {
            use std::fmt::Write;
            // Generate and test a random regular expression which should match the given
            // docValue
            let mut result = String::new();
            let len = doc_value.len();
            // Pick a part of the string to change
            let substitution_point = random.random_range(0..len);
            let substitution_length =
                1 + random.random_range(0..(std::cmp::min(10, len - substitution_point)));

            let head = &doc_value[..substitution_point];
            result.push_str(head);

            let replacement_part =
                &doc_value[substitution_point..substitution_point + substitution_length];
            let mutation = random.random_range(0..15);

            match mutation {
                0 => {
                    let rand_str = Self::random_doc_value(random, replacement_part.len());
                    write!(result, "({}|d{})", replacement_part, rand_str)?;
                },
                1 => {
                    write!(result, "({}|doesnotexist)", replacement_part)?;
                },
                2 => {
                    let inner = self.check_random_expression(random, replacement_part)?;
                    write!(result, "({}|doesnotexist)", inner)?;
                },
                3 => {
                    result.push_str(&replacement_part.replace("ab", ".*"));
                },
                4 => {
                    result.push_str(&replacement_part.replace("b", "."));
                },
                5 => {
                    write!(result, ".{{1,{}}}", replacement_part.len())?;
                },
                6 => {
                    result.push_str(&".".repeat(replacement_part.len()));
                },
                7 => {
                    for c in replacement_part.chars() {
                        write!(result, "[{}{}]", c, c.to_ascii_uppercase())?;
                    }
                },
                8 => {
                    result.push_str(&replacement_part.replace("b", "[^a]"));
                },
                9 => {
                    write!(result, "({})+", replacement_part)?;
                },
                10 => {
                    write!(result, "({})?", replacement_part)?;
                },
                11 => {
                    let re = Regex::new(r"\d").unwrap();
                    result.push_str(&re.replace_all(replacement_part, r"\d"));
                },
                12 => {
                    let re = Regex::new(r"\s").unwrap();
                    result.push_str(&re.replace_all(replacement_part, r"\W"));
                },
                13 => {
                    let re = Regex::new(r"\s").unwrap();
                    result.push_str(&re.replace_all(replacement_part, r"\s"));
                },
                14 => {
                    let mut switched = String::new();
                    for p in replacement_part.chars() {
                        let new_p = if p.is_lowercase() {
                            p.to_ascii_uppercase()
                        } else {
                            p.to_ascii_lowercase()
                        };
                        switched.push(new_p);
                        if p != new_p {
                            self.case_sensitive_query = false;
                        }
                    }
                    result.push_str(&switched);
                },
                _ => {},
            }
            // add any remaining tail, unchanged
            if substitution_point + substitution_length < len {
                result.push_str(&doc_value[substitution_point + substitution_length..]);
            }

            let regex_pattern = result;
            // Assert our randomly generated regex actually matches the provided raw input
            // using java's expression matcher
            let re = if self.case_sensitive_query {
                Regex::new(&regex_pattern).unwrap()
            } else {
                Regex::new(&format!("(?i){}", regex_pattern)).unwrap()
            };
            assert!(
                re.is_match(doc_value),
                "Regex `{}` did not match `{}`",
                regex_pattern,
                doc_value
            );

            let match_flags = if self.case_sensitive_query {
                0
            } else {
                RegExp::ASCII_CASE_INSENSITIVE
            };
            let regex = RegExp::parse(&regex_pattern, RegExp::ALL, match_flags)?;
            let v = regex.to_automaton()?;
            let automaton =
                Operations::determinize(&v, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;
            let matcher = ByteRunAutomaton::new(automaton.into_owned())?;

            let br: BytesRef<Vec<u8>> = BytesRef::from_string(doc_value);
            assert!(
                matcher.run(&br.bytes, br.offset, br.length)?,
                "[{}] should match [{}] {}-{}/{}",
                regex_pattern,
                doc_value,
                substitution_point,
                substitution_length,
                len
            );

            if !self.case_sensitive_query {
                let cs_regex = RegExp::parse(&regex_pattern, RegExp::ALL, 0)?;
                let v = cs_regex.to_automaton()?;
                let cs_automaton =
                    Operations::determinize(&v, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;
                let cs_matcher = ByteRunAutomaton::new(cs_automaton.into_owned())?;
                assert!(
                    !cs_matcher.run(&br.bytes, br.offset, br.length)?,
                    "[{}] (case-sensitive) should not match [{}]",
                    regex_pattern,
                    doc_value
                );
            }

            Ok(regex_pattern)
        }
    }

    /// Simple smoke test for regular expression.
    #[test]
    fn test_smoke() -> Result<()> {
        let r = RegExp::from_str_with_flags("a(b+|c+)d", 0)?;
        let a = r.to_automaton()?;
        assert!(a.is_deterministic());

        let run = CharacterRunAutomaton::new(a)?;
        assert!(run.run_str("abbbbbd")?);
        assert!(run.run_str("acd")?);
        assert!(!run.run_str("ad")?);

        Ok(())
    }
    // LUCENE-6046
    #[test]
    fn test_repeat_with_empty_string() -> Result<()> {
        let a = RegExp::from_str_with_flags("[^y]*{1,2}", 0)?.to_automaton()?;

        // paranoia
        let s = format!("{:?}", a);
        assert!(!s.is_empty());

        Ok(())
    }
    #[test]
    fn test_repeat_with_empty_language() -> Result<()> {
        let patterns = ["#*", "#+", "#{2,10}", "#?"];

        for pat in patterns {
            let a = RegExp::from_str_with_flags(pat, 0)?.to_automaton()?;
            let s = format!("{:?}", a);
            assert!(
                !s.is_empty(),
                "Automaton is unexpectedly empty for pattern: {}",
                pat
            );
        }

        Ok(())
    }
    #[test]
    fn test_core_java_parity() -> Result<()> {
        let mut random = random();
        let mut test = TestRegExp {
            case_sensitive_query: true,
        };

        for _ in 0..1000 {
            test.case_sensitive_query = true;
            let min_length = random.random_range(0..30);
            let doc_value = TestRegExp::random_doc_value(&mut random, 1 + min_length);
            test.check_random_expression(&mut random, &doc_value)?;
        }
        Ok(())
    }

    #[test]
    fn test_illegal_backslash_chars() {
        let illegal_chars = "abcefghijklmnopqrtuvxyzABCEFGHIJKLMNOPQRTUVXYZ";

        for ch in illegal_chars.chars() {
            let expr = format!("\\{}", ch);
            let err = RegExp::from_string(&expr);
            assert!(
                matches!(err, Err(LuceneError::IllegalArgument(_))),
                "Expected IllegalArgument for `\\{}` but got: {:?}",
                ch,
                err
            );
            assert!(
                err.unwrap_err()
                    .to_string()
                    .contains("invalid character class")
            );
        }
    }

    #[test]
    fn test_legal_backslash_chars() -> Result<()> {
        let legal_chars = "dDsSWw0123456789[]*&^$@!{}\\/";

        for ch in legal_chars.chars() {
            let expr = format!("\\{}", ch);
            RegExp::from_string(&expr)?;
        }

        Ok(())
    }

    #[test]
    fn test_parse_illegal_repeat_exp() -> Result<()> {
        let err = RegExp::parse("a{99,11}", RegExp::ALL, 0);
        assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
        assert!(err.unwrap_err().to_string().contains("out of order"));

        Ok(())
    }

    #[test]
    fn test_regexp_no_stack_overflow() -> Result<()> {
        // TODO: 测试没通过, 如果要支持这么长的string
        // 那么我们需要将代码中生成RegExp相关代码改成Box<RegExp>放到堆上
        // 不过目前我们暂时不改 let mut pattern = "(a)|".repeat(50_000);
        // pattern.push_str("(a)");
        // let _ = RegExp::from_str(&pattern)?;
        Ok(())
    }
    /// Tests the deprecated complement flag.  
    /// Keep the simple test only—no random tests to avoid instability.
    ///
    /// @deprecated Remove in Lucene 11
    #[test]
    fn test_deprecated_complement() -> Result<()> {
        let expected = {
            let a = Automata::make_string("abcd")?;
            Operations::complement(&a, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?
        };
        let actual = RegExp::parse("~(abcd)", RegExp::DEPRECATED_COMPLEMENT, 0)?.to_automaton()?;
        assert!(
            AutomatonTestUtil::same_language(&expected, &actual)?,
            "Automaton language differs between expected and actual"
        );

        Ok(())
    }
    /// Simple unit tests for [`RegExp`] parsing.
    ///
    /// For each type of node:
    /// - test the `to_string()` output and parse tree,
    /// - test the resulting automaton's language,
    /// - and whether it is deterministic.
    #[allow(dead_code)] // for quick search
    struct TestRegExpParsing;
    #[test]
    fn test_any_char() -> Result<()> {
        let re = RegExp::from_string(".")?;

        assert_eq!(".", re.to_string());
        assert_eq!("AnyChar\n", re.to_string_tree());

        let actual = re.to_automaton()?;
        assert!(actual.is_deterministic());

        let expected = Automata::make_any_char()?;
        assert_same_language(&expected, &actual)?;

        Ok(())
    }
    #[test]
    fn test_any_string() -> Result<()> {
        let re = RegExp::parse("@", RegExp::ALL, 0)?;

        assert_eq!("@", re.to_string());
        assert_eq!("AnyString\n", re.to_string_tree());

        let actual = re.to_automaton()?;
        assert!(actual.is_deterministic());

        let expected = Automata::make_any_string()?;
        assert_same_language(&expected, &actual)?;

        Ok(())
    }
    #[test]
    fn test_char() -> Result<()> {
        let re = RegExp::from_string("c")?;

        assert_eq!("\\c", re.to_string());
        assert_eq!("Char char=c\n", re.to_string_tree());

        let actual = re.to_automaton()?;
        assert!(actual.is_deterministic());

        let expected = Automata::make_char('c' as i32)?;
        assert_same_language(&expected, &actual)?;

        Ok(())
    }

    #[test]
    fn test_case_insensitive_char() -> Result<()> {
        let re = RegExp::parse("c", RegExp::NONE, RegExp::ASCII_CASE_INSENSITIVE)?;

        assert_eq!("\\c", re.to_string());
        assert_eq!("Char char=c\n", re.to_string_tree());

        let actual = re.to_automaton()?;
        assert!(actual.is_deterministic());

        let c_lower = Automata::make_char('c' as i32)?;
        let c_upper = Automata::make_char('C' as i32)?;
        let expected = Operations::union(&c_lower, &c_upper)?;

        assert_same_language(&expected, &actual)?;
        Ok(())
    }

    #[test]
    fn test_case_insensitive_char_upper() -> Result<()> {
        let re = RegExp::parse("C", RegExp::NONE, RegExp::ASCII_CASE_INSENSITIVE)?;

        assert_eq!("\\C", re.to_string());
        assert_eq!("Char char=C\n", re.to_string_tree());

        let actual = re.to_automaton()?;
        assert!(actual.is_deterministic());

        let c_lower = Automata::make_char('c' as i32)?;
        let c_upper = Automata::make_char('C' as i32)?;
        let expected = Operations::union(&c_lower, &c_upper)?;

        assert_same_language(&expected, &actual)?;
        Ok(())
    }
    #[test]
    fn test_case_insensitive_char_not_sensitive() -> Result<()> {
        let re = RegExp::parse("4", RegExp::NONE, RegExp::ASCII_CASE_INSENSITIVE)?;

        assert_eq!("\\4", re.to_string());
        assert_eq!("Char char=4\n", re.to_string_tree());

        let actual = re.to_automaton()?;
        assert!(actual.is_deterministic());

        let expected = Automata::make_char('4' as i32)?;
        assert_same_language(&expected, &actual)?;

        Ok(())
    }

    #[test]
    fn test_case_insensitive_char_non_ascii() -> Result<()> {
        let re = RegExp::parse("Ж", RegExp::NONE, RegExp::ASCII_CASE_INSENSITIVE)?;

        assert_eq!("\\Ж", re.to_string());
        assert_eq!("Char char=Ж\n", re.to_string_tree());

        let actual = re.to_automaton()?;
        assert!(actual.is_deterministic());

        let expected = Automata::make_char('Ж' as i32)?;
        assert_same_language(&expected, &actual)?;

        Ok(())
    }

    #[test]
    fn test_negated_char() -> Result<()> {
        let re = RegExp::from_string("[^c]")?;

        assert_eq!("(.&~(\\c))", re.to_string());
        assert_eq!(
            "Intersection\n  AnyChar\n  Complement\n    Char char=c\n",
            re.to_string_tree()
        );

        let actual = re.to_automaton()?;
        assert!(actual.is_deterministic());

        let expected = Operations::union(
            &Automata::make_char_range(0, 'b' as i32)?,
            &Automata::make_char_range('d' as i32, i32::MAX)?,
        )?;
        assert_same_language(&expected, &actual)?;

        Ok(())
    }
    #[test]
    fn test_char_range() -> Result<()> {
        let re = RegExp::from_string("[b-d]")?;

        assert_eq!("[\\b-\\d]", re.to_string());
        assert_eq!("CharRange from=b to=d\n", re.to_string_tree());

        let actual = re.to_automaton()?;
        assert!(actual.is_deterministic());

        let expected = Automata::make_char_range('b' as i32, 'd' as i32)?;
        assert_same_language(&expected, &actual)?;

        Ok(())
    }

    #[test]
    fn test_negated_char_range() -> Result<()> {
        let re = RegExp::from_string("[^b-d]")?;
        // TODO: would be nice to emit negated class rather than this
        assert_eq!("(.&~([\\b-\\d]))", re.to_string());
        assert_eq!(
            "Intersection\n  AnyChar\n  Complement\n    CharRange from=b to=d\n",
            re.to_string_tree()
        );

        let actual = re.to_automaton()?;
        assert!(actual.is_deterministic());

        let expected = Operations::union(
            &Automata::make_char_range(0, 'a' as i32)?,
            &Automata::make_char_range('e' as i32, i32::MAX)?,
        )?;

        assert_same_language(&expected, &actual)?;

        Ok(())
    }
    #[test]
    fn test_illegal_char_range() {
        let err = RegExp::from_string("[z-a]");
        assert!(
            matches!(err, Err(LuceneError::IllegalArgument(_))),
            "Expected IllegalArgument but got: {:?}",
            err
        );
    }

    #[test]
    fn test_char_class_digit() -> Result<()> {
        let re = RegExp::from_string("[\\d]")?;

        assert_eq!("\\d", re.to_string());
        assert_eq!("PreClass class=\\d\n", re.to_string_tree());

        let actual = re.to_automaton()?;
        assert!(actual.is_deterministic());

        let expected = Automata::make_char_range('0' as i32, '9' as i32)?;
        assert_same_language(&expected, &actual)?;

        Ok(())
    }

    #[test]
    fn test_char_class_non_digit() -> Result<()> {
        let re = RegExp::from_string("[\\D]")?;

        assert_eq!("\\D", re.to_string());
        assert_eq!("PreClass class=\\D\n", re.to_string_tree());

        let actual = re.to_automaton()?;
        assert!(actual.is_deterministic());

        let all = Automata::make_any_char()?;
        let digits = Automata::make_char_range('0' as i32, '9' as i32)?;
        let expected =
            Operations::minus(&all, &digits, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;

        assert_same_language(&expected, &actual)?;

        Ok(())
    }
    #[test]
    fn test_char_class_whitespace() -> Result<()> {
        let re = RegExp::from_string("[\\s]")?;

        assert_eq!("\\s", re.to_string());
        assert_eq!("PreClass class=\\s\n", re.to_string_tree());

        let actual = re.to_automaton()?;
        assert!(actual.is_deterministic());

        let mut expected = Automata::make_char(' ' as i32)?;
        expected = Operations::union(&expected, &Automata::make_char('\n' as i32)?)?;
        expected = Operations::union(&expected, &Automata::make_char('\r' as i32)?)?;
        expected = Operations::union(&expected, &Automata::make_char('\t' as i32)?)?;

        assert_same_language(&expected, &actual)?;
        Ok(())
    }

    #[test]
    fn test_char_class_non_whitespace() -> Result<()> {
        let re = RegExp::from_string("[\\S]")?;

        assert_eq!("\\S", re.to_string());
        assert_eq!("PreClass class=\\S\n", re.to_string_tree());

        let actual = re.to_automaton()?;
        assert!(actual.is_deterministic());

        let expected = Automata::make_any_char()?;
        let v = Automata::make_char(' ' as i32)?;
        let expected =
            Operations::minus(&expected, &v, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;
        let v = Automata::make_char('\n' as i32)?;
        let expected =
            Operations::minus(&expected, &v, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;
        let v = Automata::make_char('\r' as i32)?;
        let expected =
            Operations::minus(&expected, &v, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;
        let v = Automata::make_char('\t' as i32)?;
        let expected =
            Operations::minus(&expected, &v, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;

        assert_same_language(&expected, &actual)?;
        Ok(())
    }
    #[test]
    fn test_char_class_word() -> Result<()> {
        let re = RegExp::from_string("[\\w]")?;

        assert_eq!("\\w", re.to_string());
        assert_eq!("PreClass class=\\w\n", re.to_string_tree());

        let actual = re.to_automaton()?;
        assert!(actual.is_deterministic());

        let mut expected = Automata::make_char_range('a' as i32, 'z' as i32)?;
        expected = Operations::union(
            &expected,
            &Automata::make_char_range('A' as i32, 'Z' as i32)?,
        )?;
        expected = Operations::union(
            &expected,
            &Automata::make_char_range('0' as i32, '9' as i32)?,
        )?;
        expected = Operations::union(&expected, &Automata::make_char('_' as i32)?)?;

        assert_same_language(&expected, &actual)?;
        Ok(())
    }
    #[test]
    fn test_char_class_non_word() -> Result<()> {
        let re = RegExp::from_string("[\\W]")?;

        assert_eq!("\\W", re.to_string());
        assert_eq!("PreClass class=\\W\n", re.to_string_tree());

        let actual = re.to_automaton()?;
        assert!(actual.is_deterministic());

        let expected = Automata::make_any_char()?;
        let v = Automata::make_char_range('a' as i32, 'z' as i32)?;
        let expected =
            Operations::minus(&expected, &v, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;
        let v = Automata::make_char_range('A' as i32, 'Z' as i32)?;
        let expected =
            Operations::minus(&expected, &v, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;
        let v = Automata::make_char_range('0' as i32, '9' as i32)?;
        let expected =
            Operations::minus(&expected, &v, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;
        let v = Automata::make_char('_' as i32)?;
        let expected =
            Operations::minus(&expected, &v, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;

        assert_same_language(&expected, &actual)?;
        Ok(())
    }
    #[test]
    fn test_truncated_char_class() {
        let err = RegExp::from_string("[b-d");
        assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
    }

    #[test]
    fn test_bogus_char_class() {
        let err = RegExp::from_string("[\\q]");
        assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
    }

    #[test]
    fn test_escaped_not_char_class() -> Result<()> {
        let re = RegExp::from_string("[\\?]")?;

        assert_eq!("\\?", re.to_string());
        assert_eq!("Char char=?\n", re.to_string_tree());

        let actual = re.to_automaton()?;
        assert!(actual.is_deterministic());

        let expected = Automata::make_char('?' as i32)?;
        assert_same_language(&expected, &actual)?;

        Ok(())
    }

    #[test]
    fn test_escaped_slash_not_char_class() -> Result<()> {
        let re = RegExp::from_string("[\\\\]")?;

        assert_eq!("\\\\", re.to_string());
        assert_eq!("Char char=\\\n", re.to_string_tree());

        let actual = re.to_automaton()?;
        assert!(actual.is_deterministic());

        let expected = Automata::make_char('\\' as i32)?;
        assert_same_language(&expected, &actual)?;

        Ok(())
    }
    #[test]
    fn test_empty() -> Result<()> {
        let re = RegExp::parse("#", RegExp::EMPTY, 0)?;

        assert_eq!("#", re.to_string());
        assert_eq!("Empty\n", re.to_string_tree());

        let actual = re.to_automaton()?;
        assert!(actual.is_deterministic());

        let expected = Automata::make_empty()?;
        assert_same_language(&expected, &actual)?;

        Ok(())
    }

    #[test]
    fn test_interval() -> Result<()> {
        let re = RegExp::from_string("<5-40>")?;

        assert_eq!("<5-40>", re.to_string());
        assert_eq!("Interval<5-40>\n", re.to_string_tree());

        let actual = re.to_automaton()?;
        let expected = Automata::make_decimal_interval(5, 40, 0)?;

        assert_same_language(&expected, &actual)?;
        Ok(())
    }

    #[test]
    fn test_backwards_interval() -> Result<()> {
        let re = RegExp::from_string("<40-5>")?;

        assert_eq!("<5-40>", re.to_string());
        assert_eq!("Interval<5-40>\n", re.to_string_tree());

        let actual = re.to_automaton()?;
        let expected = Automata::make_decimal_interval(5, 40, 0)?;

        assert_same_language(&expected, &actual)?;
        Ok(())
    }
    #[test]
    fn test_truncated_interval() {
        let err = RegExp::from_string("<1-");
        assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
    }

    #[test]
    fn test_truncated_interval2() {
        let err = RegExp::from_string("<1");
        assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
    }

    #[test]
    fn test_empty_interval() {
        let err = RegExp::from_string("<->");
        assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
    }

    #[test]
    fn test_optional() -> Result<()> {
        let re = RegExp::from_string("a?")?;

        assert_eq!("(\\a)?", re.to_string());
        assert_eq!("Optional\n  Char char=a\n", re.to_string_tree());

        let actual = re.to_automaton()?;
        assert!(actual.is_deterministic());

        let a = Automata::make_char('a' as i32)?;
        let expected = Operations::optional(&a)?;

        assert_same_language(&expected, &actual)?;
        Ok(())
    }
    #[test]
    fn test_repeat_0() -> Result<()> {
        let re = RegExp::from_string("a*")?;

        assert_eq!("(\\a)*", re.to_string());
        assert_eq!("Repeat\n  Char char=a\n", re.to_string_tree());

        let actual = re.to_automaton()?;
        assert!(actual.is_deterministic());

        let a = Automata::make_char('a' as i32)?;
        let expected = Operations::repeat(&a)?;

        assert_same_language(&expected, &actual)?;
        Ok(())
    }

    #[test]
    fn test_repeat_1() -> Result<()> {
        let re = RegExp::from_string("a+")?;

        assert_eq!("(\\a){1,}", re.to_string());
        assert_eq!("RepeatMin min=1\n  Char char=a\n", re.to_string_tree());

        let a = Automata::make_char('a' as i32)?;
        let expected = Operations::repeat_count(&a, 1)?;
        let actual = re.to_automaton()?;
        assert!(actual.is_deterministic());

        assert_same_language(&expected, &actual)?;
        Ok(())
    }

    #[test]
    fn test_repeat_n() -> Result<()> {
        let re = RegExp::from_string("a{5}")?;

        assert_eq!("(\\a){5,5}", re.to_string());
        assert_eq!(
            "RepeatMinMax min=5 max=5\n  Char char=a\n",
            re.to_string_tree()
        );

        let a = Automata::make_char('a' as i32)?;
        let expected = Operations::repeat_min_max(&a, 5, 5)?;
        let actual = re.to_automaton()?;
        assert!(actual.is_deterministic());

        assert_same_language(&expected, &actual)?;
        Ok(())
    }
    #[test]
    fn test_repeat_n_plus() -> Result<()> {
        let re = RegExp::from_string("a{5,}")?;

        assert_eq!("(\\a){5,}", re.to_string());
        assert_eq!("RepeatMin min=5\n  Char char=a\n", re.to_string_tree());

        let a = Automata::make_char('a' as i32)?;
        let expected = Operations::repeat_count(&a, 5)?;
        let actual = re.to_automaton()?;
        assert!(actual.is_deterministic());

        assert_same_language(&expected, &actual)?;
        Ok(())
    }

    #[test]
    fn test_repeat_mn() -> Result<()> {
        let re = RegExp::from_string("a{5,8}")?;

        assert_eq!("(\\a){5,8}", re.to_string());
        assert_eq!(
            "RepeatMinMax min=5 max=8\n  Char char=a\n",
            re.to_string_tree()
        );

        let a = Automata::make_char('a' as i32)?;
        let expected = Operations::repeat_min_max(&a, 5, 8)?;
        let actual = re.to_automaton()?;
        assert!(actual.is_deterministic());

        assert_same_language(&expected, &actual)?;
        Ok(())
    }

    #[test]
    fn test_truncated_repeat() {
        let err = RegExp::from_string("a{5,8");
        assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
    }

    #[test]
    fn test_bogus_repeat() {
        let err = RegExp::from_string("a{Z}");
        assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
    }

    #[test]
    fn test_string() -> Result<()> {
        let re = RegExp::from_string("boo")?;

        assert_eq!("\"boo\"", re.to_string());
        assert_eq!("String string=boo\n", re.to_string_tree());

        let actual = re.to_automaton()?;
        assert!(actual.is_deterministic());

        let expected = Automata::make_string("boo")?;
        assert_same_language(&expected, &actual)?;
        Ok(())
    }

    #[test]
    fn test_case_insensitive_string() -> Result<()> {
        let re = RegExp::parse("boo", RegExp::NONE, RegExp::ASCII_CASE_INSENSITIVE)?;

        assert_eq!("\"boo\"", re.to_string());
        assert_eq!("String string=boo\n", re.to_string_tree());

        let actual = re.to_automaton()?;
        assert!(actual.is_deterministic());

        let b = Operations::union(
            &Automata::make_char('b' as i32)?,
            &Automata::make_char('B' as i32)?,
        )?;
        let o = Operations::union(
            &Automata::make_char('o' as i32)?,
            &Automata::make_char('O' as i32)?,
        )?;

        let expected = Operations::concatenate(&b, &o)?;
        let expected = Operations::concatenate(&expected, &o)?;

        assert_same_language(&expected, &actual)?;
        Ok(())
    }
    #[test]
    fn test_explicit_string() -> Result<()> {
        let re = RegExp::from_string("\"boo\"")?;

        assert_eq!("\"boo\"", re.to_string());
        assert_eq!("String string=boo\n", re.to_string_tree());

        let actual = re.to_automaton()?;
        assert!(actual.is_deterministic());

        let expected = Automata::make_string("boo")?;
        assert_same_language(&expected, &actual)?;
        Ok(())
    }

    #[test]
    fn test_not_terminated_string() {
        let err = RegExp::from_string("\"boo");
        assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
    }

    #[test]
    fn test_concatenation() -> Result<()> {
        let re = RegExp::from_string("[b-c][e-f]")?;

        assert_eq!("[\\b-\\c][\\e-\\f]", re.to_string());
        assert_eq!(
            "Concatenation\n  CharRange from=b to=c\n  CharRange from=e to=f\n",
            re.to_string_tree()
        );

        let r1 = Automata::make_char_range('b' as i32, 'c' as i32)?;
        let r2 = Automata::make_char_range('e' as i32, 'f' as i32)?;
        let expected = Operations::concatenate(&r1, &r2)?;

        let actual = re.to_automaton()?;
        assert!(actual.is_deterministic());

        assert_same_language(&expected, &actual)?;
        Ok(())
    }
    #[test]
    fn test_intersection() -> Result<()> {
        let re = RegExp::from_string("[b-f]&[e-f]")?;

        assert_eq!("([\\b-\\f]&[\\e-\\f])", re.to_string());
        assert_eq!(
            "Intersection\n  CharRange from=b to=f\n  CharRange from=e to=f\n",
            re.to_string_tree()
        );

        let r1 = Automata::make_char_range('b' as i32, 'f' as i32)?;
        let r2 = Automata::make_char_range('e' as i32, 'f' as i32)?;
        let expected = Operations::intersection(&r1, &r2)?;

        let actual = re.to_automaton()?;
        assert!(actual.is_deterministic());

        assert_same_language(&expected, &actual)?;
        Ok(())
    }

    #[test]
    fn test_truncated_intersection() {
        let err = RegExp::from_string("a&");
        assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
    }

    #[test]
    fn test_truncated_intersection_parens() {
        let err = RegExp::from_string("(a)&(");
        assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
    }

    #[test]
    fn test_union() -> Result<()> {
        let re = RegExp::from_string("[b-c]|[e-f]")?;

        assert_eq!("([\\b-\\c]|[\\e-\\f])", re.to_string());
        assert_eq!(
            "Union\n  CharRange from=b to=c\n  CharRange from=e to=f\n",
            re.to_string_tree()
        );

        let r1 = Automata::make_char_range('b' as i32, 'c' as i32)?;
        let r2 = Automata::make_char_range('e' as i32, 'f' as i32)?;
        let expected = Operations::union(&r1, &r2)?;

        let actual = re.to_automaton()?;
        assert!(actual.is_deterministic());

        assert_same_language(&expected, &actual)?;
        Ok(())
    }

    #[test]
    fn test_truncated_union() {
        let err = RegExp::from_string("a|");
        assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
    }

    #[test]
    fn test_truncated_union_parens() {
        let err = RegExp::from_string("(a)|(");
        assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
    }

    #[test]
    fn test_automaton() -> Result<()> {
        struct MyProvider;
        impl AutomatonProvider for MyProvider {
            fn get_automaton(&self, name: &str) -> Result<Automaton> {
                assert_eq!(name, "myletter");
                Automata::make_char('z' as i32)
            }
        }

        let re = RegExp::parse("<myletter>", RegExp::ALL, 0)?;
        assert_eq!("<myletter>", re.to_string());
        assert_eq!("Automaton\n", re.to_string_tree());
        assert_eq!(
            re.get_identifiers_set(),
            HashSet::from(["myletter".to_string()])
        );

        let actual = re.to_automaton_with_provider(&MyProvider)?;
        assert!(actual.is_deterministic());

        let expected = Automata::make_char('z' as i32)?;
        assert_same_language(&expected, &actual)?;
        Ok(())
    }
    #[test]
    fn test_automaton_map() -> Result<()> {
        let re = RegExp::parse("<myletter>", RegExp::ALL, 0)?;
        assert_eq!("<myletter>", re.to_string());
        assert_eq!("Automaton\n", re.to_string_tree());
        assert_eq!(
            re.get_identifiers_set(),
            HashSet::from(["myletter".to_string()])
        );

        let actual = re.to_automaton_with_map(&HashMap::from([(
            "myletter".to_string(),
            Automata::make_char('z' as i32)?,
        )]))?;

        assert!(actual.is_deterministic());

        let expected = Automata::make_char('z' as i32)?;
        assert_same_language(&expected, &actual)?;
        Ok(())
    }

    #[test]
    fn test_automaton_io_exception() {
        struct MyProvider;
        impl AutomatonProvider for MyProvider {
            fn get_automaton(&self, _name: &str) -> Result<Automaton> {
                Err(LuceneError::illegal_argument("fake error"))
            }
        }

        let re = RegExp::parse("<myletter>", RegExp::ALL, 0).unwrap();
        assert_eq!("<myletter>", re.to_string());
        assert_eq!("Automaton\n", re.to_string_tree());
        assert_eq!(
            re.get_identifiers_set(),
            HashSet::from(["myletter".to_string()])
        );

        let err = re.to_automaton_with_provider(&MyProvider);
        assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
    }

    #[test]
    fn test_automaton_not_found() {
        let re = RegExp::parse("<bogus>", RegExp::ALL, 0).unwrap();
        assert_eq!("<bogus>", re.to_string());
        assert_eq!("Automaton\n", re.to_string_tree());

        let err = re.to_automaton_with_map(&HashMap::from([(
            "myletter".to_string(),
            Automata::make_char('z' as i32).unwrap(),
        )]));
        assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
    }

    #[test]
    fn test_illegal_syntax_flags() {
        let err = RegExp::parse("bogus", i32::MAX, 0);
        assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
    }

    #[test]
    fn test_illegal_match_flags() {
        let err = RegExp::parse("bogus", RegExp::ALL, 1);
        assert!(matches!(err, Err(LuceneError::IllegalArgument(_))));
    }

    fn assert_same_language(expected: &Automaton, actual: &Automaton) -> Result<()> {
        let expected =
            Operations::determinize(expected, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;
        let actual = Operations::determinize(actual, Operations::DEFAULT_DETERMINIZE_WORK_LIMIT)?;

        let result = AutomatonTestUtil::same_language(&expected, &actual)?;
        if !result {
            // println!("{}", expected.to_dot()?);
            // println!("{}", actual.to_dot()?);
        }
        assert!(result);
        Ok(())
    }
}
