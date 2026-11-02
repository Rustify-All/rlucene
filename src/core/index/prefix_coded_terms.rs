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
use std::cmp::Ordering;
use std::hash::{Hash, Hasher};
use std::io::Cursor;
use std::rc::Rc;
use std::sync::Arc;

use crate::core::index::field_term_iterator::FieldTermIterator;
use crate::core::index::term::Term;
use crate::core::index::{BytesRef, BytesRefBuilder};
use crate::core::store::byte_buffers_data_input::{
  ByteBuffersDataInput, ByteBuffersDataInputBlock,
};
use crate::core::store::{ByteBuffersDataOutput, DataInput, DataOutput};
use crate::core::util::StringHelper;
use crate::core::util::access::{ByteSource, WritableVec};
use crate::core::util::accountable::Accountable;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::ram_usage_estimator::size_of_vec;

/// Prefix codes term instances (prefixes are shared). This is expected to be
/// faster to build than an FST and might also be more compact if there are no
/// common suffixes.
///
/// # Lucene Internal
#[derive(Debug, Clone)]
pub struct PrefixCodedTerms<B = Vec<u8>> {
  content: Vec<Cursor<B>>,
  content_len: usize,
  size: i64,
  del_gen: i64,

  lazy_hash: i32,
}

pub type PrefixCodedTermsRc = PrefixCodedTerms<Rc<Vec<u8>>>;
pub type PrefixCodedTermsArc = PrefixCodedTerms<Arc<Vec<u8>>>;

impl<B> PrefixCodedTerms<B> {
  pub fn new(content: Vec<Cursor<B>>, content_len: usize, size: i64) -> Self {
    debug_assert!(!content.is_empty());
    PrefixCodedTerms {
      content,
      content_len,
      size,
      del_gen: 0,
      lazy_hash: 0,
    }
  }

  /// Records del gen for this packet.
  pub fn set_del_gen(&mut self, del_gen: i64) {
    self.del_gen = del_gen;
  }
  /// Return the number of terms stored in this [`PrefixCodedTerms`].
  pub fn size(&self) -> i64 {
    self.size
  }
}

impl PrefixCodedTerms<Vec<u8>> {
  pub fn iterator(&self) -> Result<TermIteratorRef<'_>> {
    let content = self.content.iter().map(|cursor| {
      let slice = cursor.get_ref().as_slice();
      let mut cursor = Cursor::new(slice);
      cursor.set_position(0);
      cursor
    });
    Ok(TermIterator::new(
      self.del_gen,
      ByteBuffersDataInput::new(content, self.content_len)?,
    ))
  }
}

impl PrefixCodedTerms<Rc<Vec<u8>>> {
  pub fn iterator(&self) -> Result<TermIteratorRc> {
    let content = self.content.iter().map(|cursor| {
      let mut cursor = Cursor::new(cursor.get_ref().clone());
      cursor.set_position(0);
      cursor
    });
    Ok(TermIterator::new(
      self.del_gen,
      ByteBuffersDataInput::new(content, self.content_len)?,
    ))
  }
}

impl PrefixCodedTerms<Arc<Vec<u8>>> {
  pub fn iterator(&self) -> Result<TermIteratorArc> {
    let content = self.content.iter().map(|cursor| {
      let mut cursor = Cursor::new(cursor.get_ref().clone());
      cursor.set_position(0);
      cursor
    });
    Ok(TermIterator::new(
      self.del_gen,
      ByteBuffersDataInput::new(content, self.content_len)?,
    ))
  }
}

impl From<PrefixCodedTerms<Vec<u8>>> for PrefixCodedTermsRc {
  fn from(value: PrefixCodedTerms<Vec<u8>>) -> Self {
    let content = value
      .content
      .into_iter()
      .map(|cursor| {
        let mut cursor = Cursor::new(Rc::new(cursor.into_inner()));
        cursor.set_position(0);
        cursor
      })
      .collect();
    PrefixCodedTerms {
      content,
      content_len: value.content_len,
      size: value.size,
      del_gen: value.del_gen,
      lazy_hash: value.lazy_hash,
    }
  }
}

