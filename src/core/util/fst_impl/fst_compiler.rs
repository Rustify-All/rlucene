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
use std::fmt::{Display, Formatter};

use crate::core::store::directory::Directory;
use crate::core::store::{ByteArrayDataOutput, DataOutput};
use crate::core::util::accountable::Accountable;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fst_impl::dummy::dummy_bytes_reader::DummyBytesReader;
use crate::core::util::fst_impl::fst::{
    ARCS_FOR_BINARY_SEARCH, ARCS_FOR_CONTINUOUS, ARCS_FOR_DIRECT_ADDRESSING,
    BIT_ARC_HAS_FINAL_OUTPUT, BIT_ARC_HAS_OUTPUT, BIT_FINAL_ARC, BIT_LAST_ARC, BIT_STOP_NODE,
    BIT_TARGET_NEXT, Either2BytesReader, FINAL_END_NODE, FST, FSTMetadata, InputType,
    NON_FINAL_END_NODE, VERSION_90, VERSION_CONTINUOUS_ARCS, VERSION_CURRENT,
    get_num_presence_bytes,
};
use crate::core::util::fst_impl::fst_reader::FstReader;
use crate::core::util::fst_impl::growable_byte_array_data_output::GrowableByteArrayDataOutput;
use crate::core::util::fst_impl::node_hash::NodeHash;
use crate::core::util::fst_impl::outputs::{Outputs, OutputsBound};
use crate::core::util::fst_impl::read_write_data_output::{BytesReaderImpl, ReadWriteDataOutput};
use crate::core::util::fst_impl::reverse_bytes_reader::ReverseBytesReader;
use crate::core::util::ints_ref::IntsRef;
use crate::core::util::ints_ref_builder::IntsRefBuilder;
use crate::core::util::{OutputIdentity, SliceCopyOps};

