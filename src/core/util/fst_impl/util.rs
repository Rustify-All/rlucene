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
use std::cmp::Ordering;
use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::fmt::Display;
use std::fmt::Write as _;
use std::io::Write;

use crate::core::index::{BytesRef, BytesRefBuilder};
use crate::core::store::DataInput;
use crate::core::util::access::{SharedAccessVec, WritableVec};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fst_impl::fst::{
  ARCS_FOR_BINARY_SEARCH, ARCS_FOR_CONTINUOUS, ARCS_FOR_DIRECT_ADDRESSING, Arc, BIT_TARGET_NEXT,
  BitTable, BytesReader, END_LABEL, FST, InputType, read_end_arc, target_has_arcs,
};
use crate::core::util::fst_impl::fst_reader::FstReader;
use crate::core::util::fst_impl::outputs::{Outputs, OutputsBound};
use crate::core::util::ints_ref::IntsRef;
use crate::core::util::ints_ref_builder::IntsRefBuilder;
use crate::core::util::{Comparator, ToInt};

/// Static helper methods.
///
/// # Lucene experimental
pub struct Util;
impl Util {
  /// Looks up the output for this input, or None if the input is not
  /// accepted.
  pub fn get_from_ints<O, F, AV>(
    fst: &FST<O, F>,
    input: &IntsRef<AV>,
  ) -> Result<Option<Option<O::V>>>
  where
    O: Outputs,
    F: FstReader,
    AV: SharedAccessVec<i32>,
  {
    let mut arc = fst.get_first_arc_owned();
    let mut fst_reader = fst.get_bytes_reader()?;
    let mut output = None;

    for i in 0..input.length {
      let label = input.ints.access(|ints| ints[input.offset + i]);
      let found = fst.find_target_arc_impl(label, None, &mut arc, &mut fst_reader)?;
      if found.is_none() {
        return Ok(None);
      }
      fst.outputs.add_assign(&mut output, &mut arc.output);
    }

    if arc.is_final() {
      fst
        .outputs
        .add_assign(&mut output, &mut arc.next_final_output);
      Ok(Some(output))
    } else {
      Ok(None)
    }
  }
  /// Looks up the output for this input, or `None` if the input is not
  /// accepted.
  pub fn get_from_bytes<O, F, AV>(
    fst: &FST<O, F>,
    input: &BytesRef<AV>,
  ) -> Result<Option<Option<O::V>>>
  where
    O: Outputs,
    F: FstReader,
    AV: SharedAccessVec<u8>,
  {
    debug_assert_eq!(fst.metadata.input_type, InputType::Byte1);

    let mut fst_reader = fst.get_bytes_reader()?;
    let mut arc = fst.get_first_arc_owned();
    let mut output = None;

    for i in 0..input.length {
      let label = input.bytes.access(|bytes| bytes[input.offset + i] as i32);
      let found = fst.find_target_arc_impl(label, None, &mut arc, &mut fst_reader)?;
      if found.is_none() {
        return Ok(None);
      }
      fst.outputs.add_assign(&mut output, &mut arc.output);
    }

    if arc.is_final() {
      fst
        .outputs
        .add_assign(&mut output, &mut arc.next_final_output);
      Ok(Some(output))
    } else {
      Ok(None)
    }
  }

  /// Starting from node, find the top N min cost completions to a final node.
  pub fn shortest_paths<O, F, C>(
    fst: &FST<O, F>,
    from_node: &Arc<O::V>,
    start_output: Option<O::V>,
    comparator: C,
    top_n: usize,
    allow_empty_string: bool,
  ) -> Result<TopResults<O::V>>
  where
    O: Outputs,
    F: FstReader,
    C: Comparator<Option<O::V>> + Clone,
  {
    let mut searcher = TopNSearcher::new(fst, top_n, top_n, comparator)?;
    searcher.add_start_paths(
      from_node,
      start_output,
      allow_empty_string,
      IntsRefBuilder::new(),
    )?;
    searcher.search()
  }

