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
use crate::core::codecs::CodecUtil;
use crate::core::store::output_stream_data_output::OutputStreamDataOutput;
use crate::core::store::{ByteBuffersDataOutput, DataInput, DataOutput};
use crate::core::util::IOUtils;
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fst_impl::bit_table_util::BitTableUtil;
use crate::core::util::fst_impl::fst_reader::FstReader;
use crate::core::util::fst_impl::on_heap_fst_store::OnHeapFSTStore;
use crate::core::util::fst_impl::outputs::{Outputs, OutputsBound};
use core::fmt;
use parking_lot::Mutex;
use std::cell::RefCell;
use std::fmt::{Display, Formatter};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub struct FST<O, F>
where
  O: Outputs,
  F: FstReader,
{
  pub metadata: FSTMetadata<O>,
  pub outputs: O,
  // wrap with RefCell to allow interior mutability
  pub fst_reader: Mutex<F>,
}

impl<O> FST<O, OnHeapFSTStore>
where
  O: Outputs,
{
  /// Load a previously saved FST with a DataInput for metadata using an
  /// [`OnHeapFSTStore`] with `maxBlockBits` set to
  /// [`DEFAULT_MAX_BLOCK_BITS`]
  pub fn from_on_heap_store(metadata: FSTMetadata<O>, input: &mut impl DataInput) -> Result<Self> {
    let store = OnHeapFSTStore::new(DEFAULT_MAX_BLOCK_BITS, input, metadata.num_bytes)?;
    Ok(Self::new(metadata, store))
  }
}
impl<O, F> FST<O, F>
where
  O: Outputs,
  F: FstReader,
{
  /// Create the FST with a metadata object and a reader.
  pub fn new(metadata: FSTMetadata<O>, fst_reader: F) -> Self {
    Self {
      outputs: metadata.outputs.clone(),
      metadata,
      fst_reader: Mutex::new(fst_reader),
    }
  }
  /// Create an FST from metadata and reader. Returns `None` if the metadata
  /// is `None`.
  pub fn from_fst_reader(metadata: FSTMetadata<O>, fst_reader: F) -> Option<Self> {
    Some(Self {
      outputs: metadata.outputs.clone(),
      metadata,
      fst_reader: Mutex::new(fst_reader),
    })
  }
  pub fn num_bytes(&self) -> i64 {
    self.metadata.num_bytes
  }

  pub fn get_empty_output(&self) -> Option<&O::V> {
    self.metadata.empty_output.as_ref()
  }

  pub fn metadata(&self) -> &FSTMetadata<O> {
    &self.metadata
  }

  /// Save the FST to DataOutput.
  ///
  /// # Arguments
  ///
  /// * `metaOut` - the DataOutput to write the metadata to
  /// * `out` - the DataOutput to write the FST bytes to
  pub fn save(&mut self, meta_out: &mut impl DataOutput, out: &mut impl DataOutput) -> Result<()> {
    self.metadata.save(meta_out)?;
    self.fst_reader.lock().write_to(out)
  }
  pub fn save_with_same_data_out(&self, out: &mut impl DataOutput) -> Result<()> {
    self.metadata.save(out)?;
    self.fst_reader.lock().write_to(out)
  }

  /// Writes the automaton to a file.
  pub fn save_to_path(&mut self, path: &PathBuf) -> Result<()> {
    let file = File::create(path)?; // or: path.as_path()
    let mut out = OutputStreamDataOutput::new(file);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      self.save_with_same_data_out(&mut out)
    }));
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| out.close()));
    IOUtils::use_or_suppress_caught_result(result, close_result)
  }
  /// Reads the automaton from a file.
  pub fn read_from_path(_path: &Path, _outputs: Rc<RefCell<O>>) -> Result<Self> {
    todo!()
  }
  /// Reads one BYTE1/2/4 label from the provided DataInput.
  pub fn read_label(&self, input: &mut impl DataInput) -> Result<i32> {
    let input_type = self.metadata.input_type;
    let version = self.metadata.version;

    let v = match input_type {
      InputType::Byte1 => input.read_byte()? as i32,
      InputType::Byte2 => {
        let raw = input.read_short()?;
        if version < VERSION_LITTLE_ENDIAN {
          i32::from(u16::from_be_bytes(raw.to_be_bytes()))
        } else {
          raw as u16 as i32
        }
      },
      InputType::Byte4 => input.read_vint()?,
    };
    Ok(v)
  }
  /// Reads the presence bits of a direct-addressing node.
  /// Actually we don't read them here — we just record the bit-table start
  /// position and skip.
  fn read_presence_bytes(&self, arc: &mut Arc<O::V>, reader: &mut impl BytesReader) -> Result<()> {
    debug_assert!(arc.bytes_per_arc > 0);
    debug_assert_eq!(arc.node_flags, ARCS_FOR_DIRECT_ADDRESSING);

    arc.bit_table_start = reader.get_position();
    let skip_bytes = get_num_presence_bytes(arc.num_arcs);
    reader.skip_bytes(skip_bytes as i64)?;
    Ok(())
  }
  /// Fills the virtual 'start' arc, i.e., an empty incoming arc to the FST's
  /// start node.
  pub fn get_first_arc(&self, arc: &mut Arc<O::V>) {
    let no_output = self.outputs.get_no_output();

    if let Some(ref empty_output) = self.metadata.empty_output {
      arc.flags = BIT_FINAL_ARC | BIT_LAST_ARC;
      arc.next_final_output = empty_output.clone();
      if *empty_output != no_output {
        arc.flags |= BIT_ARC_HAS_FINAL_OUTPUT;
      }
    } else {
      arc.flags = BIT_LAST_ARC;
      arc.next_final_output = no_output.clone();
    }

    arc.output = no_output;
    // If there are no nodes, ie, the FST only accepts the
    // empty string, then startNode is 0
    arc.target = self.metadata.start_node;
  }
  /// Follows the `follow` arc and reads the last arc of its target; this
  /// changes the provided `arc` (2nd arg) in-place and returns it.
  ///
  /// # Returns
  ///
  /// Returns the second argument (`arc`).
  pub(crate) fn read_last_target_arc<'a>(
    &self,
    follow: &Arc<O::V>,
    arc: &'a mut Arc<O::V>,
    input: &mut impl BytesReader,
  ) -> Result<&'a mut Arc<O::V>> {
    if !target_has_arcs(follow) {
      debug_assert!(follow.is_final());
      arc.label = END_LABEL;
      arc.target = FINAL_END_NODE;
      arc.output = follow.next_final_output.clone();
      arc.flags = BIT_LAST_ARC;
      arc.node_flags = arc.flags;
      return Ok(arc);
    }

    input.set_position(follow.target());
    let flags = input.read_byte()?;
    arc.node_flags = flags;

    if flags == ARCS_FOR_BINARY_SEARCH
      || flags == ARCS_FOR_DIRECT_ADDRESSING
      || flags == ARCS_FOR_CONTINUOUS
    {
      // Special arc which is actually a node header for fixed length
      // arcs. Jump straight to end to find the last arc.
      arc.num_arcs = input.read_vint()?;
      arc.bytes_per_arc = input.read_vint()?;

      if flags == ARCS_FOR_DIRECT_ADDRESSING {
        read_presence_bytes(arc, input)?;
        arc.first_label = self.read_label(input)?;
        arc.pos_arcs_start = input.get_position();
        self.read_last_arc_by_direct_addressing(arc, input)?;
      } else if flags == ARCS_FOR_BINARY_SEARCH {
        arc.arc_idx = arc.num_arcs - 2;
        arc.pos_arcs_start = input.get_position();
        self.read_next_real_arc(arc, input)?;
      } else {
        arc.first_label = self.read_label(input)?;
        arc.pos_arcs_start = input.get_position();
        self.read_last_arc_by_continuous(arc, input)?;
      }
    } else {
      arc.flags = flags;
      arc.bytes_per_arc = 0;

      // Linear scan through variable-length arcs
      while !arc.is_last() {
        self.read_label(input)?;
        if arc.flag(BIT_ARC_HAS_OUTPUT as i32) {
          self.outputs.skip_output(input)?;
        }
        if arc.flag(BIT_ARC_HAS_FINAL_OUTPUT as i32) {
          self.outputs.skip_final_output(input)?;
        }
        if arc.flag(BIT_STOP_NODE) || arc.flag(BIT_TARGET_NEXT as i32) {
          // no-op
        } else {
          self.read_unpacked_node_target(input)?;
        }
        arc.flags = input.read_byte()?;
      }
      // Undo the byte flags we read:
      input.skip_bytes(-1)?;
      arc.next_arc = input.get_position();
      self.read_next_real_arc(arc, input)?;
    }

    debug_assert!(arc.is_last());
    Ok(arc)
  }
  /// Reads an unpacked node target address (as a `vLong`) from the input.
  fn read_unpacked_node_target(&self, reader: &mut impl BytesReader) -> Result<i64> {
    reader.read_vlong()
  }
  /// Follow the `follow` arc and read the first arc of its target;
  /// modifies `arc` in-place and returns it.
  pub fn read_first_target_arc(
    &self,
    follow: &Arc<O::V>,
    arc: &mut Arc<O::V>,
    reader: &mut impl BytesReader,
  ) -> Result<()> {
    if follow.is_final() {
      // Insert "fake" final arc to END_LABEL
      arc.label = END_LABEL;
      arc.output = follow.next_final_output.clone();
      arc.flags = BIT_FINAL_ARC;

      if follow.target <= 0 {
        arc.flags |= BIT_LAST_ARC;
      } else {
        arc.next_arc = follow.target;
      }

      arc.target = FINAL_END_NODE;
      arc.node_flags = arc.flags;
      Ok(())
    } else {
      self.read_first_real_target_arc(follow.target, arc, reader)
    }
  }
  fn read_first_arc_info(
    &self,
    node_address: i64,
    arc: &mut Arc<O::V>,
    reader: &mut impl BytesReader,
  ) -> Result<()> {
    reader.set_position(node_address);

    let flags = reader.read_byte()?;
    arc.node_flags = flags;

    if flags == ARCS_FOR_BINARY_SEARCH
      || flags == ARCS_FOR_DIRECT_ADDRESSING
      || flags == ARCS_FOR_CONTINUOUS
    {
      // Special arc which is actually a node header for fixed length
      // arcs.
      arc.num_arcs = reader.read_vint()?;
      arc.bytes_per_arc = reader.read_vint()?;
      arc.arc_idx = -1;

      if flags == ARCS_FOR_DIRECT_ADDRESSING {
        self.read_presence_bytes(arc, reader)?;
        arc.first_label = self.read_label(reader)?;
        arc.presence_index = -1;
      } else if flags == ARCS_FOR_CONTINUOUS {
        arc.first_label = self.read_label(reader)?;
      }

      arc.pos_arcs_start = reader.get_position();
    } else {
      arc.next_arc = node_address;
      arc.bytes_per_arc = 0;
    }

    Ok(())
  }
  pub fn read_first_real_target_arc(
    &self,
    node_address: i64,
    arc: &mut Arc<O::V>,
    reader: &mut impl BytesReader,
  ) -> Result<()> {
    self.read_first_arc_info(node_address, arc, reader)?;
    self.read_next_real_arc(arc, reader)
  }
  /// Returns whether `arc`'s target points to a node in expanded format
  /// (fixed length arcs).
  pub fn is_expanded_target(
    &self,
    follow: &Arc<O::V>,
    reader: &mut impl BytesReader,
  ) -> Result<bool> {
    if !target_has_arcs(follow) {
      Ok(false)
    } else {
      reader.set_position(follow.target);
      let flags = reader.read_byte()?;
      Ok(
        flags == ARCS_FOR_BINARY_SEARCH
          || flags == ARCS_FOR_DIRECT_ADDRESSING
          || flags == ARCS_FOR_CONTINUOUS,
      )
    }
  }
  /// In-place read; returns the arc.
  pub fn read_next_arc(&self, arc: &mut Arc<O::V>, input: &mut impl BytesReader) -> Result<()> {
    if arc.label() == END_LABEL {
      // This was a fake inserted "final" arc
      if arc.next_arc() <= 0 {
        return Err(LuceneError::illegal_argument(
          "cannot read_next_arc when arc.is_last()=true",
        ));
      }
      self.read_first_real_target_arc(arc.next_arc(), arc, input)
    } else {
      self.read_next_real_arc(arc, input)
    }
  }
  /// Peeks at next arc's label; does not alter arc.
  /// Do not call this if `arc.is_last()`!
  pub(crate) fn read_next_arc_label(
    &self,
    arc: &Arc<O::V>,
    input: &mut impl BytesReader,
  ) -> Result<i32> {
    debug_assert!(!arc.is_last());

    if arc.label() == END_LABEL {
      // Next arc is the first arc of a node.
      // Position to read the first arc label.
      input.set_position(arc.next_arc());
      let flags = input.read_byte()?;

      if flags == ARCS_FOR_BINARY_SEARCH
        || flags == ARCS_FOR_DIRECT_ADDRESSING
        || flags == ARCS_FOR_CONTINUOUS
      {
        // Special arc which is actually a node header for fixed length
        // arcs.
        let num_arcs = input.read_vint()?;
        input.read_vint()?; // Skip bytesPerArc.
        if flags == ARCS_FOR_BINARY_SEARCH {
          input.read_byte()?; // Skip arc flags.
        } else if flags == ARCS_FOR_DIRECT_ADDRESSING {
          input.skip_bytes(get_num_presence_bytes(num_arcs) as i64)?;
        } // Nothing to do for ARCS_FOR_CONTINUOUS
      }
    } else {
      match arc.node_flags() {
        ARCS_FOR_BINARY_SEARCH => {
          // Point to next arc, -1 to skip arc flags.
          let pos =
            arc.pos_arcs_start() - (1 + arc.arc_idx()) as i64 * arc.bytes_per_arc() as i64 - 1;
          input.set_position(pos);
        },
        ARCS_FOR_DIRECT_ADDRESSING => {
          // Direct addressing node. The label is not stored but
          // rather inferred based on first label
          // and arc index in the range.
          debug_assert!(BitTable::assert_is_valid(arc, input)?);
          debug_assert!(BitTable::is_bit_set(arc.arc_idx(), arc, input)?);
          let next_index = BitTable::next_bit_set(arc.arc_idx(), arc, input)?;
          debug_assert!(next_index != -1);
          return Ok(arc.first_label() + next_index);
        },
        ARCS_FOR_CONTINUOUS => {
          return Ok(arc.first_label() + arc.arc_idx() + 1);
        },
        _ => {
          // Variable length arcs - linear search.
          debug_assert_eq!(arc.bytes_per_arc(), 0);
          // Arcs have variable length.
          // Position to next arc, -1 to skip flags.
          input.set_position(arc.next_arc() - 1);
        },
      }
    }

    self.read_label(input)
  }
  /// Reads a binary search node arc by its index.
  ///
  /// # Arguments
  ///
  /// * `arc` - the arc to update
  /// * `in` - the bytes reader
  /// * `idx` - the arc index (must be within range)
  ///
  /// # Returns
  ///
  /// The updated arc
  pub fn read_arc_by_index(
    &self,
    arc: &mut Arc<O::V>,
    input: &mut impl BytesReader,
    idx: i32,
  ) -> Result<()> {
    debug_assert!(arc.bytes_per_arc() > 0);
    debug_assert_eq!(arc.node_flags(), ARCS_FOR_BINARY_SEARCH);
    debug_assert!(idx >= 0 && idx < arc.num_arcs());
    input.set_position(arc.pos_arcs_start() - idx as i64 * arc.bytes_per_arc() as i64);
    arc.arc_idx = idx;
    arc.flags = input.read_byte()?;
    self.read_arc(arc, input)
  }

  /// Reads a continuous node arc, with the provided index in the label range.
  ///
  /// `range_index` must be within the label range.
  pub fn read_arc_by_continuous(
    &self,
    arc: &mut Arc<O::V>,
    reader: &mut impl BytesReader,
    range_index: i32,
  ) -> Result<()> {
    debug_assert!(range_index >= 0 && range_index < arc.num_arcs);
    let pos = arc.pos_arcs_start - (range_index as i64 * arc.bytes_per_arc as i64);
    reader.set_position(pos);
    arc.arc_idx = range_index;
    arc.flags = reader.read_byte()?;
    self.read_arc(arc, reader)
  }
  /// Reads a present direct addressing node arc, with the provided index in
  /// the label range.
  ///
  /// `range_index` must point to a present arc; the actual offset is computed
  /// from presence bits.
  pub fn read_arc_by_direct_addressing(
    &self,
    arc: &mut Arc<O::V>,
    reader: &mut impl BytesReader,
    range_index: i32,
  ) -> Result<()> {
    debug_assert!(BitTable::assert_is_valid(arc, reader)?);
    debug_assert!(
      range_index >= 0 && range_index < arc.num_arcs,
      "range_index {} out of bounds (0..{})",
      range_index,
      arc.num_arcs
    );
    debug_assert!(
      BitTable::is_bit_set(range_index, arc, reader)?,
      "bit not set at index {range_index}"
    );

    let presence_index = BitTable::count_bits_upto(range_index, arc, reader)?;
    self.read_arc_by_direct_addressing_with_presence_index(arc, reader, range_index, presence_index)
  }
  /// Reads a present direct addressing node arc, with the provided index in
  /// the label range and its corresponding presence index (which is the
  /// count of presence bits before it).
  pub fn read_arc_by_direct_addressing_with_presence_index(
    &self,
    arc: &mut Arc<O::V>,
    reader: &mut impl BytesReader,
    range_index: i32,
    presence_index: i32,
  ) -> Result<()> {
    let pos = arc.pos_arcs_start - (presence_index as i64 * arc.bytes_per_arc as i64);
    reader.set_position(pos);
    arc.arc_idx = range_index;
    arc.presence_index = presence_index;
    arc.flags = reader.read_byte()?;
    self.read_arc(arc, reader)
  }

  /// Reads the last arc of a direct addressing node.
  ///
  /// This method is equivalent to calling
  /// [`read_arc_by_direct_addressing`](Self::read_arc_by_direct_addressing)
  /// with `range_index` equal to `arc.num_arcs() - 1`, but it is faster.
  pub fn read_last_arc_by_direct_addressing(
    &self,
    arc: &mut Arc<O::V>,
    reader: &mut impl BytesReader,
  ) -> Result<()> {
    debug_assert!(BitTable::assert_is_valid(arc, reader)?);

    let presence_index = BitTable::count_bits(arc, reader)? - 1;

    self.read_arc_by_direct_addressing_with_presence_index(
      arc,
      reader,
      arc.num_arcs - 1,
      presence_index,
    )
  }

  /// Reads the last arc of a continuous node.
  pub fn read_last_arc_by_continuous(
    &self,
    arc: &mut Arc<O::V>,
    reader: &mut impl BytesReader,
  ) -> Result<()> {
    self.read_arc_by_continuous(arc, reader, arc.num_arcs - 1)
  }
  /// Never returns `None`, but must not be called if `arc.is_last()` is true.
  pub fn read_next_real_arc(
    &self,
    arc: &mut Arc<O::V>,
    reader: &mut impl BytesReader,
  ) -> Result<()> {
    match arc.node_flags {
      ARCS_FOR_BINARY_SEARCH | ARCS_FOR_CONTINUOUS => {
        debug_assert!(arc.bytes_per_arc > 0);
        arc.arc_idx += 1;
        debug_assert!(arc.arc_idx >= 0 && arc.arc_idx < arc.num_arcs);
        let pos = arc.pos_arcs_start - (arc.arc_idx as i64 * arc.bytes_per_arc as i64);
        reader.set_position(pos);
        arc.flags = reader.read_byte()?;
      },

      ARCS_FOR_DIRECT_ADDRESSING => {
        debug_assert!(BitTable::assert_is_valid(arc, reader)?);
        debug_assert!(
          arc.arc_idx == -1 || BitTable::is_bit_set(arc.arc_idx, arc, reader)?,
          "arcIdx {} must be valid or unset",
          arc.arc_idx
        );
        let next_index = BitTable::next_bit_set(arc.arc_idx, arc, reader)?;
        return self.read_arc_by_direct_addressing_with_presence_index(
          arc,
          reader,
          next_index,
          arc.presence_index + 1,
        );
      },

      _ => {
        // Variable-length arcs (linear scan)
        debug_assert_eq!(arc.bytes_per_arc, 0);
        reader.set_position(arc.next_arc);
        arc.flags = reader.read_byte()?;
      },
    }

    self.read_arc(arc, reader)
  }

  // Reads an arc.
  ///
  /// Precondition: The arc flags byte has already been read and set;
  /// the given `BytesReader` is positioned just after the arc flags byte.
  pub fn read_arc(&self, arc: &mut Arc<O::V>, reader: &mut impl BytesReader) -> Result<()> {
    if arc.node_flags == ARCS_FOR_DIRECT_ADDRESSING || arc.node_flags == ARCS_FOR_CONTINUOUS {
      arc.label = arc.first_label() + arc.arc_idx();
    } else {
      arc.label = self.read_label(reader)?;
    }

    if arc.flag(BIT_ARC_HAS_OUTPUT as i32) {
      arc.output = self.outputs.read(reader)?;
    } else {
      arc.output = self.outputs.get_no_output();
    }

    if arc.flag(BIT_ARC_HAS_FINAL_OUTPUT as i32) {
      arc.next_final_output = self.outputs.read_final_output(reader)?;
    } else {
      arc.next_final_output = self.outputs.get_no_output();
    }

    if arc.flag(BIT_STOP_NODE) {
      arc.target = if arc.flag(BIT_FINAL_ARC as i32) {
        FINAL_END_NODE
      } else {
        NON_FINAL_END_NODE
      };
      arc.next_arc = reader.get_position();
    } else if arc.flag(BIT_TARGET_NEXT as i32) {
      arc.next_arc = reader.get_position();

      if !arc.flag(BIT_LAST_ARC as i32) {
        if arc.bytes_per_arc() == 0 {
          self.seek_to_next_node(reader)?;
        } else {
          let num_arcs = if arc.node_flags == ARCS_FOR_DIRECT_ADDRESSING {
            BitTable::count_bits(arc, reader)?
          } else {
            arc.num_arcs()
          };
          let pos = arc.pos_arcs_start - (arc.bytes_per_arc as i64 * num_arcs as i64);
          reader.set_position(pos);
        }
      }

      arc.target = reader.get_position();
    } else {
      arc.target = self.read_unpacked_node_target(reader)?;
      arc.next_arc = reader.get_position();
    }
    Ok(())
  }
  /// Finds an arc leaving the `follow` arc with the specified label, updating
  /// `arc` in-place. This returns `None` if the arc was not found,
  /// otherwise returns the updated `arc`.
  ///
  /// # Arguments
  ///
  /// * `label_to_match` - The label to search for
  /// * `follow` - The arc from which to search
  /// * `arc` - The arc to update
  /// * `input` - The input stream to read from
  ///
  /// # Returns
  ///
  /// `Some(arc)` if the target arc is found, `None` otherwise
  ///
  /// # Errors
  ///
  /// Returns an error if reading from the input fails
  pub fn find_target_arc(
    &self,
    label_to_match: i32,
    follow: &Arc<O::V>,
    arc: &mut Arc<O::V>,
    input: &mut impl BytesReader,
  ) -> Result<Option<()>> {
    if label_to_match == END_LABEL {
      return if follow.is_final() {
        if follow.target() <= 0 {
          arc.flags = BIT_LAST_ARC;
        } else {
          arc.flags = 0;
          arc.next_arc = follow.target();
        }
        arc.output = follow.next_final_output();
        arc.label = END_LABEL;
        arc.node_flags = arc.flags;
        Ok(Some(()))
      } else {
        Ok(None)
      };
    }

    if !target_has_arcs(follow) {
      return Ok(None);
    }

    input.set_position(follow.target());

    let flags = input.read_byte()?;
    arc.node_flags = flags;

    if flags == ARCS_FOR_DIRECT_ADDRESSING {
      arc.num_arcs = input.read_vint()?;
      arc.bytes_per_arc = input.read_vint()?;
      self.read_presence_bytes(arc, input)?;
      arc.first_label = self.read_label(input)?;
      arc.pos_arcs_start = input.get_position();

      let arc_index = label_to_match - arc.first_label;
      if arc_index < 0 || arc_index >= arc.num_arcs {
        return Ok(None); // Before or after label range.
      } else if !BitTable::is_bit_set(arc_index, arc, input)? {
        return Ok(None); // Arc missing in the range.
      }
      return self
        .read_arc_by_direct_addressing(arc, input, arc_index)
        .map(Some);
    } else if flags == ARCS_FOR_BINARY_SEARCH {
      arc.num_arcs = input.read_vint()?;
      arc.bytes_per_arc = input.read_vint()?;
      arc.pos_arcs_start = input.get_position();
      // Array is sparse; do binary search:
      let mut low = 0;
      let mut high = arc.num_arcs - 1;
      while low <= high {
        let mid = (low + high) >> 1;
        input.set_position(arc.pos_arcs_start - (arc.bytes_per_arc * mid + 1) as i64);
        let mid_label = self.read_label(input)?;
        match mid_label.cmp(&label_to_match) {
          std::cmp::Ordering::Less => low = mid + 1,
          std::cmp::Ordering::Greater => high = mid - 1,
          std::cmp::Ordering::Equal => {
            arc.arc_idx = mid - 1;
            return self.read_next_real_arc(arc, input).map(Some);
          },
        }
      }
      return Ok(None);
    } else if flags == ARCS_FOR_CONTINUOUS {
      arc.num_arcs = input.read_vint()?;
      arc.bytes_per_arc = input.read_vint()?;
      arc.first_label = self.read_label(input)?;
      arc.pos_arcs_start = input.get_position();
      let arc_index = label_to_match - arc.first_label;
      if arc_index < 0 || arc_index >= arc.num_arcs {
        return Ok(None); // Before or after label range.
      }
      arc.arc_idx = arc_index - 1;
      return self.read_next_real_arc(arc, input).map(Some);
    }

    self.read_first_arc_info(follow.target(), arc, input)?;
    input.set_position(arc.next_arc);
    loop {
      debug_assert_eq!(arc.bytes_per_arc, 0);
      arc.flags = input.read_byte()?;
      let pos = input.get_position();
      let label = self.read_label(input)?;
      if label == label_to_match {
        input.set_position(pos);
        return self.read_arc(arc, input).map(Some);
      } else if label > label_to_match || arc.is_last() {
        return Ok(None);
      } else {
        let flag = arc.flags as i32;
        if flag_mod(flag, BIT_ARC_HAS_OUTPUT as i32) {
          self.outputs.skip_output(input)?;
        }
        if flag_mod(flag, BIT_ARC_HAS_FINAL_OUTPUT as i32) {
          self.outputs.skip_final_output(input)?;
        }
        if !flag_mod(flag, BIT_STOP_NODE) && !flag_mod(flag, BIT_TARGET_NEXT as i32) {
          self.read_unpacked_node_target(input)?;
        }
      }
    }
  }

  /// Skips over a variable-length arc node until it reaches the last arc.
  pub fn seek_to_next_node(&self, reader: &mut impl BytesReader) -> Result<()> {
    loop {
      let flags = reader.read_byte()?;
      self.read_label(reader)?;

      if flags & BIT_ARC_HAS_OUTPUT != 0 {
        self.outputs.skip_output(reader)?;
      }

      if flags & BIT_ARC_HAS_FINAL_OUTPUT != 0 {
        self.outputs.skip_final_output(reader)?;
      }

      if flag_mod(flags as i32, BIT_STOP_NODE) && flag_mod(flags as i32, BIT_TARGET_NEXT as i32) {
        self.read_unpacked_node_target(reader)?;
      }

      if flags & BIT_LAST_ARC != 0 {
        return Ok(());
      }
    }
  }
  /// Returns a [`BytesReader`] for this FST, positioned at position 0.
  pub fn get_bytes_reader(&self) -> Result<F::FstBytesReader> {
    let mut fst_reader = self.fst_reader.lock();
    fst_reader.init_reader();
    fst_reader.get_reverse_bytes_reader()
  }
}

