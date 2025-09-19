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
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;

pub struct IndexSearcher<IRC, LR>
where
    IRC: IndexReaderContext<LR>,
    LR: LeafReader,
{
    reader_context: IRC,
    leaf_contexts: Vec<LeafReaderContext<LR>>,
}

impl<IRC, LR> IndexSearcher<IRC, LR>
where
    IRC: IndexReaderContext<LR>,
    LR: LeafReader,
{
    pub fn stored_fields(&self) {}

    pub fn get_top_reader_context(&self) -> &IRC {
        &self.reader_context
    }
}