  /// Dumps an FST to a GraphViz's dot language description for visualization.
  ///
  /// Example of use:
  ///
  /// ```text
  /// let mut writer = File::create("out.dot")?;
  /// Util::to_dot(fst, &mut writer, true, true)?;
  /// ```
  ///
  /// and then, from command line:
  ///
  /// ```text
  /// dot -Tpng -o out.png out.dot
  /// ```
  ///
  /// Note: larger FSTs (a few thousand nodes) won't even render, don't bother.
  ///
  /// * `same_rank` - If `true`, the resulting dot file will try to order
  ///   states in layers of breadth-first traversal. This may mess up arcs, but
  ///   makes the output FST's structure a bit clearer.
  /// * `label_states` - If `true`, states will have labels equal to their
  ///   offsets in their binary format. Expands the graph considerably.
  pub fn to_dot<O, F, T>(
    fst: &FST<O, F>,
    out: &mut T,
    same_rank: bool,
    label_states: bool,
  ) -> Result<()>
  where
    O: Outputs,
    F: FstReader,
    T: Write,
  {
    let expanded_node_color = "blue";

    let mut start_arc = Arc::default();
    fst.get_first_arc(&mut start_arc);

    let mut this_level_queue = smallvec::SmallVec::<[i64; 4]>::new();
    let mut next_level_queue = smallvec::SmallVec::<[i64; 4]>::new();
    next_level_queue.push(start_arc.target());

    let mut same_level_states = smallvec::SmallVec::<[i64; 4]>::new();
    let mut seen = HashSet::new();
    seen.insert(start_arc.target());

    let state_shape = "circle";
    let final_state_shape = "doublecircle";

    out.write_all(b"digraph FST {\n")?;
    out.write_all(
      b"  rankdir = LR; splines=true; concentrate=true; ordering=out; ranksep=2.5; \n",
    )?;

    if !label_states {
      out.write_all(b"  node [shape=circle, width=.2, height=.2, style=filled]\n")?;
    }

    Self::emit_dot_state(out, "initial", Some("point"), Some("white"), Some(""))?;

    let mut r = fst.get_bytes_reader()?;

    {
      let state_color = if fst.is_expanded_target(&start_arc, &mut r)? {
        Some(expanded_node_color)
      } else {
        None
      };

      let (is_final, final_output) = if start_arc.is_final() {
        let next_final_output = &start_arc.next_final_output;
        if next_final_output.is_none() {
          (true, String::new())
        } else {
          (true, fst.outputs.output_to_string(next_final_output))
        }
      } else {
        (false, String::new())
      };

      Self::emit_dot_state(
        out,
        &start_arc.target().to_string(),
        Some(if is_final {
          final_state_shape
        } else {
          state_shape
        }),
        state_color,
        Some(&final_output),
      )?;
    }

    out.write_all(format!("  initial -> {}\n", start_arc.target()).as_bytes())?;

    let mut arc = start_arc;
    let mut level = 0;

    while !next_level_queue.is_empty() {
      this_level_queue.append(&mut next_level_queue);

      level += 1;
      out.write_all(format!("\n  // Transitions and states at level: {level}\n").as_bytes())?;
      while let Some(node) = this_level_queue.pop() {
        if node > 0 {
          fst.read_first_real_target_arc(node, &mut arc, &mut r)?;

          loop {
            if arc.target() >= 0 && !seen.contains(&arc.target()) {
              let state_color = if fst.is_expanded_target(&arc, &mut r)? {
                Some(expanded_node_color)
              } else {
                None
              };

              let next_final_output = &arc.next_final_output;
              let final_output = if next_final_output.is_some() {
                fst.outputs.output_to_string(next_final_output)
              } else {
                String::new()
              };

              Self::emit_dot_state(
                out,
                &arc.target().to_string(),
                Some(state_shape),
                state_color,
                Some(&final_output),
              )?;
              seen.insert(arc.target());
              next_level_queue.push(arc.target());
              same_level_states.push(arc.target());
            }

            let mut outs = if arc.output.is_some() {
              format!("/{}", fst.outputs.output_to_string(&arc.output))
            } else {
              String::new()
            };

            if !target_has_arcs(&arc) && arc.is_final() && arc.next_final_output.is_some() {
              write!(
                outs,
                "/[{}]",
                fst.outputs.output_to_string(&arc.next_final_output)
              )?;
            }

            let arc_color = if arc.flag(BIT_TARGET_NEXT) {
              "red"
            } else {
              "black"
            };

            debug_assert_ne!(arc.label(), END_LABEL);
            out.write_all(
              format!(
                "  {} -> {} [label=\"{}{}\"{} color=\"{}\"]\n",
                node,
                arc.target(),
                Self::printable_label(arc.label()),
                outs,
                if arc.is_final() {
                  " style=\"bold\""
                } else {
                  ""
                },
                arc_color
              )
              .as_bytes(),
            )?;

            if arc.is_last() {
              break;
            }
            fst.read_next_real_arc(&mut arc, &mut r)?;
          }
        }
      }

      if same_rank && same_level_states.len() > 1 {
        out.write_all(b"  {rank=same; ")?;
        for state in &same_level_states {
          out.write_all(format!("{state}; ").as_bytes())?;
        }
        out.write_all(b" }\n")?;
      }
      same_level_states.clear();
    }

    out.write_all(b"  -1 [style=filled, color=black, shape=doublecircle, label=\"\"]\n\n")?;
    out.write_all(b"  {rank=sink; -1 }\n")?;

    out.write_all(b"}\n")?;
    out.flush()?;
    Ok(())
  }

