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
use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::Rc;

use rand::Rng;

use crate::core::store::IOContext;
use crate::core::store::directory::Directory;
use crate::core::util::ToInt;
use crate::core::util::access::SharedAccessVec;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fst_impl::fst::{Arc, END_LABEL, FST, InputType, read_metadata};
use crate::core::util::fst_impl::fst_compiler::{Builder, DataOutputEnum};
use crate::core::util::fst_impl::fst_reader::FstReader;
use crate::core::util::fst_impl::ints_ref_fst_enum::IntsRefFSTEnum;
use crate::core::util::fst_impl::on_heap_fst_store::OnHeapFSTStore;
use crate::core::util::fst_impl::outputs::{Outputs, OutputsBound};
use crate::core::util::ints_ref::IntsRef;
use crate::core::util::ints_ref_builder::IntsRefBuilder;
use crate::test::util::lucene_test_case::lucene_test_case_util::{
    at_least, new_io_context, random_from_seed,
};
/// Helper struct to test FSTs.
#[allow(clippy::type_complexity)]
pub struct FSTTester<D, R, O, S>
where
    D: Directory,
    R: Rng,
    O: Outputs,
    S: FSTTesterBase,
{
    pub random: R,
    pub pairs: Vec<InputOutput<O::V, Vec<i32>>>,
    pub input_mode: i32,
    pub outputs: O,
    pub dir: Rc<RefCell<D>>,

    pub node_count: i64,
    pub arc_count: i64,
    pub sub: Option<S>,
}
impl<D, R, O, S> FSTTester<D, R, O, S>
where
    D: Directory,
    R: Rng,
    O: Outputs,
    S: FSTTesterBase,
{
    #[allow(clippy::type_complexity)]
    pub fn new(
        random: R,
        dir: Rc<RefCell<D>>,
        input_mode: i32,
        pairs: Vec<InputOutput<O::V, Vec<i32>>>,
        outputs: O,
    ) -> Self {
        Self {
            random,
            dir,
            input_mode,
            pairs,
            outputs,
            node_count: 0,
            arc_count: 0,
            sub: None,
        }
    }
    // runs the term, returning the output, or null if term
    // isn't accepted.  if prefixLength is non-null it must be
    // length 1 int array; prefixLength[0] is set to the length
    // of the term prefix that matches
    pub fn run<F, AV>(
        fst: &FST<O, F>,
        term: &IntsRef<AV>,
        mut prefix_length: Option<&mut [i32]>,
    ) -> Result<Option<O::V>>
    where
        F: FstReader,
        AV: SharedAccessVec<i32>,
    {
        assert!(prefix_length.is_none() || prefix_length.as_ref().unwrap().len() == 1);
        let mut arc = Arc::default();
        fst.get_first_arc(&mut arc);
        let mut output = fst.outputs.get_no_output();
        let mut reader = fst.get_bytes_reader()?;

        for i in 0..=term.length {
            let label = if i == term.length {
                END_LABEL
            } else {
                term.ints.access(|ints| ints[term.offset + i])
            };

            let find = fst.find_target_arc(label, &arc.clone(), &mut arc, &mut reader)?;
            if find.is_none() {
                if let Some(prefix) = prefix_length.as_mut() {
                    prefix[0] = i as i32;
                    return Ok(Some(output));
                } else {
                    return Ok(None);
                }
            }
            output = fst.outputs.add(&output, &arc.output());
        }
        if let Some(prefix) = prefix_length.as_mut() {
            prefix[0] = term.length as i32;
        }

        Ok(Some(output))
    }
    pub fn random_accepted_word<F, AV>(
        fst: &FST<O, F>,
        in_builder: &mut IntsRefBuilder<AV>,
        random: &mut impl Rng,
    ) -> Result<O::V>
    where
        F: FstReader,
        AV: SharedAccessVec<i32>,
    {
        let mut arc = Arc::default();
        fst.get_first_arc(&mut arc);
        let mut arcs = Vec::new();
        in_builder.clear();
        let mut output = fst.outputs.get_no_output();
        let mut reader = fst.get_bytes_reader()?;

        loop {
            fst.read_first_target_arc(&arc.clone(), &mut arc, &mut reader)?;
            let mut new_arc = Arc::default();
            new_arc.copy_from(&arc);
            arcs.push(new_arc);
            while !arc.is_last() {
                fst.read_next_arc(&mut arc, &mut reader)?;
                let mut new_arc = Arc::default();
                new_arc.copy_from(&arc);
                arcs.push(new_arc);
            }
            let idx = random.random_range(0..arcs.len());
            arc = arcs[idx].clone();
            arcs.clear();
            output = fst.outputs.add(&output, &arc.output());

            if arc.label() == END_LABEL {
                break;
            }
            in_builder.append(arc.label());
        }

        Ok(output)
    }

    // Using the same seed to generate the same type of FST object allows the fst
    // inside IntsRefFSTEnum to be replaced using std::mem::replace. The purpose
    // of this is to remain consistent with the behavior in Java Lucene.
    #[allow(clippy::type_complexity)]
    pub fn get_fst(&self, seed: u64) -> Result<(Option<FSTEnums<O, D>>, i64, i64)> {
        let mut random = random_from_seed(seed);
        let input_type = if self.input_mode == 0 {
            InputType::Byte1
        } else {
            InputType::Byte4
        };
        let mut fst_compiler_builder: Builder<_, D> =
            Builder::new(input_type, self.outputs.clone());
        let use_off_heap = random.random_bool(0.5);
        if use_off_heap {
            let out = self
                .dir
                .borrow_mut()
                .create_output("fstOffHeap.bin", &IOContext::default_io_context()?)?;
            let out = DataOutputEnum::FromDir(out);
            fst_compiler_builder.data_output(out);
        }
        let mut fst_compiler = fst_compiler_builder.build()?;
        for pair in &self.pairs {
            pair.input.ints.access(|ints| {
                // TODO: 没有判断是否为
                // if let Some(list) = pair.output.as_list_of_longs() {
                //     for value in list {
                //         fst_compiler.add(&pair.input, &value)?;
                //     }
                // } else {
                let v = IntsRef::from_slice(ints.clone(), pair.input.offset, pair.input.length);
                fst_compiler.add(&v, pair.output.clone())?;
                // Help the compiler infer types.
                Ok::<(), LuceneError>(())
            })?;
        }
        let fst_metadata = fst_compiler.compile()?;
        let node_count = fst_compiler.get_node_count();
        let arc_count = fst_compiler.get_arc_count();
        let fst = if use_off_heap {
            if fst_metadata.is_none() {
                self.dir.borrow_mut().delete_file("fstOffHeap.bin")?;
                None
            } else {
                // flush data to file
                drop(fst_compiler);
                let mut input = self
                    .dir
                    .borrow_mut()
                    .open_input("fstOffHeap.bin", &IOContext::default_io_context()?)?;
                let fst = FST::from_on_heap_store(fst_metadata.unwrap(), &mut input)?;
                self.dir.borrow_mut().delete_file("fstOffHeap.bin")?;
                Some(FSTEnums::FST1(fst))
            }
        } else if fst_metadata.is_some() {
            let fst = FST::from_fst_reader(fst_metadata, Some(fst_compiler.get_fst_reader()?));
            if random.random_bool(0.5) {
                let ctx = new_io_context(&mut random)?;
                {
                    let mut out = self.dir.borrow_mut().create_output("fst.bin", &ctx)?;
                    if let Some(fst_ref) = &fst {
                        fst_ref.save_with_same_data_out(&mut out)?;
                    }
                }
                let mut input = self.dir.borrow_mut().open_input("fst.bin", &ctx)?;
                let metadata = read_metadata(&mut input, self.outputs.clone())?;
                let fst = FST::from_on_heap_store(metadata, &mut input)?;
                self.dir.borrow_mut().delete_file("fst.bin")?;
                Some(FSTEnums::FST1(fst))
            } else {
                Some(FSTEnums::FST2(fst.unwrap()))
            }
        } else {
            None
        };

        Ok((fst, node_count, arc_count))
    }

    pub fn do_test(&mut self) -> Result<Option<FSTEnums<O, D>>> {
        let seed = self.random.random();
        let (fst_enums, node_count, arc_count) = self.get_fst(seed)?;

        // if cfg!(feature = "test_log_verbose") && self.pairs.len() <= 20 {
        //     if let Some(fst_ref) = &fst {
        //         println!("Printing FST as dot file to stdout:");
        //         let mut writer = std::io::BufWriter::new(std::io::stdout());
        //         Util::to_dot(fst_ref, &mut writer, false, false)?;
        //         writer.flush()?;
        //         println!("END dot file");
        //     }
        // }
        //
        // if cfg!(feature = "test_log_verbose") {
        //     if fst.is_none() {
        //         println!("  fst has 0 nodes (fully pruned)");
        //     } else {
        //         println!(
        //             "  fst has {} nodes and {} arcs",
        //             fst_compiler.get_node_count(),
        //             fst_compiler.get_arc_count()
        //         );
        //     }
        // }
        //

        match fst_enums {
            Some(FSTEnums::FST1(reuse)) => {
                self.run_steps(seed, reuse, |e| match e {
                    FSTEnums::FST1(fst) => fst,
                    _ => panic!("Expected FST1"),
                })?;
            },
            Some(FSTEnums::FST2(reuse)) => {
                self.run_steps(seed, reuse, |e| match e {
                    FSTEnums::FST2(fst) => fst,
                    _ => panic!("Expected FST2"),
                })?;
            },
            None => {},
        }
        self.node_count = node_count;
        self.arc_count = arc_count;
        let (fst, _, _) = self.get_fst(seed)?;
        Ok(fst)
    }
    fn run_steps<C, F>(&mut self, seed: u64, mut reuse: FST<O, F>, unwrap_fn: C) -> Result<()>
    where
        O: Outputs,
        C: Fn(FSTEnums<O, D>) -> FST<O, F>,
        F: FstReader,
        D: Directory,
    {
        // step 1
        let mut v = self.step1(self.input_mode, Some(reuse))?;
        let (fst, _, _) = self.get_fst(seed)?;
        let padding_fst = unwrap_fn(fst.unwrap());
        reuse = std::mem::replace(&mut v.base.fst, padding_fst);

        // init terms_map
        let mut terms_map = HashMap::new();
        for pair in &self.pairs {
            terms_map.insert(pair.input.clone(), pair.output.clone());
        }

        // step 2
        let mut v = self.step2(self.input_mode, Some(reuse), &terms_map)?;
        let (fst, _, _) = self.get_fst(seed)?;
        let padding_fst = unwrap_fn(fst.unwrap());
        reuse = std::mem::replace(&mut v.base.fst, padding_fst);

        // step 3
        let num = at_least(&mut self.random, 100);
        for i in 0..num {
            if cfg!(feature = "test_log_verbose") {
                println!("TEST: iter {}", i);
            }
            let fst_enum = IntsRefFSTEnum::new(reuse)?;
            let mut v = self.step3(self.input_mode, fst_enum, &terms_map)?;
            let (fst, _, _) = self.get_fst(seed)?;
            let padding_fst = unwrap_fn(fst.unwrap());
            reuse = std::mem::replace(&mut v.base.fst, padding_fst);
        }

        Ok(())
    }
    #[allow(clippy::type_complexity)]
    pub fn step3<F>(
        &mut self,
        input_mode: i32,
        mut fst_enum: IntsRefFSTEnum<O, F>,
        terms_map: &HashMap<IntsRef<Vec<i32>>, O::V>,
    ) -> Result<IntsRefFSTEnum<O, F>>
    where
        F: FstReader,
    {
        let mut upto: i32 = -1;
        loop {
            let mut is_done = false;

            if (upto) == self.pairs.len() as i32 - 1 || self.random.random_bool(0.5) {
                // next
                upto += 1;
                if cfg!(feature = "test_log_verbose") {
                    println!("  do next");
                }
                is_done = fst_enum.next_value()?.is_none();
            } else if upto != -1
                && upto < (0.75 * (self.pairs.len() as f32)) as i32
                && self.random.random_bool(0.5)
            {
                let mut attempt = 0;
                while attempt < 10 {
                    let term_str = get_random_string(&mut self.random);
                    let mut ir_builder = IntsRefBuilder::default();
                    let term: IntsRef<Vec<i32>> = to_ints_ref_from_string_with_builder(
                        &term_str,
                        input_mode,
                        &mut ir_builder,
                    );
                    if !terms_map.contains_key(&term)
                        && term.cmp(&self.pairs[upto as usize].input).to_int() > 0
                    {
                        let pos = self
                            .pairs
                            .binary_search_by(|p| p.input.cmp(&term))
                            .expect_err("expected term to not exist");
                        upto = pos as i32;
                        if self.random.random_bool(0.5) {
                            // if true{
                            upto -= 1;
                            assert_ne!(upto, -1);
                            if cfg!(feature = "test_log_verbose") {
                                println!(
                                    "  do non-exist seekFloor({})",
                                    input_to_string(input_mode, &term)?
                                );
                            }
                            is_done = fst_enum.seek_floor(&term)?.is_none();
                        } else {
                            if cfg!(feature = "test_log_verbose") {
                                println!(
                                    "  do non-exist seekCeil({})",
                                    input_to_string(input_mode, &term)?
                                );
                            }
                            is_done = fst_enum.seek_ceil(&term)?.is_none();
                        }
                        break;
                    }
                    attempt += 1;
                }
                if attempt == 10 {
                    continue;
                }
            } else {
                let inc = self
                    .random
                    .random_range(0..(self.pairs.len() - (upto + 1) as usize));
                upto += inc as i32;
                if upto == -1 {
                    upto = 0;
                }

                if self.random.random_bool(0.5) {
                    // if false {
                    if cfg!(feature = "test_log_verbose") {
                        println!(
                            "  do seekCeil({})",
                            input_to_string(input_mode, &self.pairs[upto as usize].input)?
                        );
                    }
                    is_done = fst_enum
                        .seek_ceil(&self.pairs[upto as usize].input)?
                        .is_none();
                } else {
                    if cfg!(feature = "test_log_verbose") {
                        println!(
                            "  do seekFloor({})",
                            input_to_string(input_mode, &self.pairs[upto as usize].input)?
                        );
                    }
                    is_done = fst_enum
                        .seek_floor(&self.pairs[upto as usize].input)?
                        .is_none();
                }
            }

            if cfg!(feature = "test_log_verbose") {
                if !is_done {
                    let current = fst_enum.current();
                    println!("    got {}", input_to_string(input_mode, &current.input)?);
                } else {
                    println!("    got null");
                }
            }

            if upto as usize == self.pairs.len() {
                assert!(is_done);
                break;
            } else {
                assert!(!is_done);
                let current = fst_enum.current();
                assert_eq!(
                    &current.input,
                    &self.pairs[upto as usize].input,
                    "expected input={} but got {}",
                    input_to_string(input_mode, &self.pairs[upto as usize].input)?,
                    input_to_string(input_mode, &current.input)?
                );
                assert!(
                    self.outputs_equal(&self.pairs[upto as usize].output, &current.output),
                    "output mismatch at input={}",
                    input_to_string(input_mode, &self.pairs[upto as usize].input)?
                );
            }
        }
        Ok(fst_enum)
    }
    #[allow(clippy::type_complexity)]
    pub fn step2<F>(
        &mut self,
        input_mode: i32,
        mut fst: Option<FST<O, F>>,
        terms_map: &HashMap<IntsRef<Vec<i32>>, O::V>,
    ) -> Result<IntsRefFSTEnum<O, F>>
    where
        F: FstReader,
    {
        if cfg!(feature = "test_log_verbose") {
            println!("TEST: verify random accepted terms");
        }

        let mut scratch = IntsRefBuilder::default();
        let num = at_least(&mut self.random, 500);
        for _ in 0..num {
            let output = FSTTester::<D, R, O, S>::random_accepted_word(
                fst.as_mut().unwrap(),
                &mut scratch,
                &mut self.random,
            )?;
            let key = scratch.get();
            let error_msg = format!(
                "accepted word {} is not valid",
                input_to_string(input_mode, key)?
            );
            let expected = terms_map.get(key).expect(&error_msg);
            assert!(
                self.outputs_equal(expected, &output),
                "mismatched output for {}",
                input_to_string(input_mode, key)?
            );
        }

        if cfg!(feature = "test_log_verbose") {
            println!("TEST: verify seek");
        }
        let mut fst_enum = IntsRefFSTEnum::new(fst.unwrap())?;
        let num_seek = at_least(&mut self.random, 100);
        // let num_seek = 1;
        for iter in 0..num_seek {
            if cfg!(feature = "test_log_verbose") {
                println!("  iter={}", iter);
            }
            if self.random.random_bool(0.5) {
                // if true {
                // seek to term that doesn't exist
                loop {
                    let term_str = get_random_string(&mut self.random);
                    let mut ir_builder = IntsRefBuilder::default();
                    let term = to_ints_ref_from_string_with_builder(
                        &term_str,
                        input_mode,
                        &mut ir_builder,
                    );

                    let target = InputOutput::new(term.clone(), self.outputs.get_no_output());
                    let pos = self.pairs.binary_search_by(|p| p.input.cmp(&target.input));

                    if let Err(pos) = pos {
                        let mut pos = pos as i32;
                        // Not found
                        let seek_result = if self.random.random_range(0..3) == 0 {
                            if cfg!(feature = "test_log_verbose") {
                                println!(
                                    "  do non-exist seekExact term={}",
                                    input_to_string(input_mode, &term)?
                                );
                            }
                            pos = -1;
                            fst_enum.seek_exact(&term)?
                        // } else if false{
                        } else if self.random.random_bool(0.5) {
                            if cfg!(feature = "test_log_verbose") {
                                println!(
                                    "  do non-exist seekFloor term={}",
                                    input_to_string(input_mode, &term)?
                                );
                            }

                            pos = pos.saturating_sub(1);
                            fst_enum.seek_floor(&term)?
                        } else {
                            if cfg!(feature = "test_log_verbose") {
                                println!(
                                    "  do non-exist seekCeil term={}",
                                    input_to_string(input_mode, &term)?
                                );
                            }

                            fst_enum.seek_ceil(&term)?
                        };

                        if pos != -1 && pos < self.pairs.len() as i32 {
                            let expected = &self.pairs[pos as usize];

                            assert!(
                                seek_result.is_some(),
                                "got null but expected term={}",
                                input_to_string(input_mode, &expected.input)?
                            );

                            let actual = seek_result.unwrap();
                            if cfg!(feature = "test_log_verbose") {
                                println!("    got {}", input_to_string(input_mode, &actual.input)?);
                            }

                            assert_eq!(
                                &actual.input,
                                &expected.input,
                                "expected input={} but got {}",
                                input_to_string(input_mode, &expected.input)?,
                                input_to_string(input_mode, &actual.input)?
                            );

                            assert!(
                                self.outputs_equal(&expected.output, &actual.output),
                                "output mismatch at term={}",
                                input_to_string(input_mode, &expected.input)?
                            );
                        } else {
                            // seeked before start or beyond end
                            assert!(
                                seek_result.is_none(),
                                "expected null but got {}",
                                input_to_string(input_mode, &seek_result.unwrap().input)?
                            );
                            if cfg!(feature = "test_log_verbose") {
                                println!("    got null");
                            }
                        }

                        break;
                    }
                }
            } else {
                // seek to existing term
                let len = self.pairs.len();
                let pair = &self.pairs[self.random.random_range(0..len)];
                let seek_result = if self.random.random_range(0..3) == 2 {
                    // let seek_result = if true {
                    if cfg!(feature = "test_log_verbose") {
                        println!(
                            "  do exists seekExact term={}",
                            input_to_string(input_mode, &pair.input,)?
                        );
                    }
                    fst_enum.seek_exact(&pair.input)?
                } else if self.random.random_bool(0.5) {
                    // } else if false {
                    if cfg!(feature = "test_log_verbose") {
                        println!(
                            "  do exists seekFloor term={}",
                            input_to_string(input_mode, &pair.input,)?
                        );
                    };
                    fst_enum.seek_floor(&pair.input)?
                } else {
                    if cfg!(feature = "test_log_verbose") {
                        println!(
                            "  do exists seekCeil term={}",
                            input_to_string(input_mode, &pair.input,)?
                        );
                    };
                    fst_enum.seek_ceil(&pair.input)?
                };

                let seek_result = seek_result.expect("expected seek result, got None");

                assert_eq!(
                    &seek_result.input,
                    &pair.input,
                    "got {} but expected {}",
                    input_to_string(input_mode, &seek_result.input)?,
                    input_to_string(input_mode, &pair.input)?
                );

                assert!(
                    self.outputs_equal(&pair.output, &seek_result.output),
                    "output mismatch at input={}",
                    input_to_string(input_mode, &pair.input)?
                );
            }
        }
        if cfg!(feature = "test_log_verbose") {
            println!("TEST: mixed next/seek");
        }
        Ok(fst_enum)
    }

    pub fn step1<F>(&self, input_mode: i32, fst: Option<FST<O, F>>) -> Result<IntsRefFSTEnum<O, F>>
    where
        O: Outputs,
        F: FstReader,
    {
        let mut fst_enum = IntsRefFSTEnum::new(fst.unwrap())?;

        for pair in &self.pairs {
            let term = &pair.input;
            let output = FSTTester::<D, R, O, S>::run(&fst_enum.base.fst, term, None)?;
            assert!(
                output.is_some(),
                "term {} is not accepted",
                input_to_string(input_mode, term)?
            );
            assert!(self.outputs_equal(&pair.output, output.as_ref().unwrap()));

            let t = fst_enum.next_value()?;
            assert!(t.is_some(), "expected more terms");
            let t = t.unwrap();
            assert_eq!(
                &t.input,
                term,
                "expected input={} but got {}",
                input_to_string(input_mode, term,)?,
                input_to_string(input_mode, &t.input,)?
            );
            assert!(self.outputs_equal(&pair.output, &t.output));
        }

        assert!(
            fst_enum.next_value()?.is_none(),
            "expected no more terms at end"
        );
        Ok(fst_enum)
    }

    pub fn verify_unpruned<F>(
        &self,
        _input_mode: i32,
        _fst: Option<FST<O, F>>,
        _random: &mut impl Rng,
        _seed: u64,
    ) -> Result<()>
    where
        F: FstReader,
    {
        // Due to Rust's ownership and borrowing rules, once ownership of fst is
        // transferred to IntsRefFSTEnum, it can no longer be reused.
        // To make this functionality work and keep consistent with Java Lucene, the
        // method was split into three separate steps in `run_steps()`, allowing fst to
        // be reused.
        // See `self.step1`,`self.step2`,`self.step2`
        Ok(())
    }
    fn outputs_equal(&self, a: &O::V, b: &O::V) -> bool {
        if self.sub.is_some() {
            self.sub.as_ref().unwrap().outputs_equal_impl(a, b)
        } else {
            *a == *b
        }
    }
}
pub trait FSTTesterBase {
    fn outputs_equal_impl<T>(&self, a: &T, b: &T) -> bool
    where
        T: OutputsBound;
}
pub struct DummyFSTTesterBaseImpl;
impl FSTTesterBase for DummyFSTTesterBaseImpl {
    fn outputs_equal_impl<T>(&self, _a: &T, _b: &T) -> bool
    where
        T: OutputsBound,
    {
        unreachable!()
    }
}

