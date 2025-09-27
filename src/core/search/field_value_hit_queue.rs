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
use crate::core::index::leaf_reader::LeafReader;
use crate::core::search::leaf_field_comparator::LeafFieldComparatorEnum;
use crate::core::search::score_doc::{ScoreDoc, ScoreDocLike};
use crate::core::search::sort_field::SortField;
use std::fmt;

pub struct FieldValueHitQueue<LR>
where
    LR: LeafReader,
{
    pub(crate) fields: Vec<SortField>,
    comparators: Vec<LeafFieldComparatorEnum<LR>>,
    reverse_mul: Vec<i32>,
}

pub struct Entry {
    pub base: ScoreDoc,
    pub slot: i32,
}
impl Entry {
    pub fn new(slot: i32, doc: i32) -> Self {
        let base = ScoreDoc::new(doc, f32::NAN);
        Self { base, slot }
    }
}
impl ScoreDocLike for Entry {
    fn doc(&self) -> i32 {
        self.base.doc
    }

    fn score(&self) -> f32 {
        self.base.score
    }

    fn shard_index(&self) -> i32 {
        self.base.shard_index
    }
}
impl fmt::Display for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "slot:{} {}", self.slot, self.base)
    }
}
