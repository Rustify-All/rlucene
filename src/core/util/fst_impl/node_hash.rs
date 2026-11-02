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
use crate::core::store::IndexOutput;
use crate::core::util::HashCode;
use crate::core::util::allocator_byte::DirectAllocatorByte;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fst_impl::byte_block_pool_reverse_bytes_reader::ByteBlockPoolReverseBytesReader;
use crate::core::util::fst_impl::fst::{
  ARCS_FOR_BINARY_SEARCH, ARCS_FOR_CONTINUOUS, ARCS_FOR_DIRECT_ADDRESSING, Arc, BitTable,
  BytesReader, FINAL_END_NODE, FST, NON_FINAL_END_NODE,
};
use crate::core::util::fst_impl::fst_compiler::{
  FSTCompiler, NodeEnum, NullFSTReader, UnCompiledNode,
};
use crate::core::util::fst_impl::fst_reader::FstReader;
use crate::core::util::fst_impl::outputs::{Outputs, OutputsBound};
use crate::core::util::long_values::LongValues;
use crate::core::util::packed::PackedInts;
use crate::core::util::packed::abstract_paged_mutable::AbstractPagedMutable;
use crate::core::util::packed::paged_growable_writer::PagedGrowableWriter;
use crate::core::util::{ByteBlockPool, TryIntoInt};
pub struct NodeHash<T> {
  // primary table -- we add nodes into this until it reaches the requested
  // tableSizeLimit/2, then we move it to fallback
  primary_table: PagedGrowableHash<T>,
  // how many nodes are allowed to store in both primary and fallback tables;
  // when primary gets full (tableSizeLimit/2), we move it to the
  // fallback table
  ram_limit_bytes: i64,
  // fallback table.  if we fallback and find the frozen node here, we
  // promote it to primary table, for a simplistic and
  // lowish-RAM-overhead (compared to e.g. LinkedHashMap) LRU behaviour.
  // fallbackTable is read-only.
  fallback_table: Option<PagedGrowableHash<T>>,
  // Store the last fallback table hash slot and node byte length in getFallback().
  last_fallback_node: Option<(usize, usize)>,
}
impl<T> NodeHash<T>
where
  T: OutputsBound,
{
  /// Creates a new `NodeHash` instance.
  ///
  /// `ram_limit_mb` is the max RAM (in MB) allowed for recording suffixes.
  /// If this limit is hit, least recently used suffixes are discarded and the
  /// FST is no longer minimal. A larger `ram_limit_mb` makes the FST
  /// smaller (closer to minimal).
  pub fn new(ram_limit_mb: f64) -> Result<Self> {
    if ram_limit_mb <= 0.0 {
      return Err(LuceneError::illegal_argument(format!(
        "ram_limit_mb must be > 0; got: {ram_limit_mb}"
      )));
    }

    let ram_limit_bytes = if ram_limit_mb >= (i64::MAX as f64) / 1024.0 / 1024.0 {
      // Quietly truncate to `i64::MAX` bytes too.
      i64::MAX
    } else {
      (ram_limit_mb * 1024.0 * 1024.0) as i64
    };

    Ok(Self {
      primary_table: PagedGrowableHash::new()?,
      fallback_table: None, // Empty initially
      ram_limit_bytes,
      last_fallback_node: None,
    })
  }
  fn get_fallback<O, DO>(
    node_in_index: usize,
    hash: usize,
    fst_compiler: &mut FSTCompiler<O, DO>,
  ) -> Result<i64>
  where
    O: Outputs<V = T>,
    DO: IndexOutput,
  {
    let fallback_table = {
      let node_hash = fst_compiler.dedup_hash.as_mut().ok_or_else(|| {
        LuceneError::illegal_state("FST node deduplication hash is not initialized")
      })?;
      node_hash.last_fallback_node = None;
      &mut node_hash.fallback_table
    };

    match fallback_table {
      Some(fallback_table) => {
        let mut hash_slot = hash & fallback_table.mask;
        let mut c = 0;

        loop {
          let node_address = fallback_table.get_node_address(hash_slot)?;
          if node_address == 0 {
            // not found
            return Ok(0);
          } else {
            let node = &fst_compiler.frontier[node_in_index];
            let fst = &fst_compiler.fst;
            if let Some(length) = fallback_table.nodes_equal(node_address, hash_slot, fst, node)? {
              let node_hash = fst_compiler.dedup_hash.as_mut().ok_or_else(|| {
                LuceneError::illegal_state("FST node deduplication hash is not initialized")
              })?;
              // store the node length for further use
              node_hash.last_fallback_node = Some((hash_slot, length));
              // frozen version of this node is already here
              return Ok(node_address);
            }
          }

          // quadratic probe (but is it, really?)
          c += 1;
          hash_slot = (hash_slot + c) & fallback_table.mask;
        }
      },
      None => {
        // no fallback yet (primary table is not yet large enough to
        // swap)
        Ok(0)
      },
    }
  }
  pub fn add<O, DO>(node_in_index: usize, fst_compiler: &mut FSTCompiler<O, DO>) -> Result<i64>
  where
    O: Outputs<V = T>,
    DO: IndexOutput,
  {
    let (mut hash_slot, mut c, hash) = {
      let node_hash = fst_compiler.dedup_hash.as_mut().ok_or_else(|| {
        LuceneError::illegal_state("FST node deduplication hash is not initialized")
      })?;
      let hash: usize = {
        let node_in = &fst_compiler.frontier[node_in_index];
        node_hash.hash(node_in)?.try_convert()?
      };
      (hash & node_hash.primary_table.mask, 0, hash)
    };

    loop {
      let mut node_address = {
        let node_hash = fst_compiler.dedup_hash.as_mut().ok_or_else(|| {
          LuceneError::illegal_state("FST node deduplication hash is not initialized")
        })?;
        node_hash.primary_table.get_node_address(hash_slot)?
      };
      if node_address == 0 {
        // not in primary, check fallback
        node_address = NodeHash::get_fallback(node_in_index, hash, fst_compiler)?;
        if node_address != 0 {
          let node_hash = fst_compiler.dedup_hash.as_mut().ok_or_else(|| {
            LuceneError::illegal_state("FST node deduplication hash is not initialized")
          })?;
          debug_assert!(node_hash.last_fallback_node.is_some());
          // it was already in fallback -- promote to primary
          node_hash
            .primary_table
            .set_node_address(hash_slot, node_address)?;
          let (fallback_hash_slot, fallback_node_length) = node_hash
            .last_fallback_node
            .ok_or_else(|| LuceneError::illegal_state("fallback FST hash slot is missing"))?;
          let fallback_table = node_hash
            .fallback_table
            .as_mut()
            .ok_or_else(|| LuceneError::illegal_state("fallback FST hash table is missing"))?;
          node_hash.primary_table.copy_fallback_node_bytes(
            hash_slot,
            fallback_table,
            fallback_hash_slot,
            fallback_node_length,
          )?;
        } else {
          // not in fallback either -- freeze & add the incoming node

          // freeze & add
          node_address = fst_compiler.add_node(node_in_index)?;
          // we use 0 as empty marker in hash table, so it better be
          // impossible to get a frozen node at 0:
          debug_assert!(node_address != FINAL_END_NODE && node_address != NON_FINAL_END_NODE);
          let node_hash = fst_compiler.dedup_hash.as_mut().ok_or_else(|| {
            LuceneError::illegal_state("FST node deduplication hash is not initialized")
          })?;
          node_hash
            .primary_table
            .set_node_address(hash_slot, node_address)?;
          {
            let pos = fst_compiler.scratch_bytes.get_position();
            node_hash.primary_table.copy_node_bytes(
              hash_slot,
              fst_compiler.scratch_bytes.get_bytes(),
              pos,
            )?;
          }

          // confirm frozen hash and unfrozen hash are the same
          debug_assert_eq!(
            node_hash
              .primary_table
              .hash(node_address, hash_slot, &fst_compiler.fst)?,
            hash,
            "Frozen hash mismatch"
          );
        }

        // how many bytes would be used if we had "perfect" hashing:
        //  - x2 for fstNodeAddress for FST node address
        //  - x2 for copiedNodeAddress for copied node address
        //  - the bytes copied out FST to the hashtable copiedNodes
        // each account for approximate hash table overhead halfway
        // between 33.3% and 66.6% note that some of the
        // copiedNodes are shared between fallback and primary tables so
        // this computation is pessimistic
        let node_hash = fst_compiler.dedup_hash.as_mut().ok_or_else(|| {
          LuceneError::illegal_state("FST node deduplication hash is not initialized")
        })?;
        let copied_bytes = node_hash
          .primary_table
          .inner
          .bytes_reader
          .buf
          .get_position();
        let ram_bytes_used =
          node_hash.primary_table.count * 2 * PackedInts::bits_required(node_address)? as i64 / 8
            + node_hash.primary_table.count * 2 * PackedInts::bits_required(copied_bytes)? as i64
              / 8
            + copied_bytes;
        // NOTE: we could instead use the more precise RAM used, but
        // this leads to unpredictable
        // quantized behavior due to 2X rehashing where for large ranges
        // of the RAM limit, the size of the FST does
        // not change, and then suddenly when you cross a secret
        // threshold, it drops.  With this approach
        // (measuring "perfect" hash storage and approximating the
        // overhead), the behaviour is more strictly monotonic: larger
        // RAM limits smoothly result in smaller FSTs,
        // even if the precise RAM used is not always under the limit.

        // divide limit by 2 because fallback gets half the RAM and
        // primary gets the other half
        if ram_bytes_used >= node_hash.ram_limit_bytes / 2 {
          // time to fallback -- fallback is now used read-only to
          // promote a node (suffix) to primary if
          // we encounter it again
          let size = node_hash.primary_table.inner.fst_node_address.size();
          node_hash.fallback_table = Some(std::mem::replace(&mut node_hash.primary_table, {
            PagedGrowableHash::with_size(node_address, size.max(16))?
          }));
        } else if node_hash.primary_table.count
          > (node_hash.primary_table.inner.fst_node_address.size() as f32 * (2f32 / 3f32)) as i64
        {
          // rehash at 2/3 occupancy
          node_hash
            .primary_table
            .rehash(node_address, &fst_compiler.fst)?;
        }

        return Ok(node_address);
      } else {
        let node = &fst_compiler.frontier[node_in_index];
        let node_hash = fst_compiler.dedup_hash.as_mut().ok_or_else(|| {
          LuceneError::illegal_state("FST node deduplication hash is not initialized")
        })?;
        if node_hash
          .primary_table
          .nodes_equal(node_address, hash_slot, &fst_compiler.fst, node)?
          .is_some()
        {
          // same node (in frozen form) is already in primary table
          return Ok(node_address);
        }
      }

      c += 1;
      // quadratic probe (but is it, really?)
      let node_hash = fst_compiler.dedup_hash.as_mut().ok_or_else(|| {
        LuceneError::illegal_state("FST node deduplication hash is not initialized")
      })?;
      hash_slot = (hash_slot + c) & node_hash.primary_table.mask;
    }
  }
  fn hash(&self, node: &UnCompiledNode<T>) -> Result<i64> {
    const PRIME: i64 = 31;
    let mut h: i64 = 0;

    for arc in &node.arcs[..node.num_arcs] {
      h = h.wrapping_mul(PRIME).wrapping_add(arc.label as i64);

      let n = match &arc.target {
        NodeEnum::CompiledNode(compiled_node) => compiled_node.node,
        _ => return Err(LuceneError::illegal_state("Node should be compiled")),
      };

      let mixed_node = ((n ^ (n >> 32)) & 0xFFFF_FFFF) as i32;
      h = h.wrapping_mul(PRIME).wrapping_add(mixed_node as i64);

      let output_hash = arc.output.hash_code() as i64;
      h = h.wrapping_mul(PRIME).wrapping_add(output_hash);

      let next_final_output_hash = arc.next_final_output.hash_code() as i64;
      h = h.wrapping_mul(PRIME).wrapping_add(next_final_output_hash);

      if arc.is_final {
        h = h.wrapping_add(17);
      }
    }
    Ok(h & i64::MAX)
  }
}

