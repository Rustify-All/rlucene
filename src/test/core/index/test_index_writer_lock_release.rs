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
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::index_writer_config::OpenMode;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::util::lucene_test_case::{
  create_temp_dir_with_prefix, new_fs_directory, new_index_writer_config_with_analyzer, random,
};

#[allow(dead_code)] // for quick search
struct TestIndexWriterLockRelease;

#[test]
fn test_index_writer_lock_release() -> Result<()> {
  let mut random = random();
  let tmp = create_temp_dir_with_prefix("testLockRelease")?;
  let dir = new_fs_directory(&mut random, tmp)?;

  let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
    let mock = MockAnalyzer::new(&mut random);
    let mut iwc = IndexWriterConfig::with_analyzer(mock)?;
    iwc.set_open_mode(OpenMode::Append);

    if let Err(error) = IndexWriter::new(dir.clone(), iwc) {
      match &error {
        LuceneError::IndexNotFound(_) | LuceneError::NoSuchFile(_) => {},
        LuceneError::Io { source, .. } | LuceneError::IoWithPath { source, .. }
          if source.kind() == std::io::ErrorKind::NotFound => {},
        _ => return Err(error),
      }

      let mock = MockAnalyzer::new(&mut random);
      let mut iwc = IndexWriterConfig::with_analyzer(mock)?;
      iwc.set_open_mode(OpenMode::Append);
      let result = IndexWriter::new(dir.clone(), iwc);
      let error = result.as_ref().err();
      let expected = match error {
        None => true,
        Some(LuceneError::IndexNotFound(_) | LuceneError::NoSuchFile(_)) => true,
        Some(LuceneError::Io { source, .. } | LuceneError::IoWithPath { source, .. })
          if source.kind() == std::io::ErrorKind::NotFound =>
        {
          true
        },
        Some(_) => false,
      };
      assert!(
        expected,
        "expected FileNotFoundException or NoSuchFileException, got {error:?}"
      );
    }
    Ok(())
  }));

  match dir.close() {
    Err(error) => Err(error),
    Ok(()) => match result {
      Ok(result) => result,
      Err(payload) => std::panic::resume_unwind(payload),
    },
  }
}
