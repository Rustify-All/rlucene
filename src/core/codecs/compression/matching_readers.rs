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
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::merge_state::MergeState;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::info_stream::InfoStream;

/// Computes which segments have identical field name to number mappings,
/// which allows stored fields and term vectors in this codec to be bulk-merged.
pub struct MatchingReaders {
  /// [`SegmentReader`](crate::core::index::segment_reader::SegmentReader)'s that
  /// have identical field name/number mapping, so their stored fields
  /// and term vectors may be bulk merged.
  pub matching_readers: Vec<bool>,

  /// How many `matching_readers` are set.
  pub count: usize,
}

impl MatchingReaders {
  pub fn new<D, CR>(merge_state: &MergeState<D, CR>) -> Result<Self>
  where
    CR: CodecReader,
  {
    // If the i'th reader is a SegmentReader and has
    // identical fieldName -> number mapping, then this
    // array will be present at position i:
    let num_readers = merge_state.max_docs.len();
    let mut matching_readers = Vec::with_capacity(num_readers);
    let mut matched_count = 0;

    debug_assert_eq!(merge_state.field_infos.len(), num_readers);
    'next_reader: for field_infos in merge_state.field_infos.iter() {
      for fi in &**field_infos {
        match merge_state
          .merge_field_infos
          .field_info_by_number(fi.number)?
        {
          Some(other) if other.name == fi.name => continue,
          _ => {
            matching_readers.push(false);
            continue 'next_reader;
          },
        }
      }
      matching_readers.push(true);
      matched_count += 1;
    }
    if merge_state.info_stream.is_enabled("SM") {
      merge_state.info_stream.message(
        "SM",
        &format!("merge store matched_count={matched_count} vs {num_readers}"),
      )?;
      if matched_count != num_readers {
        merge_state.info_stream.message(
          "SM",
          &format!("{} non-bulk merges", num_readers - matched_count),
        )?;
      }
    }

    Ok(Self {
      matching_readers,
      count: matched_count,
    })
  }
}
