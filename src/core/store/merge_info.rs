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
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct MergeInfo {
  total_max_doc: i32,
  estimated_merge_bytes: i64,
  is_external: bool,
  merge_max_num_segments: i32,
}

impl MergeInfo {
  pub fn new(
    total_max_doc: i32,
    estimated_merge_bytes: i64,
    is_external: bool,
    merge_max_num_segments: i32,
  ) -> MergeInfo {
    Self {
      total_max_doc,
      estimated_merge_bytes,
      is_external,
      merge_max_num_segments,
    }
  }

  pub fn get_total_max_doc(&self) -> i32 {
    self.total_max_doc
  }

  pub fn get_estimated_merge_bytes(&self) -> i64 {
    self.estimated_merge_bytes
  }

  pub fn get_is_external(&self) -> bool {
    self.is_external
  }

  pub fn get_merge_max_num_segments(&self) -> i32 {
    self.merge_max_num_segments
  }
}