impl From<PrefixCodedTerms<Vec<u8>>> for PrefixCodedTermsArc {
  fn from(value: PrefixCodedTerms<Vec<u8>>) -> Self {
    let content = value
      .content
      .into_iter()
      .map(|cursor| {
        let mut cursor = Cursor::new(Arc::new(cursor.into_inner()));
        cursor.set_position(0);
        cursor
      })
      .collect();
    PrefixCodedTerms {
      content,
      content_len: value.content_len,
      size: value.size,
      del_gen: value.del_gen,
      lazy_hash: value.lazy_hash,
    }
  }
}

impl<B> Hash for PrefixCodedTerms<B>
where
  B: ByteBuffersDataInputBlock + Clone,
{
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    for cursor in &self.content {
      cursor.get_ref().as_slice().hash(state);
    }
    self.size.hash(state);
    self.del_gen.hash(state);
  }
}
impl<B> PartialEq for PrefixCodedTerms<B>
where
  B: ByteBuffersDataInputBlock + Clone,
{
  fn eq(&self, other: &Self) -> bool {
    if std::ptr::eq(self, other) {
      return true;
    }
    self.del_gen == other.del_gen
      && self.size() == other.size()
      && self.content.len() == other.content.len()
      && self
        .content
        .iter()
        .zip(&other.content)
        .all(|(a, b)| a.get_ref().as_slice() == b.get_ref().as_slice())
  }
}

impl<B> Eq for PrefixCodedTerms<B> where B: ByteBuffersDataInputBlock + Clone {}
impl Accountable for PrefixCodedTerms<Vec<u8>> {
  fn ram_bytes_used(&self) -> Result<i64> {
    let mut size = size_of_vec(&self.content);
    for block in &self.content {
      size = size.saturating_add(size_of_vec(block.get_ref()));
    }
    Ok(size)
  }
}

impl Accountable for PrefixCodedTerms<Rc<Vec<u8>>> {
  fn ram_bytes_used(&self) -> Result<i64> {
    let mut size = size_of_vec(&self.content);
    for block in &self.content {
      size = size
        .saturating_add(std::mem::size_of_val(block.get_ref().as_ref()) as i64)
        .saturating_add(size_of_vec(block.get_ref().as_ref()));
    }
    Ok(size)
  }
}

impl Accountable for PrefixCodedTerms<Arc<Vec<u8>>> {
  fn ram_bytes_used(&self) -> Result<i64> {
    let mut size = size_of_vec(&self.content);
    for block in &self.content {
      size = size
        .saturating_add(std::mem::size_of_val(block.get_ref().as_ref()) as i64)
        .saturating_add(size_of_vec(block.get_ref().as_ref()));
    }
    Ok(size)
  }
}

/// Builder for [`PrefixCodedTerms`]: call `add` repeatedly, then `finish`.
pub struct PrefixCodedTermsBuilder {
  output: ByteBuffersDataOutput,
  last_field: String,
  last_term_bytes: BytesRefBuilder<Vec<u8>>,
  size: i64,
}

impl Default for PrefixCodedTermsBuilder {
  fn default() -> Self {
    Self::new()
  }
}

