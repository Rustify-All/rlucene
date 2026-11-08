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
pub(crate) use crate::core::codecs::block_term_state::TermStateEnum;
use crate::core::index::index_reader::Identity;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader::{LRTermsEnum, LeafReader};
use crate::core::index::leaf_reader_context::{LeafReaderContext, TopParentMeta};
use crate::core::index::term::Term;
use crate::core::index::term_state::TermState;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::util::accountable::Accountable;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::ram_usage_estimator::size_of_slice;
use std::fmt::{Display, Formatter};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, LazyLock, OnceLock};

static EMPTY_TERM_STATE: LazyLock<Arc<TermStateEnum>> =
  LazyLock::new(|| Arc::new(EmptyTermState.into()));

/// Maintains an [`IndexReader`](crate::core::index::index_reader::IndexReader) [`TermState`] view over [`IndexReader`](crate::core::index::index_reader::IndexReader) instances
/// containing a single term. The [`TermStates`] doesn't track if the given [`TermState`]
/// objects are valid, neither if the [`TermState`] instances refer to the same terms in the
/// associated readers.
///
/// Share this object through `Arc<TermStates>` rather than cloning its state directory.
/// Lazy leaf results are published independently; failed lookups leave the slot uninitialized.
/// Registration, statistics updates and clearing still require exclusive access.
#[derive(Default)]
pub struct TermStates {
  identity: Identity,
  pub(crate) top_reader_context_identity: Identity,
  states: smallvec::SmallVec<[OnceLock<Arc<TermStateEnum>>; 1]>,
  term: Option<Arc<Term>>,
  doc_freq: i32,
  total_term_freq: i64,
}
impl TermStates {
  pub fn with_term<IRC>(term: Option<Arc<Term>>, context: &IRC) -> Result<Self>
  where
    IRC: IndexReaderContext,
  {
    debug_assert!(context.base().is_top_level);
    let num_leaves = context.leaves()?.len();
    let mut states = smallvec::SmallVec::with_capacity(num_leaves);
    for _ in 0..num_leaves {
      states.push(OnceLock::new())
    }
    Ok(TermStates {
      identity: Identity::new(),
      top_reader_context_identity: context.base().identity.clone(),
      doc_freq: 0,
      total_term_freq: 0,
      states,
      term,
    })
  }
  pub fn new<IRC>(context: &IRC) -> Result<Self>
  where
    IRC: IndexReaderContext,
  {
    Self::with_term(None, context)
  }

  pub fn was_built_for(&self, meta: &TopParentMeta) -> bool {
    self.was_built_for_id(&meta.id)
  }
  pub fn was_built_for_id(&self, id: &Identity) -> bool {
    self.top_reader_context_identity.eq(id)
  }
  pub fn with_state_and_stats<IRC>(
    context: &IRC,
    state: TermStateEnum,
    ord: usize,
    doc_freq: i32,
    total_term_freq: i64,
  ) -> Result<Self>
  where
    IRC: IndexReaderContext,
  {
    let mut ts = TermStates::new(context)?;
    ts.register_with_stats(state, ord, doc_freq, total_term_freq);
    Ok(ts)
  }