/// Builds a minimal FST (maps an `IntsRef` term to an arbitrary output) from
/// pre-sorted terms with outputs. The FST becomes an FSA if you use
/// `NoOutputs`. The FST is written on-the-fly into a compact serialized format
/// byte array, which can be saved to / loaded from a `Directory` or used
/// directly for traversal. The FST is always finite (no cycles).
///
///
/// **NOTE**: The algorithm is described at:
/// <http://citeseerx.ist.psu.edu/viewdoc/summary?doi=10.1.1.24.3698>
///
///
/// The parameterized type `T` is the output type. See the SubStruct of
/// [`Outputs`].
///
///
/// FSTs larger than 2.1GB are now possible (as of Lucene 4.2). FSTs containing
/// more than 2.1B nodes are also now possible, however they cannot be packed.
///
///
/// It now supports 3 different workflows:
///
/// - Build FST and use it immediately entirely in RAM and then discard it
/// - Build FST and use it immediately entirely in RAM and also save it to other
///   `DataOutput`, and load it later and use it
/// - Build FST but stream it immediately to disk (except the `FSTMetaData`, to
///   be saved at the end). In order to use it, you need to construct the
///   corresponding `DataInput` and use the FST constructor to read it.
pub struct FSTCompiler<O, D>
where
    O: Outputs,
    D: Directory,
{
    pub(crate) dedup_hash: NodeHash<O::V>,
    /// A temporary FST used during building for NodeHash cache.
    pub(crate) fst: FST<O, NullFSTReader>,
    pub(crate) no_output: O::V,
    /// A FSTReader used when a non-FSTReader DataOutput is configured.
    /// Will panic if `get_reverse_bytes_reader()` or `write_to()` is called.
    pub(crate) null_fst_reader: NullFSTReader,
    /// Node deduplication hash table.
    /// Last input added.
    pub(crate) last_input: IntsRefBuilder<Vec<i32>>,
    /// Whether the initial padding byte needs to be written.
    pub(crate) padding_byte_pending: bool,

    /// Used for the BIT_TARGET_NEXT optimization (whereby
    /// instead of storing the address of the target node for
    /// a given arc, we mark a single bit noting that the next
    /// node in the byte[] is the target node):
    pub(crate) last_frozen_node: i64,
    /// Reused temporarily while building the FST:
    pub(crate) num_bytes_per_arc: Vec<i32>,
    pub(crate) num_label_bytes_per_arc: Vec<i32>,
    pub(crate) fixed_length_arcs_buffer: FixedLengthArcsBuffer,
    pub(crate) arc_count: i64,
    pub(crate) node_count: i64,
    pub(crate) binary_search_node_count: i64,
    pub(crate) direct_addressing_node_count: i64,
    pub(crate) continuous_node_count: i64,
    pub(crate) allow_fixed_length_arcs: bool,
    pub(crate) direct_addressing_max_oversizing_factor: f32,
    pub(crate) version: i32,
    pub(crate) direct_addressing_expansion_credit: i64,
    pub(crate) data_output: DataOutputEnum<D>,
    pub(crate) scratch_bytes: GrowableByteArrayDataOutput,
    pub(crate) num_bytes_written: i64,
    /// NOTE: cutting this over to ArrayList instead loses ~6%
    /// in build performance on 9.8M Wikipedia terms; so we
    /// left this as an array:
    /// current "frontier"
    /// # Note:
    /// Wrap with `Option` for easy frontier growing
    pub(crate) frontier: Vec<Option<UnCompiledNode<O::V>>>,
}
impl<O, D> FSTCompiler<O, D>
where
    O: Outputs,
    D: Directory,
{
    fn new(
        input_type: InputType,
        suffix_ram_limit_mb: f64,
        outputs: O,
        allow_fixed_length_arcs: bool,
        data_output: DataOutputEnum<D>,
        direct_addressing_max_oversizing_factor: f32,
        version: i32,
    ) -> Result<Self> {
        if suffix_ram_limit_mb < 0.0 {
            return Err(LuceneError::illegal_argument(format!(
                "ramLimitMB must be >= 0; got: {suffix_ram_limit_mb}"
            )));
        }

        let num_bytes_written = 1; // pad 1 byte, written lazily
        let padding_byte_pending = true;

        let null_fst_reader = NullFSTReader; // assume you implemented Default
        let no_output = outputs.get_no_output();
        let fst_meta = FSTMetadata::new(input_type, outputs, None, -1, version, 0);
        let fst = FST::new(fst_meta, NullFSTReader);
        let mut frontier = vec![];
        for i in 0..10 {
            frontier.push(Some(UnCompiledNode::new(no_output.clone(), i)));
        }
        let dedup_hash = NodeHash::new(suffix_ram_limit_mb)?;
        Ok(Self {
            dedup_hash,
            fst,
            no_output,
            null_fst_reader,
            last_input: IntsRefBuilder::default(),
            padding_byte_pending,
            last_frozen_node: 0,
            num_bytes_per_arc: vec![],
            num_label_bytes_per_arc: vec![],
            fixed_length_arcs_buffer: FixedLengthArcsBuffer::new(),
            arc_count: 0,
            node_count: 0,
            binary_search_node_count: 0,
            direct_addressing_node_count: 0,
            continuous_node_count: 0,
            allow_fixed_length_arcs,
            direct_addressing_max_oversizing_factor,
            version,
            direct_addressing_expansion_credit: 0,
            data_output,
            scratch_bytes: GrowableByteArrayDataOutput::new(),
            num_bytes_written,
            frontier,
        })
    }
    fn compile_node(&mut self, node_in_idx: usize) -> Result<(CompiledNode, usize)> {
        let num_arcs = self.frontier[node_in_idx].as_mut().unwrap().num_arcs;

        let bytes_pos_start = self.num_bytes_written;

        let node = if num_arcs == 0 {
            let node = self.add_node(node_in_idx)?;
            self.last_frozen_node = node;
            node
        } else {
            NodeHash::add(node_in_idx, self)?
        };

        debug_assert!(node != -2);

        let bytes_pos_end = self.num_bytes_written;
        if bytes_pos_end != bytes_pos_start {
            // The FST added a new node:
            debug_assert!(bytes_pos_end > bytes_pos_start);
            self.last_frozen_node = node;
        }

        let v = self.no_output.clone();
        self.frontier[node_in_idx].as_mut().unwrap().clear(v);

        Ok((CompiledNode { node }, node_in_idx))
    }
    fn freeze_tail(&mut self, prefix_len_plus1: i32) -> Result<()> {
        let (len, down_to) = { (self.last_input.length(), prefix_len_plus1.max(1) as usize) };

        for idx in (down_to..=len).rev() {
            let (label, next_final_output, is_final, prev_idx) = {
                let node = self.frontier[idx].as_ref().unwrap();
                let prev_idx = idx - 1;

                let next_final_output = node.output.clone();
                // We "fake" the node as being final if it has no
                // outgoing arcs; in theory we could leave it
                // as non-final (the FST can represent this), but
                // FSTEnum, Util, etc., have trouble w/ non-final
                // dead-end states:
                let is_final = node.is_final || node.num_arcs == 0;
                let label = self.last_input.int_at(prev_idx);
                (label, next_final_output, is_final, prev_idx)
            };
            let (compiled, _) = self.compile_node(idx)?;
            let parent = self.frontier[prev_idx].as_mut().unwrap();
            // this node makes it and we now compile it.  first,
            // compile any targets that were previously
            // undecided:
            parent.replace_last(
                label,
                NodeEnum::CompiledNode(compiled),
                next_final_output,
                is_final,
            );
        }

        Ok(())
    }
    /// Add the next input/output pair. The provided input must be sorted after
    /// the previous one according to [`IntsRef::cmp`]. It's also OK to add
    /// the same input twice in a row with different outputs, as long as
    /// [`Outputs`] implements the [`Outputs::merge`] method. Note
    /// that input is fully consumed after this method returns (so the caller is
    /// free to reuse), but output is not. So if your outputs are changeable
    /// (e.g. [`ByteSequenceOutputs`](crate::core::util::fst_impl::byte_sequence_outputs::ByteSequenceOutputs)
    /// or
    /// [`IntSequenceOutputs`](crate::core::util::fst_impl::int_sequence_outputs::IntSequenceOutputs)), then you cannot reuse them across calls.
    pub(crate) fn add(&mut self, input: &IntsRef<Vec<i32>>, mut output: O::V) -> Result<()> {
        let ints = &input.ints;

        // De-dup NO_OUTPUT since it must be a singleton:
        if output == self.no_output {
            output = self.no_output.clone();
        }

        debug_assert!(
            self.last_input.length() == 0 || *input >= *self.last_input.get(),
            "inputs are added out of order lastInput={:?} vs input={:?}",
            self.last_input.get(),
            input
        );
        debug_assert!(self.valid_output(&output));

        if input.length == 0 {
            // empty input: only allowed as first input.  we have
            // to special case this because the packed FST
            // format cannot represent the empty input since
            // 'finalness' is stored on the incoming arc, not on
            // the node
            self.frontier[0].as_mut().unwrap().is_final = true;
            self.set_empty_output(output)?;
            return Ok(());
        }

        // Compare shared prefix length
        let mut pos1 = 0;
        let mut pos2 = input.offset;
        let pos1_stop = self.last_input.length().min(input.length);
        while pos1 < pos1_stop && self.last_input.int_at(pos1) == ints[pos2] {
            pos1 += 1;
            pos2 += 1;
        }
        let prefix_len_plus1 = pos1 + 1;

        if self.frontier.len() < (input.length + 1) {
            let old_len = self.frontier.len();
            debug_assert!(old_len <= i32::MAX as usize);
            ArrayUtil::grow_with_len(&mut self.frontier, input.length);
            debug_assert!(self.frontier.len() <= i32::MAX as usize);
            for i in old_len..self.frontier.len() {
                self.frontier[i] = Some(UnCompiledNode::new(self.no_output.clone(), i as i32));
            }
        }
        // minimize/compile states from previous input's
        // orphan'd suffix
        debug_assert!(prefix_len_plus1 <= i32::MAX as usize);
        self.freeze_tail(prefix_len_plus1 as i32)?;
        let no_output = self.no_output.clone();
        // init tail states for current input
        let offset = input.offset;
        for idx in prefix_len_plus1..=input.length {
            let label = ints[offset + idx - 1];
            let un_compiled = NodeEnum::UnCompiledNode(idx);
            let v = self.no_output.clone();
            self.frontier[idx - 1]
                .as_mut()
                .unwrap()
                .add_arc(label, un_compiled, v)?;
        }

        let last_input_len = self.last_input.length();
        let last_node = self.frontier[input.length].as_mut().unwrap();
        if last_input_len != input.length || prefix_len_plus1 != input.length + 1 {
            last_node.is_final = true;
            last_node.output = no_output.clone();
        }
        // push conflicting outputs forward, only as far as
        // needed
        for idx in 1..prefix_len_plus1 {
            let (last_output, label) = {
                let parent = &mut self.frontier[idx - 1];
                let label = ints[offset + idx - 1];
                (parent.as_mut().unwrap().get_last_output(label), label)
            };

            debug_assert!(self.valid_output(&last_output));

            let common_output_prefix;
            if !self.no_output.is_same_reference(&last_output) {
                common_output_prefix = self.fst.outputs.common(&output, &last_output);
                debug_assert!(self.valid_output(&common_output_prefix));

                let word_suffix = self
                    .fst
                    .outputs
                    .subtract(&last_output, &common_output_prefix);
                debug_assert!(self.valid_output(&word_suffix));

                UnCompiledNode::<O::V>::set_last_output(
                    label,
                    common_output_prefix.clone(),
                    self,
                    idx - 1,
                );
                UnCompiledNode::<O::V>::prepend_output(&word_suffix, self, idx);
            } else {
                common_output_prefix = self.no_output.clone();
            }

            output = self.fst.outputs.subtract(&output, &common_output_prefix);
            debug_assert!(self.valid_output(&output));
        }
        if self.last_input.length() == input.length && prefix_len_plus1 == input.length + 1 {
            // same input more than 1 time in a row,
            // mapping to multiple outputs
            let output = self.frontier[input.offset].as_mut().unwrap().output.clone();
            let last_node = self.frontier[input.length].as_ref().unwrap();
            let v = self.fst.outputs.merge(&last_node.output, &output)?;
            self.frontier[input.length].as_mut().unwrap().output = v;
        } else {
            // this new arc is private to this new input; set its
            // arc output to the leftover output:
            let label = ints[input.offset + prefix_len_plus1 - 1];
            UnCompiledNode::<O::V>::set_last_output(label, output, self, prefix_len_plus1 - 1);
        }

        // Save last input
        self.last_input.copy_ints_ref(input);
        Ok(())
    }
    /// Returns the metadata of the final FST. NOTE: this will return null if
    /// nothing is accepted by the FST themselves.
    ///
    /// To create the FST, you need to:
    ///
    /// - If a FSTReader DataOutput was used, such as the one returned by
    ///   [`getOnHeapReaderWriter`](get_on_heap_reader_writer)
    ///
    /// ```text
    ///     fstMetadata = fstCompiler.compile();
    ///     fst = FST.fromFSTReader(fstMetadata, fstCompiler.getFSTReader());
    /// ```
    ///
    /// - If a non-FSTReader DataOutput was used, such as
    ///   [`IndexOutput`](crate::core::store::index_output::IndexOutput), you need to
    ///   first create the corresponding
    ///   [`DataInput`](crate::core::store::data_input::DataInput), such as
    ///   [`IndexInput`](crate::core::store::data_input::DataInput) then pass it to
    ///   the FST construct
    pub fn compile(&mut self) -> Result<Option<FSTMetadata<O>>> {
        // Minimize nodes in the last word's suffix
        self.freeze_tail(0)?;
        if self.frontier[0].as_ref().unwrap().num_arcs == 0 {
            if self.fst.metadata.as_ref().unwrap().empty_output.is_none() {
                // return null for completely empty FST which accepts nothing
                return Ok(None);
            } else {
                // we haven't written the padding byte so far, but the FST is
                // still valid
                self.write_padding_byte()?;
            }
        }
        let (compiled_root, _) = self.compile_node(0)?;
        self.finish(compiled_root.node)?;
        Ok(Some(self.fst.metadata.take().unwrap()))
    }
    // serializes new node by appending its bytes to the end
    // of the current byte[]
    pub(crate) fn add_node(&mut self, node_in_idx: usize) -> Result<i64> {
        let node_in = self.frontier[node_in_idx].as_ref().unwrap();
        if node_in.num_arcs == 0 {
            return Ok(if node_in.is_final {
                FINAL_END_NODE
            } else {
                NON_FINAL_END_NODE
            });
        }
        // reset the scratch writer to prepare for new write
        self.scratch_bytes.set_position(0);

        let do_fixed_length_arcs = self.should_expand_node_with_fixed_length_arcs(node_in);
        if do_fixed_length_arcs && self.num_bytes_per_arc.len() < node_in.num_arcs as usize {
            let new_len = ArrayUtil::oversize(node_in.num_arcs as usize, BitUtil::INT_BYTES);
            self.num_bytes_per_arc = vec![0; new_len];
            self.num_label_bytes_per_arc = vec![0; new_len];
        }

        self.arc_count += node_in.num_arcs as i64;

        let last_arc = node_in.num_arcs - 1;
        let mut last_arc_start = 0;
        let mut max_bytes_per_arc = 0;
        let mut max_bytes_per_arc_without_label = 0;

        for (arc_idx, arc) in node_in
            .arcs
            .iter()
            .enumerate()
            .take(node_in.num_arcs as usize)
        {
            match &arc.target {
                NodeEnum::CompiledNode(target) => {
                    let mut flags = 0;

                    if arc_idx == last_arc as usize {
                        flags |= BIT_LAST_ARC as i32;
                    }

                    if self.last_frozen_node == target.node && !do_fixed_length_arcs {
                        // TODO: for better perf (but more RAM used) we
                        // could avoid this except when arc is "near" the
                        // last arc:
                        flags |= BIT_TARGET_NEXT as i32;
                    }

                    if arc.is_final {
                        flags |= BIT_FINAL_ARC as i32;
                        if !self.no_output.is_same_reference(&arc.next_final_output) {
                            flags |= BIT_ARC_HAS_FINAL_OUTPUT as i32;
                        }
                    } else {
                        debug_assert!(self.no_output.is_same_reference(&arc.next_final_output));
                    }

                    let target_has_arcs = target.node > 0;
                    if !target_has_arcs {
                        flags |= BIT_STOP_NODE;
                    }

                    if !self.no_output.is_same_reference(&arc.output) {
                        flags |= BIT_ARC_HAS_OUTPUT as i32;
                    }

                    self.scratch_bytes.write_byte(flags as u8)?;

                    let label_start = self.scratch_bytes.get_position();
                    // this code should be keep same with `self.write_label`;
                    {
                        debug_assert!(arc.label >= 0, "v = {}", arc.label);

                        match self.fst.metadata.as_ref().unwrap().input_type {
                            InputType::Byte1 => {
                                debug_assert!(arc.label <= 255, "v = {}", arc.label);
                                self.scratch_bytes.write_byte(arc.label as u8)?;
                            },
                            InputType::Byte2 => {
                                debug_assert!(arc.label <= 65535, "v = {}", arc.label);
                                self.scratch_bytes.write_short(arc.label as i16)?;
                            },
                            InputType::Byte4 => {
                                self.scratch_bytes.write_vint(arc.label)?;
                            },
                        }
                    }
                    let label_end = self.scratch_bytes.get_position();
                    let num_label_bytes = label_end - label_start;

                    if !self.no_output.is_same_reference(&arc.output) {
                        self.fst
                            .outputs
                            .write(&arc.output, &mut self.scratch_bytes)?;
                    }

                    if !self.no_output.is_same_reference(&arc.next_final_output) {
                        self.fst
                            .outputs
                            .write_final_output(&arc.next_final_output, &mut self.scratch_bytes)?;
                    }

                    if target_has_arcs && (flags & BIT_TARGET_NEXT as i32) == 0 {
                        self.scratch_bytes.write_vlong(target.node)?;
                    }

                    if do_fixed_length_arcs {
                        let num_arc_bytes = self.scratch_bytes.get_position() - last_arc_start;
                        self.num_bytes_per_arc[arc_idx] = num_arc_bytes;
                        self.num_label_bytes_per_arc[arc_idx] = num_label_bytes;
                        last_arc_start = self.scratch_bytes.get_position();
                        max_bytes_per_arc = max_bytes_per_arc.max(num_arc_bytes);
                        max_bytes_per_arc_without_label =
                            max_bytes_per_arc_without_label.max(num_arc_bytes - num_label_bytes);
                    }
                },
                NodeEnum::UnCompiledNode(_) => {
                    return Err(LuceneError::illegal_state("should be compiled"));
                },
            }
        }
        // TODO: try to avoid wasteful cases: disable doFixedLengthArcs in that
        // case
        /*
         *
         * LUCENE-4682: what is a fair heuristic here?
         * It could involve some of these:
         * 1. how "busy" the node is: nodeIn.inputCount relative to frontier[0].inputCount?
         * 2. how much binSearch saves over scan: nodeIn.numArcs
         * 3. waste: numBytes vs numBytesExpanded
         *
         * the one below just looks at #3
        if (doFixedLengthArcs) {
          // rough heuristic: make this 1.25 "waste factor" a parameter to the phd ctor????
          int numBytes = lastArcStart - startAddress;
          int numBytesExpanded = maxBytesPerArc * nodeIn.numArcs;
          if (numBytesExpanded > numBytes*1.25) {
            doFixedLengthArcs = false;
          }
        }
         */
        if do_fixed_length_arcs {
            debug_assert!(max_bytes_per_arc > 0);
            let label_range = node_in.arcs[last_arc as usize].label - node_in.arcs[0].label + 1;
            debug_assert!(label_range > 0);
            let continuous_label = label_range == node_in.num_arcs;
            if continuous_label && self.version >= VERSION_CONTINUOUS_ARCS {
                self.write_node_for_direct_addressing_or_continuous(
                    node_in_idx,
                    max_bytes_per_arc_without_label,
                    label_range,
                    true,
                )?;
                self.continuous_node_count += 1;
            } else if self.should_expand_node_with_direct_addressing(
                node_in_idx,
                max_bytes_per_arc,
                max_bytes_per_arc_without_label,
                label_range,
            ) {
                self.write_node_for_direct_addressing_or_continuous(
                    node_in_idx,
                    max_bytes_per_arc_without_label,
                    label_range,
                    false,
                )?;
                self.direct_addressing_node_count += 1;
            } else {
                self.write_node_for_binary_search(node_in_idx, max_bytes_per_arc)?;
                self.binary_search_node_count += 1;
            }
        }

        self.reverse_scratch_bytes();
        // write the padding byte if needed
        if self.padding_byte_pending {
            self.write_padding_byte()?;
        }

        self.scratch_bytes
            .write_to_data_output(&mut self.data_output)?;
        self.num_bytes_written += self.scratch_bytes.get_position() as i64;

        self.node_count += 1;
        Ok(self.num_bytes_written - 1)
    }
    /// Get the respective [`DataOutputEnum`]. To call this method, you need
    /// to use the default `DataOutput` or
    /// [`get_on_heap_reader_writer`],
    /// otherwise an error will be thrown.
    pub fn get_fst_reader(&mut self) -> Result<DataOutputEnum<D>> {
        let is_fst_reader = match self.data_output {
            DataOutputEnum::FromDir(_) => false,
            DataOutputEnum::ReadWriter(_) => true,
        };
        if is_fst_reader {
            let v = std::mem::replace(
                &mut self.data_output,
                DataOutputEnum::ReadWriter(ReadWriteDataOutput::default()),
            );
            Ok(v)
        } else {
            Err(LuceneError::illegal_state(format!(
                "The DataOutput must implement FSTReader, but got {}",
                self.data_output
            )))
        }
    }
    pub fn get_direct_addressing_max_oversizing_factor(&self) -> f32 {
        self.direct_addressing_max_oversizing_factor
    }

    pub fn get_node_count(&self) -> i64 {
        1 + self.node_count
    }

    pub fn get_arc_count(&self) -> i64 {
        self.arc_count
    }
    /// Write the padding byte, ensure no node gets address 0 which is reserved
    /// to mean the stop state w/ no arcs
    fn write_padding_byte(&mut self) -> Result<()> {
        debug_assert!(self.padding_byte_pending);
        self.data_output.write_byte(0)?;
        self.padding_byte_pending = false;
        Ok(())
    }
    //
    // Due to Rust's borrowing rules, this method performs a mutable operation
    // within logic that holds an immutable reference to self. Therefore, we
    // manually inlined this function into the code.
    #[allow(dead_code)]
    fn write_label(&mut self, _v: i32) -> Result<()> {
        Ok(())
    }

    /// Returns whether the given node should be expanded with fixed length
    /// arcs. Nodes will be expanded depending on their depth (distance from
    /// the root node) and their number of arcs.
    ///
    ///
    /// Nodes with fixed length arcs use more space, because they encode all
    /// arcs with a fixed number of bytes, but they allow either binary
    /// search or direct addressing on the arcs (instead of linear scan) on
    /// lookup by arc label.
    fn should_expand_node_with_fixed_length_arcs(&self, node: &UnCompiledNode<O::V>) -> bool {
        self.allow_fixed_length_arcs
            && ((node.depth <= FIXED_LENGTH_ARC_SHALLOW_DEPTH
                && node.num_arcs >= FIXED_LENGTH_ARC_SHALLOW_NUM_ARCS)
                || node.num_arcs >= FIXED_LENGTH_ARC_DEEP_NUM_ARCS)
    }
    /// Returns whether the given node should be expanded with direct addressing
    /// instead of binary search.
    ///
    ///
    /// Prefer direct addressing for performance if it does not oversize binary
    /// search byte size too much, so that the arcs can be directly
    /// addressed by label.
    ///
    ///
    /// # See also
    ///
    /// [`FSTCompiler::get_direct_addressing_max_oversizing_factor`](Self::get_direct_addressing_max_oversizing_factor)
    fn should_expand_node_with_direct_addressing(
        &mut self,
        node_in_idx: usize,
        num_bytes_per_arc: i32,
        max_bytes_per_arc_without_label: i32,
        label_range: i32,
    ) -> bool {
        // Anticipate precisely the size of the encodings.
        let node_in = self.frontier[node_in_idx].as_ref().unwrap();
        let size_for_binary_search = num_bytes_per_arc * node_in.num_arcs;
        let size_for_direct_addressing = get_num_presence_bytes(label_range)
            + self.num_label_bytes_per_arc[0]
            + max_bytes_per_arc_without_label * node_in.num_arcs;

        // Determine the allowed oversize compared to binary search.
        // This is defined by a parameter of FST Builder (default 1: no
        // oversize).
        let allowed_oversize = (size_for_binary_search as f32
            * self.get_direct_addressing_max_oversizing_factor())
            as i32;
        let expansion_cost = size_for_direct_addressing - allowed_oversize;

        // Select direct addressing if either:
        // - Direct addressing size is smaller than binary search. In this case,
        //   increment the credit by the reduced size (to use it later).
        // - Direct addressing size is larger than binary search, but the positive
        //   credit allows the oversizing. In this case, decrement the credit by the
        //   oversize.
        // In addition, do not try to oversize to a clearly too large node size
        // (this is the DIRECT_ADDRESSING_MAX_OVERSIZE_WITH_CREDIT_FACTOR
        // parameter).
        if expansion_cost == 0
            || (self.direct_addressing_expansion_credit >= expansion_cost as i64
                && size_for_direct_addressing
                    <= (allowed_oversize as f32 * DIRECT_ADDRESSING_MAX_OVERSIZE_WITH_CREDIT_FACTOR)
                        as i32)
        {
            self.direct_addressing_expansion_credit -= expansion_cost as i64;
            return true;
        }

        false
    }
    fn write_node_for_binary_search(
        &mut self,
        node_in_idx: usize,
        max_bytes_per_arc: i32,
    ) -> Result<()> {
        // Build the header in a buffer.
        // It is a false/special arc which is in fact a node header with node
        // flags followed by node metadata.
        // self.fixed_length_arcs_buffer.reset_position();
        self.fixed_length_arcs_buffer.reset_position()?;
        self.fixed_length_arcs_buffer
            .write_byte(ARCS_FOR_BINARY_SEARCH)?;
        let node_in = self.frontier[node_in_idx].as_ref().unwrap();
        self.fixed_length_arcs_buffer.write_vint(node_in.num_arcs)?;
        self.fixed_length_arcs_buffer
            .write_vint(max_bytes_per_arc)?;

        let header_len = self.fixed_length_arcs_buffer.get_position();
        // Expand the arcs in place, backwards.
        let src_pos = self.scratch_bytes.get_position();
        let dest_pos = header_len + node_in.num_arcs * max_bytes_per_arc;

        debug_assert!(dest_pos >= src_pos);

        if dest_pos > src_pos {
            self.scratch_bytes.set_position(dest_pos);
            let scratch_bytes = self.scratch_bytes.get_bytes();
            let arc_bytes = &self.num_bytes_per_arc;
            let mut src_pos = src_pos as usize;
            let mut dest_pos = dest_pos as usize;
            let max_bytes_per_arc = max_bytes_per_arc as usize;
            for arc_idx in (0..node_in.num_arcs as usize).rev() {
                dest_pos -= max_bytes_per_arc;
                let arc_len = arc_bytes[arc_idx] as usize;
                src_pos -= arc_len;
                if src_pos != dest_pos {
                    debug_assert!(
                        dest_pos >= src_pos,
                        "dest_pos={} < src_pos={} at arc_idx={}, max_bytes_per_arc={}, arc_len={}, num_arcs={}",
                        dest_pos,
                        src_pos,
                        arc_idx,
                        max_bytes_per_arc,
                        arc_len,
                        node_in.num_arcs
                    );
                    scratch_bytes.copy_within(src_pos..src_pos + arc_len, dest_pos);
                }
            }
        }

        // Write header at the beginning
        self.scratch_bytes.get_bytes().copy_from(
            &self.fixed_length_arcs_buffer.get_bytes()[..header_len as usize],
            0,
        );

        Ok(())
    }
    /// Reverse the scratch bytes in place. This operation does not affect
    /// `scratch_bytes.get_position()`.
    fn reverse_scratch_bytes(&mut self) {
        let pos = self.scratch_bytes.get_position() as usize;
        let bytes = self.scratch_bytes.get_bytes();
        let limit = pos / 2;
        for i in 0..limit {
            let j = pos - 1 - i;
            bytes.swap(i, j);
        }
    }
    /// Write bytes from a source `byte[]` to the scratch bytes. The written
    /// bytes must fit within what was already written in the scratch bytes.
    ///
    ///
    /// This operation does not affect `scratch_bytes.get_position()`.
    ///
    ///
    /// # Arguments
    ///
    /// * `dest_pos` - the position in the scratch bytes
    /// * `bytes` - the source byte array
    /// * `offset` - the offset inside the source byte array
    /// * `length` - the number of bytes to write
    #[allow(dead_code)]
    fn write_scratch_bytes(&mut self, _dest_pos: i32, _bytes: &[u8], _offset: i32, _length: i32) {
        // Not Implement in Rust Lucene
    }
    fn write_node_for_direct_addressing_or_continuous(
        &mut self,
        node_in_idx: usize,
        max_bytes_per_arc_without_label: i32,
        label_range: i32,
        continuous: bool,
    ) -> Result<()> {
        // Expand the arcs backwards in a buffer because we remove the labels.
        // So the obtained arcs might occupy less space. This is the reason why
        // this whole method is more complex.
        // Drop the label bytes since we can infer the label based on the arc
        // index, the presence bits, and the first label. Keep the first
        // label.
        let header_max_len = 11;
        let num_presence_bytes = if continuous {
            0
        } else {
            get_num_presence_bytes(label_range)
        };

        let mut src_pos = self.scratch_bytes.get_position();
        let node_in = self.frontier[node_in_idx].as_ref().unwrap();
        let total_arc_bytes =
            self.num_label_bytes_per_arc[0] + node_in.num_arcs * max_bytes_per_arc_without_label;

        let mut buffer_offset = header_max_len + num_presence_bytes + total_arc_bytes;
        self.fixed_length_arcs_buffer
            .ensure_capacity(buffer_offset)?;
        let buffer = self.fixed_length_arcs_buffer.get_bytes();
        // Copy the arcs to the buffer, dropping all labels except first one.
        for arc_idx in (0..node_in.num_arcs as usize).rev() {
            buffer_offset -= max_bytes_per_arc_without_label;
            let src_arc_len = self.num_bytes_per_arc[arc_idx];
            src_pos -= src_arc_len;
            let label_len = self.num_label_bytes_per_arc[arc_idx];
            self.scratch_bytes
                .write_to(src_pos, buffer, buffer_offset, 1);
            // Skip the label, copy the remaining.
            let remaining_len = src_arc_len - 1 - label_len;
            if remaining_len != 0 {
                self.scratch_bytes.write_to(
                    src_pos + 1 + label_len,
                    buffer,
                    buffer_offset + 1,
                    remaining_len,
                );
            }

            // Copy label for first arc only
            if arc_idx == 0 {
                buffer_offset -= label_len;
                self.scratch_bytes
                    .write_to(src_pos + 1, buffer, buffer_offset, label_len);
            }
        }

        debug_assert_eq!(
            buffer_offset,
            header_max_len + num_presence_bytes,
            "buffer_offset mismatch"
        );
        // Build the header in the buffer.
        // It is a false/special arc which is in fact a node header with node
        // flags followed by node metadata.
        // Write header
        self.fixed_length_arcs_buffer.reset_position()?;
        self.fixed_length_arcs_buffer.write_byte(if continuous {
            ARCS_FOR_CONTINUOUS
        } else {
            ARCS_FOR_DIRECT_ADDRESSING
        })?;
        self.fixed_length_arcs_buffer.write_vint(label_range)?; // labelRange instead of numArcs.
        self.fixed_length_arcs_buffer
            .write_vint(max_bytes_per_arc_without_label)?; // maxBytesPerArcWithoutLabel instead of maxBytesPerArc.
        let header_len = self.fixed_length_arcs_buffer.get_position();

        self.scratch_bytes.set_position(0);
        self.scratch_bytes.write_bytes_range(
            self.fixed_length_arcs_buffer.get_bytes(),
            0,
            header_len,
        )?;

        // Write presence bits if not continuous
        if !continuous {
            self.write_presence_bits(node_in_idx)?;
            debug_assert_eq!(
                self.scratch_bytes.get_position(),
                header_len + num_presence_bytes
            );
        }

        // Write first label + arcs
        self.scratch_bytes.write_bytes_range(
            self.fixed_length_arcs_buffer.get_bytes(),
            buffer_offset,
            total_arc_bytes,
        )?;

        debug_assert_eq!(
            self.scratch_bytes.get_position(),
            header_len + num_presence_bytes + total_arc_bytes
        );
        Ok(())
    }

    fn write_presence_bits(&mut self, node_in_idx: usize) -> Result<()> {
        let mut presence_bits: u8 = 1; // The first arc is always present.
        let mut presence_index = 0;
        let node_in = self.frontier[node_in_idx].as_ref().unwrap();
        let mut previous_label = node_in.arcs[0].label;

        let byte_size = i8::BITS as i32;
        for arc_idx in 1..node_in.num_arcs as usize {
            let label = node_in.arcs[arc_idx].label;
            debug_assert!(label > previous_label);
            presence_index += label - previous_label;
            while presence_index >= byte_size {
                self.scratch_bytes.write_byte(presence_bits)?;
                presence_bits = 0;
                presence_index -= byte_size;
            }
            // Set the bit at presenceIndex to flag that the corresponding arc
            // is present.
            presence_bits |= 1 << presence_index;
            previous_label = label;
        }

        debug_assert!({
            let last_label = node_in.arcs[node_in.num_arcs as usize - 1].label;
            let first_label = node_in.arcs[0].label;
            presence_index == (last_label - first_label) % 8
        });
        debug_assert!(presence_bits != 0); // The last byte is not 0.
        debug_assert!((presence_bits & (1 << presence_index)) != 0); // The last arc is always present.

        self.scratch_bytes.write_byte(presence_bits)?;
        Ok(())
    }
    pub(crate) fn set_empty_output(&mut self, v: O::V) -> Result<()> {
        match self.fst.metadata {
            Some(ref mut metadata) => {
                if let Some(existing) = &mut metadata.empty_output {
                    metadata.empty_output = Some(self.fst.outputs.merge(&existing.clone(), &v)?);
                } else {
                    metadata.empty_output = Some(v);
                }
            },
            None => {
                return Err(LuceneError::illegal_state("FSTCompiler's metadata is None"));
            },
        }
        Ok(())
    }
    /// Finish Data Writing
    ///
    /// # Warning
    ///
    /// If Data written to file (See:DataOutputEnum::FromDir) ,The actual data
    /// is not flushed to the underlying file until the [`FSTCompiler`] is
    /// dropped. If you need to ensure the data is written before the
    /// compiler is dropped, consider manually finalizing or flushing the
    /// data, or ensure the compiler goes out of scope.
    ///
    /// This method does not guarantee that the data has been persisted on disk.
    pub(crate) fn finish(&mut self, mut new_start_node: i64) -> Result<()> {
        debug_assert!(new_start_node <= self.num_bytes_written);

        match self.fst.metadata {
            Some(ref mut metadata) => {
                if metadata.start_node != -1 {
                    return Err(LuceneError::illegal_state("already finished"));
                }

                if new_start_node == FINAL_END_NODE && metadata.empty_output.is_some() {
                    new_start_node = 0;
                }

                metadata.start_node = new_start_node;
                metadata.num_bytes = self.num_bytes_written;

                // Freeze the data output if it's a read-write buffer
                match self.data_output {
                    DataOutputEnum::FromDir(_) => {
                        // file flush by when Drop FSTCompiler
                    },
                    DataOutputEnum::ReadWriter(ref mut rw) => {
                        rw.freeze()?;
                    },
                }
            },
            None => {
                return Err(LuceneError::illegal_state("FSTCompiler's metadata is None"));
            },
        }

        Ok(())
    }
    fn valid_output(&self, output: &O::V) -> bool {
        self.no_output.is_same_reference(output) || *output != self.no_output
    }
    /// Returns the estimated heap memory used by the in-construction FST.
    pub fn fst_ram_bytes_used(&self) -> Result<i64> {
        let mut ram_bytes_used = self.scratch_bytes.ram_bytes_used()?;
        ram_bytes_used += self.data_output.ram_bytes_used()?;
        Ok(ram_bytes_used)
    }

    /// Returns the current byte size of the FST being built.
    pub fn fst_size_in_bytes(&self) -> i64 {
        self.num_bytes_written
    }
}

