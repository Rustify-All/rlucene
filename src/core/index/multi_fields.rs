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
use crate::core::index::fields::Fields;
use crate::core::index::multi_terms::MultiTerms;
use crate::core::index::reader_slice::ReaderSlice;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::merged_iterator::MergedIterator;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// Provides a single [`Fields`] term index view over an [`IndexReader`](crate::core::index::index_reader::IndexReader).
///
/// This is useful when interacting with an [`IndexReader`](crate::core::index::index_reader::IndexReader) implementation that consists of
/// sequential sub-readers (for example, [`DirectoryReader`](crate::core::index::directory_reader::DirectoryReader) or [`MultiReader`](crate::core::index::multi_reader::MultiReader)) and you must treat it
/// as a [`LeafReader`](crate::core::index::leaf_reader::LeafReader).
///
/// **NOTE**: For composite readers, you will generally get better performance by gathering the
/// sub-readers via `IndexReader::get_context()` to obtain the atomic leaves and then operating
/// per-[`LeafReader`](crate::core::index::leaf_reader::LeafReader), instead of using this type.
pub struct MultiFields<F>
where
  F: Fields,
{
  pub(crate) subs: Box<[F]>,
  sub_slices: Box<[ReaderSlice]>,
  terms: RefCell<HashMap<String, Rc<TermsType<F>>>>,
}
pub type TermsType<F> = MultiTerms<<F as Fields>::Terms>;
impl<F> MultiFields<F>
where
  F: Fields,
{
  /// Creates a new instance.
  pub fn new(subs: Vec<F>, sub_slices: Vec<ReaderSlice>) -> Self {
    Self {
      subs: subs.into_boxed_slice(),
      sub_slices: sub_slices.into_boxed_slice(),
      terms: RefCell::new(HashMap::new()),
    }
  }
}
pub type MultiFieldsTerms<T> = Rc<MultiTerms<T>>;
impl<F> Fields for MultiFields<F>
where
  F: Fields,
{
  type FieldIter<'a>
    = MergedIterator<<F as Fields>::FieldIter<'a>>
  where
    Self: 'a;

  fn iterator(&self) -> Result<Self::FieldIter<'_>> {
    let mut sub_iterators = Vec::with_capacity(self.subs.len());
    for sub in &self.subs {
      sub_iterators.push(sub.iterator()?);
    }
    MergedIterator::new(sub_iterators)
  }

  type Terms = MultiFieldsTerms<<F as Fields>::Terms>;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    if let Some(v) = self.terms.borrow().get(field) {
      return Ok(Some(v.clone()));
    }

    // Lazy init: first time this field is requested
    let mut subs2 = Vec::with_capacity(self.subs.len());
    let mut slices2 = Vec::with_capacity(self.subs.len());
    // Gather all sub-readers that share this field
    for i in 0..self.subs.len() {
      if let Some(terms) = self.subs[i].terms(field)? {
        subs2.push(terms);
        slices2.push(self.sub_slices[i]);
      }
    }

    if !subs2.is_empty() {
      let result = Rc::new(MultiTerms::new(subs2, slices2)?);
      self
        .terms
        .borrow_mut()
        .insert(field.to_string(), result.clone());
      Ok(Some(result))
    } else {
      Ok(None)
    }
  }

  fn size(&self) -> Result<i32> {
    Ok(-1)
  }
}