impl<O, F> Display for FST<O, F>
where
  O: Outputs,
  F: FstReader,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
    write!(
      f,
      "{}(input={:?}, output={})",
      std::any::type_name::<Self>(),
      self.metadata.input_type,
      self.outputs
    )
  }
}

#[derive(Default, Clone, Debug)]
pub struct Arc<T>
where
  T: OutputsBound,
{
  // *** Arc fields.
  pub(crate) label: i32,
  pub(crate) output: T,
  target: i64,
  flags: u8,
  pub(crate) next_final_output: T,
  next_arc: i64,
  node_flags: u8,

  // *** Fields for arcs belonging to a node with fixed length arcs.
  // So only valid when bytes_per_arc != 0.
  // node_flags == ARCS_FOR_BINARY_SEARCH || node_flags ==
  // ARCS_FOR_DIRECT_ADDRESSING.
  bytes_per_arc: i32,
  pos_arcs_start: i64,
  arc_idx: i32,
  num_arcs: i32,

  // *** Fields for a direct addressing node. node_flags ==
  // ARCS_FOR_DIRECT_ADDRESSING.
  /// Start position in the [`BytesReader`] of the presence bits for a direct
  /// addressing node, aka the bit-table
  bit_table_start: i64,

  /// First label of a direct addressing node.
  first_label: i32,

  /// Index of the current label of a direct addressing node. While `arc_idx`
  /// is the current index in the label range, `presence_index` is its
  /// corresponding index in the list of actually present labels. It is
  /// equal to the number of bits set before the bit at `arc_idx`
  /// in the bit-table. This field is a cache to avoid counting bits
  /// repeatedly when iterating arcs.
  presence_index: i32,
}
impl<T> Arc<T>
where
  T: OutputsBound,
{
  pub(crate) fn flag(&self, flag: i32) -> bool {
    flag_mod(self.flags as i32, flag)
  }
  pub fn is_last(&self) -> bool {
    self.flag(BIT_LAST_ARC as i32)
  }
  pub fn is_final(&self) -> bool {
    self.flag(BIT_FINAL_ARC as i32)
  }

  pub fn label(&self) -> i32 {
    self.label
  }
  /// Ord/address to target node.
  pub fn target(&self) -> i64 {
    self.target
  }

  pub fn flags(&self) -> u8 {
    self.flags
  }

  /// Address (into the byte[]) of the next arc - only for list of variable
  /// length arc. Or ord/address to the next node if label ==
  /// [`END_LABEL`].
  pub fn next_arc(&self) -> i64 {
    self.next_arc
  }

  /// Only valid if bytes_per_arc != 0.
  pub fn arc_idx(&self) -> i32 {
    self.arc_idx
  }

  /// Node header flags. Only meaningful to check if the value is either
  /// [`ARCS_FOR_BINARY_SEARCH`] or
  /// [`ARCS_FOR_DIRECT_ADDRESSING`]
  /// or [`ARCS_FOR_CONTINUOUS`] (other value
  /// when bytesPerArc == 0).
  pub fn node_flags(&self) -> u8 {
    self.node_flags
  }

  /// Start position of the arc array (only valid if bytes_per_arc != 0).
  pub fn pos_arcs_start(&self) -> i64 {
    self.pos_arcs_start
  }

  ///  Non-zero if this arc is part of a node with fixed length arcs, which
  /// means all arcs for the  node are encoded with a fixed number of
  /// bytes so that we binary search or direct address. We  do when there
  /// are enough arcs leaving one node. It wastes some bytes but gives faster
  ///  lookups.
  pub fn bytes_per_arc(&self) -> i32 {
    self.bytes_per_arc
  }

  /// How many arcs; only valid if bytesPerArc != 0 (fixed length arcs). For a
  /// node designed for binary search this is the array size. For a node
  /// designed for direct addressing, this is the label range.
  pub fn num_arcs(&self) -> i32 {
    self.num_arcs
  }

  /// First label of a direct addressing node. Only valid if nodeFlags ==
  /// [`ARCS_FOR_DIRECT_ADDRESSING`] or
  /// [`ARCS_FOR_CONTINUOUS`].
  pub fn first_label(&self) -> i32 {
    self.first_label
  }
}
impl<T: Clone> Arc<T>
where
  T: OutputsBound,
{
  /// Returns `self` after copying all fields from `other`.
  pub fn copy_from(&mut self, other: &Arc<T>) {
    self.label = other.label();
    self.target = other.target();
    self.flags = other.flags();
    self.output = other.output();
    self.next_final_output = other.next_final_output();
    self.next_arc = other.next_arc();
    self.node_flags = other.node_flags();
    self.bytes_per_arc = other.bytes_per_arc();
    // Fields for arcs belonging to a node with fixed length arcs.
    // We could avoid copying them if bytesPerArc() == 0 (this was the case
    // with previous code, and the current code
    // still supports that), but it may actually help external uses of FST
    // to have consistent arc state, and debugging
    // is easier.
    self.pos_arcs_start = other.pos_arcs_start();
    self.arc_idx = other.arc_idx();
    self.num_arcs = other.num_arcs();
    self.bit_table_start = other.bit_table_start;
    self.first_label = other.first_label();
    self.presence_index = other.presence_index;
  }
  pub fn output(&self) -> T {
    self.output.clone()
  }
  pub fn next_final_output(&self) -> T {
    self.next_final_output.clone()
  }
}
impl<T: Display + Clone> Display for Arc<T>
where
  T: OutputsBound,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
    write!(f, " target={}", self.target)?;
    write!(f, " label=0x{:x}", self.label)?;

    if self.flag(BIT_FINAL_ARC as i32) {
      write!(f, " final")?;
    }
    if self.flag(BIT_LAST_ARC as i32) {
      write!(f, " last")?;
    }
    if self.flag(BIT_TARGET_NEXT as i32) {
      write!(f, " targetNext")?;
    }
    if self.flag(BIT_STOP_NODE) {
      write!(f, " stop")?;
    }
    if self.flag(BIT_ARC_HAS_OUTPUT as i32) {
      write!(f, " output={}", self.output.clone())?;
    }
    if self.flag(BIT_ARC_HAS_FINAL_OUTPUT as i32) {
      write!(f, " nextFinalOutput={}", self.next_final_output.clone())?;
    }
    if self.bytes_per_arc() != 0 {
      let node_flag_str = match self.node_flags {
        ARCS_FOR_DIRECT_ADDRESSING => "da",
        ARCS_FOR_CONTINUOUS => "cs",
        _ => "bs",
      };
      write!(
        f,
        " arcArray(idx={} of {})({})",
        self.arc_idx, self.num_arcs, node_flag_str
      )?;
    }

    Ok(())
  }
}
pub(crate) struct BitTable;
impl BitTable {
  /// See [`BitTableUtil::is_bit_set`].
  pub(crate) fn is_bit_set<T>(
    bit_index: i32,
    arc: &Arc<T>,
    reader: &mut impl BytesReader,
  ) -> Result<bool>
  where
    T: OutputsBound,
  {
    debug_assert_eq!(arc.node_flags(), ARCS_FOR_DIRECT_ADDRESSING);
    reader.set_position(arc.bit_table_start);
    BitTableUtil::is_bit_set(bit_index, reader)
  }

