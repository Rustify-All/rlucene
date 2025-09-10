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
use std::fmt::{Debug, Display, Formatter};

/// A simple class that stores key Strings as char[]'s in a hash table
pub struct CharArrayMap<T> {
    ignore: bool,
    map: HashMap<Vec<char>, T>,
}
impl<T> CharArrayMap<T>
where
    T: Debug,
{
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
    fn put_all_with<K, F>(&mut self, v: HashMap<K, T>, mut key_fn: F)
    where
        F: FnMut(K) -> Vec<char>,
    {
        for (k, val) in v {
            let key = key_fn(k);
            match norm(self.ignore, &key) {
                Cow::Borrowed(_) => {
                    self.map.insert(key, val);
                },
                Cow::Owned(o) => {
                    self.map.insert(o, val);
                },
            }
        }
    }

    pub fn put_all(&mut self, v: HashMap<Vec<char>, T>) {
        self.put_all_with(v, |k| k);
    }

    pub fn put_all_str(&mut self, v: HashMap<String, T>) {
        self.put_all_with(v, |k| k.chars().collect());
    }

    pub fn put_all_any<V>(&mut self, v: HashMap<V, T>)
    where
        V: Display,
    {
        self.put_all_with(v, |k| k.to_string().chars().collect());
    }
    pub fn clear(&mut self) {
        self.map.clear();
    }
    pub fn contains_key(&self, key: &[char], off: i32, len: i32) -> bool {
        let slice = &key[off as usize..(off + len) as usize];
        match norm(self.ignore, slice) {
            Cow::Borrowed(_) => self.map.contains_key(slice),
            Cow::Owned(o) => self.map.contains_key(o.as_slice()),
        }
    }
    pub fn contains_key_str(&self, key: &str) -> bool {
        let chars: Vec<char> = key.to_string().chars().collect();
        debug_assert!(chars.len() <= i32::MAX as usize);
        self.contains_key(chars.as_slice(), 0, chars.len() as i32)
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
        match norm(self.ignore, &chars) {
            Cow::Borrowed(_) => self.map.insert(chars.into_owned(), val),
            Cow::Owned(o) => self.map.insert(o, val),
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
    pub fn get(&self, key: &[char], off: i32, len: i32) -> Option<&T> {
        let slice = &key[off as usize..(off + len) as usize];
        match norm(self.ignore, slice) {
            Cow::Borrowed(_) => self.map.get(slice),
            Cow::Owned(o) => self.map.get(o.as_slice()),
        }
    }
    pub fn get_str(&self, key: &str) -> Option<&T> {
        let chars: Vec<char> = key.chars().collect();
        self.get(chars.as_slice(), 0, chars.len() as i32)
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
            let key_str: String = k.iter().collect();
            write!(f, "{}={}", key_str, v)?;
        }
        write!(f, "}}")
    }
}
fn norm(ignore: bool, s: &[char]) -> Cow<'_, [char]> {
    if ignore {
        Cow::Owned(s.iter().flat_map(|c| c.to_lowercase()).collect())
    } else {
        Cow::Borrowed(s)
    }
}

#[cfg(test)]
mod tests {
    use crate::core::analysis::char_array_map::CharArrayMap;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{random, random_multiplier};
    use rand::Rng;
    use std::collections::HashMap;

    fn do_random<R: Rng + ?Sized>(random: &mut R, iter: i32, ignore_case: bool) {
        let mut cmap = CharArrayMap::new(ignore_case);
        let mut hmap: HashMap<String, i32> = HashMap::new();

        for _ in 0..iter {
            let len = random.random_range(0..5);
            let key: Vec<char> = (0..len)
                .map(|_| (random.random_range(0..127) as u8 as char))
                .collect();
            let key_str: String = key.iter().collect();
            let hmap_key = if ignore_case {
                key_str.to_lowercase()
            } else {
                key_str.clone()
            };

            let val: i32 = random.random();

            let o1 = cmap.put(&key, val);
            let o2 = hmap.insert(hmap_key.clone(), val);
            assert_eq!(o1, o2, "put return value mismatch");
            assert_eq!(
                val,
                cmap.put_str(&key_str, val).unwrap(),
                "put_str mismatch"
            );
            assert_eq!(
                val,
                *cmap.get(&key, 0, key.len() as i32).unwrap(),
                "get(&[char], off, len) mismatch"
            );
            assert_eq!(
                Some(&val),
                cmap.get(&key, 0, key.len() as i32),
                "get(&[char]) mismatch"
            );
            assert_eq!(Some(&val), cmap.get_str(&key_str), "get(&str) mismatch");
            assert_eq!(hmap.len(), cmap.size());
        }
    }
    #[test]
    fn test_char_array_map() {
        let mut random = random();
        let num = 5 * random_multiplier();
        for i in 0..num {
            do_random(&mut random, i, false);
            do_random(&mut random, i, true);
        }
    }

    #[test]
    fn test_put_all_variants() {
        use std::collections::HashMap;
        let mut cmap = CharArrayMap::new(true);

        let mut v1: HashMap<Vec<char>, i32> = HashMap::new();
        v1.insert(vec!['a'], 1);
        cmap.put_all(v1);
        assert!(cmap.contains_key_str("a"));

        let mut v2: HashMap<String, i32> = HashMap::new();
        v2.insert("b".to_string(), 2);
        cmap.put_all_str(v2);
        assert!(cmap.contains_key_str("b"));

        let mut v3: HashMap<i32, i32> = HashMap::new();
        v3.insert(3, 3);
        cmap.put_all_any(v3);
        assert!(cmap.contains_key_str("3"));
    }
    #[test]
    fn test_methods() {
        // TODO
    }
    #[test]
    fn test_modify_on_unmodifiable() {
        // this test is not required in Rust Lucene
    }
    #[test]
    fn test_to_string() {
        let mut cm = CharArrayMap::new(false);
        cm.put("test".chars().collect::<Vec<char>>(), 1);
        assert_eq!("{test=1}", cm.to_string());
        cm.put("test2".chars().collect::<Vec<char>>(), 2);
        assert!(cm.to_string().contains(", "));
    }
}
