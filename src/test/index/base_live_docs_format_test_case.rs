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
use std::collections::HashMap;

use rand::Rng;

use crate::core::codecs::live_docs_format::LiveDocsFormat;
use crate::core::codecs::{Codec, LATEST_CODEC};
use crate::core::index::index_writer::MAX_DOCS;
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::store::IOContext;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::{LATEST, StringHelper};
use crate::test::util::lucene_test_case::lucene_test_case_util::new_directory_shared;
use crate::test::util::test_util::TestUtil;

pub trait BaseLiveDocsFormatTestCase {
    fn test_dense_live_docs<R: Rng + ?Sized>(&self, random: &mut R) -> Result<()> {
        let max_doc = TestUtil::next_int(random, 3, 1000);
        Self::test_serialization(random, max_doc, max_doc - 1, false)?;
        Self::test_serialization(random, max_doc, max_doc - 1, true)?;
        Ok(())
    }
    fn test_empty_live_docs<R: Rng + ?Sized>(&self, random: &mut R) -> Result<()> {
        let max_doc = TestUtil::next_int(random, 3, 1000);
        Self::test_serialization(random, max_doc, 0, false)?;
        Self::test_serialization(random, max_doc, 0, true)?;

        Ok(())
    }
    fn test_sparse_live_docs<R: Rng + ?Sized>(&self, random: &mut R) -> Result<()> {
        let max_doc = TestUtil::next_int(random, 3, 1000);
        Self::test_serialization(random, max_doc, 1, false)?;
        Self::test_serialization(random, max_doc, 1, true)?;
        Ok(())
    }
    fn test_over_flow<R: Rng + ?Sized>(&self, random: &mut R) -> Result<()> {
        Self::test_serialization(random, MAX_DOCS, MAX_DOCS - 7, true)?;
        Ok(())
    }

    fn test_serialization<R: Rng + ?Sized>(
        random: &mut R,
        max_doc: i32,
        num_live_docs: i32,
        fixed_bit_set: bool,
    ) -> Result<()> {
        let format = LATEST_CODEC.live_docs_format();
        let mut live_docs = FixedBitSet::new(max_doc as usize);
        if num_live_docs > max_doc / 2 {
            live_docs.set_with_range(0, max_doc as usize);
            for _ in 0..(max_doc - num_live_docs) {
                let mut clear_bit;
                loop {
                    clear_bit = random.random_range(0..max_doc);
                    if live_docs.get(clear_bit as usize)? {
                        break;
                    }
                }
                live_docs.clear_with_index(clear_bit as usize);
            }
        } else {
            for _ in 0..num_live_docs {
                let mut set_bit;
                loop {
                    set_bit = random.random_range(0..max_doc);
                    if !live_docs.get(set_bit as usize)? {
                        break;
                    }
                }
                live_docs.set(set_bit as usize);
            }
        }
        let bits = if fixed_bit_set {
            TestBitsEnum::Fixed(live_docs)
        } else {
            TestBitsEnum::Test(TestBits::new(live_docs))
        };
        let dir = new_directory_shared(random)?;
        let si = SegmentInfo::new(
            dir.clone(),
            Option::from(LATEST.clone()),
            Option::from(LATEST.clone()),
            "foo",
            max_doc,
            rand::random(),
            false,
            HashMap::new(),
            StringHelper::random_id(),
            HashMap::new(),
            None,
        )?;
        let io_context = IOContext::default_io_context()?;
        let si1 = si.clone();
        let mut sci =
            SegmentCommitInfo::new(si, 0, 0, 0, -1, -1, Option::from(StringHelper::random_id()))?;
        format.write_live_docs(
            &bits,
            dir.as_ref(),
            &sci,
            max_doc - num_live_docs,
            &io_context,
        )?;

        sci = SegmentCommitInfo::new(
            si1,
            max_doc - num_live_docs,
            0,
            1,
            -1,
            -1,
            Option::from(StringHelper::random_id()),
        )?;
        let io_context = IOContext::read_once_io_context()?;
        let dir = dir;
        let bits2 = format.read_live_docs(dir.as_ref(), &sci, &io_context)?;

        assert_eq!(max_doc as usize, bits2.length());
        for i in 0..max_doc as usize {
            assert_eq!(bits.get(i)?, bits2.get(i)?);
        }
        Ok(())
    }
}

pub struct TestBits {
    live_docs: FixedBitSet,
}
impl TestBits {
    pub fn new(live_docs: FixedBitSet) -> Self {
        TestBits { live_docs }
    }
}
impl Bits for TestBits {
    fn get(&self, index: usize) -> Result<bool> {
        self.live_docs.get(index)
    }

    fn length(&self) -> usize {
        self.live_docs.length()
    }
}

enum TestBitsEnum {
    Test(TestBits),
    Fixed(FixedBitSet),
}
impl Bits for TestBitsEnum {
    fn get(&self, index: usize) -> Result<bool> {
        match self {
            TestBitsEnum::Test(test) => test.get(index),
            TestBitsEnum::Fixed(fixed) => fixed.get(index),
        }
    }
    fn length(&self) -> usize {
        match self {
            TestBitsEnum::Test(test) => test.length(),
            TestBitsEnum::Fixed(fixed) => fixed.length(),
        }
    }
}
