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
use std::fmt;

use crate::test::util::lucene_test_case::EnvConfig::{Multiplier, NightMode, TestSeed};

#[allow(dead_code)] // for quick search
pub struct LuceneTestCase;
/// Describes the currently supported environment variables used to control
/// Lucene tests.
///
/// Each variant corresponds to an environment variable that configures specific
/// behaviors of the tests. For example, environment variables can be used to
/// control the test mode, random number generator seed, etc.
#[derive(Debug, Clone, Copy)]
pub enum EnvConfig {
    NightMode,
    Multiplier,
    TestSeed,
}

impl fmt::Display for EnvConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let key = match self {
            NightMode => "tests.nightly",
            Multiplier => "tests.multiplier",
            TestSeed => "tests.seed",
        };
        write!(f, "{}", key)
    }
}

pub mod lucene_test_case_util {
    use crate::core::document::field::{Field, FieldDataEnum, Store};
    use crate::core::document::field_type::FieldType;
    use crate::core::document::string_field::string;
    use crate::core::document::text_field::text;
    use crate::core::index::BytesRef;
    use crate::core::index::index_options::IndexOptions;

    use crate::core::index::index_writer_config::IndexWriterConfig;
    use crate::core::index::indexable_field_type::IndexableFieldType;
    use crate::core::store::directory::Directory;
    use crate::core::store::flush_info::FlushInfo;
    use crate::core::store::merge_info::MergeInfo;
    use crate::core::store::nio_fs_directory::NIOFSDirectory;
    use crate::core::store::{
        FSDirectory, IO_CONTEXT_DEFAULT, IO_CONTEXT_READ_ONCE, IOContext, NativeFSLockFactory,
    };
    use crate::core::util::SliceCopyOps;
    use crate::core::util::access::SharedAccessVec;
    use crate::core::util::error::lucene_error::{LuceneError, Result};
    use crate::test::util::lucene_test_case::EnvConfig::{Multiplier, NightMode, TestSeed};
    use crate::test::util::test_util::TestUtil;
    use once_cell::sync::Lazy;
    use parking_lot::Mutex;
    use rand::prelude::StdRng;
    use rand::{Rng, RngCore, SeedableRng};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;
    use tempfile::TempDir;

