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
use crate::core::index::BytesRef;
use crate::core::store::{DataInput, DataOutput};
use crate::core::util::error::lucene_error::{LuceneError, Result};

/// Utility that efficiently compresses arrays mostly containing characters in
/// the `[0x1F, 0x3F)` or `[0x5F, 0x7F)` ranges,
/// which notably include all digits, lowercase letters, `.`, `-`, and `_`.
pub struct LowercaseAsciiCompression;
impl LowercaseAsciiCompression {
    fn is_compressible(b: i32) -> bool {
        let high3_bits = (b.wrapping_add(1)) & !0x1F;
        high3_bits == 0x20 || high3_bits == 0x60
    }
    /// Compresses `input[0..len]` into `out`.
    ///
    /// Returns `false` if the content cannot be compressed.
    /// If compression succeeds, the number of bytes written is guaranteed to be
    /// less than `len`.
    pub fn compress(
        input: &[u8],
        len: usize,
        tmp: &mut [u8],
        out: &mut impl DataOutput,
    ) -> Result<bool> {
        if len < 8 {
            return Ok(false);
        }

        // 1. Count exceptions and fail compression if there are too many of them.
        let max_exceptions = len >> 5;
        let mut previous_exception_index = 0;
        let mut num_exceptions = 0;

        for (i, &b) in input.iter().take(len).enumerate() {
            let b = b as i32;
            if !Self::is_compressible(b) {
                while i - previous_exception_index > 0xFF {
                    num_exceptions += 1;
                    previous_exception_index += 0xFF;
                }
                num_exceptions += 1;
                if num_exceptions > max_exceptions {
                    return Ok(false);
                }
                previous_exception_index = i;
            }
        }

        debug_assert!(num_exceptions <= max_exceptions);

        // 2. Move to 6-bit space
        let compressed_len = len - (len >> 2);
        debug_assert!(compressed_len < len);
        for i in 0..len {
            let b = (input[i] as i32) + 1;
            tmp[i] = ((b & 0x1F) | ((b as u32 & 0x40) >> 1) as i32) as u8;
        }

        // 3. Pack exception bits into tmp[0..compressed_len]
        let mut o = 0usize;
        for i in compressed_len..len {
            tmp[o] |= (tmp[i] & 0x30) << 2; // bits 4-5
            o += 1;
        }
        for i in compressed_len..len {
            tmp[o] |= (tmp[i] & 0x0C) << 4; // bits 2-3
            o += 1;
        }
        for i in compressed_len..len {
            tmp[o] |= (tmp[i] & 0x03) << 6; // bits 0-1
            o += 1;
        }

        debug_assert!(o <= compressed_len);
        debug_assert!(compressed_len <= i32::MAX as usize);
        out.write_bytes_range(tmp, 0, compressed_len as i32)?;

        // 4. Write exception deltas
        debug_assert!(num_exceptions <= i32::MAX as usize);
        out.write_vint(num_exceptions as i32)?;
        if num_exceptions > 0 {
            previous_exception_index = 0;
            let mut num_exceptions2 = 0;

            for i in 0..len {
                let b = input[i] as i32;
                if !Self::is_compressible(b) {
                    while i - previous_exception_index > 0xFF {
                        // We record deltas between exceptions as bytes, so we need to create
                        // "artificial" exceptions if the delta between two of them is greater
                        // than the maximum unsigned byte value.
                        out.write_byte(0xFF)?;
                        previous_exception_index += 0xFF;
                        out.write_byte(input[previous_exception_index])?;
                        num_exceptions2 += 1;
                    }

                    out.write_byte((i - previous_exception_index) as u8)?;
                    previous_exception_index = i;
                    out.write_byte(input[i])?;
                    num_exceptions2 += 1;
                }
            }

            if num_exceptions != num_exceptions2 {
                return Err(LuceneError::illegal_state(format!(
                    "{} <> {} {}",
                    num_exceptions,
                    num_exceptions2,
                    BytesRef::from_slice(input.to_vec(), 0, len),
                )));
            }
        }

        Ok(true)
    }
    /// Decompresses data that was previously compressed using
    /// [`Self::compress`].
    ///
    /// `len` must be the original (uncompressed) length, not the compressed
    /// length.
    pub fn decompress(input: &mut impl DataInput, out: &mut [u8], len: usize) -> Result<()> {
        let saved = len >> 2;
        let compressed_len = len - saved;

        // 1. Copy the packed bytes
        debug_assert!(compressed_len <= i32::MAX as usize);
        input.read_bytes(out, 0, compressed_len as i32)?;

        // 2. Restore the leading 2 bits into whole bytes
        for i in 0..saved {
            let a = (out[i] as u32 & 0xC0) >> 2;
            let b = (out[saved + i] as u32 & 0xC0) >> 4;
            let c = (out[(saved << 1) + i] as u32 & 0xC0) >> 6;
            out[compressed_len + i] = (a | b | c) as u8;
        }

        // 3. Move back to original range
        for b in out.iter_mut().take(len) {
            *b = (((*b as u32 & 0x1F) | 0x20 | ((*b as u32 & 0x20) << 1)) - 1) as u8;
        }

        // 4. Restore exceptions
        let num_exceptions = input.read_vint()? as usize;
        let mut i = 0usize;

        for _ in 0..num_exceptions {
            i += input.read_byte()? as usize;
            out[i] = input.read_byte()?;
        }

        Ok(())
    }
}
#[cfg(test)]
mod tests {