/// Inner struct because it needs access to hash function and FST bytes.
pub struct PagedGrowableHash<T> {
  count: i64,
  mask: usize,
  inner: Inner,
  scratch_arc: Arc<T>,
}
pub struct Inner {
  /// the [`FST.BytesReader`](FstReader)
  /// to read from copiedNodes. we use this when computing a frozen
  /// node hash or comparing if a frozen and unfrozen nodes are equal
  bytes_reader: ByteBlockPoolReverseBytesReader,
  /// Storing the FST node address where the position is the masked hash of
  /// the node arcs.
  fst_node_address: AbstractPagedMutable<PagedGrowableWriter>,
  /// Storing the local copiedNodes address in the same position as
  /// fst_node_address. Effectively a map from FST node address to
  /// copiedNodes address.
  copied_node_address: AbstractPagedMutable<PagedGrowableWriter>,
}
impl Inner {
  /// Returns the [`ByteBlockPoolReverseBytesReader`] positioned for the given
  /// node.
  pub(crate) fn get_bytes_reader(
    &mut self,
    node_address: i64,
    hash_slot: usize,
  ) -> Result<&mut ByteBlockPoolReverseBytesReader> {
    debug_assert_eq!(self.fst_node_address.get(hash_slot)?, node_address);
    let local_address = self.copied_node_address.get(hash_slot)?;
    self
      .bytes_reader
      .set_pos_delta(node_address - local_address);
    Ok(&mut self.bytes_reader)
  }
}

