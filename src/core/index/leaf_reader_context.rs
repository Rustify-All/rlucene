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
use crate::core::index::composite_reader_context::CompositeReaderContext;
use crate::core::index::index_reader_context::{
    IndexReaderContext, IndexReaderContextBase, IndexReaderContextEnum, IndexReaderContextSealed,
};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::fmt;

/// [`IndexReaderContext`] for [`LeafReader`] instances.
pub struct LeafReaderContext<LR> {
    /// The reader's ord in the top-level's leaves array
    pub(crate) ord: usize,
    /// The reader's absolute doc base
    doc_base: i32,
    reader: LR,
    base: IndexReaderContextBase<LR>,
}
impl<LR> LeafReaderContext<LR>
where
    LR: LeafReader,
{
    pub fn new(
        parent: Option<CompositeReaderContext<LR>>,
        reader: LR,
        ord: i32,
        doc_base: i32,
        leaf_ord: usize,
        leaf_doc_base: i32,
    ) -> Self {
        Self {
            ord: leaf_ord,
            doc_base: leaf_doc_base,
            reader,
            base: IndexReaderContextBase::new(parent, ord, doc_base),
        }
    }

    pub fn new_single(reader: LR) -> Self {
        Self::new(None, reader, 0, 0, 0, 0)
    }
}
impl<LR> IndexReaderContextSealed for LeafReaderContext<LR> where LR: LeafReader {}

impl<LR> IndexReaderContext<LR> for LeafReaderContext<LR>
where
    LR: LeafReader,
{
    type IndexReader = LR;

    fn reader(&self) -> &Self::IndexReader {
        &self.reader
    }

    fn leaves(&self) -> Result<&[LeafReaderContext<LR>]> {
        if !self.base.is_top_level {
            return Err(LuceneError::unsupported_operation(
                "This is not a top-level context".to_string(),
            ));
        }
        Ok(std::slice::from_ref(self))
    }

    fn children(&self) -> Option<&[IndexReaderContextEnum<LR>]> {
        None
    }

    fn base(&self) -> &IndexReaderContextBase<LR> {
        &self.base
    }

    fn base_mut(&mut self) -> &mut IndexReaderContextBase<LR> {
        &mut self.base
    }
}
impl<LR> fmt::Display for LeafReaderContext<LR>
where
    LR: fmt::Display,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "LeafReaderContext({} docBase={} ord={})",
            self.reader, self.doc_base, self.ord
        )
    }
}
