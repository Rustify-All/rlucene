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
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::hnsw::hnsw_graph::HnswGraph;

/// A trait that provides an HNSW graph. It is useful when gathering multiple HNSW
/// graphs to bootstrap segment merging. The graph may be stored outside normal owned memory.
pub trait HnswGraphProvider {
  type HnswGraph: HnswGraph;

  /// Whether this reader corresponds to Java's `HnswGraphProvider` capability.
  fn is_hnsw_graph_provider(&self, _field: &str) -> bool {
    false
  }

  /// Return the stored HnswGraph for the given field.
  ///
  /// # Arguments
  /// * `field` - the field containing the graph
  ///
  /// # Returns
  /// the HnswGraph for the given field if found
  ///
  /// # Errors
  /// when reading potentially off-heap graph fails
  fn get_graph(&self, _field: &str) -> Result<Self::HnswGraph> {
    Err(LuceneError::unsupported_operation(""))
  }
}
