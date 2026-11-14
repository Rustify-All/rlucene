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
use crate::core::index::field_infos::FieldInfos;
use crate::core::store::IOContext;
use crate::core::store::directory::Directory;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::info_stream::InfoStreamMT;
use std::sync::Arc;

/// Holder struct for common parameters used during write.
pub struct SegmentWriteState<'a, D> {
  /// InfoStream used for debugging messages.
  pub info_stream: InfoStreamMT,

  /// Directory where this segment will be written to.
  pub directory: &'a D,

  /// FieldInfos describing all fields in this segment.
  pub field_infos: Arc<FieldInfos>,

  /// Number of deleted documents set while flushing the segment.
  pub del_count_on_flush: i32,

  /// Number of only soft deleted documents set while flushing the segment.
  pub soft_del_count_on_flush: i32,

  /// FixedBitSet recording live documents; this is only set if there
  /// is one or more deleted documents.
  pub live_docs: Option<Arc<FixedBitSet>>,

  /// Unique suffix for any postings files written for this segment.
  /// PerFieldPostingsFormat sets this for each of the postings formats it
  /// wraps. If you create a new PostingsFormat, then any files you
  /// write/read must be derived using this suffix (use
  /// IndexFileNames::segment_file_name).
  ///
  /// Note: the suffix must be either empty, or be a textual suffix
  /// containing exactly two parts (separated by underscore), or be a
  /// base36 generation.
  pub segment_suffix: String,

  /// IOContext for all writes; you should pass this to
  /// [`Directory::create_output`].
  pub context: &'a IOContext,
}

impl<'a, D> SegmentWriteState<'a, D> {
  /// Creates an instance without a suffix.
  pub(crate) fn new(
    info_stream: InfoStreamMT,
    directory: &'a D,
    field_infos: Arc<FieldInfos>,
    context: &'a IOContext,
  ) -> Self {
    Self::with_suffix(info_stream, directory, field_infos, context, "")
  }

  /// Creates an instance with a segment suffix.
  pub(crate) fn with_suffix<S>(
    info_stream: InfoStreamMT,
    directory: &'a D,
    field_infos: Arc<FieldInfos>,
    context: &'a IOContext,
    segment_suffix: S,
  ) -> Self
  where
    S: AsRef<str> + Into<String>,
  {
    debug_assert!(Self::assert_segment_suffix(segment_suffix.as_ref()));
    Self {
      info_stream,
      directory,
      field_infos,
      context,
      segment_suffix: segment_suffix.into(),
      del_count_on_flush: 0,
      soft_del_count_on_flush: 0,
      live_docs: None,
    }
  }

  pub(crate) fn create_segment_output(
    &self,
    segment_name: &str,
    extension: &str,
  ) -> Result<D::IndexOutput>
  where
    D: Directory,
  {
    crate::core::index::IndexFileNames::with_segment_file_name(
      segment_name,
      &self.segment_suffix,
      extension,
      |name| self.directory.create_output(name, self.context),
    )
  }

  /// Create a shallow copy of SegmentWriteState with a new segment suffix.
  pub fn copy_with_suffix<S>(state: &SegmentWriteState<'a, D>, segment_suffix: S) -> Self
  where
    S: Into<String>,
  {
    let segment_suffix = segment_suffix.into();
    Self {
      info_stream: state.info_stream.clone(),
      directory: state.directory,
      field_infos: Arc::clone(&state.field_infos),
      context: state.context,
      segment_suffix,
      del_count_on_flush: state.del_count_on_flush,
      soft_del_count_on_flush: state.soft_del_count_on_flush,
      live_docs: state.live_docs.clone(),
    }
  }
  // currently only used by assert? clean up and make real check?
  // either it's a segment suffix (_X_Y) or it's a parsable generation
  fn assert_segment_suffix(suffix: &str) -> bool {
    if suffix.is_empty() {
      return true;
    }

    let mut parts = suffix.split('_');
    parts.next();
    match (parts.next(), parts.next()) {
      (Some(_), None) => true,
      (None, None) => i64::from_str_radix(suffix, 36).is_ok(),
      _ => false,
    }
  }
}