  /// See [`BitTableUtil::count_bits`]. The count of bit set is the number of
  /// arcs of a direct addressing node.
  pub(crate) fn count_bits<R, T>(arc: &Arc<T>, reader: &mut R) -> Result<i32>
  where
    R: BytesReader,
    T: OutputsBound,
  {
    debug_assert_eq!(arc.node_flags(), ARCS_FOR_DIRECT_ADDRESSING);
    reader.set_position(arc.bit_table_start);
    let num_presence_bytes = get_num_presence_bytes(arc.num_arcs());
    BitTableUtil::count_bits(num_presence_bytes, reader)
  }
  /// See [`BitTableUtil::count_bits_upto`].
  pub(crate) fn count_bits_upto<T>(
    bit_index: i32,
    arc: &Arc<T>,
    reader: &mut impl BytesReader,
  ) -> Result<i32>
  where
    T: OutputsBound,
  {
    debug_assert_eq!(arc.node_flags(), ARCS_FOR_DIRECT_ADDRESSING);
    reader.set_position(arc.bit_table_start);
    BitTableUtil::count_bits_upto(bit_index, reader)
  }

  /// See [`BitTableUtil::next_bit_set`].
  pub(crate) fn next_bit_set<T>(
    bit_index: i32,
    arc: &Arc<T>,
    reader: &mut impl BytesReader,
  ) -> Result<i32>
  where
    T: OutputsBound,
  {
    debug_assert_eq!(arc.node_flags(), ARCS_FOR_DIRECT_ADDRESSING);
    reader.set_position(arc.bit_table_start);
    let num_bytes = get_num_presence_bytes(arc.num_arcs());
    BitTableUtil::next_bit_set(bit_index, num_bytes, reader)
  }

