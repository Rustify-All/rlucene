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
use crate::core::index::dummy::dummy_index_reader::DummyIndexReader;
use crate::core::index::dummy::dummy_leaf_reader::DummyLeafReader;
use crate::core::index::index_reader_context::{
    IndexReaderContext, IndexReaderContextBase, IndexReaderContextEnum, IndexReaderContextSealed,
};
use crate::core::index::leaf_reader_context::LeafReaderContext;

pub struct DummyIndexReaderContext;

impl IndexReaderContextSealed for DummyIndexReaderContext {}

impl IndexReaderContext for DummyIndexReaderContext {
    type IndexReader = DummyIndexReader;

    fn reader(&self) -> &Self::IndexReader {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    type LeafReader = DummyLeafReader;

    fn leaves(
        &self,
    ) -> crate::core::util::error::lucene_error::Result<&[LeafReaderContext<Self::LeafReader>]>
    {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn children(&self) -> Option<&[IndexReaderContextEnum<Self::LeafReader>]> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn base(&self) -> &IndexReaderContextBase<Self::LeafReader> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn base_mut(&mut self) -> &mut IndexReaderContextBase<Self::LeafReader> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }
}
