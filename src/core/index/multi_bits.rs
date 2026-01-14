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
use crate::core::index::composite_reader::{CompositeReader, CompositeReaderBits, get_context};
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::reader_util::ReaderUtil;
use crate::core::util::TryIntoInt;
use crate::core::util::bits::{Bits, BitsEnum2};
use crate::core::util::error::lucene_error::Result;
use std::fmt::{Display, Formatter};

/// Concatenates multiple `Bits` together on every lookup.
///
/// **NOTE:** This is very costly, as every lookup must perform a binary search
/// to locate the correct sub-reader.
pub struct MultiBits<B>
where
    B: Bits,
{
    subs: Vec<Option<B>>,
    starts: Vec<usize>,
    default_value: bool,
}

impl<B> MultiBits<B>
where
    B: Bits,
{
    pub fn new(subs: Vec<Option<B>>, starts: Vec<usize>, default_value: bool) -> Self {
        debug_assert_eq!(starts.len(), subs.len() + 1);
        Self {
            subs,
            starts,
            default_value,
        }
    }
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
                        bits.as_string()
                    )?;
                },
            }
        }
        write!(f, " end={}", self.starts[self.subs.len()])
    }
}
/// Returns a single `Bits` instance for this reader, merging live documents on the fly.
/// This method will return `None` if the reader has no deletions.
///
/// **NOTE:** this is a very slow way to access live docs.
/// For example, each `Bits` access will require a binary search.
/// It's better to get the sub-readers and iterate through them yourself.
pub fn get_live_docs<CR>(reader: CR) -> Result<Option<BitsType<CR>>>
where
    CR: CompositeReader,
{
    if !reader.has_deletions()? {
        return Ok(None);
    }
    let max_doc = reader.max_doc()?;
    let ctx = get_context(reader)?;
    let leaves = ctx.leaves()?;
    let size = leaves.len();
    debug_assert!(
        size > 0,
        "A reader with deletions must have at least one leave"
    );

    if size == 1 {
        return match leaves[0].reader().get_live_docs()? {
            Some(bits) => Ok(Some(BitsEnum2::A(bits))),
            None => Ok(None),
        };
    }

    let mut live_docs = Vec::with_capacity(size);
    let mut starts: Vec<usize> = Vec::with_capacity(size + 1);

    for ctx in leaves {
        // record all liveDocs, even if they are null
        live_docs.push(ctx.reader().get_live_docs()?);
        starts.push(ctx.doc_base);
    }

    starts.push(max_doc.try_convert()?);

    Ok(Some(BitsType::<CR>::B(MultiBits::new(
        live_docs, starts, true,
    ))))
}
pub type BitsType<CR> = BitsEnum2<CompositeReaderBits<CR>, MultiBits<CompositeReaderBits<CR>>>;