  /// See [`BitTableUtil::previous_bit_set`].
  pub(crate) fn previous_bit_set<T>(
    bit_index: i32,
    arc: &Arc<T>,
    reader: &mut impl BytesReader,
  ) -> Result<i32>
  where
    T: OutputsBound,
  {
    debug_assert_eq!(arc.node_flags(), ARCS_FOR_DIRECT_ADDRESSING);
    reader.set_position(arc.bit_table_start);
    BitTableUtil::previous_bit_set(bit_index, reader)
  }

  /// Asserts the bit-table of the provided [`Arc`] is valid.
  pub(crate) fn assert_is_valid<T>(arc: &Arc<T>, reader: &mut impl BytesReader) -> Result<bool>
  where
    T: OutputsBound,
  {
    debug_assert!(arc.bytes_per_arc() > 0);
    debug_assert_eq!(arc.node_flags(), ARCS_FOR_DIRECT_ADDRESSING);

    // First bit must be set
    debug_assert!(Self::is_bit_set(0, arc, reader)?);

    // Last bit must be set
    debug_assert!(Self::is_bit_set(arc.num_arcs() - 1, arc, reader)?);

    // No bit set after the last arc
    debug_assert_eq!(Self::next_bit_set(arc.num_arcs() - 1, arc, reader)?, -1);

    Ok(true)
  }
}

