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
use crate::core::codecs::Codecs;
use crate::core::document::document::Document;
use crate::core::document::stored_field::StoredField;
use crate::core::index::directory_reader;
use crate::core::index::index_reader::Identity;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::stored_fields::StoredFields;
use crate::core::store::directory::Directory;
use crate::core::store::random_access_input::RandomAccessInputWrapper;
use crate::core::store::{DataInput, IOContext, IndexInput};
use crate::core::util::HasIdentity;
use crate::core::util::clone::TryClone;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::test_framework::core::codecs::compressing::dummy::dummy_compressing_codec::DummyCompressingCodec;
use crate::test_framework::core::index::base_index_file_format_test_case::BaseIndexFileFormatTestCase;
use crate::test_framework::core::index::base_stored_fields_format_test_case::BaseStoredFieldsFormatTestCase;
use crate::test_framework::core::util::lucene_test_case::{
  new_directory, new_index_writer_config, random,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::Rng;
use rand::prelude::StdRng;
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[allow(dead_code)] // for quick search
pub struct TestLucene90StoredFieldsFormat;

impl BaseIndexFileFormatTestCase for TestLucene90StoredFieldsFormat {
  type Defaults = crate::test_framework::core::index::base_stored_fields_format_test_case::BaseStoredFieldsFormatTestCaseDefaults;

  fn get_codec(&self) -> Result<Codecs> {
    Ok(TestUtil::get_default_codec().into())
  }
}
fn run_case<F>(f: F) -> Result<()>
where
  F: FnOnce(&TestLucene90StoredFieldsFormat, &mut StdRng) -> Result<()>,
{
  let mut random = random();
  let case = TestLucene90StoredFieldsFormat;
  let codec_guard = case.set_up()?;
  let result = f(&case, &mut random);
  case.tear_down(codec_guard);
  result
}

impl BaseStoredFieldsFormatTestCase for TestLucene90StoredFieldsFormat {}
impl TestLucene90StoredFieldsFormatTests for TestLucene90StoredFieldsFormat {}

mod base_stored_fields_format_test_case_test {
  use crate::core::util::error::lucene_error::Result;
  use crate::test::core::codecs::lucene90::test_lucene90_stored_fields_format::run_case;
  use crate::test_framework::core::index::base_stored_fields_format_test_case::BaseStoredFieldsFormatTestCase;

  #[test]
  fn test_random_stored_fields() -> Result<()> {
    run_case(|case, random| case.test_random_stored_fields(random))
  }

  #[test]
  fn test_stored_fields_order() -> Result<()> {
    run_case(|case, random| case.test_stored_fields_order(random))
  }

  #[test]
  fn test_binary_field_offset_length() -> Result<()> {
    run_case(|case, random| case.test_binary_field_offset_length(random))
  }

  #[test]
  fn test_numeric_field() -> Result<()> {
    run_case(|case, random| case.test_numeric_field(random))
  }

  #[test]
  fn test_indexed_bit() -> Result<()> {
    run_case(|case, random| case.test_indexed_bit(random))
  }

  #[test]
  fn test_read_skip() -> Result<()> {
    run_case(|case, random| case.test_read_skip(random))
  }

  #[test]
  fn test_empty_docs() -> Result<()> {
    run_case(|case, random| case.test_empty_docs(random))
  }
  #[test]
  fn test_concurrent_reads() -> Result<()> {
    run_case(|case, random| case.test_concurrent_reads(random))
  }

  #[test]
  fn test_write_read_merge() -> Result<()> {
    run_case(|case, random| case.test_write_read_merge(random))
  }

  #[test]
  fn test_merge_filter_reader() -> Result<()> {
    run_case(|case, random| case.test_merge_filter_reader(random))
  }

  #[cfg(feature = "nightly")]
  #[test]
  #[ignore = "nightly"]
  fn test_big_documents() -> Result<()> {
    run_case(|case, random| case.test_big_documents(random))
  }

  #[test]
  fn test_bulk_merge_with_deletes() -> Result<()> {
    run_case(|case, random| case.test_bulk_merge_with_deletes(random))
  }

  #[test]
  fn test_mismatched_fields() -> Result<()> {
    run_case(|case, random| case.test_mismatched_fields(random))
  }

  #[test]
  fn test_random_stored_fields_with_index_sort() -> Result<()> {
    run_case(|case, random| case.test_random_stored_fields_with_index_sort(random))
  }

  #[test]
  fn test_line_file_docs() -> Result<()> {
    run_case(|case, random| case.test_line_file_docs(random))
  }
}

#[test]
fn test_skip_redundant_prefetches() -> Result<()> {
  run_case(|case, random| case.test_skip_redundant_prefetches(random))
}
pub(super) trait TestLucene90StoredFieldsFormatTests:
  BaseStoredFieldsFormatTestCase
{
  fn test_skip_redundant_prefetches<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let orig_dir = new_directory(random)?;
    let counter = Arc::new(AtomicUsize::new(0));
    let dir = Arc::new(CountingPrefetchDirectory::new(orig_dir, counter.clone()));

    let mut iwc = IndexWriterConfig::new()?;
    iwc.set_codec(DummyCompressingCodec::new(1 << 10, 2, false, 16)?);
    let writer = IndexWriter::new(dir.clone(), iwc)?;

    for _ in 0..100 {
      let mut doc = Document::new();
      doc.add(StoredField::from_string(
        "content",
        TestUtil::random_simple_string(random),
      )?);
      writer.add_document(doc)?;
    }
    writer.force_merge(1)?;
    writer.close()?;

    let reader = directory_reader::open(dir)?;
    let mut stored_fields = reader.stored_fields()?;

    counter.store(0, Ordering::SeqCst);
    assert_eq!(0, counter.load(Ordering::SeqCst));
    stored_fields.prefetch(0)?;
    assert_eq!(1, counter.load(Ordering::SeqCst));
    stored_fields.prefetch(1)?;
    assert_eq!(1, counter.load(Ordering::SeqCst));
    stored_fields.prefetch(15)?;
    assert_eq!(2, counter.load(Ordering::SeqCst));
    stored_fields.prefetch(14)?;
    assert_eq!(2, counter.load(Ordering::SeqCst));
    stored_fields.prefetch(1)?;
    assert_eq!(2, counter.load(Ordering::SeqCst));

    reader.close()?;
    Ok(())
  }
}

