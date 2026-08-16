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

use crate::core::index::directory_reader;
use crate::core::index::index_reader::{Identity, IndexReader};
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer::MAX_TERM_LENGTH;
use crate::core::index::standard_directory_reader::StandardDirectoryReader;
use crate::core::index::term::Term;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::term_query::TermQuery;
use crate::core::store::directory::{DirEnum, Directory};
use crate::core::store::byte_buffers_directory::DirectoryByteBuffersIndexOutput;
use crate::core::store::flush_info::FlushInfo;
use crate::core::store::index_input::NRTCachingIndexInput;
use crate::core::store::nrt_caching_directory::{NRTCachingDirectory, NRTCachingDirectoryHook};
use crate::core::store::single_instance_lock_factory::SingleInstanceLockFactory;
use crate::core::store::{
  ByteBuffersDirectory, ByteBuffersIndexInputOwned, DataOutput, IO_CONTEXT_DEFAULT, IOContext,
  IndexOutput, IndexOutputEnum2,
};
use crate::core::util::HasIdentity;
use crate::core::util::accountable::Accountable;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::store::base_directory_test_case::BaseDirectoryTestCase;
use crate::test_framework::core::store::test_nrt_caching_directory::AssertCacheWriteNRTCachingDirectory;
use crate::test_framework::core::util::line_file_docs::LineFileDocs;
use crate::test_framework::core::util::lucene_test_case::{
  new_directory_shared, new_index_writer_config_with_analyzer, new_searcher_with_reader, random,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::{Rng, RngExt};
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::sync::Arc;

type InnerByteBuffersNRTCachingDirectory =
  NRTCachingDirectory<ByteBuffersDirectory<SingleInstanceLockFactory>>;

pub struct ByteBuffersNRTCachingDirectory(InnerByteBuffersNRTCachingDirectory);

impl Display for ByteBuffersNRTCachingDirectory {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    self.0.fmt(f)
  }
}

impl HasIdentity for ByteBuffersNRTCachingDirectory {
  fn identity(&self) -> &Identity {
    self.0.identity()
  }
}

impl CloseableRef for ByteBuffersNRTCachingDirectory {
  fn close(&self) -> Result<()> {
    self.0.close()
  }
}

impl Directory for ByteBuffersNRTCachingDirectory {
  fn list_all(&self) -> Result<Vec<String>> {
    self.0.list_all()
  }

  fn delete_file(&self, name: &str) -> Result<()> {
    self.0.delete_file(name)
  }

  fn file_length(&self, name: &str) -> Result<usize> {
    self.0.file_length(name)
  }

  type IndexOutput = DirectoryByteBuffersIndexOutput;

  fn create_output(&self, name: &str, context: &IOContext) -> Result<Self::IndexOutput> {
    match self.0.create_output(name, context)? {
      IndexOutputEnum2::A(output) | IndexOutputEnum2::B(output) => Ok(output),
    }
  }

  fn create_temp_output(
    &self,
    prefix: &str,
    suffix: &str,
    context: &IOContext,
  ) -> Result<Self::IndexOutput> {
    match self.0.create_temp_output(prefix, suffix, context)? {
      IndexOutputEnum2::A(output) | IndexOutputEnum2::B(output) => Ok(output),
    }
  }

  fn sync(&self, names: &[String]) -> Result<()> {
    self.0.sync(names)
  }

  fn sync_metadata(&self) -> Result<()> {
    self.0.sync_metadata()
  }

  fn rename(&self, source: &str, dest: &str) -> Result<()> {
    self.0.rename(source, dest)
  }

  type IndexInput = ByteBuffersIndexInputOwned;

  fn open_input(&self, name: &str, context: &IOContext) -> Result<Self::IndexInput> {
    match self.0.open_input(name, context)? {
      NRTCachingIndexInput::A(input) | NRTCachingIndexInput::B(input) => Ok(input),
    }
  }

  type Lock = <InnerByteBuffersNRTCachingDirectory as Directory>::Lock;

  fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
    self.0.obtain_lock(name)
  }

  fn copy_from<D>(&self, from: &D, src: &str, dest: &str, context: &IOContext) -> Result<()>
  where
    D: Directory + ?Sized,
  {
    self.0.copy_from(from, src, dest, context)
  }

  fn get_pending_deletions(&self) -> Result<HashSet<String>> {
    self.0.get_pending_deletions()
  }

  #[cfg(debug_assertions)]
  fn is_fs_directory(&self) -> bool {
    self.0.is_fs_directory()
  }

  fn ensure_open(&self) -> Result<()> {
    self.0.ensure_open()
  }
}

#[allow(dead_code)] // for quick search
pub struct TestNRTCachingDirectory;

impl BaseDirectoryTestCase for TestNRTCachingDirectory {
  type Directory = ByteBuffersNRTCachingDirectory;
  type Output = ByteBuffersIndexInputOwned;

  // A RAM directory is used here because filesystem directories are still too
  // slow for the threaded tests, possibly because `list_all` is synchronized.
  fn get_directory<R>(&self, _path: PathBuf, random: &mut R) -> Result<Self::Directory>
  where
    R: Rng + ?Sized,
  {
    Ok(ByteBuffersNRTCachingDirectory(NRTCachingDirectory::new(
      ByteBuffersDirectory::new(),
      0.1 + 2.0 * random.random::<f64>(),
      0.1 + 5.0 * random.random::<f64>(),
    )))
  }
}

impl TestNRTCachingDirectory {
  fn test_nrt_and_commit<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let cached_dir = Arc::new(NRTCachingDirectory::new(dir.clone(), 2.0, 25.0));
    let mut analyzer = MockAnalyzer::new(random);
    analyzer.set_max_token_length(TestUtil::next_int(random, 1, MAX_TERM_LENGTH));
    let config = new_index_writer_config_with_analyzer(random, analyzer)?;
    let writer = RandomIndexWriter::with_config(random, cached_dir.clone(), config);
    let mut docs = LineFileDocs::new(random)?;
    let num_docs = TestUtil::next_int(random, 100, 400);

    if cfg!(feature = "test_log_verbose") {
      println!("TEST: numDocs={num_docs}");
    }

    let mut ids = Vec::new();
    let mut r: Option<Arc<StandardDirectoryReader<NRTCachingDirectory<Arc<DirEnum>>>>> = None;
    for doc_count in 0..num_docs {
      let doc = docs.next_doc()?;
      ids.push(
        doc
          .get("docid")?
          .expect("LineFileDocs document must have docid")
          .into_owned(),
      );
      writer.add_document(random, doc)?;
      if random.random_range(0..20) == 17 {
        if let Some(current) = &r {
          if let Some(changed) = directory_reader::open_if_changed(current)? {
            current.close()?;
            r = Some(Arc::new(changed));
          }
        } else {
          r = Some(Arc::new(directory_reader::open_from_writer(&writer.w)?));
        }
        let current = r.as_ref().expect("reader was opened");
        assert_eq!(1 + doc_count, current.num_docs()?);
        let searcher = new_searcher_with_reader(current.clone())?;
        // Just make sure search can run; the total hits may be zero.
        searcher.search(TermQuery::new(Term::from_text("body", "the")), 10)?;
      }
    }

    if let Some(reader) = r {
      reader.close()?;
    }

    // Close should force the cache to clear since all files are synced.
    writer.close(random)?;

    let cached_files = cached_dir.list_cached_files()?;
    for file in &cached_files {
      println!("FAIL: cached file {file} remains after sync");
    }
    assert!(cached_files.is_empty());