  /// Emit a single state in the dot language.
  fn emit_dot_state<T>(
    out: &mut T,
    name: &str,
    shape: Option<&str>,
    color: Option<&str>,
    label: Option<&str>,
  ) -> Result<()>
  where
    T: Write,
  {
    let mut line = String::with_capacity(
      name.len()
        + shape.map_or(0, str::len)
        + color.map_or(0, str::len)
        + label.map_or(0, str::len)
        + 32,
    );
    line.push_str("  ");
    line.push_str(name);
    line.push_str(" [");
    if let Some(shape) = shape {
      line.push_str("shape=");
      line.push_str(shape);
    }
    line.push(' ');
    if let Some(color) = color {
      line.push_str("color=");
      line.push_str(color);
    }
    line.push_str(" label=\"");
    if let Some(label) = label {
      line.push_str(label);
    }
    line.push_str("\" ]\n");
    out.write_all(line.as_bytes())?;
    Ok(())
  }

  /// Ensures an arc's label is indeed printable (dot uses US-ASCII).
  fn printable_label(label: i32) -> String {
    if (0x20..=0x7d).contains(&label) && label != 0x22 && label != 0x5c {
      (label as u8 as char).to_string()
    } else {
      format!("0x{label:x}")
    }
  }

  /// Decodes the Unicode codepoints from the provided string and places them
  /// in the provided scratch [`IntsRef`], which must not be `None`, returning it.
  pub fn to_utf32<AV>(s: &str, scratch: &mut IntsRefBuilder<AV>) -> Result<()>
  where
    AV: SharedAccessVec<i32> + WritableVec<i32>,
  {
    let len = s.len();
    Self::to_utf32_with_slice(s, 0, len, scratch)
  }
  /// Decodes the Unicode codepoints from the provided `char[]` and places
  /// them into the provided scratch [`IntsRef`], which must not be `None`,
  /// and returns it.
  pub fn to_utf32_with_slice<AV>(
    s: &str,
    offset: usize,
    length: usize,
    scratch: &mut IntsRefBuilder<AV>,
  ) -> Result<()>
  where
    AV: SharedAccessVec<i32> + WritableVec<i32>,
  {
    let mut int_idx = 0;
    for c in s[offset..offset + length].chars() {
      scratch.grow(int_idx + 1)?;
      scratch.set_int_at(int_idx, c as i32);
      int_idx += 1;
    }
    scratch.set_length(int_idx);
    Ok(())
  }
  /// Takes `u8` values from the [`BytesRef`] and converts them into an
  /// IntsRef.
  pub fn to_ints_ref<AV1, AV2>(
    input: &BytesRef<AV1>,
    scratch: &mut IntsRefBuilder<AV2>,
  ) -> Result<()>
  where
    AV1: SharedAccessVec<u8>,
    AV2: SharedAccessVec<i32> + WritableVec<i32>,
  {
    scratch.grow_no_copy(input.length)?;
    for i in 0..input.length {
      input.bytes.access(|bytes| {
        let byte = bytes[input.offset + i];
        scratch.set_int_at(i, byte as i32);
      })
    }
    scratch.set_length(input.length);
    Ok(())
  }
  /// Converts [`IntsRef`] to [`BytesRef`]; all `i32` values must fit
  /// into a byte.
  pub fn to_bytes_ref<AV1, AV2>(
    input: &IntsRef<AV1>,
    scratch: &mut BytesRefBuilder<AV2>,
  ) -> Result<BytesRef<AV2>>
  where
    AV1: SharedAccessVec<i32>,
    AV2: SharedAccessVec<u8> + WritableVec<u8>,
  {
    scratch.grow(input.length)?;
    for i in 0..input.length {
      input.ints.access(|v| {
        let value = v[i + input.offset];
        debug_assert!(value >= u8::MIN as i32 && value <= u8::MAX as i32);
        scratch.set_byte_at(i, value as u8);
      })
    }
    scratch.set_length(input.length);
    Ok(scratch.get_bytes_owner())
  }