/// This struct is used for FST backed by non-FSTReader DataOutput. It does not
/// allow getting the reverse BytesReader nor writing to a DataOutput.
pub(crate) struct NullFSTReader;
impl Accountable for NullFSTReader {
    fn ram_bytes_used(&self) -> Result<i64> {
        todo!()
    }
}
impl FstReader for NullFSTReader {
    type FstBytesReader = DummyBytesReader;

    fn get_reverse_bytes_reader(&self) -> Result<Self::FstBytesReader> {
        Err(LuceneError::unsupported_operation(
            "FST was not constructed with getOnHeapReaderWriter()",
        ))
    }

    fn write_to(&self, _out: &mut impl DataOutput) -> Result<()> {
        Err(LuceneError::unsupported_operation(
            "FST was not constructed with getOnHeapReaderWriter()",
        ))
    }
}
/// Fluent-style builder for constructing an [`FSTCompiler`].
///
/// Creates an FST/FSA builder with all possible tuning and construction tweaks.
/// Read parameter documentation carefully.
pub struct Builder<O, D>
where
    O: Outputs,
    D: Directory,
{
    input_type: InputType,
    outputs: O,
    suffix_ram_limit_mb: f64,
    allow_fixed_length_arcs: bool,
    data_output: Option<DataOutputEnum<D>>,
    direct_addressing_max_oversizing_factor: f32,
    version: i32,
}
impl<O, D> Builder<O, D>
where
    O: Outputs,
    D: Directory,
{
    /// Creates a new [`Builder`] with the given input type and outputs.
    ///
    /// - `input_type`: The input type (transition labels). Can be any variant
    ///   of [`InputType`]. Shorter types consume less memory. Strings
    ///   (character sequences) are typically represented using
    ///   [`InputType::Byte4`] for full Unicode codepoints.
    ///
    /// - `outputs`: The output type for each input sequence. Applies only when
    ///   building an FST. For FSA, use
    ///   [`NoOutputs::singleton()`](crate::core::util::fst_impl::no_outputs::NoOutputs::get_singleton)
    ///   and [`NoOutputs::no_output()`](crate::core::util::fst_impl::no_outputs::NoOutputs::get_no_output)
    ///   as the singleton output.
    pub fn new(input_type: InputType, outputs: O) -> Self {
        Self {
            input_type,
            outputs,
            suffix_ram_limit_mb: 32.0,
            allow_fixed_length_arcs: true,
            data_output: None,
            direct_addressing_max_oversizing_factor: DIRECT_ADDRESSING_MAX_OVERSIZING_FACTOR,
            version: VERSION_CURRENT,
        }
    }
    /// Sets the approximate maximum amount of RAM (in MB) to use for holding
    /// the suffix cache.
    ///
    /// This cache enables the FST to share common suffixes. Passing
    /// `f64::INFINITY` keeps all suffixes, resulting in an exactly minimal
    /// FST. The actual memory usage in that case will be bounded by the
    /// number of unique suffixes.
    ///
    /// If a smaller value is passed, the least recently used suffixes are
    /// discarded, reducing suffix sharing and producing a non-minimal FST.
    /// The larger the limit, the closer the result will be to the minimal
    /// FST, with diminishing returns.
    ///
    /// Pass `0.0` to disable suffix sharing entirely (may result in a
    /// substantially larger FST).
    ///
    /// Note: this is an approximate limit. The implementation uses hash tables
    /// to map suffixes and estimates overhead from unused slots.
    ///
    /// Default: `32.0`
    pub fn suffix_ram_limit_mb(&mut self, mb: f64) -> Result<()> {
        if mb < 0f64 {
            return Err(LuceneError::illegal_argument(format!(
                "suffix_ram_limit_mb must be >= 0; got: {mb}"
            )));
        }
        self.suffix_ram_limit_mb = mb;
        Ok(())
    }

    /// Controls whether fixed-length arc optimization (binary search or direct
    /// addressing) is enabled.
    ///
    /// Disabling this makes the resulting FST smaller but slower to traverse.
    ///
    /// Default: `true`
    pub fn allow_fixed_length_arcs(&mut self, allow: bool) {
        self.allow_fixed_length_arcs = allow;
    }

    /// Set the [`DataOutput`] which is used for low-level writing of FST. If
    /// you want the FST to be immediately readable, you need to use
    /// [`get_on_heap_reader_writer`].
    ///
    /// Otherwise you need to construct the corresponding
    /// [`DataInput`](crate::core::store::data_input::DataInput) and use the FST
    /// constructor to read it.
    ///
    /// # Arguments
    ///
    /// * `data_output` - the `DataOutput`
    ///
    /// # Returns
    ///
    /// This builder.
    ///
    /// # See also
    ///
    /// [`get_on_heap_reader_writer`]
    pub fn data_output(&mut self, data_output: DataOutputEnum<D>) {
        self.data_output = Some(data_output);
    }
    /// Overrides the default maximum oversizing of fixed array allowed to
    /// enable direct addressing of arcs instead of binary search.
    ///
    /// Setting this factor to a negative value (e.g. `-1`) effectively disables
    /// direct addressing, only binary search nodes will be created.
    ///
    /// This factor does not determine whether to encode a node with a list of
    /// variable length arcs or with fixed length arcs. It only determines
    /// the effective encoding of a node that is already known to be encoded
    /// with fixed length arcs.
    ///
    ///
    /// Default = `1`.
    pub fn with_direct_addressing_max_oversizing_factor(&mut self, factor: f32) {
        self.direct_addressing_max_oversizing_factor = factor;
    }
    ///  Expert: Set the codec version.
    pub fn with_version(&mut self, version: i32) -> Result<()> {
        if (VERSION_90..=VERSION_CURRENT).contains(&version) {
            return Err(LuceneError::illegal_argument(format!(
                "Version must be in range [{} - {}]; got: {}",
                VERSION_90, VERSION_CURRENT, version
            )));
        }
        self.version = version;
        Ok(())
    }
    /// Creates a new {@link FSTCompiler}
    pub fn build(mut self) -> Result<FSTCompiler<O, D>> {
        if self.data_output.is_none() {
            self.data_output = Some(DataOutputEnum::ReadWriter(get_on_heap_reader_writer(15)?));
        }
        FSTCompiler::new(
            self.input_type,
            self.suffix_ram_limit_mb,
            self.outputs,
            self.allow_fixed_length_arcs,
            self.data_output.take().unwrap(),
            self.direct_addressing_max_oversizing_factor,
            self.version,
        )
    }
}
pub enum DataOutputEnum<D>
where
    D: Directory,
{
    FromDir(D::IndexOutput),
    ReadWriter(ReadWriteDataOutput),
}
impl<D> Display for DataOutputEnum<D>
where
    D: Directory,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            DataOutputEnum::FromDir(data_output) => {
                write!(f, "{data_output}")
            },
            DataOutputEnum::ReadWriter(_) => {
                write!(f, "ReadWriteDataOutput")
            },
        }
    }
}