impl PrefixCodedTermsBuilder {
  /// Creates a new instance.
  pub fn new() -> Self {
    Self {
      output: ByteBuffersDataOutput::new_resettable_instance(),
      last_field: String::new(),
      last_term_bytes: BytesRefBuilder::new(),
      size: 0,
    }
  }
  /// add a term.
  pub fn add_term(&mut self, term: &Term) -> Result<()> {
    self.add(term.field.as_str(), &term.bytes)
  }
  /// Add a term. This fully consumes the incoming [`BytesRef`].
  pub fn add<'a, FName, B>(&mut self, field: FName, bytes: &BytesRef<B>) -> Result<()>
  where
    FName: Into<Cow<'a, str>>,
    B: ByteSource,
  {
    let field = field.into();
    let field_name = field.as_ref();
    debug_assert!(
      (self.last_field.is_empty() && self.last_term_bytes.length() == 0)
        || field_name.cmp(&self.last_field) == Ordering::Greater
        || (field_name == self.last_field
          && bytes.bytes.as_slice()[bytes.offset..bytes.offset + bytes.length]
            > self.last_term_bytes.bytes().bytes[..self.last_term_bytes.length()]),
    );

    let prefix;
    let mut new_field = None;
    if self.size > 0 && field_name == self.last_field {
      // Same field as the last term
      prefix = StringHelper::bytes_difference(self.last_term_bytes.bytes(), bytes)?;
      self.output.write_vint((prefix << 1) as i32)?;
    } else {
      // Field change
      let field = field.into_owned();
      prefix = 0;
      self.output.write_vint(1)?;
      self.output.write_string(&field)?;
      new_field = Some(field);
    }

    let suffix = bytes.length - prefix;
    self.output.write_vint(suffix as i32)?;
    let source = bytes.bytes.as_slice();
    self.output.write_bytes_range(
      &source[(bytes.offset + prefix)..(bytes.offset + prefix + suffix)],
      0,
      suffix,
    )?;
    // A size error must preserve the previous term used for prefix encoding.
    self.last_term_bytes.grow_no_copy(bytes.length)?;
    self
      .last_term_bytes
      .copy_bytes_from_vec(source, bytes.offset, bytes.length)?;
    if let Some(field) = new_field {
      self.last_field = field;
    }
    self.size += 1;

    Ok(())
  }
  /// return finalized form.
  pub fn finish(&mut self) -> PrefixCodedTerms {
    let content = self.output.to_buffer_list_owner(false);
    PrefixCodedTerms::new(content.1, content.0, self.size)
  }
}
/// An iterator over the list of terms stored in a [`PrefixCodedTerms`].
pub type TermIteratorRef<'a> = TermIterator<&'a [u8]>;
pub type TermIteratorRc = TermIterator<Rc<Vec<u8>>>;
pub type TermIteratorArc = TermIterator<Arc<Vec<u8>>>;

pub struct TermIterator<B> {
  input: ByteBuffersDataInput<B>,
  pub(crate) builder: BytesRefBuilder<Vec<u8>>,
  end: usize,
  del_gen: i64,
  pub(crate) field: String,
}

impl<B> TermIterator<B>
where
  B: ByteBuffersDataInputBlock,
{
  pub fn new(del_gen: i64, input: ByteBuffersDataInput<B>) -> Self {
    let builder = BytesRefBuilder::new();
    let end = input.length();
    Self {
      input,
      builder,
      end,
      del_gen,
      field: "".to_string(),
    }
  }
  pub fn read_term_bytes(&mut self, prefix: usize, suffix: usize) -> Result<()> {
    let len = prefix + suffix;
    self.builder.grow(len)?;
    self.builder.bytes_mut().bytes.access_mut(|bytes| {
      DataInput::read_bytes(&mut self.input, bytes, prefix, suffix)?;
      // Help the compiler infer types.
      Ok::<(), LuceneError>(())
    })?;

    self.builder.set_length(len);
    Ok(())
  }
}

impl<B> BytesRefIterator for TermIterator<B>
where
  B: ByteBuffersDataInputBlock,
{
  type Value<'a>
    = Cow<'a, BytesRef<Vec<u8>>>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
    let v = self.set_next()?;
    if v {
      Ok(Some(Cow::Borrowed(self.builder.bytes())))
    } else {
      Ok(None)
    }
  }

  fn set_next(&mut self) -> Result<bool> {
    if self.input.position()? < self.end {
      let code = self.input.read_vint()?;
      let new_field = (code & 1) != 0;
      if new_field {
        match self.input.read_string()? {
          Cow::Borrowed(field) => field.clone_into(&mut self.field),
          Cow::Owned(field) => self.field = field,
        }
      }
      let prefix = (code >> 1) as usize;
      let suffix = self.input.read_vint()? as usize;
      self.read_term_bytes(prefix, suffix)?;
      return Ok(true);
    } else {
      self.field.clear();
    }
    Ok(false)
  }
}

impl<B> FieldTermIterator for TermIterator<B>
where
  B: ByteBuffersDataInputBlock,
{
  /// Returns current field. This method should not be called after iteration
  /// is done. Note that you may use == to detect a change in field.
  fn field(&self) -> &str {
    &self.field
  }

  /// Del gen of the current term
  fn del_gen(&self) -> i64 {
    self.del_gen
  }
}
