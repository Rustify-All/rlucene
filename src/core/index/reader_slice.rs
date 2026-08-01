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
/// Subreader slice from a parent composite reader.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReaderSlice {
  pub start: usize,
  pub length: i32,
  pub reader_index: i32,
}
impl ReaderSlice {
  pub fn new(start: usize, length: i32, reader_index: i32) -> Self {
    Self {
      start,
      length,
      reader_index,
    }
  }

  pub fn get_start(&self) -> usize {
    self.start
  }

  pub fn get_length(&self) -> i32 {
    self.length
  }

  pub fn get_reader_index(&self) -> i32 {
    self.reader_index
  }
}