    let reader = directory_reader::open(dir.clone())?;
    for id in ids {
      assert_eq!(1, reader.doc_freq(&Term::from_text("docid", id))?);
    }
    reader.close()?;
    cached_dir.close()?;
    docs.close();
    Ok(())
  }

  // NOTE: not a test; this only makes sure the example in the Rust docs
  // compiles.
  #[allow(dead_code)]
  fn verify_compiles<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let fs_dir = new_directory_shared(random)?;
    let cached_fs_dir = Arc::new(NRTCachingDirectory::new(fs_dir, 2.0, 25.0));
    let analyzer = MockAnalyzer::new(random);
    let config = new_index_writer_config_with_analyzer(random, analyzer)?;
    let writer = IndexWriter::new(cached_fs_dir.clone(), config)?;
    writer.close()?;
    cached_fs_dir.close()
  }

  fn test_create_temp_output_same_name<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let fs_dir = new_directory_shared(random)?;
    let nrt_dir = NRTCachingDirectory::new(fs_dir.clone(), 2.0, 25.0);
    let name = "foo_bar_0.tmp";
    let mut existing = nrt_dir.create_output(name, &IO_CONTEXT_DEFAULT)?;
    existing.close()?;

    let mut out = nrt_dir.create_temp_output("foo", "bar", &IO_CONTEXT_DEFAULT)?;
    assert_ne!(name, out.get_name());
    out.close()?;
    nrt_dir.close()?;
    fs_dir.close()
  }

  fn test_unknown_file_size<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;

    let nrt_dir1 = NRTCachingDirectory::with_hook(
      dir.clone(),
      1.0,
      1.0,
      NRTCachingDirectoryHook::AssertCacheWrite(AssertCacheWriteNRTCachingDirectory::new(true)),
    );
    let io_context = IOContext::with_flush(FlushInfo::new(3, 42))?;
    let mut out = nrt_dir1.create_output("foo", &io_context)?;
    out.close()?;
    let mut out = nrt_dir1.create_temp_output("bar", "baz", &io_context)?;
    out.close()?;

    let nrt_dir2 = NRTCachingDirectory::with_hook(
      dir.clone(),
      1.0,
      1.0,
      NRTCachingDirectoryHook::AssertCacheWrite(AssertCacheWriteNRTCachingDirectory::new(false)),
    );
    let io_context = IOContext::default_io_context()?;
    let mut out = nrt_dir2.create_output("foo", &io_context)?;
    out.close()?;
    let mut out = nrt_dir2.create_temp_output("bar", "baz", &io_context)?;
    out.close()?;

    dir.close()
  }

  fn test_cache_size_after_delete<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let io_context = IOContext::with_flush(FlushInfo::new(3, 40))?;
    let file_name = "f1";
    let dir = new_directory_shared(random)?;
    let nrt = NRTCachingDirectory::new(dir.clone(), 1.0, 1.0);

    // Deletes a closed file.
    let mut out = nrt.create_output(file_name, &io_context)?;
    for i in 0..10 {
      out.write_int(i)?;
    }
    out.close()?;
    assert_eq!(40, nrt.ram_bytes_used()?);
    nrt.delete_file(file_name)?;
    assert_eq!(0, nrt.ram_bytes_used()?);

    // Deletes an unclosed file, with writes before and after deletion.
    let mut out = nrt.create_output(file_name, &io_context)?;
    for i in 0..10 {
      out.write_int(i)?;
    }
    nrt.delete_file(file_name)?;
    for i in 0..10 {
      out.write_int(i)?;
    }
    out.close()?;
    assert_eq!(0, nrt.ram_bytes_used()?);

    nrt.close()?;
    dir.close()
  }
}

#[test]
fn test_nrt_and_commit() -> Result<()> {
  run_case(|case, random| case.test_nrt_and_commit(random))
}

#[test]
fn test_create_temp_output_same_name() -> Result<()> {
  run_case(|case, random| case.test_create_temp_output_same_name(random))
}

#[test]
fn test_unknown_file_size() -> Result<()> {
  run_case(|case, random| case.test_unknown_file_size(random))
}

#[test]
fn test_cache_size_after_delete() -> Result<()> {
  run_case(|case, random| case.test_cache_size_after_delete(random))
}

mod base_directory_test_case_tests {
  use crate::core::util::error::lucene_error::Result;
  use crate::test::core::store::test_nrt_caching_directory::run_case;
  use crate::test_framework::core::store::base_directory_test_case::BaseDirectoryTestCase;

  #[test]
  fn test_copy_from() -> Result<()> {
    run_case(|case, random| case.test_copy_from(random))
  }