/// Represents the FST metadata.
///
/// `T` is the FST output type.
#[derive(Default)]
pub struct FSTMetadata<O>
where
  O: Outputs,
{
  pub input_type: InputType,
  pub outputs: O,
  pub version: i32,
  /// If `Some`, this FST accepts the empty string and produces this
  /// output.
  pub empty_output: Option<O::V>,
  pub start_node: i64,
  pub num_bytes: i64,
}
impl<O: Outputs> FSTMetadata<O> {
  pub fn new(
    input_type: InputType,
    outputs: O,
    empty_output: Option<O::V>,
    start_node: i64,
    version: i32,
    num_bytes: i64,
  ) -> Self {
    Self {
      input_type,
      outputs,
      version,
      empty_output,
      start_node,
      num_bytes,
    }
  }

  /// Returns the version constant of the binary format this FST was written
  /// in. See the static version constants in `FST` such as
  /// [`VERSION_CONTINUOUS_ARCS`].
  pub fn version(&self) -> i32 {
    self.version
  }

  pub fn empty_output(&self) -> Option<&O::V> {
    self.empty_output.as_ref()
  }

  pub fn num_bytes(&self) -> i64 {
    self.num_bytes
  }
  /// Save the metadata to a `DataOutput`.
  ///
  /// # Arguments
  ///
  /// * `meta_out` - The `DataOutput` to write the metadata to.
  pub fn save(&self, meta_out: &mut impl DataOutput) -> Result<()> {
    CodecUtil::write_header(meta_out, FILE_FORMAT_NAME, VERSION_CURRENT)?;

    if let Some(ref empty_output) = self.empty_output {
      // Accepts empty string
      meta_out.write_byte(1)?;

      // Serialize empty-string output
      let mut ros = ByteBuffersDataOutput::new();
      self.outputs.write_final_output(empty_output, &mut ros)?;
      let mut empty_output_bytes = ros.try_get_array_ownership();
      let empty_len = empty_output_bytes.len();
      // reverse
      let stop_at = empty_len / 2;
      for i in 0..stop_at {
        empty_output_bytes.swap(i, empty_len - i - 1);
      }

      meta_out.write_vint(empty_len as i32)?;
      debug_assert!(empty_output_bytes.len() <= i32::MAX as usize);
      meta_out.write_bytes_range(&empty_output_bytes, 0, empty_len as i32 as usize)?;
    } else {
      meta_out.write_byte(0)?;
    }

    let t = match self.input_type {
      InputType::Byte1 => 0u8,
      InputType::Byte2 => 1u8,
      InputType::Byte4 => 2u8,
    };
    meta_out.write_byte(t)?;
    meta_out.write_vlong(self.start_node)?;
    meta_out.write_vlong(self.num_bytes)?;

    Ok(())
  }
}