  /// Reads the first arc greater or equal than the given label into the
  /// provided arc in place and returns it iff found, otherwise return `None`.
  ///
  /// * `label` - the label to ceil on
  /// * `fst` - the FST to operate on
  /// * `follow` - the arc to follow reading the label from
  /// * `arc` - the arc to read into in place
  /// * `in_reader` - the FST's [`BytesReader`]
  pub fn read_ceil_arc<O, F>(
    label: i32,
    fst: &FST<O, F>,
    follow: &Arc<O::V>,
    arc: &mut Arc<O::V>,
    in_reader: &mut F::FstBytesReader,
  ) -> Result<Option<()>>
  where
    O: Outputs,
    F: FstReader,
  {
    if label == END_LABEL {
      return Ok(read_end_arc(follow, arc));
    }

    if !target_has_arcs(follow) {
      return Ok(None);
    }

    fst.read_first_target_arc(follow, arc, in_reader)?;

    if arc.bytes_per_arc() != 0 && arc.label() != END_LABEL {
      match arc.node_flags() {
        ARCS_FOR_DIRECT_ADDRESSING => {
          let target_index = label - arc.label();
          if target_index >= arc.num_arcs() {
            return Ok(None);
          } else if target_index < 0 {
            return Ok(Some(()));
          }

          if BitTable::is_bit_set(target_index, arc, in_reader)? {
            fst.read_arc_by_direct_addressing(arc, in_reader, target_index)?;
            debug_assert_eq!(arc.label(), label);
          } else {
            let ceil_index = BitTable::next_bit_set(target_index, arc, in_reader)?;
            debug_assert!(ceil_index != -1);
            fst.read_arc_by_direct_addressing(arc, in_reader, ceil_index)?;
            debug_assert!(arc.label() > label);
          }
          return Ok(Some(()));
        },

        ARCS_FOR_CONTINUOUS => {
          let target_index = label - arc.label();
          return if target_index >= arc.num_arcs() {
            Ok(None)
          } else if target_index < 0 {
            Ok(Some(()))
          } else {
            fst.read_arc_by_continuous(arc, in_reader, target_index)?;
            debug_assert_eq!(arc.label(), label);
            Ok(Some(()))
          };
        },

        _ => {
          // Fixed length arcs in a binary search node.
          let mut idx = Self::binary_search(fst, arc, label)?;
          if idx >= 0 {
            fst.read_arc_by_index(arc, in_reader, idx)?;
            return Ok(Some(()));
          }
          idx = -1 - idx;
          if idx == arc.num_arcs() {
            // DEAD END!
            return Ok(None);
          }
          fst.read_arc_by_index(arc, in_reader, idx)?;
          return Ok(Some(()));
        },
      }
    }

    // Variable length arcs in a linear scan list,
    // or special arc with label == FST.END_LABEL.
    fst.read_first_real_target_arc(follow.target(), arc, in_reader)?;
    loop {
      if arc.label() >= label {
        return Ok(Some(()));
      } else if arc.is_last() {
        return Ok(None);
      }
      fst.read_next_real_arc(arc, in_reader)?;
    }
  }

