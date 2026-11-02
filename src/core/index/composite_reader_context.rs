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
use crate::core::index::composite_reader::CompositeReader;
use crate::core::index::index_reader::Identity;
use crate::core::index::index_reader_context::{IndexReaderContext, IndexReaderContextBase};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::{LeafReaderContext, TopParentMeta};
use crate::core::util::TryIntoInt;
use crate::core::util::error::lucene_error::Result;

/// [`IndexReaderContext`] for CompositeReader instance.
pub struct CompositeReaderContext<CR>
where
  CR: CompositeReader,
{
  leaves: Box<[LeafReaderContext<CR::LeafReader>]>,
  reader: CR,
  base: IndexReaderContextBase,
}
pub(crate) fn create<CR>(reader: CR) -> Result<CompositeReaderContext<CR>>
where
  CR: CompositeReader,
{
  let base = IndexReaderContextBase::new(true, 0, 0);
  let mut builder = Builder::new();
  reader.visit_leaves(&mut |leaf_reader| builder.add_leaf(leaf_reader, base.id()))?;
  let max_doc = builder.max_doc;
  let leaves = builder.leaves;
  let mut ctx = CompositeReaderContext {
    leaves: leaves.into_boxed_slice(),
    reader,
    base,
  };
  let leaves_num = ctx.leaves.len();
  ctx.leaves.iter_mut().for_each(|leaf| {
    // add_leaf already retained this context's identity for every leaf.
    leaf.top_parent.leaves_num = leaves_num;
    leaf.top_parent.max_doc = max_doc;
  });
  Ok(ctx)
}

impl<CR> IndexReaderContext for CompositeReaderContext<CR>
where
  CR: CompositeReader,
{
  type IndexReader = CR;

  fn reader(&self) -> &Self::IndexReader {
    &self.reader
  }

  type LeafReader = CR::LeafReader;

  fn leaves(&self) -> Result<&[LeafReaderContext<Self::LeafReader>]> {
    Ok(self.leaves.as_ref())
  }

  fn base(&self) -> &IndexReaderContextBase {
    &self.base
  }
}

struct Builder<LR> {
  pub(crate) leaves: Vec<LeafReaderContext<LR>>,
  pub(crate) leaf_doc_base: usize,
  pub(crate) max_doc: i32,
}
impl<LR> Builder<LR> {
  fn new() -> Self {
    Self {
      leaves: Vec::new(),
      leaf_doc_base: 0,
      max_doc: 0,
    }
  }
}
impl<LR> Builder<LR>
where
  LR: LeafReader + Clone,
{
  fn add_leaf(&mut self, reader: &LR, parent_id: &Identity) -> Result<()> {
    let leaves_size = self.leaves.len();
    let atomic = LeafReaderContext::new(
      reader.clone(),
      0,
      self.leaf_doc_base,
      leaves_size,
      self.leaf_doc_base,
      TopParentMeta {
        leaves_num: 0,
        max_doc: 0,
        id: parent_id.clone(),
      },
    );
    self.leaves.push(atomic);
    let max_doc = reader.max_doc()?;
    self.leaf_doc_base += max_doc.try_convert()?;
    self.max_doc += max_doc;
    Ok(())
  }
}