/// Specifies allowed range of each int input label for this FST.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputType {
  #[default]
  Byte1,
  Byte2,
  Byte4,
}

/// Reads bytes stored in an FST.
pub trait BytesReader: DataInput {
  /// Get current read position.
  fn get_position(&self) -> i64;

  /// Set current read position.
  fn set_position(&mut self, pos: i64);
}

pub enum BytesReaderEnum2<A, B> {
  A(A),
  B(B),
}

impl<A, B> crate::core::util::close::Closeable for BytesReaderEnum2<A, B> {}

impl<A, B> DataInput for BytesReaderEnum2<A, B>
where
  A: BytesReader,
  B: BytesReader,
{
  fn read_byte(&mut self) -> Result<u8> {
    match self {
      BytesReaderEnum2::A(reader) => reader.read_byte(),
      BytesReaderEnum2::B(reader) => reader.read_byte(),
    }
  }

  fn read_bytes(&mut self, b: &mut [u8], offset: usize, len: usize) -> Result<()> {
    match self {
      BytesReaderEnum2::A(reader) => reader.read_bytes(b, offset, len),
      BytesReaderEnum2::B(reader) => reader.read_bytes(b, offset, len),
    }
  }

  fn read_group_vint(&mut self, dst: &mut [i32], offset: usize) -> Result<()> {
    match self {
      BytesReaderEnum2::A(reader) => reader.read_group_vint(dst, offset),
      BytesReaderEnum2::B(reader) => reader.read_group_vint(dst, offset),
    }
  }

  fn skip_bytes(&mut self, num_bytes: i64) -> Result<()> {
    match self {
      BytesReaderEnum2::A(reader) => reader.skip_bytes(num_bytes),
      BytesReaderEnum2::B(reader) => reader.skip_bytes(num_bytes),
    }
  }
}