  /// Perform a binary search of Arcs encoded as a packed array.
  ///
  /// * `fst` - the FST from which to read
  /// * `arc` - the starting arc; sibling arcs greater than this will be
  ///   searched. Usually the first arc in the array.
  /// * `target_label` - the label to search for
  ///
  /// Returns the index of the Arc having the target label, or if no Arc has
  /// the matching label, `-1 - idx`, where `idx` is the index of the Arc with
  /// the next highest label, or the total number of arcs if the target label
  /// exceeds the maximum.
  pub fn binary_search<O, F>(fst: &FST<O, F>, arc: &Arc<O::V>, target_label: i32) -> Result<i32>
  where
    O: Outputs,
    F: FstReader,
  {
    debug_assert!(
      arc.node_flags() == ARCS_FOR_BINARY_SEARCH,
      "Arc is not encoded as packed array for binary search (nodeFlags={})",
      arc.node_flags()
    );

    let mut in_reader = fst.get_bytes_reader()?;
    let mut low = arc.arc_idx();
    let mut high = arc.num_arcs() - 1;

    while low <= high {
      let mid = (low + high) >> 1;

      in_reader.set_position(arc.pos_arcs_start());
      in_reader.skip_bytes((arc.bytes_per_arc() * mid + 1) as i64)?;

      let mid_label = fst.read_label(&mut in_reader)?;
      let cmp = mid_label - target_label;

      if cmp < 0 {
        low = mid + 1;
      } else if cmp > 0 {
        high = mid - 1;
      } else {
        return Ok(mid);
      }
    }

    Ok(-1 - low)
  }
}
/// Represents a path in TopNSearcher, parameterized by its output payload type.
pub struct FSTPath<T> {
  /// Holds the last arc appended to this path
  pub arc: Arc<T>,
  /// Holds cost plus any usage-specific output:
  pub output: Option<T>,
  pub input: IntsRefBuilder<Vec<i32>>,
  pub boost: f32,
  pub context: String,
  // Custom int payload for consumers; the NRT suggester uses this to record
  // if this path has already enumerated a surface form
  pub payload: i32,
}

impl<T> FSTPath<T>
where
  T: Clone,
{
  pub fn new<V>(
    output: Option<T>,
    other: &Arc<T>,
    input: IntsRefBuilder<Vec<i32>>,
    boost: f32,
    context: V,
    payload: i32,
  ) -> Self
  where
    V: Into<String>,
  {
    FSTPath {
      arc: other.clone(),
      output,
      input,
      boost,
      context: context.into(),
      payload,
    }
  }
}