impl<D> Accountable for DataOutputEnum<D>
where
    D: Directory,
{
    fn ram_bytes_used(&self) -> Result<i64> {
        todo!()
    }
}

impl<D> FstReader for DataOutputEnum<D>
where
    D: Directory,
{
    type FstBytesReader = Either2BytesReader<BytesReaderImpl, ReverseBytesReader>;

    fn get_reverse_bytes_reader(&self) -> Result<Self::FstBytesReader> {
        match self {
            DataOutputEnum::FromDir(_) => Err(LuceneError::unsupported_operation("")),
            DataOutputEnum::ReadWriter(rw) => {
                let reader = rw.get_reverse_bytes_reader()?;
                Ok(reader)
            },
        }
    }

    fn write_to(&self, out: &mut impl DataOutput) -> Result<()> {
        match self {
            DataOutputEnum::FromDir(_) => Err(LuceneError::unsupported_operation("")),
            DataOutputEnum::ReadWriter(rw) => rw.write_to(out),
        }
    }

    fn init_reader(&mut self) {
        match self {
            DataOutputEnum::FromDir(_) => {},
            DataOutputEnum::ReadWriter(rw) => rw.init_reader(),
        }
    }
}
impl<D> DataOutput for DataOutputEnum<D>
where
    D: Directory,
{
    fn write_byte(&mut self, b: u8) -> Result<()> {
        match self {
            DataOutputEnum::FromDir(data_output) => data_output.write_byte(b),
            DataOutputEnum::ReadWriter(rw) => rw.write_byte(b),
        }
    }

    fn write_bytes_range(&mut self, b: &[u8], offset: i32, length: i32) -> Result<()> {
        match self {
            DataOutputEnum::FromDir(data_output) => {
                data_output.write_bytes_range(b, offset, length)
            },
            DataOutputEnum::ReadWriter(rw) => rw.write_bytes_range(b, offset, length),
        }
    }
}
/// Expert: holds a pending (seen but not yet serialized) arc.
pub(crate) struct Arc<T>
where
    T: OutputsBound,
{
    pub label: i32, // really an "unsigned" byte
    pub target: NodeEnum,
    pub is_final: bool,
    pub output: T,
    pub next_final_output: T,
}
impl<T> Default for Arc<T>
where
    T: OutputsBound,
{
    fn default() -> Self {
        Self {
            label: 0,
            target: NodeEnum::CompiledNode(CompiledNode::default()),
            is_final: false,
            output: T::default(),
            next_final_output: T::default(),
        }
    }
}

