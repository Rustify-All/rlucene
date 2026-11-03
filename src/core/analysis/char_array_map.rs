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
use std::collections::HashMap;
use std::fmt::{Display, Formatter};

/// A map that stores string keys as character arrays in a hash table.
#[derive(Clone)]
pub struct CharArrayMap<T> {
  ignore: bool,
  map: HashMap<Vec<char>, T>,
}
impl<T> CharArrayMap<T> {
  /// Creates a map with enough capacity to hold `start_size` terms.
  ///
  /// # Arguments
  ///
  /// * `ignore_case` — `false` if and only if the set should be case-sensitive, otherwise `true`.
  pub fn new(ignore: bool) -> Self {
    CharArrayMap {
      ignore,
      map: HashMap::new(),
    }
  }
  fn put_all_with<I, K, F>(&mut self, v: I, mut key_fn: F)
  where
    I: IntoIterator<Item = (K, T)>,
    F: FnMut(K) -> Vec<char>,
  {
    for (k, val) in v {
      let key = key_fn(k);
      self.put(key, val);
    }
  }

  /// Inserts owned character keys from a map, array, or iterator.
  pub fn put_all<I>(&mut self, v: I)
  where
    I: IntoIterator<Item = (Vec<char>, T)>,
  {
    self.put_all_with(v, |k| k);
  }

  /// Inserts string keys from a map, array, or iterator.
  pub fn put_all_str<I, K>(&mut self, v: I)
  where
    I: IntoIterator<Item = (K, T)>,
    K: AsRef<str>,
  {
    self.put_all_with(v, |k| k.as_ref().chars().collect());
  }

  pub fn put_all_any<I, V>(&mut self, v: I)
  where
    I: IntoIterator<Item = (V, T)>,
    V: Display,
  {
    self.put_all_with(v, |k| k.to_string().chars().collect());
  }
  pub fn clear(&mut self) {
    self.map.clear();
  }
  pub fn contains_key(&self, key: &[char], off: usize, len: usize) -> bool {
    let slice = &key[off..off + len];
    match norm(self.ignore, slice) {
      Cow::Borrowed(_) => self.map.contains_key(slice),
      Cow::Owned(o) => self.map.contains_key(o.as_slice()),
    }
  }
  pub fn contains_key_str(&self, key: &str) -> bool {
    // UTF-8 byte length bounds the number of characters; longer keys keep the heap path.
    if key.len() <= 32 {
      let mut chars = ['\0'; 32];
      let mut len = 0;
      for c in key.chars() {
        chars[len] = c;
        len += 1;
      }
      return self.contains_key(&chars[..len], 0, len);
    }
    let chars: Vec<char> = key.chars().collect();
    debug_assert!(chars.len() <= i32::MAX as usize);
    self.contains_key(chars.as_slice(), 0, chars.len())
  }
  pub fn contains_key_any<V>(&self, key: &V) -> bool
  where
    V: Display,
  {
    let chars = key.to_string();
    self.contains_key_str(&chars)
  }

  pub fn put<'a, K>(&mut self, key: K, val: T) -> Option<T>
  where
    K: Into<Cow<'a, [char]>>,
  {
    let chars = key.into();
    if !self.ignore {
      return self.map.insert(chars.into_owned(), val);
    }
    match chars {
      Cow::Owned(mut chars) => {
        // Match Lucene's single-code-point lowercase mapping (U+0130 becomes 'i').
        for c in &mut chars {
          *c = if *c < '\u{C0}' {
            c.to_ascii_lowercase()
          } else {
            c.to_lowercase().next().unwrap_or(*c)
          };
        }
        self.map.insert(chars, val)
      },
      Cow::Borrowed(chars) => self.map.insert(norm(self.ignore, chars).into_owned(), val),
    }
  }
  pub fn put_str(&mut self, key: &str, val: T) -> Option<T> {
    let key: Vec<char> = key.chars().collect();
    self.put(key, val)
  }
  pub fn put_any<V>(&mut self, key: &V, val: T) -> Option<T>
  where
    V: Display,
  {
    let key: Vec<char> = key.to_string().chars().collect();
    self.put(key, val)
  }
  pub fn get(&self, key: &[char], off: usize, len: usize) -> Option<&T> {
    let slice = &key[off..off + len];
    match norm(self.ignore, slice) {
      Cow::Borrowed(_) => self.map.get(slice),
      Cow::Owned(o) => self.map.get(o.as_slice()),
    }
  }
  pub fn get_str(&self, key: &str) -> Option<&T> {
    // UTF-8 byte length bounds the number of characters; longer keys keep the heap path.
    if key.len() <= 32 {
      let mut chars = ['\0'; 32];
      let mut len = 0;
      for c in key.chars() {
        chars[len] = c;
        len += 1;
      }
      return self.get(&chars[..len], 0, len);
    }
    let chars: Vec<char> = key.chars().collect();
    self.get(chars.as_slice(), 0, chars.len())
  }
  pub fn get_any<V>(&self, key: &V) -> Option<&T>
  where
    V: Display,
  {
    let key = key.to_string();
    self.get_str(&key)
  }
  pub fn size(&self) -> usize {
    self.map.len()
  }
  pub(crate) fn original_keys(&self) -> impl Iterator<Item = &[char]> {
    self.map.keys().map(Vec::as_slice)
  }
  pub fn entry_iter(&mut self) -> impl Iterator<Item = (&Vec<char>, &mut T)> {
    self.map.iter_mut()
  }
  pub fn is_empty(&self) -> bool {
    self.map.is_empty()
  }
}
impl<T> Display for CharArrayMap<T>
where
  T: Display,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{{")?;
    let mut first = true;
    for (k, v) in &self.map {
      if !first {
        write!(f, ", ")?;
      }
      first = false;
      for ch in k {
        write!(f, "{ch}")?;
      }
      write!(f, "={v}")?;
    }
    write!(f, "}}")
  }
}
fn norm(ignore: bool, s: &[char]) -> Cow<'_, [char]> {
  // Match Lucene's single-code-point lowercase mapping (U+0130 becomes 'i').
  let lowercase = || {
    s.iter().map(|c| {
      if *c < '\u{C0}' {
        c.to_ascii_lowercase()
      } else {
        c.to_lowercase().next().unwrap_or(*c)
      }
    })
  };
  if ignore && !s.iter().copied().eq(lowercase()) {
    Cow::Owned(lowercase().collect())
  } else {
    Cow::Borrowed(s)
  }
}
pub fn empty_map() -> CharArrayMap<()> {
  CharArrayMap::new(false)
}