impl<T> FSTPath<T>
where
  T: Clone,
{
  pub fn new_path(&self, output: Option<T>, input: IntsRefBuilder<Vec<i32>>) -> Self {
    FSTPath {
      arc: self.arc.clone(),
      output,
      input,
      boost: self.boost,
      context: self.context.clone(),
      payload: self.payload,
    }
  }
}
impl<T> Display for FSTPath<T>
where
  T: Display,
{
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "input={} output=", self.input.get())?;
    match &self.output {
      Some(output) => write!(f, "{output}")?,
      None => write!(f, "None")?,
    }
    write!(
      f,
      " context={} boost={} payload={}",
      self.context, self.boost, self.payload
    )
  }
}

/// Compares first by the provided comparator, and then tie breaks by path.input.
pub struct TieBreakByInputComparator<C> {
  comparator: C,
}

impl<C> TieBreakByInputComparator<C> {
  pub fn new(comparator: C) -> Self {
    Self { comparator }
  }
}

impl<T, C> Comparator<FSTPath<T>> for TieBreakByInputComparator<C>
where
  T: OutputsBound,
  C: Comparator<Option<T>>,
{
  const TYPE: &'static str = "TieBreakByInputComparator";

  fn compare(&self, a: &FSTPath<T>, b: &FSTPath<T>) -> Result<i32> {
    let cmp = self.comparator.compare(&a.output, &b.output)?;
    if cmp == 0 {
      Ok(a.input.get().cmp(b.input.get()).to_int())
    } else {
      Ok(cmp)
    }
  }
}

/// Utility for finding the top N shortest paths from one or more starting points.
pub struct TopNSearcher<'a, O, F, C, PC>
where
  O: Outputs,
  F: FstReader,
  C: Comparator<Option<O::V>>,
  PC: Comparator<FSTPath<O::V>>,
{
  fst: &'a FST<O, F>,
  bytes_reader: F::FstBytesReader,
  top_n: usize,
  max_queue_depth: usize,
  scratch_arc: Arc<O::V>,
  comparator: C,
  path_comparator: PC,
  base: Box<dyn TopNSearcherBase<O::V> + 'a>,
  queue: Option<VecDeque<FSTPath<O::V>>>,
}

pub trait TopNSearcherBase<T>
where
  T: OutputsBound,
{
  fn accept_result_path(&mut self, path: &FSTPath<T>) -> bool {
    self.accept_result(path.input.get(), &path.output)
  }

  /// Implement this to prevent considering a path before it is complete.
  fn accept_partial_path(&mut self, _path: &FSTPath<T>) -> bool {
    true
  }

  fn accept_result(&mut self, _input: &IntsRef<Vec<i32>>, _output: &Option<T>) -> bool {
    true
  }
}

pub struct DefaultTopNSearcherBase;

impl<T> TopNSearcherBase<T> for DefaultTopNSearcherBase where T: OutputsBound {}

impl<'a, O, F, C> TopNSearcher<'a, O, F, C, TieBreakByInputComparator<C>>
where
  O: Outputs,
  F: FstReader,
  C: Comparator<Option<O::V>> + Clone,
{
  /// Creates an unbounded TopNSearcher.
  pub fn new(
    fst: &'a FST<O, F>,
    top_n: usize,
    max_queue_depth: usize,
    comparator: C,
  ) -> Result<Self> {
    Self::with_path_comparator(
      fst,
      top_n,
      max_queue_depth,
      comparator.clone(),
      TieBreakByInputComparator::new(comparator),
    )
  }
}