    static FIELD_TO_TYPE: Lazy<Mutex<HashMap<String, FieldType>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));
    pub(crate) fn random_multiplier() -> i32 {
        let multiplier = std::env::var(Multiplier.to_string()).ok();

        multiplier
            .and_then(|v| v.parse::<i32>().ok())
            .unwrap_or(default_random_multiplier())
    }

    fn default_random_multiplier() -> i32 {
        if is_night_mode() { 2 } else { 1 }
    }
    /// Returns a number of at least `i`
    ///
    /// The actual number returned will be influenced by whether `TEST_NIGHTLY` is
    /// active and `RANDOM_MULTIPLIER`, but also with some random fudge.
    pub(crate) fn at_least<R: Rng + ?Sized>(random: &mut R, i: i32) -> i32 {
        let min = i * random_multiplier();
        let max = min + (min / 2);
        TestUtil::next_int(random, min, max)
    }

    pub(crate) fn rarely<R: Rng + ?Sized>(random: &mut R) -> bool {
        let mut p = if is_night_mode() { 5 } else { 1 };
        p += (p as f64 * (random_multiplier() as f64).ln()).round() as i32;
        let min = 100 - p.min(20); // Never more than 20% chance
        random.random_range(0..100) >= min
    }

    pub(crate) fn new_index_writer_config<R: Rng + ?Sized>(_random: &mut R) -> IndexWriterConfig {
        // TODO: 这里简单返回IndexWriterConfig::default()，后续可以根据random随机生成不同的配置
        IndexWriterConfig::new()
    }

    // TODO: When we have implemented multiple directories, we need to select one
    // randomly. Currently, we choose NIOFSDirectory.
    pub(crate) fn new_directory<R: Rng + ?Sized>(
        _random: &mut R,
    ) -> Result<FSDirectory<NativeFSLockFactory, NIOFSDirectory>> {
        let temp_dir = TempDir::new()?;
        let sub_directory = NIOFSDirectory::new();
        FSDirectory::new(temp_dir.keep(), sub_directory)
    }
    pub(crate) fn new_string_field<S1, S2>(name: S1, value: S2, stored: Store) -> Result<Field>
    where
        S1: Into<String>,
        S2: Into<String>,
    {
        let mut rng = random();
        let field_type = match stored {
            Store::Yes => string::TYPE_STORED.clone(),
            Store::No => string::TYPE_NOT_STORED.clone(),
        };

        new_field_with_random(
            &mut rng,
            name.into(),
            FieldDataEnum::String(value.into()),
            &field_type,
        )
    }

    pub(crate) fn new_string_field_binary<S>(
        name: S,
        value: BytesRef<Vec<u8>>,
        stored: Store,
    ) -> Result<Field>
    where
        S: Into<String>,
    {
        let mut rng = random();
        let field_type = match stored {
            Store::Yes => string::TYPE_STORED.clone(),
            Store::No => string::TYPE_NOT_STORED.clone(),
        };

        new_field_with_random(
            &mut rng,
            name.into(),
            FieldDataEnum::Binary(value),
            &field_type,
        )
    }
    pub(crate) fn new_text_field<S1, S2>(name: S1, value: S2, stored: Store) -> Result<Field>
    where
        S1: Into<String>,
        S2: Into<String>,
    {
        let mut random = random();
        let field_type = match stored {
            Store::Yes => text::TYPE_STORED.clone(),
            Store::No => text::TYPE_NOT_STORED.clone(),
        };

        new_field_with_random(
            &mut random,
            name,
            FieldDataEnum::String(value.into()),
            &field_type,
        )
    }
    pub(crate) fn new_string_field_string_with_random<S1, S2, R: Rng + ?Sized>(
        random: &mut R,
        name: S1,
        value: S2,
        stored: Store,
    ) -> Result<Field>
    where
        S1: Into<String>,
        S2: Into<String>,
    {
        let field_type = match stored {
            Store::Yes => string::TYPE_STORED.clone(),
            Store::No => string::TYPE_NOT_STORED.clone(),
        };

        new_field_with_random(
            random,
            name,
            FieldDataEnum::String(value.into()),
            &field_type,
        )
    }
    pub(crate) fn new_string_field_binary_with_random<S, R: Rng + ?Sized>(
        random: &mut R,
        name: S,
        value: BytesRef<Vec<u8>>,
        stored: Store,
    ) -> Result<Field>
    where
        S: Into<String>,
    {
        let field_type = match stored {
            Store::Yes => string::TYPE_STORED.clone(),
            Store::No => string::TYPE_NOT_STORED.clone(),
        };
        new_field_with_random(random, name, FieldDataEnum::Binary(value), &field_type)
    }

    pub(crate) fn new_text_field_with_random<S1, S2, R: Rng + ?Sized>(
        random: &mut R,
        name: S1,
        value: S2,
        stored: Store,
    ) -> Result<Field>
    where
        S1: Into<String>,
        S2: Into<String>,
    {
        let field_type = match stored {
            Store::Yes => text::TYPE_STORED.clone(),
            Store::No => text::TYPE_NOT_STORED.clone(),
        };
        new_field_with_random(
            random,
            name,
            FieldDataEnum::String(value.into()),
            &field_type,
        )
    }
    pub(crate) fn new_field<S>(
        name: S,
        value: FieldDataEnum,
        field_type: &FieldType,
    ) -> Result<Field>
    where
        S: Into<String>,
    {
        let mut random = random();
        new_field_with_random(&mut random, name, value, field_type)
    }
    // TODO: if we can pull out the "make term vector options
    // consistent across all instances of the same field name"
    // write-once schema sort of helper class then we can
    // remove the sync here.  We can also fold the random
    // "enable norms" (now commented out, below) into that:
    pub(crate) fn new_field_with_random<S, R: Rng + ?Sized>(
        random: &mut R,
        name: S,
        value: FieldDataEnum,
        field_type: &FieldType,
    ) -> Result<Field>
    where
        S: Into<String>,
    {
        let name = name.into();

        let mut map = FIELD_TO_TYPE.lock();
        if let Some(prev_type) = map.get(&name) {
            return create_field(&name, value, prev_type.clone());
        }
        // TODO: once all core & test codecs can index
        // offsets, sometimes randomly turn on offsets if we are
        // already indexing positions...
        let mut new_type = FieldType::from_ref(field_type)?;
        if !new_type.stored() && random.random_bool(0.5) {
            new_type.set_stored(true)?; // randomly store it
        }

        if *new_type.index_options() != IndexOptions::None
            && !new_type.store_term_vectors()
            && random.random_bool(0.5)
        {
            new_type.set_store_term_vectors(true)?;

            if !new_type.store_term_vector_positions() && random.random_bool(0.5) {
                new_type.set_store_term_vector_positions(true)?;

                if !new_type.store_term_vector_payloads() {
                    new_type.set_store_term_vector_payloads(random.random_bool(0.5))?;
                }
            }

            // Check for strings as offsets are disallowed on binary fields
            if matches!(value, FieldDataEnum::String(_)) && !new_type.store_term_vector_offsets() {
                new_type.set_store_term_vector_offsets(random.random_bool(0.5))?;
            }

            if cfg!(feature = "test_log_verbose") {
                println!(
                    "NOTE: LuceneTestCase: upgrade name={} type={:?}",
                    name, new_type
                );
            }
        }
        new_type.freeze();
        map.insert(name.clone(), new_type.clone());
        create_field(&name, value, new_type)
    }
    pub(crate) fn create_field(
        name: &str,
        value: FieldDataEnum,
        field_type: FieldType,
    ) -> Result<Field> {
        match value {
            FieldDataEnum::String(_) => Ok(Field::new(name, field_type, value)),
            FieldDataEnum::Binary(_) => Ok(Field::new(name, field_type, value)),
            _ => Err(LuceneError::illegal_argument(
                "Unsupported FieldDataEnum variant",
            )),
        }
    }

    pub(crate) fn new_io_context<R: Rng + ?Sized>(random: &mut R) -> Result<IOContext> {
        new_io_context_with_default(random, &IO_CONTEXT_DEFAULT)
    }

    pub(crate) fn new_io_context_with_default<R: Rng + ?Sized>(
        random: &mut R,
        old_context: &IOContext,
    ) -> Result<IOContext> {
        if *old_context == *IO_CONTEXT_READ_ONCE {
            // Don't modify the READONCE SINGLETON
            return Ok(old_context.clone());
        }

        // Generate random parameters
        let random_num_docs: i32 = random.random_range(0..4192);
        let size = random.random_range(0..512) * random_num_docs as i64;

        if let Some(flush_info) = &old_context.flush_info {
            // Always return at least the estimatedSegmentSize of the incoming
            // IOContext
            Ok(IOContext::with_flush(FlushInfo::new(
                random_num_docs,
                size.max(flush_info.get_estimated_segment_size()),
            ))?)
        } else if let Some(merge_info) = &old_context.merge_info {
            // Always return at least the estimatedMergeBytes of the incoming
            // IOContext
            IOContext::with_merge(MergeInfo::new(
                random_num_docs,
                size.max(merge_info.get_estimated_merge_bytes()),
                random.random_bool(0.5), /* Randomly decide if it's an external
                                          * merge  */
                random.random_range(1..=100),
            ))
        } else {
            // Make a totally random IOContext, except READONCE which has semantic
            // implications
            let context_type = random.random_range(0..3);
            match context_type {
                0 => Ok(IOContext::default_io_context()?),
                1 => Ok(IOContext::with_merge(MergeInfo::new(
                    random_num_docs,
                    size,
                    true,
                    -1,
                ))?),
                2 => Ok(IOContext::with_flush(FlushInfo::new(
                    random_num_docs,
                    size,
                ))?),
                _ => Ok(IOContext::default_io_context()?),
            }
        }
    }
    pub(crate) fn slow_file_exists(dir: &impl Directory, name: &str) -> Result<bool> {
        let result = dir.open_input(name, &IOContext::default_io_context()?);
        match result {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        }
    }
    /// Creates a `BytesRef` holding UTF-8 bytes for the incoming string,
    /// that sometimes uses a non-zero offset and non-zero end-padding to
    /// tickle latent bugs that fail to look at `BytesRef.offset`.
    pub(crate) fn new_bytes_ref_from_string<R: Rng + ?Sized, AV: SharedAccessVec<u8>>(
        random: &mut R,
        s: &str,
    ) -> Result<BytesRef<AV>> {
        let bytes = s.as_bytes();
        new_bytes_ref(random, bytes, 0, bytes.len() as i32)
    }

    /// Creates a copy of the incoming `BytesRef` that sometimes uses a non-zero
    /// offset, and non-zero end-padding, to tickle latent bugs that fail to look at
    /// `BytesRef.offset`.
    pub(crate) fn new_bytes_ref_from_bytes_ref<R: Rng + ?Sized, AV: SharedAccessVec<u8>>(
        random: &mut R,
        b: &BytesRef<AV>,
    ) -> Result<BytesRef<AV>> {
        assert!(b.is_valid()?);
        b.bytes
            .access(|bytes| new_bytes_ref(random, bytes, b.offset as i32, b.length as i32))
    }

    /// Creates a random `BytesRef` from the incoming bytes, sometimes using a
    /// non-zero offset, and non-zero end-padding, to tickle latent bugs that fail
    /// to look at `BytesRef.offset`.
    pub(crate) fn new_bytes_ref_from_bytes<R: Rng + ?Sized, AV: SharedAccessVec<u8>>(
        random: &mut R,
        bytes_in: &[u8],
    ) -> Result<BytesRef<AV>> {
        new_bytes_ref(random, bytes_in, 0, bytes_in.len() as i32)
    }

    /// Creates a random empty `BytesRef` that sometimes uses a non-zero offset, and
    /// non-zero end-padding, to tickle latent bugs that fail to look at
    /// `BytesRef.offset`.
    pub(crate) fn new_bytes_ref_empty<R: Rng + ?Sized, AV: SharedAccessVec<u8>>(
        random: &mut R,
    ) -> Result<BytesRef<AV>> {
        // Calling the existing `new_bytes_ref` function
        new_bytes_ref(random, &[], 0, 0)
    }

    /// Creates a random empty `BytesRef`, with at least the requested length of
    /// bytes free, that sometimes uses a non-zero offset and non-zero end-padding
    /// to tickle latent bugs that fail to look at `BytesRef.offset`.
    pub(crate) fn new_bytes_ref_with_length<R: Rng + ?Sized, AV: SharedAccessVec<u8>>(
        byte_length: i32,
        random: &mut R,
    ) -> Result<BytesRef<AV>> {
        let bytes_in = vec![0u8; byte_length as usize];
        new_bytes_ref(random, &bytes_in, 0, byte_length)
    }

    /// Creates a copy of the incoming bytes slice that sometimes uses a non-zero
    /// {@code offset}, and non-zero end-padding, to tickle latent bugs that fail to
    /// look at {@code BytesRef.offset}.
    pub(crate) fn new_bytes_ref<R: Rng + ?Sized, AV: SharedAccessVec<u8>>(
        random: &mut R,
        bytes_in: &[u8],
        offset: i32,
        length: i32,
    ) -> Result<BytesRef<AV>> {
        assert!(
            bytes_in.len() >= (offset + length) as usize,
            "got offset={} length={} bytesIn.length={}",
            offset,
            length,
            bytes_in.len()
        );
        // Randomly set a non-zero offset
        let start_offset = if random.random_bool(0.5) {
            random.random_range(1..=20)
        } else {
            0
        };

        // Randomly set an end padding (between 1 and 20)
        let end_padding = if random.random_bool(0.5) {
            random.random_range(1..=20)
        } else {
            0
        };

        let mut bytes = vec![0u8; (start_offset + length + end_padding) as usize];

        bytes.copy_from(
            &bytes_in[offset as usize..(offset + length) as usize],
            start_offset as usize,
        );
        // Create a BytesRef and return it
        let vec = AV::from_vec(bytes);
        let it = BytesRef {
            bytes: vec,
            offset: start_offset as usize,
            length: length as usize,
        };
        assert!(it.is_valid()?);

        if random.random_range(1..=17) == 7 {
            return it
                .bytes
                .access(|bytes| new_bytes_ref(random, bytes, it.offset as i32, it.length as i32));
        }
        Ok(it)
    }

    thread_local! {
        static THREAD_LOCAL_RANDOM: RefCell<Option<Rc<RefCell<StdRng>>>> =
            const { RefCell::new(None) };
    }

    #[derive(Clone)]
    pub(crate) struct TestRng {
        inner: Rc<RefCell<StdRng>>,
    }

    impl TestRng {
        fn new(inner: Rc<RefCell<StdRng>>) -> Self {
            Self { inner }
        }
    }

    impl RngCore for TestRng {
        fn next_u32(&mut self) -> u32 {
            self.inner.borrow_mut().next_u32()
        }

        fn next_u64(&mut self) -> u64 {
            self.inner.borrow_mut().next_u64()
        }

        fn fill_bytes(&mut self, dest: &mut [u8]) {
            self.inner.borrow_mut().fill_bytes(dest);
        }
    }

    /// Retrieves the seed from the environment variable "tests.seed".
    /// If the environment variable is not set or cannot be parsed as a `u64`,
    /// it generates a random seed and logs the result.
    ///
    /// # Returns
    /// A valid `u64` seed.
    pub(crate) fn get_seed_from_env() -> u64 {
        if let Ok(seed_str) = std::env::var(TestSeed.to_string()) {
            if let Ok(seed) = seed_str.parse::<u64>() {
                println!("Using Global Seed from environment: '{}'", seed);
                return seed;
            } else {
                println!("Environment variable tests.seed is invalid: '{}'", seed_str);
            }
        }

        let seed = rand::rng().random_range(0..u64::MAX);
        println!("Generated random seed : {}", seed);
        seed
    }

    pub(crate) fn random() -> TestRng {
        let shared = THREAD_LOCAL_RANDOM.with(|cell| {
            let mut slot = cell.borrow_mut();
            slot.get_or_insert_with(|| {
                Rc::new(RefCell::new(StdRng::seed_from_u64(get_seed_from_env())))
            })
            .clone()
        });

        TestRng::new(shared)
    }

    pub(crate) fn random_from_seed(seed: u64) -> StdRng {
        StdRng::seed_from_u64(seed)
    }

    pub fn is_night_mode() -> bool {
        std::env::var(NightMode.to_string()).is_ok_and(|v| v == "true")
    }
}