    use rand::Rng;

    use crate::core::store::ByteBuffersDataOutput;
    use crate::core::util::compress::lowercase_ascii_compression::LowercaseAsciiCompression;
    use crate::core::util::error::lucene_error::Result;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{at_least, random};
    use crate::test::util::test_util::TestUtil;
    fn do_test_compress<R: Rng + ?Sized>(random: &mut R, bytes: &[u8]) -> Result<bool> {
        do_test_compress_with_len(random, bytes, bytes.len())
    }

    fn do_test_compress_with_len<R: Rng + ?Sized>(
        random: &mut R,
        bytes: &[u8],
        len: usize,
    ) -> Result<bool> {
        let mut compressed = ByteBuffersDataOutput::new();
        let mut tmp = vec![0u8; len + random.random_range(0..10)];
        random.fill(&mut tmp[..]);

        if LowercaseAsciiCompression::compress(bytes, len, &mut tmp, &mut compressed)? {
            assert!(compressed.size() < len as i64);

            let mut restored = vec![0u8; len + random.random_range(0..10)];
            let mut input = compressed.get_data_input_ref();
            LowercaseAsciiCompression::decompress(&mut input, &mut restored, len)?;

            assert_eq!(&restored[..len], &bytes[..len]);
            Ok(true)
        } else {
            Ok(false)
        }
    }
    #[test]
    fn test_simple() -> Result<()> {
        let mut random = random();

        assert!(!do_test_compress(&mut random, b"")?); // too short
        assert!(!do_test_compress(&mut random, b"ab1")?); // too short
        assert!(!do_test_compress(&mut random, b"ab1cdef")?); // too short
        assert!(do_test_compress(&mut random, b"ab1cdefg")?);
        assert!(!do_test_compress(&mut random, b"ab1cdEfg")?); // too many exceptions
        assert!(do_test_compress(&mut random, b"ab1cdefg")?);

        // 1 exception, but enough chars to be worth encoding an exception
        assert!(do_test_compress(
            &mut random,
            b"ab1.dEfg427hiogchio:'nwm un!94twxz"
        )?);

        Ok(())
    }
    #[test]
    fn test_not_really_simple() -> Result<()> {
        let mut random = random();
        let input = b"cion1cion_desarrollociones_oraclecionesnaturacionesnatura2tedppsa-integrationdemotiontion cloud gen2tion instance - dev1tion instance - testtion-devbtion-instancetion-prdtion-promerication-qation064533tion535217tion697401tion761348tion892818tion_matrationcauto_simmonsintgic_testtioncloudprodictioncloudservicetiongateway10tioninstance-jtsundatamartprd??o";
        do_test_compress(&mut random, input)?;
        Ok(())
    }
    #[test]
    fn test_not_really_simple2() -> Result<()> {
        let mut random = random();
        let input = b"analytics-platform-test/koala/cluster-tool:1.0-20220310151438.492,mesh_istio_examples-bookinfo-details-v1:1.16.2mesh_istio_examples-bookinfo-reviews-v3:1.16.2oce-clamav:1.0.219oce-tesseract:1.0.7oce-traefik:2.5.1oci-opensearch:1.2.4.8.103oda-digital-assistant-control-plane-train-pool-workflow-v6:22.02.14oke-coresvcs-k8s-dns-dnsmasq-nanny-amd64@sha256:41aa9160ceeaf712369ddb660d02e5ec06d1679965e6930351967c8cf5ed62d4oke-coresvcs-k8s-dns-kube-dns-amd64@sha256:2cf34b04106974952996c6ef1313f165ce65b4ad68a3051f51b1b8f91ba5f838oke-coresvcs-k8s-dns-sidecar-amd64@sha256:8a82c7288725cb4de9c7cd8d5a78279208e379f35751539b406077f9a3163dcdoke-coresvcs-node-problem-detector@sha256:9d54df11804a862c54276648702a45a6a0027a9d930a86becd69c34cc84bf510oke-coresvcs-oke-fluentd-lumberjack@sha256:5f3f10b187eb804ce4e84bc3672de1cf318c0f793f00dac01cd7da8beea8f269oke-etcd-operator@sha256:4353a2e5ef02bb0f6b046a8d6219b1af359a2c1141c358ff110e395f29d0bfc8oke-oke-hyperkube-amd64@sha256:3c734f46099400507f938090eb9a874338fa25cde425ac9409df4c885759752foke-public-busybox@sha256:4cee1979ba0bf7db9fc5d28fb7b798ca69ae95a47c5fecf46327720df4ff352doke-public-coredns@sha256:86f8cfc74497f04e181ab2e1d26d2fd8bd46c4b33ce24b55620efcdfcb214670oke-public-coredns@sha256:8cd974302f1f6108f6f31312f8181ae723b514e2022089cdcc3db10666c49228oke-public-etcd@sha256:b751e459bc2a8f079f6730dd8462671b253c7c8b0d0eb47c67888d5091c6bb77oke-public-etcd@sha256:d6a76200a6e9103681bc2cf7fefbcada0dd9372d52cf8964178d846b89959d14oke-public-etcd@sha256:fa056479342b45479ac74c58176ddad43687d5fc295375d705808f9dfb48439aoke-public-kube-proxy@sha256:93b2da69d03413671606e22294c59a69fe404088a5f6e74d6394a8641fdb899boke-public-tiller@sha256:c2eb6e580123622e1bc0ff3becae3a3a71ac36c98a2786d780590197839175e5osms/opcbuild-osms-agent-proxy-java:0.4.0-129rosms/opcbuild-osms-agent-proxy-nginx:0.4.0-129rosms/opcbuild-osms-ingestion-cert:0.4.0-129rscs-lcm/drift-detector:227scs-lcm/salt-state-sync:242streaming-alpine:30.10.183streaming-kafka:30.10.183vision-service-document-classification:1.1.55vision-service-image-classification:1.4.52";
        do_test_compress(&mut random, input)?;
        Ok(())
    }
    #[test]
    fn test_far_away_exceptions() -> Result<()> {
        let mut random = random();
        let mut s = String::from("01W");
        s.extend(std::iter::repeat_n("a", 300));
        s.push_str("W.");
        let bytes = s.as_bytes();
        assert!(do_test_compress(&mut random, bytes)?);
        Ok(())
    }
    #[test]
    fn test_random_ascii() -> Result<()> {
        let mut random = random();
        for _ in 0..1000 {
            let len = random.random_range(0..1000);
            let mut bytes = vec![0u8; len + random.random_range(0..10)];
            for b in &mut bytes {
                *b = TestUtil::next_int(&mut random, b' ' as i32, b'~' as i32) as u8;
            }
            do_test_compress_with_len(&mut random, &bytes, len)?;
        }
        Ok(())
    }
    #[test]
    fn test_random_compressible_ascii() -> Result<()> {
        let mut random = random();
        for _ in 0..1000 {
            let len = TestUtil::next_int(&mut random, 8, 1000) as usize;
            let mut bytes = vec![0u8; len + random.random_range(0..10)];
            for b in &mut bytes {
                let mut x = random.random_range(0..32);
                x |= 0x20 | ((x & 0x20) << 1);
                x -= 1;
                *b = x as u8;
            }
            assert!(do_test_compress_with_len(&mut random, &bytes, len)?);
        }
        Ok(())
    }
    #[test]
    fn test_random_compressible_ascii_with_exceptions() -> Result<()> {
        let mut random = random();
        for _ in 0..1000 {
            let len = TestUtil::next_int(&mut random, 8, 1000) as usize;
            let mut exceptions = 0;
            let max_exceptions = len >> 5;
            let mut bytes = vec![0u8; len + random.random_range(0..10)];
            for b in &mut bytes {
                if exceptions == max_exceptions || random.random_range(0..100) != 0 {
                    let mut x = random.random_range(0..32);
                    x |= 0x20 | ((x & 0x20) << 1);
                    x -= 1;
                    *b = x as u8;
                } else {
                    exceptions += 1;
                    *b = random.random_range(0..256) as u8;
                }
            }
            assert!(do_test_compress_with_len(&mut random, &bytes, len)?);
        }
        Ok(())
    }
    #[test]
    fn test_random() -> Result<()> {
        let mut random = random();
        for _ in 0..1000 {
            let len = random.random_range(0..1000);
            let mut bytes = vec![0u8; len + random.random_range(0..10)];
            random.fill(&mut bytes[..]);
            do_test_compress_with_len(&mut random, &bytes, len)?;
        }
        Ok(())
    }
    #[test]
    fn test_ascii_compression_random2() -> Result<()> {
        let mut random = random();
        let iters = at_least(&mut random, 1000);
        for _ in 0..iters {
            let word_len = at_least(&mut random, 400) as usize;
            let simple = random.random_bool(0.5);
            let s = TestUtil::random_substring(&mut random, word_len, simple);
            do_test_compress(&mut random, s.as_bytes())?;
        }
        Ok(())
    }
}