impl<'a, O, F, C, PC> TopNSearcher<'a, O, F, C, PC>
where
  O: Outputs,
  F: FstReader,
  C: Comparator<Option<O::V>>,
  PC: Comparator<FSTPath<O::V>>,
{
  pub fn with_path_comparator(
    fst: &'a FST<O, F>,
    top_n: usize,
    max_queue_depth: usize,
    comparator: C,
    path_comparator: PC,
  ) -> Result<Self> {
    Ok(Self {
      fst,
      bytes_reader: fst.get_bytes_reader()?,
      top_n,
      max_queue_depth,
      scratch_arc: Arc::default(),
      comparator,
      path_comparator,
      base: Box::new(DefaultTopNSearcherBase),
      queue: Some(VecDeque::new()),
    })
  }

  pub fn set_base<B>(&mut self, base: B)
  where
    B: TopNSearcherBase<O::V> + 'a,
  {
    self.base = Box::new(base);
  }

  fn insert_queue(&mut self, path: FSTPath<O::V>) -> Result<()> {
    let Some(queue) = self.queue.as_mut() else {
      return Ok(());
    };

    for i in 0..queue.len() {
      match self.path_comparator.compare(&path, &queue[i])?.cmp(&0) {
        Ordering::Less => {
          queue.insert(i, path);
          return Ok(());
        },
        Ordering::Equal => return Ok(()),
        Ordering::Greater => {},
      }
    }

    queue.push_back(path);
    Ok(())
  }

  // If back plus this arc is competitive then add to queue:
  pub fn add_if_competitive(&mut self, path: &mut FSTPath<O::V>) -> Result<()> {
    debug_assert!(self.queue.is_some());

    let output = self
      .fst
      .outputs
      .add(&path.output, &path.arc.output)
      .map(std::borrow::Cow::into_owned);

    if let Some(queue) = self.queue.as_ref()
      && queue.len() == self.max_queue_depth
      && self.max_queue_depth > 0
    {
      let bottom = queue
        .back()
        .ok_or_else(|| LuceneError::illegal_state("queue must have a bottom path"))?;
      let comp = self.path_comparator.compare(path, bottom)?;
      if comp > 0 {
        return Ok(());
      } else if comp == 0 {
        path.input.append(path.arc.label())?;
        let cmp = bottom.input.get().cmp(path.input.get()).to_int();
        path.input.set_length(path.input.length() - 1);

        debug_assert_ne!(cmp, 0);

        if cmp < 0 {
          return Ok(());
        }
      }
    }

    let mut new_input = IntsRefBuilder::new();
    new_input.copy_ints_ref(path.input.get())?;
    new_input.append(path.arc.label())?;

    let new_path = path.new_path(output, new_input);
    if self.base.accept_partial_path(&new_path) {
      self.insert_queue(new_path)?;
      if let Some(queue) = self.queue.as_mut()
        && queue.len() == self.max_queue_depth + 1
      {
        queue.pop_back();
      }
    }

    Ok(())
  }

  pub fn add_start_paths(
    &mut self,
    node: &Arc<O::V>,
    start_output: Option<O::V>,
    allow_empty_string: bool,
    input: IntsRefBuilder<Vec<i32>>,
  ) -> Result<()> {
    self.add_start_paths_with_context(node, start_output, allow_empty_string, input, 0.0, "", -1)
  }

  /// Adds all leaving arcs, including 'finished' arc, if the node is final, from this node into
  /// the queue.
  #[allow(clippy::too_many_arguments)]
  pub fn add_start_paths_with_context<T>(
    &mut self,
    node: &Arc<O::V>,
    start_output: Option<O::V>,
    allow_empty_string: bool,
    input: IntsRefBuilder<Vec<i32>>,
    boost: f32,
    context: T,
    payload: i32,
  ) -> Result<()>
  where
    T: Into<String>,
  {
    let mut path = FSTPath::new(start_output, node, input, boost, context, payload);
    self
      .fst
      .read_first_target_arc(node, &mut path.arc, &mut self.bytes_reader)?;

    loop {
      if allow_empty_string || path.arc.label() != END_LABEL {
        self.add_if_competitive(&mut path)?;
      }
      if path.arc.is_last() {
        break;
      }
      self
        .fst
        .read_next_arc(&mut path.arc, &mut self.bytes_reader)?;
    }

    Ok(())
  }

  pub fn search(&mut self) -> Result<TopResults<O::V>> {
    let mut results = Vec::new();
    let mut fst_reader = self.fst.get_bytes_reader()?;
    let mut reject_count = 0;

    while results.len() < self.top_n {
      let Some(queue) = self.queue.as_mut() else {
        break;
      };

      if queue.is_empty() {
        break;
      }

      let mut path = expect_invariant!(
        queue.pop_front(),
        "the queue was checked to be non-empty immediately above"
      );

      if !self.base.accept_partial_path(&path) {
        continue;
      }

      if path.arc.label() == END_LABEL {
        path.input.set_length(path.input.length() - 1);
        let mut input = path.input.get_owner();
        input.ints.truncate(input.length);
        results.push(TopResult::new(input, path.output));
        continue;
      }

      if results.len() == self.top_n - 1 && self.max_queue_depth == self.top_n {
        self.queue = None;
      }

      loop {
        self
          .fst
          .read_first_target_arc_impl(None, &mut path.arc, &mut fst_reader)?;

        let mut found_zero = false;
        let mut arc_copy_is_pending = false;
        loop {
          if self.comparator.compare(&None, &path.arc.output)? == 0 {
            if self.queue.is_none() {
              found_zero = true;
              break;
            } else if !found_zero {
              arc_copy_is_pending = true;
              found_zero = true;
            } else {
              self.add_if_competitive(&mut path)?;
            }
          } else if self.queue.is_some() {
            self.add_if_competitive(&mut path)?;
          }
          if path.arc.is_last() {
            break;
          }
          if arc_copy_is_pending {
            self.scratch_arc.copy_from(&path.arc);
            arc_copy_is_pending = false;
          }
          self.fst.read_next_arc(&mut path.arc, &mut fst_reader)?;
        }

        debug_assert!(found_zero);

        if self.queue.is_some() && !arc_copy_is_pending {
          path.arc.copy_from(&self.scratch_arc);
        }

        if path.arc.label() == END_LABEL {
          path.output = self
            .fst
            .outputs
            .add(&path.output, &path.arc.output)
            .map(std::borrow::Cow::into_owned);
          if self.base.accept_result_path(&path) {
            let mut input = path.input.get_owner();
            input.ints.truncate(input.length);
            results.push(TopResult::new(input, path.output));
          } else {
            reject_count += 1;
          }
          break;
        } else {
          path.input.append(path.arc.label())?;
          path.output = self
            .fst
            .outputs
            .add(&path.output, &path.arc.output)
            .map(std::borrow::Cow::into_owned);
          if !self.base.accept_partial_path(&path) {
            break;
          }
        }
      }
    }

    Ok(TopResults::new(
      reject_count + self.top_n <= self.max_queue_depth,
      results,
    ))
  }
}

/// Holds a single input (IntsRef) + output, returned by shortest_paths.
/// `T` is the payload type; an empty output is stored as `None`.
pub struct TopResult<T> {
  pub input: IntsRef<Vec<i32>>,
  pub output: Option<T>,
}

impl<T> TopResult<T> {
  pub fn new(input: IntsRef<Vec<i32>>, output: Option<T>) -> Self {
    Self { input, output }
  }
}

/// Holds the results for a top N search using TopNSearcher.
pub struct TopResults<T> {
  /// `true` iff this is a complete result ie. if the specified queue size was large enough to find
  /// the complete list of results. This might be `false` if the [`TopNSearcher`] rejected too many
  /// results.
  pub is_complete: bool,
  /// The top results.
  pub top_n: Vec<TopResult<T>>,
}

impl<T> TopResults<T> {
  fn new(is_complete: bool, top_n: Vec<TopResult<T>>) -> Self {
    Self { is_complete, top_n }
  }
}

impl<T> IntoIterator for TopResults<T> {
  type Item = TopResult<T>;
  type IntoIter = std::vec::IntoIter<TopResult<T>>;

  fn into_iter(self) -> Self::IntoIter {
    self.top_n.into_iter()
  }
}
