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
pub struct FlushInfo {
  num_docs: i32,
  estimated_segment_size: i64,
}

impl FlushInfo {
  pub fn new(num_docs: i32, estimated_segment_size: i64) -> FlushInfo {
    Self {
      num_docs,
      estimated_segment_size,
    }
  }

  pub fn get_num_docs(&self) -> i32 {
    self.num_docs
  }

  pub fn get_estimated_segment_size(&self) -> i64 {
    self.estimated_segment_size
  }
}