  /// Clears the TermStates internal state and removes all registered TermStates
  pub fn clear(&mut self) {
    self.doc_freq = 0;
    self.total_term_freq = 0;
    for slot in self.states.iter_mut() {
      slot.take();
    }
  }
  /// Registers and associates a TermState with an leaf ordinal.
  /// The leaf ordinal should be derived from a IndexReaderContext's leaf ord.
  pub fn register_with_stats(
    &mut self,
    state: TermStateEnum,
    ord: usize,
    doc_freq: i32,
    total_term_freq: i64,
  ) {
    self.register(state, ord);
    self.accumulate_statistics(doc_freq, total_term_freq);
  }
  /// Expert: Registers and associates a [`TermState`] with a leaf ordinal.
  /// The leaf ordinal should be derived from an [`IndexReaderContext`]'s leaf ord.
  /// Unlike [`register`](Self::register_with_stats), this method does **not** update term statistics.
  pub fn register<T>(&mut self, state: T, ord: usize)
  where
    T: Into<Arc<TermStateEnum>>,
  {
    debug_assert!(ord < self.states.len(), "ord {} out of bounds", ord);
    debug_assert!(
      self.states[ord].get().is_none(),
      "state for ord: {ord} already registered"
    );
    self.states[ord] = OnceLock::from(state.into());
  }
  /// Expert: Accumulate term statistics.
  pub fn accumulate_statistics(&mut self, doc_freq: i32, total_term_freq: i64) {
    debug_assert!(doc_freq >= 0);
    debug_assert!(total_term_freq >= 0);
    debug_assert!(
      (doc_freq as i64) <= total_term_freq,
      "doc_freq must not exceed total_term_freq"
    );
    self.doc_freq = self.doc_freq.wrapping_add(doc_freq);
    self.total_term_freq = self.total_term_freq.wrapping_add(total_term_freq);
  }
  /// Returns a [`PrepareState`] for a [`TermState`] for the given [`LeafReaderContext`].
  /// This may return `None` if some cheap checks help figure out that this term
  /// doesn't exist in this leaf. The [`Supplier`](crate::core::util::supplier::Supplier) may then also return `None`
  /// if the term doesn't exist.
  ///
  /// Calling this method typically schedules some I/O in the background, so it is
  /// recommended to retrieve [`PrepareState`]s across all required terms first before
  /// calling `resolve` on all [`PrepareState`]s so that the I/O for these terms
  /// can be performed in parallel.
  ///
  /// # Arguments
  /// * `ctx` - the [`LeafReaderContext`] to get the [`TermState`] for.
  ///
  /// # Returns
  /// A [`PrepareState`] for a [`TermState`].
  pub fn get<LR>(
    &self,
    ctx: &LeafReaderContext<LR>,
  ) -> Result<Option<PrepareState<LRTermsEnum<LR>>>>
  where
    LR: LeafReader,
  {
    let ctx_ord = ctx.ord;
    if ctx_ord >= self.states.len() {
      return Err(LuceneError::illegal_argument(format!(
        "leaf ordinal {ctx_ord} is out of bounds for {} states",
        self.states.len()
      )));
    }

    let Some(term) = self.term.as_ref() else {
      return Ok(if self.states[ctx_ord].get().is_none() {
        None
      } else {
        Some(PrepareState::Ready(ctx_ord))
      });
    };

    if self.states[ctx_ord].get().is_none() {
      let Some(terms) = ctx.reader().terms(term.field())? else {
        // Another reader may have published the same immutable leaf result.
        let _ = self.states[ctx_ord].set(Arc::clone(&EMPTY_TERM_STATE));
        return Ok(None);
      };

      let mut te = terms.iterator()?;
      let io_boolean_supplier = te.prepare_seek_exact(term.bytes())?;
      if io_boolean_supplier.is_none() {
        // Another reader may have published the same immutable leaf result.
        let _ = self.states[ctx_ord].set(Arc::clone(&EMPTY_TERM_STATE));
        return Ok(None);
      }
      return Ok(Some(PrepareState::Pending(term.clone(), ctx_ord, te)));
    }
    let state = self.states[ctx_ord]
      .get()
      .ok_or_else(|| LuceneError::illegal_state("term state is missing"))?;
    if matches!(state.as_ref(), TermStateEnum::Empty(_)) {
      Ok(None)
    } else {
      Ok(Some(PrepareState::Ready(ctx_ord)))
    }
  }
  /// Resolves a prepared lookup to a state borrowed from this [`TermStates`].
  /// Call `.cloned()` on the returned `Option` to hold the `Arc` independently.
  pub fn resolve<TE>(&self, state: &mut PrepareState<TE>) -> Result<Option<&Arc<TermStateEnum>>>
  where
    TE: TermsEnum,
  {
    match state {
      PrepareState::Ready(ord) => self.states.get(*ord).map(|slot| slot.get()).ok_or_else(|| {
        LuceneError::illegal_argument(format!(
          "leaf ordinal {ord} is out of bounds for {} states",
          self.states.len()
        ))
      }),

      PrepareState::Pending(term, ord, te) => {
        let state_count = self.states.len();
        let state_slot = self.states.get(*ord).ok_or_else(|| {
          LuceneError::illegal_argument(format!(
            "leaf ordinal {ord} is out of bounds for {state_count} states"
          ))
        })?;
        if state_slot.get().is_none() {
          let state = if te.get_prepare_seek_exact_status(term.bytes())? {
            Arc::new(te.term_state()?)
          } else {
            Arc::clone(&EMPTY_TERM_STATE)
          };
          // Publish only after successful I/O. A competing lookup may finish first;
          // all callers below use the state retained in this leaf's shared slot.
          let _ = state_slot.set(state);
        }
        let state = state_slot
          .get()
          .ok_or_else(|| LuceneError::illegal_state("resolved term state is missing"))?;
        if matches!(state.as_ref(), TermStateEnum::Empty(_)) {
          Ok(None)
        } else {
          Ok(Some(state))
        }
      },
    }
  }

