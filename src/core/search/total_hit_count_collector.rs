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
use crate::core::index::index_reader::Identity;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::search::collector::Collector;
use crate::core::search::doc_id_stream::DocIdStream;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::similarities_impl::similarities::Similarity;

use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::search::leaf_collector::LeafCollector;
use crate::core::search::scorable::Scorable;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::weight::Weight;
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind, Result};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fmt::{Display, Formatter};
use std::sync::{Arc, OnceLock};

pub(crate) type EarlyTerminatedMap = HashMap<Identity, Arc<OnceLock<bool>>>;

/// Just counts the total number of hits. This is the collector behind [`IndexSearcher::count`](crate::core::search::index_searcher::IndexSearcher::count).
/// When the [`Weight`] implements [`Weight::count`], this collector will skip collecting segments.
pub struct TotalHitCountCollector {
  pub(crate) total_hit: i32,
  early_terminated_map: Option<Arc<Mutex<EarlyTerminatedMap>>>,
}
impl Default for TotalHitCountCollector {
  fn default() -> Self {
    Self::new()
  }
}

impl TotalHitCountCollector {
  pub fn new() -> Self {
    Self {
      total_hit: 0,
      early_terminated_map: None,
    }
  }
  pub(crate) fn new_leaf_partition_aware(
    early_terminated_map: Arc<Mutex<EarlyTerminatedMap>>,
  ) -> Self {
    Self {
      total_hit: 0,
      early_terminated_map: Some(early_terminated_map),
    }
  }
  fn try_count<W, IRC, SIM>(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    weight: Option<&W>,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<bool>
  where
    SIM: Similarity,
    IRC: IndexReaderContext,
    W: Weight<IRC, SIM> + ?Sized,
    IndexSearcher<IRC, SIM>: Sync,
  {
    let leaf_count = match weight {
      Some(w) => w.count(context, searcher)?,
      None => -1,
    };
    if leaf_count != -1 {
      self.total_hit += leaf_count;
      return Ok(true);
    }
    Ok(false)
  }
  /// Returns how many hits matched the search.
  pub fn get_total_hits(&self) -> i32 {
    self.total_hit
  }
}
impl Collector for TotalHitCountCollector {
  type LeafCollector<'a, IRC>
    = TotalHitCountLeafCollector<'a>
  where
    Self: 'a,
    IRC: IndexReaderContext + 'a;

  fn get_leaf_collector<'a, W, IRC, SIM>(
    &'a mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    weight: Option<&W>,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Self::LeafCollector<'a, IRC>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext,
    W: Weight<IRC, SIM> + ?Sized,
    IndexSearcher<IRC, SIM>: Sync,
  {
    if let Some(early_terminated_map) = &self.early_terminated_map {
      let (early_terminated, first) = {
        let mut early_terminated_map = early_terminated_map.lock();
        if let Some(early_terminated) = early_terminated_map.get(context.base().id()) {
          (early_terminated.clone(), false)
        } else {
          match early_terminated_map.entry(context.base().id().clone()) {
            Entry::Occupied(entry) => (entry.get().clone(), false),
            Entry::Vacant(entry) => {
              let first_early_terminated = Arc::new(OnceLock::new());
              entry.insert(first_early_terminated.clone());
              (first_early_terminated, true)
            },
          }
        }
      };

      if first {
        // The first thread for a given leaf gets to decide what the next threads targeting the
        // same leaf do.
        if self
          .try_count(context, weight, searcher)
          .inspect_err(|error| {
            if matches!(error.kind(), LuceneErrorKind::CollectionTerminated(_)) {
              let _ = early_terminated.set(true);
            }
          })?
        {
          let _ = early_terminated.set(true);
          return Err(LuceneError::collection_terminated(""));
        }
        let _ = early_terminated.set(false);
        return Ok(self.create_leaf_collector());
      }

      if *early_terminated.wait() {
        // The first partition of the same leaf early terminated, do the same for subsequent ones.
        return Err(LuceneError::collection_terminated(""));
      }

      // The first partition of the same leaf computed hit counts, do the same for subsequent ones.
      return Ok(self.create_leaf_collector());
    }

    if self.try_count(context, weight, searcher)? {
      return Err(LuceneError::collection_terminated(""));
    }
    Ok(self.create_leaf_collector())
  }

  fn score_mode(&self) -> ScoreMode {
    ScoreMode::CompleteNoScores
  }
}

impl TotalHitCountCollector {
  fn create_leaf_collector(&mut self) -> TotalHitCountLeafCollector<'_> {
    TotalHitCountLeafCollector::new(self)
  }
}

pub struct TotalHitCountLeafCollector<'a> {
  collector: &'a mut TotalHitCountCollector,
}

impl<'a> TotalHitCountLeafCollector<'a> {
  fn new(collector: &'a mut TotalHitCountCollector) -> Self {
    Self { collector }
  }
}

impl Display for TotalHitCountLeafCollector<'_> {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

impl<'a> LeafCollector for TotalHitCountLeafCollector<'a> {
  type CompetitiveIterator<'view>
    = crate::core::search::doc_id_set_iterator::EmptyDISI
  where
    Self: 'view;

  fn collect<SC: Scorable + ?Sized>(&mut self, _doc: i32, _scorer: &mut SC) -> Result<()> {
    self.collector.total_hit += 1;
    Ok(())
  }

  fn collect_stream<DS: DocIdStream + ?Sized, SC: Scorable + ?Sized>(
    &mut self,
    stream: &mut DS,
    mut scorer: &mut SC,
  ) -> Result<()> {
    self.collector.total_hit += stream.count(&mut scorer)?;
    Ok(())
  }
}