impl<A, B> Display for BytesReaderEnum2<A, B>
where
  A: BytesReader,
  B: BytesReader,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
    match self {
      BytesReaderEnum2::A(reader) => {
        write!(f, "BytesReaderEnum(F: {})", reader.get_position())
      },
      BytesReaderEnum2::B(reader) => {
        write!(f, "BytesReaderEnum(S: {})", reader.get_position())
      },
    }
  }
}

impl<A, B> BytesReader for BytesReaderEnum2<A, B>
where
  A: BytesReader,
  B: BytesReader,
{
  fn get_position(&self) -> i64 {
    match self {
      BytesReaderEnum2::A(reader) => reader.get_position(),
      BytesReaderEnum2::B(reader) => reader.get_position(),
    }
  }

  fn set_position(&mut self, pos: i64) {
    match self {
      BytesReaderEnum2::A(reader) => reader.set_position(pos),
      BytesReaderEnum2::B(reader) => reader.set_position(pos),
    }
  }
}

use crate::core::util::fst_impl::fst_compiler::get_on_heap_reader_writer;

pub(crate) const BIT_FINAL_ARC: u8 = 1 << 0;
pub(crate) const BIT_LAST_ARC: u8 = 1 << 1;
pub(crate) const BIT_TARGET_NEXT: u8 = 1 << 2;

pub(crate) const BIT_STOP_NODE: i32 = 1 << 3;

/// This flag is set if the arc has an output.
pub const BIT_ARC_HAS_OUTPUT: u8 = 1 << 4;
pub(crate) const BIT_ARC_HAS_FINAL_OUTPUT: u8 = 1 << 5;

