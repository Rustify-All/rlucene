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
use crate::core::index::index_reader::{Identity, IndexReader, IndexReaderContextType};
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::leaf_reader::{LRBits, LeafReader};
use crate::core::index::reader_util::ReaderUtil;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::{HasIdentity, TryIntoInt};
use std::fmt::{Display, Formatter};

/// Concatenates multiple [`Bits`] together on every lookup.
///
/// **NOTE:** This is very costly, as every lookup must perform a binary search
/// to locate the correct sub-reader.
pub struct MultiBits<B> {
  subs: Box<[Option<B>]>,
  starts: Box<[usize]>,
  default_value: bool,
  id: Identity,
}

pub enum MultiBitsType<B> {
  A(B),
  B(MultiBits<B>),
}

impl<B> Clone for MultiBitsType<B>
where
  B: Clone,
  MultiBits<B>: Clone,
{
  fn clone(&self) -> Self {
    match self {
      Self::A(bits) => Self::A(bits.clone()),
      Self::B(bits) => Self::B(bits.clone()),
    }
  }
}

impl<B> HasIdentity for MultiBitsType<B>
where
  B: HasIdentity,
{
  fn identity(&self) -> &Identity {
    match self {
      Self::A(bits) => bits.identity(),
      Self::B(bits) => bits.identity(),
    }
  }
}

impl<B> Bits for MultiBitsType<B>
where
  B: Bits,
{
  fn get(&self, index: usize) -> Result<bool> {
    match self {
      Self::A(bits) => bits.get(index),
      Self::B(bits) => bits.get(index),
    }
  }

  fn length(&self) -> usize {
    match self {
      Self::A(bits) => bits.length(),
      Self::B(bits) => bits.length(),
    }
  }

  fn copy_of(&self) -> Result<FixedBitSet> {
    match self {
      Self::A(bits) => bits.copy_of(),
      Self::B(bits) => bits.copy_of(),
    }
  }

  fn to_string(&self) -> String {
    match self {
      Self::A(bits) => bits.to_string(),
      Self::B(bits) => Bits::to_string(bits),
    }
  }
}

impl<B> MultiBits<B> {
  pub fn new(subs: Vec<Option<B>>, starts: Vec<usize>, default_value: bool) -> Self {
    debug_assert_eq!(starts.len(), subs.len() + 1);
    Self {
      subs: subs.into_boxed_slice(),
      starts: starts.into_boxed_slice(),
      default_value,
      id: Identity::new(),
    }
  }
}

impl<B> MultiBits<B>
where
  B: Bits,
{
  fn check_length(&self, reader: usize, doc: usize) -> bool {
    let length = self.starts[reader + 1] - self.starts[reader];
    debug_assert!(
      doc - self.starts[reader] < length,
      "doc={} reader={} starts[reader]={} length={}",
      doc,
      reader,
      self.starts[reader],
      length
    );
    true
  }
}

impl<B> HasIdentity for MultiBits<B> {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl<B> Bits for MultiBits<B>
where
  B: Bits,
{
  fn get(&self, index: usize) -> Result<bool> {
    let reader = ReaderUtil::sub_index(index, &self.starts);
    debug_assert!(reader != -1);

    let reader = reader as usize;
    let bits = &self.subs[reader];
    match bits {
      None => Ok(self.default_value),
      Some(bits) => {
        debug_assert!(self.check_length(reader, index));
        bits.get(index - self.starts[reader])
      },
    }
  }

  fn length(&self) -> usize {
    let len = self.starts.len() - 1;
    self.starts[len]
  }
}
impl<B> Display for MultiBits<B>
where
  B: Bits,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{} subs: ", self.subs.len())?;

    for i in 0..self.subs.len() {
      if i != 0 {
        write!(f, "; ")?;
      }

      match &self.subs[i] {
        None => {
          write!(f, "s={} l=null", self.starts[i])?;
        },
        Some(bits) => {
          write!(
            f,
            "s={} l={} b={}",
            self.starts[i],
            bits.length(),
            bits.to_string()
          )?;
        },
      }
    }
    write!(f, " end={}", self.starts[self.subs.len()])
  }
}
/// Returns a single [`Bits`] instance for this reader, merging live documents on the fly.
/// This method will return `None` if the reader has no deletions.
///
/// **NOTE:** this is a very slow way to access live docs.
/// For example, each [`Bits`] access will require a binary search.
/// It's better to get the sub-readers and iterate through them yourself.
pub fn get_live_docs<IR>(reader: IR) -> Result<Option<BitsType<IR>>>
where
  IR: IndexReader,
{
  if !reader.has_deletions()? {
    return Ok(None);
  }
  let max_doc = reader.max_doc()?;
  let ctx = reader.get_context()?;
  let leaves = ctx.leaves()?;
  let size = leaves.len();
  debug_assert!(
    size > 0,
    "A reader with deletions must have at least one leave"
  );

  if size == 1 {
    return match leaves[0].reader().get_live_docs()? {
      Some(bits) => Ok(Some(MultiBitsType::A(bits))),
      None => Ok(None),
    };
  }

  let mut live_docs = Vec::with_capacity(size);
  let mut starts: Vec<usize> = Vec::with_capacity(size + 1);

  for ctx in leaves {
    // record all liveDocs, even if they are None
    live_docs.push(ctx.reader().get_live_docs()?);
    starts.push(ctx.doc_base);
  }

  starts.push(max_doc.try_convert()?);

  Ok(Some(MultiBitsType::B(MultiBits::new(
    live_docs, starts, true,
  ))))
}
pub type BitsType<IR> = MultiBitsType<LRBits<IRCLeafReader<IndexReaderContextType<IR>>>>;
