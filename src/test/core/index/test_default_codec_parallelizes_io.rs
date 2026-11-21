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
use crate::core::index::directory_reader::{self, DirectoryReader};
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::standard_directory_reader::StandardDirectoryReader;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::store::byte_buffers_directory::ByteBuffersDirectory;
use crate::core::store::single_instance_lock_factory::SingleInstanceLockFactory;
use crate::core::util::IOUtils;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::io_boolean_supplier::IOBooleanSupplier;
use crate::test_framework::core::store::serial_io_counting_directory::SerialIOCountingDirectory;
use crate::test_framework::core::util::line_file_docs::LineFileDocs;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, get_only_leaf_reader, new_log_merge_policy_with_cfs, random,
};
use crate::test_framework::core::util::test_util::TestUtil;
use parking_lot::Mutex;
use rand::RngExt;
use rand::prelude::StdRng;
use std::sync::{Arc, LazyLock};

type InnerDirectory = ByteBuffersDirectory<SingleInstanceLockFactory>;
type CountingDirectory = SerialIOCountingDirectory<Arc<InnerDirectory>>;
type TestReader = StandardDirectoryReader<CountingDirectory>;

struct TestDefaultCodecParallelizesIO {
  dir: Arc<CountingDirectory>,
  reader: TestReader,
}

static CONTEXT: LazyLock<Mutex<TestDefaultCodecParallelizesIO>> = LazyLock::new(|| {
  let mut random = random();
  Mutex::new(
    TestDefaultCodecParallelizesIO::before_class(&mut random)
      .expect("failed to initialize TestDefaultCodecParallelizesIO"),
  )
});

impl TestDefaultCodecParallelizesIO {
  fn before_class(random: &mut StdRng) -> Result<Self> {
    let bb_dir = Arc::new(ByteBuffersDirectory::new());
    let mut docs = LineFileDocs::new(random)?;
    let writer = match (|| -> Result<_> {
      let mut config = IndexWriterConfig::new()?;
      // Disable CFS, this test needs to know about files that are open with the
      // RANDOM_PRELOAD advice, which CFS doesn't allow us to detect.
      config
        .set_use_compound_file(false)
        .set_merge_policy(new_log_merge_policy_with_cfs(random, false)?);
      config.set_codec(TestUtil::get_default_codec());
      IndexWriter::new(bb_dir.clone(), config)
    })() {
      Ok(writer) => writer,
      Err(error) => {
        docs.close();
        return Err(error);
      },
    };

    let result = (|| -> Result<()> {
      let num_docs = at_least(random, 10_000);
      for _ in 0..num_docs {
        let doc = docs.next_doc()?;
        writer.add_document(doc)?;
      }
      writer.force_merge(1)
    })();
    let result = IOUtils::use_or_suppress_result(result, writer.close());
    docs.close();
    result?;

    let dir = Arc::new(SerialIOCountingDirectory::new(bb_dir));
    let reader = directory_reader::open(dir.clone())?;
    Ok(Self { dir, reader })
  }

  /// Simulate term lookup in a BooleanQuery.
  fn test_terms_seek_exact(&self) -> Result<()> {
    let prev_count = self.dir.count();

    let leaf_reader = get_only_leaf_reader(&self.reader)?;
    let terms = leaf_reader
      .terms("body")?
      .expect("body must have indexed terms");
    let term_values = ["a", "which", "the", "for", "he"];
    let mut suppliers = Vec::with_capacity(term_values.len());
    for term_value in term_values {
      let mut terms_enum = terms.iterator()?;
      let term = BytesRef::<Vec<u8>>::from_string(term_value);
      if let Some(seek) = TermsEnum::prepare_seek_exact(&mut terms_enum, &term)? {
        suppliers.push(Some((terms_enum, term, seek)));
      } else {
        suppliers.push(None);
      }
    }
    let mut non_null_io_suppliers = 0;
    for (cursor, target, seek) in suppliers.iter_mut().flatten() {
      non_null_io_suppliers += 1;
      seek.get(cursor, target)?;
    }

    assert!(non_null_io_suppliers > 0);
    let new_count = self.dir.count();
    assert!(new_count - prev_count > 0);
    assert!(new_count - prev_count < non_null_io_suppliers);
    Ok(())
  }

  /// Simulate stored fields retrieval.
  fn test_stored_fields(&self, random: &mut StdRng) -> Result<()> {
    let prev_count = self.dir.count();

    let leaf_reader = get_only_leaf_reader(&self.reader)?;
    let mut stored_fields = leaf_reader.stored_fields()?;
    let mut docs = [0; 20];
    for doc in &mut docs {
      *doc = random.random_range(0..leaf_reader.max_doc()?);
      stored_fields.prefetch(*doc)?;
    }
    for doc in docs {
      stored_fields.document(doc)?;
    }

    let new_count = self.dir.count();
    assert!(new_count - prev_count > 0);
    assert!(new_count - prev_count < docs.len() as i64);
    Ok(())
  }
}

#[test]
fn test_terms_seek_exact() -> Result<()> {
  CONTEXT.lock().test_terms_seek_exact()
}

#[test]
fn test_stored_fields() -> Result<()> {
  let case = CONTEXT.lock();
  let mut random = random();
  case.test_stored_fields(&mut random)
}
