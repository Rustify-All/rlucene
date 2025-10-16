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
use crate::core::index::BytesRef;
use crate::core::index::binary_doc_values_field_updates::BinaryDocValuesFieldUpdates;
use crate::core::index::buffered_updates::BufferedUpdates;
use crate::core::index::buffered_updates_stream::SegmentState;
use crate::core::index::doc_values_field_updates::{
    DocValuesFieldUpdates, DocValuesFieldUpdatesBase, DocValuesFieldUpdatesEnum,
    SingleValueDocValuesFieldUpdates, SingleValueDocValuesFieldUpdatesBase,
};
use crate::core::index::field_updates_buffer::FieldUpdatesBuffer;
use crate::core::index::fields::Fields;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::numeric_doc_values_field_updates::{
    NumericDocValuesFieldUpdates, SingleValueNumericDocValuesFieldUpdates,
};
use crate::core::index::postings_enum::NONE;
use crate::core::index::prefix_coded_terms::{PrefixCodedTerms, PrefixCodedTermsBuilder};
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::sorter::DocMap;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::search::query::Query;
use crate::core::store::directory::Directory;
use crate::core::util::access::SharedAccess;
use crate::core::util::accountable::Accountable;
use crate::core::util::bits::Bits;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::info_stream::{InfoStream, InfoStreamMT};
use crate::core::util::int_consumer::IntConsumer;
use crate::core::util::{ByteBlockPool, CounterEnum, StringHelper, ToInt};
use parking_lot::lock_api::ReentrantMutexGuard;
use parking_lot::{RawMutex, RawThreadId, ReentrantMutex};
use std::collections::HashMap;
use std::fmt;
use std::fmt::{Display, Formatter};
use std::hash::Hash;
use std::sync::atomic::AtomicI64;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;

// NOTE: we now apply this frozen packet immediately on creation, yet this
// process is heavy, and runs in multiple threads, and this compression
// is sizable (~8.3% of the original size), so it's important
// we run this before applying the deletes/updates.
// Query we often undercount (say 24 bytes), plus int.
const BYTES_PER_DEL_QUERY: i32 = 0;
/// Holds buffered deletes and updates by term or query, once pushed.
///
/// Pushed deletes/updates are write-once, so a more memory-efficient data structure is used
/// to store them. We don’t keep document IDs because they are applied on flush.
#[derive(Debug)]
pub(crate) struct FrozenBufferedUpdates {
    info_stream: InfoStreamMT,
    // Terms, in sorted order:
    pub delete_terms: PrefixCodedTerms,
    // Parallel array of deleted query, and the docIDUpto for each
    pub delete_queries: Vec<Arc<Query>>,
    delete_query_limits: Vec<i32>,
    // Counts down once all deletes/updates have been applied
    pub(crate) applied: AtomicBool,
    pub(crate) apply_lock: ReentrantMutex<()>,
    field_updates: HashMap<String, FieldUpdatesBuffer>,
    /// How many total documents were deleted/updated.
    pub(crate) total_del_count: AtomicI64,
    field_updates_count: i32,
    pub(crate) bytes_used: i32,
    // assigned by BufferedUpdatesStream once pushed
    del_gen: i64,
    // SegmentInfo ID in SegmentCommitInfo
    pub(crate) private_segment: Option<String>,
    id: String,
}