/// # NOTE:
/// Not many instances of Node or CompiledNode are in
/// memory while the FST is being built; it's only the
/// current "frontier":
pub(crate) trait Node {
    fn is_compiled(&self) -> bool;
}
pub(crate) enum NodeEnum {
    // Since UnCompiledNode in Java is a reference within the frontier, we record the index of the
    // frontier instead to satisfy Rust's ownership rules.
    UnCompiledNode(usize),
    CompiledNode(CompiledNode),
}
impl Node for NodeEnum {
    fn is_compiled(&self) -> bool {
        match self {
            NodeEnum::UnCompiledNode(_) => false,
            NodeEnum::CompiledNode(node) => node.is_compiled(),
        }
    }
}
#[derive(Default)]
pub(crate) struct CompiledNode {
    pub(crate) node: i64,
}
impl Node for CompiledNode {
    fn is_compiled(&self) -> bool {
        true
    }
}
/// Expert: holds a pending (seen but not yet serialized) Node.
pub(crate) struct UnCompiledNode<T>
where
    T: OutputsBound,
{
    pub(crate) num_arcs: i32,
    pub(crate) arcs: Vec<Arc<T>>,
    // TODO: instead of recording is_final/output on the node,
    // maybe we should use -1 arc to mean "end" (like we do when reading the
    // FST). Would simplify much code here...
    pub(crate) output: T,
    pub(crate) is_final: bool,

    /// This node's depth, starting from the automaton root.
    pub depth: i32,
}
impl<T> UnCompiledNode<T>
where
    T: OutputsBound,
{
    /// Creates a new uncompiled node.
    ///
    /// # Parameters
    /// - `depth`: The node's depth starting from the automaton root. Needed for
    ///   LUCENE-2934 (node expansion based on conditions other than the fanout
    ///   size).
    pub(crate) fn new(no_output: T, depth: i32) -> Self {
        let arcs = vec![Arc::default()];

        Self {
            num_arcs: 0,
            arcs,
            output: no_output,
            is_final: false,
            depth,
        }
    }

    pub(crate) fn is_compiled(&self) -> bool {
        false
    }

    pub(crate) fn clear(&mut self, no_outputs: T) {
        self.num_arcs = 0;
        self.is_final = false;
        self.output = no_outputs;
        // We don't clear the depth here because it never changes
        // for nodes on the frontier (even when reused).
    }

    pub(crate) fn get_last_output(&self, label_to_match: i32) -> T {
        debug_assert!(self.num_arcs > 0);
        debug_assert!(self.arcs[self.num_arcs as usize - 1].label == label_to_match);
        self.arcs[self.num_arcs as usize - 1].output.clone()
    }

    pub(crate) fn add_arc(&mut self, label: i32, target: NodeEnum, no_outputs: T) -> Result<()> {
        debug_assert!(label >= 0);
        debug_assert!(
            self.num_arcs == 0 || label > self.arcs[self.num_arcs as usize - 1].label,
            "arc[numArcs-1].label={} new label={} numArcs={}",
            self.arcs[self.num_arcs as usize - 1].label,
            label,
            self.num_arcs
        );

        if self.num_arcs as usize == self.arcs.len() {
            ArrayUtil::grow(&mut self.arcs)?;
        }

        let arc = &mut self.arcs[self.num_arcs as usize];
        self.num_arcs += 1;
        arc.label = label;
        arc.target = target;
        arc.output = no_outputs;
        arc.next_final_output = arc.output.clone();
        arc.is_final = false;
        Ok(())
    }
    pub(crate) fn replace_last(
        &mut self,
        label_to_match: i32,
        target: NodeEnum,
        next_final_output: T,
        is_final: bool,
    ) {
        debug_assert!(self.num_arcs > 0);
        let arc = &mut self.arcs[self.num_arcs as usize - 1];
        debug_assert_eq!(
            arc.label, label_to_match,
            "arc.label={} vs {}",
            arc.label, label_to_match
        );
        arc.target = target;
        arc.next_final_output = next_final_output;
        arc.is_final = is_final;
    }

    pub(crate) fn set_last_output<O: Outputs, D: Directory>(
        label_to_match: i32,
        new_output: O::V,
        compiler: &mut FSTCompiler<O, D>,
        node_idx: usize,
    ) {
        debug_assert!(compiler.valid_output(&new_output));
        let un_compile_node = compiler.frontier[node_idx].as_mut().unwrap();
        debug_assert!(un_compile_node.num_arcs > 0);
        let arc = &mut un_compile_node.arcs[un_compile_node.num_arcs as usize - 1];
        debug_assert_eq!(arc.label, label_to_match);
        arc.output = new_output;
    }

    /// Pushes an output prefix forward onto all arcs.
    pub(crate) fn prepend_output<O: Outputs, D: Directory>(
        output_prefix: &O::V,
        compiler: &mut FSTCompiler<O, D>,
        node_index: usize,
    ) {
        debug_assert!(compiler.valid_output(output_prefix));
        let un_compiled_node = compiler.frontier[node_index].as_mut().unwrap();
        for i in 0..un_compiled_node.num_arcs as usize {
            let new_output = compiler
                .fst
                .outputs
                .add(output_prefix, &un_compiled_node.arcs[i].output);
            un_compiled_node.arcs[i].output = new_output;
            // TODO:
            // debug_assert!(compiler.valid_output(&new_output));
        }

        if un_compiled_node.is_final {
            let new_output = compiler
                .fst
                .outputs
                .add(output_prefix, &un_compiled_node.output);
            un_compiled_node.output = new_output;
            // TODO:
            // debug_assert!(compiler.valid_output(&new_output));
        }
    }
}
impl<T> Node for UnCompiledNode<T>
where
    T: OutputsBound,
{
    fn is_compiled(&self) -> bool {
        false
    }
}

