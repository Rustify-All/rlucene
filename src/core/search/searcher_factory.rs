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
use crate::core::index::index_reader::{
  IndexReader, IndexReaderContextKind, IndexReaderContextType,
};
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::util::error::lucene_error::Result;
use std::marker::PhantomData;
use std::sync::Arc;

#[cfg(test)]
use crate::test_framework::core::search::test_lru_query_cache::{
  CachingSearcherFactory, RandomSegmentSkippingPredicate,
};
#[cfg(test)]
use crate::test_framework::core::search::test_searcher_manager::{
  BlockingSearcherFactory, EvilSearcherFactory, TrackingSearcherFactory, WarmingSearcherFactory,
};

/// Factory used by `SearcherManager` to create new [`IndexSearcher`] instances. The default
/// implementation just creates an [`IndexSearcher`] with no custom behavior:
///
/// ```text
/// fn new_searcher<IR>(
///     reader: Arc<IR>,
///     _previous_reader: Option<&Arc<IR>>,
/// ) -> Result<IndexSearcher<IndexReaderContextType<Arc<IR>>>>
/// where
///     IR: IndexReader + 'static,
///     IR::ContextKind: IndexReaderContextKind<Arc<IR>>,
///     IndexReaderContextType<Arc<IR>>: Sync + 'static,
/// {
///     IndexSearcher::new(reader.get_context()?)
/// }
/// ```
///
/// You can pass your own factory instead if you want custom behavior, such as:
///
/// - Setting a custom scoring model: [`IndexSearcher::set_similarity`]
/// - Parallel per-segment search
/// - Returning custom subclasses of `IndexSearcher`, for example for distributed scoring
/// - Running queries to warm your [`IndexSearcher`] before it is used. Note: when using
///   near-realtime search you may also want to warm newly merged segments in the background,
///   outside of the reopen path.
///
/// @lucene.experimental
pub struct SearcherFactory<IR> {
  hook: SearcherFactoryHook<IR>,
}

pub(crate) enum SearcherFactoryHook<IR> {
  Default(PhantomData<fn() -> IR>),
  #[cfg(test)]
  Warming(WarmingSearcherFactory),
  #[cfg(test)]
  Blocking(BlockingSearcherFactory),
  #[cfg(test)]
  Evil(EvilSearcherFactory<Arc<IR>>),
  #[cfg(test)]
  Tracking(TrackingSearcherFactory<IR>),
  #[cfg(test)]
  CachingRandom(CachingSearcherFactory<RandomSegmentSkippingPredicate>),
}

impl<IR> Default for SearcherFactoryHook<IR> {
  fn default() -> Self {
    Self::Default(PhantomData)
  }
}

pub(crate) struct SearcherFactoryDefaults;

pub(crate) trait SearcherFactoryBase<IR>
where
  IR: IndexReader + 'static,
  IR::ContextKind: IndexReaderContextKind<Arc<IR>>,
  IndexReaderContextType<Arc<IR>>: Sync + 'static,
{
  fn new_searcher(
    &self,
    reader: Arc<IR>,
    previous_reader: Option<&Arc<IR>>,
  ) -> Result<IndexSearcher<IndexReaderContextType<Arc<IR>>>> {
    SearcherFactoryDefaults::new_searcher(reader, previous_reader)
  }
}

impl<IR> SearcherFactoryBase<IR> for SearcherFactoryHook<IR>
where
  IR: IndexReader + 'static,
  IR::ContextKind: IndexReaderContextKind<Arc<IR>>,
  IndexReaderContextType<Arc<IR>>: Sync + 'static,
{
  fn new_searcher(
    &self,
    reader: Arc<IR>,
    previous_reader: Option<&Arc<IR>>,
  ) -> Result<IndexSearcher<IndexReaderContextType<Arc<IR>>>> {
    match self {
      Self::Default(_) => SearcherFactoryDefaults::new_searcher(reader, previous_reader),
      #[cfg(test)]
      Self::Warming(factory) => factory.new_searcher(reader, previous_reader),
      #[cfg(test)]
      Self::Blocking(factory) => factory.new_searcher(reader, previous_reader),
      #[cfg(test)]
      Self::Evil(factory) => factory.new_searcher(reader, previous_reader),
      #[cfg(test)]
      Self::Tracking(factory) => factory.new_searcher(reader, previous_reader),
      #[cfg(test)]
      Self::CachingRandom(factory) => factory.new_searcher(reader, previous_reader),
    }
  }
}

impl<IR> Default for SearcherFactory<IR> {
  fn default() -> Self {
    Self::new()
  }
}

impl<IR> SearcherFactory<IR> {
  pub fn new() -> Self {
    Self::with_hook(SearcherFactoryHook::default())
  }

  pub(crate) fn with_hook(hook: SearcherFactoryHook<IR>) -> Self {
    Self { hook }
  }

  /// Returns a new [`IndexSearcher`] over the given reader.
  ///
  /// `previous_reader` is the reader previously used to create a new searcher. It is `None` if
  /// unknown or if `reader` is the initially opened reader. When it is present, it can be used to
  /// find newly opened segments compared to the new reader and warm the searcher before returning.
  pub fn new_searcher(
    &self,
    reader: Arc<IR>,
    previous_reader: Option<&Arc<IR>>,
  ) -> Result<IndexSearcher<IndexReaderContextType<Arc<IR>>>>
  where
    IR: IndexReader + 'static,
    IR::ContextKind: IndexReaderContextKind<Arc<IR>>,
    IndexReaderContextType<Arc<IR>>: Sync + 'static,
  {
    <SearcherFactoryHook<IR> as SearcherFactoryBase<IR>>::new_searcher(
      &self.hook,
      reader,
      previous_reader,
    )
  }
}

impl SearcherFactoryDefaults {
  /// Returns a new [`IndexSearcher`] over the given reader.
  ///
  /// # Parameters
  /// - `reader`: the reader to create a new searcher for.
  /// - `_previous_reader`: the reader previously used to create a new searcher. It is `None` if
  ///   unknown or if `reader` is the initially opened reader. When it is present, it can be used to
  ///   find newly opened segments compared to the new reader and warm the searcher before
  ///   returning.
  pub(crate) fn new_searcher<IR>(
    reader: Arc<IR>,
    _previous_reader: Option<&Arc<IR>>,
  ) -> Result<IndexSearcher<IndexReaderContextType<Arc<IR>>>>
  where
    IR: IndexReader + 'static,
    IR::ContextKind: IndexReaderContextKind<Arc<IR>>,
    IndexReaderContextType<Arc<IR>>: 'static,
  {
    IndexSearcher::new(reader.get_context()?)
  }
}