impl FrozenBufferedUpdates {
    pub fn new<C, B>(
        info_stream: InfoStreamMT,
        updates: &mut BufferedUpdates<C, B>,
        private_segment: Option<String>,
    ) -> Result<Self>
    where
        C: SharedAccess<CounterEnum>,
        B: SharedAccess<ByteBlockPool<C>>,
    {
        assert!(
            private_segment.is_none() || updates.delete_terms.is_empty(),
            "segment private packet should only have del queries"
        );

        let mut builder = PrefixCodedTermsBuilder::new();
        updates
            .delete_terms
            .for_each_ordered(|term, _| builder.add_term(term))?;
        let delete_terms = builder.finish();

        let (delete_queries, delete_query_limits) = {
            let mut queries = Vec::with_capacity(updates.delete_queries.len());
            let mut limits = Vec::with_capacity(updates.delete_queries.len());
            for (query, limit) in &updates.delete_queries {
                queries.push(query.clone());
                limits.push(*limit);
            }
            (queries, limits)
        };
        // TODO if a Term affects multiple fields, we could keep the updates
        // key'd by Term so that it maps to all fields it affects,
        // sorted by their docUpto, and traverse that Term only once,
        // applying the update to all fields that still need to be
        // updated.
        for value in updates.field_updates.values_mut() {
            value.finish()?
        }
        let field_updates = std::mem::take(&mut updates.field_updates);
        let field_updates_count = updates.num_field_updates.load(Ordering::Relaxed);

        // TODO: memory calculation not implemented
        let bytes_used = 0;
        if info_stream.enabled("BD") {
            let private_segment_msg = if private_segment.is_none() {
                "None".to_string()
            } else {
                format!("; private segment {}", private_segment.as_ref().unwrap())
            };
            info_stream.message(
                "BD",
                &format!(
                    "compressed {} to {} bytes ({:.2}%) for deletes/updates; private segment {}",
                    updates.ram_bytes_used()?,
                    bytes_used,
                    100.0 * bytes_used as f64 / updates.ram_bytes_used()? as f64,
                    private_segment_msg
                ),
            );
        }
        let id = StringHelper::id_to_string(Some(&StringHelper::random_id()));
        Ok(Self {
            info_stream: info_stream.clone(),
            delete_terms,
            delete_queries,
            delete_query_limits,
            applied: AtomicBool::new(false),
            apply_lock: ReentrantMutex::new(()),
            field_updates,
            total_del_count: AtomicI64::new(0),
            bytes_used,
            field_updates_count,
            del_gen: 0,
            private_segment,
            id,
        })
    }
    /// Tries to lock this buffered update instance
    pub(crate) fn try_lock(&self) -> Option<ReentrantMutexGuard<'_, RawMutex, RawThreadId, ()>> {
        self.apply_lock.try_lock()
    }
    /// locks this buffered update instance
    pub(crate) fn lock(&self) -> ReentrantMutexGuard<'_, RawMutex, RawThreadId, ()> {
        self.apply_lock.lock()
    }

    /// Returns `true` if this buffered updates instance has already been
    /// applied.
    pub(crate) fn is_applied(&self) -> bool {
        assert!(self.apply_lock.is_owned_by_current_thread());
        self.applied.load(Ordering::Relaxed)
    }
    /// Applies pending delete-by-term, delete-by-query and doc values updates to all segments in the index,
    /// Returning the number of new deleted or updated documents.
    pub(crate) fn apply<D>(
        &self,
        seg_states: &[SegmentState<D>],
        infos: &HashMap<String, SegmentCommitInfo<D>>,
    ) -> Result<i64>
    where
        D: Directory,
    {
        debug_assert!(
            self.apply_lock.is_owned_by_current_thread(),
            "apply() must be called while holding apply_lock"
        );

        if self.del_gen == -1 {
            return Err(LuceneError::illegal_argument(
                "gen is not yet set; call BufferedUpdatesStream.push first",
            ));
        }

        debug_assert!(!self.applied.load(Ordering::Relaxed));

        if let Some(ref private_segment) = self.private_segment {
            debug_assert_eq!(
                seg_states.len(),
                1,
                "private packet must target exactly one segment"
            );
            let seg0_id = &seg_states[0].rld.info_id;
            debug_assert!(
                private_segment.as_str() == seg0_id.as_str(),
                "privateSegment={} vs seg0={}",
                private_segment,
                seg0_id
            );
        }

        let mut count = self.apply_term_deletes(seg_states, infos)?;
        // totalDelCount += applyQueryDeletes(segStates);
        count += self.apply_doc_values_updates_all(seg_states)?;
        self.total_del_count.store(count, Ordering::Relaxed);
        Ok(count)
    }
    pub(crate) fn apply_doc_values_updates_all<D>(
        &self,
        seg_states: &[SegmentState<D>],
    ) -> Result<i64>
    where
        D: Directory,
    {
        if self.field_updates.is_empty() {
            return Ok(0);
        }

        let start = Instant::now();
        let mut update_count: i64 = 0;

        for seg_state in seg_states {
            if self.del_gen() < seg_state.del_gen {
                // segment is newer than this deletes packet
                continue;
            }
            if seg_state.rld.ref_count() == 1 {
                // This means we are the only remaining reference to this segment, meaning
                // it was merged away while we were running, so we can safely skip running
                // because we will run on the newly merged segment next:
                continue;
            }

            let is_segment_private_deletes = self.private_segment.is_some();

            if !self.field_updates.is_empty() {
                update_count += Self::apply_doc_values_updates(
                    seg_state,
                    &self.field_updates,
                    self.del_gen,
                    is_segment_private_deletes,
                )?;
            }
        }

        if self.info_stream.enabled("BD") {
            let elapsed_ms = start.elapsed().as_secs_f64() * 1_000.0;
            self.info_stream.message(
                "BD",
                &format!(
                    "applyDocValuesUpdates {:.1} msec for {} segments, {} field updates; {} new updates",
                    elapsed_ms,
                    seg_states.len(),
                    self.field_updates_count,
                    update_count
                ),
            );
        }

        Ok(update_count)
    }
    pub(crate) fn apply_doc_values_updates<D>(
        seg_state: &SegmentState<D>,
        updates: &HashMap<String, FieldUpdatesBuffer>,
        del_gen: i64,
        segment_private_deletes: bool,
    ) -> Result<i64>
    where
        D: Directory,
    {
        // TODO: we can process the updates per DV field, from last to first so that
        // if multiple terms affect same document for the same field, we add an update
        // only once (that of the last term). To do that, we can keep a bitset which
        // marks which documents have already been updated. So e.g. if term T1
        // updates doc 7, and then we process term T2 and it updates doc 7 as well,
        // we don't apply the update since we know T1 came last and therefore wins
        // the update.
        // We can also use that bitset as 'liveDocs' to pass to TermEnum.docs(), so
        // that these documents aren't even returned.
        let mut update_count: i64 = 0;

        let mut resolved_updates = Vec::new();

        let accept_docs = seg_state.rld.get_live_docs();
        for (update_field, value) in updates.iter() {
            let is_numeric = value.is_numeric();
            let mut iterator = value.iterator()?;
            let inner = seg_state.rld.inner.lock();
            let reader = inner.reader.as_ref().unwrap();
            let mut term_docs_iterator =
                TermDocsIterator::new(TermsProviderImpl2::new(reader), iterator.is_sorted_terms());
            let mut dv_updates = None;

            while let Some(buffered_update) = iterator.next_value()? {
                // TODO: we traverse the terms in update order (not term order) so that we
                // apply the updates in the correct order, i.e. if two terms update the
                // same document, the last one that came in wins, irrespective of the
                // terms lexical order.
                // we can apply the updates in terms order if we keep an updatesGen (and
                // increment it with every update) and attach it to each NumericUpdate. Note
                // that we cannot rely only on docIDUpto because an app may send two updates
                // which will get same docIDUpto, yet will still need to respect the order
                // those updates arrived.
                // TODO: we could at least *collate* by field?
                if let Some(doc_id_set_iterator) = term_docs_iterator.next_term(
                    buffered_update.term_field.as_str(),
                    buffered_update.term_value.as_ref().unwrap(),
                )? {
                    let limit = if del_gen == seg_state.del_gen {
                        debug_assert!(segment_private_deletes);
                        buffered_update.doc_upto
                    } else {
                        i32::MAX
                    };

                    let (long_value, binary_value) = if !buffered_update.has_value {
                        (-1, None)
                    } else {
                        (
                            buffered_update.numeric_value,
                            buffered_update.get_binary_value(),
                        )
                    };

                    if dv_updates.is_none() {
                        let max_doc = reader.max_doc()?;
                        let field = update_field.clone();
                        let v = if is_numeric {
                            if value.has_single_value() {
                                let sub = SingleValueNumericDocValuesFieldUpdates::new(
                                    value.get_numeric_value(0),
                                );
                                let sub_type = sub.sub_type();
                                let sub = SingleValueDocValuesFieldUpdates::new(
                                    sub, max_doc, del_gen, sub_type,
                                )?;
                                let sub_type = sub.sub_type();
                                DocValuesFieldUpdatesEnum::SingleValue(DocValuesFieldUpdates::new(
                                    max_doc, del_gen, field, sub_type, sub,
                                )?)
                            } else {
                                let sub = NumericDocValuesFieldUpdates::with_range(
                                    value.get_min_numeric(),
                                    value.get_max_numeric(),
                                )?;
                                let sub_type = sub.sub_type();
                                DocValuesFieldUpdatesEnum::Numeric(DocValuesFieldUpdates::new(
                                    max_doc, del_gen, field, sub_type, sub,
                                )?)
                            }
                        } else {
                            let sub = BinaryDocValuesFieldUpdates::new()?;
                            let sub_type = sub.sub_type();
                            DocValuesFieldUpdatesEnum::Binary(DocValuesFieldUpdates::new(
                                max_doc, del_gen, field, sub_type, sub,
                            )?)
                        };
                        resolved_updates.push(v);
                        dv_updates = Some(resolved_updates.len() - 1);
                    }

                    let update = resolved_updates.get_mut(dv_updates.unwrap()).unwrap();
                    let mut doc_id_consumer = IntConsumerImpl::new(
                        update,
                        buffered_update.has_value,
                        long_value,
                        binary_value,
                        is_numeric,
                    );
                    if seg_state.rld.sort_map.is_some() && segment_private_deletes {
                        // This segment was sorted on flush; we must apply seg-private deletes carefully in this
                        // case:
                        let sort_map = seg_state.rld.sort_map.as_ref().unwrap();
                        loop {
                            let doc = doc_id_set_iterator.next_doc()?;
                            if doc == NO_MORE_DOCS {
                                break;
                            }
                            if accept_docs.as_ref().is_none_or(|bits| bits.get(doc)) {
                                // The limit is in the pre-sorted doc space:
                                if sort_map.new_to_old(doc) < limit {
                                    doc_id_consumer.accept(doc)?;
                                    update_count += 1;
                                }
                            }
                        }
                    } else {
                        loop {
                            let doc = doc_id_set_iterator.next_doc()?;
                            if doc == NO_MORE_DOCS {
                                break;
                            }
                            if doc >= limit {
                                break; // no more docs that can be updated for this term
                            }
                            if accept_docs.as_ref().is_none_or(|bits| bits.get(doc)) {
                                doc_id_consumer.accept(doc)?;
                                update_count += 1;
                            }
                        }
                    }
                }
            }
        }

        // now freeze & publish:
        for mut upd in resolved_updates {
            if upd.any() {
                upd.finish()?;
                seg_state.rld.add_dv_update(upd)?;
            }
        }

        Ok(update_count)
    }
    fn apply_term_deletes<D>(
        &self,
        seg_states: &[SegmentState<D>],
        infos: &HashMap<String, SegmentCommitInfo<D>>,
    ) -> Result<i64>
    where
        D: Directory,
    {
        if self.delete_terms.size() == 0 {
            return Ok(0);
        }

        debug_assert!(self.private_segment.is_none());

        let start = Instant::now();
        let mut del_count: i64 = 0;

        for seg_state in seg_states {
            debug_assert!(
                seg_state.del_gen != self.del_gen(),
                "segState.delGen={} vs this.gen={}",
                seg_state.del_gen,
                self.del_gen()
            );
            if seg_state.del_gen > self.del_gen() {
                continue;
            }

            if seg_state.rld.ref_count() == 1 {
                continue;
            }

            let mut iter = self.delete_terms.iterator();
            let inner = seg_state.rld.inner.lock();
            let mut term_docs_iter = TermDocsIterator::new(
                TermsProviderImpl2::new(inner.reader.as_ref().unwrap()),
                true,
            );
            let field = std::mem::take(&mut iter.field);
            while let Some(del_term) = iter.next()? {
                if let Some(it) = term_docs_iter.next_term(&field, &del_term)? {
                    loop {
                        let doc_id = it.next_doc()?;
                        if doc_id == NO_MORE_DOCS {
                            break;
                        }
                        let info = infos.get(&seg_state.rld.info_id);
                        debug_assert!(info.is_some());
                        if seg_state.rld.delete(doc_id, info.unwrap())? {
                            del_count += 1;
                        }
                    }
                }
            }
        }

        if self.info_stream.enabled("BD") {
            let elapsed_ms = start.elapsed().as_secs_f64() * 1_000.0;
            self.info_stream.message(
                "BD",
                &format!(
                    "applyTermDeletes took {:.2} msec for {} segments and {} del terms; {} new deletions",
                    elapsed_ms,
                    seg_states.len(),
                    self.delete_terms.size(),
                    del_count
                ),
            );
        }

        Ok(del_count)
    }
    pub fn set_del_gen(&mut self, del_gen: i64) {
        debug_assert!(
            self.del_gen == -1,
            "del_gen was already previously set to {}",
            self.del_gen
        );
        self.del_gen = del_gen;
        self.delete_terms.set_del_gen(del_gen);
    }

    pub fn del_gen(&self) -> i64 {
        debug_assert!(self.del_gen != -1);
        self.del_gen
    }
    pub(crate) fn any(&self) -> bool {
        self.delete_terms.size() > 0
            || !self.delete_queries.is_empty()
            || self.field_updates_count > 0
    }
}
impl Display for FrozenBufferedUpdates {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "delGen={}", self.del_gen)?;
        if self.delete_terms.size() != 0 {
            write!(f, " unique deleteTerms={}", self.delete_terms.size())?;
        }
        if !self.delete_queries.is_empty() {
            write!(f, " numDeleteQueries={}", self.delete_queries.len())?;
        }
        if self.field_updates_count > 0 {
            write!(f, " fieldUpdates={}", self.field_updates_count)?;
        }
        if self.bytes_used != 0 {
            write!(f, " bytesUsed={}", self.bytes_used)?;
        }
        if let Some(ref seg) = self.private_segment {
            write!(f, " privateSegment={seg}")?;
        }

        Ok(())
    }
}
impl Hash for FrozenBufferedUpdates {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}
impl PartialEq for FrozenBufferedUpdates {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for FrozenBufferedUpdates {}

/// This struct helps iterating a term dictionary and consuming all the docs for each term.
/// It accepts a (field, value) tuple and returns a [`DocIdSetIterator`](crate::core::search::doc_id_set_iterator::DocIdSetIterator) if the field has an entry
/// for the given value.  
///
/// It has an optimized way of iterating the term dictionary if the terms are  
/// passed in sorted order and makes sure terms and postings are reused as much as possible.
pub(crate) struct TermDocsIterator<P>
where
    P: TermsProvider,
    <P as TermsProvider>::Terms:,
{
    provider: P,
    field: Option<String>,
    terms_enum: Option<<<P as TermsProvider>::Terms as Terms>::TermsEnum>,
    postings_enum: Option<Disi<P>>,
    sorted_terms: bool,
    // TODO: we should avoid copy here
    reader_term: Option<BytesRef<Vec<u8>>>,
    #[cfg(debug_assertions)]
    last_term: Option<BytesRef<Vec<u8>>>, // only set with debug_assert
}

impl<P> TermDocsIterator<P>
where
    P: TermsProvider,
{
    pub(crate) fn new(provider: P, sorted_terms: bool) -> Self {
        TermDocsIterator {
            provider,
            field: None,
            terms_enum: None,
            postings_enum: None,
            sorted_terms,
            reader_term: None,
            #[cfg(debug_assertions)]
            last_term: None,
        }
    }
    fn set_field(&mut self, mut field: Option<String>) -> Result<()> {
        if field.is_some() && self.field.as_ref() != field.as_ref() {
            self.field = field.take();

            match self.provider.terms(field.as_ref().unwrap())? {
                Some(terms) => {
                    let mut terms_enum = terms.iterator()?;
                    if self.sorted_terms {
                        // need to reset otherwise we fail the assertSorted below since we sort per field
                        debug_assert!(self.last_term.is_none());
                        self.reader_term = Option::from(terms_enum.next()?.unwrap().into_owned());
                    }
                    self.terms_enum = Some(terms_enum);
                },
                _ => {
                    self.terms_enum = None;
                },
            }
        }
        Ok(())
    }
    pub(crate) fn next_term(
        &mut self,
        field: &str,
        term: &BytesRef<Vec<u8>>,
    ) -> Result<Option<&mut Disi<P>>> {
        self.set_field(Some(field.to_string()))?;

        if let Some(terms_enum) = self.terms_enum.as_mut() {
            if self.sorted_terms {
                #[cfg(debug_assertions)]
                Self::assert_sorted(self.sorted_terms, &mut self.last_term, term);
                // in the sorted case we can take advantage of the "seeking forward" property
                // this allows us depending on the term dict impl to reuse data-structures internally
                // which speed up iteration over terms and docs significantly.
                let cmp = term
                    .cmp(self.reader_term.as_ref().expect("reader_term must be set"))
                    .to_int();

                return if cmp < 0 {
                    Ok(None) // requested term does not exist in this segment
                } else if cmp == 0 {
                    self.get_docs().map(Some)
                } else {
                    match terms_enum.seek_ceil(term)? {
                        SeekStatus::Found => self.get_docs().map(Some),
                        SeekStatus::NotFound => {
                            self.reader_term = Some(terms_enum.term()?.into_owned());
                            Ok(None)
                        },
                        SeekStatus::End => {
                            self.terms_enum = None;
                            Ok(None)
                        },
                    }
                };
            } else if terms_enum.seek_exact(term)? {
                return self.get_docs().map(Some);
            }
        }

        Ok(None)
    }
    #[cfg(debug_assertions)]
    fn assert_sorted(
        sorted_terms: bool,
        last_term: &mut Option<BytesRef<Vec<u8>>>,
        term: &BytesRef<Vec<u8>>,
    ) {
        debug_assert!(sorted_terms);
        if let Some(last) = last_term {
            debug_assert!(
                term >= last,
                "boom: {:?} last: {:?}",
                term.utf8_to_string(),
                last.utf8_to_string()
            );
        }
        *last_term = Some(BytesRef::deep_copy_of(term));
    }
    fn get_docs(&mut self) -> Result<&mut Disi<P>> {
        debug_assert!(self.terms_enum.is_some());

        let terms_enum = self.terms_enum.as_mut().unwrap();
        let postings_enum =
            terms_enum.postings_with_flags(self.postings_enum.take(), NONE as i32)?;
        self.postings_enum = Some(postings_enum);
        Ok(self.postings_enum.as_mut().unwrap())
    }
}
type Disi<P> = <<<P as TermsProvider>::Terms as Terms>::TermsEnum as TermsEnum>::PostingsEnum;

pub(crate) trait TermsProvider {
    type Terms: Terms;
    fn terms(&mut self, field: &str) -> Result<Option<Self::Terms>>;
}

pub(crate) struct TermsProviderImpl1<'a, F>
where
    F: Fields,
{
    pub(crate) fields: &'a F,
}
impl<'a, F> TermsProviderImpl1<'a, F>
where
    F: Fields,
{
    pub(crate) fn new(fields: &'a F) -> Self {
        Self { fields }
    }
}
impl<F> TermsProvider for TermsProviderImpl1<'_, F>
where
    F: Fields,
{
    type Terms = F::Terms;

    fn terms(&mut self, field: &str) -> Result<Option<Self::Terms>> {
        self.fields.terms(field)
    }
}

struct TermsProviderImpl2<'a, L>
where
    L: LeafReader,
{
    reader: &'a L,
}
impl<'a, L> TermsProviderImpl2<'a, L>
where
    L: LeafReader,
{
    pub(crate) fn new(reader: &'a L) -> Self {
        Self { reader }
    }
}
impl<'a, L> TermsProvider for TermsProviderImpl2<'a, L>
where
    L: LeafReader,
{
    type Terms = L::Terms;

    fn terms(&mut self, field: &str) -> Result<Option<Self::Terms>> {
        self.reader.terms(field)
    }
}

struct IntConsumerImpl<'a> {
    update: &'a mut DocValuesFieldUpdatesEnum,
    long_value: i64,
    binary_value: Option<&'a BytesRef<Vec<u8>>>,
    has_value: bool,
    is_numeric: bool,
}
impl<'a> IntConsumerImpl<'a> {
    fn new(
        update: &'a mut DocValuesFieldUpdatesEnum,
        has_value: bool,
        long_value: i64,
        binary_value: Option<&'a BytesRef<Vec<u8>>>,
        is_numeric: bool,
    ) -> Self {
        Self {
            update,
            has_value,
            long_value,
            binary_value,
            is_numeric,
        }
    }
}
impl<'a> IntConsumer for IntConsumerImpl<'a> {
    fn accept(&mut self, doc: i32) -> Result<()> {
        if !self.has_value {
            self.update.reset(doc)?;
        } else if self.is_numeric {
            self.update.add_value(doc, self.long_value)?;
        } else {
            self.update
                .add_binary_value(doc, self.binary_value.as_ref().unwrap())?;
        }
        Ok(())
    }
}
impl AsRef<FrozenBufferedUpdates> for FrozenBufferedUpdates {
    fn as_ref(&self) -> &FrozenBufferedUpdates {
        self
    }
}