pub struct CountingPrefetchDirectory<D> {
  in_: D,
  count: Arc<AtomicUsize>,
  id: Identity,
}
impl<D> CountingPrefetchDirectory<D>
where
  D: Directory,
{
  pub fn new(in_: D, count: Arc<AtomicUsize>) -> Self {
    Self {
      in_,
      count,
      id: Identity::new(),
    }
  }
}

impl<D> Display for CountingPrefetchDirectory<D>
where
  D: Directory,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

impl<D> CloseableRef for CountingPrefetchDirectory<D>
where
  D: Directory,
{
  fn close(&self) -> Result<()> {
    self.in_.close()
  }
}

impl<D> HasIdentity for CountingPrefetchDirectory<D>
where
  D: Directory,
{
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl<D> Directory for CountingPrefetchDirectory<D>
where
  D: Directory,
{
  fn list_all(&self) -> Result<Vec<String>> {
    self.in_.list_all()
  }

  fn delete_file(&self, name: &str) -> Result<()> {
    self.in_.delete_file(name)
  }

  fn file_length(&self, name: &str) -> Result<usize> {
    self.in_.file_length(name)
  }

  fn create_output(&self, name: &str, context: &IOContext) -> Result<Self::IndexOutput> {
    self.in_.create_output(name, context)
  }

  type IndexOutput = D::IndexOutput;

  fn create_temp_output(
    &self,
    prefix: &str,
    suffix: &str,
    context: &IOContext,
  ) -> Result<Self::IndexOutput> {
    self.in_.create_temp_output(prefix, suffix, context)
  }

  fn sync(&self, names: &[String]) -> Result<()> {
    self.in_.sync(names)
  }

  fn sync_metadata(&self) -> Result<()> {
    self.in_.sync_metadata()
  }

  fn rename(&self, source: &str, dest: &str) -> Result<()> {
    self.in_.rename(source, dest)
  }

  type IndexInput = CountingPrefetchIndexInput<D::IndexInput>;

  fn open_input(&self, name: &str, context: &IOContext) -> Result<Self::IndexInput> {
    let v = self.in_.open_input(name, context)?;
    Ok(CountingPrefetchIndexInput::new(self.count.clone(), v))
  }

  type Lock = D::Lock;

  fn obtain_lock(&self, name: &str) -> Result<Self::Lock> {
    self.in_.obtain_lock(name)
  }

  fn get_pending_deletions(&self) -> Result<HashSet<String>> {
    self.in_.get_pending_deletions()
  }
}

pub struct CountingPrefetchIndexInput<I> {
  count: Arc<AtomicUsize>,
  in_: I,
}
impl<I> CountingPrefetchIndexInput<I>
where
  I: IndexInput,
{
  pub fn new(count: Arc<AtomicUsize>, in_: I) -> CountingPrefetchIndexInput<I> {
    Self { count, in_ }
  }
}

impl<I> crate::core::util::close::CloseableRef for CountingPrefetchIndexInput<I>
where
  I: IndexInput,
{
  fn close(&self) -> Result<()> {
    self.in_.close()
  }
}

impl<I> DataInput for CountingPrefetchIndexInput<I>
where
  I: IndexInput,
{
  fn read_byte(&mut self) -> Result<u8> {
    self.in_.read_byte()
  }

  fn read_bytes(&mut self, b: &mut [u8], offset: usize, len: usize) -> Result<()> {
    self.in_.read_bytes(b, offset, len)
  }

  fn read_group_vint(&mut self, _dst: &mut [i32], _offset: usize) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn skip_bytes(&mut self, num_bytes: i64) -> Result<()> {
    IndexInput::skip_bytes(&mut self.in_, num_bytes)
  }
}

impl<I> Display for CountingPrefetchIndexInput<I>
where
  I: IndexInput,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

impl<I> TryClone for CountingPrefetchIndexInput<I>
where
  I: IndexInput,
{
  fn try_clone(&self) -> Result<Self>
  where
    Self: Sized,
  {
    Ok(CountingPrefetchIndexInput::new(
      self.count.clone(),
      self.in_.try_clone()?,
    ))
  }
}

impl<I> IndexInput for CountingPrefetchIndexInput<I>
where
  I: IndexInput<IndexInput = I>,
{
  type IndexInput = CountingPrefetchIndexInput<I>;

  fn get_file_pointer(&self) -> Result<usize> {
    self.in_.get_file_pointer()
  }

  fn seek(&mut self, pos: usize) -> Result<()> {
    self.in_.seek(pos)
  }

  fn length(&self) -> Result<usize> {
    self.in_.length()
  }

  fn slice(
    &self,
    slice_description: &str,
    offset: usize,
    length: usize,
  ) -> Result<Self::IndexInput> {
    let slice = self.in_.slice(slice_description, offset, length)?;
    Ok(CountingPrefetchIndexInput::new(self.count.clone(), slice))
  }

  type RandomAccessSlice = RandomAccessInputWrapper<CountingPrefetchIndexInput<I>>;

  fn random_access_slice(&self, offset: usize, length: usize) -> Result<Self::RandomAccessSlice> {
    Ok(RandomAccessInputWrapper::new(self.slice(
      "randomaccess",
      offset,
      length,
    )?))
  }

  fn prefetch(&mut self, pos: usize, len: usize) -> Result<()> {
    self.in_.prefetch(pos, len)?;
    self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    Ok(())
  }
}

mod base_index_file_format_test_case_test {
  use super::run_case;
  use crate::core::util::error::lucene_error::Result;
  use crate::test_framework::core::index::base_index_file_format_test_case::BaseIndexFileFormatTestCase;

  #[test]
  fn test_merge_stability() -> Result<()> {
    run_case(|case, random| case.test_merge_stability(random))
  }

  #[test]
  fn test_multi_close() -> Result<()> {
    run_case(|case, random| case.test_multi_close(random))
  }

  #[test]
  fn test_random_exceptions() -> Result<()> {
    run_case(|case, random| case.test_random_exceptions(random))
  }

  #[test]
  fn test_check_integrity_reads_all_bytes() -> Result<()> {
    run_case(|case, random| case.test_check_integrity_reads_all_bytes(random))
  }
}
