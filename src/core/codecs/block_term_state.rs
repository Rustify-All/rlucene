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
use std::fmt::{Display, Formatter};

use crate::core::codecs::lucene101::lucene101_postings_format::IntBlockTermState;
use crate::core::index::ord_term_state::OrdTermState;
use crate::core::index::term_state::TermState;
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::lucene_error::Result;

/// Holds all state required for
/// [`PostingsReaderBase`](crate::core::codecs::postings_reader_base::PostingsReaderBase)
/// to produce a [`PostingsEnum`](crate::core::index::postings_enum::PostingsEnum)
/// without re-seeking the terms dict.
#[derive(Default, Clone)]
pub struct BlockTermState {
    /// how many docs have this term
    pub doc_freq: i32,
    /// total number of occurrences of this term
    pub total_term_freq: i64,
    /// the term's ord in the current block
    pub term_block_ord: i32,
    /// fp into the terms dict primary file (_X.tim) that holds this term
    // TODO: update BTR to nuke this
    pub block_file_pointer: i64,
    ord: OrdTermState,
}

impl Display for BlockTermState {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} docFreq={} totalTermFreq={} termBlockOrd={} blockFP={}",
            self.ord,
            self.doc_freq,
            self.total_term_freq,
            self.term_block_ord,
            self.block_file_pointer
        )
    }
}

impl TermState for BlockTermState {
    fn copy_from(&mut self, other: &Self) -> Result<()> {
        self.doc_freq = other.doc_freq;
        self.total_term_freq = other.total_term_freq;
        self.term_block_ord = other.term_block_ord;
        self.block_file_pointer = other.block_file_pointer;
        self.ord = other.ord.clone();
        Ok(())
    }
}

#[derive(Clone)]
pub enum BlockTermStateEnum {
    Int(IntBlockTermState),
    Block(BlockTermState),
}
impl BlockTermStateEnum {
    pub fn get_block_term_state(&mut self) -> &mut BlockTermState {
        match self {
            BlockTermStateEnum::Int(int) => &mut int.base,
            BlockTermStateEnum::Block(block) => block,
        }
    }
}

impl Display for BlockTermStateEnum {
    fn fmt(&self, _f: &mut Formatter<'_>) -> std::fmt::Result {
        todo!()
    }
}

impl TermState for BlockTermStateEnum {
    fn copy_from(&mut self, other: &Self) -> Result<()> {
        match (self, other) {
            (BlockTermStateEnum::Int(int), BlockTermStateEnum::Int(o)) => int.copy_from(o),
            (BlockTermStateEnum::Block(block), BlockTermStateEnum::Block(o)) => block.copy_from(o),
            _ => Err(LuceneError::illegal_state(
                "TermState variants must match when copying",
            )),
        }
    }
}