/// Value of the arc flags to declare a node with fixed length (sparse) arcs
/// designed for binary search.
pub const ARCS_FOR_BINARY_SEARCH: u8 = BIT_ARC_HAS_FINAL_OUTPUT;

/// Value of the arc flags to declare a node with fixed length dense arcs
/// and bit table designed for direct addressing.
pub const ARCS_FOR_DIRECT_ADDRESSING: u8 = 1 << 6;

///  Value of the arc flags to declare a node with continuous arcs designed
/// for pos the arc directly  with labelToPos - firstLabel. like
/// [`ARCS_FOR_BINARY_SEARCH`] we use flag
/// combinations  that will not occur at the same time.
pub const ARCS_FOR_CONTINUOUS: u8 = ARCS_FOR_DIRECT_ADDRESSING + ARCS_FOR_BINARY_SEARCH;

/// Format name for the FST file.
const FILE_FORMAT_NAME: &str = "FST";

/// First supported version (Lucene 7.0).
pub const VERSION_START: i32 = 6;
// Version 7 introduced direct addressing for arcs, but it's not recorded
// here because it doesn't need version checks on the read side, it uses
// new flag values on arcs instead.
const VERSION_LITTLE_ENDIAN: i32 = 8;

/// Version that started storing continuous arcs.
pub const VERSION_CONTINUOUS_ARCS: i32 = 9;

/// Current format version.
pub const VERSION_CURRENT: i32 = VERSION_CONTINUOUS_ARCS;

/// Version that was used for Lucene 9.0.
pub const VERSION_90: i32 = VERSION_LITTLE_ENDIAN;

/// Represents a final virtual node with no arcs (never serialized).
pub(crate) const FINAL_END_NODE: i64 = -1;

/// Represents a non-final virtual node with no arcs (never serialized).
pub(crate) const NON_FINAL_END_NODE: i64 = 0;

/// If arc has this label then that arc is final/accepted.
pub const END_LABEL: i32 = -1;
#[cfg(target_pointer_width = "64")]
pub const DEFAULT_MAX_BLOCK_BITS: i32 = 30;

#[cfg(not(target_pointer_width = "64"))]
pub const DEFAULT_MAX_BLOCK_BITS: i32 = 28;
#[inline]
pub(crate) fn flag_mod(flags: i32, bit: i32) -> bool {
  (flags & bit) != 0
}

/// Read the FST metadata from DataInput
///
/// # Type Parameters
///
/// * `T` - the output type
///
/// # Arguments
///
/// * `metaIn` - the DataInput of the metadata
/// * `outputs` - the FST outputs
///
/// # Returns
///
/// the FST metadata
///
/// # Errors
///
/// Returns an error if parsing fails.
pub fn read_metadata<O>(meta_in: &mut impl DataInput, outputs: O) -> Result<FSTMetadata<O>>
where
  O: Outputs,
{
  // NOTE: only reads formats VERSION_START up to VERSION_CURRENT; we
  // don't have back-compat promise for FSTs (they are
  // experimental), but we are sometimes able to offer it
  let version = CodecUtil::check_header(meta_in, FILE_FORMAT_NAME, VERSION_START, VERSION_CURRENT)?;

  let mut empty_output: Option<O::V> = None;

  if meta_in.read_byte()? == 1 {
    // Accepts empty string
    // 1 KB blocks:
    let mut empty_bytes = get_on_heap_reader_writer(10)?;
    let num_bytes = meta_in.read_vint()?;
    empty_bytes.copy_bytes(meta_in, num_bytes as usize)?;
    empty_bytes.freeze()?;
    empty_bytes.init_reader();
    // De-serialize empty-string output:
    let mut reader = empty_bytes.get_reverse_bytes_reader()?;
    // NoOutputs uses 0 bytes when writing its output,
    // so we have to check here else BytesStore gets angry:
    if num_bytes > 0 {
      reader.set_position((num_bytes - 1) as i64);
    }
    empty_output = Some(outputs.read_final_output(&mut reader)?);
  }
  let input_type = match meta_in.read_byte()? {
    0 => InputType::Byte1,
    1 => InputType::Byte2,
    2 => InputType::Byte4,
    invalid => {
      return Err(LuceneError::corrupt_index(format!(
        "invalid input type {invalid} (resource={meta_in})"
      )));
    },
  };

  let start_node = meta_in.read_vlong()?;
  let num_bytes = meta_in.read_vlong()?;

  Ok(FSTMetadata::new(
    input_type,
    outputs,
    empty_output,
    start_node,
    version,
    num_bytes,
  ))
}

/// Returns `true` if the node at this address has any outgoing arcs.
pub fn target_has_arcs<T>(arc: &Arc<T>) -> bool
where
  T: OutputsBound,
{
  arc.target() > 0
}
/// Gets the number of bytes required to flag the presence of each arc in
/// the given label range, one bit per arc.
pub(crate) fn get_num_presence_bytes(label_range: i32) -> i32 {
  debug_assert!(label_range >= 0);
  (label_range + 7) >> 3
}
/// Reads the presence bits of a direct-addressing node. Actually we don't
/// read them here, we just keep the pointer to the bit-table start and
/// we skip them.
pub(crate) fn read_presence_bytes<T>(arc: &mut Arc<T>, reader: &mut impl BytesReader) -> Result<()>
where
  T: OutputsBound,
{
  debug_assert!(arc.bytes_per_arc() > 0);
  debug_assert_eq!(arc.node_flags(), ARCS_FOR_DIRECT_ADDRESSING);
  arc.bit_table_start = reader.get_position();
  let skip = get_num_presence_bytes(arc.num_arcs());
  reader.skip_bytes(skip as i64)
}
/// Reads the end arc based on the `follow` arc.
/// If `follow` is final, this sets up an artificial END_LABEL arc in-place
/// and returns it. Otherwise, returns `None`.
///
/// # Arguments
///
/// * `follow` - The arc to follow
/// * `arc` - The arc to fill and return
///
/// # Returns
///
/// The updated `arc` if `follow` is final, otherwise `None`
pub(crate) fn read_end_arc<T>(follow: &Arc<T>, arc: &mut Arc<T>) -> Option<()>
where
  T: OutputsBound + Clone,
{
  if follow.is_final() {
    if follow.target() <= 0 {
      arc.flags = BIT_LAST_ARC;
    } else {
      arc.flags = 0;
      // NOTE: nextArc is a node (not an address!) in this case:
      arc.next_arc = follow.target();
    }
    arc.output = follow.next_final_output();
    arc.label = END_LABEL;
    Some(())
  } else {
    None
  }
}