/// Reusable buffer for building nodes with fixed length arcs (binary search or
/// direct addressing).
pub(crate) struct FixedLengthArcsBuffer {
    bado: ByteArrayDataOutput<Vec<u8>>,
}
impl FixedLengthArcsBuffer {
    pub(crate) fn new() -> Self {
        // Initial capacity is the max length required for the header of a node
        // with fixed length arcs: header(byte) + numArcs(vint) +
        // numBytes(vint)
        let bytes = vec![0u8; 11];
        let bado = ByteArrayDataOutput::with_bytes(bytes);
        Self { bado }
    }
    /// Ensures the capacity of the internal byte array. Enlarges it if needed.
    pub(crate) fn ensure_capacity(&mut self, capacity: i32) -> Result<()> {
        if self.bado.bytes.len() < capacity as usize {
            ArrayUtil::grow_with_len(
                &mut self.bado.bytes,
                ArrayUtil::oversize(capacity as usize, 1),
            );
            self.bado.reset()?;
        }
        Ok(())
    }

    pub(crate) fn reset_position(&mut self) -> Result<()> {
        self.bado.reset()
    }

    pub(crate) fn write_byte(&mut self, b: u8) -> Result<()> {
        self.bado.write_byte(b)
    }

    pub(crate) fn write_vint(&mut self, i: i32) -> Result<()> {
        self.bado.write_vint(i)
    }