#[derive(Debug, Clone)]
pub struct InputOutput<T, AV>
where
    T: OutputsBound,
    AV: SharedAccessVec<i32>,
{
    pub input: IntsRef<AV>,
    pub output: T,
}

impl<T, AV> InputOutput<T, AV>
where
    T: OutputsBound,
    AV: SharedAccessVec<i32>,
{
    pub fn new(input: IntsRef<AV>, output: T) -> Self {
        Self { input, output }
    }
}
impl<T: PartialEq, AV> PartialEq for InputOutput<T, AV>
where
    T: OutputsBound,
    AV: SharedAccessVec<i32>,
{
    fn eq(&self, other: &Self) -> bool {
        self.input == other.input
    }
}

impl<T: Eq, AV: SharedAccessVec<i32>> Eq for InputOutput<T, AV> where T: OutputsBound {}

impl<T: Ord, AV: SharedAccessVec<i32>> PartialOrd<Self> for InputOutput<T, AV>
where
    T: OutputsBound,
{
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T: Ord, AV: SharedAccessVec<i32>> Ord for InputOutput<T, AV>
where
    T: OutputsBound,
{
    fn cmp(&self, other: &Self) -> Ordering {
        self.input.cmp(&other.input)
    }
}

pub enum FSTEnums<O, D>
where
    O: Outputs,
    D: Directory,
{
    FST1(FST<O, OnHeapFSTStore>),
    FST2(FST<O, DataOutputEnum<D>>),
}

use crate::core::index::BytesRef;
use crate::core::util::unicode_util::UnicodeUtil;
use crate::test::util::test_util::TestUtil;

pub fn input_to_string<AV: SharedAccessVec<i32>>(
    input_mode: i32,
    term: &IntsRef<AV>,
) -> Result<String> {
    input_to_string_with_flag(input_mode, term, true)
}

pub fn input_to_string_with_flag<AV: SharedAccessVec<i32>>(
    input_mode: i32,
    term: &IntsRef<AV>,
    is_valid_unicode: bool,
) -> Result<String> {
    if !is_valid_unicode {
        Ok(term.to_string())
    } else if input_mode == 0 {
        // utf8
        let br = get_bytes_ref(term);
        Ok(format!("{} {}", br.utf8_to_string()?, term))
    } else {
        term.ints.access(|ints| {
            let s = UnicodeUtil::new_string(ints, term.offset, term.length)?;
            Ok(format!("{} {}", s, term))
        })
    }
}

pub fn get_bytes_ref<AV: SharedAccessVec<i32>>(ir: &IntsRef<AV>) -> BytesRef<Vec<u8>> {
    let len = ir.length;
    let mut bytes = vec![0u8; len];

    ir.ints.access(|ints| {
        for i in 0..len {
            let x = ints[ir.offset + i];
            assert!((0..=255).contains(&x), "x={} out of range", x);
            bytes[i] = x as u8;
        }
    });

    BytesRef {
        bytes,
        offset: 0,
        length: len,
    }
}
pub fn get_random_string<R: Rng>(random: &mut R) -> String {
    if random.random_bool(0.5) {
        TestUtil::random_realistic_unicode_string(random)
    } else {
        simple_random_string(random)
    }
}
pub fn simple_random_string<R: Rng>(rng: &mut R) -> String {
    let end = rng.random_range(0..11);
    if end == 10 {
        // allow 0 length
        return String::new();
    }

    let mut buffer = String::with_capacity(end);
    for _ in 0..end {
        let c = rng.random_range(97..=102) as u8 as char; // 'a' to 'f'
        buffer.push(c);
    }

    buffer
}
pub fn to_ints_ref_from_string<AV: SharedAccessVec<i32>>(s: &str, input_mode: i32) -> IntsRef<AV> {
    let mut ir = IntsRefBuilder::default();
    to_ints_ref_from_string_with_builder(s, input_mode, &mut ir)
}

pub fn to_ints_ref_from_string_with_builder<AV: SharedAccessVec<i32>>(
    s: &str,
    input_mode: i32,
    ir: &mut IntsRefBuilder<AV>,
) -> IntsRef<AV> {
    if input_mode == 0 {
        // utf8
        let br: BytesRef<Vec<u8>> = BytesRef::from_string(s);
        to_ints_ref(&br, ir)
    } else {
        // utf32
        to_ints_ref_utf32(s, ir)
    }
}

pub fn to_ints_ref_utf32<AV: SharedAccessVec<i32>>(
    s: &str,
    ir: &mut IntsRefBuilder<AV>,
) -> IntsRef<AV> {
    ir.clear();
    for c in s.chars() {
        ir.append(c as i32);
    }
    ir.get().clone()
}

pub fn to_ints_ref_from_bytes<AV: SharedAccessVec<i32>>(
    br: &BytesRef<Vec<u8>>,
    ir: &mut IntsRefBuilder<AV>,
) -> IntsRef<AV> {
    ir.clear();
    ir.grow_no_copy(br.length);
    for i in 0..br.length {
        let byte = br.bytes[br.offset + i];
        ir.append(byte as i32);
    }
    ir.get_owner()
}
pub fn to_ints_ref<AV1: SharedAccessVec<u8>, AV2: SharedAccessVec<i32>>(
    br: &BytesRef<AV1>,
    ir: &mut IntsRefBuilder<AV2>,
) -> IntsRef<AV2> {
    ir.grow_no_copy(br.length);
    ir.clear();
    br.bytes.access(|bytes| {
        for i in 0..br.length {
            ir.append(bytes[br.offset + i] as i32);
        }
    });
    ir.get_owner()
}
