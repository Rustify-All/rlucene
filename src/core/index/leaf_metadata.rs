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
use std::sync::Arc;

use crate::core::search::sort::Sort;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::version::{LATEST, Version};

#[derive(Clone)]
#[cfg_attr(test, derive(Default))] // for test
pub struct LeafMetaData {
  /// The major version of the Lucene format used to create this segment.
  pub created_version_major: i32,
  /// The minimum version of Lucene that contributed to this segment.
  pub min_version: Option<Version>,
  /// The sort order of documents in this segment, if any.
  pub sort: Option<Arc<Sort>>,
  /// Indicates whether this segment contains documents written as blocks.
  pub has_blocks: bool,
}

impl LeafMetaData {
  /// Constructs a new `LeafMetaData` instance.
  pub fn new(
    created_version_major: i32,
    min_version: Option<Version>,
    sort: Option<Arc<Sort>>,
    has_blocks: bool,
  ) -> Result<Self> {
    if created_version_major > LATEST.major {
      return Err(LuceneError::illegal_argument(format!(
        "created_version_major is in the future: {created_version_major}"
      )));
    }
    if created_version_major < 6 {
      return Err(LuceneError::illegal_argument(format!(
        "created_version_major must be >= 6, got: {created_version_major}"
      )));
    }
    if created_version_major >= 7 && min_version.is_none() {
      return Err(LuceneError::illegal_argument(
        "min_version must be set when created_version_major is >= 7".to_string(),
      ));
    }

    Ok(Self {
      created_version_major,
      min_version,
      sort,
      has_blocks,
    })
  }

  pub fn get_created_version_major(&self) -> i32 {
    self.created_version_major
  }

  pub fn get_min_version(&self) -> &Option<Version> {
    &self.min_version
  }

  pub fn get_sort(&self) -> &Option<Arc<Sort>> {
    &self.sort
  }

  pub fn get_has_blocks(&self) -> bool {
    self.has_blocks
  }
}