    pub(crate) fn get_position(&self) -> i32 {
        debug_assert!(self.bado.bytes.len() <= i32::MAX as usize);
        self.bado.get_position() as i32
    }

    /// Gets the internal byte array.
    pub(crate) fn get_bytes(&mut self) -> &mut [u8] {
        &mut self.bado.bytes
    }
}

/// Maximum oversizing factor allowed for direct addressing.
pub(crate) const DIRECT_ADDRESSING_MAX_OVERSIZING_FACTOR: f32 = 1.0;

/// Minimum depth at which fixed-length arcs are considered for shallow
/// nodes.
///
/// See [`FSTCompiler::should_expand_node_with_fixed_length_arcs`](FSTCompiler::should_expand_node_with_fixed_length_arcs)..
pub(crate) const FIXED_LENGTH_ARC_SHALLOW_DEPTH: i32 = 3;

/// Minimum number of arcs required to consider fixed-length arcs at shallow
/// depth.
///
/// See [`FSTCompiler::should_expand_node_with_fixed_length_arcs`](FSTCompiler::should_expand_node_with_fixed_length_arcs)..
pub(crate) const FIXED_LENGTH_ARC_SHALLOW_NUM_ARCS: i32 = 5;

/// Minimum number of arcs required to consider fixed-length arcs at deep
/// depth.
///
/// See [`FSTCompiler::should_expand_node_with_fixed_length_arcs`](FSTCompiler::should_expand_node_with_fixed_length_arcs).
pub(crate) const FIXED_LENGTH_ARC_DEEP_NUM_ARCS: i32 = 10;

/// Maximum oversizing factor allowed for direct addressing compared to
/// binary search when expansion credits allow the oversizing. This
/// factor prevents expansions that are obviously too costly even if
/// there are sufficient credits.
///
/// See [`FSTCompiler::should_expand_node_with_direct_addressing`](FSTCompiler::should_expand_node_with_direct_addressing).
const DIRECT_ADDRESSING_MAX_OVERSIZE_WITH_CREDIT_FACTOR: f32 = 1.66;
pub fn get_on_heap_reader_writer(block_bits: i32) -> Result<ReadWriteDataOutput> {
    ReadWriteDataOutput::new(block_bits)
}
