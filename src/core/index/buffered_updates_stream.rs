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
use crate::core::index::frozen_buffered_updates::FrozenBufferedUpdates;
use crate::core::index::index_reader::Identity;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::readers_and_updates;
use crate::core::index::readers_and_updates::ReadersAndUpdates;
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_reader::DefaultLeafReader;
use crate::core::store::IO_CONTEXT_DEFAULT;

use crate::core::store::directory::Directory;
use crate::core::util::IOUtils;
use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::info_stream::{InfoStream, InfoStreamMT};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::fmt::Display;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

/// Tracks the stream of [`FrozenBufferedUpdates`]. When [`DocumentsWriterPerThread`](crate::core::index::documents_writer_per_thread::DocumentsWriterPerThread) flushes, its
/// buffered deletes and updates are appended to this stream and immediately resolved (to actual
/// doc IDs, per segment) using the indexing thread that triggered the flush for concurrency. When a
/// merge kicks off, we synchronize to ensure all resolving packets complete. We also apply updates
/// to all segments when an NRT reader is pulled, on commit/close, or when too many deletes or
/// updates are buffered and must be flushed (by RAM usage or by count).
///
/// Each packet is assigned a generation, and each flushed or merged segment is also assigned a
/// generation, so we can track which buffered-deletes packets to apply to any given segment.
pub(crate) struct BufferedUpdatesStream {
  inner: Mutex<BufferedUpdatesStreamInner>,
  bytes_used: AtomicI64,
  finished_segments: FinishedSegments,
}
pub(crate) struct BufferedUpdatesStreamInner {
  updates: HashMap<Identity, Arc<FrozenBufferedUpdates>>,
  // Starts at 1 so that SegmentInfos that have never had
  // deletes applied (whose bufferedDelGen defaults to 0)
  // will be correct:
  next_gen: i64,
}
impl BufferedUpdatesStream {
  pub(crate) fn new(info_stream: InfoStreamMT) -> Self {
    Self {
      inner: Mutex::new(BufferedUpdatesStreamInner {
        updates: HashMap::new(),
        next_gen: 1,
      }),
      bytes_used: AtomicI64::new(0),
      finished_segments: FinishedSegments::new(info_stream),
    }
  }
  // Appends a new packet of buffered deletes to the stream,
  // setting its generation:
  pub(crate) fn push(
    &self,
    mut packet: FrozenBufferedUpdates,
  ) -> Result<(i64, Arc<FrozenBufferedUpdates>)> {
    // The insert operation must be atomic. If we let threads increment the gen
    // and push the packet afterwards we risk that packets are out of order.
    // With DWPT this is possible if two or more flushes are racing for pushing
    // updates. If the pushed packets get our of order would loose documents
    // since deletes are applied to the wrong segments.
    let mut inner = self.inner.lock();
    packet.set_del_gen(inner.next_gen);
    inner.next_gen += 1;
    debug_assert!(packet.any());
    debug_assert!(self.check_delete_stats(&inner));

    let bytes_used = packet.bytes_used;
    let del_gen = packet.del_gen();
    let v = Arc::new(packet);
    inner.updates.insert(v.id.clone(), v.clone());
    self
      .bytes_used
      .fetch_add(bytes_used as i64, Ordering::SeqCst);
    {
      if self.finished_segments.info_stream.is_enabled("BD") {
        let packet_msg = v.to_string();
        let count = inner.updates.len();
        let used_mb = self.bytes_used.load(Ordering::SeqCst) as f64 / 1024.0 / 1024.0;
        self.finished_segments.info_stream.message(
          "BD",
          &format!(
            "push new packet ({packet_msg}), packetCount={count}, bytesUsed={used_mb:.3} MB"
          ),
        )?;
      }
    }
    debug_assert!(self.check_delete_stats(&inner));
    Ok((del_gen, v))
  }
  pub(crate) fn get_pending_updates_count(&self) -> usize {
    let inner = self.inner.lock();
    inner.updates.len()
  }
  /// Only used by IW.rollback
  #[allow(dead_code)]
  pub(crate) fn clear(&self) {
    let mut inner = self.inner.lock();
    inner.updates.clear();
    inner.next_gen = 1;
    self.finished_segments.clear();
    self.bytes_used.store(0, Ordering::SeqCst);
  }
  pub(crate) fn any(&self) -> bool {
    self.bytes_used.load(Ordering::SeqCst) != 0
  }
  /// Waits for all in-flight packets, which are being resolved concurrently by indexing threads, to finish.
  ///
  /// Returns `true` if there were any new deletes or updates.
  ///
  /// This is called during refresh and commit.
  pub(crate) fn wait_apply_all<D>(&self, writer: &IndexWriter<D>) -> Result<()>
  where
    D: Directory + 'static,
  {
    let wait_for = {
      let inner = self.inner.lock();
      inner.updates.values().cloned().collect::<Vec<_>>()
    };
    self.wait_apply(wait_for, writer)
  }
  /// Returns true if this delGen is still running.
  pub(crate) fn still_running(&self, del_gen: i64) -> bool {
    self.finished_segments.still_running(del_gen)
  }