  /// Returns the accumulated document frequency of all [`TermState`] instances
  /// passed to [`register`](Self::register).
  ///
  /// # Returns
  /// The accumulated document frequency of all [`TermState`] instances passed
  /// to [`register`](Self::register).
  pub fn doc_freq(&self) -> Result<i32> {
    if self.term.is_some() {
      return Err(LuceneError::illegal_state(
        "Cannot call docFreq() when needsStats=false",
      ));
    }
    Ok(self.doc_freq)
  }
  /// Returns the accumulated term frequency of all [`TermState`] instances
  /// passed to [`register`](Self::register_with_stats).
  ///
  /// # Returns
  /// The accumulated term frequency of all [`TermState`] instances passed
  /// to [`register`](Self::register_with_stats).
  pub fn total_term_freq(&self) -> Result<i64> {
    if self.term.is_some() {
      return Err(LuceneError::illegal_state(
        "Cannot call totalTermFreq() when needsStats=false",
      ));
    }
    Ok(self.total_term_freq)
  }
}
impl Display for TermStates {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    writeln!(f, "TermStates")?;
    for state in self.states.iter() {
      writeln!(
        f,
        "  state={}",
        match state.get() {
          None => "null".to_string(),
          Some(s) => format!("{}", s),
        }
      )?;
    }
    Ok(())
  }
}
impl PartialEq for TermStates {
  fn eq(&self, other: &Self) -> bool {
    self.identity == other.identity
  }
}

impl Eq for TermStates {}

impl Hash for TermStates {
  fn hash<H: Hasher>(&self, state: &mut H) {
    self.identity.hash(state);
  }
}

impl Accountable for TermStates {
  fn ram_bytes_used(&self) -> Result<i64> {
    let mut size = size_of_slice(&self.states);
    for state in self.states.iter().filter_map(OnceLock::get) {
      size = size.saturating_add(std::mem::size_of_val(state.as_ref()) as i64);
    }
    if let Some(term) = self.term.as_ref() {
      size = size
        .saturating_add(std::mem::size_of_val(term.as_ref()) as i64)
        .saturating_add(term.ram_bytes_used()?);
    }
    Ok(size)
  }
}

#[derive(Clone)]
pub struct EmptyTermState;

impl Display for EmptyTermState {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "TermState")
  }
}

impl TermState for EmptyTermState {
  fn copy_from(&mut self, _other: &Self) -> Result<()> {
    Ok(())
  }
}

pub enum PrepareState<TE> {
  Ready(usize),
  Pending(Arc<Term>, usize, TE),
}

pub fn build<IRC, T, SIM>(
  index_searcher: &IndexSearcher<IRC, SIM>,
  term: T,
  needs_stats: bool,
) -> Result<TermStates>
where
  SIM: Similarity,
  IRC: IndexReaderContext,
  T: Into<Arc<Term>>,
{
  let term = term.into();
  if needs_stats {
    build_with_stats(index_searcher, &term)
  } else {
    TermStates::with_term(Some(term), index_searcher.get_top_reader_context())
  }
}

/// Builds all leaf statistics without retaining the term after the lookup.
pub(crate) fn build_with_stats<IRC, SIM>(
  index_searcher: &IndexSearcher<IRC, SIM>,
  term: &Term,
) -> Result<TermStates>
where
  SIM: Similarity,
  IRC: IndexReaderContext,
{
  let context = index_searcher.get_top_reader_context();
  let mut per_reader_term_state = TermStates::with_term(None, context)?;

  let leaves = context.leaves()?;
  let mut pending_term_lookups = Vec::new();

  for ctx in leaves {
    let terms = ctx.reader().terms(term.field())?;
    match terms {
      None => {
        continue;
      },
      Some(t) => {
        let mut terms_enum = t.iterator()?;
        if terms_enum.prepare_seek_exact(term.bytes())?.is_some() {
          if pending_term_lookups.capacity() == 0 {
            pending_term_lookups.reserve_exact(leaves.len());
          }
          let ord = ctx.ord;
          if pending_term_lookups.len() <= ord {
            ArrayUtil::grow_with(&mut pending_term_lookups, ord + 1, || None)?;
          }
          pending_term_lookups[ord] = Some(terms_enum);
        }
      },
    }
  }

  for (ord, mut pending) in pending_term_lookups.into_iter().enumerate() {
    if let Some(ref mut terms_enum) = pending
      && terms_enum.get_prepare_seek_exact_status(term.bytes())?
    {
      per_reader_term_state.register_with_stats(
        terms_enum.term_state()?,
        ord,
        terms_enum.doc_freq()?,
        terms_enum.total_term_freq()?,
      );
    }
  }
  Ok(per_reader_term_state)
}
