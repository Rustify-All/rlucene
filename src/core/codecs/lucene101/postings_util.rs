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
use crate::core::store::{DataOutput, IndexInput, write_group_vints_i32};
use crate::core::util::error::lucene_error::Result;
use crate::core::util::group_vint_util::GroupVIntUtil;
/// Utility struct to encode/decode postings block.
pub(crate) struct PostingsUtil;

impl PostingsUtil {
    /// Read values that have been written using variable-length encoding and
    /// group-varint encoding instead of bit-packing.
    pub(crate) fn read_vint_block(
        doc_in: &mut impl IndexInput,
        doc_buffer: &mut [i32],
        freq_buffer: &mut [i32],
        num: usize,
        index_has_freq: bool,
        decode_freq: bool,
    ) -> Result<()> {
        GroupVIntUtil::read_group_vints_i32(doc_in, doc_buffer, num)?;
        if index_has_freq && decode_freq {
            for i in 0..num {
                freq_buffer[i] = doc_buffer[i] & 0x01;
                doc_buffer[i] = ((doc_buffer[i] as u32) >> 1) as i32;
                if freq_buffer[i] == 0 {
                    freq_buffer[i] = doc_in.read_vint()?;
                }
            }
        } else if index_has_freq {
            for val in doc_buffer.iter_mut().take(num) {
                *val = ((*val as u32) >> 1) as i32;
            }
        }
        Ok(())
    }
    /// Write freq buffer with variable-length encoding and doc buffer with
    /// group-varint encoding.
    pub(crate) fn write_vint_block(
        doc_out: &mut impl DataOutput,
        doc_buffer: &mut [i32],
        freq_buffer: &[i32],
        num: i32,
        write_freqs: bool,
    ) -> Result<()> {
        if write_freqs {
            for i in 0..num as usize {
                doc_buffer[i] = (doc_buffer[i] << 1) | if freq_buffer[i] == 1 { 1 } else { 0 };
            }
        }
        write_group_vints_i32(doc_out, doc_buffer, num)?;
        let num = num as usize;

        if write_freqs {
            for &freq in freq_buffer.iter().take(num) {
                if freq != 1 {
                    doc_out.write_vint(freq)?;
                }
            }
        }

        Ok(())
    }
}
#[cfg(test)]
mod tests {

    use rand::Rng;

    use crate::core::codecs::lucene101::for_util::ForUtil;
    use crate::core::codecs::lucene101::postings_util::PostingsUtil;
    use crate::core::store::IOContext;
    use crate::core::store::directory::Directory;
    use crate::core::util::error::lucene_error::Result;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{
        new_directory_shared, random,
    };

    // checks for bug described in https://github.com/apache/lucene/issues/13373
    #[allow(dead_code)] // for quick search
    struct TestPostingsUtil;
    #[test]
    fn test_integer_overflow() -> Result<()> {
        let mut random = random();
        let random_size1: usize = random.random_range(1..3);
        let random_size2: usize = random.random_range(4..=ForUtil::BLOCK_SIZE);
        do_test_integer_overflow(&mut random, random_size1)?;
        do_test_integer_overflow(&mut random, random_size2)?;
        Ok(())
    }
    fn do_test_integer_overflow<R: Rng + ?Sized>(random: &mut R, size: usize) -> Result<()> {
        let mut doc_delta_buffer = vec![0i32; size];
        let freq_buffer = vec![0i32; size];

        let delta = 1 << 30;
        doc_delta_buffer[0] = delta;

        // TODO: ByteBuffersDirectory not Implemented
        let dir = new_directory_shared(random)?;
        {
            let mut out = dir.create_output("test", &IOContext::default_io_context()?)?;
            PostingsUtil::write_vint_block(
                &mut out,
                &mut doc_delta_buffer,
                &freq_buffer,
                size as i32,
                true,
            )?;
        }

        let mut restored_docs = vec![0i32; size];
        let mut restored_freqs = vec![0i32; size];

        {
            let mut input = dir.open_input("test", &IOContext::default_io_context()?)?;
            PostingsUtil::read_vint_block(
                &mut input,
                &mut restored_docs,
                &mut restored_freqs,
                size,
                true,
                true,
            )?;
        }

        assert_eq!(delta, restored_docs[0]);
        Ok(())
    }
}