  pub(crate) fn finished_segment(&self, del_gen: i64) -> Result<()> {
    self.finished_segments.finished_segment(del_gen)
  }
  /// Called by indexing threads once they are fully done resolving all deletes for the provided `del_gen`.
  /// We track completed deletion generations and record the maximum `del_gen` for which all prior generations,
  /// inclusive, are completed, so that it’s safe for doc values updates to apply and write.
  pub(crate) fn finished(&self, packet: &FrozenBufferedUpdates) -> Result<()> {
    debug_assert!(!packet.applied.load(Ordering::SeqCst), "packet={packet}");
    packet.applied.store(true, Ordering::SeqCst);

    let mut inner = self.inner.lock();
    inner.updates.remove(&packet.id);

    let bytes = packet.bytes_used as i64;
    self.bytes_used.fetch_sub(bytes, Ordering::SeqCst);

    self.finished_segment(packet.del_gen())?;
    Ok(())
  }
  /// All frozen packets up to and including this del gen are guaranteed to be finished.
  pub fn get_completed_del_gen(&self) -> i64 {
    self.finished_segments.get_completed_del_gen()
  }
  /// Waits only for those in-flight packets that apply to these merge segments.
  /// This is called when a merge needs to finish and must ensure all deletes to the merging segments are resolved.
  pub(crate) fn wait_apply_for_merge<D>(
    &self,
    merge_infos_id: &[Arc<str>],
    writer: &IndexWriter<D>,
  ) -> Result<()>
  where
    D: Directory + 'static,
  {
    let mut max_del_gen = i64::MIN;
    {
      let writer_inner = writer.inner.lock();
      for info in merge_infos_id {
        let info = writer_inner
          .segment_infos
          .index_of(info.as_ref())
          .ok_or_else(|| {
            LuceneError::illegal_argument(
              "could not find merge's segment from IndexWriter's segment_infos",
            )
          })?;
        max_del_gen = max_del_gen.max(info.get_buffered_deletes_gen());
      }
    }

    let wait_for = {
      let inner = self.inner.lock();
      let mut packets = Vec::with_capacity(inner.updates.len());

      for packet in inner.updates.values() {
        if packet.del_gen() <= max_del_gen {
          // We must wait for this packet before finishing the merge because its
          // deletes apply to a subset of the segments being merged.
          packets.push(packet.clone());
        }
      }

      packets
    };

    if self.finished_segments.info_stream.is_enabled("BD") {
      self.finished_segments.info_stream.message(
        "BD",
        &format!(
          "waitApplyForMerge: {} packets, {} merging segments",
          wait_for.len(),
          merge_infos_id.len()
        ),
      )?;
    }

    self.wait_apply(wait_for, writer)
  }

  fn wait_apply<D>(
    &self,
    wait_for: Vec<Arc<FrozenBufferedUpdates>>,
    writer: &IndexWriter<D>,
  ) -> Result<()>
  where
    D: Directory + 'static,
  {
    let start_ns = std::time::Instant::now();
    let packet_count = wait_for.len();

    if wait_for.is_empty() {
      if self.finished_segments.info_stream.is_enabled("BD") {
        self
          .finished_segments
          .info_stream
          .message("BD", "waitApply: no deletes to apply")?;
      }
      return Ok(());
    }

    if self.finished_segments.info_stream.is_enabled("BD") {
      self.finished_segments.info_stream.message(
        "BD",
        &format!("waitApply: {packet_count:?} packets: {wait_for:?}"),
      )?;
    }

    let mut pending = Vec::new();
    let mut total_del_count: i64 = 0;
    for packet in &wait_for {
      // Frozen packets are now resolved, concurrently, by the indexing threads that
      // create them, by adding a DocumentsWriter.ResolveUpdatesEvent to the events queue,
      // but if we get here and the packet is not yet resolved, we resolve it now ourselves:
      if !writer.try_apply(packet)? {
        total_del_count += packet.total_del_count.load(Ordering::SeqCst);
        // if somebody else is currently applying it - move on to the next one and force apply below
        pending.push(packet);
      } else {
        total_del_count += packet.total_del_count.load(Ordering::SeqCst);
      }
    }
    for packet in pending {
      // now block on all the packets that were concurrently applied to ensure they are due before
      // we continue.
      writer.force_apply(packet)?;
    }

    if self.finished_segments.info_stream.is_enabled("BD") {
      let elapsed = start_ns.elapsed().as_secs_f64() * 1000.0;
      let bytes = self.bytes_used.load(Ordering::SeqCst);
      self.finished_segments.info_stream.message(
                    "BD",
                    &format!(
                        "waitApply: done {packet_count} packets; totalDelCount={total_del_count}; totBytesUsed={bytes}; took {elapsed:.2} msec"
                    ),
                )?;
    }
    Ok(())
  }