impl<T> PagedGrowableHash<T>
where
  T: OutputsBound,
{
  // 256K blocks, but note that the final block is sized only as needed so it
  // won't use the full block size when just a few elements were written
  // to it
  const BLOCK_SIZE_BYTES: i32 = 1 << 18;
  pub(crate) fn new() -> Result<Self> {
    Self::build(8, 8, 16, 15)
  }

  pub(crate) fn with_size(last_node_address: i64, size: usize) -> Result<Self> {
    let fst_node_address_bits_per_value = PackedInts::bits_required(last_node_address)?;
    let mask = size - 1;
    debug_assert!(
      mask & size == 0,
      "size must be a power-of-2; got size={size} mask={mask}"
    );
    Self::build(fst_node_address_bits_per_value, 8, size, mask)
  }
  fn build(
    fst_node_address_bits_per_value: i32,
    copied_node_address_bits_per_value: i32,
    size: usize,
    mask: usize,
  ) -> Result<Self> {
    let sub_reader =
      PagedGrowableWriter::with_fill_page(fst_node_address_bits_per_value, PackedInts::COMPACT);
    let fst_node_address = AbstractPagedMutable::new(size, Self::BLOCK_SIZE_BYTES, sub_reader)?;
    let sub_reader =
      PagedGrowableWriter::with_fill_page(copied_node_address_bits_per_value, PackedInts::COMPACT);
    let copied_node_address = AbstractPagedMutable::new(size, Self::BLOCK_SIZE_BYTES, sub_reader)?;

    let allocator = DirectAllocatorByte::new();
    let copied_nodes = ByteBlockPool::new(allocator);
    let bytes_reader = ByteBlockPoolReverseBytesReader::new(copied_nodes);
    let inner = Inner {
      bytes_reader,
      fst_node_address,
      copied_node_address,
    };
    Ok(Self {
      count: 0,
      mask,
      inner,
      scratch_arc: Arc::default(),
    })
  }
  /// Get the copied bytes at the provided hash slot.
  ///
  /// # Arguments
  ///
  /// * `hash_slot` - The hash slot to read from
  /// * `length` - The number of bytes to read
  ///
  /// # Returns
  ///
  /// The copied byte array
  #[cfg(test)]
  pub fn get_bytes(&self, hash_slot: usize, length: usize) -> Result<Vec<u8>> {
    let address = self.inner.copied_node_address.get(hash_slot)?;
    debug_assert!(address - length as i64 + 1 >= 0);

    let mut buf = vec![0u8; length];
    self
      .inner
      .bytes_reader
      .buf
      .read_bytes(address - length as i64 + 1, &mut buf, 0, length)?;
    Ok(buf)
  }
  /// Get the node address from the provided hash slot.
  pub fn get_node_address(&self, hash_slot: usize) -> Result<i64> {
    self.inner.fst_node_address.get(hash_slot)
  }
  /// Set the node address for the given hash slot.
  pub fn set_node_address(&mut self, hash_slot: usize, node_address: i64) -> Result<()> {
    debug_assert_eq!(self.inner.fst_node_address.get(hash_slot).unwrap_or(-1), 0);
    self.inner.fst_node_address.set(hash_slot, node_address)?;
    self.count += 1;
    Ok(())
  }
  /// Copy the node bytes from the FST.
  pub(crate) fn copy_node_bytes(
    &mut self,
    hash_slot: usize,
    bytes: &[u8],
    length: usize,
  ) -> Result<()> {
    debug_assert_eq!(
      self.inner.copied_node_address.get(hash_slot).unwrap_or(-1),
      0
    );

    let copied_nodes = &mut self.inner.bytes_reader.buf;
    copied_nodes.append_range(bytes, 0, length)?;
    let position = copied_nodes.get_position();
    // write the offset, which points to the last byte of the node we
    // copied since we later read this node in reverse
    self
      .inner
      .copied_node_address
      .set(hash_slot, position - 1)?;
    Ok(())
  }
  /// Promote the node bytes from the fallback table.
  pub(crate) fn copy_fallback_node_bytes(
    &mut self,
    hash_slot: usize,
    fallback_table: &mut PagedGrowableHash<T>,
    fallback_hash_slot: usize,
    node_length: usize,
  ) -> Result<()> {
    debug_assert_eq!(self.inner.copied_node_address.get(hash_slot)?, 0);

    let fallback_address = fallback_table
      .inner
      .copied_node_address
      .get(fallback_hash_slot)?;
    // fallbackAddress is the last offset of the node, but we need to copy
    // the bytes from the start address
    let fallback_start_address = fallback_address - node_length as i64 + 1;
    debug_assert!(fallback_start_address >= 0);
    let copied_nodes = &mut self.inner.bytes_reader.buf;
    copied_nodes.append_from_byte_block_pool(
      &fallback_table.inner.bytes_reader.buf,
      fallback_start_address,
      node_length,
    )?;
    let position = copied_nodes.get_position();
    self
      .inner
      .copied_node_address
      .set(hash_slot, position - 1)?;
    Ok(())
  }
  fn rehash<O, F>(&mut self, last_node_address: i64, fst: &FST<O, F>) -> Result<()>
  where
    O: Outputs<V = T>,
    F: FstReader,
  {
    let new_size = self.inner.fst_node_address.size() * 2;

    let position = self.inner.bytes_reader.get_position();
    let sub_reader = PagedGrowableWriter::with_fill_page(
      PackedInts::bits_required(position)?,
      PackedInts::COMPACT,
    );
    let mut new_copied_node_address =
      AbstractPagedMutable::new(new_size, Self::BLOCK_SIZE_BYTES, sub_reader)?;

    let sub_reader = PagedGrowableWriter::with_fill_page(
      PackedInts::bits_required(last_node_address)?,
      PackedInts::COMPACT,
    );
    let mut new_fst_node_address =
      AbstractPagedMutable::new(new_size, Self::BLOCK_SIZE_BYTES, sub_reader)?;

    let new_mask = new_fst_node_address.size() - 1;

    for idx in 0..self.inner.fst_node_address.size() {
      let address = self.inner.fst_node_address.get(idx)?;
      if address != 0 {
        let mut hash_slot = self.hash(address, idx, fst)? & new_mask;
        let mut c = 0;
        loop {
          if new_fst_node_address.get(hash_slot)? == 0 {
            new_fst_node_address.set(hash_slot, address)?;
            new_copied_node_address.set(hash_slot, self.inner.copied_node_address.get(idx)?)?;
            break;
          }
          // quadratic probe
          c += 1;
          hash_slot = (hash_slot + c) & new_mask;
        }
      }
    }

    self.mask = new_mask;
    self.inner.fst_node_address = new_fst_node_address;
    self.inner.copied_node_address = new_copied_node_address;

    Ok(())
  }
  fn hash<O, F>(&mut self, node_address: i64, hash_slot: usize, fst: &FST<O, F>) -> Result<usize>
  where
    O: Outputs<V = T>,
    F: FstReader,
  {
    const PRIME: i64 = 31;
    let mut h: i64 = 0;

    let reader = self.inner.get_bytes_reader(node_address, hash_slot)?;

    fst.read_first_real_target_arc(node_address, &mut self.scratch_arc, reader)?;

    loop {
      h = h
        .wrapping_mul(PRIME)
        .wrapping_add(self.scratch_arc.label() as i64);
      let target = self.scratch_arc.target();
      let mixed = (target ^ (target >> 32)) & 0xFFFF_FFFF;
      h = h.wrapping_mul(PRIME).wrapping_add(mixed as i32 as i64);
      let output_hash = self.scratch_arc.output.hash_code() as i64;
      h = h.wrapping_mul(PRIME).wrapping_add(output_hash);
      let next_output_hash = self.scratch_arc.next_final_output.hash_code() as i64;
      h = h.wrapping_mul(PRIME).wrapping_add(next_output_hash);
      if self.scratch_arc.is_final() {
        h = h.wrapping_add(17);
      }
      if self.scratch_arc.is_last() {
        break;
      }
      fst.read_next_real_arc(&mut self.scratch_arc, reader)?;
    }
    Ok((h & i64::MAX) as usize)
  }

  /// Compares an unfrozen node (`UnCompiledNode`) with a frozen node at byte
  /// location address (`i64`), returning the node length if the two nodes
  /// are equals, or `-1` otherwise.
  ///
  ///
  /// The node length will be used to promote the node from the fallback table
  /// to the primary table.
  fn nodes_equal<O>(
    &mut self,
    address: i64,
    hash_slot: usize,
    fst: &FST<O, NullFSTReader>,
    node: &UnCompiledNode<T>,
  ) -> Result<Option<usize>>
  where
    O: Outputs<V = T>,
  {
    let in_reader = self.inner.get_bytes_reader(address, hash_slot)?;
    fst.read_first_real_target_arc(address, &mut self.scratch_arc, in_reader)?;
    // fail fast for a node with fixed length arcs
    if self.scratch_arc.bytes_per_arc() != 0 {
      debug_assert!(node.num_arcs > 0);
      // the frozen node uses fixed-with arc encoding (same number of
      // bytes per arc), but may be sparse or dense
      match self.scratch_arc.node_flags() {
        ARCS_FOR_BINARY_SEARCH => {
          if node.num_arcs as i32 != self.scratch_arc.num_arcs() {
            // sparse
            return Ok(None);
          }
        },
        ARCS_FOR_DIRECT_ADDRESSING => {
          // dense -- compare both the number of labels allocated in
          // the array (some of which may
          // not actually be arcs), and the number of arcs
          let first_label = node.arcs[0].label;
          let last_label = node.arcs[node.num_arcs - 1].label;
          if (last_label - first_label + 1) != self.scratch_arc.num_arcs()
            || node.num_arcs as i32 != BitTable::count_bits(&self.scratch_arc, in_reader)?
          {
            return Ok(None);
          }
        },
        ARCS_FOR_CONTINUOUS => {
          let first_label = node.arcs[0].label;
          let last_label = node.arcs[node.num_arcs - 1].label;
          if (last_label - first_label + 1) != self.scratch_arc.num_arcs() {
            return Ok(None);
          }
        },
        _ => {
          return Err(LuceneError::illegal_state(format!(
            "unhandled scratch_arc.node_flag() {}",
            self.scratch_arc.node_flags()
          )));
        },
      }
    }
    // compare arc by arc to see if there is a difference
    for arc_idx in 0..node.num_arcs {
      let arc = &node.arcs[arc_idx];

      if arc.label != self.scratch_arc.label()
        || arc.output != self.scratch_arc.output
        || {
          match &arc.target {
            NodeEnum::CompiledNode(compiled) => compiled.node != self.scratch_arc.target(),
            _ => return Err(LuceneError::illegal_state("Node should be compiled")),
          }
        }
        || arc.next_final_output != self.scratch_arc.next_final_output
        || arc.is_final != self.scratch_arc.is_final()
      {
        return Ok(None);
      }

      match &arc.target {
        NodeEnum::CompiledNode(compiled) => {
          if compiled.node != self.scratch_arc.target() {
            return Ok(None);
          }
        },
        _ => return Ok(None),
      }

      if self.scratch_arc.is_last() {
        return if arc_idx == (node.num_arcs - 1) {
          let len = address - in_reader.get_position();
          Ok(Some(len as usize))
        } else {
          Ok(None)
        };
      }

      fst.read_next_real_arc(&mut self.scratch_arc, in_reader)?;
    }

    // unfrozen node has fewer arcs than frozen node
    Ok(None)
  }
}