  #[test]
  fn test_rename() -> Result<()> {
    run_case(|case, random| case.test_rename(random))
  }

  #[test]
  fn test_delete_file() -> Result<()> {
    run_case(|case, random| case.test_delete_file(random))
  }

  #[test]
  fn test_byte() -> Result<()> {
    run_case(|case, random| case.test_byte(random))
  }

  #[test]
  fn test_short() -> Result<()> {
    run_case(|case, random| case.test_short(random))
  }

  #[test]
  fn test_int() -> Result<()> {
    run_case(|case, random| case.test_int(random))
  }

  #[test]
  fn test_long() -> Result<()> {
    run_case(|case, random| case.test_long(random))
  }

  #[test]
  fn test_aligned_little_endian_longs() -> Result<()> {
    run_case(|case, random| case.test_aligned_little_endian_longs(random))
  }

  #[test]
  fn test_unaligned_little_endian_longs() -> Result<()> {
    run_case(|case, random| case.test_unaligned_little_endian_longs(random))
  }

  #[test]
  fn test_little_endian_longs_underflow() -> Result<()> {
    run_case(|case, random| case.test_little_endian_longs_underflow(random))
  }

  #[test]
  fn test_aligned_ints() -> Result<()> {
    run_case(|case, random| case.test_aligned_ints(random))
  }

  #[test]
  fn test_unaligned_ints() -> Result<()> {
    run_case(|case, random| case.test_unaligned_ints(random))
  }

  #[test]
  fn test_ints_underflow() -> Result<()> {
    run_case(|case, random| case.test_ints_underflow(random))
  }

  #[test]
  fn test_aligned_floats() -> Result<()> {
    run_case(|case, random| case.test_aligned_floats(random))
  }

  #[test]
  fn test_unaligned_floats() -> Result<()> {
    run_case(|case, random| case.test_unaligned_floats(random))
  }

  #[test]
  fn test_floats_underflow() -> Result<()> {
    run_case(|case, random| case.test_floats_underflow(random))
  }

  #[test]
  fn test_string() -> Result<()> {
    run_case(|case, random| case.test_string(random))
  }

  #[test]
  fn test_vint() -> Result<()> {
    run_case(|case, random| case.test_vint(random))
  }

  #[test]
  fn test_vlong() -> Result<()> {
    run_case(|case, random| case.test_vlong(random))
  }

  #[test]
  fn test_zint() -> Result<()> {
    run_case(|case, random| case.test_zint(random))
  }

  #[test]
  fn test_zlong() -> Result<()> {
    run_case(|case, random| case.test_zlong(random))
  }

  #[test]
  fn test_set_of_strings() -> Result<()> {
    run_case(|case, random| case.test_set_of_strings(random))
  }

  #[test]
  fn test_map_of_strings() -> Result<()> {
    run_case(|case, random| case.test_map_of_strings(random))
  }

  #[test]
  fn test_checksum() -> Result<()> {
    run_case(|case, random| case.test_checksum(random))
  }

  #[test]
  fn test_detect_close() -> Result<()> {
    run_case(|case, random| case.test_detect_close(random))
  }

  #[test]
  fn test_thread_safety_in_list_all() -> Result<()> {
    run_case(|case, random| case.test_thread_safety_in_list_all(random))
  }

  #[test]
  fn test_file_exists_in_list_after_created() -> Result<()> {
    run_case(|case, random| case.test_file_exists_in_list_after_created(random))
  }

  #[test]
  fn test_seek_to_eof_then_back() -> Result<()> {
    run_case(|case, random| case.test_seek_to_eof_then_back(random))
  }

  #[test]
  fn test_illegal_eof() -> Result<()> {
    run_case(|case, random| case.test_illegal_eof(random))
  }

  #[test]
  fn test_seek_past_eof() -> Result<()> {
    run_case(|case, random| case.test_seek_past_eof(random))
  }

  #[test]
  fn test_slice_out_of_bounds() -> Result<()> {
    run_case(|case, random| case.test_slice_out_of_bounds(random))
  }