  pub(crate) fn get_next_gen(&self) -> i64 {
    let mut inner = self.inner.lock();
    let gen_ = inner.next_gen;
    inner.next_gen += 1;
    gen_
  }
  // only for assert
  fn check_delete_stats(&self, inner: &BufferedUpdatesStreamInner) -> bool {
    let mut bytes_used2 = 0i64;
    for packet in inner.updates.values() {
      bytes_used2 += packet.bytes_used as i64;
    }
    let actual = self.bytes_used.load(Ordering::SeqCst);
    debug_assert_eq!(
      bytes_used2, actual,
      "bytes_used2={bytes_used2} vs bytes_used={actual}"
    );
    true
  }
}
impl Accountable for BufferedUpdatesStream {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(self.bytes_used.load(Ordering::SeqCst))
  }
}

pub(crate) struct SegmentState<D>
where
  D: Directory,
{
  pub(crate) del_gen: i64,
  pub(crate) rld: Arc<ReadersAndUpdates<D>>,
  pub(crate) reader: DefaultLeafReader<D>,
  pub(crate) start_del_count: i32,
}
impl<D> SegmentState<D>
where
  D: Directory,
{
  pub(crate) fn new(rld: Arc<ReadersAndUpdates<D>>, info: &SegmentCommitInfo<D>) -> Result<Self> {
    let reader = {
      let mut inner = rld.inner.lock();
      readers_and_updates::get_reader(
        IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?,
        info,
        &mut inner,
        rld.index_created_version_major,
      )?
    };
    Ok(SegmentState {
      del_gen: info.get_buffered_deletes_gen(),
      rld,
      reader,
      start_del_count: info.get_del_count(),
    })
  }
  pub(crate) fn close(
    &self,
    writer: &IndexWriter<D>,
    inner: &mut crate::core::index::index_writer::Inner<D>,
  ) -> Result<()> {
    IOUtils::close_with(0..2, |operation| match operation {
      0 => self.rld.release(self.reader.as_ref(), None),
      _ => writer.release(self.rld.as_ref(), inner),
    })
  }
}
impl<D> Display for SegmentState<D>
where
  D: Directory,
{
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "SegmentState({})", self.rld.info_id)
  }
}

/// Tracks the contiguous range of packets that have finished resolving.
///
/// Packets are resolved concurrently, and only contiguous completed packets can be written to disk.
pub(crate) struct FinishedSegments {
  info_stream: InfoStreamMT,
  inner: Mutex<Inner>,
}
pub(crate) struct Inner {
  /// Largest del gen, inclusive, for which all prior packets have finished applying.
  completed_del_gen: i64,
  /// This lets us track the "holes" in the current frontier of applying del gens;
  /// once the holes are filled in we can advance completedDelGen.
  finished_del_gens: HashSet<i64>,
}
impl FinishedSegments {
  pub(crate) fn new(info_stream: InfoStreamMT) -> Self {
    FinishedSegments {
      info_stream,
      inner: Mutex::new(Inner {
        completed_del_gen: 0,
        finished_del_gens: HashSet::new(),
      }),
    }
  }

  #[allow(dead_code)]
  pub(crate) fn clear(&self) {
    let mut inner = self.inner.lock();
    inner.finished_del_gens.clear();
    inner.completed_del_gen = 0;
  }

  pub(crate) fn still_running(&self, del_gen: i64) -> bool {
    let inner = self.inner.lock();
    del_gen > inner.completed_del_gen && !inner.finished_del_gens.contains(&del_gen)
  }
  pub fn get_completed_del_gen(&self) -> i64 {
    let inner = self.inner.lock();
    inner.completed_del_gen
  }

  pub(crate) fn finished_segment(&self, del_gen: i64) -> Result<()> {
    let mut inner = self.inner.lock();
    inner.finished_del_gens.insert(del_gen);
    while inner
      .finished_del_gens
      .contains(&(inner.completed_del_gen + 1))
    {
      let v = inner.completed_del_gen + 1;
      inner.finished_del_gens.remove(&v);
      inner.completed_del_gen += 1;
    }
    {
      if self.info_stream.is_enabled("BD") {
        self.info_stream.message(
          "BD",
          &format!(
            "finished packet delGen={} now completedDelGen={}",
            del_gen, inner.completed_del_gen
          ),
        )?;
      }
    }
    Ok(())
  }
}
/// Result of applying deletes.
///
/// - `any_new_deletes`: true if any actual deletes took place
/// - `all_deleted`: if `Some`, contains segments_id that are 100% deleted
pub(crate) struct ApplyDeletesResult {
  pub(crate) any_deletes: bool,
  pub(crate) all_deleted: Option<Vec<Arc<str>>>,
}
impl ApplyDeletesResult {
  pub(crate) fn any_deletes(&self) -> bool {
    self.any_deletes
  }
  pub(crate) fn all_deleted(&self) -> Option<&Vec<Arc<str>>> {
    self.all_deleted.as_ref()
  }
}