  #[test]
  fn test_no_dir() -> Result<()> {
    run_case(|case, random| case.test_no_dir(random))
  }

  #[test]
  fn test_copy_bytes() -> Result<()> {
    run_case(|case, random| case.test_copy_bytes(random))
  }

  #[test]
  fn test_copy_bytes_with_threads() -> Result<()> {
    run_case(|case, random| case.test_copy_bytes_with_threads(random))
  }

  #[test]
  fn test_fsync_doesnt_create_new_files() -> Result<()> {
    run_case(|case, random| case.test_fsync_doesnt_create_new_files(random))
  }

  #[test]
  fn test_random_long() -> Result<()> {
    run_case(|case, random| case.test_random_long(random))
  }

  #[test]
  fn test_random_int() -> Result<()> {
    run_case(|case, random| case.test_random_int(random))
  }

  #[test]
  fn test_random_short() -> Result<()> {
    run_case(|case, random| case.test_random_short(random))
  }

  #[test]
  fn test_random_byte() -> Result<()> {
    run_case(|case, random| case.test_random_byte(random))
  }

  #[test]
  fn test_slice_of_slice() -> Result<()> {
    run_case(|case, random| case.test_slice_of_slice(random))
  }

  #[test]
  fn test_large_writes() -> Result<()> {
    run_case(|case, random| case.test_large_writes(random))
  }

  #[test]
  fn test_index_output_to_string() -> Result<()> {
    run_case(|case, random| case.test_index_output_to_string(random))
  }

  #[test]
  fn test_double_close_output() -> Result<()> {
    run_case(|case, random| case.test_double_close_output(random))
  }

  #[test]
  fn test_double_close_input() -> Result<()> {
    run_case(|case, random| case.test_double_close_input(random))
  }

  #[test]
  fn test_create_temp_output() -> Result<()> {
    run_case(|case, random| case.test_create_temp_output(random))
  }

  #[test]
  fn test_create_output_for_existing_file() -> Result<()> {
    run_case(|case, random| case.test_create_output_for_existing_file(random))
  }

  #[test]
  fn test_seek_to_end_of_file() -> Result<()> {
    run_case(|case, random| case.test_seek_to_end_of_file(random))
  }

  #[test]
  fn test_seek_beyond_end_of_file() -> Result<()> {
    run_case(|case, random| case.test_seek_beyond_end_of_file(random))
  }

  #[test]
  fn test_pending_deletions() -> Result<()> {
    run_case(|case, random| case.test_pending_deletions(random))
  }

  #[test]
  fn test_list_all_is_sorted() -> Result<()> {
    run_case(|case, random| case.test_list_all_is_sorted(random))
  }

  #[test]
  fn test_data_types() -> Result<()> {
    run_case(|case, random| case.test_data_types(random))
  }

  #[test]
  fn test_group_vint_overflow() -> Result<()> {
    run_case(|case, random| case.test_group_vint_overflow(random))
  }

  #[test]
  fn test_group_vint() -> Result<()> {
    run_case(|case, random| case.test_group_vint(random))
  }

  #[test]
  fn test_prefetch() -> Result<()> {
    run_case(|case, random| case.test_prefetch(random))
  }

  #[test]
  fn test_prefetch_on_slice() -> Result<()> {
    run_case(|case, random| case.test_prefetch_on_slice(random))
  }

  #[test]
  fn test_update_read_advice() -> Result<()> {
    run_case(|case, random| case.test_update_read_advice(random))
  }

  #[test]
  fn test_is_loaded() -> Result<()> {
    run_case(|case, random| case.test_is_loaded(random))
  }

  #[test]
  fn test_is_loaded_on_slice() -> Result<()> {
    run_case(|case, random| case.test_is_loaded_on_slice(random))
  }
}

fn run_case<F>(f: F) -> Result<()>
where
  F: FnOnce(&TestNRTCachingDirectory, &mut rand::prelude::StdRng) -> Result<()>,
{
  let mut random = random();
  let case = TestNRTCachingDirectory;
  f(&case, &mut random)
}
